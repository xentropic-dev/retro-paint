//! The File modals: the unsaved-changes prompt (Save / Don't Save /
//! Cancel), the Export PNG dialog (scale 1×–8×, full-color checkbox) and
//! the message box for open and save failures. Thin: each click or key
//! calls one [`EditorState`] method from [`crate::file_ops`].
//!
//! Keys: Enter is the default button (Save, Export…, OK), Escape cancels.
//! In the prompt `D` is Don't Save. In the Export dialog 1–8 set the
//! scale, ←/→ or −/+ step it, and `F` toggles the full-color image.

use gpui::{
    Context, Div, Entity, FocusHandle, KeyDownEvent, SharedString, Window, div, prelude::*, px, rgb,
};

use crate::file_flow::handle_outcome;
use crate::file_ops::{ExportDraft, UnsavedChoice, display_name};
use crate::project_dialog::chip;
use crate::state::{EditorState, Outcome};
use crate::view::{BG_MENU, BORDER, TEXT, TEXT_DIM, panel_header};

const DIALOG_W: f32 = 420.;

pub(crate) struct FileDialog {
    state: Entity<EditorState>,
    focus: FocusHandle,
    editor_focus: FocusHandle,
}

impl FileDialog {
    pub(crate) fn new(
        state: Entity<EditorState>,
        editor_focus: FocusHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            focus: cx.focus_handle(),
            editor_focus,
        }
    }

    pub(crate) fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// Runs `f` on the state, then carries out its outcome. Gives focus
    /// back to the editor once no file modal is left.
    fn act(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut EditorState) -> Outcome,
    ) {
        let out = self.state.update(cx, |s, cx| {
            cx.notify();
            f(s)
        });
        if !self.state.read(cx).file_modal_open() {
            window.focus(&self.editor_focus, cx);
        }
        handle_outcome(&self.state, out, cx);
    }

    fn unsaved(&self, choice: UnsavedChoice, window: &mut Window, cx: &mut Context<Self>) {
        self.act(window, cx, |s| s.resolve_unsaved(choice));
    }

    fn export(&self, window: &mut Window, cx: &mut Context<Self>, confirm: bool) {
        self.act(window, cx, |s| {
            if confirm {
                s.confirm_export_dialog()
            } else {
                s.cancel_export_dialog();
                Outcome::Done
            }
        });
    }

    fn draft(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut ExportDraft)) {
        self.state.update(cx, |s, cx| {
            s.edit_export_draft(f);
            cx.notify();
        });
    }

    fn dismiss(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.act(window, cx, |s| {
            s.dismiss_alert();
            Outcome::Done
        });
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let m = &k.modifiers;
        if m.control || m.alt || m.platform {
            return;
        }
        let key = k.key.as_str();
        let s = self.state.read(cx);
        if s.alert.is_some() {
            if matches!(key, "enter" | "escape" | "space") {
                self.dismiss(window, cx);
            }
        } else if s.unsaved_prompt.is_some() {
            match key {
                "enter" => self.unsaved(UnsavedChoice::Save, window, cx),
                "d" => self.unsaved(UnsavedChoice::DontSave, window, cx),
                "escape" => self.unsaved(UnsavedChoice::Cancel, window, cx),
                _ => {}
            }
        } else if s.export_dialog.is_some() {
            match key {
                "enter" => self.export(window, cx, true),
                "escape" => self.export(window, cx, false),
                "left" | "-" => self.draft(cx, |d| d.step_scale(-1)),
                "right" | "+" | "=" => self.draft(cx, |d| d.step_scale(1)),
                "f" | "space" => self.draft(cx, |d| d.include_full_color ^= true),
                _ => {
                    if let Ok(n) = key.parse::<i64>() {
                        self.draft(cx, |d| d.set_scale(n));
                    }
                }
            }
        }
        cx.stop_propagation();
    }

    fn frame(&self, title: &'static str, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        div()
            .id("file-dialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .w(px(DIALOG_W))
            .flex()
            .flex_col()
            .bg(rgb(BG_MENU))
            .border_1()
            .border_color(rgb(BORDER))
            .rounded_md()
            .shadow_lg()
            .text_xs()
            .text_color(rgb(TEXT))
            .occlude()
            .child(panel_header(title))
    }

    fn body() -> Div {
        div().p_3().flex().flex_col().gap_3()
    }

    fn footer() -> Div {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(div().flex_1())
    }
}

