//! Transform Selection: lifting the selected pixels of a paint layer into a
//! floating piece ([`rp_doc::Float`]), moving / scaling / rotating it with
//! the image-layer handles of [`crate::transform_ops`], and stamping it back
//! into the same layer. No GPUI types.
//!
//! # Decisions
//!
//! - **The box.** With a non-empty selection and the Select or Move tool,
//!   the canvas shows a transform box on the selection bounds with the
//!   image-layer handles (corners scale, the knob rotates); while floating
//!   it follows the piece. It replaces an image layer's own handles until
//!   the selection is dropped (Ctrl+D). Edit > Transform Selection (Ctrl+T)
//!   checks the refusals below and switches to the Select tool.
//! - **Gestures.** A press inside the box moves, on a corner scales (aspect
//!   kept, Shift frees it, opposite corner fixed), on the knob rotates
//!   (Shift snaps to 15°); arrows nudge 1 px, Shift+arrows 10 px. The first
//!   of these lifts the pixels ([`Float::lift`]: the hole is transparent,
//!   soft edges split by coverage). A press outside the box while floating
//!   commits and does nothing else; without a float it moves the layer as
//!   before.
//! - **Refusals** (status bar, nothing lifted): an image layer ("Rasterize
//!   it to transform a selection"), pixel- or position-locked layers, an
//!   empty selection, the mask as edit target, no layer.
//! - **One undo step.** The lift opens a paint step on the layer
//!   ("Transform Selection"); drags and nudges only change the float's
//!   transform; commit stamps the piece and closes the step over the
//!   lifted area plus where the piece landed. A float committed where it was
//!   lifted records nothing. Escape cancels: the lifted bytes go back
//!   exactly, the step is dropped and the content id (dirty state) returns
//!   to its value before the lift. Ctrl+Z while floating cancels instead of
//!   undoing the previous step.
//! - **What commits.** Enter, a press outside the box, switching tool or
//!   layer or edit target, any File command (Save, Export, New, Open, …),
//!   and every other document edit or selection change: they all settle the
//!   float first ([`EditorState::settle_float`]), so no other step is ever
//!   opened while a float holds its paint step. New/Open drop it with the
//!   history.
//! - **Live preview.** While floating, the render snapshot
//!   ([`EditorState::render_snapshot`]) inserts [`Float::preview_layer`] just
//!   above the layer and the piece in a cloned [`SourceStore`] (a map of
//!   `Arc`s), so the canvas and the DOS view show the result through the
//!   normal flatten. A pointer step only changes the float's transform: the
//!   layer's pixels are copied once, by the lift, and never per event
//!   (asserted by the `drag_step_at_full_size` test). At 800×1280 one drag
//!   step plus its snapshot measured under 1 µs on the UI thread even in a
//!   debug build; committing a full-canvas float including the selection
//!   follow measured about 27 ms (debug build, once per commit).
//! - **The selection follows the piece.** On commit the selection's edge
//!   (coverage ≥ 50 %, the same boundary the dashed edge shows) is traced
//!   into closed pixel-corner loops, each corner is mapped through the
//!   float's placement, and the loops are filled as one even-odd polygon
//!   ([`Selection::from_polygon`], bridges between loops are traversed
//!   twice so they cancel). Moves by whole pixels keep the selection exact;
//!   scaled or rotated edges come out antialiased; a soft lasso edge
//!   becomes hard at 50 %. Work is bounds-local and runs once per commit;
//!   the edge cache is rebuilt once. While floating, the dashed edge is
//!   shifted with a pure move and hidden under scale or rotation (the box
//!   shows the piece).

use std::sync::Arc;

use rp_doc::{
    Document, EditError, Float, LayerId, LayerKind, Selection, Size, SourceStore, Tool, Transform,
};

use crate::mask_ops::EditTarget;
use crate::state::EditorState;
use crate::transform_ops::{
    DragKind, Frame, Handle, HandleLayout, StartPlacement, TransformDrag, placement, translated,
};
use crate::undo_ops::EditLabel;
use crate::viewport::Vec2;

/// A lifted selection waiting to be committed or cancelled.
#[derive(Clone, Debug)]
pub struct Floating {
    pub float: Float,
    /// [`EditorState::content`] before the lift (restored by cancel).
    content_before: u64,
}

impl EditorState {
    /// Whether a selection is floating.
    pub fn is_floating(&self) -> bool {
        self.floating.is_some()
    }

