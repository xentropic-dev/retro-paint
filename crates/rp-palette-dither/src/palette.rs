//! The 16 CGA colors and the palette subsets the dither may use.
//!
//! See docs/filter-and-palette.md, "Palette" and "4-color mode presets".

use std::fmt;

/// An 8-bit-per-channel sRGB color.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Channels as `[r, g, b]`.
    pub const fn to_array(self) -> [u8; 3] {
        [self.r, self.g, self.b]
    }
}

impl From<[u8; 3]> for Rgb {
    fn from([r, g, b]: [u8; 3]) -> Self {
        Self { r, g, b }
    }
}

impl From<Rgb> for [u8; 3] {
    fn from(c: Rgb) -> Self {
        c.to_array()
    }
}

/// Formats as `#rrggbb`, matching the hex column in the docs.
impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

impl fmt::Debug for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Rgb({self})")
    }
}

/// One of the 16 CGA colors.
///
/// The discriminant is the index in the docs' table (doc order), **not** the
/// CGA hardware color number. `CgaColor as u8` and [`CgaColor::from_index`]
/// convert between the two.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
#[repr(u8)]
pub enum CgaColor {
    Black = 0,
    DarkGray = 1,
    LightGray = 2,
    White = 3,
    Blue = 4,
    LightBlue = 5,
    Green = 6,
    LightGreen = 7,
    Cyan = 8,
    LightCyan = 9,
    Red = 10,
    LightRed = 11,
    Magenta = 12,
    LightMagenta = 13,
    Brown = 14,
    Yellow = 15,
}

impl CgaColor {
    /// All 16 colors in doc order.
    pub const ALL: [CgaColor; 16] = [
        CgaColor::Black,
        CgaColor::DarkGray,
        CgaColor::LightGray,
        CgaColor::White,
        CgaColor::Blue,
        CgaColor::LightBlue,
        CgaColor::Green,
        CgaColor::LightGreen,
        CgaColor::Cyan,
        CgaColor::LightCyan,
        CgaColor::Red,
        CgaColor::LightRed,
        CgaColor::Magenta,
        CgaColor::LightMagenta,
        CgaColor::Brown,
        CgaColor::Yellow,
    ];

    /// Doc-order index, `0..16`.
    pub const fn index(self) -> u8 {
        self as u8
    }

    /// The color at doc-order `index`, or `None` if `index >= 16`.
    pub const fn from_index(index: u8) -> Option<CgaColor> {
        if (index as usize) < Self::ALL.len() {
            Some(Self::ALL[index as usize])
        } else {
            None
        }
    }

    /// The exact sRGB value. Every channel is 0, 85, 170 or 255.
    pub const fn rgb(self) -> Rgb {
        CGA_RGB[self as usize]
    }

    /// The CGA color with exactly this sRGB value, if any.
    pub fn from_rgb(rgb: Rgb) -> Option<CgaColor> {
        Self::ALL.into_iter().find(|c| c.rgb() == rgb)
    }
}

impl From<CgaColor> for Rgb {
    fn from(c: CgaColor) -> Self {
        c.rgb()
    }
}

/// sRGB values of the 16 CGA colors, in doc order.
pub const CGA_RGB: [Rgb; 16] = [
    Rgb::new(0x00, 0x00, 0x00),
    Rgb::new(0x55, 0x55, 0x55),
    Rgb::new(0xaa, 0xaa, 0xaa),
    Rgb::new(0xff, 0xff, 0xff),
    Rgb::new(0x00, 0x00, 0xaa),
    Rgb::new(0x55, 0x55, 0xff),
    Rgb::new(0x00, 0xaa, 0x00),
    Rgb::new(0x55, 0xff, 0x55),
    Rgb::new(0x00, 0xaa, 0xaa),
    Rgb::new(0x55, 0xff, 0xff),
    Rgb::new(0xaa, 0x00, 0x00),
    Rgb::new(0xff, 0x55, 0x55),
    Rgb::new(0xaa, 0x00, 0xaa),
    Rgb::new(0xff, 0x55, 0xff),
    Rgb::new(0xaa, 0x55, 0x00),
    Rgb::new(0xff, 0xff, 0x55),
];

/// Which authentic CGA 4-color palette.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Preset {
    /// Green, red, brown (high: light green, light red, yellow).
    Palette0,
    /// Cyan, magenta, light gray (high: light cyan, light magenta, white).
    Palette1,
    /// Cyan, red, light gray (high: light cyan, light red, white).
    Mode5,
}

