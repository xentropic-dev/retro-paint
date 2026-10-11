//! Flattening the layer stack into one canvas-sized RGBA image.
//!
//! See [`crate::blend`] for the color math. Image layers are resampled with
//! **bilinear** filtering (straight alpha, premultiplied during
//! interpolation) inside the transformed source rectangle, clamping to the
//! edge texels; canvas pixels whose center falls outside the rectangle get
//! nothing from the layer, so image edges are hard rather than fringed. At an
//! identity transform with integer translation, bilinear samples land exactly
//! on texel centers, so pixels pass through unchanged.
//!
//! Performance: the canvas is rendered row by row through a row-sized `f32`
//! accumulator, in horizontal bands on scoped threads for large canvases.
//! Shortcuts (a lone unmasked 100% paint layer copies bytes; an opaque
//! source over a transparent or opaque backdrop skips the general formula)
//! are exact, so the output is byte-identical to the plain per-pixel
//! composite kept in `tests/reference_flatten`.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use crate::blend::{BlendMode, byte, composite_fast, unit};
use crate::document::Document;
use crate::layer::{Layer, LayerKind, SourceHash, Transform};
use crate::raster::{GrayImage, RgbaImage, Size};

/// Decoded source images for image layers, keyed by hash. The caller loads
/// them (from disk or a project file); this crate never does I/O.
///
/// Each image sits behind an [`Arc`], so cloning the store copies only the
/// map of pointers: a render-worker snapshot and the editor's copy share the
/// full-resolution pixel buffers, and adding a source to one copy never
/// duplicates the others.
#[derive(Clone, Default, Debug)]
pub struct SourceStore {
    images: HashMap<SourceHash, Arc<RgbaImage>>,
}

impl SourceStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces the image for `hash`.
    pub fn insert(&mut self, hash: SourceHash, image: RgbaImage) {
        self.insert_arc(hash, Arc::new(image));
    }

    /// Adds or replaces the image for `hash`, sharing the caller's buffer.
    pub fn insert_arc(&mut self, hash: SourceHash, image: Arc<RgbaImage>) {
        self.images.insert(hash, image);
    }

    pub fn get(&self, hash: &SourceHash) -> Option<&RgbaImage> {
        self.images.get(hash).map(Arc::as_ref)
    }

    /// The shared handle for `hash`; cloning it never copies pixels.
    pub fn get_arc(&self, hash: &SourceHash) -> Option<&Arc<RgbaImage>> {
        self.images.get(hash)
    }

    pub fn contains(&self, hash: &SourceHash) -> bool {
        self.images.contains_key(hash)
    }

    /// Removes the image for `hash`. The pixels are copied only if another
    /// clone of the store still shares them.
    pub fn remove(&mut self, hash: &SourceHash) -> Option<RgbaImage> {
        self.images.remove(hash).map(Arc::unwrap_or_clone)
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    /// Hashes in the store, in no particular order.
    pub fn hashes(&self) -> impl Iterator<Item = &SourceHash> {
        self.images.keys()
    }
}

impl Document {
    /// Composites the visible layers bottom to top into a canvas-sized,
    /// straight-alpha RGBA image. The canvas starts fully transparent.
    /// Image layers whose source is missing from `sources` are skipped.
    pub fn flatten(&self, sources: &SourceStore) -> RgbaImage {
        flatten(self, sources)
    }
}

/// Most threads a single flatten uses; leaves cores for the UI and DOS
/// threads.
const MAX_THREADS: usize = 8;

/// Below this many layer-pixels a flatten runs on the calling thread;
/// spawning would cost more than it saves.
const PARALLEL_MIN_WORK: usize = 1 << 16;

/// See [`Document::flatten`].
///
/// Large canvases are split into horizontal bands rendered on scoped
/// threads. Every output pixel depends only on the layer pixels above it, and
/// each band runs exactly the same per-pixel operations in the same order,
/// so the result does not depend on the thread count.
pub fn flatten(doc: &Document, sources: &SourceStore) -> RgbaImage {
    let work = doc.canvas_size.area() * doc.layers().len().max(1);
    let threads = if work < PARALLEL_MIN_WORK {
        1
    } else {
        std::thread::available_parallelism().map_or(1, |n| n.get().min(MAX_THREADS))
    };
    flatten_with_threads(doc, sources, threads)
}

