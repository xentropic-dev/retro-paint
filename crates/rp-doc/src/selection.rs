//! Lasso selections and floating pieces.
//!
//! - [`Selection`]: canvas-sized 8-bit coverage (255 selected, 0 not),
//!   built from closed polygons with [`SelectMode`] Replace / Add /
//!   Subtract.
//! - [`Float`]: the selected pixels of one paint layer, lifted out of it,
//!   placed by a [`Transform`], then committed back into the same layer or
//!   cancelled.
//!
//! # Decisions
//!
//! - **Polygons are the only shape.** A freehand lasso is its point list, a
//!   polygonal lasso its clicked vertices, a rectangle four corners. Points
//!   are canvas coordinates (`f32`, pixel `(x, y)` covers
//!   `[x, x+1) × [y, y+1)`); the path closes itself from the last point to
//!   the first. Fewer than three points, or any non-finite coordinate,
//!   selects nothing.
//! - **Even-odd fill, 4×4 supersampled.** Each pixel has 16 samples at
//!   `(i + 0.5) / 4` in both axes; a sample is inside if a horizontal ray
//!   crosses the path an odd number of times (edges half-open in y, spans
//!   half-open in x). Coverage is `round(255 · n / 16)`, so pixel-aligned
//!   edges are hard and others are antialiased. Winding direction does not
//!   matter; a self-crossing path selects both lobes.
//! - **Combine modes are min/max.** Add keeps `max(old, new)`, Subtract
//!   `min(old, 255 − new)`, Replace drops the old selection. Both are exact
//!   and idempotent (adding the same shape twice changes nothing).
//! - **Bounds-local work.** A polygon is rasterized only inside its own
//!   bounding box (clipped to the canvas) with an active-edge scan, and
//!   combining touches only that box plus, for Replace / Subtract, the old
//!   [`bounds`](Selection::bounds). Bounds are kept tight (the smallest
//!   rectangle containing every non-zero pixel). The coverage buffer is
//!   copy-on-write ([`GrayImage`]), so cloning a selection is cheap.
//! - **Selection changes are not undo steps** (see [`history`](crate::history)).
//!   A canvas resize must drop the selection; the editor makes a new one.
//! - **Lift splits each pixel by coverage.** With coverage `c` and layer
//!   alpha `a`, the piece gets `round(a·c/255)` and the layer keeps the
//!   rest, so the two alphas add up to the original. Fully covered pixels
//!   leave a transparent hole, pixels at a soft edge stay partly. Lift works
//!   in the layer's own coordinates (respecting its integer offset) and is
//!   clipped to the layer's raster; the piece's top-left canvas position is
//!   [`Float::origin`]. The layer's mask is not touched or applied.
//! - **Placement reuses the image-layer [`Transform`].** The piece is placed
//!   exactly as an image layer with the piece as its source would be:
//!   scale and rotation about the piece's center, `translate` the top-left
//!   of the scaled unrotated piece. [`Float::home_transform`] (`at(origin)`)
//!   puts it back where it came from. Another rotation pivot is a change of
//!   `translate`, which the editor computes.
//! - **Commit samples like flatten.** Every layer pixel whose center maps
//!   into the piece gets the same bilinear sample flatten uses for image
//!   layers, composited source-over (normal blend) onto the layer's
//!   current pixel. Pixels that land outside the layer's raster are dropped.
//! - **A float still at home commits as a no-op.** Source-over does not undo
//!   the coverage split at soft edges (two half-alpha halves composite to
//!   75 %, not 100 %), so committing at [`Float::home_transform`] restores
//!   the original bytes exactly, like [`Float::cancel`]. Moved floats
//!   composite normally.
//! - **Cancel restores exactly.** The float keeps the lifted rectangle's
//!   original pixels (only that rectangle) and writes them back.
//! - **Previewing a float.** During a transform the hole is in the layer and
//!   the piece lives in the float. To show it, insert
//!   [`Float::preview_layer`] just above the layer in the render snapshot
//!   and put [`Float::piece`] in the snapshot's [`SourceStore`] under
//!   [`Float::preview_source`]. It copies the layer's visibility, opacity,
//!   blend and mask, so for a normal 100 % layer it matches the committed
//!   result; for other blends and opacities it is a close preview.
//!
//! # Using it from the editor
//!
//! ```text
//! lasso drag end:  selection.combine_polygon(mode, &points)   // Shift: Add, Alt: Subtract
//! Ctrl+A / Ctrl+D: selection.select_all() / selection.clear()
//! transform start: history.begin_paint(&doc, layer_id);
//!                  float = Float::lift(doc.layer_mut(layer_id)?, &selection)?
//! handle drag:     float.set_transform(t)                     // corners() for handles
//! Enter:           let hint = float.commit(doc.layer_mut(id)?)?;
//!                  history.commit_within(&doc, hint)          // one undo step, none if unchanged
//! Escape:          float.cancel(doc.layer_mut(id)?)?; history.cancel()
//! ```
//!
//! While a float exists the editor must not change that layer's raster
//! size or offset.
//!
//! [`SourceStore`]: crate::SourceStore

