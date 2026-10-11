//! The Layers panel: a separate GPUI view over the shared [`EditorState`].
//!
//! GIMP/Photoshop-like: controls for the selected layer (blend mode,
//! opacity, locks, mask) above a list with the top of the stack at the top,
//! and two rows of buttons below it: New / Image / Delete / Up / Down (Image
//! is Insert Image…) and Duplicate / Flip H / Flip V. Click a row to select it,
//! double-click its name to rename it inline (Enter applies, Escape cancels,
//! clicking elsewhere applies). Thin: every command is an
//! [`EditorState`] method in [`crate::layer_ops`].

use gpui::{
    ClickEvent, Context, Div, Entity, FocusHandle, KeyDownEvent, SharedString, Stateful, Window,
    div, prelude::*, px, rgb,
};
use rp_doc::{BlendMode, Layer, LayerId, LayerKind};

use crate::flip_ops::FlipAxis;
use crate::insert_flow::prompt_insert_image;
use crate::layer_ops::OPACITY_STEP;
use crate::state::EditorState;
use crate::view::{ACCENT, BG_HOVER, BG_PANEL_HEADER, BORDER, TEXT, TEXT_DIM, edit_state};

/// Opacity bar segments; each sets a multiple of 100 / this.
const OPACITY_SEGMENTS: i32 = 20;
const ROW_H: f32 = 26.;
/// The layer list shows at least this many rows and scrolls past this many;
/// the panel column around it scrolls as a whole.
const LIST_MIN_ROWS: f32 = 3.;
const LIST_MAX_ROWS: f32 = 8.;

/// What a list row shows; copied out so rendering never clones pixels.
struct RowInfo {
    id: LayerId,
    name: String,
    visible: bool,
    image: bool,
    locked: bool,
    masked: bool,
    /// Rename buffer, while this row is being renamed.
    rename: Option<String>,
}

impl RowInfo {
    fn new(layer: &Layer, state: &EditorState) -> Self {
        let id = layer.id();
        Self {
            id,
            name: layer.name.clone(),
            visible: layer.visible,
            image: matches!(layer.kind, LayerKind::Image(_)),
            locked: layer.lock.pixels || layer.lock.position,
            masked: layer.mask.is_some(),
            rename: state
                .renaming
                .as_ref()
                .filter(|r| r.layer == id)
                .map(|r| r.text.clone()),
        }
    }
}

pub(crate) struct LayersPanel {
    state: Entity<EditorState>,
    /// Holds keyboard focus during an inline rename.
    focus: FocusHandle,
}

/// Display name of a blend mode.
pub(crate) const fn blend_label(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "Normal",
        BlendMode::Multiply => "Multiply",
        BlendMode::Screen => "Screen",
        BlendMode::Overlay => "Overlay",
    }
}

impl LayersPanel {
    pub(crate) fn new(
        state: Entity<EditorState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let focus = cx.focus_handle();
        cx.on_blur(&focus, window, |this, _, cx| {
            this.edit(cx, |s| s.commit_rename());
        })
        .detach();
        Self { state, focus }
    }

