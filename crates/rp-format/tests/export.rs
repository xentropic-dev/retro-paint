//! PNG export: dimensions, palette-only pixels, exact s×s blocks, equality
//! with `render`, the full-color file, invalid scale, atomic writes.

mod common;

use std::io::Cursor;
use std::path::Path;

use common::{rendered, sample_doc, temp_dir};
use rp_doc::{Document, RgbaImage, Size, SourceStore};
use rp_format::{
    ExportOptions, FormatError, encode_dithered_png, export_png, full_color_path, render_document,
};
use rp_palette_dither::test_util::{assert_palette_only, assert_rgba_palette_only};
use rp_palette_dither::{
    CgaColor, FourColor, IndexedImage, Intensity, Palette, PaletteChoice, Preset, Rgb,
};

/// A decoded PNG: size, the raw color type/depth/PLTE, and RGBA8 pixels.
struct Decoded {
    size: Size,
    color: png::ColorType,
    depth: png::BitDepth,
    plte: Option<Vec<u8>>,
    rgba: Vec<u8>,
}

fn decode(bytes: &[u8]) -> Decoded {
    // Raw header info.
    let raw = png::Decoder::new(Cursor::new(bytes)).read_info().unwrap();
    let info = raw.info();
    let (color, depth) = (info.color_type, info.bit_depth);
    let plte = info.palette.as_ref().map(|p| p.to_vec());

    // Pixels, expanded to 8-bit RGB(A).
    let mut dec = png::Decoder::new(Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let out = reader.next_frame(&mut buf).unwrap();
    buf.truncate(out.line_size * out.height as usize);
    let rgba = match out.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        other => panic!("unexpected output color {other:?}"),
    };
    Decoded {
        size: Size::new(out.width, out.height),
        color,
        depth,
        plte,
        rgba,
    }
}

fn pixel(d: &Decoded, x: u32, y: u32) -> [u8; 4] {
    let i = (y * d.size.width + x) as usize * 4;
    d.rgba[i..i + 4].try_into().unwrap()
}

fn four_color_doc() -> (Document, SourceStore) {
    let (mut doc, sources) = sample_doc();
    doc.dither.palette = PaletteChoice::FourColor(
        FourColor::new(Preset::Palette1, Intensity::Low).with_background(CgaColor::Green),
    );
    (doc, sources)
}

fn full_palette_doc() -> (Document, SourceStore) {
    let (mut doc, sources) = sample_doc();
    doc.dither.palette = PaletteChoice::Full;
    (doc, sources)
}

/// Checks a decoded export against the indexed render at scale `s`.
fn assert_matches_render(d: &Decoded, img: &IndexedImage, s: u32) {
    assert_eq!(d.size, Size::new(img.width * s, img.height * s));
    assert_rgba_palette_only(&d.rgba, &img.palette);
    for y in 0..d.size.height {
        for x in 0..d.size.width {
            let want: Rgb = img.get(x / s, y / s).unwrap().into();
            let [r, g, b, a] = pixel(d, x, y);
            assert_eq!((Rgb::new(r, g, b), a), (want, 255), "pixel ({x}, {y})");
        }
    }
}

#[test]
fn dithered_png_is_indexed_with_the_active_palette() {
    for (doc, sources) in [full_palette_doc(), four_color_doc()] {
        let img = render_document(&doc, &sources);
        let d = decode(&encode_dithered_png(&img, 1).unwrap());
        assert_eq!(d.color, png::ColorType::Indexed);
        assert_eq!(d.depth, png::BitDepth::Four);
        let palette = doc.dither.palette.palette();
        let want: Vec<u8> = palette.rgb().flat_map(|c| c.to_array()).collect();
        assert_eq!(d.plte.as_deref(), Some(&want[..]));
    }
}

#[test]
fn export_at_1x_equals_render_pixel_for_pixel() {
    for (doc, sources) in [full_palette_doc(), four_color_doc()] {
        let img = rendered(&doc, &sources);
        assert_palette_only(img.pixels.iter().copied(), &doc.dither.palette.palette());
        assert_eq!(render_document(&doc, &sources), img);
        let d = decode(&encode_dithered_png(&img, 1).unwrap());
        assert_eq!(d.size, doc.output_size);
        assert_eq!(d.rgba.len(), img.to_rgb8().len() / 3 * 4);
        assert_matches_render(&d, &img, 1);
    }
}

#[test]
fn upscaled_exports_have_exact_dimensions_and_uniform_blocks() {
    for (doc, sources) in [full_palette_doc(), four_color_doc()] {
        let img = rendered(&doc, &sources);
        for s in [1u8, 2, 4, 8] {
            let d = decode(&encode_dithered_png(&img, s).unwrap());
            let s = s as u32;
            assert_eq!(
                d.size,
                Size::new(doc.output_size.width * s, doc.output_size.height * s)
            );
            assert_matches_render(&d, &img, s);
        }
    }
}

