//! The editor canvas viewport: zoom, pan, clamping, and the mapping between
//! screen and canvas coordinates. Pure Rust, no GPUI types.
//!
//! "Screen" coordinates are logical pixels relative to the top-left corner of
//! the canvas area (the view subtracts the area's window origin). "Canvas"
//! coordinates are document pixels; pixel `(x, y)` covers `[x, x+1) × [y, y+1)`.
//!
//! The mapping is `screen = offset + canvas * zoom`.

/// A 2-D point or vector in either coordinate space.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Smallest zoom (12.5%).
pub const MIN_ZOOM: f32 = 0.125;
/// Largest zoom (3200%).
pub const MAX_ZOOM: f32 = 32.0;

/// Zoom-in / zoom-out stops, as scale factors.
pub const ZOOM_STEPS: [f32; 16] = [
    0.125,
    0.25,
    1.0 / 3.0,
    0.5,
    2.0 / 3.0,
    1.0,
    1.5,
    2.0,
    3.0,
    4.0,
    6.0,
    8.0,
    12.0,
    16.0,
    24.0,
    32.0,
];

/// Screen pixels of padding around the canvas when fitting it to the view.
pub const FIT_PADDING: f32 = 24.0;

/// At least this many screen pixels of the canvas (or the whole canvas, if it
/// is smaller on screen) stay inside the view on each axis, so the canvas can
/// never be scrolled out of sight.
pub const MIN_VISIBLE: f32 = 48.0;

/// Screen pixels panned per scroll-wheel line.
pub const LINE_PX: f32 = 48.0;

/// Relative zoom change per pixel of smooth (touchpad) ctrl+scroll.
const SMOOTH_ZOOM_PER_PX: f32 = 0.01;

/// Zoom and scroll position of the canvas inside the canvas area.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Viewport {
    /// Canvas size in document pixels.
    canvas: Vec2,
    /// Canvas area size in screen pixels.
    view: Vec2,
    /// Screen pixels per canvas pixel.
    zoom: f32,
    /// Screen position of the canvas origin.
    offset: Vec2,
}

