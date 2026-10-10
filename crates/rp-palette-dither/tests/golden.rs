//! Golden PNG tests for the dither methods and the full `render` pipeline.
//!
//! Two fixtures are generated in code (no third-party images): a gradient
//! chart and a synthetic "photo". Each is dithered with every method to the
//! full 16-color palette, plus two 4-color-mode cases. Outputs are compared
//! byte for byte with `tests/golden/*.png`; the fixtures themselves are
//! goldens too, so a drift in the generator shows up as its own failure.
//!
//! On mismatch the test writes `<name>.actual.png` next to the golden.
//! `UPDATE_GOLDEN=1 cargo test -p rp-palette-dither` rewrites the goldens.

use std::path::{Path, PathBuf};

use rp_palette_dither::test_util::assert_palette_only;
use rp_palette_dither::{
    DitherMethod, DitherParams, DitherSettings, FourColor, Intensity, NearestLut, Palette,
    PaletteChoice, PreAdjust, Preset, Renderer, Rgb, dither, linear_from_rgb, render,
};

struct Image {
    width: u32,
    height: u32,
    pixels: Vec<Rgb>,
}

impl Image {
    fn generate(width: u32, height: u32, f: impl Fn(f32, f32) -> [f32; 3]) -> Self {
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                let c = f(x as f32, y as f32);
                pixels.push(c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).into());
            }
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        self.pixels.iter().flat_map(|c| c.to_array()).collect()
    }
}

/// HSV (all `0..=1`) to sRGB.
fn hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h6 = (h.rem_euclid(1.0)) * 6.0;
    let f = |n: f32| {
        let k = (n + h6) % 6.0;
        v - v * s * k.min(4.0 - k).clamp(0.0, 1.0)
    };
    [f(5.0), f(3.0), f(1.0)]
}

/// 128×64. Rows 0–15: gray ramp. Rows 16–39: hue sweep at full value,
/// saturation 1 → 0.3 downward. Rows 40–63: hue sweep at full saturation,
/// value 1 → 0.2 downward.
fn gradient() -> Image {
    Image::generate(128, 64, |x, y| {
        let t = x / 127.0;
        if y < 16.0 {
            [t; 3]
        } else if y < 40.0 {
            hsv(t, 1.0 - 0.7 * (y - 16.0) / 23.0, 1.0)
        } else {
            hsv(t, 1.0, 1.0 - 0.8 * (y - 40.0) / 23.0)
        }
    })
}

/// Deterministic per-pixel noise in `-1..=1`.
fn noise(x: f32, y: f32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x9e37_79b1) ^ (y as u32).wrapping_mul(0x85eb_ca77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    (h & 0xffff) as f32 / 32767.5 - 1.0
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t)
}

fn rgb(r: u8, g: u8, b: u8) -> [f32; 3] {
    [r, g, b].map(|v| v as f32 / 255.0)
}

