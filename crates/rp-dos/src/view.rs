//! Pure state behind the DOS window: which frame is on screen, the composed
//! screen, window sizes per layout, and the stop signal. The raylib loop in
//! `window.rs` only uploads what this module decides.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rp_doc::{FrameMsg, IndexedFrame, Layout, Size, Tool};
use rp_palette_dither::{CgaColor, Palette};

use crate::{IndexedScreen, ScrollState, canvas_viewport, compose};

/// Default DOS window size in pixels for `layout`: 9:16 for mobile, 16:9
/// for desktop. A tiling compositor may override it; see
/// `contrib/hyprland.conf` for window rules.
pub const fn window_size(layout: Layout) -> (i32, i32) {
    match layout {
        Layout::Mobile => (540, 960),
        Layout::Desktop => (960, 540),
    }
}

/// What the window shows before the editor's first frame: an empty (black)
/// canvas filling the canvas window, full palette, no cursor.
pub fn idle_frame(layout: Layout) -> FrameMsg {
    let view = canvas_viewport(layout);
    let size = Size::new(view.w as u32, view.h as u32);
    FrameMsg {
        image: IndexedFrame::filled(size, CgaColor::Black, Palette::full()),
        layout,
        cursor: None,
        tool: Tool::Brush,
        brush_diameter: 1,
        active_color: CgaColor::White,
    }
}

/// What changed after [`DosState::accept`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Change {
    /// No new frame: keep showing the current screen.
    None,
    /// A new frame in the same layout: re-upload the screen.
    Screen,
    /// A new frame in a different layout: resize the window and screen
    /// textures, then upload.
    Layout,
}

/// The frame on screen, its composed DOS screen, and the persistent scroll.
pub struct DosState {
    frame: FrameMsg,
    scroll: ScrollState,
    screen: IndexedScreen,
}

impl DosState {
    /// Starts showing `frame`.
    pub fn new(frame: FrameMsg) -> Self {
        let mut scroll = ScrollState::default();
        let screen = compose(&frame, frame.layout, &mut scroll);
        Self {
            frame,
            scroll,
            screen,
        }
    }

    /// Takes the newest frame, if any. Without one the last frame stays on
    /// screen. A layout change resets the scroll before composing.
    pub fn accept(&mut self, frame: Option<FrameMsg>) -> Change {
        let Some(frame) = frame else {
            return Change::None;
        };
        let change = if frame.layout == self.frame.layout {
            Change::Screen
        } else {
            self.scroll = ScrollState::default();
            Change::Layout
        };
        self.screen = compose(&frame, frame.layout, &mut self.scroll);
        self.frame = frame;
        change
    }

    pub fn layout(&self) -> Layout {
        self.frame.layout
    }

    pub fn frame(&self) -> &FrameMsg {
        &self.frame
    }

    pub fn screen(&self) -> &IndexedScreen {
        &self.screen
    }

    pub fn scroll(&self) -> ScrollState {
        self.scroll
    }
}

/// Asks the DOS thread to close its window and return. Clones share one flag.
#[derive(Clone, Default, Debug)]
pub struct StopSignal(Arc<AtomicBool>);

impl StopSignal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests the stop. The DOS loop sees it within one frame.
    pub fn stop(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::sample_frame;

    #[test]
    fn window_sizes_are_9_16_and_16_9() {
        let (w, h) = window_size(Layout::Mobile);
        assert_eq!(w * 16, h * 9);
        let (w, h) = window_size(Layout::Desktop);
        assert_eq!(w * 9, h * 16);
    }

    #[test]
    fn idle_frame_composes_to_the_layout_screen() {
        for layout in [Layout::Mobile, Layout::Desktop] {
            let st = DosState::new(idle_frame(layout));
            assert_eq!(st.screen().size(), layout.dos_screen());
            assert_eq!(st.layout(), layout);
            assert_eq!(st.frame().cursor, None);
        }
    }

    #[test]
    fn no_frame_keeps_the_last_screen() {
        let mut st = DosState::new(sample_frame(Layout::Mobile));
        let before = st.screen().clone();
        assert_eq!(st.accept(None), Change::None);
        assert_eq!(st.screen(), &before);
    }

    #[test]
    fn same_layout_frame_recomposes_and_keeps_scroll() {
        let mut st = DosState::new(idle_frame(Layout::Mobile));
        let frame = sample_frame(Layout::Mobile);
        assert_eq!(st.accept(Some(frame.clone())), Change::Screen);
        let scrolled = st.scroll();
        assert_ne!(scrolled, ScrollState::default());
        let mut scroll = ScrollState::default();
        assert_eq!(st.screen(), &compose(&frame, Layout::Mobile, &mut scroll));
        // A frame without a cursor leaves the scroll where it was.
        let mut still = frame.clone();
        still.cursor = None;
        assert_eq!(st.accept(Some(still)), Change::Screen);
        assert_eq!(st.scroll(), scrolled);
    }

    #[test]
    fn layout_change_resets_scroll() {
        let mut st = DosState::new(sample_frame(Layout::Mobile));
        assert_ne!(st.scroll(), ScrollState::default());
        let mut desktop = sample_frame(Layout::Desktop);
        desktop.cursor = None;
        assert_eq!(st.accept(Some(desktop)), Change::Layout);
        assert_eq!(st.scroll(), ScrollState::default());
        assert_eq!(st.layout(), Layout::Desktop);
        assert_eq!(st.screen().size(), Layout::Desktop.dos_screen());
    }

    #[test]
    fn stop_signal_is_shared_across_clones_and_threads() {
        let stop = StopSignal::new();
        assert!(!stop.is_stopped());
        let remote = stop.clone();
        std::thread::spawn(move || remote.stop()).join().unwrap();
        assert!(stop.is_stopped());
    }
}
