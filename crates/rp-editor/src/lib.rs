//! The GPUI full-color editor.
//!
//! One window: an in-window menu bar (GPUI's native app menu does not render
//! on Wayland), a tool ribbon, the canvas area, a right-side panel column
//! (Layers, Filter, Swatches) and a status bar. Logic lives in [`state`];
//! the GPUI view is thin.
//!
//! Until the render worker exists, the editor still animates a test pattern
//! and hands each indexed frame to a callback. The binary forwards those
//! frames to the DOS view; the editor never depends on `rp-dos`.

pub mod state;
mod view;
pub mod viewport;

use std::time::Duration;

use gpui::{App, Bounds, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size};

use view::EditorView;

/// Wayland app_id of the editor window.
pub const APP_ID: &str = "retro-paint-editor";

/// Test pattern size (mobile DOS layout).
pub const PATTERN_W: u32 = 200;
pub const PATTERN_H: u32 = 320;
const SQUARE: u32 = 32;
// Doc-order CGA indices (rp_palette_dither::CGA_RGB).
const BACKGROUND: u8 = 4; // blue
const BORDER: u8 = 3; // white
const SQUARE_COLOR: u8 = 15; // yellow

/// Bounces 0..=max back and forth, `speed` pixels per tick.
fn bounce(tick: u32, speed: u32, max: u32) -> u32 {
    let p = (tick.wrapping_mul(speed)) % (2 * max);
    if p <= max { p } else { 2 * max - p }
}

/// Top-left corner of the moving square at `tick`; always inside the 1px border.
pub fn square_pos(tick: u32) -> (u32, u32) {
    (
        1 + bounce(tick, 3, PATTERN_W - SQUARE - 2),
        1 + bounce(tick, 2, PATTERN_H - SQUARE - 2),
    )
}

/// Indexed test pattern at `tick`: blue field, white border, yellow square.
pub fn test_pattern(tick: u32) -> Vec<u8> {
    let (sx, sy) = square_pos(tick);
    let mut px = vec![BACKGROUND; (PATTERN_W * PATTERN_H) as usize];
    for y in 0..PATTERN_H {
        for x in 0..PATTERN_W {
            let i = (y * PATTERN_W + x) as usize;
            if x == 0 || y == 0 || x == PATTERN_W - 1 || y == PATTERN_H - 1 {
                px[i] = BORDER;
            } else if (sx..sx + SQUARE).contains(&x) && (sy..sy + SQUARE).contains(&y) {
                px[i] = SQUARE_COLOR;
            }
        }
    }
    px
}

/// Receives each indexed frame: `(width, height, pixels)`.
pub type FrameSink = Box<dyn FnMut(u32, u32, Vec<u8>)>;

/// Runs the GPUI editor on the calling (main) thread until the app quits.
pub fn run(sink: FrameSink) {
    gpui_platform::application().run(move |cx: &mut App| {
        view::bind_keys(cx);
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some(APP_ID.into()),
            titlebar: Some(TitlebarOptions {
                title: Some("retro-paint editor".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            cx.new(|cx| {
                cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(33))
                            .await;
                        let alive = this.update(cx, |editor: &mut EditorView, _cx| {
                            editor.step();
                        });
                        if alive.is_err() {
                            break;
                        }
                    }
                })
                .detach();
                EditorView::new(sink, window, cx)
            })
        })
        .expect("open editor window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_is_200x320_cga_indices() {
        let px = test_pattern(5);
        assert_eq!(px.len(), (PATTERN_W * PATTERN_H) as usize);
        assert!(px.iter().all(|&i| i < 16));
    }

    #[test]
    fn square_is_drawn_where_square_pos_says() {
        for tick in [0, 1, 57, 1000] {
            let px = test_pattern(tick);
            let (sx, sy) = square_pos(tick);
            let count = px.iter().filter(|&&i| i == SQUARE_COLOR).count();
            assert_eq!(count, (SQUARE * SQUARE) as usize, "tick {tick}");
            assert_eq!(px[((sy + 1) * PATTERN_W + sx + 1) as usize], SQUARE_COLOR);
        }
    }

    #[test]
    fn square_moves_and_stays_inside() {
        assert_ne!(square_pos(0), square_pos(1));
        for tick in 0..2000 {
            let (x, y) = square_pos(tick);
            assert!(x + SQUARE <= PATTERN_W && y + SQUARE <= PATTERN_H);
        }
    }
}
