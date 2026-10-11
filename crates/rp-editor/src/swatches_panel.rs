//! The Swatches panel: the document's saved full-color swatches. Thin: each
//! click calls one method from [`crate::swatch_ops`].
//!
//! Left-click a swatch sets the foreground, right-click the background,
//! middle-click removes it. "+ Add fg" saves the foreground; "− Remove fg"
//! deletes the swatch equal to it.

use gpui::{Context, Entity, MouseButton, SharedString, Window, div, prelude::*, px, rgb};

use crate::color_picker::ColorPickerDialog;
use crate::color_picker_ops::PickerTarget;
use crate::layers_panel::button;
use crate::state::EditorState;
use crate::swatch_ops::MAX_SWATCHES;
use crate::view::{TEXT, TEXT_DIM, edit_state, rgba};

const CELL: f32 = 18.;

pub(crate) struct SwatchesPanel {
    state: Entity<EditorState>,
    /// "+ New color…" opens this and gives it the keyboard.
    picker: Entity<ColorPickerDialog>,
}

impl SwatchesPanel {
    pub(crate) fn new(
        state: Entity<EditorState>,
        picker: Entity<ColorPickerDialog>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state, picker }
    }

    fn edit<R>(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut EditorState) -> R) -> R {
        edit_state(&self.state, cx, f)
    }
}

impl Render for SwatchesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (swatches, fg) = {
            let d = &self.state.read(cx).document;
            (d.swatches.clone(), d.fg)
        };
        let n = swatches.len();
        let controls = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .pt_1()
            .text_xs()
            .child(
                button("swatch-add", "+ Add fg").on_click(cx.listener(|this, _, _, cx| {
                    this.edit(cx, |s| s.add_fg_swatch());
                })),
            )
            .child(button("swatch-remove", "− Remove fg").on_click(cx.listener(
                |this, _, _, cx| {
                    this.edit(cx, |s| s.remove_fg_swatch());
                },
            )))
            .child(button("swatch-new", "+ New color…").on_click(cx.listener(
                |this, _, window, cx| {
                    this.edit(cx, |s| s.open_color_picker(PickerTarget::NewSwatch));
                    if this.state.read(cx).color_picker.is_some() {
                        this.picker.update(cx, |d, cx| d.focus(window, cx));
                    }
                },
            )))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .justify_end()
                    .text_color(rgb(TEXT_DIM))
                    .child(format!("{n}/{MAX_SWATCHES}")),
            );

        let mut grid = div()
            .p_2()
            .flex()
            .flex_row()
            .flex_wrap()
            .content_start()
            .gap(px(3.));
        if swatches.is_empty() {
            grid = grid.child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT_DIM))
                    .child("No swatches. + saves the foreground."),
            );
        }
        for (i, c) in swatches.into_iter().enumerate() {
            grid = grid.child(
                div()
                    .id(SharedString::from(format!("swatch-{i}")))
                    .w(px(CELL))
                    .h(px(CELL))
                    .bg(rgba(c))
                    .border_1()
                    .when(c == fg, |d| d.border_color(rgb(0xffffff)))
                    .when(c != fg, |d| d.border_color(rgb(0x4a4a4a)))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |s| s.pick_swatch(i, false));
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |s| s.pick_swatch(i, true));
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |s| s.remove_swatch_at(i));
                        }),
                    ),
            );
        }

        div()
            .w_full()
            .flex()
            .flex_col()
            .text_color(rgb(TEXT))
            .child(controls)
            .child(grid)
            .child(
                div()
                    .px_2()
                    .pb_1()
                    .text_size(px(10.))
                    .text_color(rgb(TEXT_DIM))
                    .child("Click: fg · Right: bg · Middle: remove · X swap · D reset"),
            )
    }
}
