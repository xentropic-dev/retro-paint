//! PNG export. See docs/file-format.md, "PNG export".
//!
//! - The dithered image is the same [`render`] the live view runs, at the
//!   document's output size, upscaled by an integer factor (1×–8×) with
//!   nearest neighbor so each output pixel becomes an exact `s×s` block.
//!   It is written as a 4-bit **indexed** PNG whose `PLTE` holds the active
//!   palette's colors (all 16, or the subset) in palette order, so the file
//!   can only hold palette colors.
//! - The optional full-color image is [`Document::flatten`] at canvas size,
//!   written as straight-alpha RGBA8, next to the dithered file as
//!   `<stem>-full.png`.
//!
//! Files are written like [`save_to_path`](crate::save_to_path): temp file,
//! sync, rename. Both PNGs are encoded in memory before either is written.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use png::{BitDepth, ColorType, Encoder};
use rp_doc::{Document, RgbaImage, Size, SourceStore};
use rp_palette_dither::{IndexedImage, Renderer, render};

use crate::{FormatError, image_io, write_atomically};

/// Largest export upscale factor.
pub const MAX_EXPORT_SCALE: u8 = 8;

/// What [`export_png`] writes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExportOptions {
    /// Integer nearest-neighbor upscale, `1..=8`.
    pub scale: u8,
    /// Also write the flattened full-color image as `<stem>-full.png`.
    pub include_full_color: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            scale: 1,
            include_full_color: false,
        }
    }
}

/// What [`export_png`] wrote.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExportReport {
    /// The dithered PNG.
    pub dithered: PathBuf,
    /// Its pixel size: output size × scale.
    pub dithered_size: Size,
    /// The full-color PNG and its size (the canvas size), if requested.
    pub full_color: Option<(PathBuf, Size)>,
}

/// Checks `scale` is in `1..=8`.
pub fn check_scale(scale: u8) -> Result<(), FormatError> {
    if (1..=MAX_EXPORT_SCALE).contains(&scale) {
        Ok(())
    } else {
        Err(FormatError::InvalidScale(scale))
    }
}

/// The document's dithered output: [`Document::flatten`] then [`render`] at
/// the output size, exactly as the live view does.
pub fn render_document(doc: &Document, sources: &SourceStore) -> IndexedImage {
    let flat = doc.flatten(sources);
    render_flat(&flat, doc, None)
}

fn render_flat(flat: &RgbaImage, doc: &Document, renderer: Option<&mut Renderer>) -> IndexedImage {
    let canvas = (flat.width(), flat.height());
    let output = (doc.output_size.width, doc.output_size.height);
    match renderer {
        Some(r) => r.render(flat.as_raw(), canvas, output, &doc.dither),
        None => render(flat.as_raw(), canvas, output, &doc.dither),
    }
}

/// Nearest-neighbor integer upscale: pixel `(x, y)` fills the block
/// `[x·s, x·s+s) × [y·s, y·s+s)`. The palette is unchanged.
pub fn upscale_nearest(image: &IndexedImage, scale: u8) -> Result<IndexedImage, FormatError> {
    check_scale(scale)?;
    let s = scale as usize;
    let (w, h) = (image.width as usize, image.height as usize);
    let mut pixels = Vec::with_capacity(w * s * h * s);
    for row in image.pixels.chunks_exact(w.max(1)).take(h) {
        let start = pixels.len();
        for &c in row {
            pixels.extend(std::iter::repeat_n(c, s));
        }
        for _ in 1..s {
            pixels.extend_from_within(start..start + w * s);
        }
    }
    Ok(IndexedImage {
        width: image.width * scale as u32,
        height: image.height * scale as u32,
        pixels,
        palette: image.palette,
    })
}

/// Encodes `image` upscaled by `scale` as a 4-bit indexed PNG whose palette
/// is `image.palette` in order. Errors if `scale` is not `1..=8`, or if a
/// pixel is not in `image.palette` (which [`render`] never produces).
pub fn encode_dithered_png(image: &IndexedImage, scale: u8) -> Result<Vec<u8>, FormatError> {
    let big = upscale_nearest(image, scale)?;
    let palette = big.palette;
    let mut plte = Vec::with_capacity(palette.len() * 3);
    for rgb in palette.rgb() {
        plte.extend(rgb.to_array());
    }

    // Two pixels per byte, high nibble first; rows padded to a whole byte.
    let w = big.width as usize;
    let row_bytes = w.div_ceil(2);
    let mut data = vec![0u8; row_bytes * big.height as usize];
    for (y, row) in big.pixels.chunks_exact(w.max(1)).enumerate() {
        for (x, &c) in row.iter().enumerate() {
            let i = palette
                .position(c)
                .ok_or_else(|| FormatError::InvalidImage {
                    entry: "dithered export".into(),
                    reason: format!("pixel ({x}, {y}) is {c:?}, not in {palette:?}"),
                })? as u8;
            let shift = if x % 2 == 0 { 4 } else { 0 };
            data[y * row_bytes + x / 2] |= i << shift;
        }
    }

    let mut out = Vec::new();
    let mut enc = Encoder::new(&mut out, big.width, big.height);
    enc.set_color(ColorType::Indexed);
    enc.set_depth(BitDepth::Four);
    enc.set_palette(plte);
    let mut writer = enc.write_header()?;
    writer.write_image_data(&data)?;
    writer.finish()?;
    Ok(out)
}