    fn edit<R>(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut EditorState) -> R) -> R {
        edit_state(&self.state, cx, f)
    }

    /// Runs `f` on the selected layer's id, if any.
    fn edit_selected(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut EditorState, LayerId) -> bool,
    ) {
        self.edit(cx, |s| {
            if let Some(id) = s.selected_layer {
                f(s, id);
            }
        });
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).renaming.is_none() {
            return;
        }
        let k = &ev.keystroke;
        let m = &k.modifiers;
        if m.control || m.alt || m.platform {
            return;
        }
        match k.key.as_str() {
            "enter" => self.edit(cx, |s| s.commit_rename()),
            "escape" => self.edit(cx, |s| s.cancel_rename()),
            "backspace" => self.edit(cx, |s| s.rename_backspace()),
            _ => match &k.key_char {
                Some(text) => {
                    let text = text.clone();
                    self.edit(cx, |s| s.rename_type(&text));
                }
                None => return,
            },
        }
        cx.stop_propagation();
    }

    fn row(&self, layer: RowInfo, selected: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = layer.id;

        let eye = div()
            .id(SharedString::from(format!("layer-eye-{id:?}")))
            .w(px(18.))
            .h(px(18.))
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(rgb(0x4a4a4a))
            .rounded_sm()
            .cursor_pointer()
            .text_xs()
            .when(layer.visible, |d| d.child("●"))
            .hover(|s| s.bg(rgb(BG_HOVER)))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.edit(cx, |s| s.toggle_visibility(id));
            }));

        let name = match layer.rename {
            Some(text) => div()
                .flex_1()
                .px_1()
                .bg(rgb(0x111111))
                .border_1()
                .border_color(rgb(ACCENT))
                .text_color(rgb(0xffffff))
                .child(format!("{text}▏")),
            None => div()
                .flex_1()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .when(!layer.visible && !selected, |d| d.text_color(rgb(TEXT_DIM)))
                .when(!layer.visible && selected, |d| d.text_color(rgb(0xb8cdee)))
                .child(layer.name),
        };

        let mut badges = div().flex().flex_row().gap_1();
        if layer.image {
            badges = badges.child(badge("IMG"));
        }
        if layer.locked {
            badges = badges.child(badge("LOCK"));
        }
        if layer.masked {
            badges = badges.child(badge("MASK"));
        }

        div()
            .id(SharedString::from(format!("layer-row-{id:?}")))
            .h(px(ROW_H))
            .px_2()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .text_xs()
            .border_b_1()
            .border_color(rgb(0x1b1b1b))
            .cursor_pointer()
            .when(selected, |d| d.bg(rgb(ACCENT)).text_color(rgb(0xffffff)))
            .when(!selected, |d| d.hover(|s| s.bg(rgb(BG_HOVER))))
            .child(eye)
            .child(name)
            .child(badges)
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                if ev.click_count() >= 2 {
                    this.edit(cx, |s| s.begin_rename(id));
                    window.focus(&this.focus, cx);
                } else {
                    this.edit(cx, |s| s.select_layer(id));
                }
            }))
    }

    /// Blend, opacity, lock and mask controls for the selected layer.
    fn controls(&self, cx: &mut Context<Self>) -> Div {
        let state = self.state.read(cx);
        let Some(layer) = state.selected() else {
            return div();
        };
        let (blend, opacity, lock, has_mask) = (
            layer.blend,
            layer.opacity.min(100) as i32,
            layer.lock,
            layer.mask.is_some(),
        );

        let mut blends = div().flex().flex_row().gap_1();
        for mode in BlendMode::ALL {
            blends = blends.child(
                toggle(format!("blend-{mode:?}"), blend_label(mode), blend == mode)
                    .flex_1()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit_selected(cx, |s, id| s.set_layer_blend(id, mode));
                    })),
            );
        }

        let mut bar = div().flex_1().h(px(14.)).flex().flex_row();
        let step = 100 / OPACITY_SEGMENTS;
        for i in 0..OPACITY_SEGMENTS {
            let value = (i + 1) * step;
            bar = bar.child(
                div()
                    .id(SharedString::from(format!("opacity-{i}")))
                    .flex_1()
                    .h_full()
                    .cursor_pointer()
                    .bg(rgb(if value <= opacity { ACCENT } else { 0x3a3a3a }))
                    .border_r_1()
                    .border_color(rgb(BG_PANEL_HEADER))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit_selected(cx, |s, id| s.set_layer_opacity(id, value));
                    })),
            );
        }
        let opacity_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(div().w(px(46.)).text_color(rgb(TEXT_DIM)).child("Opacity"))
            .child(
                button("opacity-down", "−").on_click(cx.listener(|this, _, _, cx| {
                    this.edit_selected(cx, |s, id| s.step_layer_opacity(id, -OPACITY_STEP));
                })),
            )
            .child(bar)
            .child(
                button("opacity-up", "+").on_click(cx.listener(|this, _, _, cx| {
                    this.edit_selected(cx, |s, id| s.step_layer_opacity(id, OPACITY_STEP));
                })),
            )
            .child(div().w(px(32.)).text_right().child(format!("{opacity}%")));

        let lock_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(div().w(px(46.)).text_color(rgb(TEXT_DIM)).child("Lock"))
            .child(
                toggle("lock-pixels".into(), "Pixels", lock.pixels)
                    .flex_1()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.edit_selected(cx, |s, id| s.toggle_lock_pixels(id));
                    })),
            )
            .child(
                toggle("lock-position".into(), "Position", lock.position)
                    .flex_1()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.edit_selected(cx, |s, id| s.toggle_lock_position(id));
                    })),
            );

        let mask_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(div().w(px(46.)).text_color(rgb(TEXT_DIM)).child("Mask"))
            .child(
                button("mask", if has_mask { "Remove mask" } else { "Add mask" })
                    .flex_1()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit_selected(cx, |s, id| {
                            if has_mask {
                                s.remove_mask(id)
                            } else {
                                s.add_mask(id)
                            }
                        });
                    })),
            );

        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .text_xs()
            .bg(rgb(BG_PANEL_HEADER))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(blends)
            .child(opacity_row)
            .child(lock_row)
            .child(mask_row)
    }

    fn footer(&self, cx: &mut Context<Self>) -> Div {
        let first =
            div()
                .flex()
                .flex_row()
                .gap_1()
                .child(button("layer-new", "New").flex_1().on_click(cx.listener(
                    |this, _, _, cx| {
                        this.edit(cx, |s| s.add_paint_layer());
                    },
                )))
                .child(
                    button("layer-image", "Image")
                        .flex_1()
                        .on_click(cx.listener(|this, _, _, cx| {
                            prompt_insert_image(&this.state, cx);
                        })),
                )
                .child(
                    button("layer-delete", "Delete")
                        .flex_1()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.edit(cx, |s| s.delete_selected_layer());
                        })),
                )
                .child(
                    button("layer-up", "Up")
                        .flex_1()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.edit(cx, |s| s.move_selected_layer(true));
                        })),
                )
                .child(button("layer-down", "Down").flex_1().on_click(cx.listener(
                    |this, _, _, cx| {
                        this.edit(cx, |s| s.move_selected_layer(false));
                    },
                )));
        let second = div()
            .flex()
            .flex_row()
            .gap_1()
            .child(
                button("layer-duplicate", "Duplicate")
                    .flex_1()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.edit(cx, |s| s.duplicate_selected_layer());
                    })),
            )
            .child(
                button("layer-flip-h", "Flip H")
                    .flex_1()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.edit(cx, |s| s.flip_selected_layer(FlipAxis::Horizontal));
                    })),
            )
            .child(
                button("layer-flip-v", "Flip V")
                    .flex_1()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.edit(cx, |s| s.flip_selected_layer(FlipAxis::Vertical));
                    })),
            );
        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_1()
            .text_xs()
            .border_t_1()
            .border_color(rgb(BORDER))
            .bg(rgb(BG_PANEL_HEADER))
            .child(first)
            .child(second)
    }
}

