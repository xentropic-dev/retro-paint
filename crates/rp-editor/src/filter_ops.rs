//! Filter-panel commands on [`EditorState`]: every v1 dither control from
//! docs/filter-and-palette.md "User controls (v1)".
//!
//! All commands edit [`Document::dither`](rp_doc::Document::dither) and bump
//! [`EditorState::doc_version`] only when the settings really changed, so a
//! click that sets the current value does not trigger a re-render.
//!
//! Decisions:
//! - The UI works in whole percent: pre-adjust `-100..=100` (stored
//!   `-1.0..=1.0`), strength and Bayer spread `0..=100` (stored
//!   `0.0..=1.0`). Values are clamped on the way in.
//! - Switching palette mode remembers the 4-color mode and custom subset that
//!   were last used, so flipping Full → 4-color → Full → 4-color restores the
//!   same preset, intensity and background. A first switch to 4-color uses
//!   Palette 1, high intensity, black background; a first switch to Custom
//!   starts from the colors of the palette that was active.
//! - A custom subset keeps doc (swatch) order and always has at least one
//!   color: the last one cannot be toggled off.
//! - Every change is one undo step. Consecutive changes of one slider
//!   (bar clicks, −/+ steps) merge into one step; a reset is its own step.
//!   The palette-mode memory is editor state, not undone.
//! - Settings for controls that the current method ignores (Bayer spread
//!   for diffusion, strength/serpentine for Bayer) are kept, just hidden.

use rp_palette_dither::{
    CgaColor, DEFAULT_BAYER_SPREAD, DitherMethod, DitherSettings, FourColor, Intensity, Palette,
    PaletteChoice, Preset,
};

use crate::state::EditorState;
use crate::undo_ops::{Coalesce, EditLabel};

/// Step of the panel's −/+ steppers, in percent.
pub const FILTER_STEP: i32 = 5;

/// The 4-color mode a first switch to 4-color uses.
pub const DEFAULT_FOUR_COLOR: FourColor = FourColor::new(Preset::Palette1, Intensity::High);

/// Palette mode without its parameters (the panel's mode buttons).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PaletteMode {
    Full,
    FourColor,
    Custom,
}

impl PaletteMode {
    pub const ALL: [PaletteMode; 3] = [
        PaletteMode::Full,
        PaletteMode::FourColor,
        PaletteMode::Custom,
    ];

    pub fn of(choice: &PaletteChoice) -> Self {
        match choice {
            PaletteChoice::Full => PaletteMode::Full,
            PaletteChoice::FourColor(_) => PaletteMode::FourColor,
            PaletteChoice::Custom(_) => PaletteMode::Custom,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            PaletteMode::Full => "Full 16",
            PaletteMode::FourColor => "4-color",
            PaletteMode::Custom => "Custom",
        }
    }
}

/// One pre-adjust slider.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AdjustControl {
    Brightness,
    Contrast,
    Saturation,
}

impl AdjustControl {
    pub const ALL: [AdjustControl; 3] = [
        AdjustControl::Brightness,
        AdjustControl::Contrast,
        AdjustControl::Saturation,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            AdjustControl::Brightness => "Bright",
            AdjustControl::Contrast => "Contrast",
            AdjustControl::Saturation => "Satur.",
        }
    }

    fn get(self, s: &DitherSettings) -> f32 {
        match self {
            AdjustControl::Brightness => s.adjust.brightness,
            AdjustControl::Contrast => s.adjust.contrast,
            AdjustControl::Saturation => s.adjust.saturation,
        }
    }

    fn slot(self, s: &mut DitherSettings) -> &mut f32 {
        match self {
            AdjustControl::Brightness => &mut s.adjust.brightness,
            AdjustControl::Contrast => &mut s.adjust.contrast,
            AdjustControl::Saturation => &mut s.adjust.saturation,
        }
    }
}

/// A percent control of the panel: a bar, −/+ steppers, a numeric readout
/// and a reset. Dispatches to the [`EditorState`] setters.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FilterSlider {
    Adjust(AdjustControl),
    BayerSpread,
    Strength,
}

