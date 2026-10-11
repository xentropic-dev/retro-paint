//! GPUI glue for Save, Open and Export PNG: the portal file dialogs and the
//! background jobs. Every decision is an [`EditorState`] method from
//! [`crate::file_ops`]; this only carries [`Outcome`]s to the platform.

use std::path::PathBuf;

use gpui::{App, AppContext, Entity, PathPromptOptions};

use crate::file_ops::{
    default_export_name, default_project_name, dialog_directory, export_job, open_job, save_job,
};
use crate::insert_flow::prompt_insert_image;
use crate::state::{EditorState, Outcome};

/// Carries out what a command returned: quit, show a file dialog, or start
/// a save.
pub(crate) fn handle_outcome(state: &Entity<EditorState>, outcome: Outcome, cx: &mut App) {
    match outcome {
        Outcome::Done => {}
        Outcome::Quit => cx.quit(),
        Outcome::PickImages => prompt_insert_image(state, cx),
        Outcome::PickOpenPath => prompt_open(state, cx),
        Outcome::PickSavePath => prompt_save_path(state, cx),
        Outcome::SaveTo(path) => save_to(state, path, cx),
        Outcome::PickExportPath => prompt_export_path(state, cx),
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Shows the portal's save dialog in the project's folder, suggesting
/// `name`. `on_path` gets the chosen path; `on_cancel` runs when the
/// dialog is dismissed or cannot open (its error goes to the status bar).
fn prompt_new_path(
    state: &Entity<EditorState>,
    name: String,
    what: &'static str,
    on_path: impl FnOnce(&Entity<EditorState>, PathBuf, &mut App) + 'static,
    on_cancel: impl FnOnce(&mut EditorState) + 'static,
    cx: &mut App,
) {
    let dir = dialog_directory(state.read(cx).current_path.as_deref(), home());
    let picked = cx.prompt_for_new_path(&dir, Some(&name));
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let path = match picked.await {
            Ok(Ok(Some(path))) => Some(path),
            Ok(Ok(None)) | Err(_) => None,
            Ok(Err(e)) => {
                let _ = weak.update(cx, |s, cx| {
                    s.status = format!("{what}: can't open the file dialog: {e}");
                    cx.notify();
                });
                None
            }
        };
        let Some(state) = weak.upgrade() else { return };
        match path {
            Some(path) => cx.update(|cx| on_path(&state, path, cx)),
            None => cx.update(|cx| {
                state.update(cx, |s, cx| {
                    on_cancel(s);
                    cx.notify();
                })
            }),
        }
    })
    .detach();
}

/// Save As: asks for a path, then saves there.
fn prompt_save_path(state: &Entity<EditorState>, cx: &mut App) {
    let name = default_project_name(state.read(cx).current_path.as_deref());
    prompt_new_path(
        state,
        name,
        "Save As",
        |state, path, cx| {
            let out = state.update(cx, |s, cx| {
                cx.notify();
                s.chose_save_path(path)
            });
            handle_outcome(state, out, cx);
        },
        EditorState::cancel_save_path,
        cx,
    );
}

/// Writes the project to `path` on the background executor.
fn save_to(state: &Entity<EditorState>, path: PathBuf, cx: &mut App) {
    let job = state.update(cx, |s, cx| {
        cx.notify();
        s.begin_save(path)
    });
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let (doc, sources, p) = (job.document, job.sources, job.path.clone());
        let result = cx
            .background_spawn(async move { save_job(&doc, &sources, &p) })
            .await;
        let Ok(out) = weak.update(cx, |s, cx| {
            cx.notify();
            s.finish_save(job.path, job.mark, result)
        }) else {
            return;
        };
        if let Some(state) = weak.upgrade() {
            cx.update(|cx| handle_outcome(&state, out, cx));
        }
    })
    .detach();
}

/// Open: the portal's open dialog, then loads the file off the UI thread.
fn prompt_open(state: &Entity<EditorState>, cx: &mut App) {
    // The pinned GPUI has no file-type filter for the portal dialog; a
    // non-project file fails to load with a readable message.
    let picked = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("Open".into()),
    });
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let path = match picked.await {
            Ok(Ok(Some(mut paths))) if !paths.is_empty() => paths.swap_remove(0),
            Ok(Ok(_)) | Err(_) => return,
            Ok(Err(e)) => {
                let _ = weak.update(cx, |s, cx| {
                    s.status = format!("Open: can't open the file dialog: {e}");
                    cx.notify();
                });
                return;
            }
        };
        let started = weak.update(cx, |s, cx| {
            s.begin_open(&path);
            cx.notify();
        });
        if started.is_err() {
            return;
        }
        let p = path.clone();
        let result = cx.background_spawn(async move { open_job(&p) }).await;
        let _ = weak.update(cx, |s, cx| {
            s.finish_open(path, result);
            cx.notify();
        });
    })
    .detach();
}

/// Export: asks for the PNG path, then flattens, dithers and writes off the
/// UI thread with the settings from the Export dialog.
fn prompt_export_path(state: &Entity<EditorState>, cx: &mut App) {
    let name = default_export_name(state.read(cx).current_path.as_deref());
    prompt_new_path(
        state,
        name,
        "Export PNG",
        |state, path, cx| {
            let job = state.update(cx, |s, cx| {
                cx.notify();
                s.begin_export(path)
            });
            let weak = state.downgrade();
            cx.spawn(async move |cx| {
                let p = job.path.clone();
                let result = cx
                    .background_spawn(async move {
                        export_job(&job.document, &job.sources, &job.options, &job.path)
                    })
                    .await;
                let _ = weak.update(cx, |s, cx| {
                    s.finish_export(&p, result);
                    cx.notify();
                });
            })
            .detach();
        },
        |_| {},
        cx,
    );
}
