//! The Filter panel: every v1 dither control over the shared
//! [`EditorState`]. Thin: each click calls one method from
//! [`crate::filter_ops`].
//!
//! Top to bottom: method, palette mode (with the 4-color preset,
//! intensity and background picker, or the custom swatch toggles),
//! brightness / contrast / saturation, Bayer spread (Bayer only), strength
//! and serpentine (error diffusion only), and a one-line summary.

use gpui::{Context, Div, Entity, Rgba, SharedString, Stateful, Window, div, prelude::*, px, rgb};
use rp_palette_dither::{CgaColor, DitherMethod, Intensity};

use crate::filter_ops::{
    AdjustControl, FILTER_STEP, FilterSlider, PRESETS, PaletteMode, color_name, intensity_label,
    method_short_label, preset_label,
};
use crate::layers_panel::toggle;
use crate::state::EditorState;
use crate::view::{ACCENT, BG_PANEL_HEADER, TEXT_DIM, edit_state, panel_header};

/// Cells in a slider bar (an even number, for the bipolar bars).
const BAR_SEGMENTS: i32 = 20;
const LABEL_W: f32 = 50.;
const SWATCH: f32 = 13.;

pub(crate) struct FilterPanel {
    state: Entity<EditorState>,
}

fn cga(c: CgaColor) -> Rgba {
    let c = c.rgb();
    Rgba {
        r: c.r as f32 / 255.,
        g: c.g as f32 / 255.,
        b: c.b as f32 / 255.,
        a: 1.,
    }
}

fn label(text: &'static str) -> Div {
    div()
        .w(px(LABEL_W))
        .flex_none()
        .text_color(rgb(TEXT_DIM))
        .child(text)
}

fn row() -> Div {
    div().flex().flex_row().items_center().gap_1()
}

/// A CGA swatch in its real color. `on` draws a bright ring; `dim` shows it
/// faded (a custom color that is toggled off).
fn swatch(id: String, c: CgaColor, on: bool, dim: bool) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .flex_1()
        .h(px(SWATCH))
        .bg(cga(c))
        .border_1()
        .cursor_pointer()
        .when(on, |d| d.border_color(rgb(0xffffff)))
        .when(!on, |d| d.border_color(rgb(0x4a4a4a)))
        .when(dim, |d| d.opacity(0.2))
}

