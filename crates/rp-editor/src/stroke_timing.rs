//! Scripted stroke timing (ignored; a report, not a check): pointer events
//! at 120 Hz drive a real [`RenderWorker`] the way the view does, and the
//! report shows per-event UI-thread time, how often an event copied the
//! whole layer, and edit → canvas latency.
//!
//! `cargo test -p rp-editor stroke_timing -- --ignored --nocapture --test-threads 1`
//!
//! The 1024 px splatter is the densest brush of the first `.abr` under
//! `ABR_BRUSH_DIR` (default `~/brushes`), or a
//! synthetic speckle when none is found.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rp_brush::{Brush, Mask, decode_abr, file_stem, index_abr};
use rp_doc::{Document, Layer, LayerKind, Size, Tool};

use crate::paint_ops::{PaintButton, working_base};
use crate::render_worker::{Overlay, RenderJob, RenderWorker, Request, next_request};
use crate::state::EditorState;
use crate::viewport::Vec2;

const EVENT_HZ: f64 = 120.0;
const EVENTS: usize = 240;
/// Pointer speed, canvas pixels per second.
const SPEED: f32 = 1500.0;

fn find_abr(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    items.sort();
    for p in items {
        if p.is_dir() {
            find_abr(&p, out);
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("abr")) {
            out.push(p);
        }
    }
}

