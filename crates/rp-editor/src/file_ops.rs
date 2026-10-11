//! Save, Open, Export PNG and the unsaved-changes prompt, with no GPUI types.
//!
//! The GPUI glue ([`crate::file_flow`]) shows the portal dialogs and runs
//! the slow work ([`save_job`], [`open_job`], [`export_job`]) off the UI
//! thread; everything it decides goes through the [`EditorState`] methods
//! here.
//!
//! Decisions:
//! - **Dirty** means the document differs from what was last saved or
//!   opened. Pixel and settings edits change [`EditorState::content`], and
//!   undo/redo restore it (see [`crate::undo_ops`]), so undoing back to the
//!   saved state is clean again. The brush colors and swatches are saved
//!   too but never change it, so the [`SavedMark`] also records them.
//!   Changing a color and changing it back is clean again.
//! - A save records the mark taken when it **started**, so an edit made
//!   while the file is being written leaves the document dirty.
//! - New, Open and Quit with a dirty document ask Save / Don't Save /
//!   Cancel first. Save runs the normal Save (Save As if the document has
//!   no file yet) and continues only if it succeeds.
//! - One file job at a time: while a save, open or export runs, other file
//!   commands are refused with a status message.
//! - Save As adds `.rpaint` and Export adds `.png` when the chosen name has
//!   a different (or no) extension.

use std::path::{Path, PathBuf};

use rp_doc::{Document, Size, SourceStore};
use rp_format::{ExportOptions, ExportReport, FormatError, MAX_EXPORT_SCALE};
use rp_palette_dither::Rgb;

use crate::state::{EditorState, Outcome, new_document};

/// Project file extension.
pub const PROJECT_EXT: &str = "rpaint";

/// Display name of a document that has never been saved.
pub const UNTITLED: &str = "Untitled";

/// What a document looked like when it was last saved or opened.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SavedMark {
    /// [`EditorState::content`] when saved.
    pub content: u64,
    pub fg: Rgb,
    pub bg: Rgb,
    pub swatches: Vec<Rgb>,
}

impl SavedMark {
    pub fn of(state: &EditorState) -> Self {
        let d = &state.document;
        Self {
            content: state.content,
            fg: d.fg,
            bg: d.bg,
            swatches: d.swatches.clone(),
        }
    }
}

/// The command waiting behind the unsaved-changes prompt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PendingAction {
    New,
    Open,
    Quit,
}

impl PendingAction {
    /// Prompt wording: "Save changes before …?"
    pub const fn verb(self) -> &'static str {
        match self {
            PendingAction::New => "starting a new document",
            PendingAction::Open => "opening another file",
            PendingAction::Quit => "quitting",
        }
    }
}

/// A button of the unsaved-changes prompt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnsavedChoice {
    Save,
    DontSave,
    Cancel,
}

/// A message box (open and save failures).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Alert {
    pub title: &'static str,
    pub message: String,
}

/// The Export PNG dialog's settings while it is open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExportDraft {
    pub scale: u8,
    pub include_full_color: bool,
}

/// Clamps an export scale to `1..=8`.
pub fn clamp_scale(scale: i64) -> u8 {
    scale.clamp(1, MAX_EXPORT_SCALE as i64) as u8
}

impl ExportDraft {
    pub fn from_options(o: ExportOptions) -> Self {
        Self {
            scale: clamp_scale(o.scale as i64),
            include_full_color: o.include_full_color,
        }
    }

    pub fn options(self) -> ExportOptions {
        ExportOptions {
            scale: self.scale,
            include_full_color: self.include_full_color,
        }
    }

    pub fn set_scale(&mut self, scale: i64) {
        self.scale = clamp_scale(scale);
    }

    pub fn step_scale(&mut self, delta: i64) {
        self.set_scale(self.scale as i64 + delta);
    }

    /// Pixel size of the dithered PNG for `output`.
    pub fn pixel_size(self, output: Size) -> Size {
        let s = self.scale as u32;
        Size::new(output.width * s, output.height * s)
    }
}

/// `path` with extension `ext`, unless it already has it (any case).
pub fn with_extension(path: PathBuf, ext: &str) -> PathBuf {
    let has = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext));
    if has {
        return path;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(ext);
    path.with_file_name(name)
}

