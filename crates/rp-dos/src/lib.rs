//! The DOS view: screen composer, raylib window, and CRT shader.
//!
//! The composer ([`compose`]) is pure: it turns an [`rp_doc::FrameMsg`] into
//! the indexed 320×200 / 200×320 DOS screen. See docs/aesthetics.md.
//!
//! Spike state: the window still takes a temporary [`FrameMsg`] over a
//! latest-only channel and draws the indexed frame through the CGA palette
//! with nearest-neighbor integer scaling; `dos-window` switches it to
//! `rp_doc::FrameMsg` + [`compose`] + CRT.

use std::sync::{Arc, Mutex};

use rp_palette_dither::Rgb;

mod compose;
pub mod crt;
mod glyphs;
pub mod sample;
mod screen;
mod scroll;
mod window;

pub use compose::{canvas_viewport, compose};
pub use crt::{CrtParams, tube_rect};
pub use screen::{IndexedScreen, Rect};
pub use scroll::{SCROLL_MARGIN, ScrollState};
pub use window::{run_crt_demo, run_window};

/// Wayland app_id (and X11 class) of the DOS window.
pub const APP_ID: &str = "retro-paint-dos";

/// Temporary frame message: one palette index per pixel, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameMsg {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Sending half of a latest-only channel. Sending replaces any unread frame.
#[derive(Clone)]
pub struct FrameSender(Arc<Mutex<Option<FrameMsg>>>);

/// Receiving half of a latest-only channel.
pub struct FrameReceiver(Arc<Mutex<Option<FrameMsg>>>);

/// A channel that keeps only the newest frame; stale frames are dropped.
pub fn latest_channel() -> (FrameSender, FrameReceiver) {
    let slot = Arc::new(Mutex::new(None));
    (FrameSender(slot.clone()), FrameReceiver(slot))
}

impl FrameSender {
    pub fn send(&self, frame: FrameMsg) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame);
    }
}

impl FrameReceiver {
    /// Takes the newest frame, if one arrived since the last call.
    pub fn try_recv(&self) -> Option<FrameMsg> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// Expands indexed pixels to RGBA8 through `palette`. Out-of-range indices draw black.
pub fn indexed_to_rgba(pixels: &[u8], palette: &[Rgb], out: &mut Vec<u8>) {
    out.clear();
    out.extend(pixels.iter().flat_map(|&i| {
        let [r, g, b] = palette.get(i as usize).map_or([0, 0, 0], |c| c.to_array());
        [r, g, b, 0xFF]
    }));
}

/// Largest whole-number scale of `src` that fits in `dst` (at least 1), centered.
/// Returns `(x, y, width, height)` in window pixels.
pub fn fit_integer_scale(src: (i32, i32), dst: (i32, i32)) -> (i32, i32, i32, i32) {
    let scale = (dst.0 / src.0).min(dst.1 / src.1).max(1);
    let (w, h) = (src.0 * scale, src.1 * scale);
    ((dst.0 - w) / 2, (dst.1 - h) / 2, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rp_palette_dither::CGA_RGB;

    fn frame(fill: u8) -> FrameMsg {
        FrameMsg {
            width: 2,
            height: 1,
            pixels: vec![fill; 2],
        }
    }

    #[test]
    fn latest_channel_keeps_only_newest() {
        let (tx, rx) = latest_channel();
        assert_eq!(rx.try_recv(), None);
        tx.send(frame(1));
        tx.send(frame(2));
        assert_eq!(rx.try_recv(), Some(frame(2)));
        assert_eq!(rx.try_recv(), None);
    }

    #[test]
    fn latest_channel_crosses_threads() {
        let (tx, rx) = latest_channel();
        std::thread::spawn(move || tx.send(frame(7)))
            .join()
            .unwrap();
        assert_eq!(rx.try_recv(), Some(frame(7)));
    }

    #[test]
    fn indexed_to_rgba_looks_up_cga() {
        let mut out = Vec::new();
        indexed_to_rgba(&[0, 5, 15, 200], &CGA_RGB, &mut out);
        assert_eq!(
            out,
            [
                0, 0, 0, 255, 0x55, 0x55, 0xFF, 255, 0xFF, 0xFF, 0x55, 255, 0, 0, 0, 255
            ]
        );
    }

    #[test]
    fn fit_integer_scale_centers_whole_multiples() {
        assert_eq!(fit_integer_scale((200, 320), (400, 640)), (0, 0, 400, 640));
        assert_eq!(
            fit_integer_scale((200, 320), (500, 700)),
            (50, 30, 400, 640)
        );
        assert_eq!(
            fit_integer_scale((200, 320), (100, 100)),
            (-50, -110, 200, 320)
        );
    }
}
