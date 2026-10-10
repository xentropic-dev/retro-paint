//! The render pipeline: flattened canvas RGBA → indexed output image.
//!
//! See docs/filter-and-palette.md, "Pipeline placement" and "User controls
//! (v1)". Steps, all after the caller has flattened the layers:
//!
//! 1. **Alpha**: each straight-alpha RGBA8 pixel is composited over
//!    **black** in linear light (`linear(rgb) * alpha`). Transparent canvas
//!    shows as black, the CGA border color.
//! 2. **Box downscale** ([`box_downscale`]): area average in linear light
//!    from canvas to output size. Non-integer ratios weight the source
//!    pixels on each output pixel's edge by their fractional coverage.
//!    Equal sizes are the identity.
//! 3. **Pre-adjust** ([`PreAdjust`]): brightness, contrast, saturation, in
//!    gamma-encoded sRGB. Neutral settings are skipped entirely.
//! 4. **Dither** ([`dither`]) with the method and palette from
//!    [`DitherSettings`].
//!
//! [`render`] is the one entry point for live view and export. The render
//! worker holds a [`Renderer`], which caches the [`NearestLut`] so it is only
//! rebuilt when the palette changes.

use crate::color::{linear_to_srgb, srgb_to_linear, srgb8_to_linear};
use crate::dither::{DitherMethod, DitherParams, IndexedImage, dither};
use crate::lut::NearestLut;
use crate::palette::{FourColor, Palette};

/// Which colors the dither may output, as the user chose them. Kept as the
/// choice (not just the resulting [`Palette`]) so the UI and project file
/// can show and restore it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum PaletteChoice {
    /// All 16 CGA colors.
    #[default]
    Full,
    /// An authentic 4-color mode with its background color.
    FourColor(FourColor),
    /// A user-picked subset.
    Custom(Palette),
}

impl PaletteChoice {
    /// The palette the dither should use.
    pub fn palette(&self) -> Palette {
        match *self {
            PaletteChoice::Full => Palette::full(),
            PaletteChoice::FourColor(m) => m.palette(),
            PaletteChoice::Custom(p) => p,
        }
    }
}

/// Pre-adjust controls. Each is in `-1.0..=1.0` (the UI's −100..100) with
/// `0.0` neutral; values outside are clamped. Applied in gamma-encoded sRGB
/// (`0.0..=1.0`) in this order, then clamped to the gamut:
///
/// - **brightness** `b`: `c + b`.
/// - **contrast** `k`: `(c − 0.5) · (1 + k) + 0.5`. `−1` flattens to mid
///   gray, `+1` doubles the slope.
/// - **saturation** `s`: `Y + (c − Y) · (1 + s)`, with `Y` the Rec. 709 luma
///   of the sRGB values. `−1` is grayscale, `+1` doubles chroma.
///
/// sRGB rather than linear light: a slider step then looks even across the
/// tone range (as in other paint programs), and the CGA levels are evenly
/// spaced in sRGB, so brightness and contrast move colors toward and across
/// palette levels in equal steps.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct PreAdjust {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
}

/// Rec. 709 luma weights.
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

impl PreAdjust {
    /// True when every control is `0.0`: [`apply`](Self::apply) is the
    /// identity and the pipeline skips this step.
    pub fn is_neutral(&self) -> bool {
        self.brightness == 0.0 && self.contrast == 0.0 && self.saturation == 0.0
    }

    /// Adjusts one gamma-encoded sRGB color (`0.0..=1.0`). Output is clamped
    /// to `0.0..=1.0`.
    pub fn apply(&self, srgb: [f32; 3]) -> [f32; 3] {
        let b = self.brightness.clamp(-1.0, 1.0);
        let k = 1.0 + self.contrast.clamp(-1.0, 1.0);
        let s = 1.0 + self.saturation.clamp(-1.0, 1.0);
        let c = srgb.map(|v| (v + b - 0.5) * k + 0.5);
        let y = c[0] * LUMA[0] + c[1] * LUMA[1] + c[2] * LUMA[2];
        c.map(|v| (y + (v - y) * s).clamp(0.0, 1.0))
    }

    /// Adjusts a linear-light image in place (decode → [`apply`] → encode).
    /// Does nothing when neutral.
    ///
    /// [`apply`]: Self::apply
    pub fn apply_linear(&self, pixels: &mut [[f32; 3]]) {
        if self.is_neutral() {
            return;
        }
        for p in pixels {
            let srgb = p.map(|v| linear_to_srgb(v.clamp(0.0, 1.0)));
            *p = self.apply(srgb).map(srgb_to_linear);
        }
    }
}

