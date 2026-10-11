//! GIMP .gbr / .vbr / .gih and Photoshop .abr brushes, and dab stamping.
//!
//! A [`Brush`] is a grayscale coverage [`Mask`] (255 = full paint), an optional
//! RGB pixmap for color brushes, and a spacing in percent of the brush size.
//! [`stamp`] composites one dab onto an RGBA buffer and [`Spacer`] turns pointer
//! samples into evenly spaced dab positions.
//!
//! # Brush size
//!
//! [`Brush::resized`] returns the brush at a new *diameter*, in pixels:
//! - Parametric (`.vbr` and the built-in round brushes): the diameter is
//!   `2 * radius`, i.e. the shape's length along its long axis before
//!   rotation. Resizing regenerates the mask from the [`VbrParams`] with the
//!   new radius, so hardness, shape, spikes, aspect ratio and angle are kept
//!   exactly. Masks are always odd-sized (center on the middle pixel), so a
//!   hard circle of even diameter `d` comes out `d - 1` pixels wide.
//! - Bitmap (`.gbr`): the diameter is the larger mask dimension. The mask (and
//!   pixmap) are resampled so the larger side is exactly the diameter and the
//!   smaller side keeps the aspect ratio (at least 1 px). Downscaling uses
//!   area (box) filtering, upscaling bilinear. A resized bitmap brush keeps
//!   its original in [`BrushSource::Resampled`], so resizing again always
//!   starts from the loaded pixels.
//!
//! A diameter below 1 is treated as 1, and diameter 1 always gives a 1x1 dab.
//! Spacing is a percentage of the size, so it is unchanged by resizing and
//! [`Brush::step`] scales with the brush.
//!
//! [`Brush::round_hard`] and [`Brush::round_soft`] are built-in default
//! brushes, so painting works without any brush files.
//!
//! # Brush files and the brush panel
//!
//! - [`load_library`] reads a path and returns every brush in it: one for a
//!   `.gbr` / `.vbr`, the first cell of a `.gih` image hose, all sampled
//!   brushes of a Photoshop `.abr`. [`load_library_bytes`] does the same for
//!   bytes already in memory. Errors are [`BrushError`], with
//!   [`BrushError::Io`] for a file that cannot be read.
//! - `.abr` packs can be 75 MB of 5000 px samples, so a panel should not
//!   decode them all up front. [`index_abr`] walks the file (well under a
//!   millisecond for 132 brushes) and returns an [`AbrIndex`] whose
//!   [`AbrEntry`]s carry name, spacing, size and the byte range of the
//!   pixels. [`decode_abr`] turns one entry into a [`Brush`] (a few ms for a
//!   5000 px sample); [`decode_abr_data`] does it from just
//!   `file[entry.data]`, so a caller can drop the file bytes and re-read
//!   only that range from disk. Index and decode are pure and safe to run on
//!   a worker thread; malformed input returns an error, never panics.
//! - [`thumbnail`] downscales a [`Mask`] with area averaging to at most
//!   `max_side` px (e.g. 64) for panel previews; it never upscales.
//! - Decoded `.abr` brushes are [`BrushSource::Bitmap`], so
//!   [`Brush::resized`] area-averages them down to the painting size without
//!   aliasing, always from the full-size sample.

mod abr;
mod gbr;
mod library;
mod resize;
mod stamp;
mod vbr;

pub use abr::{AbrEntry, AbrIndex, decode_abr, decode_abr_data, index_abr, parse_abr};
pub use gbr::parse_gbr;
pub use library::{file_stem, load_library, load_library_bytes, parse_gih};
pub use resize::thumbnail;
pub use stamp::{Spacer, stamp, stroke_dabs};
pub use vbr::{Shape, VbrParams, generate, parse_vbr};

use std::fmt;
use std::sync::Arc;

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
    /// Where the mask came from; decides how [`Brush::resized`] works.
    pub source: BrushSource,
}

/// How a brush's mask was made.
#[derive(Debug, Clone, PartialEq)]
pub enum BrushSource {
    /// The mask is the brush's original pixels (a loaded `.gbr`).
    Bitmap,
    /// Generated from these parameters (a `.vbr` or a built-in brush).
    Parametric(VbrParams),
    /// Resampled from this original bitmap brush.
    Resampled(Arc<Brush>),
}

impl Brush {
    /// A bitmap brush from a mask and optional pixmap.
    pub fn bitmap(
        name: impl Into<String>,
        spacing: f32,
        mask: Mask,
        pixmap: Option<Vec<[u8; 3]>>,
    ) -> Self {
        if let Some(pm) = &pixmap {
            assert_eq!(pm.len(), mask.data.len());
        }
        Self {
            name: name.into(),
            spacing,
            mask,
            pixmap,
            source: BrushSource::Bitmap,
        }
    }

    /// Built-in hard round brush (circle, hardness 1, 10% spacing).
    pub fn round_hard(diameter: f32) -> Self {
        generate(&round_params("Round Hard", 1.0, diameter))
    }

    /// Built-in soft round brush (circle, hardness 0.5, 10% spacing).
    pub fn round_soft(diameter: f32) -> Self {
        generate(&round_params("Round Soft", 0.5, diameter))
    }

    /// The brush's size in pixels; see the crate docs for what that means
    /// per brush kind.
    pub fn diameter(&self) -> f32 {
        match &self.source {
            BrushSource::Parametric(p) => 2.0 * p.radius,
            _ => self.mask.width.max(self.mask.height) as f32,
        }
    }

    /// This brush at a new diameter in pixels (at least 1). Name and spacing
    /// are kept.
    pub fn resized(&self, diameter: f32) -> Brush {
        let d = if diameter.is_finite() {
            diameter.clamp(1.0, resize::MAX_DIAMETER)
        } else {
            1.0
        };
        let mut out = match &self.source {
            BrushSource::Parametric(p) => {
                let mut p = p.clone();
                p.radius = d / 2.0;
                generate(&p)
            }
            BrushSource::Bitmap => resize::resample(&Arc::new(self.clone()), d.round() as u32),
            BrushSource::Resampled(orig) => resize::resample(orig, d.round() as u32),
        };
        out.name = self.name.clone();
        out.spacing = self.spacing;
        out
    }

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
    /// The file could not be read.
    Io(String),
}

impl fmt::Display for BrushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BrushError::Truncated => write!(f, "brush data is truncated"),
            BrushError::Unsupported(s) => write!(f, "unsupported brush: {s}"),
            BrushError::Invalid(s) => write!(f, "invalid brush: {s}"),
            BrushError::Io(s) => write!(f, "cannot read brush file: {s}"),
        }
    }
}

impl std::error::Error for BrushError {}

fn round_params(name: &str, hardness: f32, diameter: f32) -> VbrParams {
    VbrParams {
        name: name.into(),
        shape: Shape::Circle,
        spacing: 10.0,
        radius: diameter.max(1.0) / 2.0,
        spikes: 2,
        hardness,
        aspect_ratio: 1.0,
        angle: 0.0,
    }
}

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
