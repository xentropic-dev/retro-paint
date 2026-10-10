//! The Layers panel: a separate GPUI view over the shared [`EditorState`].

use gpui::{Context, Entity, Window, div, prelude::*, rgb};

use crate::state::EditorState;
use crate::view::{TEXT_DIM, panel_header};

pub(crate) struct LayersPanel {
    state: Entity<EditorState>,
}

impl LayersPanel {
    pub(crate) fn new(state: Entity<EditorState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state }
    }
}

impl Render for LayersPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layers = self.state.read(cx).document.layers().len();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(panel_header("Layers"))
            .child(
                div()
                    .flex_1()
                    .p_2()
                    .text_xs()
                    .text_color(rgb(TEXT_DIM))
                    .child(format!("{layers} layers")),
            )
    }
}
