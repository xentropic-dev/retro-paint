//! GPUI upload and painting of the flattened canvas. Thin: the pixel work is
//! in [`crate::canvas_image`], the geometry in [`crate::viewport`].
//!
//! The document is flattened only when [`EditorState::doc_version`] changes.
//! Images are handed to GPUI as BGRA [`RenderImage`]s; replaced ones are
//! removed from the sprite atlas on the next paint.

use std::sync::Arc;

use gpui::{Bounds, Corners, Pixels, RenderImage, Window, fill, point, px, rgb, size};

use crate::canvas_image::{
    CHECKER, SharpTile, checkerboard_bgra, plan_sharp_tile, rgba_to_bgra, upscale_tile,
};
use crate::state::EditorState;
use crate::viewport::Vec2;

/// Canvas-area surround color.
pub(crate) const SURROUND: u32 = 0x161616;

/// GPU images for the canvas, rebuilt only when their inputs change.
#[derive(Default)]
pub(crate) struct CanvasCache {
    version: Option<u64>,
    width: u32,
    bgra: Vec<u8>,
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
    image: Arc<RenderImage>,
    image_bounds: Bounds<Pixels>,
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
    /// Brings the images up to date for `state` drawn into `area` (window
    /// coordinates) at `scale` device pixels per logical pixel.
    pub(crate) fn prepare(
        &mut self,
        state: &EditorState,
        area: Bounds<Pixels>,
        scale: f32,
    ) -> CanvasPaint {
        if self.version != Some(state.doc_version) {
            let flat = state.flatten();
            self.width = flat.width();
            let height = flat.height();
            self.bgra = rgba_to_bgra(&flat);
            let full = render_image(self.width, height, self.bgra.clone());
            self.stale.extend(self.full.replace(full));
            self.stale.extend(self.tile.take().map(|(_, img)| img));
            self.version = Some(state.doc_version);
        }

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

        let vp = &state.viewport;
        let ox = f32::from(area.origin.x);
        let oy = f32::from(area.origin.y);
        let to_window = |c: Vec2| {
            let s = vp.canvas_to_screen(c);
            (s.x + ox, s.y + oy)
        };
        let zoom = vp.zoom();
        let canvas = vp.canvas_size();
        let canvas_bounds = rect(to_window(Vec2::default()), canvas.x * zoom, canvas.y * zoom);

        let (image, image_bounds) = match plan_sharp_tile(vp, scale) {
            Some(plan) => {
                if self.tile.as_ref().map(|(t, _)| *t) != Some(plan) {
                    let (w, h) = plan.image_size();
                    let img = render_image(w, h, upscale_tile(&self.bgra, self.width, &plan));
                    self.stale
                        .extend(self.tile.replace((plan, img)).map(|(_, i)| i));
                }
                let origin = to_window(Vec2::new(plan.x as f32, plan.y as f32));
                let bounds = rect(origin, plan.width as f32 * zoom, plan.height as f32 * zoom);
                (self.tile.as_ref().unwrap().1.clone(), bounds)
            }
            None => {
                self.stale.extend(self.tile.take().map(|(_, i)| i));
                (self.full.clone().unwrap(), canvas_bounds)
            }
        };

        CanvasPaint {
            area,
            visible: canvas_bounds.intersect(&area),
            checker: self.checker.as_ref().unwrap().1.clone(),
            image,
            image_bounds,
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
        let _ = window.paint_image(
            visible,
            p.image_bounds,
            Corners::default(),
            p.image,
            0,
            false,
        );
    }
    for img in p.stale {
        let _ = window.drop_image(img);
    }
}
