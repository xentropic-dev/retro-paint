//! Foreground / background colors and the Swatches panel, with no GPUI types.
//!
//! The colors and swatches live in [`rp_doc::Document`] so `.rpaint` files
//! keep them, but none of these edits change pixels: they never bump
//! [`EditorState::doc_version`] and never re-dither. A foreground change
//! reaches the DOS view as an overlay update (see
//! [`crate::render_worker::Overlay`]).

use rp_palette_dither::Rgb;

use crate::state::EditorState;

/// Most swatches a document keeps. Adding beyond this is refused.
pub const MAX_SWATCHES: usize = 64;

/// `D` resets the foreground to this (GIMP-style)…
pub const DEFAULT_FG: Rgb = Rgb::new(0, 0, 0);
/// …and the background to this.
pub const DEFAULT_BG: Rgb = Rgb::new(255, 255, 255);

/// `#rrggbb`.
pub fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// Result of [`add_swatch`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AddSwatch {
    /// Appended at the end.
    Added,
    /// Was already saved; moved to the end (newest last).
    Moved,
    /// The list already holds [`MAX_SWATCHES`].
    Full,
}

/// Saves `c` at the end of `swatches`. A color that is already saved moves
/// to the end instead of appearing twice; a full list is left alone.
pub fn add_swatch(swatches: &mut Vec<Rgb>, c: Rgb) -> AddSwatch {
    if let Some(i) = swatches.iter().position(|&s| s == c) {
        swatches.remove(i);
        swatches.push(c);
        return AddSwatch::Moved;
    }
    if swatches.len() >= MAX_SWATCHES {
        return AddSwatch::Full;
    }
    swatches.push(c);
    AddSwatch::Added
}

/// Removes the swatch at `index`; returns it, or `None` if out of range.
pub fn remove_swatch(swatches: &mut Vec<Rgb>, index: usize) -> Option<Rgb> {
    (index < swatches.len()).then(|| swatches.remove(index))
}

/// A color key binding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorKey {
    /// `X`: swap foreground and background.
    Swap,
    /// `D`: reset to black on white.
    Reset,
}

/// The color command for a key press with no modifiers held (`plain`
/// excludes ctrl, alt, super and shift), or `None`.
pub fn color_key(key: &str, plain: bool) -> Option<ColorKey> {
    if !plain {
        return None;
    }
    match key {
        "x" => Some(ColorKey::Swap),
        "d" => Some(ColorKey::Reset),
        _ => None,
    }
}

impl EditorState {
    /// Runs a [`ColorKey`].
    pub fn color_key(&mut self, key: ColorKey) {
        match key {
            ColorKey::Swap => self.swap_colors(),
            ColorKey::Reset => self.reset_colors(),
        }
    }

    /// Swaps foreground and background (`X`, or the ribbon's swap arrow).
    pub fn swap_colors(&mut self) {
        let d = &mut self.document;
        std::mem::swap(&mut d.fg, &mut d.bg);
        self.status = format!("Foreground {}, background {}", hex(d.fg), hex(d.bg));
    }

    /// Resets to black foreground, white background (`D`).
    pub fn reset_colors(&mut self) {
        self.document.fg = DEFAULT_FG;
        self.document.bg = DEFAULT_BG;
        self.status = "Colors reset: black on white".into();
    }

    /// Swatches panel "+": saves the foreground. See [`add_swatch`].
    pub fn add_fg_swatch(&mut self) -> AddSwatch {
        let c = self.document.fg;
        let r = add_swatch(&mut self.document.swatches, c);
        self.status = match r {
            AddSwatch::Added => format!("Saved swatch {}", hex(c)),
            AddSwatch::Moved => format!("Swatch {} moved to the end", hex(c)),
            AddSwatch::Full => {
                format!("Swatches full ({MAX_SWATCHES}): remove one first")
            }
        };
        r
    }

    /// Removes swatch `index` (middle-click on it).
    pub fn remove_swatch_at(&mut self, index: usize) -> bool {
        match remove_swatch(&mut self.document.swatches, index) {
            Some(c) => {
                self.status = format!("Removed swatch {}", hex(c));
                true
            }
            None => false,
        }
    }

    /// Swatches panel "−": removes the swatch equal to the foreground, if
    /// one is saved.
    pub fn remove_fg_swatch(&mut self) -> bool {
        let fg = self.document.fg;
        match self.document.swatches.iter().position(|&s| s == fg) {
            Some(i) => self.remove_swatch_at(i),
            None => {
                self.status = format!("{} is not a saved swatch", hex(fg));
                false
            }
        }
    }

