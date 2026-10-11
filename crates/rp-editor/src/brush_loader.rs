//! The brush library's file work, meant for a background thread: scanning
//! added roots for brush files, indexing them, making thumbnails, and
//! decoding a selected brush into its working base. Pure apart from file
//! reads; results go to [`crate::brush_library`] as [`LoadEvent`]s.
//!
//! - A root is a brush file or a folder, scanned recursively (sorted by
//!   path, symlinked folders not followed, at most [`MAX_DEPTH`] deep);
//!   only `.abr`, `.gbr`, `.gih` and `.vbr` files count, so zips and
//!   everything else are skipped. Files are deduped by canonical path.
//! - `.abr` files are indexed without decoding (well under a millisecond),
//!   so their entries appear at once; thumbnails follow in small batches,
//!   decoded from the file bytes read once for the whole file. Selecting
//!   an `.abr` brush later re-reads only its pixel range from disk.
//! - Other formats are small and kept decoded.

use std::collections::HashSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rp_brush::{Brush, BrushError, Mask, decode_abr, decode_abr_data, index_abr, thumbnail};

use crate::brush_library::{
    BRUSH_EXTENSIONS, BRUSH_THUMB_SIDE, BrushData, IndexedBrush, LoadEvent,
};
use crate::paint_ops::working_base;

/// Deepest folder level a scan descends into.
pub const MAX_DEPTH: usize = 16;
/// Thumbnails per [`LoadEvent::Thumbs`] (keeps UI updates few but steady).
pub const THUMB_BATCH: usize = 8;

/// Whether `path` has a brush file extension (any case).
pub fn is_brush_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| BRUSH_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut items: Vec<_> = rd.filter_map(Result::ok).collect();
    items.sort_by_key(|e| e.file_name());
    for e in items {
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() {
            walk(&p, depth + 1, out);
        } else if is_brush_file(&p) && p.is_file() {
            out.push(p);
        }
    }
}

/// The brush files under `roots`, canonical, deduped, in root order (a
/// folder's files sorted by path), minus `known`. Also returns the roots
/// that do not exist.
pub fn scan_roots(roots: &[PathBuf], known: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut seen: HashSet<PathBuf> = known.iter().cloned().collect();
    let (mut files, mut missing) = (Vec::new(), Vec::new());
    for root in roots {
        let mut found = Vec::new();
        if root.is_dir() {
            walk(root, 0, &mut found);
        } else if root.is_file() {
            if is_brush_file(root) {
                found.push(root.clone());
            }
        } else {
            missing.push(root.clone());
        }
        for f in found {
            let f = fs::canonicalize(&f).unwrap_or(f);
            if seen.insert(f.clone()) {
                files.push(f);
            }
        }
    }
    (files, missing)
}

fn ext_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Indexes the brush file `path` whose contents are `bytes`.
pub fn index_file(path: &Path, bytes: &[u8]) -> Result<Vec<IndexedBrush>, BrushError> {
    let stem = rp_brush::file_stem(path);
    if ext_of(path) == "abr" {
        let index = index_abr(bytes, &stem)?;
        return Ok(index
            .entries
            .into_iter()
            .map(|e| IndexedBrush {
                name: e.name.clone(),
                width: e.width,
                height: e.height,
                data: BrushData::Abr {
                    file: path.to_path_buf(),
                    entry: Arc::new(e),
                },
            })
            .collect());
    }
    let brushes = rp_brush::load_library_bytes(bytes, &ext_of(path), &stem)?;
    Ok(brushes
        .into_iter()
        .map(|b| {
            let d = b.diameter().round().max(1.0) as u32;
            let (width, height) = match b.source {
                rp_brush::BrushSource::Parametric(_) => (d, d),
                _ => (b.mask.width, b.mask.height),
            };
            IndexedBrush {
                name: b.name.clone(),
                width,
                height,
                data: BrushData::Decoded(Arc::new(b)),
            }
        })
        .collect())
}

/// Thumbnail of an indexed brush; `bytes` is its file (for `.abr`).
pub fn thumbnail_of(brush: &IndexedBrush, bytes: &[u8]) -> Result<Mask, BrushError> {
    Ok(match &brush.data {
        BrushData::Builtin(tip) => thumbnail(&tip.make(BRUSH_THUMB_SIDE).mask, BRUSH_THUMB_SIDE),
        BrushData::Decoded(b) => thumbnail(&b.mask, BRUSH_THUMB_SIDE),
        BrushData::Abr { entry, .. } => {
            thumbnail(&decode_abr(bytes, entry)?.mask, BRUSH_THUMB_SIDE)
        }
    })
}

