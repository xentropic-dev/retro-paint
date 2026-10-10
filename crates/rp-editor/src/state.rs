//! Editor state and File-menu command dispatch, with no GPUI types.
//!
//! The GPUI views share one [`EditorState`] entity and forward every menu click and
//! key binding to [`EditorState::dispatch`]. Commands that the `file-ops`
//! feature will implement return [`Outcome::NotImplemented`] for now.

use std::sync::Arc;

use rp_doc::{Document, Layer, LayerId, LayerKind, RgbaImage, Size, SourceStore, Tool};

use crate::filter_ops::PaletteMemory;
use crate::layer_ops::RenameEdit;
use crate::viewport::{Vec2, Viewport};

/// Canvas size of a fresh document: 4× the 200×320 mobile output.
pub const NEW_CANVAS_SIZE: Size = Size::new(800, 1280);

/// Name of the layer a new document starts with.
pub const BACKGROUND_LAYER: &str = "Background";

/// A File-menu command. Each has one GPUI action and one key binding.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FileCommand {
    New,
    Open,
    Save,
    SaveAs,
    ExportPng,
    Quit,
}

impl FileCommand {
    /// Every command, in File-menu order.
    pub const ALL: [FileCommand; 6] = [
        FileCommand::New,
        FileCommand::Open,
        FileCommand::Save,
        FileCommand::SaveAs,
        FileCommand::ExportPng,
        FileCommand::Quit,
    ];

    /// Menu label.
    pub const fn label(self) -> &'static str {
        match self {
            FileCommand::New => "New",
            FileCommand::Open => "Open…",
            FileCommand::Save => "Save",
            FileCommand::SaveAs => "Save As…",
            FileCommand::ExportPng => "Export PNG…",
            FileCommand::Quit => "Quit",
        }
    }

    /// GPUI keystroke string for the binding.
    pub const fn keystroke(self) -> &'static str {
        match self {
            FileCommand::New => "ctrl-n",
            FileCommand::Open => "ctrl-o",
            FileCommand::Save => "ctrl-s",
            FileCommand::SaveAs => "ctrl-shift-s",
            FileCommand::ExportPng => "ctrl-e",
            FileCommand::Quit => "ctrl-q",
        }
    }

    /// Whether a separator follows this item in the menu.
    pub const fn separator_after(self) -> bool {
        matches!(
            self,
            FileCommand::Open | FileCommand::SaveAs | FileCommand::ExportPng
        )
    }
}

/// Human-readable shortcut: `ctrl-shift-s` → `Ctrl+Shift+S`.
pub fn shortcut_label(keystroke: &str) -> String {
    keystroke
        .split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// Display name of a tool for the ribbon.
pub const fn tool_label(tool: Tool) -> &'static str {
    match tool {
        Tool::Select => "Select",
        Tool::Move => "Move",
        Tool::Brush => "Brush",
        Tool::Eraser => "Eraser",
        Tool::Picker => "Picker",
    }
}

/// A zoom command with a key binding (no menu yet).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ViewCommand {
    ZoomIn,
    ZoomOut,
    /// 100%.
    ActualSize,
    /// Fit the whole canvas in the view.
    Fit,
}

impl ViewCommand {
    pub const ALL: [ViewCommand; 4] = [
        ViewCommand::ZoomIn,
        ViewCommand::ZoomOut,
        ViewCommand::ActualSize,
        ViewCommand::Fit,
    ];

    /// GPUI keystroke strings bound to the command.
    pub const fn keystrokes(self) -> &'static [&'static str] {
        match self {
            ViewCommand::ZoomIn => &["ctrl-=", "ctrl-+", "ctrl-shift-="],
            ViewCommand::ZoomOut => &["ctrl--"],
            ViewCommand::ActualSize => &["ctrl-1"],
            ViewCommand::Fit => &["ctrl-0"],
        }
    }
}

/// What the view must do after a command.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// State changed; redraw.
    Done,
    /// Quit the application.
    Quit,
    /// Not wired yet (the `file-ops` feature implements it).
    NotImplemented(FileCommand),
}

