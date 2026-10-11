//! Selection tools (Lasso, Rect Select), Select All / Deselect, and Clear
//! (erase the selected pixels) on [`EditorState`], with no GPUI types.
//!
//! The selection itself is [`rp_doc::Selection`] (canvas-sized coverage).
//! This module captures the outline being drawn, combines it into the
//! selection, and keeps a cached outline of the selection edge for drawing.
//!
//! # Decisions
//!
//! - **Rect Select:** press and drag a rectangle; release selects it,
//!   snapped to whole pixels and clipped to the canvas. Pressing Shift
//!   *during* the drag squares it. Shift held at the press means Add instead;
//!   to square an Add drag, release Shift and press it again. A click
//!   without a drag (empty rectangle) in Replace mode deselects.
//! - **Lasso:** an outline that *starts with a drag* is a classic freehand
//!   lasso: releasing the button closes it. An outline that *starts with a
//!   click* is polygonal: each click adds a straight segment, and a drag
//!   between clicks adds a freehand piece (the two mix freely). It closes on
//!   a double-click, Enter, or a click within [`CLOSE_RADIUS`] screen pixels
//!   of its first point. Freehand points are recorded every [`LASSO_STEP`]
//!   screen pixels. Escape cancels the unfinished outline; switching tools
//!   cancels it too. Closing an outline of fewer than three points selects
//!   nothing (so a Replace click-click deselects).
//! - **Modes:** taken at the press that starts an outline: Shift adds, Alt
//!   subtracts (Alt wins if both are held), otherwise the outline replaces
//!   the selection.
//! - **Ctrl+A** selects the whole canvas, **Ctrl+D** (Edit > Deselect)
//!   clears it. Both drop an unfinished outline.
//! - **Selection is per document and not an undo step.** It survives tool
//!   switches, and New, Open and a canvas resize (Project Settings, or an
//!   undo/redo that changes the canvas size) drop it.
//! - **Clear** (Delete, Backspace, Edit > Clear) erases the selected pixels
//!   of the selected paint layer exactly like [`rp_doc::erase_selected`] (by
//!   coverage; soft edges keep part of their alpha) as one undo step "Clear
//!   Selection" (a paint step over the selection bounds). It refuses on
//!   image layers ("Rasterize it to erase"), pixel-locked layers, or without
//!   a selection. With the mask as the edit target it moves the selected
//!   mask area toward white (shown) by coverage instead; like mask strokes,
//!   that ignores the pixel lock.
//! - **The edge is cached.** [`selection_edges`] runs once per selection
//!   change: it traces the boundary of coverage ≥ 128 along pixel edges,
//!   merged into horizontal and vertical runs, inside the selection bounds
//!   only. Each frame just maps the runs to the screen and culls them
//!   ([`screen_runs`]); the view draws them as 1 px dark lines with light
//!   [`DASH`]-pixel dashes, the same width at any zoom.

use std::sync::Arc;

use rp_doc::{EditError, LayerKind, Rect, SelectMode, Selection, Size, Tool, erase_selected};

use crate::mask_ops::{EditTarget, MASK_SHOW};
use crate::state::EditorState;
use crate::undo_ops::EditLabel;
use crate::viewport::{Vec2, Viewport};

/// Screen pixels the pointer must travel before a freehand lasso point is
/// recorded.
pub const LASSO_STEP: f32 = 2.0;
/// A click within this many screen pixels of the outline's first point
/// closes a polygonal lasso.
pub const CLOSE_RADIUS: f32 = 8.0;
/// Dash length of the selection edge, in screen pixels.
pub const DASH: f32 = 4.0;
/// Coverage at or above which a pixel counts as inside for the drawn edge.
pub const EDGE_THRESHOLD: u8 = 128;

/// The combine mode for modifiers held at the press: Alt subtracts, Shift
/// adds, neither replaces.
pub const fn select_mode(shift: bool, alt: bool) -> SelectMode {
    if alt {
        SelectMode::Subtract
    } else if shift {
        SelectMode::Add
    } else {
        SelectMode::Replace
    }
}

/// One straight run of the selection edge along pixel boundaries, in canvas
/// coordinates: from `(x, y)` for `len` pixels to the right (horizontal) or
/// down (vertical).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Edge {
    pub x: u32,
    pub y: u32,
    pub len: u32,
    pub horizontal: bool,
}

