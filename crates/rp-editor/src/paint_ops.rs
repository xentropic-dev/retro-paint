//! Painting on [`EditorState`]: Brush and Eraser strokes on the selected
//! paint layer, and the color Picker.
//!
//! Decisions:
//! - Brush paints the foreground color with the left button and the
//!   background color with the right button. Eraser lowers alpha with the
//!   same brush (either button). Picker samples the flattened image under the
//!   cursor into the foreground (left) or background (right), whatever the
//!   edit target.
//! - The default brush is the built-in hard round brush; a Hard/Soft toggle
//!   swaps in the built-in soft one (round brushes only). Size is 1–256
//!   canvas pixels for round brushes; for a sampled library brush (see
//!   [`crate::brush_library`]) it is the longest side, 1–1024, starting at
//!   the native size clamped to the canvas. The brush is resized once per
//!   size change and the sized brush is cached, never resized per dab; a
//!   sampled brush resizes from its ≤1024 px working base, never from the
//!   original sample. Sampled brushes stamp the fg/bg color through their
//!   mask (a color `.gbr`'s pixmap is dropped).
//! - Dabs are spaced with [`Spacer`] at [`Brush::step`] along the pointer
//!   path, so fast moves interpolate between mouse events and leftover
//!   distance carries into the next event.
//! - Strokes happen in canvas coordinates; the layer's integer offset is
//!   subtracted before stamping, and dabs are clipped to the layer raster.
//! - Locked and image layers refuse the stroke and say why in the status bar.
//! - With the layer's mask as the edit target, strokes paint the mask
//!   instead (see [`crate::mask_ops`]): Brush left hides, Brush right and
//!   Eraser reveal; image layers and pixel-locked layers allow it.
//! - The document version is bumped once per pointer event that laid down at
//!   least one dab, never per dab; the render worker coalesces.
//! - A stroke never writes to a layer (or mask) the render worker's snapshot
//!   still shares: that write would copy the whole raster on the UI thread
//!   (copy-on-write). While a snapshot is out ([`EditorState::snapshots`])
//!   the spaced dabs are queued, and laid in order once it is released:
//!   when the worker's canvas frame arrives ([`EditorState::flush_stroke`]),
//!   at the next pointer event, or at the release (which lays them
//!   regardless). The worker releases its snapshot right after flattening,
//!   so dabs wait at most one flatten and the canvas shows them no later
//!   than a copying stroke would (the copy's snapshot would wait for the
//!   same flatten). Dab positions and results are the same as laying each
//!   immediately.
//! - One stroke is one undo step: the press opens a paint step on the
//!   layer, the release commits it, and only the rectangle covering every
//!   dab (in the layer raster) is searched for changes and stored.
//! - The Picker reads the render worker's flattened frame (what the canvas
//!   shows), never flattens on the UI thread. Fully transparent pixels are
//!   not picked.

use rp_brush::{Brush, BrushSource, Spacer, stamp};
use rp_doc::{EditError, LayerId, LayerKind, Rect, Size, Tool};

use crate::brush_library::BrushKey;
use crate::undo_ops::EditLabel;
use rp_palette_dither::Rgb;

use crate::mask_ops::{EditTarget, MASK_HIDE, MASK_SHOW, mask_dab};
use crate::state::EditorState;
use crate::viewport::Vec2;

/// Smallest brush size, in canvas pixels.
pub const MIN_BRUSH_SIZE: u32 = 1;
/// Largest size of the built-in round brushes, in canvas pixels.
pub const MAX_BRUSH_SIZE: u32 = 256;
/// Largest size of a sampled (library) brush: its longest side, in canvas
/// pixels. Also the longest side of its working base.
pub const MAX_SAMPLED_BRUSH_SIZE: u32 = 1024;
/// Brush size of a fresh editor.
pub const DEFAULT_BRUSH_SIZE: u32 = 12;

/// The built-in brush tips.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum BrushTip {
    #[default]
    Hard,
    Soft,
}

impl BrushTip {
    pub const fn label(self) -> &'static str {
        match self {
            BrushTip::Hard => "Hard",
            BrushTip::Soft => "Soft",
        }
    }

    /// Name shown in the brush library and the ribbon tooltip.
    pub const fn name(self) -> &'static str {
        match self {
            BrushTip::Hard => "Round Hard",
            BrushTip::Soft => "Round Soft",
        }
    }

    pub fn make(self, size: u32) -> Brush {
        match self {
            BrushTip::Hard => Brush::round_hard(size as f32),
            BrushTip::Soft => Brush::round_soft(size as f32),
        }
    }
}

/// The selected brush and its size, with the brush kept at that size.
///
/// Either a built-in round tip (size 1–256, Hard/Soft applies) or a sampled
/// brush from the library (size = its longest side, 1–1024). A sampled
/// brush keeps a *working base* no larger than
/// [`MAX_SAMPLED_BRUSH_SIZE`], built off the UI thread, so a size step
/// resizes from at most 1024 px instead of the 5000 px original.
#[derive(Clone, Debug)]
pub struct BrushState {
    /// The round tip in use, or the one Hard/Soft returns to.
    tip: BrushTip,
    /// Name of the sampled brush in use; `None` for a round tip.
    sampled: Option<String>,
    size: u32,
    /// The brush as chosen (a built-in tip, or a sampled working base).
    base: Brush,
    /// `base` at `size`; what every dab uses.
    sized: Brush,
    /// How many times the brush was resized (for tests).
    resizes: u64,
}