/// Everything the editor knows, independent of GPUI.
#[derive(Clone, Debug)]
pub struct EditorState {
    pub document: Document,
    pub tool: Tool,
    /// Whether the File dropdown is open.
    pub file_menu_open: bool,
    /// One-line message for the status bar.
    pub status: String,
    /// Decoded image-layer sources for [`Document::flatten`]. Shared with
    /// the render worker's snapshots; mutate through `Arc::make_mut`.
    pub sources: Arc<SourceStore>,
    /// Bumped whenever the document's pixels may have changed; the view
    /// re-flattens only when this differs from what it last drew.
    pub doc_version: u64,
    /// Zoom and scroll of the canvas area.
    pub viewport: Viewport,
    /// While set, the viewport refits the canvas whenever the canvas area is
    /// resized. Set on startup, after New and by Fit; cleared as soon as the
    /// user pans or zooms.
    pub auto_fit: bool,
    /// Pointer position in canvas coordinates while it is over the canvas
    /// area (may lie outside the canvas itself). The render worker forwards
    /// it to the DOS view.
    pub cursor_canvas: Option<Vec2>,
    /// Last screen position of an active pan drag (middle button, or left
    /// button with space held).
    pub pan_drag: Option<Vec2>,
    /// Whether the space bar is held (space + drag pans).
    pub space_held: bool,
    /// The layer the Layers panel acts on.
    pub selected_layer: Option<LayerId>,
    /// Inline rename in progress in the Layers panel.
    pub renaming: Option<RenameEdit>,
    /// Filter panel: parameters of the palette modes not currently selected.
    pub palette_memory: PaletteMemory,
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorState {
    /// A fresh editor with a new, empty document and the default tool.
    pub fn new() -> Self {
        let document = new_document();
        let s = document.canvas_size;
        let selected_layer = document.layers().last().map(Layer::id);
        Self {
            document,
            tool: Tool::default(),
            file_menu_open: false,
            status: "Ready".into(),
            sources: Arc::new(SourceStore::new()),
            doc_version: 0,
            viewport: Viewport::new(s.width, s.height),
            auto_fit: true,
            cursor_canvas: None,
            pan_drag: None,
            space_held: false,
            selected_layer,
            renaming: None,
            palette_memory: PaletteMemory::default(),
        }
    }

    /// Replaces the document, selects its top layer, marks it changed and
    /// refits the view.
    pub fn set_document(&mut self, document: Document) {
        let s = document.canvas_size;
        self.selected_layer = document.layers().last().map(Layer::id);
        self.renaming = None;
        self.palette_memory = PaletteMemory::default();
        self.document = document;
        self.viewport.set_canvas_size(s.width, s.height);
        self.auto_fit = true;
        self.viewport.fit();
        self.mark_document_changed();
    }

    /// Call after any edit that can change the flattened image.
    pub fn mark_document_changed(&mut self) {
        self.doc_version = self.doc_version.wrapping_add(1);
    }

    /// Flattens the document. The view calls this only when
    /// [`EditorState::doc_version`] changed.
    pub fn flatten(&self) -> RgbaImage {
        self.document.flatten(&self.sources)
    }

    /// Records the canvas area's size; refits while [`EditorState::auto_fit`].
    pub fn set_view_size(&mut self, width: f32, height: f32) {
        let changed = self.viewport.view_size() != Vec2::new(width, height);
        self.viewport.set_view_size(width, height);
        if self.auto_fit && changed && width > 0.0 && height > 0.0 {
            self.viewport.fit();
        }
    }

    /// Runs a View-menu / zoom shortcut command.
    pub fn view_command(&mut self, cmd: ViewCommand) {
        match cmd {
            ViewCommand::ZoomIn => self.viewport.zoom_in_centered(),
            ViewCommand::ZoomOut => self.viewport.zoom_out_centered(),
            ViewCommand::ActualSize => self.viewport.actual_size(),
            ViewCommand::Fit => self.viewport.fit(),
        }
        self.auto_fit = cmd == ViewCommand::Fit;
    }

    /// Pointer moved to `screen` (canvas-area coordinates). Pans if a pan
    /// drag is active, and updates [`EditorState::cursor_canvas`].
    pub fn pointer_moved(&mut self, screen: Vec2) {
        if let Some(last) = self.pan_drag {
            self.viewport.pan_by(screen.x - last.x, screen.y - last.y);
            self.pan_drag = Some(screen);
        }
        self.cursor_canvas = Some(self.viewport.screen_to_canvas(screen));
    }

    /// Pointer left the canvas area.
    pub fn pointer_left(&mut self) {
        self.cursor_canvas = None;
    }

    /// Starts a pan drag at `screen`.
    pub fn begin_pan(&mut self, screen: Vec2) {
        self.pan_drag = Some(screen);
        self.auto_fit = false;
    }

    pub fn end_pan(&mut self) {
        self.pan_drag = None;
    }

    /// Scroll wheel over the canvas at `screen`: pans, or zooms about the
    /// pointer with ctrl held. Keeps the cursor's canvas position current.
    pub fn wheel(&mut self, screen: Vec2, dx: f32, dy: f32, lines: bool, ctrl: bool) {
        self.auto_fit = false;
        if ctrl {
            self.viewport.scroll_zoom(dy, lines, screen);
        } else {
            self.viewport.scroll(dx, dy, lines);
        }
        self.cursor_canvas = Some(self.viewport.screen_to_canvas(screen));
    }

    /// Status bar text for the cursor: the canvas pixel under it, or a dash.
    pub fn cursor_label(&self) -> String {
        let s = self.document.canvas_size;
        if let Some(c) = self.cursor_canvas {
            let (x, y) = (c.x.floor(), c.y.floor());
            if x >= 0.0 && y >= 0.0 && x < s.width as f32 && y < s.height as f32 {
                return format!("{x}, {y}");
            }
        }
        "—".into()
    }

    /// Status bar text for the zoom, e.g. `"52%"`.
    pub fn zoom_label(&self) -> String {
        format!("{}%", self.viewport.zoom_percent())
    }

    /// Runs a File-menu command. Always closes the File dropdown.
    pub fn dispatch(&mut self, cmd: FileCommand) -> Outcome {
        self.file_menu_open = false;
        match cmd {
            FileCommand::New => {
                self.set_document(new_document());
                let s = self.document.canvas_size;
                self.status = format!("New document {}×{}", s.width, s.height);
                Outcome::Done
            }
            FileCommand::Quit => Outcome::Quit,
            FileCommand::Open
            | FileCommand::Save
            | FileCommand::SaveAs
            | FileCommand::ExportPng => {
                self.status = format!("{}: not implemented yet", cmd.label().trim_end_matches('…'));
                Outcome::NotImplemented(cmd)
            }
        }
    }

    pub fn select_tool(&mut self, tool: Tool) {
        self.tool = tool;
    }

    pub fn toggle_file_menu(&mut self) {
        self.file_menu_open = !self.file_menu_open;
    }

    pub fn close_menus(&mut self) {
        self.file_menu_open = false;
    }
}

/// The document File > New creates: [`NEW_CANVAS_SIZE`] with one opaque
/// white paint layer named [`BACKGROUND_LAYER`], like a fresh sheet of paper.
pub fn new_document() -> Document {
    let mut doc = Document::new(NEW_CANVAS_SIZE);
    let mut layer = Layer::paint(BACKGROUND_LAYER, NEW_CANVAS_SIZE);
    if let LayerKind::Paint(p) = &mut layer.kind {
        p.pixels = RgbaImage::filled(NEW_CANVAS_SIZE, [255, 255, 255, 255]);
    }
    doc.push_layer(layer);
    doc
}

#[cfg(test)]
mod tests {
    use super::*;
    use rp_doc::Layer;
    use std::collections::HashSet;

