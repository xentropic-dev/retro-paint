//! DOS screen composer: golden screens per layout and tool/cursor state, plus
//! property checks for 1:1 display, highlights, and autoscroll.
//!
//! Regenerate goldens on purpose with `UPDATE_GOLDEN=1 cargo test -p rp-dos`
//! and look at every changed PNG before committing.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;

use rp_doc::{FrameMsg, IndexedFrame, Layout, OutputPoint, Size, Tool};
use rp_dos::{IndexedScreen, Rect, ScrollState, canvas_viewport, compose, sample};
use rp_palette_dither::test_util::assert_palette_only;
use rp_palette_dither::{CgaColor, FourColor, Intensity, Palette, Preset, nearest_oklab};

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.png"))
}

fn write_png(path: &PathBuf, s: &IndexedScreen) {
    let file = File::create(path).unwrap();
    let mut enc = png::Encoder::new(BufWriter::new(file), s.size().width, s.size().height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .unwrap()
        .write_image_data(&s.to_rgb())
        .unwrap();
}

fn read_png(path: &PathBuf) -> (Size, Vec<u8>) {
    let dec = png::Decoder::new(BufReader::new(File::open(path).unwrap()));
    let mut reader = dec.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgb, "{}", path.display());
    buf.truncate(info.buffer_size());
    (Size::new(info.width, info.height), buf)
}

#[track_caller]
fn assert_golden(name: &str, screen: &IndexedScreen) {
    assert_palette_only(screen.pixels().iter().copied(), &Palette::full());
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        write_png(&path, screen);
        return;
    }
    assert!(
        path.exists(),
        "missing golden {}; run with UPDATE_GOLDEN=1 to create it",
        path.display()
    );
    let (size, rgb) = read_png(&path);
    let actual = screen.to_rgb();
    if size != screen.size() || rgb != actual {
        let out = std::env::temp_dir().join(format!("rp-dos-{name}-actual.png"));
        write_png(&out, screen);
        let first = rgb
            .chunks(3)
            .zip(actual.chunks(3))
            .position(|(a, b)| a != b)
            .map(|i| (i as u32 % size.width, i as u32 / size.width));
        panic!(
            "{name} differs from its golden (first diff at {first:?}, golden {size}, actual {}); \
             actual written to {}",
            screen.size(),
            out.display()
        );
    }
}

fn four_color() -> Palette {
    FourColor::new(Preset::Palette1, Intensity::High).palette()
}

/// The sample image remapped to `palette` by nearest color.
fn remapped(size: Size, palette: Palette) -> IndexedFrame {
    let mut f = sample::sample_image(size);
    for p in &mut f.pixels {
        *p = nearest_oklab(&palette, p.rgb().to_linear());
    }
    f.palette = palette;
    f
}

fn compose_fresh(frame: &FrameMsg) -> IndexedScreen {
    compose(frame, frame.layout, &mut ScrollState::default())
}

// ---- goldens ----

#[test]
fn golden_mobile_brush() {
    assert_golden(
        "mobile_brush",
        &compose_fresh(&sample::sample_frame(Layout::Mobile)),
    );
}

#[test]
fn golden_mobile_select_small_image_centered() {
    let mut f = sample::sample_frame(Layout::Mobile);
    f.image = sample::sample_image(Size::new(120, 90));
    f.tool = Tool::Select;
    f.cursor = Some(OutputPoint { x: 40, y: 30 });
    f.active_color = CgaColor::Yellow;
    assert_golden("mobile_select_small", &compose_fresh(&f));
}

#[test]
fn golden_mobile_picker_four_color() {
    let mut f = sample::sample_frame(Layout::Mobile);
    f.image = remapped(f.image.size, four_color());
    f.tool = Tool::Picker;
    f.active_color = four_color().colors()[1];
    assert_golden("mobile_picker_4color", &compose_fresh(&f));
}

#[test]
fn golden_mobile_no_cursor() {
    let mut f = sample::sample_frame(Layout::Mobile);
    f.cursor = None;
    f.tool = Tool::Eraser;
    f.active_color = CgaColor::Black;
    assert_golden("mobile_no_cursor", &compose_fresh(&f));
}

#[test]
fn golden_desktop_brush() {
    assert_golden(
        "desktop_brush",
        &compose_fresh(&sample::sample_frame(Layout::Desktop)),
    );
}

#[test]
fn golden_desktop_move() {
    let mut f = sample::sample_frame(Layout::Desktop);
    f.tool = Tool::Move;
    f.cursor = Some(OutputPoint { x: 60, y: 40 });
    f.active_color = CgaColor::LightCyan;
    assert_golden("desktop_move", &compose_fresh(&f));
}

