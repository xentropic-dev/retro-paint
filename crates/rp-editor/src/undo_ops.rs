//! Undo / redo on [`EditorState`], built on [`rp_doc::History`], with no
//! GPUI types.
//!
//! Every document edit goes through one of three paths here:
//! - [`EditorState::record`]: a one-shot edit (layer ops, filter changes,
//!   insert image, swatch add/remove) as one snapshot step;
//! - a gesture: a brush/eraser stroke opens a paint step on press and
//!   commits it on release (only the stroke's dirty rectangle is stored); a
//!   mask stroke opens a snapshot step on press (only the stroked mask is
//!   copied, the rest is shared copy-on-write) and commits it on release; a
//!   move/scale/rotate drag opens a snapshot step on press and commits it on
//!   release;
//! - Project Settings pushes the document it returns as a snapshot step.
//!
//! Decisions:
//! - **Memory bound**: the history keeps about [`DEFAULT_BUDGET`] (512 MiB)
//!   of steps; the oldest are dropped first, and the newest is always kept.
//!   Strokes store only the changed rectangle of the layer; snapshot steps
//!   count every paint layer and mask they hold in full.
//! - **Coalescing**: consecutive edits of the same control merge into one
//!   step when nothing else was recorded in between: a filter slider (bar
//!   clicks, −/+ steps), one layer's opacity, and arrow-key nudges of one
//!   layer. A reset, any other step, an undo, or a redo ends the run. This
//!   works because a snapshot step swaps the whole document, so later edits
//!   of the same run need no step of their own.
//! - **Not steps**: fg/bg colors, selection, viewport, tool, brush, panel
//!   collapse. Undo/redo keep the current fg/bg (a snapshot step would
//!   otherwise bring back the colors of its moment).
//! - Undo/redo first finish any stroke or drag in progress (so it becomes
//!   the step undone) and cancel an inline rename. They do nothing while a
//!   modal dialog is open.
//! - After undo/redo the selected layer is kept if it still exists, else the
//!   top layer is selected; a changed canvas size refits the view.
//! - New and Open clear the history.
//! - **Dirty tracking**: [`EditorState::content`] names the document's
//!   content: each edit sets it to the new [`EditorState::doc_version`], and
//!   each step remembers the content before and after it. Undo back to what
//!   was saved therefore makes the document clean again. A gesture that
//!   changed nothing records no step and restores the previous content.

use std::collections::VecDeque;

use rp_doc::history::DEFAULT_BUDGET;
use rp_doc::{Document, Edit, History, Layer, LayerId, Rect};

use crate::filter_ops::FilterSlider;
use crate::flip_ops::FlipAxis;
use crate::state::EditorState;

/// What a step did, for the Edit menu ("Undo Brush Stroke").
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum EditLabel {
    BrushStroke,
    EraserStroke,
    MaskStroke,
    AddLayer,
    DeleteLayer,
    ReorderLayer,
    RenameLayer,
    Visibility,
    Opacity,
    BlendMode,
    LockPixels,
    LockPosition,
    AddMask,
    RemoveMask,
    Filter,
    InsertImage,
    Move,
    Scale,
    Rotate,
    Nudge,
    ProjectSettings,
    AddSwatch,
    RemoveSwatch,
    DuplicateLayer,
    FlipHorizontal,
    FlipVertical,
    RasterizeLayer,
    ClearSelection,
    TransformSelection,
}

impl EditLabel {
    pub const fn label(self) -> &'static str {
        match self {
            EditLabel::BrushStroke => "Brush Stroke",
            EditLabel::EraserStroke => "Eraser Stroke",
            EditLabel::MaskStroke => "Mask Stroke",
            EditLabel::AddLayer => "Add Layer",
            EditLabel::DeleteLayer => "Delete Layer",
            EditLabel::ReorderLayer => "Reorder Layer",
            EditLabel::RenameLayer => "Rename Layer",
            EditLabel::Visibility => "Layer Visibility",
            EditLabel::Opacity => "Layer Opacity",
            EditLabel::BlendMode => "Blend Mode",
            EditLabel::LockPixels => "Lock Pixels",
            EditLabel::LockPosition => "Lock Position",
            EditLabel::AddMask => "Add Mask",
            EditLabel::RemoveMask => "Remove Mask",
            EditLabel::Filter => "Filter Change",
            EditLabel::InsertImage => "Insert Image",
            EditLabel::Move => "Move",
            EditLabel::Scale => "Scale",
            EditLabel::Rotate => "Rotate",
            EditLabel::Nudge => "Nudge",
            EditLabel::ProjectSettings => "Project Settings",
            EditLabel::AddSwatch => "Add Swatch",
            EditLabel::RemoveSwatch => "Remove Swatch",
            EditLabel::DuplicateLayer => "Duplicate Layer",
            EditLabel::FlipHorizontal => "Flip Horizontal",
            EditLabel::FlipVertical => "Flip Vertical",
            EditLabel::RasterizeLayer => "Rasterize Layer",
            EditLabel::ClearSelection => "Clear Selection",
            EditLabel::TransformSelection => "Transform Selection",
        }
    }
}

