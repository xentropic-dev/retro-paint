//! Undo engine: byte-exact undo → redo for every step kind, dirty-rect
//! minimality, eviction by memory, redo invalidation.

use rp_doc::history::{DEFAULT_BUDGET, dirty_rect};
use rp_doc::{
    BlendMode, Document, Edit, EditKind, GrayImage, History, Layer, LayerId, Rect, RgbaImage, Size,
    SourceHash, Transform,
};
use rp_palette_dither::{DitherMethod, Rgb};

const CANVAS: Size = Size::new(16, 12);

fn doc() -> (Document, LayerId, LayerId) {
    let mut d = Document::new(CANVAS);
    let paint = d.push_layer(Layer::paint("paint", CANVAS));
    let img = d.push_layer(Layer::image(
        "img",
        SourceHash::new("ab"),
        Transform::IDENTITY,
    ));
    let px = d.layer_mut(paint).unwrap().pixels_mut().unwrap();
    for y in 0..CANVAS.height {
        for x in 0..CANVAS.width {
            px.put(x, y, [x as u8, y as u8, 7, 255]);
        }
    }
    (d, paint, img)
}

fn dab(d: &mut Document, layer: LayerId, x0: u32, y0: u32, w: u32, h: u32, c: [u8; 4]) {
    let px = d.layer_mut(layer).unwrap().pixels_mut().unwrap();
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            px.put(x, y, c);
        }
    }
}

/// Undo must give `before`, redo must give `after`, twice over.
fn assert_round_trip(h: &mut History, d: &mut Document, before: &Document, after: &Document) {
    for _ in 0..2 {
        assert!(h.undo(d));
        assert_eq!(d, before);
        assert!(h.redo(d));
        assert_eq!(d, after);
    }
}

#[test]
fn paint_stroke_round_trips_and_stores_only_dirty_rect() {
    let (mut d, paint, _) = doc();
    let before = d.clone();
    let mut h = History::new();
    assert!(h.begin_paint(&d, paint));
    dab(&mut d, paint, 3, 2, 2, 2, [255, 0, 0, 255]);
    dab(&mut d, paint, 9, 6, 1, 3, [0, 255, 0, 128]);
    assert!(h.commit(&d));
    let after = d.clone();

    let e = h.peek_undo().unwrap();
    assert_eq!(e.kind(), EditKind::Paint);
    assert_eq!(e.paint_target(), Some((paint, Rect::new(3, 2, 7, 7))));
    assert!(e.memory_bytes() < 7 * 7 * 4 + 256);

    assert_round_trip(&mut h, &mut d, &before, &after);
}

#[test]
fn paint_on_shifted_layer_uses_raster_coordinates() {
    let (mut d, paint, _) = doc();
    d.layer_mut(paint).unwrap().translate_by(-5, 3).unwrap();
    let before = d.clone();
    let mut h = History::new();
    h.begin_paint(&d, paint);
    dab(&mut d, paint, 0, 0, 1, 1, [1, 1, 1, 1]);
    h.commit(&d);
    assert_eq!(
        h.peek_undo().unwrap().paint_target(),
        Some((paint, Rect::new(0, 0, 1, 1)))
    );
    let after = d.clone();
    assert_round_trip(&mut h, &mut d, &before, &after);
}

#[test]
fn unchanged_stroke_records_nothing() {
    let (mut d, paint, _) = doc();
    let mut h = History::new();
    h.begin_paint(&d, paint);
    // Paint the same color that is already there.
    let c = d.layer(paint).unwrap();
    let rp_doc::LayerKind::Paint(p) = &c.kind else {
        panic!()
    };
    let same = p.pixels.get(4, 4);
    dab(&mut d, paint, 4, 4, 1, 1, same);
    assert!(!h.commit(&d));
    assert!(!h.can_undo());

    h.begin_snapshot(&d);
    assert!(!h.commit(&d));
    assert!(!h.can_undo());
    assert!(!h.commit(&d), "nothing open");
}

#[test]
fn begin_paint_refuses_non_paint_layers() {
    let (d, _, img) = doc();
    let mut h = History::new();
    assert!(!h.begin_paint(&d, img));
    assert!(!h.is_open());
}

