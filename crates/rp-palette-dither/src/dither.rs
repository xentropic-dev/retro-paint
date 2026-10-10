//! The v1 dither methods: none, Bayer 4×4, Floyd–Steinberg, Atkinson.
//!
//! See docs/filter-and-palette.md, "Dithering methods" and "Decisions".
//!
//! Input is a row-major image in **linear light** (`[f32; 3]` per pixel,
//! nominally `0.0..=1.0`); the pipeline box-downscales and pre-adjusts in
//! that form and hands it here. [`linear_from_rgb`] converts an 8-bit sRGB
//! image. Output is an [`IndexedImage`]: one [`CgaColor`] per pixel, all
//! from the [`NearestLut`]'s palette.
//!
//! - **None**: each pixel maps to its nearest palette color.
//! - **Bayer 4×4**: ordered dither. The threshold offset is added in
//!   **gamma-encoded sRGB** (see [`bayer_pixel`]), then the nearest color is
//!   looked up. Each pixel is independent of its neighbors.
//! - **Floyd–Steinberg / Atkinson**: error diffusion in floating-point
//!   linear light (see [`diffuse`]), with a strength factor and optional
//!   serpentine scanning.
//!
//! The per-pixel building blocks ([`nearest_pixel`], [`bayer_pixel`],
//! [`bayer_threshold`]) and the kernels ([`FLOYD_STEINBERG`], [`ATKINSON`])
//! are public so the pipeline can reuse them.

use crate::color::{linear_to_srgb, srgb8_to_linear};
use crate::lut::NearestLut;
use crate::palette::{CgaColor, Palette, Rgb};

/// A dithered image: one palette color per pixel, row-major.
///
/// Every pixel is a color of `palette`. Field-for-field this matches
/// `rp_doc::IndexedFrame` (`size` there is `width`/`height` here), so the
/// conversion is a move of `pixels` with no copy.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct IndexedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<CgaColor>,
    /// The palette the dither drew from.
    pub palette: Palette,
}

impl IndexedImage {
    /// The color at `(x, y)`, or `None` outside the image.
    pub fn get(&self, x: u32, y: u32) -> Option<CgaColor> {
        (x < self.width && y < self.height)
            .then(|| self.pixels[y as usize * self.width as usize + x as usize])
    }

    /// Packed RGB8 bytes (3 per pixel, row-major), e.g. for PNG export.
    pub fn to_rgb8(&self) -> Vec<u8> {
        self.pixels
            .iter()
            .flat_map(|c| c.rgb().to_array())
            .collect()
    }
}

/// Which dither to run.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum DitherMethod {
    /// Nearest color, no dither.
    None,
    /// Ordered dither with the 4×4 Bayer matrix.
    Bayer4,
    /// Floyd–Steinberg error diffusion.
    #[default]
    FloydSteinberg,
    /// Atkinson error diffusion (spreads 6/8 of the error).
    Atkinson,
}

impl DitherMethod {
    /// All methods, in UI order.
    pub const ALL: [DitherMethod; 4] = [
        DitherMethod::None,
        DitherMethod::Bayer4,
        DitherMethod::FloydSteinberg,
        DitherMethod::Atkinson,
    ];

    /// The diffusion kernel, for the error-diffusion methods.
    pub fn kernel(self) -> Option<&'static DiffusionKernel> {
        match self {
            DitherMethod::FloydSteinberg => Some(&FLOYD_STEINBERG),
            DitherMethod::Atkinson => Some(&ATKINSON),
            DitherMethod::None | DitherMethod::Bayer4 => None,
        }
    }
}

/// Default Bayer spread: one CGA channel step (85/255) in sRGB units, so a
/// color between two levels is split between them in proportion.
pub const DEFAULT_BAYER_SPREAD: f32 = 85.0 / 255.0;

