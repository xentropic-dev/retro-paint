//! Editor state and File-menu command dispatch, with no GPUI types.
//!
//! The GPUI view owns one [`EditorState`] and forwards every menu click and
//! key binding to [`EditorState::dispatch`]. Commands that the `file-ops`
//! feature will implement return [`Outcome::NotImplemented`] for now.

use rp_doc::{Document, Size, Tool};

/// Canvas size of a fresh document: 4× the 200×320 mobile output.
pub const NEW_CANVAS_SIZE: Size = Size::new(800, 1280);

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
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorState {
    /// A fresh editor with a new, empty document and the default tool.
    pub fn new() -> Self {
        Self {
            document: new_document(),
            tool: Tool::default(),
            file_menu_open: false,
            status: "Ready".into(),
        }
    }

    /// Runs a File-menu command. Always closes the File dropdown.
    pub fn dispatch(&mut self, cmd: FileCommand) -> Outcome {
        self.file_menu_open = false;
        match cmd {
            FileCommand::New => {
                self.document = new_document();
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

/// The document File > New creates.
pub fn new_document() -> Document {
    Document::new(NEW_CANVAS_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rp_doc::Layer;
    use std::collections::HashSet;

    #[test]
    fn starts_with_empty_default_document_and_brush() {
        let st = EditorState::new();
        assert_eq!(st.document, new_document());
        assert_eq!(st.document.canvas_size, NEW_CANVAS_SIZE);
        assert_eq!(st.document.output_size, Size::new(200, 320));
        assert!(st.document.layers().is_empty());
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
    fn shortcut_labels() {
        assert_eq!(shortcut_label("ctrl-n"), "Ctrl+N");
        assert_eq!(shortcut_label("ctrl-shift-s"), "Ctrl+Shift+S");
    }
}
