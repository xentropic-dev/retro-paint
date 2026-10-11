//! Blend modes and per-pixel compositing.
//!
//! Color space: blending runs on **straight (non-linear) sRGB values** scaled
//! to `f32` in `0.0..=1.0`, like GIMP's "legacy" and Photoshop's default
//! modes. That is what users expect multiply/screen/overlay to look like, and
//! it keeps a layer at 100% opacity in normal mode bit-exact. Flatten keeps an
//! `f32` accumulator for the whole stack and rounds to 8 bits once at the end.
//!
//! The formulas are the separable modes of the W3C Compositing and Blending
//! spec, with `Cb` the backdrop and `Cs` the layer (source) color:
//!
//! | mode     | `B(Cb, Cs)`                                              |
//! |----------|----------------------------------------------------------|
//! | normal   | `Cs`                                                     |
//! | multiply | `Cb · Cs`                                                |
//! | screen   | `Cb + Cs − Cb · Cs`                                      |
//! | overlay  | `Cb ≤ 0.5 ? 2 · Cb · Cs : screen(Cs, 2 · Cb − 1)`         |
//!
//! The blended color is then composited source-over with the effective source
//! alpha `αs = pixel alpha × opacity × mask`:
//!
//! ```text
//! Cs' = (1 − αb) · Cs + αb · B(Cb, Cs)
//! αo  = αs + αb · (1 − αs)
//! Co  = (αs · Cs' + αb · (1 − αs) · Cb) / αo
//! ```
//!
//! So over a transparent backdrop every mode behaves like normal.

/// How a layer's colors combine with the layers below it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
}

impl BlendMode {
    /// Every mode, in menu order.
    pub const ALL: [BlendMode; 4] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
    ];

    /// Stable lowercase name, suitable for the project manifest.
    pub const fn name(self) -> &'static str {
        match self {
            BlendMode::Normal => "normal",
            BlendMode::Multiply => "multiply",
            BlendMode::Screen => "screen",
            BlendMode::Overlay => "overlay",
        }
    }

    /// Inverse of [`BlendMode::name`].
    pub fn from_name(name: &str) -> Option<BlendMode> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }

    /// `B(Cb, Cs)` for one channel in `0.0..=1.0`.
    pub fn blend_channel(self, cb: f32, cs: f32) -> f32 {
        match self {
            BlendMode::Normal => cs,
            BlendMode::Multiply => cb * cs,
            BlendMode::Screen => screen(cb, cs),
            BlendMode::Overlay => {
                if cb <= 0.5 {
                    2.0 * cb * cs
                } else {
                    screen(cs, 2.0 * cb - 1.0)
                }
            }
        }
    }

    /// Composites straight-alpha `src` (alpha already multiplied by opacity
    /// and mask) over straight-alpha `dst`, in place. Channels in `0.0..=1.0`.
    pub fn composite(self, dst: &mut [f32; 4], src: [f32; 4]) {
        let a_s = src[3];
        if a_s <= 0.0 {
            return;
        }
        let a_b = dst[3];
        let a_o = a_s + a_b * (1.0 - a_s);
        for c in 0..3 {
            let cb = dst[c];
            let cs = src[c];
            let mixed = (1.0 - a_b) * cs + a_b * self.blend_channel(cb, cs);
            dst[c] = (a_s * mixed + a_b * (1.0 - a_s) * cb) / a_o;
        }
        dst[3] = a_o;
    }
}

fn screen(a: f32, b: f32) -> f32 {
    a + b - a * b
}

/// [`unit`] for every byte, computed once (same IEEE division, so the
/// values are identical).
pub(crate) const UNIT: [f32; 256] = {
    let mut t = [0.0f32; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = i as f32 / 255.0;
        i += 1;
    }
    t
};

/// 8-bit channel to `0.0..=1.0`.
#[inline]
pub(crate) fn unit(v: u8) -> f32 {
    UNIT[v as usize]
}

