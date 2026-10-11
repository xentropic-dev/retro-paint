//! Layers-panel commands on [`EditorState`]: selection, add, delete,
//! reorder, rename, visibility, opacity, blend mode, locks and masks.
//!
//! Every command that changes the document bumps
//! [`EditorState::doc_version`] so the canvas re-flattens; selection changes
//! and refused commands leave it alone.
//!
//! Decisions:
//! - New paint layers go directly above the selected layer (on top if none
//!   is selected), are transparent and canvas-sized, are named `Layer N`
//!   (one more than the highest existing `Layer N`), and become selected.
//! - The last remaining layer cannot be deleted.
//! - After a delete, the layer that was below is selected, or the new bottom
//!   layer if the deleted one was at the bottom.
//! - A new mask is all white (shows the whole layer), canvas-sized.
//! - Every command that changes the document is one undo step; an inline
//!   rename is one step when it is committed, and consecutive opacity
//!   changes of one layer merge into one step. Selection is not a step.

use rp_doc::{BlendMode, GrayImage, Layer, LayerId};

use crate::state::EditorState;
use crate::undo_ops::{Coalesce, EditLabel};

/// Opacity change per click of the panel's −/+ steppers, in percent.
pub const OPACITY_STEP: i32 = 5;

/// An in-progress inline rename in the Layers panel.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RenameEdit {
    pub layer: LayerId,
    /// The text typed so far; starts as the layer's current name.
    pub text: String,
}