/// The file name shown in the title and status bar: `name.rpaint`, or
/// [`UNTITLED`].
pub fn display_name(path: Option<&Path>) -> String {
    path.and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| UNTITLED.into())
}

/// Suggested name in the Save As dialog.
pub fn default_project_name(path: Option<&Path>) -> String {
    match path.and_then(|p| p.file_name()) {
        Some(n) => n.to_string_lossy().into_owned(),
        None => format!("{UNTITLED}.{PROJECT_EXT}"),
    }
}

/// Suggested name in the Export dialog: the project file's stem + `.png`.
pub fn default_export_name(path: Option<&Path>) -> String {
    let stem = path
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| UNTITLED.into());
    format!("{stem}.png")
}

/// Folder a save dialog opens in: the project file's folder, else `home`.
pub fn dialog_directory(path: Option<&Path>, home: Option<PathBuf>) -> PathBuf {
    path.and_then(|p| p.parent())
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .or(home)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Window title: `name.rpaint • — retro-paint` (the dot while dirty).
pub fn window_title(name: &str, dirty: bool) -> String {
    let dot = if dirty { " •" } else { "" };
    format!("{name}{dot} — retro-paint")
}

/// Readable text for a load failure of `path`.
pub fn load_error_message(path: &Path, e: &FormatError) -> String {
    let name = display_name(Some(path));
    match e {
        FormatError::NewerVersion { found, supported } => format!(
            "{name} was saved by a newer retro-paint (format {found}); \
             this version supports up to format {supported}. \
             Update retro-paint to open it."
        ),
        FormatError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
            format!("Can't open {name}: the file does not exist.")
        }
        FormatError::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied => {
            format!("Can't open {name}: permission denied.")
        }
        FormatError::Io(io) => format!("Can't open {name}: {io}."),
        FormatError::Zip(_) => {
            format!("Can't open {name}: it is not a retro-paint project (.rpaint) file.")
        }
        FormatError::MissingEntry { .. }
        | FormatError::Json(_)
        | FormatError::InvalidManifest(_)
        | FormatError::InvalidImage { .. }
        | FormatError::HashMismatch { .. }
        | FormatError::BadSourceHash(_) => {
            format!("Can't open {name}: the project file is damaged ({e}).")
        }
        other => format!("Can't open {name}: {other}."),
    }
}

/// Readable text for a save failure to `path`.
pub fn save_error_message(path: &Path, e: &FormatError) -> String {
    let name = display_name(Some(path));
    match e {
        FormatError::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied => {
            format!("Can't save {name}: permission denied.")
        }
        FormatError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
            format!("Can't save {name}: the folder does not exist.")
        }
        other => format!("Can't save {name}: {other}."),
    }
}

/// Readable text for an export failure to `path`.
pub fn export_error_message(path: &Path, e: &FormatError) -> String {
    let name = display_name(Some(path));
    match e {
        FormatError::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied => {
            format!("Can't export {name}: permission denied.")
        }
        other => format!("Can't export {name}: {other}."),
    }
}

/// Status text for a finished export.
pub fn export_report_message(r: &ExportReport) -> String {
    let file = |p: &Path, s: Size| format!("{} ({}×{})", display_name(Some(p)), s.width, s.height);
    match &r.full_color {
        Some((p, s)) => format!(
            "Exported {} and {}",
            file(&r.dithered, r.dithered_size),
            file(p, *s)
        ),
        None => format!("Exported {}", file(&r.dithered, r.dithered_size)),
    }
}

/// Background work: writes the project (atomically).
pub fn save_job(doc: &Document, sources: &SourceStore, path: &Path) -> Result<(), FormatError> {
    rp_format::save_to_path(doc, sources, path)
}

/// Background work: reads a project.
pub fn open_job(path: &Path) -> Result<(Document, SourceStore), FormatError> {
    rp_format::load_from_path(path)
}

/// Background work: flattens, dithers and writes the PNG(s).
pub fn export_job(
    doc: &Document,
    sources: &SourceStore,
    options: &ExportOptions,
    path: &Path,
) -> Result<ExportReport, FormatError> {
    rp_format::export_png(doc, sources, options, path)
}

