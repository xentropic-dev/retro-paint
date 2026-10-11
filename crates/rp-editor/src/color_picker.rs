//! The color picker popover (click the fg or bg chip, or the Swatches
//! panel's "+ New color…"). Thin: every input calls one method from
//! [`crate::color_picker_ops`].
//!
//! The saturation/value square is a small cached image, rebuilt only when
//! the hue changes; the hue bar is built once. Drags are tracked with
//! window-level mouse listeners so they keep going outside the square.

use std::sync::Arc;

use gpui::{
    App, Bounds, Context, DispatchPhase, Div, Entity, FocusHandle, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, RenderImage, SharedString, Window,
    canvas, div, img, prelude::*, px, rgb,
};
use rp_palette_dither::CGA_RGB;

use crate::canvas_image::rgba_into_bgra;
use crate::color_picker_ops::{
    ColorPicker, LutCache, PickerDrag, PickerField, PickerTarget, dithers_to, hue_bar_rgba,
    hue_pos, square_pos, square_rgba,
};
use crate::filter_ops::color_name;
use crate::project_dialog::chip;
use crate::state::EditorState;
use crate::view::{ACCENT, BG_MENU, BORDER, TEXT, TEXT_DIM, edit_state, panel_header, rgba};

/// Pixels of the cached saturation/value image (it is drawn scaled).
const SQUARE_IMG: u32 = 128;
/// On-screen side of the square.
const SQUARE_PX: f32 = 168.;
const HUE_W: f32 = 18.;
const HUE_IMG_H: u32 = 256;
const DIALOG_W: f32 = 344.;
const CGA_CELL: f32 = 17.;

pub(crate) struct ColorPickerDialog {
    state: Entity<EditorState>,
    /// Holds keyboard focus while the picker is open.
    focus: FocusHandle,
    /// The editor's focus, restored when the picker closes.
    editor_focus: FocusHandle,
    lut: LutCache,
    /// The square image and the hue it was built for.
    square: Option<(f32, Arc<RenderImage>)>,
    hue_bar: Arc<RenderImage>,
    /// Replaced square images, freed on the next render.
    stale: Vec<Arc<RenderImage>>,
}

fn render_image(width: u32, height: u32, rgba: Vec<u8>) -> Arc<RenderImage> {
    let buf = image::RgbaImage::from_raw(width, height, rgba_into_bgra(rgba))
        .expect("buffer matches size");
    Arc::new(RenderImage::new(vec![image::Frame::new(buf)]))
}