impl Preset {
    /// Colors 1–3 of this preset at the given intensity.
    pub const fn colors(self, intensity: Intensity) -> [CgaColor; 3] {
        use CgaColor::*;
        match (self, intensity) {
            (Preset::Palette0, Intensity::Low) => [Green, Red, Brown],
            (Preset::Palette0, Intensity::High) => [LightGreen, LightRed, Yellow],
            (Preset::Palette1, Intensity::Low) => [Cyan, Magenta, LightGray],
            (Preset::Palette1, Intensity::High) => [LightCyan, LightMagenta, White],
            (Preset::Mode5, Intensity::Low) => [Cyan, Red, LightGray],
            (Preset::Mode5, Intensity::High) => [LightCyan, LightRed, White],
        }
    }
}

/// Low or high intensity variant of a [`Preset`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Intensity {
    #[default]
    Low,
    High,
}

/// A 4-color mode: a preset, its intensity, and the background (color 0),
/// which may be any of the 16 colors.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FourColor {
    pub preset: Preset,
    pub intensity: Intensity,
    pub background: CgaColor,
}

impl FourColor {
    /// The preset with the default black background.
    pub const fn new(preset: Preset, intensity: Intensity) -> Self {
        Self {
            preset,
            intensity,
            background: CgaColor::Black,
        }
    }

    /// Same mode with a different background color.
    pub const fn with_background(self, background: CgaColor) -> Self {
        Self { background, ..self }
    }

    /// `[background, c1, c2, c3]`. May contain a duplicate when the
    /// background equals one of colors 1–3.
    pub const fn colors(self) -> [CgaColor; 4] {
        let [c1, c2, c3] = self.preset.colors(self.intensity);
        [self.background, c1, c2, c3]
    }

    /// The palette for this mode. See [`Palette::new`] for how a background
    /// that repeats one of colors 1–3 is handled.
    pub fn palette(self) -> Palette {
        Palette::new(self.colors()).expect("four-color mode is never empty")
    }
}

impl From<FourColor> for Palette {
    fn from(m: FourColor) -> Self {
        m.palette()
    }
}

/// Why [`Palette::new`] refused a color list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaletteError {
    /// A palette needs at least one color.
    Empty,
}

impl fmt::Display for PaletteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PaletteError::Empty => f.write_str("palette must contain at least one color"),
        }
    }
}

impl std::error::Error for PaletteError {}

/// An ordered, non-empty set of distinct CGA colors the dither may output.
///
/// The full 16-color palette is the default. Order is kept as given (for a
/// 4-color mode the background is entry 0); duplicates are dropped, keeping
/// the first occurrence. `Palette` is `Copy` and cheap to compare and hash, so
/// it can key a per-subset lookup table.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Palette {
    colors: [CgaColor; 16],
    len: u8,
}

impl Palette {
    /// All 16 CGA colors in doc order.
    pub const fn full() -> Self {
        Self {
            colors: CgaColor::ALL,
            len: 16,
        }
    }

    /// A custom subset, in the given order, with duplicates removed.
    pub fn new(colors: impl IntoIterator<Item = CgaColor>) -> Result<Self, PaletteError> {
        let mut out = Self {
            colors: [CgaColor::Black; 16],
            len: 0,
        };
        for c in colors {
            if !out.contains(c) {
                out.colors[out.len as usize] = c;
                out.len += 1;
            }
        }
        if out.len == 0 {
            Err(PaletteError::Empty)
        } else {
            Ok(out)
        }
    }

    /// The colors, in palette order. Never empty.
    pub fn colors(&self) -> &[CgaColor] {
        &self.colors[..self.len as usize]
    }

    /// Number of colors, `1..=16`.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Always `false`; a palette has at least one color.
    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn contains(&self, color: CgaColor) -> bool {
        self.colors().contains(&color)
    }

    /// Whether `rgb` is exactly one of this palette's colors.
    pub fn contains_rgb(&self, rgb: Rgb) -> bool {
        self.colors().iter().any(|c| c.rgb() == rgb)
    }

    /// Position of `color` in this palette.
    pub fn position(&self, color: CgaColor) -> Option<usize> {
        self.colors().iter().position(|&c| c == color)
    }

    /// Iterates the sRGB values in palette order.
    pub fn rgb(&self) -> impl ExactSizeIterator<Item = Rgb> + '_ {
        self.colors().iter().map(|c| c.rgb())
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::full()
    }
}

impl fmt::Debug for Palette {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Palette").field(&self.colors()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CgaColor::*;