/// What a background save needs, taken on the UI thread.
#[derive(Clone, Debug)]
pub struct SaveJob {
    pub document: Document,
    pub sources: std::sync::Arc<SourceStore>,
    pub path: PathBuf,
    pub mark: SavedMark,
}

/// What a background export needs, taken on the UI thread.
#[derive(Clone, Debug)]
pub struct ExportJob {
    pub document: Document,
    pub sources: std::sync::Arc<SourceStore>,
    pub options: ExportOptions,
    pub path: PathBuf,
}

impl EditorState {
    /// Whether the document changed since it was last saved or opened.
    pub fn is_dirty(&self) -> bool {
        self.saved != SavedMark::of(self)
    }

    /// Records the current document as saved.
    pub fn mark_saved(&mut self) {
        self.saved = SavedMark::of(self);
    }

    /// `name.rpaint` or `Untitled`.
    pub fn file_name(&self) -> String {
        display_name(self.current_path.as_deref())
    }

    pub fn window_title(&self) -> String {
        window_title(&self.file_name(), self.is_dirty())
    }

    /// Whether a modal file dialog (prompt, export, alert) is open.
    pub fn file_modal_open(&self) -> bool {
        self.unsaved_prompt.is_some() || self.export_dialog.is_some() || self.alert.is_some()
    }

    /// Replaces the document with a fresh one, no questions asked.
    pub fn new_document_now(&mut self) {
        self.set_document(new_document());
        self.sources = std::sync::Arc::new(SourceStore::new());
        self.current_path = None;
        self.mark_saved();
        let s = self.document.canvas_size;
        self.status = format!("New document {}×{}", s.width, s.height);
    }

    /// New, Open or Quit: asks first if the document is dirty.
    pub(crate) fn guard_unsaved(&mut self, action: PendingAction) -> Outcome {
        if self.is_dirty() {
            self.unsaved_prompt = Some(action);
            Outcome::Done
        } else {
            self.proceed(action)
        }
    }

    /// Runs `action` without asking.
    fn proceed(&mut self, action: PendingAction) -> Outcome {
        match action {
            PendingAction::New => {
                self.new_document_now();
                Outcome::Done
            }
            PendingAction::Open => Outcome::PickOpenPath,
            PendingAction::Quit => Outcome::Quit,
        }
    }

    /// Save: to the current file, or ask for one.
    pub(crate) fn save_command(&mut self) -> Outcome {
        match &self.current_path {
            Some(p) => Outcome::SaveTo(p.clone()),
            None => Outcome::PickSavePath,
        }
    }

    /// A button of the unsaved-changes prompt.
    pub fn resolve_unsaved(&mut self, choice: UnsavedChoice) -> Outcome {
        let Some(action) = self.unsaved_prompt.take() else {
            return Outcome::Done;
        };
        match choice {
            UnsavedChoice::Cancel => Outcome::Done,
            UnsavedChoice::DontSave => self.proceed(action),
            UnsavedChoice::Save => {
                self.after_save = Some(action);
                self.save_command()
            }
        }
    }

    /// Window close button: `true` closes now; `false` keeps the window
    /// open because the unsaved-changes prompt is showing (or a file job is
    /// running).
    pub fn request_close(&mut self) -> bool {
        if self.file_busy {
            self.status = "Wait for the file operation to finish".into();
            return false;
        }
        if self.is_dirty() {
            self.export_dialog = None;
            self.alert = None;
            self.unsaved_prompt = Some(PendingAction::Quit);
            return false;
        }
        true
    }

    /// The save dialog was cancelled or failed to open: drop any command
    /// that was waiting for the save.
    pub fn cancel_save_path(&mut self) {
        self.after_save = None;
    }

    /// A save path was chosen (`.rpaint` added if missing).
    pub fn chose_save_path(&mut self, path: PathBuf) -> Outcome {
        Outcome::SaveTo(with_extension(path, PROJECT_EXT))
    }

    /// Starts a save to `path`: marks the editor busy and returns what the
    /// background job needs.
    pub fn begin_save(&mut self, path: PathBuf) -> SaveJob {
        self.file_busy = true;
        self.status = format!("Saving {}…", display_name(Some(&path)));
        SaveJob {
            document: self.document.clone(),
            sources: self.sources.clone(),
            path,
            mark: SavedMark::of(self),
        }
    }

