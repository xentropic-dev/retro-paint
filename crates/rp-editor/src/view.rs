//! The GPUI editor window: menu bar, tool ribbon, canvas area, right panels,
//! status bar. Thin: every decision goes through [`EditorState`].

use gpui::{
    AnyView, App, Context, Div, Entity, ExternalPaths, FocusHandle, KeyBinding, KeyDownEvent,
    KeyUpEvent, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, Rgba, ScrollDelta, ScrollWheelEvent, SharedString, Stateful, Window, actions,
    canvas, div, img, point, prelude::*, px, rgb, svg,
};
use rp_doc::Tool;
use rp_palette_dither::Rgb;

use crate::brush_flow::restore_saved_brushes;
use crate::brushes_panel::{BrushesPanel, fit};
use crate::canvas_view::{self, CanvasCache};
use crate::color_picker::ColorPickerDialog;
use crate::color_picker_ops::PickerTarget;
use crate::file_dialog::FileDialog;
use crate::file_flow::handle_outcome;
use crate::filter_panel::FilterPanel;
use crate::insert_flow::{insert_image_files, paste_from_clipboard};
use crate::layers_panel::LayersPanel;
use crate::paint_ops::{BrushTip, MIN_BRUSH_SIZE, PaintButton};
use crate::panel_layout::{PanelSection, PanelSections};
use crate::project_dialog::ProjectDialog;
use crate::render_worker::{
    CanvasReceiver, Overlay, RenderJob, Request, TIMING_ENV, WorkerClient, next_request,
};
use crate::ribbon_icons::{Icon, tool_key, tool_tooltip, tooltip_text};
use crate::state::{EditorState, FileCommand, ViewCommand, shortcut_label, tool_label};
use crate::swatch_ops::color_key;
use crate::swatches_panel::SwatchesPanel;
use crate::transform_ops::arrow_nudge;
use crate::undo_ops::EditCommand;
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
        ProjectSettings,
        Quit,
        ZoomIn,
        ZoomOut,
        ActualSize,
        FitCanvas,
        Undo,
        Redo,
        Paste,
        DuplicateLayer,
        SelectAll,
        Deselect,
        TransformSelection
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
            FileCommand::ProjectSettings => KeyBinding::new(k, ProjectSettings, None),
            FileCommand::Quit => KeyBinding::new(k, Quit, None),
        }
    }));
    for cmd in EditCommand::ALL {
        cx.bind_keys(cmd.keystrokes().iter().map(|&k| match cmd {
            EditCommand::Undo => KeyBinding::new(k, Undo, None),
            EditCommand::Redo => KeyBinding::new(k, Redo, None),
            EditCommand::Paste => KeyBinding::new(k, Paste, None),
            EditCommand::DuplicateLayer => KeyBinding::new(k, DuplicateLayer, None),
            EditCommand::SelectAll => KeyBinding::new(k, SelectAll, None),
            EditCommand::Deselect => KeyBinding::new(k, Deselect, None),
            EditCommand::TransformSelection => KeyBinding::new(k, TransformSelection, None),
            EditCommand::FlipHorizontal
            | EditCommand::FlipVertical
            | EditCommand::RasterizeLayer
            | EditCommand::Clear => {
                unreachable!("flips, rasterize and clear have no key binding")
            }
        }));
    }
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
/// Width of a top-level menu-bar button (File, Edit).
const MENU_BUTTON_W: f32 = 48.;
const RIBBON_H: f32 = 44.;
/// Hit size of a ribbon icon button (square).
const ICON_BUTTON_PX: f32 = 28.;
/// Drawn size of a ribbon icon: one logical pixel per icon grid unit.
const ICON_PX: f32 = crate::ribbon_icons::ICON_GRID as f32;
/// Width of the right-hand panel column. Its sections size to their content
/// and the column scrolls as a whole when the window is too short.
const PANEL_W: f32 = 260.;
/// Height of a collapsible section header in the panel column.
const SECTION_HEADER_H: f32 = 24.;

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
    swatches_panel: Entity<SwatchesPanel>,
    /// Also caches the brush thumbnail images the ribbon shows.
    brushes_panel: Entity<BrushesPanel>,
    project_dialog: Entity<ProjectDialog>,
    color_picker: Entity<ColorPickerDialog>,
    file_dialog: Entity<FileDialog>,
    /// Last title given to the window (file name, • while dirty).
    title: String,
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
        let project_dialog = cx.new(|cx| ProjectDialog::new(state.clone(), focus.clone(), cx));
        let color_picker = cx.new(|cx| ColorPickerDialog::new(state.clone(), focus.clone(), cx));
        let swatches_panel =
            cx.new(|cx| SwatchesPanel::new(state.clone(), color_picker.clone(), cx));
        let file_dialog = cx.new(|cx| FileDialog::new(state.clone(), focus.clone(), cx));
        let brushes_panel = cx.new(|cx| BrushesPanel::new(state.clone(), cx));
        restore_saved_brushes(&state, cx);
        // The window's close button asks about unsaved changes like Quit.
        let (close_state, close_dialog) = (state.clone(), file_dialog.clone());
        window.on_window_should_close(cx, move |window, cx| {
            let close = close_state.update(cx, |s, cx| {
                cx.notify();
                s.request_close()
            });
            if !close {
                let focus = close_dialog.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
            }
            close
        });
        let mut this = Self {
            state,
            layers_panel,
            filter_panel,
            swatches_panel,
            brushes_panel,
            project_dialog,
            color_picker,
            file_dialog,
            title: String::new(),
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

    fn run(&mut self, cmd: FileCommand, window: &mut Window, cx: &mut Context<Self>) {
        let out = self.edit(cx, |s| s.dispatch(cmd));
        handle_outcome(&self.state, out, cx);
        if self.state.read(cx).file_modal_open() {
            let focus = self.file_dialog.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        } else if self.state.read(cx).project_dialog.is_some() {
            self.project_dialog.update(cx, |d, cx| d.focus(window, cx));
        } else if !self.focus.contains_focused(window, cx) {
            // New closes the dialog; take focus back from it.
            window.focus(&self.focus, cx);
        }
    }

    // One handler per action.
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
    fn on_project_settings(&mut self, _: &ProjectSettings, w: &mut Window, cx: &mut Context<Self>) {
        self.run(FileCommand::ProjectSettings, w, cx);
    }
    fn on_quit(&mut self, _: &Quit, w: &mut Window, cx: &mut Context<Self>) {
        self.run(FileCommand::Quit, w, cx);
    }
    fn on_undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |s| s.edit_command(EditCommand::Undo));
    }
    fn on_redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |s| s.edit_command(EditCommand::Redo));
    }
    fn on_paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        paste_from_clipboard(&self.state, cx);
    }
    fn on_duplicate_layer(&mut self, _: &DuplicateLayer, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |s| s.edit_command(EditCommand::DuplicateLayer));
    }
    fn on_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).renaming.is_none() {
            self.edit(cx, |s| s.edit_command(EditCommand::SelectAll));
        }
    }
    fn on_deselect(&mut self, _: &Deselect, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |s| s.edit_command(EditCommand::Deselect));
    }
    fn on_transform_selection(
        &mut self,
        _: &TransformSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.read(cx).renaming.is_none() {
            self.edit(cx, |s| s.edit_command(EditCommand::TransformSelection));
        }
    }

    fn menu_bar(&self, cx: &mut Context<Self>) -> Div {
        let state = self.state.read(cx);
        let (file_open, edit_open) = (state.file_menu_open, state.edit_menu_open);
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
                menu_button("menu-file", "File", file_open).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.edit(cx, |s| s.toggle_file_menu());
                    },
                )),
            )
            .child(
                menu_button("menu-edit", "Edit", edit_open).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.edit(cx, |s| s.toggle_edit_menu());
                    },
                )),
            )
    }

    fn edit_menu(&self, cx: &mut Context<Self>) -> Div {
        let history = &self.state.read(cx).history;
        let mut menu = dropdown(4. + MENU_BUTTON_W);
        for cmd in EditCommand::ALL {
            let enabled = cmd.enabled(history);
            let item = div()
                .id(SharedString::from(format!("edit-{cmd:?}")))
                .flex()
                .flex_row()
                .justify_between()
                .px_3()
                .py_1()
                .child(cmd.menu_label(history))
                .child(
                    div()
                        .text_color(rgb(TEXT_DIM))
                        .child(cmd.shortcut_hint().unwrap_or_default()),
                );
            menu = menu.child(if enabled {
                item.cursor_pointer()
                    .hover(|s| s.bg(rgb(ACCENT)))
                    .on_click(cx.listener(move |this, _, _, cx| match cmd {
                        // The clipboard read needs GPUI; it starts the paste.
                        EditCommand::Paste => paste_from_clipboard(&this.state, cx),
                        _ => {
                            this.edit(cx, |s| s.edit_command(cmd));
                        }
                    }))
            } else {
                item.text_color(rgb(TEXT_DIM))
            });
            if cmd.separator_after() {
                menu = menu.child(div().my_1().h(px(1.)).bg(rgb(0x444444)));
            }
        }
        menu
    }

    fn file_menu(&self, cx: &mut Context<Self>) -> Div {
        let mut menu = dropdown(4.);
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
        // Photobash input next to the tools: the same portal dialog as
        // File > Insert Image… (Ctrl+V pastes).
        let insert_tip = tooltip_text(
            "Insert Image",
            Some(&shortcut_label(FileCommand::InsertImage.keystroke())),
        );
        tools = tools.child(div().ml_2().child(
            icon_button("insert-image", Icon::InsertImage, insert_tip, false, true).on_click(
                cx.listener(|this, _, window, cx| {
                    this.run(FileCommand::InsertImage, window, cx);
                }),
            ),
        ));
        // Wraps onto a second line instead of clipping in narrow windows.
        div()
            .flex()
            .flex_row()
            .flex_none()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_x_4()
            .gap_y_1()
            .min_h(px(RIBBON_H))
            .px_2()
            .py_1()
            .bg(rgb(BG_RIBBON))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_x_4()
                    .gap_y_1()
                    .child(tools)
                    .child(brush_controls),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .relative()
                            .w(px(34.))
                            .h(px(34.))
                            .child(
                                color_chip("bg-chip", doc.bg)
                                    .absolute()
                                    .right_0()
                                    .bottom_0()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_picker(PickerTarget::Background, window, cx);
                                    })),
                            )
                            .child(
                                color_chip("fg-chip", doc.fg)
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_picker(PickerTarget::Foreground, window, cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_1()
                            .child(
                                icon_button(
                                    "swap-colors",
                                    Icon::SwapColors,
                                    tooltip_text("Swap colors", Some("X")),
                                    false,
                                    true,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.edit(cx, |s| s.swap_colors());
                                    },
                                )),
                            )
                            .child(
                                icon_button(
                                    "reset-colors",
                                    Icon::ResetColors,
                                    tooltip_text("Reset colors", Some("D")),
                                    false,
                                    true,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.edit(cx, |s| s.reset_colors());
                                    },
                                )),
                            ),
                    ),
            )
    }

    /// Opens the color picker on `target` and gives it the keyboard.
    fn open_picker(&mut self, target: PickerTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |s| s.open_color_picker(target));
        if self.state.read(cx).color_picker.is_some() {
            self.color_picker.update(cx, |d, cx| d.focus(window, cx));
        }
    }

    /// The current brush's thumbnail (name in its tooltip), the size
    /// stepper (`[` / `]`) and the Hard/Soft tip toggle (round brushes only).
    fn brush_controls(&self, cx: &mut Context<Self>) -> Div {
        let s = self.state.read(cx);
        let brush = &s.brush;
        let (size, tip, round, max) = (
            brush.size(),
            brush.tip(),
            brush.is_round(),
            brush.max_size(),
        );
        let name = brush.name().to_string();
        let key = s.brushes.selected().clone();
        let thumb = s.brushes.entry(&key).and_then(|e| e.thumb.clone());
        let thumb = thumb.map(|mask| {
            let image = self.brushes_panel.update(cx, |p, _| p.image(&key, &mask));
            let (w, h) = fit(mask.width, mask.height, ICON_BUTTON_PX - 6.);
            img(image).w(px(w)).h(px(h))
        });
        let current = div()
            .id("brush-current")
            .w(px(ICON_BUTTON_PX))
            .h(px(ICON_BUTTON_PX))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded_sm()
            .border_1()
            .border_color(rgb(0x404040))
            .bg(rgb(0x1a1a1a))
            .tooltip(text_tooltip(format!("Brush: {name}")))
            .children(thumb)
            .on_click(cx.listener(|this, _, _, cx| {
                // Opens the Brushes section if it was collapsed.
                this.edit(cx, |s| s.panels.set_collapsed(PanelSection::Brushes, false));
            }));
        let mut tips = div().flex().flex_row().gap_1();
        for t in [BrushTip::Hard, BrushTip::Soft] {
            let icon = match t {
                BrushTip::Hard => Icon::TipHard,
                BrushTip::Soft => Icon::TipSoft,
            };
            tips = tips.child(
                icon_button(
                    SharedString::from(format!("tip-{t:?}")),
                    icon,
                    tooltip_text(&format!("{} brush", t.label()), None),
                    round && tip == t,
                    round,
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
            .child(current)
            .child(div().text_color(rgb(TEXT_DIM)).child("Size"))
            .child(
                icon_button(
                    "size-down",
                    Icon::SizeDown,
                    tooltip_text("Smaller brush", Some("[")),
                    false,
                    size > MIN_BRUSH_SIZE,
                )
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
                icon_button(
                    "size-up",
                    Icon::SizeUp,
                    tooltip_text("Larger brush", Some("]")),
                    false,
                    size < max,
                )
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
                        plan.selection = s.selection_overlay();
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
                        let sel = SelectPress {
                            shift,
                            alt: ev.modifiers.alt,
                            clicks: ev.click_count,
                        };
                        this.press(p, PaintButton::Primary, sel, cx);
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                    let p = this.local(ev.position);
                    this.press(p, PaintButton::Secondary, SelectPress::default(), cx);
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
    /// drag (Move, Select; left button only), or a selection outline
    /// (Lasso, Rect Select; left button only, `sel` holds its modifiers).
    fn press(&mut self, p: Vec2, button: PaintButton, sel: SelectPress, cx: &mut Context<Self>) {
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
            } else if s.tool_selects() && button == PaintButton::Primary {
                s.begin_select(p, sel.shift, sel.alt, sel.clicks);
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
        } else if s.select_dragging() {
            self.edit(cx, |s| s.end_select_drag());
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
            key if s.renaming.is_none()
                && let Some(c) = color_key(key, plain && !m.shift) =>
            {
                self.edit(cx, |s| s.color_key(c));
            }
            "]" if plain => self.edit(cx, |s| s.brush_size_up()),
            "enter" if s.renaming.is_none() && s.is_floating() => {
                self.edit(cx, |s| s.commit_float());
            }
            "enter" if s.renaming.is_none() && s.selection.draft.is_some() => {
                self.edit(cx, |s| s.finish_lasso());
            }
            "escape" if plain => {
                self.edit(cx, |s| s.escape());
            }
            "delete" | "backspace" if plain && s.renaming.is_none() => {
                self.edit(cx, |s| s.edit_command(EditCommand::Clear));
            }
            key if s.renaming.is_none()
                && let Some(t) = tool_key(key, plain && !m.shift) =>
            {
                self.edit(cx, |s| s.select_tool(t));
            }
            _ => {}
        }
    }

    fn on_key_up(&mut self, ev: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if ev.keystroke.key == "space" {
            self.edit(cx, |s| s.space_held = false);
        }
    }

    /// The right-hand column: Layers, Filter and Swatches under collapsible
    /// headers. Sections take their natural height; the column scrolls.
    fn panels(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let sections = self.state.read(cx).panels;
        let mut column = div()
            .id("panel-column")
            .w(px(PANEL_W))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(BG_PANEL))
            .border_l_1()
            .border_color(rgb(BORDER))
            .overflow_y_scroll();
        for section in PanelSection::ALL {
            column = column.child(section_header(section, &sections).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.edit(cx, |s| s.toggle_panel(section));
                },
            )));
            if sections.is_collapsed(section) {
                continue;
            }
            let body = div()
                .flex_none()
                .w_full()
                .border_b_1()
                .border_color(rgb(BORDER));
            column = column.child(match section {
                PanelSection::Layers => body.child(self.layers_panel.clone()),
                PanelSection::Filter => body.child(self.filter_panel.clone()),
                PanelSection::Swatches => body.child(self.swatches_panel.clone()),
                PanelSection::Brushes => body.child(self.brushes_panel.clone()),
            });
        }
        column
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
            .child(div().text_color(rgb(TEXT)).child(format!(
                "{}{}",
                state.file_name(),
                if state.is_dirty() { " •" } else { "" }
            )))
            .child(tool_label(state.tool))
            .children(
                state
                    .edit_target_label()
                    .map(|l| div().text_color(rgb(0xffffff)).child(l)),
            )
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

/// Modifiers and click count of a left press, for the selection tools.
#[derive(Clone, Copy, Default)]
struct SelectPress {
    shift: bool,
    alt: bool,
    clicks: usize,
}

/// A top-level menu-bar button.
fn menu_button(id: &'static str, label: &'static str, open: bool) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(MENU_BUTTON_W))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(open, |d| d.bg(rgb(BG_HOVER)))
        .hover(|s| s.bg(rgb(BG_HOVER)))
        .child(label)
}

