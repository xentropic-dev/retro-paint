//! Dab stamping and stroke spacing.
//!
//! # Speed
//!
//! The dab functions give exactly the bytes of the plain per-pixel float
//! formulas in [`crate::reference`] (tests compare them exhaustively), but
//! skip the work those do per pixel:
//! - Rows are trimmed to their first and last nonzero coverage, and empty
//!   rows are skipped, so splatters cost what they cover.
//! - Every float factor that depends only on the coverage byte is computed
//!   once per dab into a 256-entry table, with the same expression.
//! - Full coverage and transparent destination pixels are written directly:
//!   the formula provably returns the source color there.
//! - Over an opaque destination pixel, and for erase and mask dabs, the
//!   result depends only on (coverage, old byte); for big dabs it is read
//!   from a 256 × 256 table built from the formula. Tables are cached per
//!   thread for the last color/opacity, so a stroke builds them once.
//! - Rounding uses truncation instead of a `roundf` call (same result for
//!   the non-negative values these formulas produce).

use std::cell::RefCell;
use std::rc::Rc;
use std::thread::LocalKey;

use crate::{Brush, Mask};

/// Composite one dab of `brush` onto a straight-alpha RGBA8 buffer
/// (`dst.len() == width * height * 4`), source-over.
///
/// The mask's middle pixel (`width / 2`, `height / 2`) lands on the pixel that
/// contains `(cx, cy)`. Grayscale brushes paint `color`; RGBA brushes paint
/// their own pixmap. Coverage is `mask * color alpha * opacity`. Pixels outside
/// the buffer are clipped.
///
/// Byte-identical to [`crate::reference::stamp`]; see the module docs.
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
    let base = color[3] as f32 / 255.0 * opacity.clamp(0.0, 1.0);
    if base.is_nan() {
        return crate::reference::stamp(dst, width, height, brush, cx, cy, color, opacity);
    }
    if base <= 0.0 {
        return;
    }
    let m = &brush.mask;
    let Some(c) = Clip::new(m.width, m.height, width, height, cx, cy) else {
        return;
    };
    let t = OverTables::new(base);
    match &brush.pixmap {
        None => stamp_color(dst, width, m, &c, &t, [color[0], color[1], color[2]], base),
        Some(pm) => stamp_pixmap(dst, width, m, &c, &t, pm),
    }
}

fn stamp_color(
    dst: &mut [u8],
    width: u32,
    m: &Mask,
    c: &Clip,
    t: &OverTables,
    src: [u8; 3],
    base: f32,
) {
    // Over a transparent pixel: the source color at the formula's alpha.
    let mut clear = [[0u8; 4]; 256];
    for (mv, px) in clear.iter_mut().enumerate() {
        *px = [src[0], src[1], src[2], t.alpha0[mv]];
    }
    // Over an opaque pixel: per channel, `(src * a + p * (1 - a)) / out_a`,
    // from tables (cached, or built for a big enough dab).
    let key = (
        u32::from_le_bytes([src[0], src[1], src[2], 0]),
        base.to_bits(),
    );
    let opaque = cached(&OPAQUE_LUT, &key).or_else(|| {
        (c.pixels() >= OPAQUE_LUT_MIN_PIXELS).then(|| store(&OPAQUE_LUT, key, opaque_lut(src, t)))
    });
    let opaque = opaque.as_deref();
    let full = [src[0], src[1], src[2], 255];
    for_each_span(m, c, width, |_, di, row| {
        let out = &mut dst[di * 4..(di + row.len()) * 4];
        for (&mv, px) in row.iter().zip(out.as_chunks_mut::<4>().0) {
            if mv == 0 {
                continue;
            }
            let mi = mv as usize;
            match px[3] {
                0 if t.clear_ok => *px = clear[mi],
                255 => {
                    let [p0, p1, p2, _] = *px;
                    let alpha = t.alpha1[mi];
                    *px = if let Some([r, g, b]) = opaque {
                        let (r, g, b) = (&**r, &**g, &**b);
                        [
                            r[lut_index(mv, p0)],
                            g[lut_index(mv, p1)],
                            b[lut_index(mv, p2)],
                            alpha,
                        ]
                    } else if mv == 255 && t.a[255] == 1.0 {
                        full
                    } else {
                        let (a, om, oa) = (t.a[mi], t.om[mi], t.oa1[mi]);
                        [
                            over_opaque(src[0], p0, a, om, oa),
                            over_opaque(src[1], p1, a, om, oa),
                            over_opaque(src[2], p2, a, om, oa),
                            alpha,
                        ]
                    };
                }
                _ => {
                    let a = t.a[mi];
                    if a == 1.0 {
                        *px = full;
                    } else {
                        over(px, src, a);
                    }
                }
            }
        }
    });
}

