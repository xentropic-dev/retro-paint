//! GPUI glue for Insert Image and Paste: the file dialog, the clipboard
//! read, and decoding dropped, chosen or pasted images on the background
//! executor. The logic lives in [`crate::image_ops`] and
//! [`crate::paste_ops`].

use std::path::{Path, PathBuf};

use gpui::{App, AppContext, ClipboardEntry, ClipboardItem, Entity, PathPromptOptions};

use crate::image_ops::{load_image_bytes, load_image_file};
use crate::paste_ops::{
    ClipboardContent, PASTED_LAYER_NAME, PastePlan, plan_paste, read_with, run_wl_paste,
};
use crate::state::EditorState;

fn is_file(p: &Path) -> bool {
    p.is_file()
}

/// Converts GPUI's clipboard item: an image entry wins, then file paths,
/// then text.
fn clipboard_content(item: Option<ClipboardItem>) -> ClipboardContent {
    let Some(item) = item else {
        return ClipboardContent::Nothing;
    };
    let text = item.text();
    let mut files = None;
    for entry in item.into_entries() {
        match entry {
            ClipboardEntry::Image(img) => return ClipboardContent::Image(img.bytes),
            ClipboardEntry::ExternalPaths(p) if files.is_none() => {
                files = Some(p.paths().to_vec());
            }
            _ => {}
        }
    }
    match (files, text) {
        (Some(f), _) => ClipboardContent::Files(f),
        (None, Some(t)) => ClipboardContent::Text(t),
        (None, None) => ClipboardContent::Nothing,
    }
}

/// Edit > Paste / Ctrl+V. Reads the clipboard through GPUI; if that yields
/// no image and no file list, asks `wl-paste` off the UI thread. Images
/// decode on the background executor and insert as "Pasted image"; file
/// lists go through [`insert_image_files`].
pub(crate) fn paste_from_clipboard(state: &Entity<EditorState>, cx: &mut App) {
    let started = state.update(cx, |s, cx| {
        let ok = s.begin_paste();
        cx.notify();
        ok
    });
    if !started {
        return;
    }
    match plan_paste(clipboard_content(cx.read_from_clipboard()), is_file) {
        PastePlan::Image(bytes) => paste_image_bytes(state, bytes, cx),
        PastePlan::Files(paths) => insert_image_files(state, paths, cx),
        PastePlan::NoImage => {
            let weak = state.downgrade();
            cx.spawn(async move |cx| {
                let plan = cx
                    .background_spawn(async move { plan_paste(read_with(run_wl_paste), is_file) })
                    .await;
                let Some(state) = weak.upgrade() else {
                    return;
                };
                cx.update(|cx| match plan {
                    PastePlan::Image(bytes) => paste_image_bytes(&state, bytes, cx),
                    PastePlan::Files(paths) => insert_image_files(&state, paths, cx),
                    PastePlan::NoImage => state.update(cx, |s, cx| {
                        s.paste_found_nothing();
                        cx.notify();
                    }),
                });
            })
            .detach();
        }
    }
}

/// Decodes pasted bytes off the UI thread and inserts them.
fn paste_image_bytes(state: &Entity<EditorState>, bytes: Vec<u8>, cx: &mut App) {
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn(async move { load_image_bytes(PASTED_LAYER_NAME.into(), &bytes) })
            .await;
        let _ = weak.update(cx, |s, cx| {
            s.finish_paste(result);
            cx.notify();
        });
    })
    .detach();
}

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
