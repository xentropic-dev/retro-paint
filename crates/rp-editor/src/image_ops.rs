//! Insert Image: decode a user's image file into a source, place it as a new
//! image layer.
//!
//! Decisions:
//! - Decoding reads the file's content, not its extension: PNG, JPEG and
//!   WebP are supported (other formats GPUI's own `image` features happen to
//!   enable also decode). Pixels become RGBA8 with straight alpha; 16-bit and
//!   grayscale images are converted. The EXIF orientation (JPEG, PNG, WebP)
//!   is applied, so the source is stored upright.
//! - Sources are named by [`rp_format::source_hash`] of the decoded pixels,
//!   so inserting the same picture twice stores it once.
//! - The new layer goes directly above the selection (on top if none is
//!   selected), is named after the file stem, and becomes selected.
//! - Placement: the image is centered on the canvas at 1:1; if it is larger
//!   than the canvas it is scaled down uniformly to fit inside it. It is
//!   never upscaled. The translation is whole pixels, so a 1:1 image passes
//!   through flatten unchanged.
//! - Decoding and hashing run off the UI thread ([`load_image_file`] is
//!   blocking); the status bar shows `Loading <file>…` meanwhile and the
//!   result or the error afterwards.

use std::fmt;
use std::io::Cursor;
use std::path::Path;

use image::{DynamicImage, ImageDecoder, ImageError, ImageReader, error::LimitErrorKind};
use rp_doc::{Layer, LayerId, RgbaImage, Size, SourceHash, Transform};

use crate::state::EditorState;

/// Name used when a path has no usable file stem.
pub const FALLBACK_LAYER_NAME: &str = "Image";

/// Why an image could not be inserted.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum InsertError {
    /// The file could not be read.
    Io(String),
    /// Not an image format retro-paint decodes.
    Unsupported,
    /// The file looks like a supported image but its data is broken.
    Corrupt(String),
    /// The image exceeds the decoder's size limits.
    TooLarge,
    /// Zero width or height.
    Empty,
}

impl fmt::Display for InsertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InsertError::Io(e) => write!(f, "can't read file: {e}"),
            InsertError::Unsupported => {
                f.write_str("unsupported image format (use PNG, JPEG or WebP)")
            }
            InsertError::Corrupt(e) => write!(f, "corrupt image: {e}"),
            InsertError::TooLarge => f.write_str("image is too large"),
            InsertError::Empty => f.write_str("image is empty"),
        }
    }
}

impl std::error::Error for InsertError {}

impl From<ImageError> for InsertError {
    fn from(e: ImageError) -> Self {
        match e {
            ImageError::Unsupported(_) => InsertError::Unsupported,
            ImageError::Limits(l) => match l.kind() {
                LimitErrorKind::DimensionError | LimitErrorKind::InsufficientMemory => {
                    InsertError::TooLarge
                }
                _ => InsertError::Unsupported,
            },
            ImageError::IoError(e) => InsertError::Corrupt(e.to_string()),
            ImageError::Decoding(e) => InsertError::Corrupt(e.to_string()),
            ImageError::Parameter(e) => InsertError::Corrupt(e.to_string()),
            ImageError::Encoding(e) => InsertError::Corrupt(e.to_string()),
        }
    }
}

/// A decoded, hashed image ready to become a layer.
#[derive(Clone, PartialEq, Debug)]
pub struct LoadedImage {
    /// Layer name (the file stem).
    pub name: String,
    pub hash: SourceHash,
    pub image: RgbaImage,
}

/// Decodes an encoded image (format sniffed from the bytes) into upright
/// straight-alpha RGBA8.
pub fn decode_image(bytes: &[u8]) -> Result<RgbaImage, InsertError> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| InsertError::Io(e.to_string()))?;
    if reader.format().is_none() {
        return Err(InsertError::Unsupported);
    }
    let mut decoder = reader.into_decoder()?;
    // A broken EXIF block must not stop the import; it just stays unrotated.
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    let rgba = img.into_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return Err(InsertError::Empty);
    }
    Ok(RgbaImage::from_raw(Size::new(w, h), rgba.into_raw()).expect("decoder buffer matches size"))
}

