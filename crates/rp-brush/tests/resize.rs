use rp_brush::{Brush, BrushSource, Mask, Shape, VbrParams, generate, load};

fn vbr(shape: Shape, hardness: f32, aspect: f32, angle: f32) -> Brush {
    generate(&VbrParams {
        name: "v".into(),
        shape,
        spacing: 20.0,
        radius: 12.0,
        spikes: 2,
        hardness,
        aspect_ratio: aspect,
        angle,
    })
}

/// A 30x10 bitmap brush: a filled ellipse.
fn ellipse_gbr() -> Brush {
    let (w, h) = (30u32, 10u32);
    let mut data = vec![0u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let u = (x as f32 + 0.5 - 15.0) / 15.0;
            let v = (y as f32 + 0.5 - 5.0) / 5.0;
            if u * u + v * v <= 1.0 {
                data[(y * w + x) as usize] = 255;
            }
        }
    }
    Brush::bitmap("e", 25.0, Mask::new(w, h, data), None)
}

fn sum(m: &Mask) -> f64 {
    m.data.iter().map(|&v| v as f64).sum()
}

/// Second moments of the mask around its center: (xx, yy, xy).
fn moments(m: &Mask) -> (f64, f64, f64) {
    let (cx, cy) = ((m.width as f64 - 1.0) / 2.0, (m.height as f64 - 1.0) / 2.0);
    let (mut xx, mut yy, mut xy, mut t) = (0.0, 0.0, 0.0, 0.0);
    for y in 0..m.height {
        for x in 0..m.width {
            let w = m.get(x, y) as f64;
            let (dx, dy) = (x as f64 - cx, cy - y as f64);
            xx += w * dx * dx;
            yy += w * dy * dy;
            xy += w * dx * dy;
            t += w;
        }
    }
    (xx / t, yy / t, xy / t)
}

fn check_basic(b: &Brush, r: &Brush, d: u32) {
    assert!(r.mask.width >= 1 && r.mask.height >= 1, "size {d}");
    assert_eq!(
        r.mask.data.len(),
        (r.mask.width * r.mask.height) as usize,
        "size {d}"
    );
    assert!(r.mask.data.iter().any(|&v| v > 0), "size {d} is empty");
    assert_eq!(r.name, b.name);
    assert_eq!(r.spacing, b.spacing);
    if d == 1 {
        assert_eq!((r.mask.width, r.mask.height), (1, 1), "size 1");
    }
}

#[test]
fn vbr_sizes_1_to_256() {
    for b in [
        vbr(Shape::Circle, 1.0, 1.0, 0.0),
        vbr(Shape::Circle, 0.0, 1.0, 0.0),
        vbr(Shape::Square, 0.5, 2.5, 30.0),
        vbr(Shape::Diamond, 1.0, 1.0, 45.0),
    ] {
        for d in 1..=256u32 {
            let r = b.resized(d as f32);
            check_basic(&b, &r, d);
            let BrushSource::Parametric(p) = &r.source else {
                panic!("vbr lost its params")
            };
            assert_eq!(p.radius, d as f32 / 2.0);
            assert_eq!(r.diameter(), d as f32);
            // Square corners at 45 degrees reach sqrt(2) further than the radius.
            let max = (d as f32 * std::f32::consts::SQRT_2).ceil() as u32 + 2;
            assert!(r.mask.width.max(r.mask.height) <= max, "size {d}");
        }
    }
}

#[test]
fn hard_circle_matches_diameter() {
    let b = vbr(Shape::Circle, 1.0, 1.0, 0.0);
    for d in 1..=256u32 {
        let m = b.resized(d as f32).mask;
        assert_eq!(m.width, m.height);
        let want = if d % 2 == 1 { d } else { d - 1 };
        assert_eq!(m.width, want.max(1), "size {d}");
    }
}

