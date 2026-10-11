//! Headless end-to-end check of the whole pipeline without any window:
//! document → render → DOS screen → PNG export → `.rpaint` save → load →
//! re-render. The live view and export must agree, and a saved project must
//! reopen to byte-identical output.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rp_doc::{
    BlendMode, Document, FrameMsg, GrayImage, IndexedFrame, Layer, Layout, OutputPoint, RgbaImage,
    Size, SourceStore, Tool, Transform,
};
use rp_dos::{IndexedScreen, ScrollState, compose};
use rp_format::{ExportOptions, export_png_with, load_from_path, save_to_path, source_hash};
use rp_palette_dither::test_util::{assert_palette_only, assert_rgba_palette_only};
use rp_palette_dither::{
    DitherMethod, FourColor, IndexedImage, Intensity, PaletteChoice, Preset, Renderer, Rgb,
};

const SCALE: u8 = 4;

/// A unique scratch directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("retro-paint-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A photobash in miniature: an opaque gradient paint layer, a scaled and
/// rotated image layer inserted the way the editor does (source stored under
/// its `source_hash`), and a masked, semi-transparent paint layer on top.
/// Output is canvas ÷ 2 in the mobile layout, larger than the DOS canvas
/// window so the composed screen has to scroll. Dither: Floyd–Steinberg into
/// the 4-color Palette 1 (high) mode on a blue background.
fn photobash() -> (Document, SourceStore) {
    let canvas = Size::new(480, 720);
    let mut doc = Document::new(canvas);
    doc.output_size = Size::new(240, 360);
    doc.layout = Layout::Mobile;
    doc.dither.method = DitherMethod::FloydSteinberg;
    doc.dither.palette = PaletteChoice::FourColor(
        FourColor::new(Preset::Palette1, Intensity::High)
            .with_background(rp_palette_dither::CgaColor::Blue),
    );
    doc.fg = Rgb::new(0xff, 0x40, 0xc0);

    // Bottom: opaque sky-to-ground gradient.
    let mut base = Layer::paint("Background", canvas);
    {
        let px = base.pixels_mut().unwrap();
        for y in 0..canvas.height {
            for x in 0..canvas.width {
                let t = y * 255 / (canvas.height - 1);
                px.put(
                    x,
                    y,
                    [(x * 255 / canvas.width) as u8, t as u8, 255 - t as u8, 255],
                );
            }
        }
    }
    doc.push_layer(base);

    // Middle: an inserted "photo", scaled up and rotated.
    let src_size = Size::new(160, 120);
    let mut src = RgbaImage::new(src_size);
    for y in 0..src_size.height {
        for x in 0..src_size.width {
            let ring = (((x as i32 - 80).pow(2) + (y as i32 - 60).pow(2)) / 64) as u8;
            src.put(
                x,
                y,
                [ring.wrapping_mul(9), 255 - (x as u8), y as u8 * 2, 255],
            );
        }
    }
    let hash = source_hash(&src);
    let mut sources = SourceStore::new();
    sources.insert(hash.clone(), src);
    let mut photo = Layer::image(
        "photo",
        hash,
        Transform {
            translate_x: 90.0,
            translate_y: 200.0,
            scale_x: 1.75,
            scale_y: 1.75,
            rotation: 0.35,
        },
    );
    photo.opacity = 85;
    doc.push_layer(photo);

    // Top: semi-transparent ink, screened, masked to the upper half.
    let mut ink = Layer::paint("Ink", canvas);
    {
        let px = ink.pixels_mut().unwrap();
        for i in 0..canvas.width {
            for w in 0..6 {
                px.put(i, (i + w).min(canvas.height - 1), [255, 255, 0, 200]);
            }
        }
    }
    ink.blend = BlendMode::Screen;
    let mut mask = GrayImage::filled(canvas, 0);
    for y in 0..canvas.height / 2 {
        for x in 0..canvas.width {
            mask.put(x, y, 255);
        }
    }
    ink.mask = Some(mask);
    doc.push_layer(ink);

    (doc, sources)
}

/// The live view's render: flatten, then the render worker's `Renderer`.
fn render(renderer: &mut Renderer, doc: &Document, sources: &SourceStore) -> IndexedImage {
    let flat = doc.flatten(sources);
    renderer.render(
        flat.as_raw(),
        (flat.width(), flat.height()),
        (doc.output_size.width, doc.output_size.height),
        &doc.dither,
    )
}

