//! Selection coverage and floating-piece tests: exact small polygons,
//! combine modes, lift + cancel / identity commit byte-identity, scaling,
//! and offset layers.

use rp_doc::{
    Document, Float, Layer, LayerKind, Rect, RgbaImage, SelectMode, Selection, Size, SourceStore,
    Transform,
};

const R: [u8; 4] = [255, 0, 0, 255];
const G: [u8; 4] = [0, 255, 0, 255];
const B: [u8; 4] = [0, 0, 255, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

fn rect_poly(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<[f32; 2]> {
    vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
}

/// Coverage rows as nested vectors, for readable asserts.
fn rows(sel: &Selection) -> Vec<Vec<u8>> {
    let w = sel.size().width as usize;
    sel.coverage()
        .as_raw()
        .chunks(w)
        .map(|r| r.to_vec())
        .collect()
}

fn paint_layer(pixels: RgbaImage, offset: (i32, i32)) -> Layer {
    let mut l = Layer::paint("p", pixels.size());
    *l.pixels_mut().unwrap() = pixels;
    l.translate_by(offset.0, offset.1).unwrap();
    l
}

fn layer_pixels(l: &Layer) -> &RgbaImage {
    match &l.kind {
        LayerKind::Paint(p) => &p.pixels,
        _ => panic!("not a paint layer"),
    }
}

/// A `size` raster with a distinct opaque-ish color per pixel and a few
/// translucent ones.
fn pattern(size: Size) -> RgbaImage {
    let mut v = Vec::with_capacity(size.area() * 4);
    for y in 0..size.height {
        for x in 0..size.width {
            let a = if (x + y) % 5 == 0 { 140 } else { 255 };
            v.extend_from_slice(&[(x * 37 % 256) as u8, (y * 53 % 256) as u8, 77, a]);
        }
    }
    RgbaImage::from_raw(size, v).unwrap()
}

// ---------------------------------------------------------------- coverage

#[test]
fn new_selection_is_empty() {
    let s = Selection::new(Size::new(4, 3));
    assert!(s.is_empty());
    assert_eq!(s.bounds(), None);
    assert!(s.coverage().as_raw().iter().all(|&c| c == 0));
}

#[test]
fn pixel_aligned_square_is_hard() {
    let s = Selection::from_polygon(Size::new(4, 4), &rect_poly(1.0, 1.0, 3.0, 3.0));
    assert_eq!(
        rows(&s),
        vec![
            vec![0, 0, 0, 0],
            vec![0, 255, 255, 0],
            vec![0, 255, 255, 0],
            vec![0, 0, 0, 0],
        ]
    );
    assert_eq!(s.bounds(), Some(Rect::new(1, 1, 2, 2)));
}

#[test]
fn half_pixel_square_is_antialiased() {
    // 4×4 samples per pixel: a half-covered edge pixel has 8/16 samples, a
    // quarter-covered corner 4/16.
    let s = Selection::from_polygon(Size::new(4, 4), &rect_poly(0.5, 0.5, 2.5, 2.5));
    assert_eq!(
        rows(&s),
        vec![
            vec![64, 128, 64, 0],
            vec![128, 255, 128, 0],
            vec![64, 128, 64, 0],
            vec![0, 0, 0, 0],
        ]
    );
    assert_eq!(s.bounds(), Some(Rect::new(0, 0, 3, 3)));
}

#[test]
fn winding_order_does_not_matter() {
    let mut ccw = rect_poly(0.5, 0.5, 2.5, 2.5);
    ccw.reverse();
    assert_eq!(
        Selection::from_polygon(Size::new(4, 4), &ccw),
        Selection::from_polygon(Size::new(4, 4), &rect_poly(0.5, 0.5, 2.5, 2.5))
    );
}

#[test]
fn even_odd_fill_leaves_a_hole() {
    // Outer square, then an inner square, as one closed path through a
    // zero-width bridge: even-odd makes the inner square a hole.
    let poly = vec![
        [0.0, 0.0],
        [5.0, 0.0],
        [5.0, 5.0],
        [0.0, 5.0],
        [0.0, 0.0],
        [2.0, 2.0],
        [2.0, 3.0],
        [3.0, 3.0],
        [3.0, 2.0],
        [2.0, 2.0],
    ];
    let s = Selection::from_polygon(Size::new(5, 5), &poly);
    assert_eq!(s.get(2, 2), 0);
    for (x, y) in [(0, 0), (4, 4), (1, 2), (3, 2), (2, 1), (2, 3)] {
        assert_eq!(s.get(x, y), 255, "({x}, {y})");
    }
}

#[test]
fn self_intersecting_bowtie_fills_both_lobes() {
    let poly = vec![[0.0, 0.0], [4.0, 4.0], [4.0, 0.0], [0.0, 4.0]];
    let s = Selection::from_polygon(Size::new(4, 4), &poly);
    // Left and right lobes are filled, top and bottom triangles are not.
    assert_eq!(s.get(0, 2), 255);
    assert_eq!(s.get(3, 1), 255);
    assert_eq!(s.get(1, 0), 0);
    assert_eq!(s.get(2, 3), 0);
}

#[test]
fn polygon_is_clipped_to_the_canvas() {
    let s = Selection::from_polygon(Size::new(4, 4), &rect_poly(-10.0, 2.0, 2.0, 50.0));
    assert_eq!(s.bounds(), Some(Rect::new(0, 2, 2, 2)));
    assert_eq!(s.get(1, 3), 255);
    assert_eq!(s.get(2, 3), 0);
    let off = Selection::from_polygon(Size::new(4, 4), &rect_poly(5.0, 5.0, 9.0, 9.0));
    assert!(off.is_empty());
}

#[test]
fn degenerate_polygons_select_nothing() {
    let c = Size::new(4, 4);
    assert!(Selection::from_polygon(c, &[]).is_empty());
    assert!(Selection::from_polygon(c, &[[1.0, 1.0], [3.0, 3.0]]).is_empty());
    assert!(Selection::from_polygon(c, &[[0.0, 1.0], [4.0, 1.0], [2.0, 1.0]]).is_empty());
    assert!(Selection::from_polygon(c, &[[0.0, 0.0], [f32::NAN, 1.0], [2.0, 2.0]]).is_empty());
}

#[test]
fn select_all_and_clear() {
    let mut s = Selection::all(Size::new(3, 2));
    assert_eq!(s.bounds(), Some(Rect::new(0, 0, 3, 2)));
    assert!(s.coverage().as_raw().iter().all(|&c| c == 255));
    s.clear();
    assert!(s.is_empty());
    assert!(s.coverage().as_raw().iter().all(|&c| c == 0));
    s.select_all();
    assert_eq!(s, Selection::all(Size::new(3, 2)));
}

#[test]
fn add_unions_and_subtract_cuts() {
    let c = Size::new(6, 4);
    let mut s = Selection::from_polygon(c, &rect_poly(0.0, 0.0, 2.0, 2.0));
    s.combine_polygon(SelectMode::Add, &rect_poly(3.0, 1.0, 5.0, 3.0));
    assert_eq!(
        rows(&s),
        vec![
            vec![255, 255, 0, 0, 0, 0],
            vec![255, 255, 0, 255, 255, 0],
            vec![0, 0, 0, 255, 255, 0],
            vec![0, 0, 0, 0, 0, 0],
        ]
    );
    assert_eq!(s.bounds(), Some(Rect::new(0, 0, 5, 3)));

    // Subtracting the left square shrinks the bounds to the right one.
    s.combine_polygon(SelectMode::Subtract, &rect_poly(0.0, 0.0, 2.0, 2.0));
    assert_eq!(s.bounds(), Some(Rect::new(3, 1, 2, 2)));
    assert_eq!(s.get(0, 0), 0);
    assert_eq!(s.get(4, 2), 255);

    // Replace drops everything else.
    s.combine_polygon(SelectMode::Replace, &rect_poly(0.0, 3.0, 1.0, 4.0));
    assert_eq!(s.bounds(), Some(Rect::new(0, 3, 1, 1)));
    assert_eq!(s.coverage().as_raw().iter().filter(|&&c| c != 0).count(), 1);

    // Subtracting everything empties it.
    s.combine_polygon(SelectMode::Subtract, &rect_poly(0.0, 0.0, 6.0, 4.0));
    assert!(s.is_empty());
}

#[test]
fn add_and_subtract_with_soft_edges() {
    let c = Size::new(4, 1);
    let mut s = Selection::from_polygon(c, &rect_poly(0.0, 0.0, 1.5, 1.0));
    assert_eq!(rows(&s), vec![vec![255, 128, 0, 0]]);
    // Add takes the larger coverage.
    s.combine_polygon(SelectMode::Add, &rect_poly(1.75, 0.0, 3.0, 1.0));
    assert_eq!(rows(&s), vec![vec![255, 128, 255, 0]]);
    s.combine_polygon(SelectMode::Add, &rect_poly(1.0, 0.0, 1.75, 1.0));
    assert_eq!(rows(&s), vec![vec![255, 191, 255, 0]]);
    // Subtract keeps at most the uncovered part.
    s.combine_polygon(SelectMode::Subtract, &rect_poly(0.5, 0.0, 2.25, 1.0));
    assert_eq!(rows(&s), vec![vec![127, 0, 191, 0]]);
}

#[test]
fn equal_selections_built_differently_compare_equal() {
    let c = Size::new(5, 5);
    let mut a = Selection::from_polygon(c, &rect_poly(0.0, 0.0, 5.0, 5.0));
    a.combine_polygon(SelectMode::Subtract, &rect_poly(0.0, 2.0, 5.0, 5.0));
    let b = Selection::from_polygon(c, &rect_poly(0.0, 0.0, 5.0, 2.0));
    assert_eq!(a, b);
}

// ---------------------------------------------------------------- float

#[test]
fn lift_splits_by_coverage() {
    let src = RgbaImage::from_raw(Size::new(3, 1), [R, G, [0, 0, 255, 200]].concat()).unwrap();
    let mut layer = paint_layer(src, (0, 0));
    // Covers pixel 0 fully, pixel 1 half, pixel 2 not at all.
    let sel = Selection::from_polygon(Size::new(3, 1), &rect_poly(0.0, 0.0, 1.5, 1.0));
    let f = Float::lift(&mut layer, &sel).unwrap().unwrap();
    assert_eq!(f.origin(), (0, 0));
    assert_eq!(f.piece().size(), Size::new(2, 1));
    assert_eq!(f.piece().get(0, 0), R);
    assert_eq!(f.piece().get(1, 0), [0, 255, 0, 128]);
    let px = layer_pixels(&layer);
    assert_eq!(px.get(0, 0), CLEAR);
    assert_eq!(px.get(1, 0), [0, 255, 0, 127]);
    assert_eq!(px.get(2, 0), [0, 0, 255, 200]);
    assert_eq!(f.transform(), Transform::at(0.0, 0.0));
    assert!(f.is_home());
}

#[test]
fn lift_then_cancel_is_byte_identical() {
    let size = Size::new(9, 7);
    let orig = pattern(size);
    let mut layer = paint_layer(orig.clone(), (0, 0));
    let mut sel = Selection::from_polygon(size, &[[1.3, 0.7], [7.9, 2.2], [4.1, 6.6]]);
    sel.combine_polygon(SelectMode::Add, &rect_poly(0.0, 4.5, 2.5, 7.0));
    let mut f = Float::lift(&mut layer, &sel).unwrap().unwrap();
    assert_ne!(layer_pixels(&layer), &orig);
    f.set_transform(Transform {
        translate_x: 3.0,
        translate_y: -1.0,
        scale_x: 1.7,
        scale_y: 0.6,
        rotation: 0.4,
    });
    f.cancel(&mut layer).unwrap();
    assert_eq!(layer_pixels(&layer), &orig);
}

#[test]
fn lift_then_identity_commit_is_byte_identical() {
    let size = Size::new(9, 7);
    let orig = pattern(size);
    let mut layer = paint_layer(orig.clone(), (0, 0));
    let sel = Selection::from_polygon(size, &[[1.3, 0.7], [7.9, 2.2], [4.1, 6.6]]);
    let f = Float::lift(&mut layer, &sel).unwrap().unwrap();
    let touched = f.commit(&mut layer).unwrap();
    assert_eq!(layer_pixels(&layer), &orig);
    assert_eq!(touched, sel.bounds());
}

#[test]
fn hard_selection_moved_by_whole_pixels_copies_bytes() {
    let size = Size::new(6, 4);
    let mut orig = RgbaImage::new(size);
    orig.put(0, 0, R);
    orig.put(1, 0, [9, 8, 7, 100]);
    orig.put(0, 1, B);
    let mut layer = paint_layer(orig, (0, 0));
    let sel = Selection::from_polygon(size, &rect_poly(0.0, 0.0, 2.0, 2.0));
    let mut f = Float::lift(&mut layer, &sel).unwrap().unwrap();
    f.set_transform(Transform::at(3.0, 2.0));
    assert!(!f.is_home());
    let touched = f.commit(&mut layer).unwrap();
    assert_eq!(touched, Some(Rect::new(0, 0, 5, 4)));
    let px = layer_pixels(&layer);
    assert_eq!(px.get(0, 0), CLEAR);
    assert_eq!(px.get(1, 0), CLEAR);
    assert_eq!(px.get(0, 1), CLEAR);
    assert_eq!(px.get(3, 2), R);
    assert_eq!(px.get(4, 2), [9, 8, 7, 100]);
    assert_eq!(px.get(3, 3), B);
    assert_eq!(px.get(4, 3), CLEAR);
}

#[test]
fn commit_is_source_over_the_destination() {
    let size = Size::new(4, 1);
    let mut orig = RgbaImage::filled(size, G);
    orig.put(0, 0, [255, 0, 0, 128]);
    let mut layer = paint_layer(orig, (0, 0));
    let sel = Selection::from_polygon(size, &rect_poly(0.0, 0.0, 1.0, 1.0));
    let mut f = Float::lift(&mut layer, &sel).unwrap().unwrap();
    f.set_transform(Transform::at(2.0, 0.0));
    f.commit(&mut layer).unwrap();
    let px = layer_pixels(&layer);
    assert_eq!(px.get(0, 0), CLEAR);
    // 50% red over opaque green.
    assert_eq!(px.get(2, 0), [128, 127, 0, 255]);
    assert_eq!(px.get(3, 0), G);
}

#[test]
fn scale_2x_of_a_known_piece() {
    let size = Size::new(8, 8);
    let mut orig = RgbaImage::new(size);
    orig.put(2, 2, R);
    orig.put(3, 2, B);
    orig.put(2, 3, R);
    orig.put(3, 3, B);
    let mut layer = paint_layer(orig, (0, 0));
    let sel = Selection::from_polygon(size, &rect_poly(2.0, 2.0, 4.0, 4.0));
    let mut f = Float::lift(&mut layer, &sel).unwrap().unwrap();
    assert_eq!(f.piece().size(), Size::new(2, 2));
    // Twice the size, same top-left.
    f.set_transform(Transform {
        scale_x: 2.0,
        scale_y: 2.0,
        ..Transform::at(2.0, 2.0)
    });
    assert_eq!(
        f.corners(),
        [[2.0, 2.0], [6.0, 2.0], [6.0, 6.0], [2.0, 6.0]]
    );
    let touched = f.commit(&mut layer).unwrap();
    assert_eq!(touched, Some(Rect::new(2, 2, 4, 4)));
    let px = layer_pixels(&layer);
    for y in 0..8 {
        for x in 0..8 {
            let p = px.get(x, y);
            let inside = (2..6).contains(&x) && (2..6).contains(&y);
            assert_eq!(p[3], if inside { 255 } else { 0 }, "({x}, {y})");
            if !inside {
                continue;
            }
            // Outer columns sit on (clamped) texel centers; inner columns
            // blend 3:1 toward the nearer texel, like flatten's bilinear.
            let expect = match x {
                2 => R,
                3 => [191, 0, 64, 255],
                4 => [64, 0, 191, 255],
                _ => B,
            };
            assert_eq!(p, expect, "({x}, {y})");
        }
    }
}

#[test]
fn offset_layer_lifts_in_canvas_coordinates() {
    // A 4×4 layer at canvas (3, 2) on a 10×10 canvas.
    let lsize = Size::new(4, 4);
    let orig = pattern(lsize);
    let mut layer = paint_layer(orig.clone(), (3, 2));
    let canvas = Size::new(10, 10);
    // Canvas square (1,1)-(5,4): overlaps layer pixels (0..2, 0..2).
    let sel = Selection::from_polygon(canvas, &rect_poly(1.0, 1.0, 5.0, 4.0));
    let f = Float::lift(&mut layer, &sel).unwrap().unwrap();
    assert_eq!(f.origin(), (3, 2));
    assert_eq!(f.piece().size(), Size::new(2, 2));
    assert_eq!(f.piece().get(0, 0), orig.get(0, 0));
    assert_eq!(f.piece().get(1, 1), orig.get(1, 1));
    let px = layer_pixels(&layer).clone();
    assert_eq!(px.get(1, 1), CLEAR);
    assert_eq!(px.get(2, 0), orig.get(2, 0));
    assert_eq!(px.get(0, 2), orig.get(0, 2));
    f.cancel(&mut layer).unwrap();
    assert_eq!(layer_pixels(&layer), &orig);
}

#[test]
fn offset_layer_commit_is_clipped_to_the_raster() {
    let lsize = Size::new(4, 4);
    let mut layer = paint_layer(RgbaImage::filled(lsize, R), (-2, 5));
    let canvas = Size::new(10, 10);
    let sel = Selection::from_polygon(canvas, &rect_poly(0.0, 5.0, 2.0, 7.0));
    let mut f = Float::lift(&mut layer, &sel).unwrap().unwrap();
    assert_eq!(f.origin(), (0, 5));
    // Layer pixels (2..4, 0..2) were lifted.
    assert_eq!(layer_pixels(&layer).get(2, 0), CLEAR);
    assert_eq!(layer_pixels(&layer).get(1, 0), R);
    // Move it one canvas pixel right: half of it lands past the layer's
    // right edge and is dropped.
    f.set_transform(Transform::at(1.0, 5.0));
    let touched = f.commit(&mut layer).unwrap();
    assert_eq!(touched, Some(Rect::new(2, 0, 2, 2)));
    let px = layer_pixels(&layer);
    assert_eq!(px.get(2, 0), CLEAR);
    assert_eq!(px.get(3, 0), R);
    assert_eq!(px.get(3, 1), R);
    assert_eq!(px.get(2, 1), CLEAR);
}

#[test]
fn lift_outside_the_layer_or_empty_selection_is_none() {
    let mut layer = paint_layer(pattern(Size::new(2, 2)), (6, 6));
    let canvas = Size::new(10, 10);
    let sel = Selection::from_polygon(canvas, &rect_poly(0.0, 0.0, 3.0, 3.0));
    assert!(Float::lift(&mut layer, &sel).unwrap().is_none());
    assert!(
        Float::lift(&mut layer, &Selection::new(canvas))
            .unwrap()
            .is_none()
    );
}

#[test]
fn lift_honors_locks_and_layer_kind() {
    let canvas = Size::new(4, 4);
    let sel = Selection::all(canvas);
    let mut layer = paint_layer(pattern(canvas), (0, 0));
    layer.lock.pixels = true;
    assert_eq!(
        Float::lift(&mut layer, &sel).unwrap_err(),
        rp_doc::EditError::PixelsLocked
    );
    let mut img = Layer::image("i", rp_doc::SourceHash::new("ab"), Transform::IDENTITY);
    assert_eq!(
        Float::lift(&mut img, &sel).unwrap_err(),
        rp_doc::EditError::NotAPaintLayer
    );
}

#[test]
fn preview_layer_flattens_like_the_committed_result() {
    // Hard selection, opaque pixels: the hole layer plus the preview layer
    // flatten to the same image as the committed layer.
    let canvas = Size::new(8, 6);
    let mut d = Document::new(canvas);
    d.push_layer(paint_layer(RgbaImage::filled(canvas, B), (0, 0)));
    let mut top = Layer::paint("top", canvas);
    for y in 1..4 {
        for x in 1..5 {
            top.pixels_mut()
                .unwrap()
                .put(x, y, [x as u8 * 40, y as u8 * 50, 9, 255]);
        }
    }
    let top_id = d.push_layer(top);
    let sel = Selection::from_polygon(canvas, &rect_poly(1.0, 1.0, 3.0, 3.0));
    let mut f = Float::lift(d.layer_mut(top_id).unwrap(), &sel)
        .unwrap()
        .unwrap();
    assert_eq!(f.layer(), top_id);
    f.set_transform(Transform::at(5.0, 3.0));

    let mut sources = SourceStore::new();
    sources.insert(Float::preview_source(), f.piece().clone());
    let mut preview = d.clone();
    let idx = preview.index_of(top_id).unwrap();
    let base = preview.layer(top_id).unwrap().clone();
    preview.insert_layer(idx + 1, f.preview_layer(&base));
    let shown = preview.flatten(&sources);

    f.commit(d.layer_mut(top_id).unwrap()).unwrap();
    assert_eq!(shown, d.flatten(&SourceStore::new()));
}

#[test]
fn placed_bounds_cover_the_transformed_piece() {
    let canvas = Size::new(20, 20);
    let mut layer = paint_layer(pattern(canvas), (0, 0));
    let sel = Selection::from_polygon(canvas, &rect_poly(4.0, 4.0, 8.0, 6.0));
    let mut f = Float::lift(&mut layer, &sel).unwrap().unwrap();
    assert_eq!(f.placed_bounds(canvas), Some(Rect::new(4, 4, 4, 2)));
    f.set_transform(Transform {
        rotation: std::f32::consts::FRAC_PI_2,
        ..Transform::at(4.0, 4.0)
    });
    // Rotated 90° about its center (6, 5): 2 wide, 4 tall.
    assert_eq!(f.placed_bounds(canvas), Some(Rect::new(5, 3, 2, 4)));
    f.set_transform(Transform::at(30.0, 30.0));
    assert_eq!(f.placed_bounds(canvas), None);
}