fn stamp_pixmap(dst: &mut [u8], width: u32, m: &Mask, c: &Clip, t: &OverTables, pm: &[[u8; 3]]) {
    for_each_span(m, c, width, |mi0, di, row| {
        let out = &mut dst[di * 4..(di + row.len()) * 4];
        let pm = &pm[mi0..mi0 + row.len()];
        for ((&mv, px), &src) in row.iter().zip(out.as_chunks_mut::<4>().0).zip(pm) {
            if mv == 0 {
                continue;
            }
            let mi = mv as usize;
            let a = t.a[mi];
            match px[3] {
                _ if a == 1.0 => *px = [src[0], src[1], src[2], 255],
                0 if t.clear_ok => *px = [src[0], src[1], src[2], t.alpha0[mi]],
                255 => {
                    let (om, oa) = (t.om[mi], t.oa1[mi]);
                    for k in 0..3 {
                        px[k] = over_opaque(src[k], px[k], a, om, oa);
                    }
                    px[3] = t.alpha1[mi];
                }
                _ => over(px, src, a),
            }
        }
    });
}

/// Lowers alpha under one dab of `brush`'s mask by coverage × `opacity`
/// (0–1), with the same placement as [`stamp`]. Pixels outside the buffer
/// are clipped. `dst` is RGBA8, `width * height * 4` bytes; color channels
/// are left alone.
///
/// Byte-identical to [`crate::reference::erase_dab`]: alpha times
/// `1 - coverage * opacity`, rounded.
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
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity.is_nan() {
        return crate::reference::erase_dab(dst, width, height, brush, cx, cy, opacity);
    }
    if opacity <= 0.0 {
        return;
    }
    let m = &brush.mask;
    let Some(c) = Clip::new(m.width, m.height, width, height, cx, cy) else {
        return;
    };
    let om = coverage_table(|a| 1.0 - a, opacity);
    let f = |mv: u8, v: u8| round_u8(v as f32 * om[mv as usize]);
    let lut = byte_lut(&ERASE_LUT, (opacity.to_bits(), 0), c.pixels(), f);
    for_each_span(m, &c, width, |_, di, row| {
        let out = &mut dst[di * 4..(di + row.len()) * 4];
        match &lut {
            Some(lut) => {
                for (&mv, px) in row.iter().zip(out.as_chunks_mut::<4>().0) {
                    px[3] = lut[lut_index(mv, px[3])];
                }
            }
            None => {
                for (&mv, px) in row.iter().zip(out.as_chunks_mut::<4>().0) {
                    if mv != 0 {
                        px[3] = f(mv, px[3]);
                    }
                }
            }
        }
    });
}

