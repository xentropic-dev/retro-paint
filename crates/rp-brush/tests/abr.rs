//! Photoshop `.abr` parsing against small synthetic files built here.

use rp_brush::{
    BrushError, Mask, decode_abr, decode_abr_data, index_abr, load_library, load_library_bytes,
    parse_abr, parse_gih, thumbnail,
};

// ---- builders --------------------------------------------------------------

fn be16(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}
fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// PackBits: runs of 3+ equal bytes as repeats, the rest as literals.
fn packbits(row: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < row.len() {
        let mut run = 1;
        while i + run < row.len() && row[i + run] == row[i] && run < 128 {
            run += 1;
        }
        if run >= 3 {
            out.push((1 - run as i32) as i8 as u8);
            out.push(row[i]);
            i += run;
        } else {
            let start = i;
            while i < row.len() && i - start < 128 {
                if i + 2 < row.len() && row[i] == row[i + 1] && row[i] == row[i + 2] {
                    break;
                }
                i += 1;
            }
            out.push((i - start - 1) as u8);
            out.extend_from_slice(&row[start..i]);
        }
    }
    out
}

/// Bounds, depth, compression and pixel data shared by all versions.
/// `pixels` are already at `depth` (BE u16 pairs for 16-bit).
fn sample_body(w: u32, h: u32, depth: u16, rle: bool, pixels: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    // Non-zero origin: width/height are right - left, bottom - top.
    let (top, left) = (5i32, 7i32);
    for x in [top, left, top + h as i32, left + w as i32] {
        v.extend(be32(x as u32));
    }
    v.extend(be16(depth));
    v.push(rle as u8);
    if rle {
        let row = (w * depth as u32 / 8) as usize;
        let rows: Vec<Vec<u8>> = pixels.chunks(row).map(packbits).collect();
        for r in &rows {
            v.extend(be16(r.len() as u16));
        }
        for r in rows {
            v.extend(r);
        }
    } else {
        v.extend_from_slice(pixels);
    }
    v
}

/// One v6/v10 `samp` record, padded to 4 bytes.
fn v6_sample(sub: u16, uuid: &str, w: u32, h: u32, depth: u16, rle: bool, px: &[u8]) -> Vec<u8> {
    assert_eq!(uuid.len(), 36);
    let mut rec = vec![36u8];
    rec.extend(uuid.as_bytes());
    rec.extend(vec![0u8; if sub == 1 { 10 } else { 264 }]);
    rec.extend(sample_body(w, h, depth, rle, px));
    let mut v = be32(rec.len() as u32).to_vec();
    v.extend(&rec);
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
    v
}

fn section(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = b"8BIM".to_vec();
    v.extend(tag);
    v.extend(be32(body.len() as u32));
    v.extend(body);
    v
}

fn v6_file(version: u16, sub: u16, samples: &[Vec<u8>], extra: &[Vec<u8>]) -> Vec<u8> {
    let mut v = be16(version).to_vec();
    v.extend(be16(sub));
    v.extend(section(b"samp", &samples.concat()));
    for e in extra {
        v.extend(e);
    }
    v
}

fn ucs2(s: &str) -> Vec<u8> {
    let units: Vec<u16> = s.encode_utf16().chain([0]).collect();
    let mut v = be32(units.len() as u32).to_vec();
    for u in units {
        v.extend(be16(u));
    }
    v
}

fn key(k: &str) -> Vec<u8> {
    if k.len() == 4 {
        let mut v = be32(0).to_vec();
        v.extend(k.as_bytes());
        v
    } else {
        let mut v = be32(k.len() as u32).to_vec();
        v.extend(k.as_bytes());
        v
    }
}

/// An `Objc` value body: name, class, items.
fn obj(class: &str, items: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut v = ucs2("");
    v.extend(key(class));
    v.extend(be32(items.len() as u32));
    for (k, val) in items {
        v.extend(key(k));
        v.extend(val);
    }
    v
}