/// 128×96 synthetic landscape: sunset sky with a sun and glow, a far
/// mountain ridge, a green hill, a shaded red sphere with a highlight, and
/// a little sensor noise.
fn photo() -> Image {
    Image::generate(128, 96, |x, y| {
        let far = 50.0 + 6.0 * (x * 0.09).sin() + 3.0 * (x * 0.23 + 2.0).sin();
        let near = 68.0 + 5.0 * (x * 0.05 + 1.0).sin();
        let mut c = if y < far {
            let mut sky = mix(rgb(40, 90, 190), rgb(235, 175, 135), y / 55.0);
            let d2 = (x - 92.0).powi(2) + (y - 28.0).powi(2);
            let glow = (-d2 / 350.0).exp();
            sky = mix(sky, rgb(255, 210, 120), glow * 0.8);
            if d2 < 81.0 {
                sky = rgb(255, 245, 190);
            }
            sky
        } else if y < near {
            mix(rgb(120, 95, 140), rgb(80, 70, 110), (y - far) / 20.0)
        } else {
            mix(rgb(70, 140, 55), rgb(25, 60, 25), (y - near) / 28.0)
        };
        let (dx, dy) = ((x - 34.0) / 16.0, (y - 74.0) / 16.0);
        let r2 = dx * dx + dy * dy;
        if r2 < 1.0 {
            let n = [dx, dy, (1.0 - r2).sqrt()];
            let l = [-0.5f32, -0.6, 0.62];
            let ll = (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt();
            let ndl = ((n[0] * l[0] + n[1] * l[1] + n[2] * l[2]) / ll).max(0.0);
            let base = rgb(205, 55, 45).map(|v| v * (0.12 + 0.88 * ndl));
            let spec = ndl.powi(24);
            c = mix(base, [1.0; 3], spec);
        }
        let n = noise(x, y) * 6.0 / 255.0;
        c.map(|v| v + n)
    })
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn encode(path: &Path, width: u32, height: u32, rgb8: &[u8]) {
    let file = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgb8).unwrap();
}

fn decode(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let file = std::fs::File::open(path).ok()?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(file));
    dec.set_transformations(png::Transformations::EXPAND);
    let mut reader = dec.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!(
        (info.color_type, info.bit_depth),
        (png::ColorType::Rgb, png::BitDepth::Eight),
        "{} is not RGB8",
        path.display()
    );
    buf.truncate(info.buffer_size());
    Some((info.width, info.height, buf))
}

/// Compares `rgb8` with `tests/golden/<name>.png`, or rewrites it when
/// `UPDATE_GOLDEN` is set.
fn check_golden(name: &str, width: u32, height: u32, rgb8: &[u8]) {
    let dir = golden_dir();
    let path = dir.join(format!("{name}.png"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        encode(&path, width, height, rgb8);
        return;
    }
    let actual = dir.join(format!("{name}.actual.png"));
    match decode(&path) {
        Some((w, h, want)) if (w, h) == (width, height) && want == rgb8 => {
            let _ = std::fs::remove_file(&actual);
        }
        found => {
            encode(&actual, width, height, rgb8);
            let detail = match found {
                None => "golden is missing".to_string(),
                Some((w, h, _)) if (w, h) != (width, height) => {
                    format!("golden is {w}x{h}, output is {width}x{height}")
                }
                Some((_, _, want)) => {
                    let diff = want
                        .chunks(3)
                        .zip(rgb8.chunks(3))
                        .filter(|(a, b)| a != b)
                        .count();
                    format!("{diff} pixels differ")
                }
            };
            panic!(
                "{name}: {detail}; wrote {}. Look at both; if the change is intended, \
                 rerun with UPDATE_GOLDEN=1.",
                actual.display()
            );
        }
    }
}

fn method_name(m: DitherMethod) -> &'static str {
    match m {
        DitherMethod::None => "none",
        DitherMethod::Bayer4 => "bayer4",
        DitherMethod::FloydSteinberg => "floyd_steinberg",
        DitherMethod::Atkinson => "atkinson",
    }
}

fn run(fixture: &str, img: &Image, palette: Palette, params: &DitherParams, name: &str) {
    let lut = NearestLut::new(palette);
    let out = dither(
        img.width,
        img.height,
        &linear_from_rgb(&img.pixels),
        &lut,
        params,
    );
    assert_palette_only(out.pixels.iter().copied(), &palette);
    assert_eq!(
        (out.width, out.height),
        (img.width, img.height),
        "{fixture}"
    );
    check_golden(name, out.width, out.height, &out.to_rgb8());
}