use crate::blend::{BlendMode, byte, composite_fast, unit};
use crate::flatten::{InverseMap, sample_bilinear};
use crate::history::Rect;
use crate::layer::{EditError, Layer, LayerId, LayerKind, SourceHash, Transform};
use crate::raster::{GrayImage, RgbaImage, Size};

/// Samples per pixel along each axis.
const SUB: usize = 4;

/// How a new shape combines with the current selection.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum SelectMode {
    /// The shape becomes the selection.
    #[default]
    Replace,
    /// Union: `max(old, new)`.
    Add,
    /// Difference: `min(old, 255 − new)`.
    Subtract,
}

/// Canvas-sized selection coverage. See the [module docs](self).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Selection {
    coverage: GrayImage,
    /// Tight bounds of the non-zero coverage.
    bounds: Option<Rect>,
}

impl Selection {
    /// Nothing selected.
    pub fn new(canvas: Size) -> Self {
        Self {
            coverage: GrayImage::filled(canvas, 0),
            bounds: None,
        }
    }

    /// The whole canvas selected.
    pub fn all(canvas: Size) -> Self {
        let mut s = Self::new(canvas);
        s.select_all();
        s
    }

    /// The polygon `points` (closed, even-odd) selected.
    pub fn from_polygon(canvas: Size, points: &[[f32; 2]]) -> Self {
        let mut s = Self::new(canvas);
        s.combine_polygon(SelectMode::Replace, points);
        s
    }

    /// Canvas size.
    pub fn size(&self) -> Size {
        self.coverage.size()
    }

    /// Coverage per canvas pixel, 0..=255.
    pub fn coverage(&self) -> &GrayImage {
        &self.coverage
    }

    /// Coverage at `(x, y)`. Panics if outside the canvas.
    pub fn get(&self, x: u32, y: u32) -> u8 {
        self.coverage.get(x, y)
    }

    /// The smallest rectangle containing every selected pixel, or `None`
    /// if nothing is selected.
    pub fn bounds(&self) -> Option<Rect> {
        self.bounds
    }

    pub fn is_empty(&self) -> bool {
        self.bounds.is_none()
    }

    /// Deselects everything (touches only the old bounds).
    pub fn clear(&mut self) {
        if let Some(b) = self.bounds.take() {
            fill_rect(&mut self.coverage, b, 0);
        }
    }

    /// Selects the whole canvas.
    pub fn select_all(&mut self) {
        let size = self.size();
        if size.area() == 0 {
            return;
        }
        self.coverage.as_raw_mut().fill(255);
        self.bounds = Some(Rect::new(0, 0, size.width, size.height));
    }

    /// Combines the closed polygon `points` with the selection.
    pub fn combine_polygon(&mut self, mode: SelectMode, points: &[[f32; 2]]) {
        let shape = rasterize(points, self.size());
        match mode {
            SelectMode::Replace => {
                self.clear();
                if let Some((rect, buf)) = shape {
                    self.write(rect, &buf, |_, n| n);
                    self.bounds = tight_bounds(&self.coverage, rect);
                }
            }
            SelectMode::Add => {
                if let Some((rect, buf)) = shape {
                    self.write(rect, &buf, u8::max);
                    self.bounds = union(self.bounds, tight_bounds(&self.coverage, rect));
                }
            }
            SelectMode::Subtract => {
                if let (Some((rect, buf)), Some(old)) = (shape, self.bounds) {
                    self.write(rect, &buf, |o, n| o.min(255 - n));
                    self.bounds = tight_bounds(&self.coverage, old);
                }
            }
        }
    }

