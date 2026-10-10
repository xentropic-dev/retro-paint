//! Color space conversions: sRGB ↔ linear light ↔ OKLab.
//!
//! See docs/filter-and-palette.md, "Color matching". Colors are plain
//! `[f32; 3]` triples so the dither can do arithmetic on them directly:
//!
//! - **sRGB** (`[r, g, b]`, nominally `0.0..=1.0`): gamma-encoded, what an
//!   [`Rgb`] holds divided by 255.
//! - **linear** (`[r, g, b]`, nominally `0.0..=1.0`): linear light. Error
//!   diffusion works here.
//! - **OKLab** (`[L, a, b]`): perceptual space used for nearest-color
//!   distance. `L` is about `0..=1`, `a` and `b` about `-0.4..=0.4`.
//!
//! The OKLab matrices are Björn Ottosson's reference values.

use std::sync::LazyLock;

use crate::palette::{CgaColor, Rgb};

/// Decodes one sRGB channel (`0.0..=1.0`) to linear light.
///
/// Values outside the range are extended along the same curve (mirrored for
/// negatives), so dither error that overshoots stays monotonic.
pub fn srgb_to_linear(c: f32) -> f32 {
    let a = c.abs();
    let v = if a <= 0.04045 {
        a / 12.92
    } else {
        ((a + 0.055) / 1.055).powf(2.4)
    };
    v.copysign(c)
}

/// Encodes one linear-light channel to sRGB (`0.0..=1.0` for in-range input).
/// Inverse of [`srgb_to_linear`].
pub fn linear_to_srgb(l: f32) -> f32 {
    let a = l.abs();
    let v = if a <= 0.003_130_8 {
        a * 12.92
    } else {
        1.055 * a.powf(1.0 / 2.4) - 0.055
    };
    v.copysign(l)
}

static SRGB8_TO_LINEAR: LazyLock<[f32; 256]> =
    LazyLock::new(|| std::array::from_fn(|i| srgb_to_linear(i as f32 / 255.0)));

/// Decodes an 8-bit sRGB channel to linear light (table lookup).
pub fn srgb8_to_linear(v: u8) -> f32 {
    SRGB8_TO_LINEAR[v as usize]
}

/// Encodes a linear-light channel to 8-bit sRGB, clamping and rounding.
/// `linear_to_srgb8(srgb8_to_linear(v)) == v` for every `v`.
pub fn linear_to_srgb8(l: f32) -> u8 {
    (linear_to_srgb(l.clamp(0.0, 1.0)) * 255.0 + 0.5) as u8
}

impl Rgb {
    /// This color in linear light.
    pub fn to_linear(self) -> [f32; 3] {
        self.to_array().map(srgb8_to_linear)
    }

    /// Encodes a linear-light color to 8-bit sRGB, clamping each channel.
    pub fn from_linear(lin: [f32; 3]) -> Rgb {
        lin.map(linear_to_srgb8).into()
    }

    /// This color in OKLab.
    pub fn to_oklab(self) -> [f32; 3] {
        linear_to_oklab(self.to_linear())
    }
}

impl CgaColor {
    /// This color in linear light (what error diffusion subtracts).
    pub fn linear(self) -> [f32; 3] {
        self.rgb().to_linear()
    }

    /// This color in OKLab.
    pub fn oklab(self) -> [f32; 3] {
        self.rgb().to_oklab()
    }
}

