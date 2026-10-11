//! The GPUI full-color editor.
//!
//! One window: an in-window menu bar (GPUI's native app menu does not render
//! on Wayland), a tool ribbon, the canvas area, a right-side panel column
//! (Layers, Filter, Swatches, Brushes) and a status bar. Logic lives in [`state`];
//! the GPUI view is thin.
//!
//! The [`render_worker`] flattens and dithers the document off the UI
//! thread and hands each [`rp_doc::FrameMsg`] to a [`FrameSink`]. The binary
//! forwards those frames to the DOS view; the editor never depends on
//! `rp-dos`. The same flattened image comes back to the canvas view, so the
//! UI thread never flattens.

pub mod brush_config;
mod brush_flow;
pub mod brush_library;
pub mod brush_loader;
mod brushes_panel;
pub mod canvas_image;
mod canvas_view;
mod color_picker;
pub mod color_picker_ops;
mod file_dialog;
mod file_flow;
pub mod file_ops;
pub mod filter_ops;
mod filter_panel;
pub mod flip_ops;
pub mod float_ops;
pub mod image_ops;
mod insert_flow;
pub mod layer_ops;
mod layers_panel;
pub mod mask_ops;
pub mod paint_ops;
pub mod panel_layout;
pub mod paste_ops;
mod project_dialog;
pub mod project_ops;
pub mod rasterize_ops;
pub mod render_worker;
pub mod ribbon_icons;
pub mod select_ops;
pub mod state;
pub mod swatch_ops;
mod swatches_panel;
pub mod transform_ops;
pub mod undo_ops;
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
    let (mut worker, client, canvas) = RenderWorker::spawn(sink);
    gpui_platform::application()
        .with_assets(ribbon_icons::IconAssets)
        .run(move |cx: &mut App| {
            view::bind_keys(cx);
            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some(APP_ID.into()),
                titlebar: Some(TitlebarOptions {
                    title: Some("Untitled — retro-paint".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            cx.open_window(options, |window, cx| {
                cx.new(|cx| EditorView::new(client, canvas, window, cx))
            })
            .expect("open editor window");
            cx.on_window_closed(|cx, _| cx.quit()).detach();
            cx.activate(true);
        });
    worker.shutdown();
}
