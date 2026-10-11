//! The optimized flatten must match the original straightforward one byte
//! for byte. `reference` below is the flatten from before `flatten-perf`,
//! kept verbatim (apart from reaching the public API), and compared against
//! over a broad fixed-seed random set of documents.

use rp_doc::flatten::flatten_with_threads;
use rp_doc::{
    BlendMode, Document, GrayImage, Layer, LayerKind, RgbaImage, Size, SourceHash, SourceStore,
    Transform,
};

#[path = "reference_flatten/mod.rs"]
mod reference;

/// xorshift64, fixed seed: the same documents every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo) as u64 + 1) as i64
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    /// A byte biased toward the values that take special paths.
    fn byte(&mut self) -> u8 {
        match self.below(4) {
            0 => 0,
            1 => 255,
            _ => self.next() as u8,
        }
    }
}

fn random_rgba(rng: &mut Rng, size: Size) -> RgbaImage {
    let mut img = RgbaImage::new(size);
    // Pick a style so some layers are fully opaque, some mostly
    // transparent, and some noisy.
    let style = rng.below(4);
    for px in img.as_raw_mut().as_chunks_mut::<4>().0 {
        let a = match style {
            0 => 255,
            1 => {
                if rng.chance(80) {
                    0
                } else {
                    rng.byte()
                }
            }
            _ => rng.byte(),
        };
        px.copy_from_slice(&[rng.byte(), rng.byte(), rng.byte(), a]);
    }
    img
}

fn random_mask(rng: &mut Rng, canvas: Size) -> GrayImage {
    let size = if rng.chance(70) {
        canvas
    } else {
        Size::new(
            rng.range(1, canvas.width as i64 + 5) as u32,
            rng.range(1, canvas.height as i64 + 5) as u32,
        )
    };
    let mut m = GrayImage::filled(size, 255);
    for v in m.as_raw_mut() {
        *v = rng.byte();
    }
    m
}

fn random_transform(rng: &mut Rng, canvas: Size, src: Size) -> Transform {
    match rng.below(5) {
        // Integer placement, the common insert case.
        0 => Transform::at(
            rng.range(-(src.width as i64), canvas.width as i64) as f32,
            rng.range(-(src.height as i64), canvas.height as i64) as f32,
        ),
        // Degenerate scale: draws nothing.
        1 if rng.chance(30) => Transform {
            scale_x: 0.0,
            ..Transform::IDENTITY
        },
        _ => Transform {
            translate_x: (rng.unit() * 1.4 - 0.2) * canvas.width as f32,
            translate_y: (rng.unit() * 1.4 - 0.2) * canvas.height as f32,
            scale_x: (rng.unit() * 3.0 + 0.1) * if rng.chance(15) { -1.0 } else { 1.0 },
            scale_y: rng.unit() * 3.0 + 0.1,
            rotation: if rng.chance(30) {
                0.0
            } else {
                (rng.unit() - 0.5) * 7.0
            },
        },
    }
}

fn random_doc(rng: &mut Rng, canvas: Size, sources: &mut SourceStore) -> Document {
    let mut d = Document::new(canvas);
    let layers = rng.range(0, 6);
    for n in 0..layers {
        let mut layer = if rng.chance(70) {
            let size = if rng.chance(70) {
                canvas
            } else {
                Size::new(
                    rng.range(1, canvas.width as i64 + 8) as u32,
                    rng.range(1, canvas.height as i64 + 8) as u32,
                )
            };
            let mut l = Layer::paint(format!("p{n}"), size);
            *l.pixels_mut().unwrap() = random_rgba(rng, size);
            if rng.chance(40) {
                let (w, h) = (canvas.width as i64, canvas.height as i64);
                l.translate_by(
                    rng.range(-w - 2, w + 2) as i32,
                    rng.range(-h - 2, h + 2) as i32,
                )
                .unwrap();
            }
            l
        } else {
            let hash = SourceHash::new(format!("s{}", rng.below(4)));
            let src_size = Size::new(rng.range(1, 24) as u32, rng.range(1, 24) as u32);
            // Some sources stay missing, so the layer is skipped.
            if !sources.contains(&hash) && rng.chance(85) {
                sources.insert(hash.clone(), random_rgba(rng, src_size));
            }
            let real = sources.get(&hash).map_or(src_size, RgbaImage::size);
            Layer::image(format!("i{n}"), hash, random_transform(rng, canvas, real))
        };
        layer.visible = !rng.chance(10);
        layer.opacity = match rng.below(5) {
            0 | 1 => 100,
            2 => 0,
            3 => rng.range(1, 99) as u8,
            _ => rng.range(101, 255) as u8,
        };
        layer.blend = BlendMode::ALL[rng.below(4) as usize];
        if rng.chance(30) {
            layer.mask = Some(random_mask(rng, canvas));
        }
        d.push_layer(layer);
    }
    d
}

