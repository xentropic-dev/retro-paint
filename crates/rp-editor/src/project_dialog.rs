//! The Project Settings dialog (File > Project Settings…, Ctrl+Shift+P): a
//! modal over the editor that edits a [`ProjectDraft`] and applies it in one
//! document edit. Thin: every click calls one draft or state method from
//! [`crate::project_ops`].
//!
//! Size fields take typed digits: click a number, type, then Enter or Tab
//! (or click elsewhere) to set it, Escape to undo the typing. Steppers and
//! presets do the rest. Enter with no field being typed applies; Escape
//! cancels the dialog.

use gpui::{
    Context, Div, Entity, FocusHandle, KeyDownEvent, MouseButton, SharedString, Stateful, Window,
    div, prelude::*, px, rgb,
};
use rp_doc::Layout;

use crate::project_ops::{
    Anchor, CANVAS_PRESETS, Field, OUTPUT_DIVISORS, ProjectDraft, divided, layout_label,
};
use crate::state::EditorState;
use crate::view::{ACCENT, BG_HOVER, BG_MENU, BORDER, TEXT, TEXT_DIM, edit_state, panel_header};

const DIALOG_W: f32 = 460.;

pub(crate) struct ProjectDialog {
    state: Entity<EditorState>,
    /// Holds keyboard focus while the dialog is open.
    focus: FocusHandle,
    /// The editor's focus, restored when the dialog closes.
    editor_focus: FocusHandle,
}

impl ProjectDialog {
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

    /// Takes keyboard focus (call when the dialog opens).
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
    }

    fn draft(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut ProjectDraft)) {
        edit_state(&self.state, cx, |s| s.edit_project_draft(f));
    }

    fn close(&self, window: &mut Window, cx: &mut Context<Self>, apply: bool) {
        edit_state(&self.state, cx, |s| {
            if apply {
                // The before-state is for undo-redo (not built yet).
                let _ = s.apply_project_settings();
            } else {
                s.cancel_project_settings();
            }
        });
        window.focus(&self.editor_focus, cx);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let m = &k.modifiers;
        if m.control || m.alt || m.platform {
            return;
        }
        let editing = self
            .state
            .read(cx)
            .project_dialog
            .as_ref()
            .is_some_and(|d| d.editing.is_some());
        match k.key.as_str() {
            "enter" if editing => self.draft(cx, |d| d.commit_edit()),
            "enter" => self.close(window, cx, true),
            "escape" if editing => self.draft(cx, |d| d.cancel_edit()),
            "escape" => self.close(window, cx, false),
            "tab" => self.draft(cx, |d| d.commit_edit()),
            "backspace" => self.draft(cx, |d| d.backspace()),
            _ => {
                if let Some(text) = k.key_char.clone() {
                    self.draft(cx, |d| d.type_text(&text));
                }
            }
        }
        // Keep editor shortcuts (brush size, color keys) out of the dialog.
        cx.stop_propagation();
    }

    fn size_row(&self, draft: &ProjectDraft, field: Field, cx: &mut Context<Self>) -> Div {
        let label = match field {
            Field::CanvasW | Field::OutputW => "W",
            Field::CanvasH | Field::OutputH => "H",
        };
        let (small, big) = field.steps();
        let editing = draft.editing.as_ref().filter(|e| e.field == field);
        let value = match editing {
            Some(e) => format!("{}▏", e.text),
            None => draft.value(field).to_string(),
        };
        let stepper = |delta: i64| {
            let sign = if delta < 0 { "−" } else { "+" };
            chip(
                format!("step-{field:?}-{delta}"),
                format!("{sign}{}", delta.abs()),
                false,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.draft(cx, |d| {
                    d.commit_edit();
                    d.step(field, delta);
                });
            }))
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(div().w(px(14.)).text_color(rgb(TEXT_DIM)).child(label))
            .child(stepper(-big))
            .child(stepper(-small))
            .child(
                div()
                    .id(SharedString::from(format!("field-{field:?}")))
                    .w(px(64.))
                    .px_1()
                    .flex()
                    .justify_end()
                    .rounded_sm()
                    .border_1()
                    .cursor_text()
                    .bg(rgb(0x1a1a1a))
                    .border_color(rgb(if editing.is_some() { ACCENT } else { 0x4a4a4a }))
                    .child(value)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.draft(cx, |d| d.begin_edit(field));
                        this.focus(window, cx);
                    })),
            )
            .child(stepper(small))
            .child(stepper(big))
    }

    fn layout_section(&self, draft: &ProjectDraft, cx: &mut Context<Self>) -> Div {
        let mut row = div().flex().flex_row().gap_1();
        for layout in [Layout::Mobile, Layout::Desktop] {
            let s = layout.dos_screen();
            row = row.child(
                chip(
                    format!("layout-{layout:?}"),
                    format!("{} · DOS {}×{}", layout_label(layout), s.width, s.height),
                    draft.settings.layout == layout,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.draft(cx, |d| d.set_layout(layout));
                })),
            );
        }
        section("Layout").child(row)
    }

    fn canvas_section(&self, draft: &ProjectDraft, cx: &mut Context<Self>) -> Div {
        let mut presets = div().flex().flex_row().flex_wrap().gap_1();
        for p in CANVAS_PRESETS {
            presets = presets.child(
                chip(
                    format!("canvas-{}x{}", p.width, p.height),
                    format!("{}×{}", p.width, p.height),
                    draft.settings.canvas == p,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.draft(cx, |d| {
                        d.cancel_edit();
                        d.set_canvas(p);
                    });
                })),
            );
        }
        let mut grid = div().flex().flex_col().gap(px(2.));
        for row in Anchor::ALL.chunks(3) {
            let mut r = div().flex().flex_row().gap(px(2.));
            for &a in row {
                r = r.child(
                    chip(
                        format!("anchor-{a:?}"),
                        a.glyph().to_string(),
                        draft.settings.anchor == a,
                    )
                    .w(px(22.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.draft(cx, |d| d.set_anchor(a));
                    })),
                );
            }
            grid = grid.child(r);
        }
        section("Canvas")
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_4()
                    .items_start()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(self.size_row(draft, Field::CanvasW, cx))
                            .child(self.size_row(draft, Field::CanvasH, cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .items_center()
                            .child(div().text_color(rgb(TEXT_DIM)).child("Anchor"))
                            .child(grid),
                    ),
            )
            .child(presets)
    }

    fn output_section(&self, draft: &ProjectDraft, cx: &mut Context<Self>) -> Div {
        let mut presets = div().flex().flex_row().flex_wrap().gap_1();
        for n in OUTPUT_DIVISORS {
            let s = divided(draft.settings.canvas, n);
            presets = presets.child(
                chip(
                    format!("div-{n}"),
                    format!("÷{n} {}×{}", s.width, s.height),
                    draft.link == Some(n),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.draft(cx, |d| {
                        d.cancel_edit();
                        d.set_divisor(n);
                    });
                })),
            );
        }
        let dos = draft.settings.layout.dos_screen();
        presets = presets.child(
            chip(
                "dos-native".into(),
                format!("DOS {}×{}", dos.width, dos.height),
                draft.link.is_none() && draft.is_dos_native(),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.draft(cx, |d| {
                    d.cancel_edit();
                    d.set_dos_native();
                });
            })),
        );
        let link = match draft.link {
            Some(n) => format!("Linked: output stays canvas ÷ {n} when the canvas changes"),
            None => "Custom: output keeps its size when the canvas changes".into(),
        };
        section("Output (dither, DOS canvas, PNG export)")
            .child(self.size_row(draft, Field::OutputW, cx))
            .child(self.size_row(draft, Field::OutputH, cx))
            .child(presets)
            .child(div().text_color(rgb(TEXT_DIM)).child(link))
    }
}

