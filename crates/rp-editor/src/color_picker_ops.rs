//! The color picker popover, with no GPUI types: HSV ↔ RGB, hex parsing,
//! typed R/G/B and hex entry, drag mapping for the saturation/value square
//! and the hue bar, and the "dithers to" preview.
//!
//! Clicking the ribbon's foreground or background chip opens the picker on
//! that color; the Swatches panel's "+ New color…" opens it to make a new
//! swatch. While it is open the target color follows the picker live (an
//! overlay-only change, like every fg/bg change: no re-dither, not an undo
//! step); Cancel puts the old color back. Adding a swatch is an undo step.

use rp_palette_dither::{CgaColor, NearestLut, Palette, PreAdjust, Rgb};

use crate::state::EditorState;
use crate::swatch_ops::{AddSwatch, hex};

/// A color in HSV: hue in degrees `0.0..=360.0`, saturation and value in
/// `0.0..=1.0`, all over gamma-encoded sRGB (as in GIMP and the web).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Hsv {
    pub h: f32,
    pub s: f32,
    pub v: f32,
}

impl Hsv {
    /// Clamps each part to its range.
    pub fn new(h: f32, s: f32, v: f32) -> Self {
        Self {
            h: h.clamp(0.0, 360.0),
            s: s.clamp(0.0, 1.0),
            v: v.clamp(0.0, 1.0),
        }
    }

    /// The 8-bit sRGB color (rounded).
    pub fn to_rgb(self) -> Rgb {
        let h = (self.h as f64).rem_euclid(360.0) / 60.0;
        let (s, v) = (self.s as f64, self.v as f64);
        let c = v * s;
        let x = c * (1.0 - (h % 2.0 - 1.0).abs());
        let (r, g, b) = match h as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = v - c;
        let q = |f: f64| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u8;
        Rgb::new(q(r), q(g), q(b))
    }

