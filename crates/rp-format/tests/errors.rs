//! Bad files and bad documents give typed errors, never panics; newer
//! versions are refused; older versions migrate.

mod common;

use std::io::Cursor;

use common::{assert_docs_equal, edit_manifest, rendered, rewrite, sample_doc, save_bytes};
use rp_doc::{LayerKind, RgbaImage, Size, SourceHash, SourceStore};
use rp_format::{FORMAT_VERSION, FormatError, load, save};
use serde_json::json;

fn load_err(bytes: Vec<u8>) -> FormatError {
    match load(Cursor::new(bytes)) {
        Ok(_) => panic!("load unexpectedly succeeded"),
        Err(e) => e,
    }
}

#[test]
fn refuses_newer_version_naming_both() {
    let (doc, sources) = sample_doc();
    let bytes = edit_manifest(&save_bytes(&doc, &sources), |v| {
        v["format_version"] = json!(FORMAT_VERSION + 1);
    });
    let err = load_err(bytes);
    assert!(
        matches!(
            err,
            FormatError::NewerVersion {
                found: 2,
                supported: 1
            }
        ),
        "{err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("version 2") && msg.contains("version 1"),
        "{msg}"
    );
}

#[test]
fn v0_file_migrates_and_loads() {
    let (doc, sources) = sample_doc();
    let bytes = edit_manifest(&save_bytes(&doc, &sources), |v| {
        let m = v.as_object_mut().unwrap();
        let canvas = m.remove("canvas").unwrap();
        let output = m.remove("output").unwrap();
        m.insert("format_version".into(), json!(0));
        m.insert("canvas_width".into(), canvas["width"].clone());
        m.insert("canvas_height".into(), canvas["height"].clone());
        m.insert("output_width".into(), output["width"].clone());
        m.insert("output_height".into(), output["height"].clone());
    });
    let (back, back_sources) = load(Cursor::new(bytes)).unwrap();
    assert_docs_equal(&doc, &back);
    assert_eq!(rendered(&doc, &sources), rendered(&back, &back_sources));
}

#[test]
fn not_a_zip() {
    assert!(matches!(load_err(b"hello".to_vec()), FormatError::Zip(_)));
    assert!(matches!(load_err(Vec::new()), FormatError::Zip(_)));
}

#[test]
fn truncated_zip() {
    let (doc, sources) = sample_doc();
    let bytes = save_bytes(&doc, &sources);
    for cut in [10, bytes.len() / 2, bytes.len() - 10] {
        let err = load_err(bytes[..cut].to_vec());
        assert!(matches!(err, FormatError::Zip(_)), "cut {cut}: {err:?}");
    }
}

#[test]
fn flipped_bytes_never_panic() {
    let (doc, sources) = sample_doc();
    let bytes = save_bytes(&doc, &sources);
    for i in (0..bytes.len()).step_by(37) {
        let mut b = bytes.clone();
        b[i] ^= 0x5a;
        // Any outcome but a panic is fine; a CRC or decode error is expected.
        let _ = load(Cursor::new(b));
    }
}

#[test]
fn missing_manifest() {
    let (doc, sources) = sample_doc();
    let bytes = rewrite(&save_bytes(&doc, &sources), |name, data| {
        (name != "manifest.json").then_some(data)
    });
    assert!(matches!(
        load_err(bytes),
        FormatError::MissingEntry { name } if name == "manifest.json"
    ));
}

#[test]
fn manifest_not_json() {
    let (doc, sources) = sample_doc();
    let bytes = rewrite(&save_bytes(&doc, &sources), |name, data| {
        Some(if name == "manifest.json" {
            b"{oops".to_vec()
        } else {
            data
        })
    });
    assert!(matches!(load_err(bytes), FormatError::Json(_)));
}

#[test]
fn manifest_without_version() {
    let (doc, sources) = sample_doc();
    let bytes = edit_manifest(&save_bytes(&doc, &sources), |v| {
        v.as_object_mut().unwrap().remove("format_version");
    });
    assert!(matches!(load_err(bytes), FormatError::InvalidManifest(_)));
}

#[test]
fn manifest_schema_errors() {
    let (doc, sources) = sample_doc();
    let bytes = save_bytes(&doc, &sources);
    type Edit = fn(&mut serde_json::Value);
    let cases: [(&str, Edit); 6] = [
        ("missing layers", |v| {
            v.as_object_mut().unwrap().remove("layers");
        }),
        ("bad layout", |v| v["layout"] = json!("sideways")),
        ("bad cga name", |v| {
            v["dither"]["palette"] = json!({"kind": "custom", "colors": ["mauve"]})
        }),
        ("bad blend", |v| v["layers"][0]["blend"] = json!("dissolve")),
        ("bad color", |v| v["fg"] = json!("red")),
        ("opacity > 100", |v| v["layers"][0]["opacity"] = json!(150)),
    ];
    for (what, edit) in cases {
        let err = load_err(edit_manifest(&bytes, edit));
        assert!(
            matches!(err, FormatError::Json(_) | FormatError::InvalidManifest(_)),
            "{what}: {err:?}"
        );
    }
    let empty_custom = edit_manifest(&bytes, |v| {
        v["dither"]["palette"] = json!({"kind": "custom", "colors": []})
    });
    assert!(matches!(
        load_err(empty_custom),
        FormatError::InvalidManifest(_)
    ));
}

