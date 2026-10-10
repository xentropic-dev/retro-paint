//! The 64×64×64 OKLab nearest-color lookup table.
//!
//! See docs/filter-and-palette.md, "Color matching" and "Decisions". The
//! table is indexed by sRGB: each 8-bit channel is reduced to 6 bits
//! (`v >> 2`), so bin `i` covers channel values `4i..=4i+3`. Each entry holds
//! the palette color nearest in OKLab to the bin's **center**, sRGB
//! `(4i + 1.5) / 255` per channel. Ties go to the color earlier in palette
//! order.
//!
//! Lookups return a [`CgaColor`]. Use [`CgaColor::linear`] for the error term
//! in diffusion and [`Palette::position`] if a palette-relative index is
//! needed.
//!
//! Accuracy: for a bin-center color the table equals [`nearest_oklab`]
//! exactly. For any other color `x` in a bin with center `c`, the triangle
//! inequality bounds the error: `d(x, lut) <= d(x, nearest) + 2·d(x, c)` in
//! OKLab distance. In practice a small fraction of colors right on a decision
//! boundary pick the neighbor; see the tests.

use crate::color::{linear_to_oklab, linear_to_srgb8, oklab_dist2, srgb_to_linear};
use crate::palette::{CgaColor, Palette, Rgb};

/// Bins per channel.
pub const LUT_SIZE: usize = 64;
const SHIFT: u32 = 2;

/// Nearest palette color for any sRGB color, in one table read.
///
/// Build one per palette (about 1 MB of work, a few milliseconds in release)
/// when the palette setting changes, and share it with `Arc` if needed.
#[derive(Clone)]
pub struct NearestLut {
    palette: Palette,
    /// Doc-order `CgaColor` indices, `[r][g][b]` flattened.
    table: Box<[u8]>,
}

impl NearestLut {
    /// Builds the table for `palette`.
    pub fn new(palette: Palette) -> Self {
        let targets: Vec<(CgaColor, [f32; 3])> =
            palette.colors().iter().map(|&c| (c, c.oklab())).collect();
        let lin: [f32; LUT_SIZE] = std::array::from_fn(|i| srgb_to_linear(bin_center(i)));
        let mut table = vec![0u8; LUT_SIZE * LUT_SIZE * LUT_SIZE].into_boxed_slice();
        let mut k = 0;
        for &r in &lin {
            for &g in &lin {
                for &b in &lin {
                    table[k] = nearest_in(&targets, linear_to_oklab([r, g, b])).index();
                    k += 1;
                }
            }
        }
        Self { palette, table }
    }

    /// The palette this table was built for.
    pub fn palette(&self) -> Palette {
        self.palette
    }

    /// Nearest palette color to an 8-bit sRGB color.
    #[inline]
    pub fn nearest(&self, rgb: Rgb) -> CgaColor {
        let i = ((rgb.r as usize >> SHIFT) * LUT_SIZE + (rgb.g as usize >> SHIFT)) * LUT_SIZE
            + (rgb.b as usize >> SHIFT);
        CgaColor::ALL[self.table[i] as usize]
    }

    /// Nearest palette color to a linear-light color. Channels are clamped
    /// to `0.0..=1.0`, re-encoded to 8-bit sRGB, then looked up. This is the
    /// entry point for error diffusion and ordered dither.
    #[inline]
    pub fn nearest_linear(&self, lin: [f32; 3]) -> CgaColor {
        self.nearest(lin.map(linear_to_srgb8).into())
    }
}

impl std::fmt::Debug for NearestLut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NearestLut")
            .field("palette", &self.palette)
            .finish_non_exhaustive()
    }
}

/// sRGB (`0.0..=1.0`) center of bin `i`.
fn bin_center(i: usize) -> f32 {
    ((i << SHIFT) as f32 + 1.5) / 255.0
}

fn nearest_in(targets: &[(CgaColor, [f32; 3])], lab: [f32; 3]) -> CgaColor {
    let mut best = targets[0].0;
    let mut best_d = f32::INFINITY;
    for &(c, t) in targets {
        let d = oklab_dist2(lab, t);
        if d < best_d {
            best_d = d;
            best = c;
        }
    }
    best
}

