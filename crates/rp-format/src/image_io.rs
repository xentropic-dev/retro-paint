//! Lossless PNG encode/decode for layer pixels, masks and sources.

use std::io::Cursor;

use png::{BitDepth, ColorType, Decoder, Encoder, Limits, Transformations};
use rp_doc::{GrayImage, RgbaImage, Size};

use crate::FormatError;

/// Decoder allocation cap. Well above any canvas retro-paint makes, low
/// enough that a hostile header cannot ask for unbounded memory.
const DECODE_LIMIT_BYTES: usize = 1 << 30;

/// RGBA8 PNG of `image`.
pub fn encode_rgba(image: &RgbaImage) -> Result<Vec<u8>, FormatError> {
    encode(image.size(), ColorType::Rgba, image.as_raw())
}

/// 8-bit grayscale PNG of `image`.
pub fn encode_gray(image: &GrayImage) -> Result<Vec<u8>, FormatError> {
    encode(image.size(), ColorType::Grayscale, image.as_raw())
}

fn encode(size: Size, color: ColorType, data: &[u8]) -> Result<Vec<u8>, FormatError> {
    let mut out = Vec::new();
    let mut enc = Encoder::new(&mut out, size.width, size.height);
    enc.set_color(color);
    enc.set_depth(BitDepth::Eight);
    let mut writer = enc.write_header()?;
    writer.write_image_data(data)?;
    writer.finish()?;
    Ok(out)
}

/// Decodes any 8- or 16-bit PNG to straight-alpha RGBA8. Files retro-paint
/// writes are RGBA8 already and pass through unchanged.
pub fn decode_rgba(entry: &str, bytes: &[u8]) -> Result<RgbaImage, FormatError> {
    let (size, color, data) = decode(entry, bytes)?;
    let rgba = match color {
        ColorType::Rgba => data,
        ColorType::Rgb => data
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        ColorType::Grayscale => data.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        ColorType::GrayscaleAlpha => data
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        ColorType::Indexed => return Err(invalid(entry, "indexed color was not expanded")),
    };
    RgbaImage::from_raw(size, rgba).map_err(|e| invalid(entry, e))
}

/// Decodes an 8-bit grayscale PNG (a layer mask).
pub fn decode_gray(entry: &str, bytes: &[u8]) -> Result<GrayImage, FormatError> {
    let (size, color, data) = decode(entry, bytes)?;
    if color != ColorType::Grayscale {
        return Err(invalid(
            entry,
            format!("mask must be grayscale, found {color:?}"),
        ));
    }
    GrayImage::from_raw(size, data).map_err(|e| invalid(entry, e))
}

fn decode(entry: &str, bytes: &[u8]) -> Result<(Size, ColorType, Vec<u8>), FormatError> {
    let mut dec = Decoder::new_with_limits(
        Cursor::new(bytes),
        Limits {
            bytes: DECODE_LIMIT_BYTES,
        },
    );
    dec.set_transformations(Transformations::normalize_to_color8());
    let mut reader = dec.read_info().map_err(|e| invalid(entry, e))?;
    let len = reader
        .output_buffer_size()
        .ok_or_else(|| invalid(entry, "image too large"))?;
    let mut buf = vec![0; len];
    let info = reader.next_frame(&mut buf).map_err(|e| invalid(entry, e))?;
    if info.bit_depth != BitDepth::Eight {
        return Err(invalid(entry, "expected 8-bit output"));
    }
    let size = Size::new(info.width, info.height);
    let row = info.line_size;
    let packed = row * info.height as usize;
    buf.truncate(packed);
    Ok((size, info.color_type, buf))
}

fn invalid(entry: &str, reason: impl ToString) -> FormatError {
    FormatError::InvalidImage {
        entry: entry.to_string(),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_round_trips_losslessly_with_straight_alpha() {
        let mut img = RgbaImage::new(Size::new(3, 2));
        img.put(0, 0, [255, 0, 0, 255]);
        img.put(1, 0, [10, 20, 30, 0]); // color under zero alpha is kept
        img.put(2, 1, [200, 100, 50, 7]);
        let back = decode_rgba("t", &encode_rgba(&img).unwrap()).unwrap();
        assert_eq!(back, img);
    }

    #[test]
    fn gray_round_trips() {
        let mut m = GrayImage::filled(Size::new(4, 3), 128);
        m.put(3, 2, 0);
        let back = decode_gray("m", &encode_gray(&m).unwrap()).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn mask_must_be_gray() {
        let png = encode_rgba(&RgbaImage::new(Size::new(1, 1))).unwrap();
        assert!(matches!(
            decode_gray("m", &png),
            Err(FormatError::InvalidImage { .. })
        ));
    }

    #[test]
    fn other_color_types_expand_to_rgba() {
        let png = encode_gray(&GrayImage::filled(Size::new(1, 1), 9)).unwrap();
        let img = decode_rgba("g", &png).unwrap();
        assert_eq!(img.get(0, 0), [9, 9, 9, 255]);
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(matches!(
            decode_rgba("x", b"not a png"),
            Err(FormatError::InvalidImage { .. })
        ));
    }
}
