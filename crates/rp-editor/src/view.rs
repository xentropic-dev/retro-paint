//! The GPUI editor window: menu bar, tool ribbon, canvas area, right panels,
//! status bar. Thin: every decision goes through [`EditorState`].

use gpui::{
    App, Context, Div, FocusHandle, KeyBinding, MouseButton, Rgba, SharedString, Stateful, Window,
    actions, div, prelude::*, px, rgb,
};
use rp_doc::Tool;
use rp_palette_dither::Rgb;

use crate::FrameSink;
use crate::state::{EditorState, FileCommand, Outcome, shortcut_label, tool_label};

actions!(
    retro_paint,
    [NewFile, OpenFile, SaveFile, SaveFileAs, ExportPng, Quit]
);

/// Registers the File-menu key bindings.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys(FileCommand::ALL.map(|cmd| {
        let k = cmd.keystroke();
        match cmd {
            FileCommand::New => KeyBinding::new(k, NewFile, None),
            FileCommand::Open => KeyBinding::new(k, OpenFile, None),
            FileCommand::Save => KeyBinding::new(k, SaveFile, None),
            FileCommand::SaveAs => KeyBinding::new(k, SaveFileAs, None),
            FileCommand::ExportPng => KeyBinding::new(k, ExportPng, None),
            FileCommand::Quit => KeyBinding::new(k, Quit, None),
        }
    }));
}

// Theme: dark neutral greys, one blue accent.
const BG_APP: u32 = 0x1e1e1e;
const BG_BAR: u32 = 0x2b2b2b;
const BG_RIBBON: u32 = 0x323232;
const BG_PANEL: u32 = 0x262626;
const BG_PANEL_HEADER: u32 = 0x303030;
const BG_CANVAS: u32 = 0x161616;
const BG_HOVER: u32 = 0x3d3d3d;
const BG_MENU: u32 = 0x2d2d2d;
const ACCENT: u32 = 0x3d7fd9;
const BORDER: u32 = 0x111111;
const TEXT: u32 = 0xdcdcdc;
const TEXT_DIM: u32 = 0x8a8a8a;

const MENU_BAR_H: f32 = 26.;
const RIBBON_H: f32 = 44.;
const PANEL_W: f32 = 260.;

pub(crate) struct EditorView {
    pub(crate) state: EditorState,
    focus: FocusHandle,
    pub(crate) tick: u32,
    sink: FrameSink,
}

