//! Before/after dab timings: `rp_brush::reference` (the plain float
//! formulas) against the fast `stamp` / `erase_dab` / `mask_dab`, for round
//! brushes and a real `.abr` splatter (if present; see `abr_real.rs`).
//! Also checks the outputs are identical.
//!
//! `cargo test -p rp-brush --release --test stamp_timing -- --ignored --nocapture`

use std::path::{Path, PathBuf};
use std::time::Instant;

use rp_brush::{Brush, decode_abr, erase_dab, file_stem, index_abr, mask_dab, reference, stamp};

fn find_abr(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    items.sort();
    for p in items {
        if p.is_dir() {
            find_abr(&p, out);
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("abr")) {
            out.push(p);
        }
    }
}

/// The densest brush (highest mean coverage) of the first `.abr` found:
/// the worst case for a splatter.
fn real_splatter() -> Option<Brush> {
    let dir = std::env::var_os("ABR_BRUSH_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("brushes")))?;
    let mut files = Vec::new();
    find_abr(&dir, &mut files);
    let path = files.first()?;
    let bytes = std::fs::read(path).ok()?;
    let idx = index_abr(&bytes, &file_stem(path)).ok()?;
    let mut best: Option<(f64, Brush)> = None;
    for e in &idx.entries {
        let b = decode_abr(&bytes, e).ok()?;
        let mean = b.mask.data.iter().map(|&v| v as f64).sum::<f64>() / b.mask.data.len() as f64;
        if best.as_ref().is_none_or(|(m, _)| mean > *m) {
            best = Some((mean, b));
        }
    }
    let (mean, b) = best?;
    eprintln!(
        "splatter: {} / {} ({}x{}, mean coverage {mean:.1})",
        path.display(),
        b.name,
        b.mask.width,
        b.mask.height
    );
    Some(b)
}

/// Best time in microseconds of `f` over a few runs, minus the best time to
/// reset a layer of `reset_bytes` (each run starts from a fresh copy).
fn time_us(reset_bytes: usize, f: impl FnMut()) -> f64 {
    let src = vec![1u8; reset_bytes];
    let mut dst = vec![0u8; reset_bytes];
    let reset = best(|| dst.copy_from_slice(std::hint::black_box(&src)));
    (best(f) - reset).max(0.0)
}

fn best(mut f: impl FnMut()) -> f64 {
    (0..15)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1e6
        })
        .fold(f64::INFINITY, f64::min)
}

#[test]
#[ignore = "timing report; run with --ignored --nocapture"]
fn dab_timings() {
    let mut brushes: Vec<(String, Brush)> = Vec::new();
    let splatter = real_splatter();
    if splatter.is_none() {
        eprintln!("no .abr found; round brushes only");
    }
    for d in [64.0, 256.0, 1024.0] {
        brushes.push((format!("round hard {d}"), Brush::round_hard(d)));
        brushes.push((format!("round soft {d}"), Brush::round_soft(d)));
        if let Some(s) = &splatter {
            brushes.push((format!("abr splatter {d}"), s.resized(d)));
        }
    }
    // Pure row-scan cost: a dab with no coverage at all.
    brushes.push((
        "empty 1024".into(),
        Brush::bitmap(
            "empty",
            10.0,
            rp_brush::Mask::new(1023, 1023, vec![0; 1023 * 1023]),
            None,
        ),
    ));
    eprintln!(
        "{:<20} {:>22} {:>22} {:>22} {:>22}   first dab (opaque / erase / mask)",
        "brush", "stamp clear layer", "stamp opaque layer", "erase", "mask",
    );
    for (name, b) in &brushes {
        // A layer just big enough for the dab, so resets stay cheap.
        let (w, h) = (b.mask.width + 2, b.mask.height + 2);
        let (cx, cy) = ((w / 2) as f32, (h / 2) as f32);
        let clear = vec![0u8; (w * h * 4) as usize];
        let mut opaque = vec![0u8; (w * h * 4) as usize];
        for (i, px) in opaque.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            *px = [(i % 251) as u8, (i % 13) as u8 * 19, 90, 255];
        }
        let gray = vec![200u8; (w * h) as usize];
        let color = [200, 30, 40, 255];
        let mut cells = Vec::new();
        for (layer, label) in [(&clear, "clear"), (&opaque, "opaque")] {
            let (mut a, mut b2) = (layer.clone(), layer.clone());
            let before = time_us(layer.len(), || {
                a.copy_from_slice(layer);
                reference::stamp(&mut a, w, h, b, cx, cy, color, 1.0);
            });
            let after = time_us(layer.len(), || {
                b2.copy_from_slice(layer);
                stamp(&mut b2, w, h, b, cx, cy, color, 1.0);
            });
            assert!(a == b2, "{name} stamp {label}");
            cells.push((before, after));
        }
        {
            let (mut a, mut b2) = (opaque.clone(), opaque.clone());
            let before = time_us(opaque.len(), || {
                a.copy_from_slice(&opaque);
                reference::erase_dab(&mut a, w, h, b, cx, cy, 1.0);
            });
            let after = time_us(opaque.len(), || {
                b2.copy_from_slice(&opaque);
                erase_dab(&mut b2, w, h, b, cx, cy, 1.0);
            });
            assert!(a == b2, "{name} erase");
            cells.push((before, after));
        }
        {
            let (mut a, mut b2) = (gray.clone(), gray.clone());
            let before = time_us(gray.len(), || {
                a.copy_from_slice(&gray);
                reference::mask_dab(&mut a, w, h, b, cx, cy, 0, 1.0);
            });
            let after = time_us(gray.len(), || {
                b2.copy_from_slice(&gray);
                mask_dab(&mut b2, w, h, b, cx, cy, 0, 1.0);
            });
            assert!(a == b2, "{name} mask");
            cells.push((before, after));
        }
        // First dab of a stroke: a fresh thread has no cached tables.
        let cold = std::thread::scope(|s| {
            s.spawn(|| {
                let mut x = opaque.clone();
                let t = Instant::now();
                stamp(&mut x, w, h, b, cx, cy, color, 1.0);
                let st = t.elapsed().as_secs_f64() * 1e6;
                let t = Instant::now();
                erase_dab(&mut x, w, h, b, cx, cy, 1.0);
                let er = t.elapsed().as_secs_f64() * 1e6;
                let mut g = gray.clone();
                let t = Instant::now();
                mask_dab(&mut g, w, h, b, cx, cy, 0, 1.0);
                let ma = t.elapsed().as_secs_f64() * 1e6;
                (st, er, ma)
            })
            .join()
            .unwrap()
        });
        let fmt = |(b, a): (f64, f64)| format!("{:>8.0} -> {:>6.0} us", b, a);
        eprintln!(
            "{:<20} {:>22} {:>22} {:>22} {:>22}   cold {:>5.0} / {:>4.0} / {:>4.0} us",
            name,
            fmt(cells[0]),
            fmt(cells[1]),
            fmt(cells[2]),
            fmt(cells[3]),
            cold.0,
            cold.1,
            cold.2
        );
    }
}
