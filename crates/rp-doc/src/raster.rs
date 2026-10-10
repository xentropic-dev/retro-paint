//! Plain raster buffers: canvas-sized RGBA layers, grayscale masks, and
//! decoded source images.

use std::fmt;

/// A width × height in pixels.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// `width * height`.
    pub const fn area(self) -> usize {
        self.width as usize * self.height as usize
    }

    /// Whether `(x, y)` lies inside `0..width` × `0..height`.
    pub fn contains(self, x: i64, y: i64) -> bool {
        x >= 0 && y >= 0 && x < self.width as i64 && y < self.height as i64
    }
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

/// Why a raster could not be built from a buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BufferSizeError {
    pub size: Size,
    pub expected_len: usize,
    pub actual_len: usize,
}

impl fmt::Display for BufferSizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "buffer for {} needs {} bytes, got {}",
            self.size, self.expected_len, self.actual_len
        )
    }
}

impl std::error::Error for BufferSizeError {}

/// A straight-alpha (not premultiplied) sRGB RGBA8 image, row-major, 4 bytes
/// per pixel.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RgbaImage {
    size: Size,
    data: Vec<u8>,
}

impl RgbaImage {
    /// A fully transparent image.
    pub fn new(size: Size) -> Self {
        Self {
            size,
            data: vec![0; size.area() * 4],
        }
    }

    /// Every pixel set to `rgba`.
    pub fn filled(size: Size, rgba: [u8; 4]) -> Self {
        Self {
            size,
            data: rgba.repeat(size.area()),
        }
    }

    /// Wraps a packed RGBA8 buffer; its length must be `width * height * 4`.
    pub fn from_raw(size: Size, data: Vec<u8>) -> Result<Self, BufferSizeError> {
        let expected_len = size.area() * 4;
        if data.len() != expected_len {
            return Err(BufferSizeError {
                size,
                expected_len,
                actual_len: data.len(),
            });
        }
        Ok(Self { size, data })
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn width(&self) -> u32 {
        self.size.width
    }

    pub fn height(&self) -> u32 {
        self.size.height
    }

    /// The packed RGBA8 bytes.
    pub fn as_raw(&self) -> &[u8] {
        &self.data
    }

    pub fn as_raw_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    pub fn into_raw(self) -> Vec<u8> {
        self.data
    }

    /// The pixel at `(x, y)`. Panics if out of bounds.
    pub fn get(&self, x: u32, y: u32) -> [u8; 4] {
        let i = self.index(x, y);
        self.data[i..i + 4].try_into().unwrap()
    }

    /// Sets the pixel at `(x, y)`. Panics if out of bounds.
    pub fn put(&mut self, x: u32, y: u32, rgba: [u8; 4]) {
        let i = self.index(x, y);
        self.data[i..i + 4].copy_from_slice(&rgba);
    }

    fn index(&self, x: u32, y: u32) -> usize {
        assert!(
            x < self.size.width && y < self.size.height,
            "({x}, {y}) outside {}",
            self.size
        );
        (y as usize * self.size.width as usize + x as usize) * 4
    }
}

impl fmt::Debug for RgbaImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RgbaImage({})", self.size)
    }
}

/// An 8-bit grayscale image, row-major. Used for layer masks: 255 shows the
/// layer, 0 hides it.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct GrayImage {
    size: Size,
    data: Vec<u8>,
}

impl GrayImage {
    /// Every pixel set to `value`.
    pub fn filled(size: Size, value: u8) -> Self {
        Self {
            size,
            data: vec![value; size.area()],
        }
    }

    /// Wraps a buffer of `width * height` bytes.
    pub fn from_raw(size: Size, data: Vec<u8>) -> Result<Self, BufferSizeError> {
        if data.len() != size.area() {
            return Err(BufferSizeError {
                size,
                expected_len: size.area(),
                actual_len: data.len(),
            });
        }
        Ok(Self { size, data })
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn as_raw(&self) -> &[u8] {
        &self.data
    }

    pub fn as_raw_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    pub fn into_raw(self) -> Vec<u8> {
        self.data
    }

    /// The value at `(x, y)`. Panics if out of bounds.
    pub fn get(&self, x: u32, y: u32) -> u8 {
        self.data[self.index(x, y)]
    }

    /// Sets the value at `(x, y)`. Panics if out of bounds.
    pub fn put(&mut self, x: u32, y: u32, value: u8) {
        let i = self.index(x, y);
        self.data[i] = value;
    }

    fn index(&self, x: u32, y: u32) -> usize {
        assert!(
            x < self.size.width && y < self.size.height,
            "({x}, {y}) outside {}",
            self.size
        );
        y as usize * self.size.width as usize + x as usize
    }
}

impl fmt::Debug for GrayImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GrayImage({})", self.size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_from_raw_checks_length() {
        let s = Size::new(2, 3);
        assert!(RgbaImage::from_raw(s, vec![0; 24]).is_ok());
        let e = RgbaImage::from_raw(s, vec![0; 23]).unwrap_err();
        assert_eq!((e.expected_len, e.actual_len), (24, 23));
        assert!(GrayImage::from_raw(s, vec![0; 6]).is_ok());
        assert!(GrayImage::from_raw(s, vec![0; 7]).is_err());
    }

    #[test]
    fn get_put_round_trip() {
        let mut img = RgbaImage::new(Size::new(3, 2));
        img.put(2, 1, [1, 2, 3, 4]);
        assert_eq!(img.get(2, 1), [1, 2, 3, 4]);
        assert_eq!(&img.as_raw()[20..24], &[1, 2, 3, 4]);
        let mut m = GrayImage::filled(Size::new(3, 2), 9);
        m.put(1, 1, 200);
        assert_eq!(m.get(1, 1), 200);
        assert_eq!(m.as_raw()[4], 200);
    }

    #[test]
    #[should_panic(expected = "outside 3x2")]
    fn out_of_bounds_panics() {
        RgbaImage::new(Size::new(3, 2)).get(3, 0);
    }
}