impl FilterSlider {
    pub const fn label(self) -> &'static str {
        match self {
            FilterSlider::Adjust(c) => c.label(),
            FilterSlider::BayerSpread => "Spread",
            FilterSlider::Strength => "Strength",
        }
    }

    /// `(min, max)` in percent.
    pub const fn range(self) -> (i32, i32) {
        match self {
            FilterSlider::Adjust(_) => (-100, 100),
            FilterSlider::BayerSpread | FilterSlider::Strength => (0, 100),
        }
    }

    pub fn value(self, st: &EditorState) -> i32 {
        match self {
            FilterSlider::Adjust(c) => st.adjust_percent(c),
            FilterSlider::BayerSpread => st.bayer_spread_percent(),
            FilterSlider::Strength => st.strength_percent(),
        }
    }

    pub fn set(self, st: &mut EditorState, percent: i32) -> bool {
        match self {
            FilterSlider::Adjust(c) => st.set_adjust_percent(c, percent),
            FilterSlider::BayerSpread => st.set_bayer_spread_percent(percent),
            FilterSlider::Strength => st.set_strength_percent(percent),
        }
    }

    pub fn step(self, st: &mut EditorState, delta: i32) -> bool {
        self.set(st, self.value(st) + delta)
    }

    pub fn reset(self, st: &mut EditorState) -> bool {
        match self {
            FilterSlider::Adjust(c) => st.reset_adjust(c),
            FilterSlider::BayerSpread => st.reset_bayer_spread(),
            FilterSlider::Strength => st.reset_strength(),
        }
    }

    /// The clickable bar: `segments` cells, each `(value it sets, filled)`.
    /// Unipolar bars fill from the left; the bipolar pre-adjust bar fills
    /// outward from its center, so `segments` should be even.
    pub fn bar(self, value: i32, segments: i32) -> Vec<(i32, bool)> {
        let (lo, hi) = self.range();
        let step = (hi - lo) / segments;
        (0..segments)
            .map(|i| {
                let start = lo + i * step;
                if lo < 0 && start < 0 {
                    (start, value <= start)
                } else {
                    let end = start + step;
                    (end, value >= end)
                }
            })
            .collect()
    }

    /// Readout text: signed for pre-adjust (`+20`, `-5`, `0`), else `40%`.
    pub fn readout(self, value: i32) -> String {
        match self {
            FilterSlider::Adjust(_) if value > 0 => format!("+{value}"),
            FilterSlider::Adjust(_) => format!("{value}"),
            _ => format!("{value}%"),
        }
    }
}

/// Memory of the palette modes not currently selected (see the module docs).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PaletteMemory {
    pub four_color: Option<FourColor>,
    pub custom: Option<Palette>,
}

pub const fn method_label(m: DitherMethod) -> &'static str {
    match m {
        DitherMethod::None => "None",
        DitherMethod::Bayer4 => "Bayer 4×4",
        DitherMethod::FloydSteinberg => "Floyd–Steinberg",
        DitherMethod::Atkinson => "Atkinson",
    }
}

/// Short label for the method buttons.
pub const fn method_short_label(m: DitherMethod) -> &'static str {
    match m {
        DitherMethod::None => "None",
        DitherMethod::Bayer4 => "Bayer",
        DitherMethod::FloydSteinberg => "F–S",
        DitherMethod::Atkinson => "Atkinson",
    }
}

pub const PRESETS: [Preset; 3] = [Preset::Palette0, Preset::Palette1, Preset::Mode5];

pub const fn preset_label(p: Preset) -> &'static str {
    match p {
        Preset::Palette0 => "Palette 0",
        Preset::Palette1 => "Palette 1",
        Preset::Mode5 => "Mode 5",
    }
}

pub const fn intensity_label(i: Intensity) -> &'static str {
    match i {
        Intensity::Low => "Low",
        Intensity::High => "High",
    }
}

pub const fn color_name(c: CgaColor) -> &'static str {
    match c {
        CgaColor::Black => "black",
        CgaColor::DarkGray => "dark gray",
        CgaColor::LightGray => "light gray",
        CgaColor::White => "white",
        CgaColor::Blue => "blue",
        CgaColor::LightBlue => "light blue",
        CgaColor::Green => "green",
        CgaColor::LightGreen => "light green",
        CgaColor::Cyan => "cyan",
        CgaColor::LightCyan => "light cyan",
        CgaColor::Red => "red",
        CgaColor::LightRed => "light red",
        CgaColor::Magenta => "magenta",
        CgaColor::LightMagenta => "light magenta",
        CgaColor::Brown => "brown",
        CgaColor::Yellow => "yellow",
    }
}

fn to_percent(v: f32) -> i32 {
    (v * 100.0).round() as i32
}