/// A dialog button; `default` marks the one Enter presses.
fn button(id: &'static str, label: &'static str, default: bool) -> gpui::Stateful<Div> {
    chip(id.into(), label.into(), default)
}

impl Render for FileDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = self.state.read(cx);
        let (alert, prompt, export) = (s.alert.clone(), s.unsaved_prompt, s.export_dialog);
        let name = s.file_name();
        let doc = &s.document;
        let (output, canvas) = (doc.output_size, doc.canvas_size);
        let full_name = display_name(Some(&rp_format::full_color_path(std::path::Path::new(
            &crate::file_ops::default_export_name(s.current_path.as_deref()),
        ))));

        if let Some(a) = alert {
            return self
                .frame(a.title, cx)
                .child(
                    Self::body().child(div().child(a.message)).child(
                        Self::footer().child(
                            button("alert-ok", "OK", true)
                                .on_click(cx.listener(|this, _, w, cx| this.dismiss(w, cx))),
                        ),
                    ),
                )
                .into_any_element();
        }

        if let Some(action) = prompt {
            return self
                .frame("Unsaved changes", cx)
                .child(
                    Self::body()
                        .child(
                            div()
                                .child(format!("Save changes to {name} before {}?", action.verb())),
                        )
                        .child(
                            div()
                                .text_color(rgb(TEXT_DIM))
                                .child("Your changes are lost if you don't save them."),
                        )
                        .child(
                            Self::footer()
                                .child(button("unsaved-dont", "Don't Save", false).on_click(
                                    cx.listener(|this, _, w, cx| {
                                        this.unsaved(UnsavedChoice::DontSave, w, cx)
                                    }),
                                ))
                                .child(button("unsaved-cancel", "Cancel", false).on_click(
                                    cx.listener(|this, _, w, cx| {
                                        this.unsaved(UnsavedChoice::Cancel, w, cx)
                                    }),
                                ))
                                .child(button("unsaved-save", "Save", true).on_click(cx.listener(
                                    |this, _, w, cx| this.unsaved(UnsavedChoice::Save, w, cx),
                                ))),
                        ),
                )
                .into_any_element();
        }

        let Some(draft) = export else {
            return div().into_any_element();
        };
        let mut scales = div().flex().flex_row().gap_1();
        for n in 1..=rp_format::MAX_EXPORT_SCALE {
            scales = scales.child(
                chip(format!("scale-{n}"), format!("{n}×"), draft.scale == n)
                    .w(px(32.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.draft(cx, |d| d.set_scale(n as i64));
                    })),
            );
        }
        let px_size = draft.pixel_size(output);
        let check = if draft.include_full_color {
            "☑"
        } else {
            "☐"
        };
        self.frame("Export PNG", cx)
            .child(
                Self::body()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_color(rgb(TEXT_DIM)).child("Scale"))
                            .child(scales)
                            .child(div().child(SharedString::from(format!(
                                "Dithered CGA PNG: {}×{} px (output {}×{} × {})",
                                px_size.width,
                                px_size.height,
                                output.width,
                                output.height,
                                draft.scale
                            )))),
                    )
                    .child(
                        div()
                            .id("export-full")
                            .flex()
                            .flex_row()
                            .gap_2()
                            .items_center()
                            .cursor_pointer()
                            .child(div().text_sm().child(check))
                            .child(format!(
                                "Also export full-color image ({full_name}, {}×{} px)",
                                canvas.width, canvas.height
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.draft(cx, |d| d.include_full_color ^= true);
                            })),
                    )
                    .child(
                        Self::footer()
                            .child(
                                button("export-cancel", "Cancel", false).on_click(
                                    cx.listener(|this, _, w, cx| this.export(w, cx, false)),
                                ),
                            )
                            .child(
                                button("export-go", "Export…", true).on_click(
                                    cx.listener(|this, _, w, cx| this.export(w, cx, true)),
                                ),
                            ),
                    ),
            )
            .into_any_element()
    }
}