fn t_obj(class: &str, items: &[(&str, Vec<u8>)]) -> Vec<u8> {
    [b"Objc".to_vec(), obj(class, items)].concat()
}
fn t_text(s: &str) -> Vec<u8> {
    [b"TEXT".to_vec(), ucs2(s)].concat()
}
fn t_untf(unit: &str, x: f64) -> Vec<u8> {
    [
        b"UntF".to_vec(),
        unit.as_bytes().to_vec(),
        x.to_be_bytes().to_vec(),
    ]
    .concat()
}
fn t_bool(b: bool) -> Vec<u8> {
    vec![b'b', b'o', b'o', b'l', b as u8]
}
fn t_enum(t: &str, e: &str) -> Vec<u8> {
    [b"enum".to_vec(), key(t), key(e)].concat()
}
fn t_long(x: i32) -> Vec<u8> {
    [b"long".to_vec(), be32(x as u32).to_vec()].concat()
}

fn sampled_preset(name: &str, uuid: &str, spacing: f64) -> Vec<u8> {
    t_obj(
        "brushPreset",
        &[
            ("Nm  ", t_text(name)),
            (
                "Brsh",
                t_obj(
                    "sampledBrush",
                    &[
                        ("Dmtr", t_untf("#Pxl", 100.0)),
                        ("Nm  ", t_text("inner name")),
                        ("Spcn", t_untf("#Prc", spacing)),
                        ("Intr", t_bool(true)),
                        ("sampledData", t_text(uuid)),
                    ],
                ),
            ),
            ("useTipDynamics", t_bool(false)),
            ("mode", t_enum("BlnM", "Nrml")),
            ("count", t_long(3)),
        ],
    )
}

fn computed_preset(name: &str) -> Vec<u8> {
    t_obj(
        "brushPreset",
        &[
            ("Nm  ", t_text(name)),
            (
                "Brsh",
                t_obj(
                    "computedBrush",
                    &[
                        ("Dmtr", t_untf("#Pxl", 9.0)),
                        ("Hrdn", t_untf("#Prc", 100.0)),
                    ],
                ),
            ),
        ],
    )
}

fn desc(presets: &[Vec<u8>]) -> Vec<u8> {
    let mut list = b"VlLs".to_vec();
    list.extend(be32(presets.len() as u32));
    for p in presets {
        list.extend(p);
    }
    let mut v = be32(16).to_vec();
    v.extend(obj("null", &[("Brsh", list)]));
    section(b"desc", &v)
}

const U1: &str = "11111111-aaaa-bbbb-cccc-000000000001";
const U2: &str = "22222222-aaaa-bbbb-cccc-000000000002";

/// A 6x4 splat: 255 in the middle, a gradient row, flat zero rows.
fn px6x4() -> Vec<u8> {
    let mut v = vec![0u8; 6];
    v.extend([0, 255, 255, 255, 255, 0]);
    v.extend([10, 20, 30, 40, 50, 60]);
    v.extend([0u8; 6]);
    v
}

fn px3x2() -> Vec<u8> {
    vec![1, 2, 3, 200, 200, 200]
}

/// v6.2: one RLE and one raw sample, no desc.
fn v6_rle_raw() -> Vec<u8> {
    v6_file(
        6,
        2,
        &[
            v6_sample(2, U1, 6, 4, 8, true, &px6x4()),
            v6_sample(2, U2, 3, 2, 8, false, &px3x2()),
        ],
        &[],
    )
}

// ---- tests -------------------------------------------------------------------

#[test]
fn v6_rle_and_raw_without_desc_get_default_names() {
    let data = v6_rle_raw();
    let idx = index_abr(&data, "Splats").unwrap();
    assert_eq!((idx.version, idx.subversion), (6, 2));
    assert_eq!(idx.entries.len(), 2);
    let (a, b) = (&idx.entries[0], &idx.entries[1]);
    assert_eq!(a.name, "Splats 1");
    assert_eq!(b.name, "Splats 2");
    assert_eq!((a.width, a.height, a.depth, a.compressed), (6, 4, 8, true));
    assert_eq!((b.width, b.height, b.depth, b.compressed), (3, 2, 8, false));
    assert_eq!(a.uuid.as_deref(), Some(U1));
    assert_eq!(a.spacing, 25.0);

    let ba = decode_abr(&data, a).unwrap();
    assert_eq!(ba.name, "Splats 1");
    assert_eq!(ba.spacing, 25.0);
    assert_eq!(ba.mask, Mask::new(6, 4, px6x4()));
    assert!(ba.pixmap.is_none());
    let bb = decode_abr(&data, b).unwrap();
    assert_eq!(bb.mask, Mask::new(3, 2, px3x2()));
}

