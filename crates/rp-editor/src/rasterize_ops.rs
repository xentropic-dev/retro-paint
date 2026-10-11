//! Rasterize: turn the selected image layer into a canvas-sized paint layer
//! so the brush and eraser work on it.
//!
//! Decisions:
//! - The pixels are the layer rendered on its own by [`Document::flatten`]
//!   (a one-layer document with the layer visible, Normal blend, 100%
//!   opacity and no mask), so the sampling is exactly the flatten's.
//! - The layer keeps its id, name, visibility, opacity, blend mode, mask
//!   and locks; only its kind changes, to a canvas-sized paint raster at
//!   offset 0. The flattened document does not change, except that the
//!   bilinear samples of a scaled or rotated image are now stored as 8-bit
//!   values (at most 1 level per channel; integer placements, including
//!   flips and half turns, are exact).
//! - Locks never refuse a rasterize: it changes neither how the layer
//!   looks nor where it is. A pixel-locked result still refuses strokes.
//! - The source image stays in the store (undo needs it); saving writes
//!   only the sources image layers still reference.
//! - It runs synchronously on the UI thread (one flatten of a single
//!   layer, about 7 ms for a rotated 3000×4000 source on an 800×1280
//!   canvas in a release build) and is one undo step; a refused one
//!   records none.

use rp_doc::{BlendMode, Document, Layer, LayerKind, PaintLayer, RgbaImage, SourceStore};

use crate::state::EditorState;
use crate::undo_ops::EditLabel;

/// `layer` rendered alone onto a transparent `doc`-canvas-sized raster,
/// with flatten's sampling but as if visible, Normal, 100% and unmasked.
pub fn render_layer_alone(doc: &Document, layer: &Layer, sources: &SourceStore) -> RgbaImage {
    let mut solo = Document::new(doc.canvas_size);
    let mut l = Layer::with_kind(layer.name.clone(), layer.kind.clone());
    l.visible = true;
    l.opacity = 100;
    l.blend = BlendMode::Normal;
    l.mask = None;
    solo.push_layer(l);
    solo.flatten(sources)
}

impl EditorState {
    /// Converts the selected image layer into a paint layer (see the module
    /// docs). One undo step; refusals set the status and return `false`.
    pub fn rasterize_selected_layer(&mut self) -> bool {
        self.record(
            EditLabel::RasterizeLayer,
            Self::rasterize_selected_layer_now,
        )
    }

