//! Layer-mask editing on [`EditorState`]: the edit target (layer pixels or
//! the layer's mask), mask dabs, shifting a mask with its layer, and the
//! Layers panel's mask thumbnail. Pure; the GPUI panel only calls these.
//!
//! Decisions:
//! - The edit target belongs to the selected layer: clicking a row's mask
//!   thumbnail (or MASK badge, or the panel's Edit Mask toggle) selects that
//!   layer and targets its mask; clicking the thumbnail again, or the Layer
//!   toggle, goes back to the pixels. Selecting another layer, adding,
//!   duplicating, deleting or inserting a layer, New and Open all go back to
//!   the Layer target. Removing the targeted mask (or undoing its Add Mask)
//!   does too. The status bar shows "Editing mask" while the mask is the
//!   target.
//! - With the mask targeted, Brush (left button) paints black (hides),
//!   Brush right button and Eraser (either button) paint white (reveal). A
//!   dab moves each mask value toward black or white by the brush mask's
//!   coverage, so soft brushes feather. Masks are canvas-space, so strokes
//!   ignore the layer's offset or transform; image layers' masks paint the
//!   same way and their pixels are never touched.
//! - A pixel lock does not lock the mask (as in Photoshop): masks paint on
//!   pixel-locked layers. Position locks don't matter to strokes.
//! - One mask stroke is one undo step ("Mask Stroke"), a snapshot step:
//!   only the stroked mask is copied (everything else is copy-on-write).
//! - The Picker ignores the target and always picks the flattened color.
//! - Moving a layer (Move/Select drag or arrow nudge) shifts its mask by
//!   the same whole-pixel delta. The mask stays canvas-sized: values shifted
//!   off the canvas are lost and the uncovered edge is padded with white
//!   (255, showing). A drag shifts the mask from where it was at the press,
//!   so dragging away and back within one drag loses nothing; each nudge
//!   shifts the current mask. Scaling and rotating an image layer leave the
//!   mask where it is (v1).
//! - The thumbnail is the mask box-averaged down to at most
//!   [`THUMB_SIDE`]² (aspect kept), recomputed only when the document
//!   version changes.

use rp_doc::{GrayImage, LayerId};

use crate::state::EditorState;

/// What strokes on the selected layer change.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EditTarget {
    /// The layer's pixels.
    #[default]
    Layer,
    /// The layer's mask.
    Mask,
}

/// Mask value Brush paints (hides the layer).
pub const MASK_HIDE: u8 = 0;
/// Mask value Eraser and right-click paint (shows the layer).
pub const MASK_SHOW: u8 = 255;

/// Longest side of a mask thumbnail, in pixels.
pub const THUMB_SIDE: u32 = 40;

/// Mask dabs: the fast, byte-identical [`rp_brush::mask_dab`] (moves mask
/// values under one dab toward `value` by coverage × opacity).
pub use rp_brush::mask_dab;

/// `mask` moved by `(dx, dy)` pixels, same size: values moved past an edge
/// are dropped, the uncovered area is [`MASK_SHOW`].
pub fn shifted_mask(mask: &GrayImage, dx: i32, dy: i32) -> GrayImage {
    if (dx, dy) == (0, 0) {
        return mask.clone();
    }
    let size = mask.size();
    let (w, h) = (size.width as i64, size.height as i64);
    let (dx, dy) = (dx as i64, dy as i64);
    let mut out = vec![MASK_SHOW; size.area()];
    let src = mask.as_raw();
    // Destination columns that receive a source column.
    let x0 = dx.clamp(0, w);
    let x1 = (w + dx).clamp(0, w);
    if x0 < x1 {
        for y in 0..h {
            let sy = y - dy;
            if sy < 0 || sy >= h {
                continue;
            }
            let d = (y * w) as usize;
            let s = (sy * w) as usize;
            out[d + x0 as usize..d + x1 as usize]
                .copy_from_slice(&src[s + (x0 - dx) as usize..s + (x1 - dx) as usize]);
        }
    }
    GrayImage::from_raw(size, out).expect("same size")
}

/// A small grayscale preview of a mask.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MaskThumb {
    pub width: u32,
    pub height: u32,
    /// `width × height` gray values, row-major.
    pub gray: Vec<u8>,
}

impl MaskThumb {
    /// Opaque BGRA for a GPUI image.
    pub fn to_bgra(&self) -> Vec<u8> {
        self.gray.iter().flat_map(|&g| [g, g, g, 255]).collect()
    }
}

