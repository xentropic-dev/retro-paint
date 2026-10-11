//! The render worker: turns document changes into canvas images and DOS
//! frames off the UI thread. See docs/tech-stack.md, "Data flow between
//! views".
//!
//! The UI thread never flattens or dithers. When
//! [`EditorState::doc_version`] changes it snapshots what the pipeline needs
//! ([`RenderJob`]: a clone of the [`Document`] plus the shared
//! `Arc<SourceStore>`) and hands it over; when only the pointer, tool or
//! foreground color changed it hands over a new [`Overlay`]. Both are cheap.
//!
//! Two stages, one thread each, so the canvas never waits for the dither:
//!
//! 1. **Flatten** (thread `render-worker`): flattens the newest job, drops
//!    the snapshot at once (releasing its rasters, see below), publishes the
//!    flattened image as a [`CanvasFrame`] (BGRA for GPUI, tagged with its
//!    `doc_version`) to a latest-only [`LatestSlot`], wakes the
//!    [`CanvasReceiver`] the view awaits, then hands the RGBA to stage 2.
//! 2. **Dither** (thread `render-dos`): owns a [`Renderer`] (cached LUT),
//!    downscales + dithers the newest flattened image and sends one
//!    [`FrameMsg`] built from the last indexed image and the newest overlay.
//!    An overlay-only wake-up re-sends the last image without re-dithering.
//!
//! Requests coalesce at both stages: only the newest pending job, image and
//! overlay are kept, so a burst of edits renders once.
//!
//! **Snapshot leases.** A snapshot shares the document's rasters
//! copy-on-write, so a stroke writing to the layer while the worker still
//! holds a snapshot would copy the whole layer (4 MB at 800×1280) on the UI
//! thread. Each [`RenderJob`] holds a [`SnapshotLease`] from the state's
//! [`SnapshotCounter`]; it is released with the snapshot right after the
//! flatten, before the canvas frame is published. While a lease is out,
//! strokes queue their dabs instead of writing (see
//! [`EditorState::continue_stroke`]) and lay them when the frame arrives
//! ([`EditorState::flush_stroke`]) or on the next pointer event.
//!
//! Shutdown: [`RenderWorker::shutdown`] (also on drop) wakes both threads,
//! lets them finish the frame in progress, and joins them.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rp_doc::{Document, FrameMsg, IndexedFrame, Layout, OutputPoint, Size, SourceStore, Tool};
use rp_palette_dither::{CgaColor, DitherSettings, Palette, Renderer, Rgb};

use crate::canvas_image::rgba_bytes_to_bgra;
use crate::paint_ops::brush_output_diameter;
use crate::state::EditorState;
use crate::viewport::Vec2;

/// Receives each frame for the DOS view, on the worker thread.
pub type FrameSink = Box<dyn FnMut(FrameMsg) + Send>;

/// Environment variable: when set, the worker prints per-frame timings
/// (request → frame sent) to stderr, and the editor prints when each canvas
/// image is shown and how long taking it cost the UI thread.
pub const TIMING_ENV: &str = "RP_RENDER_TIMING";

/// Counts the render snapshots alive (posted to the worker and not yet
/// flattened). Clones share the count.
#[derive(Clone, Debug, Default)]
pub struct SnapshotCounter(Arc<AtomicUsize>);

impl SnapshotCounter {
    /// A lease for one more snapshot; dropping it gives it back.
    pub fn lease(&self) -> SnapshotLease {
        self.0.fetch_add(1, Ordering::AcqRel);
        SnapshotLease(self.0.clone())
    }

    /// Whether any snapshot (and so possibly the document's rasters) is
    /// still held by the worker.
    pub fn busy(&self) -> bool {
        self.0.load(Ordering::Acquire) > 0
    }
}

/// One snapshot's share of a [`SnapshotCounter`]; see the module docs.
#[derive(Debug)]
pub struct SnapshotLease(Arc<AtomicUsize>);

impl Clone for SnapshotLease {
    fn clone(&self) -> Self {
        self.0.fetch_add(1, Ordering::AcqRel);
        Self(self.0.clone())
    }
}

impl Drop for SnapshotLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// What the pipeline needs to re-dither: a snapshot of the document.
#[derive(Clone, Debug)]
pub struct RenderJob {
    pub document: Document,
    pub sources: Arc<SourceStore>,
    /// [`EditorState::doc_version`] of the snapshot.
    pub version: u64,
    /// When the UI thread asked for it (for latency logging).
    pub requested: Instant,
    /// Held until the snapshot is dropped (right after the flatten).
    pub lease: SnapshotLease,
}

impl RenderJob {
    /// The state's [`render_snapshot`](EditorState::render_snapshot): a
    /// floating selection is drawn as a temporary layer.
    pub fn from_state(state: &EditorState) -> Self {
        let lease = state.snapshots.lease();
        let (document, sources) = state.render_snapshot();
        Self {
            document,
            sources,
            version: state.doc_version,
            requested: Instant::now(),
            lease,
        }
    }
}

/// A flattened document on its way to the dither stage.
#[derive(Clone, Debug)]
pub struct Flat {
    pub version: u64,
    /// Straight RGBA8 at `canvas` size.
    pub rgba: Vec<u8>,
    pub canvas: Size,
    pub output: Size,
    pub dither: DitherSettings,
    /// When the UI thread asked for it, and how long the flatten took (for
    /// latency logging).
    pub requested: Instant,
    pub flatten: Duration,
}