impl FilterPanel {
    pub(crate) fn new(state: Entity<EditorState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state }
    }

    fn edit<R>(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut EditorState) -> R) -> R {
        edit_state(&self.state, cx, f)
    }

    fn method_row(&self, method: DitherMethod, cx: &mut Context<Self>) -> Div {
        let mut r = row();
        for m in DitherMethod::ALL {
            r = r.child(
                toggle(format!("method-{m:?}"), method_short_label(m), m == method)
                    .flex_1()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |s| s.set_dither_method(m));
                    })),
            );
        }
        r
    }

    fn palette_rows(&self, cx: &mut Context<Self>) -> Div {
        let (mode, four, custom) = {
            let s = self.state.read(cx);
            (s.palette_mode(), s.four_color(), s.custom_palette())
        };
        let mut modes = row().child(label("Palette"));
        for m in PaletteMode::ALL {
            modes = modes.child(
                toggle(format!("palette-{m:?}"), m.label(), m == mode)
                    .flex_1()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |s| s.set_palette_mode(m));
                    })),
            );
        }
        let mut out = div().flex().flex_col().gap_1().child(modes);

        if let Some(m) = four {
            let mut presets = row().child(label("Preset"));
            for p in PRESETS {
                presets = presets.child(
                    toggle(format!("preset-{p:?}"), preset_label(p), p == m.preset)
                        .flex_1()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |s| s.set_four_color_preset(p));
                        })),
                );
            }
            let mut intensity = row().child(label("Intensity"));
            for i in [Intensity::Low, Intensity::High] {
                intensity = intensity.child(
                    toggle(
                        format!("intensity-{i:?}"),
                        intensity_label(i),
                        i == m.intensity,
                    )
                    .flex_1()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |s| s.set_four_color_intensity(i));
                    })),
                );
            }
            // The three preset colors, read-only, next to the intensity.
            for c in m.preset.colors(m.intensity) {
                intensity = intensity.child(
                    div()
                        .w(px(SWATCH))
                        .h(px(SWATCH))
                        .bg(cga(c))
                        .border_1()
                        .border_color(rgb(0x4a4a4a)),
                );
            }
            out = out
                .child(presets)
                .child(intensity)
                .child(
                    row()
                        .child(label("Backgr."))
                        .child(format!("color 0: {}", color_name(m.background))),
                )
                .child(self.swatch_grid(
                    "bg",
                    cx,
                    |c| (c == m.background, false),
                    |s, c| {
                        s.set_four_color_background(c);
                    },
                ));
        }
        if let Some(p) = custom {
            out = out
                .child(
                    row()
                        .child(label("Colors"))
                        .child(format!("{} of 16 (click to toggle)", p.len())),
                )
                .child(self.swatch_grid(
                    "custom",
                    cx,
                    |c| (p.contains(c), !p.contains(c)),
                    |s, c| {
                        s.toggle_custom_color(c);
                    },
                ));
        }
        out
    }

    /// Two rows of eight CGA swatches. `look` gives `(ring, dim)` per color.
    fn swatch_grid(
        &self,
        prefix: &'static str,
        cx: &mut Context<Self>,
        look: impl Fn(CgaColor) -> (bool, bool),
        on_click: fn(&mut EditorState, CgaColor),
    ) -> Div {
        let mut grid = div().flex().flex_col().gap(px(2.));
        for half in CgaColor::ALL.chunks(8) {
            let mut r = div().flex().flex_row().gap(px(2.));
            for &c in half {
                let (on, dim) = look(c);
                r = r.child(
                    swatch(format!("{prefix}-{c:?}"), c, on, dim).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.edit(cx, |s| on_click(s, c));
                        },
                    )),
                );
            }
            grid = grid.child(r);
        }
        grid
    }

    fn slider_row(&self, slider: FilterSlider, cx: &mut Context<Self>) -> Div {
        let value = slider.value(self.state.read(cx));
        let id = format!("{slider:?}");
        let mut bar = div().flex_1().h(px(12.)).flex().flex_row();
        let bipolar = slider.range().0 < 0;
        for (i, (target, filled)) in slider.bar(value, BAR_SEGMENTS).into_iter().enumerate() {
            let center = bipolar && i as i32 == BAR_SEGMENTS / 2;
            bar = bar.child(
                div()
                    .id(SharedString::from(format!("{id}-bar-{i}")))
                    .flex_1()
                    .h_full()
                    .cursor_pointer()
                    .bg(rgb(if filled { ACCENT } else { 0x3a3a3a }))
                    .border_r_1()
                    .border_color(rgb(BG_PANEL_HEADER))
                    .when(center, |d| d.border_l_1().border_color(rgb(0x8a8a8a)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |s| slider.set(s, target));
                    })),
            );
        }
        row()
            .child(label(slider.label()))
            .child(
                toggle(format!("{id}-down"), "−", false).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.edit(cx, |s| slider.step(s, -FILTER_STEP));
                    },
                )),
            )
            .child(bar)
            .child(toggle(format!("{id}-up"), "+", false).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.edit(cx, |s| slider.step(s, FILTER_STEP));
                },
            )))
            .child(
                div()
                    .w(px(30.))
                    .flex_none()
                    .text_right()
                    .child(slider.readout(value)),
            )
            .child(
                toggle(format!("{id}-reset"), "↺", false).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.edit(cx, |s| slider.reset(s));
                    },
                )),
            )
    }
}

impl Render for FilterPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (method, serpentine, bayer, diffusion, summary) = {
            let s = self.state.read(cx);
            (
                s.dither().method,
                s.dither().serpentine,
                s.bayer_controls_enabled(),
                s.diffusion_controls_enabled(),
                s.dither_summary(),
            )
        };
        let mut body = div()
            .id("filter-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .text_xs()
            .child(self.method_row(method, cx))
            .child(self.palette_rows(cx));
        for c in AdjustControl::ALL {
            body = body.child(self.slider_row(FilterSlider::Adjust(c), cx));
        }
        if bayer {
            body = body.child(self.slider_row(FilterSlider::BayerSpread, cx));
        }
        if diffusion {
            body = body
                .child(self.slider_row(FilterSlider::Strength, cx))
                .child(
                    row().child(label("Scan")).child(
                        toggle("serpentine".into(), "Serpentine", serpentine)
                            .flex_1()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.edit(cx, |s| s.toggle_serpentine());
                            })),
                    ),
                );
        }
        body = body.child(div().pt_1().text_color(rgb(TEXT_DIM)).child(summary));
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(panel_header("Filter"))
            .child(body)
    }
}
