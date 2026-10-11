//! Loading every brush in a brush file, whatever its format.

use std::path::Path;

use crate::{Brush, BrushError, load, parse_abr, parse_gbr};

/// A GIMP `.gih` image hose: a name line, a parameter line, then the cells
/// as concatenated `.gbr` brushes. Only the first cell is returned, named
/// after the hose.
pub fn parse_gih(bytes: &[u8]) -> Result<Brush, BrushError> {
    let mut lines = bytes.splitn(3, |&b| b == b'\n');
    let name = lines.next().ok_or(BrushError::Truncated)?;
    let _params = lines.next().ok_or(BrushError::Truncated)?;
    let rest = lines.next().ok_or(BrushError::Truncated)?;
    let mut b = parse_gbr(rest)?;
    let name = String::from_utf8_lossy(name).trim().to_string();
    if !name.is_empty() {
        b.name = name;
    }
    Ok(b)
}

/// Every brush in `bytes`, chosen by the file extension `ext` (`gbr`, `vbr`,
/// `gih` or `abr`, any case). `stem` names unnamed `.abr` brushes.
pub fn load_library_bytes(bytes: &[u8], ext: &str, stem: &str) -> Result<Vec<Brush>, BrushError> {
    match ext.to_ascii_lowercase().as_str() {
        "gbr" | "vbr" => load(bytes).map(|b| vec![b]),
        "gih" => parse_gih(bytes).map(|b| vec![b]),
        "abr" => parse_abr(bytes, stem),
        e => Err(BrushError::Unsupported(format!("brush file type {e:?}"))),
    }
}

/// Read a brush file and return every brush in it (see [`load_library_bytes`]).
pub fn load_library(path: &Path) -> Result<Vec<Brush>, BrushError> {
    let bytes =
        std::fs::read(path).map_err(|e| BrushError::Io(format!("{}: {e}", path.display())))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    load_library_bytes(&bytes, ext, &file_stem(path))
}

/// The file name without its extension, used to name unnamed brushes.
pub fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Brush".into())
}