impl Render for ProjectDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(draft) = self.state.read(cx).project_dialog.clone() else {
            return div().into_any_element();
        };
        let doc = &self.state.read(cx).document;
        let resizing = draft.settings.canvas != doc.canvas_size;
        let changed = draft.settings.differs_from(doc);
        let note = if resizing {
            "Layers are cropped or padded around the anchor; pixels are never resampled."
        } else {
            ""
        };
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(div().flex_1().text_color(rgb(TEXT_DIM)).child(note))
            .child(
                chip("project-cancel".into(), "Cancel".into(), false)
                    .on_click(cx.listener(|this, _, window, cx| this.close(window, cx, false))),
            )
            .child(
                chip("project-apply".into(), "Apply".into(), changed)
                    .on_click(cx.listener(|this, _, window, cx| this.close(window, cx, true))),
            );
        div()
            .id("project-dialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            // Clicks on empty dialog space finish any typing.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.draft(cx, |d| d.commit_edit())),
            )
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
            .child(panel_header("Project Settings"))
            .child(
                div()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(self.layout_section(&draft, cx))
                    .child(self.canvas_section(&draft, cx))
                    .child(self.output_section(&draft, cx))
                    .child(footer),
            )
            .into_any_element()
    }
}

fn section(title: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_color(rgb(TEXT_DIM)).child(title))
}

/// A small button with a runtime label; `on` highlights it.
fn chip(id: String, label: String, on: bool) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .px_2()
        .py(px(2.))
        .flex()
        .justify_center()
        .rounded_sm()
        .border_1()
        .cursor_pointer()
        .when(on, |d| {
            d.bg(rgb(ACCENT))
                .border_color(rgb(0x5a96e8))
                .text_color(rgb(0xffffff))
        })
        .when(!on, |d| {
            d.bg(rgb(0x383838))
                .border_color(rgb(0x4a4a4a))
                .hover(|s| s.bg(rgb(BG_HOVER)))
        })
        .child(label)
}
