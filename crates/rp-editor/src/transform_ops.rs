//! Move and Select tools on [`EditorState`]: dragging the selected layer,
//! arrow-key nudges, and scaling / rotating image layers with on-canvas
//! handles. The geometry is pure; the GPUI view only draws
//! [`EditorState::transform_handles`] and forwards pointer events.
//!
//! Decisions:
//! - **Move** drags the selected layer, paint or image, wherever the press
//!   lands. **Select** does the same and, on an image layer, also shows the
//!   transformed bounding box with four corner handles (scale) and a
//!   rotation handle above the top edge; pressing a handle scales or rotates
//!   instead of moving. Paint layers have no handles (they only move).
//! - Moves are whole canvas pixels for both layer kinds: the drag offset is
//!   rounded, so a 1:1 image layer stays pixel-aligned and a paint layer's
//!   integer offset is exact. An image layer keeps any fractional part its
//!   translation already had.
//! - Arrow keys nudge the selected layer 1 px, shift+arrow 10 px, with the
//!   Move or Select tool active.
//! - Corner handles scale about the opposite corner, which stays fixed on the
//!   canvas (also when rotated). The aspect ratio is kept by default; shift
//!   frees it. The scaled box never gets smaller than [`MIN_SCALED_SIZE`]
//!   canvas pixels on a side and never flips.
//! - The rotation handle rotates about the image center (which stays fixed);
//!   shift snaps the resulting angle to 15° steps.
//! - Handles are hit-tested and drawn in screen space, so their size does
//!   not change with zoom. The rotation handle wins over corners, and the
//!   nearest corner wins among corners.
//! - A position-locked layer refuses moves, nudges and transforms; the
//!   status bar says why.
//! - Masks follow moves: a Move/Select drag or nudge shifts the layer's
//!   mask by the same whole-pixel delta, padding with white; values pushed
//!   off the canvas are lost (masks stay canvas-sized). A drag shifts the
//!   mask as it was at the press, so nothing is lost until the release.
//!   Scaling and rotating leave the mask where it is (v1). See
//!   [`crate::mask_ops`].
//! - One drag is `begin_transform` → `continue_transform`* →
//!   `end_transform`; the layer's state before the drag is kept in
//!   [`TransformDrag`]. One drag is one undo step (a snapshot opened on
//!   press, committed on release). The document version is bumped once per
//!   pointer event that changed the layer.
//! - Consecutive arrow-key nudges of the same layer are one undo step (see
//!   [`crate::undo_ops`]).

use std::f32::consts::PI;

use rp_doc::{EditError, GrayImage, LayerId, LayerKind, Size, Tool, Transform};

use crate::mask_ops::shifted_mask;
use crate::state::EditorState;
use crate::undo_ops::{Coalesce, EditLabel};
use crate::viewport::{Vec2, Viewport};

/// Screen pixels around a handle's center that still grab it.
pub const HANDLE_HIT_RADIUS: f32 = 8.0;
/// Side of a drawn corner handle, in screen pixels.
pub const HANDLE_SIZE: f32 = 8.0;
/// Screen distance of the rotation handle above the top edge's midpoint.
pub const ROTATE_HANDLE_OFFSET: f32 = 24.0;
/// Smallest scaled image side, in canvas pixels.
pub const MIN_SCALED_SIZE: f32 = 1.0;
/// Rotation snap step with shift held (15°).
pub const ROTATION_SNAP: f32 = PI / 12.0;
/// Arrow-key nudge, in canvas pixels.
pub const NUDGE: i32 = 1;
/// Shift+arrow nudge, in canvas pixels.
pub const NUDGE_LARGE: i32 = 10;

/// A corner of the transformed box, named for the unrotated image.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
}

impl Corner {
    /// In drawing order around the box.
    pub const ALL: [Corner; 4] = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomRight,
        Corner::BottomLeft,
    ];

    /// `(±1, ±1)`: which way the corner lies from the center, unrotated.
    pub const fn sign(self) -> (f32, f32) {
        match self {
            Corner::TopLeft => (-1.0, -1.0),
            Corner::TopRight => (1.0, -1.0),
            Corner::BottomRight => (1.0, 1.0),
            Corner::BottomLeft => (-1.0, 1.0),
        }
    }

    pub const fn opposite(self) -> Corner {
        match self {
            Corner::TopLeft => Corner::BottomRight,
            Corner::TopRight => Corner::BottomLeft,
            Corner::BottomRight => Corner::TopLeft,
            Corner::BottomLeft => Corner::TopRight,
        }
    }
}

/// A grabbable transform handle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handle {
    Corner(Corner),
    Rotate,
}

/// An image layer's placed box on the canvas: center, half extents (always
/// positive, in canvas pixels) and rotation (clockwise on screen).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Frame {
    pub center: Vec2,
    pub half: Vec2,
    pub rotation: f32,
}

/// `R(θ) · v`, clockwise on screen (y down).
fn rotate(v: Vec2, angle: f32) -> Vec2 {
    let (s, c) = angle.sin_cos();
    Vec2::new(c * v.x - s * v.y, s * v.x + c * v.y)
}

fn add(a: Vec2, b: Vec2) -> Vec2 {
    Vec2::new(a.x + b.x, a.y + b.y)
}

fn sub(a: Vec2, b: Vec2) -> Vec2 {
    Vec2::new(a.x - b.x, a.y - b.y)
}

fn dist(a: Vec2, b: Vec2) -> f32 {
    let d = sub(a, b);
    d.x.hypot(d.y)
}

impl Frame {
    /// The box `t` places a `src`-sized image in (see [`Transform`]).
    pub fn of(t: &Transform, src: Size) -> Self {
        let (w, h) = (src.width as f32, src.height as f32);
        Frame {
            center: Vec2::new(
                t.translate_x + w * t.scale_x / 2.0,
                t.translate_y + h * t.scale_y / 2.0,
            ),
            half: Vec2::new((w * t.scale_x / 2.0).abs(), (h * t.scale_y / 2.0).abs()),
            rotation: t.rotation,
        }
    }