/// Layer name for an image inserted from `path`: its file stem.
pub fn layer_name_for(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| FALLBACK_LAYER_NAME.to_string())
}

/// File name for status messages.
pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Decodes `bytes` and hashes the result. Blocking; call off the UI thread.
pub fn load_image_bytes(name: String, bytes: &[u8]) -> Result<LoadedImage, InsertError> {
    let image = decode_image(bytes)?;
    let hash = rp_format::source_hash(&image);
    Ok(LoadedImage { name, hash, image })
}

/// Reads, decodes and hashes the image at `path`. Blocking; call off the
/// UI thread.
pub fn load_image_file(path: &Path) -> Result<LoadedImage, InsertError> {
    let bytes = std::fs::read(path).map_err(|e| InsertError::Io(e.to_string()))?;
    load_image_bytes(layer_name_for(path), &bytes)
}

/// Transform that centers a `src`-sized image on a `canvas`-sized canvas,
/// scaled down uniformly to fit inside it if needed, never up.
pub fn fit_transform(src: Size, canvas: Size) -> Transform {
    let (sw, sh) = (src.width.max(1) as f32, src.height.max(1) as f32);
    let (cw, ch) = (canvas.width as f32, canvas.height as f32);
    let scale = (cw / sw).min(ch / sh).min(1.0);
    Transform {
        translate_x: ((cw - sw * scale) / 2.0).floor(),
        translate_y: ((ch - sh * scale) / 2.0).floor(),
        scale_x: scale,
        scale_y: scale,
        rotation: 0.0,
    }
}

impl EditorState {
    /// Status shown while a file decodes.
    pub fn begin_image_load(&mut self, path: &Path) {
        self.status = format!("Loading {}…", display_name(path));
    }

    /// Adds `loaded` as an image layer above the selection, fitted to the
    /// canvas, and selects it. The source is stored once per hash.
    pub fn insert_loaded_image(&mut self, loaded: LoadedImage) -> LayerId {
        let LoadedImage { name, hash, image } = loaded;
        let size = image.size();
        if !self.sources.contains(&hash) {
            std::sync::Arc::make_mut(&mut self.sources).insert(hash.clone(), image);
        }
        let transform = fit_transform(size, self.document.canvas_size);
        let layer = Layer::image(name.clone(), hash, transform);
        let index = self
            .selected_index()
            .map_or(self.document.layers().len(), |i| i + 1);
        let id = self.document.insert_layer(index, layer);
        self.selected_layer = Some(id);
        self.mark_document_changed();
        let pct = (transform.scale_x * 100.0).round();
        self.status = if transform.scale_x < 1.0 {
            format!(
                "Inserted {name} ({}×{}, fit at {pct}%)",
                size.width, size.height
            )
        } else {
            format!("Inserted {name} ({}×{})", size.width, size.height)
        };
        id
    }