#[test]
fn golden_desktop_eraser_four_color() {
    let mut f = sample::sample_frame(Layout::Desktop);
    f.image = remapped(f.image.size, four_color());
    f.tool = Tool::Eraser;
    f.brush_diameter = 15;
    f.active_color = four_color().colors()[3];
    assert_golden("desktop_eraser_4color", &compose_fresh(&f));
}

#[test]
fn golden_mobile_lasso() {
    let mut f = sample::sample_frame(Layout::Mobile);
    f.tool = Tool::Lasso;
    f.cursor = Some(OutputPoint { x: 90, y: 140 });
    f.active_color = CgaColor::LightMagenta;
    assert_golden("mobile_lasso", &compose_fresh(&f));
}

#[test]
fn golden_desktop_rect_select() {
    let mut f = sample::sample_frame(Layout::Desktop);
    f.tool = Tool::RectSelect;
    f.cursor = Some(OutputPoint { x: 100, y: 60 });
    f.active_color = CgaColor::LightGreen;
    assert_golden("desktop_rect_select", &compose_fresh(&f));
}

/// Every editor tool has its own tool strip highlight: above the status bar
/// (which names the tool), no two tools compose the same screen.
#[test]
fn every_tool_has_its_own_highlight() {
    for layout in [Layout::Mobile, Layout::Desktop] {
        let mut f = sample::sample_frame(layout);
        f.cursor = None;
        let screen = layout.dos_screen();
        let above_status = (screen.width * (screen.height - 9)) as usize;
        let screens: Vec<_> = Tool::ALL
            .iter()
            .map(|&t| {
                f.tool = t;
                compose_fresh(&f).pixels()[..above_status].to_vec()
            })
            .collect();
        for (i, a) in screens.iter().enumerate() {
            for (j, b) in screens.iter().enumerate().skip(i + 1) {
                assert_ne!(a, b, "{layout:?} {:?} vs {:?}", Tool::ALL[i], Tool::ALL[j]);
            }
        }
    }
}

/// Selection tools draw the crosshair, not the arrow: the pixel left of the
/// hotspot changes (the arrow never draws left of its hotspot).
#[test]
fn selection_tools_use_the_crosshair() {
    for layout in [Layout::Mobile, Layout::Desktop] {
        for tool in [Tool::Lasso, Tool::RectSelect] {
            let mut f = sample::sample_frame(layout);
            f.tool = tool;
            let c = OutputPoint { x: 50, y: 50 };
            f.cursor = Some(c);
            let mut scroll = ScrollState::default();
            let with = compose(&f, layout, &mut scroll);
            let mut none = f.clone();
            none.cursor = None;
            let without = compose(&none, layout, &mut scroll.clone());
            let (sx, sy) = cursor_screen_pos(layout, f.image.size, scroll, c);
            assert_ne!(
                with.get(sx - 3, sy),
                without.get(sx - 3, sy),
                "{layout:?} {tool:?}"
            );
            assert_ne!(
                with.get(sx, sy - 3),
                without.get(sx, sy - 3),
                "{layout:?} {tool:?}"
            );
        }
    }
}

// ---- properties ----

#[test]
fn screen_size_matches_layout() {
    for layout in [Layout::Mobile, Layout::Desktop] {
        let s = compose_fresh(&sample::sample_frame(layout));
        assert_eq!(s.size(), layout.dos_screen());
        assert_eq!(s.pixels().len(), layout.dos_screen().area());
    }
}

#[test]
fn compose_is_deterministic() {
    let f = sample::sample_frame(Layout::Mobile);
    assert_eq!(compose_fresh(&f), compose_fresh(&f));
}

/// The canvas viewport shows the image 1:1 at the scroll offset.
#[test]
fn canvas_shows_image_one_to_one() {
    for layout in [Layout::Mobile, Layout::Desktop] {
        let mut f = sample::sample_frame(layout);
        f.cursor = None;
        let mut scroll = ScrollState { x: 17, y: 23 };
        let s = compose(&f, layout, &mut scroll);
        assert_eq!(scroll, ScrollState { x: 17, y: 23 }, "no cursor, in range");
        let v = canvas_viewport(layout);
        for y in v.y..v.bottom() {
            for x in v.x..v.right() {
                let img = f
                    .image
                    .get((x - v.x + scroll.x) as i64, (y - v.y + scroll.y) as i64);
                assert_eq!(s.get(x, y), img, "{layout:?} at screen ({x}, {y})");
            }
        }
    }
}

/// Where a cursor lands on screen, from the composed scroll state.
fn cursor_screen_pos(
    layout: Layout,
    image: Size,
    scroll: ScrollState,
    c: OutputPoint,
) -> (i32, i32) {
    let v = canvas_viewport(layout);
    let ox = if (image.width as i32) < v.w {
        v.x + (v.w - image.width as i32) / 2
    } else {
        v.x - scroll.x
    };
    let oy = if (image.height as i32) < v.h {
        v.y + (v.h - image.height as i32) / 2
    } else {
        v.y - scroll.y
    };
    (ox + c.x, oy + c.y)
}