fn assert_same(d: &Document, sources: &SourceStore, case: usize) {
    let want = reference::flatten(d, sources);
    for threads in [1, 3, 8] {
        let got = flatten_with_threads(d, sources, threads);
        assert_eq!(got.size(), want.size());
        if got != want {
            let i = got
                .as_raw()
                .iter()
                .zip(want.as_raw())
                .position(|(a, b)| a != b)
                .unwrap()
                / 4;
            let w = d.canvas_size.width as usize;
            panic!(
                "case {case} ({threads} threads): pixel ({}, {}) is {:?}, reference {:?}",
                i % w,
                i / w,
                &got.as_raw()[i * 4..i * 4 + 4],
                &want.as_raw()[i * 4..i * 4 + 4],
            );
        }
    }
    assert_eq!(d.flatten(sources), want, "case {case} (default threads)");
}

#[test]
fn matches_reference_on_random_documents() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for case in 0..1500 {
        let canvas = Size::new(rng.range(1, 40) as u32, rng.range(1, 40) as u32);
        let mut sources = SourceStore::new();
        let d = random_doc(&mut rng, canvas, &mut sources);
        assert_same(&d, &sources, case);
    }
}

#[test]
fn matches_reference_on_tall_documents_split_into_bands() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for case in 0..40 {
        let canvas = Size::new(rng.range(1, 70) as u32, rng.range(100, 400) as u32);
        let mut sources = SourceStore::new();
        let d = random_doc(&mut rng, canvas, &mut sources);
        assert_same(&d, &sources, case);
    }
}

/// One layer over transparent, every color and alpha byte: the fast paths
/// for opaque and single-layer content must agree with the full formula.
#[test]
fn matches_reference_for_every_byte_pair() {
    let size = Size::new(256, 256);
    let mut img = RgbaImage::new(size);
    for y in 0..256u32 {
        for x in 0..256u32 {
            img.put(x, y, [x as u8, y as u8, (x ^ y) as u8, y as u8]);
        }
    }
    let sources = SourceStore::new();
    for mode in BlendMode::ALL {
        for opacity in [100, 37] {
            let mut d = Document::new(size);
            // Opaque gray under the layer, then the layer on its own.
            d.push_layer({
                let mut l = Layer::paint("under", size);
                *l.pixels_mut().unwrap() = RgbaImage::filled(size, [90, 160, 230, 255]);
                l
            });
            let mut l = Layer::paint("top", size);
            *l.pixels_mut().unwrap() = img.clone();
            l.blend = mode;
            l.opacity = opacity;
            d.push_layer(l);
            assert_same(&d, &sources, 0);
            d.layers_mut()[0].visible = false;
            assert_same(&d, &sources, 1);
        }
    }
}

#[test]
fn reference_agrees_on_a_known_case() {
    // Guards the copied reference itself against accidental edits.
    let s = Size::new(1, 1);
    let mut d = Document::new(s);
    let mut base = Layer::paint("b", s);
    *base.pixels_mut().unwrap() = RgbaImage::filled(s, [200, 100, 0, 255]);
    d.push_layer(base);
    let mut top = Layer::paint("t", s);
    *top.pixels_mut().unwrap() = RgbaImage::filled(s, [100, 255, 128, 255]);
    top.blend = BlendMode::Overlay;
    d.push_layer(top);
    assert_eq!(
        reference::flatten(&d, &SourceStore::new()).get(0, 0),
        [188, 200, 0, 255]
    );
    assert!(matches!(d.layers()[0].kind, LayerKind::Paint(_)));
}
