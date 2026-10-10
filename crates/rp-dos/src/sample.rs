//! A built-in sample output image for goldens and the `dump-screen` example:
//! a synthwave sunset (dithered sky, striped sun, mountains, perspective
//! grid) in the full CGA palette. Integer math only, so it is the same
//! everywhere.

use rp_doc::{FrameMsg, IndexedFrame, Layout, OutputPoint, Size, Tool};
use rp_palette_dither::{CgaColor, Palette};

const BAYER4: [[u32; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

const SKY: [CgaColor; 5] = [
    CgaColor::Black,
    CgaColor::Blue,
    CgaColor::Magenta,
    CgaColor::LightMagenta,
    CgaColor::LightRed,
];

/// Grid lines on each side of the vanishing point.
const GRID_LINES: i32 = 10;

fn hash(mut v: u32) -> u32 {
    v ^= v >> 16;
    v = v.wrapping_mul(0x7feb_352d);
    v ^= v >> 15;
    v = v.wrapping_mul(0x846c_a68b);
    v ^ (v >> 16)
}

/// Ridge height (pixels above the horizon) at column `x`: random peaks every
/// `step` columns joined by straight slopes.
fn ridge(x: i32, step: i32, max: i32, seed: u32) -> i32 {
    let k = x.div_euclid(step);
    let t = x.rem_euclid(step);
    let peak = |k: i32| (hash(k as u32 ^ seed) % max.max(1) as u32) as i32;
    let (a, b) = (peak(k), peak(k + 1));
    a + (b - a) * t / step
}

/// Output size of the sample for each layout: larger than the canvas window,
/// so the window scrolls.
pub const fn sample_size(layout: Layout) -> Size {
    match layout {
        Layout::Mobile => Size::new(240, 400),
        Layout::Desktop => Size::new(400, 240),
    }
}

/// A complete sample frame: the sample scene at [`sample_size`], brush tool
/// (9 px) near the bottom right so the window has scrolled, light magenta
/// active.
pub fn sample_frame(layout: Layout) -> FrameMsg {
    let size = sample_size(layout);
    FrameMsg {
        image: sample_image(size),
        layout,
        cursor: Some(OutputPoint {
            x: size.width as i32 * 3 / 4,
            y: size.height as i32 * 3 / 4,
        }),
        tool: Tool::Brush,
        brush_diameter: 9,
        active_color: CgaColor::LightMagenta,
    }
}

/// The sample scene at `size`, full 16-color palette.
pub fn sample_image(size: Size) -> IndexedFrame {
    let (w, h) = (size.width as i32, size.height as i32);
    let horizon = h * 11 / 20;
    let (cx, sun_r) = (w / 2, (w.min(h) * 7 / 20).max(2));
    let mut frame = IndexedFrame::filled(size, CgaColor::Black, Palette::full());

    for y in 0..h {
        for x in 0..w {
            let bayer = BAYER4[(y & 3) as usize][(x & 3) as usize];
            let c = if y < horizon {
                sky(x, y, w, horizon, cx, sun_r, bayer)
            } else {
                ground(x, y, horizon, cx)
            };
            frame.pixels[(y * w + x) as usize] = c;
        }
    }
    frame
}

fn sky(x: i32, y: i32, w: i32, horizon: i32, cx: i32, sun_r: i32, bayer: u32) -> CgaColor {
    // Mountains in front of the sun.
    let far = ridge(x, (w / 7).max(2), (horizon / 6).max(1), 0x5eed);
    let near = ridge(x + 13, (w / 4).max(2), (horizon / 10).max(1), 0xbeef);
    if y >= horizon - near {
        return CgaColor::Blue;
    }
    if y >= horizon - far {
        return CgaColor::Magenta;
    }

    // Striped sun.
    let (dx, dy) = (x - cx, y - horizon);
    if dx * dx + dy * dy <= sun_r * sun_r {
        let up = horizon - y; // 0 at the horizon, sun_r at the top
        // Stripes in the lower part, widening toward the horizon.
        let low = sun_r * 3 / 5;
        let gap = up < low && up % 8 < 1 + (low - up) * 4 / sun_r.max(1);
        if !gap {
            return CgaColor::Yellow;
        }
    }

    // Stars in the dark top band.
    if y < horizon / 3 && hash((y * w + x) as u32).is_multiple_of(97) {
        return if hash(x as u32).is_multiple_of(3) {
            CgaColor::White
        } else {
            CgaColor::LightGray
        };
    }

    // Dithered gradient.
    let steps = (SKY.len() as i32 - 1) * 16;
    let pos = y * steps / horizon.max(1);
    let (i, frac) = ((pos / 16) as usize, (pos % 16) as u32);
    if frac > bayer && i + 1 < SKY.len() {
        SKY[i + 1]
    } else {
        SKY[i.min(SKY.len() - 1)]
    }
}

fn ground(x: i32, y: i32, horizon: i32, cx: i32) -> CgaColor {
    let depth = y - horizon + 1;
    // Horizontal lines bunch up toward the horizon.
    let mut row = 2;
    let mut step = 2;
    while row < depth {
        row += step;
        step += 2;
    }
    if depth == 1 || row == depth {
        return CgaColor::LightMagenta;
    }
    // Lines converging on the vanishing point: line `k` sits at
    // `cx + k * depth * 3 / 8`. Each row fills the run to the next row's
    // position so shallow lines stay connected.
    let span8 = depth * 3;
    let k0 = ((x - cx) * 8).div_euclid(span8.max(1));
    for k in (k0 - 1..=k0 + 1).filter(|k| k.abs() <= GRID_LINES) {
        let xa = cx + k * depth * 3 / 8;
        let xb = cx + k * (depth + 1) * 3 / 8;
        if (xa.min(xb)..=xa.max(xb)).contains(&x) {
            return CgaColor::LightCyan;
        }
    }
    if depth < 3 {
        CgaColor::Magenta
    } else {
        CgaColor::Black
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rp_palette_dither::test_util::assert_palette_only;

    #[test]
    fn sample_has_the_requested_size_and_cga_colors() {
        let f = sample_image(Size::new(240, 400));
        assert_eq!(f.pixels.len(), 240 * 400);
        assert_palette_only(f.pixels.iter().copied(), &f.palette);
        let distinct: std::collections::HashSet<_> = f.pixels.iter().collect();
        assert!(distinct.len() >= 8, "only {} colors", distinct.len());
    }

    #[test]
    fn tiny_sizes_do_not_panic() {
        for (w, h) in [(1, 1), (2, 3), (7, 1), (1, 9)] {
            sample_image(Size::new(w, h));
        }
    }
}