impl Default for BrushState {
    fn default() -> Self {
        let tip = BrushTip::default();
        let brush = tip.make(DEFAULT_BRUSH_SIZE);
        Self {
            tip,
            sampled: None,
            size: DEFAULT_BRUSH_SIZE,
            base: brush.clone(),
            sized: brush,
            resizes: 0,
        }
    }
}

impl BrushState {
    /// Brush size in canvas pixels.
    pub fn size(&self) -> u32 {
        self.size
    }

    pub fn tip(&self) -> BrushTip {
        self.tip
    }

    /// Whether a built-in round tip is in use (Hard/Soft applies).
    pub fn is_round(&self) -> bool {
        self.sampled.is_none()
    }

    /// The brush's display name.
    pub fn name(&self) -> &str {
        self.sampled.as_deref().unwrap_or(self.tip.name())
    }

    /// Largest size for the current brush kind.
    pub fn max_size(&self) -> u32 {
        if self.is_round() {
            MAX_BRUSH_SIZE
        } else {
            MAX_SAMPLED_BRUSH_SIZE
        }
    }

    /// The brush at the current size.
    pub fn brush(&self) -> &Brush {
        &self.sized
    }

    /// Times the brush has been resized so far.
    pub fn resizes(&self) -> u64 {
        self.resizes
    }

    /// Sets the size (clamped to 1–[`BrushState::max_size`]). Resizes the
    /// brush only when the size actually changes. Returns whether it changed.
    pub fn set_size(&mut self, size: u32) -> bool {
        let size = size.clamp(MIN_BRUSH_SIZE, self.max_size());
        if size == self.size {
            return false;
        }
        self.size = size;
        self.sized = self.base.resized(size as f32);
        self.resizes += 1;
        true
    }

    /// Switches to a built-in round tip at the current size (clamped to
    /// 1–256). Generating a round brush is not a resize.
    pub fn set_tip(&mut self, tip: BrushTip) {
        self.tip = tip;
        self.sampled = None;
        self.size = self.size.clamp(MIN_BRUSH_SIZE, MAX_BRUSH_SIZE);
        self.base = tip.make(self.size);
        self.sized = self.base.clone();
    }

    /// Uses a sampled brush: `base` is its working base (longest side at
    /// most [`MAX_SAMPLED_BRUSH_SIZE`], see [`working_base`]), painted at
    /// `size` (clamped to 1–1024).
    pub fn set_sampled(&mut self, name: impl Into<String>, base: Brush, size: u32) {
        self.sampled = Some(name.into());
        self.size = size.clamp(MIN_BRUSH_SIZE, MAX_SAMPLED_BRUSH_SIZE);
        self.sized = if base.diameter() == self.size as f32 {
            base.clone()
        } else {
            self.resizes += 1;
            base.resized(self.size as f32)
        };
        self.base = base;
    }
}

/// A decoded library brush made ready to paint with: its pixmap dropped (it
/// stamps the fg/bg color through its mask) and, for bitmaps larger than
/// [`MAX_SAMPLED_BRUSH_SIZE`], area-downscaled to that longest side and
/// made a plain bitmap again, so later resizes start from at most 1024 px.
/// Parametric brushes are kept as they are (they regenerate at any size).
/// Slow for big samples (tens of ms); run it off the UI thread.
pub fn working_base(brush: Brush) -> Brush {
    let mut b = brush;
    b.pixmap = None;
    if matches!(b.source, BrushSource::Parametric(_)) {
        return b;
    }
    let long = b.mask.width.max(b.mask.height);
    let mask = if long > MAX_SAMPLED_BRUSH_SIZE {
        b.resized(MAX_SAMPLED_BRUSH_SIZE as f32).mask
    } else {
        b.mask
    };
    Brush::bitmap(b.name, b.spacing, mask, None)
}

/// The size a sampled brush starts at: its native longest side, clamped to
/// the canvas's shorter side and to 1–1024.
pub fn default_sampled_size(native_long_side: u32, canvas: Size) -> u32 {
    native_long_side
        .min(canvas.width.min(canvas.height))
        .clamp(MIN_BRUSH_SIZE, MAX_SAMPLED_BRUSH_SIZE)
}

/// The sizes `[` and `]` step through: 1–10 by 1, then by 5 to 50, by 10
/// to 100, by 25 to 250, 256, then (sampled brushes only) by 50 to 500, by
/// 100 to 1000, and 1024.
const SIZE_LADDER: [u32; 41] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 15, 20, 25, 30, 35, 40, 45, 50, 60, 70, 80, 90, 100, 125, 150,
    175, 200, 225, 250, 256, 300, 350, 400, 450, 500, 600, 700, 800, 900, 1000, 1024,
];

/// Size after one `]` press: the next larger size on the ladder, at most
/// `max`.
pub fn next_size_up(size: u32, max: u32) -> u32 {
    SIZE_LADDER
        .into_iter()
        .find(|&s| s > size)
        .unwrap_or(max)
        .min(max)
}

/// Size after one `[` press: the next smaller size on the ladder.
pub fn next_size_down(size: u32) -> u32 {
    SIZE_LADDER
        .into_iter()
        .rev()
        .find(|&s| s < size)
        .unwrap_or(MIN_BRUSH_SIZE)
}

/// The brush size as the DOS view's cursor outline, in output pixels:
/// `size × output / canvas` (by width), at least 1.
pub fn brush_output_diameter(size: u32, canvas: Size, output: Size) -> u32 {
    if canvas.width == 0 {
        return 1;
    }
    let d = size as f32 * output.width as f32 / canvas.width as f32;
    (d.round() as u32).max(1)
}