/// Scans `roots` (skipping `known` files) and loads every brush file found,
/// reporting through `send` in order: per file `Indexed` then `Thumbs`
/// batches (or `Failed`), then one `Finished`. Stops early when `send`
/// returns false (nobody is listening any more). Blocking: run it on a
/// background thread.
pub fn run_load(roots: &[PathBuf], known: &[PathBuf], mut send: impl FnMut(LoadEvent) -> bool) {
    let (files, missing) = scan_roots(roots, known);
    let (mut n_files, mut n_brushes) = (0, 0);
    for file in files {
        let loaded = fs::read(&file)
            .map_err(|e| BrushError::Io(e.to_string()))
            .and_then(|bytes| index_file(&file, &bytes).map(|b| (bytes, b)));
        let (bytes, brushes) = match loaded {
            Ok(v) => v,
            Err(e) => {
                if !send(LoadEvent::Failed {
                    file,
                    error: e.to_string(),
                }) {
                    return;
                }
                continue;
            }
        };
        n_files += 1;
        n_brushes += brushes.len();
        let thumbs_for = brushes.clone();
        if !send(LoadEvent::Indexed {
            file: file.clone(),
            brushes,
        }) {
            return;
        }
        let mut batch = Vec::new();
        for (i, b) in thumbs_for.iter().enumerate() {
            // A brush whose pixels fail to decode keeps a blank cell; picking
            // it reports the error.
            if let Ok(t) = thumbnail_of(b, &bytes) {
                batch.push((i, t));
            }
            if batch.len() >= THUMB_BATCH || i + 1 == thumbs_for.len() {
                let thumbs = std::mem::take(&mut batch);
                if !thumbs.is_empty()
                    && !send(LoadEvent::Thumbs {
                        file: file.clone(),
                        thumbs,
                    })
                {
                    return;
                }
            }
        }
    }
    send(LoadEvent::Finished {
        files: n_files,
        brushes: n_brushes,
        missing,
    });
}