/// The boundary of `selection`'s coverage ≥ [`EDGE_THRESHOLD`], as merged
/// horizontal and vertical runs. Works inside the selection bounds only.
pub fn selection_edges(selection: &Selection) -> Vec<Edge> {
    let Some(b) = selection.bounds() else {
        return Vec::new();
    };
    let (w, h) = (b.width as usize, b.height as usize);
    let cw = selection.size().width as usize;
    let cov = selection.coverage().as_raw();
    let mut inside = vec![false; w * h];
    for y in 0..h {
        let row = &cov[(b.y as usize + y) * cw + b.x as usize..][..w];
        for (dst, &c) in inside[y * w..][..w].iter_mut().zip(row) {
            *dst = c >= EDGE_THRESHOLD;
        }
    }
    let at = |x: usize, y: usize| inside[y * w + x];
    let mut edges = Vec::new();
    // Horizontal pixel boundaries: line `ly` lies between rows ly-1 and ly.
    for ly in 0..=h {
        let mut run: Option<usize> = None;
        for x in 0..=w {
            let edge = x < w && (ly > 0 && at(x, ly - 1)) != (ly < h && at(x, ly));
            match (edge, run) {
                (true, None) => run = Some(x),
                (false, Some(start)) => {
                    edges.push(Edge {
                        x: b.x + start as u32,
                        y: b.y + ly as u32,
                        len: (x - start) as u32,
                        horizontal: true,
                    });
                    run = None;
                }
                _ => {}
            }
        }
    }
    // Vertical boundaries: line `lx` lies between columns lx-1 and lx.
    for lx in 0..=w {
        let mut run: Option<usize> = None;
        for y in 0..=h {
            let edge = y < h && (lx > 0 && at(lx - 1, y)) != (lx < w && at(lx, y));
            match (edge, run) {
                (true, None) => run = Some(y),
                (false, Some(start)) => {
                    edges.push(Edge {
                        x: b.x + lx as u32,
                        y: b.y + start as u32,
                        len: (y - start) as u32,
                        horizontal: false,
                    });
                    run = None;
                }
                _ => {}
            }
        }
    }
    edges
}

/// An [`Edge`] on screen (canvas-area coordinates).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ScreenRun {
    pub x: f32,
    pub y: f32,
    pub len: f32,
    pub horizontal: bool,
}

impl ScreenRun {
    /// The light dashes of this run as `(start, len)` along it: dash `k`
    /// covers `[k·2·DASH, k·2·DASH + DASH)` of the screen coordinate along
    /// the run's axis, so the pattern lines up across runs.
    pub fn dashes(&self) -> Vec<(f32, f32)> {
        let start = if self.horizontal { self.x } else { self.y };
        let end = start + self.len;
        let period = 2.0 * DASH;
        let mut k = (start / period).floor();
        let mut out = Vec::new();
        loop {
            let d0 = (k * period).max(start);
            let d1 = (k * period + DASH).min(end);
            if k * period >= end {
                break;
            }
            if d1 > d0 {
                out.push((d0 - start, d1 - d0));
            }
            k += 1.0;
        }
        out
    }
}

/// `edges` mapped to the screen by `vp`, dropping runs entirely outside the
/// view (with a 1 px margin).
pub fn screen_runs(edges: &[Edge], vp: &Viewport) -> Vec<ScreenRun> {
    let view = vp.view_size();
    let z = vp.zoom();
    edges
        .iter()
        .filter_map(|e| {
            let p = vp.canvas_to_screen(Vec2::new(e.x as f32, e.y as f32));
            let len = e.len as f32 * z;
            let (x1, y1) = if e.horizontal {
                (p.x + len, p.y)
            } else {
                (p.x, p.y + len)
            };
            let visible = x1 >= -1.0 && p.x <= view.x + 1.0 && y1 >= -1.0 && p.y <= view.y + 1.0;
            visible.then_some(ScreenRun {
                x: p.x,
                y: p.y,
                len,
                horizontal: e.horizontal,
            })
        })
        .collect()
}

/// A lasso outline being drawn. Points are canvas coordinates.
#[derive(Clone, PartialEq, Debug)]
pub struct LassoDraft {
    pub points: Vec<[f32; 2]>,
    pub mode: SelectMode,
    /// The button is down: moves add freehand points.
    pub dragging: bool,
    /// The outline is polygonal (it started with a click): releases do not
    /// close it.
    pub polygonal: bool,
    /// Number of points right after the last press.
    press_len: usize,
}

/// What a press on an open lasso outline did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LassoPress {
    /// Added a vertex; the outline stays open.
    Vertex,
    /// The outline is finished.
    Close,
}

impl LassoDraft {
    /// A new outline at canvas point `c`, with the button down.
    pub fn start(c: Vec2, mode: SelectMode) -> Self {
        Self {
            points: vec![[c.x, c.y]],
            mode,
            dragging: true,
            polygonal: false,
            press_len: 1,
        }
    }

    /// The pointer moved to `c` (canvas) at `zoom`: records a freehand point
    /// once it is [`LASSO_STEP`] screen pixels from the last one. Returns
    /// whether a point was added.
    pub fn drag_to(&mut self, c: Vec2, zoom: f32) -> bool {
        if !self.dragging {
            return false;
        }
        let [lx, ly] = *self.points.last().expect("an outline has a point");
        let d = (c.x - lx).hypot(c.y - ly) * zoom;
        if d < LASSO_STEP {
            return false;
        }
        self.points.push([c.x, c.y]);
        true
    }

    /// The button went up. Returns whether that closes the outline (a
    /// freehand lasso that started with a drag).
    pub fn release(&mut self) -> bool {
        if !self.dragging {
            return false;
        }
        self.dragging = false;
        let dragged = self.points.len() > self.press_len;
        if !self.polygonal && !dragged {
            self.polygonal = true;
        }
        !self.polygonal
    }

    /// Whether screen point `s` is close enough to the first point to close
    /// the outline (it needs three points).
    pub fn near_start(&self, s: Vec2, vp: &Viewport) -> bool {
        let [x, y] = self.points[0];
        let p = vp.canvas_to_screen(Vec2::new(x, y));
        self.points.len() >= 3 && (p.x - s.x).hypot(p.y - s.y) <= CLOSE_RADIUS
    }