impl EditorState {
    /// The document's dither settings.
    pub fn dither(&self) -> &DitherSettings {
        &self.document.dither
    }

    /// Applies `f` to the dither settings; bumps the document version and
    /// returns `true` only if they changed.
    fn modify_dither(&mut self, f: impl FnOnce(&mut DitherSettings)) -> bool {
        self.record(EditLabel::Filter, |s| s.modify_dither_now(f))
    }

    /// Like `modify_dither`, merging into the previous step if it was the
    /// same slider.
    fn modify_slider(&mut self, slider: FilterSlider, f: impl FnOnce(&mut DitherSettings)) -> bool {
        self.record_coalesced(EditLabel::Filter, Coalesce::Filter(slider), |s| {
            s.modify_dither_now(f)
        })
    }

    fn modify_dither_now(&mut self, f: impl FnOnce(&mut DitherSettings)) -> bool {
        let before = self.document.dither;
        f(&mut self.document.dither);
        let changed = self.document.dither != before;
        if changed {
            self.mark_document_changed();
        }
        changed
    }

    pub fn set_dither_method(&mut self, method: DitherMethod) -> bool {
        self.modify_dither(|s| s.method = method)
    }

    /// Whether the Bayer spread control applies (Bayer 4×4).
    pub fn bayer_controls_enabled(&self) -> bool {
        self.dither().method == DitherMethod::Bayer4
    }

    /// Whether strength and serpentine apply (error-diffusion methods).
    pub fn diffusion_controls_enabled(&self) -> bool {
        self.dither().method.kernel().is_some()
    }

    pub fn palette_mode(&self) -> PaletteMode {
        PaletteMode::of(&self.dither().palette)
    }

    /// Switches palette mode, restoring the remembered parameters of the
    /// target mode and remembering those of the current one.
    pub fn set_palette_mode(&mut self, mode: PaletteMode) -> bool {
        let current = self.dither().palette;
        if PaletteMode::of(&current) == mode {
            return false;
        }
        match current {
            PaletteChoice::FourColor(m) => self.palette_memory.four_color = Some(m),
            PaletteChoice::Custom(p) => self.palette_memory.custom = Some(p),
            PaletteChoice::Full => {}
        }
        let next = match mode {
            PaletteMode::Full => PaletteChoice::Full,
            PaletteMode::FourColor => PaletteChoice::FourColor(
                self.palette_memory.four_color.unwrap_or(DEFAULT_FOUR_COLOR),
            ),
            PaletteMode::Custom => PaletteChoice::Custom(
                self.palette_memory
                    .custom
                    .unwrap_or_else(|| doc_ordered(current.palette())),
            ),
        };
        self.modify_dither(|s| s.palette = next)
    }

    /// The current 4-color mode, if that is the palette mode.
    pub fn four_color(&self) -> Option<FourColor> {
        match self.dither().palette {
            PaletteChoice::FourColor(m) => Some(m),
            _ => None,
        }
    }

    fn modify_four_color(&mut self, f: impl FnOnce(&mut FourColor)) -> bool {
        let Some(mut m) = self.four_color() else {
            return false;
        };
        f(&mut m);
        self.modify_dither(|s| s.palette = PaletteChoice::FourColor(m))
    }

    /// Sets the 4-color preset; no-op outside 4-color mode.
    pub fn set_four_color_preset(&mut self, preset: Preset) -> bool {
        self.modify_four_color(|m| m.preset = preset)
    }

    /// Sets the 4-color intensity; no-op outside 4-color mode.
    pub fn set_four_color_intensity(&mut self, intensity: Intensity) -> bool {
        self.modify_four_color(|m| m.intensity = intensity)
    }

    /// Sets the 4-color background (color 0); no-op outside 4-color mode.
    pub fn set_four_color_background(&mut self, background: CgaColor) -> bool {
        self.modify_four_color(|m| m.background = background)
    }

    /// The custom subset, if that is the palette mode.
    pub fn custom_palette(&self) -> Option<Palette> {
        match self.dither().palette {
            PaletteChoice::Custom(p) => Some(p),
            _ => None,
        }
    }

