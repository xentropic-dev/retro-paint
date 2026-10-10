//! The document: sizes, layout, filter palette, colors, swatches, and the
//! layer stack.

use rp_palette_dither::{FourColor, Palette, Rgb};

use crate::layer::{Layer, LayerId};
use crate::raster::Size;

/// DOS view layout. See docs/aesthetics.md.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Layout {
    /// Vertical 200×320 DOS screen, 9:16 tube. The primary target.
    #[default]
    Mobile,
    /// 320×200 DOS screen, 4:3 tube in a 16:9 frame.
    Desktop,
}

impl Layout {
    /// The fixed DOS screen size for this layout.
    pub const fn dos_screen(self) -> Size {
        match self {
            Layout::Mobile => Size::new(200, 320),
            Layout::Desktop => Size::new(320, 200),
        }
    }
}

/// Which colors the dither may output, as the user chose them. Kept as the
/// user's choice (not just the resulting [`Palette`]) so the filter panel and
/// project file can show and restore it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum PaletteChoice {
    /// All 16 CGA colors.
    #[default]
    Full,
    /// An authentic 4-color mode with its background color.
    FourColor(FourColor),
    /// A user-picked subset.
    Custom(Palette),
}

impl PaletteChoice {
    /// The palette the dither should use.
    pub fn palette(&self) -> Palette {
        match *self {
            PaletteChoice::Full => Palette::full(),
            PaletteChoice::FourColor(m) => m.palette(),
            PaletteChoice::Custom(p) => p,
        }
    }
}

/// A retro-paint project.
///
/// The layer stack is ordered **bottom to top**: index 0 is drawn first.
/// Dither method, strength and pre-adjust settings belong to the pipeline's
/// `DitherSettings` and will join this struct with that feature; for now only
/// the palette choice is stored.
#[derive(Clone, PartialEq, Debug)]
pub struct Document {
    /// Full-color working image size. Paint layers are this size.
    pub canvas_size: Size,
    /// Size of the dithered output (DOS canvas window, PNG export).
    pub output_size: Size,
    pub layout: Layout,
    pub palette: PaletteChoice,
    /// Foreground brush color.
    pub fg: Rgb,
    /// Background brush color.
    pub bg: Rgb,
    /// User-saved full-color swatches.
    pub swatches: Vec<Rgb>,
    layers: Vec<Layer>,
    next_id: u64,
}

impl Document {
    /// An empty document: no layers, output at a quarter of the canvas size
    /// (at least 1×1), mobile layout, full palette, black on white.
    pub fn new(canvas_size: Size) -> Self {
        Self {
            canvas_size,
            output_size: Size::new(
                (canvas_size.width / 4).max(1),
                (canvas_size.height / 4).max(1),
            ),
            layout: Layout::default(),
            palette: PaletteChoice::default(),
            fg: Rgb::new(0, 0, 0),
            bg: Rgb::new(255, 255, 255),
            swatches: Vec::new(),
            layers: Vec::new(),
            next_id: 1,
        }
    }

    /// Layers bottom to top.
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// Mutable layers bottom to top. Ids cannot be changed through this.
    pub fn layers_mut(&mut self) -> &mut [Layer] {
        &mut self.layers
    }

    /// Adds `layer` on top of the stack and returns its new id.
    pub fn push_layer(&mut self, layer: Layer) -> LayerId {
        let index = self.layers.len();
        self.insert_layer(index, layer)
    }

    /// Inserts `layer` at stack `index` (0 = bottom, clamped to the top) and
    /// returns its new id.
    pub fn insert_layer(&mut self, index: usize, mut layer: Layer) -> LayerId {
        let id = LayerId(self.next_id);
        self.next_id += 1;
        layer.id = id;
        let index = index.min(self.layers.len());
        self.layers.insert(index, layer);
        id
    }

    /// Removes and returns the layer with `id`.
    pub fn remove_layer(&mut self, id: LayerId) -> Option<Layer> {
        let i = self.index_of(id)?;
        Some(self.layers.remove(i))
    }

    /// Moves the layer with `id` to stack `index` (clamped to the top).
    /// Returns `false` if there is no such layer.
    pub fn move_layer(&mut self, id: LayerId, index: usize) -> bool {
        let Some(from) = self.index_of(id) else {
            return false;
        };
        let layer = self.layers.remove(from);
        let to = index.min(self.layers.len());
        self.layers.insert(to, layer);
        true
    }

    /// Stack index of the layer with `id`.
    pub fn index_of(&self, id: LayerId) -> Option<usize> {
        self.layers.iter().position(|l| l.id == id)
    }

    pub fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    pub fn layer_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rp_palette_dither::{CgaColor, Intensity, Preset};

    fn names(d: &Document) -> Vec<&str> {
        d.layers().iter().map(|l| l.name.as_str()).collect()
    }

    #[test]
    fn new_document_defaults() {
        let d = Document::new(Size::new(800, 1280));
        assert_eq!(d.output_size, Size::new(200, 320));
        assert_eq!(d.layout, Layout::Mobile);
        assert_eq!(d.palette.palette(), Palette::full());
        assert!(d.layers().is_empty());
        assert_eq!(Document::new(Size::new(3, 3)).output_size, Size::new(1, 1));
    }

    #[test]
    fn dos_screen_sizes() {
        assert_eq!(Layout::Mobile.dos_screen(), Size::new(200, 320));
        assert_eq!(Layout::Desktop.dos_screen(), Size::new(320, 200));
    }

    #[test]
    fn palette_choice_resolves() {
        let m = FourColor::new(Preset::Mode5, Intensity::High).with_background(CgaColor::Blue);
        assert_eq!(PaletteChoice::FourColor(m).palette(), m.palette());
        let custom = Palette::new([CgaColor::White, CgaColor::Black]).unwrap();
        assert_eq!(PaletteChoice::Custom(custom).palette(), custom);
    }

    #[test]
    fn layer_stack_operations() {
        let s = Size::new(2, 2);
        let mut d = Document::new(s);
        let a = d.push_layer(Layer::paint("a", s));
        let b = d.push_layer(Layer::paint("b", s));
        let c = d.insert_layer(0, Layer::paint("c", s));
        assert_eq!(names(&d), ["c", "a", "b"]);
        assert!(a != b && b != c && a != c);
        assert_eq!(d.layer(b).unwrap().id(), b);

        assert!(d.move_layer(c, 99));
        assert_eq!(names(&d), ["a", "b", "c"]);
        assert!(d.move_layer(b, 0));
        assert_eq!(names(&d), ["b", "a", "c"]);
        assert_eq!(d.index_of(c), Some(2));

        d.layer_mut(a).unwrap().name = "renamed".into();
        assert_eq!(d.remove_layer(a).unwrap().name, "renamed");
        assert_eq!(names(&d), ["b", "c"]);
        assert!(d.remove_layer(a).is_none());
        assert!(!d.move_layer(a, 0));

        // Ids are never reused.
        let e = d.push_layer(Layer::paint("e", s));
        assert!(![a, b, c].contains(&e));
    }
}
