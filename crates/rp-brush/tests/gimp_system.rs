//! Parses real GIMP brushes from the system install, if present.
//! Set `GIMP_BRUSH_DIR` to override the default `/usr/share/gimp/3.0/brushes`.
//! The tests skip (pass with a note) when the files are absent; GIMP data is
//! not bundled in the repo.

use std::path::PathBuf;

fn gimp_file(rel: &str) -> Option<Vec<u8>> {
    let dir = std::env::var_os("GIMP_BRUSH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/share/gimp/3.0/brushes"));
    let path = dir.join(rel);
    match std::fs::read(&path) {
        Ok(d) => Some(d),
        Err(_) => {
            eprintln!("skip: {} not found", path.display());
            None
        }
    }
}

#[test]
fn gimp_pixel_gbr() {
    let Some(d) = gimp_file("Basic/pixel.gbr") else {
        return;
    };
    let b = rp_brush::parse_gbr(&d).unwrap();
    assert_eq!((b.mask.width, b.mask.height), (1, 1));
    assert_eq!(b.mask.data, vec![255]);
}

#[test]
fn gimp_confetti_gbr() {
    let Some(d) = gimp_file("Legacy/confetti.gbr") else {
        return;
    };
    let b = rp_brush::parse_gbr(&d).unwrap();
    assert_eq!(b.name, "Confetti");
    assert_eq!((b.mask.width, b.mask.height), (27, 28));
    assert!(b.mask.data.iter().any(|&v| v > 0));
}

#[test]
fn gimp_basic_vbrs() {
    for name in [
        "Star.vbr",
        "Hardness-050.vbr",
        "1-pixel.vbr",
        "Block-01.vbr",
    ] {
        let Some(d) = gimp_file(&format!("Basic/{name}")) else {
            continue;
        };
        let b = rp_brush::load(&d).unwrap_or_else(|e| panic!("{name}: {e}"));
        let m = &b.mask;
        assert_eq!(m.get(m.width / 2, m.height / 2), 255, "{name}");
    }
    if let Some(d) = gimp_file("Basic/1-pixel.vbr") {
        let b = rp_brush::load(&d).unwrap();
        assert_eq!((b.mask.width, b.mask.height), (1, 1));
    }
}