impl Viewport {
    /// A viewport for a `width × height` canvas with no view size yet, at
    /// 100% with the canvas at the view origin. Call [`Viewport::set_view_size`]
    /// and [`Viewport::fit`] once the canvas area has been laid out.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            canvas: Vec2::new(width as f32, height as f32),
            view: Vec2::default(),
            zoom: 1.0,
            offset: Vec2::default(),
        }
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    pub fn offset(&self) -> Vec2 {
        self.offset
    }

    pub fn view_size(&self) -> Vec2 {
        self.view
    }

    pub fn canvas_size(&self) -> Vec2 {
        self.canvas
    }

    /// Zoom as a whole percentage for display.
    pub fn zoom_percent(&self) -> u32 {
        (self.zoom * 100.0).round() as u32
    }

    /// Replaces the canvas size (e.g. a new document) without moving the view.
    pub fn set_canvas_size(&mut self, width: u32, height: u32) {
        self.canvas = Vec2::new(width as f32, height as f32);
        self.clamp();
    }

    /// Records the canvas area's size. Keeps the zoom and offset (re-clamped).
    pub fn set_view_size(&mut self, width: f32, height: f32) {
        self.view = Vec2::new(width.max(0.0), height.max(0.0));
        self.clamp();
    }

    /// Canvas → screen.
    pub fn canvas_to_screen(&self, c: Vec2) -> Vec2 {
        Vec2::new(
            self.offset.x + c.x * self.zoom,
            self.offset.y + c.y * self.zoom,
        )
    }

    /// Screen → canvas. The result may lie outside the canvas.
    pub fn screen_to_canvas(&self, s: Vec2) -> Vec2 {
        Vec2::new(
            (s.x - self.offset.x) / self.zoom,
            (s.y - self.offset.y) / self.zoom,
        )
    }

    /// The canvas pixel under screen point `s`, or `None` outside the canvas.
    pub fn pixel_at(&self, s: Vec2) -> Option<(u32, u32)> {
        let c = self.screen_to_canvas(s);
        let (x, y) = (c.x.floor(), c.y.floor());
        (x >= 0.0 && y >= 0.0 && x < self.canvas.x && y < self.canvas.y)
            .then_some((x as u32, y as u32))
    }

    /// Screen rectangle of the whole canvas: `(origin, size)`.
    pub fn canvas_rect(&self) -> (Vec2, Vec2) {
        (
            self.offset,
            Vec2::new(self.canvas.x * self.zoom, self.canvas.y * self.zoom),
        )
    }

    /// The part of the canvas inside the view, in canvas coordinates, as
    /// `(min, max)`; `None` if nothing is visible.
    pub fn visible_canvas_rect(&self) -> Option<(Vec2, Vec2)> {
        let a = self.screen_to_canvas(Vec2::default());
        let b = self.screen_to_canvas(self.view);
        let min = Vec2::new(a.x.max(0.0), a.y.max(0.0));
        let max = Vec2::new(b.x.min(self.canvas.x), b.y.min(self.canvas.y));
        (min.x < max.x && min.y < max.y).then_some((min, max))
    }

    /// Zoom that fits the whole canvas in the view with [`FIT_PADDING`].
    pub fn fit_zoom(&self) -> f32 {
        if self.canvas.x <= 0.0 || self.canvas.y <= 0.0 {
            return 1.0;
        }
        let avail_w = (self.view.x - 2.0 * FIT_PADDING).max(1.0);
        let avail_h = (self.view.y - 2.0 * FIT_PADDING).max(1.0);
        (avail_w / self.canvas.x)
            .min(avail_h / self.canvas.y)
            .clamp(MIN_ZOOM, MAX_ZOOM)
    }

    /// Fits the canvas to the view and centers it.
    pub fn fit(&mut self) {
        self.zoom = self.fit_zoom();
        self.center();
    }

    /// Centers the canvas in the view at the current zoom.
    pub fn center(&mut self) {
        self.offset = Vec2::new(
            (self.view.x - self.canvas.x * self.zoom) / 2.0,
            (self.view.y - self.canvas.y * self.zoom) / 2.0,
        );
        self.clamp();
    }

    /// Sets the zoom (clamped to [`MIN_ZOOM`]..=[`MAX_ZOOM`]) keeping the
    /// canvas point under screen point `anchor` fixed, then clamps the pan.
    pub fn zoom_about(&mut self, zoom: f32, anchor: Vec2) {
        let c = self.screen_to_canvas(anchor);
        self.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        self.offset = Vec2::new(anchor.x - c.x * self.zoom, anchor.y - c.y * self.zoom);
        self.clamp();
    }

    /// Zooms to the next stop above the current zoom, about `anchor`.
    pub fn zoom_in(&mut self, anchor: Vec2) {
        self.zoom_about(step_zoom(self.zoom, true), anchor);
    }

    /// Zooms to the next stop below the current zoom, about `anchor`.
    pub fn zoom_out(&mut self, anchor: Vec2) {
        self.zoom_about(step_zoom(self.zoom, false), anchor);
    }

    /// Zoom about the view center.
    pub fn zoom_in_centered(&mut self) {
        self.zoom_in(self.view_center());
    }

    pub fn zoom_out_centered(&mut self) {
        self.zoom_out(self.view_center());
    }

    /// Sets 100% about the view center.
    pub fn actual_size(&mut self) {
        self.zoom_about(1.0, self.view_center());
    }

    fn view_center(&self) -> Vec2 {
        Vec2::new(self.view.x / 2.0, self.view.y / 2.0)
    }

    /// Moves the canvas by `(dx, dy)` screen pixels, then clamps.
    pub fn pan_by(&mut self, dx: f32, dy: f32) {
        self.offset.x += dx;
        self.offset.y += dy;
        self.clamp();
    }

    /// Plain scroll wheel: pans. `lines` is true for line-based (mouse wheel)
    /// deltas, false for pixel (touchpad) deltas. Positive `dy` scrolls up,
    /// which moves the canvas down.
    pub fn scroll(&mut self, dx: f32, dy: f32, lines: bool) {
        let k = if lines { LINE_PX } else { 1.0 };
        self.pan_by(dx * k, dy * k);
    }

    /// Ctrl + scroll wheel: zooms about `anchor`. Line deltas move one zoom
    /// stop per notch; pixel deltas zoom smoothly. Positive `dy` zooms in.
    pub fn scroll_zoom(&mut self, dy: f32, lines: bool, anchor: Vec2) {
        if dy == 0.0 {
            return;
        }
        if lines {
            let notches = dy.abs().round().max(1.0) as u32;
            for _ in 0..notches {
                if dy > 0.0 {
                    self.zoom_in(anchor);
                } else {
                    self.zoom_out(anchor);
                }
            }
        } else {
            let factor = (1.0 + SMOOTH_ZOOM_PER_PX).powf(dy);
            self.zoom_about(self.zoom * factor, anchor);
        }
    }

    /// Keeps at least [`MIN_VISIBLE`] screen pixels of the canvas (or all of
    /// it, if smaller) inside the view on each axis. A no-op until the view
    /// has a size.
    fn clamp(&mut self) {
        if self.view.x <= 0.0 || self.view.y <= 0.0 {
            return;
        }
        self.offset.x = clamp_axis(self.offset.x, self.canvas.x * self.zoom, self.view.x);
        self.offset.y = clamp_axis(self.offset.y, self.canvas.y * self.zoom, self.view.y);
    }
}