#[test]
fn snapshot_steps_round_trip_every_structural_edit() {
    type Op = Box<dyn Fn(&mut Document, LayerId, LayerId)>;
    let ops: Vec<(&str, Op)> = vec![
        (
            "add layer",
            Box::new(|d, _, _| {
                d.push_layer(Layer::paint("new", CANVAS));
            }),
        ),
        (
            "remove layer",
            Box::new(|d, p, _| {
                d.remove_layer(p);
            }),
        ),
        (
            "reorder",
            Box::new(|d, p, _| {
                d.move_layer(p, 9);
            }),
        ),
        (
            "properties",
            Box::new(|d, p, _| {
                let l = d.layer_mut(p).unwrap();
                l.name = "renamed".into();
                l.visible = false;
                l.set_opacity(40);
                l.blend = BlendMode::Multiply;
                l.lock.pixels = true;
            }),
        ),
        (
            "mask",
            Box::new(|d, p, _| {
                d.layer_mut(p).unwrap().mask = Some(GrayImage::filled(CANVAS, 128));
            }),
        ),
        (
            "transform",
            Box::new(|d, _, i| {
                let l = d.layer_mut(i).unwrap();
                l.set_transform(Transform {
                    translate_x: 3.5,
                    translate_y: -2.0,
                    scale_x: 0.5,
                    scale_y: 2.0,
                    rotation: 0.3,
                })
                .unwrap();
            }),
        ),
        (
            "move paint layer",
            Box::new(|d, p, _| d.layer_mut(p).unwrap().translate_by(4, -1).unwrap()),
        ),
        (
            "filter",
            Box::new(|d, _, _| {
                d.dither.method = DitherMethod::Atkinson;
                d.dither.strength = 0.5;
            }),
        ),
        (
            "project settings",
            Box::new(|d, _, _| {
                d.canvas_size = Size::new(32, 24);
                d.output_size = Size::new(8, 6);
                d.layout = rp_doc::Layout::Desktop;
            }),
        ),
        (
            "insert image",
            Box::new(|d, _, _| {
                d.insert_layer(
                    1,
                    Layer::image("ins", SourceHash::new("cd"), Transform::at(1.0, 1.0)),
                );
            }),
        ),
        (
            "swatch add",
            Box::new(|d, _, _| d.swatches.push(Rgb::new(1, 2, 3))),
        ),
    ];
    for (name, op) in ops {
        let (mut d, paint, img) = doc();
        d.swatches.push(Rgb::new(9, 9, 9));
        let before = d.clone();
        let mut h = History::new();
        h.begin_snapshot(&d);
        op(&mut d, paint, img);
        assert!(h.commit(&d), "{name}");
        assert_eq!(h.peek_undo().unwrap().kind(), EditKind::Snapshot);
        let after = d.clone();
        assert_ne!(before, after, "{name}");
        assert_round_trip(&mut h, &mut d, &before, &after);
    }
}

#[test]
fn mixed_sequence_undoes_back_to_start_and_redoes_to_end() {
    let (mut d, paint, _) = doc();
    let mut h = History::new();
    let mut states = vec![d.clone()];

    h.begin_paint(&d, paint);
    dab(&mut d, paint, 0, 0, 4, 4, [9, 9, 9, 9]);
    h.commit(&d);
    states.push(d.clone());

    h.begin_snapshot(&d);
    let p2 = d.push_layer(Layer::paint("p2", CANVAS));
    h.commit(&d);
    states.push(d.clone());

    h.begin_paint(&d, p2);
    dab(&mut d, p2, 10, 10, 2, 2, [1, 2, 3, 4]);
    h.commit(&d);
    states.push(d.clone());

    h.begin_snapshot(&d);
    d.layer_mut(paint).unwrap().translate_by(2, 2).unwrap();
    h.commit(&d);
    states.push(d.clone());

    h.begin_paint(&d, paint);
    dab(&mut d, paint, 15, 11, 1, 1, [0, 0, 0, 0]);
    h.commit(&d);
    states.push(d.clone());

    for s in states.iter().rev().skip(1) {
        assert!(h.undo(&mut d));
        assert_eq!(&d, s);
    }
    assert!(!h.undo(&mut d));
    for s in states.iter().skip(1) {
        assert!(h.redo(&mut d));
        assert_eq!(&d, s);
    }
    assert!(!h.redo(&mut d));
}