/// Edits that merge into the previous step when they follow it directly.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Coalesce {
    Filter(FilterSlider),
    Opacity(LayerId),
    Nudge(LayerId),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct StepMeta {
    label: EditLabel,
    coalesce: Option<Coalesce>,
    /// [`EditorState::content`] before and after the step.
    before: u64,
    after: u64,
}

/// What an undo or redo did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Applied {
    pub label: EditLabel,
    /// The content id the document now has.
    pub content: u64,
}

/// [`History`] plus a label and content ids per step, kept in step with it.
#[derive(Clone, Debug)]
pub struct UndoHistory {
    history: History,
    /// Oldest at the front, like `History`'s undo stack.
    undo: VecDeque<StepMeta>,
    /// Next redo at the back.
    redo: Vec<StepMeta>,
    pending: Option<StepMeta>,
}

impl Default for UndoHistory {
    fn default() -> Self {
        Self::with_budget(DEFAULT_BUDGET)
    }
}

impl UndoHistory {
    pub fn with_budget(bytes: usize) -> Self {
        Self {
            history: History::with_budget(bytes),
            undo: VecDeque::new(),
            redo: Vec::new(),
            pending: None,
        }
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    pub fn undo_label(&self) -> Option<EditLabel> {
        self.undo.back().map(|m| m.label)
    }

    pub fn redo_label(&self) -> Option<EditLabel> {
        self.redo.last().map(|m| m.label)
    }

    pub fn undo_len(&self) -> usize {
        self.history.undo_len()
    }

    pub fn redo_len(&self) -> usize {
        self.history.redo_len()
    }

    pub fn is_open(&self) -> bool {
        self.pending.is_some()
    }

    pub fn clear(&mut self) {
        self.history.clear();
        self.undo.clear();
        self.redo.clear();
        self.pending = None;
    }

    /// Drops metadata of steps the history evicted or cleared.
    fn sync(&mut self) {
        while self.undo.len() > self.history.undo_len() {
            self.undo.pop_front();
        }
        while self.redo.len() > self.history.redo_len() {
            self.redo.remove(0);
        }
        debug_assert_eq!(self.undo.len(), self.history.undo_len());
        debug_assert_eq!(self.redo.len(), self.history.redo_len());
    }

    fn open(&mut self, label: EditLabel, coalesce: Option<Coalesce>, content: u64) {
        self.pending = Some(StepMeta {
            label,
            coalesce,
            before: content,
            after: content,
        });
    }

    /// Opens a snapshot step (see [`History::begin_snapshot`]). Commit any
    /// open step first.
    pub fn begin_snapshot(
        &mut self,
        doc: &Document,
        label: EditLabel,
        coalesce: Option<Coalesce>,
        content: u64,
    ) {
        self.commit(doc, None, content);
        self.history.begin_snapshot(doc);
        self.open(label, coalesce, content);
    }

    /// Opens a paint step on `layer`; `false` if it is not a paint layer.
    pub fn begin_paint(
        &mut self,
        doc: &Document,
        layer: LayerId,
        label: EditLabel,
        content: u64,
    ) -> bool {
        self.commit(doc, None, content);
        if !self.history.begin_paint(doc, layer) {
            return false;
        }
        self.open(label, None, content);
        true
    }

    /// Closes the open step (`hint` limits a paint step's dirty search).
    /// Returns `Some(true)` if a step was recorded, `Some(false)` with the
    /// content id from before the step if nothing changed, `None` if no step
    /// was open.
    pub fn commit(
        &mut self,
        doc: &Document,
        hint: Option<Rect>,
        content: u64,
    ) -> Option<Result<(), u64>> {
        let mut meta = self.pending.take()?;
        if !self.history.commit_within(doc, hint) {
            return Some(Err(meta.before));
        }
        meta.after = content;
        self.redo.clear();
        self.undo.push_back(meta);
        self.sync();
        Some(Ok(()))
    }

    /// Drops the open step; the caller restores the document.
    pub fn cancel(&mut self) {
        self.history.cancel();
        self.pending = None;
    }

    /// Records a finished snapshot step whose before-document is `before`.
    pub fn push_snapshot(
        &mut self,
        before: Document,
        label: EditLabel,
        content_before: u64,
        content_after: u64,
    ) {
        self.cancel();
        self.history.push(Edit::snapshot(before));
        self.redo.clear();
        self.undo.push_back(StepMeta {
            label,
            coalesce: None,
            before: content_before,
            after: content_after,
        });
        self.sync();
    }

    /// Whether an edit keyed `key` at document content `content` merges into
    /// the newest step.
    pub fn coalesces(&self, key: Coalesce, content: u64) -> bool {
        self.pending.is_none()
            && !self.can_redo()
            && self
                .undo
                .back()
                .is_some_and(|m| m.coalesce == Some(key) && m.after == content)
    }

    /// After a merged edit: the newest step now ends at `content`.
    pub fn extend_newest(&mut self, content: u64) {
        if let Some(m) = self.undo.back_mut() {
            m.after = content;
        }
    }

    /// Reverts the newest step. `None` if there was nothing to undo or the
    /// history was out of sync (it is then cleared).
    pub fn undo(&mut self, doc: &mut Document) -> Option<Applied> {
        self.pending = None;
        let ok = self.history.undo(doc);
        if !ok {
            self.clear();
            return None;
        }
        let meta = self.undo.pop_back().expect("meta in step with history");
        self.redo.push(meta);
        self.sync();
        Some(Applied {
            label: meta.label,
            content: meta.before,
        })
    }

    /// Reapplies the newest undone step. Same failure rules as
    /// [`undo`](Self::undo).
    pub fn redo(&mut self, doc: &mut Document) -> Option<Applied> {
        self.pending = None;
        let ok = self.history.redo(doc);
        if !ok {
            self.clear();
            return None;
        }
        let meta = self.redo.pop().expect("meta in step with history");
        self.undo.push_back(meta);
        self.sync();
        Some(Applied {
            label: meta.label,
            content: meta.after,
        })
    }
}

/// An Edit-menu command.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum EditCommand {
    Undo,
    Redo,
    /// Paste an image or image files from the clipboard. The view reads the
    /// clipboard (see `insert_flow::paste_from_clipboard`).
    Paste,
    /// Duplicate the selected layer (see [`crate::flip_ops`]).
    DuplicateLayer,
    /// Mirror the selected layer left ↔ right.
    FlipHorizontal,
    /// Mirror the selected layer top ↔ bottom.
    FlipVertical,
    /// Turn the selected image layer into a paint layer (see
    /// [`crate::rasterize_ops`]).
    RasterizeLayer,
    /// Select the whole canvas (see [`crate::select_ops`]).
    SelectAll,
    /// Drop the selection.
    Deselect,
    /// Erase the selected pixels of the selected layer (Delete).
    Clear,
    /// Show the transform box on the selection (see
    /// [`crate::float_ops`]).
    TransformSelection,
}