#[test]
fn vbr_keeps_shape_params_and_aspect_rotation() {
    let b = vbr(Shape::Circle, 0.7, 3.0, 90.0);
    let r = b.resized(101.0);
    let (BrushSource::Parametric(p0), BrushSource::Parametric(p1)) = (&b.source, &r.source) else {
        panic!()
    };
    assert_eq!(
        (p1.shape, p1.hardness, p1.aspect_ratio, p1.angle, p1.spikes),
        (p0.shape, p0.hardness, p0.aspect_ratio, p0.angle, p0.spikes)
    );
    // Rotated 90 degrees: tall and thin, about 3:1.
    let ratio = r.mask.height as f32 / r.mask.width as f32;
    assert!((2.6..=3.4).contains(&ratio), "ratio {ratio}");
    assert!(r.mask.height <= 101);

    // 30 degrees: the principal axis of the mask follows the angle.
    let r = vbr(Shape::Circle, 1.0, 3.0, 30.0).resized(151.0);
    let (xx, yy, xy) = moments(&r.mask);
    let angle = 0.5 * (2.0 * xy).atan2(xx - yy);
    assert!(
        (angle.to_degrees() - 30.0).abs() < 1.0,
        "angle {}",
        angle.to_degrees()
    );
}

#[test]
fn vbr_mask_sum_scales_with_area() {
    for b in [
        vbr(Shape::Circle, 1.0, 1.0, 0.0),
        vbr(Shape::Circle, 0.5, 2.0, 20.0),
    ] {
        let base = b.resized(64.0);
        for d in [16u32, 32, 128, 256] {
            let r = b.resized(d as f32);
            let ratio = sum(&r.mask) / sum(&base.mask);
            let want = (d as f64 / 64.0).powi(2);
            assert!(
                (ratio / want - 1.0).abs() < 0.1,
                "size {d}: {ratio} vs {want}"
            );
        }
    }
    // Hard circle of diameter d covers about pi d^2 / 4 pixels.
    let r = vbr(Shape::Circle, 1.0, 1.0, 0.0).resized(201.0);
    let area = sum(&r.mask) / 255.0;
    let want = std::f64::consts::PI * 100.5 * 100.5;
    assert!((area / want - 1.0).abs() < 0.02, "{area} vs {want}");
}

#[test]
fn gbr_sizes_1_to_256_keep_aspect() {
    let b = ellipse_gbr();
    for d in 1..=256u32 {
        let r = b.resized(d as f32);
        check_basic(&b, &r, d);
        assert_eq!(r.mask.width, d, "size {d}");
        let want_h = ((d as f64 / 3.0).round() as u32).max(1);
        assert_eq!(r.mask.height, want_h, "size {d}");
        assert_eq!(r.diameter(), d as f32);
        assert!(matches!(r.source, BrushSource::Resampled(_)));
    }
    // Tall brushes keep the long side vertical.
    let tall = Brush::bitmap("t", 25.0, Mask::new(4, 12, vec![255; 48]), None);
    let r = tall.resized(6.0);
    assert_eq!((r.mask.width, r.mask.height), (2, 6));
    assert!(r.mask.data.iter().all(|&v| v == 255));
}

#[test]
fn gbr_mask_sum_scales_with_area() {
    let b = ellipse_gbr();
    let s0 = sum(&b.mask);
    for d in [6u32, 12, 15, 21, 60, 90, 150, 256] {
        let r = b.resized(d as f32);
        let want = r.mask.width as f64 * r.mask.height as f64 / 300.0;
        let ratio = sum(&r.mask) / s0;
        assert!(
            (ratio / want - 1.0).abs() < 0.1,
            "size {d}: {ratio} vs {want}"
        );
    }
}

#[test]
fn gbr_same_size_is_identity_and_resizes_from_original() {
    let b = ellipse_gbr();
    assert_eq!(b.resized(30.0).mask, b.mask);
    let direct = b.resized(90.0);
    let chained = b.resized(4.0).resized(90.0);
    assert_eq!(chained.mask, direct.mask);
    assert_eq!(chained.resized(30.0).mask, b.mask);
}