    #[test]
    fn cga_table_matches_doc_order() {
        let hex: Vec<String> = CGA_RGB.iter().map(|c| c.to_string()).collect();
        assert_eq!(
            hex,
            [
                "#000000", "#555555", "#aaaaaa", "#ffffff", "#0000aa", "#5555ff", "#00aa00",
                "#55ff55", "#00aaaa", "#55ffff", "#aa0000", "#ff5555", "#aa00aa", "#ff55ff",
                "#aa5500", "#ffff55",
            ]
        );
    }

    #[test]
    fn every_channel_is_a_cga_level() {
        for c in CGA_RGB {
            for v in c.to_array() {
                assert!([0, 85, 170, 255].contains(&v), "{c}");
            }
        }
    }

    #[test]
    fn colors_are_distinct() {
        for (i, a) in CGA_RGB.iter().enumerate() {
            for b in &CGA_RGB[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn index_round_trips() {
        for (i, c) in CgaColor::ALL.into_iter().enumerate() {
            assert_eq!(c.index() as usize, i);
            assert_eq!(CgaColor::from_index(i as u8), Some(c));
            assert_eq!(CgaColor::from_rgb(c.rgb()), Some(c));
        }
        assert_eq!(CgaColor::from_index(16), None);
        assert_eq!(CgaColor::from_rgb(Rgb::new(1, 2, 3)), None);
    }

    #[test]
    fn named_colors_have_doc_values() {
        assert_eq!(Brown.rgb().to_string(), "#aa5500");
        assert_eq!(Yellow.rgb().to_string(), "#ffff55");
        assert_eq!(LightBlue.rgb().to_string(), "#5555ff");
        assert_eq!(Magenta.rgb().to_string(), "#aa00aa");
    }

    #[test]
    fn default_palette_is_full_16() {
        let p = Palette::default();
        assert_eq!(p, Palette::full());
        assert_eq!(p.colors(), &CgaColor::ALL);
        assert_eq!(p.len(), 16);
    }

    fn hexes(p: &Palette) -> Vec<String> {
        p.rgb().map(|c| c.to_string()).collect()
    }

    #[test]
    fn presets_match_doc_table() {
        let cases = [
            (
                Preset::Palette0,
                Intensity::Low,
                ["#00aa00", "#aa0000", "#aa5500"],
            ),
            (
                Preset::Palette0,
                Intensity::High,
                ["#55ff55", "#ff5555", "#ffff55"],
            ),
            (
                Preset::Palette1,
                Intensity::Low,
                ["#00aaaa", "#aa00aa", "#aaaaaa"],
            ),
            (
                Preset::Palette1,
                Intensity::High,
                ["#55ffff", "#ff55ff", "#ffffff"],
            ),
            (
                Preset::Mode5,
                Intensity::Low,
                ["#00aaaa", "#aa0000", "#aaaaaa"],
            ),
            (
                Preset::Mode5,
                Intensity::High,
                ["#55ffff", "#ff5555", "#ffffff"],
            ),
        ];
        for (preset, intensity, want) in cases {
            let p = FourColor::new(preset, intensity).palette();
            let mut expect = vec!["#000000".to_string()];
            expect.extend(want.iter().map(|s| s.to_string()));
            assert_eq!(hexes(&p), expect, "{preset:?} {intensity:?}");
        }
    }

    #[test]
    fn four_color_background_defaults_to_black_and_can_be_any_color() {
        let m = FourColor::new(Preset::Palette1, Intensity::High);
        assert_eq!(m.background, Black);
        for bg in CgaColor::ALL {
            let p = m.with_background(bg).palette();
            assert_eq!(p.colors()[0], bg);
            assert!(p.contains(LightCyan) && p.contains(LightMagenta) && p.contains(White));
        }
    }

    #[test]
    fn background_equal_to_a_preset_color_is_deduplicated() {
        let p = FourColor::new(Preset::Palette0, Intensity::Low)
            .with_background(Red)
            .palette();
        assert_eq!(p.colors(), &[Red, Green, Brown]);
    }

    #[test]
    fn custom_subset_keeps_order_and_drops_duplicates() {
        let p = Palette::new([White, Blue, White, Black, Blue]).unwrap();
        assert_eq!(p.colors(), &[White, Blue, Black]);
        assert_eq!(p.position(Black), Some(2));
        assert_eq!(p.position(Red), None);
        assert!(p.contains_rgb(Rgb::new(0, 0, 0xaa)));
        assert!(!p.contains_rgb(Rgb::new(0xaa, 0, 0)));
    }

    #[test]
    fn single_color_subset_is_allowed() {
        assert_eq!(Palette::new([Yellow]).unwrap().len(), 1);
    }

    #[test]
    fn empty_subset_is_rejected() {
        assert_eq!(Palette::new([]), Err(PaletteError::Empty));
    }
}