    /// The selection transform box: the float's placed box, or the
    /// selection bounds with the Select or Move tool. `None` if neither.
    pub fn selection_frame(&self) -> Option<Frame> {
        if let Some(f) = &self.floating {
            return Some(Frame::of(&f.float.transform(), f.float.piece().size()));
        }
        if !self.tool_transforms() {
            return None;
        }
        let b = self.selection.selection().bounds()?;
        Some(Frame::of(
            &Transform::at(b.x as f32, b.y as f32),
            Size::new(b.width, b.height),
        ))
    }

    /// Why the selected layer's selection can't be transformed, if it
    /// can't; else the layer.
    fn float_target(&self) -> Result<LayerId, String> {
        if self.selection.selection().is_empty() {
            return Err("Can't transform: nothing selected".into());
        }
        let Some(layer) = self.selected() else {
            return Err("Can't transform: no layer selected".into());
        };
        let name = &layer.name;
        if self.edit_target() == EditTarget::Mask {
            return Err(format!(
                "Can't transform a selection of the mask of {name}: edit the layer instead"
            ));
        }
        if let LayerKind::Image(_) = layer.kind {
            return Err(format!(
                "Can't transform the selection on image layer {name}: Rasterize it to transform a selection"
            ));
        }
        if layer.lock.pixels {
            return Err(format!(
                "Can't transform the selection: {name} pixels are locked"
            ));
        }
        if layer.lock.position {
            return Err(format!(
                "Can't transform the selection: {name} position is locked"
            ));
        }
        Ok(layer.id())
    }

    /// Lifts the selection out of the selected layer unless already
    /// floating. Refusals set the status. Returns whether a float exists.
    fn ensure_float(&mut self) -> bool {
        if self.floating.is_some() {
            return true;
        }
        let id = match self.float_target() {
            Ok(id) => id,
            Err(msg) => {
                self.status = msg;
                return false;
            }
        };
        let content_before = self.content;
        self.history.begin_paint(
            &self.document,
            id,
            EditLabel::TransformSelection,
            self.content,
        );
        let result = match self.document.layer_mut(id) {
            Some(l) => Float::lift(l, self.selection.selection()),
            None => Err(EditError::NotAPaintLayer),
        };
        match result {
            Ok(Some(float)) => {
                self.floating = Some(Floating {
                    float,
                    content_before,
                });
                self.mark_document_changed();
                true
            }
            Ok(None) => {
                self.history.cancel();
                self.status = "Selection misses the layer".into();
                false
            }
            Err(e) => {
                self.history.cancel();
                self.status = format!("Can't transform the selection: {e}");
                false
            }
        }
    }

    /// A Select/Move press at screen point `screen` while the selection box
    /// is shown: `Some(started)` if the box handled it (a drag on the box,
    /// a refusal, or a commit by pressing outside), `None` to let the layer
    /// drag handle it.
    pub(crate) fn begin_float_drag(&mut self, screen: Vec2) -> Option<bool> {
        let frame = self.selection_frame()?;
        let hit = HandleLayout::new(&frame, &self.viewport).hit(screen);
        let c = self.viewport.screen_to_canvas(screen);
        let kind = match hit {
            Some(Handle::Corner(k)) => DragKind::Scale(k),
            Some(Handle::Rotate) => DragKind::Rotate,
            None if frame.contains(c) => DragKind::Move,
            None if self.floating.is_some() => {
                self.commit_float();
                return Some(false);
            }
            None => return None,
        };
        if !self.ensure_float() {
            return Some(false);
        }
        let f = &self.floating.as_ref()?.float;
        self.transform_drag = Some(TransformDrag {
            layer: f.layer(),
            kind,
            start: c,
            before: StartPlacement::Float(f.transform(), f.piece().size()),
            mask_before: None,
        });
        Some(true)
    }

    /// One pointer step of a float drag (see
    /// [`continue_transform`](Self::continue_transform)).
    pub(crate) fn continue_float_drag(
        &mut self,
        drag: &TransformDrag,
        t0: Transform,
        src: Size,
        p: Vec2,
        shift: bool,
    ) -> bool {
        let placed = placement(drag.kind, t0, src, drag.start, p, shift);
        self.status = placed.status;
        self.set_float_transform(placed.transform)
    }

    /// Places the float at `t`; bumps the document version if it moved.
    fn set_float_transform(&mut self, t: Transform) -> bool {
        let Some(f) = &mut self.floating else {
            return false;
        };
        if f.float.transform() == t {
            return false;
        }
        f.float.set_transform(t);
        self.doc_version = self.doc_version.wrapping_add(1);
        true
    }