/// Stage 1: flattens `job` and drops the snapshot (giving back its lease)
/// before anything else, then converts for the canvas. Returns the canvas
/// image and the RGBA for the dither stage.
pub fn flatten_job(job: RenderJob) -> (CanvasFrame, Flat) {
    let t = Instant::now();
    let RenderJob {
        document,
        sources,
        version,
        requested,
        lease,
    } = job;
    let flat = document.flatten(&sources);
    let (canvas, output, dither) = (document.canvas_size, document.output_size, document.dither);
    // Release the rasters first, then the lease, so a stroke that sees the
    // count drop to zero writes to unshared buffers.
    drop(document);
    drop(sources);
    drop(lease);
    let flatten = t.elapsed();
    let (width, height) = (flat.width(), flat.height());
    let rgba = flat.into_raw();
    let frame = CanvasFrame {
        version,
        width,
        height,
        bgra: rgba_bytes_to_bgra(&rgba),
        requested,
    };
    let flat = Flat {
        version,
        rgba,
        canvas,
        output,
        dither,
        requested,
        flatten,
    };
    (frame, flat)
}

/// The flattened document for the editor canvas, ready for GPUI: straight
/// BGRA, `width × height`, row-major.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CanvasFrame {
    /// The [`EditorState::doc_version`] it was flattened from.
    pub version: u64,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    /// When the UI thread asked for it (for latency logging).
    pub requested: Instant,
}

/// Something tagged with a document version.
pub trait Versioned {
    fn version(&self) -> u64;
}

impl Versioned for CanvasFrame {
    fn version(&self) -> u64 {
        self.version
    }
}

/// A latest-only mailbox between threads: holds at most one value, and a
/// value older than the one already held is dropped.
pub struct LatestSlot<T> {
    value: Mutex<Option<T>>,
}

impl<T> Default for LatestSlot<T> {
    fn default() -> Self {
        Self {
            value: Mutex::new(None),
        }
    }
}

impl<T: Versioned> LatestSlot<T> {
    /// Stores `value` unless the slot holds a newer one. Returns whether it
    /// was stored.
    pub fn publish(&self, value: T) -> bool {
        let mut slot = self.value.lock().expect("latest slot lock");
        if slot.as_ref().is_some_and(|v| v.version() > value.version()) {
            return false;
        }
        *slot = Some(value);
        true
    }

    /// Takes the held value, leaving the slot empty.
    pub fn take(&self) -> Option<T> {
        self.value.lock().expect("latest slot lock").take()
    }
}

/// Worker side of the canvas feed: publishes to the slot and wakes the
/// receiver. Wake-ups coalesce (a one-message channel), and the receiver
/// always takes from the slot after waking, so none is lost.
pub struct CanvasSender {
    slot: Arc<LatestSlot<CanvasFrame>>,
    wake: async_channel::Sender<()>,
}

impl CanvasSender {
    pub fn send(&self, frame: CanvasFrame) {
        if self.slot.publish(frame) {
            let _ = self.wake.try_send(());
        }
    }
}

/// UI side of the canvas feed. Await [`CanvasReceiver::next`] from a GPUI
/// task; it never blocks the thread.
pub struct CanvasReceiver {
    slot: Arc<LatestSlot<CanvasFrame>>,
    wake: async_channel::Receiver<()>,
}

impl CanvasReceiver {
    /// The newest frame published since the last call, waiting for one if
    /// there is none. `None` once the worker has shut down.
    pub async fn next(&self) -> Option<CanvasFrame> {
        loop {
            if let Some(f) = self.slot.take() {
                return Some(f);
            }
            self.wake.recv().await.ok()?;
        }
    }

    /// The newest frame published since the last call, without waiting.
    /// For tests.
    #[cfg(test)]
    pub(crate) fn try_take(&self) -> Option<CanvasFrame> {
        self.slot.take()
    }

    /// Blocks until a frame arrives, the worker shuts down, or `timeout`
    /// passes. For tests.
    #[cfg(test)]
    pub(crate) fn next_blocking(&self, timeout: Duration) -> Option<CanvasFrame> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(f) = self.slot.take() {
                return Some(f);
            }
            // The slot was just checked, so nothing is left once the sender
            // is gone (a stale wake-up may still sit in the channel).
            let closed = self.wake.is_closed();
            if closed || Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// A connected canvas sender/receiver pair.
pub fn canvas_feed() -> (CanvasSender, CanvasReceiver) {
    let slot = Arc::new(LatestSlot::default());
    let (tx, rx) = async_channel::bounded(1);
    (
        CanvasSender {
            slot: slot.clone(),
            wake: tx,
        },
        CanvasReceiver { slot, wake: rx },
    )
}

/// Everything in a [`FrameMsg`] besides the image: cheap to change, never
/// re-dithers.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Overlay {
    pub layout: Layout,
    pub cursor: Option<OutputPoint>,
    pub tool: Tool,
    /// Brush size in output pixels (see [`brush_output_diameter`]).
    pub brush_diameter: u32,
    /// Full-color foreground; the worker maps it to [`FrameMsg::active_color`].
    pub fg: Rgb,
    /// The palette the foreground is matched against (the current filter
    /// palette).
    pub palette: Palette,
}