/// Every user-facing dither control (the v1 filter panel). Defaults:
/// Floyd–Steinberg, full palette, neutral pre-adjust, Bayer spread one CGA
/// step, strength 100%, serpentine on.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DitherSettings {
    pub method: DitherMethod,
    pub palette: PaletteChoice,
    pub adjust: PreAdjust,
    /// See [`DitherParams::bayer_spread`].
    pub bayer_spread: f32,
    /// See [`DitherParams::strength`].
    pub strength: f32,
    /// See [`DitherParams::serpentine`].
    pub serpentine: bool,
}

impl Default for DitherSettings {
    fn default() -> Self {
        let p = DitherParams::default();
        Self {
            method: p.method,
            palette: PaletteChoice::default(),
            adjust: PreAdjust::default(),
            bayer_spread: p.bayer_spread,
            strength: p.strength,
            serpentine: p.serpentine,
        }
    }
}

impl DitherSettings {
    /// Default settings with the given method.
    pub fn with_method(method: DitherMethod) -> Self {
        Self {
            method,
            ..Self::default()
        }
    }

    /// The [`dither`] parameters these settings select.
    pub fn params(&self) -> DitherParams {
        DitherParams {
            method: self.method,
            bayer_spread: self.bayer_spread,
            strength: self.strength,
            serpentine: self.serpentine,
        }
    }
}

/// Renders a canvas to the indexed output image: alpha over black → box
/// downscale → pre-adjust → dither. See the module docs.
///
/// `rgba` is straight-alpha RGBA8, row-major, `canvas_size` = `(width,
/// height)`. Builds a [`NearestLut`] (about 10 ms); callers rendering
/// repeatedly should hold a [`Renderer`] instead. Both give identical output.
///
/// Panics if `rgba.len() != canvas width * height * 4`.
pub fn render(
    rgba: &[u8],
    canvas_size: (u32, u32),
    output_size: (u32, u32),
    settings: &DitherSettings,
) -> IndexedImage {
    let lut = NearestLut::new(settings.palette.palette());
    render_with_lut(rgba, canvas_size, output_size, settings, &lut)
}

/// [`render`] with a cached [`NearestLut`], rebuilt only when the palette
/// changes. Keep one per render thread.
#[derive(Clone, Debug, Default)]
pub struct Renderer {
    lut: Option<NearestLut>,
}

impl Renderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The table for `palette`, building it if the cached one differs.
    pub fn lut(&mut self, palette: Palette) -> &NearestLut {
        match &mut self.lut {
            Some(lut) if lut.palette() == palette => {}
            slot => *slot = Some(NearestLut::new(palette)),
        }
        self.lut.as_ref().expect("just filled")
    }

    /// Same as [`render`], reusing the cached table.
    pub fn render(
        &mut self,
        rgba: &[u8],
        canvas_size: (u32, u32),
        output_size: (u32, u32),
        settings: &DitherSettings,
    ) -> IndexedImage {
        let lut = self.lut(settings.palette.palette());
        render_with_lut(rgba, canvas_size, output_size, settings, lut)
    }
}

fn render_with_lut(
    rgba: &[u8],
    canvas_size: (u32, u32),
    output_size: (u32, u32),
    settings: &DitherSettings,
    lut: &NearestLut,
) -> IndexedImage {
    debug_assert_eq!(lut.palette(), settings.palette.palette());
    let mut linear = box_downscale(rgba, canvas_size, output_size);
    settings.adjust.apply_linear(&mut linear);
    dither(
        output_size.0,
        output_size.1,
        &linear,
        lut,
        &settings.params(),
    )
}

