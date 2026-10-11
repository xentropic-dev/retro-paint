//! Release-mode timings for flatten, document clones and history snapshots.
//!
//! `cargo run --release -p rp-doc --example flatten_bench`

use std::alloc::{GlobalAlloc, Layout as AllocLayout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rp_doc::flatten::flatten_with_threads;
use rp_doc::{
    BlendMode, Document, GrayImage, History, Layer, RgbaImage, Size, SourceHash, SourceStore,
    Transform,
};
use rp_palette_dither::Rgb;

#[path = "../tests/reference_flatten/mod.rs"]
mod reference;

/// Counts live heap bytes so clone/snapshot memory can be reported.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: AllocLayout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: AllocLayout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Ordering::Relaxed);
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: AllocLayout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: AllocLayout, new: usize) -> *mut u8 {
        LIVE.fetch_add(new, Ordering::Relaxed);
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const CANVAS: Size = Size::new(800, 1280);

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Opaque noisy background.
fn background(rng: &mut Rng) -> Layer {
    let mut l = Layer::paint("bg", CANVAS);
    for b in l.pixels_mut().unwrap().as_raw_mut().as_chunks_mut::<4>().0 {
        let v = rng.next();
        b.copy_from_slice(&[v as u8, (v >> 8) as u8, (v >> 16) as u8, 255]);
    }
    l
}

/// Mostly transparent with opaque, soft-edged blobs.
fn strokes(rng: &mut Rng, name: &str) -> Layer {
    let mut l = Layer::paint(name, CANVAS);
    let px = l.pixels_mut().unwrap();
    for _ in 0..40 {
        let cx = (rng.next() % CANVAS.width as u64) as i64;
        let cy = (rng.next() % CANVAS.height as u64) as i64;
        let r = 20 + (rng.next() % 60) as i64;
        let c = rng.next();
        for y in (cy - r).max(0)..(cy + r).min(CANVAS.height as i64) {
            for x in (cx - r).max(0)..(cx + r).min(CANVAS.width as i64) {
                let d2 = (x - cx).pow(2) + (y - cy).pow(2);
                if d2 < r * r {
                    let a = if d2 < (r - 4).pow(2) { 255 } else { 128 };
                    px.put(
                        x as u32,
                        y as u32,
                        [c as u8, (c >> 8) as u8, (c >> 16) as u8, a],
                    );
                }
            }
        }
    }
    l
}

/// Fastest of `iters` runs (the machine may be busy; the minimum is the
/// least noisy estimate).
fn time<T>(iters: u32, mut f: impl FnMut() -> T) -> Duration {
    black_box(f());
    (0..iters)
        .map(|_| {
            let start = Instant::now();
            black_box(f());
            start.elapsed()
        })
        .min()
        .unwrap()
}

fn main() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut sources = SourceStore::new();
    let mut src = RgbaImage::new(Size::new(640, 480));
    for b in src.as_raw_mut().as_chunks_mut::<4>().0 {
        let v = rng.next();
        b.copy_from_slice(&[v as u8, (v >> 8) as u8, (v >> 16) as u8, 255]);
    }
    sources.insert(SourceHash::new("src"), src);

    let mut one = Document::new(CANVAS);
    one.push_layer(background(&mut rng));

    let mut four = Document::new(CANVAS);
    four.push_layer(background(&mut rng));
    four.push_layer(Layer::image(
        "photo",
        SourceHash::new("src"),
        Transform {
            translate_x: 80.0,
            translate_y: 300.0,
            scale_x: 1.1,
            scale_y: 1.1,
            rotation: 0.2,
        },
    ));
    four.push_layer(strokes(&mut rng, "strokes"));
    let mut shade = strokes(&mut rng, "shade");
    shade.blend = BlendMode::Multiply;
    shade.opacity = 60;
    let mut mask = GrayImage::filled(CANVAS, 255);
    for (i, m) in mask.as_raw_mut().iter_mut().enumerate() {
        *m = (i % 256) as u8;
    }
    shade.mask = Some(mask);
    four.push_layer(shade);

    let iters = 30;
    for (name, doc) in [("1 paint layer", &one), ("4 layers     ", &four)] {
        assert_eq!(doc.flatten(&sources), reference::flatten(doc, &sources));
        println!(
            "flatten 800x1280, {name}: before {:?}, after {:?} ({:?} on 1 thread)",
            time(iters, || reference::flatten(doc, &sources)),
            time(iters, || doc.flatten(&sources)),
            time(iters, || flatten_with_threads(doc, &sources, 1)),
        );
    }

    // RenderJob-style clone of the 4-layer document.
    let base = LIVE.load(Ordering::Relaxed);
    let clone = four.clone();
    let clone_bytes = LIVE.load(Ordering::Relaxed) - base;
    drop(clone);
    println!(
        "Document clone (4 layers):        {:?}, {} KiB new heap",
        time(iters, || four.clone()),
        clone_bytes / 1024
    );

    // 10 snapshot steps that each change only a swatch.
    let mut doc = four.clone();
    let base = LIVE.load(Ordering::Relaxed);
    let mut h = History::new();
    let start = Instant::now();
    for i in 0..10u8 {
        h.begin_snapshot(&doc);
        doc.swatches.push(Rgb::new(i, i, i));
        h.commit(&doc);
    }
    let each = start.elapsed() / 10;
    let heap = LIVE.load(Ordering::Relaxed) - base;
    println!(
        "History snapshot (4 layers):      {:?} each; 10 steps hold {} KiB heap, memory_used() {} KiB",
        each,
        heap / 1024,
        h.memory_used() / 1024
    );
    black_box((h, doc));
}
