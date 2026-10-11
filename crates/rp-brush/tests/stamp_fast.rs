//! The fast dab functions must give exactly the bytes of the plain per-pixel
//! float formulas kept in `rp_brush::reference`.

use rp_brush::{Brush, Mask, erase_dab, mask_dab, reference, stamp};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn byte(&mut self) -> u8 {
        (self.next() >> 24) as u8
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.byte()).collect()
    }
    /// Bytes biased to 0 and 255, like real brushes and layers.
    fn edgy(&mut self, n: usize) -> Vec<u8> {
        (0..n)
            .map(|_| match self.next() % 4 {
                0 => 0,
                1 => 255,
                _ => self.byte(),
            })
            .collect()
    }
}

fn gray(w: u32, h: u32, data: Vec<u8>) -> Brush {
    Brush::bitmap("g", 25.0, Mask::new(w, h, data), None)
}

/// Every coverage value 0..=255 (columns) over every destination alpha
/// (rows); each pass sets the color channels to a different value, so all
/// (coverage, channel, alpha) triples are covered.
#[test]
fn stamp_matches_reference_exhaustively() {
    let brush = gray(256, 256, (0..256 * 256).map(|i| (i % 256) as u8).collect());
    let colors = [[255, 0, 128, 255], [7, 200, 33, 128], [90, 91, 92, 1]];
    let opacities = [1.0, 0.5, 0.3, 1e-4, 0.0];
    for color in colors {
        for &op in &opacities {
            for pass in 0..256u32 {
                let mut dst = Vec::with_capacity(256 * 256 * 4);
                for y in 0..256u32 {
                    for x in 0..256u32 {
                        let c = (pass + x * 7) as u8;
                        dst.extend_from_slice(&[
                            c,
                            c.wrapping_add(85),
                            c.wrapping_add(170),
                            y as u8,
                        ]);
                    }
                }
                let mut want = dst.clone();
                reference::stamp(&mut want, 256, 256, &brush, 128.0, 128.0, color, op);
                stamp(&mut dst, 256, 256, &brush, 128.0, 128.0, color, op);
                assert!(dst == want, "color {color:?} opacity {op} pass {pass}");
            }
        }
    }
}

#[test]
fn stamp_matches_reference_random_placements() {
    let mut rng = Rng(0x9e3779b97f4a7c15);
    for case in 0..400 {
        let (mw, mh) = (1 + rng.next() as u32 % 40, 1 + rng.next() as u32 % 40);
        let (w, h) = (1 + rng.next() as u32 % 50, 1 + rng.next() as u32 % 50);
        let mut brush = gray(mw, mh, rng.edgy((mw * mh) as usize));
        if case % 3 == 0 {
            let pm = (0..mw * mh)
                .map(|_| [rng.byte(), rng.byte(), rng.byte()])
                .collect();
            brush.pixmap = Some(pm);
        }
        let cx = (rng.next() % 1000) as f32 / 10.0 - 30.0;
        let cy = (rng.next() % 1000) as f32 / 10.0 - 30.0;
        let color = [rng.byte(), rng.byte(), rng.byte(), rng.edgy(1)[0]];
        let op = [1.0, 0.75, 0.01, 2.0, -1.0, f32::NAN][case % 6];
        let dst0 = rng.edgy((w * h * 4) as usize);
        let mut want = dst0.clone();
        reference::stamp(&mut want, w, h, &brush, cx, cy, color, op);
        let mut got = dst0;
        stamp(&mut got, w, h, &brush, cx, cy, color, op);
        assert!(got == want, "case {case}");
    }
}

/// Pixmap brushes: every coverage over every destination alpha, random
/// source and destination colors.
#[test]
fn stamp_pixmap_matches_reference() {
    let mut rng = Rng(42);
    let mut brush = gray(256, 256, (0..256 * 256).map(|i| (i % 256) as u8).collect());
    for &op in &[1.0, 0.6] {
        for _ in 0..16 {
            brush.pixmap = Some(
                (0..256 * 256)
                    .map(|_| [rng.byte(), rng.byte(), rng.byte()])
                    .collect(),
            );
            let mut dst = rng.bytes(256 * 256 * 4);
            for (i, px) in dst.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                px[3] = (i / 256) as u8;
            }
            let mut want = dst.clone();
            reference::stamp(
                &mut want,
                256,
                256,
                &brush,
                128.0,
                128.0,
                [0, 0, 0, 255],
                op,
            );
            stamp(&mut dst, 256, 256, &brush, 128.0, 128.0, [0, 0, 0, 255], op);
            assert!(dst == want, "opacity {op}");
        }
    }
}

