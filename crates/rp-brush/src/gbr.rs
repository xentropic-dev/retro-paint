//! GIMP `.gbr` bitmap brushes (versions 1 and 2, 1 or 4 bytes per pixel).
//!
//! Layout, all integers big-endian u32:
//! `header_size, version, width, height, bytes` then, for version 2 only,
//! `magic "GIMP", spacing`; then a NUL-terminated name filling the rest of the
//! header, then `width * height * bytes` pixel bytes. Version 1 has no spacing;
//! GIMP uses 25% for it.

use crate::{Brush, BrushError, Mask};

const MAX_DIM: u32 = 10_000;

fn be_u32(d: &[u8], off: usize) -> Result<u32, BrushError> {
    d.get(off..off + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(BrushError::Truncated)
}

pub fn parse_gbr(data: &[u8]) -> Result<Brush, BrushError> {
    let header_size = be_u32(data, 0)? as usize;
    let version = be_u32(data, 4)?;
    let width = be_u32(data, 8)?;
    let height = be_u32(data, 12)?;
    let bytes = be_u32(data, 16)?;

    let (fixed, spacing) = match version {
        1 => (20, 25.0),
        2 => {
            let magic = data.get(20..24).ok_or(BrushError::Truncated)?;
            if magic != b"GIMP" {
                return Err(BrushError::Invalid("missing GIMP magic".into()));
            }
            (28, be_u32(data, 24)? as f32)
        }
        v => return Err(BrushError::Unsupported(format!("gbr version {v}"))),
    };
    if bytes != 1 && bytes != 4 {
        return Err(BrushError::Unsupported(format!("{bytes} bytes per pixel")));
    }
    if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
        return Err(BrushError::Invalid(format!("size {width}x{height}")));
    }
    if header_size < fixed {
        return Err(BrushError::Invalid(format!("header size {header_size}")));
    }

    let name_bytes = data.get(fixed..header_size).ok_or(BrushError::Truncated)?;
    let name_end = name_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(name_bytes.len());
    let name = String::from_utf8_lossy(&name_bytes[..name_end]).into_owned();

    let n = width as usize * height as usize;
    let pixels = data
        .get(header_size..header_size + n * bytes as usize)
        .ok_or(BrushError::Truncated)?;

    let (mask, pixmap) = if bytes == 1 {
        (pixels.to_vec(), None)
    } else {
        let (px, _) = pixels.as_chunks::<4>();
        let mask = px.iter().map(|p| p[3]).collect();
        let rgb = px.iter().map(|p| [p[0], p[1], p[2]]).collect();
        (mask, Some(rgb))
    };

    Ok(Brush::bitmap(
        name,
        spacing,
        Mask::new(width, height, mask),
        pixmap,
    ))
}