    /// A save finished. On success the file becomes the current one and
    /// any command waiting for the save runs.
    pub fn finish_save(
        &mut self,
        path: PathBuf,
        mark: SavedMark,
        result: Result<(), FormatError>,
    ) -> Outcome {
        self.file_busy = false;
        match result {
            Ok(()) => {
                self.status = format!("Saved {}", display_name(Some(&path)));
                self.current_path = Some(path);
                self.saved = mark;
                match self.after_save.take() {
                    Some(action) => self.proceed(action),
                    None => Outcome::Done,
                }
            }
            Err(e) => {
                self.after_save = None;
                let message = save_error_message(&path, &e);
                self.status = message.clone();
                self.alert = Some(Alert {
                    title: "Save failed",
                    message,
                });
                Outcome::Done
            }
        }
    }

    /// Starts opening `path`.
    pub fn begin_open(&mut self, path: &Path) {
        self.file_busy = true;
        self.status = format!("Opening {}…", display_name(Some(path)));
    }

    /// An open finished: the loaded document replaces the current one, or
    /// the error is shown.
    pub fn finish_open(
        &mut self,
        path: PathBuf,
        result: Result<(Document, SourceStore), FormatError>,
    ) {
        self.file_busy = false;
        match result {
            Ok((doc, sources)) => {
                self.set_document(doc);
                self.sources = std::sync::Arc::new(sources);
                self.status = format!("Opened {}", display_name(Some(&path)));
                self.current_path = Some(path);
                self.mark_saved();
            }
            Err(e) => {
                let message = load_error_message(&path, &e);
                self.status = message.clone();
                let title = match e {
                    FormatError::NewerVersion { .. } => "Newer file format",
                    _ => "Can't open file",
                };
                self.alert = Some(Alert { title, message });
            }
        }
    }

    /// File > Export PNG…: opens the dialog with the last export's settings.
    pub fn open_export_dialog(&mut self) {
        self.export_dialog = Some(ExportDraft::from_options(self.last_export));
    }

    pub fn edit_export_draft(&mut self, f: impl FnOnce(&mut ExportDraft)) {
        if let Some(d) = &mut self.export_dialog {
            f(d);
        }
    }

    pub fn cancel_export_dialog(&mut self) {
        self.export_dialog = None;
    }

    /// Export in the dialog: remembers the settings and asks for a path.
    pub fn confirm_export_dialog(&mut self) -> Outcome {
        match self.export_dialog.take() {
            Some(d) => {
                self.last_export = d.options();
                Outcome::PickExportPath
            }
            None => Outcome::Done,
        }
    }

    /// Starts an export to `path` (`.png` added if missing) with the last
    /// export settings.
    pub fn begin_export(&mut self, path: PathBuf) -> ExportJob {
        let path = with_extension(path, "png");
        self.file_busy = true;
        self.status = format!("Exporting {}…", display_name(Some(&path)));
        ExportJob {
            document: self.document.clone(),
            sources: self.sources.clone(),
            options: self.last_export,
            path,
        }
    }

    pub fn finish_export(&mut self, path: &Path, result: Result<ExportReport, FormatError>) {
        self.file_busy = false;
        self.status = match result {
            Ok(r) => export_report_message(&r),
            Err(e) => export_error_message(path, &e),
        };
    }