    #[test]
    fn starts_with_default_document_and_brush() {
        let st = EditorState::new();
        assert_eq!(st.document, new_document());
        assert_eq!(st.document.canvas_size, NEW_CANVAS_SIZE);
        assert_eq!(st.document.output_size, Size::new(200, 320));
        assert_eq!(st.document.layers().len(), 1);
        assert_eq!(st.tool, Tool::Brush);
        assert!(!st.file_menu_open);
    }

    #[test]
    fn new_replaces_document_and_closes_menu() {
        let mut st = EditorState::new();
        st.document
            .push_layer(Layer::paint("Paint", NEW_CANVAS_SIZE));
        st.document
            .swatches
            .push(rp_palette_dither::Rgb::new(1, 2, 3));
        st.file_menu_open = true;
        assert_eq!(st.dispatch(FileCommand::New), Outcome::Done);
        assert_eq!(st.document, new_document());
        assert!(!st.file_menu_open);
    }

    #[test]
    fn new_keeps_selected_tool() {
        let mut st = EditorState::new();
        st.select_tool(Tool::Picker);
        st.dispatch(FileCommand::New);
        assert_eq!(st.tool, Tool::Picker);
    }

    #[test]
    fn quit_returns_quit() {
        let mut st = EditorState::new();
        assert_eq!(st.dispatch(FileCommand::Quit), Outcome::Quit);
    }