/// Which mouse button started a stroke or pick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaintButton {
    /// Foreground color.
    Primary,
    /// Background color.
    Secondary,
}

/// What a stroke paints.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StrokePaint {
    /// The layer's pixels in this color.
    Color([u8; 4]),
    /// Lowers the layer's alpha.
    Erase,
    /// The layer's mask toward this value.
    Mask(u8),
}

/// A stroke in progress.
#[derive(Clone, Debug)]
pub struct Stroke {
    pub layer: LayerId,
    pub paint: StrokePaint,
    spacer: Spacer,
    /// Spaced dab centers (canvas coordinates) not laid yet: the render
    /// worker still held a snapshot sharing the raster.
    queued: Vec<(f32, f32)>,
    /// Raster pixels any dab touched so far, `[x0, y0, x1, y1)` in layer
    /// raster coordinates (unclipped).
    bounds: Option<[i64; 4]>,
}

impl Stroke {
    fn grow(&mut self, [a, b, c, d]: [i64; 4]) {
        self.bounds = Some(match self.bounds {
            Some([x0, y0, x1, y1]) => [x0.min(a), y0.min(b), x1.max(c), y1.max(d)],
            None => [a, b, c, d],
        });
    }

    /// Dabs waiting for the render worker to release its snapshot.
    pub fn queued(&self) -> usize {
        self.queued.len()
    }

    /// The stroke's dab bounds clipped to a `size` raster, for
    /// [`rp_doc::History::commit_within`].
    pub fn dirty_rect(&self, size: Size) -> Option<Rect> {
        let [x0, y0, x1, y1] = self.bounds?;
        let clamp = |v: i64, max: u32| v.clamp(0, max as i64) as u32;
        let (x0, x1) = (clamp(x0, size.width), clamp(x1, size.width));
        let (y0, y1) = (clamp(y0, size.height), clamp(y1, size.height));
        Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
    }
}

/// Raster pixels one dab of a `mw × mh` mask centered at `(cx, cy)` covers,
/// `[x0, y0, x1, y1)`; the same placement as [`rp_brush::stamp`].
fn dab_bounds(mw: u32, mh: u32, cx: f32, cy: f32) -> [i64; 4] {
    let x0 = cx.floor() as i64 - (mw / 2) as i64;
    let y0 = cy.floor() as i64 - (mh / 2) as i64;
    [x0, y0, x0 + mw as i64, y0 + mh as i64]
}

/// The flattened document as the render worker delivered it: straight BGRA,
/// `width × height`, row-major.
#[derive(Clone, Copy, Debug)]
pub struct FrameView<'a> {
    pub width: u32,
    pub height: u32,
    pub bgra: &'a [u8],
}

impl FrameView<'_> {
    /// RGBA of the canvas pixel under `c`, or `None` outside the frame.
    pub fn sample(&self, c: Vec2) -> Option<[u8; 4]> {
        if !(c.x >= 0.0 && c.y >= 0.0) {
            return None;
        }
        let (x, y) = (c.x.floor() as u64, c.y.floor() as u64);
        if x >= self.width as u64 || y >= self.height as u64 {
            return None;
        }
        let i = ((y * self.width as u64 + x) * 4) as usize;
        let p = self.bgra.get(i..i + 4)?;
        Some([p[2], p[1], p[0], p[3]])
    }
}

/// Eraser dabs: the fast, byte-identical [`rp_brush::erase_dab`] (lowers
/// alpha under one dab by coverage × opacity).
pub use rp_brush::erase_dab;

fn rgba(c: Rgb) -> [u8; 4] {
    [c.r, c.g, c.b, 255]
}

impl EditorState {
    /// Sets the brush size (1–256 round, 1–1024 sampled); see
    /// [`BrushState::set_size`].
    pub fn set_brush_size(&mut self, size: u32) {
        if self.brush.set_size(size) {
            self.status = format!("Brush size {}", self.brush.size());
        }
    }

    /// `]`: next larger brush size.
    pub fn brush_size_up(&mut self) {
        let max = self.brush.max_size();
        self.set_brush_size(next_size_up(self.brush.size(), max));
    }

    /// `[`: next smaller brush size.
    pub fn brush_size_down(&mut self) {
        self.set_brush_size(next_size_down(self.brush.size()));
    }

    /// The ribbon's Hard/Soft toggle. Only round brushes have a tip; with a
    /// sampled brush selected it refuses and says so.
    pub fn set_brush_tip(&mut self, tip: BrushTip) {
        if !self.brush.is_round() {
            self.status = "Hard/Soft applies to the round brushes only".into();
            return;
        }
        self.select_round_brush(tip);
    }

    /// Switches to a built-in round brush (library or ribbon).
    pub fn select_round_brush(&mut self, tip: BrushTip) {
        self.brush.set_tip(tip);
        self.brushes.select(BrushKey::Builtin(tip));
        self.status = format!("{} brush", tip.label());
    }

    /// Whether a press with the current tool starts a stroke (Brush, Eraser).
    pub fn tool_paints(&self) -> bool {
        matches!(self.tool, Tool::Brush | Tool::Eraser)
    }