    /// HSV of an 8-bit sRGB color. Grays get hue 0 (and black saturation
    /// 0); see [`Hsv::from_rgb_keeping`] to keep a hue instead.
    pub fn from_rgb(c: Rgb) -> Self {
        let [r, g, b] = [c.r, c.g, c.b].map(|v| v as f64 / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let d = max - min;
        let h = if d == 0.0 {
            0.0
        } else if max == r {
            60.0 * ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };
        let s = if max == 0.0 { 0.0 } else { d / max };
        Self::new(h as f32, s as f32, max as f32)
    }

    /// Like [`Hsv::from_rgb`], but where `c` leaves a part undefined (hue
    /// of a gray, saturation of black) the part of `prev` is kept, so the
    /// square does not jump back to red when a gray is picked.
    pub fn from_rgb_keeping(c: Rgb, prev: Hsv) -> Self {
        let mut hsv = Self::from_rgb(c);
        if hsv.s == 0.0 {
            hsv.h = prev.h;
        }
        if hsv.v == 0.0 {
            hsv.h = prev.h;
            hsv.s = prev.s;
        }
        hsv
    }
}

/// Parses `#rrggbb`, `rrggbb`, `#rgb` or `rgb` (any case, surrounding
/// spaces ignored).
pub fn parse_hex(text: &str) -> Option<Rgb> {
    let t = text.trim();
    let t = t.strip_prefix('#').unwrap_or(t);
    if !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let digit = |i: usize| u8::from_str_radix(&t[i..i + 1], 16).ok();
    let pair = |i: usize| u8::from_str_radix(&t[i..i + 2], 16).ok();
    match t.len() {
        3 => Some(Rgb::new(digit(0)? * 17, digit(1)? * 17, digit(2)? * 17)),
        6 => Some(Rgb::new(pair(0)?, pair(2)?, pair(4)?)),
        _ => None,
    }
}

/// Saturation and value under a point at (`x`, `y`) inside a square of
/// `w × h` (saturation grows to the right, value grows upward). Points
/// outside are clamped to the edge.
pub fn square_at(x: f32, y: f32, w: f32, h: f32) -> (f32, f32) {
    let fx = if w > 0.0 {
        (x / w).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let fy = if h > 0.0 {
        (y / h).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (fx, 1.0 - fy)
}

/// Where (`s`, `v`) sits in a `w × h` square: the inverse of [`square_at`].
pub fn square_pos(s: f32, v: f32, w: f32, h: f32) -> (f32, f32) {
    (s.clamp(0.0, 1.0) * w, (1.0 - v.clamp(0.0, 1.0)) * h)
}

/// Hue under `y` on a vertical bar of height `h` (red at the top, through
/// yellow, green, cyan, blue and magenta, back to red at the bottom).
pub fn hue_at(y: f32, h: f32) -> f32 {
    if h > 0.0 {
        (y / h).clamp(0.0, 1.0) * 360.0
    } else {
        0.0
    }
}

/// Where `hue` sits on a bar of height `h`: the inverse of [`hue_at`].
pub fn hue_pos(hue: f32, h: f32) -> f32 {
    hue.clamp(0.0, 360.0) / 360.0 * h
}

/// An RGBA image (`side × side`) of the saturation/value square for `hue`:
/// each pixel shows the color [`square_at`] gives for its center.
pub fn square_rgba(hue: f32, side: u32) -> Vec<u8> {
    let n = side as f32;
    let mut out = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let (s, v) = square_at(x as f32 + 0.5, y as f32 + 0.5, n, n);
            let c = Hsv::new(hue, s, v).to_rgb();
            out.extend_from_slice(&[c.r, c.g, c.b, 255]);
        }
    }
    out
}

/// The hue bar's colors at hue 0, 60, …, 360 (top to bottom). Between two
/// stops the full-saturation, full-value hue ramp is a straight line in
/// sRGB, so the bar is six two-stop sRGB gradients.
pub const HUE_STOPS: [Rgb; 7] = [
    Rgb::new(255, 0, 0),
    Rgb::new(255, 255, 0),
    Rgb::new(0, 255, 0),
    Rgb::new(0, 255, 255),
    Rgb::new(0, 0, 255),
    Rgb::new(255, 0, 255),
    Rgb::new(255, 0, 0),
];

/// The palette color a flat area of `c` becomes in the dither: the active
/// pre-adjust, then the nearest palette color (no dither pattern).
pub fn dithers_to(lut: &NearestLut, adjust: &PreAdjust, c: Rgb) -> CgaColor {
    if adjust.is_neutral() {
        return lut.nearest(c);
    }
    let srgb = [c.r, c.g, c.b].map(|v| v as f32 / 255.0);
    let a = adjust
        .apply(srgb)
        .map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8);
    lut.nearest(Rgb::new(a[0], a[1], a[2]))
}

/// One [`NearestLut`], rebuilt only when the palette changes (building one
/// takes milliseconds; the picker asks on every redraw).
#[derive(Default)]
pub struct LutCache {
    lut: Option<NearestLut>,
}

impl LutCache {
    /// The table for `palette`, building it if the cached one is for
    /// another palette.
    pub fn get(&mut self, palette: Palette) -> &NearestLut {
        if self.lut.as_ref().is_none_or(|l| l.palette() != palette) {
            self.lut = Some(NearestLut::new(palette));
        }
        self.lut.as_ref().expect("just built")
    }
}

/// Which color the picker edits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickerTarget {
    Foreground,
    Background,
    /// The Swatches panel's "+ New color…": OK saves a swatch.
    NewSwatch,
}

/// A typed entry field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickerField {
    Hex,
    Red,
    Green,
    Blue,
}

impl PickerField {
    pub const ALL: [PickerField; 4] = [
        PickerField::Hex,
        PickerField::Red,
        PickerField::Green,
        PickerField::Blue,
    ];

