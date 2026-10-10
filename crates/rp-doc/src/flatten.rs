//! Flattening the layer stack into one canvas-sized RGBA image.
//!
//! See [`crate::blend`] for the color math. Image layers are resampled with
//! **bilinear** filtering (straight alpha, premultiplied during
//! interpolation) inside the transformed source rectangle, clamping to the
//! edge texels; canvas pixels whose center falls outside the rectangle get
//! nothing from the layer, so image edges are hard rather than fringed. At an
//! identity transform with integer translation, bilinear samples land exactly
//! on texel centers, so pixels pass through unchanged.

use std::collections::HashMap;

use crate::blend::{BlendMode, byte, unit};
use crate::document::Document;
use crate::layer::{ImageLayer, LayerKind, PaintLayer, SourceHash, Transform};
use crate::raster::{GrayImage, RgbaImage, Size};

/// Decoded source images for image layers, keyed by hash. The caller loads
/// them (from disk or a project file); this crate never does I/O.
#[derive(Clone, Default, Debug)]
pub struct SourceStore {
    images: HashMap<SourceHash, RgbaImage>,
}

impl SourceStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces the image for `hash`.
    pub fn insert(&mut self, hash: SourceHash, image: RgbaImage) {
        self.images.insert(hash, image);
    }

    pub fn get(&self, hash: &SourceHash) -> Option<&RgbaImage> {
        self.images.get(hash)
    }

    pub fn contains(&self, hash: &SourceHash) -> bool {
        self.images.contains_key(hash)
    }

    pub fn remove(&mut self, hash: &SourceHash) -> Option<RgbaImage> {
        self.images.remove(hash)
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

/// See [`Document::flatten`].
pub fn flatten(doc: &Document, sources: &SourceStore) -> RgbaImage {
    let size = doc.canvas_size;
    let mut acc = vec![[0.0f32; 4]; size.area()];
    for layer in doc.layers() {
        if !layer.visible || layer.opacity == 0 {
            continue;
        }
        let ctx = LayerCtx {
            canvas: size,
            opacity: layer.opacity_unit(),
            mask: layer.mask.as_ref(),
            blend: layer.blend,
        };
        match &layer.kind {
            LayerKind::Paint(p) => draw_paint(&mut acc, &ctx, p),
            LayerKind::Image(i) => {
                if let Some(src) = sources.get(&i.source) {
                    draw_image(&mut acc, &ctx, i, src);
                }
            }
        }
    }
    let mut out = Vec::with_capacity(size.area() * 4);
    for px in &acc {
        if px[3] <= 0.0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            out.extend(px.map(byte));
        }
    }
    RgbaImage::from_raw(size, out).expect("sized from canvas")
}

struct LayerCtx<'a> {
    canvas: Size,
    opacity: f32,
    mask: Option<&'a GrayImage>,
    blend: BlendMode,
}

impl LayerCtx<'_> {
    fn put(&self, acc: &mut [[f32; 4]], x: u32, y: u32, mut src: [f32; 4]) {
        let m = match self.mask {
            Some(m) if x < m.size().width && y < m.size().height => unit(m.get(x, y)),
            _ => 1.0,
        };
        src[3] *= self.opacity * m;
        let i = y as usize * self.canvas.width as usize + x as usize;
        self.blend.composite(&mut acc[i], src);
    }
}

fn draw_paint(acc: &mut [[f32; 4]], ctx: &LayerCtx, p: &PaintLayer) {
    let ls = p.pixels.size();
    // Canvas range covered by the raster, clipped to the canvas.
    let span = |offset: i32, len: u32, canvas: u32| {
        let lo = (offset as i64).max(0);
        let hi = (offset as i64 + len as i64).min(canvas as i64);
        lo..hi.max(lo)
    };
    let data = p.pixels.as_raw();
    for y in span(p.offset_y, ls.height, ctx.canvas.height) {
        let ly = (y - p.offset_y as i64) as usize;
        for x in span(p.offset_x, ls.width, ctx.canvas.width) {
            let lx = (x - p.offset_x as i64) as usize;
            let i = (ly * ls.width as usize + lx) * 4;
            let px = [data[i], data[i + 1], data[i + 2], data[i + 3]];
            if px[3] == 0 {
                continue;
            }
            ctx.put(acc, x as u32, y as u32, px.map(unit));
        }
    }
}

fn draw_image(acc: &mut [[f32; 4]], ctx: &LayerCtx, layer: &ImageLayer, src: &RgbaImage) {
    let Some(map) = InverseMap::new(&layer.transform, src.size()) else {
        return;
    };
    let Some((x0, y0, x1, y1)) = map.canvas_bounds(ctx.canvas) else {
        return;
    };
    for y in y0..y1 {
        for x in x0..x1 {
            let (sx, sy) = map.source_point(x as f32 + 0.5, y as f32 + 0.5);
            if let Some(px) = sample_bilinear(src, sx, sy)
                && px[3] > 0.0
            {
                ctx.put(acc, x, y, px);
            }
        }
    }
}

/// Canvas → source mapping for one image layer.
struct InverseMap {
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
    fn new(t: &Transform, src: Size) -> Option<Self> {
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
    fn source_point(&self, px: f32, py: f32) -> (f32, f32) {
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

    /// Canvas pixel range `[x0, x1) × [y0, y1)` that can hit the image.
    fn canvas_bounds(&self, canvas: Size) -> Option<(u32, u32, u32, u32)> {
        let hw = self.src_w * self.scale_x / 2.0;
        let hh = self.src_h * self.scale_y / 2.0;
        let mut min = (f32::INFINITY, f32::INFINITY);
        let mut max = (f32::NEG_INFINITY, f32::NEG_INFINITY);
        for (ox, oy) in [(-hw, -hh), (hw, -hh), (-hw, hh), (hw, hh)] {
            // R(θ) · offset
            let x = self.center_x + self.cos * ox - self.sin * oy;
            let y = self.center_y + self.sin * ox + self.cos * oy;
            min = (min.0.min(x), min.1.min(y));
            max = (max.0.max(x), max.1.max(y));
        }
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
fn sample_bilinear(src: &RgbaImage, sx: f32, sy: f32) -> Option<[f32; 4]> {
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