    /// Starts a Brush or Eraser stroke at canvas point `c` on the selected
    /// layer and lays down the first dab. Refusals (no layer, locked, image
    /// layer) leave the document alone and set the status. Returns whether
    /// a stroke started.
    pub fn begin_stroke(&mut self, c: Vec2, button: PaintButton) -> bool {
        self.stroke = None;
        if !self.tool_paints() {
            return false;
        }
        self.settle_float();
        if self.edit_target() == EditTarget::Mask {
            return self.begin_mask_stroke(c, button);
        }
        let paint = match (self.tool, button) {
            (Tool::Brush, PaintButton::Primary) => StrokePaint::Color(rgba(self.document.fg)),
            (Tool::Brush, PaintButton::Secondary) => StrokePaint::Color(rgba(self.document.bg)),
            _ => StrokePaint::Erase,
        };
        let Some(id) = self.selected_layer else {
            self.status = "Can't paint: no layer selected".into();
            return false;
        };
        let Some(layer) = self.document.layer_mut(id) else {
            self.status = "Can't paint: no layer selected".into();
            return false;
        };
        if let Err(e) = layer.pixels_mut() {
            let name = &layer.name;
            self.status = match e {
                EditError::PixelsLocked => format!("Can't paint: {name} pixels are locked"),
                EditError::NotAPaintLayer => {
                    format!("Can't paint on image layer {name}: Rasterize it to paint")
                }
                other => format!("Can't paint: {other}"),
            };
            return false;
        }
        let label = match paint {
            StrokePaint::Erase => EditLabel::EraserStroke,
            _ => EditLabel::BrushStroke,
        };
        self.history
            .begin_paint(&self.document, id, label, self.content);
        self.start_stroke(id, paint, c);
        true
    }

    /// A stroke on the selected layer's mask (the edit target is Mask).
    fn begin_mask_stroke(&mut self, c: Vec2, button: PaintButton) -> bool {
        let Some(id) = self.selected_layer else {
            return false;
        };
        let value = match (self.tool, button) {
            (Tool::Brush, PaintButton::Primary) => MASK_HIDE,
            _ => MASK_SHOW,
        };
        self.history
            .begin_snapshot(&self.document, EditLabel::MaskStroke, None, self.content);
        self.start_stroke(id, StrokePaint::Mask(value), c);
        true
    }

    fn start_stroke(&mut self, id: LayerId, paint: StrokePaint, c: Vec2) {
        self.stroke = Some(Stroke {
            layer: id,
            paint,
            spacer: Spacer::new(self.brush.brush().step()),
            queued: Vec::new(),
            bounds: None,
        });
        self.continue_stroke(c);
    }

    /// Extends the stroke to canvas point `c`: stamps every spaced dab
    /// between the last point and `c` (and any queued before). Bumps the
    /// document version once if any dab was laid. Returns the number of
    /// dabs laid now; while the render worker holds a snapshot they are
    /// queued instead and this returns 0 (see the module docs).
    pub fn continue_stroke(&mut self, c: Vec2) -> usize {
        let Some(stroke) = &mut self.stroke else {
            return 0;
        };
        let dabs = stroke.spacer.push((c.x, c.y));
        stroke.queued.extend(dabs);
        self.flush_stroke()
    }

    /// Lays the stroke's queued dabs if the render worker no longer holds a
    /// snapshot. The view calls it when a canvas frame arrives. Returns the
    /// number of dabs laid (the document version was bumped if nonzero).
    pub fn flush_stroke(&mut self) -> usize {
        if self.snapshots.busy() {
            return 0;
        }
        self.lay_queued_dabs()
    }

    /// Lays the stroke's queued dabs now, shared raster or not.
    fn lay_queued_dabs(&mut self) -> usize {
        let Some(stroke) = &mut self.stroke else {
            return 0;
        };
        if stroke.queued.is_empty() {
            return 0;
        }
        let dabs = std::mem::take(&mut stroke.queued);
        let (id, paint) = (stroke.layer, stroke.paint);
        let brush = self.brush.brush();
        let (mw, mh) = (brush.mask.width, brush.mask.height);
        let Some(layer) = self.document.layer_mut(id) else {
            self.stroke = None;
            return 0;
        };
        if let StrokePaint::Mask(value) = paint {
            // Masks are canvas-space: no offset.
            let Some(mask) = &mut layer.mask else {
                self.stroke = None;
                return 0;
            };
            let s = mask.size();
            let buf = mask.as_raw_mut();
            for &(x, y) in &dabs {
                stroke.grow(dab_bounds(mw, mh, x, y));
                mask_dab(buf, s.width, s.height, brush, x, y, value, 1.0);
            }
            self.mark_document_changed();
            return dabs.len();
        }
        let (ox, oy) = match &layer.kind {
            rp_doc::LayerKind::Paint(p) => (p.offset_x as f32, p.offset_y as f32),
            rp_doc::LayerKind::Image(_) => (0.0, 0.0),
        };
        let Ok(pixels) = layer.pixels_mut() else {
            self.stroke = None;
            return 0;
        };
        let (w, h) = (pixels.width(), pixels.height());
        let buf = pixels.as_raw_mut();
        for &(x, y) in &dabs {
            let (lx, ly) = (x - ox, y - oy);
            stroke.grow(dab_bounds(mw, mh, lx, ly));
            match paint {
                StrokePaint::Color(rgba) => stamp(buf, w, h, brush, lx, ly, rgba, 1.0),
                _ => erase_dab(buf, w, h, brush, lx, ly, 1.0),
            }
        }
        self.mark_document_changed();
        dabs.len()
    }