/// `mask` scaled to fit `side × side` (aspect kept, at least 1×1), each
/// thumbnail pixel the mean of up to 4×4 samples spread over its source
/// box, so thin strokes still show and big masks stay cheap.
pub fn mask_thumbnail(mask: &GrayImage, side: u32) -> MaskThumb {
    let size = mask.size();
    let (sw, sh) = (size.width.max(1), size.height.max(1));
    let side = side.max(1);
    let (tw, th) = if sw >= sh {
        (
            side.min(sw),
            ((sh as u64 * side as u64) / sw as u64).max(1) as u32,
        )
    } else {
        (
            ((sw as u64 * side as u64) / sh as u64).max(1) as u32,
            side.min(sh),
        )
    };
    let (tw, th) = (tw.min(sw), th.min(sh));
    let src = mask.as_raw();
    let mut gray = Vec::with_capacity((tw * th) as usize);
    if src.is_empty() {
        gray.resize((tw * th) as usize, MASK_SHOW);
        return MaskThumb {
            width: tw,
            height: th,
            gray,
        };
    }
    const N: u64 = 4;
    for ty in 0..th as u64 {
        let (y0, y1) = (ty * sh as u64 / th as u64, (ty + 1) * sh as u64 / th as u64);
        for tx in 0..tw as u64 {
            let (x0, x1) = (tx * sw as u64 / tw as u64, (tx + 1) * sw as u64 / tw as u64);
            let (mut sum, mut n) = (0u64, 0u64);
            for j in 0..N {
                let y = y0 + (y1 - y0) * (2 * j + 1) / (2 * N);
                for i in 0..N {
                    let x = x0 + (x1 - x0) * (2 * i + 1) / (2 * N);
                    sum += src[(y * sw as u64 + x) as usize] as u64;
                    n += 1;
                }
            }
            gray.push(((sum + n / 2) / n) as u8);
        }
    }
    MaskThumb {
        width: tw,
        height: th,
        gray,
    }
}

impl EditorState {
    /// What strokes on the selected layer change: its mask only while the
    /// mask is targeted and still exists.
    pub fn edit_target(&self) -> EditTarget {
        match (self.mask_target, self.selected()) {
            (Some(t), Some(l)) if t == l.id() && l.mask.is_some() => EditTarget::Mask,
            _ => EditTarget::Layer,
        }
    }

    /// The target of layer `id` as the Layers panel shows it.
    pub fn edit_target_of(&self, id: LayerId) -> EditTarget {
        if self.selected_layer == Some(id) {
            self.edit_target()
        } else {
            EditTarget::Layer
        }
    }

    /// Selects layer `id` and edits its pixels or its mask. Targeting a
    /// layer without a mask is refused (the target stays Layer). Returns
    /// whether `id` exists.
    pub fn set_edit_target(&mut self, id: LayerId, target: EditTarget) -> bool {
        let Some(layer) = self.document.layer(id) else {
            return false;
        };
        let has_mask = layer.mask.is_some();
        let name = layer.name.clone();
        self.settle_float();
        self.select_layer(id);
        match target {
            EditTarget::Mask if has_mask => {
                self.mask_target = Some(id);
                self.status = format!("Editing mask of {name}");
            }
            EditTarget::Mask => {
                self.mask_target = None;
                self.status = format!("{name} has no mask");
            }
            EditTarget::Layer => {
                let was = self.mask_target.take().is_some();
                if was {
                    self.status = format!("Editing {name}");
                }
            }
        }
        true
    }

    /// A click on layer `id`'s mask thumbnail: targets its mask, or goes
    /// back to its pixels if the mask already was the target.
    pub fn toggle_mask_target(&mut self, id: LayerId) -> bool {
        let next = match self.edit_target_of(id) {
            EditTarget::Mask => EditTarget::Layer,
            EditTarget::Layer => EditTarget::Mask,
        };
        self.set_edit_target(id, next)
    }

    /// Drops a mask target that no longer applies (other layer selected,
    /// mask gone).
    pub(crate) fn fix_edit_target(&mut self) {
        if self.edit_target() == EditTarget::Layer {
            self.mask_target = None;
        }
    }

    /// Status-bar chip: "Editing mask" while the mask is the target.
    pub fn edit_target_label(&self) -> Option<&'static str> {
        (self.edit_target() == EditTarget::Mask).then_some("Editing mask")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rp_brush::Brush;
    use rp_doc::Size;

    fn gray(size: Size, f: impl Fn(u32, u32) -> u8) -> GrayImage {
        let data = (0..size.height)
            .flat_map(|y| (0..size.width).map(move |x| (x, y)))
            .map(|(x, y)| f(x, y))
            .collect();
        GrayImage::from_raw(size, data).unwrap()
    }

