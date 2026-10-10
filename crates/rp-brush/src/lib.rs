//! GIMP .gbr and .vbr brushes and dab stamping.
//!
//! A [`Brush`] is a grayscale coverage [`Mask`] (255 = full paint), an optional
//! RGB pixmap for color brushes, and a spacing in percent of the brush size.
//! [`stamp`] composites one dab onto an RGBA buffer and [`Spacer`] turns pointer
//! samples into evenly spaced dab positions.

mod gbr;
mod stamp;
mod vbr;

pub use gbr::parse_gbr;
pub use stamp::{Spacer, stamp, stroke_dabs};
pub use vbr::{Shape, VbrParams, generate, parse_vbr};

use std::fmt;

/// Grayscale coverage mask, row-major, 0 = no paint, 255 = full paint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Mask {
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        assert_eq!(data.len(), width as usize * height as usize);
        Self {
            width,
            height,
            data,
        }
    }

    /// Coverage at (x, y). Panics when out of bounds.
    pub fn get(&self, x: u32, y: u32) -> u8 {
        assert!(x < self.width && y < self.height);
        self.data[(y * self.width + x) as usize]
    }
}

/// A loaded brush.
#[derive(Debug, Clone, PartialEq)]
pub struct Brush {
    pub name: String,
    /// Dab spacing as a percentage of the larger brush dimension.
    pub spacing: f32,
    pub mask: Mask,
    /// Per-pixel color for RGBA `.gbr` brushes; same size as `mask`.
    pub pixmap: Option<Vec<[u8; 3]>>,
}

impl Brush {
    /// Distance in pixels between dabs along a stroke (at least 1).
    pub fn step(&self) -> f32 {
        let size = self.mask.width.max(self.mask.height) as f32;
        (size * self.spacing / 100.0).max(1.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrushError {
    /// The data ended before the header or pixel data did.
    Truncated,
    /// A valid-looking file in a variant we do not handle.
    Unsupported(String),
    /// Malformed data.
    Invalid(String),
}

impl fmt::Display for BrushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BrushError::Truncated => write!(f, "brush data is truncated"),
            BrushError::Unsupported(s) => write!(f, "unsupported brush: {s}"),
            BrushError::Invalid(s) => write!(f, "invalid brush: {s}"),
        }
    }
}

impl std::error::Error for BrushError {}

/// Parse a `.gbr` or `.vbr` brush, detected from its contents.
pub fn load(bytes: &[u8]) -> Result<Brush, BrushError> {
    if bytes.starts_with(b"GIMP-VBR") {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| BrushError::Invalid("vbr is not UTF-8".into()))?;
        parse_vbr(text).map(|(_, b)| b)
    } else {
        parse_gbr(bytes)
    }
}
