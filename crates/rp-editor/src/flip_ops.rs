//! Duplicate layer and Flip Horizontal / Flip Vertical on [`EditorState`],
//! plus the pure raster and transform flips they use.
//!
//! Decisions:
//! - **Duplicate** inserts a copy of the selected layer directly above it,
//!   named `<name> copy`, and selects it. Everything is copied (visibility,
//!   opacity, blend, locks, mask, offset or transform). Paint rasters and
//!   masks are shared copy-on-write, and an image copy references the same
//!   source, so a duplicate costs no memory until one of them is edited.
//!   Duplicate is never refused by a lock.
//! - **Flip** works on the selected layer, as seen on screen:
//!   - a paint layer mirrors its pixels about its own bounds (the raster
//!     stays where it is). A pixel lock refuses it; a position lock does not.
//!   - an image layer mirrors its transform about the image center: the
//!     scale on the flipped axis is negated and the rotation is negated, so
//!     a rotated image flips about the canvas axis, not its own. The center
//!     stays fixed. A position lock refuses it; a pixel lock does not.
//! - Masks are in canvas space. Flipping a paint layer flips its mask about
//!   the same bounds, so mask and pixels stay matched. Where the layer
//!   reaches past the mask (the canvas), mask values mirrored out are
//!   dropped and the area mirrored in becomes 255 (shows), which is what an
//!   absent mask pixel means. Mask pixels outside the layer's bounds do not
//!   change. Flipping an image layer leaves its mask alone (like a
//!   transform drag).
//! - Each duplicate or flip is one undo step; a refused one records none.

use rp_doc::{EditError, GrayImage, LayerKind, RgbaImage, Size, Transform};

use crate::state::EditorState;
use crate::undo_ops::EditLabel;

/// Which way a flip mirrors.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FlipAxis {
    /// Left ↔ right.
    Horizontal,
    /// Top ↔ bottom.
    Vertical,
}

impl FlipAxis {
    const fn label(self) -> EditLabel {
        match self {
            FlipAxis::Horizontal => EditLabel::FlipHorizontal,
            FlipAxis::Vertical => EditLabel::FlipVertical,
        }
    }

    const fn adverb(self) -> &'static str {
        match self {
            FlipAxis::Horizontal => "horizontally",
            FlipAxis::Vertical => "vertically",
        }
    }
}

/// Mirrors rows of `bpp`-byte pixels in `data` (`width` pixels per row).
fn flip_raw(data: &mut [u8], width: usize, bpp: usize, axis: FlipAxis) {
    let stride = width * bpp;
    if stride == 0 {
        return;
    }
    match axis {
        FlipAxis::Horizontal => {
            for row in data.chunks_exact_mut(stride) {
                row.reverse();
                if bpp > 1 {
                    for px in row.chunks_exact_mut(bpp) {
                        px.reverse();
                    }
                }
            }
        }
        FlipAxis::Vertical => {
            let rows = data.len() / stride;
            for y in 0..rows / 2 {
                let (top, bottom) = data.split_at_mut((rows - 1 - y) * stride);
                top[y * stride..(y + 1) * stride].swap_with_slice(&mut bottom[..stride]);
            }
        }
    }
}

/// Mirrors a whole RGBA raster in place.
pub fn flip_rgba(image: &mut RgbaImage, axis: FlipAxis) {
    let w = image.width() as usize;
    flip_raw(image.as_raw_mut(), w, 4, axis);
}

/// Canvas-space rectangle `x, y, width, height` (may reach past the canvas).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bounds {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

