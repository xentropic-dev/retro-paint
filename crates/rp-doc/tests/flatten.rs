//! Flatten tests: one per blend mode, masks, opacity, visibility, paint
//! offsets, and image-layer transforms.

use std::f32::consts::FRAC_PI_2;

use rp_doc::{
    BlendMode, Document, GrayImage, Layer, LayerKind, RgbaImage, Size, SourceHash, SourceStore,
    Transform,
};

const R: [u8; 4] = [255, 0, 0, 255];
const G: [u8; 4] = [0, 255, 0, 255];
const B: [u8; 4] = [0, 0, 255, 255];
const W: [u8; 4] = [255, 255, 255, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

fn img(size: Size, px: &[[u8; 4]]) -> RgbaImage {
    RgbaImage::from_raw(size, px.concat()).unwrap()
}

fn pixels(img: &RgbaImage) -> Vec<[u8; 4]> {
    img.as_raw().as_chunks::<4>().0.to_vec()
}

fn paint(name: &str, pixels: RgbaImage) -> Layer {
    let mut l = Layer::paint(name, pixels.size());
    *l.pixels_mut().unwrap() = pixels;
    l
}

/// A 1×1 document: opaque `base`, then `top` with `mode`.
fn two_layer(base: [u8; 4], top: [u8; 4], mode: BlendMode) -> [u8; 4] {
    let s = Size::new(1, 1);
    let mut d = Document::new(s);
    d.push_layer(paint("base", RgbaImage::filled(s, base)));
    let mut t = paint("top", RgbaImage::filled(s, top));
    t.blend = mode;
    d.push_layer(t);
    d.flatten(&SourceStore::new()).get(0, 0)
}

const BASE: [u8; 4] = [200, 100, 0, 255];
const TOP: [u8; 4] = [100, 255, 128, 255];

#[test]
fn empty_document_is_transparent() {
    let d = Document::new(Size::new(3, 2));
    let out = d.flatten(&SourceStore::new());
    assert_eq!(out.size(), Size::new(3, 2));
    assert!(out.as_raw().iter().all(|&b| b == 0));
}

#[test]
fn single_layer_passes_through_exactly() {
    let s = Size::new(2, 2);
    let src = img(s, &[R, [10, 20, 30, 128], CLEAR, [1, 2, 3, 255]]);
    let mut d = Document::new(s);
    d.push_layer(paint("a", src.clone()));
    assert_eq!(d.flatten(&SourceStore::new()), src);
}

#[test]
fn blend_normal() {
    assert_eq!(two_layer(BASE, TOP, BlendMode::Normal), TOP);
    // A transparent top pixel leaves the base.
    assert_eq!(two_layer(BASE, CLEAR, BlendMode::Normal), BASE);
}

#[test]
fn blend_multiply() {
    // 200·100/255 = 78.4, 100·255/255 = 100, 0·128 = 0
    assert_eq!(two_layer(BASE, TOP, BlendMode::Multiply), [78, 100, 0, 255]);
    // White is the identity for multiply.
    assert_eq!(two_layer(BASE, W, BlendMode::Multiply), BASE);
}

#[test]
fn blend_screen() {
    // 200+100−78.4 = 221.6, 100+255−100 = 255, 0+128−0 = 128
    assert_eq!(
        two_layer(BASE, TOP, BlendMode::Screen),
        [222, 255, 128, 255]
    );
    // Black is the identity for screen.
    assert_eq!(two_layer(BASE, [0, 0, 0, 255], BlendMode::Screen), BASE);
}

#[test]
fn blend_overlay() {
    // r: backdrop 200 > half → screen(100, 145) = 188.1
    // g: backdrop 100 ≤ half → 2·100·255/255 = 200
    // b: backdrop 0 → 0
    assert_eq!(two_layer(BASE, TOP, BlendMode::Overlay), [188, 200, 0, 255]);
    // 50% gray is the identity for overlay (to within rounding).
    let out = two_layer(BASE, [128, 128, 128, 255], BlendMode::Overlay);
    for c in 0..3 {
        assert!(out[c].abs_diff(BASE[c]) <= 1, "{out:?}");
    }
}

#[test]
fn every_mode_over_transparent_is_normal() {
    for mode in BlendMode::ALL {
        let s = Size::new(1, 1);
        let mut d = Document::new(s);
        let mut t = paint("top", RgbaImage::filled(s, [10, 20, 30, 200]));
        t.blend = mode;
        d.push_layer(t);
        assert_eq!(d.flatten(&SourceStore::new()).get(0, 0), [10, 20, 30, 200]);
    }
}

#[test]
fn opacity_scales_layer_alpha() {
    let s = Size::new(1, 1);
    let mut d = Document::new(s);
    d.push_layer(paint("base", RgbaImage::filled(s, [0, 0, 0, 255])));
    let id = d.push_layer(paint("top", RgbaImage::filled(s, [200, 200, 200, 255])));
    d.layer_mut(id).unwrap().set_opacity(50);
    assert_eq!(
        d.flatten(&SourceStore::new()).get(0, 0),
        [100, 100, 100, 255]
    );

    d.layer_mut(id).unwrap().set_opacity(0);
    assert_eq!(d.flatten(&SourceStore::new()).get(0, 0), [0, 0, 0, 255]);
}

#[test]
fn hidden_layers_are_skipped() {
    let s = Size::new(1, 1);
    let mut d = Document::new(s);
    d.push_layer(paint("base", RgbaImage::filled(s, B)));
    let id = d.push_layer(paint("top", RgbaImage::filled(s, R)));
    d.layer_mut(id).unwrap().visible = false;
    assert_eq!(d.flatten(&SourceStore::new()).get(0, 0), B);
}

#[test]
fn stack_order_is_bottom_to_top() {
    let s = Size::new(1, 1);
    let mut d = Document::new(s);
    let r = d.push_layer(paint("r", RgbaImage::filled(s, R)));
    d.push_layer(paint("g", RgbaImage::filled(s, G)));
    assert_eq!(d.flatten(&SourceStore::new()).get(0, 0), G);
    d.move_layer(r, 1);
    assert_eq!(d.flatten(&SourceStore::new()).get(0, 0), R);
}

#[test]
fn mask_controls_visibility_per_pixel() {
    let s = Size::new(3, 1);
    let mut d = Document::new(s);
    d.push_layer(paint("base", RgbaImage::filled(s, [0, 0, 0, 255])));
    let mut top = paint("top", RgbaImage::filled(s, [200, 200, 200, 255]));
    top.mask = Some(GrayImage::from_raw(s, vec![0, 128, 255]).unwrap());
    d.push_layer(top);
    let out = pixels(&d.flatten(&SourceStore::new()));
    // 0 hides, 255 shows, 128 shows 128/255 of the layer: 200·0.502 = 100.4.
    assert_eq!(
        out,
        [[0, 0, 0, 255], [100, 100, 100, 255], [200, 200, 200, 255]]
    );
}

#[test]
fn mask_combines_with_opacity_and_blend_mode() {
    let s = Size::new(2, 1);
    let mut d = Document::new(s);
    d.push_layer(paint("base", RgbaImage::filled(s, W)));
    let mut top = paint("top", RgbaImage::filled(s, [0, 0, 0, 255]));
    top.blend = BlendMode::Multiply;
    top.set_opacity(50);
    top.mask = Some(GrayImage::from_raw(s, vec![255, 0]).unwrap());
    d.push_layer(top);
    let out = pixels(&d.flatten(&SourceStore::new()));
    // Multiply by black at 50%: 255·0.5 = 127.5 → 128. Masked pixel untouched.
    assert_eq!(out[1], W);
    assert!(out[0][0].abs_diff(128) <= 1, "{out:?}");
    assert_eq!(out[0][3], 255);
}

#[test]
fn mask_on_transparent_backdrop_scales_alpha() {
    let s = Size::new(1, 1);
    let mut d = Document::new(s);
    let mut top = paint("top", RgbaImage::filled(s, R));
    top.mask = Some(GrayImage::filled(s, 51));
    d.push_layer(top);
    assert_eq!(d.flatten(&SourceStore::new()).get(0, 0), [255, 0, 0, 51]);
}

#[test]
fn paint_layer_offset_shifts_and_clips() {
    let s = Size::new(3, 1);
    let mut d = Document::new(s);
    let id = d.push_layer(paint("a", img(s, &[R, G, B])));
    d.layer_mut(id).unwrap().translate_by(1, 0).unwrap();
    assert_eq!(pixels(&d.flatten(&SourceStore::new())), [CLEAR, R, G]);
    d.layer_mut(id).unwrap().translate_by(-2, 0).unwrap();
    assert_eq!(pixels(&d.flatten(&SourceStore::new())), [G, B, CLEAR]);
    d.layer_mut(id).unwrap().translate_by(0, 5).unwrap();
    assert_eq!(pixels(&d.flatten(&SourceStore::new())), [CLEAR; 3]);
}

fn image_doc(canvas: Size, src: RgbaImage, t: Transform) -> (Document, SourceStore) {
    let hash = SourceHash::new("cafe");
    let mut store = SourceStore::new();
    store.insert(hash.clone(), src);
    let mut d = Document::new(canvas);
    d.push_layer(Layer::image("img", hash, t));
    (d, store)
}

#[test]
fn image_layer_identity_is_exact() {
    let src = img(Size::new(2, 2), &[R, G, B, [9, 8, 7, 100]]);
    let (d, store) = image_doc(Size::new(2, 2), src.clone(), Transform::IDENTITY);
    assert_eq!(d.flatten(&store), src);
}

#[test]
fn image_layer_integer_translate_is_exact() {
    let src = img(Size::new(2, 1), &[R, G]);
    let (d, store) = image_doc(Size::new(4, 2), src, Transform::at(1.0, 1.0));
    let out = pixels(&d.flatten(&store));
    assert_eq!(out, [CLEAR, CLEAR, CLEAR, CLEAR, CLEAR, R, G, CLEAR]);
}

#[test]
fn image_layer_scale_is_bilinear() {
    let black = [0, 0, 0, 255];
    let src = img(Size::new(2, 1), &[black, W]);
    let t = Transform {
        scale_x: 2.0,
        ..Transform::IDENTITY
    };
    let (d, store) = image_doc(Size::new(5, 1), src, t);
    let out = pixels(&d.flatten(&store));
    let gray = |v: u8| [v, v, v, 255];
    // Centers 0.5, 1.5, 2.5, 3.5 map to source 0.25, 0.75, 1.25, 1.75:
    // clamped edge, 25%, 75%, clamped edge. Pixel 4 is outside the image.
    assert_eq!(out, [gray(0), gray(64), gray(191), gray(255), CLEAR]);
}

#[test]
fn image_layer_rotates_clockwise_about_center() {
    let src = img(Size::new(2, 2), &[R, G, B, W]);
    let t = Transform {
        rotation: FRAC_PI_2,
        ..Transform::IDENTITY
    };
    let (d, store) = image_doc(Size::new(2, 2), src, t);
    // R G      B R
    // B W  →   W G
    assert_eq!(pixels(&d.flatten(&store)), [B, R, W, G]);
}

#[test]
fn image_layer_rotated_off_square_bounds() {
    // A 4×2 image rotated 90° about its center (2, 1) covers x 1..3, y −1..3.
    let src = RgbaImage::filled(Size::new(4, 2), R);
    let t = Transform {
        rotation: FRAC_PI_2,
        ..Transform::IDENTITY
    };
    let (d, store) = image_doc(Size::new(4, 2), src, t);
    let out = pixels(&d.flatten(&store));
    assert_eq!(out, [CLEAR, R, R, CLEAR, CLEAR, R, R, CLEAR]);
}

#[test]
fn image_layer_blends_with_mask_and_opacity() {
    let s = Size::new(2, 1);
    let hash = SourceHash::new("beef");
    let mut store = SourceStore::new();
    store.insert(hash.clone(), RgbaImage::filled(s, [200, 200, 200, 255]));
    let mut d = Document::new(s);
    d.push_layer(paint("base", RgbaImage::filled(s, [0, 0, 0, 255])));
    let mut l = Layer::image("img", hash, Transform::IDENTITY);
    l.set_opacity(50);
    l.mask = Some(GrayImage::from_raw(s, vec![255, 0]).unwrap());
    d.push_layer(l);
    assert_eq!(
        pixels(&d.flatten(&store)),
        [[100, 100, 100, 255], [0, 0, 0, 255]]
    );
}

#[test]
fn missing_source_is_skipped() {
    let s = Size::new(1, 1);
    let mut d = Document::new(s);
    d.push_layer(paint("base", RgbaImage::filled(s, B)));
    d.push_layer(Layer::image(
        "img",
        SourceHash::new("missing"),
        Transform::IDENTITY,
    ));
    assert_eq!(d.flatten(&SourceStore::new()).get(0, 0), B);
}

#[test]
fn zero_scale_draws_nothing() {
    let src = RgbaImage::filled(Size::new(2, 2), R);
    let t = Transform {
        scale_x: 0.0,
        ..Transform::IDENTITY
    };
    let (d, store) = image_doc(Size::new(2, 2), src, t);
    assert!(d.flatten(&store).as_raw().iter().all(|&b| b == 0));
}

#[test]
fn layer_kinds_are_reported() {
    let (d, _) = image_doc(
        Size::new(1, 1),
        RgbaImage::new(Size::new(1, 1)),
        Transform::IDENTITY,
    );
    assert!(matches!(d.layers()[0].kind, LayerKind::Image(_)));
}