    /// Arrow keys with a selection box: lifts if needed and moves the float.
    pub(crate) fn nudge_float(&mut self, dx: i32, dy: i32) -> bool {
        if !self.ensure_float() {
            return false;
        }
        let Some(f) = &self.floating else {
            return false;
        };
        let t = translated(f.float.transform(), (dx, dy));
        self.status = format!("Nudge {dx}, {dy}");
        self.set_float_transform(t)
    }

    /// Edit > Transform Selection (Ctrl+T): checks the selection can be
    /// transformed and shows its box (switching to the Select tool).
    pub fn transform_selection(&mut self) -> bool {
        if self.floating.is_some() {
            self.status = "Transforming: Enter applies, Escape cancels".into();
            return false;
        }
        if let Err(msg) = self.float_target() {
            self.status = msg;
            return false;
        }
        if !self.tool_transforms() {
            self.select_tool(Tool::Select);
        }
        self.status =
            "Transform Selection: drag inside to move, corners scale, the knob rotates".into();
        true
    }

    /// Stamps the float into its layer as one undo step ("Transform
    /// Selection") and moves the selection with it. Returns whether there
    /// was a float.
    pub fn commit_float(&mut self) -> bool {
        let Some(Floating { float, .. }) = self.floating.take() else {
            return false;
        };
        self.drop_float_drag();
        let moved =
            (!float.is_home()).then(|| (float.origin(), float.piece().size(), float.transform()));
        let result = match self.document.layer_mut(float.layer()) {
            Some(l) => float.commit(l),
            None => Err(EditError::NotAPaintLayer),
        };
        match result {
            Ok(hint) => {
                self.mark_document_changed();
                self.commit_step(hint);
                self.status = "Transform Selection applied".into();
            }
            Err(e) => {
                self.history.cancel();
                self.mark_document_changed();
                self.status = format!("Can't apply the transform: {e}");
            }
        }
        if let Some((origin, piece, t)) = moved {
            let placed = transformed_selection(self.selection.selection(), origin, piece, &t);
            self.selection.update(|s| *s = placed);
        }
        true
    }

    /// Puts the lifted pixels back exactly and drops the step. Returns
    /// whether there was a float.
    pub fn cancel_float(&mut self) -> bool {
        let Some(Floating {
            float,
            content_before,
        }) = self.floating.take()
        else {
            return false;
        };
        self.drop_float_drag();
        if let Some(l) = self.document.layer_mut(float.layer()) {
            // Restoring needs nothing the lift didn't already check.
            let _ = float.cancel(l);
        }
        self.history.cancel();
        self.content = content_before;
        self.doc_version = self.doc_version.wrapping_add(1);
        self.status = "Transform Selection cancelled".into();
        true
    }

    /// Commits a float, if any, before another edit or a tool, layer or
    /// file change.
    pub(crate) fn settle_float(&mut self) {
        if self.floating.is_some() {
            self.commit_float();
        }
    }

    fn drop_float_drag(&mut self) {
        if matches!(
            self.transform_drag,
            Some(TransformDrag {
                before: StartPlacement::Float(..),
                ..
            })
        ) {
            self.transform_drag = None;
        }
    }

    /// The canvas offset to draw the selection edge at while floating:
    /// `Some((0, 0))` when not floating, the move for a pure move, `None`
    /// when the float is scaled or rotated (the edge is hidden).
    pub(crate) fn selection_edge_shift(&self) -> Option<(f32, f32)> {
        let Some(f) = &self.floating else {
            return Some((0.0, 0.0));
        };
        let t = f.float.transform();
        let (ox, oy) = f.float.origin();
        (t.scale_x == 1.0 && t.scale_y == 1.0 && t.rotation == 0.0)
            .then_some((t.translate_x - ox as f32, t.translate_y - oy as f32))
    }

    /// What the render worker flattens: the document and sources, plus the
    /// float drawn as a temporary image layer just above its layer.
    pub fn render_snapshot(&self) -> (Document, Arc<SourceStore>) {
        let mut doc = self.document.clone();
        let Some(f) = &self.floating else {
            return (doc, self.sources.clone());
        };
        let Some(i) = doc.index_of(f.float.layer()) else {
            return (doc, self.sources.clone());
        };
        let preview = f.float.preview_layer(&doc.layers()[i]);
        doc.insert_layer(i + 1, preview);
        let mut sources = (*self.sources).clone();
        sources.insert(Float::preview_source(), f.float.piece().clone());
        (doc, Arc::new(sources))
    }
}

/// Direction bits of a boundary edge leaving a vertex.
const RIGHT: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const UP: u8 = 8;