/// An empty menu dropdown under the menu bar, `left` px from the window edge.
fn dropdown(left: f32) -> Div {
    div()
        .absolute()
        .top(px(MENU_BAR_H))
        .left(px(left))
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
        .occlude()
}

fn tool_button(tool: Tool, selected: bool) -> Stateful<Div> {
    icon_button(
        SharedString::from(format!("tool-{tool:?}")),
        Icon::for_tool(tool),
        tool_tooltip(tool),
        selected,
        true,
    )
}

/// A square ribbon button showing `icon`, with a hover tooltip. The icon is
/// tinted with the text color: white on the accent fill when `selected`,
/// dimmed when not `enabled` (still clickable; the state refuses). Every
/// ribbon tool and command button uses this.
pub(crate) fn icon_button(
    id: impl Into<SharedString>,
    icon: Icon,
    tooltip: impl Into<SharedString>,
    selected: bool,
    enabled: bool,
) -> Stateful<Div> {
    let tooltip: SharedString = tooltip.into();
    let tint = if selected {
        0xffffff
    } else if enabled {
        TEXT
    } else {
        TEXT_DIM
    };
    div()
        .id(id.into())
        .w(px(ICON_BUTTON_PX))
        .h(px(ICON_BUTTON_PX))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .border_1()
        .when(selected, |d| d.bg(rgb(ACCENT)).border_color(rgb(0x5a96e8)))
        .when(!selected, |d| {
            d.border_color(rgb(0x404040)).hover(|s| s.bg(rgb(BG_HOVER)))
        })
        .tooltip(text_tooltip(tooltip))
        .child(
            svg()
                .path(icon.path())
                .flex_none()
                .size(px(ICON_PX))
                .text_color(rgb(tint)),
        )
}