pub(crate) fn button(id: &'static str, label: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py(px(2.))
        .flex()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(0x4a4a4a))
        .bg(rgb(0x383838))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(BG_HOVER)))
        .child(label)
}

pub(crate) fn toggle(id: String, label: &'static str, on: bool) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .px_1()
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

fn badge(label: &'static str) -> Div {
    div()
        .px(px(3.))
        .rounded_sm()
        .border_1()
        .border_color(rgb(0x6a6a6a))
        .text_size(px(9.))
        .child(label)
}

impl Render for LayersPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (rows, selected) = {
            let s = self.state.read(cx);
            let rows: Vec<_> = s
                .document
                .layers()
                .iter()
                .rev()
                .map(|l| RowInfo::new(l, s))
                .collect();
            (rows, s.selected_layer)
        };
        let mut list = div()
            .id("layer-list")
            .min_h(px(ROW_H * LIST_MIN_ROWS))
            .max_h(px(ROW_H * LIST_MAX_ROWS))
            .flex()
            .flex_col()
            .overflow_y_scroll();
        for row in rows {
            let is_selected = selected == Some(row.id);
            list = list.child(self.row(row, is_selected, cx));
        }
        div()
            .id("layers-panel")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .w_full()
            .flex()
            .flex_col()
            .text_color(rgb(TEXT))
            .child(self.controls(cx))
            .child(list)
            .child(self.footer(cx))
    }
}