#[test]
fn missing_layer_png() {
    let (doc, sources) = sample_doc();
    let bytes = rewrite(&save_bytes(&doc, &sources), |name, data| {
        (name != "layers/0003.png").then_some(data)
    });
    assert!(matches!(
        load_err(bytes),
        FormatError::MissingEntry { name } if name == "layers/0003.png"
    ));
}

#[test]
fn missing_mask_png() {
    let (doc, sources) = sample_doc();
    let bytes = rewrite(&save_bytes(&doc, &sources), |name, data| {
        (name != "layers/0002-mask.png").then_some(data)
    });
    assert!(matches!(
        load_err(bytes),
        FormatError::MissingEntry { name } if name == "layers/0002-mask.png"
    ));
}

#[test]
fn missing_source_png() {
    let (doc, sources) = sample_doc();
    let bytes = rewrite(&save_bytes(&doc, &sources), |name, data| {
        (!name.starts_with("sources/")).then_some(data)
    });
    assert!(matches!(
        load_err(bytes),
        FormatError::MissingEntry { name } if name.starts_with("sources/")
    ));
}

#[test]
fn corrupt_layer_png() {
    let (doc, sources) = sample_doc();
    let bytes = rewrite(&save_bytes(&doc, &sources), |name, data| {
        Some(if name == "layers/0001.png" {
            data[..data.len() / 2].to_vec()
        } else {
            data
        })
    });
    assert!(matches!(
        load_err(bytes),
        FormatError::InvalidImage { entry, .. } if entry == "layers/0001.png"
    ));
}

#[test]
fn source_pixels_not_matching_hash() {
    let (doc, sources) = sample_doc();
    let other = encode_png(&RgbaImage::filled(Size::new(2, 2), [1, 2, 3, 4]));
    let bytes = rewrite(&save_bytes(&doc, &sources), |name, data| {
        Some(if name.starts_with("sources/") {
            other.clone()
        } else {
            data
        })
    });
    let expected = sources.hashes().next().unwrap().clone();
    assert!(matches!(
        load_err(bytes),
        FormatError::HashMismatch { expected: e, .. } if e == expected
    ));
}

#[test]
fn malformed_source_hash_in_manifest() {
    let (doc, sources) = sample_doc();
    let bytes = edit_manifest(&save_bytes(&doc, &sources), |v| {
        v["layers"][1]["source"] = json!("../../etc/passwd");
    });
    assert!(matches!(load_err(bytes), FormatError::BadSourceHash(_)));
}

#[test]
fn save_refuses_missing_wrong_and_malformed_sources() {
    let (mut doc, sources) = sample_doc();
    let mut out = Cursor::new(Vec::new());
    assert!(matches!(
        save(&doc, &SourceStore::new(), &mut out),
        Err(FormatError::MissingSource(_))
    ));

    let hash = sources.hashes().next().unwrap().clone();
    let mut wrong = SourceStore::new();
    wrong.insert(hash, RgbaImage::new(Size::new(1, 1)));
    assert!(matches!(
        save(&doc, &wrong, &mut out),
        Err(FormatError::HashMismatch { .. })
    ));

    for l in doc.layers_mut() {
        if let LayerKind::Image(i) = &mut l.kind {
            i.source = SourceHash::new("cafe");
        }
    }
    assert!(matches!(
        save(&doc, &sources, &mut out),
        Err(FormatError::BadSourceHash(_))
    ));
}

#[test]
fn save_refuses_non_finite_floats() {
    let (mut doc, sources) = sample_doc();
    doc.dither.strength = f32::NAN;
    let mut out = Cursor::new(Vec::new());
    let err = save(&doc, &sources, &mut out).unwrap_err();
    assert!(
        matches!(&err, FormatError::NonFinite { field } if field == "dither.strength"),
        "{err:?}"
    );

    let (mut doc, sources) = sample_doc();
    for l in doc.layers_mut() {
        if let LayerKind::Image(i) = &mut l.kind {
            i.transform.rotation = f32::INFINITY;
        }
    }
    let err = save(&doc, &sources, &mut out).unwrap_err();
    assert!(
        matches!(&err, FormatError::NonFinite { field } if field == "layers[1].transform.rotation"),
        "{err:?}"
    );
}

fn encode_png(img: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, img.width(), img.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .unwrap()
        .write_image_data(img.as_raw())
        .unwrap();
    out
}
