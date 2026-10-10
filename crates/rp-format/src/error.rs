//! Typed errors for saving and loading `.rpaint` files.

use std::fmt;
use std::io;

use rp_doc::SourceHash;

/// Why a project could not be saved or loaded. Loading never panics on bad
/// input; every problem with the file maps to one of these.
#[derive(Debug)]
pub enum FormatError {
    /// Reading or writing the underlying file failed.
    Io(io::Error),
    /// The file is not a readable zip archive.
    Zip(zip::result::ZipError),
    /// A required entry (`manifest.json`, a layer PNG, a source PNG) is
    /// missing from the archive.
    MissingEntry { name: String },
    /// `manifest.json` is not valid JSON or does not match the manifest
    /// schema.
    Json(serde_json::Error),
    /// The manifest parsed but holds a value retro-paint cannot use.
    InvalidManifest(String),
    /// The file was written by a newer retro-paint. It is refused rather
    /// than partly loaded.
    NewerVersion { found: u64, supported: u32 },
    /// A PNG entry could not be decoded, or decoded to an unusable image.
    InvalidImage { entry: String, reason: String },
    /// Encoding a PNG while saving failed.
    PngEncode(png::EncodingError),
    /// A source image's pixels do not hash to the name it is stored or
    /// referenced under.
    HashMismatch {
        expected: SourceHash,
        actual: SourceHash,
    },
    /// A source hash is not 64 lowercase hex digits.
    BadSourceHash(SourceHash),
    /// Saving: an image layer refers to a source that is not in the
    /// [`SourceStore`](rp_doc::SourceStore).
    MissingSource(SourceHash),
    /// Saving: a float field is NaN or infinite, which JSON cannot hold.
    NonFinite { field: String },
    /// Export: the upscale factor is not in `1..=8`.
    InvalidScale(u8),
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatError::Io(e) => write!(f, "I/O error: {e}"),
            FormatError::Zip(e) => write!(f, "not a valid .rpaint archive: {e}"),
            FormatError::MissingEntry { name } => {
                write!(f, "project file is missing `{name}`")
            }
            FormatError::Json(e) => write!(f, "invalid manifest.json: {e}"),
            FormatError::InvalidManifest(msg) => write!(f, "invalid manifest.json: {msg}"),
            FormatError::NewerVersion { found, supported } => write!(
                f,
                "project file format version {found} is newer than this retro-paint \
                 supports (version {supported}); update retro-paint to open it"
            ),
            FormatError::InvalidImage { entry, reason } => {
                write!(f, "bad image `{entry}`: {reason}")
            }
            FormatError::PngEncode(e) => write!(f, "PNG encoding failed: {e}"),
            FormatError::HashMismatch { expected, actual } => write!(
                f,
                "source image hash mismatch: expected {expected}, pixels hash to {actual}"
            ),
            FormatError::BadSourceHash(h) => {
                write!(f, "bad source hash `{h}`: expected 64 lowercase hex digits")
            }
            FormatError::MissingSource(h) => {
                write!(f, "image layer source {h} is not loaded")
            }
            FormatError::NonFinite { field } => {
                write!(f, "`{field}` is not a finite number")
            }
            FormatError::InvalidScale(s) => {
                write!(f, "export scale {s}× is out of range; use 1× to 8×")
            }
        }
    }
}

impl std::error::Error for FormatError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FormatError::Io(e) => Some(e),
            FormatError::Zip(e) => Some(e),
            FormatError::Json(e) => Some(e),
            FormatError::PngEncode(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for FormatError {
    fn from(e: io::Error) -> Self {
        FormatError::Io(e)
    }
}

impl From<serde_json::Error> for FormatError {
    fn from(e: serde_json::Error) -> Self {
        FormatError::Json(e)
    }
}

impl From<png::EncodingError> for FormatError {
    fn from(e: png::EncodingError) -> Self {
        FormatError::PngEncode(e)
    }
}

impl From<zip::result::ZipError> for FormatError {
    fn from(e: zip::result::ZipError) -> Self {
        match e {
            zip::result::ZipError::Io(e) => FormatError::Io(e),
            e => FormatError::Zip(e),
        }
    }
}