    fn rasterize_selected_layer_now(&mut self) -> bool {
        let (Some(id), Some(layer)) = (self.selected_layer, self.selected()) else {
            self.status = "Can't rasterize: no layer selected".into();
            return false;
        };
        let name = layer.name.clone();
        let source = match &layer.kind {
            LayerKind::Image(i) => &i.source,
            LayerKind::Paint(_) => {
                self.status = format!("{name} is already a paint layer");
                return false;
            }
        };
        if !self.sources.contains(source) {
            self.status = "Can't rasterize: image source is missing".into();
            return false;
        }
        let pixels = render_layer_alone(&self.document, layer, &self.sources);
        let layer = self.document.layer_mut(id).expect("selected layer exists");
        layer.kind = LayerKind::Paint(PaintLayer {
            pixels,
            offset_x: 0,
            offset_y: 0,
        });
        self.mark_document_changed();
        self.status = format!("Rasterized {name}");
        true
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;
    use std::sync::Arc;
    use std::time::Instant;

    use rp_doc::{GrayImage, LayerId, Size, SourceHash, Transform};

    use super::*;
    use crate::flip_ops::{FlipAxis, flipped_transform};
    use crate::undo_ops::EditCommand;

    const CANVAS: Size = Size::new(24, 18);
    const SRC: Size = Size::new(9, 7);

    fn rgba(size: Size, f: impl Fn(u32, u32) -> [u8; 4]) -> RgbaImage {
        let mut data = Vec::new();
        for y in 0..size.height {
            for x in 0..size.width {
                data.extend(f(x, y));
            }
        }
        RgbaImage::from_raw(size, data).unwrap()
    }

    fn gray(size: Size, f: impl Fn(u32, u32) -> u8) -> GrayImage {
        let mut data = Vec::new();
        for y in 0..size.height {
            for x in 0..size.width {
                data.push(f(x, y));
            }
        }
        GrayImage::from_raw(size, data).unwrap()
    }

    /// Source with varied color and alpha, including transparent texels.
    fn source(x: u32, y: u32) -> [u8; 4] {
        let a = match (x + y) % 4 {
            0 => 255,
            1 => 180,
            2 => 60,
            _ => {
                if x == 3 {
                    0
                } else {
                    255
                }
            }
        };
        [
            (x * 29) as u8,
            (y * 37) as u8,
            ((x * y * 13) % 256) as u8,
            a,
        ]
    }

    /// A background, a translucent paint layer, and an image layer "Pic"
    /// (selected) placed by `t`, with `setup` applied to it.
    fn editor(t: Transform, setup: impl FnOnce(&mut Layer)) -> (EditorState, LayerId) {
        let mut st = EditorState::new();
        let mut doc = Document::new(CANVAS);
        let mut bg = Layer::paint("Background", CANVAS);
        if let LayerKind::Paint(p) = &mut bg.kind {
            p.pixels = rgba(CANVAS, |x, y| [(x * 10) as u8, 90, (y * 14) as u8, 255]);
        }
        doc.push_layer(bg);
        let mut mid = Layer::paint("Wash", CANVAS);
        if let LayerKind::Paint(p) = &mut mid.kind {
            p.pixels = rgba(CANVAS, |x, y| {
                [
                    200,
                    (x * 7) as u8,
                    40,
                    if (x + y) % 3 == 0 { 0 } else { 128 },
                ]
            });
        }
        doc.push_layer(mid);
        let hash = SourceHash::new("src");
        let mut img = Layer::image("Pic", hash.clone(), t);
        setup(&mut img);
        let id = doc.push_layer(img);
        st.set_document(doc);
        Arc::make_mut(&mut st.sources).insert(hash, rgba(SRC, source));
        st.selected_layer = Some(id);
        (st, id)
    }

    fn max_diff(a: &RgbaImage, b: &RgbaImage) -> u8 {
        a.as_raw()
            .iter()
            .zip(b.as_raw())
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0)
    }

    fn masked(l: &mut Layer) {
        l.mask = Some(gray(CANVAS, |x, y| ((x * 11 + y * 5) % 256) as u8));
    }

    /// Placements that land on texel centers (exact) and ones that
    /// resample (within one level).
    fn placements() -> Vec<(&'static str, Transform, bool)> {
        let at = Transform::at(5.0, 4.0);
        let scaled = Transform {
            scale_x: 1.7,
            scale_y: 1.3,
            ..Transform::at(2.5, 1.25)
        };
        let rotated = Transform {
            rotation: 0.6,
            ..Transform::at(7.0, 5.0)
        };
        let both = Transform {
            scale_x: 2.2,
            scale_y: 0.8,
            rotation: -2.0,
            ..Transform::at(6.3, 6.6)
        };
        vec![
            ("translated", at, true),
            ("off canvas", Transform::at(-4.0, 13.0), true),
            (
                "flipped h",
                flipped_transform(at, SRC, FlipAxis::Horizontal),
                true,
            ),
            (
                "flipped v",
                flipped_transform(at, SRC, FlipAxis::Vertical),
                true,
            ),
            ("rotated 180", Transform { rotation: PI, ..at }, true),
            ("fractional", Transform::at(5.5, 3.25), false),
            ("scaled", scaled, false),
            ("rotated", rotated, false),
            ("scaled rotated", both, false),
            (
                "flipped rotated",
                flipped_transform(both, SRC, FlipAxis::Horizontal),
                false,
            ),
        ]
    }