fn fixtures() -> [(&'static str, Image); 2] {
    [("gradient", gradient()), ("photo", photo())]
}

#[test]
fn fixtures_are_stable() {
    for (name, img) in fixtures() {
        check_golden(
            &format!("{name}_input"),
            img.width,
            img.height,
            &img.bytes(),
        );
    }
}

#[test]
fn every_method_full_palette() {
    for (fixture, img) in fixtures() {
        for method in DitherMethod::ALL {
            run(
                fixture,
                &img,
                Palette::full(),
                &DitherParams::with_method(method),
                &format!("{fixture}_{}", method_name(method)),
            );
        }
    }
}

#[test]
fn four_color_modes() {
    let p1 = FourColor::new(Preset::Palette1, Intensity::High).palette();
    let (_, grad) = &fixtures()[0];
    let photo = photo();
    run(
        "gradient",
        grad,
        p1,
        &DitherParams::with_method(DitherMethod::Bayer4),
        "gradient_bayer4_palette1_high",
    );
    run(
        "photo",
        &photo,
        p1,
        &DitherParams::with_method(DitherMethod::FloydSteinberg),
        "photo_floyd_steinberg_palette1_high",
    );
}

/// Runs `render` on `rgba` and checks the output against a golden, plus the
/// palette rule and that a `Renderer` gives the same bytes.
fn run_render(
    rgba: &[u8],
    canvas: (u32, u32),
    output: (u32, u32),
    settings: &DitherSettings,
    name: &str,
) {
    let out = render(rgba, canvas, output, settings);
    let palette = settings.palette.palette();
    assert_palette_only(out.pixels.iter().copied(), &palette);
    assert_eq!((out.width, out.height), output, "{name}");
    assert_eq!(out, Renderer::new().render(rgba, canvas, output, settings));
    check_golden(name, out.width, out.height, &out.to_rgb8());
}

fn opaque_rgba(img: &Image) -> Vec<u8> {
    img.pixels
        .iter()
        .flat_map(|c| {
            let [r, g, b] = c.to_array();
            [r, g, b, 255]
        })
        .collect()
}

#[test]
fn render_photo_half_size_default() {
    let img = photo();
    run_render(
        &opaque_rgba(&img),
        (img.width, img.height),
        (64, 48),
        &DitherSettings::default(),
        "render_photo_64x48_floyd_steinberg",
    );
}

#[test]
fn render_photo_fractional_bayer_adjusted() {
    let img = photo();
    let settings = DitherSettings {
        adjust: PreAdjust {
            brightness: 0.05,
            contrast: 0.25,
            saturation: 0.4,
        },
        ..DitherSettings::with_method(DitherMethod::Bayer4)
    };
    run_render(
        &opaque_rgba(&img),
        (img.width, img.height),
        (80, 60),
        &settings,
        "render_photo_80x60_bayer4_adjusted",
    );
}

#[test]
fn render_photo_alpha_fade_four_color() {
    // Alpha fades from 0 at the left edge to opaque at the right, so the
    // left of the output is composited toward black.
    let img = photo();
    let mut rgba = opaque_rgba(&img);
    for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
        let x = (i as u32 % img.width) as f32;
        px[3] = (x / (img.width - 1) as f32 * 255.0 + 0.5) as u8;
    }
    let settings = DitherSettings {
        palette: PaletteChoice::FourColor(FourColor::new(Preset::Palette0, Intensity::High)),
        ..DitherSettings::with_method(DitherMethod::Atkinson)
    };
    run_render(
        &rgba,
        (img.width, img.height),
        (96, 72),
        &settings,
        "render_photo_96x72_atkinson_palette0_alpha",
    );
}

#[test]
fn render_identity_size_matches_dither() {
    // output == canvas: render is exactly dither of the linear input.
    let img = photo();
    let settings = DitherSettings::default();
    let out = render(
        &opaque_rgba(&img),
        (img.width, img.height),
        (img.width, img.height),
        &settings,
    );
    let want = dither(
        img.width,
        img.height,
        &linear_from_rgb(&img.pixels),
        &NearestLut::new(Palette::full()),
        &settings.params(),
    );
    assert_eq!(out, want);
}
