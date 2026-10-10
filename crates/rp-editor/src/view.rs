//! The GPUI editor window: menu bar, tool ribbon, canvas area, right panels,
//! status bar. Thin: every decision goes through [`EditorState`].

use gpui::{
    App, Context, Div, Entity, ExternalPaths, FocusHandle, KeyBinding, KeyDownEvent, KeyUpEvent,
    ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, Rgba, ScrollDelta, ScrollWheelEvent, SharedString, Stateful, Window, actions, canvas,
    div, point, prelude::*, px, rgb,
};
use rp_doc::Tool;
use rp_palette_dither::Rgb;

use crate::canvas_view::{self, CanvasCache};
use crate::filter_panel::FilterPanel;
use crate::insert_flow::{insert_image_files, prompt_insert_image};
use crate::layers_panel::LayersPanel;
use crate::paint_ops::{BrushTip, MAX_BRUSH_SIZE, MIN_BRUSH_SIZE, PaintButton};
use crate::render_worker::{
    CanvasReceiver, Overlay, RenderJob, Request, TIMING_ENV, WorkerClient, next_request,
};
use crate::state::{EditorState, FileCommand, Outcome, ViewCommand, shortcut_label, tool_label};
use crate::transform_ops::arrow_nudge;
use crate::viewport::Vec2;

actions!(
    retro_paint,
    [
        NewFile,
        OpenFile,
        InsertImage,
        SaveFile,
        SaveFileAs,
        ExportPng,
        Quit,
        ZoomIn,
        ZoomOut,
        ActualSize,
        FitCanvas
    ]
);

/// Registers the File-menu key bindings.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys(FileCommand::ALL.map(|cmd| {
        let k = cmd.keystroke();
        match cmd {
            FileCommand::New => KeyBinding::new(k, NewFile, None),
            FileCommand::Open => KeyBinding::new(k, OpenFile, None),
            FileCommand::InsertImage => KeyBinding::new(k, InsertImage, None),
            FileCommand::Save => KeyBinding::new(k, SaveFile, None),
            FileCommand::SaveAs => KeyBinding::new(k, SaveFileAs, None),
            FileCommand::ExportPng => KeyBinding::new(k, ExportPng, None),
            FileCommand::Quit => KeyBinding::new(k, Quit, None),
        }
    }));
    for cmd in ViewCommand::ALL {
        cx.bind_keys(cmd.keystrokes().iter().map(|&k| match cmd {
            ViewCommand::ZoomIn => KeyBinding::new(k, ZoomIn, None),
            ViewCommand::ZoomOut => KeyBinding::new(k, ZoomOut, None),
            ViewCommand::ActualSize => KeyBinding::new(k, ActualSize, None),
            ViewCommand::Fit => KeyBinding::new(k, FitCanvas, None),
        }));
    }
}

// Theme: dark neutral greys, one blue accent.
pub(crate) const BG_APP: u32 = 0x1e1e1e;
pub(crate) const BG_BAR: u32 = 0x2b2b2b;
pub(crate) const BG_RIBBON: u32 = 0x323232;
pub(crate) const BG_PANEL: u32 = 0x262626;
pub(crate) const BG_PANEL_HEADER: u32 = 0x303030;
pub(crate) const BG_HOVER: u32 = 0x3d3d3d;
pub(crate) const BG_MENU: u32 = 0x2d2d2d;
pub(crate) const ACCENT: u32 = 0x3d7fd9;
pub(crate) const BORDER: u32 = 0x111111;
pub(crate) const TEXT: u32 = 0xdcdcdc;
pub(crate) const TEXT_DIM: u32 = 0x8a8a8a;
/// Canvas-area background while files are dragged over it.
const DROP_HIGHLIGHT: u32 = 0x2a3f5c;

const MENU_BAR_H: f32 = 26.;
const RIBBON_H: f32 = 44.;
const PANEL_W: f32 = 260.;
/// The Layers panel keeps at least this height; Filter and Swatches give way.
const LAYERS_MIN_H: f32 = 260.;
/// Filter panel height when the window has room for it.
const FILTER_H: f32 = 340.;

