//! Ribbon icons: 16×16 one-bit pixel-art bitmaps (a DOS-paint look) turned
//! into SVGs and served to GPUI through [`IconAssets`]. GPUI draws an SVG as
//! an alpha mask tinted with the element's text color, so every icon is
//! monochrome and follows the button's text color (selected, dimmed).
//!
//! The bitmaps are axis-aligned 1-unit squares, so they stay sharp at
//! integer display scales and only soften slightly at fractional ones.
//!
//! Also here: the tool shortcut letters and the tooltip text for every
//! ribbon icon button.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};
use rp_doc::Tool;

use crate::state::tool_label;

/// Icon grid size in units (and in logical pixels when drawn).
pub const ICON_GRID: usize = 16;

/// Asset path prefix of the ribbon icons.
const PREFIX: &str = "icons/";

/// A ribbon icon.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Icon {
    Select,
    Move,
    Brush,
    Eraser,
    Picker,
    Lasso,
    RectSelect,
    InsertImage,
    SizeDown,
    SizeUp,
    TipHard,
    TipSoft,
    SwapColors,
    ResetColors,
}

impl Icon {
    pub const ALL: [Icon; 14] = [
        Icon::Select,
        Icon::Lasso,
        Icon::RectSelect,
        Icon::Move,
        Icon::Brush,
        Icon::Eraser,
        Icon::Picker,
        Icon::InsertImage,
        Icon::SizeDown,
        Icon::SizeUp,
        Icon::TipHard,
        Icon::TipSoft,
        Icon::SwapColors,
        Icon::ResetColors,
    ];

    /// The icon for a tool button.
    pub const fn for_tool(tool: Tool) -> Icon {
        match tool {
            Tool::Select => Icon::Select,
            Tool::Lasso => Icon::Lasso,
            Tool::RectSelect => Icon::RectSelect,
            Tool::Move => Icon::Move,
            Tool::Brush => Icon::Brush,
            Tool::Eraser => Icon::Eraser,
            Tool::Picker => Icon::Picker,
        }
    }