impl EditCommand {
    /// Every command, in Edit-menu order.
    pub const ALL: [EditCommand; 11] = [
        EditCommand::Undo,
        EditCommand::Redo,
        EditCommand::Paste,
        EditCommand::DuplicateLayer,
        EditCommand::FlipHorizontal,
        EditCommand::FlipVertical,
        EditCommand::RasterizeLayer,
        EditCommand::SelectAll,
        EditCommand::Deselect,
        EditCommand::Clear,
        EditCommand::TransformSelection,
    ];

    /// GPUI keystroke strings bound to the command (possibly none); the
    /// first is shown in the menu.
    pub const fn keystrokes(self) -> &'static [&'static str] {
        match self {
            EditCommand::Undo => &["ctrl-z"],
            EditCommand::Redo => &["ctrl-shift-z", "ctrl-y"],
            EditCommand::Paste => &["ctrl-v"],
            EditCommand::DuplicateLayer => &["ctrl-j"],
            EditCommand::SelectAll => &["ctrl-a"],
            EditCommand::Deselect => &["ctrl-d"],
            EditCommand::TransformSelection => &["ctrl-t"],
            // Delete and Backspace are plain keys, handled by the view's
            // key handler (not while renaming a layer).
            EditCommand::FlipHorizontal
            | EditCommand::FlipVertical
            | EditCommand::RasterizeLayer
            | EditCommand::Clear => &[],
        }
    }

    /// Shortcut shown in the Edit menu, if any.
    pub fn shortcut_hint(self) -> Option<String> {
        match self {
            EditCommand::Clear => Some("Delete".into()),
            _ => self
                .keystrokes()
                .first()
                .map(|k| crate::state::shortcut_label(k)),
        }
    }

    pub const fn verb(self) -> &'static str {
        match self {
            EditCommand::Undo => "Undo",
            EditCommand::Redo => "Redo",
            EditCommand::Paste => "Paste",
            EditCommand::DuplicateLayer => "Duplicate Layer",
            EditCommand::FlipHorizontal => "Flip Horizontal",
            EditCommand::FlipVertical => "Flip Vertical",
            EditCommand::RasterizeLayer => "Rasterize Layer",
            EditCommand::SelectAll => "Select All",
            EditCommand::Deselect => "Deselect",
            EditCommand::Clear => "Clear",
            EditCommand::TransformSelection => "Transform Selection",
        }
    }

    /// Whether the Edit menu draws a separator below this entry.
    pub const fn separator_after(self) -> bool {
        matches!(
            self,
            EditCommand::Redo | EditCommand::Paste | EditCommand::RasterizeLayer
        )
    }

    /// Menu label: `Undo Brush Stroke`, or just `Undo` with nothing to undo.
    pub fn menu_label(self, history: &UndoHistory) -> String {
        let step = match self {
            EditCommand::Undo => history.undo_label(),
            EditCommand::Redo => history.redo_label(),
            _ => None,
        };
        match step {
            Some(l) => format!("{} {}", self.verb(), l.label()),
            None => self.verb().to_string(),
        }
    }

    /// Whether the menu entry is enabled.
    pub fn enabled(self, history: &UndoHistory) -> bool {
        match self {
            EditCommand::Undo => history.can_undo(),
            EditCommand::Redo => history.can_redo(),
            _ => true,
        }
    }
}

impl EditorState {
    /// Runs a one-shot document edit as one snapshot step (none if the
    /// document did not change).
    pub(crate) fn record<R>(&mut self, label: EditLabel, f: impl FnOnce(&mut Self) -> R) -> R {
        self.settle_float();
        self.history
            .begin_snapshot(&self.document, label, None, self.content);
        let r = f(self);
        self.commit_step(None);
        r
    }

