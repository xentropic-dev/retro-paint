//! Layers: paint layers (canvas-sized RGBA) and image layers (a source image
//! reference plus a transform), with the properties every layer shares.

use std::fmt;

use crate::blend::BlendMode;
use crate::raster::{GrayImage, RgbaImage, Size};

/// Identifies a layer within one [`Document`](crate::Document). Assigned by
/// the document when the layer is added; not stored in project files.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct LayerId(pub(crate) u64);

/// Content hash of an inserted image, as a lowercase hex string. Names the
/// file `sources/<hash>.png` in a project. The hash algorithm is chosen by
/// whoever inserts the image; this crate only compares the strings.
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct SourceHash(pub String);

impl SourceHash {
    pub fn new(hex: impl Into<String>) -> Self {
        Self(hex.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Places a source image on the canvas.
///
/// Scale and rotation happen about the **center** of the source image.
/// `translate` is where the top-left corner of the scaled, *unrotated* image
/// lands on the canvas, so the image's center sits at
/// `translate + (src_w · scale_x / 2, src_h · scale_y / 2)`. `rotation` is in
/// radians, clockwise on screen (y points down). For a canvas point `p` the
/// sampled source point is
///
/// ```text
/// src = src_center + S⁻¹ · R(−rotation) · (p − canvas_center)
/// ```
///
/// with pixel centers at `i + 0.5` in both spaces.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Transform {
    pub translate_x: f32,
    pub translate_y: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub rotation: f32,
}

impl Transform {
    /// Source pixels map 1:1 onto canvas pixels starting at the origin.
    pub const IDENTITY: Transform = Transform {
        translate_x: 0.0,
        translate_y: 0.0,
        scale_x: 1.0,
        scale_y: 1.0,
        rotation: 0.0,
    };

    /// Identity scale and rotation at `(x, y)`.
    pub const fn at(x: f32, y: f32) -> Self {
        Transform {
            translate_x: x,
            translate_y: y,
            ..Self::IDENTITY
        }
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Which edits a layer refuses.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Lock {
    /// Pixel data (paint layers) cannot be changed.
    pub pixels: bool,
    /// Translation, scale and rotation cannot be changed.
    pub position: bool,
}

/// An edit was refused because of a [`Lock`] or the layer kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditError {
    PixelsLocked,
    PositionLocked,
    /// Pixel edits need a paint layer.
    NotAPaintLayer,
    /// Scale and rotation need an image layer.
    NotAnImageLayer,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EditError::PixelsLocked => "layer pixels are locked",
            EditError::PositionLocked => "layer position is locked",
            EditError::NotAPaintLayer => "not a paint layer",
            EditError::NotAnImageLayer => "not an image layer",
        })
    }
}

impl std::error::Error for EditError {}

/// A canvas-sized raster the user paints on, shifted by an integer offset.
/// Cloning shares the pixel buffer copy-on-write.
#[derive(Clone, PartialEq, Debug)]
pub struct PaintLayer {
    pub pixels: RgbaImage,
    /// Canvas position of the raster's top-left pixel.
    pub offset_x: i32,
    pub offset_y: i32,
}

/// An inserted image kept at full resolution, placed by a [`Transform`].
#[derive(Clone, PartialEq, Debug)]
pub struct ImageLayer {
    pub source: SourceHash,
    pub transform: Transform,
}

/// What a layer draws.
#[derive(Clone, PartialEq, Debug)]
pub enum LayerKind {
    Paint(PaintLayer),
    Image(ImageLayer),
}

/// One entry in the layer stack.
///
/// Fields are public so the project loader can rebuild layers exactly. Editor
/// code should change content through [`Layer::pixels_mut`],
/// [`Layer::translate_by`] and [`Layer::set_transform`], which honor
/// [`Lock`].
#[derive(Clone, PartialEq, Debug)]
pub struct Layer {
    pub(crate) id: LayerId,
    pub name: String,
    pub visible: bool,
    /// Opacity in percent, `0..=100`. Values above 100 are treated as 100.
    pub opacity: u8,
    pub blend: BlendMode,
    /// Optional canvas-sized grayscale mask, in **canvas** coordinates (it
    /// does not move with the layer). 255 shows the layer, 0 hides it. Pixels
    /// outside the mask's size are unmasked. Shared copy-on-write between
    /// clones, like paint pixels.
    pub mask: Option<GrayImage>,
    pub lock: Lock,
    pub kind: LayerKind,
}

impl Layer {
    /// A transparent paint layer of `canvas` size at offset 0.
    pub fn paint(name: impl Into<String>, canvas: Size) -> Self {
        Self::with_kind(
            name,
            LayerKind::Paint(PaintLayer {
                pixels: RgbaImage::new(canvas),
                offset_x: 0,
                offset_y: 0,
            }),
        )
    }

    /// An image layer showing `source` through `transform`.
    pub fn image(name: impl Into<String>, source: SourceHash, transform: Transform) -> Self {
        Self::with_kind(name, LayerKind::Image(ImageLayer { source, transform }))
    }