    /// A press at canvas point `c` (screen `s`) on the open outline;
    /// `double` for the second press of a double-click.
    pub fn press(&mut self, c: Vec2, s: Vec2, double: bool, vp: &Viewport) -> LassoPress {
        if double || self.near_start(s, vp) {
            return LassoPress::Close;
        }
        if self.points.last() != Some(&[c.x, c.y]) {
            self.points.push([c.x, c.y]);
        }
        self.dragging = true;
        self.press_len = self.points.len();
        LassoPress::Vertex
    }
}

/// A rectangle being dragged. Corners are canvas coordinates.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RectDraft {
    pub anchor: Vec2,
    pub corner: Vec2,
    pub mode: SelectMode,
    /// Shift squares the rectangle once it has been up since the press.
    square_armed: bool,
}

impl RectDraft {
    /// A new drag at canvas point `c`; `shift` is whether Shift is held at
    /// the press (then it chose Add and does not square yet).
    pub fn start(c: Vec2, mode: SelectMode, shift: bool) -> Self {
        Self {
            anchor: c,
            corner: c,
            mode,
            square_armed: !shift,
        }
    }

    /// Notes the Shift state; releasing it arms squaring.
    pub fn observe_shift(&mut self, shift: bool) {
        if !shift {
            self.square_armed = true;
        }
    }

    /// Whether the rectangle is squared with Shift in state `shift`.
    pub fn squares(&self, shift: bool) -> bool {
        self.square_armed && shift
    }

    /// The far corner, squared if `square`.
    pub fn far_corner(&self, square: bool) -> Vec2 {
        if !square {
            return self.corner;
        }
        let (dx, dy) = (self.corner.x - self.anchor.x, self.corner.y - self.anchor.y);
        let side = dx.abs().max(dy.abs());
        Vec2::new(
            self.anchor.x + side.copysign(dx),
            self.anchor.y + side.copysign(dy),
        )
    }

    /// The pixel-aligned rectangle, clipped to `canvas`; `None` if empty.
    pub fn rect(&self, square: bool, canvas: Size) -> Option<Rect> {
        drag_rect(self.anchor, self.far_corner(square), canvas)
    }
}

