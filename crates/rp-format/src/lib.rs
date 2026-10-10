//! The `.rpaint` project format. See docs/file-format.md.
//!
//! A project is a zip archive:
//!
//! ```text
//! manifest.json          format_version, project settings, layer stack
//! layers/0001.png        paint layer pixels, RGBA8 (1-based, bottom first)
//! layers/0001-mask.png   layer mask, 8-bit grayscale, if present
//! sources/<hash>.png     image layer sources, RGBA8, original resolution
//! ```
//!
//! - [`save`] / [`save_to_path`] write a [`Document`] and the sources its
//!   image layers use. `save_to_path` writes a temp file and renames it over
//!   the target, so a failed save never leaves a half-written project.
//! - [`load`] / [`load_from_path`] read one back. Older manifests are
//!   migrated step by step ([`migrate`]); newer ones are refused with
//!   [`FormatError::NewerVersion`]. Every problem with the file is a typed
//!   [`FormatError`], never a panic.
//! - Sources are named by [`source_hash`] (SHA-256 of size + RGBA pixels).
//!   Both save and load check that pixels match their hash.
//!
//! - [`export_png`] writes the dithered output (and optionally the
//!   flattened full-color image) as PNG; see [`ExportOptions`].
//!
//! All PNGs are lossless RGBA8 / Gray8 with straight alpha, so save → load
//! gives back identical pixels, and rendering the loaded document gives
//! byte-identical dithered output. Layer ids are not stored; loading assigns
//! fresh ones in stack order.

mod error;
mod export;
mod hash;
mod image_io;
mod manifest;
mod migrate;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{Read, Seek, Write};
use std::path::Path;

use rp_doc::{Document, Layer, LayerKind, PaintLayer, RgbaImage, SourceHash, SourceStore};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

pub use error::FormatError;
pub use export::{
    ExportOptions, ExportReport, MAX_EXPORT_SCALE, check_scale, encode_dithered_png,
    encode_full_color_png, export_png, export_png_with, full_color_path, render_document,
    upscale_nearest,
};
pub use hash::source_hash;
pub use migrate::{FORMAT_VERSION, migrate};

use manifest::{ContentDto, Manifest, layer_entry, mask_entry, source_entry};

/// Name of the manifest entry.
pub const MANIFEST: &str = "manifest.json";

/// Writes `doc` as a `.rpaint` archive. Only the sources referenced by image
/// layers are stored; each must be in `sources` and match its hash.
pub fn save<W: Write + Seek>(
    doc: &Document,
    sources: &SourceStore,
    out: &mut W,
) -> Result<(), FormatError> {
    let manifest = Manifest::from_document(doc)?;

    // Check every source before writing anything.
    let mut used: BTreeMap<&str, &RgbaImage> = BTreeMap::new();
    for layer in doc.layers() {
        if let LayerKind::Image(img) = &layer.kind {
            if used.contains_key(img.source.as_str()) {
                continue;
            }
            check_well_formed(&img.source)?;
            let image = sources
                .get(&img.source)
                .ok_or_else(|| FormatError::MissingSource(img.source.clone()))?;
            check_hash(&img.source, image)?;
            used.insert(img.source.as_str(), image);
        }
    }

    let mut zip = ZipWriter::new(out);
    let deflate = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    // PNGs are already compressed.
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    zip.start_file(MANIFEST, deflate)?;
    serde_json::to_writer_pretty(&mut zip, &manifest)?;

    for (i, layer) in doc.layers().iter().enumerate() {
        if let LayerKind::Paint(p) = &layer.kind {
            zip.start_file(layer_entry(i), stored)?;
            zip.write_all(&image_io::encode_rgba(&p.pixels)?)?;
        }
        if let Some(mask) = &layer.mask {
            zip.start_file(mask_entry(i), stored)?;
            zip.write_all(&image_io::encode_gray(mask)?)?;
        }
    }
    for (hash, image) in used {
        zip.start_file(source_entry(hash), stored)?;
        zip.write_all(&image_io::encode_rgba(image)?)?;
    }
    zip.finish()?;
    Ok(())
}

