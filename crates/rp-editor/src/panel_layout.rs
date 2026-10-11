//! The right-hand panel column: which sections it has and which of them are
//! collapsed. Editor-session state only: it is not part of the document and
//! is never saved to a file.

/// A collapsible section of the right-hand column, top to bottom.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PanelSection {
    Layers,
    Filter,
    Swatches,
    Brushes,
}

impl PanelSection {
    /// Top-to-bottom order in the column.
    pub const ALL: [PanelSection; 4] = [
        PanelSection::Layers,
        PanelSection::Filter,
        PanelSection::Swatches,
        PanelSection::Brushes,
    ];

    /// Header title.
    pub const fn title(self) -> &'static str {
        match self {
            PanelSection::Layers => "Layers",
            PanelSection::Filter => "Filter",
            PanelSection::Swatches => "Swatches",
            PanelSection::Brushes => "Brushes",
        }
    }

    const fn index(self) -> usize {
        match self {
            PanelSection::Layers => 0,
            PanelSection::Filter => 1,
            PanelSection::Swatches => 2,
            PanelSection::Brushes => 3,
        }
    }
}

/// Collapsed flags of the column's sections. Everything starts expanded;
/// the column scrolls as a whole when the window is too short.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PanelSections {
    collapsed: [bool; 4],
}

impl PanelSections {
    pub fn is_collapsed(&self, section: PanelSection) -> bool {
        self.collapsed[section.index()]
    }

    pub fn set_collapsed(&mut self, section: PanelSection, collapsed: bool) {
        self.collapsed[section.index()] = collapsed;
    }

    /// Flips one section; the others keep their state.
    pub fn toggle(&mut self, section: PanelSection) {
        let i = section.index();
        self.collapsed[i] = !self.collapsed[i];
    }

    /// Disclosure glyph for a section header: ▼ expanded, ▶ collapsed.
    pub fn glyph(&self, section: PanelSection) -> &'static str {
        if self.is_collapsed(section) {
            "▶"
        } else {
            "▼"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_sections_start_expanded() {
        let p = PanelSections::default();
        for s in PanelSection::ALL {
            assert!(!p.is_collapsed(s), "{s:?}");
            assert_eq!(p.glyph(s), "▼");
        }
    }

    #[test]
    fn toggle_flips_only_that_section() {
        let mut p = PanelSections::default();
        p.toggle(PanelSection::Filter);
        assert!(p.is_collapsed(PanelSection::Filter));
        assert!(!p.is_collapsed(PanelSection::Layers));
        assert!(!p.is_collapsed(PanelSection::Swatches));
        assert_eq!(p.glyph(PanelSection::Filter), "▶");
        p.toggle(PanelSection::Filter);
        assert_eq!(p, PanelSections::default());
    }

    #[test]
    fn sections_are_independent() {
        let mut p = PanelSections::default();
        p.toggle(PanelSection::Layers);
        p.toggle(PanelSection::Swatches);
        p.set_collapsed(PanelSection::Layers, false);
        assert!(!p.is_collapsed(PanelSection::Layers));
        assert!(!p.is_collapsed(PanelSection::Filter));
        assert!(p.is_collapsed(PanelSection::Swatches));
    }

    #[test]
    fn titles_in_column_order() {
        let titles: Vec<_> = PanelSection::ALL.iter().map(|s| s.title()).collect();
        assert_eq!(titles, ["Layers", "Filter", "Swatches", "Brushes"]);
    }
}
