//! Dab stamping and stroke spacing.

use crate::Brush;

/// Composite one dab of `brush` onto a straight-alpha RGBA8 buffer
/// (`dst.len() == width * height * 4`), source-over.
///
/// The mask's middle pixel (`width / 2`, `height / 2`) lands on the pixel that
/// contains `(cx, cy)`. Grayscale brushes paint `color`; RGBA brushes paint
/// their own pixmap. Coverage is `mask * color alpha * opacity`. Pixels outside
/// the buffer are clipped.
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

/// Turns a stream of pointer positions into dab positions `step` pixels apart
/// along the polyline, carrying leftover distance across segments.
#[derive(Debug, Clone)]
pub struct Spacer {
    step: f32,
    last: Option<(f32, f32)>,
    /// Distance travelled since the last dab.
    carry: f32,
}

impl Spacer {
    /// `step` is clamped to at least 0.5 px; use [`Brush::step`] for a brush.
    pub fn new(step: f32) -> Self {
        Self {
            step: if step.is_finite() { step.max(0.5) } else { 1.0 },
            last: None,
            carry: 0.0,
        }
    }

    /// Feed the next pointer position; returns the dabs it produces. The
    /// first point always produces a dab.
    pub fn push(&mut self, p: (f32, f32)) -> Vec<(f32, f32)> {
        let Some(last) = self.last else {
            self.last = Some(p);
            self.carry = 0.0;
            return vec![p];
        };
        let (dx, dy) = (p.0 - last.0, p.1 - last.1);
        let len = dx.hypot(dy);
        self.last = Some(p);
        if len <= 0.0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut t = self.step - self.carry;
        while t <= len {
            let f = t / len;
            out.push((last.0 + dx * f, last.1 + dy * f));
            t += self.step;
        }
        self.carry = len - (t - self.step);
        out
    }
}

/// All dab positions for a whole polyline stroke.
pub fn stroke_dabs(points: &[(f32, f32)], step: f32) -> Vec<(f32, f32)> {
    let mut s = Spacer::new(step);
    points.iter().flat_map(|&p| s.push(p)).collect()
}
