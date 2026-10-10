use rp_brush::{Brush, Mask, Spacer, stamp, stroke_dabs};

fn gray(w: u32, h: u32, data: Vec<u8>) -> Brush {
    Brush {
        name: "g".into(),
        spacing: 25.0,
        mask: Mask::new(w, h, data),
        pixmap: None,
    }
}

fn px(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

#[test]
fn stamp_centers_and_paints_color() {
    let b = gray(3, 3, vec![0, 255, 0, 255, 255, 255, 0, 255, 0]);
    let mut buf = vec![0u8; 5 * 5 * 4];
    stamp(&mut buf, 5, 5, &b, 2.4, 2.9, [200, 10, 20, 255], 1.0);
    for y in 0..5 {
        for x in 0..5 {
            let plus = (x == 2 && (1..=3).contains(&y)) || (y == 2 && (1..=3).contains(&x));
            let want = if plus {
                [200, 10, 20, 255]
            } else {
                [0, 0, 0, 0]
            };
            assert_eq!(px(&buf, 5, x, y), want, "({x},{y})");
        }
    }
}

#[test]
fn stamp_blends_source_over() {
    let b = gray(1, 1, vec![128]);
    let mut buf = vec![0, 0, 255, 255];
    stamp(&mut buf, 1, 1, &b, 0.0, 0.0, [255, 0, 0, 255], 1.0);
    assert_eq!(px(&buf, 1, 0, 0), [128, 0, 127, 255]);
    // Opacity and color alpha scale coverage; on transparent the color is kept.
    let mut buf = vec![0u8; 4];
    stamp(
        &mut buf,
        1,
        1,
        &gray(1, 1, vec![255]),
        0.0,
        0.0,
        [9, 8, 7, 128],
        0.5,
    );
    assert_eq!(px(&buf, 1, 0, 0), [9, 8, 7, 64]);
}

#[test]
fn stamp_clips_at_edges() {
    let b = gray(3, 3, vec![255; 9]);
    let mut buf = vec![0u8; 2 * 2 * 4];
    stamp(&mut buf, 2, 2, &b, 0.0, 0.0, [1, 2, 3, 255], 1.0);
    assert_eq!(px(&buf, 2, 0, 0), [1, 2, 3, 255]);
    assert_eq!(px(&buf, 2, 1, 1), [1, 2, 3, 255]);
    stamp(&mut buf, 2, 2, &b, -10.0, 50.0, [9, 9, 9, 255], 1.0);
    assert_eq!(px(&buf, 2, 0, 0), [1, 2, 3, 255]);
}

#[test]
fn stamp_rgba_brush_uses_pixmap() {
    let b = Brush {
        name: "c".into(),
        spacing: 25.0,
        mask: Mask::new(2, 1, vec![255, 0]),
        pixmap: Some(vec![[10, 20, 30], [40, 50, 60]]),
    };
    let mut buf = vec![0u8; 2 * 4];
    stamp(&mut buf, 2, 1, &b, 1.0, 0.0, [255, 255, 255, 255], 1.0);
    assert_eq!(px(&buf, 2, 0, 0), [10, 20, 30, 255]);
    assert_eq!(px(&buf, 2, 1, 0), [0, 0, 0, 0]);
}

#[test]
fn brush_step_uses_spacing_percent() {
    let mut b = gray(20, 10, vec![255; 200]);
    b.spacing = 25.0;
    assert_eq!(b.step(), 5.0);
    b.spacing = 1.0;
    assert_eq!(b.step(), 1.0);
}

#[test]
fn stroke_dabs_evenly_spaced() {
    let dabs = stroke_dabs(&[(0.0, 0.0), (10.0, 0.0)], 2.5);
    assert_eq!(
        dabs,
        vec![(0.0, 0.0), (2.5, 0.0), (5.0, 0.0), (7.5, 0.0), (10.0, 0.0)]
    );
}

#[test]
fn spacing_carries_across_segments() {
    // Path of total length 9 split into uneven pieces, step 3.
    let dabs = stroke_dabs(&[(0.0, 0.0), (1.0, 0.0), (2.0, 0.0), (2.0, 7.0)], 3.0);
    assert_eq!(dabs, vec![(0.0, 0.0), (2.0, 1.0), (2.0, 4.0), (2.0, 7.0)]);
}

#[test]
fn spacer_streams_and_ignores_repeats() {
    let mut s = Spacer::new(4.0);
    assert_eq!(s.push((1.0, 1.0)), vec![(1.0, 1.0)]);
    assert!(s.push((1.0, 1.0)).is_empty());
    assert!(s.push((1.0, 4.0)).is_empty());
    assert_eq!(s.push((1.0, 9.0)), vec![(1.0, 5.0), (1.0, 9.0)]);
}

#[test]
fn diagonal_stroke_dab_count() {
    let dabs = stroke_dabs(&[(0.0, 0.0), (30.0, 40.0)], 5.0);
    assert_eq!(dabs.len(), 11);
    let (x, y) = dabs[10];
    assert!((x - 30.0).abs() < 1e-4 && (y - 40.0).abs() < 1e-4);
}