fn clamp_axis(offset: f32, extent: f32, view: f32) -> f32 {
    let keep = MIN_VISIBLE.min(extent).min(view);
    offset.clamp(keep - extent, view - keep)
}

/// The next zoom stop strictly above (`up`) or below `zoom`, or the limit.
pub fn step_zoom(zoom: f32, up: bool) -> f32 {
    const EPS: f32 = 1e-4;
    if up {
        ZOOM_STEPS
            .iter()
            .copied()
            .find(|&s| s > zoom * (1.0 + EPS))
            .unwrap_or(MAX_ZOOM)
    } else {
        ZOOM_STEPS
            .iter()
            .rev()
            .copied()
            .find(|&s| s < zoom * (1.0 - EPS))
            .unwrap_or(MIN_ZOOM)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec2, b: Vec2, eps: f32) -> bool {
        (a.x - b.x).abs() <= eps && (a.y - b.y).abs() <= eps
    }

    /// 800×1280 canvas in a 1000×700 view, fitted.
    fn fitted() -> Viewport {
        let mut v = Viewport::new(800, 1280);
        v.set_view_size(1000.0, 700.0);
        v.fit();
        v
    }

    #[test]
    fn identity_mapping_at_100_percent_origin() {
        let v = Viewport::new(100, 100);
        let p = Vec2::new(12.5, 40.0);
        assert_eq!(v.canvas_to_screen(p), p);
        assert_eq!(v.screen_to_canvas(p), p);
    }

    #[test]
    fn mapping_follows_offset_and_zoom() {
        let mut v = Viewport::new(800, 1280);
        v.set_view_size(1000.0, 700.0);
        v.zoom_about(2.0, Vec2::default());
        v.pan_by(-30.0, -50.0);
        let o = v.offset();
        assert_eq!(v.zoom(), 2.0);
        assert_eq!(
            v.canvas_to_screen(Vec2::new(10.0, 20.0)),
            Vec2::new(o.x + 20.0, o.y + 40.0)
        );
        assert_eq!(v.screen_to_canvas(o), Vec2::default());
    }

    #[test]
    fn round_trips_at_many_zooms_and_offsets() {
        for &zoom in &[0.125, 0.3, 1.0 / 3.0, 1.0, 1.5, 7.0, 32.0] {
            for &(dx, dy) in &[(0.0, 0.0), (-500.0, 120.0), (333.3, -77.7)] {
                let mut v = Viewport::new(800, 1280);
                v.set_view_size(1000.0, 700.0);
                v.zoom_about(zoom, Vec2::new(500.0, 350.0));
                v.pan_by(dx, dy);
                for &(x, y) in &[(0.0, 0.0), (17.25, 999.5), (799.0, 1279.0), (-40.0, 3000.0)] {
                    let c = Vec2::new(x, y);
                    let back = v.screen_to_canvas(v.canvas_to_screen(c));
                    assert!(close(back, c, 1e-3), "zoom {zoom}: {c:?} -> {back:?}");
                    let s = Vec2::new(x * 0.7, y * 0.3);
                    let back = v.canvas_to_screen(v.screen_to_canvas(s));
                    assert!(close(back, s, 1e-2), "zoom {zoom}: {s:?} -> {back:?}");
                }
            }
        }
    }

    #[test]
    fn zoom_about_keeps_the_anchor_point_fixed() {
        for &anchor in &[
            Vec2::new(500.0, 350.0),
            Vec2::new(10.0, 690.0),
            Vec2::new(812.5, 3.0),
        ] {
            for &target in &ZOOM_STEPS {
                let mut v = fitted();
                v.zoom_about(4.0, Vec2::new(500.0, 350.0));
                let before = v.screen_to_canvas(anchor);
                v.zoom_about(target, anchor);
                // Clamping may move the canvas when it gets small on screen;
                // the invariant must hold whenever the canvas covers the view.
                let (o, s) = v.canvas_rect();
                if o.x <= 0.0 && o.y <= 0.0 && o.x + s.x >= 1000.0 && o.y + s.y >= 700.0 {
                    let after = v.screen_to_canvas(anchor);
                    assert!(close(before, after, 1e-3), "{target}: {before:?} vs {after:?}");
                }
                assert_eq!(v.zoom(), target.clamp(MIN_ZOOM, MAX_ZOOM));
            }
        }
    }

    #[test]
    fn zoom_in_then_out_about_cursor_returns_to_same_view() {
        let mut v = fitted();
        v.zoom_about(2.0, Vec2::new(500.0, 350.0));
        let start = v;
        let cursor = Vec2::new(321.0, 123.0);
        v.zoom_in(cursor);
        assert_eq!(v.zoom(), 3.0);
        v.zoom_out(cursor);
        assert_eq!(v.zoom(), 2.0);
        assert!(close(v.offset(), start.offset(), 1e-3));
    }

    #[test]
    fn fit_shows_whole_canvas_centered() {
        let v = fitted();
        let expected = (700.0 - 2.0 * FIT_PADDING) / 1280.0;
        assert!((v.zoom() - expected).abs() < 1e-6);
        let (o, s) = v.canvas_rect();
        assert!(o.x >= 0.0 && o.y >= FIT_PADDING - 1e-3);
        assert!(o.x + s.x <= 1000.0 && o.y + s.y <= 700.0 - FIT_PADDING + 1e-3);
        assert!(((o.x + s.x / 2.0) - 500.0).abs() < 1e-3);
        assert!(((o.y + s.y / 2.0) - 350.0).abs() < 1e-3);
    }

    #[test]
    fn fit_of_tiny_canvas_can_zoom_in_up_to_max() {
        let mut v = Viewport::new(2, 2);
        v.set_view_size(1000.0, 700.0);
        v.fit();
        assert_eq!(v.zoom(), MAX_ZOOM);
    }

    #[test]
    fn zoom_steps_cover_12_5_to_3200_and_increase() {
        assert_eq!(ZOOM_STEPS[0], MIN_ZOOM);
        assert_eq!(*ZOOM_STEPS.last().unwrap(), MAX_ZOOM);
        assert!(ZOOM_STEPS.windows(2).all(|w| w[0] < w[1]));
        assert!(ZOOM_STEPS.contains(&1.0));
    }

    #[test]
    fn step_zoom_walks_the_stops_and_saturates() {
        let mut z = MIN_ZOOM;
        let mut seen = vec![z];
        while z < MAX_ZOOM {
            z = step_zoom(z, true);
            seen.push(z);
        }
        assert_eq!(seen.len(), 16);
        assert_eq!(step_zoom(MAX_ZOOM, true), MAX_ZOOM);
        assert_eq!(step_zoom(MIN_ZOOM, false), MIN_ZOOM);
        // From an off-stop zoom (e.g. fit) go to the neighbours.
        assert_eq!(step_zoom(0.52, true), 2.0 / 3.0);
        assert_eq!(step_zoom(0.52, false), 0.5);
        assert_eq!(step_zoom(1.0, false), 2.0 / 3.0);
    }

    #[test]
    fn zoom_is_clamped() {
        let mut v = fitted();
        v.zoom_about(1000.0, Vec2::new(10.0, 10.0));
        assert_eq!(v.zoom(), MAX_ZOOM);
        v.zoom_about(0.0001, Vec2::new(10.0, 10.0));
        assert_eq!(v.zoom(), MIN_ZOOM);
    }

    #[test]
    fn panning_cannot_lose_the_canvas() {
        let mut v = fitted();
        v.zoom_about(4.0, Vec2::new(500.0, 350.0));
        v.pan_by(1e6, 1e6);
        let (o, _) = v.canvas_rect();
        assert_eq!(o, Vec2::new(1000.0 - MIN_VISIBLE, 700.0 - MIN_VISIBLE));
        v.pan_by(-1e7, -1e7);
        let (o, s) = v.canvas_rect();
        assert_eq!(o.x + s.x, MIN_VISIBLE);
        assert_eq!(o.y + s.y, MIN_VISIBLE);
    }

    #[test]
    fn small_canvas_stays_fully_inside_view() {
        let mut v = Viewport::new(10, 10);
        v.set_view_size(1000.0, 700.0);
        v.zoom_about(1.0, Vec2::default());
        v.pan_by(5000.0, -5000.0);
        let (o, s) = v.canvas_rect();
        assert_eq!(o, Vec2::new(990.0, 0.0));
        assert_eq!(s, Vec2::new(10.0, 10.0));
    }

    #[test]
    fn wheel_pans_and_ctrl_wheel_zooms() {
        let mut v = fitted();
        v.zoom_about(2.0, Vec2::new(500.0, 350.0));
        let o = v.offset();
        v.scroll(0.0, -1.0, true);
        assert_eq!(v.offset(), Vec2::new(o.x, o.y - LINE_PX));
        v.scroll(-3.0, 0.0, false);
        assert_eq!(v.offset(), Vec2::new(o.x - 3.0, o.y - LINE_PX));

        let anchor = Vec2::new(400.0, 300.0);
        let c = v.screen_to_canvas(anchor);
        v.scroll_zoom(2.0, true, anchor);
        assert_eq!(v.zoom(), 4.0);
        assert!(close(v.screen_to_canvas(anchor), c, 1e-3));
        v.scroll_zoom(-1.0, true, anchor);
        assert_eq!(v.zoom(), 3.0);
        v.scroll_zoom(10.0, false, anchor);
        assert!(v.zoom() > 3.0 && v.zoom() < 3.5);
        assert!(close(v.screen_to_canvas(anchor), c, 1e-3));
    }

    #[test]
    fn pixel_at_floors_and_rejects_outside() {
        let mut v = Viewport::new(800, 1280);
        v.set_view_size(1000.0, 700.0);
        v.zoom_about(4.0, Vec2::default());
        let o = v.offset();
        assert_eq!(v.pixel_at(Vec2::new(o.x + 0.5, o.y + 3.9)), Some((0, 0)));
        assert_eq!(v.pixel_at(Vec2::new(o.x + 4.0, o.y + 8.0)), Some((1, 2)));
        assert_eq!(v.pixel_at(Vec2::new(o.x - 0.1, o.y)), None);
        let mut v = Viewport::new(800, 1280);
        v.set_view_size(1000.0, 700.0);
        v.fit();
        let (o, s) = v.canvas_rect();
        assert_eq!(v.pixel_at(Vec2::new(o.x + s.x + 1.0, o.y)), None);
        assert_eq!(
            v.pixel_at(Vec2::new(o.x + s.x - 0.01, o.y + s.y - 0.01)),
            Some((799, 1279))
        );
    }

    #[test]
    fn visible_rect_is_view_intersected_with_canvas() {
        let v = fitted();
        let (min, max) = v.visible_canvas_rect().unwrap();
        assert!(close(min, Vec2::default(), 1e-3));
        assert!(close(max, Vec2::new(800.0, 1280.0), 1e-3));

        let mut v = Viewport::new(800, 1280);
        v.set_view_size(1000.0, 700.0);
        v.zoom_about(2.0, Vec2::default());
        v.pan_by(-100.0, -200.0);
        let (min, max) = v.visible_canvas_rect().unwrap();
        assert!(close(min, Vec2::new(50.0, 100.0), 1e-3));
        assert!(close(max, Vec2::new(550.0, 450.0), 1e-3));
    }

    #[test]
    fn resize_keeps_zoom_and_reclamps() {
        let mut v = fitted();
        let z = v.zoom();
        v.set_view_size(200.0, 100.0);
        assert_eq!(v.zoom(), z);
        let (o, s) = v.canvas_rect();
        assert!(o.x <= 200.0 - MIN_VISIBLE.min(s.x) && o.y <= 100.0 - MIN_VISIBLE);
    }
}