    /// Canvas position of point `local` given relative to the center in the
    /// unrotated box.
    pub fn to_canvas(&self, local: Vec2) -> Vec2 {
        add(self.center, rotate(local, self.rotation))
    }

    /// Canvas point `p` relative to the center, in the unrotated box.
    pub fn to_local(&self, p: Vec2) -> Vec2 {
        rotate(sub(p, self.center), -self.rotation)
    }

    pub fn corner(&self, c: Corner) -> Vec2 {
        let (sx, sy) = c.sign();
        self.to_canvas(Vec2::new(sx * self.half.x, sy * self.half.y))
    }

    /// Corners in [`Corner::ALL`] order.
    pub fn corners(&self) -> [Vec2; 4] {
        Corner::ALL.map(|c| self.corner(c))
    }

    /// Whether canvas point `p` is inside the box.
    pub fn contains(&self, p: Vec2) -> bool {
        let l = self.to_local(p);
        l.x.abs() <= self.half.x && l.y.abs() <= self.half.y
    }
}

/// The handles of a [`Frame`] in screen (canvas-area) coordinates.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct HandleLayout {
    /// Box corners in [`Corner::ALL`] order.
    pub corners: [Vec2; 4],
    /// Midpoint of the top edge (the rotation handle's stem starts here).
    pub top_mid: Vec2,
    pub rotate: Vec2,
}

impl HandleLayout {
    pub fn new(frame: &Frame, vp: &Viewport) -> Self {
        let corners = frame.corners().map(|c| vp.canvas_to_screen(c));
        let top_mid = vp.canvas_to_screen(frame.to_canvas(Vec2::new(0.0, -frame.half.y)));
        // The box's "up" on screen; zoom is uniform, so rotation carries over.
        let up = rotate(Vec2::new(0.0, -1.0), frame.rotation);
        let rotate = Vec2::new(
            top_mid.x + up.x * ROTATE_HANDLE_OFFSET,
            top_mid.y + up.y * ROTATE_HANDLE_OFFSET,
        );
        HandleLayout {
            corners,
            top_mid,
            rotate,
        }
    }

    /// The handle under screen point `s`, if any.
    pub fn hit(&self, s: Vec2) -> Option<Handle> {
        if dist(self.rotate, s) <= HANDLE_HIT_RADIUS {
            return Some(Handle::Rotate);
        }
        Corner::ALL
            .into_iter()
            .zip(self.corners)
            .map(|(c, p)| (c, dist(p, s)))
            .filter(|&(_, d)| d <= HANDLE_HIT_RADIUS)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(c, _)| Handle::Corner(c))
    }
}

/// Whole-pixel drag offset from `start` to `p` (canvas coordinates).
pub fn drag_delta(start: Vec2, p: Vec2) -> (i32, i32) {
    (
        (p.x - start.x).round() as i32,
        (p.y - start.y).round() as i32,
    )
}

/// `t` moved by `(dx, dy)` canvas pixels.
pub fn translated(t: Transform, (dx, dy): (i32, i32)) -> Transform {
    Transform {
        translate_x: t.translate_x + dx as f32,
        translate_y: t.translate_y + dy as f32,
        ..t
    }
}

/// `angle` wrapped to `(−π, π]`.
pub fn normalize_angle(angle: f32) -> f32 {
    let a = (angle + PI).rem_euclid(2.0 * PI) - PI;
    if a <= -PI { a + 2.0 * PI } else { a }
}

/// `angle` rounded to the nearest multiple of [`ROTATION_SNAP`].
pub fn snap_angle(angle: f32) -> f32 {
    normalize_angle((angle / ROTATION_SNAP).round() * ROTATION_SNAP)
}

/// `t0` (placing a `src` image) scaled by dragging `corner` to canvas point
/// `p`. The opposite corner stays where it was. With `keep_aspect` both
/// axes scale by the same factor (the pointer is projected onto the box's
/// diagonal).
pub fn scale_from_corner(
    t0: Transform,
    src: Size,
    corner: Corner,
    p: Vec2,
    keep_aspect: bool,
) -> Transform {
    let f0 = Frame::of(&t0, src);
    let (w, h) = (src.width as f32, src.height as f32);
    if w == 0.0 || h == 0.0 {
        return t0;
    }
    let anchor = f0.corner(corner.opposite());
    let (kx, ky) = corner.sign();
    // Pointer relative to the fixed corner, in the box's own axes.
    let d = rotate(sub(p, anchor), -f0.rotation);
    let (want_w, want_h) = (kx * d.x, ky * d.y);
    let (w0, h0) = (2.0 * f0.half.x, 2.0 * f0.half.y);
    let (new_w, new_h) = if keep_aspect && w0 > 0.0 && h0 > 0.0 {
        let f = (want_w * w0 + want_h * h0) / (w0 * w0 + h0 * h0);
        let f = f.max(MIN_SCALED_SIZE / w0.min(h0));
        (w0 * f, h0 * f)
    } else {
        (want_w.max(MIN_SCALED_SIZE), want_h.max(MIN_SCALED_SIZE))
    };
    let sign = |v: f32| if v < 0.0 { -1.0 } else { 1.0 };
    let sx = sign(t0.scale_x) * new_w / w;
    let sy = sign(t0.scale_y) * new_h / h;
    let center = add(
        anchor,
        rotate(Vec2::new(kx * new_w / 2.0, ky * new_h / 2.0), f0.rotation),
    );
    Transform {
        translate_x: center.x - w * sx / 2.0,
        translate_y: center.y - h * sy / 2.0,
        scale_x: sx,
        scale_y: sy,
        rotation: t0.rotation,
    }
}

/// `t0` rotated about its center by the pointer's angle change from `start`
/// to `p` (canvas coordinates, measured around `center`). `snap` rounds the
/// result to 15° steps.
pub fn rotate_about_center(
    t0: Transform,
    center: Vec2,
    start: Vec2,
    p: Vec2,
    snap: bool,
) -> Transform {
    let angle = |q: Vec2| (q.y - center.y).atan2(q.x - center.x);
    let mut r = normalize_angle(t0.rotation + angle(p) - angle(start));
    if snap {
        r = snap_angle(r);
    }
    // The center depends only on translation and scale, so it stays put.
    Transform { rotation: r, ..t0 }
}

