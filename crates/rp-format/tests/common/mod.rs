//! Shared fixtures for rp-format integration tests.
#![allow(dead_code)]

use std::io::{Cursor, Read, Write};
use std::path::PathBuf;

use rp_doc::{
    BlendMode, Document, GrayImage, Layer, LayerKind, Layout, Lock, PaintLayer, RgbaImage, Size,
    SourceStore, Transform,
};
use rp_format::{save, source_hash};
use rp_palette_dither::{
    CgaColor, DitherMethod, FourColor, IndexedImage, Intensity, PreAdjust, Preset, Rgb, render,
};

/// A document that sets every saved field away from its default: a paint
/// layer with an offset, an image layer with a full transform and a mask, a
/// second paint layer with a mask; every blend mode but one, partial
/// opacity, hidden layer, both locks; non-default dither settings, layout,
/// output size, colors and swatches.
pub fn sample_doc() -> (Document, SourceStore) {
    let canvas = Size::new(24, 16);
    let mut doc = Document::new(canvas);
    doc.output_size = Size::new(10, 7);
    doc.layout = Layout::Desktop;
    doc.dither.method = DitherMethod::Atkinson;
    doc.dither.palette = rp_palette_dither::PaletteChoice::FourColor(
        FourColor::new(Preset::Palette0, Intensity::High).with_background(CgaColor::Blue),
    );
    doc.dither.adjust = PreAdjust {
        brightness: 0.1,
        contrast: -0.23,
        saturation: 0.37,
    };
    doc.dither.bayer_spread = 0.21;
    doc.dither.strength = 0.85;
    doc.dither.serpentine = false;
    doc.fg = Rgb::new(0x12, 0x34, 0x56);
    doc.bg = Rgb::new(0xfe, 0xdc, 0xba);
    doc.swatches = vec![Rgb::new(1, 2, 3), Rgb::new(250, 128, 0), Rgb::new(0, 0, 0)];

    // Bottom: opaque gradient paint layer, shifted.
    let mut bg = RgbaImage::new(canvas);
    for y in 0..canvas.height {
        for x in 0..canvas.width {
            bg.put(x, y, [(x * 10) as u8, (y * 15) as u8, 128, 255]);
        }
    }
    let mut base = Layer::with_kind(
        "Background",
        LayerKind::Paint(PaintLayer {
            pixels: bg,
            offset_x: -2,
            offset_y: 1,
        }),
    );
    base.lock = Lock {
        pixels: true,
        position: false,
    };
    doc.push_layer(base);

    // Middle: image layer, scaled and rotated, masked, multiply.
    let mut src = RgbaImage::new(Size::new(9, 5));
    for y in 0..5 {
        for x in 0..9 {
            src.put(
                x,
                y,
                [255 - (x * 20) as u8, (y * 50) as u8, (x * y * 7) as u8, 200],
            );
        }
    }
    let hash = source_hash(&src);
    let mut sources = SourceStore::new();
    sources.insert(hash.clone(), src);
    let mut img = Layer::image(
        "Photo \"bash\" ✓",
        hash,
        Transform {
            translate_x: 3.25,
            translate_y: -1.5,
            scale_x: 1.7,
            scale_y: 2.1,
            rotation: 0.3,
        },
    );
    img.blend = BlendMode::Multiply;
    img.opacity = 73;
    img.lock = Lock {
        pixels: false,
        position: true,
    };
    let mut mask = GrayImage::filled(canvas, 255);
    for x in 0..canvas.width {
        mask.put(x, 0, (x * 11) as u8);
    }
    img.mask = Some(mask);
    doc.push_layer(img);

    // Top: semi-transparent paint, overlay, with a mask smaller than canvas.
    let mut top = Layer::paint("Ink", canvas);
    {
        let px = top.pixels_mut().unwrap();
        for i in 0..canvas.width.min(canvas.height) {
            px.put(i, i, [255, 255, 0, 180]);
            px.put(canvas.width - 1 - i, i, [10, 200, 30, 90]);
        }
    }
    top.blend = BlendMode::Overlay;
    top.opacity = 100;
    top.mask = Some(GrayImage::filled(Size::new(12, 8), 100));
    top.lock = Lock {
        pixels: true,
        position: true,
    };
    doc.push_layer(top);

    // Hidden screen layer.
    let mut hidden = Layer::paint("hidden", canvas);
    hidden.visible = false;
    hidden.blend = BlendMode::Screen;
    hidden.opacity = 0;
    hidden.pixels_mut().unwrap().put(1, 1, [9, 9, 9, 9]);
    doc.push_layer(hidden);

    (doc, sources)
}

/// Asserts every persisted field matches (layer ids are not persisted).
pub fn assert_docs_equal(a: &Document, b: &Document) {
    assert_eq!(a.canvas_size, b.canvas_size);
    assert_eq!(a.output_size, b.output_size);
    assert_eq!(a.layout, b.layout);
    assert_eq!(a.dither, b.dither);
    assert_eq!(a.fg, b.fg);
    assert_eq!(a.bg, b.bg);
    assert_eq!(a.swatches, b.swatches);
    assert_eq!(a.layers().len(), b.layers().len());
    for (la, lb) in a.layers().iter().zip(b.layers()) {
        assert_eq!(la.name, lb.name);
        assert_eq!(la.visible, lb.visible);
        assert_eq!(la.opacity, lb.opacity);
        assert_eq!(la.blend, lb.blend);
        assert_eq!(la.mask, lb.mask);
        assert_eq!(la.lock, lb.lock);
        assert_eq!(la.kind, lb.kind);
    }
}

/// The dithered output, through the same `render` as live view and export.
pub fn rendered(doc: &Document, sources: &SourceStore) -> IndexedImage {
    let flat = doc.flatten(sources);
    render(
        flat.as_raw(),
        (flat.width(), flat.height()),
        (doc.output_size.width, doc.output_size.height),
        &doc.dither,
    )
}

pub fn save_bytes(doc: &Document, sources: &SourceStore) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    save(doc, sources, &mut out).expect("save");
    out.into_inner()
}

/// Copies a zip, letting `edit` drop (`None`) or replace each entry.
pub fn rewrite(bytes: &[u8], mut edit: impl FnMut(&str, Vec<u8>) -> Option<Vec<u8>>) -> Vec<u8> {
    let mut zin = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut zout = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..zin.len() {
        let mut f = zin.by_index(i).unwrap();
        let name = f.name().unwrap().into_owned();
        let mut data = Vec::new();
        f.read_to_end(&mut data).unwrap();
        if let Some(data) = edit(&name, data) {
            zout.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zout.write_all(&data).unwrap();
        }
    }
    zout.finish().unwrap().into_inner()
}

/// Rewrites `manifest.json` through `edit`.
pub fn edit_manifest(bytes: &[u8], edit: impl Fn(&mut serde_json::Value)) -> Vec<u8> {
    rewrite(bytes, |name, data| {
        if name == "manifest.json" {
            let mut v: serde_json::Value = serde_json::from_slice(&data).unwrap();
            edit(&mut v);
            Some(serde_json::to_vec(&v).unwrap())
        } else {
            Some(data)
        }
    })
}

/// A fresh empty directory under the cargo target tmpdir.
pub fn temp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("rp-format-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