#[test]
fn gbr_downscale_is_area_average_and_upscale_bilinear() {
    // 4x4 checker of 2x2 blocks downscaled to 2x2 averages each block.
    let data = vec![
        255, 255, 0, 0, //
        255, 255, 0, 0, //
        0, 0, 100, 100, //
        0, 0, 100, 100,
    ];
    let b = Brush::bitmap("c", 25.0, Mask::new(4, 4, data), None);
    assert_eq!(b.resized(2.0).mask.data, vec![255, 0, 0, 100]);
    // 4 -> 3 boxes straddle pixels.
    let row = Brush::bitmap("r", 25.0, Mask::new(4, 1, vec![0, 255, 255, 0]), None);
    assert_eq!(row.resized(3.0).mask.data, vec![64, 255, 64]);
    // Upscale of a ramp stays monotonic and spans the ends.
    let ramp = Brush::bitmap("r", 25.0, Mask::new(2, 1, vec![0, 200]), None);
    let up = ramp.resized(8.0).mask;
    assert_eq!((up.width, up.height), (8, 4));
    assert_eq!(up.data[0], 0);
    assert_eq!(up.data[7], 200);
    assert!(up.data[..8].windows(2).all(|w| w[0] <= w[1]));
    assert!(up.data.chunks(8).all(|r| r == &up.data[..8]));
}

#[test]
fn gbr_pixmap_resamples_without_bleeding() {
    // Red where covered, green where transparent: green must not leak in.
    let (w, h) = (8u32, 8u32);
    let mut mask = vec![0u8; 64];
    let mut pm = vec![[0u8, 255, 0]; 64];
    for y in 2..6 {
        for x in 2..6 {
            mask[(y * w + x) as usize] = 255;
            pm[(y * w + x) as usize] = [255, 0, 0];
        }
    }
    let b = Brush::bitmap("p", 25.0, Mask::new(w, h, mask), Some(pm));
    for d in [1u32, 3, 5, 8, 13, 40] {
        let r = b.resized(d as f32);
        let pm = r.pixmap.as_ref().expect("pixmap kept");
        assert_eq!(pm.len(), r.mask.data.len());
        for (i, &m) in r.mask.data.iter().enumerate() {
            if m > 0 {
                assert_eq!(pm[i], [255, 0, 0], "size {d} pixel {i}");
            }
        }
    }
}

#[test]
fn spacing_stays_a_fraction_of_size() {
    let b = vbr(Shape::Circle, 1.0, 1.0, 0.0);
    let r = b.resized(201.0);
    assert_eq!(r.spacing, b.spacing);
    assert_eq!(r.step(), 201.0 * 0.2);
    let g = ellipse_gbr().resized(120.0);
    assert_eq!(g.step(), 30.0);
}

#[test]
fn size_one_and_degenerate_inputs() {
    for b in [
        Brush::round_hard(1.0),
        Brush::round_soft(1.0),
        vbr(Shape::Square, 0.0, 5.0, 45.0).resized(1.0),
        ellipse_gbr().resized(1.0),
        ellipse_gbr().resized(0.0),
        ellipse_gbr().resized(f32::NAN),
        vbr(Shape::Circle, 0.0, 1.0, 0.0).resized(-3.0),
    ] {
        assert_eq!((b.mask.width, b.mask.height), (1, 1));
        assert_eq!(b.mask.data, vec![255]);
    }
}

#[test]
fn built_in_round_brushes() {
    let hard = Brush::round_hard(9.0);
    assert_eq!(hard.name, "Round Hard");
    assert_eq!((hard.mask.width, hard.mask.height), (9, 9));
    assert!(hard.mask.data.iter().all(|&v| v == 0 || v == 255));
    assert_eq!(hard.mask.get(4, 4), 255);
    assert_eq!(hard.mask.get(0, 0), 0);

    let soft = Brush::round_soft(9.0);
    assert_eq!(soft.name, "Round Soft");
    assert_eq!(
        soft.mask.get(soft.mask.width / 2, soft.mask.height / 2),
        255
    );
    assert!(soft.mask.data.iter().any(|&v| v > 0 && v < 255));
    assert!(sum(&soft.mask) < sum(&hard.mask));

    // They resize like any parametric brush and keep their names.
    let big = soft.resized(64.0);
    assert_eq!(big.name, "Round Soft");
    assert_eq!(big.diameter(), 64.0);
    assert!(hard.step() >= 1.0);
}

#[test]
fn loaded_vbr_remembers_params() {
    let text = "GIMP-VBR\n1.0\nRound\n25.0\n5.0\n1.0\n1.0\n0.0\n";
    let b = load(text.as_bytes()).unwrap();
    assert!(matches!(b.source, BrushSource::Parametric(_)));
    assert_eq!(b.diameter(), 10.0);
    assert_eq!(b.resized(21.0).mask.width, 21);
}