/// What a drag does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DragKind {
    Move,
    Scale(Corner),
    Rotate,
}

/// The layer's placement when the drag began.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum StartPlacement {
    /// Paint layer offset.
    Offset(i32, i32),
    /// Image layer transform and source size.
    Image(Transform, Size),
}

/// A Move / Select drag in progress.
#[derive(Clone, PartialEq, Debug)]
pub struct TransformDrag {
    pub layer: LayerId,
    pub kind: DragKind,
    /// Canvas point where the drag began.
    pub start: Vec2,
    /// The layer's placement before the drag (for undo).
    pub before: StartPlacement,
    /// A Move drag's layer mask as it was at the press (shared, not
    /// copied); each step shifts this by the whole drag offset.
    pub mask_before: Option<GrayImage>,
}

fn refusal(verb: &str, name: &str, e: EditError) -> String {
    match e {
        EditError::PositionLocked => format!("Can't {verb}: {name} position is locked"),
        other => format!("Can't {verb} {name}: {other}"),
    }
}

impl EditorState {
    /// Whether a press with the current tool moves or transforms a layer.
    pub fn tool_transforms(&self) -> bool {
        matches!(self.tool, Tool::Move | Tool::Select)
    }

    /// The selected image layer's transform and source size, if its source
    /// is loaded.
    fn selected_image(&self) -> Option<(Transform, Size)> {
        match &self.selected()?.kind {
            LayerKind::Image(i) => Some((i.transform, self.sources.get(&i.source)?.size())),
            LayerKind::Paint(_) => None,
        }
    }

    /// Screen-space transform handles to draw: the Select tool on an image
    /// layer whose source is loaded.
    pub fn transform_handles(&self) -> Option<HandleLayout> {
        if self.tool != Tool::Select {
            return None;
        }
        let (t, src) = self.selected_image()?;
        Some(HandleLayout::new(&Frame::of(&t, src), &self.viewport))
    }

    /// Starts a Move / Select drag at screen point `screen` on the selected
    /// layer: a handle press scales or rotates, anything else moves.
    /// Refusals (no layer, position locked) set the status and return
    /// `false`.
    pub fn begin_transform(&mut self, screen: Vec2) -> bool {
        self.transform_drag = None;
        if !self.tool_transforms() {
            return false;
        }
        let Some(layer) = self.selected() else {
            self.status = "Can't move: no layer selected".into();
            return false;
        };
        let handle = self.transform_handles().and_then(|h| h.hit(screen));
        let kind = match handle {
            Some(Handle::Corner(c)) => DragKind::Scale(c),
            Some(Handle::Rotate) => DragKind::Rotate,
            None => DragKind::Move,
        };
        if layer.lock.position {
            let verb = match kind {
                DragKind::Move => "move",
                DragKind::Scale(_) => "scale",
                DragKind::Rotate => "rotate",
            };
            self.status = refusal(verb, &layer.name, EditError::PositionLocked);
            return false;
        }
        let before = match &layer.kind {
            LayerKind::Paint(p) => StartPlacement::Offset(p.offset_x, p.offset_y),
            LayerKind::Image(i) => {
                let src = self
                    .sources
                    .get(&i.source)
                    .map_or(Size::new(0, 0), |s| s.size());
                StartPlacement::Image(i.transform, src)
            }
        };
        let label = match kind {
            DragKind::Move => EditLabel::Move,
            DragKind::Scale(_) => EditLabel::Scale,
            DragKind::Rotate => EditLabel::Rotate,
        };
        let id = layer.id();
        let mask_before = match kind {
            DragKind::Move => layer.mask.clone(),
            _ => None,
        };
        self.history
            .begin_snapshot(&self.document, label, None, self.content);
        self.transform_drag = Some(TransformDrag {
            layer: id,
            kind,
            start: self.viewport.screen_to_canvas(screen),
            before,
            mask_before,
        });
        true
    }

    /// Updates the drag for the pointer at screen point `screen`; `shift`
    /// frees the aspect ratio (scale) or snaps the angle (rotate). Bumps the
    /// document version once if the layer changed. Returns whether it did.
    pub fn continue_transform(&mut self, screen: Vec2, shift: bool) -> bool {
        let Some(drag) = self.transform_drag.clone() else {
            return false;
        };
        let p = self.viewport.screen_to_canvas(screen);
        let Some(layer) = self.document.layer_mut(drag.layer) else {
            self.end_transform();
            return false;
        };
        let result = match (drag.before, drag.kind) {
            (StartPlacement::Offset(x0, y0), _) => {
                let (dx, dy) = drag_delta(drag.start, p);
                let LayerKind::Paint(cur) = &layer.kind else {
                    return false;
                };
                let step = (x0 + dx - cur.offset_x, y0 + dy - cur.offset_y);
                if step == (0, 0) {
                    return false;
                }
                self.status = format!("Move {dx}, {dy}");
                layer.translate_by(step.0, step.1).map(|()| Some((dx, dy)))
            }
            (StartPlacement::Image(t0, src), kind) => {
                let mut moved = None;
                let t = match kind {
                    DragKind::Move => {
                        let d = drag_delta(drag.start, p);
                        self.status = format!("Move {}, {}", d.0, d.1);
                        moved = Some(d);
                        translated(t0, d)
                    }
                    DragKind::Scale(c) => {
                        let t = scale_from_corner(t0, src, c, p, !shift);
                        self.status = format!(
                            "Scale {:.0}% × {:.0}%",
                            t.scale_x.abs() * 100.0,
                            t.scale_y.abs() * 100.0
                        );
                        t
                    }
                    DragKind::Rotate => {
                        let center = Frame::of(&t0, src).center;
                        let t = rotate_about_center(t0, center, drag.start, p, shift);
                        self.status = format!("Rotate {:.1}°", t.rotation.to_degrees());
                        t
                    }
                };
                let LayerKind::Image(cur) = &layer.kind else {
                    return false;
                };
                if cur.transform == t {
                    return false;
                }
                layer.set_transform(t).map(|()| moved)
            }
        };
        match result {
            Ok(moved) => {
                if let (Some((dx, dy)), Some(mask)) = (moved, &drag.mask_before) {
                    layer.mask = Some(shifted_mask(mask, dx, dy));
                }
                self.mark_document_changed();
                true
            }
            Err(e) => {
                let name = layer.name.clone();
                self.status = refusal("move", &name, e);
                self.end_transform();
                false
            }
        }
    }