    type Setup = Box<dyn Fn(&mut Layer)>;

    #[test]
    fn flatten_is_unchanged_for_every_placement_and_layer_setting() {
        let settings: Vec<(&str, Setup)> = vec![
            ("plain", Box::new(|_| {})),
            (
                "multiply 60%",
                Box::new(|l| {
                    l.blend = BlendMode::Multiply;
                    l.opacity = 60;
                }),
            ),
            (
                "screen 35% masked",
                Box::new(|l| {
                    l.blend = BlendMode::Screen;
                    l.opacity = 35;
                    masked(l);
                }),
            ),
            (
                "overlay 80% masked",
                Box::new(|l| {
                    l.blend = BlendMode::Overlay;
                    l.opacity = 80;
                    masked(l);
                }),
            ),
            ("normal masked", Box::new(masked)),
        ];
        for (pname, t, exact) in placements() {
            for (sname, setup) in &settings {
                let (mut st, id) = editor(t, setup);
                let before = st.document.flatten(&st.sources);
                assert!(st.rasterize_selected_layer(), "{pname} {sname}");
                let after = st.document.flatten(&st.sources);
                let d = max_diff(&before, &after);
                if exact {
                    assert_eq!(d, 0, "{pname} / {sname}");
                } else {
                    assert!(d <= 1, "{pname} / {sname}: diff {d}");
                }
                let l = st.document.layer(id).unwrap();
                assert!(matches!(l.kind, LayerKind::Paint(_)), "{pname}");
            }
        }
    }

    #[test]
    fn keeps_layer_properties_and_makes_a_canvas_sized_raster() {
        let (mut st, id) = editor(Transform::at(3.0, 2.0), |l| {
            l.name = "Photo".into();
            l.visible = false;
            l.opacity = 45;
            l.blend = BlendMode::Screen;
            masked(l);
            l.lock.pixels = true;
            l.lock.position = true;
        });
        let old = st.document.layer(id).unwrap().clone();
        let index = st.document.index_of(id);
        let v = st.doc_version;
        assert!(st.rasterize_selected_layer());
        assert_ne!(st.doc_version, v);
        assert_eq!(st.status, "Rasterized Photo");
        assert_eq!(st.selected_layer, Some(id));
        assert_eq!(st.document.index_of(id), index);
        let l = st.document.layer(id).unwrap();
        assert_eq!(l.name, old.name);
        assert_eq!(l.visible, old.visible);
        assert_eq!(l.opacity, old.opacity);
        assert_eq!(l.blend, old.blend);
        assert_eq!(l.mask, old.mask);
        assert_eq!(l.lock, old.lock);
        let LayerKind::Paint(p) = &l.kind else {
            panic!("paint layer")
        };
        assert_eq!((p.offset_x, p.offset_y), (0, 0));
        assert_eq!(p.pixels.size(), CANVAS);
        // A hidden, faded, masked layer still rasterizes its full pixels.
        assert_eq!(p.pixels.get(3, 2), source(0, 0));
        assert_eq!(p.pixels.get(11, 8), source(8, 6));
        assert_eq!(p.pixels.get(2, 2), [0, 0, 0, 0]);
        assert_eq!(p.pixels.get(12, 2), [0, 0, 0, 0]);
    }

    #[test]
    fn rasterized_layer_takes_strokes() {
        use crate::paint_ops::PaintButton;
        use crate::viewport::Vec2;
        use rp_doc::Tool;
        let (mut st, id) = editor(Transform::at(3.0, 2.0), |_| {});
        st.tool = Tool::Brush;
        assert!(!st.begin_stroke(Vec2::new(4.5, 4.5), PaintButton::Primary));
        assert!(st.status.contains("Rasterize"), "{}", st.status);
        assert!(st.rasterize_selected_layer());
        assert!(st.begin_stroke(Vec2::new(4.5, 4.5), PaintButton::Primary));
        st.end_stroke();
        assert!(matches!(
            st.document.layer(id).unwrap().kind,
            LayerKind::Paint(_)
        ));
    }