    /// Asset path, as passed to GPUI's `svg().path(..)`.
    pub const fn path(self) -> &'static str {
        match self {
            Icon::Select => "icons/select.svg",
            Icon::Move => "icons/move.svg",
            Icon::Brush => "icons/brush.svg",
            Icon::Eraser => "icons/eraser.svg",
            Icon::Picker => "icons/picker.svg",
            Icon::Lasso => "icons/lasso.svg",
            Icon::RectSelect => "icons/rect-select.svg",
            Icon::InsertImage => "icons/insert-image.svg",
            Icon::SizeDown => "icons/size-down.svg",
            Icon::SizeUp => "icons/size-up.svg",
            Icon::TipHard => "icons/tip-hard.svg",
            Icon::TipSoft => "icons/tip-soft.svg",
            Icon::SwapColors => "icons/swap-colors.svg",
            Icon::ResetColors => "icons/reset-colors.svg",
        }
    }

    /// The icon at an asset path.
    pub fn from_path(path: &str) -> Option<Icon> {
        Icon::ALL.into_iter().find(|i| i.path() == path)
    }

    /// The icon's pixels: `#` is ink, `.` is clear; top row first.
    pub const fn bitmap(self) -> [&'static str; ICON_GRID] {
        match self {
            // Deluxe Paint-style arrow pointer.
            Icon::Select => [
                "................",
                ".#..............",
                ".##.............",
                ".###............",
                ".####...........",
                ".#####..........",
                ".######.........",
                ".#######........",
                ".########.......",
                ".#########......",
                ".######.........",
                ".##.###.........",
                ".#...###........",
                ".....###........",
                "......###.......",
                "................",
            ],
            Icon::Move => [
                "................",
                ".......##.......",
                "......####......",
                ".....######.....",
                ".......##.......",
                "..#....##....#..",
                ".##....##....##.",
                "################",
                "################",
                ".##....##....##.",
                "..#....##....#..",
                ".......##.......",
                ".....######.....",
                "......####......",
                ".......##.......",
                "................",
            ],
            Icon::Brush => [
                "................",
                "..............#.",
                ".............###",
                "............###.",
                "...........###..",
                "..........###...",
                ".........###....",
                "........###.....",
                "......#.##......",
                ".....###.#......",
                "....#####.......",
                "....####........",
                "...####.........",
                "..###...........",
                ".##.............",
                "................",
            ],
            Icon::Eraser => [
                "................",
                "................",
                "................",
                "........######..",
                ".......#######..",
                "......########..",
                ".....#.######...",
                "....#...####....",
                "...#.....##.....",
                "..#.....##......",
                "..#....##.......",
                "...#..##........",
                "....###.........",
                "................",
                "..##.##.##.##.##",
                "................",
            ],
            Icon::Picker => [
                "................",
                "...........###..",
                "..........#####.",
                "..........#####.",
                "........#######.",
                ".........#####..",
                "........#.###...",
                ".......#...#....",
                "......#...#.....",
                ".....#...#......",
                "....#...#.......",
                "...#...#........",
                "..#..##.........",
                "..###...........",
                ".#..............",
                "................",
            ],
            Icon::Lasso => [
                "................",
                ".....######.....",
                "...##......##...",
                "..#..........#..",
                ".#............#.",
                ".#............#.",
                ".#............#.",
                "..#..........#..",
                "...##......##...",
                ".....######.....",
                "......#.........",
                ".....#..........",
                ".....#..........",
                "......##........",
                "........#.......",
                "................",
            ],
            // Marching-ants rectangle.
            Icon::RectSelect => [
                "................",
                "................",
                ".##..##..##..##.",
                ".#............#.",
                "................",
                "..............#.",
                ".#............#.",
                ".#..............",
                "................",
                ".#............#.",
                "..............#.",
                "................",
                ".#............#.",
                ".##..##..##..##.",
                "................",
                "................",
            ],
            // A picture with a plus: add an image layer.
            Icon::InsertImage => [
                "................",
                "############....",
                "#..........#....",
                "#.......##.#....",
                "#.......##.#....",
                "#..#.......#....",
                "#.###...#..#....",
                "#####..###.#....",
                "#######.####....",
                "#########.......",
                "#########...##..",
                "#########...##..",
                "..........######",
                "..........######",
                "............##..",
                "............##..",
            ],
            Icon::SizeDown => [
                "................",
                "................",
                "................",
                "................",
                "................",
                "................",
                "................",
                "...##########...",
                "...##########...",
                "................",
                "................",
                "................",
                "................",
                "................",
                "................",
                "................",
            ],
            Icon::SizeUp => [
                "................",
                "................",
                "................",
                ".......##.......",
                ".......##.......",
                ".......##.......",
                ".......##.......",
                "...##########...",
                "...##########...",
                ".......##.......",
                ".......##.......",
                ".......##.......",
                ".......##.......",
                "................",
                "................",
                "................",
            ],
            Icon::TipHard => [
                "................",
                "................",
                "................",
                "......####......",
                "....########....",
                "....########....",
                "...##########...",
                "...##########...",
                "...##########...",
                "...##########...",
                "....########....",
                "....########....",
                "......####......",
                "................",
                "................",
                "................",
            ],
            // The same disc, dithered toward its edge.
            Icon::TipSoft => [
                "................",
                "......#.#.......",
                "...#.#.#.#.#....",
                "....#.###.#.#...",
                "...#.######.#...",
                "..#.########.#..",
                "...#########.#..",
                "..#.##########..",
                "...##########.#.",
                "..#.#########...",
                "...#.########.#.",
                "..#..######.#...",
                "...#.#.##.#.#...",
                ".....#.#.#.#....",
                ".......#.#......",
                "................",
            ],
            Icon::SwapColors => [
                "................",
                "................",
                "................",
                "..........#.....",
                "..........##....",
                "..#############.",
                "..#############.",
                "..........##....",
                "..........#.....",
                ".....#..........",
                "....##..........",
                ".#############..",
                ".#############..",
                "....##..........",
                ".....#..........",
                "................",
            ],
            // Small fg chip over bg chip: black on white.
            Icon::ResetColors => [
                "................",
                ".########.......",
                ".########.......",
                ".########.......",
                ".########.......",
                ".#############..",
                ".########....#..",
                ".########....#..",
                ".########....#..",
                ".....#.......#..",
                ".....#.......#..",
                ".....#.......#..",
                ".....#.......#..",
                ".....#########..",
                "................",
                "................",
            ],
        }
    }

    /// The icon as an SVG document: one path of 1-unit squares, with runs
    /// of ink in a row merged into one rectangle.
    pub fn svg(self) -> String {
        bitmap_svg(&self.bitmap())
    }
}

/// An SVG for a `#`/`.` bitmap on a [`ICON_GRID`]-unit square.
pub fn bitmap_svg(rows: &[&str; ICON_GRID]) -> String {
    let mut d = String::new();
    for (y, row) in rows.iter().enumerate() {
        let bytes = row.as_bytes();
        let mut x = 0;
        while x < bytes.len() {
            if bytes[x] != b'#' {
                x += 1;
                continue;
            }
            let start = x;
            while x < bytes.len() && bytes[x] == b'#' {
                x += 1;
            }
            let n = x - start;
            d.push_str(&format!("M{start} {y}h{n}v1h-{n}z"));
        }
    }
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{g}\" height=\"{g}\" \
         viewBox=\"0 0 {g} {g}\"><path fill=\"#000\" d=\"{d}\"/></svg>",
        g = ICON_GRID
    )
}

/// GPUI asset source serving the ribbon icons (`icons/<name>.svg`).
pub struct IconAssets;

impl AssetSource for IconAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(Icon::from_path(path).map(|i| Cow::Owned(i.svg().into_bytes())))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Icon::ALL
            .iter()
            .map(|i| i.path())
            .filter(|p| p.starts_with(path) || PREFIX.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

/// A tool's single-key shortcut, as typed (lowercase).
pub const fn tool_shortcut(tool: Tool) -> &'static str {
    match tool {
        Tool::Select => "s",
        Tool::Lasso => "l",
        Tool::RectSelect => "r",
        Tool::Move => "m",
        Tool::Brush => "b",
        Tool::Eraser => "e",
        Tool::Picker => "i",
    }
}

