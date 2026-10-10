//! GPUI upload and painting of the flattened canvas. Thin: the pixel work is
//! in [`crate::canvas_image`], the geometry in [`crate::viewport`].
//!
//! The view never flattens: the render worker hands it each flattened
//! document as a BGRA [`CanvasFrame`] ([`CanvasCache::accept`]), and the
//! cache only reads the viewport. Until the first frame arrives only the
//! checkerboard is drawn; while a render is in flight the previous image
//! stays up. Images are handed to GPUI as [`RenderImage`]s (moving the
//! buffer, no copy); replaced ones are removed from the sprite atlas on the
//! next paint.

use std::sync::Arc;

use gpui::{Bounds, Corners, Pixels, RenderImage, Window, fill, point, px, rgb, size};

use crate::canvas_image::{CHECKER, SharpTile, checkerboard_bgra, plan_sharp_tile, upscale_tile};
use crate::paint_ops::FrameView;
use crate::render_worker::CanvasFrame;
use crate::viewport::{Vec2, Viewport};

/// Canvas-area surround color.
pub(crate) const SURROUND: u32 = 0x161616;

/// GPU images for the canvas, rebuilt only when their inputs change.
#[derive(Default)]
pub(crate) struct CanvasCache {
    /// `doc_version` of the displayed image.
    version: Option<u64>,
    width: u32,
    height: u32,
    /// The flattened document (its BGRA buffer is read for sharp tiles).
    full: Option<Arc<RenderImage>>,
    tile: Option<(SharpTile, Arc<RenderImage>)>,
    checker: Option<((u32, u32, u32), Arc<RenderImage>)>,
    stale: Vec<Arc<RenderImage>>,
}

/// Everything [`paint`] needs for one frame.
pub(crate) struct CanvasPaint {
    area: Bounds<Pixels>,
    /// Visible part of the canvas, in window coordinates.
    visible: Bounds<Pixels>,
    checker: Arc<RenderImage>,
    /// The canvas image and where to draw it; `None` before the first frame.
    image: Option<(Arc<RenderImage>, Bounds<Pixels>)>,
    stale: Vec<Arc<RenderImage>>,
}

fn render_image(width: u32, height: u32, bgra: Vec<u8>) -> Arc<RenderImage> {
    let buf = image::RgbaImage::from_raw(width, height, bgra).expect("buffer matches size");
    Arc::new(RenderImage::new(vec![image::Frame::new(buf)]))
}

fn rect(origin: Point2, w: f32, h: f32) -> Bounds<Pixels> {
    Bounds::new(point(px(origin.0), px(origin.1)), size(px(w), px(h)))
}

type Point2 = (f32, f32);

impl CanvasCache {
    /// `doc_version` of the displayed image, if any.
    #[cfg(test)]
    pub(crate) fn version(&self) -> Option<u64> {
        self.version
    }

    /// The displayed flattened document, for the Picker.
    pub(crate) fn frame_view(&self) -> Option<FrameView<'_>> {
        let full = self.full.as_ref()?;
        Some(FrameView {
            width: self.width,
            height: self.height,
            bgra: full.as_bytes(0)?,
        })
    }

    /// Takes a flattened document from the render worker. Frames not newer
    /// than the displayed one are dropped. Returns whether the image changed
    /// (the view should repaint).
    pub(crate) fn accept(&mut self, frame: CanvasFrame) -> bool {
        if self.version.is_some_and(|v| frame.version <= v) {
            return false;
        }
        let full = render_image(frame.width, frame.height, frame.bgra);
        self.stale.extend(self.full.replace(full));
        self.stale.extend(self.tile.take().map(|(_, img)| img));
        self.width = frame.width;
        self.height = frame.height;
        self.version = Some(frame.version);
        true
    }

    /// Brings the images up to date for `vp` drawn into `area` (window
    /// coordinates) at `scale` device pixels per logical pixel. Reads only
    /// the viewport, never the document.
    pub(crate) fn prepare(
        &mut self,
        vp: &Viewport,
        area: Bounds<Pixels>,
        scale: f32,
    ) -> CanvasPaint {
        let area_w = f32::from(area.size.width);
        let area_h = f32::from(area.size.height);
        let dev_w = (area_w * scale).ceil().max(1.0) as u32;
        let dev_h = (area_h * scale).ceil().max(1.0) as u32;
        let square = ((CHECKER as f32) * scale).round().max(1.0) as u32;
        let key = (dev_w, dev_h, square);
        if self.checker.as_ref().map(|(k, _)| *k) != Some(key) {
            let img = render_image(dev_w, dev_h, checkerboard_bgra(dev_w, dev_h, square));
            self.stale
                .extend(self.checker.replace((key, img)).map(|(_, img)| img));
        }

        let ox = f32::from(area.origin.x);
        let oy = f32::from(area.origin.y);
        let to_window = |c: Vec2| {
            let s = vp.canvas_to_screen(c);
            (s.x + ox, s.y + oy)
        };
        let zoom = vp.zoom();
        let canvas = vp.canvas_size();
        let canvas_bounds = rect(to_window(Vec2::default()), canvas.x * zoom, canvas.y * zoom);

        // The image may lag the viewport (e.g. a New with another canvas
        // size, still rendering): draw it at its own size, and cut sharp
        // tiles only when it matches the canvas the tile is planned for.
        let matches_canvas = (self.width as f32, self.height as f32) == (canvas.x, canvas.y);
        let plan = plan_sharp_tile(vp, scale).filter(|_| matches_canvas);
        let image = match (&self.full, plan) {
            (None, _) => None,
            (Some(full), Some(plan)) => {
                if self.tile.as_ref().map(|(t, _)| *t) != Some(plan) {
                    let (w, h) = plan.image_size();
                    let src = full.as_bytes(0).expect("canvas image has one frame");
                    let img = render_image(w, h, upscale_tile(src, self.width, &plan));
                    self.stale
                        .extend(self.tile.replace((plan, img)).map(|(_, i)| i));
                }
                let origin = to_window(Vec2::new(plan.x as f32, plan.y as f32));
                let bounds = rect(origin, plan.width as f32 * zoom, plan.height as f32 * zoom);
                Some((self.tile.as_ref().unwrap().1.clone(), bounds))
            }
            (Some(full), None) => {
                self.stale.extend(self.tile.take().map(|(_, i)| i));
                let bounds = rect(
                    to_window(Vec2::default()),
                    self.width as f32 * zoom,
                    self.height as f32 * zoom,
                );
                Some((full.clone(), bounds))
            }
        };

        CanvasPaint {
            area,
            visible: canvas_bounds.intersect(&area),
            checker: self.checker.as_ref().unwrap().1.clone(),
            image,
            stale: std::mem::take(&mut self.stale),
        }
    }
}