#[test]
fn new_edit_clears_redo() {
    let (mut d, paint, _) = doc();
    let mut h = History::new();
    for i in 0..3 {
        h.begin_paint(&d, paint);
        dab(&mut d, paint, i, 0, 1, 1, [200, 0, 0, 255]);
        h.commit(&d);
    }
    h.undo(&mut d);
    h.undo(&mut d);
    assert_eq!(h.redo_len(), 2);
    assert!(h.can_redo());
    let mem_before = h.memory_used();

    h.begin_snapshot(&d);
    d.swatches.push(Rgb::new(5, 5, 5));
    h.commit(&d);
    assert!(!h.can_redo());
    assert_eq!(h.undo_len(), 2);
    assert!(!h.redo(&mut d));
    // Redo memory was released; only the new snapshot was added.
    let added = h.peek_undo().unwrap().memory_bytes();
    assert!(h.memory_used() < mem_before + added);
}

#[test]
fn eviction_drops_oldest_by_memory() {
    let (mut d, paint, _) = doc();
    // Each step is a full-raster patch: 16*12*4 = 768 bytes plus overhead.
    let mut h = History::with_budget(3 * 1024);
    for i in 0..10u8 {
        h.begin_paint(&d, paint);
        dab(
            &mut d,
            paint,
            0,
            0,
            CANVAS.width,
            CANVAS.height,
            [i, i, i, 255],
        );
        assert!(h.commit(&d));
        assert!(h.memory_used() <= h.budget());
    }
    let n = h.undo_len();
    assert!((2..10).contains(&n), "kept {n}");
    // The kept steps are the newest ones.
    while h.undo(&mut d) {}
    let rp_doc::LayerKind::Paint(p) = &d.layer(paint).unwrap().kind else {
        panic!()
    };
    let v = (10 - n - 1) as u8;
    assert_eq!(p.pixels.get(5, 5), [v, v, v, 255]);
    assert_eq!(h.redo_len(), n);
}

#[test]
fn newest_step_survives_even_over_budget() {
    let (mut d, paint, _) = doc();
    let mut h = History::with_budget(10);
    h.begin_snapshot(&d);
    d.remove_layer(paint);
    h.commit(&d);
    assert_eq!(h.undo_len(), 1);
    h.begin_snapshot(&d);
    d.swatches.push(Rgb::new(1, 1, 1));
    h.commit(&d);
    assert_eq!(h.undo_len(), 1);
    assert!(h.memory_used() > h.budget());
}

#[test]
fn set_budget_evicts_and_memory_counts_paint_pixels() {
    let (mut d, _, _) = doc();
    let big = Size::new(256, 256);
    d.push_layer(Layer::paint("big", big));
    let mut h = History::new();
    assert_eq!(h.budget(), DEFAULT_BUDGET);
    for _ in 0..4 {
        h.begin_snapshot(&d);
        d.swatches.push(Rgb::new(1, 1, 1));
        h.commit(&d);
    }
    // Every snapshot holds at least the big raster.
    assert!(h.memory_used() >= 4 * big.area() * 4);
    h.set_budget(2 * big.area() * 4 + 4096);
    assert_eq!(h.undo_len(), 2);
}

#[test]
fn clear_drops_everything() {
    let (mut d, paint, _) = doc();
    let mut h = History::new();
    h.begin_paint(&d, paint);
    dab(&mut d, paint, 0, 0, 1, 1, [1, 1, 1, 1]);
    h.commit(&d);
    h.undo(&mut d);
    h.begin_snapshot(&d);
    h.clear();
    assert!(!h.can_undo() && !h.can_redo() && !h.is_open());
    assert_eq!(h.memory_used(), 0);
}