    #[test]
    fn one_undo_step_that_restores_the_image_layer_exactly() {
        let (mut st, id) = editor(
            Transform {
                rotation: 0.4,
                ..Transform::at(4.0, 3.0)
            },
            masked,
        );
        let before = st.document.clone();
        let flat = st.document.flatten(&st.sources);
        assert!(st.rasterize_selected_layer());
        assert_eq!(st.history.undo_label(), Some(EditLabel::RasterizeLayer));
        let rasterized = st.document.clone();
        assert!(st.undo());
        assert_eq!(st.document, before);
        assert_eq!(st.document.flatten(&st.sources), flat);
        assert!(matches!(
            st.document.layer(id).unwrap().kind,
            LayerKind::Image(_)
        ));
        assert!(st.redo());
        assert_eq!(st.document, rasterized);
        assert!(!st.history.can_redo());
    }

    #[test]
    fn refusals_record_nothing() {
        let (mut st, _) = editor(Transform::at(0.0, 0.0), |_| {});
        // A paint layer.
        let bg = st.document.layers()[0].id();
        st.select_layer(bg);
        let (v, n) = (st.doc_version, st.history.undo_len());
        assert!(!st.rasterize_selected_layer());
        assert_eq!(st.status, "Background is already a paint layer");
        assert_eq!((st.doc_version, st.history.undo_len()), (v, n));
        // A missing source.
        let top = st.document.layers()[2].id();
        st.select_layer(top);
        Arc::make_mut(&mut st.sources).remove(&SourceHash::new("src"));
        assert!(!st.rasterize_selected_layer());
        assert!(st.status.contains("missing"), "{}", st.status);
        assert_eq!((st.doc_version, st.history.undo_len()), (v, n));
        // No selection.
        st.selected_layer = None;
        assert!(!st.rasterize_selected_layer());
        assert_eq!(st.history.undo_len(), n);
    }

    #[test]
    fn locks_do_not_refuse() {
        let (mut st, id) = editor(Transform::at(1.0, 1.0), |l| {
            l.lock.pixels = true;
            l.lock.position = true;
        });
        assert!(st.rasterize_selected_layer());
        assert!(matches!(
            st.document.layer(id).unwrap().kind,
            LayerKind::Paint(_)
        ));
    }

    #[test]
    fn edit_menu_runs_rasterize() {
        let (mut st, id) = editor(Transform::at(1.0, 1.0), |_| {});
        st.edit_menu_open = true;
        assert!(st.edit_command(EditCommand::RasterizeLayer));
        assert!(!st.edit_menu_open);
        assert!(matches!(
            st.document.layer(id).unwrap().kind,
            LayerKind::Paint(_)
        ));
        assert!(EditCommand::RasterizeLayer.keystrokes().is_empty());
        assert_eq!(
            EditCommand::RasterizeLayer.menu_label(&st.history),
            "Rasterize Layer"
        );
    }

    /// Timing at the largest default canvas; run with `--ignored
    /// --nocapture` in release to measure.
    #[test]
    #[ignore]
    fn rasterize_timing_800x1280() {
        let canvas = Size::new(800, 1280);
        let mut st = EditorState::new();
        let mut doc = Document::new(canvas);
        let hash = SourceHash::new("big");
        let id = doc.push_layer(Layer::image(
            "Big",
            hash.clone(),
            Transform {
                scale_x: 0.37,
                scale_y: 0.37,
                rotation: 0.3,
                ..Transform::at(-200.0, -100.0)
            },
        ));
        st.set_document(doc);
        Arc::make_mut(&mut st.sources).insert(
            hash,
            rgba(Size::new(3000, 4000), |x, y| [x as u8, y as u8, 7, 255]),
        );
        st.selected_layer = Some(id);
        let t = Instant::now();
        assert!(st.rasterize_selected_layer());
        println!("rasterize 800x1280: {:?}", t.elapsed());
    }
}
