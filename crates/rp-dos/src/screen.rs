//! The indexed DOS screen and the drawing primitives the composer uses.
//!
//! Everything here works in DOS screen pixels and clips to the screen.

use rp_doc::Size;
use rp_palette_dither::CgaColor;

/// An axis-aligned rectangle in screen pixels.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// One past the last column.
    pub const fn right(self) -> i32 {
        self.x + self.w
    }

    /// One past the last row.
    pub const fn bottom(self) -> i32 {
        self.y + self.h
    }

    pub const fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    /// The rectangle shrunk by `n` on every side.
    pub const fn inset(self, n: i32) -> Self {
        Self::new(self.x + n, self.y + n, self.w - 2 * n, self.h - 2 * n)
    }
}

/// A composed DOS screen: one CGA color per pixel, row-major.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct IndexedScreen {
    size: Size,
    pixels: Vec<CgaColor>,
}

impl IndexedScreen {
    /// A screen filled with one color.
    pub fn new(size: Size, fill: CgaColor) -> Self {
        Self {
            size,
            pixels: vec![fill; size.area()],
        }
    }

    pub fn size(&self) -> Size {
        self.size
    }

    /// Row-major pixels, `width * height` of them.
    pub fn pixels(&self) -> &[CgaColor] {
        &self.pixels
    }

    /// The color at `(x, y)`, or `None` off screen.
    pub fn get(&self, x: i32, y: i32) -> Option<CgaColor> {
        self.size
            .contains(x as i64, y as i64)
            .then(|| self.pixels[self.index(x, y)])
    }

    /// Packed RGBA8 (opaque), for textures and PNGs.
    pub fn to_rgba(&self) -> Vec<u8> {
        self.pixels
            .iter()
            .flat_map(|c| {
                let [r, g, b] = c.rgb().to_array();
                [r, g, b, 0xFF]
            })
            .collect()
    }

    /// Packed RGB8.
    pub fn to_rgb(&self) -> Vec<u8> {
        self.pixels
            .iter()
            .flat_map(|c| c.rgb().to_array())
            .collect()
    }

    fn index(&self, x: i32, y: i32) -> usize {
        y as usize * self.size.width as usize + x as usize
    }

    /// Sets one pixel; off-screen writes are ignored.
    pub(crate) fn set(&mut self, x: i32, y: i32, color: CgaColor) {
        if self.size.contains(x as i64, y as i64) {
            let i = self.index(x, y);
            self.pixels[i] = color;
        }
    }

    /// Sets one pixel only if it lies inside `clip`.
    pub(crate) fn set_clipped(&mut self, x: i32, y: i32, color: CgaColor, clip: Rect) {
        if clip.contains(x, y) {
            self.set(x, y, color);
        }
    }

    pub(crate) fn fill(&mut self, r: Rect, color: CgaColor) {
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                self.set(x, y, color);
            }
        }
    }

    pub(crate) fn hline(&mut self, x: i32, y: i32, w: i32, color: CgaColor) {
        self.fill(Rect::new(x, y, w, 1), color);
    }

    pub(crate) fn vline(&mut self, x: i32, y: i32, h: i32, color: CgaColor) {
        self.fill(Rect::new(x, y, 1, h), color);
    }

    /// A 1-pixel outline just inside `r`.
    pub(crate) fn outline(&mut self, r: Rect, color: CgaColor) {
        self.hline(r.x, r.y, r.w, color);
        self.hline(r.x, r.bottom() - 1, r.w, color);
        self.vline(r.x, r.y, r.h, color);
        self.vline(r.right() - 1, r.y, r.h, color);
    }

    /// A 1-pixel 3D edge just inside `r`: light top-left and dark
    /// bottom-right when raised, the other way round when sunken.
    pub(crate) fn bevel(&mut self, r: Rect, raised: bool) {
        let (light, dark) = if raised {
            (CgaColor::White, CgaColor::DarkGray)
        } else {
            (CgaColor::DarkGray, CgaColor::White)
        };
        self.hline(r.x, r.y, r.w, light);
        self.vline(r.x, r.y, r.h, light);
        self.hline(r.x, r.bottom() - 1, r.w, dark);
        self.vline(r.right() - 1, r.y, r.h, dark);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawing_clips_to_screen() {
        let mut s = IndexedScreen::new(Size::new(4, 3), CgaColor::Black);
        s.fill(Rect::new(-2, -2, 4, 4), CgaColor::Red);
        s.set(10, 10, CgaColor::Red);
        assert_eq!(s.get(0, 0), Some(CgaColor::Red));
        assert_eq!(s.get(1, 1), Some(CgaColor::Red));
        assert_eq!(s.get(2, 0), Some(CgaColor::Black));
        assert_eq!(s.get(4, 0), None);
        assert_eq!(s.get(-1, 0), None);
    }

    #[test]
    fn outline_and_bevel_stay_inside() {
        let mut s = IndexedScreen::new(Size::new(5, 5), CgaColor::Black);
        let r = Rect::new(1, 1, 3, 3);
        s.bevel(r, true);
        assert_eq!(s.get(1, 1), Some(CgaColor::White));
        assert_eq!(s.get(3, 3), Some(CgaColor::DarkGray));
        assert_eq!(s.get(2, 2), Some(CgaColor::Black));
        assert_eq!(s.get(0, 0), Some(CgaColor::Black));
        assert_eq!(s.get(4, 4), Some(CgaColor::Black));
    }

    #[test]
    fn rgba_is_opaque_cga() {
        let s = IndexedScreen::new(Size::new(1, 1), CgaColor::LightBlue);
        assert_eq!(s.to_rgba(), [0x55, 0x55, 0xFF, 0xFF]);
    }

    #[test]
    fn rect_inset_and_contains() {
        let r = Rect::new(2, 3, 10, 6).inset(1);
        assert_eq!(r, Rect::new(3, 4, 8, 4));
        assert!(r.contains(3, 4));
        assert!(!r.contains(11, 4));
        assert!(!r.contains(3, 8));
    }
}