impl EditorView {
    pub(crate) fn new(sink: FrameSink, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            state: EditorState::new(),
            focus,
            tick: 0,
            sink,
        }
    }

    /// Advances the DOS test pattern (kept until the render worker exists).
    pub(crate) fn step(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        (self.sink)(
            crate::PATTERN_W,
            crate::PATTERN_H,
            crate::test_pattern(self.tick),
        );
    }

    fn run(&mut self, cmd: FileCommand, _window: &mut Window, cx: &mut Context<Self>) {
        match self.state.dispatch(cmd) {
            Outcome::Quit => cx.quit(),
            Outcome::Done | Outcome::NotImplemented(_) => {}
        }
        cx.notify();
    }

    // One handler per action; file-ops replaces the stubs behind them.
    fn on_new(&mut self, _: &NewFile, w: &mut Window, cx: &mut Context<Self>) {
        self.run(FileCommand::New, w, cx);
    }
    fn on_open(&mut self, _: &OpenFile, w: &mut Window, cx: &mut Context<Self>) {
        self.run(FileCommand::Open, w, cx);
    }
    fn on_save(&mut self, _: &SaveFile, w: &mut Window, cx: &mut Context<Self>) {
        self.run(FileCommand::Save, w, cx);
    }
    fn on_save_as(&mut self, _: &SaveFileAs, w: &mut Window, cx: &mut Context<Self>) {
        self.run(FileCommand::SaveAs, w, cx);
    }
    fn on_export_png(&mut self, _: &ExportPng, w: &mut Window, cx: &mut Context<Self>) {
        self.run(FileCommand::ExportPng, w, cx);
    }
    fn on_quit(&mut self, _: &Quit, w: &mut Window, cx: &mut Context<Self>) {
        self.run(FileCommand::Quit, w, cx);
    }

    fn menu_bar(&self, cx: &mut Context<Self>) -> Div {
        let open = self.state.file_menu_open;
        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(MENU_BAR_H))
            .px_1()
            .bg(rgb(BG_BAR))
            .border_b_1()
            .border_color(rgb(BORDER))
            .text_sm()
            .child(
                div()
                    .id("menu-file")
                    .px_3()
                    .h_full()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .when(open, |d| d.bg(rgb(BG_HOVER)))
                    .hover(|s| s.bg(rgb(BG_HOVER)))
                    .child("File")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.toggle_file_menu();
                        cx.notify();
                    })),
            )
    }

    fn file_menu(&self, cx: &mut Context<Self>) -> Div {
        let mut menu = div()
            .absolute()
            .top(px(MENU_BAR_H))
            .left(px(4.))
            .w(px(240.))
            .py_1()
            .flex()
            .flex_col()
            .bg(rgb(BG_MENU))
            .border_1()
            .border_color(rgb(BORDER))
            .rounded_sm()
            .shadow_lg()
            .text_sm()
            .occlude();
        for cmd in FileCommand::ALL {
            menu = menu.child(
                div()
                    .id(SharedString::from(format!("file-{cmd:?}")))
                    .flex()
                    .flex_row()
                    .justify_between()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(ACCENT)))
                    .child(cmd.label())
                    .child(
                        div()
                            .text_color(rgb(TEXT_DIM))
                            .child(shortcut_label(cmd.keystroke())),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.run(cmd, window, cx))),
            );
            if cmd.separator_after() {
                menu = menu.child(div().my_1().h(px(1.)).bg(rgb(0x444444)));
            }
        }
        menu
    }

    fn ribbon(&self, cx: &mut Context<Self>) -> Div {
        let doc = &self.state.document;
        let mut tools = div().flex().flex_row().gap_1();
        for tool in Tool::ALL {
            tools = tools.child(
                tool_button(tool, self.state.tool == tool).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.state.select_tool(tool);
                        cx.notify();
                    },
                )),
            );
        }
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(RIBBON_H))
            .px_2()
            .bg(rgb(BG_RIBBON))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(tools)
            .child(
                div()
                    .relative()
                    .w(px(34.))
                    .h(px(34.))
                    .child(color_chip(doc.bg).absolute().right_0().bottom_0())
                    .child(color_chip(doc.fg).absolute().left_0().top_0()),
            )
    }

    fn canvas_area(&self) -> Div {
        let doc = &self.state.document;
        let (c, o) = (doc.canvas_size, doc.output_size);
        div()
            .flex_1()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(BG_CANVAS))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .text_color(rgb(TEXT_DIM))
                    .text_sm()
                    .child(format!("Canvas {}×{}", c.width, c.height))
                    .child(format!("Output {}×{}", o.width, o.height)),
            )
    }

    fn panels(&self) -> Div {
        let layers = self.state.document.layers().len();
        div()
            .w(px(PANEL_W))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(BG_PANEL))
            .border_l_1()
            .border_color(rgb(BORDER))
            .child(panel("Layers", format!("{layers} layers")).flex_1())
            .child(panel("Filter", "Dither settings".into()).h(px(220.)))
            .child(panel("Swatches", "No swatches".into()).h(px(140.)))
    }

    fn status_bar(&self) -> Div {
        let doc = &self.state.document;
        div()
            .flex()
            .flex_row()
            .gap_4()
            .h(px(22.))
            .px_2()
            .items_center()
            .bg(rgb(BG_BAR))
            .border_t_1()
            .border_color(rgb(BORDER))
            .text_xs()
            .text_color(rgb(TEXT_DIM))
            .child(tool_label(self.state.tool))
            .child(format!(
                "{}×{} → {}×{}",
                doc.canvas_size.width,
                doc.canvas_size.height,
                doc.output_size.width,
                doc.output_size.height
            ))
            .child(self.state.status.clone())
    }
}

fn tool_button(tool: Tool, selected: bool) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("tool-{tool:?}")))
        .px_3()
        .py_1()
        .rounded_sm()
        .text_sm()
        .cursor_pointer()
        .border_1()
        .when(selected, |d| {
            d.bg(rgb(ACCENT))
                .border_color(rgb(0x5a96e8))
                .text_color(rgb(0xffffff))
        })
        .when(!selected, |d| {
            d.border_color(rgb(0x404040)).hover(|s| s.bg(rgb(BG_HOVER)))
        })
        .child(tool_label(tool))
}

fn color_chip(c: Rgb) -> Div {
    let color = Rgba {
        r: c.r as f32 / 255.,
        g: c.g as f32 / 255.,
        b: c.b as f32 / 255.,
        a: 1.,
    };
    div()
        .w(px(22.))
        .h(px(22.))
        .bg(color)
        .border_1()
        .border_color(rgb(0x9a9a9a))
}

fn panel(title: &'static str, placeholder: String) -> Div {
    div()
        .flex()
        .flex_col()
        .border_b_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .px_2()
                .py_1()
                .bg(rgb(BG_PANEL_HEADER))
                .text_xs()
                .text_color(rgb(TEXT))
                .child(title),
        )
        .child(
            div()
                .flex_1()
                .p_2()
                .text_xs()
                .text_color(rgb(TEXT_DIM))
                .child(placeholder),
        )
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let menu_open = self.state.file_menu_open;
        div()
            .id("editor")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_new))
            .on_action(cx.listener(Self::on_open))
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_save_as))
            .on_action(cx.listener(Self::on_export_png))
            .on_action(cx.listener(Self::on_quit))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG_APP))
            .text_color(rgb(TEXT))
            .child(self.menu_bar(cx))
            .child(self.ribbon(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(self.canvas_area())
                    .child(self.panels()),
            )
            .child(self.status_bar())
            .when(menu_open, |root| {
                root.child(
                    // Click-away backdrop below the menu bar closes the menu.
                    div()
                        .absolute()
                        .top(px(MENU_BAR_H))
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .occlude()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.state.close_menus();
                                cx.notify();
                            }),
                        ),
                )
                .child(self.file_menu(cx))
            })
    }
}