impl Overlay {
    pub fn from_state(state: &EditorState) -> Self {
        let doc = &state.document;
        Self {
            layout: doc.layout,
            cursor: state
                .cursor_canvas
                .and_then(|c| cursor_to_output(doc.canvas_size, doc.output_size, c)),
            tool: state.tool,
            brush_diameter: brush_output_diameter(
                state.brush.size(),
                doc.canvas_size,
                doc.output_size,
            ),
            fg: doc.fg,
            palette: doc.dither.palette.palette(),
        }
    }
}

/// What the UI thread should post to the worker.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Request {
    /// The document changed (or nothing was sent yet): snapshot and re-dither.
    Render,
    /// Only cursor, tool, colors or layout changed: re-send the last image.
    Overlay,
    /// Nothing the DOS view shows changed.
    Nothing,
}

/// Decides the request given what was last sent (`(doc_version, overlay)`)
/// and the current state.
pub fn next_request(sent: Option<(u64, Overlay)>, doc_version: u64, overlay: &Overlay) -> Request {
    match sent {
        None => Request::Render,
        Some((v, _)) if v != doc_version => Request::Render,
        Some((_, o)) if o != *overlay => Request::Overlay,
        Some(_) => Request::Nothing,
    }
}

/// Maps a canvas-space pointer position to the output pixel under it, or
/// `None` when it lies outside the canvas. Works for non-integer
/// canvas/output ratios; the result is always inside the output image.
pub fn cursor_to_output(canvas: Size, output: Size, c: Vec2) -> Option<OutputPoint> {
    let inside = c.x >= 0.0
        && c.y >= 0.0
        && c.x < canvas.width as f32
        && c.y < canvas.height as f32
        && output.width > 0
        && output.height > 0;
    if !inside {
        return None;
    }
    let p = FrameMsg::canvas_to_output(canvas, output, c.x, c.y);
    Some(OutputPoint {
        x: p.x.clamp(0, output.width as i32 - 1),
        y: p.y.clamp(0, output.height as i32 - 1),
    })
}

/// The dither stage's state: renderer, last image, render count. Pure apart
/// from the pipeline itself, so it is tested directly.
#[derive(Default)]
pub struct WorkerCore {
    renderer: Renderer,
    image: Option<IndexedFrame>,
    renders: u64,
}

impl WorkerCore {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many times the dither ran.
    pub fn renders(&self) -> u64 {
        self.renders
    }

    /// Stage 2: downscales + dithers `flat`, keeping the result as the
    /// current image.
    pub fn dither(&mut self, flat: &Flat) {
        let (c, o) = (flat.canvas, flat.output);
        let img = self.renderer.render(
            &flat.rgba,
            (c.width, c.height),
            (o.width, o.height),
            &flat.dither,
        );
        self.image = Some(img.into());
        self.renders += 1;
    }

    /// Both stages in one go: flattens and dithers `job`, keeping the
    /// dithered result as the current image, and returns the flattened
    /// image for the canvas.
    pub fn render(&mut self, job: RenderJob) -> CanvasFrame {
        let (frame, flat) = flatten_job(job);
        self.dither(&flat);
        frame
    }

    /// The foreground as the nearest color of `palette` (OKLab, through the
    /// cached LUT, the same match the dither uses for a flat area of `fg`).
    pub fn active_color(&mut self, fg: Rgb, palette: Palette) -> CgaColor {
        self.renderer.lut(palette).nearest(fg)
    }

    /// The frame to send for `overlay`, or `None` before the first render.
    pub fn frame(&mut self, overlay: &Overlay) -> Option<FrameMsg> {
        let active_color = self.active_color(overlay.fg, overlay.palette);
        let image = self.image.clone()?;
        Some(FrameMsg {
            image,
            layout: overlay.layout,
            cursor: overlay.cursor,
            tool: overlay.tool,
            brush_diameter: overlay.brush_diameter,
            active_color,
        })
    }
}

/// A latest-only request box with a stop flag, for one stage thread.
struct Mailbox<T> {
    state: Mutex<(T, bool)>,
    wake: Condvar,
}

impl<T: Default> Mailbox<T> {
    fn new() -> Self {
        Self {
            state: Mutex::new((T::default(), false)),
            wake: Condvar::new(),
        }
    }

    fn post(&self, f: impl FnOnce(&mut T)) {
        let mut s = self.state.lock().expect("render worker lock");
        f(&mut s.0);
        drop(s);
        self.wake.notify_one();
    }

    fn stop(&self) {
        self.state.lock().expect("render worker lock").1 = true;
        self.wake.notify_one();
    }

    /// Waits until `ready` holds or a stop is asked, then takes the
    /// contents. `None` on stop.
    fn wait(&self, ready: impl Fn(&T) -> bool) -> Option<T> {
        let mut s = self.state.lock().expect("render worker lock");
        while !s.1 && !ready(&s.0) {
            s = self.wake.wait(s).expect("render worker lock");
        }
        (!s.1).then(|| std::mem::take(&mut s.0))
    }
}

/// Pending work for the dither stage.
#[derive(Default)]
struct DosPending {
    flat: Option<Flat>,
    overlay: Option<Overlay>,
}