    /// Applies the result of a background load of `path`.
    pub fn finish_image_load(
        &mut self,
        path: &Path,
        result: Result<LoadedImage, InsertError>,
    ) -> Option<LayerId> {
        match result {
            Ok(loaded) => Some(self.insert_loaded_image(loaded)),
            Err(e) => {
                self.status = format!("Can't insert {}: {e}", display_name(path));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::NEW_CANVAS_SIZE;
    use image::{ImageEncoder, ImageFormat, codecs::jpeg::JpegEncoder};
    use rp_doc::LayerKind;
    use std::path::PathBuf;

    fn encode(img: &image::RgbaImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(img.clone())
            .write_to(&mut out, format)
            .unwrap();
        out.into_inner()
    }

    fn gradient(w: u32, h: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([(x * 40) as u8, (y * 40) as u8, 128, (100 + x + y) as u8])
        })
    }

    fn loaded(name: &str, w: u32, h: u32) -> LoadedImage {
        load_image_bytes(name.into(), &encode(&gradient(w, h), ImageFormat::Png)).unwrap()
    }

    fn image_layer(st: &EditorState, id: LayerId) -> rp_doc::ImageLayer {
        match &st.document.layer(id).unwrap().kind {
            LayerKind::Image(i) => i.clone(),
            other => panic!("not an image layer: {other:?}"),
        }
    }

    // ---- decoding ----------------------------------------------------------

    #[test]
    fn png_decodes_exactly_with_straight_alpha() {
        let src = gradient(5, 3);
        let img = decode_image(&encode(&src, ImageFormat::Png)).unwrap();
        assert_eq!(img.size(), Size::new(5, 3));
        assert_eq!(img.as_raw(), src.as_raw().as_slice());
    }

    #[test]
    fn jpeg_decodes_to_opaque_rgba() {
        let src = image::RgbImage::from_pixel(16, 8, image::Rgb([200, 40, 40]));
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(src)
            .write_to(&mut out, ImageFormat::Jpeg)
            .unwrap();
        let img = decode_image(&out.into_inner()).unwrap();
        assert_eq!(img.size(), Size::new(16, 8));
        for px in img.as_raw().chunks(4) {
            assert_eq!(px[3], 255);
            assert!(px[0] > 180 && px[1] < 70 && px[2] < 70, "{px:?}");
        }
    }

    #[test]
    fn webp_decodes() {
        let src = gradient(4, 4);
        let img = decode_image(&encode(&src, ImageFormat::WebP)).unwrap();
        assert_eq!(img.as_raw(), src.as_raw().as_slice());
    }

    #[test]
    fn grayscale_16_bit_png_becomes_rgba8() {
        let src = image::ImageBuffer::<image::Luma<u16>, _>::from_pixel(2, 2, image::Luma([65535]));
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageLuma16(src)
            .write_to(&mut out, ImageFormat::Png)
            .unwrap();
        let img = decode_image(&out.into_inner()).unwrap();
        assert!(img.as_raw().iter().all(|&b| b == 255));
    }

    /// Minimal big-endian EXIF (TIFF) block with one Orientation entry.
    fn exif_orientation(value: u16) -> Vec<u8> {
        let mut v = b"MM\0\x2a".to_vec();
        v.extend(8u32.to_be_bytes());
        v.extend(1u16.to_be_bytes());
        v.extend(0x0112u16.to_be_bytes());
        v.extend(3u16.to_be_bytes());
        v.extend(1u32.to_be_bytes());
        v.extend(value.to_be_bytes());
        v.extend([0, 0]);
        v.extend(0u32.to_be_bytes());
        v
    }

    #[test]
    fn exif_orientation_is_applied() {
        // Orientation 6: rotate 90° clockwise to display.
        let src = image::RgbImage::from_fn(16, 8, |x, _| {
            if x < 8 {
                image::Rgb([255, 0, 0])
            } else {
                image::Rgb([0, 0, 255])
            }
        });
        let mut out = Vec::new();
        let mut enc = JpegEncoder::new_with_quality(&mut out, 95);
        enc.set_exif_metadata(exif_orientation(6)).unwrap();
        enc.write_image(src.as_raw(), 16, 8, image::ExtendedColorType::Rgb8)
            .unwrap();
        let img = decode_image(&out).unwrap();
        assert_eq!(img.size(), Size::new(8, 16));
        // The left (red) half is now the top half.
        let top = img.get(4, 2);
        let bottom = img.get(4, 13);
        assert!(top[0] > 200 && top[2] < 60, "{top:?}");
        assert!(bottom[2] > 200 && bottom[0] < 60, "{bottom:?}");
    }

    // ---- error mapping -----------------------------------------------------

    #[test]
    fn unknown_bytes_are_unsupported() {
        assert_eq!(
            decode_image(b"this is not an image at all"),
            Err(InsertError::Unsupported)
        );
        assert_eq!(decode_image(&[]), Err(InsertError::Unsupported));
    }

    #[test]
    fn truncated_png_is_corrupt() {
        let bytes = encode(&gradient(32, 32), ImageFormat::Png);
        let err = decode_image(&bytes[..bytes.len() / 2]).unwrap_err();
        assert!(matches!(err, InsertError::Corrupt(_)), "{err:?}");
    }

    #[test]
    fn garbage_after_png_signature_is_corrupt() {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend([0xAB; 64]);
        let err = decode_image(&bytes).unwrap_err();
        assert!(matches!(err, InsertError::Corrupt(_)), "{err:?}");
    }

    #[test]
    fn missing_file_is_io_error() {
        let err = load_image_file(Path::new("/nonexistent/dir/photo.png")).unwrap_err();
        assert!(matches!(err, InsertError::Io(_)), "{err:?}");
    }

    #[test]
    fn errors_read_well_in_the_status_bar() {
        let mut st = EditorState::new();
        let v = st.doc_version;
        let layers = st.document.layers().len();
        let r = st.finish_image_load(Path::new("/x/notes.txt"), Err(InsertError::Unsupported));
        assert_eq!(r, None);
        assert_eq!(
            st.status,
            "Can't insert notes.txt: unsupported image format (use PNG, JPEG or WebP)"
        );
        assert_eq!(st.doc_version, v);
        assert_eq!(st.document.layers().len(), layers);
        st.finish_image_load(
            Path::new("a.png"),
            Err(InsertError::Corrupt("bad crc".into())),
        );
        assert_eq!(st.status, "Can't insert a.png: corrupt image: bad crc");
    }

    #[test]
    fn loading_status_names_the_file() {
        let mut st = EditorState::new();
        st.begin_image_load(Path::new("/home/me/Pictures/cat.jpg"));
        assert_eq!(st.status, "Loading cat.jpg…");
    }

    // ---- naming ------------------------------------------------------------

    #[test]
    fn layer_is_named_after_the_file_stem() {
        assert_eq!(
            layer_name_for(Path::new("/a/b/Sunset Beach.jpeg")),
            "Sunset Beach"
        );
        assert_eq!(layer_name_for(Path::new("archive.tar.png")), "archive.tar");
        assert_eq!(layer_name_for(Path::new(".png")), ".png");
        assert_eq!(layer_name_for(Path::new("/")), FALLBACK_LAYER_NAME);
        assert_eq!(layer_name_for(&PathBuf::from("")), FALLBACK_LAYER_NAME);
    }

    #[test]
    fn load_file_uses_stem_and_source_hash() {
        let dir = std::env::temp_dir().join(format!("rp-insert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rocks.png");
        std::fs::write(&path, encode(&gradient(3, 2), ImageFormat::Png)).unwrap();
        let l = load_image_file(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(l.name, "rocks");
        assert_eq!(l.hash, rp_format::source_hash(&l.image));
        assert_eq!(l.hash.as_str().len(), 64);
    }

    // ---- placement ---------------------------------------------------------

    const CANVAS: Size = Size::new(800, 1280);

    #[test]
    fn smaller_image_is_centered_at_one_to_one() {
        let t = fit_transform(Size::new(200, 100), CANVAS);
        assert_eq!(t, Transform::at(300.0, 590.0));
    }

    #[test]
    fn odd_margins_round_to_whole_pixels() {
        let t = fit_transform(Size::new(201, 101), CANVAS);
        assert_eq!(t, Transform::at(299.0, 589.0));
    }

    #[test]
    fn same_size_image_covers_canvas_exactly() {
        assert_eq!(fit_transform(CANVAS, CANVAS), Transform::IDENTITY);
    }

    #[test]
    fn wide_image_fits_width_and_centers_vertically() {
        let t = fit_transform(Size::new(4000, 1000), CANVAS);
        assert_eq!(t.scale_x, 0.2);
        assert_eq!(t.scale_y, 0.2);
        assert_eq!(t.translate_x, 0.0);
        assert_eq!(t.translate_y, (1280.0f32 - 200.0) / 2.0);
        assert_eq!(t.rotation, 0.0);
    }

    #[test]
    fn tall_image_fits_height_and_centers_horizontally() {
        let t = fit_transform(Size::new(1000, 6400), CANVAS);
        assert_eq!(t.scale_x, 0.2);
        assert_eq!(t.translate_y, 0.0);
        assert_eq!(t.translate_x, 300.0);
    }

    #[test]
    fn larger_image_in_both_axes_fits_the_tighter_one() {
        // 4000×3000 on 800×1280: width is tighter (0.2 vs 0.4267).
        let t = fit_transform(Size::new(4000, 3000), CANVAS);
        assert_eq!(t.scale_x, 0.2);
        let (w, h) = (4000.0 * t.scale_x, 3000.0 * t.scale_y);
        assert!(w <= 800.0 && h <= 1280.0);
        assert_eq!(t.translate_x, 0.0);
        assert_eq!(t.translate_y, ((1280.0 - h) / 2.0).floor());
    }

    #[test]
    fn larger_in_one_axis_only_still_scales_down() {
        let t = fit_transform(Size::new(900, 100), CANVAS);
        assert_eq!(t.scale_x, 800.0 / 900.0);
        assert!(900.0 * t.scale_x <= 800.0);
    }

    // ---- inserting ---------------------------------------------------------

    #[test]
    fn insert_adds_selected_image_layer_above_selection() {
        let mut st = EditorState::new();
        let bottom = st.document.layers()[0].id();
        let top = st.add_paint_layer();
        st.select_layer(bottom);
        let v = st.doc_version;
        let id = st.insert_loaded_image(loaded("pebbles", 10, 6));
        assert_ne!(st.doc_version, v);
        assert_eq!(st.selected_layer, Some(id));
        let ids: Vec<_> = st.document.layers().iter().map(|l| l.id()).collect();
        assert_eq!(ids, [bottom, id, top]);
        let layer = st.document.layer(id).unwrap();
        assert_eq!(layer.name, "pebbles");
        let img = image_layer(&st, id);
        assert_eq!(
            img.transform,
            fit_transform(Size::new(10, 6), NEW_CANVAS_SIZE)
        );
        assert_eq!(
            st.sources.get(&img.source).unwrap().size(),
            Size::new(10, 6)
        );
        assert_eq!(st.status, "Inserted pebbles (10×6)");
    }

    #[test]
    fn insert_with_no_selection_goes_on_top() {
        let mut st = EditorState::new();
        st.selected_layer = None;
        let id = st.insert_loaded_image(loaded("x", 2, 2));
        assert_eq!(st.document.layers().last().unwrap().id(), id);
    }

    #[test]
    fn large_insert_reports_fit_scale() {
        let mut st = EditorState::new();
        let id = st.insert_loaded_image(loaded("big", 1600, 400));
        assert_eq!(image_layer(&st, id).transform.scale_x, 0.5);
        assert_eq!(st.status, "Inserted big (1600×400, fit at 50%)");
    }

    #[test]
    fn same_image_twice_is_stored_once() {
        let mut st = EditorState::new();
        let a = st.insert_loaded_image(loaded("one", 7, 5));
        let b = st.insert_loaded_image(loaded("two", 7, 5));
        assert_ne!(a, b);
        assert_eq!(st.sources.len(), 1);
        assert_eq!(image_layer(&st, a).source, image_layer(&st, b).source);
        st.insert_loaded_image(loaded("three", 5, 7));
        assert_eq!(st.sources.len(), 2);
    }

    #[test]
    fn inserted_image_flattens_into_the_canvas() {
        let mut st = EditorState::new();
        let src = image::RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 255]));
        let l = load_image_bytes("sq".into(), &encode(&src, ImageFormat::Png)).unwrap();
        st.insert_loaded_image(l);
        let flat = st.document.flatten(&st.sources);
        let (cx, cy) = (NEW_CANVAS_SIZE.width / 2, NEW_CANVAS_SIZE.height / 2);
        assert_eq!(flat.get(cx, cy), [10, 20, 30, 255]);
        assert_eq!(flat.get(cx - 3, cy), [255, 255, 255, 255]);
        assert_eq!(flat.get(cx - 2, cy - 2), [10, 20, 30, 255]);
    }

    #[test]
    fn source_snapshot_held_elsewhere_is_not_mutated() {
        let mut st = EditorState::new();
        let snapshot = st.sources.clone();
        st.insert_loaded_image(loaded("x", 3, 3));
        assert!(snapshot.is_empty());
        assert_eq!(st.sources.len(), 1);
    }
}