    /// Like [`record`](Self::record), but merges into the newest step if it
    /// was the same control (see the module docs).
    pub(crate) fn record_coalesced<R>(
        &mut self,
        label: EditLabel,
        key: Coalesce,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        self.settle_float();
        if self.history.coalesces(key, self.content) {
            let r = f(self);
            self.history.extend_newest(self.content);
            return r;
        }
        self.history
            .begin_snapshot(&self.document, label, Some(key), self.content);
        let r = f(self);
        self.commit_step(None);
        r
    }

    /// Commits the open step, if any. When nothing changed, the content id
    /// goes back to what it was, so the document is not marked dirty.
    pub(crate) fn commit_step(&mut self, hint: Option<Rect>) {
        if let Some(Err(before)) = self.history.commit(&self.document, hint, self.content) {
            self.content = before;
        }
    }

    /// Finishes any stroke or drag (recording it) and cancels a rename.
    pub(crate) fn finish_gestures(&mut self) {
        if self.stroke.is_some() {
            self.end_stroke();
        }
        if self.transform_drag.is_some() {
            self.end_transform();
        }
        self.settle_float();
        self.renaming = None;
    }

    /// Whether undo/redo are blocked by a modal dialog.
    fn history_blocked(&self) -> bool {
        self.file_modal_open() || self.project_dialog.is_some() || self.color_picker.is_some()
    }

    /// Runs an Edit-menu command (or its key binding). Closes the menus.
    /// Paste only starts here ([`begin_paste`](Self::begin_paste)); the view
    /// reads the clipboard and finishes it.
    pub fn edit_command(&mut self, cmd: EditCommand) -> bool {
        self.close_menus();
        match cmd {
            EditCommand::Undo => self.undo(),
            EditCommand::Redo => self.redo(),
            EditCommand::Paste => self.begin_paste(),
            EditCommand::DuplicateLayer => self.layer_command(|s| s.duplicate_selected_layer()),
            EditCommand::FlipHorizontal => {
                self.layer_command(|s| s.flip_selected_layer(FlipAxis::Horizontal))
            }
            EditCommand::FlipVertical => {
                self.layer_command(|s| s.flip_selected_layer(FlipAxis::Vertical))
            }
            EditCommand::RasterizeLayer => self.layer_command(|s| s.rasterize_selected_layer()),
            EditCommand::SelectAll => self.selection_command(|s| {
                s.select_all();
                false
            }),
            EditCommand::Deselect => self.selection_command(|s| {
                s.deselect();
                false
            }),
            EditCommand::Clear => self.layer_command(|s| s.clear_selected()),
            EditCommand::TransformSelection => self.selection_command(|s| s.transform_selection()),
        }
    }

    /// A selection command (not a document edit): nothing while a dialog
    /// is open.
    fn selection_command(&mut self, f: impl FnOnce(&mut Self) -> bool) -> bool {
        if self.history_blocked() {
            return false;
        }
        f(self)
    }

    /// A layer command from the Edit menu or a key: nothing while a dialog
    /// is open; otherwise finishes any gesture first so it stays its own step.
    fn layer_command(&mut self, f: impl FnOnce(&mut Self) -> bool) -> bool {
        if self.history_blocked() {
            return false;
        }
        self.finish_gestures();
        f(self)
    }

    /// Ctrl+Z: reverts the newest step. Returns whether anything changed.
    pub fn undo(&mut self) -> bool {
        self.apply_history(EditCommand::Undo)
    }

    /// Ctrl+Shift+Z / Ctrl+Y: reapplies the newest undone step.
    pub fn redo(&mut self) -> bool {
        self.apply_history(EditCommand::Redo)
    }

    fn apply_history(&mut self, cmd: EditCommand) -> bool {
        if self.history_blocked() {
            return false;
        }
        if cmd == EditCommand::Undo && self.cancel_float() {
            return true;
        }
        self.finish_gestures();
        if !cmd.enabled(&self.history) {
            self.status = match cmd {
                EditCommand::Undo => "Nothing to undo".into(),
                EditCommand::Redo => "Nothing to redo".into(),
                _ => unreachable!("only undo and redo are history steps"),
            };
            return false;
        }
        let (fg, bg) = (self.document.fg, self.document.bg);
        let canvas = self.document.canvas_size;
        let applied = match cmd {
            EditCommand::Undo => self.history.undo(&mut self.document),
            EditCommand::Redo => self.history.redo(&mut self.document),
            _ => unreachable!("only undo and redo are history steps"),
        };
        let Some(applied) = applied else {
            self.status = "Undo history was out of step with the document and was cleared".into();
            return false;
        };
        self.document.fg = fg;
        self.document.bg = bg;
        self.content = applied.content;
        self.doc_version = self.doc_version.wrapping_add(1);
        self.fix_selection();
        self.drop_stale_selection();
        let s = self.document.canvas_size;
        if s != canvas {
            self.viewport.set_canvas_size(s.width, s.height);
            self.auto_fit = true;
            self.viewport.fit();
        }
        self.status = format!("{} {}", cmd.verb(), applied.label.label());
        true
    }

