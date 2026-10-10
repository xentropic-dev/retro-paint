//! The GPUI full-color editor.
//!
//! One window: an in-window menu bar (GPUI's native app menu does not render
//! on Wayland), a tool ribbon, the canvas area, a right-side panel column
//! (Layers, Filter, Swatches) and a status bar. Logic lives in [`state`];
//! the GPUI view is thin.
//!
//! The [`render_worker`] dithers the document off the UI thread and hands
//! each [`rp_doc::FrameMsg`] to a [`FrameSink`]. The binary forwards those
//! frames to the DOS view; the editor never depends on `rp-dos`.

pub mod canvas_image;
mod canvas_view;
pub mod filter_ops;
mod filter_panel;
pub mod layer_ops;
mod layers_panel;
pub mod render_worker;
pub mod state;
mod view;
pub mod viewport;

use gpui::{App, Bounds, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size};

pub use render_worker::FrameSink;
use render_worker::RenderWorker;
use view::EditorView;

/// Wayland app_id of the editor window.
pub const APP_ID: &str = "retro-paint-editor";

/// Runs the GPUI editor on the calling (main) thread until the app quits.
/// Frames for the DOS view go to `sink` from the render worker thread, which
/// is stopped and joined before this returns.
pub fn run(sink: FrameSink) {
    let (mut worker, client) = RenderWorker::spawn(sink);
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
            cx.new(|cx| EditorView::new(client, window, cx))
        })
        .expect("open editor window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
    worker.shutdown();
}