/// Mirrors the part of `mask` inside `bounds` about the center of `bounds`.
/// Values mirrored from outside the mask read as 255 (an absent mask pixel
/// shows the layer); pixels outside `bounds` are unchanged.
pub fn flip_mask_in(mask: &mut GrayImage, bounds: Bounds, axis: FlipAxis) {
    let size = mask.size();
    let (mw, mh) = (size.width as i64, size.height as i64);
    let (x0, x1) = (bounds.x.max(0), (bounds.x + bounds.width).min(mw));
    let (y0, y1) = (bounds.y.max(0), (bounds.y + bounds.height).min(mh));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    if (bounds.x, bounds.y, bounds.width, bounds.height) == (0, 0, mw, mh) {
        flip_raw(mask.as_raw_mut(), mw as usize, 1, axis);
        return;
    }
    let old = mask.as_raw().to_vec();
    let at = |x: i64, y: i64| -> u8 {
        if (0..mw).contains(&x) && (0..mh).contains(&y) {
            old[(y * mw + x) as usize]
        } else {
            255
        }
    };
    let data = mask.as_raw_mut();
    for y in y0..y1 {
        for x in x0..x1 {
            let (sx, sy) = match axis {
                FlipAxis::Horizontal => (2 * bounds.x + bounds.width - 1 - x, y),
                FlipAxis::Vertical => (x, 2 * bounds.y + bounds.height - 1 - y),
            };
            data[(y * mw + x) as usize] = at(sx, sy);
        }
    }
}

/// `t` mirrored on screen about the center of the `src`-sized image it
/// places: the flipped axis's scale and the rotation are negated, and the
/// translation moves so the center stays put (see [`Transform`]).
pub fn flipped_transform(t: Transform, src: Size, axis: FlipAxis) -> Transform {
    let (w, h) = (src.width as f32, src.height as f32);
    // `0.0 - r` keeps an unrotated image at +0.0.
    let rotation = 0.0 - t.rotation;
    match axis {
        FlipAxis::Horizontal => Transform {
            translate_x: t.translate_x + w * t.scale_x,
            scale_x: -t.scale_x,
            rotation,
            ..t
        },
        FlipAxis::Vertical => Transform {
            translate_y: t.translate_y + h * t.scale_y,
            scale_y: -t.scale_y,
            rotation,
            ..t
        },
    }
}

impl EditorState {
    /// Inserts a copy of the selected layer above it, named `<name> copy`,
    /// and selects it. One undo step.
    pub fn duplicate_selected_layer(&mut self) -> bool {
        self.record(
            EditLabel::DuplicateLayer,
            Self::duplicate_selected_layer_now,
        )
    }

    fn duplicate_selected_layer_now(&mut self) -> bool {
        let (Some(index), Some(layer)) = (self.selected_index(), self.selected()) else {
            self.status = "Can't duplicate: no layer selected".into();
            return false;
        };
        let mut copy = layer.clone();
        copy.name = format!("{} copy", layer.name);
        let name = copy.name.clone();
        let id = self.document.insert_layer(index + 1, copy);
        self.selected_layer = Some(id);
        self.mark_document_changed();
        self.status = format!("Added {name}");
        true
    }

    /// Mirrors the selected layer (see the module docs). One undo step;
    /// refusals set the status and return `false`.
    pub fn flip_selected_layer(&mut self, axis: FlipAxis) -> bool {
        self.record(axis.label(), |s| s.flip_selected_layer_now(axis))
    }

