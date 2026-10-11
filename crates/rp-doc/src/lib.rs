//! Document model, flattening, and the `FrameMsg` shared by both views.
//!
//! - [`Document`]: canvas and output sizes, DOS layout, dither settings
//!   ([`DitherSettings`], including the [`PaletteChoice`]),
//!   brush colors, swatches, and the layer stack (bottom to top).
//! - [`Layer`]: a paint layer ([`PaintLayer`], canvas-sized RGBA) or an image
//!   layer ([`ImageLayer`], a [`SourceHash`] plus a [`Transform`]), with
//!   visibility, opacity, [`BlendMode`], optional grayscale mask and [`Lock`].
//! - [`Document::flatten`]: composites the stack into one RGBA image. Image
//!   layer pixels come from a caller-supplied [`SourceStore`]; this crate does
//!   no I/O. Blending is in straight sRGB `f32` (see [`blend`]); image layers
//!   are bilinear-sampled (see [`flatten`](mod@flatten)).
//! - [`FrameMsg`]: the editor → DOS view message (indexed output image,
//!   cursor in output pixels, active tool, active color).
//! - [`History`]: memory-bounded undo/redo of whole-document snapshots and
//!   paint-layer dirty-rect patches (see [`history`]).
//! - [`Selection`] and [`Float`]: antialiased lasso selections and a lifted,
//!   transformable piece of a paint layer (see [`selection`]).

pub mod blend;
mod document;
pub mod flatten;
mod frame;
pub mod history;
mod layer;
mod raster;
pub mod selection;

pub use blend::BlendMode;
pub use document::{Document, Layout};
pub use flatten::{SourceStore, flatten};
pub use frame::{FrameMsg, IndexedFrame, OutputPoint, Tool};
pub use history::{Edit, EditKind, History, Rect};
pub use layer::{
    EditError, ImageLayer, Layer, LayerId, LayerKind, Lock, PaintLayer, SourceHash, Transform,
};
pub use raster::{BufferSizeError, GrayImage, RgbaImage, Size};
pub use rp_palette_dither::{DitherSettings, PaletteChoice};
pub use selection::{Float, SelectMode, Selection, erase_selected};
