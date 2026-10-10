//! Pixel preparation for drawing the flattened canvas in GPUI. Pure, no GPUI
//! types.
//!
//! GPUI's image sprites are BGRA (straight alpha) and its renderer samples
//! them with a **linear** filter only; there is no nearest-neighbor option at
//! the pinned revision. To keep pixels crisp when zoomed in, the view draws a
//! [`SharpTile`] instead of the whole canvas once one canvas pixel covers two
//! or more device pixels: the visible region upscaled on the CPU by an
//! integer factor with nearest-neighbor, so the GPU only has to scale it by
//! less than 2× and edges blur by at most one device pixel.

use rp_doc::RgbaImage;

use crate::viewport::Viewport;

/// Size in screen pixels of one transparency checkerboard square.
pub const CHECKER: u32 = 8;
/// Checkerboard colors (light, dark) as RGB.
pub const CHECKER_LIGHT: [u8; 3] = [0xcc, 0xcc, 0xcc];
pub const CHECKER_DARK: [u8; 3] = [0x99, 0x99, 0x99];

/// Device zoom at or above which the view switches to a [`SharpTile`].
pub const SHARP_MIN_DEVICE_ZOOM: f32 = 2.0;

/// Straight RGBA → straight BGRA, the layout GPUI's `RenderImage` expects.
pub fn rgba_to_bgra(img: &RgbaImage) -> Vec<u8> {
    let mut out = img.as_raw().to_vec();
    for px in out.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    out
}

/// A `width × height` opaque BGRA checkerboard of [`CHECKER`]-pixel squares,
/// light in the top-left corner.
pub fn checkerboard_bgra(width: u32, height: u32, square: u32) -> Vec<u8> {
    let square = square.max(1);
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let [r, g, b] = if ((x / square) + (y / square)).is_multiple_of(2) {
                CHECKER_LIGHT
            } else {
                CHECKER_DARK
            };
            out.extend_from_slice(&[b, g, r, 255]);
        }
    }
    out
}

/// A canvas region upscaled by an integer factor for crisp zoomed drawing.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SharpTile {
    /// Canvas pixel rectangle covered: origin and size.
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Nearest-neighbor upscale factor (≥ 2).
    pub factor: u32,
}

impl SharpTile {
    /// Size of the upscaled image in pixels.
    pub fn image_size(&self) -> (u32, u32) {
        (self.width * self.factor, self.height * self.factor)
    }
}

/// The tile to draw for `vp` on a display with `scale_factor` device pixels
/// per logical pixel, or `None` to draw the whole canvas image directly.
///
/// The tile covers every canvas pixel at least partly visible, and its
/// factor is `floor(device zoom)`, so the image is never larger than the
/// view in device pixels plus one canvas pixel on each side.
pub fn plan_sharp_tile(vp: &Viewport, scale_factor: f32) -> Option<SharpTile> {
    let device_zoom = vp.zoom() * scale_factor;
    if device_zoom < SHARP_MIN_DEVICE_ZOOM {
        return None;
    }
    let (min, max) = vp.visible_canvas_rect()?;
    let canvas = vp.canvas_size();
    let x0 = min.x.floor().max(0.0) as u32;
    let y0 = min.y.floor().max(0.0) as u32;
    let x1 = (max.x.ceil().min(canvas.x) as u32).max(x0 + 1);
    let y1 = (max.y.ceil().min(canvas.y) as u32).max(y0 + 1);
    Some(SharpTile {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
        factor: device_zoom.floor() as u32,
    })
}