    #[test]
    fn hard_dab_sets_value_and_clips() {
        let s = Size::new(10, 10);
        let mut m = GrayImage::filled(s, 255);
        let brush = Brush::round_hard(3.0);
        mask_dab(m.as_raw_mut(), 10, 10, &brush, 0.5, 0.5, 0, 1.0);
        assert_eq!(m.get(0, 0), 0);
        assert_eq!(m.get(5, 5), 255);
        mask_dab(m.as_raw_mut(), 10, 10, &brush, 0.5, 0.5, 255, 1.0);
        assert_eq!(m.get(0, 0), 255);
        // Half opacity goes half way.
        mask_dab(m.as_raw_mut(), 10, 10, &brush, 5.5, 5.5, 0, 0.5);
        assert_eq!(m.get(5, 5), 128);
    }

    #[test]
    fn shift_moves_values_and_pads_white() {
        let s = Size::new(4, 3);
        let m = gray(s, |x, y| (y * 4 + x) as u8);
        let r = shifted_mask(&m, 1, 1);
        assert_eq!(r.size(), s);
        assert_eq!(r.get(0, 0), 255);
        assert_eq!(r.get(3, 0), 255);
        assert_eq!(r.get(0, 2), 255);
        assert_eq!(r.get(1, 1), 0);
        assert_eq!(r.get(3, 2), m.get(2, 1));
        let l = shifted_mask(&m, -2, 0);
        assert_eq!(l.get(0, 0), 2);
        assert_eq!(l.get(1, 2), 11);
        assert_eq!(l.get(2, 0), 255);
        // Shifted fully off: all white.
        let gone = shifted_mask(&m, 0, -3);
        assert!(gone.as_raw().iter().all(|&v| v == 255));
        let gone = shifted_mask(&m, 9, 0);
        assert!(gone.as_raw().iter().all(|&v| v == 255));
        // Zero shift shares the buffer.
        assert!(shifted_mask(&m, 0, 0).shares_buffer(&m));
    }

    #[test]
    fn thumbnail_keeps_aspect_and_averages() {
        let m = gray(Size::new(800, 1280), |x, _| if x < 400 { 0 } else { 255 });
        let t = mask_thumbnail(&m, THUMB_SIDE);
        assert_eq!((t.width, t.height), (25, 40));
        assert_eq!(t.gray.len(), 25 * 40);
        assert_eq!(t.gray[0], 0);
        assert_eq!(t.gray[24], 255);
        let wide = mask_thumbnail(&GrayImage::filled(Size::new(1280, 800), 7), 40);
        assert_eq!((wide.width, wide.height), (40, 25));
        assert!(wide.gray.iter().all(|&g| g == 7));
        let tiny = mask_thumbnail(&GrayImage::filled(Size::new(3, 2), 9), 40);
        assert_eq!((tiny.width, tiny.height), (3, 2));
        assert_eq!(tiny.to_bgra()[..4], [9, 9, 9, 255]);
    }

    mod editor {
        use super::super::*;
        use crate::paint_ops::{BrushTip, PaintButton};
        use crate::undo_ops::EditLabel;
        use crate::viewport::Vec2;
        use rp_doc::{Document, Layer, LayerKind, RgbaImage, Size, SourceHash, Tool, Transform};
        use std::sync::Arc;

        const S: Size = Size::new(64, 48);

        /// A paint layer (red, opaque) under an image layer (blue source
        /// at (8, 8)), both masked; the image layer is selected.
        fn editor() -> (EditorState, LayerId, LayerId) {
            let mut st = EditorState::new();
            let mut doc = Document::new(S);
            let mut paint = Layer::paint("Paint", S);
            if let LayerKind::Paint(p) = &mut paint.kind {
                p.pixels = RgbaImage::filled(S, [255, 0, 0, 255]);
            }
            let paint = doc.push_layer(paint);
            let hash = SourceHash::new("blue");
            let img = doc.push_layer(Layer::image("Img", hash.clone(), Transform::at(8.0, 8.0)));
            st.set_document(doc);
            Arc::make_mut(&mut st.sources)
                .insert(hash, RgbaImage::filled(Size::new(40, 30), [0, 0, 255, 255]));
            st.add_mask(paint);
            st.add_mask(img);
            st.history.clear();
            st.tool = Tool::Brush;
            (st, paint, img)
        }

