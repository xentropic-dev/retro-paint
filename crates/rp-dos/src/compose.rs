//! The DOS screen composer: a pure function from a [`FrameMsg`] to the
//! 320×200 (desktop) or 200×320 (mobile) indexed screen of a little
//! Deluxe Paint-style CGA paint app.
//!
//! Screen parts, top to bottom on mobile:
//! - title bar ("RETRO PAINT") and menu bar (display-only),
//! - the canvas window: the output image at 1:1 with scrollbars, panning to
//!   follow the cursor (see [`ScrollState`]); an image smaller than the window
//!   is centered on a dark gray backdrop,
//! - the tool strip, with the active tool drawn inverted on blue,
//! - the current-color box and the palette strip (the frame's palette), with
//!   the active color framed in white,
//! - a status bar with cursor position, tool, and image size.
//!
//! Desktop puts the tools and palette in a column on the right, Deluxe Paint
//! style, and merges the title into the menu bar.
//!
//! The tool strip shows the five editor tools followed by five decorative
//! ones (fill, line, rectangle, ellipse, text) that never highlight; they
//! make the strip read as a real paint program's toolbox.

use rp_doc::{FrameMsg, IndexedFrame, Layout, Size, Tool};
use rp_palette_dither::CgaColor;

use crate::glyphs::{self, Icon};
use crate::screen::{IndexedScreen, Rect};
use crate::scroll::ScrollState;

/// Tool strip entries in order: the editor tool each icon mirrors (`None` for
/// decorative icons) and its icon.
const TOOL_STRIP: [(Option<Tool>, Icon); 10] = [
    (Some(Tool::Select), glyphs::ICON_SELECT),
    (Some(Tool::Move), glyphs::ICON_MOVE),
    (Some(Tool::Brush), glyphs::ICON_BRUSH),
    (Some(Tool::Eraser), glyphs::ICON_ERASER),
    (Some(Tool::Picker), glyphs::ICON_PICKER),
    (None, glyphs::ICON_FILL),
    (None, glyphs::ICON_LINE),
    (None, glyphs::ICON_RECT),
    (None, glyphs::ICON_ELLIPSE),
    (None, glyphs::ICON_TEXT),
];

const MENU: &str = "FILE  EDIT  BRUSH  VIEW";
const TITLE: &str = "RETRO PAINT";

/// Width of each canvas window scrollbar, including its black separator.
const SCROLLBAR: i32 = 6;

/// Where everything sits on one layout's screen.
#[derive(Clone, Copy, Debug)]
struct ScreenLayout {
    size: Size,
    /// Separate title bar (mobile); desktop shows the title in the menu bar.
    title: Option<Rect>,
    menu: Rect,
    canvas_window: Rect,
    panel: Rect,
    /// Top-left of the tool grid, cell size, and columns.
    tools_origin: (i32, i32),
    tool_cell: (i32, i32),
    tool_cols: usize,
    current_color: Rect,
    swatches: Rect,
    /// Palette grid lanes (rows on mobile, columns on desktop) for big palettes.
    horizontal_palette: bool,
    status: Rect,
}

impl ScreenLayout {
    fn for_layout(layout: Layout) -> Self {
        match layout {
            Layout::Mobile => Self {
                size: layout.dos_screen(),
                title: Some(Rect::new(0, 0, 200, 9)),
                menu: Rect::new(0, 9, 200, 9),
                canvas_window: Rect::new(0, 18, 200, 244),
                panel: Rect::new(0, 262, 200, 49),
                tools_origin: (5, 265),
                tool_cell: (19, 16),
                tool_cols: 10,
                current_color: Rect::new(5, 284, 24, 24),
                swatches: Rect::new(35, 284, 160, 24),
                horizontal_palette: true,
                status: Rect::new(0, 311, 200, 9),
            },
            Layout::Desktop => Self {
                size: layout.dos_screen(),
                title: None,
                menu: Rect::new(0, 0, 320, 9),
                canvas_window: Rect::new(0, 9, 278, 182),
                panel: Rect::new(278, 9, 42, 182),
                tools_origin: (281, 12),
                tool_cell: (18, 16),
                tool_cols: 2,
                current_color: Rect::new(281, 96, 36, 14),
                swatches: Rect::new(281, 114, 36, 72),
                horizontal_palette: false,
                status: Rect::new(0, 191, 320, 9),
            },
        }
    }

