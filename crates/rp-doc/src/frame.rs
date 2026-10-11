//! `FrameMsg`: what the editor sends the DOS view for each frame.
//!
//! The editor's render worker builds one from the dithered output and the
//! current editor state; the DOS thread draws it and never sends anything
//! back. See docs/tech-stack.md, "Data flow between views".

use rp_palette_dither::{CgaColor, IndexedImage, Palette};

use crate::document::Layout;
use crate::raster::Size;

/// The editor tool, mirrored in the DOS tool strip and cursor shape.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Tool {
    /// Selection. DOS cursor: arrow.
    Select,
    /// Lasso selection (freehand or polygon). DOS cursor: crosshair.
    Lasso,
    /// Rectangle selection. DOS cursor: crosshair.
    RectSelect,
    /// Move or transform a layer. DOS cursor: arrow.
    Move,
    /// Paint with the current brush. DOS cursor: crosshair or brush outline.
    #[default]
    Brush,
    /// Erase with the current brush. DOS cursor: crosshair or brush outline.
    Eraser,
    /// Pick a color from the canvas. DOS cursor: crosshair.
    Picker,
}

impl Tool {
    /// Every tool, in tool-strip order.
    pub const ALL: [Tool; 7] = [
        Tool::Select,
        Tool::Lasso,
        Tool::RectSelect,
        Tool::Move,
        Tool::Brush,
        Tool::Eraser,
        Tool::Picker,
    ];

    /// Whether the DOS view shows the classic arrow cursor for this tool
    /// (otherwise a crosshair or brush outline).
    pub const fn uses_arrow_cursor(self) -> bool {
        matches!(self, Tool::Select | Tool::Move)
    }
}

/// A point in output pixels. May lie outside the output image (the pointer
/// can leave the canvas).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct OutputPoint {
    pub x: i32,
    pub y: i32,
}

/// The dithered output image: one CGA color per pixel, row-major, at output
/// resolution. Every pixel is in `palette` (the palette rule).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct IndexedFrame {
    pub size: Size,
    pub pixels: Vec<CgaColor>,
    /// The palette the dither used; the DOS palette strip shows it.
    pub palette: Palette,
}

impl IndexedFrame {
    /// A frame filled with one color. Panics if `color` is not in `palette`.
    pub fn filled(size: Size, color: CgaColor, palette: Palette) -> Self {
        assert!(palette.contains(color), "{color:?} not in {palette:?}");
        Self {
            size,
            pixels: vec![color; size.area()],
            palette,
        }
    }

    /// The color at `(x, y)`, or `None` outside the image.
    pub fn get(&self, x: i64, y: i64) -> Option<CgaColor> {
        self.size
            .contains(x, y)
            .then(|| self.pixels[y as usize * self.size.width as usize + x as usize])
    }
}

/// Takes the pipeline's output as is: the pixel buffer is moved, not copied.
impl From<IndexedImage> for IndexedFrame {
    fn from(img: IndexedImage) -> Self {
        Self {
            size: Size::new(img.width, img.height),
            pixels: img.pixels,
            palette: img.palette,
        }
    }
}

/// One frame for the DOS view.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FrameMsg {
    /// The dithered image at output resolution.
    pub image: IndexedFrame,
    /// Which DOS screen to compose.
    pub layout: Layout,
    /// Pointer position in output pixels, or `None` when the pointer is not
    /// over the editor canvas.
    pub cursor: Option<OutputPoint>,
    pub tool: Tool,
    /// Brush diameter in output pixels, for the brush-outline cursor.
    pub brush_diameter: u32,
    /// The foreground color as the CGA color the DOS palette strip
    /// highlights (the editor maps the full-color foreground to its nearest
    /// palette color).
    pub active_color: CgaColor,
}

impl FrameMsg {
    /// Converts an editor canvas position to output pixels (floor of the
    /// scaled position). The DOS view only ever sees output pixels.
    pub fn canvas_to_output(canvas: Size, output: Size, x: f32, y: f32) -> OutputPoint {
        let sx = output.width as f32 / canvas.width.max(1) as f32;
        let sy = output.height as f32 / canvas.height.max(1) as f32;
        OutputPoint {
            x: (x * sx).floor() as i32,
            y: (y * sy).floor() as i32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_shape_per_tool() {
        assert!(Tool::Select.uses_arrow_cursor());
        assert!(Tool::Move.uses_arrow_cursor());
        assert!(!Tool::Brush.uses_arrow_cursor());
        assert!(!Tool::Eraser.uses_arrow_cursor());
        assert!(!Tool::Picker.uses_arrow_cursor());
        assert!(!Tool::Lasso.uses_arrow_cursor());
        assert!(!Tool::RectSelect.uses_arrow_cursor());
    }

    #[test]
    fn canvas_to_output_scales_and_floors() {
        let c = Size::new(800, 1280);
        let o = Size::new(200, 320);
        assert_eq!(
            FrameMsg::canvas_to_output(c, o, 0.0, 0.0),
            OutputPoint { x: 0, y: 0 }
        );
        assert_eq!(
            FrameMsg::canvas_to_output(c, o, 7.9, 4.0),
            OutputPoint { x: 1, y: 1 }
        );
        assert_eq!(
            FrameMsg::canvas_to_output(c, o, -1.0, 1283.0),
            OutputPoint { x: -1, y: 320 }
        );
    }

    #[test]
    fn indexed_frame_lookup() {
        let mut f = IndexedFrame::filled(Size::new(3, 2), CgaColor::Blue, Palette::full());
        f.pixels[5] = CgaColor::Yellow;
        assert_eq!(f.get(2, 1), Some(CgaColor::Yellow));
        assert_eq!(f.get(0, 0), Some(CgaColor::Blue));
        assert_eq!(f.get(3, 0), None);
        assert_eq!(f.get(0, -1), None);
    }

    #[test]
    fn indexed_image_converts_without_copy() {
        use rp_palette_dither::{FourColor, Intensity, Preset};
        let palette = FourColor::new(Preset::Palette1, Intensity::High).palette();
        let pixels = vec![
            CgaColor::Black,
            CgaColor::LightCyan,
            CgaColor::LightMagenta,
            CgaColor::White,
            CgaColor::Black,
            CgaColor::White,
        ];
        let expected = pixels.clone();
        let ptr = pixels.as_ptr();
        let img = IndexedImage {
            width: 3,
            height: 2,
            pixels,
            palette,
        };
        let f = IndexedFrame::from(img);
        assert_eq!(f.size, Size::new(3, 2));
        assert_eq!(f.pixels, expected);
        assert_eq!(f.pixels.as_ptr(), ptr, "pixel buffer was copied");
        assert_eq!(f.palette, palette);
        assert_eq!(f.get(1, 0), Some(CgaColor::LightCyan));
        assert_eq!(f.get(2, 1), Some(CgaColor::White));
    }

    #[test]
    fn frame_msg_is_send() {
        fn assert_send<T: Send + 'static>() {}
        assert_send::<FrameMsg>();
    }
}
