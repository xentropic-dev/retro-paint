use rp_brush::{Mask, Shape, VbrParams, generate};

fn params(shape: Shape, radius: f32) -> VbrParams {
    VbrParams {
        name: "t".into(),
        shape,
        spacing: 25.0,
        radius,
        spikes: 2,
        hardness: 1.0,
        aspect_ratio: 1.0,
        angle: 0.0,
    }
}

fn lit(m: &Mask) -> Vec<(i32, i32)> {
    let (cx, cy) = ((m.width / 2) as i32, (m.height / 2) as i32);
    let mut v = Vec::new();
    for y in 0..m.height {
        for x in 0..m.width {
            if m.get(x, y) > 0 {
                v.push((x as i32 - cx, y as i32 - cy));
            }
        }
    }
    v
}

/// Lit state at an offset from the mask center (outside the mask = unlit).
fn on(m: &Mask, dx: i32, dy: i32) -> bool {
    let (x, y) = ((m.width / 2) as i32 + dx, (m.height / 2) as i32 + dy);
    x >= 0
        && y >= 0
        && (x as u32) < m.width
        && (y as u32) < m.height
        && m.get(x as u32, y as u32) > 0
}

#[test]
fn hard_square_is_full_block() {
    let b = generate(&params(Shape::Square, 3.5));
    assert_eq!((b.mask.width, b.mask.height), (7, 7));
    assert!(b.mask.data.iter().all(|&v| v == 255));
}

#[test]
fn hard_diamond_is_l1_ball() {
    let b = generate(&params(Shape::Diamond, 3.5));
    assert_eq!((b.mask.width, b.mask.height), (7, 7));
    for (dx, dy) in lit(&b.mask) {
        assert!(dx.abs() + dy.abs() <= 3);
    }
    assert_eq!(lit(&b.mask).len(), 25);
}

#[test]
fn hard_circle_is_round_and_binary() {
    let b = generate(&params(Shape::Circle, 5.0));
    assert_eq!((b.mask.width, b.mask.height), (9, 9));
    assert!(b.mask.data.iter().all(|&v| v == 0 || v == 255));
    assert!(on(&b.mask, 4, 0) && on(&b.mask, 0, -4) && on(&b.mask, 3, 3));
    assert!(!on(&b.mask, 4, 4));
}

#[test]
fn aspect_squashes_and_angle_rotates() {
    let mut p = params(Shape::Circle, 10.0);
    p.aspect_ratio = 2.0;
    let wide = generate(&p);
    assert_eq!((wide.mask.width, wide.mask.height), (19, 9));
    p.angle = 90.0;
    let tall = generate(&p);
    assert_eq!((tall.mask.width, tall.mask.height), (9, 19));
}

#[test]
fn spikes_make_a_star() {
    let mut p = params(Shape::Circle, 12.0);
    p.spikes = 4;
    p.shape = Shape::Diamond;
    let star = generate(&p);
    // Tips on the axes reach the radius; diagonals are pulled in.
    assert!(on(&star.mask, 11, 0) && on(&star.mask, 0, 11) && on(&star.mask, -11, 0));
    assert!(!on(&star.mask, 7, 7));
    // Five spikes: one tip at 0°, none at 180°.
    p.spikes = 5;
    let five = generate(&p);
    assert!(on(&five.mask, 11, 0));
    assert!(!on(&five.mask, -11, 0));
}

#[test]
fn soft_brush_falls_off_radially() {
    let mut p = params(Shape::Circle, 8.0);
    p.hardness = 0.0;
    let b = generate(&p);
    let c = b.mask.width / 2;
    assert_eq!(b.mask.get(c, c), 255);
    let mid = b.mask.get(c + 4, c);
    assert!(mid > 0 && mid < 255);
    // Symmetric around the center.
    assert_eq!(b.mask.get(c + 3, c), b.mask.get(c - 3, c));
    assert_eq!(b.mask.get(c, c + 3), b.mask.get(c, c - 3));
}