    /// Records the shift key. During a drag the pointer's current position
    /// is re-applied, so pressing or releasing shift takes effect at once.
    pub fn set_shift(&mut self, shift: bool) {
        if shift == self.shift_held {
            return;
        }
        self.shift_held = shift;
        if let (Some(_), Some(c)) = (&self.transform_drag, self.cursor_canvas) {
            self.continue_transform(self.viewport.canvas_to_screen(c), shift);
        }
    }

    /// Ends the drag, if any, recording it as one undo step.
    pub fn end_transform(&mut self) {
        if self.transform_drag.take().is_some() {
            self.commit_step(None);
        }
    }

    /// Arrow keys: moves the selected layer by `(dx, dy)` canvas pixels.
    /// Refused (with a status message) if the layer's position is locked.
    pub fn nudge_selected(&mut self, dx: i32, dy: i32) -> bool {
        let Some(id) = self.selected_layer else {
            self.status = "Can't move: no layer selected".into();
            return false;
        };
        self.record_coalesced(EditLabel::Nudge, Coalesce::Nudge(id), |s| {
            s.nudge_layer(id, dx, dy)
        })
    }

    fn nudge_layer(&mut self, id: LayerId, dx: i32, dy: i32) -> bool {
        let Some(layer) = self.document.layer_mut(id) else {
            self.status = "Can't move: no layer selected".into();
            return false;
        };
        match layer.translate_by(dx, dy) {
            Ok(()) => {
                if let Some(mask) = &layer.mask {
                    layer.mask = Some(shifted_mask(mask, dx, dy));
                }
                self.status = format!("Nudge {dx}, {dy}");
                self.mark_document_changed();
                true
            }
            Err(e) => {
                self.status = refusal("move", &layer.name.clone(), e);
                false
            }
        }
    }
}

