//! CRT post-process: the GLSL shader source, its parameters, where the tube
//! sits in the window, and a pure-Rust mirror of the shader math for tests.
//!
//! The mirror functions ([`curve_uv`], [`scanline`], [`flicker`],
//! [`vignette`], [`shade`]) follow `shaders/crt.fs` line for line. Change both
//! together. See docs/aesthetics.md, "CRT shader".

use std::f32::consts::PI;

use rp_doc::Layout;

use crate::Rect;

/// Fragment shader source (GLSL 330, for raylib's default vertex shader).
pub const CRT_FS: &str = include_str!("../shaders/crt.fs");

/// CRT shader parameters. `time` is passed per frame, and `resolution` is the
/// DOS screen size.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CrtParams {
    /// Barrel distortion strength; 0 is a flat screen.
    pub curvature: f32,
    /// How dark the gap between scanlines gets, 0..=1.
    pub scanline_intensity: f32,
    /// Peak brightness dip of the flicker, 0..=1.
    pub flicker: f32,
    /// Per-channel tint. White leaves CGA colors unchanged.
    pub phosphor: [f32; 3],
}

impl Default for CrtParams {
    /// The original shader's values.
    fn default() -> Self {
        Self {
            curvature: 0.18,
            scanline_intensity: 0.35,
            flicker: 0.04,
            phosphor: [1.0, 1.0, 1.0],
        }
    }
}

/// Barrel-distorts a UV in `[0, 1]²`. Each axis is pushed out by the squared
/// distance along the other axis, rounding the corners. Results outside
/// `[0, 1]²` are bezel (see [`in_tube`]).
pub fn curve_uv(uv: [f32; 2], curvature: f32) -> [f32; 2] {
    let c = [uv[0] * 2.0 - 1.0, uv[1] * 2.0 - 1.0];
    let o = [c[1].abs() * curvature, c[0].abs() * curvature];
    [
        (c[0] + c[0] * o[0] * o[0]) * 0.5 + 0.5,
        (c[1] + c[1] * o[1] * o[1]) * 0.5 + 0.5,
    ]
}

/// Whether a curved UV lands on the lit screen (not the black bezel).
pub fn in_tube(curved: [f32; 2]) -> bool {
    curved.iter().all(|v| (0.0..=1.0).contains(v))
}

/// Scanline brightness at curved `y` for a screen of `rows` rows: 1 at each
/// row center, `1 - intensity` on the boundary between rows.
pub fn scanline(y: f32, rows: f32, intensity: f32) -> f32 {
    let line = (2.0 * PI * y * rows).cos();
    1.0 - intensity * (0.5 + 0.5 * line)
}

/// Flicker brightness at `time` seconds: between `1 - amount` and 1.
pub fn flicker(time: f32, amount: f32) -> f32 {
    1.0 - amount * (0.5 + 0.5 * (time * 100.0).sin())
}

/// Vignette brightness at a curved UV: 1 over most of the screen, falling
/// toward 0 at the edges.
pub fn vignette(curved: [f32; 2]) -> f32 {
    let edge = [curved[0] * (1.0 - curved[1]), curved[1] * (1.0 - curved[0])];
    (edge[0] * edge[1] * 40.0).powf(0.25).clamp(0.0, 1.0)
}

/// The whole shader at one tube UV. `sample` reads the screen texture at a
/// UV (point filtered) and returns linear 0..=1 RGB as the GPU sees it.
pub fn shade(
    uv: [f32; 2],
    resolution: [f32; 2],
    params: &CrtParams,
    time: f32,
    sample: impl Fn([f32; 2]) -> [f32; 3],
) -> [f32; 3] {
    let curved = curve_uv(uv, params.curvature);
    if !in_tube(curved) {
        return [0.0; 3];
    }
    let k = scanline(curved[1], resolution[1], params.scanline_intensity)
        * flicker(time, params.flicker)
        * vignette(curved);
    let src = sample(curved);
    std::array::from_fn(|i| src[i] * params.phosphor[i] * k)
}