impl ColorPickerDialog {
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
            lut: LutCache::default(),
            square: None,
            hue_bar: render_image(1, HUE_IMG_H, hue_bar_rgba(1, HUE_IMG_H)),
            stale: Vec::new(),
        }
    }

    /// Takes keyboard focus (call when the picker opens).
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
    }

    fn picker(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut ColorPicker)) {
        edit_state(&self.state, cx, |s| s.edit_color_picker(f));
    }

    fn close(&self, window: &mut Window, cx: &mut Context<Self>, ok: bool) {
        edit_state(&self.state, cx, |s| {
            if ok {
                s.confirm_color_picker();
            } else {
                s.cancel_color_picker();
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
            .color_picker
            .as_ref()
            .is_some_and(|p| p.editing.is_some());
        match k.key.as_str() {
            "enter" if editing => self.picker(cx, |p| p.commit_edit()),
            "enter" => self.close(window, cx, true),
            "escape" if editing => self.picker(cx, |p| p.cancel_edit()),
            "escape" => self.close(window, cx, false),
            "tab" => self.picker(cx, |p| p.tab()),
            "backspace" => self.picker(cx, |p| p.backspace()),
            _ => {
                if let Some(text) = k.key_char.clone() {
                    self.picker(cx, |p| p.type_text(&text));
                }
            }
        }
        // Keep editor shortcuts ([ ] X D, space) out of the picker.
        cx.stop_propagation();
    }

    /// A press, drag or release over a drag surface of `kind`.
    fn pointer(
        &mut self,
        kind: PickerDrag,
        pos: Point<Pixels>,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let x = f32::from(pos.x - bounds.origin.x);
        let y = f32::from(pos.y - bounds.origin.y);
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        self.picker(cx, |p| {
            p.drag = Some(kind);
            match kind {
                PickerDrag::Square => p.drag_square(x, y, w, h),
                PickerDrag::Hue => p.drag_hue(y, h),
            }
        });
    }

    fn dragging(&self, kind: PickerDrag, cx: &App) -> bool {
        self.state
            .read(cx)
            .color_picker
            .as_ref()
            .is_some_and(|p| p.drag == Some(kind))
    }

    /// An invisible layer over the square or hue bar that turns presses and
    /// drags into picker updates.
    fn drag_surface(&self, kind: PickerDrag, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        canvas(
            |bounds, _, _| bounds,
            move |_, bounds, window, _| {
                let t = this.clone();
                window.on_mouse_event(move |ev: &MouseDownEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble
                        && ev.button == MouseButton::Left
                        && bounds.contains(&ev.position)
                    {
                        t.update(cx, |v, cx| v.pointer(kind, ev.position, bounds, cx));
                    }
                });
                let t = this.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Bubble || !t.read(cx).dragging(kind, cx) {
                        return;
                    }
                    t.update(cx, |v, cx| {
                        if ev.pressed_button == Some(MouseButton::Left) {
                            v.pointer(kind, ev.position, bounds, cx);
                        } else {
                            // The release was missed: end the drag.
                            v.picker(cx, |p| p.drag = None);
                        }
                    });
                });
                let t = this.clone();
                window.on_mouse_event(move |ev: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble
                        && ev.button == MouseButton::Left
                        && t.read(cx).dragging(kind, cx)
                    {
                        t.update(cx, |v, cx| v.picker(cx, |p| p.drag = None));
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    fn square(&mut self, p: &ColorPicker, cx: &mut Context<Self>) -> Div {
        let hue = p.hsv.h;
        if self.square.as_ref().is_none_or(|(h, _)| *h != hue) {
            let image = render_image(SQUARE_IMG, SQUARE_IMG, square_rgba(hue, SQUARE_IMG));
            if let Some((_, old)) = self.square.replace((hue, image)) {
                self.stale.push(old);
            }
        }
        let image = self.square.as_ref().expect("just built").1.clone();
        let (mx, my) = square_pos(p.hsv.s, p.hsv.v, SQUARE_PX, SQUARE_PX);
        // White ring on dark colors, black on light ones.
        let light = p.hsv.v > 0.6 && p.hsv.s < 0.5;
        div()
            .relative()
            .flex_none()
            .w(px(SQUARE_PX))
            .h(px(SQUARE_PX))
            .border_1()
            .border_color(rgb(BORDER))
            .cursor_crosshair()
            .child(img(image).size_full())
            .child(
                div()
                    .absolute()
                    .left(px(mx - 5.))
                    .top(px(my - 5.))
                    .w(px(10.))
                    .h(px(10.))
                    .rounded_full()
                    .border_2()
                    .border_color(rgb(if light { 0x000000 } else { 0xffffff })),
            )
            .child(self.drag_surface(PickerDrag::Square, cx))
    }

    fn hue_bar(&self, p: &ColorPicker, cx: &mut Context<Self>) -> Div {
        let y = hue_pos(p.hsv.h, SQUARE_PX);
        div()
            .relative()
            .flex_none()
            .w(px(HUE_W))
            .h(px(SQUARE_PX))
            .border_1()
            .border_color(rgb(BORDER))
            .cursor_pointer()
            .child(img(self.hue_bar.clone()).size_full())
            .child(
                div()
                    .absolute()
                    .left(px(-2.))
                    .top(px(y - 2.))
                    .w(px(HUE_W + 2.))
                    .h(px(4.))
                    .border_1()
                    .border_color(rgb(0xffffff))
                    .bg(gpui::rgba(0x00000066)),
            )
            .child(self.drag_surface(PickerDrag::Hue, cx))
    }

    fn field(&self, p: &ColorPicker, field: PickerField, cx: &mut Context<Self>) -> Div {
        let editing = p.editing.as_ref().filter(|e| e.field == field);
        let value = match editing {
            Some(e) if field == PickerField::Hex => format!("#{}▏", e.text),
            Some(e) => format!("{}▏", e.text),
            None => p.field_value(field),
        };
        let width = if field == PickerField::Hex { 72. } else { 40. };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(
                div()
                    .w(px(24.))
                    .text_color(rgb(TEXT_DIM))
                    .child(field.label()),
            )
            .child(
                div()
                    .id(SharedString::from(format!("picker-field-{field:?}")))
                    .w(px(width))
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
                        this.picker(cx, |p| p.begin_edit(field));
                        this.focus(window, cx);
                    })),
            )
    }

    /// Old/new comparison, "dithers to" and the typed fields.
    fn side(&mut self, p: &ColorPicker, cx: &mut Context<Self>) -> Div {
        let (palette, adjust) = {
            let d = &self.state.read(cx).document.dither;
            (d.palette.palette(), d.adjust)
        };
        let to = dithers_to(self.lut.get(palette), &adjust, p.color);
        let compare = div()
            .flex()
            .flex_row()
            .child(
                div()
                    .id("picker-old")
                    .w(px(48.))
                    .h(px(28.))
                    .bg(rgba(p.old))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.picker(cx, |p| p.set_rgb(p.old));
                    })),
            )
            .child(div().w(px(48.)).h(px(28.)).bg(rgba(p.color)));
        let mut fields = div().flex().flex_col().gap_1();
        for f in PickerField::ALL {
            fields = fields.child(self.field(p, f, cx));
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .text_color(rgb(TEXT_DIM))
                            .child(div().w(px(48.)).child("Old"))
                            .child(div().w(px(48.)).child("New")),
                    )
                    .child(div().border_1().border_color(rgb(BORDER)).child(compare)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().text_color(rgb(TEXT_DIM)).child("Dithers to"))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .w(px(16.))
                                    .h(px(16.))
                                    .bg(rgba(to.rgb()))
                                    .border_1()
                                    .border_color(rgb(0x9a9a9a)),
                            )
                            .child(color_name(to)),
                    ),
            )
            .child(fields)
            .child(div().text_color(rgb(TEXT_DIM)).child(format!(
                "H {:.0}°  S {:.0}%  V {:.0}%",
                p.hsv.h,
                p.hsv.s * 100.,
                p.hsv.v * 100.
            )))
    }

    fn cga_row(&self, p: &ColorPicker, cx: &mut Context<Self>) -> Div {
        let mut row = div().flex().flex_row().gap(px(2.));
        for (i, c) in CGA_RGB.into_iter().enumerate() {
            row = row.child(
                div()
                    .id(SharedString::from(format!("picker-cga-{i}")))
                    .w(px(CGA_CELL))
                    .h(px(CGA_CELL))
                    .bg(rgba(c))
                    .border_1()
                    .border_color(rgb(if c == p.color { 0xffffff } else { 0x4a4a4a }))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.picker(cx, |p| p.set_rgb(c));
                    })),
            );
        }
        row
    }
}