/// The nudge for an arrow key name (`"left"`, …), or `None` for other keys.
pub fn arrow_nudge(key: &str, shift: bool) -> Option<(i32, i32)> {
    let n = if shift { NUDGE_LARGE } else { NUDGE };
    match key {
        "left" => Some((-n, 0)),
        "right" => Some((n, 0)),
        "up" => Some((0, -n)),
        "down" => Some((0, n)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rp_doc::{Document, GrayImage, Layer, RgbaImage, SourceHash};
    use std::sync::Arc;

    const EPS: f32 = 1e-3;

    fn close(a: Vec2, b: Vec2) -> bool {
        (a.x - b.x).abs() < EPS && (a.y - b.y).abs() < EPS
    }

    fn assert_close(a: Vec2, b: Vec2) {
        assert!(close(a, b), "{a:?} != {b:?}");
    }

    fn t(tx: f32, ty: f32, sx: f32, sy: f32, deg: f32) -> Transform {
        Transform {
            translate_x: tx,
            translate_y: ty,
            scale_x: sx,
            scale_y: sy,
            rotation: deg.to_radians(),
        }
    }

    const SRC: Size = Size::new(40, 20);

    /// 200×200 canvas, a paint layer at the bottom and a 40×20 image layer
    /// (selected) on top at (50, 60), view 400×300 at 100%, origin at 0.
    fn editor() -> (EditorState, LayerId, LayerId) {
        let mut st = EditorState::new();
        let canvas = Size::new(200, 200);
        let mut doc = Document::new(canvas);
        let paint = doc.push_layer(Layer::paint("Paint", canvas));
        let hash = SourceHash::new("abc");
        let img = doc.push_layer(Layer::image("Img", hash.clone(), Transform::at(50.0, 60.0)));
        st.set_document(doc);
        Arc::make_mut(&mut st.sources).insert(hash, RgbaImage::filled(SRC, [1, 2, 3, 255]));
        st.set_view_size(400.0, 300.0);
        st.viewport.actual_size();
        st.auto_fit = false;
        st.selected_layer = Some(img);
        st.tool = Tool::Select;
        (st, paint, img)
    }

    fn transform_of(st: &EditorState, id: LayerId) -> Transform {
        match &st.document.layer(id).unwrap().kind {
            LayerKind::Image(i) => i.transform,
            _ => panic!("image layer"),
        }
    }

    fn offset_of(st: &EditorState, id: LayerId) -> (i32, i32) {
        match &st.document.layer(id).unwrap().kind {
            LayerKind::Paint(p) => (p.offset_x, p.offset_y),
            _ => panic!("paint layer"),
        }
    }

    fn s(st: &EditorState, c: Vec2) -> Vec2 {
        st.viewport.canvas_to_screen(c)
    }

    #[test]
    fn frame_corners_identity_scaled_and_rotated() {
        let f = Frame::of(&Transform::at(10.0, 20.0), SRC);
        assert_eq!(
            f.corners(),
            [
                Vec2::new(10.0, 20.0),
                Vec2::new(50.0, 20.0),
                Vec2::new(50.0, 40.0),
                Vec2::new(10.0, 40.0)
            ]
        );
        // Scaled 2× / 0.5×: center at translate + scaled half size.
        let f = Frame::of(&t(0.0, 0.0, 2.0, 0.5, 0.0), SRC);
        assert_close(f.center, Vec2::new(40.0, 5.0));
        assert_close(f.corner(Corner::BottomRight), Vec2::new(80.0, 10.0));
        // 90° clockwise about the center (20, 10): the top-left corner
        // (offset −20, −10) goes to the top-right of the turned box.
        let f = Frame::of(&t(0.0, 0.0, 1.0, 1.0, 90.0), SRC);
        assert_close(f.corner(Corner::TopLeft), Vec2::new(30.0, -10.0));
        assert_close(f.corner(Corner::TopRight), Vec2::new(30.0, 30.0));
        assert_close(f.corner(Corner::BottomRight), Vec2::new(10.0, 30.0));
        assert_close(f.corner(Corner::BottomLeft), Vec2::new(10.0, -10.0));
        // 45° scaled: corners stay at the half-diagonal distance.
        let f = Frame::of(&t(5.0, 5.0, 1.5, 3.0, 45.0), SRC);
        let r = (30.0f32).hypot(30.0);
        for c in f.corners() {
            assert!((dist(c, f.center) - r).abs() < EPS);
        }
        // A flipped scale gives the same box.
        let a = Frame::of(&t(0.0, 0.0, 1.0, 1.0, 0.0), SRC);
        let b = Frame::of(&t(40.0, 0.0, -1.0, 1.0, 0.0), SRC);
        assert_close(a.center, b.center);
        assert_eq!(a.half, b.half);
    }

    #[test]
    fn frame_matches_rp_doc_flatten_placement() {
        // A 2×1 red source rotated 90° about its center at (2.5, 2) covers
        // canvas pixels (2, 1) and (2, 2) in rp-doc's flatten; the frame
        // must cover the same pixel centers.
        let mut doc = Document::new(Size::new(4, 4));
        let hash = SourceHash::new("r");
        doc.push_layer(Layer::image("r", hash.clone(), t(1.5, 1.5, 1.0, 1.0, 90.0)));
        let mut sources = rp_doc::SourceStore::new();
        sources.insert(hash, RgbaImage::filled(Size::new(2, 1), [255, 0, 0, 255]));
        let flat = doc.flatten(&sources);
        let f = Frame::of(&t(1.5, 1.5, 1.0, 1.0, 90.0), Size::new(2, 1));
        for y in 0..4 {
            for x in 0..4 {
                let inside = f.contains(Vec2::new(x as f32 + 0.5, y as f32 + 0.5));
                assert_eq!(flat.get(x, y)[3] > 0, inside, "pixel ({x}, {y})");
            }
        }
    }

    #[test]
    fn contains_and_local_round_trip() {
        let f = Frame::of(&t(10.0, 10.0, 2.0, 1.0, 30.0), SRC);
        assert!(f.contains(f.center));
        let p = Vec2::new(3.0, -7.0);
        assert_close(f.to_local(f.to_canvas(p)), p);
        let outside = f.to_canvas(Vec2::new(f.half.x + 1.0, 0.0));
        assert!(!f.contains(outside));
    }

    #[test]
    fn handles_hit_in_screen_space_at_several_zooms() {
        for zoom in [0.25, 0.5, 1.0, 4.0, 16.0] {
            let mut vp = Viewport::new(200, 200);
            vp.set_view_size(400.0, 300.0);
            vp.zoom_about(zoom, Vec2::new(123.0, 77.0));
            let f = Frame::of(&t(20.0, 30.0, 2.0, 2.0, 30.0), SRC);
            let h = HandleLayout::new(&f, &vp);
            for (c, p) in Corner::ALL.into_iter().zip(h.corners) {
                assert_close(p, vp.canvas_to_screen(f.corner(c)));
                // Within the screen radius, whatever the zoom.
                let near = Vec2::new(p.x + HANDLE_HIT_RADIUS * 0.7, p.y);
                assert_eq!(h.hit(near), Some(Handle::Corner(c)), "zoom {zoom}");
                let far = Vec2::new(p.x + HANDLE_HIT_RADIUS * 1.5, p.y);
                assert_ne!(h.hit(far), Some(Handle::Corner(c)), "zoom {zoom}");
            }
            // The rotation handle sits a fixed screen distance above the top
            // edge, along the box's own "up".
            assert!((dist(h.rotate, h.top_mid) - ROTATE_HANDLE_OFFSET).abs() < EPS);
            let up = rotate(Vec2::new(0.0, -1.0), f.rotation);
            assert_close(
                h.rotate,
                Vec2::new(
                    h.top_mid.x + up.x * ROTATE_HANDLE_OFFSET,
                    h.top_mid.y + up.y * ROTATE_HANDLE_OFFSET,
                ),
            );
            assert_eq!(h.hit(h.rotate), Some(Handle::Rotate));
            assert_eq!(h.hit(vp.canvas_to_screen(f.center)), None);
        }
    }

    #[test]
    fn rotation_handle_wins_and_nearest_corner_wins() {
        // A box one screen pixel tall: corners overlap, rotate is above.
        let mut vp = Viewport::new(200, 200);
        vp.set_view_size(400.0, 300.0);
        let f = Frame::of(&t(10.0, 10.0, 1.0, 0.05, 0.0), SRC);
        let h = HandleLayout::new(&f, &vp);
        let tl = h.corners[0];
        assert_eq!(
            h.hit(Vec2::new(tl.x, tl.y - 0.4)),
            Some(Handle::Corner(Corner::TopLeft))
        );
        assert_eq!(
            h.hit(Vec2::new(tl.x, tl.y + 1.4)),
            Some(Handle::Corner(Corner::BottomLeft))
        );
        // Tiny box: the rotation handle overlaps nothing but is checked first.
        let f = Frame::of(&t(10.0, 10.0, 0.01, 0.01, 0.0), SRC);
        let mut h = HandleLayout::new(&f, &vp);
        h.rotate = h.corners[0];
        assert_eq!(h.hit(h.corners[0]), Some(Handle::Rotate));
    }

    #[test]
    fn scale_keeps_the_opposite_corner_fixed() {
        for deg in [0.0, 30.0, 90.0, -135.0] {
            for corner in Corner::ALL {
                for keep in [true, false] {
                    let t0 = t(15.0, 25.0, 1.5, 0.75, deg);
                    let f0 = Frame::of(&t0, SRC);
                    let anchor = f0.corner(corner.opposite());
                    let dragged = f0.corner(corner);
                    let p = Vec2::new(dragged.x + 13.0, dragged.y - 4.0);
                    let t1 = scale_from_corner(t0, SRC, corner, p, keep);
                    let f1 = Frame::of(&t1, SRC);
                    assert_close(f1.corner(corner.opposite()), anchor);
                    assert!((t1.rotation - t0.rotation).abs() < EPS);
                    if keep {
                        assert!(
                            (t1.scale_x / t1.scale_y - t0.scale_x / t0.scale_y).abs() < EPS,
                            "aspect kept"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn free_scale_puts_the_corner_under_the_pointer() {
        let t0 = t(0.0, 0.0, 1.0, 1.0, 30.0);
        let f0 = Frame::of(&t0, SRC);
        let p = f0.to_canvas(Vec2::new(40.0, 25.0)); // past the bottom-right
        let t1 = scale_from_corner(t0, SRC, Corner::BottomRight, p, false);
        assert_close(Frame::of(&t1, SRC).corner(Corner::BottomRight), p);
        // 40 + 20 = 60 wide, 10 + 25 = 35 tall.
        assert!((t1.scale_x - 60.0 / 40.0).abs() < EPS);
        assert!((t1.scale_y - 35.0 / 20.0).abs() < EPS);
    }

    #[test]
    fn aspect_scale_on_the_diagonal_is_exact_and_never_flips() {
        let t0 = t(0.0, 0.0, 1.0, 1.0, 0.0);
        let t1 = scale_from_corner(t0, SRC, Corner::BottomRight, Vec2::new(80.0, 40.0), true);
        assert!((t1.scale_x - 2.0).abs() < EPS && (t1.scale_y - 2.0).abs() < EPS);
        assert_close(Vec2::new(t1.translate_x, t1.translate_y), Vec2::default());
        // Dragging past the anchor clamps to the minimum size, no flip.
        for keep in [true, false] {
            let t2 = scale_from_corner(t0, SRC, Corner::BottomRight, Vec2::new(-50.0, -50.0), keep);
            let f = Frame::of(&t2, SRC);
            assert!(t2.scale_x > 0.0 && t2.scale_y > 0.0);
            assert!(2.0 * f.half.x >= MIN_SCALED_SIZE - EPS);
            assert!(2.0 * f.half.y >= MIN_SCALED_SIZE - EPS);
            assert_close(f.corner(Corner::TopLeft), Vec2::default());
        }
        // A flipped image stays flipped.
        let flipped = t(40.0, 0.0, -1.0, 1.0, 0.0);
        let t3 = scale_from_corner(
            flipped,
            SRC,
            Corner::BottomRight,
            Vec2::new(80.0, 40.0),
            true,
        );
        assert!(t3.scale_x < 0.0);
        assert_close(Frame::of(&t3, SRC).corner(Corner::TopLeft), Vec2::default());
    }

    #[test]
    fn rotation_is_about_the_center() {
        let t0 = t(10.0, 10.0, 2.0, 1.0, 0.0);
        let c = Frame::of(&t0, SRC).center;
        // Pointer goes from right of center to below it: +90° clockwise.
        let start = Vec2::new(c.x + 50.0, c.y);
        let p = Vec2::new(c.x, c.y + 30.0);
        let t1 = rotate_about_center(t0, c, start, p, false);
        assert!((t1.rotation - PI / 2.0).abs() < EPS);
        assert_close(Frame::of(&t1, SRC).center, c);
        assert_eq!((t1.translate_x, t1.scale_x), (t0.translate_x, t0.scale_x));
    }

    #[test]
    fn rotation_snaps_to_15_degrees_with_shift() {
        let t0 = t(0.0, 0.0, 1.0, 1.0, 10.0);
        let c = Frame::of(&t0, SRC).center;
        let start = Vec2::new(c.x + 10.0, c.y);
        let a = 12.0f32.to_radians();
        let p = Vec2::new(c.x + 10.0 * a.cos(), c.y + 10.0 * a.sin());
        let free = rotate_about_center(t0, c, start, p, false);
        assert!((free.rotation.to_degrees() - 22.0).abs() < 1e-2);
        let snapped = rotate_about_center(t0, c, start, p, true);
        assert!((snapped.rotation.to_degrees() - 15.0).abs() < 1e-3);
        assert!((snap_angle(7.6f32.to_radians()).to_degrees() - 15.0).abs() < 1e-3);
        assert!((snap_angle(-7.4f32.to_radians())).abs() < 1e-6);
        assert!((snap_angle(PI - 0.01) - PI).abs() < 1e-5);
        assert!((normalize_angle(3.0 * PI) - PI).abs() < 1e-5);
        assert!((normalize_angle(-PI / 2.0 - 2.0 * PI) + PI / 2.0).abs() < 1e-5);
    }

    #[test]
    fn nudge_keys() {
        assert_eq!(arrow_nudge("left", false), Some((-1, 0)));
        assert_eq!(arrow_nudge("right", true), Some((10, 0)));
        assert_eq!(arrow_nudge("up", true), Some((0, -10)));
        assert_eq!(arrow_nudge("down", false), Some((0, 1)));
        assert_eq!(arrow_nudge("a", false), None);
    }

    #[test]
    fn nudge_moves_paint_and_image_layers_and_bumps_version() {
        let (mut st, paint, img) = editor();
        let v = st.doc_version;
        assert!(st.nudge_selected(10, -1));
        assert_eq!(st.doc_version, v + 1);
        assert_eq!(transform_of(&st, img), Transform::at(60.0, 59.0));
        st.selected_layer = Some(paint);
        assert!(st.nudge_selected(-1, 0));
        assert_eq!(offset_of(&st, paint), (-1, 0));
        assert_eq!(st.doc_version, v + 2);
    }

    #[test]
    fn position_lock_refuses_nudge_and_drags() {
        let (mut st, _, img) = editor();
        st.toggle_lock_position(img);
        let before = st.document.clone();
        let v = st.doc_version;
        assert!(!st.nudge_selected(1, 0));
        assert!(st.status.contains("position is locked"), "{}", st.status);
        let center = s(&st, Vec2::new(70.0, 70.0));
        assert!(!st.begin_transform(center));
        assert!(st.status.contains("Can't move"), "{}", st.status);
        let h = st.transform_handles().unwrap();
        assert!(!st.begin_transform(h.corners[2]));
        assert!(st.status.contains("Can't scale"), "{}", st.status);
        assert!(!st.begin_transform(h.rotate));
        assert!(st.status.contains("Can't rotate"), "{}", st.status);
        assert!(!st.continue_transform(Vec2::new(0.0, 0.0), false));
        assert_eq!(st.document, before);
        assert_eq!(st.doc_version, v);
        // Pixel lock does not stop moving.
        st.toggle_lock_position(img);
        st.document.layer_mut(img).unwrap().lock.pixels = true;
        assert!(st.nudge_selected(1, 0));
    }

    #[test]
    fn no_selection_refuses() {
        let (mut st, _, _) = editor();
        st.selected_layer = None;
        assert!(!st.begin_transform(Vec2::new(10.0, 10.0)));
        assert!(!st.nudge_selected(1, 1));
        assert!(st.status.contains("no layer"), "{}", st.status);
    }

    #[test]
    fn move_drag_paint_layer_by_whole_pixels_one_bump_per_event() {
        let (mut st, paint, _) = editor();
        st.selected_layer = Some(paint);
        st.tool = Tool::Move;
        st.viewport.zoom_about(4.0, Vec2::default());
        let start = s(&st, Vec2::new(10.0, 10.0));
        assert!(st.begin_transform(start));
        let v = st.doc_version;
        // A quarter canvas pixel: rounds to zero, no change, no bump.
        assert!(!st.continue_transform(Vec2::new(start.x + 1.0, start.y), false));
        assert_eq!(st.doc_version, v);
        assert!(st.continue_transform(Vec2::new(start.x + 10.0, start.y - 8.0), false));
        assert_eq!(offset_of(&st, paint), (3, -2)); // 2.5 rounds away, −2
        assert_eq!(st.doc_version, v + 1);
        assert!(st.continue_transform(Vec2::new(start.x + 40.0, start.y + 40.0), false));
        assert_eq!(offset_of(&st, paint), (10, 10));
        assert_eq!(st.doc_version, v + 2);
        // Back to the start: the layer returns exactly.
        assert!(st.continue_transform(start, false));
        assert_eq!(offset_of(&st, paint), (0, 0));
        assert_eq!(
            st.transform_drag.clone().unwrap().before,
            StartPlacement::Offset(0, 0)
        );
        st.end_transform();
        assert!(st.transform_drag.is_none());
        assert!(!st.continue_transform(Vec2::new(300.0, 300.0), false));
        assert_eq!(offset_of(&st, paint), (0, 0));
    }

    #[test]
    fn move_drag_image_layer_keeps_fraction_and_rounds_delta() {
        let (mut st, _, img) = editor();
        st.tool = Tool::Move;
        st.document
            .layer_mut(img)
            .unwrap()
            .set_transform(t(50.25, 60.0, 1.0, 1.0, 0.0))
            .unwrap();
        // The Move tool moves even from a corner (no handles).
        assert!(st.transform_handles().is_none());
        let start = s(&st, Vec2::new(50.25, 60.0));
        assert!(st.begin_transform(start));
        assert_eq!(st.transform_drag.clone().unwrap().kind, DragKind::Move);
        st.continue_transform(Vec2::new(start.x + 7.4, start.y + 2.6), false);
        let t1 = transform_of(&st, img);
        assert_eq!((t1.translate_x, t1.translate_y), (57.25, 63.0));
        assert!(st.status.starts_with("Move 7, 3"), "{}", st.status);
    }

    #[test]
    fn select_drag_on_handles_scales_and_rotates() {
        let (mut st, _, img) = editor();
        st.viewport.zoom_about(2.0, Vec2::new(30.0, 40.0));
        let h = st.transform_handles().unwrap();
        // Bottom-right corner (90, 80) dragged to canvas (130, 100): 2×.
        assert!(st.begin_transform(h.corners[2]));
        assert_eq!(
            st.transform_drag.clone().unwrap().kind,
            DragKind::Scale(Corner::BottomRight)
        );
        let v = st.doc_version;
        assert!(st.continue_transform(s(&st, Vec2::new(130.0, 100.0)), false));
        assert_eq!(st.doc_version, v + 1);
        let t1 = transform_of(&st, img);
        assert!((t1.scale_x - 2.0).abs() < EPS && (t1.scale_y - 2.0).abs() < EPS);
        assert_close(
            Vec2::new(t1.translate_x, t1.translate_y),
            Vec2::new(50.0, 60.0),
        );
        // Shift frees the aspect ratio.
        assert!(st.continue_transform(s(&st, Vec2::new(130.0, 70.0)), true));
        let t2 = transform_of(&st, img);
        assert!((t2.scale_x - 2.0).abs() < EPS && (t2.scale_y - 0.5).abs() < EPS);
        st.end_transform();

        // Rotate: from the handle (straight above the center) to the right
        // of the center is +90°.
        let h = st.transform_handles().unwrap();
        let center = Frame::of(&transform_of(&st, img), SRC).center;
        assert!(st.begin_transform(h.rotate));
        assert_eq!(st.transform_drag.clone().unwrap().kind, DragKind::Rotate);
        st.continue_transform(s(&st, Vec2::new(center.x + 100.0, center.y + 3.0)), true);
        let t3 = transform_of(&st, img);
        assert!((t3.rotation - PI / 2.0).abs() < EPS, "{}", t3.rotation);
        assert_close(Frame::of(&t3, SRC).center, center);
        st.end_transform();

        // Inside the box (not on a handle) moves.
        let inside = s(&st, center);
        assert!(st.begin_transform(inside));
        assert_eq!(st.transform_drag.clone().unwrap().kind, DragKind::Move);
    }

    #[test]
    fn handles_only_for_select_on_loaded_image_layers() {
        let (mut st, paint, img) = editor();
        assert!(st.transform_handles().is_some());
        st.tool = Tool::Move;
        assert!(st.transform_handles().is_none());
        st.tool = Tool::Select;
        st.selected_layer = Some(paint);
        assert!(st.transform_handles().is_none());
        // Select on a paint layer still moves it.
        assert!(st.begin_transform(Vec2::new(5.0, 5.0)));
        assert!(st.continue_transform(Vec2::new(8.0, 5.0), false));
        assert_eq!(offset_of(&st, paint), (3, 0));
        st.end_transform();
        st.selected_layer = Some(img);
        Arc::make_mut(&mut st.sources).remove(&SourceHash::new("abc"));
        assert!(st.transform_handles().is_none());
        st.tool = Tool::Brush;
        assert!(!st.begin_transform(Vec2::new(5.0, 5.0)));
    }

    #[test]
    fn pointer_moved_drives_the_drag_with_shift_state() {
        let (mut st, _, img) = editor();
        let h = st.transform_handles().unwrap();
        st.pointer_moved(h.rotate);
        assert!(st.begin_transform(h.rotate));
        let center = Frame::of(&transform_of(&st, img), SRC).center;
        st.shift_held = true;
        let a = 50.0f32.to_radians(); // 50° from up → snaps to 45°
        st.pointer_moved(s(
            &st,
            Vec2::new(center.x + 40.0 * a.sin(), center.y - 40.0 * a.cos()),
        ));
        let r = transform_of(&st, img).rotation.to_degrees();
        assert!((r - 45.0).abs() < 1e-3, "{r}");
        // Releasing shift without moving re-applies the free angle.
        let v = st.doc_version;
        st.set_shift(false);
        let r = transform_of(&st, img).rotation.to_degrees();
        assert!((r - 50.0).abs() < 1e-2, "{r}");
        assert_eq!(st.doc_version, v + 1);
        st.set_shift(false);
        assert_eq!(st.doc_version, v + 1);
    }

    /// A 200×200 mask that is 0 in the 10×10 square at (20, 20), 255 else.
    fn square_mask() -> GrayImage {
        let mut m = GrayImage::filled(Size::new(200, 200), 255);
        for y in 20..30 {
            for x in 20..30 {
                m.put(x, y, 0);
            }
        }
        m
    }

    fn mask_of(st: &EditorState, id: LayerId) -> &GrayImage {
        st.document.layer(id).unwrap().mask.as_ref().unwrap()
    }

    #[test]
    fn move_drag_shifts_the_mask_with_white_padding_and_one_undo() {
        for paint_layer in [true, false] {
            let (mut st, paint, img) = editor();
            let id = if paint_layer { paint } else { img };
            st.selected_layer = Some(id);
            st.tool = Tool::Move;
            st.document.layer_mut(id).unwrap().mask = Some(square_mask());
            let before = st.document.clone();
            let start = s(&st, Vec2::new(25.0, 25.0));
            assert!(st.begin_transform(start));
            // Far left first: the square leaves the canvas mid-drag...
            st.continue_transform(Vec2::new(start.x - 100.0, start.y), false);
            assert!(mask_of(&st, id).as_raw().iter().all(|&v| v == 255));
            // ...and comes back intact, since the drag shifts the mask from
            // the press.
            st.continue_transform(Vec2::new(start.x + 5.0, start.y - 3.0), false);
            st.end_transform();
            let m = mask_of(&st, id);
            assert_eq!(m.size(), Size::new(200, 200));
            assert_eq!(m.get(25, 17), 0);
            assert_eq!(m.get(34, 26), 0);
            assert_eq!(m.get(24, 17), 255);
            assert_eq!(m.get(25, 27), 255);
            assert_eq!(m, &shifted_mask(&square_mask(), 5, -3));
            assert_eq!(st.history.undo_len(), 1);
            st.undo();
            assert_eq!(st.document, before);
            st.redo();
            assert_eq!(mask_of(&st, id), &shifted_mask(&square_mask(), 5, -3));
        }
    }

    #[test]
    fn nudges_shift_the_mask_and_lose_what_leaves_the_canvas() {
        let (mut st, paint, img) = editor();
        st.tool = Tool::Move;
        for id in [paint, img] {
            st.selected_layer = Some(id);
            st.document.layer_mut(id).unwrap().mask = Some(square_mask());
            assert!(st.nudge_selected(10, 0));
            assert!(st.nudge_selected(0, -1));
            assert_eq!(mask_of(&st, id), &shifted_mask(&square_mask(), 10, -1));
            // Pushed off the left edge and back: the square is gone.
            st.nudge_selected(-40, 0);
            st.nudge_selected(30, 0);
            assert!(mask_of(&st, id).as_raw().iter().all(|&v| v == 255));
        }
        // A position lock refuses the nudge and leaves the mask alone.
        st.document.layer_mut(img).unwrap().mask = Some(square_mask());
        st.document.layer_mut(img).unwrap().lock.position = true;
        assert!(!st.nudge_selected(5, 5));
        assert_eq!(mask_of(&st, img), &square_mask());
    }

    #[test]
    fn scale_and_rotate_leave_the_mask_alone() {
        let (mut st, _, img) = editor();
        st.document.layer_mut(img).unwrap().mask = Some(square_mask());
        let h = st.transform_handles().unwrap();
        assert!(st.begin_transform(h.corners[2]));
        assert!(
            st.continue_transform(Vec2::new(h.corners[2].x + 30.0, h.corners[2].y + 9.0), true)
        );
        st.end_transform();
        assert!(st.transform_drag.is_none());
        let h = st.transform_handles().unwrap();
        assert!(st.begin_transform(h.rotate));
        assert_eq!(st.transform_drag.clone().unwrap().kind, DragKind::Rotate);
        assert!(st.continue_transform(Vec2::new(h.rotate.x + 40.0, h.rotate.y + 15.0), false));
        st.end_transform();
        assert_ne!(transform_of(&st, img), Transform::at(50.0, 60.0));
        assert_eq!(mask_of(&st, img), &square_mask());
    }
}