/// Linear sRGB to OKLab.
pub fn linear_to_oklab([r, g, b]: [f32; 3]) -> [f32; 3] {
    let l = 0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// OKLab to linear sRGB. Inverse of [`linear_to_oklab`]; the result may fall
/// outside `0.0..=1.0` for colors outside the sRGB gamut.
pub fn oklab_to_linear([ll, a, b]: [f32; 3]) -> [f32; 3] {
    let l = ll + 0.396_337_78 * a + 0.215_803_76 * b;
    let m = ll - 0.105_561_346 * a - 0.063_854_17 * b;
    let s = ll - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_4 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

/// Squared Euclidean distance between two OKLab colors.
pub fn oklab_dist2(p: [f32; 3], q: [f32; 3]) -> f32 {
    let d = [p[0] - q[0], p[1] - q[1], p[2] - q[2]];
    d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
}

/// Weight on the chroma terms (`a`, `b`) of [`match_dist2`], relative to `L`.
///
/// Plain OKLab puts sRGB mid-gray (~0.59 L) closer to Cyan (0.67 L, chroma
/// 0.11) than to DarkGray or LightGray (0.45 / 0.74 L), because the CGA grays
/// are far apart in lightness. Doubling the chroma terms is the smallest
/// round weight that keeps a tube of radius ≥ 0.02 (about one JND) around
/// the whole gray axis mapping to grays (1.75 gives 0.019; 2 gives 0.024).
/// Lightness error is what dithering reproduces well; a hue cast on a
/// neutral is not. See docs/filter-and-palette.md, "Color matching".
pub const CHROMA_WEIGHT: f32 = 2.0;

/// Squared nearest-color distance: OKLab with the `a`/`b` differences
/// weighted by [`CHROMA_WEIGHT`], `ΔL² + w·(Δa² + Δb²)`. Still a Euclidean
/// metric (OKLab with `a`, `b` scaled by `√w`), so the triangle inequality
/// holds. Both [`crate::NearestLut`] and [`crate::nearest_oklab`] use it.
pub fn match_dist2(p: [f32; 3], q: [f32; 3]) -> f32 {
    let d = [p[0] - q[0], p[1] - q[1], p[2] - q[2]];
    d[0] * d[0] + CHROMA_WEIGHT * (d[1] * d[1] + d[2] * d[2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::CGA_RGB;

    fn close(a: [f32; 3], b: [f32; 3], eps: f32) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() <= eps)
    }

    #[test]
    fn srgb8_round_trips_through_linear() {
        for v in 0..=255u8 {
            assert_eq!(linear_to_srgb8(srgb8_to_linear(v)), v);
        }
    }

    #[test]
    fn srgb_curve_known_values() {
        assert_eq!(srgb_to_linear(0.0), 0.0);
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-6);
        // sRGB 0.5 ≈ 0.2140 linear; 170/255 ≈ 0.4020 linear.
        assert!((srgb_to_linear(0.5) - 0.214_041).abs() < 1e-4);
        assert!((srgb8_to_linear(170) - 0.402_02).abs() < 1e-4);
        assert!((linear_to_srgb(0.214_041) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn srgb_curve_extends_outside_range() {
        assert!(srgb_to_linear(-0.5) < 0.0);
        assert!((srgb_to_linear(-0.5) + srgb_to_linear(0.5)).abs() < 1e-7);
        assert!(srgb_to_linear(1.2) > 1.0);
        assert!((linear_to_srgb(srgb_to_linear(1.2)) - 1.2).abs() < 1e-5);
        assert_eq!(linear_to_srgb8(-1.0), 0);
        assert_eq!(linear_to_srgb8(2.0), 255);
    }

    #[test]
    fn oklab_reference_values() {
        // White is L=1, a=b=0; black is all zero.
        assert!(close(linear_to_oklab([1.0; 3]), [1.0, 0.0, 0.0], 1e-4));
        assert!(close(linear_to_oklab([0.0; 3]), [0.0; 3], 1e-7));
        // Ottosson's published sRGB primaries.
        assert!(close(
            linear_to_oklab([1.0, 0.0, 0.0]),
            [0.627_955, 0.224_863, 0.125_846],
            1e-3
        ));
        assert!(close(
            linear_to_oklab([0.0, 1.0, 0.0]),
            [0.866_440, -0.233_888, 0.179_498],
            1e-3
        ));
        assert!(close(
            linear_to_oklab([0.0, 0.0, 1.0]),
            [0.452_014, -0.032_457, -0.311_528],
            1e-3
        ));
    }

    #[test]
    fn oklab_round_trips() {
        for c in CGA_RGB {
            let lin = c.to_linear();
            assert!(
                close(oklab_to_linear(linear_to_oklab(lin)), lin, 1e-4),
                "{c}"
            );
            assert_eq!(Rgb::from_linear(lin), c);
        }
        for v in (0..=255u8).step_by(5) {
            let lin = [srgb8_to_linear(v), srgb8_to_linear(255 - v), 0.3];
            assert!(close(oklab_to_linear(linear_to_oklab(lin)), lin, 1e-4));
        }
    }

    #[test]
    fn grays_have_zero_chroma() {
        for v in [0u8, 85, 170, 255] {
            let [_, a, b] = Rgb::new(v, v, v).to_oklab();
            assert!(a.abs() < 1e-4 && b.abs() < 1e-4, "{v}: {a} {b}");
        }
    }
}
