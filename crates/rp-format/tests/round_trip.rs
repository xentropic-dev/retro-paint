//! Save → load gives back every document field, and `render` of the loaded
//! document is byte-identical to the original.

mod common;

use std::io::Cursor;

use common::{assert_docs_equal, rendered, sample_doc, save_bytes};
use rp_doc::{Document, Layer, Layout, Size, SourceStore};
use rp_format::{FormatError, load, load_from_path, save_to_path};
use rp_palette_dither::test_util::assert_palette_only;
use rp_palette_dither::{
    CgaColor, DitherMethod, FourColor, Intensity, Palette, PaletteChoice, Preset,
};

fn round_trip(doc: &Document, sources: &SourceStore) -> (Document, SourceStore) {
    let bytes = save_bytes(doc, sources);
    load(Cursor::new(bytes)).expect("load")
}

fn check(doc: &Document, sources: &SourceStore) {
    let (back, back_sources) = round_trip(doc, sources);
    assert_docs_equal(doc, &back);
    assert_eq!(back_sources.len(), 1);
    for h in back_sources.hashes() {
        assert_eq!(back_sources.get(h), sources.get(h));
    }
    let a = rendered(doc, sources);
    let b = rendered(&back, &back_sources);
    assert_palette_only(a.pixels.iter().copied(), &doc.dither.palette.palette());
    assert_eq!(a, b, "render output differs after round trip");
}

#[test]
fn every_field_round_trips_with_identical_render() {
    let (doc, sources) = sample_doc();
    check(&doc, &sources);
}

#[test]
fn palette_choices_round_trip() {
    let (mut doc, sources) = sample_doc();
    let choices = [
        PaletteChoice::Full,
        PaletteChoice::FourColor(
            FourColor::new(Preset::Palette1, Intensity::High).with_background(CgaColor::Blue),
        ),
        PaletteChoice::FourColor(FourColor::new(Preset::Mode5, Intensity::Low)),
        // Background repeats a preset color: the palette dedups it.
        PaletteChoice::FourColor(
            FourColor::new(Preset::Palette0, Intensity::Low).with_background(CgaColor::Green),
        ),
        PaletteChoice::Custom(
            Palette::new([CgaColor::Yellow, CgaColor::Black, CgaColor::LightMagenta]).unwrap(),
        ),
    ];
    for (choice, method) in choices
        .into_iter()
        .zip(DitherMethod::ALL.into_iter().cycle())
    {
        doc.dither.palette = choice;
        doc.dither.method = method;
        check(&doc, &sources);
    }
}

#[test]
fn empty_document_round_trips() {
    let doc = Document::new(Size::new(5, 3));
    let (back, sources) = round_trip(&doc, &SourceStore::new());
    assert_eq!(back, doc);
    assert!(sources.is_empty());
}

#[test]
fn layers_get_fresh_ids_in_stack_order() {
    let (doc, sources) = sample_doc();
    let (back, _) = round_trip(&doc, &sources);
    let ids: Vec<_> = back.layers().iter().map(|l| l.id()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(ids, sorted);
    // The loaded document accepts new layers without id clashes.
    let mut back = back;
    let new = back.push_layer(Layer::paint("new", back.canvas_size));
    assert!(!ids.contains(&new));
}

#[test]
fn save_is_deterministic() {
    let (doc, sources) = sample_doc();
    assert_eq!(save_bytes(&doc, &sources), save_bytes(&doc, &sources));
}

#[test]
fn shared_source_is_stored_once() {
    let (mut doc, sources) = sample_doc();
    let image = doc
        .layers()
        .iter()
        .find(|l| matches!(l.kind, rp_doc::LayerKind::Image(_)))
        .unwrap()
        .clone();
    doc.push_layer(image);
    let bytes = save_bytes(&doc, &sources);
    let zip = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
    let n = zip
        .file_names()
        .filter(|n| n.as_ref().unwrap().starts_with("sources/"))
        .count();
    assert_eq!(n, 1);
    check(&doc, &sources);
}

#[test]
fn archive_layout_matches_the_doc() {
    let (doc, sources) = sample_doc();
    let bytes = save_bytes(&doc, &sources);
    let zip = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
    let mut names: Vec<_> = zip.file_names().map(|n| n.unwrap().into_owned()).collect();
    names.sort();
    let hash = sources.hashes().next().unwrap().to_string();
    assert_eq!(
        names,
        [
            "layers/0001.png".to_owned(),
            "layers/0002-mask.png".to_owned(),
            "layers/0003-mask.png".to_owned(),
            "layers/0003.png".to_owned(),
            "layers/0004.png".to_owned(),
            "manifest.json".to_owned(),
            format!("sources/{hash}.png"),
        ]
    );
}

#[test]
fn save_to_path_is_atomic_and_loads_back() {
    let dir = common::temp_dir("save_to_path");
    let path = dir.join("project.rpaint");
    let (doc, sources) = sample_doc();
    save_to_path(&doc, &sources, &path).unwrap();
    let (back, back_sources) = load_from_path(&path).unwrap();
    assert_docs_equal(&doc, &back);
    assert_eq!(rendered(&doc, &sources), rendered(&back, &back_sources));
    assert!(!dir.join("project.rpaint.tmp").exists());

    // A failing save leaves the existing file untouched and no temp file.
    let before = std::fs::read(&path).unwrap();
    let mut bad = doc.clone();
    bad.layout = Layout::Desktop;
    let err = save_to_path(&bad, &SourceStore::new(), &path).unwrap_err();
    assert!(matches!(err, FormatError::MissingSource(_)), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!dir.join("project.rpaint.tmp").exists());
}
