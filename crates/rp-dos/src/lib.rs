//! The DOS view: screen composer, raylib window, and CRT shader.
//!
//! The composer ([`compose`]) is pure: it turns an [`rp_doc::FrameMsg`] into
//! the indexed 320×200 / 200×320 DOS screen. [`run_window`] takes the newest
//! frame from a latest-only channel, composes it, and draws it through the
//! CRT shader until a [`StopSignal`] fires. See docs/aesthetics.md and
//! docs/tech-stack.md.

use std::sync::{Arc, Mutex};

use rp_doc::FrameMsg;

mod compose;
pub mod crt;
mod glyphs;
pub mod hypr;
pub mod sample;
mod screen;
mod scroll;
mod view;
mod window;

pub use compose::{canvas_viewport, compose};
pub use crt::{CrtParams, tube_rect};
pub use screen::{IndexedScreen, Rect};
pub use scroll::{SCROLL_MARGIN, ScrollState};
pub use view::{
    Change, DosState, StopSignal, WindowSystem, frame_tube, idle_frame, layout_switch_size,
    window_size,
};
pub use window::{run_crt_demo, run_window};

/// Wayland app_id (and X11 class) of the DOS window.
pub const APP_ID: &str = "retro-paint-dos";

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

#[cfg(test)]
mod tests {
    use super::*;
    use rp_doc::Layout;

    fn frame(layout: Layout, brush: u32) -> FrameMsg {
        FrameMsg {
            brush_diameter: brush,
            ..idle_frame(layout)
        }
    }

    #[test]
    fn latest_channel_keeps_only_newest() {
        let (tx, rx) = latest_channel();
        assert_eq!(rx.try_recv(), None);
        tx.send(frame(Layout::Mobile, 1));
        tx.send(frame(Layout::Desktop, 2));
        assert_eq!(rx.try_recv(), Some(frame(Layout::Desktop, 2)));
        assert_eq!(rx.try_recv(), None);
    }

    #[test]
    fn latest_channel_crosses_threads() {
        let (tx, rx) = latest_channel();
        std::thread::spawn(move || tx.send(frame(Layout::Mobile, 7)))
            .join()
            .unwrap();
        assert_eq!(rx.try_recv(), Some(frame(Layout::Mobile, 7)));
    }
}