        fn mask(st: &EditorState, id: LayerId) -> &GrayImage {
            st.document.layer(id).unwrap().mask.as_ref().unwrap()
        }

        fn stroke(st: &mut EditorState, from: (f32, f32), to: (f32, f32), b: PaintButton) {
            assert!(st.begin_stroke(Vec2::new(from.0, from.1), b));
            st.continue_stroke(Vec2::new(to.0, to.1));
            st.end_stroke();
        }

        #[test]
        fn target_switching_and_highlight_state() {
            let (mut st, paint, img) = editor();
            assert_eq!(st.edit_target(), EditTarget::Layer);
            assert_eq!(st.edit_target_label(), None);
            // Clicking the paint layer's thumbnail selects it and targets
            // its mask.
            assert!(st.toggle_mask_target(paint));
            assert_eq!(st.selected_layer, Some(paint));
            assert_eq!(st.edit_target(), EditTarget::Mask);
            assert_eq!(st.edit_target_of(paint), EditTarget::Mask);
            assert_eq!(st.edit_target_of(img), EditTarget::Layer);
            assert_eq!(st.edit_target_label(), Some("Editing mask"));
            assert!(st.status.contains("Editing mask"), "{}", st.status);
            // Clicking it again goes back to the pixels.
            st.toggle_mask_target(paint);
            assert_eq!(st.edit_target(), EditTarget::Layer);
            // Selecting another layer resets the target; selecting the same
            // one keeps it.
            st.set_edit_target(paint, EditTarget::Mask);
            st.select_layer(paint);
            assert_eq!(st.edit_target(), EditTarget::Mask);
            st.select_layer(img);
            assert_eq!(st.edit_target(), EditTarget::Layer);
            st.select_layer(paint);
            assert_eq!(st.edit_target(), EditTarget::Layer, "not remembered");
            // A layer without a mask can't be targeted.
            let bare = st.add_paint_layer();
            assert!(st.set_edit_target(bare, EditTarget::Mask));
            assert_eq!(st.edit_target(), EditTarget::Layer);
            // Adding a layer and New reset it.
            st.set_edit_target(paint, EditTarget::Mask);
            st.add_paint_layer();
            assert_eq!(st.mask_target, None);
            st.set_edit_target(paint, EditTarget::Mask);
            let doc = st.document.clone();
            st.set_document(doc);
            assert_eq!(st.mask_target, None);
        }

        #[test]
        fn removing_the_targeted_mask_resets_the_target() {
            let (mut st, paint, img) = editor();
            st.set_edit_target(paint, EditTarget::Mask);
            assert!(st.remove_mask(paint));
            assert_eq!(st.edit_target(), EditTarget::Layer);
            assert_eq!(st.mask_target, None);
            // Undo brings the mask back but not the target.
            st.undo();
            assert_eq!(st.edit_target(), EditTarget::Layer);
            // Undoing Add Mask while it is the target also resets it.
            st.remove_mask(img);
            st.add_mask(img);
            st.set_edit_target(img, EditTarget::Mask);
            st.undo();
            assert_eq!(st.mask_target, None);
            st.redo();
            assert_eq!(st.edit_target(), EditTarget::Layer);
        }

        #[test]
        fn brush_hides_on_the_mask_only_and_is_one_undo_step() {
            let (mut st, paint, _) = editor();
            st.set_edit_target(paint, EditTarget::Mask);
            st.brush.set_size(5);
            let before = st.document.clone();
            let v = st.doc_version;
            stroke(&mut st, (10.5, 40.5), (30.5, 40.5), PaintButton::Primary);
            assert!(st.doc_version > v);
            let m = mask(&st, paint);
            assert_eq!(m.get(10, 40), 0);
            assert_eq!(m.get(30, 40), 0);
            assert_eq!(m.get(20, 30), 255);
            // Pixels untouched (still the same buffer).
            let pixels = |d: &Document| match &d.layer(paint).unwrap().kind {
                LayerKind::Paint(p) => p.pixels.clone(),
                _ => unreachable!(),
            };
            assert!(pixels(&st.document).shares_buffer(&pixels(&before)));
            // One step, labelled; undo restores the mask exactly.
            assert_eq!(st.history.undo_len(), 1);
            assert_eq!(st.history.undo_label(), Some(EditLabel::MaskStroke));
            let after = st.document.clone();
            st.undo();
            assert_eq!(st.document, before);
            assert_eq!(st.edit_target(), EditTarget::Mask, "target survives undo");
            st.redo();
            assert_eq!(st.document, after);
            // A second stroke is a second step.
            stroke(&mut st, (40.5, 5.5), (50.5, 5.5), PaintButton::Primary);
            assert_eq!(st.history.undo_len(), 2);
        }