    #[test]
    fn file_ops_are_stubs_that_leave_document_alone() {
        for cmd in [
            FileCommand::Open,
            FileCommand::Save,
            FileCommand::SaveAs,
            FileCommand::ExportPng,
        ] {
            let mut st = EditorState::new();
            st.document
                .push_layer(Layer::paint("Paint", NEW_CANVAS_SIZE));
            let before = st.document.clone();
            st.file_menu_open = true;
            assert_eq!(st.dispatch(cmd), Outcome::NotImplemented(cmd));
            assert_eq!(st.document, before);
            assert!(!st.file_menu_open);
            assert!(st.status.contains("not implemented"), "{}", st.status);
        }
    }

    #[test]
    fn tool_selection_covers_every_tool() {
        let mut st = EditorState::new();
        for tool in Tool::ALL {
            st.select_tool(tool);
            assert_eq!(st.tool, tool);
        }
        let labels: HashSet<_> = Tool::ALL.iter().map(|&t| tool_label(t)).collect();
        assert_eq!(labels.len(), Tool::ALL.len());
    }

    #[test]
    fn toggle_and_close_file_menu() {
        let mut st = EditorState::new();
        st.toggle_file_menu();
        assert!(st.file_menu_open);
        st.toggle_file_menu();
        assert!(!st.file_menu_open);
        st.toggle_file_menu();
        st.close_menus();
        assert!(!st.file_menu_open);
    }

    #[test]
    fn keystrokes_are_the_spec_bindings_and_unique() {
        let keys: Vec<_> = FileCommand::ALL.iter().map(|c| c.keystroke()).collect();
        assert_eq!(
            keys,
            [
                "ctrl-n",
                "ctrl-o",
                "ctrl-s",
                "ctrl-shift-s",
                "ctrl-e",
                "ctrl-q"
            ]
        );
        let unique: HashSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len());
    }

    #[test]
    fn new_document_is_a_white_page() {
        let doc = new_document();
        assert_eq!(doc.layers()[0].name, BACKGROUND_LAYER);
        let flat = doc.flatten(&SourceStore::new());
        assert_eq!(flat.size(), NEW_CANVAS_SIZE);
        assert!(flat.as_raw().iter().all(|&b| b == 255));
    }

    #[test]
    fn new_bumps_doc_version_and_refits() {
        let mut st = EditorState::new();
        st.set_view_size(1000.0, 700.0);
        st.wheel(Vec2::new(10.0, 10.0), 0.0, 3.0, true, true);
        assert!(!st.auto_fit);
        let v = st.doc_version;
        st.dispatch(FileCommand::New);
        assert_ne!(st.doc_version, v);
        assert!(st.auto_fit);
        assert_eq!(st.viewport.zoom(), st.viewport.fit_zoom());
    }

    #[test]
    fn stubbed_commands_leave_doc_version_alone() {
        let mut st = EditorState::new();
        let v = st.doc_version;
        st.dispatch(FileCommand::Save);
        assert_eq!(st.doc_version, v);
    }

    #[test]
    fn refits_on_resize_until_the_user_navigates() {
        let mut st = EditorState::new();
        st.set_view_size(0.0, 0.0);
        assert!(st.auto_fit);
        st.set_view_size(1000.0, 700.0);
        let z = st.viewport.zoom();
        assert!((z - (700.0 - 48.0) / 1280.0).abs() < 1e-6);
        st.set_view_size(1000.0, 500.0);
        assert!((st.viewport.zoom() - (500.0 - 48.0) / 1280.0).abs() < 1e-6);
        st.begin_pan(Vec2::new(1.0, 1.0));
        st.end_pan();
        assert!(!st.auto_fit);
        let z = st.viewport.zoom();
        st.set_view_size(1200.0, 800.0);
        assert_eq!(st.viewport.zoom(), z);
    }