/// [`flatten`] with an explicit band/thread count (clamped to the canvas
/// height). The output is the same for every count; exposed for tests and
/// benchmarks.
#[doc(hidden)]
pub fn flatten_with_threads(doc: &Document, sources: &SourceStore, threads: usize) -> RgbaImage {
    let size = doc.canvas_size;
    let mut out = vec![0u8; size.area() * 4];
    let draws: Vec<Draw> = doc
        .layers()
        .iter()
        .filter_map(|l| Draw::new(l, sources, size))
        .collect();
    if !draws.is_empty() && size.area() > 0 {
        let rows = size.height as usize;
        let row_bytes = size.width as usize * 4;
        let bands = threads.clamp(1, rows);
        let band_rows = rows.div_ceil(bands);
        let mut chunks = out
            .chunks_mut(band_rows * row_bytes)
            .enumerate()
            .map(|(i, c)| (i * band_rows, c));
        let (first_y, first) = chunks.next().expect("non-empty canvas");
        std::thread::scope(|s| {
            for (y, chunk) in chunks {
                let draws = &draws;
                s.spawn(move || render_rows(draws, size, y, chunk));
            }
            render_rows(&draws, size, first_y, first);
        });
    }
    RgbaImage::from_raw(size, out).expect("sized from canvas")
}

/// Composites rows `y0..` (as many as fit in `out`) into `out`, one row at a
/// time through a row-sized `f32` accumulator.
fn render_rows(draws: &[Draw], canvas: Size, y0: usize, out: &mut [u8]) {
    let width = canvas.width as usize;
    let mut acc = vec![[0.0f32; 4]; width];
    for (r, row_out) in out.chunks_exact_mut(width * 4).enumerate() {
        let y = (y0 + r) as u32;
        let mut active = draws.iter().filter(|d| d.touches_row(y));
        if let (Some(only), None) = (active.next(), active.next())
            && let Some((xs, src)) = only.passthrough_row(y)
        {
            copy_row(&mut row_out.as_chunks_mut::<4>().0[xs], src);
            continue;
        }
        acc.fill([0.0; 4]);
        for d in draws {
            d.draw_row(&mut acc, y);
        }
        for (px, o) in acc.iter().zip(row_out.as_chunks_mut::<4>().0) {
            // `out` starts zeroed, so transparent pixels need no write.
            if px[3] > 0.0 {
                *o = px.map(byte);
            }
        }
    }
}

/// A lone unmasked 100% paint layer over the transparent canvas: the
/// composite gives back each pixel's own bytes (`byte((α·Cs)/α) = Cs` for
/// every 8-bit `Cs` and `α > 0`, checked exhaustively by the reference
/// tests), and fully transparent pixels stay zero.
fn copy_row(out: &mut [[u8; 4]], src: &[[u8; 4]]) {
    for (o, px) in out.iter_mut().zip(src) {
        if px[3] != 0 {
            *o = *px;
        }
    }
}

/// One visible layer, prepared for drawing row by row.
struct Draw<'a> {
    opacity: f32,
    mask: Option<&'a GrayImage>,
    blend: BlendMode,
    kind: DrawKind<'a>,
}

enum DrawKind<'a> {
    Paint {
        pixels: &'a RgbaImage,
        offset_x: i64,
        offset_y: i64,
        /// Canvas columns covered by the raster.
        xs: Range<usize>,
    },
    Image {
        src: &'a RgbaImage,
        map: InverseMap,
        /// Canvas pixel range `[x0, x1) × [y0, y1)` that can hit the image.
        bounds: (u32, u32, u32, u32),
    },
}

impl<'a> Draw<'a> {
    /// `None` if the layer cannot draw anything.
    fn new(layer: &'a Layer, sources: &'a SourceStore, canvas: Size) -> Option<Self> {
        if !layer.visible || layer.opacity == 0 {
            return None;
        }
        let kind = match &layer.kind {
            LayerKind::Paint(p) => {
                let lo = (p.offset_x as i64).max(0);
                let hi = (p.offset_x as i64 + p.pixels.width() as i64).min(canvas.width as i64);
                if hi <= lo {
                    return None;
                }
                DrawKind::Paint {
                    pixels: &p.pixels,
                    offset_x: p.offset_x as i64,
                    offset_y: p.offset_y as i64,
                    xs: lo as usize..hi as usize,
                }
            }
            LayerKind::Image(i) => {
                let src = sources.get(&i.source)?;
                let map = InverseMap::new(&i.transform, src.size())?;
                let bounds = map.canvas_bounds(canvas)?;
                DrawKind::Image { src, map, bounds }
            }
        };
        Some(Self {
            opacity: layer.opacity_unit(),
            mask: layer.mask.as_ref(),
            blend: layer.blend,
            kind,
        })
    }