/// The name for a new layer: `Layer N`, one past the highest `Layer N` in
/// `layers` (so `Layer 1` in a fresh document).
pub fn next_layer_name(layers: &[Layer]) -> String {
    let max = layers
        .iter()
        .filter_map(|l| l.name.strip_prefix("Layer ")?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("Layer {}", max + 1)
}

impl EditorState {
    /// Index of the selected layer in the stack (0 = bottom).
    pub fn selected_index(&self) -> Option<usize> {
        self.document.index_of(self.selected_layer?)
    }

    /// The selected layer, if it still exists.
    pub fn selected(&self) -> Option<&Layer> {
        self.document.layer(self.selected_layer?)
    }

    /// Selects `id` if it is in the document. Ends any rename of another layer.
    pub fn select_layer(&mut self, id: LayerId) {
        if self.document.layer(id).is_some() {
            if self.selected_layer != Some(id) {
                self.settle_float();
            }
            if self.renaming.as_ref().is_some_and(|r| r.layer != id) {
                self.commit_rename();
            }
            if self.selected_layer != Some(id) {
                self.mask_target = None;
            }
            self.selected_layer = Some(id);
        }
    }

    /// Runs `f` on layer `id` as one undo step labelled `label`; bumps the
    /// document version if it returns `true` (the layer changed).
    fn modify_layer(
        &mut self,
        id: LayerId,
        label: EditLabel,
        f: impl FnOnce(&mut Layer) -> bool,
    ) -> bool {
        self.record(label, |s| s.modify_layer_now(id, f))
    }

    fn modify_layer_now(&mut self, id: LayerId, f: impl FnOnce(&mut Layer) -> bool) -> bool {
        let changed = self.document.layer_mut(id).is_some_and(f);
        if changed {
            self.mark_document_changed();
        }
        changed
    }

    /// Adds a transparent canvas-sized paint layer above the selection and
    /// selects it.
    pub fn add_paint_layer(&mut self) -> LayerId {
        self.record(EditLabel::AddLayer, Self::add_paint_layer_now)
    }

    fn add_paint_layer_now(&mut self) -> LayerId {
        let name = next_layer_name(self.document.layers());
        let layer = Layer::paint(name.clone(), self.document.canvas_size);
        let index = self
            .selected_index()
            .map_or(self.document.layers().len(), |i| i + 1);
        let id = self.document.insert_layer(index, layer);
        self.selected_layer = Some(id);
        self.mask_target = None;
        self.mark_document_changed();
        self.status = format!("Added {name}");
        id
    }

    /// Deletes the selected layer unless it is the only one. Returns whether
    /// a layer was deleted.
    pub fn delete_selected_layer(&mut self) -> bool {
        self.record(EditLabel::DeleteLayer, Self::delete_selected_layer_now)
    }

    fn delete_selected_layer_now(&mut self) -> bool {
        let Some(index) = self.selected_index() else {
            return false;
        };
        if self.document.layers().len() <= 1 {
            self.status = "Can't delete the only layer".into();
            return false;
        }
        let id = self.document.layers()[index].id();
        let removed = self
            .document
            .remove_layer(id)
            .expect("selected layer exists");
        if self.renaming.as_ref().is_some_and(|r| r.layer == id) {
            self.renaming = None;
        }
        let next = index.saturating_sub(1);
        self.selected_layer = Some(self.document.layers()[next].id());
        self.mask_target = None;
        self.mark_document_changed();
        self.status = format!("Deleted {}", removed.name);
        true
    }

    /// Moves the selected layer one step up (toward the top of the stack) or
    /// down. Returns whether it moved.
    pub fn move_selected_layer(&mut self, up: bool) -> bool {
        self.record(EditLabel::ReorderLayer, |s| s.move_selected_layer_now(up))
    }

    fn move_selected_layer_now(&mut self, up: bool) -> bool {
        let (Some(id), Some(index)) = (self.selected_layer, self.selected_index()) else {
            return false;
        };
        let to = if up {
            if index + 1 >= self.document.layers().len() {
                return false;
            }
            index + 1
        } else {
            if index == 0 {
                return false;
            }
            index - 1
        };
        self.document.move_layer(id, to);
        self.mark_document_changed();
        true
    }

    /// Renames layer `id` to `name` (trimmed). Empty names are refused.
    pub fn rename_layer(&mut self, id: LayerId, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        self.modify_layer(id, EditLabel::RenameLayer, |l| {
            if l.name == name {
                return false;
            }
            l.name = name.to_string();
            true
        })
    }

    pub fn toggle_visibility(&mut self, id: LayerId) -> bool {
        self.modify_layer(id, EditLabel::Visibility, |l| {
            l.visible = !l.visible;
            true
        })
    }

    /// Sets opacity in percent, clamped to `0..=100`.
    pub fn set_layer_opacity(&mut self, id: LayerId, percent: i32) -> bool {
        let percent = percent.clamp(0, 100) as u8;
        let f = |l: &mut Layer| {
            if l.opacity == percent {
                return false;
            }
            l.set_opacity(percent);
            true
        };
        self.record_coalesced(EditLabel::Opacity, Coalesce::Opacity(id), |s| {
            s.modify_layer_now(id, f)
        })
    }

    /// Changes opacity by `delta` percent, clamped to `0..=100`.
    pub fn step_layer_opacity(&mut self, id: LayerId, delta: i32) -> bool {
        let Some(current) = self.document.layer(id).map(|l| l.opacity.min(100)) else {
            return false;
        };
        self.set_layer_opacity(id, current as i32 + delta)
    }

    pub fn set_layer_blend(&mut self, id: LayerId, blend: BlendMode) -> bool {
        self.modify_layer(id, EditLabel::BlendMode, |l| {
            if l.blend == blend {
                return false;
            }
            l.blend = blend;
            true
        })
    }

    pub fn toggle_lock_pixels(&mut self, id: LayerId) -> bool {
        self.modify_layer(id, EditLabel::LockPixels, |l| {
            l.lock.pixels = !l.lock.pixels;
            true
        })
    }

    pub fn toggle_lock_position(&mut self, id: LayerId) -> bool {
        self.modify_layer(id, EditLabel::LockPosition, |l| {
            l.lock.position = !l.lock.position;
            true
        })
    }

    /// Gives layer `id` an all-white canvas-sized mask if it has none.
    pub fn add_mask(&mut self, id: LayerId) -> bool {
        let size = self.document.canvas_size;
        self.modify_layer(id, EditLabel::AddMask, |l| {
            if l.mask.is_some() {
                return false;
            }
            l.mask = Some(GrayImage::filled(size, 255));
            true
        })
    }

    /// Removes layer `id`'s mask; if it was the edit target, strokes go
    /// back to the layer's pixels.
    pub fn remove_mask(&mut self, id: LayerId) -> bool {
        let removed = self.modify_layer(id, EditLabel::RemoveMask, |l| l.mask.take().is_some());
        if removed && self.mask_target == Some(id) {
            self.mask_target = None;
        }
        removed
    }

    /// Starts an inline rename of layer `id` and selects it.
    pub fn begin_rename(&mut self, id: LayerId) {
        let Some(name) = self.document.layer(id).map(|l| l.name.clone()) else {
            return;
        };
        self.select_layer(id);
        self.renaming = Some(RenameEdit {
            layer: id,
            text: name,
        });
    }

    /// Appends typed text to the rename buffer. Control characters are
    /// dropped.
    pub fn rename_type(&mut self, text: &str) {
        if let Some(r) = &mut self.renaming {
            r.text.extend(text.chars().filter(|c| !c.is_control()));
        }
    }

    pub fn rename_backspace(&mut self) {
        if let Some(r) = &mut self.renaming {
            r.text.pop();
        }
    }

    /// Ends the rename, applying it. An empty name keeps the old one.
    pub fn commit_rename(&mut self) {
        if let Some(r) = self.renaming.take() {
            self.rename_layer(r.layer, &r.text);
        }
    }

    /// Ends the rename without changing the layer.
    pub fn cancel_rename(&mut self) {
        self.renaming = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{BACKGROUND_LAYER, NEW_CANVAS_SIZE};

    fn names(st: &EditorState) -> Vec<&str> {
        st.document
            .layers()
            .iter()
            .map(|l| l.name.as_str())
            .collect()
    }

    fn bg(st: &EditorState) -> LayerId {
        st.document.layers()[0].id()
    }

    #[test]
    fn background_is_selected_at_start_and_after_new() {
        let mut st = EditorState::new();
        assert_eq!(st.selected_layer, Some(bg(&st)));
        st.add_paint_layer();
        st.new_document_now();
        assert_eq!(st.selected_layer, Some(bg(&st)));
        assert_eq!(st.selected().unwrap().name, BACKGROUND_LAYER);
    }

    #[test]
    fn add_inserts_above_selection_and_selects() {
        let mut st = EditorState::new();
        let v = st.doc_version;
        let l1 = st.add_paint_layer();
        assert_ne!(st.doc_version, v);
        assert_eq!(st.selected_layer, Some(l1));
        let l2 = st.add_paint_layer();
        assert_eq!(names(&st), ["Background", "Layer 1", "Layer 2"]);
        st.select_layer(l1);
        let l3 = st.add_paint_layer();
        assert_eq!(names(&st), ["Background", "Layer 1", "Layer 3", "Layer 2"]);
        assert_eq!(st.selected_layer, Some(l3));
        let layer = st.document.layer(l2).unwrap();
        match &layer.kind {
            rp_doc::LayerKind::Paint(p) => {
                assert_eq!(p.pixels.size(), NEW_CANVAS_SIZE);
                assert!(p.pixels.as_raw().iter().all(|&b| b == 0));
            }
            _ => panic!("not a paint layer"),
        }
    }

    #[test]
    fn layer_names_continue_from_the_highest() {
        let mut st = EditorState::new();
        let id = st.add_paint_layer();
        st.rename_layer(id, "Layer 7");
        st.add_paint_layer();
        assert_eq!(names(&st)[2], "Layer 8");
    }

    #[test]
    fn delete_selects_the_layer_below() {
        let mut st = EditorState::new();
        let l1 = st.add_paint_layer();
        let l2 = st.add_paint_layer();
        let v = st.doc_version;
        assert!(st.delete_selected_layer());
        assert_ne!(st.doc_version, v);
        assert_eq!(st.selected_layer, Some(l1));
        assert!(st.document.layer(l2).is_none());
        // Deleting the bottom layer selects the new bottom.
        st.select_layer(bg(&st));
        assert!(st.delete_selected_layer());
        assert_eq!(st.selected_layer, Some(l1));
        assert_eq!(names(&st), ["Layer 1"]);
    }

    #[test]
    fn the_last_layer_cannot_be_deleted() {
        let mut st = EditorState::new();
        let v = st.doc_version;
        assert!(!st.delete_selected_layer());
        assert_eq!(st.document.layers().len(), 1);
        assert_eq!(st.doc_version, v);
        assert!(st.status.contains("only layer"), "{}", st.status);
    }

    #[test]
    fn move_up_and_down_stop_at_the_ends() {
        let mut st = EditorState::new();
        let l1 = st.add_paint_layer();
        st.add_paint_layer();
        st.select_layer(l1);
        let v = st.doc_version;
        assert!(st.move_selected_layer(true));
        assert_ne!(st.doc_version, v);
        assert_eq!(names(&st), ["Background", "Layer 2", "Layer 1"]);
        assert_eq!(st.selected_layer, Some(l1));
        let v = st.doc_version;
        assert!(!st.move_selected_layer(true));
        assert_eq!(st.doc_version, v);
        assert!(st.move_selected_layer(false));
        assert!(st.move_selected_layer(false));
        assert_eq!(names(&st), ["Layer 1", "Background", "Layer 2"]);
        assert!(!st.move_selected_layer(false));
    }

    #[test]
    fn rename_trims_and_refuses_empty() {
        let mut st = EditorState::new();
        let id = bg(&st);
        let v = st.doc_version;
        assert!(st.rename_layer(id, "  Sky "));
        assert_eq!(st.document.layer(id).unwrap().name, "Sky");
        assert_ne!(st.doc_version, v);
        let v = st.doc_version;
        assert!(!st.rename_layer(id, "   "));
        assert!(!st.rename_layer(id, "Sky"));
        assert_eq!(st.doc_version, v);
    }

    #[test]
    fn inline_rename_edits_a_buffer_then_commits_or_cancels() {
        let mut st = EditorState::new();
        let id = bg(&st);
        st.begin_rename(id);
        assert_eq!(st.renaming.as_ref().unwrap().text, "Background");
        for _ in 0.."ground".len() {
            st.rename_backspace();
        }
        st.rename_type("drop\u{8}!");
        assert_eq!(st.renaming.as_ref().unwrap().text, "Backdrop!");
        assert_eq!(st.document.layer(id).unwrap().name, "Background");
        st.commit_rename();
        assert_eq!(st.renaming, None);
        assert_eq!(st.document.layer(id).unwrap().name, "Backdrop!");

        st.begin_rename(id);
        st.rename_type("xyz");
        st.cancel_rename();
        assert_eq!(st.document.layer(id).unwrap().name, "Backdrop!");

        // Selecting another layer commits; deleting the layer drops the edit.
        let l1 = st.add_paint_layer();
        st.begin_rename(id);
        st.rename_type("2");
        st.select_layer(l1);
        assert_eq!(st.renaming, None);
        assert_eq!(st.document.layer(id).unwrap().name, "Backdrop!2");
        st.begin_rename(l1);
        st.delete_selected_layer();
        assert_eq!(st.renaming, None);
    }

    #[test]
    fn visibility_toggles_and_bumps() {
        let mut st = EditorState::new();
        let id = bg(&st);
        let v = st.doc_version;
        assert!(st.toggle_visibility(id));
        assert!(!st.document.layer(id).unwrap().visible);
        assert_ne!(st.doc_version, v);
        // Hidden white background flattens to transparent.
        assert!(
            st.document
                .flatten(&st.sources)
                .as_raw()
                .iter()
                .all(|&b| b == 0)
        );
        st.toggle_visibility(id);
        assert!(st.document.layer(id).unwrap().visible);
    }

    #[test]
    fn opacity_clamps_and_steps() {
        let mut st = EditorState::new();
        let id = bg(&st);
        let v = st.doc_version;
        assert!(st.step_layer_opacity(id, -OPACITY_STEP));
        assert_eq!(st.document.layer(id).unwrap().opacity, 95);
        assert_ne!(st.doc_version, v);
        assert!(st.set_layer_opacity(id, -20));
        assert_eq!(st.document.layer(id).unwrap().opacity, 0);
        let v = st.doc_version;
        assert!(!st.step_layer_opacity(id, -OPACITY_STEP));
        assert_eq!(st.doc_version, v);
        assert!(st.set_layer_opacity(id, 250));
        assert_eq!(st.document.layer(id).unwrap().opacity, 100);
        let gone = st.add_paint_layer();
        st.delete_selected_layer();
        assert!(!st.set_layer_opacity(gone, 50));
    }

    #[test]
    fn blend_mode_changes_only_when_different() {
        let mut st = EditorState::new();
        let id = bg(&st);
        let v = st.doc_version;
        assert!(!st.set_layer_blend(id, BlendMode::Normal));
        assert_eq!(st.doc_version, v);
        for mode in BlendMode::ALL.into_iter().skip(1) {
            assert!(st.set_layer_blend(id, mode));
            assert_eq!(st.document.layer(id).unwrap().blend, mode);
        }
        assert_ne!(st.doc_version, v);
    }

    #[test]
    fn locks_toggle_independently() {
        let mut st = EditorState::new();
        let id = bg(&st);
        let v = st.doc_version;
        assert!(st.toggle_lock_pixels(id));
        let lock = st.document.layer(id).unwrap().lock;
        assert!(lock.pixels && !lock.position);
        assert!(st.toggle_lock_position(id));
        let lock = st.document.layer(id).unwrap().lock;
        assert!(lock.pixels && lock.position);
        assert!(st.document.layer_mut(id).unwrap().pixels_mut().is_err());
        st.toggle_lock_pixels(id);
        assert!(!st.document.layer(id).unwrap().lock.pixels);
        assert_ne!(st.doc_version, v);
    }

    #[test]
    fn masks_are_white_canvas_sized_and_removable() {
        let mut st = EditorState::new();
        let id = bg(&st);
        let v = st.doc_version;
        assert!(st.add_mask(id));
        assert_ne!(st.doc_version, v);
        let mask = st.document.layer(id).unwrap().mask.as_ref().unwrap();
        assert_eq!(mask.size(), NEW_CANVAS_SIZE);
        assert!(mask.as_raw().iter().all(|&b| b == 255));
        // A white mask shows the whole layer.
        assert!(
            st.document
                .flatten(&st.sources)
                .as_raw()
                .iter()
                .all(|&b| b == 255)
        );
        assert!(!st.add_mask(id));
        let v = st.doc_version;
        assert!(st.remove_mask(id));
        assert_ne!(st.doc_version, v);
        assert!(st.document.layer(id).unwrap().mask.is_none());
        assert!(!st.remove_mask(id));
    }

    #[test]
    fn selection_changes_leave_doc_version_alone() {
        let mut st = EditorState::new();
        let gone = st.add_paint_layer();
        st.delete_selected_layer();
        let l1 = st.add_paint_layer();
        let v = st.doc_version;
        st.select_layer(bg(&st));
        st.select_layer(l1);
        st.select_layer(gone);
        assert_eq!(st.selected_layer, Some(l1));
        assert_eq!(st.doc_version, v);
    }
}