/// The tool for a key press with no modifiers held (`plain` excludes ctrl,
/// alt, super and shift), or `None`.
pub fn tool_key(key: &str, plain: bool) -> Option<Tool> {
    if !plain {
        return None;
    }
    Tool::ALL.into_iter().find(|&t| tool_shortcut(t) == key)
}

/// Tooltip text: the name, then the shortcut in parentheses if there is one.
pub fn tooltip_text(name: &str, shortcut: Option<&str>) -> String {
    match shortcut {
        Some(k) => format!("{name} ({k})"),
        None => name.to_string(),
    }
}

/// Tooltip of a tool button, e.g. "Brush (B)".
pub fn tool_tooltip(tool: Tool) -> String {
    tooltip_text(tool_label(tool), Some(&tool_shortcut(tool).to_uppercase()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn bitmaps_are_square_one_bit_and_not_empty() {
        for icon in Icon::ALL {
            let rows = icon.bitmap();
            for row in rows {
                assert_eq!(row.len(), ICON_GRID, "{icon:?} row {row:?}");
                assert!(row.bytes().all(|b| b == b'#' || b == b'.'), "{icon:?}");
            }
            assert!(rows.iter().any(|r| r.contains('#')), "{icon:?} is empty");
        }
    }

    #[test]
    fn icons_are_distinct() {
        let maps: HashSet<_> = Icon::ALL.iter().map(|i| i.bitmap()).collect();
        assert_eq!(maps.len(), Icon::ALL.len());
        let paths: HashSet<_> = Icon::ALL.iter().map(|i| i.path()).collect();
        assert_eq!(paths.len(), Icon::ALL.len());
    }

    #[test]
    fn svg_merges_runs_into_rectangles() {
        let mut rows = ["................"; ICON_GRID];
        rows[2] = ".###....#.......";
        let svg = bitmap_svg(&rows);
        assert!(svg.contains("d=\"M1 2h3v1h-3zM8 2h1v1h-1z\""), "{svg}");
        assert!(svg.contains("viewBox=\"0 0 16 16\""));
    }

    #[test]
    fn every_icon_parses_as_svg_inside_its_grid() {
        let opts = usvg::Options::default();
        let g = ICON_GRID as f32;
        for icon in Icon::ALL {
            let tree = usvg::Tree::from_str(&icon.svg(), &opts)
                .unwrap_or_else(|e| panic!("{icon:?}: {e}"));
            assert_eq!((tree.size().width(), tree.size().height()), (g, g));
            let b = tree.root().abs_bounding_box();
            assert!(b.width() > 0. && b.height() > 0., "{icon:?}");
            assert!(b.left() >= 0. && b.top() >= 0. && b.right() <= g && b.bottom() <= g);
        }
    }

    #[test]
    fn asset_source_serves_every_icon_path() {
        for icon in Icon::ALL {
            let bytes = IconAssets.load(icon.path()).unwrap().expect("icon loads");
            assert_eq!(&*bytes, icon.svg().as_bytes());
        }
        assert!(IconAssets.load("icons/nope.svg").unwrap().is_none());
        assert!(IconAssets.load("fonts/x.ttf").unwrap().is_none());
        assert_eq!(IconAssets.list("icons/").unwrap().len(), Icon::ALL.len());
        assert!(IconAssets.list("fonts/").unwrap().is_empty());
    }

    #[test]
    fn every_tool_has_an_icon_and_a_unique_key() {
        let icons: HashSet<_> = Tool::ALL.iter().map(|&t| Icon::for_tool(t)).collect();
        assert_eq!(icons.len(), Tool::ALL.len());
        let keys: HashSet<_> = Tool::ALL.iter().map(|&t| tool_shortcut(t)).collect();
        assert_eq!(keys.len(), Tool::ALL.len());
        for t in Tool::ALL {
            assert_eq!(tool_key(tool_shortcut(t), true), Some(t));
            assert_eq!(tool_key(tool_shortcut(t), false), None);
        }
        // Must not steal the color keys.
        assert_eq!(tool_key("x", true), None);
        assert_eq!(tool_key("d", true), None);
        assert_eq!(tool_key("l", true), Some(Tool::Lasso));
        assert_eq!(tool_key("r", true), Some(Tool::RectSelect));
    }

    #[test]
    fn tooltips_name_the_shortcut() {
        assert_eq!(tool_tooltip(Tool::Brush), "Brush (B)");
        assert_eq!(tool_tooltip(Tool::Picker), "Picker (I)");
        assert_eq!(tool_tooltip(Tool::Lasso), "Lasso (L)");
        assert_eq!(tool_tooltip(Tool::RectSelect), "Rect Select (R)");
        assert_eq!(tooltip_text("Hard brush", None), "Hard brush");
        assert_eq!(tooltip_text("Swap colors", Some("X")), "Swap colors (X)");
    }
}