struct Shared {
    jobs: Mailbox<Option<RenderJob>>,
    dos: Mailbox<DosPending>,
    renders: AtomicU64,
}

/// UI-side handle: posts requests to the worker. Cheap to clone; never
/// blocks beyond a short mutex hold.
#[derive(Clone)]
pub struct WorkerClient {
    shared: Arc<Shared>,
}

impl WorkerClient {
    /// Asks for a re-flatten and re-dither of `job`, shown with `overlay`.
    /// Replaces any job not yet started.
    pub fn render(&self, job: RenderJob, overlay: Overlay) {
        self.shared.jobs.post(|j| *j = Some(job));
        self.overlay(overlay);
    }

    /// Re-sends the last image with a new overlay; no re-dither.
    pub fn overlay(&self, overlay: Overlay) {
        self.shared.dos.post(|p| p.overlay = Some(overlay));
    }

    /// Number of dither runs so far.
    pub fn renders(&self) -> u64 {
        self.shared.renders.load(Ordering::Relaxed)
    }
}

/// Owns the worker threads. Shut it down (or drop it) to stop and join.
pub struct RenderWorker {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl RenderWorker {
    /// Starts the worker threads; DOS frames go to `sink`, flattened canvas
    /// images to the returned [`CanvasReceiver`].
    pub fn spawn(sink: FrameSink) -> (Self, WorkerClient, CanvasReceiver) {
        let (canvas_tx, canvas_rx) = canvas_feed();
        let shared = Arc::new(Shared {
            jobs: Mailbox::new(),
            dos: Mailbox::new(),
            renders: AtomicU64::new(0),
        });
        let timing = std::env::var_os(TIMING_ENV).is_some();
        let flatten = {
            let shared = shared.clone();
            move || flatten_loop(&shared, canvas_tx, timing)
        };
        let dither = {
            let shared = shared.clone();
            move || dither_loop(&shared, sink, timing)
        };
        let threads = vec![
            std::thread::Builder::new()
                .name("render-worker".into())
                .spawn(flatten)
                .expect("spawn render worker"),
            std::thread::Builder::new()
                .name("render-dos".into())
                .spawn(dither)
                .expect("spawn render worker"),
        ];
        let client = WorkerClient {
            shared: shared.clone(),
        };
        (Self { shared, threads }, client, canvas_rx)
    }