    /// Inside of the canvas window border.
    fn canvas_inner(&self) -> Rect {
        self.canvas_window.inset(1)
    }

    /// The part of the canvas window that shows the image.
    fn viewport(&self) -> Rect {
        let i = self.canvas_inner();
        Rect::new(i.x, i.y, i.w - SCROLLBAR, i.h - SCROLLBAR)
    }

    fn tool_rect(&self, i: usize) -> Rect {
        let (cw, ch) = self.tool_cell;
        let (col, row) = ((i % self.tool_cols) as i32, (i / self.tool_cols) as i32);
        Rect::new(
            self.tools_origin.0 + col * cw,
            self.tools_origin.1 + row * ch,
            cw,
            ch,
        )
    }

    /// Swatch rectangles for an `n`-color palette, in palette order.
    fn swatch_rects(&self, n: usize) -> Vec<Rect> {
        let n = n.max(1);
        let (cols, rows) = if self.horizontal_palette {
            if n <= 8 { (n, 1) } else { (n.div_ceil(2), 2) }
        } else {
            let cols = n.min(2);
            (cols, n.div_ceil(cols))
        };
        let a = self.swatches;
        let (cw, ch) = (a.w / cols as i32, a.h / rows as i32);
        let x0 = a.x + (a.w - cw * cols as i32) / 2;
        let y0 = a.y + (a.h - ch * rows as i32) / 2;
        (0..n)
            .map(|i| {
                let (c, r) = ((i % cols) as i32, (i / cols) as i32);
                Rect::new(x0 + c * cw, y0 + r * ch, cw, ch)
            })
            .collect()
    }
}

/// The canvas window's image viewport for `layout`, in screen pixels: the
/// window size that [`ScrollState`] pans over.
pub fn canvas_viewport(layout: Layout) -> Rect {
    ScreenLayout::for_layout(layout).viewport()
}

/// Composes one DOS screen for `layout` (normally `frame.layout`).
///
/// Updates `scroll` so the cursor stays in view; keep the same
/// [`ScrollState`] between frames. Pure: no I/O, and the same inputs always
/// give the same screen.
pub fn compose(frame: &FrameMsg, layout: Layout, scroll: &mut ScrollState) -> IndexedScreen {
    let l = ScreenLayout::for_layout(layout);
    let mut s = IndexedScreen::new(l.size, CgaColor::LightGray);

    draw_bars(&mut s, &l);

    let view = l.viewport();
    scroll.follow(frame.cursor, frame.image.size, (view.w, view.h));
    let origin = image_origin(view, frame.image.size, *scroll);
    draw_canvas_window(&mut s, &l, &frame.image, origin, *scroll);

    draw_panel(&mut s, &l, frame);
    draw_status(&mut s, &l, frame);

    if let Some(c) = frame.cursor {
        let (x, y) = (origin.0 + c.x, origin.1 + c.y);
        draw_cursor(&mut s, x, y, frame.tool, frame.brush_diameter, view);
    }
    s
}

/// Screen position of output pixel (0, 0): centered on axes where the image
/// fits, scrolled otherwise.
fn image_origin(view: Rect, image: Size, scroll: ScrollState) -> (i32, i32) {
    let axis = |v0: i32, vlen: i32, ilen: i32, s: i32| {
        if ilen < vlen {
            v0 + (vlen - ilen) / 2
        } else {
            v0 - s
        }
    };
    (
        axis(view.x, view.w, image.width as i32, scroll.x),
        axis(view.y, view.h, image.height as i32, scroll.y),
    )
}

fn draw_text(s: &mut IndexedScreen, x: i32, y: i32, text: &str, color: CgaColor) {
    for (i, c) in text.chars().enumerate() {
        let g = glyphs::glyph(c);
        let gx = x + i as i32 * glyphs::CHAR_ADVANCE;
        for (row, bits) in g.iter().enumerate() {
            for col in 0..3 {
                if bits & (0b100 >> col) != 0 {
                    s.set(gx + col, y + row as i32, color);
                }
            }
        }
    }
}

/// Text vertically centered in a bar of height `bar.h`.
fn text_y(bar: Rect) -> i32 {
    bar.y + (bar.h - glyphs::CHAR_HEIGHT) / 2
}