    /// Ends the stroke, if any, recording it as one undo step. Queued dabs
    /// are laid first.
    pub fn end_stroke(&mut self) {
        self.lay_queued_dabs();
        let Some(stroke) = self.stroke.take() else {
            return;
        };
        if let StrokePaint::Mask(_) = stroke.paint {
            // A snapshot step: compares the whole document.
            self.commit_step(None);
            return;
        }
        let hint = match self.document.layer(stroke.layer).map(|l| &l.kind) {
            Some(LayerKind::Paint(p)) => stroke.dirty_rect(p.pixels.size()),
            _ => None,
        };
        // No dab landed: an empty hint records nothing.
        self.commit_step(Some(hint.unwrap_or_default()));
    }

    /// Picker: sets the foreground (primary) or background (secondary) to
    /// the flattened color under canvas point `c` in `frame`. Returns
    /// whether a color was picked; transparent or outside pixels are not.
    pub fn pick_color(
        &mut self,
        frame: Option<FrameView<'_>>,
        c: Vec2,
        button: PaintButton,
    ) -> bool {
        let Some(px) = frame.and_then(|f| f.sample(c)) else {
            return false;
        };
        if px[3] == 0 {
            self.status = "Nothing to pick: transparent".into();
            return false;
        }
        let color = Rgb::new(px[0], px[1], px[2]);
        let which = match button {
            PaintButton::Primary => {
                self.document.fg = color;
                "Foreground"
            }
            PaintButton::Secondary => {
                self.document.bg = color;
                "Background"
            }
        };
        self.status = format!("{which} #{:02x}{:02x}{:02x}", color.r, color.g, color.b);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rp_doc::{Layer, SourceHash, Transform};

    /// A small editor with one transparent paint layer, selected.
    fn editor(w: u32, h: u32) -> EditorState {
        let mut st = EditorState::new();
        let mut doc = rp_doc::Document::new(Size::new(w, h));
        doc.push_layer(Layer::paint("Paint", Size::new(w, h)));
        st.set_document(doc);
        st.tool = Tool::Brush;
        st
    }

    fn pixels(st: &EditorState) -> &[u8] {
        match &st.selected().unwrap().kind {
            rp_doc::LayerKind::Paint(p) => p.pixels.as_raw(),
            _ => panic!("paint layer"),
        }
    }

    fn alpha_at(st: &EditorState, x: u32, y: u32) -> u8 {
        let w = st.document.canvas_size.width;
        pixels(st)[((y * w + x) * 4 + 3) as usize]
    }

    fn rgba_at(st: &EditorState, x: u32, y: u32) -> [u8; 4] {
        let w = st.document.canvas_size.width;
        let i = ((y * w + x) * 4) as usize;
        pixels(st)[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn default_brush_is_round_hard_at_default_size() {
        let st = EditorState::new();
        assert_eq!(st.brush.size(), DEFAULT_BRUSH_SIZE);
        assert_eq!(st.brush.tip(), BrushTip::Hard);
        assert_eq!(
            st.brush.brush(),
            &Brush::round_hard(DEFAULT_BRUSH_SIZE as f32)
        );
        assert_eq!(st.brush.resizes(), 0);
    }

    #[test]
    fn size_change_resizes_once_and_is_cached() {
        let mut st = editor(64, 64);
        st.set_brush_size(20);
        assert_eq!(st.brush.resizes(), 1);
        assert_eq!(st.brush.brush().diameter(), 20.0);
        st.set_brush_size(20);
        assert_eq!(st.brush.resizes(), 1, "same size: no resize");
        // Painting a long stroke never resizes.
        st.begin_stroke(Vec2::new(5.0, 5.0), PaintButton::Primary);
        for i in 0..50 {
            st.continue_stroke(Vec2::new(5.0 + i as f32, 30.0));
        }
        st.end_stroke();
        assert_eq!(st.brush.resizes(), 1);
        st.set_brush_size(0);
        assert_eq!(st.brush.size(), 1);
        st.set_brush_size(1000);
        assert_eq!(st.brush.size(), MAX_BRUSH_SIZE);
        assert_eq!(st.brush.resizes(), 3);
    }

    #[test]
    fn tip_toggle_uses_builtin_soft_and_hard_at_current_size() {
        let mut st = EditorState::new();
        st.set_brush_size(30);
        st.set_brush_tip(BrushTip::Soft);
        assert_eq!(st.brush.brush(), &Brush::round_soft(30.0));
        st.set_brush_tip(BrushTip::Hard);
        assert_eq!(st.brush.brush(), &Brush::round_hard(30.0));
        assert_eq!(st.brush.resizes(), 1);
    }

    #[test]
    fn bracket_steps_are_monotonic_and_clamped() {
        let m = MAX_BRUSH_SIZE;
        assert_eq!(next_size_up(1, m), 2);
        assert_eq!(next_size_up(9, m), 10);
        assert_eq!(next_size_up(10, m), 15);
        assert_eq!(next_size_up(12, m), 15);
        assert_eq!(next_size_up(250, m), 256);
        assert_eq!(next_size_up(256, m), 256);
        let big = MAX_SAMPLED_BRUSH_SIZE;
        assert_eq!(next_size_up(256, big), 300);
        assert_eq!(next_size_up(1000, big), 1024);
        assert_eq!(next_size_up(1024, big), 1024);
        assert_eq!(next_size_down(1024), 1000);
        assert_eq!(next_size_down(300), 256);
        for s in MIN_BRUSH_SIZE..=big {
            let up = next_size_up(s, big);
            assert!(up >= s && up <= big);
        }
        assert_eq!(next_size_down(1), 1);
        assert_eq!(next_size_down(15), 10);
        assert_eq!(next_size_down(12), 10);
        assert_eq!(next_size_down(10), 9);
        assert_eq!(next_size_down(256), 250);
        for s in MIN_BRUSH_SIZE..=MAX_BRUSH_SIZE {
            assert!(next_size_up(s, m) >= s && next_size_down(s) <= s);
        }
        let mut st = EditorState::new();
        st.brush_size_up();
        assert_eq!(st.brush.size(), 15);
        st.brush_size_down();
        st.brush_size_down();
        assert_eq!(st.brush.size(), 9);
    }

    #[test]
    fn stroke_dabs_follow_the_spacer_with_carry() {
        let mut st = editor(200, 20);
        st.set_brush_size(1); // 1x1 dab, step 1px: each dab is one pixel.
        let step = st.brush.brush().step();
        assert_eq!(step, 1.0);
        st.set_brush_size(20); // 19x19 mask, 10% spacing: step 1.9px.
        let step = st.brush.brush().step();
        assert!((step - 1.9).abs() < 1e-4, "{step}");

        let mut st = editor(200, 20);
        st.set_brush_size(1);
        assert!(st.begin_stroke(Vec2::new(10.5, 10.5), PaintButton::Primary));
        // Moves of 0.5px: a dab only once a whole step (1px) is covered,
        // with leftover distance carried across events.
        let mut n = 0;
        for i in 1..=12 {
            let d = st.continue_stroke(Vec2::new(10.5 + 0.5 * i as f32, 10.5));
            assert_eq!(d, (i % 2 == 0) as usize, "event {i}");
            n += d;
        }
        assert_eq!(n, 6, "6px travelled at 1px spacing");
        st.end_stroke();
        for x in 10..=16 {
            assert_eq!(alpha_at(&st, x, 10), 255, "x={x}");
        }
        assert_eq!(alpha_at(&st, 17, 10), 0);
        assert_eq!(alpha_at(&st, 9, 10), 0);
        assert_eq!(alpha_at(&st, 12, 11), 0);
    }

    #[test]
    fn fast_moves_interpolate_between_events() {
        let mut st = editor(200, 20);
        st.set_brush_size(1);
        st.begin_stroke(Vec2::new(5.5, 5.5), PaintButton::Primary);
        let n = st.continue_stroke(Vec2::new(105.5, 5.5));
        assert_eq!(n, 100);
        assert!((5..=105).all(|x| alpha_at(&st, x, 5) == 255));
    }

    #[test]
    fn brush_paints_fg_left_and_bg_right() {
        let mut st = editor(32, 32);
        st.document.fg = Rgb::new(200, 10, 20);
        st.document.bg = Rgb::new(1, 2, 250);
        st.begin_stroke(Vec2::new(8.5, 8.5), PaintButton::Primary);
        st.end_stroke();
        assert_eq!(rgba_at(&st, 8, 8), [200, 10, 20, 255]);
        st.begin_stroke(Vec2::new(24.5, 24.5), PaintButton::Secondary);
        st.end_stroke();
        assert_eq!(rgba_at(&st, 24, 24), [1, 2, 250, 255]);
    }

    #[test]
    fn eraser_lowers_alpha_with_the_same_brush() {
        let mut st = editor(32, 32);
        st.set_brush_size(5);
        st.begin_stroke(Vec2::new(16.5, 16.5), PaintButton::Primary);
        st.end_stroke();
        assert_eq!(alpha_at(&st, 16, 16), 255);
        st.tool = Tool::Eraser;
        st.begin_stroke(Vec2::new(16.5, 16.5), PaintButton::Primary);
        st.end_stroke();
        assert_eq!(alpha_at(&st, 16, 16), 0);

        // A soft brush's edge only partly erases.
        let mut st = editor(32, 32);
        st.set_brush_size(15);
        st.begin_stroke(Vec2::new(16.5, 16.5), PaintButton::Primary);
        st.end_stroke();
        st.set_brush_tip(BrushTip::Soft);
        st.tool = Tool::Eraser;
        st.begin_stroke(Vec2::new(16.5, 16.5), PaintButton::Primary);
        st.end_stroke();
        assert_eq!(alpha_at(&st, 16, 16), 0);
        let edge = alpha_at(&st, 21, 16);
        assert!(edge > 0 && edge < 255, "partial erase at the edge: {edge}");
    }

    #[test]
    fn locked_layer_refuses_and_leaves_document_alone() {
        let mut st = editor(16, 16);
        let id = st.selected_layer.unwrap();
        st.document.layer_mut(id).unwrap().lock.pixels = true;
        let (before, v) = (st.document.clone(), st.doc_version);
        assert!(!st.begin_stroke(Vec2::new(4.5, 4.5), PaintButton::Primary));
        assert_eq!(st.continue_stroke(Vec2::new(8.5, 4.5)), 0);
        assert_eq!(st.document, before);
        assert_eq!(st.doc_version, v);
        assert!(st.status.contains("locked"), "{}", st.status);
        st.tool = Tool::Eraser;
        assert!(!st.begin_stroke(Vec2::new(4.5, 4.5), PaintButton::Primary));
    }

    #[test]
    fn image_layer_refuses() {
        let mut st = editor(16, 16);
        let id = st.document.push_layer(Layer::image(
            "Photo",
            SourceHash::new("ab"),
            Transform::default(),
        ));
        st.select_layer(id);
        let (before, v) = (st.document.clone(), st.doc_version);
        assert!(!st.begin_stroke(Vec2::new(4.5, 4.5), PaintButton::Primary));
        assert_eq!(st.document, before);
        assert_eq!(st.doc_version, v);
        assert!(st.status.contains("image layer"), "{}", st.status);
    }

    #[test]
    fn select_and_picker_tools_do_not_stroke() {
        let mut st = editor(16, 16);
        for tool in [Tool::Select, Tool::Move, Tool::Picker] {
            st.tool = tool;
            let v = st.doc_version;
            assert!(!st.begin_stroke(Vec2::new(4.5, 4.5), PaintButton::Primary));
            assert_eq!(st.doc_version, v);
        }
    }

    #[test]
    fn layer_offset_is_applied_and_dabs_clip_to_the_layer() {
        let mut st = editor(32, 32);
        st.set_brush_size(1);
        let id = st.selected_layer.unwrap();
        st.document
            .layer_mut(id)
            .unwrap()
            .translate_by(5, -3)
            .unwrap();
        st.begin_stroke(Vec2::new(10.5, 10.5), PaintButton::Primary);
        st.end_stroke();
        // Canvas (10, 10) is layer pixel (5, 13).
        assert_eq!(alpha_at(&st, 5, 13), 255);
        assert_eq!(alpha_at(&st, 10, 10), 0);
        // A big dab hanging off the layer's edge is clipped, not a panic.
        st.set_brush_size(40);
        st.begin_stroke(Vec2::new(2.5, 31.5), PaintButton::Primary);
        st.continue_stroke(Vec2::new(-30.5, 60.5));
        st.end_stroke();
        assert_eq!(alpha_at(&st, 0, 20), 255);
        assert_eq!(alpha_at(&st, 31, 0), 0);
    }

    #[test]
    fn doc_version_bumps_once_per_event_not_per_dab() {
        let mut st = editor(200, 20);
        st.set_brush_size(1);
        let v0 = st.doc_version;
        st.begin_stroke(Vec2::new(1.5, 5.5), PaintButton::Primary);
        assert_eq!(st.doc_version, v0 + 1);
        assert_eq!(st.continue_stroke(Vec2::new(51.5, 5.5)), 50);
        assert_eq!(st.doc_version, v0 + 2, "50 dabs, one bump");
        assert_eq!(st.continue_stroke(Vec2::new(51.7, 5.5)), 0);
        assert_eq!(st.doc_version, v0 + 2, "no dab, no bump");
        st.end_stroke();
        assert_eq!(st.continue_stroke(Vec2::new(90.5, 5.5)), 0);
        assert_eq!(st.doc_version, v0 + 2, "after end_stroke nothing paints");
    }

    #[test]
    fn pointer_moves_continue_the_stroke_one_bump_each() {
        let mut st = editor(100, 100);
        st.set_view_size(100.0, 100.0);
        st.view_command(crate::state::ViewCommand::ActualSize);
        st.set_brush_size(1);
        let at = |st: &EditorState, x: f32, y: f32| {
            let o = st.viewport.offset();
            Vec2::new(o.x + x, o.y + y)
        };
        let v = st.doc_version;
        st.begin_stroke(Vec2::new(10.5, 10.5), PaintButton::Primary);
        let p = at(&st, 40.5, 10.5);
        st.pointer_moved(p);
        assert_eq!(st.doc_version, v + 2);
        assert!((10..=40).all(|x| alpha_at(&st, x, 10) == 255));
        st.end_stroke();
        let p = at(&st, 40.5, 50.5);
        st.pointer_moved(p);
        assert_eq!(
            st.doc_version,
            v + 2,
            "hover without a stroke paints nothing"
        );
        assert_eq!(alpha_at(&st, 40, 30), 0);
    }

    #[test]
    fn stroke_stops_if_the_layer_disappears() {
        let mut st = editor(16, 16);
        st.add_paint_layer();
        st.begin_stroke(Vec2::new(4.5, 4.5), PaintButton::Primary);
        st.delete_selected_layer();
        assert_eq!(st.continue_stroke(Vec2::new(12.5, 4.5)), 0);
        assert!(st.stroke.is_none());
    }

    #[test]
    fn picker_reads_from_the_frame() {
        let mut st = editor(4, 2);
        st.tool = Tool::Picker;
        // BGRA: pixel (1, 1) is RGB (10, 20, 30); pixel (0, 0) transparent.
        let mut bgra = vec![0u8; 4 * 2 * 4];
        bgra[(4 + 1) * 4..(4 + 1) * 4 + 4].copy_from_slice(&[30, 20, 10, 255]);
        bgra[4..8].copy_from_slice(&[1, 2, 3, 128]);
        let frame = FrameView {
            width: 4,
            height: 2,
            bgra: &bgra,
        };
        let v = st.doc_version;
        assert!(st.pick_color(Some(frame), Vec2::new(1.2, 1.9), PaintButton::Primary));
        assert_eq!(st.document.fg, Rgb::new(10, 20, 30));
        assert_eq!(st.doc_version, v, "picking does not re-render");
        assert!(st.pick_color(Some(frame), Vec2::new(1.0, 0.0), PaintButton::Secondary));
        assert_eq!(st.document.bg, Rgb::new(3, 2, 1));
        assert!(!st.pick_color(Some(frame), Vec2::new(0.5, 0.5), PaintButton::Primary));
        assert!(!st.pick_color(Some(frame), Vec2::new(4.0, 0.5), PaintButton::Primary));
        assert!(!st.pick_color(Some(frame), Vec2::new(-0.5, 0.5), PaintButton::Primary));
        assert!(!st.pick_color(None, Vec2::new(1.5, 1.5), PaintButton::Primary));
        assert_eq!(st.document.fg, Rgb::new(10, 20, 30));
    }

    /// A 9×1 horizontal bar: a sampled brush no round tip could fake.
    fn bar_brush() -> Brush {
        Brush::bitmap("Bar", 100.0, rp_brush::Mask::new(9, 1, vec![255; 9]), None)
    }

    #[test]
    fn sampled_brush_paints_fg_bg_erases_and_masks_with_its_shape() {
        let mut st = editor(32, 32);
        st.document.fg = Rgb::new(9, 8, 7);
        st.document.bg = Rgb::new(1, 2, 3);
        st.brush.set_sampled("Bar", working_base(bar_brush()), 9);
        st.begin_stroke(Vec2::new(10.5, 10.5), PaintButton::Primary);
        st.end_stroke();
        assert_eq!(rgba_at(&st, 6, 10), [9, 8, 7, 255]);
        assert_eq!(rgba_at(&st, 14, 10), [9, 8, 7, 255]);
        assert_eq!(alpha_at(&st, 10, 9), 0, "a bar, not a disc");
        assert_eq!(alpha_at(&st, 10, 11), 0);
        st.begin_stroke(Vec2::new(10.5, 20.5), PaintButton::Secondary);
        st.end_stroke();
        assert_eq!(rgba_at(&st, 10, 20), [1, 2, 3, 255]);
        st.tool = Tool::Eraser;
        st.begin_stroke(Vec2::new(10.5, 10.5), PaintButton::Primary);
        st.end_stroke();
        assert_eq!(alpha_at(&st, 6, 10), 0);
        assert_eq!(alpha_at(&st, 10, 20), 255);

        // Mask painting uses it too.
        let id = st.selected_layer.unwrap();
        st.add_mask(id);
        st.toggle_mask_target(id);
        st.tool = Tool::Brush;
        st.begin_stroke(Vec2::new(16.5, 5.5), PaintButton::Primary);
        st.end_stroke();
        let m = st.document.layer(id).unwrap().mask.as_ref().unwrap();
        let at = |x: u32, y: u32| m.as_raw()[(y * 32 + x) as usize];
        assert_eq!(at(12, 5), MASK_HIDE);
        assert_eq!(at(20, 5), MASK_HIDE);
        assert_eq!(at(16, 6), MASK_SHOW);
    }

    /// Timing note for big sampled brushes (the user's 75 MB pack has
    /// 5000 px samples): a size step resizes from the ≤1024 px working base,
    /// and a stroke with a 1024 px dab at 25% spacing stamps one dab per
    /// 256 px. Measured on the dev box (rp-brush builds at opt-level 3 in
    /// dev): resize 10–15 ms, 15–23 ms per fully covered 1024 px dab
    /// (`rp_brush::stamp`, ~15 ns/px; splatter samples are mostly empty and
    /// cheaper). The bounds here are loose; they catch a regression to
    /// resizing from the original or stamping per pixel moved.
    ///
    /// stroke-perf: ~7 ms/dab here is the stamp itself, not a layer copy
    /// (there is no render worker in this test, and the one copy, the
    /// stroke's first write against the undo `before`, lands in the first
    /// event): this brush's coverage is translucent everywhere, so after
    /// the first dab (~1.5 ms over empty pixels) each overlapping dab takes
    /// `stamp`'s blend-over-paint path (~7–8 ms per 1024² dab). See
    /// `stroke_timing` for strokes against a live worker.
    #[test]
    fn big_sampled_brush_resize_and_stroke_cost() {
        use std::time::Instant;
        let n = 1024u32;
        let data: Vec<u8> = (0..n * n)
            .map(|i| ((i % n) * 255 / n) as u8 / 2 + ((i / n) % 7) as u8)
            .collect();
        let sample = Brush::bitmap("Big", 25.0, rp_brush::Mask::new(n, n, data), None);
        let mut st = editor(2048, 2048);
        st.brush.set_sampled("Big", working_base(sample), 1024);

        let t = Instant::now();
        st.brush_size_down(); // 1000
        st.brush_size_up(); // 1024
        let resize = t.elapsed() / 2;
        assert_eq!(st.brush.size(), 1024);

        let t = Instant::now();
        st.begin_stroke(Vec2::new(100.5, 1000.5), PaintButton::Primary);
        let mut dabs = 1;
        for i in 1..=19 {
            dabs += st.continue_stroke(Vec2::new(100.5 + 100.0 * i as f32, 1000.5));
        }
        let painting = t.elapsed();
        st.end_stroke();
        let stroke = t.elapsed();
        assert_eq!(dabs, 8, "1900 px at a 256 px step, plus the first dab");
        eprintln!(
            "1024 px sampled brush: resize {resize:.2?}; {dabs} dabs {painting:.2?} \
             ({:.2?}/dab); with the undo commit {stroke:.2?}",
            painting / dabs as u32
        );
        assert!(resize.as_millis() < 250, "resize {resize:?}");
        assert!(stroke.as_millis() < 2000, "stroke {stroke:?}");
    }

    #[test]
    fn output_diameter_scales_by_output_over_canvas() {
        let c = Size::new(800, 1280);
        let o = Size::new(200, 320);
        assert_eq!(brush_output_diameter(12, c, o), 3);
        assert_eq!(brush_output_diameter(1, c, o), 1);
        assert_eq!(brush_output_diameter(256, c, o), 64);
        assert_eq!(brush_output_diameter(10, c, o), 3);
    }
}