/// Runs `f` on the shared state and notifies its observers (every view that
/// shows it). The single path for mutating [`EditorState`] from a view.
pub(crate) fn edit_state<T, R>(
    state: &Entity<EditorState>,
    cx: &mut Context<T>,
    f: impl FnOnce(&mut EditorState) -> R,
) -> R {
    state.update(cx, |s, cx| {
        let r = f(s);
        cx.notify();
        r
    })
}

pub(crate) struct EditorView {
    /// Shared with the panel views.
    pub(crate) state: Entity<EditorState>,
    layers_panel: Entity<LayersPanel>,
    filter_panel: Entity<FilterPanel>,
    focus: FocusHandle,
    worker: WorkerClient,
    /// What the render worker was last given: document version and overlay.
    sent: Option<(u64, Overlay)>,
    canvas_cache: CanvasCache,
    /// Window position of the canvas area's top-left corner (last layout).
    canvas_origin: Point<Pixels>,
}

impl EditorView {
    pub(crate) fn new(
        worker: WorkerClient,
        canvas: CanvasReceiver,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let state = cx.new(|_| EditorState::new());
        cx.observe(&state, |this, _, cx| {
            this.sync_worker(cx);
            cx.notify();
        })
        .detach();
        let layers_panel = cx.new(|cx| LayersPanel::new(state.clone(), window, cx));
        let filter_panel = cx.new(|cx| FilterPanel::new(state.clone(), cx));
        let mut this = Self {
            state,
            layers_panel,
            filter_panel,
            focus,
            worker,
            sent: None,
            canvas_cache: CanvasCache::default(),
            canvas_origin: point(px(0.), px(0.)),
        };
        this.sync_worker(cx);
        this.receive_canvas(canvas, cx);
        this
    }