fn draw_icon(s: &mut IndexedScreen, x: i32, y: i32, icon: &Icon, color: CgaColor) {
    for (row, line) in icon.iter().enumerate() {
        for (col, ch) in line.bytes().enumerate() {
            if ch == b'#' {
                s.set(x + col as i32, y + row as i32, color);
            }
        }
    }
}

fn draw_bars(s: &mut IndexedScreen, l: &ScreenLayout) {
    let menu = l.menu;
    s.fill(menu, CgaColor::LightGray);
    s.hline(menu.x, menu.bottom() - 1, menu.w, CgaColor::Black);
    draw_text(s, menu.x + 4, text_y(menu) - 1, MENU, CgaColor::Black);

    match l.title {
        Some(t) => {
            s.fill(t, CgaColor::Blue);
            // Close box, like a DOS GUI window.
            let bx = Rect::new(t.x + 2, t.y + 2, 5, 5);
            s.fill(bx, CgaColor::LightGray);
            s.hline(bx.x + 1, bx.y + 2, 3, CgaColor::Black);
            let tx = t.x + (t.w - glyphs::text_width(TITLE)) / 2;
            draw_text(s, tx, text_y(t), TITLE, CgaColor::White);
        }
        None => {
            let w = glyphs::text_width(TITLE) + 6;
            let t = Rect::new(menu.right() - w, menu.y, w, menu.h - 1);
            s.fill(t, CgaColor::Blue);
            draw_text(s, t.x + 3, text_y(t), TITLE, CgaColor::White);
        }
    }

    let st = l.status;
    s.fill(st, CgaColor::LightGray);
    s.hline(st.x, st.y, st.w, CgaColor::Black);
}

fn draw_canvas_window(
    s: &mut IndexedScreen,
    l: &ScreenLayout,
    image: &IndexedFrame,
    origin: (i32, i32),
    scroll: ScrollState,
) {
    let win = l.canvas_window;
    s.outline(win, CgaColor::Black);
    let view = l.viewport();
    s.fill(view, CgaColor::DarkGray);

    // The image, 1:1, clipped to the viewport.
    let (iw, ih) = (image.size.width as i32, image.size.height as i32);
    let x0 = view.x.max(origin.0);
    let x1 = view.right().min(origin.0 + iw);
    let y0 = view.y.max(origin.1);
    let y1 = view.bottom().min(origin.1 + ih);
    for y in y0..y1 {
        let row = (y - origin.1) as usize * iw as usize;
        for x in x0..x1 {
            if let Some(&c) = image.pixels.get(row + (x - origin.0) as usize) {
                s.set(x, y, c);
            }
        }
    }

    // Scrollbars: black separator, dark track, raised light thumb.
    let inner = l.canvas_inner();
    let vbar = Rect::new(view.right(), inner.y, SCROLLBAR, view.h);
    let hbar = Rect::new(inner.x, view.bottom(), view.w, SCROLLBAR);
    s.vline(vbar.x, vbar.y, vbar.h + SCROLLBAR, CgaColor::Black);
    s.hline(hbar.x, hbar.y, hbar.w + SCROLLBAR, CgaColor::Black);
    let vtrack = Rect::new(vbar.x + 1, vbar.y, vbar.w - 1, vbar.h);
    let htrack = Rect::new(hbar.x, hbar.y + 1, hbar.w, hbar.h - 1);
    s.fill(vtrack, CgaColor::DarkGray);
    s.fill(htrack, CgaColor::DarkGray);
    s.fill(
        Rect::new(vtrack.x, htrack.y, vtrack.w, htrack.h),
        CgaColor::LightGray,
    );

    let (pos, len) = thumb(vtrack.h, view.h, ih, scroll.y);
    let vthumb = Rect::new(vtrack.x, vtrack.y + pos, vtrack.w, len);
    let (pos, len) = thumb(htrack.w, view.w, iw, scroll.x);
    let hthumb = Rect::new(htrack.x + pos, htrack.y, len, htrack.h);
    for t in [vthumb, hthumb] {
        s.fill(t, CgaColor::LightGray);
        s.bevel(t, true);
    }
}

/// Scrollbar thumb `(offset, length)` along a `track`-long track.
fn thumb(track: i32, view: i32, image: i32, scroll: i32) -> (i32, i32) {
    if image <= view {
        return (0, track);
    }
    let len = (track * view / image).clamp(6.min(track), track);
    let pos = (track - len) * scroll / (image - view);
    (pos, len)
}