/// Reads `range` of `file` from disk.
fn read_range(file: &Path, range: std::ops::Range<usize>) -> std::io::Result<Vec<u8>> {
    let mut f = fs::File::open(file)?;
    f.seek(SeekFrom::Start(range.start as u64))?;
    let mut buf = vec![0; range.len()];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

/// Decodes a library brush at full size (`.abr`: only its pixel range is
/// read from disk).
pub fn decode_brush(data: &BrushData) -> Result<Brush, String> {
    match data {
        BrushData::Builtin(tip) => Ok(tip.make(BRUSH_THUMB_SIDE)),
        BrushData::Decoded(b) => Ok((**b).clone()),
        BrushData::Abr { file, entry } => {
            let bytes = read_range(file, entry.data.clone())
                .map_err(|e| format!("{}: {e}", file.display()))?;
            decode_abr_data(&bytes, entry).map_err(|e| e.to_string())
        }
    }
}

/// Decodes a library brush and makes its working base (see
/// [`working_base`]). Tens of ms for a 5000 px sample: background only.
pub fn prepare_brush(data: &BrushData) -> Result<Brush, String> {
    decode_brush(data).map(working_base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush_library::{BrushKey, BrushLibrary};

    /// A fresh empty directory under the target dir.
    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rp-brush-loader-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A GIMP .gbr v2 grayscale brush.
    fn gbr(name: &str, w: u32, h: u32, fill: u8) -> Vec<u8> {
        let mut v = Vec::new();
        let header_len = 28 + name.len() as u32 + 1;
        for x in [header_len, 2, w, h, 1] {
            v.extend_from_slice(&x.to_be_bytes());
        }
        v.extend_from_slice(b"GIMP");
        v.extend_from_slice(&20u32.to_be_bytes());
        v.extend_from_slice(name.as_bytes());
        v.push(0);
        v.extend(std::iter::repeat_n(fill, (w * h) as usize));
        v
    }

    /// A minimal v1 .abr with one raw 8-bit sampled brush.
    fn abr_v1(w: u32, h: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes()); // misc
        body.extend_from_slice(&25u16.to_be_bytes()); // spacing
        body.push(0); // antialias
        for v in [0i16, 0, 0, 0] {
            body.extend_from_slice(&v.to_be_bytes()); // short bounds
        }
        for v in [0i32, 0, h as i32, w as i32] {
            body.extend_from_slice(&v.to_be_bytes()); // top left bottom right
        }
        body.extend_from_slice(&8u16.to_be_bytes()); // depth
        body.push(0); // raw
        body.extend(std::iter::repeat_n(200u8, (w * h) as usize));
        let mut v = Vec::new();
        v.extend_from_slice(&1u16.to_be_bytes()); // version
        v.extend_from_slice(&1u16.to_be_bytes()); // count
        v.extend_from_slice(&2u16.to_be_bytes()); // type: sampled
        v.extend_from_slice(&(body.len() as u32).to_be_bytes());
        v.extend(body);
        v
    }

    #[test]
    fn scan_is_recursive_sorted_deduped_and_skips_zips() {
        let d = temp_dir("scan");
        fs::create_dir_all(d.join("sub/deeper")).unwrap();
        fs::write(d.join("b.gbr"), gbr("b", 2, 2, 255)).unwrap();
        fs::write(d.join("a.GBR"), gbr("a", 2, 2, 255)).unwrap();
        fs::write(d.join("pack.zip"), b"PK").unwrap();
        fs::write(d.join("notes.txt"), b"x").unwrap();
        fs::write(d.join("sub/deeper/c.vbr"), b"x").unwrap();
        let known = vec![fs::canonicalize(d.join("b.gbr")).unwrap()];
        let roots = vec![d.clone(), d.join("a.GBR"), d.join("gone")];
        let (files, missing) = scan_roots(&roots, &known);
        let names: Vec<_> = files
            .iter()
            .map(|f| {
                f.strip_prefix(fs::canonicalize(&d).unwrap())
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            names,
            [PathBuf::from("a.GBR"), PathBuf::from("sub/deeper/c.vbr")]
        );
        assert_eq!(missing, [d.join("gone")]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn load_runs_on_another_thread_and_feeds_the_library_in_order() {
        let d = temp_dir("load");
        fs::write(d.join("1-small.gbr"), gbr("Dot", 3, 5, 255)).unwrap();
        fs::write(d.join("2-pack.abr"), abr_v1(40, 30)).unwrap();
        fs::write(d.join("3-bad.gbr"), b"not a brush").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let roots = vec![d.clone()];
        let worker = std::thread::spawn(move || {
            run_load(&roots, &[], |ev| tx.send(ev).is_ok());
        });
        let mut lib = BrushLibrary::new();
        let mut kinds = Vec::new();
        let mut last = None;
        for ev in rx {
            kinds.push(match &ev {
                LoadEvent::Indexed { .. } => "indexed",
                LoadEvent::Thumbs { .. } => "thumbs",
                LoadEvent::Failed { .. } => "failed",
                LoadEvent::Finished { .. } => "finished",
            });
            last = lib.apply(ev).or(last);
        }
        worker.join().unwrap();
        assert_eq!(
            kinds,
            [
                "indexed", "thumbs", "indexed", "thumbs", "failed", "finished"
            ]
        );
        let last = last.unwrap();
        assert!(last.contains("2 brushes from 2 files"), "{last}");
        let names: Vec<_> = lib
            .entries()
            .iter()
            .map(|e| e.brush.name.as_str())
            .collect();
        assert_eq!(names, ["Round Hard", "Round Soft", "Dot", "2-pack 1"]);
        assert!(lib.entries().iter().all(|e| e.thumb.is_some()));
        let abr = &lib.entries()[3];
        assert_eq!((abr.brush.width, abr.brush.height), (40, 30));
        assert!(matches!(abr.brush.data, BrushData::Abr { .. }));

        // Selecting decodes just the pixel range from disk, off-thread.
        let BrushKey::File { .. } = &abr.key else {
            panic!()
        };
        let data = abr.brush.data.clone();
        let b = std::thread::spawn(move || prepare_brush(&data))
            .join()
            .unwrap()
            .unwrap();
        assert_eq!((b.mask.width, b.mask.height), (40, 30));
        assert!(b.mask.data.iter().all(|&v| v == 200));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn loader_stops_when_nobody_listens() {
        let d = temp_dir("stop");
        fs::write(d.join("a.gbr"), gbr("a", 2, 2, 255)).unwrap();
        fs::write(d.join("b.gbr"), gbr("b", 2, 2, 255)).unwrap();
        let mut n = 0;
        run_load(std::slice::from_ref(&d), &[], |_| {
            n += 1;
            false
        });
        assert_eq!(n, 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn working_base_caps_big_samples_at_1024_as_a_plain_bitmap() {
        let big = Brush::bitmap(
            "big",
            10.0,
            Mask::new(3000, 1500, vec![255; 3000 * 1500]),
            Some(vec![[1, 2, 3]; 3000 * 1500]),
        );
        let w = working_base(big);
        assert_eq!((w.mask.width, w.mask.height), (1024, 512));
        assert!(w.pixmap.is_none(), "stamps fg/bg through the mask");
        assert!(matches!(w.source, rp_brush::BrushSource::Bitmap));
        assert_eq!(w.spacing, 10.0);
        let small = working_base(Brush::bitmap("s", 5.0, Mask::new(3, 2, vec![1; 6]), None));
        assert_eq!((small.mask.width, small.mask.height), (3, 2));
    }
}