    pub fn dismiss_alert(&mut self) {
        self.alert = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::FileCommand;
    use rp_doc::Layer;

    fn tmp_dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("rp-editor-file-ops-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn fresh_document_is_clean_and_untitled() {
        let st = EditorState::new();
        assert!(!st.is_dirty());
        assert_eq!(st.file_name(), "Untitled");
        assert_eq!(st.window_title(), "Untitled — retro-paint");
    }

    #[test]
    fn pixel_edits_make_dirty() {
        let mut st = EditorState::new();
        st.add_paint_layer();
        assert!(st.is_dirty());
        assert_eq!(st.window_title(), "Untitled • — retro-paint");
    }

    #[test]
    fn color_and_swatch_edits_make_dirty_without_doc_version() {
        let mut st = EditorState::new();
        let v = st.doc_version;
        st.swap_colors();
        assert_eq!(st.doc_version, v);
        assert!(st.is_dirty());
        st.swap_colors();
        assert!(!st.is_dirty(), "swapping back is clean");
        st.add_fg_swatch();
        assert!(st.is_dirty());
        st.mark_saved();
        st.remove_fg_swatch();
        assert!(st.is_dirty());
    }

    #[test]
    fn selection_and_view_changes_stay_clean() {
        let mut st = EditorState::new();
        st.select_tool(rp_doc::Tool::Move);
        st.set_view_size(800.0, 600.0);
        st.wheel(crate::viewport::Vec2::new(5.0, 5.0), 0.0, 2.0, true, true);
        st.brush_size_up();
        assert!(!st.is_dirty());
    }

    #[test]
    fn save_marks_clean_with_the_mark_from_when_it_started() {
        let mut st = EditorState::new();
        st.add_paint_layer();
        let job = st.begin_save(PathBuf::from("/x/a.rpaint"));
        assert!(st.file_busy);
        st.add_paint_layer(); // edited while the file was being written
        st.finish_save(job.path, job.mark, Ok(()));
        assert!(!st.file_busy);
        assert!(st.is_dirty());
        assert_eq!(st.current_path, Some(PathBuf::from("/x/a.rpaint")));
        assert_eq!(st.status, "Saved a.rpaint");
        let job = st.begin_save(PathBuf::from("/x/a.rpaint"));
        st.finish_save(job.path, job.mark, Ok(()));
        assert!(!st.is_dirty());
        assert_eq!(st.window_title(), "a.rpaint — retro-paint");
    }

    #[test]
    fn failed_save_stays_dirty_and_alerts() {
        let mut st = EditorState::new();
        st.add_paint_layer();
        let job = st.begin_save(PathBuf::from("/nope/a.rpaint"));
        let err = FormatError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        st.finish_save(job.path, job.mark, Err(err));
        assert!(st.is_dirty());
        assert_eq!(st.current_path, None);
        assert_eq!(st.status, "Can't save a.rpaint: permission denied.");
        assert_eq!(st.alert.as_ref().unwrap().title, "Save failed");
    }

    #[test]
    fn save_uses_current_path_else_asks() {
        let mut st = EditorState::new();
        assert_eq!(st.dispatch(FileCommand::Save), Outcome::PickSavePath);
        st.current_path = Some("/x/a.rpaint".into());
        assert_eq!(
            st.dispatch(FileCommand::Save),
            Outcome::SaveTo("/x/a.rpaint".into())
        );
        assert_eq!(st.dispatch(FileCommand::SaveAs), Outcome::PickSavePath);
        assert_eq!(
            st.chose_save_path("/y/b".into()),
            Outcome::SaveTo("/y/b.rpaint".into())
        );
    }

    #[test]
    fn open_success_replaces_document_and_is_clean() {
        let mut st = EditorState::new();
        st.add_paint_layer();
        let mut doc = new_document();
        doc.push_layer(Layer::paint("Ink", doc.canvas_size));
        doc.fg = Rgb::new(9, 8, 7);
        st.begin_open(Path::new("/x/b.rpaint"));
        st.finish_open("/x/b.rpaint".into(), Ok((doc.clone(), SourceStore::new())));
        assert_eq!(st.document, doc);
        assert!(!st.is_dirty());
        assert!(!st.file_busy);
        assert_eq!(st.file_name(), "b.rpaint");
        assert_eq!(st.status, "Opened b.rpaint");
        assert_eq!(st.selected().unwrap().name, "Ink");
    }

    #[test]
    fn open_newer_version_shows_clear_message_and_keeps_document() {
        let mut st = EditorState::new();
        st.add_paint_layer();
        let before = st.document.clone();
        st.begin_open(Path::new("/x/new.rpaint"));
        st.finish_open(
            "/x/new.rpaint".into(),
            Err(FormatError::NewerVersion {
                found: 3,
                supported: 1,
            }),
        );
        assert_eq!(st.document, before);
        assert!(st.is_dirty());
        assert_eq!(st.current_path, None);
        let a = st.alert.clone().unwrap();
        assert_eq!(a.title, "Newer file format");
        assert_eq!(
            a.message,
            "new.rpaint was saved by a newer retro-paint (format 3); this version \
             supports up to format 1. Update retro-paint to open it."
        );
        assert_eq!(st.status, a.message);
        assert!(st.file_modal_open());
        st.dismiss_alert();
        assert!(!st.file_modal_open());
    }

    #[test]
    fn load_errors_map_to_readable_messages() {
        let p = Path::new("/x/f.rpaint");
        let io = |k| FormatError::Io(std::io::Error::from(k));
        assert_eq!(
            load_error_message(p, &io(std::io::ErrorKind::NotFound)),
            "Can't open f.rpaint: the file does not exist."
        );
        assert_eq!(
            load_error_message(p, &io(std::io::ErrorKind::PermissionDenied)),
            "Can't open f.rpaint: permission denied."
        );
        let zip = rp_format::load(std::io::Cursor::new(b"not a zip".to_vec())).unwrap_err();
        assert_eq!(
            load_error_message(p, &zip),
            "Can't open f.rpaint: it is not a retro-paint project (.rpaint) file."
        );
        let m = load_error_message(
            p,
            &FormatError::MissingEntry {
                name: "manifest.json".into(),
            },
        );
        assert!(
            m.starts_with("Can't open f.rpaint: the project file is damaged"),
            "{m}"
        );
        assert!(m.contains("manifest.json"), "{m}");
    }

    #[test]
    fn unsaved_prompt_guards_new_open_quit() {
        for (cmd, action) in [
            (FileCommand::New, PendingAction::New),
            (FileCommand::Open, PendingAction::Open),
            (FileCommand::Quit, PendingAction::Quit),
        ] {
            let mut st = EditorState::new();
            st.add_paint_layer();
            let before = st.document.clone();
            assert_eq!(st.dispatch(cmd), Outcome::Done);
            assert_eq!(st.unsaved_prompt, Some(action));
            assert_eq!(st.document, before);
            // Cancel: nothing happens.
            assert_eq!(st.resolve_unsaved(UnsavedChoice::Cancel), Outcome::Done);
            assert_eq!(st.unsaved_prompt, None);
            assert_eq!(st.document, before);
        }
    }

    #[test]
    fn clean_document_skips_the_prompt() {
        let mut st = EditorState::new();
        assert_eq!(st.dispatch(FileCommand::Open), Outcome::PickOpenPath);
        assert_eq!(st.dispatch(FileCommand::Quit), Outcome::Quit);
        assert_eq!(st.dispatch(FileCommand::New), Outcome::Done);
        assert_eq!(st.unsaved_prompt, None);
    }

    #[test]
    fn dont_save_proceeds() {
        let mut st = EditorState::new();
        st.add_paint_layer();
        st.dispatch(FileCommand::New);
        assert_eq!(st.resolve_unsaved(UnsavedChoice::DontSave), Outcome::Done);
        assert_eq!(st.document, new_document());
        assert!(!st.is_dirty());

        st.add_paint_layer();
        st.dispatch(FileCommand::Open);
        assert_eq!(
            st.resolve_unsaved(UnsavedChoice::DontSave),
            Outcome::PickOpenPath
        );
        st.dispatch(FileCommand::Quit);
        assert_eq!(st.resolve_unsaved(UnsavedChoice::DontSave), Outcome::Quit);
    }

    #[test]
    fn save_then_continue() {
        let mut st = EditorState::new();
        st.add_paint_layer();
        st.current_path = Some("/x/a.rpaint".into());
        st.dispatch(FileCommand::Quit);
        let out = st.resolve_unsaved(UnsavedChoice::Save);
        assert_eq!(out, Outcome::SaveTo("/x/a.rpaint".into()));
        let job = st.begin_save("/x/a.rpaint".into());
        assert_eq!(st.finish_save(job.path, job.mark, Ok(())), Outcome::Quit);

        // Untitled: Save asks for a path first; cancelling it drops the New.
        let mut st = EditorState::new();
        st.add_paint_layer();
        st.dispatch(FileCommand::New);
        assert_eq!(
            st.resolve_unsaved(UnsavedChoice::Save),
            Outcome::PickSavePath
        );
        st.cancel_save_path();
        assert_eq!(st.after_save, None);
        assert!(st.is_dirty());

        // Path chosen, saved: the New runs.
        st.dispatch(FileCommand::New);
        st.resolve_unsaved(UnsavedChoice::Save);
        let Outcome::SaveTo(p) = st.chose_save_path("/x/n".into()) else {
            panic!()
        };
        let job = st.begin_save(p);
        assert_eq!(st.finish_save(job.path, job.mark, Ok(())), Outcome::Done);
        assert_eq!(st.document, new_document());
        assert_eq!(st.current_path, None);
        assert!(!st.is_dirty());

        // A failed save drops the waiting Open.
        st.add_paint_layer();
        st.current_path = Some("/x/a.rpaint".into());
        st.dispatch(FileCommand::Open);
        st.resolve_unsaved(UnsavedChoice::Save);
        let job = st.begin_save("/x/a.rpaint".into());
        let err = FormatError::Io(std::io::Error::other("disk full"));
        assert_eq!(st.finish_save(job.path, job.mark, Err(err)), Outcome::Done);
        assert_eq!(st.after_save, None);
        assert!(st.is_dirty());
    }

    #[test]
    fn window_close_asks_when_dirty() {
        let mut st = EditorState::new();
        assert!(st.request_close());
        st.swap_colors();
        assert!(!st.request_close());
        assert_eq!(st.unsaved_prompt, Some(PendingAction::Quit));
        assert_eq!(st.resolve_unsaved(UnsavedChoice::DontSave), Outcome::Quit);
        st.file_busy = true;
        assert!(!st.request_close());
    }

    #[test]
    fn busy_refuses_file_commands() {
        let mut st = EditorState::new();
        st.begin_open(Path::new("/x/a.rpaint"));
        for cmd in [
            FileCommand::New,
            FileCommand::Open,
            FileCommand::Save,
            FileCommand::SaveAs,
            FileCommand::ExportPng,
            FileCommand::Quit,
        ] {
            assert_eq!(st.dispatch(cmd), Outcome::Done);
            assert!(st.status.contains("Wait"), "{}", st.status);
        }
        assert!(st.export_dialog.is_none());
    }

    #[test]
    fn commands_are_ignored_while_a_file_dialog_is_open() {
        let mut st = EditorState::new();
        st.dispatch(FileCommand::ExportPng);
        assert!(st.export_dialog.is_some());
        assert_eq!(st.dispatch(FileCommand::Quit), Outcome::Done);
        assert!(st.export_dialog.is_some());
    }

    #[test]
    fn names_and_extensions() {
        assert_eq!(
            with_extension("/a/b".into(), PROJECT_EXT),
            PathBuf::from("/a/b.rpaint")
        );
        assert_eq!(
            with_extension("/a/b.RPAINT".into(), PROJECT_EXT),
            PathBuf::from("/a/b.RPAINT")
        );
        assert_eq!(
            with_extension("/a/b.png".into(), PROJECT_EXT),
            PathBuf::from("/a/b.png.rpaint")
        );
        assert_eq!(
            with_extension("/a/shot.v2".into(), "png"),
            PathBuf::from("/a/shot.v2.png")
        );
        assert_eq!(default_project_name(None), "Untitled.rpaint");
        assert_eq!(
            default_project_name(Some(Path::new("/a/cat.rpaint"))),
            "cat.rpaint"
        );
        assert_eq!(default_export_name(None), "Untitled.png");
        assert_eq!(
            default_export_name(Some(Path::new("/a/cat.rpaint"))),
            "cat.png"
        );
        assert_eq!(
            dialog_directory(Some(Path::new("/a/cat.rpaint")), Some("/home/u".into())),
            PathBuf::from("/a")
        );
        assert_eq!(
            dialog_directory(None, Some("/home/u".into())),
            PathBuf::from("/home/u")
        );
        assert_eq!(dialog_directory(None, None), PathBuf::from("."));
    }

    #[test]
    fn export_draft_clamps_and_sizes() {
        assert_eq!(clamp_scale(0), 1);
        assert_eq!(clamp_scale(-5), 1);
        assert_eq!(clamp_scale(9), 8);
        assert_eq!(clamp_scale(3), 3);
        let mut d = ExportDraft::from_options(ExportOptions {
            scale: 200,
            include_full_color: true,
        });
        assert_eq!(d.scale, 8);
        d.step_scale(1);
        assert_eq!(d.scale, 8);
        d.set_scale(2);
        assert_eq!(d.pixel_size(Size::new(200, 320)), Size::new(400, 640));
        d.step_scale(-5);
        assert_eq!(d.scale, 1);
    }

    #[test]
    fn export_dialog_remembers_last_settings() {
        let mut st = EditorState::new();
        st.dispatch(FileCommand::ExportPng);
        assert_eq!(
            st.export_dialog,
            Some(ExportDraft {
                scale: 1,
                include_full_color: false
            })
        );
        st.edit_export_draft(|d| {
            d.set_scale(4);
            d.include_full_color = true;
        });
        assert_eq!(st.confirm_export_dialog(), Outcome::PickExportPath);
        assert!(st.export_dialog.is_none());
        st.dispatch(FileCommand::ExportPng);
        assert_eq!(st.export_dialog.unwrap().scale, 4);
        st.edit_export_draft(|d| d.set_scale(2));
        st.cancel_export_dialog();
        st.dispatch(FileCommand::ExportPng);
        assert_eq!(st.export_dialog.unwrap().scale, 4, "cancel keeps the last");
        let job = st.begin_export("/x/out".into());
        assert_eq!(job.path, PathBuf::from("/x/out.png"));
        assert_eq!(job.options.scale, 4);
        assert!(job.options.include_full_color);
    }

    #[test]
    fn export_report_and_error_text() {
        let r = ExportReport {
            dithered: "/x/a.png".into(),
            dithered_size: Size::new(400, 640),
            full_color: Some(("/x/a-full.png".into(), Size::new(800, 1280))),
        };
        assert_eq!(
            export_report_message(&r),
            "Exported a.png (400×640) and a-full.png (800×1280)"
        );
        let r = ExportReport {
            full_color: None,
            ..r
        };
        assert_eq!(export_report_message(&r), "Exported a.png (400×640)");
        assert_eq!(
            export_error_message(Path::new("/x/a.png"), &FormatError::InvalidScale(9)),
            "Can't export a.png: export scale 9× is out of range; use 1× to 8×."
        );
    }

    #[test]
    fn save_open_export_jobs_round_trip_on_disk() {
        let dir = tmp_dir("jobs");
        let mut st = EditorState::new();
        st.document.fg = Rgb::new(200, 20, 20);
        st.begin_stroke(
            crate::viewport::Vec2::new(100.0, 100.0),
            crate::paint_ops::PaintButton::Primary,
        );
        st.continue_stroke(crate::viewport::Vec2::new(400.0, 600.0));
        st.end_stroke();
        st.add_fg_swatch();
        let want = rp_format::render_document(&st.document, &st.sources);

        let Outcome::SaveTo(path) = st.chose_save_path(dir.join("art")) else {
            panic!()
        };
        let job = st.begin_save(path);
        let r = save_job(&job.document, &job.sources, &job.path);
        st.finish_save(job.path, job.mark, r);
        assert!(!st.is_dirty());
        let path = dir.join("art.rpaint");
        assert_eq!(st.current_path.as_deref(), Some(path.as_path()));

        st.dispatch(FileCommand::New);
        st.begin_open(&path);
        st.finish_open(path.clone(), open_job(&path));
        assert!(!st.is_dirty());
        assert_eq!(st.document.swatches, vec![Rgb::new(200, 20, 20)]);
        assert_eq!(rp_format::render_document(&st.document, &st.sources), want);

        st.dispatch(FileCommand::ExportPng);
        st.edit_export_draft(|d| {
            d.set_scale(2);
            d.include_full_color = true;
        });
        st.confirm_export_dialog();
        let job = st.begin_export(dir.join("art"));
        let r = export_job(&job.document, &job.sources, &job.options, &job.path);
        st.finish_export(&job.path, r);
        assert_eq!(
            st.status,
            "Exported art.png (400×640) and art-full.png (800×1280)"
        );
        assert!(dir.join("art.png").is_file());
        assert!(dir.join("art-full.png").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