/// Encodes a flattened full-color image as straight-alpha RGBA8 PNG.
pub fn encode_full_color_png(flat: &RgbaImage) -> Result<Vec<u8>, FormatError> {
    image_io::encode_rgba(flat)
}

/// `<dir>/<stem>-full.<ext>` for a dithered export path `<dir>/<stem>.<ext>`
/// (`.png` if it has no extension).
pub fn full_color_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default();
    let mut name = OsString::from(stem);
    name.push("-full.");
    name.push(path.extension().unwrap_or("png".as_ref()));
    path.with_file_name(name)
}

/// Exports `doc` to `path` as a dithered PNG (see the module docs) and, if
/// `options.include_full_color`, the flattened image to
/// [`full_color_path`]. Each file is written atomically; an invalid scale
/// writes nothing.
pub fn export_png(
    doc: &Document,
    sources: &SourceStore,
    options: &ExportOptions,
    path: impl AsRef<Path>,
) -> Result<ExportReport, FormatError> {
    export_png_with(&mut Renderer::new(), doc, sources, options, path)
}

/// [`export_png`] reusing a [`Renderer`]'s cached color table. Output is
/// identical.
pub fn export_png_with(
    renderer: &mut Renderer,
    doc: &Document,
    sources: &SourceStore,
    options: &ExportOptions,
    path: impl AsRef<Path>,
) -> Result<ExportReport, FormatError> {
    check_scale(options.scale)?;
    let path = path.as_ref();
    let flat = doc.flatten(sources);
    let indexed = render_flat(&flat, doc, Some(renderer));
    let dithered_png = encode_dithered_png(&indexed, options.scale)?;
    let full = if options.include_full_color {
        Some((full_color_path(path), encode_full_color_png(&flat)?))
    } else {
        None
    };

    write_atomically(path, |f| Ok(std::io::Write::write_all(f, &dithered_png)?))?;
    let s = options.scale as u32;
    let mut report = ExportReport {
        dithered: path.to_path_buf(),
        dithered_size: Size::new(indexed.width * s, indexed.height * s),
        full_color: None,
    };
    if let Some((full_path, bytes)) = full {
        write_atomically(&full_path, |f| Ok(std::io::Write::write_all(f, &bytes)?))?;
        report.full_color = Some((full_path, flat.size()));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rp_palette_dither::{CgaColor, Palette};

    fn tiny() -> IndexedImage {
        IndexedImage {
            width: 3,
            height: 2,
            pixels: vec![
                CgaColor::Black,
                CgaColor::Blue,
                CgaColor::White,
                CgaColor::Red,
                CgaColor::Yellow,
                CgaColor::Black,
            ],
            palette: Palette::full(),
        }
    }

    #[test]
    fn upscale_makes_exact_blocks() {
        let img = tiny();
        for s in 1..=8u8 {
            let big = upscale_nearest(&img, s).unwrap();
            assert_eq!((big.width, big.height), (3 * s as u32, 2 * s as u32));
            for y in 0..big.height {
                for x in 0..big.width {
                    assert_eq!(big.get(x, y), img.get(x / s as u32, y / s as u32));
                }
            }
        }
    }

    #[test]
    fn bad_scale_is_typed_error() {
        for s in [0, 9, 255] {
            assert!(matches!(
                upscale_nearest(&tiny(), s),
                Err(FormatError::InvalidScale(v)) if v == s
            ));
            assert!(matches!(
                encode_dithered_png(&tiny(), s),
                Err(FormatError::InvalidScale(_))
            ));
        }
    }

    #[test]
    fn color_outside_palette_is_refused() {
        let mut img = tiny();
        img.palette = Palette::new([CgaColor::Black, CgaColor::Blue]).unwrap();
        assert!(matches!(
            encode_dithered_png(&img, 1),
            Err(FormatError::InvalidImage { .. })
        ));
    }

    #[test]
    fn full_color_path_appends_suffix() {
        assert_eq!(
            full_color_path(Path::new("/a/b/pic.png")),
            Path::new("/a/b/pic-full.png")
        );
        assert_eq!(full_color_path(Path::new("pic")), Path::new("pic-full.png"));
    }
}
