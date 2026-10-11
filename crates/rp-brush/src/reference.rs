//! The straightforward per-pixel float dab functions the fast ones in
//! [`crate::stamp`] must match byte for byte. Kept as the test reference and
//! for before/after timings; not for use in the app.

use crate::Brush;

/// The original [`crate::stamp`]: float source-over, per pixel.
#[allow(clippy::too_many_arguments)]
pub fn stamp(
    dst: &mut [u8],
    width: u32,
    height: u32,
    brush: &Brush,
    cx: f32,
    cy: f32,
    color: [u8; 4],
    opacity: f32,
) {
    assert_eq!(dst.len(), width as usize * height as usize * 4);
    let m = &brush.mask;
    let x0 = cx.floor() as i64 - (m.width / 2) as i64;
    let y0 = cy.floor() as i64 - (m.height / 2) as i64;
    let base = color[3] as f32 / 255.0 * opacity.clamp(0.0, 1.0);
    if base <= 0.0 {
        return;
    }
    for my in 0..m.height {
        let y = y0 + my as i64;
        if y < 0 || y >= height as i64 {
            continue;
        }
        for mx in 0..m.width {
            let x = x0 + mx as i64;
            if x < 0 || x >= width as i64 {
                continue;
            }
            let mi = (my * m.width + mx) as usize;
            let a = m.data[mi] as f32 / 255.0 * base;
            if a <= 0.0 {
                continue;
            }
            let src = match &brush.pixmap {
                Some(pm) => pm[mi],
                None => [color[0], color[1], color[2]],
            };
            let di = (y as usize * width as usize + x as usize) * 4;
            let px = &mut dst[di..di + 4];
            let da = px[3] as f32 / 255.0;
            let out_a = a + da * (1.0 - a);
            for k in 0..3 {
                let v = (src[k] as f32 * a + px[k] as f32 * da * (1.0 - a)) / out_a;
                px[k] = v.round().clamp(0.0, 255.0) as u8;
            }
            px[3] = (out_a * 255.0).round() as u8;
        }
    }
}

/// The original eraser dab (from `rp-editor`'s `paint_ops::erase_dab`).
pub fn erase_dab(
    dst: &mut [u8],
    width: u32,
    height: u32,
    brush: &Brush,
    cx: f32,
    cy: f32,
    opacity: f32,
) {
    assert_eq!(dst.len(), width as usize * height as usize * 4);
    let m = &brush.mask;
    let x0 = cx.floor() as i64 - (m.width / 2) as i64;
    let y0 = cy.floor() as i64 - (m.height / 2) as i64;
    let opacity = opacity.clamp(0.0, 1.0);
    for my in 0..m.height {
        let y = y0 + my as i64;
        if y < 0 || y >= height as i64 {
            continue;
        }
        for mx in 0..m.width {
            let x = x0 + mx as i64;
            if x < 0 || x >= width as i64 {
                continue;
            }
            let a = m.data[(my * m.width + mx) as usize] as f32 / 255.0 * opacity;
            if a <= 0.0 {
                continue;
            }
            let ai = (y as usize * width as usize + x as usize) * 4 + 3;
            dst[ai] = (dst[ai] as f32 * (1.0 - a)).round() as u8;
        }
    }
}

/// The original layer-mask dab (from `rp-editor`'s `mask_ops::mask_dab`).
#[allow(clippy::too_many_arguments)]
pub fn mask_dab(
    dst: &mut [u8],
    width: u32,
    height: u32,
    brush: &Brush,
    cx: f32,
    cy: f32,
    value: u8,
    opacity: f32,
) {
    assert_eq!(dst.len(), width as usize * height as usize);
    let m = &brush.mask;
    let x0 = cx.floor() as i64 - (m.width / 2) as i64;
    let y0 = cy.floor() as i64 - (m.height / 2) as i64;
    let opacity = opacity.clamp(0.0, 1.0);
    let target = value as f32;
    for my in 0..m.height {
        let y = y0 + my as i64;
        if y < 0 || y >= height as i64 {
            continue;
        }
        for mx in 0..m.width {
            let x = x0 + mx as i64;
            if x < 0 || x >= width as i64 {
                continue;
            }
            let a = m.data[(my * m.width + mx) as usize] as f32 / 255.0 * opacity;
            if a <= 0.0 {
                continue;
            }
            let i = y as usize * width as usize + x as usize;
            let v = dst[i] as f32;
            dst[i] = (v + (target - v) * a).round() as u8;
        }
    }
}