fn draw_panel(s: &mut IndexedScreen, l: &ScreenLayout, frame: &FrameMsg) {
    s.fill(l.panel, CgaColor::LightGray);
    s.bevel(l.panel, true);

    for (i, (tool, icon)) in TOOL_STRIP.iter().enumerate() {
        let r = l.tool_rect(i);
        let active = *tool == Some(frame.tool);
        let (bg, ink) = if active {
            (CgaColor::Blue, CgaColor::White)
        } else {
            (CgaColor::LightGray, CgaColor::Black)
        };
        s.fill(r, bg);
        s.bevel(r, !active);
        let ix = r.x + (r.w - glyphs::ICON_SIZE) / 2;
        let iy = r.y + (r.h - glyphs::ICON_SIZE) / 2;
        draw_icon(s, ix, iy, icon, ink);
    }

    // Current color: sunken well.
    let cc = l.current_color;
    s.bevel(cc, false);
    s.outline(cc.inset(1), CgaColor::Black);
    s.fill(cc.inset(2), frame.active_color);

    let colors = frame.image.palette.colors();
    for (r, &c) in l.swatch_rects(colors.len()).iter().zip(colors) {
        if c == frame.active_color {
            s.outline(*r, CgaColor::White);
            s.outline(r.inset(1), CgaColor::Black);
            s.fill(r.inset(2), c);
        } else {
            s.outline(*r, CgaColor::Black);
            s.fill(r.inset(1), c);
        }
    }
}

fn tool_label(frame: &FrameMsg) -> String {
    let name = match frame.tool {
        Tool::Select => "SELECT",
        Tool::Lasso => "LASSO",
        Tool::RectSelect => "RECT SELECT",
        Tool::Move => "MOVE",
        Tool::Brush => "BRUSH",
        Tool::Eraser => "ERASER",
        Tool::Picker => "PICKER",
    };
    match frame.tool {
        Tool::Brush | Tool::Eraser => format!("{name} {}PX", frame.brush_diameter),
        _ => name.to_string(),
    }
}

fn draw_status(s: &mut IndexedScreen, l: &ScreenLayout, frame: &FrameMsg) {
    let st = l.status;
    let y = text_y(st) + 1;
    let pos = match frame.cursor {
        Some(c) => format!("X:{:03} Y:{:03}", c.x, c.y),
        None => "X:--- Y:---".to_string(),
    };
    draw_text(s, st.x + 4, y, &pos, CgaColor::Black);
    draw_text(s, st.x + 56, y, &tool_label(frame), CgaColor::Blue);
    let size = format!("{}X{}", frame.image.size.width, frame.image.size.height);
    let sx = st.right() - 4 - glyphs::text_width(&size);
    draw_text(s, sx, y, &size, CgaColor::DarkGray);
}

/// Black or white, whichever stands out against `c`.
fn contrast(c: CgaColor) -> CgaColor {
    let [r, g, b] = c.rgb().to_array().map(u32::from);
    if r * 299 + g * 587 + b * 114 < 128_000 {
        CgaColor::White
    } else {
        CgaColor::Black
    }
}

/// Sets a pixel in the color that contrasts with what is already there.
fn set_contrast(s: &mut IndexedScreen, x: i32, y: i32, clip: Rect) {
    if let Some(under) = s.get(x, y) {
        s.set_clipped(x, y, contrast(under), clip);
    }
}

fn draw_cursor(s: &mut IndexedScreen, x: i32, y: i32, tool: Tool, diameter: u32, clip: Rect) {
    if !clip.contains(x, y) {
        return;
    }
    if tool.uses_arrow_cursor() {
        for (row, line) in glyphs::ARROW.iter().enumerate() {
            for (col, ch) in line.bytes().enumerate() {
                let c = match ch {
                    b'B' => CgaColor::Black,
                    b'W' => CgaColor::White,
                    _ => continue,
                };
                s.set_clipped(x + col as i32, y + row as i32, c, clip);
            }
        }
        return;
    }
    let d = diameter.min(512) as i32;
    match tool {
        Tool::Brush | Tool::Eraser if d >= 3 => {
            let (bx, by) = (x - d / 2, y - d / 2);
            for (i, j) in brush_outline(d, tool == Tool::Eraser) {
                set_contrast(s, bx + i, by + j, clip);
            }
            if d >= 7 {
                set_contrast(s, x, y, clip);
            }
        }
        _ => draw_crosshair(s, x, y, clip),
    }
}