    /// Writes `f(old, new)` for every pixel of `rect`, `buf` being the new
    /// coverage of `rect` row by row.
    fn write(&mut self, rect: Rect, buf: &[u8], f: impl Fn(u8, u8) -> u8) {
        let w = self.size().width as usize;
        let data = self.coverage.as_raw_mut();
        for (r, src) in buf.chunks_exact(rect.width as usize).enumerate() {
            let start = (rect.y as usize + r) * w + rect.x as usize;
            for (o, &n) in data[start..start + src.len()].iter_mut().zip(src) {
                *o = f(*o, n);
            }
        }
    }
}

fn fill_rect(img: &mut GrayImage, r: Rect, v: u8) {
    let w = img.size().width as usize;
    let data = img.as_raw_mut();
    for y in r.y..r.y + r.height {
        let start = y as usize * w + r.x as usize;
        data[start..start + r.width as usize].fill(v);
    }
}

fn union(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let x0 = a.x.min(b.x);
            let y0 = a.y.min(b.y);
            let x1 = (a.x + a.width).max(b.x + b.width);
            let y1 = (a.y + a.height).max(b.y + b.height);
            Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
        }
        (a, None) => a,
        (None, b) => b,
    }
}

/// Tight bounds of the non-zero pixels of `img` inside `within`.
fn tight_bounds(img: &GrayImage, within: Rect) -> Option<Rect> {
    let w = img.size().width as usize;
    let data = img.as_raw();
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for y in within.y..within.y + within.height {
        let start = y as usize * w + within.x as usize;
        let row = &data[start..start + within.width as usize];
        let Some(first) = row.iter().position(|&c| c != 0) else {
            continue;
        };
        let last = row.iter().rposition(|&c| c != 0).expect("has a non-zero");
        x0 = x0.min(within.x + first as u32);
        x1 = x1.max(within.x + last as u32 + 1);
        y0 = y0.min(y);
        y1 = y + 1;
    }
    (x0 < x1).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
}

/// One polygon edge, top to bottom.
struct Edge {
    y_top: f32,
    y_bot: f32,
    x_top: f32,
    dxdy: f32,
}

