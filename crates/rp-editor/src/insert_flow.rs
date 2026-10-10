//! GPUI glue for Insert Image: the file dialog, and decoding dropped or
//! chosen files on the background executor. The logic lives in
//! [`crate::image_ops`].

use std::path::PathBuf;

use gpui::{App, AppContext, Entity, PathPromptOptions};

use crate::image_ops::load_image_file;
use crate::state::EditorState;

/// Opens the platform file dialog (the XDG portal on Linux) and inserts
/// every chosen file. A dialog that cannot open reports in the status bar.
pub(crate) fn prompt_insert_image(state: &Entity<EditorState>, cx: &mut App) {
    let picked = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: true,
        prompt: Some("Insert".into()),
    });
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let paths = match picked.await {
            Ok(Ok(Some(paths))) => paths,
            // Cancelled, or the dialog went away.
            Ok(Ok(None)) | Err(_) => return,
            Ok(Err(e)) => {
                let _ = weak.update(cx, |s, cx| {
                    s.status = format!("Insert Image: can't open the file dialog: {e}");
                    cx.notify();
                });
                return;
            }
        };
        if let Some(state) = weak.upgrade() {
            cx.update(|cx| insert_image_files(&state, paths, cx));
        }
    })
    .detach();
}

/// Decodes `paths` one after another off the UI thread and inserts each as
/// an image layer, in order. Errors go to the status bar.
pub(crate) fn insert_image_files(state: &Entity<EditorState>, paths: Vec<PathBuf>, cx: &mut App) {
    if paths.is_empty() {
        return;
    }
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        for path in paths {
            let shown = weak.update(cx, |s, cx| {
                s.begin_image_load(&path);
                cx.notify();
            });
            if shown.is_err() {
                return;
            }
            let p = path.clone();
            let result = cx
                .background_spawn(async move { load_image_file(&p) })
                .await;
            let applied = weak.update(cx, |s, cx| {
                s.finish_image_load(&path, result);
                cx.notify();
            });
            if applied.is_err() {
                return;
            }
        }
    })
    .detach();
}