/// Moves 8-bit layer-mask values under one dab of `brush`'s mask toward
/// `value` by coverage × `opacity` (0–1), with the same placement as
/// [`stamp`]. `dst` is `width * height` bytes; pixels outside are clipped.
///
/// Byte-identical to [`crate::reference::mask_dab`]:
/// `v + (value - v) * coverage * opacity`, rounded.
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
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity.is_nan() {
        return crate::reference::mask_dab(dst, width, height, brush, cx, cy, value, opacity);
    }
    if opacity <= 0.0 {
        return;
    }
    let m = &brush.mask;
    let Some(c) = Clip::new(m.width, m.height, width, height, cx, cy) else {
        return;
    };
    let a = coverage_table(|a| a, opacity);
    let target = value as f32;
    let f = |mv: u8, v: u8| {
        let v = v as f32;
        round_u8(v + (target - v) * a[mv as usize])
    };
    let lut = byte_lut(&MASK_LUT, (opacity.to_bits(), value), c.pixels(), f);
    for_each_span(m, &c, width, |_, di, row| {
        let out = &mut dst[di..di + row.len()];
        match &lut {
            Some(lut) => {
                for (&mv, d) in row.iter().zip(out.iter_mut()) {
                    *d = lut[lut_index(mv, *d)];
                }
            }
            None => {
                for (&mv, d) in row.iter().zip(out.iter_mut()) {
                    if mv != 0 {
                        *d = f(mv, *d);
                    }
                }
            }
        }
    });
}

/// `g(coverage)` per coverage byte, where coverage is `mv / 255 * opacity`
/// as the formulas compute it. Entry 0 is unused (no paint, no change).
fn coverage_table(g: impl Fn(f32) -> f32, opacity: f32) -> [f32; 256] {
    let mut t = [0f32; 256];
    for (mv, out) in t.iter_mut().enumerate() {
        *out = g(mv as f32 / 255.0 * opacity);
    }
    t
}

/// The part of a dab that lands in the buffer: mask columns `mx0..mx1` and
/// rows `my0..my1`; mask pixel (0, 0) goes to buffer pixel `(x0, y0)`.
struct Clip {
    x0: i64,
    y0: i64,
    mx0: usize,
    mx1: usize,
    my0: usize,
    my1: usize,
}

impl Clip {
    fn new(mw: u32, mh: u32, width: u32, height: u32, cx: f32, cy: f32) -> Option<Clip> {
        let x0 = cx.floor() as i64 - (mw / 2) as i64;
        let y0 = cy.floor() as i64 - (mh / 2) as i64;
        let (mx0, mx1) = ((-x0).max(0), (width as i64 - x0).min(mw as i64));
        let (my0, my1) = ((-y0).max(0), (height as i64 - y0).min(mh as i64));
        if mx0 >= mx1 || my0 >= my1 {
            return None;
        }
        Some(Clip {
            x0,
            y0,
            mx0: mx0 as usize,
            mx1: mx1 as usize,
            my0: my0 as usize,
            my1: my1 as usize,
        })
    }

    fn pixels(&self) -> usize {
        (self.mx1 - self.mx0) * (self.my1 - self.my0)
    }
}

/// Calls `f(mask index, buffer pixel index, coverage)` for each clipped mask
/// row, trimmed to its first and last nonzero coverage; empty rows are
/// skipped.
#[inline(always)]
fn for_each_span(m: &Mask, c: &Clip, width: u32, mut f: impl FnMut(usize, usize, &[u8])) {
    let mw = m.width as usize;
    for my in c.my0..c.my1 {
        let start = my * mw + c.mx0;
        let row = &m.data[start..my * mw + c.mx1];
        let Some((s, e)) = nonzero_span(row) else {
            continue;
        };
        let y = (c.y0 + my as i64) as usize;
        let x = (c.x0 + (c.mx0 + s) as i64) as usize;
        f(start + s, y * width as usize + x, &row[s..e]);
    }
}

/// `[first, last + 1)` of the nonzero bytes of `row`, scanning zeros 16
/// bytes at a time.
fn nonzero_span(row: &[u8]) -> Option<(usize, usize)> {
    const W: usize = 16;
    let word = |c: &[u8; W]| u128::from_ne_bytes(*c);
    let mut s = 0;
    for c in row.as_chunks::<W>().0 {
        if word(c) != 0 {
            break;
        }
        s += W;
    }
    s += row[s..].iter().position(|&v| v != 0)?;
    let mut e = row.len();
    for c in row[s..].as_rchunks::<W>().1.iter().rev() {
        if word(c) != 0 {
            break;
        }
        e -= W;
    }
    e = s + row[s..e].iter().rposition(|&v| v != 0).map_or(0, |i| i + 1);
    Some((s, e))
}