    #[test]
    fn view_commands_zoom_and_fit() {
        let mut st = EditorState::new();
        st.set_view_size(1000.0, 700.0);
        st.view_command(ViewCommand::ActualSize);
        assert_eq!(st.viewport.zoom(), 1.0);
        assert!(!st.auto_fit);
        st.view_command(ViewCommand::ZoomIn);
        assert_eq!(st.viewport.zoom(), 1.5);
        st.view_command(ViewCommand::ZoomOut);
        st.view_command(ViewCommand::ZoomOut);
        assert!((st.viewport.zoom() - 2.0 / 3.0).abs() < 1e-6);
        st.view_command(ViewCommand::Fit);
        assert!(st.auto_fit);
        assert_eq!(st.viewport.zoom(), st.viewport.fit_zoom());
        let keys: HashSet<_> = ViewCommand::ALL
            .iter()
            .flat_map(|c| c.keystrokes().iter().copied())
            .chain(FileCommand::ALL.iter().map(|c| c.keystroke()))
            .collect();
        let n: usize = ViewCommand::ALL.iter().map(|c| c.keystrokes().len()).sum();
        assert_eq!(keys.len(), n + FileCommand::ALL.len());
    }

    #[test]
    fn cursor_tracks_canvas_coordinates() {
        let mut st = EditorState::new();
        st.set_view_size(1000.0, 700.0);
        st.viewport.zoom_about(2.0, Vec2::new(0.0, 0.0));
        let o = st.viewport.offset();
        st.pointer_moved(Vec2::new(o.x + 21.0, o.y + 41.0));
        let c = st.cursor_canvas.unwrap();
        assert!(
            (c.x - 10.5).abs() < 1e-3 && (c.y - 20.5).abs() < 1e-3,
            "{c:?}"
        );
        assert_eq!(st.cursor_label(), "10, 20");
        st.pointer_moved(Vec2::new(o.x - 5.0, o.y));
        assert_eq!(st.cursor_label(), "—");
        st.pointer_left();
        assert_eq!(st.cursor_canvas, None);
        assert_eq!(st.zoom_label(), "200%");
    }

    #[test]
    fn pan_drag_moves_the_canvas_with_the_pointer() {
        let mut st = EditorState::new();
        st.set_view_size(1000.0, 700.0);
        st.viewport.zoom_about(4.0, Vec2::new(500.0, 350.0));
        let o = st.viewport.offset();
        let grabbed = st.viewport.screen_to_canvas(Vec2::new(300.0, 300.0));
        st.begin_pan(Vec2::new(300.0, 300.0));
        st.pointer_moved(Vec2::new(320.0, 290.0));
        st.pointer_moved(Vec2::new(340.0, 280.0));
        st.end_pan();
        assert_eq!(st.viewport.offset(), Vec2::new(o.x + 40.0, o.y - 20.0));
        // The grabbed canvas point stays under the pointer.
        assert_eq!(st.cursor_canvas, Some(grabbed));
        st.pointer_moved(Vec2::new(0.0, 0.0));
        assert_eq!(st.viewport.offset(), Vec2::new(o.x + 40.0, o.y - 20.0));
    }

    #[test]
    fn wheel_pans_or_zooms_about_pointer() {
        let mut st = EditorState::new();
        st.set_view_size(1000.0, 700.0);
        let p = Vec2::new(400.0, 300.0);
        let c = st.viewport.screen_to_canvas(p);
        let z = st.viewport.zoom();
        st.wheel(p, 0.0, 1.0, true, true);
        assert!(st.viewport.zoom() > z);
        let after = st.viewport.screen_to_canvas(p);
        assert!((after.x - c.x).abs() < 1e-3 && (after.y - c.y).abs() < 1e-3);
        let o = st.viewport.offset();
        st.wheel(p, 0.0, -1.0, true, false);
        assert_eq!(st.viewport.offset().y, o.y - crate::viewport::LINE_PX);
        assert_eq!(st.cursor_canvas, Some(st.viewport.screen_to_canvas(p)));
    }

    #[test]
    fn shortcut_labels() {
        assert_eq!(shortcut_label("ctrl-n"), "Ctrl+N");
        assert_eq!(shortcut_label("ctrl-shift-s"), "Ctrl+Shift+S");
    }
}