#[test]
fn odd_width_packs_nibbles_correctly() {
    // Width 1 and 3 exercise the half-filled last byte of each row.
    let palette = Palette::full();
    let all: Vec<CgaColor> = palette.colors().to_vec();
    for w in [1u32, 3, 5] {
        let h = 16u32.div_ceil(w);
        let pixels = (0..w * h).map(|i| all[i as usize % 16]).collect();
        let img = IndexedImage {
            width: w,
            height: h,
            pixels,
            palette,
        };
        for s in [1u8, 3] {
            let d = decode(&encode_dithered_png(&img, s).unwrap());
            assert_matches_render(&d, &img, s as u32);
        }
    }
}

#[test]
fn export_png_writes_dithered_file_at_scale() {
    let dir = temp_dir("export-scale");
    let (doc, sources) = four_color_doc();
    let img = rendered(&doc, &sources);
    for s in [1u8, 4, 8] {
        let path = dir.join(format!("out{s}.png"));
        let opts = ExportOptions {
            scale: s,
            include_full_color: false,
        };
        let report = export_png(&doc, &sources, &opts, &path).unwrap();
        assert_eq!(report.dithered, path);
        assert_eq!(report.full_color, None);
        let d = decode(&std::fs::read(&path).unwrap());
        assert_eq!(report.dithered_size, d.size);
        assert_matches_render(&d, &img, s as u32);
        assert!(!full_color_path(&path).exists());
    }
    assert_no_temp_files(&dir);
}

#[test]
fn full_color_export_is_the_flattened_canvas() {
    let dir = temp_dir("export-full");
    let (doc, sources) = sample_doc();
    let path = dir.join("pic.png");
    let opts = ExportOptions {
        scale: 3,
        include_full_color: true,
    };
    let report = export_png(&doc, &sources, &opts, &path).unwrap();
    let full_path = dir.join("pic-full.png");
    assert_eq!(
        report.full_color,
        Some((full_path.clone(), doc.canvas_size))
    );

    let d = decode(&std::fs::read(&full_path).unwrap());
    assert_eq!(d.color, png::ColorType::Rgba);
    assert_eq!(d.depth, png::BitDepth::Eight);
    assert_eq!(d.size, doc.canvas_size);
    let flat: RgbaImage = doc.flatten(&sources);
    assert_eq!(d.rgba, flat.as_raw());

    // The dithered file is still written and still palette-only.
    let dd = decode(&std::fs::read(&path).unwrap());
    assert_matches_render(&dd, &rendered(&doc, &sources), 3);
    assert_no_temp_files(&dir);
}

#[test]
fn invalid_scale_is_a_typed_error_and_writes_nothing() {
    let dir = temp_dir("export-bad-scale");
    let (doc, sources) = sample_doc();
    let img = rendered(&doc, &sources);
    for s in [0u8, 9, 200] {
        assert!(matches!(
            encode_dithered_png(&img, s),
            Err(FormatError::InvalidScale(v)) if v == s
        ));
        let path = dir.join("bad.png");
        let opts = ExportOptions {
            scale: s,
            include_full_color: true,
        };
        let err = export_png(&doc, &sources, &opts, &path).unwrap_err();
        assert!(matches!(err, FormatError::InvalidScale(v) if v == s));
        assert!(err.to_string().contains("1× to 8×"));
    }
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

#[test]
fn failed_write_leaves_no_temp_and_keeps_existing_file() {
    let dir = temp_dir("export-fail");
    let (doc, sources) = sample_doc();
    // The target is a non-empty directory, so the final rename fails after
    // the temp file was written.
    let target = dir.join("taken.png");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep"), b"x").unwrap();
    let err = export_png(&doc, &sources, &ExportOptions::default(), &target).unwrap_err();
    assert!(matches!(err, FormatError::Io(_)), "{err:?}");
    assert!(target.join("keep").exists());
    assert_no_temp_files(&dir);

    // An existing file at the target is replaced on success.
    let ok = dir.join("ok.png");
    std::fs::write(&ok, b"old").unwrap();
    export_png(&doc, &sources, &ExportOptions::default(), &ok).unwrap();
    assert_ne!(std::fs::read(&ok).unwrap(), b"old");
    assert_no_temp_files(&dir);
}

fn assert_no_temp_files(dir: &Path) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let name = entry.unwrap().file_name();
        assert!(
            !name.to_string_lossy().ends_with(".tmp"),
            "temp file left behind: {name:?}"
        );
    }
}