/// The boundary of `selection`'s coverage ≥ 128 as closed loops of pixel
/// corners (canvas coordinates), with the inside on the right when walking
/// (y down). Collinear corners are dropped. Bounds-local.
pub fn selection_loops(selection: &Selection) -> Vec<Vec<[i32; 2]>> {
    let Some(b) = selection.bounds() else {
        return Vec::new();
    };
    let (w, h) = (b.width as usize, b.height as usize);
    let cw = selection.size().width as usize;
    let cov = selection.coverage().as_raw();
    let threshold = crate::select_ops::EDGE_THRESHOLD;
    let inside = |x: usize, y: usize| cov[(b.y as usize + y) * cw + b.x as usize + x] >= threshold;
    // Outgoing boundary edges per vertex of the (w+1)×(h+1) corner grid.
    let vw = w + 1;
    let mut out = vec![0u8; vw * (h + 1)];
    for y in 0..=h {
        for x in 0..=w {
            let tr = x < w && y > 0 && inside(x, y - 1);
            let bl = x > 0 && y < h && inside(x - 1, y);
            let br = x < w && y < h && inside(x, y);
            // Edge to the right along this row line: inside below → right.
            if x < w && tr != br {
                if br {
                    out[y * vw + x] |= RIGHT;
                } else {
                    out[y * vw + x + 1] |= LEFT;
                }
            }
            // Edge down along this column line: inside left → down.
            if y < h && bl != br {
                if bl {
                    out[y * vw + x] |= DOWN;
                } else {
                    out[(y + 1) * vw + x] |= UP;
                }
            }
        }
    }
    let mut loops = Vec::new();
    for start in 0..out.len() {
        if out[start] == 0 {
            continue;
        }
        let mut pts: Vec<[i32; 2]> = Vec::new();
        let mut cur = start;
        let mut last_dir = 0u8;
        while out[cur] != 0 {
            // Prefer going straight, so runs stay merged.
            let d = if out[cur] & last_dir != 0 {
                last_dir
            } else {
                1 << out[cur].trailing_zeros()
            };
            out[cur] &= !d;
            if d != last_dir {
                let (x, y) = (cur % vw, cur / vw);
                pts.push([b.x as i32 + x as i32, b.y as i32 + y as i32]);
            }
            last_dir = d;
            cur = match d {
                RIGHT => cur + 1,
                LEFT => cur - 1,
                DOWN => cur + vw,
                _ => cur - vw,
            };
        }
        // Back at the start; drop it if the walk ends going the way it began.
        if pts.len() >= 3 {
            loops.push(pts);
        }
    }
    loops
}