    /// The mask values for canvas row `y`, if the mask reaches that row.
    /// Columns past the returned slice are unmasked.
    fn mask_row(&self, y: u32) -> &'a [u8] {
        match self.mask {
            Some(m) if y < m.size().height => {
                let w = m.size().width as usize;
                &m.as_raw()[y as usize * w..][..w]
            }
            _ => &[],
        }
    }

    /// Whether this layer can draw anything on canvas row `y`.
    fn touches_row(&self, y: u32) -> bool {
        match &self.kind {
            DrawKind::Paint {
                pixels, offset_y, ..
            } => (*offset_y..*offset_y + pixels.height() as i64).contains(&(y as i64)),
            DrawKind::Image {
                bounds: (_, y0, _, y1),
                ..
            } => (*y0..*y1).contains(&y),
        }
    }

    /// For a paint layer at 100% opacity with no mask on row `y`: the canvas
    /// columns it covers and its pixels there. Only call when the row
    /// touches this layer.
    fn passthrough_row(&self, y: u32) -> Option<(Range<usize>, &'a [[u8; 4]])> {
        match &self.kind {
            DrawKind::Paint {
                pixels,
                offset_x,
                offset_y,
                xs,
            } if self.opacity == 1.0 && self.mask_row(y).len() <= xs.start => {
                let ly = (y as i64 - offset_y) as usize;
                let lw = pixels.width() as usize;
                let lx0 = (xs.start as i64 - offset_x) as usize;
                let row = &pixels.as_raw()[ly * lw * 4..][..lw * 4];
                Some((xs.clone(), &row.as_chunks::<4>().0[lx0..lx0 + xs.len()]))
            }
            _ => None,
        }
    }

    /// Composites this layer into `acc`, the accumulator for canvas row `y`.
    fn draw_row(&self, acc: &mut [[f32; 4]], y: u32) {
        let mask = self.mask_row(y);
        // Effective alpha factor, computed exactly like `opacity * m` with
        // `m = 1.0` outside the mask.
        let factor = |x: usize| match mask.get(x) {
            Some(&m) => self.opacity * unit(m),
            None => self.opacity,
        };
        match &self.kind {
            DrawKind::Paint {
                pixels,
                offset_x,
                offset_y,
                xs,
            } => {
                let ly = y as i64 - offset_y;
                if ly < 0 || ly >= pixels.height() as i64 {
                    return;
                }
                let lw = pixels.width() as usize;
                let lx0 = (xs.start as i64 - offset_x) as usize;
                let row = &pixels.as_raw()[ly as usize * lw * 4..][..lw * 4];
                let row = &row.as_chunks::<4>().0[lx0..lx0 + xs.len()];
                let acc = &mut acc[xs.clone()];
                if mask.len() <= xs.start {
                    // Unmasked row: one factor for every pixel, and at 100%
                    // opacity `α × 1.0` is `α`, so skip the multiply.
                    let opacity = self.opacity;
                    for (dst, px) in acc.iter_mut().zip(row) {
                        if px[3] == 0 {
                            continue;
                        }
                        let mut src = px.map(unit);
                        if opacity != 1.0 {
                            src[3] *= opacity;
                        }
                        composite_fast(self.blend, dst, src);
                    }
                } else {
                    for ((x, dst), px) in xs.clone().zip(acc).zip(row) {
                        if px[3] == 0 {
                            continue;
                        }
                        let mut src = px.map(unit);
                        src[3] *= factor(x);
                        composite_fast(self.blend, dst, src);
                    }
                }
            }
            DrawKind::Image {
                src,
                map,
                bounds: (x0, y0, x1, y1),
            } => {
                if y < *y0 || y >= *y1 {
                    return;
                }
                let py = y as f32 + 0.5;
                for x in *x0..*x1 {
                    let (sx, sy) = map.source_point(x as f32 + 0.5, py);
                    if let Some(mut px) = sample_bilinear(src, sx, sy)
                        && px[3] > 0.0
                    {
                        px[3] *= factor(x as usize);
                        composite_fast(self.blend, &mut acc[x as usize], px);
                    }
                }
            }
        }
    }
}

/// Canvas → source mapping for one image layer (also used to place a
/// [`Float`](crate::Float)).
pub(crate) struct InverseMap {
    src_w: f32,
    src_h: f32,
    center_x: f32,
    center_y: f32,
    cos: f32,
    sin: f32,
    scale_x: f32,
    scale_y: f32,
}

impl InverseMap {
    pub(crate) fn new(t: &Transform, src: Size) -> Option<Self> {
        let ok = |v: f32| v.is_finite() && v.abs() > 1e-6;
        if !ok(t.scale_x) || !ok(t.scale_y) || src.area() == 0 {
            return None;
        }
        let (src_w, src_h) = (src.width as f32, src.height as f32);
        Some(Self {
            src_w,
            src_h,
            center_x: t.translate_x + src_w * t.scale_x / 2.0,
            center_y: t.translate_y + src_h * t.scale_y / 2.0,
            cos: t.rotation.cos(),
            sin: t.rotation.sin(),
            scale_x: t.scale_x,
            scale_y: t.scale_y,
        })
    }