    /// Visible, 100% opacity, normal blend, no mask, unlocked.
    pub fn with_kind(name: impl Into<String>, kind: LayerKind) -> Self {
        Self {
            id: LayerId(0),
            name: name.into(),
            visible: true,
            opacity: 100,
            blend: BlendMode::Normal,
            mask: None,
            lock: Lock::default(),
            kind,
        }
    }

    /// Assigned when the layer is added to a document.
    pub fn id(&self) -> LayerId {
        self.id
    }

    /// Opacity as `0.0..=1.0`.
    pub fn opacity_unit(&self) -> f32 {
        self.opacity.min(100) as f32 / 100.0
    }

    /// Sets opacity in percent, clamped to 100.
    pub fn set_opacity(&mut self, percent: u8) {
        self.opacity = percent.min(100);
    }

    /// The paint raster, for editing. Refused if pixels are locked or this
    /// is not a paint layer.
    ///
    /// The raster is copy-on-write (see [`RgbaImage`]): if a history
    /// snapshot or a render job still shares it, the first write through the
    /// returned reference copies it (`Arc::make_mut`), leaving their copy
    /// untouched.
    pub fn pixels_mut(&mut self) -> Result<&mut RgbaImage, EditError> {
        if self.lock.pixels {
            return Err(EditError::PixelsLocked);
        }
        match &mut self.kind {
            LayerKind::Paint(p) => Ok(&mut p.pixels),
            LayerKind::Image(_) => Err(EditError::NotAPaintLayer),
        }
    }

    /// Moves the layer by `(dx, dy)` canvas pixels. Refused if position is
    /// locked. Paint layers move by whole pixels.
    pub fn translate_by(&mut self, dx: i32, dy: i32) -> Result<(), EditError> {
        if self.lock.position {
            return Err(EditError::PositionLocked);
        }
        match &mut self.kind {
            LayerKind::Paint(p) => {
                p.offset_x += dx;
                p.offset_y += dy;
            }
            LayerKind::Image(i) => {
                i.transform.translate_x += dx as f32;
                i.transform.translate_y += dy as f32;
            }
        }
        Ok(())
    }

    /// Replaces an image layer's transform. Refused if position is locked or
    /// this is not an image layer.
    pub fn set_transform(&mut self, transform: Transform) -> Result<(), EditError> {
        if self.lock.position {
            return Err(EditError::PositionLocked);
        }
        match &mut self.kind {
            LayerKind::Image(i) => {
                i.transform = transform;
                Ok(())
            }
            LayerKind::Paint(_) => Err(EditError::NotAnImageLayer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_layer_defaults() {
        let l = Layer::paint("bg", Size::new(4, 3));
        assert!(l.visible);
        assert_eq!(l.opacity, 100);
        assert_eq!(l.blend, BlendMode::Normal);
        assert!(l.mask.is_none());
        assert_eq!(l.lock, Lock::default());
        match &l.kind {
            LayerKind::Paint(p) => {
                assert_eq!(p.pixels.size(), Size::new(4, 3));
                assert!(p.pixels.as_raw().iter().all(|&b| b == 0));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn opacity_clamps() {
        let mut l = Layer::paint("a", Size::new(1, 1));
        l.set_opacity(250);
        assert_eq!(l.opacity, 100);
        l.opacity = 200;
        assert_eq!(l.opacity_unit(), 1.0);
        l.set_opacity(25);
        assert_eq!(l.opacity_unit(), 0.25);
    }

    #[test]
    fn pixel_lock_refuses_pixel_edits_only() {
        let mut l = Layer::paint("a", Size::new(2, 2));
        l.pixels_mut().unwrap().put(0, 0, [1, 2, 3, 255]);
        l.lock.pixels = true;
        assert_eq!(l.pixels_mut().unwrap_err(), EditError::PixelsLocked);
        assert_eq!(l.translate_by(1, 1), Ok(()));
    }

    #[test]
    fn position_lock_refuses_moves_only() {
        let mut l = Layer::image("img", SourceHash::new("ab"), Transform::IDENTITY);
        l.translate_by(3, -2).unwrap();
        match &l.kind {
            LayerKind::Image(i) => assert_eq!(i.transform, Transform::at(3.0, -2.0)),
            _ => panic!(),
        }
        l.lock.position = true;
        assert_eq!(l.translate_by(1, 0), Err(EditError::PositionLocked));
        assert_eq!(
            l.set_transform(Transform::IDENTITY),
            Err(EditError::PositionLocked)
        );

        let mut p = Layer::paint("p", Size::new(2, 2));
        p.translate_by(5, 6).unwrap();
        match &p.kind {
            LayerKind::Paint(pl) => assert_eq!((pl.offset_x, pl.offset_y), (5, 6)),
            _ => panic!(),
        }
        p.lock.position = true;
        assert_eq!(p.translate_by(1, 0), Err(EditError::PositionLocked));
        assert!(p.pixels_mut().is_ok());
    }

    #[test]
    fn edits_check_layer_kind() {
        let mut img = Layer::image("img", SourceHash::new("ab"), Transform::IDENTITY);
        assert_eq!(img.pixels_mut().unwrap_err(), EditError::NotAPaintLayer);
        let mut p = Layer::paint("p", Size::new(1, 1));
        assert_eq!(
            p.set_transform(Transform::IDENTITY),
            Err(EditError::NotAnImageLayer)
        );
    }
}