/// `selection` moved the way a float lifted at `origin` with a `piece`-sized
/// raster is placed by `t`: its traced boundary (see [`selection_loops`])
/// mapped through the placement and filled even-odd.
pub fn transformed_selection(
    selection: &Selection,
    origin: (i32, i32),
    piece: Size,
    t: &Transform,
) -> Selection {
    let frame = Frame::of(t, piece);
    let (hw, hh) = (piece.width as f32 / 2.0, piece.height as f32 / 2.0);
    let map = |[x, y]: [i32; 2]| -> [f32; 2] {
        let u = (x - origin.0) as f32 - hw;
        let v = (y - origin.1) as f32 - hh;
        let p = frame.to_canvas(Vec2::new(u * t.scale_x, v * t.scale_y));
        [p.x, p.y]
    };
    // One path: each loop closed on itself, joined to the first loop's
    // start by a bridge walked there and back (so it cancels under
    // even-odd).
    let mut path: Vec<[f32; 2]> = Vec::new();
    let loops = selection_loops(selection);
    let Some(first) = loops.first().map(|l| l[0]) else {
        return Selection::new(selection.size());
    };
    for l in &loops {
        path.push(map(first));
        path.extend(l.iter().map(|&p| map(p)));
        path.push(map(l[0]));
    }
    path.push(map(first));
    Selection::from_polygon(selection.size(), &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{FileCommand, ViewCommand};
    use crate::transform_ops::Corner;
    use crate::undo_ops::EditCommand;
    use rp_doc::{GrayImage, Layer, Rect, RgbaImage, SelectMode, SourceHash};
    use std::time::Instant;

    const S: Size = Size::new(40, 30);

    /// A 40×30 document: an opaque white background and a "Paint" layer
    /// (selected) that is red in the 10×10 square at (5, 5) and has a
    /// distinct blue pixel at (6, 7); the square is selected; Select tool;
    /// a 400×300 view at 400% (so the box is larger than its handles).
    fn editor() -> EditorState {
        let mut st = EditorState::new();
        let mut doc = Document::new(S);
        let mut bg = Layer::paint("Background", S);
        if let LayerKind::Paint(p) = &mut bg.kind {
            p.pixels = RgbaImage::filled(S, [255, 255, 255, 255]);
        }
        doc.push_layer(bg);
        let mut layer = Layer::paint("Paint", S);
        if let LayerKind::Paint(p) = &mut layer.kind {
            for y in 5..15 {
                for x in 5..15 {
                    p.pixels.put(x, y, [255, 0, 0, 255]);
                }
            }
            p.pixels.put(6, 7, [0, 0, 255, 255]);
        }
        doc.push_layer(layer);
        st.set_document(doc);
        st.set_view_size(400.0, 300.0);
        st.view_command(ViewCommand::ActualSize);
        st.viewport.zoom_about(4.0, Vec2::default());
        st.mark_saved();
        st.selection
            .update(|s| s.combine_rect(SelectMode::Replace, Rect::new(5, 5, 10, 10)));
        st.select_tool(Tool::Select);
        st
    }

    fn at(st: &EditorState, x: f32, y: f32) -> Vec2 {
        st.viewport.canvas_to_screen(Vec2::new(x, y))
    }

    fn px(st: &EditorState, x: u32, y: u32) -> [u8; 4] {
        match &st.document.layers()[1].kind {
            LayerKind::Paint(p) => p.pixels.get(x, y),
            _ => unreachable!(),
        }
    }

    /// Drags from canvas `from` by canvas `(dx, dy)` and releases.
    fn drag(st: &mut EditorState, from: (f32, f32), d: (f32, f32), shift: bool) -> bool {
        let p = at(st, from.0, from.1);
        st.pointer_moved(p);
        let started = st.begin_transform(p);
        st.shift_held = shift;
        st.pointer_moved(at(st, from.0 + d.0, from.1 + d.1));
        st.end_transform();
        st.shift_held = false;
        started
    }

    #[test]
    fn box_shows_on_selection_bounds_for_select_and_move() {
        let mut st = editor();
        let h = st.transform_handles().unwrap();
        assert_eq!(h.corners[0], at(&st, 5.0, 5.0));
        assert_eq!(h.corners[2], at(&st, 15.0, 15.0));
        st.select_tool(Tool::Move);
        assert!(st.transform_handles().is_some());
        st.select_tool(Tool::Brush);
        assert!(st.transform_handles().is_none());
        st.select_tool(Tool::Select);
        st.deselect();
        assert!(st.transform_handles().is_none(), "paint layers have none");
    }

    #[test]
    fn move_lifts_previews_and_commits_as_one_step() {
        let mut st = editor();
        let before = st.document.clone();
        assert!(drag(&mut st, (8.0, 8.0), (10.0, 3.0), false));
        assert!(st.is_floating());
        // The hole is transparent; the piece is not in the layer yet.
        assert_eq!(px(&st, 6, 7), [0; 4]);
        assert_eq!(px(&st, 16, 10), [0; 4]);
        assert_eq!(st.history.undo_len(), 0);
        // The render snapshot shows it in place.
        let (doc, sources) = st.render_snapshot();
        assert_eq!(doc.layers().len(), 3);
        let flat = doc.flatten(&sources);
        assert_eq!(flat.get(16, 10), [0, 0, 255, 255]);
        assert_eq!(flat.get(6, 7), [255, 255, 255, 255]);
        // A second drag is still the same float and step.
        assert!(drag(&mut st, (18.0, 10.0), (1.0, 0.0), false));
        assert!(st.commit_float());
        assert!(!st.is_floating());
        assert_eq!(px(&st, 17, 10), [0, 0, 255, 255]);
        assert_eq!(px(&st, 18, 10), [255, 0, 0, 255]);
        assert_eq!(px(&st, 6, 7), [0; 4]);
        assert_eq!(st.history.undo_len(), 1);
        assert_eq!(st.history.undo_label(), Some(EditLabel::TransformSelection));
        assert_eq!(EditLabel::TransformSelection.label(), "Transform Selection");
        assert!(st.is_dirty());
        // The selection followed the piece exactly.
        assert_eq!(
            st.selection.selection().bounds(),
            Some(Rect::new(16, 8, 10, 10))
        );
        assert_eq!(st.selection.selection().get(16, 8), 255);
        let after = st.document.clone();
        assert!(st.undo());
        assert_eq!(st.document, before);
        assert!(!st.is_dirty());
        assert!(st.redo());
        assert_eq!(st.document, after);
    }

    #[test]
    fn escape_and_undo_cancel_restoring_bytes_exactly() {
        for via_undo in [false, true] {
            let mut st = editor();
            // A soft-edged selection so the lift splits alpha.
            st.selection.update(|s| {
                s.combine_polygon(
                    SelectMode::Replace,
                    &[[4.3, 4.6], [15.5, 5.2], [14.2, 16.1], [5.0, 13.7]],
                )
            });
            let before = st.document.clone();
            let content = st.content;
            let sel = st.selection.selection().clone();
            assert!(drag(&mut st, (9.0, 9.0), (5.0, 5.0), false));
            let h = st.transform_handles().unwrap();
            let p = h.rotate;
            st.pointer_moved(p);
            assert!(st.begin_transform(p));
            st.pointer_moved(Vec2::new(p.x + 30.0, p.y + 20.0));
            st.end_transform();
            assert!(st.is_floating());
            if via_undo {
                assert!(st.undo());
            } else {
                assert!(st.cancel_float());
            }
            assert!(!st.is_floating());
            assert_eq!(st.document, before);
            assert_eq!(st.content, content);
            assert!(!st.is_dirty());
            assert_eq!(st.history.undo_len(), 0);
            assert_eq!(st.selection.selection(), &sel);
            assert!(!st.cancel_float());
        }
    }

    #[test]
    fn unmoved_float_commits_nothing() {
        let mut st = editor();
        let before = st.document.clone();
        assert!(drag(&mut st, (8.0, 8.0), (0.2, 0.0), false));
        assert!(st.is_floating());
        st.commit_float();
        for y in 0..S.height {
            for x in 0..S.width {
                let want = match &before.layers()[1].kind {
                    LayerKind::Paint(p) => p.pixels.get(x, y),
                    _ => unreachable!(),
                };
                assert_eq!(px(&st, x, y), want, "({x}, {y})");
            }
        }
        assert_eq!(st.document, before);
        assert_eq!(st.history.undo_len(), 0);
        assert!(!st.is_dirty());
    }

    #[test]
    fn corners_scale_with_the_opposite_corner_fixed_and_knob_rotates() {
        let mut st = editor();
        let h = st.transform_handles().unwrap();
        // Bottom-right corner to (25, 25): 2× about the top-left (5, 5).
        let p = h.corners[2];
        st.pointer_moved(p);
        assert!(st.begin_transform(p));
        st.pointer_moved(at(&st, 25.0, 25.0));
        st.end_transform();
        let f = st.selection_frame().unwrap();
        assert_eq!(f.corner(Corner::TopLeft), Vec2::new(5.0, 5.0));
        assert_eq!(f.corner(Corner::BottomRight), Vec2::new(25.0, 25.0));
        assert!(st.status.starts_with("Scale 200%"), "{}", st.status);
        // Shift frees the aspect ratio.
        let p = st.transform_handles().unwrap().corners[2];
        st.pointer_moved(p);
        assert!(st.begin_transform(p));
        st.set_shift(true);
        st.pointer_moved(at(&st, 25.0, 15.0));
        st.end_transform();
        st.set_shift(false);
        let f = st.selection_frame().unwrap();
        assert_eq!(f.corner(Corner::BottomRight), Vec2::new(25.0, 15.0));
        // The knob with Shift snaps to 15°.
        let h = st.transform_handles().unwrap();
        let c = st.selection_frame().unwrap().center;
        st.pointer_moved(h.rotate);
        assert!(st.begin_transform(h.rotate));
        st.set_shift(true);
        let a = 50f32.to_radians();
        st.pointer_moved(at(&st, c.x + 30.0 * a.sin(), c.y - 30.0 * a.cos()));
        st.end_transform();
        let r = st.selection_frame().unwrap().rotation.to_degrees();
        assert!((r - 45.0).abs() < 1e-3, "{r}");
        assert!(st.commit_float());
        assert_eq!(st.history.undo_len(), 1);
        // The selection is the rotated, scaled box (antialiased).
        let sel = st.selection.selection();
        let b = sel.bounds().unwrap();
        assert!(b.width > 20 && b.height > 20, "{b:?}");
        assert_eq!(sel.get(c.x as u32, c.y as u32), 255);
    }

    #[test]
    fn nudges_float_and_enter_like_commit() {
        let mut st = editor();
        // 100%: the whole canvas is in view, so no edge is culled.
        st.view_command(ViewCommand::ActualSize);
        assert!(st.nudge_selected(1, 0));
        assert!(st.nudge_selected(0, 10));
        assert!(st.is_floating());
        // The edge follows a pure move.
        assert_eq!(st.selection_edge_shift(), Some((1.0, 10.0)));
        let o = st.selection_overlay().unwrap();
        let edges = st.selection.edges();
        let base = crate::select_ops::screen_runs(edges, &st.viewport);
        let shifted = crate::select_ops::screen_runs_shifted(edges, &st.viewport, (1.0, 10.0));
        assert_eq!(o.runs, shifted);
        assert_eq!((shifted.len(), base.len()), (4, 4));
        assert_ne!(shifted, base);
        let e = crate::select_ops::Edge {
            x: 5,
            y: 5,
            len: 3,
            horizontal: true,
        };
        let r = crate::select_ops::screen_runs_shifted(&[e], &st.viewport, (1.0, 10.0));
        assert_eq!((r[0].x, r[0].y), {
            let p = at(&st, 6.0, 15.0);
            (p.x, p.y)
        });
        st.commit_float();
        assert_eq!(px(&st, 7, 17), [0, 0, 255, 255]);
        assert_eq!(st.history.undo_len(), 1);
        assert_eq!(
            st.selection.selection().bounds(),
            Some(Rect::new(6, 15, 10, 10))
        );
    }

    #[test]
    fn press_outside_commits_and_inside_without_selection_moves_the_layer() {
        let mut st = editor();
        assert!(drag(&mut st, (8.0, 8.0), (2.0, 0.0), false));
        // Outside the box: commits, no drag, layer offset untouched.
        assert!(!drag(&mut st, (35.0, 25.0), (3.0, 3.0), false));
        assert!(!st.is_floating());
        assert_eq!(st.history.undo_len(), 1);
        // Outside the box without a float: moves the layer as before.
        assert!(drag(&mut st, (35.0, 25.0), (3.0, 0.0), false));
        match &st.document.layers()[1].kind {
            LayerKind::Paint(p) => assert_eq!((p.offset_x, p.offset_y), (3, 0)),
            _ => unreachable!(),
        }
    }

    #[test]
    fn tool_layer_and_file_changes_commit() {
        let switches: [fn(&mut EditorState); 6] = [
            |s| s.select_tool(Tool::Brush),
            |s| {
                let id = s.document.layers()[0].id();
                s.select_layer(id)
            },
            |s| {
                s.dispatch(FileCommand::Save);
            },
            |s| {
                s.dispatch(FileCommand::ExportPng);
            },
            |s| {
                s.edit_command(EditCommand::Deselect);
            },
            |s| {
                let id = s.document.layers()[0].id();
                s.toggle_visibility(id);
            },
        ];
        for (i, f) in switches.into_iter().enumerate() {
            let mut st = editor();
            st.nudge_selected(3, 0);
            assert!(st.is_floating());
            f(&mut st);
            assert!(!st.is_floating(), "switch {i}");
            assert_eq!(px(&st, 9, 7), [0, 0, 255, 255], "switch {i}");
            assert_eq!(
                st.history.undo_len(),
                if i == 5 { 2 } else { 1 },
                "switch {i}"
            );
            assert_eq!(
                st.history.undo_len() == 1,
                st.history.undo_label() == Some(EditLabel::TransformSelection),
                "switch {i}"
            );
        }
    }

    /// A setup step and the status text it must produce.
    type Refusal = (fn(&mut EditorState), &'static str);

    #[test]
    fn refusals_leave_everything_alone() {
        let cases: [Refusal; 5] = [
            (
                |s| s.document.layers_mut()[1].lock.pixels = true,
                "pixels are locked",
            ),
            (
                |s| s.document.layers_mut()[1].lock.position = true,
                "position is locked",
            ),
            (|s| s.deselect(), "nothing selected"),
            (
                |s| {
                    let id = s.selected_layer.unwrap();
                    s.document.layer_mut(id).unwrap().mask = Some(GrayImage::filled(S, 255));
                    s.mask_target = Some(id);
                },
                "mask",
            ),
            (
                |s| {
                    let mut doc = s.document.clone();
                    doc.push_layer(Layer::image(
                        "Photo",
                        SourceHash::new("x"),
                        Transform::at(0.0, 0.0),
                    ));
                    let sel = s.selection.clone();
                    s.set_document(doc);
                    s.selection = sel;
                },
                "Rasterize it to transform a selection",
            ),
        ];
        for (setup, msg) in cases {
            let mut st = editor();
            setup(&mut st);
            let before = st.document.clone();
            let v = st.doc_version;
            // Ctrl+T, a drag inside the selection box and a nudge.
            st.select_tool(Tool::Brush);
            assert!(!st.edit_command(EditCommand::TransformSelection));
            assert!(st.status.contains(msg), "{msg}: {}", st.status);
            st.select_tool(Tool::Select);
            if !st.selection.selection().is_empty() {
                assert!(!drag(&mut st, (8.0, 8.0), (4.0, 4.0), false));
                assert!(st.status.contains(msg), "{msg}: {}", st.status);
                assert!(!st.nudge_selected(1, 0));
            }
            assert!(!st.is_floating());
            assert_eq!(st.document, before, "{msg}");
            assert_eq!(st.doc_version, v, "{msg}");
            assert_eq!(st.history.undo_len(), 0, "{msg}");
        }
    }

    #[test]
    fn ctrl_t_switches_to_select_and_shows_the_box() {
        let mut st = editor();
        st.select_tool(Tool::Lasso);
        assert!(st.transform_handles().is_none());
        assert!(st.edit_command(EditCommand::TransformSelection));
        assert_eq!(st.tool, Tool::Select);
        assert!(st.transform_handles().is_some());
        assert!(
            !st.is_floating(),
            "nothing lifted until the first transform"
        );
        assert_eq!(EditCommand::TransformSelection.keystrokes(), &["ctrl-t"]);
    }

    #[test]
    fn loops_trace_holes_and_transform_exactly_by_whole_pixels() {
        let mut sel = Selection::new(S);
        sel.combine_rect(SelectMode::Replace, Rect::new(2, 3, 8, 6));
        sel.combine_rect(SelectMode::Subtract, Rect::new(4, 5, 2, 2));
        sel.combine_rect(SelectMode::Add, Rect::new(20, 20, 3, 3));
        let loops = selection_loops(&sel);
        assert_eq!(loops.len(), 3);
        assert!(loops.iter().all(|l| l.len() == 4), "{loops:?}");
        let t = Transform::at(7.0, 1.0);
        let moved = transformed_selection(&sel, (2, 3), Size::new(21, 20), &t);
        for y in 0..S.height {
            for x in 0..S.width {
                let src = if x >= 5 && y + 2 < S.height {
                    sel.get(x - 5, y + 2)
                } else {
                    0
                };
                assert_eq!(moved.get(x, y), src, "({x}, {y})");
            }
        }
        // At home it is the same selection.
        let home = transformed_selection(&sel, (2, 3), Size::new(21, 20), &Transform::at(2.0, 3.0));
        assert_eq!(home, sel);
    }

    #[test]
    fn loops_handle_diagonal_touching_pixels() {
        let mut sel = Selection::new(S);
        sel.combine_rect(SelectMode::Replace, Rect::new(2, 2, 1, 1));
        sel.combine_rect(SelectMode::Add, Rect::new(3, 3, 1, 1));
        let home = transformed_selection(&sel, (0, 0), Size::new(8, 8), &Transform::at(0.0, 0.0));
        assert_eq!(home, sel);
    }

    /// The UI-thread cost of one drag step at 800×1280: the float's
    /// transform plus the render snapshot. The layer's pixels must not be
    /// copied per step. `cargo test --release -p rp-editor -- --nocapture
    /// drag_step_at_full_size` prints the timing.
    #[test]
    fn drag_step_at_full_size() {
        let mut st = EditorState::new();
        st.set_view_size(1000.0, 700.0);
        st.select_all();
        st.select_tool(Tool::Move);
        let start = at(&st, 400.0, 640.0);
        st.pointer_moved(start);
        assert!(st.begin_transform(start));
        let id = st.selected_layer.unwrap();
        let pixels = |doc: &Document| match &doc.layer(id).unwrap().kind {
            LayerKind::Paint(p) => p.pixels.clone(),
            _ => unreachable!(),
        };
        let lifted = pixels(&st.document);
        let n = 200;
        let t = Instant::now();
        for i in 0..n {
            st.pointer_moved(Vec2::new(start.x + i as f32, start.y + 1.0));
            let (doc, _) = st.render_snapshot();
            assert!(pixels(&doc).shares_buffer(&lifted), "no copy per step");
        }
        let step = t.elapsed() / n;
        eprintln!("float drag step + snapshot at 800×1280: {step:?}");
        assert!(step.as_millis() < 20, "{step:?}");
        st.end_transform();
        let t = Instant::now();
        assert!(st.commit_float());
        eprintln!("commit + selection follow at 800×1280: {:?}", t.elapsed());
    }
}