/// Erase and mask dabs: every (coverage, old value) pair, on a small dab
/// (evaluated per pixel) and a big one (table).
#[test]
fn erase_and_mask_match_reference_exhaustively() {
    let opacities = [1.0, 0.5, 0.33, 1e-4, 0.0, -1.0, 7.0, f32::NAN];
    for (w, reps) in [(256u32, 1u32), (16, 1)] {
        // A w x (256 * 256 / w) mask: all coverages per value row.
        let h = 256 * 256 / w * reps;
        let brush = gray(
            w,
            h,
            (0..w * h)
                .map(|i| ((i % (256 * 256)) / 256) as u8)
                .collect(),
        );
        let vals: Vec<u8> = (0..w * h).map(|i| (i % 256) as u8).collect();
        let (cx, cy) = ((w / 2) as f32, (h / 2) as f32);
        for &op in &opacities {
            let mut rgba: Vec<u8> = vals.iter().flat_map(|&v| [1, 2, 3, v]).collect();
            let mut want = rgba.clone();
            reference::erase_dab(&mut want, w, h, &brush, cx, cy, op);
            erase_dab(&mut rgba, w, h, &brush, cx, cy, op);
            assert!(rgba == want, "erase w {w} opacity {op}");
            for value in [0u8, 255, 100] {
                let mut m = vals.clone();
                let mut want = m.clone();
                reference::mask_dab(&mut want, w, h, &brush, cx, cy, value, op);
                mask_dab(&mut m, w, h, &brush, cx, cy, value, op);
                assert!(m == want, "mask w {w} value {value} opacity {op}");
            }
        }
    }
}

#[test]
fn erase_and_mask_match_reference_random_placements() {
    let mut rng = Rng(7);
    for case in 0..400 {
        let (mw, mh) = (1 + rng.next() as u32 % 40, 1 + rng.next() as u32 % 40);
        let (w, h) = (1 + rng.next() as u32 % 50, 1 + rng.next() as u32 % 50);
        let brush = gray(mw, mh, rng.edgy((mw * mh) as usize));
        let cx = (rng.next() % 1000) as f32 / 10.0 - 30.0;
        let cy = (rng.next() % 1000) as f32 / 10.0 - 30.0;
        let op = [1.0, 0.75, 0.01][case % 3];
        let rgba0 = rng.edgy((w * h * 4) as usize);
        let (mut got, mut want) = (rgba0.clone(), rgba0);
        reference::erase_dab(&mut want, w, h, &brush, cx, cy, op);
        erase_dab(&mut got, w, h, &brush, cx, cy, op);
        assert!(got == want, "erase case {case}");
        let m0 = rng.edgy((w * h) as usize);
        let value = rng.edgy(1)[0];
        let (mut got, mut want) = (m0.clone(), m0);
        reference::mask_dab(&mut want, w, h, &brush, cx, cy, value, op);
        mask_dab(&mut got, w, h, &brush, cx, cy, value, op);
        assert!(got == want, "mask case {case}");
    }
}

/// The built-in round brushes at painting sizes, over a mixed layer.
#[test]
fn round_brushes_match_reference() {
    let mut rng = Rng(99);
    let (w, h) = (700u32, 600u32);
    let mut layer = rng.edgy((w * h * 4) as usize);
    // Opaque and clear regions, like a real paint layer.
    for (i, px) in layer.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        match (i % w as usize) * 3 / w as usize {
            0 => px[3] = 0,
            1 => px[3] = 255,
            _ => {}
        }
    }
    let mut mask_layer = rng.edgy((w * h) as usize);
    for d in [1.0, 5.0, 64.0, 255.0, 640.0] {
        for b in [Brush::round_hard(d), Brush::round_soft(d)] {
            for (cx, cy) in [(350.5, 300.2), (3.0, 590.0), (-100.0, -50.0)] {
                let mut want = layer.clone();
                reference::stamp(&mut want, w, h, &b, cx, cy, [10, 200, 30, 255], 1.0);
                stamp(&mut layer, w, h, &b, cx, cy, [10, 200, 30, 255], 1.0);
                assert!(layer == want, "stamp {} {d}", b.name);
                reference::erase_dab(&mut want, w, h, &b, cx, cy, 1.0);
                erase_dab(&mut layer, w, h, &b, cx, cy, 1.0);
                assert!(layer == want, "erase {} {d}", b.name);
                let mut want = mask_layer.clone();
                reference::mask_dab(&mut want, w, h, &b, cx, cy, 0, 1.0);
                mask_dab(&mut mask_layer, w, h, &b, cx, cy, 0, 1.0);
                assert!(mask_layer == want, "mask {} {d}", b.name);
            }
        }
    }
}
