//! GIMP `.vbr` parametric ("generated") brushes.
//!
//! Text format, one value per line:
//! - v1.0: `GIMP-VBR`, `1.0`, name, spacing, radius, hardness, aspect ratio, angle
//! - v1.5: `GIMP-VBR`, `1.5`, name, shape, spacing, radius, spikes, hardness,
//!   aspect ratio, angle
//!
//! The mask is odd-sized with the brush center on the middle pixel. Hardness
//! falloff follows GIMP: `f = (d / radius) ^ (0.4 / (1 - hardness))` passed
//! through a smooth bell (`1 - 2f²` below 0.5, `2(1 - f)²` above).

use crate::{Brush, BrushError, Mask};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Circle,
    Square,
    Diamond,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VbrParams {
    pub name: String,
    pub shape: Shape,
    /// Percent of brush size.
    pub spacing: f32,
    /// Pixels from center to rim along the long axis.
    pub radius: f32,
    /// Number of points; 2 means the plain shape.
    pub spikes: u32,
    /// 0 = soft, 1 = hard edge.
    pub hardness: f32,
    /// Long axis / short axis, at least 1.
    pub aspect_ratio: f32,
    /// Counter-clockwise rotation in degrees.
    pub angle: f32,
}

const MAX_RADIUS: f32 = 1000.0;

pub fn parse_vbr(text: &str) -> Result<(VbrParams, Brush), BrushError> {
    let mut lines = text.lines().map(str::trim);
    let mut next = |what: &str| {
        lines
            .next()
            .ok_or_else(|| BrushError::Invalid(format!("missing {what}")))
    };
    if next("magic")? != "GIMP-VBR" {
        return Err(BrushError::Invalid("missing GIMP-VBR magic".into()));
    }
    let version = next("version")?;
    let num = |s: &str, what: &str| {
        s.parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .ok_or_else(|| BrushError::Invalid(format!("bad {what}: {s:?}")))
    };
    let params = match version {
        "1.0" => {
            let name = next("name")?.to_string();
            VbrParams {
                name,
                shape: Shape::Circle,
                spacing: num(next("spacing")?, "spacing")?,
                radius: num(next("radius")?, "radius")?,
                spikes: 2,
                hardness: num(next("hardness")?, "hardness")?,
                aspect_ratio: num(next("aspect ratio")?, "aspect ratio")?,
                angle: num(next("angle")?, "angle")?,
            }
        }
        "1.5" => {
            let name = next("name")?.to_string();
            let shape = match next("shape")? {
                "circle" => Shape::Circle,
                "square" => Shape::Square,
                "diamond" => Shape::Diamond,
                s => return Err(BrushError::Invalid(format!("unknown shape {s:?}"))),
            };
            let spacing = num(next("spacing")?, "spacing")?;
            let radius = num(next("radius")?, "radius")?;
            let spikes = next("spikes")?;
            let spikes = spikes
                .parse::<u32>()
                .map_err(|_| BrushError::Invalid(format!("bad spikes: {spikes:?}")))?;
            VbrParams {
                name,
                shape,
                spacing,
                radius,
                spikes,
                hardness: num(next("hardness")?, "hardness")?,
                aspect_ratio: num(next("aspect ratio")?, "aspect ratio")?,
                angle: num(next("angle")?, "angle")?,
            }
        }
        v => return Err(BrushError::Unsupported(format!("vbr version {v:?}"))),
    };
    let brush = generate(&params);
    Ok((params, brush))
}

fn bell(f: f32) -> f32 {
    if f < 0.5 {
        1.0 - 2.0 * f * f
    } else if f < 1.0 {
        2.0 * (1.0 - f) * (1.0 - f)
    } else {
        0.0
    }
}

/// Render a parametric brush into a mask. Out-of-range parameters are clamped
/// to GIMP's limits.
pub fn generate(p: &VbrParams) -> Brush {
    let radius = p.radius.clamp(0.1, MAX_RADIUS);
    let hardness = p.hardness.clamp(0.0, 1.0);
    let aspect = p.aspect_ratio.clamp(1.0, 1000.0);
    let spikes = p.spikes.clamp(2, 20);
    let (s, c) = p.angle.to_radians().sin_cos();
    let exponent = if 1.0 - hardness < 1e-6 {
        None
    } else {
        Some(0.4 / (1.0 - hardness))
    };
    let wedge = std::f32::consts::TAU / spikes as f32;

    let half = (radius * std::f32::consts::SQRT_2).ceil() as i32 + 1;
    let size = (2 * half + 1) as u32;
    let mut data = vec![0u8; (size * size) as usize];
    for py in -half..=half {
        for px in -half..=half {
            // Screen y points down; rotate in math coordinates.
            let (x, y) = (px as f32, -py as f32);
            let mut u = x * c + y * s;
            let mut v = (-x * s + y * c) * aspect;
            if spikes > 2 {
                let r = u.hypot(v);
                let t = v.atan2(u);
                let tf = t - wedge * (t / wedge).round();
                u = r * tf.cos();
                v = r * tf.sin();
            }
            let d = match p.shape {
                Shape::Circle => u.hypot(v),
                Shape::Square => u.abs().max(v.abs()),
                Shape::Diamond => u.abs() + v.abs(),
            };
            let ratio = d / radius;
            let value = if ratio >= 1.0 {
                0.0
            } else {
                match exponent {
                    None => 1.0,
                    Some(e) => bell(ratio.powf(e)),
                }
            };
            let idx = ((py + half) as u32 * size + (px + half) as u32) as usize;
            data[idx] = (value * 255.0).round() as u8;
        }
    }

    Brush {
        name: p.name.clone(),
        spacing: p.spacing,
        mask: trim_symmetric(Mask::new(size, size, data)),
        pixmap: None,
    }
}

/// Crop empty borders while keeping the center pixel in the middle.
fn trim_symmetric(m: Mask) -> Mask {
    let (cx, cy) = ((m.width / 2) as i64, (m.height / 2) as i64);
    let (mut hx, mut hy) = (0i64, 0i64);
    for y in 0..m.height {
        for x in 0..m.width {
            if m.get(x, y) != 0 {
                hx = hx.max((x as i64 - cx).abs());
                hy = hy.max((y as i64 - cy).abs());
            }
        }
    }
    let (w, h) = ((2 * hx + 1) as u32, (2 * hy + 1) as u32);
    let (x0, y0) = ((cx - hx) as u32, (cy - hy) as u32);
    let mut data = Vec::with_capacity((w * h) as usize);
    for y in y0..y0 + h {
        let row = (y * m.width + x0) as usize;
        data.extend_from_slice(&m.data[row..row + w as usize]);
    }
    Mask::new(w, h, data)
}