    /// Tab order: Hex → R → G → B → Hex.
    pub fn next(self) -> Self {
        match self {
            PickerField::Hex => PickerField::Red,
            PickerField::Red => PickerField::Green,
            PickerField::Green => PickerField::Blue,
            PickerField::Blue => PickerField::Hex,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PickerField::Hex => "Hex",
            PickerField::Red => "R",
            PickerField::Green => "G",
            PickerField::Blue => "B",
        }
    }

    /// Longest text the field takes.
    fn max_len(self) -> usize {
        match self {
            PickerField::Hex => 6,
            _ => 3,
        }
    }

    fn accepts(self, c: char) -> bool {
        match self {
            PickerField::Hex => c.is_ascii_hexdigit(),
            _ => c.is_ascii_digit(),
        }
    }
}

/// Text typed into a [`PickerField`] so far.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PickerEdit {
    pub field: PickerField,
    pub text: String,
}

/// What a mouse drag in the picker is moving.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickerDrag {
    Square,
    Hue,
}

/// The open picker.
#[derive(Clone, PartialEq, Debug)]
pub struct ColorPicker {
    pub target: PickerTarget,
    /// The color when the picker opened (Cancel restores it).
    pub old: Rgb,
    /// The picked color.
    pub color: Rgb,
    /// HSV of [`color`](Self::color); keeps hue and saturation through
    /// grays and black.
    pub hsv: Hsv,
    pub editing: Option<PickerEdit>,
    pub drag: Option<PickerDrag>,
}

impl ColorPicker {
    pub fn new(target: PickerTarget, old: Rgb) -> Self {
        Self {
            target,
            old,
            color: old,
            hsv: Hsv::from_rgb(old),
            editing: None,
            drag: None,
        }
    }

    /// Picks an exact color (CGA row, typed values, the old chip).
    pub fn set_rgb(&mut self, c: Rgb) {
        self.color = c;
        self.hsv = Hsv::from_rgb_keeping(c, self.hsv);
    }

    pub fn set_hsv(&mut self, hsv: Hsv) {
        self.hsv = hsv;
        self.color = hsv.to_rgb();
    }

    /// Pointer at (`x`, `y`) in the `w × h` square (press or drag).
    pub fn drag_square(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let (s, v) = square_at(x, y, w, h);
        self.set_hsv(Hsv::new(self.hsv.h, s, v));
    }

    /// Pointer at `y` on the hue bar of height `h` (press or drag).
    pub fn drag_hue(&mut self, y: f32, h: f32) {
        self.set_hsv(Hsv::new(hue_at(y, h), self.hsv.s, self.hsv.v));
    }

    /// The text a field shows when not being typed into.
    pub fn field_value(&self, field: PickerField) -> String {
        let c = self.color;
        match field {
            PickerField::Hex => hex(c),
            PickerField::Red => c.r.to_string(),
            PickerField::Green => c.g.to_string(),
            PickerField::Blue => c.b.to_string(),
        }
    }

    /// Starts typing into `field` (commits any other field first).
    pub fn begin_edit(&mut self, field: PickerField) {
        if self.editing.as_ref().is_some_and(|e| e.field == field) {
            return;
        }
        self.commit_edit();
        self.editing = Some(PickerEdit {
            field,
            text: String::new(),
        });
    }

    /// Appends the characters of `text` the field takes (hex digits or
    /// decimal digits); others, like `#`, are ignored.
    pub fn type_text(&mut self, text: &str) {
        if let Some(e) = &mut self.editing {
            for c in text.chars().filter(|&c| e.field.accepts(c)) {
                if e.text.len() < e.field.max_len() {
                    e.text.push(c.to_ascii_lowercase());
                }
            }
        }
    }

    pub fn backspace(&mut self) {
        if let Some(e) = &mut self.editing {
            e.text.pop();
        }
    }

