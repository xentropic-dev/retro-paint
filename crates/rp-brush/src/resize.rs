//! Bitmap brush resampling: area (box) filter for downscale, bilinear for
//! upscale, applied separably per axis.

use std::sync::Arc;

use crate::{Brush, BrushSource, Mask};

/// Largest diameter [`Brush::resized`] accepts (twice the `.vbr` radius limit).
pub(crate) const MAX_DIAMETER: f32 = 2000.0;

/// For each destination pixel, the source pixels and weights (summing to 1).
type Taps = Vec<Vec<(usize, f32)>>;

fn taps(src: u32, dst: u32) -> Taps {
    let (n, m) = (src as usize, dst as usize);
    if m <= n {
        // Area: destination pixel i covers [i * s, (i + 1) * s) in source.
        let s = n as f64 / m as f64;
        (0..m)
            .map(|i| {
                let (a, b) = (i as f64 * s, (i + 1) as f64 * s);
                let mut v = Vec::new();
                let mut j = a.floor() as usize;
                while (j as f64) < b && j < n {
                    let w = (b.min(j as f64 + 1.0) - a.max(j as f64)) / s;
                    if w > 0.0 {
                        v.push((j, w as f32));
                    }
                    j += 1;
                }
                v
            })
            .collect()
    } else {
        // Bilinear on pixel centers, clamped at the edges.
        let s = n as f32 / m as f32;
        (0..m)
            .map(|i| {
                let x = ((i as f32 + 0.5) * s - 0.5).clamp(0.0, (n - 1) as f32);
                let j = x.floor() as usize;
                let f = x - j as f32;
                if j + 1 < n && f > 0.0 {
                    vec![(j, 1.0 - f), (j + 1, f)]
                } else {
                    vec![(j, 1.0)]
                }
            })
            .collect()
    }
}

/// Resample a `w x h` plane to `tx.len() x ty.len()`.
fn apply(src: &[f32], w: usize, tx: &Taps, ty: &Taps) -> Vec<f32> {
    let h = src.len() / w;
    let nw = tx.len();
    let mut rows = vec![0f32; nw * h];
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        for (x, t) in tx.iter().enumerate() {
            rows[y * nw + x] = t.iter().map(|&(j, k)| row[j] * k).sum();
        }
    }
    let mut out = vec![0f32; nw * ty.len()];
    for (y, t) in ty.iter().enumerate() {
        for x in 0..nw {
            out[y * nw + x] = t.iter().map(|&(j, k)| rows[j * nw + x] * k).sum();
        }
    }
    out
}

fn to_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// Resample a bitmap brush so its larger side is `diameter` pixels. The
/// result's source is [`BrushSource::Resampled`] pointing at `orig`.
pub(crate) fn resample(orig: &Arc<Brush>, diameter: u32) -> Brush {
    let b = orig.as_ref();
    let d = diameter.max(1);
    let (w, h) = (b.mask.width, b.mask.height);
    let long = w.max(h) as f64;
    let side = |s: u32| ((s as f64 * d as f64 / long).round() as u32).max(1);
    let (nw, nh) = if w >= h { (d, side(h)) } else { (side(w), d) };
    let (tx, ty) = (taps(w, nw), taps(h, nh));

    let mask: Vec<f32> = b.mask.data.iter().map(|&v| v as f32).collect();
    let new_mask = apply(&mask, w as usize, &tx, &ty);

    // Colors are averaged weighted by coverage so transparent pixels do not
    // bleed; where coverage is zero, fall back to the plain average.
    let pixmap = b.pixmap.as_ref().map(|pm| {
        let chan = |c: usize, weighted: bool| -> Vec<f32> {
            let plane: Vec<f32> = pm
                .iter()
                .zip(&mask)
                .map(|(p, &m)| p[c] as f32 * if weighted { m } else { 1.0 })
                .collect();
            apply(&plane, w as usize, &tx, &ty)
        };
        let weighted: Vec<Vec<f32>> = (0..3).map(|c| chan(c, true)).collect();
        let plain: Vec<Vec<f32>> = (0..3).map(|c| chan(c, false)).collect();
        (0..new_mask.len())
            .map(|i| {
                let m = new_mask[i];
                std::array::from_fn(|c| {
                    if m > 1e-3 {
                        to_u8(weighted[c][i] / m)
                    } else {
                        to_u8(plain[c][i])
                    }
                })
            })
            .collect()
    });

    let mut data: Vec<u8> = new_mask.into_iter().map(to_u8).collect();
    // A 1x1 dab is the brush at its strongest, not its average, so tiny
    // sizes of sparse or soft brushes still paint a full pixel.
    if data.len() == 1 {
        data[0] = b.mask.data.iter().copied().max().unwrap_or(0);
    }

    Brush {
        name: b.name.clone(),
        spacing: b.spacing,
        mask: Mask::new(nw, nh, data),
        pixmap,
        source: BrushSource::Resampled(orig.clone()),
    }
}