    /// Source point (continuous, pixel centers at `i + 0.5`) for a canvas
    /// point.
    pub(crate) fn source_point(&self, px: f32, py: f32) -> (f32, f32) {
        let dx = px - self.center_x;
        let dy = py - self.center_y;
        // R(−θ) · d
        let rx = self.cos * dx + self.sin * dy;
        let ry = -self.sin * dx + self.cos * dy;
        (
            self.src_w / 2.0 + rx / self.scale_x,
            self.src_h / 2.0 + ry / self.scale_y,
        )
    }

    /// Canvas positions of the source rectangle's corners: top-left,
    /// top-right, bottom-right, bottom-left (in source orientation).
    pub(crate) fn corners(&self) -> [[f32; 2]; 4] {
        let hw = self.src_w * self.scale_x / 2.0;
        let hh = self.src_h * self.scale_y / 2.0;
        [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)].map(|(ox, oy)| {
            // R(θ) · offset
            [
                self.center_x + self.cos * ox - self.sin * oy,
                self.center_y + self.sin * ox + self.cos * oy,
            ]
        })
    }

    /// Axis-aligned extent of [`corners`](Self::corners):
    /// `(min_x, min_y, max_x, max_y)`.
    pub(crate) fn extent(&self) -> (f32, f32, f32, f32) {
        let mut min = (f32::INFINITY, f32::INFINITY);
        let mut max = (f32::NEG_INFINITY, f32::NEG_INFINITY);
        for [x, y] in self.corners() {
            min = (min.0.min(x), min.1.min(y));
            max = (max.0.max(x), max.1.max(y));
        }
        (min.0, min.1, max.0, max.1)
    }

    /// Canvas pixel range `[x0, x1) × [y0, y1)` that can hit the image.
    fn canvas_bounds(&self, canvas: Size) -> Option<(u32, u32, u32, u32)> {
        let (min_x, min_y, max_x, max_y) = self.extent();
        let (min, max) = ((min_x, min_y), (max_x, max_y));
        let clip = |v: f32, hi: u32| (v.max(0.0) as u32).min(hi);
        let x0 = clip(min.0.floor() - 1.0, canvas.width);
        let y0 = clip(min.1.floor() - 1.0, canvas.height);
        let x1 = clip(max.0.ceil() + 1.0, canvas.width);
        let y1 = clip(max.1.ceil() + 1.0, canvas.height);
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }
}

/// Bilinear sample at a continuous source point, or `None` outside the
/// source rectangle. Returns straight alpha in `0.0..=1.0`.
pub(crate) fn sample_bilinear(src: &RgbaImage, sx: f32, sy: f32) -> Option<[f32; 4]> {
    let (w, h) = (src.width() as f32, src.height() as f32);
    if !(sx >= 0.0 && sy >= 0.0 && sx < w && sy < h) {
        return None;
    }
    let fx = sx - 0.5;
    let fy = sy - 0.5;
    let x0 = fx.floor();
    let y0 = fy.floor();
    let tx = fx - x0;
    let ty = fy - y0;
    let max_x = src.width() as i64 - 1;
    let max_y = src.height() as i64 - 1;
    let data = src.as_raw();
    let texel = |x: i64, y: i64| {
        let x = x.clamp(0, max_x) as usize;
        let y = y.clamp(0, max_y) as usize;
        let i = (y * src.width() as usize + x) * 4;
        let a = unit(data[i + 3]);
        [
            unit(data[i]) * a,
            unit(data[i + 1]) * a,
            unit(data[i + 2]) * a,
            a,
        ]
    };
    let (x0, y0) = (x0 as i64, y0 as i64);
    let taps = [
        (texel(x0, y0), (1.0 - tx) * (1.0 - ty)),
        (texel(x0 + 1, y0), tx * (1.0 - ty)),
        (texel(x0, y0 + 1), (1.0 - tx) * ty),
        (texel(x0 + 1, y0 + 1), tx * ty),
    ];
    let mut p = [0.0f32; 4];
    for (t, wgt) in taps {
        if wgt == 0.0 {
            continue;
        }
        for c in 0..4 {
            p[c] += t[c] * wgt;
        }
    }
    if p[3] <= 0.0 {
        return Some([0.0; 4]);
    }
    Some([p[0] / p[3], p[1] / p[3], p[2] / p[3], p[3].min(1.0)])
}
