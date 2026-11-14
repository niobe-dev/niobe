// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The scene the session pane shows before anything has happened in it: an
//! animation of each theme's own, which [`Theme::scene`] names.
//!
//! Every scene is worked out whole from the seed, the moment it is drawn at
//! and where each cell is, and nothing is kept between frames: a resize needs
//! nothing redone, and two draws at one moment draw one frame. Every scene is
//! drawn in its theme's colours — values where the theme has them, the
//! sixteen names where it does not, with no shade between two of the sixteen
//! that a terminal could not draw.

mod plexus;
mod rain;
mod skyline;
mod starfield;

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::theme::{Scene, Theme};

/// Draws `theme`'s scene into `area` of `buffer` as it stands at `clock`, a
/// moment counted from any fixed origin — the shell counts from the epoch —
/// from `seed`. Every cell of the area is drawn, a blank on the pane's colour
/// where the scene puts nothing.
pub(crate) fn draw(buffer: &mut Buffer, area: Rect, clock: Duration, seed: u64, theme: &Theme) {
    clear(buffer, area, theme);
    let millis = u64::try_from(clock.as_millis()).unwrap_or(u64::MAX);
    match theme.scene {
        Scene::Rain => rain::draw(buffer, area, millis, seed, theme),
        Scene::Skyline => skyline::draw(buffer, area, millis, seed, theme),
        Scene::Starfield => starfield::draw(buffer, area, millis, seed, theme),
        Scene::Plexus => plexus::draw(buffer, area, millis, seed, theme),
    }
}

/// Every cell of `area` reset whole rather than styled over: a style laid on
/// a cell adds its modifiers to the ones already there, and a dim or bold
/// left by what was drawn before would show through.
fn clear(buffer: &mut Buffer, area: Rect, theme: &Theme) {
    let blank = Style::new().fg(theme.fg).bg(theme.pane_bg);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buffer[(x, y)];
            cell.reset();
            cell.set_style(blank);
        }
    }
}