    fn flip_selected_layer_now(&mut self, axis: FlipAxis) -> bool {
        let Some(id) = self.selected_layer else {
            self.status = "Can't flip: no layer selected".into();
            return false;
        };
        let src = match &self.selected().map(|l| &l.kind) {
            Some(LayerKind::Image(i)) => match self.sources.get(&i.source) {
                Some(img) => Some(img.size()),
                None => {
                    self.status = "Can't flip: image source is missing".into();
                    return false;
                }
            },
            Some(LayerKind::Paint(_)) => None,
            None => {
                self.status = "Can't flip: no layer selected".into();
                return false;
            }
        };
        let layer = self.document.layer_mut(id).expect("selected layer exists");
        let name = layer.name.clone();
        let result = match src {
            Some(src) => match &layer.kind {
                LayerKind::Image(i) => {
                    layer.set_transform(flipped_transform(i.transform, src, axis))
                }
                LayerKind::Paint(_) => unreachable!("checked above"),
            },
            None => match layer.pixels_mut() {
                Ok(pixels) => {
                    flip_rgba(pixels, axis);
                    let LayerKind::Paint(p) = &layer.kind else {
                        unreachable!("pixels_mut is paint only")
                    };
                    let bounds = Bounds {
                        x: p.offset_x as i64,
                        y: p.offset_y as i64,
                        width: p.pixels.width() as i64,
                        height: p.pixels.height() as i64,
                    };
                    if let Some(mask) = &mut layer.mask {
                        flip_mask_in(mask, bounds, axis);
                    }
                    Ok(())
                }
                Err(e) => Err(e),
            },
        };
        match result {
            Ok(()) => {
                self.mark_document_changed();
                self.status = format!("Flipped {name} {}", axis.adverb());
                true
            }
            Err(e) => {
                self.status = match e {
                    EditError::PixelsLocked => format!("Can't flip: {name} pixels are locked"),
                    EditError::PositionLocked => {
                        format!("Can't flip: {name} position is locked")
                    }
                    other => format!("Can't flip {name}: {other}"),
                };
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rp_doc::{Document, Layer, LayerId, SourceHash};

    use super::*;
    use crate::undo_ops::EditCommand;

    const CANVAS: Size = Size::new(8, 6);

    fn gray(size: Size, f: impl Fn(u32, u32) -> u8) -> GrayImage {
        let mut data = Vec::new();
        for y in 0..size.height {
            for x in 0..size.width {
                data.push(f(x, y));
            }
        }
        GrayImage::from_raw(size, data).unwrap()
    }

    fn rgba(size: Size, f: impl Fn(u32, u32) -> [u8; 4]) -> RgbaImage {
        let mut data = Vec::new();
        for y in 0..size.height {
            for x in 0..size.width {
                data.extend(f(x, y));
            }
        }
        RgbaImage::from_raw(size, data).unwrap()
    }

    fn pattern(x: u32, y: u32) -> [u8; 4] {
        [x as u8, y as u8, (x * 10 + y) as u8, 200 + x as u8]
    }

    /// A canvas with a background, a patterned paint layer (selected) and a
    /// 4×2 image layer at (2, 3).
    fn editor() -> (EditorState, LayerId, LayerId) {
        let mut st = EditorState::new();
        let mut doc = Document::new(CANVAS);
        doc.push_layer(Layer::paint("Background", CANVAS));
        let mut paint = Layer::paint("Ink", CANVAS);
        if let LayerKind::Paint(p) = &mut paint.kind {
            p.pixels = rgba(CANVAS, pattern);
        }
        let paint = doc.push_layer(paint);
        let hash = SourceHash::new("src");
        let img = doc.push_layer(Layer::image("Pic", hash.clone(), Transform::at(2.0, 3.0)));
        st.set_document(doc);
        Arc::make_mut(&mut st.sources).insert(
            hash,
            rgba(Size::new(4, 2), |x, y| {
                [x as u8 * 60, y as u8 * 100, 7, 255]
            }),
        );
        st.selected_layer = Some(paint);
        (st, paint, img)
    }

    fn pixels(st: &EditorState, id: LayerId) -> &RgbaImage {
        match &st.document.layer(id).unwrap().kind {
            LayerKind::Paint(p) => &p.pixels,
            _ => panic!("paint layer"),
        }
    }

    fn transform(st: &EditorState, id: LayerId) -> Transform {
        match &st.document.layer(id).unwrap().kind {
            LayerKind::Image(i) => i.transform,
            _ => panic!("image layer"),
        }
    }

    #[test]
    fn raster_flips_mirror_and_round_trip() {
        let src = rgba(Size::new(5, 3), pattern);
        let mut h = src.clone();
        flip_rgba(&mut h, FlipAxis::Horizontal);
        let mut v = src.clone();
        flip_rgba(&mut v, FlipAxis::Vertical);
        for y in 0..3 {
            for x in 0..5 {
                assert_eq!(h.get(x, y), src.get(4 - x, y));
                assert_eq!(v.get(x, y), src.get(x, 2 - y));
            }
        }
        flip_rgba(&mut h, FlipAxis::Horizontal);
        flip_rgba(&mut v, FlipAxis::Vertical);
        assert_eq!(h, src);
        assert_eq!(v, src);
    }

    #[test]
    fn flipping_a_shared_raster_copies_it() {
        let src = rgba(Size::new(4, 4), pattern);
        let mut copy = src.clone();
        assert!(copy.shares_buffer(&src));
        flip_rgba(&mut copy, FlipAxis::Horizontal);
        assert!(!copy.shares_buffer(&src));
        assert_eq!(src, rgba(Size::new(4, 4), pattern));
    }

    #[test]
    fn mask_flips_within_bounds_and_pads_with_white() {
        let size = Size::new(6, 4);
        let m = gray(size, |x, y| (x + 10 * y) as u8);
        // Bounds equal to the mask: plain mirror.
        let mut full = m.clone();
        let all = Bounds {
            x: 0,
            y: 0,
            width: 6,
            height: 4,
        };
        flip_mask_in(&mut full, all, FlipAxis::Horizontal);
        assert_eq!(full.get(0, 1), 15);
        flip_mask_in(&mut full, all, FlipAxis::Horizontal);
        assert_eq!(full, m);
        // Bounds x 2..8 reach 2 px past the right edge: columns 2..6 take
        // mirrored columns 7..3; 7 and 6 are outside, so they become 255.
        let mut part = m.clone();
        let b = Bounds {
            x: 2,
            y: 0,
            width: 6,
            height: 4,
        };
        flip_mask_in(&mut part, b, FlipAxis::Horizontal);
        for y in 0..4 {
            assert_eq!(part.get(0, y), m.get(0, y));
            assert_eq!(part.get(1, y), m.get(1, y));
            assert_eq!(part.get(2, y), 255);
            assert_eq!(part.get(3, y), 255);
            assert_eq!(part.get(4, y), m.get(5, y));
            assert_eq!(part.get(5, y), m.get(4, y));
        }
        // Vertical, bounds y -1..3 (one row above the top).
        let mut vert = m.clone();
        let b = Bounds {
            x: 0,
            y: -1,
            width: 6,
            height: 4,
        };
        flip_mask_in(&mut vert, b, FlipAxis::Vertical);
        for x in 0..6 {
            assert_eq!(vert.get(x, 0), m.get(x, 1));
            assert_eq!(vert.get(x, 1), m.get(x, 0));
            assert_eq!(vert.get(x, 2), 255);
            assert_eq!(vert.get(x, 3), m.get(x, 3));
        }
    }

    #[test]
    fn transform_flip_keeps_the_center_and_round_trips() {
        let src = Size::new(40, 20);
        let t = Transform {
            translate_x: 10.0,
            translate_y: 5.0,
            scale_x: 1.5,
            scale_y: 0.5,
            rotation: 0.3,
        };
        let center = |t: &Transform| {
            (
                t.translate_x + 40.0 * t.scale_x / 2.0,
                t.translate_y + 20.0 * t.scale_y / 2.0,
            )
        };
        for axis in [FlipAxis::Horizontal, FlipAxis::Vertical] {
            let f = flipped_transform(t, src, axis);
            assert_eq!(center(&f), center(&t));
            assert_eq!(f.rotation, -0.3);
            match axis {
                FlipAxis::Horizontal => assert_eq!((f.scale_x, f.scale_y), (-1.5, 0.5)),
                FlipAxis::Vertical => assert_eq!((f.scale_x, f.scale_y), (1.5, -0.5)),
            }
            assert_eq!(flipped_transform(f, src, axis), t);
        }
        let plain = flipped_transform(Transform::at(1.0, 2.0), src, FlipAxis::Horizontal);
        assert!(plain.rotation.is_sign_positive());
    }

    #[test]
    fn duplicate_goes_above_selected_and_shares_buffers() {
        let (mut st, paint, img) = editor();
        st.add_mask(paint);
        let v = st.doc_version;
        assert!(st.duplicate_selected_layer());
        assert_ne!(st.doc_version, v);
        let names: Vec<_> = st
            .document
            .layers()
            .iter()
            .map(|l| l.name.clone())
            .collect();
        assert_eq!(names, ["Background", "Ink", "Ink copy", "Pic"]);
        let copy = st.selected_layer.unwrap();
        assert_ne!(copy, paint);
        assert_eq!(st.selected_index(), Some(2));
        let (a, b) = (
            st.document.layer(paint).unwrap(),
            st.document.layer(copy).unwrap(),
        );
        assert!(pixels(&st, copy).shares_buffer(pixels(&st, paint)));
        assert!(
            a.mask
                .as_ref()
                .unwrap()
                .shares_buffer(b.mask.as_ref().unwrap())
        );
        // Editing the copy leaves the original alone.
        flip_rgba(
            st.document.layer_mut(copy).unwrap().pixels_mut().unwrap(),
            FlipAxis::Horizontal,
        );
        assert_eq!(*pixels(&st, paint), rgba(CANVAS, pattern));

        // Image layers copy the transform and reference the same source.
        st.select_layer(img);
        assert!(st.duplicate_selected_layer());
        let copy = st.selected_layer.unwrap();
        assert_eq!(st.document.layers().last().unwrap().name, "Pic copy");
        assert_eq!(transform(&st, copy), transform(&st, img));
        assert_eq!(st.document.layers().len(), 5);
        // A second copy is "Pic copy copy".
        assert!(st.duplicate_selected_layer());
        assert_eq!(st.selected().unwrap().name, "Pic copy copy");
    }

    #[test]
    fn duplicate_ignores_locks_and_copies_them() {
        let (mut st, paint, _) = editor();
        st.toggle_lock_pixels(paint);
        st.toggle_lock_position(paint);
        assert!(st.duplicate_selected_layer());
        let lock = st.selected().unwrap().lock;
        assert!(lock.pixels && lock.position);
    }

    #[test]
    fn duplicate_and_flip_are_one_undo_step_each() {
        let (mut st, paint, img) = editor();
        st.add_mask(paint);
        let before = st.document.clone();
        assert!(st.duplicate_selected_layer());
        assert_eq!(
            st.history.undo_label(),
            Some(EditLabel::DuplicateLayer),
            "duplicate step"
        );
        assert!(st.undo());
        assert_eq!(st.document, before);
        // The copy is gone, so the top layer is selected (undo's rule).
        assert_eq!(st.selected_layer, Some(img));
        assert!(st.redo());
        assert_eq!(st.document.layers().len(), 4);

        let before = st.document.clone();
        st.select_layer(paint);
        assert!(st.flip_selected_layer(FlipAxis::Horizontal));
        assert_eq!(st.history.undo_label(), Some(EditLabel::FlipHorizontal));
        st.select_layer(img);
        assert!(st.flip_selected_layer(FlipAxis::Vertical));
        assert_eq!(st.history.undo_label(), Some(EditLabel::FlipVertical));
        assert!(st.undo());
        assert!(st.undo());
        assert_eq!(st.document, before);
    }

    #[test]
    fn paint_flip_mirrors_pixels_and_mask() {
        let (mut st, paint, _) = editor();
        let mask = gray(CANVAS, |x, y| (x * 20 + y) as u8);
        st.document.layer_mut(paint).unwrap().mask = Some(mask.clone());
        let v = st.doc_version;
        assert!(st.flip_selected_layer(FlipAxis::Horizontal));
        assert_ne!(st.doc_version, v);
        assert_eq!(st.status, "Flipped Ink horizontally");
        let p = pixels(&st, paint);
        let m = st.document.layer(paint).unwrap().mask.as_ref().unwrap();
        for y in 0..6 {
            for x in 0..8 {
                assert_eq!(p.get(x, y), pattern(7 - x, y));
                assert_eq!(m.get(x, y), mask.get(7 - x, y));
            }
        }
        assert!(st.flip_selected_layer(FlipAxis::Vertical));
        assert_eq!(pixels(&st, paint).get(0, 0), pattern(7, 5));
    }

    #[test]
    fn paint_flip_with_an_offset_mirrors_about_the_layer_bounds() {
        let (mut st, paint, _) = editor();
        st.document.layer_mut(paint).unwrap().mask = Some(gray(CANVAS, |x, _| x as u8));
        st.document
            .layer_mut(paint)
            .unwrap()
            .translate_by(2, 0)
            .unwrap();
        assert!(st.flip_selected_layer(FlipAxis::Horizontal));
        // The raster stays at offset 2, mirrored.
        assert_eq!(pixels(&st, paint).get(0, 0), pattern(7, 0));
        // Layer covers canvas x 2..10: mask columns 2..8 take 9..3 (8, 9 are
        // past the canvas → 255); columns 0, 1 are outside the layer.
        let m = st.document.layer(paint).unwrap().mask.as_ref().unwrap();
        let row: Vec<u8> = (0..8).map(|x| m.get(x, 0)).collect();
        assert_eq!(row, [0, 1, 255, 255, 7, 6, 5, 4]);
    }

    #[test]
    fn locks_refuse_flips_by_kind() {
        let (mut st, paint, img) = editor();
        // Paint: pixel lock refuses, position lock does not.
        st.toggle_lock_pixels(paint);
        let (v, undo) = (st.doc_version, st.history.undo_len());
        assert!(!st.flip_selected_layer(FlipAxis::Horizontal));
        assert_eq!(st.status, "Can't flip: Ink pixels are locked");
        assert_eq!((st.doc_version, st.history.undo_len()), (v, undo));
        st.toggle_lock_pixels(paint);
        st.toggle_lock_position(paint);
        assert!(st.flip_selected_layer(FlipAxis::Horizontal));
        // Image: position lock refuses, pixel lock does not.
        st.select_layer(img);
        st.toggle_lock_position(img);
        assert!(!st.flip_selected_layer(FlipAxis::Vertical));
        assert_eq!(st.status, "Can't flip: Pic position is locked");
        assert_eq!(transform(&st, img), Transform::at(2.0, 3.0));
        st.toggle_lock_position(img);
        st.toggle_lock_pixels(img);
        assert!(st.flip_selected_layer(FlipAxis::Vertical));
    }

    #[test]
    fn image_flip_mirrors_the_flattened_image_in_place() {
        let (mut st, _, img) = editor();
        st.select_layer(img);
        let before = st.document.flatten(&st.sources);
        let mask_before = st.document.layer(img).unwrap().mask.clone();
        assert!(st.flip_selected_layer(FlipAxis::Horizontal));
        assert_eq!(transform(&st, img).scale_x, -1.0);
        let after = st.document.flatten(&st.sources);
        // The image covers x 2..6, y 3..5.
        for y in 0..6 {
            for x in 0..8 {
                let inside = (2..6).contains(&x) && (3..5).contains(&y);
                let expect = if inside {
                    before.get(7 - x, y)
                } else {
                    before.get(x, y)
                };
                assert_eq!(after.get(x, y), expect, "({x}, {y})");
            }
        }
        assert!(st.flip_selected_layer(FlipAxis::Vertical));
        let flipped_both = st.document.flatten(&st.sources);
        assert_eq!(flipped_both.get(2, 3), before.get(5, 4));
        assert_eq!(st.document.layer(img).unwrap().mask, mask_before);
    }

    #[test]
    fn missing_source_refuses_image_flip() {
        let (mut st, _, img) = editor();
        st.select_layer(img);
        Arc::make_mut(&mut st.sources).remove(&SourceHash::new("src"));
        assert!(!st.flip_selected_layer(FlipAxis::Horizontal));
        assert!(st.status.contains("missing"), "{}", st.status);
    }

    #[test]
    fn edit_menu_runs_duplicate_and_flips() {
        let (mut st, paint, _) = editor();
        st.edit_menu_open = true;
        assert!(st.edit_command(EditCommand::DuplicateLayer));
        assert!(!st.edit_menu_open);
        assert_eq!(st.selected().unwrap().name, "Ink copy");
        assert!(st.edit_command(EditCommand::FlipHorizontal));
        assert_eq!(
            pixels(&st, st.selected_layer.unwrap()).get(0, 0),
            pattern(7, 0)
        );
        assert!(st.edit_command(EditCommand::FlipVertical));
        assert_eq!(
            pixels(&st, st.selected_layer.unwrap()).get(0, 0),
            pattern(7, 5)
        );
        assert_eq!(*pixels(&st, paint), rgba(CANVAS, pattern));
        assert_eq!(EditCommand::DuplicateLayer.keystrokes(), ["ctrl-j"]);
        assert_eq!(
            EditCommand::DuplicateLayer.menu_label(&st.history),
            "Duplicate Layer"
        );
        assert!(EditCommand::FlipHorizontal.keystrokes().is_empty());
        assert!(EditCommand::Paste.separator_after());
    }
}
