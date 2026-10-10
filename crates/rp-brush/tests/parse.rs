use rp_brush::{Brush, BrushError, Shape, VbrParams, parse_gbr, parse_vbr};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

#[test]
fn gbr_v2_grayscale() {
    let b = parse_gbr(&fixture("v2-gray.gbr")).unwrap();
    assert_eq!(b.name, "v2 gray");
    assert_eq!(b.spacing, 75.0);
    assert_eq!((b.mask.width, b.mask.height), (4, 3));
    assert_eq!(b.mask.data, (0..12).map(|i| i * 20).collect::<Vec<u8>>());
    assert!(b.pixmap.is_none());
}

#[test]
fn gbr_v1_grayscale_defaults_spacing() {
    let b = parse_gbr(&fixture("v1-gray.gbr")).unwrap();
    assert_eq!(b.name, "v1 gray");
    assert_eq!(b.spacing, 25.0);
    assert_eq!((b.mask.width, b.mask.height), (3, 2));
    assert_eq!(b.mask.data, vec![0, 128, 255, 10, 20, 30]);
}

#[test]
fn gbr_v2_rgba_splits_mask_and_pixmap() {
    let b = parse_gbr(&fixture("v2-rgba.gbr")).unwrap();
    assert_eq!(b.name, "v2 rgba");
    assert_eq!(b.spacing, 40.0);
    assert_eq!((b.mask.width, b.mask.height), (2, 2));
    assert_eq!(b.mask.data, vec![255, 128, 0, 64]);
    let pm = b.pixmap.unwrap();
    assert_eq!(
        pm,
        vec![[255, 0, 0], [0, 255, 0], [0, 0, 255], [10, 20, 30]]
    );
}

#[test]
fn gbr_rejects_truncated_and_bad_headers() {
    let data = fixture("v2-gray.gbr");
    assert!(matches!(
        parse_gbr(&data[..data.len() - 1]),
        Err(BrushError::Truncated)
    ));
    assert!(matches!(parse_gbr(&data[..10]), Err(BrushError::Truncated)));
    let mut bad = data.clone();
    bad[7] = 9; // version 9
    assert!(matches!(parse_gbr(&bad), Err(BrushError::Unsupported(_))));
    let mut bad = data.clone();
    bad[19] = 3; // 3 bytes per pixel
    assert!(matches!(parse_gbr(&bad), Err(BrushError::Unsupported(_))));
    let mut bad = data;
    bad[20] = b'X'; // magic
    assert!(matches!(parse_gbr(&bad), Err(BrushError::Invalid(_))));
}

#[test]
fn vbr_v15_star() {
    let text = String::from_utf8(fixture("star.vbr")).unwrap();
    let (p, b) = parse_vbr(&text).unwrap();
    assert_eq!(
        p,
        VbrParams {
            name: "test star".into(),
            shape: Shape::Diamond,
            spacing: 50.0,
            radius: 25.0,
            spikes: 5,
            hardness: 1.0,
            aspect_ratio: 2.5,
            angle: 17.5,
        }
    );
    assert_eq!(b.name, "test star");
    assert_eq!(b.spacing, 50.0);
    // Generated masks are odd-sized with the center lit.
    assert_eq!(b.mask.width % 2, 1);
    assert_eq!(b.mask.height % 2, 1);
    assert_eq!(b.mask.get(b.mask.width / 2, b.mask.height / 2), 255);
}

#[test]
fn vbr_v10_defaults_to_circle() {
    let text = String::from_utf8(fixture("soft.vbr")).unwrap();
    let (p, b) = parse_vbr(&text).unwrap();
    assert_eq!(p.shape, Shape::Circle);
    assert_eq!(p.spikes, 2);
    assert_eq!(p.radius, 25.0);
    assert_eq!(p.hardness, 0.5);
    assert_eq!(p.spacing, 10.0);
    // Soft brush: center full, falls off monotonically along the x axis to zero at the rim.
    let cy = b.mask.height / 2;
    let cx = b.mask.width / 2;
    assert_eq!(b.mask.get(cx, cy), 255);
    let row: Vec<u8> = (cx..b.mask.width).map(|x| b.mask.get(x, cy)).collect();
    assert!(row.windows(2).all(|w| w[0] >= w[1]), "{row:?}");
    assert!(row[12] > 0 && row[12] < 255, "{row:?}");
    assert!(*row.last().unwrap() < 5, "{row:?}");
}

#[test]
fn vbr_one_pixel_trims_to_1x1() {
    let text = String::from_utf8(fixture("one-pixel.vbr")).unwrap();
    let (_, b) = parse_vbr(&text).unwrap();
    assert_eq!((b.mask.width, b.mask.height), (1, 1));
    assert_eq!(b.mask.data, vec![255]);
}

#[test]
fn vbr_rejects_garbage() {
    assert!(parse_vbr("hello").is_err());
    assert!(parse_vbr("GIMP-VBR\n1.0\nname\n10\n").is_err());
    assert!(parse_vbr("GIMP-VBR\n1.5\nn\nblob\n10\n5\n2\n1\n1\n0\n").is_err());
    assert!(parse_vbr("GIMP-VBR\n2.0\nn\n10\n5\n1\n1\n0\n").is_err());
}

#[test]
fn load_sniffs_format() {
    let b: Brush = rp_brush::load(&fixture("star.vbr")).unwrap();
    assert_eq!(b.name, "test star");
    let b = rp_brush::load(&fixture("v2-gray.gbr")).unwrap();
    assert_eq!(b.name, "v2 gray");
}