#[test]
fn decode_from_just_the_data_range_matches() {
    let data = v6_rle_raw();
    let idx = index_abr(&data, "s").unwrap();
    for e in &idx.entries {
        let slice = &data[e.data.clone()];
        assert_eq!(
            decode_abr_data(slice, e).unwrap(),
            decode_abr(&data, e).unwrap()
        );
    }
}

#[test]
fn v6_subversion_1() {
    let data = v6_file(6, 1, &[v6_sample(1, U1, 3, 2, 8, false, &px3x2())], &[]);
    let b = parse_abr(&data, "x").unwrap();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].mask, Mask::new(3, 2, px3x2()));
}

#[test]
fn v10_desc_names_and_spacing_matched_by_uuid() {
    // Presets list the samples out of order, plus a computed preset that
    // has no sample.
    let data = v6_file(
        10,
        2,
        &[
            v6_sample(2, U1, 6, 4, 8, true, &px6x4()),
            v6_sample(2, U2, 3, 2, 8, true, &px3x2()),
        ],
        &[
            section(b"patt", &[]),
            desc(&[
                computed_preset("Hard Round"),
                sampled_preset("Second Splat", U2, 40.0),
                sampled_preset("First Splat", U1, 150.0),
            ]),
            section(b"phry", &[0, 0, 0, 1]),
        ],
    );
    // Entries follow the preset order, as Photoshop lists them.
    let idx = index_abr(&data, "file").unwrap();
    let names: Vec<_> = idx.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Second Splat", "First Splat"]);
    assert_eq!(idx.entries[0].spacing, 40.0);
    assert_eq!(idx.entries[1].spacing, 150.0);
    assert_eq!(idx.entries[0].uuid.as_deref(), Some(U2));
    let all = parse_abr(&data, "file").unwrap();
    assert_eq!(all[0].name, "Second Splat");
    assert_eq!(all[0].spacing, 40.0);
    assert_eq!(all[0].mask, Mask::new(3, 2, px3x2()));
    assert_eq!(all[1].mask, Mask::new(6, 4, px6x4()));
}

#[test]
fn samples_without_a_preset_follow_and_duplicate_names_get_numbered() {
    const U3: &str = "33333333-aaaa-bbbb-cccc-000000000003";
    let data = v6_file(
        10,
        2,
        &[
            v6_sample(2, U1, 3, 2, 8, false, &px3x2()),
            v6_sample(2, U2, 3, 2, 8, false, &px3x2()),
            v6_sample(2, U3, 3, 2, 8, false, &px3x2()),
        ],
        &[desc(&[
            sampled_preset("Ink", U3, 10.0),
            sampled_preset("Ink", U1, 10.0),
        ])],
    );
    let idx = index_abr(&data, "pack").unwrap();
    let names: Vec<_> = idx.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Ink", "Ink #2", "pack 3"]);
    assert_eq!(idx.entries[2].uuid.as_deref(), Some(U2));
    assert_eq!(idx.entries[2].spacing, 25.0);
}

#[test]
fn unreadable_desc_falls_back_to_default_names() {
    let mut bad = desc(&[sampled_preset("Named", U1, 40.0)]);
    let n = bad.len();
    bad[n - 30] = b'?'; // corrupt a type code / payload
    bad.truncate(n - 10);
    let len = (bad.len() - 12) as u32;
    bad[8..12].copy_from_slice(&be32(len));
    let data = v6_file(10, 2, &[v6_sample(2, U1, 3, 2, 8, false, &px3x2())], &[bad]);
    let idx = index_abr(&data, "stem").unwrap();
    assert_eq!(idx.entries[0].name, "stem 1");
    assert_eq!(idx.entries[0].spacing, 25.0);
}

#[test]
fn sixteen_bit_samples_reduce_to_eight() {
    let vals: [u16; 6] = [0, 0xffff, 0x8080, 0x0101, 0xfefe, 0x8000];
    let px: Vec<u8> = vals.iter().flat_map(|v| v.to_be_bytes()).collect();
    let want = vec![0, 255, 128, 1, 254, 128];
    for rle in [false, true] {
        let data = v6_file(6, 2, &[v6_sample(2, U1, 3, 2, 16, rle, &px)], &[]);
        let idx = index_abr(&data, "d").unwrap();
        assert_eq!(idx.entries[0].depth, 16);
        let b = decode_abr(&data, &idx.entries[0]).unwrap();
        assert_eq!(b.mask, Mask::new(3, 2, want.clone()), "rle {rle}");
    }
}

