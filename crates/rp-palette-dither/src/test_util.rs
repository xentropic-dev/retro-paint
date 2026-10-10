//! Test helpers for the palette rule. Enabled with the `test-util` feature.
//!
//! Every pixel of dithered output and export must be exactly one of the
//! active palette's colors. Use these in any crate's tests:
//!
//! ```toml
//! [dev-dependencies]
//! rp-palette-dither = { workspace = true, features = ["test-util"] }
//! ```

use crate::palette::{Palette, Rgb};

/// Panics unless every pixel is exactly one of `palette`'s colors.
///
/// `pixels` can yield anything convertible to [`Rgb`]: `Rgb`, `[u8; 3]`, or
/// [`CgaColor`](crate::CgaColor). For a packed RGBA8 buffer use
/// [`assert_rgba_palette_only`]. The panic message names the first offending
/// pixel's position and color.
#[track_caller]
pub fn assert_palette_only<I>(pixels: I, palette: &Palette)
where
    I: IntoIterator,
    I::Item: Into<Rgb>,
{
    for (i, px) in pixels.into_iter().enumerate() {
        let rgb = px.into();
        assert!(
            palette.contains_rgb(rgb),
            "pixel {i} is {rgb}, which is not in {palette:?}"
        );
    }
}

/// Like [`assert_palette_only`] for a packed RGBA8 buffer (4 bytes per pixel,
/// row-major). Also requires every pixel to be fully opaque.
#[track_caller]
pub fn assert_rgba_palette_only(rgba: &[u8], palette: &Palette) {
    assert!(
        rgba.len().is_multiple_of(4),
        "RGBA buffer length {} is not a multiple of 4",
        rgba.len()
    );
    for (i, px) in rgba.as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(px[3], 255, "pixel {i} has alpha {}, expected 255", px[3]);
        let rgb = Rgb::new(px[0], px[1], px[2]);
        assert!(
            palette.contains_rgb(rgb),
            "pixel {i} is {rgb}, which is not in {palette:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CgaColor, FourColor, Intensity, Preset};

    fn mode() -> Palette {
        FourColor::new(Preset::Mode5, Intensity::High).palette()
    }

    #[test]
    fn accepts_palette_pixels() {
        assert_palette_only([[0, 0, 0], [0x55, 0xff, 0xff], [0xff, 0xff, 0xff]], &mode());
        assert_palette_only(CgaColor::ALL, &Palette::full());
        assert_rgba_palette_only(&[0, 0, 0, 255, 0xff, 0x55, 0x55, 255], &mode());
    }

    #[test]
    #[should_panic(expected = "pixel 1 is #aa0000")]
    fn rejects_color_outside_subset() {
        // Red is CGA but not in high-intensity Mode 5.
        assert_palette_only([CgaColor::White, CgaColor::Red], &mode());
    }

    #[test]
    #[should_panic(expected = "pixel 0 is #010000")]
    fn rejects_near_miss() {
        assert_palette_only([[1u8, 0, 0]], &Palette::full());
    }

    #[test]
    #[should_panic(expected = "alpha 0")]
    fn rgba_rejects_transparency() {
        assert_rgba_palette_only(&[0, 0, 0, 0], &Palette::full());
    }

    #[test]
    #[should_panic(expected = "not a multiple of 4")]
    fn rgba_rejects_ragged_buffer() {
        assert_rgba_palette_only(&[0, 0, 0], &Palette::full());
    }
}