/// [`save`] to a file, atomically: writes `<path>.tmp` beside it, syncs, and
/// renames it over `path`. On error the temp file is removed and any
/// existing file at `path` is untouched.
pub fn save_to_path(
    doc: &Document,
    sources: &SourceStore,
    path: impl AsRef<Path>,
) -> Result<(), FormatError> {
    write_atomically(path.as_ref(), |file| save(doc, sources, file))
}

/// Writes `path` atomically: `write` fills `<path>.tmp` beside it, which is
/// synced and renamed over `path`. On error the temp file is removed and
/// any existing file at `path` is untouched.
pub(crate) fn write_atomically(
    path: &Path,
    write: impl FnOnce(&mut fs::File) -> Result<(), FormatError>,
) -> Result<(), FormatError> {
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    let result = (|| {
        let mut file = fs::File::create(&tmp)?;
        write(&mut file)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Reads a `.rpaint` archive. Returns the document and the sources its
/// image layers use.
pub fn load<R: Read + Seek>(input: R) -> Result<(Document, SourceStore), FormatError> {
    let mut zip = ZipArchive::new(input).map_err(FormatError::Zip)?;

    let raw: serde_json::Value = serde_json::from_slice(&read_entry(&mut zip, MANIFEST)?)?;
    let version = migrate::format_version(&raw)?;
    let manifest: Manifest = serde_json::from_value(migrate(raw, version)?)?;

    let mut doc = manifest.to_empty_document()?;
    let mut sources = SourceStore::new();
    let mut loaded: HashSet<String> = HashSet::new();

    for dto in &manifest.layers {
        let kind = match &dto.content {
            ContentDto::Paint {
                pixels,
                offset_x,
                offset_y,
            } => LayerKind::Paint(PaintLayer {
                pixels: image_io::decode_rgba(pixels, &read_entry(&mut zip, pixels)?)?,
                offset_x: *offset_x,
                offset_y: *offset_y,
            }),
            ContentDto::Image { source, transform } => {
                let hash = SourceHash::new(source.clone());
                if loaded.insert(source.clone()) {
                    check_well_formed(&hash)?;
                    let entry = source_entry(source);
                    let image = image_io::decode_rgba(&entry, &read_entry(&mut zip, &entry)?)?;
                    check_hash(&hash, &image)?;
                    sources.insert(hash.clone(), image);
                }
                LayerKind::Image(rp_doc::ImageLayer {
                    source: hash,
                    transform: (*transform).into(),
                })
            }
        };
        let mut layer = Layer::with_kind(dto.name.clone(), kind);
        dto.apply_common(&mut layer)?;
        if let Some(entry) = &dto.mask {
            layer.mask = Some(image_io::decode_gray(entry, &read_entry(&mut zip, entry)?)?);
        }
        doc.push_layer(layer);
    }
    Ok((doc, sources))
}

/// [`load`] from a file.
pub fn load_from_path(path: impl AsRef<Path>) -> Result<(Document, SourceStore), FormatError> {
    let file = fs::File::open(path)?;
    load(std::io::BufReader::new(file))
}

fn read_entry<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str) -> Result<Vec<u8>, FormatError> {
    let mut file = zip.by_name(name).map_err(|e| match e {
        zip::result::ZipError::FileNotFound => FormatError::MissingEntry {
            name: name.to_owned(),
        },
        e => FormatError::Zip(e),
    })?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(|e| {
        // A failed read inside an entry (bad deflate stream, CRC mismatch)
        // means the archive is corrupt.
        FormatError::Zip(zip::result::ZipError::Io(e))
    })?;
    Ok(buf)
}

fn check_well_formed(hash: &SourceHash) -> Result<(), FormatError> {
    if hash::is_well_formed(hash) {
        Ok(())
    } else {
        Err(FormatError::BadSourceHash(hash.clone()))
    }
}

fn check_hash(expected: &SourceHash, image: &RgbaImage) -> Result<(), FormatError> {
    let actual = source_hash(image);
    if &actual == expected {
        Ok(())
    } else {
        Err(FormatError::HashMismatch {
            expected: expected.clone(),
            actual,
        })
    }
}