fn v12_sampled(
    version: u16,
    name: &str,
    spacing: u16,
    w: u32,
    h: u32,
    rle: bool,
    px: &[u8],
) -> Vec<u8> {
    let mut body = be32(0).to_vec(); // misc
    body.extend(be16(spacing));
    if version == 2 {
        body.extend(ucs2(name));
    }
    body.push(1); // antialiasing
    body.extend([0u8; 8]); // short bounds
    body.extend(sample_body(w, h, 8, rle, px));
    let mut v = be16(2).to_vec();
    v.extend(be32(body.len() as u32));
    v.extend(body);
    v
}

fn v12_computed() -> Vec<u8> {
    let body = [0u8; 14];
    let mut v = be16(1).to_vec();
    v.extend(be32(body.len() as u32));
    v.extend(body);
    v
}

#[test]
fn v2_names_and_spacing_and_v1_defaults() {
    let mut data = be16(2).to_vec();
    data.extend(be16(3));
    data.extend(v12_computed());
    data.extend(v12_sampled(2, "Drip", 30, 6, 4, true, &px6x4()));
    data.extend(v12_sampled(2, "", 50, 3, 2, false, &px3x2()));
    let idx = index_abr(&data, "old").unwrap();
    assert_eq!(idx.entries.len(), 2);
    assert_eq!(idx.entries[0].name, "Drip");
    assert_eq!(idx.entries[0].spacing, 30.0);
    assert_eq!(idx.entries[1].name, "old 2");
    let all = parse_abr(&data, "old").unwrap();
    assert_eq!(all[0].mask, Mask::new(6, 4, px6x4()));
    assert_eq!(all[1].mask, Mask::new(3, 2, px3x2()));

    let mut v1 = be16(1).to_vec();
    v1.extend(be16(1));
    v1.extend(v12_sampled(1, "", 20, 3, 2, false, &px3x2()));
    let b = parse_abr(&v1, "ancient").unwrap();
    assert_eq!(b[0].name, "ancient 1");
    assert_eq!(b[0].spacing, 20.0);
    assert_eq!(b[0].mask, Mask::new(3, 2, px3x2()));
}

/// Every truncation and many single-byte corruptions return an error or a
/// brush; none panic.
#[test]
fn malformed_input_errors_without_panicking() {
    let files = [
        v6_rle_raw(),
        v6_file(
            10,
            2,
            &[v6_sample(2, U1, 6, 4, 16, true, &[7u8; 48])],
            &[desc(&[sampled_preset("n", U1, 10.0)])],
        ),
    ];
    for data in &files {
        for n in 0..data.len() {
            let cut = &data[..n];
            if let Ok(idx) = index_abr(cut, "t") {
                for e in &idx.entries {
                    let _ = decode_abr(cut, e);
                }
            }
        }
        for i in 0..data.len() {
            for x in [0x00, 0x7f, 0x80, 0xff] {
                let mut bad = data.clone();
                bad[i] = x;
                if let Ok(idx) = index_abr(&bad, "t") {
                    for e in &idx.entries {
                        let _ = decode_abr(&bad, e);
                    }
                }
            }
        }
    }
    let data = v6_rle_raw();
    assert_eq!(index_abr(&data[..3], "t"), Err(BrushError::Truncated));
    assert!(matches!(
        index_abr(&data[..data.len() - 1], "t"),
        Err(BrushError::Truncated)
    ));
    let mut v = data.clone();
    v[1] = 7;
    assert!(matches!(
        index_abr(&v, "t"),
        Err(BrushError::Unsupported(_))
    ));
    let mut v = data.clone();
    v[4] = b'X'; // 8BIM signature
    assert!(matches!(index_abr(&v, "t"), Err(BrushError::Invalid(_))));
    assert!(parse_abr(b"", "t").is_err());
}

