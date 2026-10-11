//! Parses real `.abr` files, if present. These are third-party brushes and
//! are never bundled in the repo; the test skips (passes with a note) when
//! the folder is absent. Set `ABR_BRUSH_DIR` to override the default
//! `~/brushes`, and `RP_BRUSH_CONTACT_SHEET=<file.pgm>`
//! to write a contact sheet of every brush's thumbnail.
//!
//! Run with `cargo test -p rp-brush --test abr_real -- --nocapture` to see
//! counts, sizes and timings.

use std::path::{Path, PathBuf};
use std::time::Instant;

use rp_brush::{Mask, decode_abr, file_stem, index_abr, thumbnail};

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

/// Mean coverage of the outermost pixel ring.
fn border_mean(m: &Mask) -> f64 {
    let (w, h) = (m.width, m.height);
    let mut sum = 0u64;
    let mut n = 0u64;
    for y in 0..h {
        for x in 0..w {
            if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                sum += m.get(x, y) as u64;
                n += 1;
            }
        }
    }
    sum as f64 / n as f64
}

const CELL: u32 = 64;

#[test]
fn real_abr_files() {
    let dir = std::env::var_os("ABR_BRUSH_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("brushes"))
        });
    let Some(dir) = dir else { return };
    let mut files = Vec::new();
    find_abr(&dir, &mut files);
    if files.is_empty() {
        eprintln!("skip: no .abr files under {}", dir.display());
        return;
    }

    let mut thumbs = Vec::new();
    for path in &files {
        let t = Instant::now();
        let bytes = std::fs::read(path).unwrap();
        let t_read = t.elapsed();
        let t = Instant::now();
        let idx = index_abr(&bytes, &file_stem(path))
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let t_index = t.elapsed();
        eprintln!(
            "{}: v{}.{} {} MB, {} brushes; read {:?}, index {:?}",
            path.display(),
            idx.version,
            idx.subversion,
            bytes.len() / 1_000_000,
            idx.entries.len(),
            t_read,
            t_index
        );
        assert!(!idx.entries.is_empty());
        let mut names: Vec<_> = idx.entries.iter().map(|e| &e.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), idx.entries.len(), "names are unique");

        let mut t_decode = std::time::Duration::ZERO;
        let mut t_thumb = std::time::Duration::ZERO;
        let mut slowest = (std::time::Duration::ZERO, String::new());
        let mut paint_border = 0;
        for e in &idx.entries {
            let t1 = Instant::now();
            let b = decode_abr(&bytes, e).unwrap_or_else(|err| panic!("{}: {err}", e.name));
            let dt = t1.elapsed();
            t_decode += dt;
            if dt > slowest.0 {
                slowest = (dt, e.name.clone());
            }
            let mean = b.mask.data.iter().map(|&v| v as u64).sum::<u64>() as f64
                / b.mask.data.len() as f64;
            let border = border_mean(&b.mask);
            eprintln!(
                "  {:<40} {:>4}x{:<4} d{} {} spacing {:>5.1}%  max {:>3} mean {:>5.1} border {:>5.1}",
                e.name,
                e.width,
                e.height,
                e.depth,
                if e.compressed { "rle" } else { "raw" },
                e.spacing,
                b.mask.data.iter().max().unwrap(),
                mean,
                border
            );
            assert!(b.mask.data.iter().any(|&v| v > 0), "{} is empty", e.name);
            // Coverage convention: the background around a splatter is no
            // paint, so the border is (nearly) zero and the mean is low.
            if border > mean {
                paint_border += 1;
            }
            let t1 = Instant::now();
            thumbs.push(thumbnail(&b.mask, CELL));
            t_thumb += t1.elapsed();
        }
        eprintln!(
            "  decode all {:?} (slowest {:?}: {}), thumbnails {:?}",
            t_decode, slowest.0, slowest.1, t_thumb
        );
        assert!(
            paint_border * 10 < idx.entries.len(),
            "{paint_border} brushes look inverted"
        );
    }

    if let Some(out) = std::env::var_os("RP_BRUSH_CONTACT_SHEET") {
        let cols = 12u32;
        let rows = (thumbs.len() as u32).div_ceil(cols);
        let (w, h) = (cols * (CELL + 4), rows * (CELL + 4));
        // White paper, black paint: how the brush would print.
        let mut img = vec![255u8; (w * h) as usize];
        for (i, t) in thumbs.iter().enumerate() {
            let (cx, cy) = (
                (i as u32 % cols) * (CELL + 4) + 2,
                (i as u32 / cols) * (CELL + 4) + 2,
            );
            let (ox, oy) = (cx + (CELL - t.width) / 2, cy + (CELL - t.height) / 2);
            for y in 0..t.height {
                for x in 0..t.width {
                    img[((oy + y) * w + ox + x) as usize] = 255 - t.get(x, y);
                }
            }
        }
        let mut pgm = format!("P5\n{w} {h}\n255\n").into_bytes();
        pgm.extend(img);
        std::fs::write(&out, pgm).unwrap();
        eprintln!("contact sheet: {}", PathBuf::from(out).display());
    }
}
