//! Document model, flattening, and the `FrameMsg` shared by both views.
//!
//! - [`Document`]: canvas and output sizes, DOS layout, palette choice,
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

pub mod blend;
mod document;
pub mod flatten;
mod frame;
mod layer;
mod raster;

pub use blend::BlendMode;
pub use document::{Document, Layout, PaletteChoice};
pub use flatten::{SourceStore, flatten};
pub use frame::{FrameMsg, IndexedFrame, OutputPoint, Tool};
pub use layer::{
    EditError, ImageLayer, Layer, LayerId, LayerKind, Lock, PaintLayer, SourceHash, Transform,
};
pub use raster::{BufferSizeError, GrayImage, RgbaImage, Size};
