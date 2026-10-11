//! GPUI glue for the brush library: restoring saved brush paths at
//! startup, Add brushes… / Add folder… (the portal file chooser), dropped
//! files, and running [`crate::brush_loader`] and brush decoding on the
//! background executor. Every decision is an [`EditorState`] method in
//! [`crate::brush_library`].

use std::path::PathBuf;

use gpui::{App, AppContext, Entity, PathPromptOptions};

use crate::brush_config::{config_file_from_env, restore, write_paths};
use crate::brush_library::{AddJob, BrushKey, LoadEvent};
use crate::brush_loader::{prepare_brush, run_load};
use crate::state::EditorState;

/// Startup: reads `brushes.json` off the UI thread, drops missing paths
/// (with a status note) and loads the rest.
pub(crate) fn restore_saved_brushes(state: &Entity<EditorState>, cx: &mut App) {
    let Some(file) = config_file_from_env() else {
        return;
    };
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let restored = cx.background_spawn(async move { restore(&file) }).await;
        if restored.kept.is_empty() && restored.dropped.is_empty() {
            return;
        }
        let Some(state) = weak.upgrade() else { return };
        cx.update(|cx| {
            let note = restored.note();
            let job = state.update(cx, |s, cx| {
                cx.notify();
                s.restore_brush_roots(restored.kept, note)
            });
            if let Some(job) = job {
                // Already saved; just load.
                load(&state, job, false, cx);
            }
        });
    })
    .detach();
}

/// The portal file chooser: brush files (multi-select) or one folder per
/// pick. The portal cannot offer both at once, so the panel has a button
/// for each.
pub(crate) fn prompt_add_brushes(state: &Entity<EditorState>, folders: bool, cx: &mut App) {
    // The pinned GPUI has no file-type filter for the portal dialog; other
    // files are skipped by the scan.
    let picked = cx.prompt_for_paths(PathPromptOptions {
        files: !folders,
        directories: folders,
        multiple: true,
        prompt: Some("Add".into()),
    });
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let paths = match picked.await {
            Ok(Ok(Some(paths))) => paths,
            Ok(Ok(None)) | Err(_) => return,
            Ok(Err(e)) => {
                let _ = weak.update(cx, |s, cx| {
                    s.status = format!("Add brushes: can't open the file dialog: {e}");
                    cx.notify();
                });
                return;
            }
        };
        if let Some(state) = weak.upgrade() {
            cx.update(|cx| add_brush_paths(&state, paths, cx));
        }
    })
    .detach();
}

/// Adds brush files or folders (picked or dropped), saves the root list
/// and loads them off the UI thread.
pub(crate) fn add_brush_paths(state: &Entity<EditorState>, paths: Vec<PathBuf>, cx: &mut App) {
    let job = state.update(cx, |s, cx| {
        cx.notify();
        s.add_brush_roots(paths)
    });
    if let Some(job) = job {
        load(state, job, true, cx);
    }
}

/// Runs the loader for `job` on the background executor and feeds its
/// events to the state as they come, so entries and thumbnails appear
/// while the rest loads. With `save`, first writes the root list.
fn load(state: &Entity<EditorState>, job: AddJob, save: bool, cx: &mut App) {
    let (tx, rx) = async_channel::unbounded::<LoadEvent>();
    let AddJob {
        roots,
        save: all_roots,
        known,
    } = job;
    cx.background_spawn(async move {
        if save && let Some(file) = config_file_from_env() {
            // Best effort: the brushes still load for this session.
            let _ = write_paths(&file, &all_roots);
        }
        run_load(&roots, &known, |ev| tx.send_blocking(ev).is_ok());
    })
    .detach();
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        while let Ok(ev) = rx.recv().await {
            let alive = weak.update(cx, |s, cx| {
                s.brush_load_event(ev);
                cx.notify();
            });
            if alive.is_err() {
                return;
            }
        }
    })
    .detach();
}

/// A click in the Brushes section: selects the brush, decoding a sampled
/// one off the UI thread first.
pub(crate) fn select_brush(state: &Entity<EditorState>, key: BrushKey, cx: &mut App) {
    let job = state.update(cx, |s, cx| {
        cx.notify();
        s.select_library_brush(&key)
    });
    let Some(job) = job else { return };
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let data = job.data;
        let result = cx
            .background_spawn(async move { prepare_brush(&data) })
            .await;
        let _ = weak.update(cx, |s, cx| {
            s.finish_brush_prepare(job.key, result);
            cx.notify();
        });
    })
    .detach();
}