/// The red, green and blue of a colour given as such; `None` for one of the
/// sixteen names, whose values are the terminal's to choose.
fn rgb(colour: Color) -> Option<(u8, u8, u8)> {
    match colour {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

/// `ink` laid over `paper` at `percent` opacity.
fn blend(ink: (u8, u8, u8), paper: (u8, u8, u8), percent: u32) -> Color {
    let percent = percent.min(100);
    let mix = |ink: u8, paper: u8| {
        let mixed = (u32::from(ink) * percent + u32::from(paper) * (100 - percent) + 50) / 100;
        u8::try_from(mixed).unwrap_or(u8::MAX)
    };
    Color::Rgb(
        mix(ink.0, paper.0),
        mix(ink.1, paper.1),
        mix(ink.2, paper.2),
    )
}

/// `ink` laid over `paper` at `percent` where both are values; `fallback`,
/// one of the sixteen, where either is a name, since a terminal drawing the
/// sixteen has no shade between two of them.
fn faded(ink: Color, paper: Color, percent: u32, fallback: Color) -> Color {
    match (rgb(ink), rgb(paper)) {
        (Some(ink), Some(paper)) => blend(ink, paper, percent),
        _ => fallback,
    }
}

/// The darker of a bright one of the sixteen, which is where a shade fades
/// to when the terminal draws only those: the sixteen have no shades between.
fn darker(colour: Color) -> Color {
    match colour {
        Color::LightGreen => Color::Green,
        Color::LightBlue => Color::Blue,
        Color::LightYellow => Color::Yellow,
        Color::LightCyan => Color::Cyan,
        Color::LightRed => Color::Red,
        Color::LightMagenta => Color::Magenta,
        Color::White => Color::Gray,
        Color::Gray => Color::DarkGray,
        other => other,
    }
}

/// SplitMix64's finaliser: a well-spread number from any other.
fn mix(n: u64) -> u64 {
    let n = (n ^ (n >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let n = (n ^ (n >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    n ^ (n >> 31)
}

/// A number from `seed` for the `n`th thing of a kind `salt` names, so two
/// kinds of thing drawn from one seed do not move in step.
fn key(seed: u64, salt: u64, n: u64) -> u64 {
    mix(seed ^ mix(salt) ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::style::Modifier;

    use crate::theme::{Depth, THEMES};

    fn drawn(theme: &Theme, area: Rect, millis: u64) -> Buffer {
        let mut buffer = Buffer::empty(area);
        draw(&mut buffer, area, Duration::from_millis(millis), 7, theme);
        buffer
    }

    fn every_palette() -> Vec<Theme> {
        THEMES
            .iter()
            .flat_map(|theme| [theme.at(Depth::Sixteen), theme.at(Depth::TrueColour)])
            .collect()
    }

    #[test]
    fn every_built_in_theme_has_a_scene_of_its_own() {
        let scenes: Vec<Scene> = THEMES.iter().map(|theme| theme.scene).collect();
        for scene in [Scene::Rain, Scene::Skyline, Scene::Starfield, Scene::Plexus] {
            assert_eq!(
                scenes.iter().filter(|drawn| **drawn == scene).count(),
                1,
                "{scene:?} in {scenes:?}"
            );
        }
    }

    #[test]
    fn a_theme_at_either_depth_or_with_a_colour_changed_keeps_its_scene() {
        for theme in THEMES {
            assert_eq!(theme.at(Depth::TrueColour).scene, theme.scene);
            let recoloured = theme
                .with_colour("hot", Color::Rgb(1, 2, 3))
                .expect("hot is a colour a theme file may set");
            assert_eq!(recoloured.scene, theme.scene, "{}", theme.name);
        }
    }

    #[test]
    fn every_scene_is_already_moving_when_it_begins_and_one_moment_draws_one_frame() {
        let area = Rect::new(0, 0, 80, 24);
        for theme in every_palette() {
            let first = drawn(&theme, area, 0);
            let lit = first
                .content()
                .iter()
                .filter(|cell| cell.symbol() != " " || cell.bg != theme.pane_bg)
                .count();
            assert!(lit > 80 * 24 / 20, "{}: {lit} cells drawn", theme.name);
            assert_eq!(first, drawn(&theme, area, 0), "{}", theme.name);
            assert_ne!(first, drawn(&theme, area, 2_000), "{}", theme.name);
        }
    }

    #[test]
    fn a_scene_writes_inside_its_area_and_nowhere_else() {
        let whole = Rect::new(0, 0, 90, 30);
        let area = Rect::new(4, 3, 80, 24);
        for theme in every_palette() {
            for millis in [0, 1_700, 63_000, 3_600_000] {
                let mut buffer = Buffer::empty(whole);
                for cell in buffer.content.iter_mut() {
                    cell.set_char('X');
                }
                draw(&mut buffer, area, Duration::from_millis(millis), 7, &theme);
                for y in whole.top()..whole.bottom() {
                    for x in whole.left()..whole.right() {
                        if !area.contains((x, y).into()) {
                            assert_eq!(buffer[(x, y)].symbol(), "X", "{} ({x}, {y})", theme.name);
                        }
                    }
                }
            }
        }
    }

    /// Drawn over a pane that held italic text, no cell keeps it, and none
    /// is drawn in a colour on that colour, which would be a glyph nobody
    /// can see — in the sixteen names above all, where two shades a design
    /// told apart can become one.
    #[test]
    fn every_glyph_a_scene_draws_is_one_cell_wide_and_can_be_seen() {
        for theme in every_palette() {
            for (width, height) in [(80, 24), (200, 60), (12, 4)] {
                let area = Rect::new(0, 0, width, height);
                for millis in [0, 1_234, 40_000, 9_000_000] {
                    let mut buffer = Buffer::empty(area);
                    buffer.set_style(area, Style::new().italic());
                    draw(&mut buffer, area, Duration::from_millis(millis), 3, &theme);
                    for cell in buffer.content() {
                        assert_eq!(crate::text::width(cell.symbol()), 1, "{:?}", cell.symbol());
                        assert!(!cell.modifier.contains(Modifier::ITALIC), "{}", theme.name);
                        if cell.symbol() != " " {
                            assert_ne!(cell.fg, cell.bg, "{}: {:?}", theme.name, cell.symbol());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_pane_too_small_to_hold_a_scene_is_drawn_blank_without_panicking() {
        for theme in every_palette() {
            for (width, height) in [(0, 0), (1, 1), (1, 40), (300, 1), (2, 2)] {
                let area = Rect::new(0, 0, width, height);
                let _ = drawn(&theme, area, 5_000);
            }
        }
    }
}