/// Parameters for [`dither`]. Fields a method does not use are ignored.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DitherParams {
    pub method: DitherMethod,
    /// Bayer pattern strength in sRGB channel units (`1.0` = full range).
    /// The offset added to each channel is `threshold * spread`, with
    /// `threshold` in about `-0.47..=0.47`. Negative values act as `0`.
    pub bayer_spread: f32,
    /// Fraction of the quantization error diffused, `0.0..=1.0`
    /// (the UI's 0–100%). Clamped. `0.0` gives the same output as
    /// [`DitherMethod::None`].
    pub strength: f32,
    /// Alternate scan direction each row (odd rows right to left).
    pub serpentine: bool,
}

impl Default for DitherParams {
    fn default() -> Self {
        Self {
            method: DitherMethod::default(),
            bayer_spread: DEFAULT_BAYER_SPREAD,
            strength: 1.0,
            serpentine: true,
        }
    }
}

impl DitherParams {
    /// Default parameters with the given method.
    pub fn with_method(method: DitherMethod) -> Self {
        Self {
            method,
            ..Self::default()
        }
    }
}

/// Converts an 8-bit sRGB image to the linear-light input [`dither`] takes.
pub fn linear_from_rgb(pixels: &[Rgb]) -> Vec<[f32; 3]> {
    pixels
        .iter()
        .map(|c| c.to_array().map(srgb8_to_linear))
        .collect()
}

/// Dithers a `width`×`height` linear-light image to `lut`'s palette.
///
/// This is the single dither entry point for both the live view and export.
/// Panics if `linear.len() != width * height`.
pub fn dither(
    width: u32,
    height: u32,
    linear: &[[f32; 3]],
    lut: &NearestLut,
    params: &DitherParams,
) -> IndexedImage {
    check_len(width, height, linear);
    let pixels = match params.method {
        DitherMethod::None => linear.iter().map(|&p| nearest_pixel(lut, p)).collect(),
        DitherMethod::Bayer4 => {
            let w = width.max(1) as usize;
            linear
                .iter()
                .enumerate()
                .map(|(i, &p)| {
                    let (x, y) = ((i % w) as u32, (i / w) as u32);
                    bayer_pixel(lut, srgb_from_linear(p), x, y, params.bayer_spread)
                })
                .collect()
        }
        DitherMethod::FloydSteinberg | DitherMethod::Atkinson => {
            let kernel = params.method.kernel().expect("diffusion method");
            return diffuse(
                width,
                height,
                linear,
                lut,
                kernel,
                params.strength,
                params.serpentine,
            );
        }
    };
    IndexedImage {
        width,
        height,
        pixels,
        palette: lut.palette(),
    }
}

fn check_len(width: u32, height: u32, linear: &[[f32; 3]]) {
    assert_eq!(
        linear.len(),
        width as usize * height as usize,
        "pixel count does not match {width}x{height}"
    );
}

/// No dither: the nearest palette color to a linear-light pixel.
#[inline]
pub fn nearest_pixel(lut: &NearestLut, linear: [f32; 3]) -> CgaColor {
    lut.nearest_linear(linear)
}

/// The 4×4 Bayer matrix from the docs, `M[y][x]`.
pub const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// The Bayer threshold at `(x, y)`: `(M[y mod 4][x mod 4] + 0.5) / 16 - 0.5`,
/// in `-0.46875..=0.46875`, mean zero over each 4×4 tile.
#[inline]
pub fn bayer_threshold(x: u32, y: u32) -> f32 {
    (BAYER4[(y % 4) as usize][(x % 4) as usize] as f32 + 0.5) / 16.0 - 0.5
}