impl Render for ColorPickerDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for img in self.stale.drain(..) {
            let _ = window.drop_image(img);
        }
        let Some(p) = self.state.read(cx).color_picker.clone() else {
            return div().into_any_element();
        };
        let title = match p.target {
            PickerTarget::Foreground => "Foreground color",
            PickerTarget::Background => "Background color",
            PickerTarget::NewSwatch => "New swatch",
        };
        let new_swatch = p.target == PickerTarget::NewSwatch;
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .when(!new_swatch, |d| {
                d.child(
                    chip("picker-add".into(), "Add to swatches".into(), false).on_click(
                        cx.listener(|this, _, _, cx| {
                            edit_state(&this.state, cx, |s| s.color_picker_add_swatch());
                        }),
                    ),
                )
            })
            .child(div().flex_1())
            .child(
                chip("picker-cancel".into(), "Cancel".into(), false)
                    .on_click(cx.listener(|this, _, window, cx| this.close(window, cx, false))),
            )
            .child(
                chip(
                    "picker-ok".into(),
                    if new_swatch { "Add" } else { "OK" }.into(),
                    true,
                )
                .on_click(cx.listener(|this, _, window, cx| this.close(window, cx, true))),
            );
        let square = self.square(&p, cx);
        let hue = self.hue_bar(&p, cx);
        let side = self.side(&p, cx);
        let cga = self.cga_row(&p, cx);
        div()
            .id("color-picker")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            // Clicks on empty picker space finish any typing.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.picker(cx, |p| p.commit_edit())),
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
            .child(panel_header(title))
            .child(
                div()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(square)
                            .child(hue)
                            .child(side),
                    )
                    .child(cga)
                    .child(footer),
            )
            .into_any_element()
    }
}