/// Per-coverage factors of the source-over formula for one `base`
/// (color alpha × opacity), each computed exactly as the formula does.
struct OverTables {
    /// Coverage: `m / 255 * base`.
    a: [f32; 256],
    /// `1 - a`.
    om: [f32; 256],
    /// Output alpha over an opaque pixel: `a + 1 * (1 - a)`.
    oa1: [f32; 256],
    /// Rounded output alpha byte over an opaque pixel.
    alpha1: [u8; 256],
    /// Rounded output alpha byte over a transparent pixel.
    alpha0: [u8; 256],
    /// Over a transparent pixel the formula gives `round(src * a / a)`,
    /// which is `src` whenever `src * a` is a normal float; true when every
    /// nonzero `a` is large enough for that.
    clear_ok: bool,
}

impl OverTables {
    fn new(base: f32) -> Self {
        let mut t = OverTables {
            a: [0.0; 256],
            om: [0.0; 256],
            oa1: [0.0; 256],
            alpha1: [0; 256],
            alpha0: [0; 256],
            clear_ok: base >= 1e-3,
        };
        for mv in 0..256 {
            let a = mv as f32 / 255.0 * base;
            let om = 1.0 - a;
            let oa1 = a + 1.0 * om;
            let oa0 = a + 0.0 * om;
            t.a[mv] = a;
            t.om[mv] = om;
            t.oa1[mv] = oa1;
            t.alpha1[mv] = round_u8(oa1 * 255.0);
            t.alpha0[mv] = round_u8(oa0 * 255.0);
        }
        t
    }
}

/// The general source-over step, exactly as the formula is written.
#[inline(always)]
fn over(px: &mut [u8], src: [u8; 3], a: f32) {
    let da = px[3] as f32 / 255.0;
    let out_a = a + da * (1.0 - a);
    for k in 0..3 {
        let v = (src[k] as f32 * a + px[k] as f32 * da * (1.0 - a)) / out_a;
        px[k] = round_u8(v);
    }
    px[3] = round_u8(out_a * 255.0);
}

/// One channel of [`over`] with destination alpha 1: `da` is exactly 1.0,
/// so `p * da * (1 - a)` is `p * om` and `out_a` is `oa`.
#[inline(always)]
fn over_opaque(s: u8, p: u8, a: f32, om: f32, oa: f32) -> u8 {
    let v = s as f32 * a + p as f32 * om;
    // Dividing by exactly 1.0 is the identity; skip the slow division.
    round_u8(if oa == 1.0 { v } else { v / oa })
}

/// `v.round().clamp(0.0, 255.0) as u8` for `v >= 0`, without the `roundf`
/// call: round half away from zero via truncation (`v - trunc(v)` is exact).
#[inline(always)]
fn round_u8(v: f32) -> u8 {
    let v = v.min(255.5);
    let t = v as i32;
    let r = t.wrapping_add((v - t as f32 >= 0.5) as i32);
    r.min(255) as u8
}

/// Index into a 256 × 256 table by (coverage, old byte); always < 65536.
#[inline(always)]
fn lut_index(mv: u8, v: u8) -> usize {
    u16::from_le_bytes([v, mv]) as usize
}

/// Dabs at least this big build the opaque-destination table (3 × 65536
/// evaluations with a division) if it is not cached yet.
const OPAQUE_LUT_MIN_PIXELS: usize = 1 << 14;
/// Dabs at least this big build an erase / mask table if not cached yet.
const BYTE_LUT_MIN_PIXELS: usize = 1 << 14;

/// A 256 × 256 table indexed by [`lut_index`].
type Lut = Box<[u8; 1 << 16]>;

fn new_lut() -> Lut {
    vec![0u8; 1 << 16].into_boxed_slice().try_into().unwrap()
}

/// The last table built for a kind of dab, with what it was built for.
type LutCache<K, V> = RefCell<Option<(K, Rc<V>)>>;