    /// Ends typing and sets the color: hex takes 3 or 6 digits, R/G/B are
    /// clamped to 0–255. Nothing typed, or a hex of another length, keeps
    /// the color.
    pub fn commit_edit(&mut self) {
        let Some(e) = self.editing.take() else { return };
        let mut c = self.color;
        match e.field {
            PickerField::Hex => match parse_hex(&e.text) {
                Some(h) => c = h,
                None => return,
            },
            field => {
                let Ok(v) = e.text.parse::<u32>() else { return };
                let v = v.min(255) as u8;
                match field {
                    PickerField::Red => c.r = v,
                    PickerField::Green => c.g = v,
                    _ => c.b = v,
                }
            }
        }
        self.set_rgb(c);
    }

    /// Tab: commits the field being typed and starts the next one.
    pub fn tab(&mut self) {
        let next = self
            .editing
            .as_ref()
            .map_or(PickerField::Hex, |e| e.field.next());
        self.commit_edit();
        self.begin_edit(next);
    }

    pub fn cancel_edit(&mut self) {
        self.editing = None;
    }
}

impl EditorState {
    /// Opens the picker on the foreground, the background, or (for a new
    /// swatch) starting from the foreground. Closes the menus.
    pub fn open_color_picker(&mut self, target: PickerTarget) {
        self.close_menus();
        if self.file_modal_open() || self.project_dialog.is_some() {
            return;
        }
        let old = match target {
            PickerTarget::Background => self.document.bg,
            PickerTarget::Foreground | PickerTarget::NewSwatch => self.document.fg,
        };
        self.color_picker = Some(ColorPicker::new(target, old));
    }

    /// Runs `f` on the open picker, then shows its color live in the
    /// target (fg or bg; a new swatch waits for OK).
    pub fn edit_color_picker(&mut self, f: impl FnOnce(&mut ColorPicker)) {
        let Some(p) = &mut self.color_picker else {
            return;
        };
        f(p);
        let (target, c) = (p.target, p.color);
        match target {
            PickerTarget::Foreground => self.document.fg = c,
            PickerTarget::Background => self.document.bg = c,
            PickerTarget::NewSwatch => {}
        }
    }

    /// OK (Enter): keeps the color, or saves it as a swatch (an undo step)
    /// for "+ New color…". Closes the picker.
    pub fn confirm_color_picker(&mut self) {
        self.edit_color_picker(|p| p.commit_edit());
        let Some(p) = self.color_picker.take() else {
            return;
        };
        match p.target {
            PickerTarget::Foreground => self.status = format!("Foreground {}", hex(p.color)),
            PickerTarget::Background => self.status = format!("Background {}", hex(p.color)),
            PickerTarget::NewSwatch => {
                self.add_color_swatch(p.color);
            }
        }
    }

    /// Cancel (Escape, or a click outside): puts the old color back and
    /// closes the picker.
    pub fn cancel_color_picker(&mut self) {
        let Some(p) = self.color_picker.take() else {
            return;
        };
        match p.target {
            PickerTarget::Foreground => self.document.fg = p.old,
            PickerTarget::Background => self.document.bg = p.old,
            PickerTarget::NewSwatch => {}
        }
    }