/// The DOS screen for `image`, with the cursor near the bottom-right of the
/// output so the canvas window scrolls.
fn screen(renderer: &mut Renderer, doc: &Document, image: &IndexedImage) -> IndexedScreen {
    let palette = doc.dither.palette.palette();
    let frame = FrameMsg {
        image: IndexedFrame::from(image.clone()),
        layout: doc.layout,
        cursor: Some(OutputPoint { x: 210, y: 330 }),
        tool: Tool::Brush,
        brush_diameter: 5,
        active_color: renderer.lut(palette).nearest(doc.fg),
    };
    let mut scroll = ScrollState::default();
    let s = compose(&frame, frame.layout, &mut scroll);
    assert_ne!(
        scroll,
        ScrollState::default(),
        "the canvas window should scroll"
    );
    s
}

/// Decodes a PNG to (size, RGBA8).
fn decode_png(path: &Path) -> (Size, Vec<u8>) {
    let bytes = std::fs::read(path).unwrap();
    let mut dec = png::Decoder::new(Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let out = reader.next_frame(&mut buf).unwrap();
    buf.truncate(out.line_size * out.height as usize);
    let rgba = match out.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        other => panic!("unexpected PNG color type {other:?}"),
    };
    (Size::new(out.width, out.height), rgba)
}

#[test]
fn render_compose_export_save_load_round_trip() {
    let tmp = TempDir::new("e2e");
    let (doc, sources) = photobash();
    let palette = doc.dither.palette.palette();
    let mut renderer = Renderer::new();

    // Render, as the live view does.
    let indexed = render(&mut renderer, &doc, &sources);
    assert_eq!((indexed.width, indexed.height), (240, 360));
    assert_eq!(indexed.palette, palette);
    assert_palette_only(indexed.pixels.iter().copied(), &palette);
    let used: std::collections::HashSet<_> = indexed.pixels.iter().collect();
    assert!(
        used.len() >= 3,
        "a photobash should dither to several colors"
    );

    // Compose the DOS screen.
    let screen_a = screen(&mut renderer, &doc, &indexed);
    assert_eq!(screen_a.size(), Layout::Mobile.dos_screen());

    // Export at 4×: exact size, palette-only, every block one output pixel.
    let png_path = tmp.path("bash.png");
    let report = export_png_with(
        &mut renderer,
        &doc,
        &sources,
        &ExportOptions {
            scale: SCALE,
            include_full_color: false,
        },
        &png_path,
    )
    .unwrap();
    let s = SCALE as u32;
    assert_eq!(report.dithered_size, Size::new(240 * s, 360 * s));
    let (png_size, rgba) = decode_png(&png_path);
    assert_eq!(png_size, Size::new(240 * s, 360 * s));
    assert_rgba_palette_only(&rgba, &palette);
    for y in 0..png_size.height {
        for x in 0..png_size.width {
            let i = (y * png_size.width + x) as usize * 4;
            let want = indexed.get(x / s, y / s).unwrap().rgb().to_array();
            assert_eq!(rgba[i..i + 3], want, "export pixel ({x}, {y})");
        }
    }

    // Save, reload, re-render: identical indexed output and DOS screen.
    let project = tmp.path("bash.rpaint");
    save_to_path(&doc, &sources, &project).unwrap();
    let (loaded, loaded_sources) = load_from_path(&project).unwrap();
    assert_eq!(loaded.layers().len(), 3);

    let mut fresh = Renderer::new();
    let reindexed = render(&mut fresh, &loaded, &loaded_sources);
    assert_eq!(reindexed, indexed, "reloaded project renders differently");
    let screen_b = screen(&mut fresh, &loaded, &reindexed);
    assert_eq!(screen_b, screen_a, "reloaded project composes differently");
    assert_palette_only(
        screen_b.pixels().iter().copied(),
        &rp_palette_dither::Palette::full(),
    );

    // Exporting the reloaded project gives the same file.
    let png_again = tmp.path("bash-again.png");
    export_png_with(
        &mut fresh,
        &loaded,
        &loaded_sources,
        &ExportOptions {
            scale: SCALE,
            include_full_color: false,
        },
        &png_again,
    )
    .unwrap();
    assert_eq!(
        std::fs::read(&png_again).unwrap(),
        std::fs::read(&png_path).unwrap()
    );
}