        #[test]
        fn eraser_and_right_click_reveal() {
            let (mut st, paint, _) = editor();
            st.set_edit_target(paint, EditTarget::Mask);
            st.document.layer_mut(paint).unwrap().mask = Some(GrayImage::filled(S, 0));
            stroke(&mut st, (10.5, 10.5), (10.5, 10.5), PaintButton::Secondary);
            assert_eq!(mask(&st, paint).get(10, 10), 255);
            st.tool = Tool::Eraser;
            for (x, b) in [(30.5, PaintButton::Primary), (50.5, PaintButton::Secondary)] {
                stroke(&mut st, (x, 20.5), (x, 20.5), b);
                assert_eq!(mask(&st, paint).get(x as u32, 20), 255);
            }
            assert_eq!(mask(&st, paint).get(40, 40), 0);
            assert_eq!(st.history.undo_label(), Some(EditLabel::MaskStroke));
        }

        #[test]
        fn soft_brush_feathers_the_mask() {
            let (mut st, paint, _) = editor();
            st.set_edit_target(paint, EditTarget::Mask);
            st.brush.set_size(15);
            st.set_brush_tip(BrushTip::Soft);
            stroke(&mut st, (24.5, 24.5), (24.5, 24.5), PaintButton::Primary);
            let m = mask(&st, paint);
            assert!(m.get(24, 24) < 30, "center {}", m.get(24, 24));
            let edge = m.get(29, 24);
            assert!(edge > 0 && edge < 255, "edge {edge}");
            assert_eq!(m.get(24, 40), 255);
        }

        #[test]
        fn image_layer_masks_paint_and_pixels_stay() {
            let (mut st, _, img) = editor();
            st.set_edit_target(img, EditTarget::Mask);
            let kind = st.document.layer(img).unwrap().kind.clone();
            let flat0 = st.document.flatten(&st.sources);
            assert_eq!(flat0.get(20, 20), [0, 0, 255, 255]);
            st.brush.set_size(3);
            stroke(&mut st, (20.5, 20.5), (20.5, 20.5), PaintButton::Primary);
            assert_eq!(st.document.layer(img).unwrap().kind, kind);
            assert_eq!(mask(&st, img).get(20, 20), 0);
            // Flatten shows the red paint layer through the hidden spot.
            let flat = st.document.flatten(&st.sources);
            assert_eq!(flat.get(20, 20), [255, 0, 0, 255]);
            assert_eq!(flat.get(30, 30), [0, 0, 255, 255]);
            // Revealing brings the image back.
            st.tool = Tool::Eraser;
            stroke(&mut st, (20.5, 20.5), (20.5, 20.5), PaintButton::Primary);
            assert_eq!(
                st.document.flatten(&st.sources).get(20, 20),
                [0, 0, 255, 255]
            );
        }

        #[test]
        fn mask_strokes_ignore_offset_and_pixel_lock() {
            let (mut st, paint, _) = editor();
            {
                let l = st.document.layer_mut(paint).unwrap();
                l.translate_by(7, 3).unwrap();
                l.lock.pixels = true;
            }
            // Pixel-locked: the layer refuses, the mask paints.
            st.select_layer(paint);
            assert!(!st.begin_stroke(Vec2::new(5.5, 5.5), PaintButton::Primary));
            st.set_edit_target(paint, EditTarget::Mask);
            st.brush.set_size(1);
            stroke(&mut st, (5.5, 5.5), (5.5, 5.5), PaintButton::Primary);
            // Canvas space: (5, 5), not shifted by the offset.
            assert_eq!(mask(&st, paint).get(5, 5), 0);
            assert_eq!(mask(&st, paint).get(12, 8), 255);
        }

        #[test]
        fn picker_ignores_the_mask_target() {
            let (mut st, paint, _) = editor();
            st.set_edit_target(paint, EditTarget::Mask);
            st.tool = Tool::Picker;
            assert!(!st.begin_stroke(Vec2::new(5.5, 5.5), PaintButton::Primary));
            let bgra = [30u8, 20, 10, 255];
            let frame = crate::paint_ops::FrameView {
                width: 1,
                height: 1,
                bgra: &bgra,
            };
            assert!(st.pick_color(Some(frame), Vec2::new(0.5, 0.5), PaintButton::Primary));
            assert_eq!(st.document.fg, rp_palette_dither::Rgb::new(10, 20, 30));
        }
    }
}