/// Brute-force nearest palette color in OKLab to a linear-light color, with
/// the same tie rule as [`NearestLut`]. Slow (16 distances per call); the
/// reference the table is checked against.
pub fn nearest_oklab(palette: &Palette, lin: [f32; 3]) -> CgaColor {
    let lab = linear_to_oklab(lin);
    let mut best = palette.colors()[0];
    let mut best_d = f32::INFINITY;
    for &c in palette.colors() {
        let d = oklab_dist2(lab, c.oklab());
        if d < best_d {
            best_d = d;
            best = c;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::{FourColor, Intensity, Preset};
    use crate::test_util::assert_palette_only;
    use CgaColor::*;

    /// xorshift64*, deterministic.
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

    fn palettes() -> Vec<Palette> {
        let mut out = vec![
            Palette::full(),
            Palette::new([Black, White]).unwrap(),
            Palette::new([Yellow]).unwrap(),
            Palette::new([Brown, LightBlue, DarkGray, Magenta, Cyan]).unwrap(),
        ];
        for preset in [Preset::Palette0, Preset::Palette1, Preset::Mode5] {
            for intensity in [Intensity::Low, Intensity::High] {
                for bg in [Black, Blue, White] {
                    out.push(
                        FourColor::new(preset, intensity)
                            .with_background(bg)
                            .palette(),
                    );
                }
            }
        }
        out
    }

    fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
        oklab_dist2(a, b).sqrt()
    }

    #[test]
    fn every_palette_color_maps_to_itself() {
        for p in palettes() {
            let lut = NearestLut::new(p);
            for &c in p.colors() {
                assert_eq!(lut.nearest(c.rgb()), c, "{p:?}");
                assert_eq!(lut.nearest_linear(c.linear()), c, "{p:?}");
            }
        }
    }

    #[test]
    fn output_is_always_in_the_palette() {
        for p in palettes() {
            let lut = NearestLut::new(p);
            let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
            assert_palette_only((0..2000).map(|_| lut.nearest(rng.rgb())), &p);
        }
    }

    #[test]
    fn bin_centers_agree_with_brute_force_exactly() {
        for p in palettes() {
            let lut = NearestLut::new(p);
            for r in 0..LUT_SIZE {
                for g in 0..LUT_SIZE {
                    for b in 0..LUT_SIZE {
                        let center = [r, g, b].map(|i| srgb_to_linear(bin_center(i)));
                        let rgb = Rgb::new((r << 2) as u8, (g << 2) as u8, (b << 2) as u8);
                        assert_eq!(lut.nearest(rgb), nearest_oklab(&p, center), "{p:?} {rgb}");
                    }
                }
            }
        }
    }

    /// For random 8-bit colors the table may disagree with brute force only
    /// near a decision boundary. Check (1) the triangle-inequality bound holds
    /// for every sample, (2) the chosen color is never more than 0.02 OKLab
    /// distance worse than the true nearest (about one just-noticeable
    /// difference; measured worst is ~0.0096, in the darks where bins are
    /// widest in OKLab), and (3) fewer than 3% of samples disagree at all
    /// (measured ~1.7% for the full 16, under 1.1% for 1–5 color subsets).
    #[test]
    fn random_colors_agree_with_brute_force_within_bin_error() {
        const N: usize = 20_000;
        for p in palettes() {
            let lut = NearestLut::new(p);
            let mut rng = Rng(0x0123_4567_89ab_cdef);
            let mut mismatches = 0;
            let mut worst = 0.0f32;
            for _ in 0..N {
                let rgb = rng.rgb();
                let x = rgb.to_oklab();
                let got = lut.nearest(rgb);
                let want = nearest_oklab(&p, rgb.to_linear());
                if got == want {
                    continue;
                }
                mismatches += 1;
                let center = rgb
                    .to_array()
                    .map(|v| srgb_to_linear(bin_center(v as usize >> SHIFT)));
                let excess = dist(x, got.oklab()) - dist(x, want.oklab());
                let bound = 2.0 * dist(x, linear_to_oklab(center));
                assert!(excess <= bound + 1e-5, "{p:?} {rgb}: {excess} > {bound}");
                worst = worst.max(excess);
            }
            assert!(worst < 0.02, "{p:?}: worst excess {worst}");
            assert!(
                mismatches * 100 < 3 * N,
                "{p:?}: {mismatches}/{N} random colors disagree"
            );
        }
    }

    #[test]
    fn nearest_linear_clamps_out_of_range() {
        let lut = NearestLut::new(Palette::full());
        assert_eq!(lut.nearest_linear([-0.5, -1.0, -0.2]), Black);
        assert_eq!(lut.nearest_linear([1.5, 2.0, 9.0]), White);
    }

    #[test]
    fn two_color_split_is_perceptual() {
        // OKLab L is perceptual lightness, so sRGB mid-gray (linear 0.21)
        // lands on white's side only above the OKLab midpoint.
        let lut = NearestLut::new(Palette::new([Black, White]).unwrap());
        assert_eq!(lut.nearest(Rgb::new(0x40, 0x40, 0x40)), Black);
        assert_eq!(lut.nearest(Rgb::new(0xc0, 0xc0, 0xc0)), White);
    }

    /// Building must be cheap enough to redo on a settings change.
    #[test]
    fn build_is_fast() {
        let t = std::time::Instant::now();
        let lut = NearestLut::new(Palette::full());
        let elapsed = t.elapsed();
        eprintln!("NearestLut::new(full 16): {elapsed:?}");
        assert_eq!(lut.nearest(Rgb::new(0, 0, 0)), Black);
        // Generous: debug builds are ~10× slower than release.
        assert!(elapsed.as_millis() < 2000, "{elapsed:?}");
    }
}