/// Ordered dither of one pixel given in gamma-encoded sRGB (`0.0..=1.0`).
///
/// Adds `bayer_threshold(x, y) * spread` to every channel **in sRGB**, clamps,
/// rounds to 8 bits, and looks up the nearest color. sRGB rather than linear
/// because the offset is a fixed step: in sRGB it is about perceptually even
/// across the tone range and matches the CGA levels, which are evenly spaced
/// in sRGB (0, 85, 170, 255). In linear light the same step would swamp the
/// darks and barely show in the highlights.
#[inline]
pub fn bayer_pixel(lut: &NearestLut, srgb: [f32; 3], x: u32, y: u32, spread: f32) -> CgaColor {
    let offset = bayer_threshold(x, y) * spread.max(0.0);
    let rgb = srgb.map(|c| ((c + offset).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
    lut.nearest(rgb.into())
}

/// Linear light to gamma-encoded sRGB, clamped to `0.0..=1.0`.
#[inline]
pub fn srgb_from_linear(linear: [f32; 3]) -> [f32; 3] {
    linear.map(|c| linear_to_srgb(c.clamp(0.0, 1.0)))
}

/// An error-diffusion kernel: where the error of the current pixel goes.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DiffusionKernel {
    /// `(dx, dy, weight)` for scanning left to right; `dy >= 0`, and taps on
    /// the current row have `dx > 0`. Mirrored (`-dx`) on right-to-left rows.
    pub taps: &'static [(i32, i32, f32)],
}

impl DiffusionKernel {
    /// Sum of the weights: `1.0` for Floyd–Steinberg, `0.75` for Atkinson.
    pub fn total_weight(&self) -> f32 {
        self.taps.iter().map(|t| t.2).sum()
    }
}

/// Floyd–Steinberg (÷16): `X 7 / 3 5 1`.
pub const FLOYD_STEINBERG: DiffusionKernel = DiffusionKernel {
    taps: &[
        (1, 0, 7.0 / 16.0),
        (-1, 1, 3.0 / 16.0),
        (0, 1, 5.0 / 16.0),
        (1, 1, 1.0 / 16.0),
    ],
};

/// Atkinson (÷8, 6/8 of the error spread): `X 1 1 / 1 1 1 / 1`.
pub const ATKINSON: DiffusionKernel = DiffusionKernel {
    taps: &[
        (1, 0, 1.0 / 8.0),
        (2, 0, 1.0 / 8.0),
        (-1, 1, 1.0 / 8.0),
        (0, 1, 1.0 / 8.0),
        (1, 1, 1.0 / 8.0),
        (0, 2, 1.0 / 8.0),
    ],
};

/// How far outside `0.0..=1.0` a pixel's value plus diffused error may go
/// in [`diffuse`] before it is clamped.
pub const ERROR_MARGIN: f32 = 1.0;

/// Error diffusion in floating-point linear light.
///
/// For each pixel in scan order: the pixel plus the error it has received
/// is clamped per channel to `-ERROR_MARGIN..=1.0 + ERROR_MARGIN`, the
/// nearest palette color to that value (clamped to `0.0..=1.0`) is picked,
/// and `(value - color.linear()) * strength` is spread to unvisited
/// neighbors by `kernel`.
///
/// The clamp is wider than the gamut on purpose. OKLab nearest-color splits
/// are not at linear midpoints (black/white splits near linear 0.125), so a
/// pixel's accumulated value often overshoots below 0 or above 1; clamping
/// at the gamut would throw that error away and bias the average. The
/// margin still bounds the error when a palette subset can never reach the
/// input color (white input, palette without white). Taps that fall outside the image are dropped.
/// With `serpentine`, odd rows run right to left with the kernel mirrored.
///
/// Panics if `linear.len() != width * height`.
pub fn diffuse(
    width: u32,
    height: u32,
    linear: &[[f32; 3]],
    lut: &NearestLut,
    kernel: &DiffusionKernel,
    strength: f32,
    serpentine: bool,
) -> IndexedImage {
    check_len(width, height, linear);
    let strength = if strength.is_nan() {
        0.0
    } else {
        strength.clamp(0.0, 1.0)
    };
    let (w, h) = (width as i64, height as i64);
    let palette = lut.palette();
    let mut pal_linear = [[0.0f32; 3]; 16];
    for &c in palette.colors() {
        pal_linear[c.index() as usize] = c.linear();
    }

    let mut work = linear.to_vec();
    let mut pixels = vec![palette.colors()[0]; linear.len()];
    for y in 0..h {
        let reverse = serpentine && y % 2 == 1;
        let dir = if reverse { -1 } else { 1 };
        for step in 0..w {
            let x = if reverse { w - 1 - step } else { step };
            let i = (y * w + x) as usize;
            let v = work[i].map(|c| c.clamp(-ERROR_MARGIN, 1.0 + ERROR_MARGIN));
            let c = lut.nearest_linear(v);
            pixels[i] = c;
            if strength == 0.0 {
                continue;
            }
            let q = pal_linear[c.index() as usize];
            let err = [
                (v[0] - q[0]) * strength,
                (v[1] - q[1]) * strength,
                (v[2] - q[2]) * strength,
            ];
            for &(dx, dy, wt) in kernel.taps {
                let (nx, ny) = (x + dx as i64 * dir, y + dy as i64);
                if nx < 0 || nx >= w || ny >= h {
                    continue;
                }
                let n = &mut work[(ny * w + nx) as usize];
                for ch in 0..3 {
                    n[ch] += err[ch] * wt;
                }
            }
        }
    }
    IndexedImage {
        width,
        height,
        pixels,
        palette,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::srgb_to_linear;
    use crate::palette::{FourColor, Intensity, Preset};
    use crate::test_util::assert_palette_only;
    use CgaColor::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        fn rgb(&mut self) -> Rgb {
            let v = self.next();
            Rgb::new(v as u8, (v >> 8) as u8, (v >> 16) as u8)
        }
    }

    fn random_image(w: u32, h: u32, seed: u64) -> Vec<[f32; 3]> {
        let mut rng = Rng(seed);
        let px: Vec<Rgb> = (0..w * h).map(|_| rng.rgb()).collect();
        linear_from_rgb(&px)
    }

    fn uniform(w: u32, h: u32, lin: [f32; 3]) -> Vec<[f32; 3]> {
        vec![lin; (w * h) as usize]
    }

    fn all_params() -> Vec<DitherParams> {
        let mut out = Vec::new();
        for method in DitherMethod::ALL {
            for (strength, serpentine) in [(1.0, true), (0.5, false), (0.0, true)] {
                out.push(DitherParams {
                    method,
                    strength,
                    serpentine,
                    ..Default::default()
                });
            }
        }
        out
    }

    fn palettes() -> Vec<Palette> {
        vec![
            Palette::full(),
            Palette::new([Black, White]).unwrap(),
            Palette::new([Yellow]).unwrap(),
            Palette::new([Blue, Red]).unwrap(),
            FourColor::new(Preset::Palette1, Intensity::High).palette(),
            FourColor::new(Preset::Palette0, Intensity::Low)
                .with_background(Blue)
                .palette(),
        ]
    }

    fn white_fraction(img: &IndexedImage) -> f32 {
        img.pixels.iter().filter(|&&c| c == White).count() as f32 / img.pixels.len() as f32
    }

    #[test]
    fn every_method_outputs_only_palette_colors() {
        let img = random_image(37, 23, 0xdead_beef);
        for p in palettes() {
            let lut = NearestLut::new(p);
            for params in all_params() {
                let out = dither(37, 23, &img, &lut, &params);
                assert_eq!((out.width, out.height, out.palette), (37, 23, p));
                assert_eq!(out.pixels.len(), 37 * 23);
                assert_palette_only(out.pixels.iter().copied(), &p);
            }
        }
    }

    #[test]
    fn out_of_range_input_stays_in_palette() {
        // Subset palettes that cannot reach the input color must not blow up.
        let img = uniform(16, 16, [3.0, -2.0, 0.5]);
        for p in palettes() {
            let lut = NearestLut::new(p);
            for params in all_params() {
                let out = dither(16, 16, &img, &lut, &params);
                assert_palette_only(out.pixels.iter().copied(), &p);
            }
        }
    }

    #[test]
    fn none_is_the_lut_nearest_color() {
        let img = random_image(20, 10, 7);
        let lut = NearestLut::new(Palette::full());
        let out = dither(
            20,
            10,
            &img,
            &lut,
            &DitherParams::with_method(DitherMethod::None),
        );
        for (i, &p) in img.iter().enumerate() {
            assert_eq!(out.pixels[i], lut.nearest_linear(p));
        }
    }

    #[test]
    fn palette_colors_pass_through_unchanged() {
        // A pixel already in the palette has zero error.
        let lut = NearestLut::new(Palette::full());
        let img: Vec<[f32; 3]> = (0..64).map(|i| CgaColor::ALL[i % 16].linear()).collect();
        for method in [
            DitherMethod::None,
            DitherMethod::FloydSteinberg,
            DitherMethod::Atkinson,
        ] {
            let out = dither(8, 8, &img, &lut, &DitherParams::with_method(method));
            for (i, &c) in out.pixels.iter().enumerate() {
                assert_eq!(c, CgaColor::ALL[i % 16], "{method:?}");
            }
        }
    }

    #[test]
    fn zero_strength_and_zero_spread_equal_none() {
        let img = random_image(31, 17, 99);
        let lut = NearestLut::new(Palette::full());
        let none = dither(
            31,
            17,
            &img,
            &lut,
            &DitherParams::with_method(DitherMethod::None),
        );
        for method in DitherMethod::ALL {
            for serpentine in [true, false] {
                let params = DitherParams {
                    method,
                    strength: 0.0,
                    bayer_spread: 0.0,
                    serpentine,
                };
                assert_eq!(dither(31, 17, &img, &lut, &params), none, "{method:?}");
            }
        }
    }

    #[test]
    fn bayer_threshold_matches_doc_formula() {
        assert_eq!(bayer_threshold(0, 0), 0.5 / 16.0 - 0.5);
        assert_eq!(bayer_threshold(0, 3), 15.5 / 16.0 - 0.5);
        assert_eq!(bayer_threshold(3, 3), 5.5 / 16.0 - 0.5);
        assert_eq!(bayer_threshold(5, 6), bayer_threshold(1, 2));
        let mut sum = 0.0;
        let mut seen = [false; 16];
        for y in 0..4 {
            for x in 0..4 {
                sum += bayer_threshold(x, y);
                seen[BAYER4[y as usize][x as usize] as usize] = true;
            }
        }
        assert!(sum.abs() < 1e-6);
        assert!(seen.iter().all(|&s| s), "matrix is a permutation of 0..16");
    }

    #[test]
    fn bayer_pixels_are_independent() {
        // Changing one input pixel changes at most that output pixel.
        let mut img = random_image(16, 16, 3);
        let lut = NearestLut::new(Palette::full());
        let params = DitherParams::with_method(DitherMethod::Bayer4);
        let a = dither(16, 16, &img, &lut, &params);
        img[5 * 16 + 7] = [1.0, 0.0, 1.0];
        let b = dither(16, 16, &img, &lut, &params);
        for i in 0..256 {
            if i != 5 * 16 + 7 {
                assert_eq!(a.pixels[i], b.pixels[i], "pixel {i}");
            }
        }
    }

    #[test]
    fn bayer_tiles_with_period_four() {
        let lut = NearestLut::new(Palette::new([Black, White]).unwrap());
        let gray = [srgb_to_linear(0.5); 3];
        let out = dither(
            12,
            12,
            &uniform(12, 12, gray),
            &lut,
            &DitherParams {
                method: DitherMethod::Bayer4,
                bayer_spread: 1.0,
                ..Default::default()
            },
        );
        for y in 0..12 {
            for x in 0..12 {
                assert_eq!(out.get(x, y), out.get(x % 4, y % 4));
            }
        }
        // A mid-gray split between black and white uses both.
        let f = white_fraction(&out);
        assert!(f > 0.2 && f < 0.8, "{f}");
    }

    #[test]
    fn bayer_spread_splits_between_cga_levels() {
        // sRGB halfway between dark gray (85) and light gray (170) with the
        // default spread: the 4×4 tile uses only those two, about half each.
        // (Grays only: in OKLab, mid-gray is nearer cyan than either gray.)
        let lut = NearestLut::new(Palette::new([Black, DarkGray, LightGray, White]).unwrap());
        let gray = [srgb_to_linear(127.5 / 255.0); 3];
        let out = dither(
            4,
            4,
            &uniform(4, 4, gray),
            &lut,
            &DitherParams::with_method(DitherMethod::Bayer4),
        );
        let dark = out.pixels.iter().filter(|&&c| c == DarkGray).count();
        let light = out.pixels.iter().filter(|&&c| c == LightGray).count();
        assert_eq!(dark + light, 16, "{:?}", out.pixels);
        assert!((6..=10).contains(&dark), "{dark} dark of 16");
    }

    #[test]
    fn floyd_steinberg_preserves_linear_brightness() {
        // Linear 0.5 (sRGB ~188) between black and white should come out
        // about half white: diffusion happens in linear light, not sRGB.
        let lut = NearestLut::new(Palette::new([Black, White]).unwrap());
        for v in [0.2f32, 0.5, 0.8] {
            for serpentine in [true, false] {
                let out = dither(
                    64,
                    64,
                    &uniform(64, 64, [v; 3]),
                    &lut,
                    &DitherParams {
                        serpentine,
                        ..DitherParams::with_method(DitherMethod::FloydSteinberg)
                    },
                );
                let f = white_fraction(&out);
                assert!((f - v).abs() < 0.02, "linear {v}: white fraction {f}");
            }
        }
    }

    #[test]
    fn atkinson_drops_a_quarter_of_the_error() {
        // FS keeps a light gray at ~80% white; Atkinson throws away the
        // small dark errors and clips it to clean white, but still mixes a
        // dark gray.
        let lut = NearestLut::new(Palette::new([Black, White]).unwrap());
        let params = DitherParams::with_method(DitherMethod::Atkinson);
        let light = white_fraction(&dither(64, 64, &uniform(64, 64, [0.8; 3]), &lut, &params));
        let dark = white_fraction(&dither(64, 64, &uniform(64, 64, [0.2; 3]), &lut, &params));
        assert_eq!(light, 1.0);
        assert!(dark > 0.1 && dark < 0.3, "dark {dark}");
    }

    #[test]
    fn strength_scales_the_diffused_error() {
        let lut = NearestLut::new(Palette::new([Black, White]).unwrap());
        let img = uniform(32, 32, [0.1; 3]);
        let frac = |s: f32| {
            let params = DitherParams {
                strength: s,
                ..DitherParams::with_method(DitherMethod::FloydSteinberg)
            };
            white_fraction(&dither(32, 32, &img, &lut, &params))
        };
        // Linear 0.1 is nearer black in OKLab: none at 0%, ~10% at 100%.
        assert_eq!(frac(0.0), 0.0);
        // Partial strength still mixes. It is not monotonic: the dropped
        // error is mostly the large negative error of white picks.
        for s in [0.25, 0.5, 0.75] {
            assert!(frac(s) > 0.0 && frac(s) < 0.25, "{s}: {}", frac(s));
        }
        assert!((frac(1.0) - 0.1).abs() < 0.02, "{}", frac(1.0));
        assert_eq!(frac(2.0), frac(1.0), "strength clamps to 100%");
        assert_eq!(frac(f32::NAN), 0.0);
    }

    #[test]
    fn serpentine_changes_the_scan() {
        let lut = NearestLut::new(Palette::full());
        let img = random_image(24, 24, 5);
        for method in [DitherMethod::FloydSteinberg, DitherMethod::Atkinson] {
            let on = dither(24, 24, &img, &lut, &DitherParams::with_method(method));
            let off = dither(
                24,
                24,
                &img,
                &lut,
                &DitherParams {
                    serpentine: false,
                    ..DitherParams::with_method(method)
                },
            );
            assert_eq!(on.pixels[..24], off.pixels[..24], "row 0 is left to right");
            assert_ne!(on, off, "{method:?}");
        }
        assert!(DitherParams::default().serpentine);
    }

    #[test]
    fn serpentine_odd_rows_run_right_to_left() {
        // Row 0 is black (zero error), so row 1 sees no incoming error. With
        // serpentine, row 1 of a mirrored ramp scanned right to left must be
        // the mirror of a one-row left-to-right scan of the ramp.
        let lut = NearestLut::new(Palette::new([Black, White]).unwrap());
        let ramp: Vec<[f32; 3]> = (0..16).map(|x| [x as f32 / 15.0 * 0.6 + 0.2; 3]).collect();
        let mut img = vec![[0.0; 3]; 16];
        img.extend(ramp.iter().rev());
        for method in [DitherMethod::FloydSteinberg, DitherMethod::Atkinson] {
            let params = DitherParams::with_method(method);
            let mut row1 = dither(16, 2, &img, &lut, &params).pixels[16..].to_vec();
            row1.reverse();
            assert_eq!(
                row1,
                dither(16, 1, &ramp, &lut, &params).pixels,
                "{method:?}"
            );
        }
    }

    #[test]
    fn kernels_match_the_doc() {
        assert!((FLOYD_STEINBERG.total_weight() - 1.0).abs() < 1e-6);
        assert!((ATKINSON.total_weight() - 0.75).abs() < 1e-6);
        for k in [&FLOYD_STEINBERG, &ATKINSON] {
            for &(dx, dy, _) in k.taps {
                assert!(dy > 0 || dx > 0, "tap ({dx},{dy}) points backwards");
            }
        }
    }

    #[test]
    fn degenerate_sizes() {
        let lut = NearestLut::new(Palette::full());
        for method in DitherMethod::ALL {
            let params = DitherParams::with_method(method);
            let empty = dither(0, 0, &[], &lut, &params);
            assert!(empty.pixels.is_empty());
            assert_eq!(dither(0, 5, &[], &lut, &params).pixels.len(), 0);
            let one = dither(1, 1, &[[1.0, 1.0, 1.0]], &lut, &params);
            assert_eq!(one.pixels, [White]);
            let col = dither(1, 9, &uniform(1, 9, [0.3; 3]), &lut, &params);
            assert_eq!(col.pixels.len(), 9);
        }
    }

    #[test]
    #[should_panic(expected = "does not match 4x4")]
    fn wrong_pixel_count_panics() {
        let lut = NearestLut::new(Palette::full());
        dither(4, 4, &[[0.0; 3]; 15], &lut, &DitherParams::default());
    }

    #[test]
    fn get_and_rgb8() {
        let img = IndexedImage {
            width: 2,
            height: 1,
            pixels: vec![Black, Yellow],
            palette: Palette::full(),
        };
        assert_eq!(img.get(1, 0), Some(Yellow));
        assert_eq!(img.get(2, 0), None);
        assert_eq!(img.get(0, 1), None);
        assert_eq!(img.to_rgb8(), [0, 0, 0, 0xff, 0xff, 0x55]);
    }

    #[test]
    fn deterministic() {
        let img = random_image(40, 30, 11);
        let lut = NearestLut::new(Palette::full());
        for params in all_params() {
            assert_eq!(
                dither(40, 30, &img, &lut, &params),
                dither(40, 30, &img, &lut, &params)
            );
        }
    }
}