/// Cuts `tile` out of a `src_width`-wide BGRA (or RGBA) buffer and upscales
/// it by `tile.factor` with nearest-neighbor. The channel order is kept.
pub fn upscale_tile(src: &[u8], src_width: u32, tile: &SharpTile) -> Vec<u8> {
    let f = tile.factor as usize;
    let (out_w, out_h) = tile.image_size();
    let row_bytes = out_w as usize * 4;
    let mut out = vec![0u8; row_bytes * out_h as usize];
    for ty in 0..tile.height as usize {
        let row = &mut out[ty * f * row_bytes..(ty * f + 1) * row_bytes];
        let src_row = (tile.y as usize + ty) * src_width as usize;
        for tx in 0..tile.width as usize {
            let s = (src_row + tile.x as usize + tx) * 4;
            let px = &src[s..s + 4];
            for k in 0..f {
                let d = (tx * f + k) * 4;
                row[d..d + 4].copy_from_slice(px);
            }
        }
        // Repeat the finished row for the remaining f-1 output rows.
        let (first, rest) =
            out[ty * f * row_bytes..(ty + 1) * f * row_bytes].split_at_mut(row_bytes);
        for chunk in rest.chunks_exact_mut(row_bytes) {
            chunk.copy_from_slice(first);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewport::Vec2;
    use rp_doc::Size;

    #[test]
    fn bgra_swaps_red_and_blue_and_keeps_alpha() {
        let mut img = RgbaImage::new(Size::new(2, 1));
        img.put(0, 0, [10, 20, 30, 40]);
        img.put(1, 0, [255, 0, 0, 255]);
        assert_eq!(rgba_to_bgra(&img), vec![30, 20, 10, 40, 0, 0, 255, 255]);
    }

    #[test]
    fn checkerboard_alternates_squares() {
        let w = 20;
        let px = checkerboard_bgra(w, 10, 8);
        let at = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            [px[i + 2], px[i + 1], px[i], px[i + 3]]
        };
        let light = [0xcc, 0xcc, 0xcc, 255];
        let dark = [0x99, 0x99, 0x99, 255];
        assert_eq!(px.len(), 20 * 10 * 4);
        assert_eq!(at(0, 0), light);
        assert_eq!(at(7, 7), light);
        assert_eq!(at(8, 0), dark);
        assert_eq!(at(0, 8), dark);
        assert_eq!(at(8, 8), light);
        assert_eq!(at(19, 9), dark);
        assert_eq!(at(16, 0), light);
    }

    fn view(zoom: f32, pan: (f32, f32)) -> Viewport {
        let mut v = Viewport::new(800, 1280);
        v.set_view_size(1000.0, 700.0);
        v.zoom_about(zoom, Vec2::default());
        v.pan_by(pan.0, pan.1);
        v
    }

    #[test]
    fn no_tile_below_two_device_pixels_per_canvas_pixel() {
        assert_eq!(plan_sharp_tile(&view(1.0, (0.0, 0.0)), 1.0), None);
        assert_eq!(plan_sharp_tile(&view(1.5, (0.0, 0.0)), 1.0), None);
        assert!(plan_sharp_tile(&view(1.0, (0.0, 0.0)), 2.0).is_some());
    }

    #[test]
    fn tile_covers_visible_pixels_with_floor_factor() {
        // 2.5× zoom, canvas origin at screen (-101, -51): visible canvas
        // x 40.4..440.4, y 20.4..300.4.
        let mut v = view(2.5, (0.0, 0.0));
        let o = v.offset();
        v.pan_by(-101.0 - o.x, -51.0 - o.y);
        let t = plan_sharp_tile(&v, 1.0).unwrap();
        assert_eq!(
            t,
            SharpTile {
                x: 40,
                y: 20,
                width: 401,
                height: 281,
                factor: 2
            }
        );
        let (w, h) = t.image_size();
        assert!(w <= 1000 + 2 * 3 && h <= 700 + 2 * 3);
    }

    #[test]
    fn tile_is_clamped_to_canvas_and_scales_with_dpi() {
        let v = view(4.0, (0.0, 0.0));
        let o = v.offset();
        let t = plan_sharp_tile(&v, 2.0).unwrap();
        assert_eq!(t.factor, 8);
        assert_eq!((t.x, t.y), (0, 0));
        let s = v.canvas_to_screen(Vec2::new(t.width as f32, t.height as f32));
        assert!(s.x >= 1000.0 && s.y >= 700.0, "{o:?} {t:?}");
        assert!(t.x + t.width <= 800 && t.y + t.height <= 1280);
    }

    #[test]
    fn upscale_repeats_each_pixel_factor_squared_times() {
        // 3×2 source, pixel value = index.
        let src: Vec<u8> = (0..6u8).flat_map(|i| [i, i, i, 255]).collect();
        let tile = SharpTile {
            x: 1,
            y: 0,
            width: 2,
            height: 2,
            factor: 3,
        };
        let out = upscale_tile(&src, 3, &tile);
        let (w, h) = tile.image_size();
        assert_eq!((w, h), (6, 6));
        assert_eq!(out.len(), 6 * 6 * 4);
        for y in 0..6 {
            for x in 0..6 {
                let expect = (1 + x / 3 + (y / 3) * 3) as u8;
                assert_eq!(out[((y * 6 + x) * 4) as usize], expect, "({x},{y})");
            }
        }
    }
}