fn draw_crosshair(s: &mut IndexedScreen, x: i32, y: i32, clip: Rect) {
    for k in 2..=5 {
        for (dx, dy) in [(k, 0), (-k, 0), (0, k), (0, -k)] {
            set_contrast(s, x + dx, y + dy, clip);
        }
    }
    set_contrast(s, x, y, clip);
}

/// Outline pixels of a `d`×`d` brush footprint, relative to its top-left:
/// a circle, or a square for the eraser.
fn brush_outline(d: i32, square: bool) -> Vec<(i32, i32)> {
    let inside = |i: i32, j: i32| {
        if i < 0 || j < 0 || i >= d || j >= d {
            return false;
        }
        if square {
            return true;
        }
        let (dx, dy) = ((2 * i + 1 - d) as i64, (2 * j + 1 - d) as i64);
        dx * dx + dy * dy <= (d as i64) * (d as i64)
    };
    let mut out = Vec::new();
    for j in 0..d {
        for i in 0..d {
            if inside(i, j)
                && [(1, 0), (-1, 0), (0, 1), (0, -1)]
                    .iter()
                    .any(|(di, dj)| !inside(i + di, j + dj))
            {
                out.push((i, j));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_fit_their_screens() {
        for layout in [Layout::Mobile, Layout::Desktop] {
            let l = ScreenLayout::for_layout(layout);
            let screen = Rect::new(0, 0, l.size.width as i32, l.size.height as i32);
            let within =
                |r: Rect| r.x >= 0 && r.y >= 0 && r.right() <= screen.w && r.bottom() <= screen.h;
            for r in [
                l.menu,
                l.canvas_window,
                l.panel,
                l.current_color,
                l.swatches,
                l.status,
            ] {
                assert!(within(r), "{layout:?} {r:?}");
            }
            for i in 0..TOOL_STRIP.len() {
                let r = l.tool_rect(i);
                assert!(within(r), "{layout:?} tool {i} {r:?}");
                assert!(r.x >= l.panel.x && r.right() <= l.panel.right());
                assert!(r.y >= l.panel.y && r.bottom() <= l.panel.bottom());
            }
            for n in 1..=16 {
                for r in l.swatch_rects(n) {
                    assert!(r.x >= l.swatches.x && r.right() <= l.swatches.right());
                    assert!(r.y >= l.swatches.y && r.bottom() <= l.swatches.bottom());
                    assert!(r.w >= 5 && r.h >= 5, "{layout:?} n={n} {r:?}");
                }
            }
        }
    }

    #[test]
    fn small_images_are_centered() {
        let view = Rect::new(10, 20, 100, 50);
        let o = image_origin(view, Size::new(40, 50), ScrollState { x: 0, y: 0 });
        assert_eq!(o, (40, 20));
        let o = image_origin(view, Size::new(300, 10), ScrollState { x: 7, y: 0 });
        assert_eq!(o, (3, 40));
    }

    #[test]
    fn thumb_spans_track_when_image_fits() {
        assert_eq!(thumb(100, 100, 80, 0), (0, 100));
        assert_eq!(thumb(100, 100, 400, 0), (0, 25));
        assert_eq!(thumb(100, 100, 400, 300), (75, 25));
    }

    #[test]
    fn brush_outline_is_a_ring() {
        let ring = brush_outline(5, false);
        assert!(!ring.contains(&(2, 2)), "center is not on the outline");
        assert!(ring.contains(&(2, 0)) && ring.contains(&(0, 2)));
        assert!(!ring.contains(&(0, 0)), "corners are outside a circle");
        let sq = brush_outline(4, true);
        assert_eq!(sq.len(), 12);
    }

    #[test]
    fn contrast_picks_black_or_white() {
        assert_eq!(contrast(CgaColor::Black), CgaColor::White);
        assert_eq!(contrast(CgaColor::Blue), CgaColor::White);
        assert_eq!(contrast(CgaColor::Yellow), CgaColor::Black);
        assert_eq!(contrast(CgaColor::White), CgaColor::Black);
    }
}