/// Where the tube (the screen texture, drawn through the shader) goes in a
/// window of `window` = `(width, height)` pixels.
///
/// Mobile fills the whole (9:16) window. Desktop is the largest 4:3 rect,
/// centered, leaving black bars on the sides of a 16:9 window.
pub fn tube_rect(window: (i32, i32), layout: Layout) -> Rect {
    let (ww, wh) = (window.0.max(0), window.1.max(0));
    match layout {
        Layout::Mobile => Rect::new(0, 0, ww, wh),
        Layout::Desktop => {
            let (w, h) = if ww * 3 >= wh * 4 {
                (wh * 4 / 3, wh)
            } else {
                (ww, ww * 3 / 4)
            };
            Rect::new((ww - w) / 2, (wh - h) / 2, w, h)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rp_palette_dither::CgaColor;

    const K: f32 = 0.18;

    #[test]
    fn defaults_match_original() {
        let p = CrtParams::default();
        assert_eq!(
            (p.curvature, p.scanline_intensity, p.flicker, p.phosphor),
            (0.18, 0.35, 0.04, [1.0; 3])
        );
    }

    #[test]
    fn shader_source_declares_every_uniform() {
        assert!(CRT_FS.starts_with("#version 330"));
        for u in [
            "resolution",
            "curvature",
            "scanlineIntensity",
            "time",
            "flicker",
            "phosphor",
        ] {
            assert!(CRT_FS.contains(&format!(" {u};")), "missing uniform {u}");
        }
    }

    #[test]
    fn center_maps_to_itself() {
        assert_eq!(curve_uv([0.5, 0.5], K), [0.5, 0.5]);
    }

    #[test]
    fn center_lines_stay_straight() {
        for t in [0.0, 0.1, 0.37, 0.9, 1.0] {
            assert!((curve_uv([0.5, t], K)[1] - t).abs() < 1e-6);
            assert!((curve_uv([t, 0.5], K)[0] - t).abs() < 1e-6);
        }
    }

    #[test]
    fn corners_curve_out_into_the_bezel() {
        for uv in [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]] {
            let c = curve_uv(uv, K);
            assert!(!in_tube(c), "{uv:?} -> {c:?} should be bezel");
            assert_eq!(
                shade(uv, [200.0, 320.0], &CrtParams::default(), 0.0, |_| [1.0; 3]),
                [0.0; 3]
            );
        }
        // Edge midpoints stay just on the screen.
        assert!(in_tube(curve_uv([0.0, 0.5], K)));
        assert!(in_tube(curve_uv([0.5, 1.0], K)));
    }

    #[test]
    fn zero_curvature_is_identity() {
        for uv in [[0.0, 0.0], [0.2, 0.9], [1.0, 1.0]] {
            let c = curve_uv(uv, 0.0);
            assert!((c[0] - uv[0]).abs() < 1e-6 && (c[1] - uv[1]).abs() < 1e-6);
        }
    }

    #[test]
    fn scanline_count_equals_row_count() {
        for rows in [200, 320] {
            let rows_f = rows as f32;
            // Bright maxima, sampled finely over the screen height.
            let n = rows * 16;
            let s: Vec<f32> = (0..=n)
                .map(|i| scanline(i as f32 / n as f32, rows_f, 0.35))
                .collect();
            let peaks = s.windows(3).filter(|w| w[1] > w[0] && w[1] >= w[2]).count();
            assert_eq!(peaks, rows);
            // Row centers are full brightness, row boundaries are darkest.
            for r in [0, 1, rows / 2, rows - 1] {
                let center = (r as f32 + 0.5) / rows_f;
                assert!((scanline(center, rows_f, 0.35) - 1.0).abs() < 1e-3);
                let edge = r as f32 / rows_f;
                assert!((scanline(edge, rows_f, 0.35) - 0.65).abs() < 1e-3);
            }
        }
    }

    #[test]
    fn flicker_stays_subtle() {
        for i in 0..1000 {
            let f = flicker(i as f32 * 0.0137, 0.04);
            assert!((0.96 - 1e-6..=1.0 + 1e-6).contains(&f));
        }
    }

    #[test]
    fn vignette_is_flat_in_the_middle_and_dark_at_edges() {
        assert_eq!(vignette([0.5, 0.5]), 1.0);
        assert_eq!(vignette([0.25, 0.75]), 1.0);
        assert_eq!(vignette([0.0, 0.5]), 0.0);
        assert!(vignette([0.02, 0.5]) < 1.0);
    }

    #[test]
    fn white_phosphor_passes_cga_colors_through() {
        let p = CrtParams::default();
        // sin(100 t) = -1: the flicker's full-brightness phase.
        let t = 3.0 * PI / 200.0;
        for (rows, res) in [(320, [200.0, 320.0]), (200, [320.0, 200.0])] {
            for color in CgaColor::ALL {
                let rgb = color.rgb().to_array().map(|c| c as f32 / 255.0);
                for row in [rows / 4, rows / 2, rows * 3 / 4] {
                    let uv = [0.5, (row as f32 + 0.5) / rows as f32];
                    let out = shade(uv, res, &p, t, |_| rgb);
                    let out = out.map(|c| (c * 255.0).round() as u8);
                    assert_eq!(out, color.rgb().to_array(), "{color:?} row {row}");
                }
            }
        }
    }

    #[test]
    fn mobile_tube_fills_the_window() {
        assert_eq!(
            tube_rect((1080, 1920), Layout::Mobile),
            Rect::new(0, 0, 1080, 1920)
        );
        assert_eq!(
            tube_rect((450, 800), Layout::Mobile),
            Rect::new(0, 0, 450, 800)
        );
    }

    #[test]
    fn desktop_tube_is_centered_4_3_with_black_sides() {
        assert_eq!(
            tube_rect((1920, 1080), Layout::Desktop),
            Rect::new(240, 0, 1440, 1080)
        );
        assert_eq!(
            tube_rect((1280, 720), Layout::Desktop),
            Rect::new(160, 0, 960, 720)
        );
        // Taller than 4:3: bars top and bottom instead.
        assert_eq!(
            tube_rect((800, 800), Layout::Desktop),
            Rect::new(0, 100, 800, 600)
        );
        assert_eq!(tube_rect((0, 0), Layout::Desktop), Rect::new(0, 0, 0, 0));
    }
}