/// Downscales straight-alpha RGBA8 to linear-light RGB by area averaging,
/// after compositing each pixel over black (`linear(rgb) · alpha`).
///
/// Each output pixel covers a `canvas / output` rectangle of the canvas;
/// source pixels are weighted by how much of them lies inside it, so
/// non-integer ratios work. Equal sizes give each pixel's linear value
/// unchanged; upscaling repeats pixels (nearest, with blended seams on
/// non-integer ratios). An empty canvas or output gives black or an empty
/// image.
///
/// Panics if `rgba.len() != canvas width * height * 4`.
pub fn box_downscale(
    rgba: &[u8],
    canvas_size: (u32, u32),
    output_size: (u32, u32),
) -> Vec<[f32; 3]> {
    let (sw, sh) = (canvas_size.0 as usize, canvas_size.1 as usize);
    let (dw, dh) = (output_size.0 as usize, output_size.1 as usize);
    assert_eq!(
        rgba.len(),
        sw * sh * 4,
        "RGBA length does not match {sw}x{sh}"
    );
    if sw == 0 || sh == 0 {
        return vec![[0.0; 3]; dw * dh];
    }
    let xs = AxisWeights::new(sw, dw);
    let ys = AxisWeights::new(sh, dh);

    // Horizontal pass: each canvas row → `dw` linear pixels.
    let mut rows = vec![[0.0f32; 3]; dw * sh];
    let mut src = vec![[0.0f32; 3]; sw];
    for (y, out_row) in rows.chunks_exact_mut(dw.max(1)).enumerate().take(sh) {
        let line = &rgba[y * sw * 4..(y + 1) * sw * 4];
        for (s, px) in src.iter_mut().zip(line.as_chunks::<4>().0) {
            *s = premul_linear(*px);
        }
        for (x, out) in out_row.iter_mut().enumerate() {
            let (start, w) = xs.taps(x);
            let mut acc = [0.0f32; 3];
            for (p, &wt) in src[start..start + w.len()].iter().zip(w) {
                for c in 0..3 {
                    acc[c] += p[c] * wt;
                }
            }
            *out = acc;
        }
    }

    // Vertical pass: weighted sum of horizontal-pass rows.
    let mut out = vec![[0.0f32; 3]; dw * dh];
    for (y, out_row) in out.chunks_exact_mut(dw.max(1)).enumerate().take(dh) {
        let (start, w) = ys.taps(y);
        for (k, &wt) in w.iter().enumerate() {
            let row = &rows[(start + k) * dw..(start + k + 1) * dw];
            for (o, p) in out_row.iter_mut().zip(row) {
                for c in 0..3 {
                    o[c] += p[c] * wt;
                }
            }
        }
    }
    out
}

/// Straight-alpha sRGB8 over black, in linear light.
#[inline]
fn premul_linear([r, g, b, a]: [u8; 4]) -> [f32; 3] {
    if a == 255 {
        [r, g, b].map(srgb8_to_linear)
    } else {
        let a = a as f32 / 255.0;
        [r, g, b].map(|v| srgb8_to_linear(v) * a)
    }
}

/// Box-filter taps along one axis: for each output index, the first source
/// index and the normalized coverage weights of the source pixels from there.
struct AxisWeights {
    /// `(start, offset into weights, count)` per output index.
    spans: Vec<(usize, usize, usize)>,
    weights: Vec<f32>,
}

impl AxisWeights {
    /// Output pixel `i` covers source `[i·src/dst, (i+1)·src/dst)`. Working in
    /// units of `1/dst` keeps the bounds integral, so coverage is exact.
    fn new(src: usize, dst: usize) -> Self {
        let mut spans = Vec::with_capacity(dst);
        let mut weights = Vec::new();
        for i in 0..dst {
            let (lo, hi) = (i * src, (i + 1) * src);
            let (first, last) = (lo / dst, hi.div_ceil(dst));
            let offset = weights.len();
            for j in first..last {
                let cover = hi.min((j + 1) * dst) - lo.max(j * dst);
                weights.push(cover as f32 / src as f32);
            }
            spans.push((first, offset, last - first));
        }
        Self { spans, weights }
    }