/// A tooltip builder showing `text` in a [`RibbonTooltip`] bubble.
pub(crate) fn text_tooltip(
    text: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text: SharedString = text.into();
    move |_, cx| cx.new(|_| RibbonTooltip(text.clone())).into()
}

/// Tooltip bubble for [`icon_button`] and the brush thumbnails.
struct RibbonTooltip(SharedString);

impl Render for RibbonTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .bg(rgb(BG_MENU))
            .border_1()
            .border_color(rgb(0x404040))
            .rounded_sm()
            .shadow_lg()
            .text_xs()
            .text_color(rgb(TEXT))
            .child(self.0.clone())
    }
}

/// A full-color [`Rgb`] as a GPUI color.
pub(crate) fn rgba(c: Rgb) -> Rgba {
    Rgba {
        r: c.r as f32 / 255.,
        g: c.g as f32 / 255.,
        b: c.b as f32 / 255.,
        a: 1.,
    }
}

/// A clickable fg/bg chip (opens the color picker).
fn color_chip(id: &'static str, c: Rgb) -> Stateful<Div> {
    div()
        .id(id)
        .cursor_pointer()
        // The fg chip overlaps the bg chip; only the top one gets the click.
        .occlude()
        .w(px(22.))
        .h(px(22.))
        .bg(rgba(c))
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

/// Clickable header of a panel-column section: disclosure glyph and title.
fn section_header(section: PanelSection, sections: &PanelSections) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("section-{section:?}")))
        .flex_none()
        .h(px(SECTION_HEADER_H))
        .px_2()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .bg(rgb(BG_PANEL_HEADER))
        .border_b_1()
        .border_color(rgb(BORDER))
        .text_xs()
        .text_color(rgb(TEXT))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(BG_HOVER)))
        .child(
            div()
                .w(px(10.))
                .text_size(px(9.))
                .text_color(rgb(TEXT_DIM))
                .child(sections.glyph(section)),
        )
        .child(section.title())
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let menu_open = self.state.read(cx).menu_open();
        let file_menu_open = self.state.read(cx).file_menu_open;
        let dialog_open = self.state.read(cx).project_dialog.is_some();
        let file_modal = self.state.read(cx).file_modal_open();
        let picker_open = self.state.read(cx).color_picker.is_some();
        let title = self.state.read(cx).window_title();
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        div()
            .id("editor")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_new))
            .on_action(cx.listener(Self::on_open))
            .on_action(cx.listener(Self::on_insert_image))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_save_as))
            .on_action(cx.listener(Self::on_export_png))
            .on_action(cx.listener(Self::on_project_settings))
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
            .on_action(cx.listener(Self::on_actual_size))
            .on_action(cx.listener(Self::on_fit))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            .on_action(cx.listener(Self::on_duplicate_layer))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::on_deselect))
            .on_action(cx.listener(Self::on_transform_selection))
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
                    .child(self.panels(cx)),
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
                .child(if file_menu_open {
                    self.file_menu(cx)
                } else {
                    self.edit_menu(cx)
                })
            })
            .when(dialog_open, |root| {
                // Modal: the backdrop swallows clicks meant for the editor.
                root.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(gpui::rgba(0x00000099))
                        .occlude()
                        .child(self.project_dialog.clone()),
                )
            })
            .when(picker_open, |root| {
                // A popover under the ribbon's color chips. The backdrop
                // keeps it modal; a click outside cancels it.
                root.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .bg(gpui::rgba(0x00000040))
                        .occlude()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                this.edit(cx, |s| s.cancel_color_picker());
                                window.focus(&this.focus, cx);
                            }),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(MENU_BAR_H + RIBBON_H + 4.))
                        .right(px(8.))
                        .child(self.color_picker.clone()),
                )
            })
            .when(file_modal, |root| {
                root.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(gpui::rgba(0x00000099))
                        .occlude()
                        .child(self.file_dialog.clone()),
                )
            })
    }
}
