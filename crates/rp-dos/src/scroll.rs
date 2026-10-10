//! Cursor-follow autoscroll for the DOS canvas window.
//!
//! The canvas window shows the output image 1:1, never zoomed. When the image
//! is larger than the window on an axis, the window pans to keep the cursor at
//! least [`SCROLL_MARGIN`] pixels from that edge, like an old paint program
//! panning as you paint near the edge. The offset is clamped so the window
//! never shows past the image. When the image fits on an axis, the offset is 0
//! and the composer centers the image on that axis.

use rp_doc::{OutputPoint, Size};

/// How close (in output pixels) the cursor may get to a canvas window edge
/// before the window pans. Capped at half the window for tiny windows.
pub const SCROLL_MARGIN: i32 = 16;

/// The canvas window's pan: the output pixel shown at the top-left of the
/// window. Keep one per DOS view and pass it to every
/// [`compose`](crate::compose) call so scrolling persists between frames.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct ScrollState {
    pub x: i32,
    pub y: i32,
}

impl ScrollState {
    /// Pans so `cursor` stays in view of a `view`-sized window over an
    /// `image`-sized output, then clamps to the image. With no cursor the
    /// offset only gets clamped (the image may have changed size).
    pub fn follow(&mut self, cursor: Option<OutputPoint>, image: Size, view: (i32, i32)) {
        self.x = follow_axis(self.x, cursor.map(|c| c.x), image.width as i32, view.0);
        self.y = follow_axis(self.y, cursor.map(|c| c.y), image.height as i32, view.1);
    }
}

fn follow_axis(scroll: i32, cursor: Option<i32>, image: i32, view: i32) -> i32 {
    let max = (image - view).max(0);
    let mut s = scroll;
    if let Some(c) = cursor {
        let m = SCROLL_MARGIN.min((view - 1) / 2).max(0);
        if c < s + m {
            s = c - m;
        } else if c > s + view - 1 - m {
            s = c - (view - 1 - m);
        }
    }
    s.clamp(0, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: i32, y: i32) -> Option<OutputPoint> {
        Some(OutputPoint { x, y })
    }

    #[test]
    fn small_image_never_scrolls() {
        let mut s = ScrollState { x: 5, y: 9 };
        s.follow(pt(90, 90), Size::new(100, 100), (150, 150));
        assert_eq!(s, ScrollState::default());
    }

    #[test]
    fn stays_put_while_cursor_is_inside_the_margin() {
        let mut s = ScrollState::default();
        s.follow(pt(50, 50), Size::new(400, 400), (100, 100));
        assert_eq!(s, ScrollState::default());
        s.follow(
            pt(99 - SCROLL_MARGIN, 99 - SCROLL_MARGIN),
            Size::new(400, 400),
            (100, 100),
        );
        assert_eq!(s, ScrollState::default());
    }

    #[test]
    fn pans_to_keep_the_margin() {
        let mut s = ScrollState::default();
        s.follow(pt(150, 0), Size::new(400, 400), (100, 100));
        assert_eq!(s.x, 150 - (99 - SCROLL_MARGIN));
        assert_eq!(s.y, 0);
        s.follow(pt(70, 0), Size::new(400, 400), (100, 100));
        assert_eq!(s.x, 70 - SCROLL_MARGIN);
    }

    #[test]
    fn clamps_to_image_bounds() {
        let mut s = ScrollState::default();
        s.follow(pt(1000, -50), Size::new(400, 300), (100, 100));
        assert_eq!(s, ScrollState { x: 300, y: 0 });
        s.follow(pt(399, 299), Size::new(400, 300), (100, 100));
        assert_eq!(s, ScrollState { x: 300, y: 200 });
    }

    #[test]
    fn no_cursor_only_clamps() {
        let mut s = ScrollState { x: 250, y: 40 };
        s.follow(None, Size::new(300, 300), (100, 100));
        assert_eq!(s, ScrollState { x: 200, y: 40 });
    }

    #[test]
    fn cursor_always_ends_up_in_view() {
        let image = Size::new(333, 517);
        let view = (97, 141);
        let mut s = ScrollState::default();
        let path = (0..2000).map(|i| ((i * 37) % 333, (i * 53 + i / 7) % 517));
        for (x, y) in path {
            s.follow(pt(x, y), image, view);
            assert!(
                (s.x..s.x + view.0).contains(&x),
                "x {x} not in view at {s:?}"
            );
            assert!(
                (s.y..s.y + view.1).contains(&y),
                "y {y} not in view at {s:?}"
            );
        }
    }
}