    /// Adds or removes `color` from the custom subset. Refuses to remove the
    /// last color (sets the status line); no-op outside Custom mode.
    pub fn toggle_custom_color(&mut self, color: CgaColor) -> bool {
        let Some(p) = self.custom_palette() else {
            return false;
        };
        let keep = |c: CgaColor| {
            if c == color {
                !p.contains(c)
            } else {
                p.contains(c)
            }
        };
        match Palette::new(CgaColor::ALL.into_iter().filter(|&c| keep(c))) {
            Ok(next) => self.modify_dither(|s| s.palette = PaletteChoice::Custom(next)),
            Err(_) => {
                self.status = "A custom palette needs at least one color".into();
                false
            }
        }
    }

    /// Pre-adjust value in percent, `-100..=100`.
    pub fn adjust_percent(&self, control: AdjustControl) -> i32 {
        to_percent(control.get(self.dither()))
    }

    /// Sets a pre-adjust control in percent, clamped to `-100..=100`.
    pub fn set_adjust_percent(&mut self, control: AdjustControl, percent: i32) -> bool {
        let v = percent.clamp(-100, 100) as f32 / 100.0;
        self.modify_slider(FilterSlider::Adjust(control), |s| *control.slot(s) = v)
    }

    pub fn step_adjust(&mut self, control: AdjustControl, delta: i32) -> bool {
        self.set_adjust_percent(control, self.adjust_percent(control) + delta)
    }

    pub fn reset_adjust(&mut self, control: AdjustControl) -> bool {
        self.modify_dither(|s| *control.slot(s) = 0.0)
    }

    /// Bayer spread in percent of the full sRGB range, `0..=100`.
    pub fn bayer_spread_percent(&self) -> i32 {
        to_percent(self.dither().bayer_spread)
    }

    pub fn set_bayer_spread_percent(&mut self, percent: i32) -> bool {
        let v = percent.clamp(0, 100) as f32 / 100.0;
        self.modify_slider(FilterSlider::BayerSpread, |s| s.bayer_spread = v)
    }

    pub fn step_bayer_spread(&mut self, delta: i32) -> bool {
        self.set_bayer_spread_percent(self.bayer_spread_percent() + delta)
    }

    /// Back to one CGA level step ([`DEFAULT_BAYER_SPREAD`]).
    pub fn reset_bayer_spread(&mut self) -> bool {
        self.modify_dither(|s| s.bayer_spread = DEFAULT_BAYER_SPREAD)
    }

    /// Error-diffusion strength in percent, `0..=100`.
    pub fn strength_percent(&self) -> i32 {
        to_percent(self.dither().strength)
    }

    pub fn set_strength_percent(&mut self, percent: i32) -> bool {
        let v = percent.clamp(0, 100) as f32 / 100.0;
        self.modify_slider(FilterSlider::Strength, |s| s.strength = v)
    }

    pub fn step_strength(&mut self, delta: i32) -> bool {
        self.set_strength_percent(self.strength_percent() + delta)
    }

    pub fn reset_strength(&mut self) -> bool {
        self.modify_dither(|s| s.strength = 1.0)
    }

    pub fn toggle_serpentine(&mut self) -> bool {
        self.modify_dither(|s| s.serpentine = !s.serpentine)
    }

    /// One-line summary for the panel, e.g.
    /// `"Floyd–Steinberg · 16 colors · 100% · serpentine"`.
    pub fn dither_summary(&self) -> String {
        let s = self.dither();
        let palette = match s.palette {
            PaletteChoice::Full => "16 colors".to_string(),
            PaletteChoice::FourColor(m) => format!(
                "{} {} on {}",
                preset_label(m.preset),
                intensity_label(m.intensity).to_lowercase(),
                color_name(m.background)
            ),
            PaletteChoice::Custom(p) => format!("custom {} colors", p.len()),
        };
        let mut parts = vec![method_label(s.method).to_string(), palette];
        if self.bayer_controls_enabled() {
            parts.push(format!("spread {}%", self.bayer_spread_percent()));
        }
        if self.diffusion_controls_enabled() {
            parts.push(format!("{}%", self.strength_percent()));
            if s.serpentine {
                parts.push("serpentine".into());
            }
        }
        parts.join(" · ")
    }
}