    /// Stops the threads after the frame in progress and joins them.
    /// Pending requests are dropped. Idempotent.
    pub fn shutdown(&mut self) {
        if self.threads.is_empty() {
            return;
        }
        self.shared.jobs.stop();
        self.shared.dos.stop();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        // Give back the lease of a job that never started.
        self.shared.jobs.post(|j| *j = None);
    }
}

impl Drop for RenderWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn flatten_loop(shared: &Shared, canvas: CanvasSender, timing: bool) {
    while let Some(Some(job)) = shared.jobs.wait(Option::is_some) {
        let (frame, flat) = flatten_job(job);
        canvas.send(frame);
        if timing {
            eprintln!(
                "render-worker: canvas sent {:.2?} after doc change (flatten {:.2?})",
                flat.requested.elapsed(),
                flat.flatten
            );
        }
        shared.dos.post(|p| p.flat = Some(flat));
    }
}

fn dither_loop(shared: &Shared, mut sink: FrameSink, timing: bool) {
    let mut core = WorkerCore::new();
    let mut overlay = None;
    while let Some(p) = shared.dos.wait(|p| p.flat.is_some() || p.overlay.is_some()) {
        if p.overlay.is_some() {
            overlay = p.overlay;
        }
        let rendered = p.flat.map(|flat| {
            let t = Instant::now();
            core.dither(&flat);
            shared.renders.store(core.renders(), Ordering::Relaxed);
            (flat.requested, t.elapsed())
        });
        let Some(ov) = overlay else { continue };
        if let Some(frame) = core.frame(&ov) {
            sink(frame);
            if timing && let Some((requested, dither)) = rendered {
                eprintln!(
                    "render-worker: DOS frame sent {:.2?} after doc change (dither {:.2?})",
                    requested.elapsed(),
                    dither
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rp_doc::{Layer, LayerKind, RgbaImage};
    use rp_palette_dither::{FourColor, Intensity, PaletteChoice, Preset};
    use std::sync::mpsc;
    use std::task::{Context, Poll, Waker};

    const TIMEOUT: Duration = Duration::from_secs(10);

    /// 8×8 canvas → 2×2 output filled with one opaque CGA color.
    fn solid_doc(c: CgaColor) -> Document {
        let size = Size::new(8, 8);
        let mut doc = Document::new(size);
        let mut layer = Layer::paint("fill", size);
        if let LayerKind::Paint(p) = &mut layer.kind {
            let Rgb { r, g, b } = c.into();
            p.pixels = RgbaImage::filled(size, [r, g, b, 255]);
        }
        doc.push_layer(layer);
        doc
    }

    fn job(doc: Document) -> RenderJob {
        job_v(doc, 0)
    }

    fn job_v(doc: Document, version: u64) -> RenderJob {
        RenderJob {
            document: doc,
            sources: Arc::new(SourceStore::new()),
            version,
            requested: Instant::now(),
            lease: SnapshotCounter::default().lease(),
        }
    }

    fn canvas_frame(version: u64) -> CanvasFrame {
        CanvasFrame {
            version,
            width: 1,
            height: 1,
            bgra: vec![0, 0, 0, 255],
            requested: Instant::now(),
        }
    }

    /// Polls `fut` once.
    fn poll_once<F: Future>(fut: F) -> Poll<F::Output> {
        let mut fut = std::pin::pin!(fut);
        fut.as_mut().poll(&mut Context::from_waker(Waker::noop()))
    }

    fn overlay() -> Overlay {
        Overlay {
            layout: Layout::Mobile,
            cursor: None,
            tool: Tool::Brush,
            brush_diameter: 3,
            fg: Rgb::new(0, 0, 0),
            palette: Palette::full(),
        }
    }

    /// A sink that forwards frames to a channel.
    fn channel_sink() -> (FrameSink, mpsc::Receiver<FrameMsg>) {
        let (tx, rx) = mpsc::channel();
        (
            Box::new(move |f| {
                let _ = tx.send(f);
            }),
            rx,
        )
    }

    #[test]
    fn core_renders_palette_only_frames() {
        let mut core = WorkerCore::new();
        assert!(core.frame(&overlay()).is_none(), "no image yet");
        core.render(job(solid_doc(CgaColor::Red)));
        let f = core.frame(&overlay()).expect("frame");
        assert_eq!(f.image.size, Size::new(2, 2));
        assert!(f.image.pixels.iter().all(|&c| c == CgaColor::Red));
        assert!(f.image.pixels.iter().all(|&c| f.image.palette.contains(c)));
    }

    #[test]
    fn core_overlay_change_does_not_rerender() {
        let mut core = WorkerCore::new();
        core.render(job(solid_doc(CgaColor::Blue)));
        let mut ov = overlay();
        let a = core.frame(&ov).unwrap();
        ov.tool = Tool::Select;
        ov.cursor = Some(OutputPoint { x: 1, y: 0 });
        let b = core.frame(&ov).unwrap();
        assert_eq!(core.renders(), 1);
        assert_eq!(a.image, b.image);
        assert_eq!((b.tool, b.cursor), (Tool::Select, ov.cursor));
    }

    #[test]
    fn request_decision() {
        let ov = overlay();
        assert_eq!(next_request(None, 0, &ov), Request::Render);
        assert_eq!(next_request(Some((0, ov)), 0, &ov), Request::Nothing);
        let mut moved = ov;
        moved.cursor = Some(OutputPoint { x: 3, y: 4 });
        assert_eq!(next_request(Some((0, ov)), 0, &moved), Request::Overlay);
        assert_eq!(next_request(Some((0, ov)), 1, &moved), Request::Render);
        assert_eq!(next_request(Some((0, ov)), 1, &ov), Request::Render);
    }

    #[test]
    fn cursor_mapping_integer_ratio() {
        let c = Size::new(800, 1280);
        let o = Size::new(200, 320);
        let m = |x, y| cursor_to_output(c, o, Vec2::new(x, y));
        assert_eq!(m(0.0, 0.0), Some(OutputPoint { x: 0, y: 0 }));
        assert_eq!(m(7.9, 4.0), Some(OutputPoint { x: 1, y: 1 }));
        assert_eq!(m(799.99, 1279.99), Some(OutputPoint { x: 199, y: 319 }));
    }

    #[test]
    fn cursor_mapping_non_integer_ratio() {
        // 1000×700 → 300×210: 0.3 output px per canvas px.
        let c = Size::new(1000, 700);
        let o = Size::new(300, 210);
        let m = |x, y| cursor_to_output(c, o, Vec2::new(x, y));
        assert_eq!(m(3.4, 3.3), Some(OutputPoint { x: 1, y: 0 }));
        assert_eq!(m(500.0, 350.0), Some(OutputPoint { x: 150, y: 105 }));
        assert_eq!(m(999.999, 699.999), Some(OutputPoint { x: 299, y: 209 }));
        // Upscale too: 3×3 → 7×7.
        let up = cursor_to_output(Size::new(3, 3), Size::new(7, 7), Vec2::new(2.9, 1.0));
        assert_eq!(up, Some(OutputPoint { x: 6, y: 2 }));
    }

    #[test]
    fn cursor_mapping_out_of_bounds_is_none() {
        let c = Size::new(800, 1280);
        let o = Size::new(200, 320);
        for (x, y) in [
            (-0.01, 10.0),
            (10.0, -3.0),
            (800.0, 10.0),
            (10.0, 1280.0),
            (5000.0, 5000.0),
        ] {
            assert_eq!(cursor_to_output(c, o, Vec2::new(x, y)), None, "{x},{y}");
        }
    }

    #[test]
    fn overlay_from_state_maps_cursor() {
        let mut st = EditorState::new();
        assert_eq!(Overlay::from_state(&st).cursor, None);
        st.cursor_canvas = Some(Vec2::new(401.0, 641.0));
        assert_eq!(
            Overlay::from_state(&st).cursor,
            Some(OutputPoint { x: 100, y: 160 })
        );
        st.cursor_canvas = Some(Vec2::new(-5.0, 641.0));
        assert_eq!(Overlay::from_state(&st).cursor, None);
    }

    #[test]
    fn overlay_brush_diameter_follows_brush_size_in_output_pixels() {
        let mut st = EditorState::new();
        // 800x1280 canvas -> 200x320 output: a quarter.
        st.set_brush_size(40);
        let o = Overlay::from_state(&st);
        assert_eq!(o.brush_diameter, 10);
        st.set_brush_size(2);
        let o2 = Overlay::from_state(&st);
        assert_eq!(o2.brush_diameter, 1);
        // A size change alone is an overlay update, not a re-render.
        assert_eq!(
            next_request(Some((st.doc_version, o)), st.doc_version, &o2),
            Request::Overlay
        );
    }

    #[test]
    fn active_color_in_four_color_mode() {
        let four = FourColor::new(Preset::Palette1, Intensity::High).palette();
        let mut core = WorkerCore::new();
        // Exact palette colors map to themselves.
        for &c in four.colors() {
            assert_eq!(core.active_color(c.into(), four), c);
        }
        // Anything else maps into the subset.
        for fg in [
            Rgb::new(255, 0, 0),
            Rgb::new(0, 200, 0),
            Rgb::new(10, 10, 255),
            Rgb::new(128, 128, 128),
            Rgb::new(255, 255, 0),
        ] {
            assert!(four.contains(core.active_color(fg, four)), "{fg}");
        }
        assert_eq!(
            core.active_color(Rgb::new(250, 250, 250), four),
            CgaColor::White
        );
        assert_eq!(core.active_color(Rgb::new(5, 5, 5), four), CgaColor::Black);
        // The same red is Red in the full palette.
        assert_eq!(
            core.active_color(Rgb::new(170, 0, 0), Palette::full()),
            CgaColor::Red
        );
    }

    #[test]
    fn overlay_uses_filter_palette() {
        let mut st = EditorState::new();
        let four = FourColor::new(Preset::Palette1, Intensity::High);
        st.document.dither.palette = PaletteChoice::FourColor(four);
        assert_eq!(Overlay::from_state(&st).palette, four.palette());
    }

    #[test]
    fn requests_coalesce_to_the_newest() {
        // The sink blocks on the first frame until released, so every
        // request posted meanwhile piles up; only the newest is rendered.
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let (tx, rx) = mpsc::channel();
        let mut sent = 0;
        let sink: FrameSink = Box::new(move |f| {
            sent += 1;
            let _ = tx.send(f);
            if sent == 1 {
                let _ = gate_rx.recv_timeout(TIMEOUT);
            }
        });
        let (mut worker, client, canvas) = RenderWorker::spawn(sink);
        client.render(job(solid_doc(CgaColor::Black)), overlay());
        let first = rx.recv_timeout(TIMEOUT).expect("first frame");
        assert!(first.image.pixels.iter().all(|&c| c == CgaColor::Black));

        let colors = [
            CgaColor::Blue,
            CgaColor::Green,
            CgaColor::Cyan,
            CgaColor::Red,
            CgaColor::Magenta,
            CgaColor::Yellow,
        ];
        for (v, c) in colors.into_iter().enumerate() {
            client.render(job_v(solid_doc(c), v as u64 + 1), overlay());
        }
        // The flatten stage does not wait for the blocked DOS sink.
        let last = colors.len() as u64;
        while canvas.next_blocking(TIMEOUT).expect("canvas frame").version != last {}
        gate_tx.send(()).unwrap();
        let second = rx.recv_timeout(TIMEOUT).expect("second frame");
        assert!(second.image.pixels.iter().all(|&c| c == CgaColor::Yellow));
        worker.shutdown();
        assert_eq!(client.renders(), 2, "the burst rendered once");
        assert!(rx.try_recv().is_err(), "no further frames");
    }

    #[test]
    fn overlay_only_update_resends_without_rerender() {
        let (sink, rx) = channel_sink();
        let (mut worker, client, _canvas) = RenderWorker::spawn(sink);
        client.render(job(solid_doc(CgaColor::Green)), overlay());
        let a = rx.recv_timeout(TIMEOUT).unwrap();
        let mut ov = overlay();
        ov.cursor = Some(OutputPoint { x: 1, y: 1 });
        ov.tool = Tool::Picker;
        ov.fg = CgaColor::LightRed.into();
        client.overlay(ov);
        let b = rx.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(client.renders(), 1);
        assert_eq!(a.image, b.image);
        assert_eq!(b.cursor, ov.cursor);
        assert_eq!(b.tool, Tool::Picker);
        assert_eq!(b.active_color, CgaColor::LightRed);
        worker.shutdown();
    }

    #[test]
    fn overlay_before_first_render_sends_nothing() {
        let (sink, rx) = channel_sink();
        let (mut worker, client, _canvas) = RenderWorker::spawn(sink);
        client.overlay(overlay());
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        worker.shutdown();
    }

    #[test]
    fn shutdown_joins_and_drops_the_sink() {
        let (sink, rx) = channel_sink();
        let (mut worker, client, _canvas) = RenderWorker::spawn(sink);
        client.render(job(solid_doc(CgaColor::Cyan)), overlay());
        rx.recv_timeout(TIMEOUT).unwrap();
        worker.shutdown();
        assert!(worker.threads.is_empty());
        // The thread is gone, so its sink (and sender) was dropped.
        assert_eq!(
            rx.recv_timeout(TIMEOUT),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
        // Requests after shutdown are harmless; shutdown is idempotent.
        client.overlay(overlay());
        worker.shutdown();
    }

    #[test]
    fn drop_joins_the_thread() {
        let (sink, rx) = channel_sink();
        let (worker, _client, _canvas) = RenderWorker::spawn(sink);
        drop(worker);
        assert_eq!(
            rx.recv_timeout(TIMEOUT),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
    }

    #[test]
    fn slot_keeps_the_newest_and_drops_stale() {
        let slot = LatestSlot::default();
        assert!(slot.take().is_none());
        assert!(slot.publish(canvas_frame(3)));
        assert!(!slot.publish(canvas_frame(2)), "older than the held one");
        assert!(slot.publish(canvas_frame(5)));
        assert_eq!(slot.take().map(|f| f.version), Some(5));
        assert!(slot.take().is_none(), "take empties the slot");
        // Once taken, ordering is the receiver's job (see CanvasCache).
        assert!(slot.publish(canvas_frame(1)));
    }

    #[test]
    fn receiver_wakes_for_published_frames_and_ends_on_shutdown() {
        let (tx, rx) = canvas_feed();
        assert!(poll_once(rx.next()).is_pending(), "nothing yet");
        tx.send(canvas_frame(1));
        tx.send(canvas_frame(2));
        tx.send(canvas_frame(0)); // stale: dropped
        match poll_once(rx.next()) {
            Poll::Ready(Some(f)) => assert_eq!(f.version, 2),
            other => panic!("expected frame 2, got {other:?}"),
        }
        // The leftover wake-up finds an empty slot and keeps waiting.
        assert!(poll_once(rx.next()).is_pending());
        tx.send(canvas_frame(3));
        drop(tx);
        // A frame published before shutdown is still delivered, then None.
        assert!(matches!(poll_once(rx.next()), Poll::Ready(Some(f)) if f.version == 3));
        assert!(matches!(poll_once(rx.next()), Poll::Ready(None)));
    }

    #[test]
    fn core_render_returns_the_flattened_canvas_as_bgra() {
        let mut core = WorkerCore::new();
        let doc = solid_doc(CgaColor::Red);
        let expect = doc.flatten(&SourceStore::new());
        let frame = core.render(job_v(doc, 7));
        assert_eq!(frame.version, 7);
        assert_eq!((frame.width, frame.height), (8, 8));
        assert_eq!(frame.bgra, crate::canvas_image::rgba_to_bgra(&expect));
        let Rgb { r, g, b } = CgaColor::Red.into();
        assert_eq!(&frame.bgra[..4], &[b, g, r, 255]);
    }

    #[test]
    fn worker_publishes_the_canvas_for_each_render() {
        let (sink, rx) = channel_sink();
        let (mut worker, client, canvas) = RenderWorker::spawn(sink);
        client.render(job_v(solid_doc(CgaColor::Green), 4), overlay());
        let f = canvas.next_blocking(TIMEOUT).expect("canvas frame");
        assert_eq!(f.version, 4);
        rx.recv_timeout(TIMEOUT).expect("DOS frame");
        // Overlay-only updates do not re-flatten.
        client.overlay(overlay());
        rx.recv_timeout(TIMEOUT).expect("DOS frame");
        assert!(canvas.next_blocking(Duration::from_millis(50)).is_none());
        worker.shutdown();
        assert!(canvas.next_blocking(TIMEOUT).is_none(), "feed closed");
    }

    #[test]
    fn canvas_does_not_wait_for_the_dos_stage() {
        // Hold the dither stage in the DOS sink on the first frame; the
        // canvas still gets every newer render, ending with the newest.
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let (tx, rx) = mpsc::channel();
        let mut sent = 0;
        let sink: FrameSink = Box::new(move |f| {
            sent += 1;
            let _ = tx.send(f);
            if sent == 1 {
                let _ = gate_rx.recv_timeout(TIMEOUT);
            }
        });
        let (mut worker, client, canvas) = RenderWorker::spawn(sink);
        client.render(job_v(solid_doc(CgaColor::Black), 1), overlay());
        assert_eq!(canvas.next_blocking(TIMEOUT).map(|f| f.version), Some(1));
        rx.recv_timeout(TIMEOUT)
            .expect("first DOS frame (now blocked)");
        for v in 2..8 {
            client.render(job_v(solid_doc(CgaColor::Blue), v), overlay());
        }
        let mut seen = 0;
        while seen != 7 {
            let f = canvas
                .next_blocking(TIMEOUT)
                .expect("canvas while DOS blocked");
            assert!(f.version > seen, "versions only grow");
            seen = f.version;
        }
        gate_tx.send(()).unwrap();
        let second = rx.recv_timeout(TIMEOUT).expect("second DOS frame");
        assert!(second.image.pixels.iter().all(|&c| c == CgaColor::Blue));
        worker.shutdown();
        assert_eq!(client.renders(), 2, "the dither coalesced the burst");
    }

    #[test]
    fn snapshot_lease_is_released_after_flatten_before_the_canvas() {
        let counter = SnapshotCounter::default();
        assert!(!counter.busy());
        let mut j = job(solid_doc(CgaColor::Red));
        j.lease = counter.lease();
        assert!(counter.busy());
        let copy = j.clone();
        drop(j);
        assert!(counter.busy(), "a clone holds its own lease");
        let (frame, flat) = flatten_job(copy);
        assert!(!counter.busy(), "released by the flatten");
        assert_eq!(frame.version, flat.version);

        // Through the worker: by the time the canvas frame arrives the
        // snapshot is gone.
        let (sink, _rx) = channel_sink();
        let (mut worker, client, canvas) = RenderWorker::spawn(sink);
        let mut j = job_v(solid_doc(CgaColor::Red), 3);
        j.lease = counter.lease();
        client.render(j, overlay());
        assert_eq!(canvas.next_blocking(TIMEOUT).map(|f| f.version), Some(3));
        assert!(!counter.busy());
        // A job still pending at shutdown gives its lease back too.
        worker.shutdown();
        let mut j = job(solid_doc(CgaColor::Red));
        j.lease = counter.lease();
        client.render(j, overlay());
        assert!(counter.busy());
        drop(worker);
    }

    #[test]
    fn strokes_queue_while_a_snapshot_is_out_and_never_copy_the_layer() {
        use crate::paint_ops::PaintButton;
        use rp_doc::LayerKind;
        let size = Size::new(64, 64);
        let fresh = || {
            let mut st = EditorState::new();
            let mut doc = Document::new(size);
            doc.push_layer(Layer::paint("Paint", size));
            st.set_document(doc);
            st.tool = Tool::Brush;
            st.set_brush_size(5);
            st
        };
        let ptr = |st: &EditorState| match &st.selected().unwrap().kind {
            LayerKind::Paint(p) => p.pixels.as_raw().as_ptr(),
            _ => panic!("paint layer"),
        };
        let path = [(10.0, 10.0), (20.0, 12.0), (30.0, 30.0), (50.0, 40.0)];

        let mut st = fresh();
        st.begin_stroke(Vec2::new(5.0, 5.0), PaintButton::Primary);
        let after_first = ptr(&st);
        let mut job = Some(RenderJob::from_state(&st));
        let v = st.doc_version;
        for (x, y) in path {
            assert_eq!(st.continue_stroke(Vec2::new(x, y)), 0, "queued");
        }
        assert_eq!(st.doc_version, v, "nothing laid, nothing to re-render");
        assert_eq!(st.flush_stroke(), 0, "still held");
        // The worker flattens and releases the snapshot.
        let (_frame, _flat) = flatten_job(job.take().unwrap());
        assert!(st.flush_stroke() > 0);
        assert_ne!(st.doc_version, v);
        assert_eq!(ptr(&st), after_first, "the layer was never copied");
        // Queued dabs left at the release are laid too.
        let held = RenderJob::from_state(&st);
        assert_eq!(st.continue_stroke(Vec2::new(60.0, 60.0)), 0);
        st.end_stroke();
        drop(held);
        // Reference: no snapshots, every dab laid at once.
        let mut reference = fresh();
        reference.begin_stroke(Vec2::new(5.0, 5.0), PaintButton::Primary);
        for (x, y) in path.into_iter().chain([(60.0, 60.0)]) {
            assert!(reference.continue_stroke(Vec2::new(x, y)) > 0);
        }
        reference.end_stroke();
        assert_eq!(
            st.document, reference.document,
            "same pixels as laying at once"
        );
        // One undo step that restores the empty layer.
        st.undo();
        assert_eq!(st.document, fresh().document);
    }

    /// Timing of the steps on a fresh 800×1280 document with four paint
    /// layers. Run with `cargo test --release -p rp-editor -- --ignored
    /// --nocapture snapshot_timing`.
    #[test]
    #[ignore = "timing report"]
    fn snapshot_timing() {
        let mut st = EditorState::new();
        for i in 0..3 {
            let size = st.document.canvas_size;
            st.document.push_layer(Layer::paint(format!("l{i}"), size));
        }
        let n = 20;
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(RenderJob::from_state(&st));
        }
        eprintln!("snapshot (UI thread): {:?}", t.elapsed() / n);
        let job = RenderJob::from_state(&st);
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(job.document.flatten(&job.sources));
        }
        eprintln!("flatten (worker): {:?}", t.elapsed() / n);
        let mut core = WorkerCore::new();
        core.render(job.clone());
        let t = Instant::now();
        for _ in 0..n {
            core.render(job.clone());
        }
        eprintln!("flatten + dither (worker): {:?}", t.elapsed() / n);

        // One paint event as the UI thread runs it: a mouse move that lays
        // dabs (brush size 40, ~30 px of travel) plus the snapshot the view
        // then hands the worker.
        let top = st.document.layers().last().unwrap().id();
        st.select_layer(top);
        st.set_brush_size(40);
        st.begin_stroke(
            Vec2::new(100.0, 100.0),
            crate::paint_ops::PaintButton::Primary,
        );
        let mut x = 100.0;
        let (mut stroke, mut snap) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
        for _ in 0..n {
            x += 30.0;
            let t = Instant::now();
            st.continue_stroke(Vec2::new(x, 100.0 + x / 4.0));
            stroke += t.elapsed();
            let t = Instant::now();
            std::hint::black_box(RenderJob::from_state(&st));
            snap += t.elapsed();
        }
        eprintln!(
            "paint event (UI thread): stroke {:?} + snapshot {:?} = {:?}",
            stroke / n,
            snap / n,
            (stroke + snap) / n
        );
    }
}