/// The rectangle between two canvas points, each rounded to the nearest
/// pixel boundary and clipped to `canvas`; `None` if it is empty.
pub fn drag_rect(a: Vec2, b: Vec2, canvas: Size) -> Option<Rect> {
    let clamp = |v: f32, max: u32| v.round().clamp(0.0, max as f32) as u32;
    let x0 = clamp(a.x.min(b.x), canvas.width);
    let x1 = clamp(a.x.max(b.x), canvas.width);
    let y0 = clamp(a.y.min(b.y), canvas.height);
    let y1 = clamp(a.y.max(b.y), canvas.height);
    (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
}

/// An outline being drawn with a selection tool.
#[derive(Clone, PartialEq, Debug)]
pub enum SelectDraft {
    Lasso(LassoDraft),
    Rect(RectDraft),
}

/// The document's selection, its cached edge, and any outline in progress.
#[derive(Clone, Debug)]
pub struct SelectionState {
    selection: Selection,
    /// [`selection_edges`] of `selection`, rebuilt on every change.
    edges: Arc<Vec<Edge>>,
    pub draft: Option<SelectDraft>,
}

impl SelectionState {
    /// Nothing selected on a `canvas`-sized document.
    pub fn new(canvas: Size) -> Self {
        Self {
            selection: Selection::new(canvas),
            edges: Arc::new(Vec::new()),
            draft: None,
        }
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// The cached edge runs (canvas coordinates).
    pub fn edges(&self) -> &Arc<Vec<Edge>> {
        &self.edges
    }

    /// Changes the selection through `f` and rebuilds the edge.
    pub fn update(&mut self, f: impl FnOnce(&mut Selection)) {
        f(&mut self.selection);
        self.edges = Arc::new(selection_edges(&self.selection));
    }
}

/// What the canvas draws for the selection tools, in canvas-area (screen)
/// coordinates.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SelectionOverlay {
    /// The selection edge, culled to the view.
    pub runs: Vec<ScreenRun>,
    /// The outline being drawn: a polyline, closed for a rectangle.
    pub draft: Vec<Vec2>,
    pub draft_closed: bool,
    /// The lasso's first point while a click there would close it.
    pub close_marker: Option<Vec2>,
}

impl EditorState {
    /// Whether a press with the current tool starts a selection outline.
    pub fn tool_selects(&self) -> bool {
        matches!(self.tool, Tool::Lasso | Tool::RectSelect)
    }

    /// Drops the selection and any outline (New, Open, canvas resize).
    pub(crate) fn reset_selection(&mut self) {
        self.selection = SelectionState::new(self.document.canvas_size);
    }

    /// Resets the selection if the canvas size changed under it.
    pub(crate) fn drop_stale_selection(&mut self) {
        if self.selection.selection().size() != self.document.canvas_size {
            self.reset_selection();
        }
    }

    /// A primary press at screen point `screen` with a selection tool:
    /// starts an outline, or extends / closes an open lasso. `shift` and
    /// `alt` are the modifiers at the press; `clicks` the click count.
    pub fn begin_select(&mut self, screen: Vec2, shift: bool, alt: bool, clicks: usize) {
        if !self.tool_selects() {
            return;
        }
        self.shift_held = shift;
        let c = self.viewport.screen_to_canvas(screen);
        let mode = select_mode(shift, alt);
        let vp = self.viewport;
        match (&mut self.selection.draft, self.tool) {
            (Some(SelectDraft::Lasso(d)), Tool::Lasso) => {
                if d.press(c, screen, clicks >= 2, &vp) == LassoPress::Close {
                    self.finish_lasso();
                }
            }
            (_, Tool::Lasso) => {
                self.selection.draft = Some(SelectDraft::Lasso(LassoDraft::start(c, mode)));
            }
            _ => {
                self.selection.draft = Some(SelectDraft::Rect(RectDraft::start(c, mode, shift)));
            }
        }
    }

    /// Pointer moved to `screen` while a selection outline is open.
    pub(crate) fn continue_select(&mut self, screen: Vec2) {
        let c = self.viewport.screen_to_canvas(screen);
        let zoom = self.viewport.zoom();
        let shift = self.shift_held;
        match &mut self.selection.draft {
            Some(SelectDraft::Lasso(d)) => {
                d.drag_to(c, zoom);
            }
            Some(SelectDraft::Rect(d)) => {
                d.observe_shift(shift);
                d.corner = c;
            }
            None => {}
        }
    }

    /// Shift changed while an outline is open.
    pub(crate) fn select_shift_changed(&mut self, shift: bool) {
        if let Some(SelectDraft::Rect(d)) = &mut self.selection.draft {
            d.observe_shift(shift);
        }
    }

    /// Whether a drag of a selection outline is in progress (button down).
    pub fn select_dragging(&self) -> bool {
        match &self.selection.draft {
            Some(SelectDraft::Lasso(d)) => d.dragging,
            Some(SelectDraft::Rect(_)) => true,
            None => false,
        }
    }

    /// The button went up: a rectangle is selected, a freehand lasso closes.
    pub fn end_select_drag(&mut self) {
        match &mut self.selection.draft {
            Some(SelectDraft::Lasso(d)) => {
                if d.release() {
                    self.finish_lasso();
                }
            }
            Some(SelectDraft::Rect(d)) => {
                let d = *d;
                self.selection.draft = None;
                let rect = d.rect(d.squares(self.shift_held), self.document.canvas_size);
                match rect {
                    Some(r) => self.selection.update(|s| s.combine_rect(d.mode, r)),
                    None if d.mode == SelectMode::Replace => self.selection.update(|s| s.clear()),
                    None => {}
                }
                self.selection_status();
            }
            None => {}
        }
    }

    /// Closes the open lasso outline (Enter, double-click, click on the
    /// start) and combines it into the selection. Returns whether there was
    /// one.
    pub fn finish_lasso(&mut self) -> bool {
        let Some(SelectDraft::Lasso(d)) = self.selection.draft.take() else {
            return false;
        };
        self.selection
            .update(|s| s.combine_polygon(d.mode, &d.points));
        self.selection_status();
        true
    }

    /// Escape: drops the unfinished outline. Returns whether there was one.
    pub fn cancel_select_draft(&mut self) -> bool {
        self.selection.draft.take().is_some()
    }

    /// Ctrl+A.
    pub fn select_all(&mut self) {
        self.selection.draft = None;
        self.selection.update(|s| s.select_all());
        self.selection_status();
    }

    /// Ctrl+D, Edit > Deselect.
    pub fn deselect(&mut self) {
        self.selection.draft = None;
        if !self.selection.selection().is_empty() {
            self.selection.update(|s| s.clear());
        }
        self.selection_status();
    }

    fn selection_status(&mut self) {
        self.status = match self.selection.selection().bounds() {
            Some(b) => format!("Selection {}×{}", b.width, b.height),
            None => "Nothing selected".into(),
        };
    }

    /// Delete / Backspace / Edit > Clear: erases the selected pixels of the
    /// selected layer (or fills its mask white with the mask as edit
    /// target) as one undo step. Refusals set the status. Returns whether
    /// the document changed.
    pub fn clear_selected(&mut self) -> bool {
        if self.selection.selection().is_empty() {
            self.status = "Can't clear: nothing selected".into();
            return false;
        }
        let Some(id) = self.selected_layer else {
            self.status = "Can't clear: no layer selected".into();
            return false;
        };
        let target = self.edit_target();
        let Some(layer) = self.document.layer(id) else {
            self.status = "Can't clear: no layer selected".into();
            return false;
        };
        let name = layer.name.clone();
        if target == EditTarget::Mask {
            let sel = self.selection.selection().clone();
            let changed = self.record(EditLabel::ClearSelection, |s| {
                let changed = s
                    .document
                    .layer_mut(id)
                    .and_then(|l| l.mask.as_mut())
                    .is_some_and(|m| fill_mask_selected(m, &sel));
                if changed {
                    s.mark_document_changed();
                }
                changed
            });
            self.status = format!("Cleared selection on the mask of {name}");
            return changed;
        }
        match &layer.kind {
            LayerKind::Image(_) => {
                self.status = format!("Can't clear image layer {name}: Rasterize it to erase");
                return false;
            }
            LayerKind::Paint(_) if layer.lock.pixels => {
                self.status = format!("Can't clear: {name} pixels are locked");
                return false;
            }
            LayerKind::Paint(_) => {}
        }
        self.history
            .begin_paint(&self.document, id, EditLabel::ClearSelection, self.content);
        let sel = self.selection.selection();
        let result = match self.document.layer_mut(id) {
            Some(l) => erase_selected(l, sel),
            None => Err(EditError::NotAPaintLayer),
        };
        match result {
            Ok(Some(hint)) => {
                self.mark_document_changed();
                self.commit_step(Some(hint));
                self.status = format!("Cleared selection on {name}");
                true
            }
            Ok(None) => {
                self.history.cancel();
                self.status = format!("Selection misses {name}");
                false
            }
            Err(e) => {
                self.history.cancel();
                self.status = format!("Can't clear: {e}");
                false
            }
        }
    }

    /// What the canvas draws for the selection: the edge and the outline
    /// being drawn, in canvas-area coordinates.
    pub fn selection_overlay(&self) -> Option<SelectionOverlay> {
        let edges = self.selection.edges();
        if edges.is_empty() && self.selection.draft.is_none() {
            return None;
        }
        let vp = &self.viewport;
        let to_screen = |[x, y]: [f32; 2]| vp.canvas_to_screen(Vec2::new(x, y));
        let mut o = SelectionOverlay {
            runs: screen_runs(edges, vp),
            ..Default::default()
        };
        match &self.selection.draft {
            Some(SelectDraft::Lasso(d)) => {
                o.draft = d.points.iter().map(|&p| to_screen(p)).collect();
                if d.polygonal
                    && !d.dragging
                    && let Some(c) = self.cursor_canvas
                {
                    let s = vp.canvas_to_screen(c);
                    o.draft.push(s);
                    if d.near_start(s, vp) {
                        o.close_marker = Some(to_screen(d.points[0]));
                    }
                }
            }
            Some(SelectDraft::Rect(d)) => {
                let a = d.anchor;
                let b = d.far_corner(d.squares(self.shift_held));
                o.draft = [[a.x, a.y], [b.x, a.y], [b.x, b.y], [a.x, b.y]]
                    .map(to_screen)
                    .to_vec();
                o.draft_closed = true;
            }
            None => {}
        }
        Some(o)
    }
}

/// Moves each selected mask value toward [`MASK_SHOW`] by its coverage.
/// Returns whether any value changed.
pub fn fill_mask_selected(mask: &mut rp_doc::GrayImage, selection: &Selection) -> bool {
    let Some(b) = selection.bounds() else {
        return false;
    };
    if mask.size() != selection.size() {
        return false;
    }
    let w = mask.size().width as usize;
    let cov = selection.coverage().as_raw();
    let data = mask.as_raw_mut();
    let mut changed = false;
    for y in b.y as usize..(b.y + b.height) as usize {
        let range = y * w + b.x as usize..y * w + (b.x + b.width) as usize;
        for (m, &c) in data[range.clone()].iter_mut().zip(&cov[range]) {
            let up = ((MASK_SHOW - *m) as u32 * c as u32 + 127) / 255;
            if up > 0 {
                *m += up as u8;
                changed = true;
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::FileCommand;
    use crate::viewport::Vec2;
    use rp_doc::{Document, GrayImage, Layer, LayerKind, RgbaImage};

    const S: Size = Size::new(40, 30);

    /// A 40×30 document with an opaque red paint layer (selected), a
    /// 400×300 view at 100% with the canvas at the view origin.
    fn editor() -> EditorState {
        let mut st = EditorState::new();
        let mut doc = Document::new(S);
        let mut layer = Layer::paint("Paint", S);
        if let LayerKind::Paint(p) = &mut layer.kind {
            p.pixels = RgbaImage::filled(S, [255, 0, 0, 255]);
        }
        doc.push_layer(layer);
        st.set_document(doc);
        st.set_view_size(400.0, 300.0);
        st.view_command(crate::state::ViewCommand::ActualSize);
        st.mark_saved();
        st
    }

    /// Screen point of canvas point `(x, y)`.
    fn at(st: &EditorState, x: f32, y: f32) -> Vec2 {
        st.viewport.canvas_to_screen(Vec2::new(x, y))
    }

    fn drag(st: &mut EditorState, from: (f32, f32), to: &[(f32, f32)], shift: bool, alt: bool) {
        let p = at(st, from.0, from.1);
        st.pointer_moved(p);
        st.begin_select(p, shift, alt, 1);
        for &(x, y) in to {
            st.pointer_moved(at(st, x, y));
        }
        st.end_select_drag();
    }

    fn click(st: &mut EditorState, x: f32, y: f32, clicks: usize) {
        let p = at(st, x, y);
        st.pointer_moved(p);
        st.begin_select(p, false, false, clicks);
        st.end_select_drag();
    }

    fn alpha(st: &EditorState, x: u32, y: u32) -> u8 {
        match &st.document.layers()[0].kind {
            LayerKind::Paint(p) => p.pixels.get(x, y)[3],
            _ => unreachable!(),
        }
    }

    #[test]
    fn modes_from_modifiers() {
        assert_eq!(select_mode(false, false), SelectMode::Replace);
        assert_eq!(select_mode(true, false), SelectMode::Add);
        assert_eq!(select_mode(false, true), SelectMode::Subtract);
        assert_eq!(select_mode(true, true), SelectMode::Subtract);
    }

    #[test]
    fn drag_rect_rounds_orders_and_clips() {
        let r = drag_rect(Vec2::new(7.6, 9.2), Vec2::new(2.4, 3.5), S);
        assert_eq!(r, Some(Rect::new(2, 4, 6, 5)));
        let r = drag_rect(Vec2::new(-5.0, 28.0), Vec2::new(10.0, 99.0), S);
        assert_eq!(r, Some(Rect::new(0, 28, 10, 2)));
        assert_eq!(drag_rect(Vec2::new(3.0, 3.0), Vec2::new(3.2, 9.0), S), None);
    }

    #[test]
    fn rect_squares_only_with_shift_pressed_after_the_press() {
        let mut d = RectDraft::start(Vec2::new(10.0, 10.0), SelectMode::Replace, false);
        d.corner = Vec2::new(14.0, 2.0);
        assert!(!d.squares(false));
        assert!(d.squares(true));
        assert_eq!(d.far_corner(true), Vec2::new(18.0, 2.0));
        assert_eq!(d.rect(true, S), Some(Rect::new(10, 2, 8, 8)));
        // Shift at the press means Add: no squaring until it goes up.
        let mut d = RectDraft::start(Vec2::new(10.0, 10.0), SelectMode::Add, true);
        assert!(!d.squares(true));
        d.observe_shift(true);
        assert!(!d.squares(true));
        d.observe_shift(false);
        assert!(d.squares(true));
    }

    #[test]
    fn rect_select_tool_selects_and_combines() {
        let mut st = editor();
        st.select_tool(Tool::RectSelect);
        drag(
            &mut st,
            (2.0, 3.0),
            &[(5.0, 5.0), (12.0, 10.0)],
            false,
            false,
        );
        assert_eq!(
            st.selection.selection().bounds(),
            Some(Rect::new(2, 3, 10, 7))
        );
        assert!(st.selection.draft.is_none());
        // Shift at press adds.
        drag(&mut st, (20.0, 20.0), &[(25.0, 25.0)], true, false);
        let sel = st.selection.selection();
        assert_eq!(sel.get(3, 4), 255);
        assert_eq!(sel.get(22, 22), 255);
        assert_eq!(sel.get(15, 15), 0);
        // Alt subtracts.
        drag(&mut st, (0.0, 0.0), &[(30.0, 8.0)], false, true);
        let sel = st.selection.selection();
        assert_eq!(sel.get(3, 4), 0);
        assert_eq!(sel.get(3, 9), 255);
        // A click (no drag) in Replace mode deselects.
        drag(&mut st, (5.0, 5.0), &[], false, false);
        assert!(st.selection.selection().is_empty());
        // Selection is not a document edit.
        assert!(!st.is_dirty());
        assert_eq!(st.history.undo_len(), 0);
    }

    #[test]
    fn freehand_lasso_closes_on_release() {
        let mut st = editor();
        st.select_tool(Tool::Lasso);
        drag(
            &mut st,
            (5.0, 5.0),
            &[(20.0, 5.0), (20.0, 20.0), (5.0, 20.0)],
            false,
            false,
        );
        assert!(st.selection.draft.is_none());
        let sel = st.selection.selection();
        assert_eq!(sel.bounds(), Some(Rect::new(5, 5, 15, 15)));
        assert_eq!(sel.get(10, 10), 255);
    }

    #[test]
    fn lasso_records_points_every_step_on_screen() {
        let mut d = LassoDraft::start(Vec2::new(0.0, 0.0), SelectMode::Replace);
        assert!(!d.drag_to(Vec2::new(0.5, 0.0), 2.0), "1 screen px");
        assert!(d.drag_to(Vec2::new(1.0, 0.0), 2.0), "2 screen px");
        assert!(!d.drag_to(Vec2::new(1.5, 0.0), 2.0));
        assert_eq!(d.points.len(), 2);
    }

    #[test]
    fn polygon_lasso_clicks_mix_with_drags_and_close_on_start() {
        let mut st = editor();
        st.select_tool(Tool::Lasso);
        click(&mut st, 5.0, 5.0, 1);
        // A click leaves the outline open.
        let Some(SelectDraft::Lasso(d)) = &st.selection.draft else {
            panic!("open outline")
        };
        assert!(d.polygonal);
        click(&mut st, 25.0, 5.0, 1);
        // A freehand piece in between clicks.
        drag(&mut st, (25.0, 15.0), &[(25.0, 25.0)], false, false);
        assert!(st.selection.draft.is_some(), "drags don't close a polygon");
        click(&mut st, 5.0, 25.0, 1);
        // The rubber band runs to the cursor; near the start it shows the
        // close marker.
        st.pointer_moved(at(&st, 6.0, 6.0));
        let o = st.selection_overlay().unwrap();
        assert_eq!(o.close_marker, Some(at(&st, 5.0, 5.0)));
        assert_eq!(o.draft.len(), 6);
        click(&mut st, 6.0, 6.0, 1);
        assert!(st.selection.draft.is_none());
        let sel = st.selection.selection();
        assert_eq!(sel.bounds(), Some(Rect::new(5, 5, 20, 20)));
    }

    #[test]
    fn lasso_closes_on_double_click_and_enter_and_cancels_on_escape() {
        let mut st = editor();
        st.select_tool(Tool::Lasso);
        for (x, y) in [(2.0, 2.0), (12.0, 2.0), (12.0, 12.0)] {
            click(&mut st, x, y, 1);
        }
        click(&mut st, 12.0, 12.0, 2);
        assert!(st.selection.draft.is_none());
        assert_eq!(st.selection.selection().bounds().unwrap().x, 2);

        for (x, y) in [(20.0, 20.0), (30.0, 20.0), (30.0, 28.0)] {
            click(&mut st, x, y, 1);
        }
        assert!(st.finish_lasso());
        assert_eq!(st.selection.selection().bounds().unwrap().x, 20);

        let before = st.selection.selection().clone();
        click(&mut st, 1.0, 1.0, 1);
        click(&mut st, 9.0, 1.0, 1);
        assert!(st.cancel_select_draft());
        assert!(!st.cancel_select_draft());
        assert_eq!(st.selection.selection(), &before);
    }

    #[test]
    fn select_all_deselect_and_reset_by_new_and_resize() {
        let mut st = editor();
        st.select_all();
        assert_eq!(
            st.selection.selection().bounds(),
            Some(Rect::new(0, 0, 40, 30))
        );
        assert_eq!(st.selection.edges().len(), 4);
        st.select_tool(Tool::Brush);
        assert!(!st.selection.selection().is_empty(), "survives tool switch");
        st.deselect();
        assert!(st.selection.selection().is_empty());
        assert!(st.selection.edges().is_empty());

        st.select_all();
        st.set_document(Document::new(Size::new(10, 10)));
        assert!(st.selection.selection().is_empty());
        assert_eq!(st.selection.selection().size(), Size::new(10, 10));

        st.select_all();
        let mut s = crate::project_ops::ProjectSettings::of(&st.document);
        s.canvas = Size::new(20, 20);
        st.apply_project(&s);
        assert!(st.selection.selection().is_empty());
        assert_eq!(st.selection.selection().size(), Size::new(20, 20));
        // Undo brings back the old canvas size: the selection is dropped.
        st.select_all();
        assert!(st.undo());
        assert_eq!(st.selection.selection().size(), Size::new(10, 10));
        assert!(st.selection.selection().is_empty());
    }

    #[test]
    fn new_and_open_drop_the_selection() {
        let mut st = EditorState::new();
        st.select_all();
        st.dispatch(FileCommand::New);
        assert!(st.selection.selection().is_empty());
    }

    #[test]
    fn tool_switch_cancels_an_open_outline() {
        let mut st = editor();
        st.select_tool(Tool::Lasso);
        click(&mut st, 2.0, 2.0, 1);
        st.select_tool(Tool::Brush);
        assert!(st.selection.draft.is_none());
    }

    #[test]
    fn clear_erases_selected_pixels_as_one_undo_step() {
        let mut st = editor();
        st.selection
            .update(|s| s.combine_rect(SelectMode::Replace, Rect::new(4, 4, 6, 3)));
        let before = st.document.clone();
        assert!(st.clear_selected());
        assert_eq!(alpha(&st, 4, 4), 0);
        assert_eq!(alpha(&st, 9, 6), 0);
        assert_eq!(alpha(&st, 10, 6), 255);
        assert_eq!(alpha(&st, 3, 4), 255);
        assert_eq!(st.history.undo_len(), 1);
        assert_eq!(st.history.undo_label(), Some(EditLabel::ClearSelection));
        assert_eq!(EditLabel::ClearSelection.label(), "Clear Selection");
        assert!(st.is_dirty());
        assert!(st.undo());
        assert_eq!(st.document, before);
        assert!(
            !st.selection.selection().is_empty(),
            "undo keeps the selection"
        );
        // Clearing an already cleared area records nothing.
        assert!(st.redo());
        let steps = st.history.undo_len();
        st.clear_selected();
        assert_eq!(st.history.undo_len(), steps);
    }

    #[test]
    fn clear_refuses_without_selection_on_image_and_locked_layers() {
        let mut st = editor();
        assert!(!st.clear_selected());
        assert!(st.status.contains("nothing selected"), "{}", st.status);
        st.select_all();
        st.document.layers_mut()[0].lock.pixels = true;
        assert!(!st.clear_selected());
        assert!(st.status.contains("locked"), "{}", st.status);
        assert_eq!(st.history.undo_len(), 0);

        let mut doc = Document::new(S);
        doc.push_layer(Layer::image(
            "Photo",
            rp_doc::SourceHash::new("x"),
            Default::default(),
        ));
        st.set_document(doc);
        st.select_all();
        assert!(!st.clear_selected());
        assert!(st.status.contains("Rasterize it to erase"), "{}", st.status);
        assert_eq!(st.history.undo_len(), 0);
    }

    #[test]
    fn clear_with_mask_target_fills_the_mask_white() {
        let mut st = editor();
        let id = st.selected_layer.unwrap();
        st.document.layer_mut(id).unwrap().mask = Some(GrayImage::filled(S, 0));
        st.mask_target = Some(id);
        st.selection
            .update(|s| s.combine_rect(SelectMode::Replace, Rect::new(1, 1, 2, 2)));
        let before = st.document.clone();
        assert!(st.clear_selected());
        let mask = st.document.layer(id).unwrap().mask.clone().unwrap();
        assert_eq!(mask.get(1, 1), 255);
        assert_eq!(mask.get(2, 2), 255);
        assert_eq!(mask.get(3, 3), 0);
        assert_eq!(alpha(&st, 1, 1), 255, "pixels untouched");
        assert_eq!(st.history.undo_label(), Some(EditLabel::ClearSelection));
        assert!(st.undo());
        assert_eq!(st.document, before);
    }

    #[test]
    fn fill_mask_by_coverage() {
        let mut sel = Selection::new(Size::new(2, 1));
        sel.combine_polygon(
            SelectMode::Replace,
            &[[0.0, 0.0], [1.5, 0.0], [1.5, 1.0], [0.0, 1.0]],
        );
        let mut mask = GrayImage::filled(Size::new(2, 1), 0);
        assert!(fill_mask_selected(&mut mask, &sel));
        assert_eq!(mask.get(0, 0), 255);
        assert_eq!(mask.get(1, 0), 128);
    }

    #[test]
    fn edges_trace_the_boundary_in_merged_runs() {
        let mut sel = Selection::new(S);
        assert!(selection_edges(&sel).is_empty());
        sel.combine_rect(SelectMode::Replace, Rect::new(2, 3, 5, 4));
        let mut e = selection_edges(&sel);
        e.sort_by_key(|e| (e.horizontal, e.x, e.y));
        assert_eq!(
            e,
            vec![
                Edge {
                    x: 2,
                    y: 3,
                    len: 4,
                    horizontal: false
                },
                Edge {
                    x: 7,
                    y: 3,
                    len: 4,
                    horizontal: false
                },
                Edge {
                    x: 2,
                    y: 3,
                    len: 5,
                    horizontal: true
                },
                Edge {
                    x: 2,
                    y: 7,
                    len: 5,
                    horizontal: true
                },
            ]
        );
        // An L shape: six runs; a hole adds four more.
        sel.combine_rect(SelectMode::Add, Rect::new(2, 7, 2, 3));
        assert_eq!(selection_edges(&sel).len(), 6);
        sel.combine_rect(SelectMode::Subtract, Rect::new(4, 4, 1, 1));
        assert_eq!(selection_edges(&sel).len(), 10);
    }

    #[test]
    fn edges_follow_the_half_coverage_boundary() {
        // A diagonal edge: antialiased pixels below 128 are outside.
        let sel = Selection::from_polygon(S, &[[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]]);
        for e in selection_edges(&sel) {
            if e.horizontal {
                for x in e.x..e.x + e.len {
                    let above = e.y > 0 && sel.get(x, e.y - 1) >= EDGE_THRESHOLD;
                    let below = e.y < S.height && sel.get(x, e.y) >= EDGE_THRESHOLD;
                    assert_ne!(above, below, "{e:?} at x {x}");
                }
            }
        }
    }

    #[test]
    fn screen_runs_scale_and_cull() {
        let mut vp = Viewport::new(100, 100);
        vp.set_view_size(50.0, 50.0);
        vp.actual_size();
        vp.zoom_about(2.0, vp.canvas_to_screen(Vec2::default()));
        let o = vp.canvas_to_screen(Vec2::default());
        let edges = [
            Edge {
                x: 1,
                y: 2,
                len: 3,
                horizontal: true,
            },
            Edge {
                x: 99,
                y: 99,
                len: 1,
                horizontal: false,
            },
        ];
        let runs = screen_runs(&edges, &vp);
        assert_eq!(
            runs,
            vec![ScreenRun {
                x: o.x + 2.0,
                y: o.y + 4.0,
                len: 6.0,
                horizontal: true
            }]
        );
    }

    #[test]
    fn dashes_keep_a_screen_space_period() {
        let r = ScreenRun {
            x: 2.0,
            y: 0.0,
            len: 15.0,
            horizontal: true,
        };
        // Light dashes at [0,4), [8,12), [16,20) in screen x, clipped to [2,17).
        assert_eq!(r.dashes(), vec![(0.0, 2.0), (6.0, 4.0), (14.0, 1.0)]);
        let v = ScreenRun {
            x: 0.0,
            y: 5.0,
            len: 2.0,
            horizontal: false,
        };
        assert!(v.dashes().is_empty());
    }

    #[test]
    fn edges_of_a_full_canvas_selection_are_cheap() {
        let size = crate::state::NEW_CANVAS_SIZE;
        let sel = Selection::all(size);
        let t = std::time::Instant::now();
        assert_eq!(selection_edges(&sel).len(), 4);
        assert!(t.elapsed().as_millis() < 500);
    }
}