#[test]
fn corrupt_rle_is_an_error() {
    let data = v6_rle_raw();
    let idx = index_abr(&data, "t").unwrap();
    let e = &idx.entries[0];
    let mut bad = data.clone();
    // First row-length entry: claim a longer row than packed.
    bad[e.data.start + 1] += 1;
    assert!(decode_abr(&bad, e).is_err());
}

#[test]
fn thumbnail_area_averages_and_never_upscales() {
    let small = Mask::new(3, 2, px3x2());
    assert_eq!(thumbnail(&small, 64), small);

    // 256x128 checkerboard of single pixels averages to mid gray.
    let (w, h) = (256u32, 128u32);
    let data = (0..w * h)
        .map(|i| if (i % w + i / w) % 2 == 0 { 255 } else { 0 })
        .collect();
    let t = thumbnail(&Mask::new(w, h, data), 64);
    assert_eq!((t.width, t.height), (64, 32));
    assert!(
        t.data.iter().all(|&v| (127..=128).contains(&v)),
        "{:?}",
        &t.data[..8]
    );
}

#[test]
fn huge_sample_resized_small_does_not_alias() {
    // 2400 px wide stripes, one pixel each: point sampling would give all 0
    // or all 255; area averaging gives gray.
    let (w, h) = (2400u32, 600u32);
    let px: Vec<u8> = (0..w * h)
        .map(|i| if i % 2 == 0 { 255 } else { 0 })
        .collect();
    let data = v6_file(10, 2, &[v6_sample(2, U1, w, h, 8, true, &px)], &[]);
    let idx = index_abr(&data, "big").unwrap();
    let b = decode_abr(&data, &idx.entries[0]).unwrap();
    let small = b.resized(24.0);
    assert_eq!((small.mask.width, small.mask.height), (24, 6));
    assert!(small.mask.data.iter().all(|&v| (127..=128).contains(&v)));
}

fn gbr_v2(name: &str, w: u32, h: u32, px: &[u8]) -> Vec<u8> {
    let n = [name.as_bytes(), b"\0"].concat();
    let mut v = Vec::new();
    for x in [28 + n.len() as u32, 2, w, h, 1] {
        v.extend(be32(x));
    }
    v.extend(b"GIMP");
    v.extend(be32(30));
    v.extend(n);
    v.extend(px);
    v
}

#[test]
fn gih_first_cell_named_after_hose() {
    let mut gih = b"Sparkle Hose\n2 ncells:2 cellwidth:2 cellheight:1 step:100 dim:1 rank0:2 placement:constant\n".to_vec();
    gih.extend(gbr_v2("cell 0", 2, 1, &[9, 99]));
    gih.extend(gbr_v2("cell 1", 2, 1, &[1, 2]));
    let b = parse_gih(&gih).unwrap();
    assert_eq!(b.name, "Sparkle Hose");
    assert_eq!(b.mask, Mask::new(2, 1, vec![9, 99]));
    assert!(parse_gih(b"only a name").is_err());
}

#[test]
fn library_loads_every_format() {
    let gbr = gbr_v2("g", 2, 1, &[1, 2]);
    assert_eq!(load_library_bytes(&gbr, "GBR", "s").unwrap()[0].name, "g");
    let vbr = b"GIMP-VBR\n1.0\nRound\n25\n5\n1\n1\n0\n";
    assert_eq!(
        load_library_bytes(vbr, "vbr", "s").unwrap()[0].name,
        "Round"
    );
    let mut gih = b"Hose\nncells:1\n".to_vec();
    gih.extend(&gbr);
    assert_eq!(
        load_library_bytes(&gih, "gih", "s").unwrap()[0].name,
        "Hose"
    );
    let abr = v6_rle_raw();
    let all = load_library_bytes(&abr, "abr", "Pack").unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[1].name, "Pack 2");
    assert!(matches!(
        load_library_bytes(&abr, "png", "s"),
        Err(BrushError::Unsupported(_))
    ));

    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let path = dir.join("Ink Pack.abr");
    std::fs::write(&path, &abr).unwrap();
    let from_file = load_library(&path).unwrap();
    assert_eq!(from_file[0].name, "Ink Pack 1");
    assert!(matches!(
        load_library(&dir.join("missing.abr")),
        Err(BrushError::Io(_))
    ));
}