/// Rasterizes the closed even-odd polygon into its canvas-clipped bounding
/// box: the box and its coverage, row by row. `None` if it covers no pixel
/// of the canvas.
fn rasterize(points: &[[f32; 2]], canvas: Size) -> Option<(Rect, Vec<u8>)> {
    if points.len() < 3 || points.iter().flatten().any(|v| !v.is_finite()) {
        return None;
    }
    let (mut min_x, mut min_y) = (f32::INFINITY, f32::INFINITY);
    let (mut max_x, mut max_y) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for &[x, y] in points {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    let clip = |v: f32, hi: u32| v.clamp(0.0, hi as f32) as u32;
    let x0 = clip(min_x.floor(), canvas.width);
    let y0 = clip(min_y.floor(), canvas.height);
    let x1 = clip(max_x.ceil(), canvas.width);
    let y1 = clip(max_y.ceil(), canvas.height);
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let rect = Rect::new(x0, y0, x1 - x0, y1 - y0);
    let (w, h) = (rect.width as usize, rect.height as usize);

    let mut edges: Vec<Edge> = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .filter(|(p, q)| p[1] != q[1])
        .map(|(&p, &q)| {
            let (a, b) = if p[1] < q[1] { (p, q) } else { (q, p) };
            Edge {
                y_top: a[1],
                y_bot: b[1],
                x_top: a[0],
                dxdy: (b[0] - a[0]) / (b[1] - a[1]),
            }
        })
        .collect();
    edges.sort_by(|a, b| a.y_top.total_cmp(&b.y_top));

    let mut out = vec![0u8; w * h];
    let mut counts = vec![0u8; w];
    let mut active: Vec<usize> = Vec::new();
    let mut next = 0;
    let mut xs: Vec<f32> = Vec::new();
    let samples = (w * SUB) as i64;
    for row in 0..h {
        counts.fill(0);
        for j in 0..SUB {
            let sy = y0 as f32 + row as f32 + (j as f32 + 0.5) / SUB as f32;
            while next < edges.len() && edges[next].y_top <= sy {
                active.push(next);
                next += 1;
            }
            active.retain(|&i| edges[i].y_bot > sy);
            xs.clear();
            xs.extend(
                active
                    .iter()
                    .map(|&i| &edges[i])
                    .filter(|e| e.y_top <= sy)
                    .map(|e| e.x_top + (sy - e.y_top) * e.dxdy),
            );
            xs.sort_by(f32::total_cmp);
            for &[xa, xb] in xs.as_chunks::<2>().0 {
                // Samples at x0 + (s + 0.5) / SUB inside [xa, xb).
                let first = |x: f32| {
                    ((((x - x0 as f32) * SUB as f32) - 0.5).ceil() as i64).clamp(0, samples)
                        as usize
                };
                let (s0, s1) = (first(xa), first(xb));
                add_span(&mut counts, s0, s1);
            }
        }
        let full = (SUB * SUB) as u32;
        for (o, &n) in out[row * w..][..w].iter_mut().zip(&counts) {
            *o = ((n as u32 * 255 + full / 2) / full) as u8;
        }
    }
    Some((rect, out))
}

/// Adds the samples `s0..s1` (sample indices along the row) to their
/// pixels' counts.
fn add_span(counts: &mut [u8], s0: usize, s1: usize) {
    if s0 >= s1 {
        return;
    }
    let (p0, p1) = (s0 / SUB, (s1 - 1) / SUB);
    if p0 == p1 {
        counts[p0] += (s1 - s0) as u8;
        return;
    }
    counts[p0] += (SUB - s0 % SUB) as u8;
    for c in &mut counts[p0 + 1..p1] {
        *c += SUB as u8;
    }
    counts[p1] += (s1 - p1 * SUB) as u8;
}

/// Selected pixels lifted out of one paint layer, placed by a transform
/// until committed back or cancelled. See the [module docs](self).
#[derive(Clone, PartialEq, Debug)]
pub struct Float {
    layer: LayerId,
    piece: RgbaImage,
    /// Canvas position of the piece's top-left pixel at lift time.
    origin: (i32, i32),
    /// The lifted rectangle, in the layer raster's own coordinates.
    rect: Rect,
    /// The layer's pixels in `rect` before the lift.
    original: RgbaImage,
    transform: Transform,
}

impl Float {
    /// Lifts the pixels of `layer` under `selection` into a new float,
    /// leaving the uncovered part in the layer. `Ok(None)` if the selection
    /// does not overlap the layer's raster. Refused if the layer's pixels
    /// are locked or it is not a paint layer.
    pub fn lift(layer: &mut Layer, selection: &Selection) -> Result<Option<Float>, EditError> {
        let id = layer.id();
        let (ox, oy) = match &layer.kind {
            LayerKind::Paint(p) => (p.offset_x as i64, p.offset_y as i64),
            LayerKind::Image(_) => (0, 0),
        };
        let pixels = layer.pixels_mut()?;
        let Some(b) = selection.bounds() else {
            return Ok(None);
        };
        // Selection bounds in layer coordinates, clipped to the raster.
        let lx0 = (b.x as i64 - ox).max(0);
        let ly0 = (b.y as i64 - oy).max(0);
        let lx1 = (b.x as i64 + b.width as i64 - ox).min(pixels.width() as i64);
        let ly1 = (b.y as i64 + b.height as i64 - oy).min(pixels.height() as i64);
        if lx0 >= lx1 || ly0 >= ly1 {
            return Ok(None);
        }
        let rect = Rect::new(
            lx0 as u32,
            ly0 as u32,
            (lx1 - lx0) as u32,
            (ly1 - ly0) as u32,
        );
        let (rw, rh) = (rect.width as usize, rect.height as usize);
        let mut piece = vec![0u8; rw * rh * 4];
        let mut original = Vec::with_capacity(rw * rh * 4);
        let lw = pixels.width() as usize;
        let cov = selection.coverage();
        let cw = cov.size().width as usize;
        let cov = cov.as_raw();
        let data = pixels.as_raw_mut();
        for r in 0..rh {
            let ly = rect.y as usize + r;
            let row = &mut data[(ly * lw + rect.x as usize) * 4..][..rw * 4];
            original.extend_from_slice(row);
            let cy = (ly as i64 + oy) as usize;
            let cx = (rect.x as i64 + ox) as usize;
            let cov_row = &cov[cy * cw + cx..][..rw];
            let piece_row = &mut piece[r * rw * 4..][..rw * 4];
            for ((px, out), &c) in row
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(piece_row.as_chunks_mut::<4>().0)
                .zip(cov_row)
            {
                if c == 0 || px[3] == 0 {
                    continue;
                }
                let a = px[3] as u32;
                let lifted = ((a * c as u32 + 127) / 255) as u8;
                if lifted == 0 {
                    continue;
                }
                *out = [px[0], px[1], px[2], lifted];
                let kept = px[3] - lifted;
                *px = if kept == 0 {
                    [0; 4]
                } else {
                    [px[0], px[1], px[2], kept]
                };
            }
        }
        let size = Size::new(rect.width, rect.height);
        let origin = ((rect.x as i64 + ox) as i32, (rect.y as i64 + oy) as i32);
        Ok(Some(Float {
            layer: id,
            piece: RgbaImage::from_raw(size, piece).expect("sized from rect"),
            origin,
            rect,
            original: RgbaImage::from_raw(size, original).expect("sized from rect"),
            transform: Transform::at(origin.0 as f32, origin.1 as f32),
        }))
    }

    /// The layer the pixels came from.
    pub fn layer(&self) -> LayerId {
        self.layer
    }

    /// The lifted pixels (straight alpha), unplaced.
    pub fn piece(&self) -> &RgbaImage {
        &self.piece
    }

    /// Canvas position of the piece's top-left pixel where it was lifted.
    pub fn origin(&self) -> (i32, i32) {
        self.origin
    }

    /// The placement where the piece was lifted: `Transform::at(origin)`.
    pub fn home_transform(&self) -> Transform {
        Transform::at(self.origin.0 as f32, self.origin.1 as f32)
    }

    /// Whether the piece is still exactly where it was lifted.
    pub fn is_home(&self) -> bool {
        self.transform == self.home_transform()
    }

    /// The current placement (image-layer [`Transform`] semantics).
    pub fn transform(&self) -> Transform {
        self.transform
    }

    pub fn set_transform(&mut self, transform: Transform) {
        self.transform = transform;
    }

    /// Canvas positions of the placed piece's corners, for handles:
    /// top-left, top-right, bottom-right, bottom-left of the piece. A
    /// zero scale collapses them onto the center.
    pub fn corners(&self) -> [[f32; 2]; 4] {
        match InverseMap::new(&self.transform, self.piece.size()) {
            Some(m) => m.corners(),
            None => {
                let t = &self.transform;
                let c = [
                    t.translate_x + self.piece.width() as f32 * t.scale_x / 2.0,
                    t.translate_y + self.piece.height() as f32 * t.scale_y / 2.0,
                ];
                [c; 4]
            }
        }
    }

    /// Canvas pixels (clipped to `canvas`) whose centers fall inside the
    /// axis-aligned extent of the placed piece: what to redraw. Conservative
    /// for rotated pieces. `None` if it misses the canvas or the scale is
    /// degenerate.
    pub fn placed_bounds(&self, canvas: Size) -> Option<Rect> {
        let (x0, y0, x1, y1) = self.placed_range()?;
        let x0 = x0.clamp(0, canvas.width as i64);
        let y0 = y0.clamp(0, canvas.height as i64);
        let x1 = x1.clamp(0, canvas.width as i64);
        let y1 = y1.clamp(0, canvas.height as i64);
        (x0 < x1 && y0 < y1)
            .then(|| Rect::new(x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32))
    }

    /// Unclipped canvas pixel range `[x0, x1) × [y0, y1)` whose centers lie
    /// in the placed extent.
    fn placed_range(&self) -> Option<(i64, i64, i64, i64)> {
        let map = InverseMap::new(&self.transform, self.piece.size())?;
        let (min_x, min_y, max_x, max_y) = map.extent();
        let px = |v: f32| (v - 0.5).ceil() as i64;
        Some((px(min_x), px(min_y), px(max_x), px(max_y)))
    }

    /// The source hash [`preview_layer`](Self::preview_layer) reads; put
    /// [`piece`](Self::piece) under it in the render snapshot's store.
    pub fn preview_source() -> SourceHash {
        SourceHash::new("float-preview")
    }

    /// An image layer that draws the placed piece with `base`'s visibility,
    /// opacity, blend and mask. Insert it just above `base` in a render
    /// snapshot (never in the real document).
    pub fn preview_layer(&self, base: &Layer) -> Layer {
        let mut l = Layer::image(
            format!("{} (floating)", base.name),
            Self::preview_source(),
            self.transform,
        );
        l.visible = base.visible;
        l.opacity = base.opacity;
        l.blend = base.blend;
        l.mask = base.mask.clone();
        l
    }

    /// Stamps the placed piece into `layer` (source-over), which must be the
    /// layer it was lifted from. At home this restores the original bytes
    /// exactly. Returns the touched rectangle in the layer raster's own
    /// coordinates (the lifted area plus where the piece landed), the hint
    /// for [`History::commit_within`](crate::History::commit_within).
    pub fn commit(self, layer: &mut Layer) -> Result<Option<Rect>, EditError> {
        debug_assert_eq!(layer.id(), self.layer, "float committed to another layer");
        if self.is_home() {
            self.restore(layer)?;
            return Ok(Some(self.rect));
        }
        let (ox, oy) = match &layer.kind {
            LayerKind::Paint(p) => (p.offset_x as i64, p.offset_y as i64),
            LayerKind::Image(_) => (0, 0),
        };
        let pixels = layer.pixels_mut()?;
        let mut touched = Some(self.rect);
        let (Some(map), Some((cx0, cy0, cx1, cy1))) = (
            InverseMap::new(&self.transform, self.piece.size()),
            self.placed_range(),
        ) else {
            return Ok(touched);
        };
        // Placed range in layer coordinates, one pixel of slack for
        // rounding, clipped to the raster.
        let (lw, lh) = (pixels.width() as i64, pixels.height() as i64);
        let lx0 = (cx0 - 1 - ox).clamp(0, lw);
        let ly0 = (cy0 - 1 - oy).clamp(0, lh);
        let lx1 = (cx1 + 1 - ox).clamp(0, lw);
        let ly1 = (cy1 + 1 - oy).clamp(0, lh);
        let (mut tx0, mut ty0, mut tx1, mut ty1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
        let data = pixels.as_raw_mut();
        for ly in ly0..ly1 {
            let py = (ly + oy) as f32 + 0.5;
            let row = &mut data[(ly * lw * 4) as usize..][..(lw * 4) as usize];
            let row = row.as_chunks_mut::<4>().0;
            for lx in lx0..lx1 {
                let (sx, sy) = map.source_point((lx + ox) as f32 + 0.5, py);
                let Some(src) = sample_bilinear(&self.piece, sx, sy) else {
                    continue;
                };
                (tx0, ty0) = (tx0.min(lx), ty0.min(ly));
                (tx1, ty1) = (tx1.max(lx + 1), ty1.max(ly + 1));
                if src[3] <= 0.0 {
                    continue;
                }
                let px = &mut row[lx as usize];
                let mut dst = px.map(unit);
                composite_fast(BlendMode::Normal, &mut dst, src);
                *px = dst.map(byte);
            }
        }
        if tx0 < tx1 {
            let stamped = Rect::new(
                tx0 as u32,
                ty0 as u32,
                (tx1 - tx0) as u32,
                (ty1 - ty0) as u32,
            );
            touched = union(touched, Some(stamped));
        }
        Ok(touched)
    }

    /// Puts the lifted pixels back exactly as they were. `layer` must be
    /// the layer they were lifted from.
    pub fn cancel(self, layer: &mut Layer) -> Result<(), EditError> {
        debug_assert_eq!(layer.id(), self.layer, "float cancelled on another layer");
        self.restore(layer)
    }

    fn restore(&self, layer: &mut Layer) -> Result<(), EditError> {
        let pixels = layer.pixels_mut()?;
        let size = pixels.size();
        let r = self.rect.clip_to(size);
        let lw = size.width as usize;
        let rw = self.rect.width as usize;
        let src = self.original.as_raw();
        let data = pixels.as_raw_mut();
        for y in 0..r.height as usize {
            let d = ((r.y as usize + y) * lw + r.x as usize) * 4;
            let s = y * rw * 4;
            data[d..d + r.width as usize * 4].copy_from_slice(&src[s..s + r.width as usize * 4]);
        }
        Ok(())
    }
}