#[test]
fn autoscroll_keeps_cursor_in_view() {
    for layout in [Layout::Mobile, Layout::Desktop] {
        let mut f = sample::sample_frame(layout);
        let size = f.image.size;
        let v = canvas_viewport(layout);
        let mut scroll = ScrollState::default();
        // Sweep down-right, then back up-left.
        let n = 40;
        let path = (0..=n)
            .map(|i| {
                (
                    i * (size.width as i32 - 1) / n,
                    i * (size.height as i32 - 1) / n,
                )
            })
            .chain(
                (0..=n)
                    .rev()
                    .map(|i| (i * 7 % size.width as i32, i * 9 % size.height as i32)),
            );
        for (x, y) in path {
            let c = OutputPoint { x, y };
            f.cursor = Some(c);
            compose(&f, layout, &mut scroll);
            let (sx, sy) = cursor_screen_pos(layout, size, scroll, c);
            assert!(
                v.contains(sx, sy),
                "{layout:?} cursor {c:?} at ({sx},{sy}) outside {v:?}"
            );
        }
        assert!(scroll.x <= size.width as i32 - v.w && scroll.y <= size.height as i32 - v.h);
    }
}

/// The canvas window around the viewport: border plus scrollbars.
fn canvas_window(layout: Layout) -> Rect {
    let v = canvas_viewport(layout);
    Rect::new(v.x - 1, v.y - 1, v.w + 8, v.h + 8)
}

/// Screen positions where two screens differ.
fn diffs(a: &IndexedScreen, b: &IndexedScreen) -> Vec<(i32, i32)> {
    let w = a.size().width as i32;
    a.pixels()
        .iter()
        .zip(b.pixels())
        .enumerate()
        .filter(|(_, (p, q))| p != q)
        .map(|(i, _)| (i as i32 % w, i as i32 / w))
        .collect()
}

#[test]
fn tool_change_shows_outside_the_canvas_window() {
    for layout in [Layout::Mobile, Layout::Desktop] {
        let mut f = sample::sample_frame(layout);
        f.cursor = None;
        f.tool = Tool::Select;
        let base = compose_fresh(&f);
        let win = canvas_window(layout);
        for tool in &Tool::ALL[1..] {
            f.tool = *tool;
            let d = diffs(&base, &compose_fresh(&f));
            assert!(!d.is_empty(), "{layout:?} {tool:?} must show");
            assert!(
                d.iter().all(|&(x, y)| !win.contains(x, y)),
                "{layout:?} {tool:?} touched the canvas window"
            );
        }
    }
}

#[test]
fn color_change_shows_outside_the_canvas_window() {
    for layout in [Layout::Mobile, Layout::Desktop] {
        let mut f = sample::sample_frame(layout);
        f.cursor = None;
        f.active_color = CgaColor::Red;
        let a = compose_fresh(&f);
        f.active_color = CgaColor::Green;
        let d = diffs(&a, &compose_fresh(&f));
        assert!(!d.is_empty(), "{layout:?} color change must show");
        let win = canvas_window(layout);
        assert!(d.iter().all(|&(x, y)| !win.contains(x, y)), "{layout:?}");
    }
}

#[test]
fn cursor_never_draws_outside_the_canvas_viewport() {
    for layout in [Layout::Mobile, Layout::Desktop] {
        let v = canvas_viewport(layout);
        let status_top = layout.dos_screen().height as i32 - 9;
        let mut f = sample::sample_frame(layout);
        f.brush_diameter = 40;
        for tool in Tool::ALL {
            f.tool = tool;
            for (x, y) in [(0, 0), (-3, -3), (239, 239), (5, 399), (399, 5)] {
                f.cursor = Some(OutputPoint { x, y });
                let mut scroll = ScrollState::default();
                let with = compose(&f, layout, &mut scroll);
                let mut no_cursor = f.clone();
                no_cursor.cursor = None;
                let without = compose(&no_cursor, layout, &mut scroll.clone());
                for (xx, yy) in diffs(&with, &without) {
                    // The status bar shows the cursor position.
                    assert!(
                        v.contains(xx, yy) || yy >= status_top,
                        "{layout:?} {tool:?} cursor ({x},{y}) drew at ({xx},{yy})"
                    );
                }
            }
        }
    }
}

#[test]
fn mismatched_pixel_buffer_does_not_panic() {
    let mut f = sample::sample_frame(Layout::Mobile);
    f.image.pixels.truncate(10);
    compose_fresh(&f);
    f.image = IndexedFrame::filled(Size::new(0, 0), CgaColor::Black, Palette::full());
    compose_fresh(&f);
}