/// `p`'s colors in doc order (swatch order in the panel).
fn doc_ordered(p: Palette) -> Palette {
    Palette::new(CgaColor::ALL.into_iter().filter(|&c| p.contains(c)))
        .expect("palette is never empty")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rp_palette_dither::PreAdjust;

    fn bumps(st: &mut EditorState, f: impl FnOnce(&mut EditorState) -> bool) -> bool {
        let v = st.doc_version;
        let changed = f(st);
        assert_eq!(changed, st.doc_version != v, "return value matches bump");
        changed
    }

    #[test]
    fn defaults_match_the_document() {
        let st = EditorState::new();
        assert_eq!(*st.dither(), DitherSettings::default());
        assert_eq!(st.palette_mode(), PaletteMode::Full);
        assert!(st.diffusion_controls_enabled());
        assert!(!st.bayer_controls_enabled());
        assert_eq!(st.strength_percent(), 100);
        assert_eq!(st.bayer_spread_percent(), 33);
        for c in AdjustControl::ALL {
            assert_eq!(st.adjust_percent(c), 0);
        }
    }

    #[test]
    fn method_change_bumps_only_on_change() {
        let mut st = EditorState::new();
        assert!(!bumps(&mut st, |s| s
            .set_dither_method(DitherMethod::FloydSteinberg)));
        for m in DitherMethod::ALL {
            st.set_dither_method(m);
            assert_eq!(st.dither().method, m);
            assert_eq!(st.bayer_controls_enabled(), m == DitherMethod::Bayer4);
            assert_eq!(
                st.diffusion_controls_enabled(),
                matches!(m, DitherMethod::FloydSteinberg | DitherMethod::Atkinson)
            );
        }
        assert!(bumps(&mut st, |s| s.set_dither_method(DitherMethod::None)));
    }

    #[test]
    fn method_switch_keeps_hidden_settings() {
        let mut st = EditorState::new();
        st.set_strength_percent(40);
        st.toggle_serpentine();
        st.set_dither_method(DitherMethod::Bayer4);
        st.set_bayer_spread_percent(70);
        st.set_dither_method(DitherMethod::Atkinson);
        assert_eq!(st.strength_percent(), 40);
        assert!(!st.dither().serpentine);
        assert_eq!(st.bayer_spread_percent(), 70);
    }

    #[test]
    fn adjust_clamps_steps_and_resets() {
        let mut st = EditorState::new();
        for c in AdjustControl::ALL {
            assert!(bumps(&mut st, |s| s.set_adjust_percent(c, 250)));
            assert_eq!(st.adjust_percent(c), 100);
            assert!(!bumps(&mut st, |s| s.step_adjust(c, FILTER_STEP)));
            assert!(bumps(&mut st, |s| s.set_adjust_percent(c, -400)));
            assert_eq!(st.adjust_percent(c), -100);
            assert!(!bumps(&mut st, |s| s.step_adjust(c, -FILTER_STEP)));
            st.step_adjust(c, FILTER_STEP);
            assert_eq!(st.adjust_percent(c), -95);
            assert!(bumps(&mut st, |s| s.reset_adjust(c)));
            assert_eq!(st.adjust_percent(c), 0);
            assert!(!bumps(&mut st, |s| s.reset_adjust(c)));
        }
        st.set_adjust_percent(AdjustControl::Contrast, 30);
        assert_eq!(
            st.dither().adjust,
            PreAdjust {
                contrast: 0.3,
                ..Default::default()
            }
        );
        // Steps land on exact percents, no float drift.
        for _ in 0..7 {
            st.step_adjust(AdjustControl::Brightness, FILTER_STEP);
        }
        assert_eq!(st.adjust_percent(AdjustControl::Brightness), 35);
        assert_eq!(st.dither().adjust.brightness, 0.35);
    }

    #[test]
    fn strength_and_spread_clamp_and_reset() {
        let mut st = EditorState::new();
        assert!(!bumps(&mut st, |s| s.step_strength(FILTER_STEP)));
        assert!(bumps(&mut st, |s| s.set_strength_percent(-20)));
        assert_eq!(st.dither().strength, 0.0);
        assert!(bumps(&mut st, |s| s.reset_strength()));
        assert_eq!(st.dither().strength, 1.0);

        assert!(bumps(&mut st, |s| s.set_bayer_spread_percent(140)));
        assert_eq!(st.dither().bayer_spread, 1.0);
        assert!(bumps(&mut st, |s| s.set_bayer_spread_percent(-3)));
        assert_eq!(st.dither().bayer_spread, 0.0);
        st.step_bayer_spread(FILTER_STEP);
        assert_eq!(st.bayer_spread_percent(), 5);
        assert!(bumps(&mut st, |s| s.reset_bayer_spread()));
        assert_eq!(st.dither().bayer_spread, DEFAULT_BAYER_SPREAD);
        assert!(!bumps(&mut st, |s| s.reset_bayer_spread()));
    }

    #[test]
    fn serpentine_toggles() {
        let mut st = EditorState::new();
        assert!(st.dither().serpentine);
        assert!(bumps(&mut st, |s| s.toggle_serpentine()));
        assert!(!st.dither().serpentine);
        st.toggle_serpentine();
        assert!(st.dither().serpentine);
    }

    #[test]
    fn four_color_mode_and_its_controls() {
        let mut st = EditorState::new();
        // Controls do nothing outside 4-color mode.
        assert!(!bumps(&mut st, |s| s.set_four_color_preset(Preset::Mode5)));
        assert!(!bumps(&mut st, |s| s.set_four_color_background(CgaColor::Blue)));
        assert!(bumps(&mut st, |s| s.set_palette_mode(PaletteMode::FourColor)));
        assert_eq!(st.four_color(), Some(DEFAULT_FOUR_COLOR));
        assert_eq!(st.four_color().unwrap().background, CgaColor::Black);
        assert!(!bumps(&mut st, |s| s.set_palette_mode(PaletteMode::FourColor)));

        assert!(bumps(&mut st, |s| s.set_four_color_preset(Preset::Palette0)));
        assert!(bumps(&mut st, |s| s.set_four_color_intensity(Intensity::Low)));
        assert!(bumps(&mut st, |s| s.set_four_color_background(CgaColor::Blue)));
        assert!(!bumps(&mut st, |s| s.set_four_color_background(CgaColor::Blue)));
        let m = st.four_color().unwrap();
        assert_eq!(
            m,
            FourColor::new(Preset::Palette0, Intensity::Low).with_background(CgaColor::Blue)
        );
        assert_eq!(
            st.dither().palette.palette().colors(),
            &[
                CgaColor::Blue,
                CgaColor::Green,
                CgaColor::Red,
                CgaColor::Brown
            ]
        );
        // Any of the 16 can be the background.
        for c in CgaColor::ALL {
            st.set_four_color_background(c);
            assert_eq!(st.four_color().unwrap().background, c);
        }
    }

    #[test]
    fn switching_modes_remembers_each_modes_values() {
        let mut st = EditorState::new();
        st.set_palette_mode(PaletteMode::FourColor);
        st.set_four_color_preset(Preset::Mode5);
        st.set_four_color_background(CgaColor::DarkGray);
        let four = st.four_color().unwrap();

        // First Custom starts from the active 4-color palette, in doc order.
        st.set_palette_mode(PaletteMode::Custom);
        let custom = st.custom_palette().unwrap();
        assert_eq!(
            custom.colors(),
            &[
                CgaColor::DarkGray,
                CgaColor::White,
                CgaColor::LightCyan,
                CgaColor::LightRed
            ]
        );
        st.toggle_custom_color(CgaColor::Yellow);
        let custom = st.custom_palette().unwrap();

        st.set_palette_mode(PaletteMode::Full);
        assert_eq!(st.dither().palette, PaletteChoice::Full);
        st.set_palette_mode(PaletteMode::FourColor);
        assert_eq!(st.four_color(), Some(four));
        st.set_palette_mode(PaletteMode::Custom);
        assert_eq!(st.custom_palette(), Some(custom));
    }

    #[test]
    fn first_custom_from_full_has_all_sixteen() {
        let mut st = EditorState::new();
        st.set_palette_mode(PaletteMode::Custom);
        assert_eq!(st.custom_palette(), Some(Palette::full()));
    }

    #[test]
    fn custom_toggles_keep_doc_order_and_the_last_color() {
        let mut st = EditorState::new();
        assert!(!bumps(&mut st, |s| s.toggle_custom_color(CgaColor::Red)));
        st.set_palette_mode(PaletteMode::Custom);
        for c in CgaColor::ALL.into_iter().skip(1) {
            assert!(bumps(&mut st, |s| s.toggle_custom_color(c)));
        }
        assert_eq!(st.custom_palette().unwrap().colors(), &[CgaColor::Black]);
        assert!(!bumps(&mut st, |s| s.toggle_custom_color(CgaColor::Black)));
        assert_eq!(st.custom_palette().unwrap().colors(), &[CgaColor::Black]);
        assert!(st.status.contains("at least one"), "{}", st.status);
        st.toggle_custom_color(CgaColor::Yellow);
        st.toggle_custom_color(CgaColor::Blue);
        assert_eq!(
            st.custom_palette().unwrap().colors(),
            &[CgaColor::Black, CgaColor::Blue, CgaColor::Yellow]
        );
        // Now Black can go.
        assert!(st.toggle_custom_color(CgaColor::Black));
        assert_eq!(
            st.custom_palette().unwrap().colors(),
            &[CgaColor::Blue, CgaColor::Yellow]
        );
    }

    #[test]
    fn new_document_forgets_palette_memory() {
        let mut st = EditorState::new();
        st.set_palette_mode(PaletteMode::FourColor);
        st.set_four_color_preset(Preset::Palette0);
        st.set_palette_mode(PaletteMode::Full);
        st.new_document_now();
        st.set_palette_mode(PaletteMode::FourColor);
        assert_eq!(st.four_color(), Some(DEFAULT_FOUR_COLOR));
    }

    #[test]
    fn summary_describes_settings() {
        let mut st = EditorState::new();
        assert_eq!(
            st.dither_summary(),
            "Floyd–Steinberg · 16 colors · 100% · serpentine"
        );
        st.set_dither_method(DitherMethod::Bayer4);
        st.set_palette_mode(PaletteMode::FourColor);
        st.set_four_color_background(CgaColor::Blue);
        assert_eq!(
            st.dither_summary(),
            "Bayer 4×4 · Palette 1 high on blue · spread 33%"
        );
        st.set_dither_method(DitherMethod::None);
        st.set_palette_mode(PaletteMode::Custom);
        assert_eq!(st.dither_summary(), "None · custom 4 colors");
    }

    #[test]
    fn sliders_dispatch_to_their_setting() {
        let mut st = EditorState::new();
        let all = [
            FilterSlider::Adjust(AdjustControl::Brightness),
            FilterSlider::Adjust(AdjustControl::Contrast),
            FilterSlider::Adjust(AdjustControl::Saturation),
            FilterSlider::BayerSpread,
            FilterSlider::Strength,
        ];
        for sl in all {
            let (lo, hi) = sl.range();
            assert!(bumps(&mut st, |s| sl.set(s, 50)));
            assert_eq!(sl.value(&st), 50);
            sl.step(&mut st, FILTER_STEP);
            assert_eq!(sl.value(&st), 55);
            sl.set(&mut st, hi + 50);
            assert_eq!(sl.value(&st), hi);
            sl.set(&mut st, lo - 50);
            assert_eq!(sl.value(&st), lo);
            sl.reset(&mut st);
        }
        assert_eq!(*st.dither(), DitherSettings::default());
        let b = FilterSlider::Adjust(AdjustControl::Brightness);
        assert_eq!(b.readout(20), "+20");
        assert_eq!(b.readout(-5), "-5");
        assert_eq!(b.readout(0), "0");
        assert_eq!(FilterSlider::Strength.readout(40), "40%");
    }

    #[test]
    fn bars_fill_from_left_or_center() {
        let s = FilterSlider::Strength.bar(30, 10);
        assert_eq!(s.len(), 10);
        assert_eq!(s[0], (10, true));
        assert_eq!(s[2], (30, true));
        assert_eq!(s[3], (40, false));
        assert_eq!(s[9], (100, false));
        let b = FilterSlider::Adjust(AdjustControl::Contrast);
        let bar = b.bar(0, 20);
        assert_eq!(bar[0], (-100, false));
        assert_eq!(bar[9], (-10, false));
        assert_eq!(bar[10], (10, false));
        assert_eq!(bar[19], (100, false));
        let bar = b.bar(-25, 20);
        let filled: Vec<_> = bar.iter().filter(|c| c.1).map(|c| c.0).collect();
        assert_eq!(filled, [-20, -10]);
        let bar = b.bar(100, 20);
        assert_eq!(bar.iter().filter(|c| c.1).count(), 10);
    }

    #[test]
    fn labels_are_distinct() {
        use std::collections::HashSet;
        let n: HashSet<_> = CgaColor::ALL.map(color_name).into_iter().collect();
        assert_eq!(n.len(), 16);
        let m: HashSet<_> = DitherMethod::ALL
            .map(method_short_label)
            .into_iter()
            .collect();
        assert_eq!(m.len(), 4);
        let p: HashSet<_> = PRESETS.map(preset_label).into_iter().collect();
        assert_eq!(p.len(), 3);
    }
}