#[test]
fn cancel_records_nothing() {
    let (mut d, paint, _) = doc();
    let mut h = History::new();
    h.begin_paint(&d, paint);
    dab(&mut d, paint, 0, 0, 1, 1, [1, 1, 1, 1]);
    h.cancel();
    assert!(!h.commit(&d));
    assert!(!h.can_undo());
}

#[test]
fn out_of_sync_patch_clears_history() {
    let (mut d, paint, _) = doc();
    let mut h = History::new();
    h.begin_paint(&d, paint);
    dab(&mut d, paint, 0, 0, 1, 1, [1, 1, 1, 1]);
    h.commit(&d);
    // Edit outside the history: the layer disappears.
    d.remove_layer(paint);
    let snapshot = d.clone();
    assert!(!h.undo(&mut d));
    assert_eq!(d, snapshot);
    assert!(!h.can_undo() && !h.can_redo());
}

#[test]
fn dirty_rect_is_minimal() {
    let a = RgbaImage::new(Size::new(20, 10));
    assert_eq!(dirty_rect(&a, &a, None), None);
    let mut b = a.clone();
    b.put(7, 3, [0, 0, 0, 1]);
    assert_eq!(dirty_rect(&a, &b, None), Some(Rect::new(7, 3, 1, 1)));
    b.put(2, 8, [0, 1, 0, 0]);
    b.put(19, 5, [1, 0, 0, 0]);
    assert_eq!(dirty_rect(&a, &b, None), Some(Rect::new(2, 3, 18, 6)));
    // Only one channel byte differing counts.
    let mut c = a.clone();
    c.put(0, 9, [0, 0, 0, 255]);
    assert_eq!(dirty_rect(&a, &c, None), Some(Rect::new(0, 9, 1, 1)));
    // A hint limits the search.
    assert_eq!(
        dirty_rect(&a, &b, Some(Rect::new(0, 0, 10, 10))),
        Some(Rect::new(2, 3, 6, 6))
    );
    assert_eq!(dirty_rect(&a, &b, Some(Rect::new(10, 0, 5, 5))), None);
}

#[test]
fn prebuilt_edits_round_trip() {
    let (mut d, paint, _) = doc();
    let before = d.clone();
    let rp_doc::LayerKind::Paint(p) = &d.layer(paint).unwrap().kind else {
        panic!()
    };
    let raster_before = p.pixels.clone();
    dab(&mut d, paint, 5, 5, 3, 2, [3, 3, 3, 3]);
    let rp_doc::LayerKind::Paint(p) = &d.layer(paint).unwrap().kind else {
        panic!()
    };
    let e = Edit::paint(paint, &raster_before, &p.pixels).unwrap();
    assert_eq!(e.paint_target(), Some((paint, Rect::new(5, 5, 3, 2))));
    let after = d.clone();
    let mut h = History::new();
    h.push(e);
    assert_round_trip(&mut h, &mut d, &before, &after);

    // Supplied rect (larger than needed, clipped to the raster).
    let mut h = History::new();
    let mut d = before.clone();
    dab(&mut d, paint, 14, 10, 2, 2, [4, 4, 4, 4]);
    let after = d.clone();
    let e = Edit::paint_rect(paint, &raster_before, Rect::new(12, 8, 100, 100));
    assert_eq!(e.paint_target(), Some((paint, Rect::new(12, 8, 4, 4))));
    h.push(e);
    assert_round_trip(&mut h, &mut d, &before, &after);

    // Snapshot.
    let mut h = History::new();
    let mut d = before.clone();
    d.dither.serpentine = !d.dither.serpentine;
    let after = d.clone();
    h.push(Edit::snapshot(before.clone()));
    assert_round_trip(&mut h, &mut d, &before, &after);
}

#[test]
fn commit_within_hint_limits_patch() {
    let (mut d, paint, _) = doc();
    let mut h = History::new();
    h.begin_paint(&d, paint);
    dab(&mut d, paint, 1, 1, 2, 2, [8, 8, 8, 8]);
    assert!(h.commit_within(&d, Some(Rect::new(0, 0, 4, 4))));
    assert_eq!(
        h.peek_undo().unwrap().paint_target(),
        Some((paint, Rect::new(1, 1, 2, 2)))
    );
}
