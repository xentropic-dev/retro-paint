//! The GPUI full-color editor.
//!
//! Spike state: one GPUI window that animates a test pattern (a moving square)
//! and hands each indexed frame to a callback. The binary forwards those frames
//! to the DOS view; the editor never depends on `rp-dos`.

use std::time::Duration;

use gpui::{
    App, Bounds, Context, TitlebarOptions, Window, WindowBounds, WindowOptions, div, prelude::*,
    px, rgb, size,
};

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

struct Editor {
    tick: u32,
    sink: FrameSink,
}

impl Editor {
    fn step(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        (self.sink)(PATTERN_W, PATTERN_H, test_pattern(self.tick));
    }
}

impl Render for Editor {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (x, y) = square_pos(self.tick);
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .bg(rgb(0x202020))
            .text_color(rgb(0xffffff))
            .child(format!("retro-paint editor spike - frame {}", self.tick))
            .child(
                div()
                    .relative()
                    .w(px(PATTERN_W as f32))
                    .h(px(PATTERN_H as f32))
                    .bg(rgb(0x3060c0))
                    .child(
                        div()
                            .absolute()
                            .left(px(x as f32))
                            .top(px(y as f32))
                            .size(px(SQUARE as f32))
                            .bg(rgb(0xffd040)),
                    ),
            )
    }
}

/// Runs the GPUI editor on the calling (main) thread until the app quits.
pub fn run(sink: FrameSink) {
    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(480.), px(480.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some(APP_ID.into()),
            titlebar: Some(TitlebarOptions {
                title: Some("retro-paint editor".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        cx.open_window(options, |_, cx| {
            cx.new(|cx| {
                cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(33))
                            .await;
                        let alive = this.update(cx, |editor: &mut Editor, cx| {
                            editor.step();
                            cx.notify();
                        });
                        if alive.is_err() {
                            break;
                        }
                    }
                })
                .detach();
                Editor { tick: 0, sink }
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
