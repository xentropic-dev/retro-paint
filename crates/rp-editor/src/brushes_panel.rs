//! The Brushes section: a thumbnail grid of the brush library (built-in
//! round Hard/Soft first), click to select; Add brushes… / Add folder…
//! open the portal file chooser, and brush files or folders can be dropped
//! on the section. Thin: the model is [`crate::brush_library`], the file
//! work [`crate::brush_flow`].
//!
//! Thumbnail images are made once per brush, when its thumbnail arrives,
//! and cached by key (the ribbon's current-brush chip uses the same cache).

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    Context, Entity, ExternalPaths, RenderImage, SharedString, Window, div, img, prelude::*, px,
    rgb,
};
use rp_brush::Mask;

use crate::brush_flow::{add_brush_paths, prompt_add_brushes, select_brush};
use crate::brush_library::BrushKey;
use crate::layers_panel::button;
use crate::state::EditorState;
use crate::view::{ACCENT, BG_HOVER, TEXT, TEXT_DIM, text_tooltip};

/// Grid cell size and the thumbnail box inside it, in logical pixels.
const CELL: f32 = 38.;
const THUMB_BOX: f32 = 32.;
/// The grid shows this many rows, then scrolls.
const MAX_ROWS: f32 = 6.;
/// Background of the section while brush files are dragged over it.
const DROP_HIGHLIGHT: u32 = 0x2a3f5c;
const CELL_BG: u32 = 0x1a1a1a;

/// A thumbnail as a GPUI image: white with the brush's coverage as alpha.
fn thumb_image(mask: &Mask) -> Arc<RenderImage> {
    let mut bgra = Vec::with_capacity(mask.data.len() * 4);
    for &a in &mask.data {
        bgra.extend_from_slice(&[255, 255, 255, a]);
    }
    let buf =
        image::RgbaImage::from_raw(mask.width, mask.height, bgra).expect("buffer matches size");
    Arc::new(RenderImage::new(vec![image::Frame::new(buf)]))
}

/// Display size of a `w × h` thumbnail fitted into a `side` box.
pub(crate) fn fit(w: u32, h: u32, side: f32) -> (f32, f32) {
    let k = side / w.max(h).max(1) as f32;
    ((w as f32 * k).max(1.), (h as f32 * k).max(1.))
}

pub(crate) struct BrushesPanel {
    state: Entity<EditorState>,
    images: HashMap<BrushKey, Arc<RenderImage>>,
}

impl BrushesPanel {
    pub(crate) fn new(state: Entity<EditorState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            images: HashMap::new(),
        }
    }

    /// The cached image for `key`'s thumbnail, made on first use.
    pub(crate) fn image(&mut self, key: &BrushKey, mask: &Mask) -> Arc<RenderImage> {
        self.images
            .entry(key.clone())
            .or_insert_with(|| thumb_image(mask))
            .clone()
    }
}

impl Render for BrushesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (cells, selected, pending, n) = {
            let lib = &self.state.read(cx).brushes;
            let cells: Vec<_> = lib
                .entries()
                .iter()
                .map(|e| {
                    (
                        e.key.clone(),
                        e.brush.name.clone(),
                        e.brush.width,
                        e.brush.height,
                        e.thumb.clone(),
                    )
                })
                .collect();
            let n = cells.len();
            (cells, lib.selected().clone(), lib.pending().cloned(), n)
        };

        let controls = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_1()
            .px_2()
            .pt_1()
            .text_xs()
            .child(
                button("brushes-add", "+ Add brushes…").on_click(cx.listener(|this, _, _, cx| {
                    prompt_add_brushes(&this.state, false, cx);
                })),
            )
            .child(
                button("brushes-add-folder", "+ Add folder…").on_click(cx.listener(
                    |this, _, _, cx| {
                        prompt_add_brushes(&this.state, true, cx);
                    },
                )),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .justify_end()
                    .text_color(rgb(TEXT_DIM))
                    .child(format!("{n}")),
            );

        let mut grid = div()
            .p_2()
            .flex()
            .flex_row()
            .flex_wrap()
            .content_start()
            .gap(px(3.));
        for (i, (key, name, w, h, thumb)) in cells.into_iter().enumerate() {
            let is_selected = key == selected;
            let is_pending = pending.as_ref() == Some(&key);
            let tip = format!("{name} ({w}×{h})");
            let inner = match &thumb {
                Some(mask) => {
                    let image = self.image(&key, mask);
                    let (dw, dh) = fit(mask.width, mask.height, THUMB_BOX);
                    div().child(img(image).w(px(dw)).h(px(dh)))
                }
                None => div().text_xs().text_color(rgb(TEXT_DIM)).child("…"),
            };
            grid = grid.child(
                div()
                    .id(SharedString::from(format!("brush-{i}")))
                    .w(px(CELL))
                    .h(px(CELL))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgb(CELL_BG))
                    .border_1()
                    .rounded_sm()
                    .when(is_selected, |d| d.border_2().border_color(rgb(ACCENT)))
                    .when(is_pending, |d| d.border_color(rgb(TEXT_DIM)))
                    .when(!is_selected && !is_pending, |d| {
                        d.border_color(rgb(0x3a3a3a))
                            .hover(|s| s.border_color(rgb(BG_HOVER)).bg(rgb(0x2a2a2a)))
                    })
                    .cursor_pointer()
                    .tooltip(text_tooltip(tip))
                    .child(inner)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        select_brush(&this.state, key.clone(), cx);
                    })),
            );
        }

        div()
            .id("brushes-section")
            .w_full()
            .flex()
            .flex_col()
            .text_color(rgb(TEXT))
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(rgb(DROP_HIGHLIGHT)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                add_brush_paths(&this.state, paths.paths().to_vec(), cx);
            }))
            .child(controls)
            .child(
                div()
                    .id("brush-grid")
                    .max_h(px(MAX_ROWS * (CELL + 3.) + 16.))
                    .overflow_y_scroll()
                    .child(grid),
            )
            .child(
                div()
                    .px_2()
                    .pb_1()
                    .text_size(px(10.))
                    .text_color(rgb(TEXT_DIM))
                    .child("Click: select · Drop .abr/.gbr/.vbr/.gih files or folders here"),
            )
    }
}