    #[inline]
    fn taps(&self, i: usize) -> (usize, &[f32]) {
        let (start, off, n) = self.spans[i];
        (start, &self.weights[off..off + n])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::srgb8_to_linear;
    use crate::palette::{CgaColor, Intensity, Preset};
    use crate::test_util::assert_palette_only;

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
        px.repeat((w * h) as usize)
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn axis_weights_sum_to_one_and_cover_fractionally() {
        for (src, dst) in [(800, 200), (1280, 320), (10, 3), (7, 7), (3, 10), (5, 2)] {
            let a = AxisWeights::new(src, dst);
            for i in 0..dst {
                let (start, w) = a.taps(i);
                let sum: f32 = w.iter().sum();
                assert!((sum - 1.0).abs() < 1e-6, "{src}->{dst} [{i}] sums to {sum}");
                assert!(start + w.len() <= src);
            }
        }
        // 5 → 2: each output covers 2.5 source pixels.
        let a = AxisWeights::new(5, 2);
        assert_eq!(a.taps(0), (0, &[0.4, 0.4, 0.2][..]));
        assert_eq!(a.taps(1), (2, &[0.2, 0.4, 0.4][..]));
        // Identity: one tap of weight exactly 1.
        let a = AxisWeights::new(4, 4);
        for i in 0..4 {
            assert_eq!(a.taps(i), (i, &[1.0][..]));
        }
    }

    #[test]
    fn uniform_color_stays_uniform() {
        let px = [200, 90, 30, 255];
        let want = [200u8, 90, 30].map(srgb8_to_linear);
        for out in [(200, 320), (7, 11), (800, 1280), (1, 1), (3, 5)] {
            let img = box_downscale(&solid(80, 128, px), (80, 128), out);
            assert_eq!(img.len(), (out.0 * out.1) as usize);
            assert!(img.iter().all(|&p| close(p, want)), "{out:?}");
        }
    }

    #[test]
    fn identity_size_is_exact_linear() {
        let rgba: Vec<u8> = (0..4 * 3 * 4).map(|i| (i * 37 % 256) as u8).collect();
        let img = box_downscale(&rgba, (4, 3), (4, 3));
        for (p, px) in img.iter().zip(rgba.as_chunks::<4>().0) {
            assert_eq!(*p, premul_linear(*px));
        }
    }

    #[test]
    fn averages_in_linear_light() {
        // Black and white columns, 2 → 1: the linear average is 0.5, not
        // the sRGB average.
        let rgba = [0, 0, 0, 255, 255, 255, 255, 255];
        let img = box_downscale(&rgba, (2, 1), (1, 1));
        assert!(close(img[0], [0.5; 3]));
        // 2×2 → 1×1 with four different reds.
        let rgba: Vec<u8> = [10u8, 60, 120, 250]
            .iter()
            .flat_map(|&r| [r, 0, 0, 255])
            .collect();
        let img = box_downscale(&rgba, (2, 2), (1, 1));
        let want = [10u8, 60, 120, 250]
            .map(srgb8_to_linear)
            .iter()
            .sum::<f32>()
            / 4.0;
        assert!(close(img[0], [want, 0.0, 0.0]));
    }

    #[test]
    fn fractional_ratio_weights_edges() {
        // 3 → 2 columns of linear 0, 1, 0 (in sRGB 0, 255, 0): each output
        // covers 1.5 pixels, half of the white one.
        let rgba = [0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255];
        let img = box_downscale(&rgba, (3, 1), (2, 1));
        assert!(close(img[0], [1.0 / 3.0; 3]), "{:?}", img[0]);
        assert!(close(img[1], [1.0 / 3.0; 3]));
    }

    #[test]
    fn alpha_composites_over_black() {
        let clear = box_downscale(&solid(4, 4, [255, 255, 255, 0]), (4, 4), (2, 2));
        assert!(clear.iter().all(|&p| p == [0.0; 3]));
        let half = box_downscale(&solid(4, 4, [255, 128, 0, 128]), (4, 4), (1, 1));
        let a = 128.0 / 255.0;
        assert!(close(half[0], [a, srgb8_to_linear(128) * a, 0.0]));
        // Opaque white next to transparent: averages with black.
        let rgba = [255, 255, 255, 255, 255, 255, 255, 0];
        let img = box_downscale(&rgba, (2, 1), (1, 1));
        assert!(close(img[0], [0.5; 3]));
    }

    #[test]
    fn empty_sizes() {
        assert!(box_downscale(&[], (0, 0), (0, 0)).is_empty());
        assert_eq!(box_downscale(&[], (0, 5), (2, 2)), vec![[0.0; 3]; 4]);
        assert!(box_downscale(&solid(2, 2, [9; 4]), (2, 2), (0, 3)).is_empty());
    }

    #[test]
    fn neutral_adjust_is_identity() {
        let n = PreAdjust::default();
        assert!(n.is_neutral());
        let mut px: Vec<[f32; 3]> = (0..=255u8)
            .map(|v| [v, 255 - v, v / 2].map(srgb8_to_linear))
            .collect();
        let before = px.clone();
        n.apply_linear(&mut px);
        assert_eq!(px, before);
        for v in [0.0, 0.25, 0.5, 1.0] {
            let c = [v, 1.0 - v, v * 0.5];
            assert!(close(n.apply(c), c));
        }
    }

    #[test]
    fn adjust_controls() {
        let mid = [0.5, 0.25, 0.75];
        let b = PreAdjust {
            brightness: 0.2,
            ..Default::default()
        };
        assert!(close(b.apply(mid), [0.7, 0.45, 0.95]));
        let c = PreAdjust {
            contrast: 1.0,
            ..Default::default()
        };
        assert!(close(c.apply(mid), [0.5, 0.0, 1.0]));
        let flat = PreAdjust {
            contrast: -1.0,
            ..Default::default()
        };
        assert!(close(flat.apply(mid), [0.5; 3]));
        let gray = PreAdjust {
            saturation: -1.0,
            ..Default::default()
        };
        let g = gray.apply([1.0, 0.0, 0.0]);
        assert!(close(g, [LUMA[0]; 3]), "{g:?}");
        // Out-of-range settings clamp; output stays in gamut.
        let wild = PreAdjust {
            brightness: 5.0,
            contrast: 9.0,
            saturation: 9.0,
        };
        assert!(wild.apply(mid).iter().all(|v| (0.0..=1.0).contains(v)));
    }

    #[test]
    fn renderer_matches_render_and_caches_lut() {
        let rgba: Vec<u8> = (0..40 * 30)
            .flat_map(|i| {
                [
                    (i * 7 % 256) as u8,
                    (i * 3 % 256) as u8,
                    (i % 256) as u8,
                    255,
                ]
            })
            .collect();
        let mut r = Renderer::new();
        let p1 = PaletteChoice::FourColor(FourColor::new(Preset::Palette1, Intensity::High));
        for (method, palette) in [
            (DitherMethod::FloydSteinberg, PaletteChoice::Full),
            (DitherMethod::Bayer4, p1),
            (DitherMethod::Atkinson, p1),
            (DitherMethod::None, PaletteChoice::Full),
        ] {
            let s = DitherSettings {
                palette,
                ..DitherSettings::with_method(method)
            };
            let a = r.render(&rgba, (40, 30), (16, 12), &s);
            let b = render(&rgba, (40, 30), (16, 12), &s);
            assert_eq!(a, b);
            assert_eq!((a.width, a.height), (16, 12));
            assert_eq!(a.palette, palette.palette());
            assert_palette_only(a.pixels.iter().copied(), &a.palette);
        }
        let lut: *const NearestLut = r.lut(Palette::full());
        assert!(std::ptr::eq(lut, r.lut(Palette::full())));
    }

    #[test]
    fn transparent_canvas_renders_black() {
        let out = render(
            &solid(8, 8, [255, 0, 0, 0]),
            (8, 8),
            (4, 4),
            &DitherSettings::default(),
        );
        assert!(out.pixels.iter().all(|&c| c == CgaColor::Black));
    }

    #[test]
    fn palette_choice_resolves() {
        let m = FourColor::new(Preset::Mode5, Intensity::Low);
        assert_eq!(PaletteChoice::Full.palette(), Palette::full());
        assert_eq!(PaletteChoice::FourColor(m).palette(), m.palette());
    }

    /// 800×1280 canvas → 200×320 output, the mobile project default. Run
    /// `cargo test --release -p rp-palette-dither render_is_fast -- --nocapture`
    /// for the real number.
    #[test]
    fn render_is_fast() {
        let (w, h) = (800u32, 1280u32);
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                [(x / 4) as u8, (y / 6) as u8, ((x + y) / 8) as u8, 255]
            })
            .collect();
        let mut r = Renderer::new();
        let adjusted = DitherSettings {
            adjust: PreAdjust {
                brightness: 0.1,
                contrast: 0.2,
                saturation: 0.3,
            },
            ..Default::default()
        };
        r.render(&rgba, (w, h), (200, 320), &adjusted);
        for (name, s) in [
            ("floyd-steinberg", DitherSettings::default()),
            ("floyd-steinberg + adjust", adjusted),
            ("bayer4", DitherSettings::with_method(DitherMethod::Bayer4)),
        ] {
            let t = std::time::Instant::now();
            let n = 5;
            for _ in 0..n {
                let out = r.render(&rgba, (w, h), (200, 320), &s);
                assert_eq!(out.pixels.len(), 200 * 320);
            }
            let per = t.elapsed() / n;
            eprintln!("render 800x1280 -> 200x320 ({name}): {per:?}");
            // Generous: debug builds are many times slower than release.
            assert!(per.as_millis() < 3000, "{per:?}");
        }
    }
}
