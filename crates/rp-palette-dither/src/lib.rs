//! CGA palette, color matching, and dithering. See docs/filter-and-palette.md.
//!
//! Palette types:
//! - [`CgaColor`]: one of the 16 CGA colors, numbered in doc order.
//! - [`Palette`]: the ordered subset the dither may output (full 16 by default).
//! - [`FourColor`]: an authentic 4-color mode ([`Preset`] + [`Intensity`] +
//!   background), convertible to a [`Palette`].
//!
//! With the `test-util` feature, `test_util::assert_palette_only` checks the
//! palette rule on any image.

mod palette;

#[cfg(any(test, feature = "test-util"))]
pub mod test_util;

pub use palette::{CGA_RGB, CgaColor, FourColor, Intensity, Palette, PaletteError, Preset, Rgb};