    /// Takes each flattened document the render worker publishes and
    /// repaints. Runs as a GPUI task: awaiting never blocks the UI thread,
    /// and the canvas keeps showing the previous image meanwhile.
    fn receive_canvas(&self, canvas: CanvasReceiver, cx: &mut Context<Self>) {
        let timing = std::env::var_os(TIMING_ENV).is_some();
        cx.spawn(async move |this, cx| {
            while let Some(frame) = canvas.next().await {
                let requested = frame.requested;
                let alive = this.update(cx, |v, cx| {
                    let t = std::time::Instant::now();
                    if v.canvas_cache.accept(frame) {
                        cx.notify();
                    }
                    if timing {
                        eprintln!(
                            "editor: canvas image shown {:.2?} after doc change \
                             (UI thread {:.2?})",
                            requested.elapsed(),
                            t.elapsed()
                        );
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Hands the render worker whatever changed since the last call: a
    /// document snapshot when [`EditorState::doc_version`] moved (re-dither),
    /// else just the overlay when cursor, tool or colors moved. Never blocks.
    fn sync_worker(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let overlay = Overlay::from_state(state);
        match next_request(self.sent, state.doc_version, &overlay) {
            Request::Render => self.worker.render(RenderJob::from_state(state), overlay),
            Request::Overlay => self.worker.overlay(overlay),
            Request::Nothing => return,
        }
        self.sent = Some((state.doc_version, overlay));
    }

    /// Window position → canvas-area (viewport screen) coordinates.
    fn local(&self, p: Point<Pixels>) -> Vec2 {
        Vec2::new(
            f32::from(p.x - self.canvas_origin.x),
            f32::from(p.y - self.canvas_origin.y),
        )
    }

    /// Runs `f` on the shared state and redraws every view that shows it.
    fn edit<R>(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut EditorState) -> R) -> R {
        edit_state(&self.state, cx, f)
    }

    fn run(&mut self, cmd: FileCommand, _window: &mut Window, cx: &mut Context<Self>) {
        match self.edit(cx, |s| s.dispatch(cmd)) {
            Outcome::Quit => cx.quit(),
            Outcome::PickImages => prompt_insert_image(&self.state, cx),
            Outcome::Done | Outcome::NotImplemented(_) => {}
        }
    }

    // One handler per action; file-ops replaces the stubs behind them.
    fn on_new(&mut self, _: &NewFile, w: &mut Window, cx: &mut Context<Self>) {
        self.run(FileCommand::New, w, cx);
    }
    fn on_open(&mut self, _: &OpenFile, w: &mut Window, cx: &mut Context<Self>) {
        self.run(FileCommand::Open, w, cx);
    }
    fn on_insert_image(&mut self, _: &InsertImage, w: &mut Window, cx: &mut Context<Self>) {
        self.run(FileCommand::InsertImage, w, cx);
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
        let open = self.state.read(cx).file_menu_open;
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
                        this.edit(cx, |s| s.toggle_file_menu());
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
        let brush_controls = self.brush_controls(cx);
        let state = self.state.read(cx);
        let doc = &state.document;
        let mut tools = div().flex().flex_row().gap_1();
        for tool in Tool::ALL {
            tools = tools.child(tool_button(tool, state.tool == tool).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.edit(cx, |s| s.select_tool(tool));
                },
            )));
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
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_4()
                    .child(tools)
                    .child(brush_controls),
            )
            .child(
                div()
                    .relative()
                    .w(px(34.))
                    .h(px(34.))
                    .child(color_chip(doc.bg).absolute().right_0().bottom_0())
                    .child(color_chip(doc.fg).absolute().left_0().top_0()),
            )
    }

    /// Brush size stepper (`[` / `]`) and the Hard/Soft tip toggle.
    fn brush_controls(&self, cx: &mut Context<Self>) -> Div {
        let brush = &self.state.read(cx).brush;
        let (size, tip) = (brush.size(), brush.tip());
        let mut tips = div().flex().flex_row().gap_1();
        for t in [BrushTip::Hard, BrushTip::Soft] {
            tips = tips.child(
                ribbon_button(
                    SharedString::from(format!("tip-{t:?}")),
                    t.label(),
                    tip == t,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.edit(cx, |s| s.set_brush_tip(t));
                })),
            );
        }
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .text_sm()
            .child(div().text_color(rgb(TEXT_DIM)).child("Size"))
            .child(
                ribbon_button("size-down".into(), "−", false)
                    .when(size <= MIN_BRUSH_SIZE, |d| d.text_color(rgb(TEXT_DIM)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.edit(cx, |s| s.brush_size_down());
                    })),
            )
            .child(
                div()
                    .id("brush-size")
                    .w(px(52.))
                    .flex()
                    .justify_center()
                    .child(format!("{size} px")),
            )
            .child(
                ribbon_button("size-up".into(), "+", false)
                    .when(size >= MAX_BRUSH_SIZE, |d| d.text_color(rgb(TEXT_DIM)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.edit(cx, |s| s.brush_size_up());
                    })),
            )
            .child(tips)
    }

    /// The canvas area: draws the flattened document and turns pointer input
    /// into viewport changes. Layout happens in the `canvas` element's
    /// prepaint, which also records the area's size and origin.
    fn canvas_area(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let this = cx.entity();
        let surface = canvas(
            move |bounds, window, cx| {
                let scale = window.scale_factor();
                let (plan, zoom_changed) = this.update(cx, |v, cx| {
                    v.canvas_origin = bounds.origin;
                    let cache = &mut v.canvas_cache;
                    // No notify: this runs during layout, and only the
                    // viewport (not the document) can change here.
                    v.state.update(cx, |s, _| {
                        let before = s.viewport.zoom();
                        s.set_view_size(
                            f32::from(bounds.size.width),
                            f32::from(bounds.size.height),
                        );
                        let mut plan = cache.prepare(&s.viewport, bounds, scale);
                        plan.handles = s.transform_handles();
                        (plan, s.viewport.zoom() != before)
                    })
                });
                if zoom_changed {
                    // The status bar was already laid out with the old zoom.
                    let this = this.clone();
                    window.on_next_frame(move |_, cx| this.update(cx, |_, cx| cx.notify()));
                }
                plan
            },
            |_, plan, window, _| canvas_view::paint(plan, window),
        )
        .size_full();

        div()
            .id("canvas-area")
            .flex_1()
            .h_full()
            .overflow_hidden()
            .bg(rgb(canvas_view::SURROUND))
            .child(surface)
            // Files dropped from a file manager become image layers.
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(rgb(DROP_HIGHLIGHT)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                insert_image_files(&this.state, paths.paths().to_vec(), cx);
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                let p = this.local(ev.position);
                let pick = match ev.pressed_button {
                    Some(MouseButton::Left) => Some(PaintButton::Primary),
                    Some(MouseButton::Right) => Some(PaintButton::Secondary),
                    _ => None,
                };
                let frame = this.canvas_cache.frame_view();
                let shift = ev.modifiers.shift;
                this.state.update(cx, |s, cx| {
                    s.shift_held = shift;
                    s.pointer_moved(p);
                    if let (Some(button), Some(c), Tool::Picker) = (pick, s.cursor_canvas, s.tool)
                        && s.pan_drag.is_none()
                    {
                        s.pick_color(frame, c, button);
                    }
                    cx.notify();
                });
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered && this.state.read(cx).pan_drag.is_none() {
                    this.edit(cx, |s| s.pointer_left());
                }
            }))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                    let p = this.local(ev.position);
                    this.edit(cx, |s| s.begin_pan(p));
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                    let p = this.local(ev.position);
                    let shift = ev.modifiers.shift;
                    this.state.update(cx, |s, _| s.shift_held = shift);
                    if this.state.read(cx).space_held {
                        this.edit(cx, |s| s.begin_pan(p));
                    } else {
                        this.press(p, PaintButton::Primary, cx);
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                    let p = this.local(ev.position);
                    this.press(p, PaintButton::Secondary, cx);
                }),
            )
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_pan_end))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_pan_end))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::on_pan_end))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_pan_end))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_paint_end))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_paint_end))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_paint_end))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::on_paint_end))
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                let (d, lines) = match ev.delta {
                    ScrollDelta::Pixels(p) => ((f32::from(p.x), f32::from(p.y)), false),
                    ScrollDelta::Lines(p) => ((p.x, p.y), true),
                };
                let p = this.local(ev.position);
                let ctrl = ev.modifiers.control;
                this.edit(cx, |s| s.wheel(p, d.0, d.1, lines, ctrl));
            }))
    }

    fn view_cmd(&mut self, cmd: ViewCommand, cx: &mut Context<Self>) {
        self.edit(cx, |s| s.view_command(cmd));
    }
    fn on_zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.view_cmd(ViewCommand::ZoomIn, cx);
    }
    fn on_zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.view_cmd(ViewCommand::ZoomOut, cx);
    }
    fn on_actual_size(&mut self, _: &ActualSize, _: &mut Window, cx: &mut Context<Self>) {
        self.view_cmd(ViewCommand::ActualSize, cx);
    }
    fn on_fit(&mut self, _: &FitCanvas, _: &mut Window, cx: &mut Context<Self>) {
        self.view_cmd(ViewCommand::Fit, cx);
    }

    /// A paint button went down at canvas-area point `p`: start a stroke
    /// (Brush, Eraser), pick a color (Picker) or start a move / transform
    /// drag (Move, Select; left button only).
    fn press(&mut self, p: Vec2, button: PaintButton, cx: &mut Context<Self>) {
        let frame = self.canvas_cache.frame_view();
        self.state.update(cx, |s, cx| {
            s.pointer_moved(p);
            let Some(c) = s.cursor_canvas else { return };
            if s.tool_paints() {
                s.begin_stroke(c, button);
            } else if s.tool == Tool::Picker {
                s.pick_color(frame, c, button);
            } else if s.tool_transforms() && button == PaintButton::Primary {
                s.begin_transform(p);
            }
            cx.notify();
        });
    }

    fn on_paint_end(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let s = self.state.read(cx);
        if s.stroke.is_some() {
            self.edit(cx, |s| s.end_stroke());
        } else if s.transform_drag.is_some() {
            self.edit(cx, |s| s.end_transform());
        }
    }

    fn on_pan_end(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).pan_drag.is_some() {
            self.edit(cx, |s| s.end_pan());
        }
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let m = &k.modifiers;
        let plain = !(m.control || m.alt || m.platform || m.function);
        let s = self.state.read(cx);
        if plain
            && s.tool_transforms()
            && s.renaming.is_none()
            && s.transform_drag.is_none()
            && let Some((dx, dy)) = arrow_nudge(&k.key, m.shift)
        {
            self.edit(cx, |s| s.nudge_selected(dx, dy));
            return;
        }
        match k.key.as_str() {
            "space" if !self.state.read(cx).space_held => {
                self.edit(cx, |s| s.space_held = true);
            }
            "[" if plain => self.edit(cx, |s| s.brush_size_down()),
            "]" if plain => self.edit(cx, |s| s.brush_size_up()),
            _ => {}
        }
    }

    fn on_key_up(&mut self, ev: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if ev.keystroke.key == "space" {
            self.edit(cx, |s| s.space_held = false);
        }
    }

    fn panels(&self) -> Div {
        div()
            .w(px(PANEL_W))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(BG_PANEL))
            .border_l_1()
            .border_color(rgb(BORDER))
            .overflow_hidden()
            // In short windows Filter (which scrolls) and Swatches shrink.
            .child(
                div()
                    .flex_1()
                    .min_h(px(LAYERS_MIN_H))
                    .overflow_hidden()
                    .child(self.layers_panel.clone()),
            )
            .child(
                div()
                    .h(px(FILTER_H))
                    .min_h(px(120.))
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .overflow_hidden()
                    .child(self.filter_panel.clone()),
            )
            .child(
                panel("Swatches", "No swatches".into())
                    .h(px(110.))
                    .min_h(px(40.))
                    .overflow_hidden(),
            )
    }

    fn status_bar(&self, cx: &App) -> Div {
        let state = self.state.read(cx);
        let doc = &state.document;
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
            .child(tool_label(state.tool))
            .child(format!(
                "{}×{} → {}×{}",
                doc.canvas_size.width,
                doc.canvas_size.height,
                doc.output_size.width,
                doc.output_size.height
            ))
            .child(format!("Zoom {}", state.zoom_label()))
            .child(format!("Cursor {}", state.cursor_label()))
            .child(state.status.clone())
    }
}