    /// Keeps the selection if its layer exists, else selects the top layer.
    pub(crate) fn fix_selection(&mut self) {
        let keep = self
            .selected_layer
            .is_some_and(|id| self.document.layer(id).is_some());
        if !keep {
            self.selected_layer = self.document.layers().last().map(Layer::id);
        }
        self.fix_edit_target();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_ops::UnsavedChoice;
    use crate::filter_ops::{AdjustControl, PaletteMode};
    use crate::image_ops::LoadedImage;
    use crate::paint_ops::PaintButton;
    use crate::project_ops::ProjectSettings;
    use crate::state::{FileCommand, ViewCommand};
    use crate::viewport::Vec2;
    use rp_doc::{BlendMode, EditKind, LayerKind, RgbaImage, Size, SourceStore, Tool};
    use rp_palette_dither::{DitherMethod, Preset, Rgb};

    const S: Size = Size::new(64, 48);

    /// A small white document with a transparent "Paint" layer on top
    /// (selected), a 400×300 view at 100%, and an empty history.
    fn editor() -> EditorState {
        let mut st = EditorState::new();
        let mut doc = Document::new(S);
        let mut bg = Layer::paint("Background", S);
        if let LayerKind::Paint(p) = &mut bg.kind {
            p.pixels = RgbaImage::filled(S, [255, 255, 255, 255]);
        }
        doc.push_layer(bg);
        doc.push_layer(Layer::paint("Paint", S));
        st.set_document(doc);
        st.set_view_size(400.0, 300.0);
        st.view_command(ViewCommand::ActualSize);
        st.mark_saved();
        st
    }

    /// Applies `f` as exactly one new step, then checks undo restores the
    /// document before it and redo the document after it, byte for byte.
    fn round_trip(st: &mut EditorState, f: impl FnOnce(&mut EditorState)) {
        let before = st.document.clone();
        let steps = st.history.undo_len();
        f(st);
        let after = st.document.clone();
        assert_ne!(before, after, "the edit changed nothing");
        assert_eq!(st.history.undo_len(), steps + 1, "one step per edit");
        assert!(st.undo());
        assert_eq!(st.document, before);
        assert!(st.redo());
        assert_eq!(st.document, after);
        assert!(st.undo());
        assert_eq!(st.document, before);
        assert!(st.redo());
    }

    fn screen(st: &EditorState, x: f32, y: f32) -> Vec2 {
        st.viewport.canvas_to_screen(Vec2::new(x, y))
    }

    fn stroke(st: &mut EditorState, from: (f32, f32), to: (f32, f32)) {
        st.select_tool(Tool::Brush);
        assert!(st.begin_stroke(Vec2::new(from.0, from.1), PaintButton::Primary));
        st.continue_stroke(Vec2::new((from.0 + to.0) / 2.0, (from.1 + to.1) / 2.0));
        st.continue_stroke(Vec2::new(to.0, to.1));
        st.end_stroke();
    }

    fn image(w: u32, h: u32) -> LoadedImage {
        let image = RgbaImage::filled(Size::new(w, h), [200, 10, 10, 255]);
        LoadedImage {
            name: "pic".into(),
            hash: rp_format::source_hash(&image),
            image,
        }
    }

    #[test]
    fn brush_stroke_is_one_patch_step() {
        let mut st = editor();
        st.brush.set_size(3);
        round_trip(&mut st, |st| stroke(st, (5.0, 5.0), (20.0, 10.0)));
        let step = st.history.history().peek_undo().unwrap();
        assert_eq!(step.kind(), EditKind::Paint);
        let (_, rect) = step.paint_target().unwrap();
        assert!(
            rect.area() < S.width as usize * S.height as usize / 4,
            "{rect:?}"
        );
        assert_eq!(st.history.undo_label(), Some(EditLabel::BrushStroke));
    }

    #[test]
    fn eraser_stroke_round_trips() {
        let mut st = editor();
        st.selected_layer = Some(st.document.layers()[0].id());
        st.select_tool(Tool::Eraser);
        round_trip(&mut st, |st| {
            st.begin_stroke(Vec2::new(10.0, 10.0), PaintButton::Primary);
            st.continue_stroke(Vec2::new(30.0, 30.0));
            st.end_stroke();
        });
        assert_eq!(st.history.undo_label(), Some(EditLabel::EraserStroke));
    }

    #[test]
    fn stroke_on_offset_layer_and_off_canvas_round_trips() {
        let mut st = editor();
        let id = st.selected_layer.unwrap();
        st.nudge_selected(-7, 5);
        st.brush.set_size(20);
        round_trip(&mut st, |st| {
            st.select_tool(Tool::Brush);
            st.begin_stroke(Vec2::new(-5.0, 2.0), PaintButton::Secondary);
            st.continue_stroke(Vec2::new(70.0, 60.0));
            st.end_stroke();
        });
        let step = st.history.history().peek_undo().unwrap();
        assert_eq!(step.paint_target().unwrap().0, id);
    }

    #[test]
    fn stroke_that_changes_nothing_records_nothing_and_stays_clean() {
        let mut st = editor();
        st.selected_layer = Some(st.document.layers()[0].id());
        st.document.fg = Rgb::new(255, 255, 255);
        st.mark_saved();
        stroke(&mut st, (5.0, 5.0), (20.0, 20.0));
        assert!(!st.history.can_undo());
        assert!(!st.is_dirty());
    }

    #[test]
    fn layer_ops_round_trip() {
        let mut st = editor();
        let bottom = st.document.layers()[0].id();
        round_trip(&mut st, |st| {
            st.add_paint_layer();
        });
        round_trip(&mut st, |st| {
            st.delete_selected_layer();
        });
        round_trip(&mut st, |st| {
            st.move_selected_layer(true);
        });
        let id = st.selected_layer.unwrap();
        round_trip(&mut st, |st| {
            st.begin_rename(id);
            st.rename_backspace();
            st.rename_type("Ink");
            st.commit_rename();
        });
        round_trip(&mut st, |st| {
            st.toggle_visibility(id);
        });
        round_trip(&mut st, |st| {
            st.set_layer_opacity(id, 40);
        });
        round_trip(&mut st, |st| {
            st.set_layer_blend(id, BlendMode::Multiply);
        });
        round_trip(&mut st, |st| {
            st.toggle_lock_pixels(id);
        });
        round_trip(&mut st, |st| {
            st.toggle_lock_position(bottom);
        });
        round_trip(&mut st, |st| {
            st.add_mask(id);
        });
        round_trip(&mut st, |st| {
            st.remove_mask(id);
        });
    }

    #[test]
    fn rename_is_one_step_on_commit_and_none_on_cancel() {
        let mut st = editor();
        let id = st.selected_layer.unwrap();
        st.begin_rename(id);
        st.rename_type("abc");
        assert!(!st.history.can_undo(), "typing is not a step");
        st.cancel_rename();
        assert!(!st.history.can_undo());
        st.begin_rename(id);
        st.rename_type("x");
        st.commit_rename();
        assert_eq!(st.history.undo_len(), 1);
        assert_eq!(st.history.undo_label(), Some(EditLabel::RenameLayer));
    }

    #[test]
    fn refused_edits_and_non_document_changes_are_not_steps() {
        let mut st = editor();
        let bottom = st.document.layers()[0].id();
        st.select_layer(bottom);
        st.select_tool(Tool::Move);
        st.view_command(ViewCommand::ZoomIn);
        st.set_layer_opacity(bottom, 100);
        st.swap_colors();
        st.reset_colors();
        st.set_brush_size(30);
        assert!(!st.history.can_undo());
        st.delete_selected_layer();
        st.delete_selected_layer();
        assert_eq!(
            st.history.undo_len(),
            1,
            "deleting the last layer is refused"
        );
    }

    #[test]
    fn opacity_steps_coalesce_per_layer() {
        let mut st = editor();
        let id = st.selected_layer.unwrap();
        let before = st.document.clone();
        for _ in 0..4 {
            st.step_layer_opacity(id, -5);
        }
        assert_eq!(st.history.undo_len(), 1);
        st.toggle_visibility(id);
        st.step_layer_opacity(id, -5);
        assert_eq!(st.history.undo_len(), 3, "another step ends the run");
        st.undo();
        st.undo();
        st.undo();
        assert_eq!(st.document, before);
    }

    #[test]
    fn filter_changes_round_trip() {
        let mut st = editor();
        round_trip(&mut st, |st| {
            st.set_dither_method(DitherMethod::Bayer4);
        });
        round_trip(&mut st, |st| {
            st.set_palette_mode(PaletteMode::FourColor);
        });
        round_trip(&mut st, |st| {
            st.set_four_color_preset(Preset::Mode5);
        });
        round_trip(&mut st, |st| {
            st.toggle_serpentine();
        });
        round_trip(&mut st, |st| {
            st.step_bayer_spread(10);
        });
        round_trip(&mut st, |st| {
            st.reset_bayer_spread();
        });
    }

    #[test]
    fn slider_steps_coalesce_until_another_control() {
        let mut st = editor();
        let before = st.document.clone();
        for _ in 0..5 {
            st.step_adjust(AdjustControl::Brightness, 5);
        }
        st.set_adjust_percent(AdjustControl::Brightness, 60);
        assert_eq!(st.history.undo_len(), 1);
        let after = st.document.clone();
        st.step_adjust(AdjustControl::Contrast, 5);
        st.step_strength(-10);
        st.step_strength(-10);
        assert_eq!(st.history.undo_len(), 3);
        st.undo();
        st.undo();
        assert_eq!(st.document, after);
        st.undo();
        assert_eq!(st.document, before);
        // After an undo the next change starts a new step.
        st.redo();
        st.step_adjust(AdjustControl::Brightness, 5);
        assert_eq!(st.history.undo_len(), 2);
        assert!(!st.history.can_redo());
    }

    #[test]
    fn insert_image_round_trips_and_redo_finds_the_source() {
        let mut st = editor();
        let img = image(10, 8);
        let hash = img.hash.clone();
        round_trip(&mut st, |st| {
            st.insert_loaded_image(img);
        });
        assert!(st.sources.contains(&hash));
        assert_eq!(st.history.undo_label(), Some(EditLabel::InsertImage));
    }

    #[test]
    fn move_drag_is_one_step() {
        let mut st = editor();
        st.select_tool(Tool::Move);
        round_trip(&mut st, |st| {
            assert!(st.begin_transform(screen(st, 10.0, 10.0)));
            for i in 1..=8 {
                let p = screen(st, 10.0 + i as f32 * 2.0, 10.0 + i as f32);
                st.pointer_moved(p);
            }
            st.end_transform();
        });
        assert_eq!(st.history.undo_label(), Some(EditLabel::Move));
        // A click without movement records nothing.
        let n = st.history.undo_len();
        st.begin_transform(screen(&st, 10.0, 10.0));
        st.end_transform();
        assert_eq!(st.history.undo_len(), n);
    }

    #[test]
    fn image_scale_and_rotate_drags_round_trip() {
        let mut st = editor();
        st.insert_loaded_image(image(20, 10));
        st.select_tool(Tool::Select);
        let h = st.transform_handles().unwrap();
        round_trip(&mut st, |st| {
            assert!(st.begin_transform(h.corners[2]));
            st.pointer_moved(Vec2::new(h.corners[2].x + 15.0, h.corners[2].y + 7.0));
            st.end_transform();
        });
        assert_eq!(st.history.undo_label(), Some(EditLabel::Scale));
        let h = st.transform_handles().unwrap();
        round_trip(&mut st, |st| {
            assert!(st.begin_transform(h.rotate));
            st.pointer_moved(Vec2::new(h.rotate.x + 30.0, h.rotate.y + 10.0));
            st.end_transform();
        });
        assert_eq!(st.history.undo_label(), Some(EditLabel::Rotate));
    }

    #[test]
    fn nudges_coalesce_per_layer() {
        let mut st = editor();
        st.select_tool(Tool::Move);
        round_trip(&mut st, |st| {
            st.nudge_selected(1, 0);
            st.nudge_selected(1, 0);
            st.nudge_selected(0, 10);
        });
        let bottom = st.document.layers()[0].id();
        st.select_layer(bottom);
        st.nudge_selected(1, 0);
        assert_eq!(st.history.undo_len(), 2, "another layer starts a new step");
    }

    #[test]
    fn project_settings_round_trip_and_refit() {
        let mut st = editor();
        let mut s = ProjectSettings::of(&st.document);
        s.canvas = Size::new(100, 30);
        s.output = Size::new(50, 15);
        round_trip(&mut st, |st| {
            st.apply_project(&s);
        });
        st.view_command(ViewCommand::ActualSize);
        st.undo();
        assert_eq!(st.document.canvas_size, S);
        assert!(st.auto_fit);
        assert_eq!(st.viewport.zoom(), st.viewport.fit_zoom());
        assert_eq!(st.history.redo_label(), Some(EditLabel::ProjectSettings));
    }

    #[test]
    fn swatch_add_and_remove_round_trip() {
        let mut st = editor();
        st.document.fg = Rgb::new(1, 2, 3);
        round_trip(&mut st, |st| {
            st.add_fg_swatch();
        });
        round_trip(&mut st, |st| {
            st.remove_fg_swatch();
        });
    }

    #[test]
    fn undo_keeps_the_current_colors() {
        let mut st = editor();
        st.document.fg = Rgb::new(10, 20, 30);
        st.document.bg = Rgb::new(40, 50, 60);
        st.add_paint_layer();
        stroke(&mut st, (5.0, 5.0), (9.0, 9.0));
        st.document.fg = Rgb::new(1, 1, 1);
        st.document.bg = Rgb::new(2, 2, 2);
        st.undo();
        st.undo();
        assert_eq!(
            (st.document.fg, st.document.bg),
            (Rgb::new(1, 1, 1), Rgb::new(2, 2, 2))
        );
        st.document.fg = Rgb::new(3, 3, 3);
        st.redo();
        st.redo();
        assert_eq!(st.document.fg, Rgb::new(3, 3, 3));
        assert_eq!(st.document.bg, Rgb::new(2, 2, 2));
    }

    #[test]
    fn selection_is_fixed_up_after_undo() {
        let mut st = editor();
        let top = st.selected_layer.unwrap();
        let added = st.add_paint_layer();
        assert_eq!(st.selected_layer, Some(added));
        st.undo();
        assert_eq!(st.selected_layer, Some(top), "gone: top layer selected");
        let bottom = st.document.layers()[0].id();
        st.select_layer(bottom);
        st.redo();
        assert_eq!(st.selected_layer, Some(bottom), "still exists: kept");
    }

    #[test]
    fn undo_finishes_a_stroke_in_progress_and_undoes_it() {
        let mut st = editor();
        let before = st.document.clone();
        st.select_tool(Tool::Brush);
        st.begin_stroke(Vec2::new(5.0, 5.0), PaintButton::Primary);
        st.continue_stroke(Vec2::new(25.0, 5.0));
        assert!(st.undo());
        assert!(st.stroke.is_none());
        assert_eq!(st.document, before);
        assert!(st.history.can_redo());
    }

    #[test]
    fn undo_finishes_a_drag_and_cancels_a_rename() {
        let mut st = editor();
        let before = st.document.clone();
        st.select_tool(Tool::Move);
        st.begin_transform(screen(&st, 5.0, 5.0));
        st.pointer_moved(screen(&st, 15.0, 9.0));
        assert!(st.transform_drag.is_some());
        let id = st.selected_layer.unwrap();
        st.renaming = Some(crate::layer_ops::RenameEdit {
            layer: id,
            text: "typed".into(),
        });
        assert!(st.undo());
        assert!(st.transform_drag.is_none() && st.renaming.is_none());
        assert_eq!(st.document, before);
    }

    #[test]
    fn new_and_open_clear_history() {
        let mut st = editor();
        st.add_paint_layer();
        st.undo();
        st.add_paint_layer();
        st.add_paint_layer();
        st.undo();
        assert!(st.history.can_undo() && st.history.can_redo());
        st.dispatch(FileCommand::New);
        st.resolve_unsaved(UnsavedChoice::DontSave);
        assert!(!st.history.can_undo() && !st.history.can_redo());
        st.add_paint_layer();
        let doc = st.document.clone();
        st.finish_open("x.rpaint".into(), Ok((doc, SourceStore::new())));
        assert!(!st.history.can_undo());
        assert!(!st.undo());
        assert_eq!(st.status, "Nothing to undo");
    }

    #[test]
    fn undo_back_to_the_saved_state_is_clean() {
        let mut st = editor();
        assert!(!st.is_dirty());
        stroke(&mut st, (5.0, 5.0), (9.0, 9.0));
        st.set_dither_method(DitherMethod::Atkinson);
        assert!(st.is_dirty());
        st.undo();
        st.undo();
        assert!(!st.is_dirty());
        st.redo();
        assert!(st.is_dirty());
        st.mark_saved();
        st.undo();
        assert!(st.is_dirty());
        st.redo();
        assert!(!st.is_dirty());
        // Undo past the save and make a new edit: dirty, and redo is gone.
        st.undo();
        st.toggle_serpentine();
        assert!(st.is_dirty());
        assert!(!st.history.can_redo());
    }

    #[test]
    fn undo_and_redo_bump_doc_version() {
        let mut st = editor();
        st.add_paint_layer();
        let v = st.doc_version;
        st.undo();
        assert_ne!(st.doc_version, v);
        let v = st.doc_version;
        st.redo();
        assert_ne!(st.doc_version, v);
    }

    #[test]
    fn modal_dialogs_block_undo() {
        let mut st = editor();
        st.add_paint_layer();
        st.open_project_settings();
        assert!(!st.undo());
        st.cancel_project_settings();
        st.open_export_dialog();
        assert!(!st.undo());
        st.cancel_export_dialog();
        assert!(st.undo());
    }

    #[test]
    fn history_stays_within_its_memory_budget() {
        let mut st = editor();
        let budget = 2_000;
        st.history = UndoHistory::with_budget(budget);
        st.brush.set_size(9);
        for i in 0..30u8 {
            st.document.fg = Rgb::new(i * 8, 0, 0);
            let y = (i % 40) as f32 + 3.0;
            stroke(&mut st, (2.0, y), (60.0, y));
            let h = st.history.history();
            assert!(
                h.memory_used() <= budget || h.undo_len() == 1,
                "{}",
                h.memory_used()
            );
        }
        let kept = st.history.undo_len();
        assert!(kept > 0 && kept < 30, "{kept}");
        for _ in 0..kept {
            assert!(st.undo());
        }
        assert!(!st.undo());
        assert_eq!(st.history.redo_len(), kept);
    }

    #[test]
    fn default_budget_is_512_mib() {
        assert_eq!(UndoHistory::default().history().budget(), 512 * 1024 * 1024);
    }

    #[test]
    fn edit_menu_labels_and_states() {
        let mut st = editor();
        let (undo, redo) = (EditCommand::Undo, EditCommand::Redo);
        assert_eq!(undo.menu_label(&st.history), "Undo");
        assert!(!undo.enabled(&st.history) && !redo.enabled(&st.history));
        stroke(&mut st, (5.0, 5.0), (9.0, 9.0));
        assert_eq!(undo.menu_label(&st.history), "Undo Brush Stroke");
        assert!(undo.enabled(&st.history));
        st.edit_menu_open = true;
        assert!(st.edit_command(undo));
        assert!(!st.edit_menu_open);
        assert_eq!(redo.menu_label(&st.history), "Redo Brush Stroke");
        assert_eq!(undo.menu_label(&st.history), "Undo");
        assert_eq!(st.status, "Undo Brush Stroke");
        assert!(st.edit_command(redo));
        assert!(!st.edit_command(redo));
        assert_eq!(EditCommand::Undo.keystrokes(), ["ctrl-z"]);
        assert_eq!(EditCommand::Redo.keystrokes(), ["ctrl-shift-z", "ctrl-y"]);
        assert_eq!(EditCommand::Paste.keystrokes(), ["ctrl-v"]);
        assert_eq!(EditCommand::Paste.menu_label(&st.history), "Paste");
        assert!(EditCommand::Paste.enabled(&st.history));
        assert!(EditCommand::Redo.separator_after());
    }

    #[test]
    fn edit_and_file_menus_are_exclusive() {
        let mut st = editor();
        st.toggle_file_menu();
        st.toggle_edit_menu();
        assert!(st.edit_menu_open && !st.file_menu_open);
        st.toggle_file_menu();
        assert!(st.file_menu_open && !st.edit_menu_open);
        st.close_menus();
        assert!(!st.menu_open());
    }
}