/// `0.0..=1.0` to the nearest 8-bit value, halves away from zero.
///
/// Same result as `(v.clamp(0.0, 1.0) * 255.0).round() as u8`, without the
/// `roundf` call: for `x` in `0.0..=255.0`, `x - trunc(x)` is exact, so
/// comparing it with 0.5 decides the rounding exactly. NaN gives 0, as
/// before.
#[inline]
pub(crate) fn byte(v: f32) -> u8 {
    let x = v.clamp(0.0, 1.0) * 255.0;
    let t = x as u32;
    (t + (x - t as f32 >= 0.5) as u32) as u8
}

/// [`BlendMode::composite`] with shortcuts for an opaque source over a fully
/// transparent or fully opaque backdrop, the bulk of most canvases.
///
/// Exact, not approximate: with `αs = 1` and `αb ∈ {0, 1}` every dropped
/// term of the general formula is a multiplication by zero, an addition of
/// zero, or a division by one, so the result is bit-identical.
#[inline]
pub(crate) fn composite_fast(mode: BlendMode, dst: &mut [f32; 4], src: [f32; 4]) {
    if src[3] == 1.0 {
        let a_b = dst[3];
        if a_b == 0.0 {
            // Over transparent every mode is normal: Co = Cs.
            *dst = [src[0], src[1], src[2], 1.0];
            return;
        }
        if a_b == 1.0 {
            // Opaque over opaque: Co = B(Cb, Cs).
            for c in 0..3 {
                dst[c] = mode.blend_channel(dst[c], src[c]);
            }
            return;
        }
    }
    mode.composite(dst, src);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(rgba: [u8; 4]) -> [f32; 4] {
        rgba.map(unit)
    }

    fn comp(mode: BlendMode, dst: [u8; 4], src: [u8; 4]) -> [u8; 4] {
        let mut d = px(dst);
        mode.composite(&mut d, px(src));
        d.map(byte)
    }

    fn byte_reference(v: f32) -> u8 {
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    }

    #[test]
    fn unit_table_matches_division() {
        for v in 0..=255u8 {
            assert_eq!(unit(v).to_bits(), (v as f32 / 255.0).to_bits());
        }
    }

    #[test]
    fn byte_matches_round() {
        let check = |v: f32| assert_eq!(byte(v), byte_reference(v), "{v:e}");
        // Every exact level, every rounding midpoint, and their neighbours.
        for k in 0..=510u32 {
            let mid = k as f32 / 510.0;
            for x in [mid, mid * (1.0 / 255.0) * 255.0] {
                let bits = x.to_bits() as i64;
                for d in -64..=64 {
                    let b = (bits + d).max(0) as u32;
                    check(f32::from_bits(b));
                }
            }
        }
        // Midpoints in the scaled domain: (k + 0.5) / 255 and neighbours.
        for k in 0..255u32 {
            let x = (k as f32 + 0.5) / 255.0;
            for d in -256..=256i64 {
                check(f32::from_bits((x.to_bits() as i64 + d) as u32));
            }
        }
        // A dense sweep and out-of-range values.
        for i in 0..=1_000_000u32 {
            check(i as f32 / 1_000_000.0);
        }
        for v in [-1.0, -0.0, 1.5, f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
            check(v);
        }
    }

    #[test]
    fn composite_fast_is_bit_identical() {
        let levels = [0u8, 1, 2, 60, 127, 128, 129, 200, 254, 255];
        for m in BlendMode::ALL {
            for &cs in &levels {
                for &cb in &levels {
                    for &a_b in &[0u8, 255, 128] {
                        for &a_s in &[255u8, 128, 1] {
                            let src = [unit(cs), unit(255 - cs), unit(cs / 2), unit(a_s)];
                            let dst = if a_b == 0 {
                                [0.0; 4]
                            } else {
                                [unit(cb), unit(cs), unit(255 - cb), unit(a_b)]
                            };
                            let (mut slow, mut fast) = (dst, dst);
                            m.composite(&mut slow, src);
                            composite_fast(m, &mut fast, src);
                            assert_eq!(slow.map(f32::to_bits), fast.map(f32::to_bits));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn names_round_trip() {
        for m in BlendMode::ALL {
            assert_eq!(BlendMode::from_name(m.name()), Some(m));
        }
        assert_eq!(BlendMode::from_name("dissolve"), None);
        assert_eq!(BlendMode::default(), BlendMode::Normal);
    }

    #[test]
    fn channel_formulas() {
        use BlendMode::*;
        assert_eq!(Normal.blend_channel(0.2, 0.7), 0.7);
        assert!((Multiply.blend_channel(0.5, 0.5) - 0.25).abs() < 1e-6);
        assert!((Screen.blend_channel(0.5, 0.5) - 0.75).abs() < 1e-6);
        // Overlay: dark backdrop multiplies, light backdrop screens.
        assert!((Overlay.blend_channel(0.25, 0.5) - 0.25).abs() < 1e-6);
        assert!((Overlay.blend_channel(0.75, 0.5) - 0.75).abs() < 1e-6);
        // Overlay keeps black and white backdrops.
        assert_eq!(Overlay.blend_channel(0.0, 0.8), 0.0);
        assert_eq!(Overlay.blend_channel(1.0, 0.2), 1.0);
    }

    #[test]
    fn opaque_over_opaque_uses_the_blend_formula() {
        let b = [200, 100, 0, 255];
        let s = [100, 255, 128, 255];
        assert_eq!(comp(BlendMode::Normal, b, s), s);
        // 200·100/255 = 78.4, 100·255/255 = 100, 0
        assert_eq!(comp(BlendMode::Multiply, b, s), [78, 100, 0, 255]);
        // 200+100−78.4 = 221.6, 255, 128
        assert_eq!(comp(BlendMode::Screen, b, s), [222, 255, 128, 255]);
        // r: Cb>0.5 → screen(100, 2·200−255=145) = 100+145−56.9 = 188.1
        // g: Cb≤0.5 → 2·100·255/255 = 200; b: 2·0·x = 0
        assert_eq!(comp(BlendMode::Overlay, b, s), [188, 200, 0, 255]);
    }

    #[test]
    fn every_mode_is_normal_over_transparent() {
        for m in BlendMode::ALL {
            assert_eq!(comp(m, [0, 0, 0, 0], [10, 20, 30, 255]), [10, 20, 30, 255]);
            assert_eq!(comp(m, [0, 0, 0, 0], [10, 20, 30, 128]), [10, 20, 30, 128]);
        }
    }

    #[test]
    fn transparent_source_changes_nothing() {
        for m in BlendMode::ALL {
            assert_eq!(comp(m, [1, 2, 3, 4], [255, 255, 255, 0]), [1, 2, 3, 4]);
        }
    }

    #[test]
    fn partial_alpha_mixes() {
        // 50% 200-gray over opaque black, normal: 100.
        let mut d = px([0, 0, 0, 255]);
        BlendMode::Normal.composite(&mut d, [unit(200), unit(200), unit(200), 0.5]);
        assert_eq!(d.map(byte), [100, 100, 100, 255]);
        // 25% multiply of black over white: 255·0.75 = 191.25 → 191.
        let mut d = px([255, 255, 255, 255]);
        BlendMode::Multiply.composite(&mut d, [0.0, 0.0, 0.0, 0.25]);
        assert_eq!(d.map(byte), [191, 191, 191, 255]);
    }

    #[test]
    fn semi_transparent_over_semi_transparent() {
        // Source-over alpha: 0.5 + 0.5·0.5 = 0.75. Color weights: src 0.5, dst 0.25.
        let mut d = [1.0, 0.0, 0.0, 0.5];
        BlendMode::Normal.composite(&mut d, [0.0, 0.0, 1.0, 0.5]);
        assert!((d[3] - 0.75).abs() < 1e-6);
        assert!((d[0] - 1.0 / 3.0).abs() < 1e-6);
        assert!((d[2] - 2.0 / 3.0).abs() < 1e-6);
    }
}