fn tool_button(tool: Tool, selected: bool) -> Stateful<Div> {
    ribbon_button(
        SharedString::from(format!("tool-{tool:?}")),
        tool_label(tool),
        selected,
    )
}

fn ribbon_button(id: SharedString, label: &'static str, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
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
        .child(label)
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

/// Panel title strip.
pub(crate) fn panel_header(title: &'static str) -> Div {
    div()
        .px_2()
        .py_1()
        .bg(rgb(BG_PANEL_HEADER))
        .text_xs()
        .text_color(rgb(TEXT))
        .child(title)
}

fn panel(title: &'static str, placeholder: String) -> Div {
    div()
        .flex()
        .flex_col()
        .border_b_1()
        .border_color(rgb(BORDER))
        .child(panel_header(title))
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
        let menu_open = self.state.read(cx).file_menu_open;
        div()
            .id("editor")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_new))
            .on_action(cx.listener(Self::on_open))
            .on_action(cx.listener(Self::on_insert_image))
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_save_as))
            .on_action(cx.listener(Self::on_export_png))
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
            .on_action(cx.listener(Self::on_actual_size))
            .on_action(cx.listener(Self::on_fit))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
            .on_modifiers_changed(cx.listener(|this, ev: &ModifiersChangedEvent, _, cx| {
                let shift = ev.modifiers.shift;
                if shift != this.state.read(cx).shift_held {
                    this.edit(cx, |s| s.set_shift(shift));
                }
            }))
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
                    .child(self.canvas_area(cx))
                    .child(self.panels()),
            )
            .child(self.status_bar(cx))
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
                                this.edit(cx, |s| s.close_menus());
                            }),
                        ),
                )
                .child(self.file_menu(cx))
            })
    }
}