    /// "Add to swatches": saves the picker's color as a swatch (an undo
    /// step) and keeps the picker open.
    pub fn color_picker_add_swatch(&mut self) -> Option<AddSwatch> {
        self.edit_color_picker(|p| p.commit_edit());
        let c = self.color_picker.as_ref()?.color;
        Some(self.add_color_swatch(c))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_worker::{Overlay, Request, next_request};
    use crate::undo_ops::{EditCommand, EditLabel};
    use rp_palette_dither::{CGA_RGB, FourColor, Intensity, Preset};

    #[test]
    fn rgb_hsv_round_trips() {
        // Every 3rd level per channel (~730k colors) plus the extremes.
        let levels: Vec<u8> = (0..=255u8).step_by(3).chain([254, 255]).collect();
        for &r in &levels {
            for &g in &levels {
                for &b in &levels {
                    let c = Rgb::new(r, g, b);
                    assert_eq!(Hsv::from_rgb(c).to_rgb(), c, "{c:?}");
                }
            }
        }
        for c in CGA_RGB {
            assert_eq!(Hsv::from_rgb(c).to_rgb(), c);
        }
    }

    #[test]
    fn hsv_known_values() {
        let near = |a: Hsv, h: f32, s: f32, v: f32| {
            assert!(
                (a.h - h).abs() < 1e-3 && (a.s - s).abs() < 1e-3 && (a.v - v).abs() < 1e-3,
                "{a:?}"
            )
        };
        near(Hsv::from_rgb(Rgb::new(255, 0, 0)), 0.0, 1.0, 1.0);
        near(Hsv::from_rgb(Rgb::new(0, 255, 0)), 120.0, 1.0, 1.0);
        near(Hsv::from_rgb(Rgb::new(0, 0, 255)), 240.0, 1.0, 1.0);
        near(Hsv::from_rgb(Rgb::new(255, 0, 255)), 300.0, 1.0, 1.0);
        near(
            Hsv::from_rgb(Rgb::new(128, 128, 128)),
            0.0,
            0.0,
            128.0 / 255.0,
        );
        near(Hsv::from_rgb(Rgb::new(0, 0, 0)), 0.0, 0.0, 0.0);
        assert_eq!(Hsv::new(60.0, 1.0, 1.0).to_rgb(), Rgb::new(255, 255, 0));
        assert_eq!(Hsv::new(180.0, 1.0, 0.5).to_rgb(), Rgb::new(0, 128, 128));
        // Hue 360 is red again.
        assert_eq!(Hsv::new(360.0, 1.0, 1.0).to_rgb(), Rgb::new(255, 0, 0));
    }

    #[test]
    fn hsv_parts_are_clamped() {
        let h = Hsv::new(-10.0, 2.0, -1.0);
        assert_eq!((h.h, h.s, h.v), (0.0, 1.0, 0.0));
        assert_eq!(Hsv::new(400.0, 0.5, 9.0).h, 360.0);
    }

    #[test]
    fn grays_keep_the_previous_hue() {
        let prev = Hsv::new(200.0, 0.7, 0.4);
        let gray = Hsv::from_rgb_keeping(Rgb::new(90, 90, 90), prev);
        assert_eq!((gray.h, gray.s), (200.0, 0.0));
        let black = Hsv::from_rgb_keeping(Rgb::new(0, 0, 0), prev);
        assert_eq!((black.h, black.s, black.v), (200.0, 0.7, 0.0));
        let red = Hsv::from_rgb_keeping(Rgb::new(255, 0, 0), prev);
        assert_eq!(red.h, 0.0);
    }

    #[test]
    fn hex_parse_and_format() {
        assert_eq!(parse_hex("#ff0010"), Some(Rgb::new(255, 0, 16)));
        assert_eq!(parse_hex("FF0010"), Some(Rgb::new(255, 0, 16)));
        assert_eq!(parse_hex("  #AbC  "), Some(Rgb::new(0xaa, 0xbb, 0xcc)));
        assert_eq!(parse_hex("abc"), Some(Rgb::new(0xaa, 0xbb, 0xcc)));
        for bad in ["", "#", "#ff00", "#ff00100", "#gg0000", "#+f0000", "ab c"] {
            assert_eq!(parse_hex(bad), None, "{bad:?}");
        }
        for c in [
            Rgb::new(1, 2, 3),
            Rgb::new(255, 255, 255),
            Rgb::new(0, 0, 0),
        ] {
            assert_eq!(parse_hex(&hex(c)), Some(c));
        }
    }

    #[test]
    fn square_and_hue_mapping_clamp_and_invert() {
        assert_eq!(square_at(0.0, 0.0, 100.0, 100.0), (0.0, 1.0));
        assert_eq!(square_at(100.0, 100.0, 100.0, 100.0), (1.0, 0.0));
        assert_eq!(square_at(-50.0, 250.0, 100.0, 100.0), (0.0, 0.0));
        assert_eq!(square_at(500.0, -5.0, 100.0, 100.0), (1.0, 1.0));
        assert_eq!(square_at(25.0, 75.0, 100.0, 100.0), (0.25, 0.25));
        assert_eq!(square_at(5.0, 5.0, 0.0, 0.0), (0.0, 1.0));
        let (x, y) = square_pos(0.25, 0.25, 100.0, 100.0);
        assert_eq!(square_at(x, y, 100.0, 100.0), (0.25, 0.25));

        assert_eq!(hue_at(0.0, 180.0), 0.0);
        assert_eq!(hue_at(90.0, 180.0), 180.0);
        assert_eq!(hue_at(-3.0, 180.0), 0.0);
        assert_eq!(hue_at(999.0, 180.0), 360.0);
        assert_eq!(hue_at(4.0, 0.0), 0.0);
        assert_eq!(hue_at(hue_pos(123.0, 180.0), 180.0), 123.0);
    }

    #[test]
    fn square_image_matches_the_drag_mapping() {
        let side = 16;
        let img = square_rgba(120.0, side);
        assert_eq!(img.len(), (side * side * 4) as usize);
        let px = |x: u32, y: u32| {
            let i = ((y * side + x) * 4) as usize;
            Rgb::new(img[i], img[i + 1], img[i + 2])
        };
        for (x, y) in [(0, 0), (15, 0), (7, 9), (15, 15)] {
            let mut p = ColorPicker::new(PickerTarget::Foreground, Rgb::new(0, 255, 0));
            p.drag_square(x as f32 + 0.5, y as f32 + 0.5, side as f32, side as f32);
            assert_eq!(px(x, y), p.color, "({x}, {y})");
        }
        // Top-right is the pure hue, bottom row nearly black.
        assert!(px(15, 0).g > 240 && px(15, 0).r < 16);
        assert!(px(0, 15).r < 16);

        // The hue bar's gradient stops are the HSV hues they stand for,
        // and the ramp between two stops is linear in sRGB.
        for (i, &c) in HUE_STOPS.iter().enumerate() {
            let h = 60.0 * i as f32;
            assert_eq!(Hsv::new(h, 1.0, 1.0).to_rgb(), c);
            if let Some(&next) = HUE_STOPS.get(i + 1) {
                for t in [0.25f32, 0.5, 0.75] {
                    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
                    let want = Rgb::new(mix(c.r, next.r), mix(c.g, next.g), mix(c.b, next.b));
                    assert_eq!(Hsv::new(h + 60.0 * t, 1.0, 1.0).to_rgb(), want, "{h} {t}");
                }
            }
        }
    }

    #[test]
    fn drags_move_saturation_value_and_hue() {
        let mut p = ColorPicker::new(PickerTarget::Foreground, Rgb::new(255, 0, 0));
        p.drag_square(100.0, 0.0, 100.0, 100.0);
        assert_eq!(p.color, Rgb::new(255, 0, 0));
        p.drag_hue(60.0, 180.0); // 120°
        assert_eq!(p.color, Rgb::new(0, 255, 0));
        p.drag_square(0.0, 0.0, 100.0, 100.0); // white
        assert_eq!(p.color, Rgb::new(255, 255, 255));
        // Hue survives white: dragging back to the right edge is green.
        p.drag_square(100.0, 0.0, 100.0, 100.0);
        assert_eq!(p.color, Rgb::new(0, 255, 0));
        p.drag_square(50.0, 500.0, 100.0, 100.0); // clamped to the bottom
        assert_eq!(p.color, Rgb::new(0, 0, 0));
        p.drag_square(100.0, 0.0, 100.0, 100.0);
        assert_eq!(p.color, Rgb::new(0, 255, 0));
    }

    #[test]
    fn typed_fields_commit_clamped() {
        let mut p = ColorPicker::new(PickerTarget::Foreground, Rgb::new(10, 20, 30));
        p.begin_edit(PickerField::Red);
        p.type_text("9x99");
        assert_eq!(p.editing.as_ref().unwrap().text, "999");
        p.commit_edit();
        assert_eq!(p.color, Rgb::new(255, 20, 30));

        p.begin_edit(PickerField::Hex);
        p.type_text("#00FF7");
        p.backspace();
        p.type_text("8g80");
        assert_eq!(p.editing.as_ref().unwrap().text, "00ff88");
        p.commit_edit();
        assert_eq!(p.color, Rgb::new(0, 255, 136));
        assert_eq!(p.field_value(PickerField::Hex), "#00ff88");
        assert_eq!(p.field_value(PickerField::Green), "255");

        // A short hex or an empty field keeps the color.
        p.begin_edit(PickerField::Hex);
        p.type_text("12345");
        p.commit_edit();
        assert_eq!(p.color, Rgb::new(0, 255, 136));
        p.begin_edit(PickerField::Blue);
        p.commit_edit();
        assert_eq!(p.color, Rgb::new(0, 255, 136));
        p.begin_edit(PickerField::Hex);
        p.type_text("f0a");
        p.commit_edit();
        assert_eq!(p.color, Rgb::new(255, 0, 170));

        // Escape drops the typing.
        p.begin_edit(PickerField::Green);
        p.type_text("1");
        p.cancel_edit();
        assert_eq!(p.color, Rgb::new(255, 0, 170));
        assert!(p.editing.is_none());
    }

    #[test]
    fn tab_commits_and_moves_to_the_next_field() {
        let mut p = ColorPicker::new(PickerTarget::Foreground, Rgb::new(0, 0, 0));
        p.tab();
        assert_eq!(p.editing.as_ref().unwrap().field, PickerField::Hex);
        p.tab();
        p.type_text("200");
        p.tab();
        assert_eq!(p.color, Rgb::new(200, 0, 0));
        assert_eq!(p.editing.as_ref().unwrap().field, PickerField::Green);
        // Clicking another field commits the one being typed.
        p.type_text("7");
        p.begin_edit(PickerField::Hex);
        assert_eq!(p.color, Rgb::new(200, 7, 0));
    }

    #[test]
    fn dithers_to_uses_the_palette_and_pre_adjust() {
        let full = NearestLut::new(Palette::full());
        let none = PreAdjust::default();
        for c in CgaColor::ALL {
            assert_eq!(dithers_to(&full, &none, c.rgb()), c);
        }
        assert_eq!(
            dithers_to(&full, &none, Rgb::new(250, 10, 10)),
            CgaColor::LightRed
        );
        let bright = PreAdjust {
            brightness: 1.0,
            ..PreAdjust::default()
        };
        assert_eq!(
            dithers_to(&full, &bright, Rgb::new(0, 0, 0)),
            CgaColor::White
        );
        let four = FourColor::new(Preset::Palette1, Intensity::High).palette();
        let lut = NearestLut::new(four);
        let got = dithers_to(&lut, &none, Rgb::new(250, 10, 10));
        assert!(four.contains(got), "{got:?}");
    }

    #[test]
    fn lut_cache_rebuilds_only_on_palette_change() {
        let mut cache = LutCache::default();
        let full = Palette::full();
        let a = cache.get(full) as *const NearestLut;
        let b = cache.get(full) as *const NearestLut;
        assert_eq!(a, b);
        let four = FourColor::new(Preset::Palette1, Intensity::Low).palette();
        assert_eq!(cache.get(four).palette(), four);
        assert_eq!(cache.get(full).palette(), full);
    }

    #[test]
    fn foreground_follows_live_and_cancel_restores() {
        let mut st = EditorState::new();
        let old = st.document.fg;
        let v = st.doc_version;
        let before = Overlay::from_state(&st);
        st.open_color_picker(PickerTarget::Foreground);
        st.edit_color_picker(|p| p.set_rgb(Rgb::new(1, 2, 3)));
        assert_eq!(st.document.fg, Rgb::new(1, 2, 3));
        assert!(st.is_dirty());
        // Overlay only: no re-dither.
        assert_eq!(st.doc_version, v);
        assert_eq!(
            next_request(Some((v, before)), st.doc_version, &Overlay::from_state(&st)),
            Request::Overlay
        );
        st.cancel_color_picker();
        assert!(st.color_picker.is_none());
        assert_eq!(st.document.fg, old);
        assert!(!st.is_dirty());
        assert!(st.history.undo_label().is_none());
    }

    #[test]
    fn ok_keeps_the_color_and_commits_typing() {
        let mut st = EditorState::new();
        st.open_color_picker(PickerTarget::Background);
        assert_eq!(st.color_picker.as_ref().unwrap().old, st.document.bg);
        st.edit_color_picker(|p| {
            p.begin_edit(PickerField::Hex);
            p.type_text("123456");
        });
        st.confirm_color_picker();
        assert_eq!(st.document.bg, Rgb::new(0x12, 0x34, 0x56));
        assert!(st.color_picker.is_none());
        assert!(st.status.contains("#123456"), "{}", st.status);
        // fg/bg changes are not undo steps.
        assert!(st.history.undo_label().is_none());
    }

    #[test]
    fn new_swatch_adds_on_ok_as_an_undo_step() {
        let mut st = EditorState::new();
        let fg = st.document.fg;
        st.open_color_picker(PickerTarget::NewSwatch);
        st.edit_color_picker(|p| p.set_rgb(Rgb::new(9, 8, 7)));
        // A new swatch does not touch fg.
        assert_eq!(st.document.fg, fg);
        st.confirm_color_picker();
        assert_eq!(st.document.swatches, [Rgb::new(9, 8, 7)]);
        assert_eq!(st.history.undo_label(), Some(EditLabel::AddSwatch));
        st.edit_command(EditCommand::Undo);
        assert!(st.document.swatches.is_empty());

        // Cancel adds nothing.
        st.open_color_picker(PickerTarget::NewSwatch);
        st.edit_color_picker(|p| p.set_rgb(Rgb::new(1, 1, 1)));
        st.cancel_color_picker();
        assert!(st.document.swatches.is_empty());
    }

    #[test]
    fn add_to_swatches_keeps_the_picker_open() {
        let mut st = EditorState::new();
        st.open_color_picker(PickerTarget::Foreground);
        st.edit_color_picker(|p| p.set_rgb(Rgb::new(5, 5, 5)));
        assert_eq!(st.color_picker_add_swatch(), Some(AddSwatch::Added));
        assert_eq!(st.document.swatches, [Rgb::new(5, 5, 5)]);
        assert!(st.color_picker.is_some());
        assert_eq!(st.history.undo_label(), Some(EditLabel::AddSwatch));
    }

    #[test]
    fn the_picker_is_modal() {
        let mut st = EditorState::new();
        st.document.fg = Rgb::new(1, 1, 1);
        st.add_fg_swatch();
        st.open_color_picker(PickerTarget::Foreground);
        // Undo, redo, layer commands and paste do nothing while it is open.
        assert!(!st.edit_command(EditCommand::Undo));
        assert_eq!(st.document.swatches.len(), 1);
        assert!(!st.edit_command(EditCommand::DuplicateLayer));
        assert!(!st.begin_paste());
        st.cancel_color_picker();
        assert!(st.edit_command(EditCommand::Undo));
        assert!(st.document.swatches.is_empty());
    }
}