thread_local! {
    /// Opaque-destination R, G, B tables, keyed by (rgb, base bits).
    static OPAQUE_LUT: LutCache<(u32, u32), [Lut; 3]> = const { RefCell::new(None) };
    /// Erase, keyed by (opacity bits, unused).
    static ERASE_LUT: LutCache<(u32, u8), Lut> = const { RefCell::new(None) };
    /// Mask, keyed by (opacity bits, target value).
    static MASK_LUT: LutCache<(u32, u8), Lut> = const { RefCell::new(None) };
}

/// The table cached for `key`, if any.
fn cached<K: PartialEq + 'static, V: 'static>(
    cache: &'static LocalKey<LutCache<K, V>>,
    key: &K,
) -> Option<Rc<V>> {
    cache.with(|c| match &*c.borrow() {
        Some((k, v)) if k == key => Some(v.clone()),
        _ => None,
    })
}

/// Caches `v` for `key`, replacing the previous table.
fn store<K: 'static, V: 'static>(cache: &'static LocalKey<LutCache<K, V>>, key: K, v: V) -> Rc<V> {
    let v = Rc::new(v);
    cache.with(|c| *c.borrow_mut() = Some((key, v.clone())));
    v
}

/// The opaque-destination tables for one color and base: per channel,
/// (coverage, old byte) -> [`over_opaque`].
fn opaque_lut(src: [u8; 3], t: &OverTables) -> [Lut; 3] {
    let mut luts = [new_lut(), new_lut(), new_lut()];
    for (lut, s) in luts.iter_mut().zip(src) {
        for (mv, row) in lut.as_chunks_mut::<256>().0.iter_mut().enumerate() {
            let (a, om, oa) = (t.a[mv], t.om[mv], t.oa1[mv]);
            for (p, out) in row.iter_mut().enumerate() {
                *out = if mv == 0 {
                    p as u8
                } else {
                    over_opaque(s, p as u8, a, om, oa)
                };
            }
        }
    }
    luts
}

/// A (coverage, old byte) -> new byte table for `f`, coverage 0 meaning no
/// change: the cached one for `key`, else a new one if the dab has at
/// least [`BYTE_LUT_MIN_PIXELS`], else `None` (evaluate per pixel).
fn byte_lut(
    cache: &'static LocalKey<LutCache<(u32, u8), Lut>>,
    key: (u32, u8),
    pixels: usize,
    f: impl Fn(u8, u8) -> u8,
) -> Option<Rc<Lut>> {
    if let Some(lut) = cached(cache, &key) {
        return Some(lut);
    }
    if pixels < BYTE_LUT_MIN_PIXELS {
        return None;
    }
    let mut lut = new_lut();
    for (mv, row) in lut.as_chunks_mut::<256>().0.iter_mut().enumerate() {
        for (v, out) in row.iter_mut().enumerate() {
            *out = if mv == 0 {
                v as u8
            } else {
                f(mv as u8, v as u8)
            };
        }
    }
    Some(store(cache, key, lut))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_u8_matches_round_clamp() {
        // Every f32 from 0.25 to 300 (sampled below that), plus the top end.
        let (mid, end) = (0.25f32.to_bits(), 300f32.to_bits());
        for bits in (0..mid).step_by(61).chain(mid..=end) {
            let v = f32::from_bits(bits);
            assert_eq!(round_u8(v), v.round().clamp(0.0, 255.0) as u8, "{v}");
        }
        assert_eq!(round_u8(f32::MAX), 255);
    }

    #[test]
    fn nonzero_span_finds_ends() {
        for len in 0..70 {
            for s in 0..len {
                for e in s..len {
                    let mut row = vec![0u8; len];
                    row[s] = 1;
                    row[e] = 2;
                    assert_eq!(nonzero_span(&row), Some((s, e + 1)), "{len} {s} {e}");
                }
            }
            assert_eq!(nonzero_span(&vec![0u8; len]), None);
        }
    }
}