/// Dark surround, transparency checkerboard under the canvas, then the canvas.
pub(crate) fn paint(p: CanvasPaint, window: &mut Window) {
    window.paint_quad(fill(p.area, rgb(SURROUND)));
    let visible = p.visible;
    if visible.size.width > px(0.) && visible.size.height > px(0.) {
        // Errors only come from atlas allocation; skip the frame's canvas then.
        let _ = window.paint_image(visible, p.area, Corners::default(), p.checker, 0, false);
        if let Some((image, bounds)) = p.image {
            let _ = window.paint_image(visible, bounds, Corners::default(), image, 0, false);
        }
    }
    for img in p.stale {
        let _ = window.drop_image(img);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn frame(version: u64, w: u32, h: u32, bgra: [u8; 4]) -> CanvasFrame {
        CanvasFrame {
            version,
            width: w,
            height: h,
            bgra: bgra.repeat((w * h) as usize),
            requested: Instant::now(),
        }
    }

    fn area() -> Bounds<Pixels> {
        rect((0.0, 0.0), 400.0, 300.0)
    }

    fn viewport(w: u32, h: u32, zoom: f32) -> Viewport {
        let mut vp = Viewport::new(w, h);
        vp.set_view_size(400.0, 300.0);
        vp.zoom_about(zoom, Vec2::default());
        vp
    }

    fn pixel(img: &RenderImage) -> [u8; 4] {
        img.as_bytes(0).unwrap()[..4].try_into().unwrap()
    }

    #[test]
    fn checkerboard_only_before_the_first_frame() {
        let mut cache = CanvasCache::default();
        let p = cache.prepare(&viewport(8, 8, 1.0), area(), 1.0);
        assert!(p.image.is_none());
        assert_eq!(cache.version(), None);
    }

    #[test]
    fn newer_frames_replace_and_stale_ones_are_dropped() {
        let mut cache = CanvasCache::default();
        let vp = viewport(8, 8, 1.0);
        assert!(cache.accept(frame(2, 8, 8, [1, 2, 3, 255])));
        let p = cache.prepare(&vp, area(), 1.0);
        assert_eq!(pixel(&p.image.unwrap().0), [1, 2, 3, 255]);
        // The Picker sees the same frame, as RGBA.
        let f = cache.frame_view().unwrap();
        assert_eq!(f.sample(Vec2::new(7.5, 7.5)), Some([3, 2, 1, 255]));

        assert!(!cache.accept(frame(1, 8, 8, [9, 9, 9, 255])), "older");
        assert!(
            !cache.accept(frame(2, 8, 8, [9, 9, 9, 255])),
            "same version"
        );
        let p = cache.prepare(&vp, area(), 1.0);
        assert_eq!(pixel(&p.image.unwrap().0), [1, 2, 3, 255]);
        assert!(p.stale.is_empty());

        assert!(cache.accept(frame(5, 8, 8, [4, 5, 6, 255])));
        assert_eq!(cache.version(), Some(5));
        let p = cache.prepare(&vp, area(), 1.0);
        assert_eq!(pixel(&p.image.unwrap().0), [4, 5, 6, 255]);
        assert_eq!(p.stale.len(), 1, "the replaced image is freed");
    }

    #[test]
    fn zoomed_in_draws_a_sharp_tile_cut_from_the_frame() {
        let mut cache = CanvasCache::default();
        cache.accept(frame(1, 8, 8, [7, 8, 9, 255]));
        let p = cache.prepare(&viewport(8, 8, 4.0), area(), 1.0);
        let (img, _) = p.image.unwrap();
        assert_eq!(img.size(0).width.0, 8 * 4);
        assert_eq!(pixel(&img), [7, 8, 9, 255]);
    }

    #[test]
    fn a_frame_of_another_size_is_drawn_whole_at_its_own_size() {
        // The viewport already shows a 16x16 canvas while the displayed
        // frame is still the old 8x8 document: no tile, own-size bounds.
        let mut cache = CanvasCache::default();
        cache.accept(frame(1, 8, 8, [7, 8, 9, 255]));
        let p = cache.prepare(&viewport(16, 16, 4.0), area(), 1.0);
        let (img, bounds) = p.image.unwrap();
        assert_eq!(img.size(0).width.0, 8);
        assert_eq!(f32::from(bounds.size.width), 8.0 * 4.0);
    }
}