/// The densest brush of the first `.abr` found, else a synthetic speckle.
fn splatter() -> Brush {
    let dir = std::env::var_os("ABR_BRUSH_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("brushes")));
    let mut files = Vec::new();
    if let Some(dir) = dir {
        find_abr(&dir, &mut files);
    }
    let real = files.first().and_then(|path| {
        let bytes = std::fs::read(path).ok()?;
        let idx = index_abr(&bytes, &file_stem(path)).ok()?;
        let mut best: Option<(f64, Brush)> = None;
        for e in &idx.entries {
            let b = decode_abr(&bytes, e).ok()?;
            let mean =
                b.mask.data.iter().map(|&v| v as f64).sum::<f64>() / b.mask.data.len() as f64;
            if best.as_ref().is_none_or(|(m, _)| mean > *m) {
                best = Some((mean, b));
            }
        }
        let (mean, b) = best?;
        eprintln!(
            "splatter: {} / {} ({}x{}, mean coverage {mean:.1}, spacing {})",
            path.display(),
            b.name,
            b.mask.width,
            b.mask.height,
            b.spacing
        );
        Some(b)
    });
    real.unwrap_or_else(|| {
        eprintln!("splatter: synthetic speckle (no .abr found)");
        let n = 1024u32;
        let data = (0..n * n)
            .map(|i| if (i * 2_654_435_761) % 7 == 0 { 255 } else { 0 })
            .collect();
        Brush::bitmap("Speckle", 10.0, Mask::new(n, n, data), None)
    })
}

/// Pointer position after `t` seconds: a zigzag bouncing inside the canvas.
fn path(size: Size, t: f32) -> Vec2 {
    let (w, h) = (size.width as f32, size.height as f32);
    let d = SPEED * t;
    let bounce = |v: f32, len: f32| {
        let span = len - 200.0;
        let m = v.rem_euclid(2.0 * span);
        100.0 + if m < span { m } else { 2.0 * span - m }
    };
    Vec2::new(bounce(d, w), bounce(d * 0.37 + 50.0, h))
}

fn layer_ptr(st: &EditorState) -> *const u8 {
    match &st.selected().expect("layer").kind {
        LayerKind::Paint(p) => p.pixels.as_raw().as_ptr(),
        LayerKind::Image(_) => panic!("paint layer"),
    }
}

fn pct(v: &mut [Duration], p: f64) -> Duration {
    if v.is_empty() {
        return Duration::ZERO;
    }
    v.sort();
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn mean(v: &[Duration]) -> Duration {
    if v.is_empty() {
        return Duration::ZERO;
    }
    v.iter().sum::<Duration>() / v.len() as u32
}

/// What the view does on the UI thread when a canvas frame arrives, besides
/// showing it. Returns whether the document changed.
fn on_canvas_frame(st: &mut EditorState) -> bool {
    st.flush_stroke() > 0
}

/// Runs one scripted stroke and prints the report line.
fn run(label: &str, size: Size, setup: impl FnOnce(&mut EditorState)) {
    let dos_frames = Arc::new(AtomicU64::new(0));
    let (mut worker, client, canvas) = RenderWorker::spawn(Box::new({
        let n = dos_frames.clone();
        move |_| {
            n.fetch_add(1, Ordering::Relaxed);
        }
    }));
    let mut st = EditorState::new();
    let mut doc = Document::new(size);
    // A busy document: a filled background plus the paint layer.
    let mut bg = Layer::paint("bg", size);
    if let LayerKind::Paint(p) = &mut bg.kind {
        p.pixels = rp_doc::RgbaImage::filled(size, [200, 180, 160, 255]);
    }
    doc.push_layer(bg);
    doc.push_layer(Layer::paint("Paint", size));
    st.set_document(doc);
    st.tool = Tool::Brush;
    setup(&mut st);

    let mut sent: Option<(u64, Overlay)> = None;
    let sync = |st: &EditorState, sent: &mut Option<(u64, Overlay)>| {
        let overlay = Overlay::from_state(st);
        match next_request(*sent, st.doc_version, &overlay) {
            Request::Render => client.render(RenderJob::from_state(st), overlay),
            Request::Overlay => client.overlay(overlay),
            Request::Nothing => return,
        }
        *sent = Some((st.doc_version, overlay));
    };
    // Settle: the first render of the document.
    sync(&st, &mut sent);
    canvas
        .next_blocking(Duration::from_secs(30))
        .expect("first frame");

    // doc_version → when the oldest pointer event whose dabs it first
    // shows happened (queued dabs count from the event that queued them).
    let mut edits: HashMap<u64, Instant> = HashMap::new();
    let mut unshown: Option<Instant> = None;
    let mut ui = Vec::new();
    let mut latency = Vec::new();
    let mut copies = 0usize;
    let mut dab_events = 0usize;
    let mut canvas_frames = 0usize;
    let queued = |st: &EditorState| st.stroke.as_ref().map_or(0, |s| s.queued());
    let period = Duration::from_secs_f64(1.0 / EVENT_HZ);
    let start = Instant::now();
    for i in 0..EVENTS {
        let due = start + period * i as u32;
        // Idle until the next pointer event, handling canvas frames as the
        // GPUI task would.
        loop {
            if let Some(f) = canvas.try_take() {
                canvas_frames += 1;
                if let Some(t) = edits.get(&f.version) {
                    latency.push(t.elapsed());
                }
                let t = Instant::now();
                let before = layer_ptr(&st);
                if on_canvas_frame(&mut st) {
                    if layer_ptr(&st) != before {
                        copies += 1;
                    }
                    sync(&st, &mut sent);
                    ui.push(t.elapsed());
                    if let Some(u) = unshown.take() {
                        edits.insert(st.doc_version, u);
                    }
                }
            }
            if Instant::now() >= due {
                break;
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        let c = path(size, i as f32 / EVENT_HZ as f32);
        let t = Instant::now();
        let before = layer_ptr(&st);
        let (v, q) = (st.doc_version, queued(&st));
        if i == 0 {
            st.begin_stroke(c, PaintButton::Primary);
        } else {
            st.continue_stroke(c);
        }
        let changed = st.doc_version != v;
        if i > 0 && layer_ptr(&st) != before {
            copies += 1;
        }
        sync(&st, &mut sent);
        let dt = t.elapsed();
        if changed || queued(&st) > q {
            unshown.get_or_insert(t);
            dab_events += 1;
        }
        if changed && let Some(u) = unshown.take() {
            edits.insert(st.doc_version, u);
        }
        ui.push(dt);
    }
    st.end_stroke();
    sync(&st, &mut sent);
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        match canvas.try_take() {
            Some(f) => {
                canvas_frames += 1;
                if let Some(t) = edits.get(&f.version) {
                    latency.push(t.elapsed());
                }
                if f.version == st.doc_version {
                    break;
                }
            }
            None => std::thread::sleep(Duration::from_micros(200)),
        }
    }
    let elapsed = start.elapsed();
    worker.shutdown();
    let mut ui_sorted = ui.clone();
    let mut lat_sorted = latency.clone();
    eprintln!(
        "{label:<28} {size}: UI/event mean {:>8.2?} p95 {:>8.2?} max {:>8.2?} | \
         layer copies {copies}/{} events ({dab_events} with dabs) | \
         edit→canvas mean {:>8.2?} p95 {:>8.2?} ({canvas_frames} canvas, {} DOS frames in {:.2?})",
        mean(&ui),
        pct(&mut ui_sorted, 0.95),
        pct(&mut ui_sorted, 1.0),
        EVENTS - 1,
        mean(&latency),
        pct(&mut lat_sorted, 0.95),
        dos_frames.load(Ordering::Relaxed),
        elapsed,
    );
}

#[test]
#[ignore = "timing report"]
fn stroke_timing() {
    let splat = splatter();
    for size in [Size::new(800, 1280), Size::new(2048, 2048)] {
        run("40 px round", size, |st| st.set_brush_size(40));
        let splat = splat.clone();
        run("1024 px splatter", size, move |st| {
            st.brush.set_sampled("Splatter", working_base(splat), 1024)
        });
    }
}