    /// Click on swatch `index`: sets the foreground (`bg == false`) or the
    /// background (`bg == true`, right-click).
    pub fn pick_swatch(&mut self, index: usize, bg: bool) -> bool {
        let Some(&c) = self.document.swatches.get(index) else {
            return false;
        };
        let which = if bg {
            self.document.bg = c;
            "Background"
        } else {
            self.document.fg = c;
            "Foreground"
        };
        self.status = format!("{which} {}", hex(c));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_worker::{Overlay, Request, next_request};

    fn c(n: u8) -> Rgb {
        Rgb::new(n, n.wrapping_mul(3), 7)
    }

    #[test]
    fn add_appends_and_moves_duplicates_to_the_end() {
        let mut s = vec![];
        assert_eq!(add_swatch(&mut s, c(1)), AddSwatch::Added);
        assert_eq!(add_swatch(&mut s, c(2)), AddSwatch::Added);
        assert_eq!(add_swatch(&mut s, c(3)), AddSwatch::Added);
        assert_eq!(add_swatch(&mut s, c(1)), AddSwatch::Moved);
        assert_eq!(s, [c(2), c(3), c(1)]);
        assert_eq!(add_swatch(&mut s, c(1)), AddSwatch::Moved);
        assert_eq!(s, [c(2), c(3), c(1)]);
    }

    #[test]
    fn add_is_capped_but_duplicates_still_move_when_full() {
        let mut s: Vec<_> = (0..MAX_SWATCHES as u8).map(c).collect();
        assert_eq!(add_swatch(&mut s, Rgb::new(255, 255, 254)), AddSwatch::Full);
        assert_eq!(s.len(), MAX_SWATCHES);
        assert_eq!(add_swatch(&mut s, c(0)), AddSwatch::Moved);
        assert_eq!(s.len(), MAX_SWATCHES);
        assert_eq!(s.last(), Some(&c(0)));
    }

    #[test]
    fn remove_by_index() {
        let mut s = vec![c(1), c(2), c(3)];
        assert_eq!(remove_swatch(&mut s, 1), Some(c(2)));
        assert_eq!(s, [c(1), c(3)]);
        assert_eq!(remove_swatch(&mut s, 2), None);
        assert_eq!(s, [c(1), c(3)]);
    }

    #[test]
    fn swap_and_reset() {
        let mut st = EditorState::new();
        st.document.fg = c(10);
        st.document.bg = c(20);
        st.swap_colors();
        assert_eq!((st.document.fg, st.document.bg), (c(20), c(10)));
        st.swap_colors();
        assert_eq!((st.document.fg, st.document.bg), (c(10), c(20)));
        st.reset_colors();
        assert_eq!(st.document.fg, Rgb::new(0, 0, 0));
        assert_eq!(st.document.bg, Rgb::new(255, 255, 255));
    }

    #[test]
    fn panel_add_remove_and_pick() {
        let mut st = EditorState::new();
        st.document.fg = c(5);
        assert_eq!(st.add_fg_swatch(), AddSwatch::Added);
        st.document.fg = c(6);
        st.add_fg_swatch();
        assert_eq!(st.document.swatches, [c(5), c(6)]);
        assert!(st.status.contains("#"), "{}", st.status);

        assert!(st.pick_swatch(0, false));
        assert_eq!(st.document.fg, c(5));
        assert!(st.pick_swatch(1, true));
        assert_eq!(st.document.bg, c(6));
        assert!(!st.pick_swatch(2, false));
        assert_eq!(st.document.fg, c(5));

        // "−" removes the swatch matching the foreground.
        assert!(st.remove_fg_swatch());
        assert_eq!(st.document.swatches, [c(6)]);
        assert!(!st.remove_fg_swatch());
        assert!(st.status.contains("not a saved swatch"), "{}", st.status);

        assert!(st.remove_swatch_at(0));
        assert!(st.document.swatches.is_empty());
        assert!(!st.remove_swatch_at(0));
    }

    #[test]
    fn full_list_refuses_with_a_status() {
        let mut st = EditorState::new();
        st.document.swatches = (0..MAX_SWATCHES as u8).map(c).collect();
        st.document.fg = Rgb::new(255, 255, 254);
        assert_eq!(st.add_fg_swatch(), AddSwatch::Full);
        assert!(st.status.contains("full"), "{}", st.status);
    }

    #[test]
    fn color_and_swatch_edits_never_re_render() {
        let mut st = EditorState::new();
        let v = st.doc_version;
        let before = Overlay::from_state(&st);
        st.document.fg = c(9);
        st.add_fg_swatch();
        st.swap_colors();
        st.pick_swatch(0, false);
        st.pick_swatch(0, true);
        st.remove_fg_swatch();
        st.reset_colors();
        st.document.fg = c(9);
        assert_eq!(st.doc_version, v);
        // Only the overlay (the DOS active color) updates.
        let after = Overlay::from_state(&st);
        assert_eq!(after.fg, c(9));
        assert_eq!(
            next_request(Some((v, before)), st.doc_version, &after),
            Request::Overlay
        );
    }

    #[test]
    fn x_swaps_and_d_resets_only_without_modifiers() {
        assert_eq!(color_key("x", true), Some(ColorKey::Swap));
        assert_eq!(color_key("d", true), Some(ColorKey::Reset));
        assert_eq!(color_key("x", false), None);
        assert_eq!(color_key("d", false), None);
        assert_eq!(color_key("s", true), None);
        let mut st = EditorState::new();
        st.document.fg = c(1);
        st.document.bg = c(2);
        st.color_key(ColorKey::Swap);
        assert_eq!((st.document.fg, st.document.bg), (c(2), c(1)));
        st.color_key(ColorKey::Reset);
        assert_eq!((st.document.fg, st.document.bg), (DEFAULT_FG, DEFAULT_BG));
    }

    #[test]
    fn hex_is_lowercase_rrggbb() {
        assert_eq!(hex(Rgb::new(255, 0, 16)), "#ff0010");
    }
}
