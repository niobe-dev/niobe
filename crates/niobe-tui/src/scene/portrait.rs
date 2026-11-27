// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The portrait `classic` shows in the session pane before anything has
//! happened in it: Leonardo's Lisa Gherardini in ASCII, in colour, hung in a
//! gilt frame on the pane's own blue.
//!
//! The painting is held as a grid of patches ([`painting`]), each with its
//! glyph and inks, and drawn at whatever size the pane gives it: each cell
//! of the painting takes the patch that covers most of the stretch of the
//! grid under it. A cell is about twice as tall as it is wide, so the
//! painting is drawn twice as many cells across, for its proportions, as
//! the grid is patches. It hangs as large as the pane allows, a cell of the
//! pane clear round its frame, centred; where something is laid over the
//! pane's corner — the greeting — it hangs beside that or under it,
//! whichever leaves it larger.
//!
//! A glint of light crosses it from the top-left corner to the bottom-right
//! and comes round again, as on a varnished panel behind glass: on a
//! terminal that draws 24-bit colour it lays white over the inks it passes,
//! most at its middle; in the sixteen names, its middle draws them in their
//! bright counterparts. The glyphs stay where they are.

mod painting;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::{blend, key, lighter, rgb};
use crate::theme::Theme;

/// How long a glint takes to come round the portrait, in milliseconds.
const GLINT_ROUND: u64 = 12_000;

/// How far either side of its middle the glint reaches, as a share of the
/// way across the portrait from corner to corner.
const GLINT_REACH: f64 = 0.12;

/// How much white the glint lays over an ink at its middle, in hundredths.
const GLINT_PEAK: f64 = 40.0;

/// How many times as tall as it is wide a cell is drawn, near enough.
const CELL_TALL: usize = 2;

/// The fewest rows the painting is hung at: fewer show nothing of her.
const FEWEST_ROWS: usize = 3;

/// The frame's gilt on a terminal that draws 24-bit colour.
const GILT: (u8, u8, u8) = (201, 160, 52);

/// The frame's gilt in the sixteen names: CGA's brown.
const GILT_NAMED: Color = Color::Yellow;

/// Draws the portrait into `area` of `buffer`, as it stands `millis` after
/// the origin, from `seed`, kept out of `over`.
pub(super) fn draw(
    buffer: &mut Buffer,
    area: Rect,
    millis: u64,
    seed: u64,
    theme: &Theme,
    over: Rect,
) {
    let Some(frame) = hang(area, over) else {
        return;
    };
    let glint = Glint::at(frame, millis, seed);
    let deep = rgb(theme.pane_bg).is_some();
    let painted = Rect {
        x: frame.x + 1,
        y: frame.y + 1,
        width: frame.width - 2,
        height: frame.height - 2,
    };
    for y in frame.top()..frame.bottom() {
        for x in frame.left()..frame.right() {
            let (glyph, ink) = match edge(frame, x, y) {
                Some(glyph) => (glyph, ink(deep, GILT, GILT_NAMED)),
                None => match patch_under(painted, x - painted.x, y - painted.y) {
                    Some(patch) => (patch.glyph, ink(deep, patch.ink, patch.named)),
                    None => continue,
                },
            };
            if glyph == ' ' {
                continue;
            }
            let ink = glint.over(ink, x, y, theme.pane_bg);
            // Only a theme a user wrote over this one could draw its pane in
            // one of the painting's inks.
            let ink = if ink == theme.pane_bg { theme.fg } else { ink };
            buffer[(x, y)].set_char(glyph).set_fg(ink);
        }
    }
}

/// `value` on a terminal that draws 24-bit colour, which is `deep`, and
/// `named` on one that draws the sixteen names.
fn ink(deep: bool, value: (u8, u8, u8), named: Color) -> Color {
    if deep {
        Color::Rgb(value.0, value.1, value.2)
    } else {
        named
    }
}

/// Where the framed portrait hangs in `area` with `over` left clear: in the
/// whole of it where nothing is laid over it, and otherwise beside or under
/// what is, whichever leaves it larger. `None` where it fits nowhere.
fn hang(area: Rect, over: Rect) -> Option<Rect> {
    let over = over.intersection(area);
    if over.is_empty() {
        return framed(area);
    }
    let beside = Rect {
        x: over.right(),
        y: area.y,
        width: area.right() - over.right(),
        height: area.height,
    };
    let under = Rect {
        x: area.x,
        y: over.bottom(),
        width: area.width,
        height: area.bottom() - over.bottom(),
    };
    match (framed(beside), framed(under)) {
        (Some(one), Some(other)) if other.height > one.height => Some(other),
        (Some(one), _) => Some(one),
        (None, other) => other,
    }
}

/// The framed portrait as large as `room` holds it with a cell of the room
/// clear all round, centred in it; `None` where it would have fewer than
/// [`FEWEST_ROWS`].
fn framed(room: Rect) -> Option<Rect> {
    let most_across = usize::from(room.width).saturating_sub(4);
    let most_down = usize::from(room.height).saturating_sub(4);
    let per_row = painting::WIDTH * CELL_TALL;
    let down = most_down.min(most_across * painting::HEIGHT / per_row);
    if down < FEWEST_ROWS {
        return None;
    }
    let across = ((down * per_row + painting::HEIGHT / 2) / painting::HEIGHT).min(most_across);
    // Both are no more than the room's sides, which fit a `u16`.
    let width = u16::try_from(across + 2).ok()?;
    let height = u16::try_from(down + 2).ok()?;
    Some(Rect {
        x: room.x + (room.width - width) / 2,
        y: room.y + (room.height - height) / 2,
        width,
        height,
    })
}

/// The glyph of `frame`'s own edge at `(x, y)`, or `None` inside it.
fn edge(frame: Rect, x: u16, y: u16) -> Option<char> {
    let side = x == frame.left() || x == frame.right() - 1;
    let end = y == frame.top() || y == frame.bottom() - 1;
    match (side, end) {
        (true, true) => Some('+'),
        (false, true) => Some('-'),
        (true, false) => Some('|'),
        (false, false) => None,
    }
}

/// The patch most of the stretch of the painting under cell `(x, y)` of
/// `painted` is: the first of them where two cover as much.
fn patch_under(painted: Rect, x: u16, y: u16) -> Option<&'static painting::Patch> {
    let across = stretch(x, painted.width, painting::WIDTH);
    let down = stretch(y, painted.height, painting::HEIGHT);
    let mut counts = [0u16; painting::PATCHES.len()];
    for row in painting::ROWS.get(down)? {
        for letter in row.as_bytes().get(across.clone())? {
            if let Some(count) = painting::index(*letter).and_then(|at| counts.get_mut(at)) {
                *count = count.saturating_add(1);
            }
        }
    }
    let (most, _) = counts
        .iter()
        .enumerate()
        .fold((None, 0), |(most, top), (at, count)| {
            if *count > top {
                (Some(at), *count)
            } else {
                (most, top)
            }
        });
    painting::PATCHES.get(most?)
}

/// The patches of a side `patches` long under cell `at` of `cells`: at
/// least one, where the painting is drawn larger than it is held.
fn stretch(at: u16, cells: u16, patches: usize) -> std::ops::Range<usize> {
    let (at, cells) = (usize::from(at), usize::from(cells).max(1));
    let start = at * patches / cells;
    start..((at + 1) * patches / cells).max(start + 1)
}

/// The glint as it stands at one moment: where its middle is on the way
/// from the frame's top-left corner to its bottom-right.
struct Glint {
    frame: Rect,
    middle: f64,
}

impl Glint {
    fn at(frame: Rect, millis: u64, seed: u64) -> Self {
        let phase = key(seed, 0x617A, 0) % GLINT_ROUND;
        let into = (millis % GLINT_ROUND + phase) % GLINT_ROUND;
        Self {
            frame,
            middle: into as f64 / GLINT_ROUND as f64,
        }
    }

    /// How much white the glint lays over cell `(x, y)`, in hundredths:
    /// none beyond its reach, most at its middle. A row counts twice a
    /// column on the way, so it crosses at the diagonal as it is seen.
    fn strength(&self, x: u16, y: u16) -> f64 {
        let way = f64::from(self.frame.width) + 2.0 * f64::from(self.frame.height);
        let along = (f64::from(x - self.frame.x) + 2.0 * f64::from(y - self.frame.y)) / way;
        // Round the way, so that as it leaves one corner it comes in at the
        // other.
        let apart = (along - self.middle).abs();
        let apart = apart.min(1.0 - apart);
        if apart >= GLINT_REACH {
            return 0.0;
        }
        let near = 1.0 - (apart / GLINT_REACH).powi(2);
        near * near * GLINT_PEAK
    }

    /// `ink` as the glint leaves it at `(x, y)`: whitened by its strength on
    /// a terminal that draws 24-bit colour, its bright counterpart where the
    /// strength is past half in the sixteen names, and never `pane`.
    fn over(&self, ink: Color, x: u16, y: u16, pane: Color) -> Color {
        let strength = self.strength(x, y);
        if strength <= 0.0 {
            return ink;
        }
        let lit = match rgb(ink) {
            Some(value) => blend((255, 255, 255), value, strength.round() as u32),
            None if strength > GLINT_PEAK / 2.0 => lighter(ink),
            None => ink,
        };
        if lit == pane { ink } else { lit }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::theme::{CLASSIC, Depth};

    /// The pane as the scene draws it: cleared to the pane's colour, then
    /// the portrait.
    fn drawn(theme: &Theme, area: Rect, millis: u64, over: Rect) -> Buffer {
        let mut buffer = Buffer::empty(area);
        let clock = std::time::Duration::from_millis(millis);
        super::super::draw(&mut buffer, area, clock, 7, theme, over);
        buffer
    }

    /// The cells the frame and the painting inside it take: the smallest
    /// rectangle holding every glyph drawn.
    fn hung(buffer: &Buffer) -> Option<Rect> {
        let area = buffer.area;
        let drawn: Vec<(u16, u16)> = (area.top()..area.bottom())
            .flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)))
            .filter(|at| buffer[*at].symbol() != " ")
            .collect();
        let left = drawn.iter().map(|at| at.0).min()?;
        let right = drawn.iter().map(|at| at.0).max()?;
        let top = drawn.iter().map(|at| at.1).min()?;
        let bottom = drawn.iter().map(|at| at.1).max()?;
        Some(Rect::new(left, top, right - left + 1, bottom - top + 1))
    }

    fn lightness(colour: Color) -> u32 {
        let Color::Rgb(r, g, b) = colour else {
            panic!("{colour:?} is a named colour on a deep terminal");
        };
        299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b)
    }

    /// The pane stays the theme's blue under the portrait and round it:
    /// nothing is drawn on a background of its own.
    #[test]
    fn the_portrait_hangs_on_the_panes_own_blue() {
        for theme in [CLASSIC, CLASSIC.at(Depth::TrueColour)] {
            let area = Rect::new(0, 0, 120, 40);
            let buffer = drawn(&theme, area, 4_000, Rect::default());
            assert!(hung(&buffer).is_some(), "nothing drawn");
            for cell in buffer.content() {
                assert_eq!(cell.bg, theme.pane_bg, "{cell:?}");
            }
        }
    }

    /// Framed in `+`, `-` and `|`, the painting inside keeps the panel's two
    /// to three, a cell being about twice as tall as it is wide, and the
    /// frame is centred with a cell of the pane round it at least.
    #[test]
    fn the_portrait_keeps_the_panels_proportions_inside_a_frame() {
        for (width, height) in [(80, 24), (150, 54), (44, 19)] {
            let area = Rect::new(0, 0, width, height);
            let buffer = drawn(&CLASSIC, area, 0, Rect::default());
            let frame = hung(&buffer).expect("a portrait is hung");
            for (x, y) in [
                (frame.left(), frame.top()),
                (frame.right() - 1, frame.top()),
                (frame.left(), frame.bottom() - 1),
                (frame.right() - 1, frame.bottom() - 1),
            ] {
                assert_eq!(buffer[(x, y)].symbol(), "+", "({x}, {y}) of {frame:?}");
            }
            for x in frame.left() + 1..frame.right() - 1 {
                assert_eq!(buffer[(x, frame.top())].symbol(), "-");
                assert_eq!(buffer[(x, frame.bottom() - 1)].symbol(), "-");
            }
            for y in frame.top() + 1..frame.bottom() - 1 {
                assert_eq!(buffer[(frame.left(), y)].symbol(), "|");
                assert_eq!(buffer[(frame.right() - 1, y)].symbol(), "|");
            }
            let (across, down) = (frame.width - 2, frame.height - 2);
            let expected = u32::from(down) * 4 / 3;
            assert!(
                u32::from(across).abs_diff(expected) <= 1,
                "{across}x{down} at {width}x{height}"
            );
            assert!(frame.left() >= 1 && frame.top() >= 1, "{frame:?}");
            assert!(
                frame.right() < width && frame.bottom() < height,
                "{frame:?}"
            );
            let (left, right) = (frame.left(), width - frame.right());
            assert!(left.abs_diff(right) <= 1, "{frame:?} in {width}");
            let (above, below) = (frame.top(), height - frame.bottom());
            assert!(above.abs_diff(below) <= 1, "{frame:?} in {height}");
        }
    }

    /// The painting is drawn in many colours, never the pane's blue: a
    /// dozen and more values where the terminal draws them, and several of
    /// the sixteen names where it does not.
    #[test]
    fn the_portrait_is_painted_in_many_colours() {
        for (theme, least) in [(CLASSIC.at(Depth::TrueColour), 16), (CLASSIC, 4)] {
            let buffer = drawn(&theme, Rect::new(0, 0, 150, 54), 0, Rect::default());
            let inks: std::collections::BTreeSet<String> = buffer
                .content()
                .iter()
                .filter(|cell| cell.symbol() != " ")
                .inspect(|cell| assert_ne!(cell.fg, theme.pane_bg, "{cell:?}"))
                .map(|cell| format!("{:?}", cell.fg))
                .collect();
            assert!(inks.len() >= least, "{}: {inks:?}", theme.name);
        }
    }

    /// It is her: the face high in the middle of the panel is lit, drawn in
    /// the densest glyph in a light ink, and the dress low in it is in a
    /// darker one.
    #[test]
    fn her_face_is_lit_and_her_dress_is_in_shadow() {
        let theme = CLASSIC.at(Depth::TrueColour);
        let buffer = drawn(&theme, Rect::new(0, 0, 150, 54), 0, Rect::default());
        let frame = hung(&buffer).expect("a portrait is hung");
        let inner = frame.inner(ratatui::layout::Margin::new(1, 1));
        let at = |across: f64, down: f64| {
            let x = inner.x + (f64::from(inner.width) * across) as u16;
            let y = inner.y + (f64::from(inner.height) * down) as u16;
            &buffer[(x, y)]
        };
        let (face, dress) = (at(0.45, 0.2), at(0.5, 0.7));
        assert_eq!(face.symbol(), "@", "{face:?}");
        assert!(
            lightness(face.fg) > lightness(dress.fg) + 60_000,
            "{face:?} {dress:?}"
        );
    }

    /// The greeting is laid over the pane's top-left corner, so the portrait
    /// hangs where nothing will cover it: beside the greeting where the pane
    /// is wide, under it where it is narrow.
    #[test]
    fn the_portrait_hangs_clear_of_what_is_laid_over_it() {
        let over = Rect::new(0, 0, 46, 7);
        for (width, height) in [(80, 24), (150, 54), (46, 30), (120, 12)] {
            let area = Rect::new(0, 0, width, height);
            let buffer = drawn(&CLASSIC, area, 0, over);
            let frame = hung(&buffer).expect("a portrait is hung");
            assert!(
                frame.intersection(over).is_empty(),
                "{frame:?} at {width}x{height}"
            );
            let alone = hung(&drawn(&CLASSIC, area, 0, Rect::default())).expect("hung");
            assert!(frame.height * 2 >= alone.height, "{frame:?} for {alone:?}");
        }
    }

    /// Nothing is covered where nothing is laid over the pane: the portrait
    /// is centred in the whole of it.
    #[test]
    fn with_nothing_laid_over_it_the_portrait_is_as_large_as_the_pane_allows() {
        let area = Rect::new(0, 0, 150, 54);
        let frame = hung(&drawn(&CLASSIC, area, 0, Rect::default())).expect("hung");
        assert_eq!(frame.height, 52, "{frame:?}");
    }

    /// A glint of light crosses the portrait and the frame, and comes round
    /// again: it changes the inks and never a glyph, and one round of it
    /// later the portrait is as it was.
    #[test]
    fn a_glint_crosses_the_portrait_and_comes_round_again() {
        for theme in [CLASSIC, CLASSIC.at(Depth::TrueColour)] {
            let area = Rect::new(0, 0, 80, 24);
            let first = drawn(&theme, area, 1_000, Rect::default());
            let later = drawn(&theme, area, 1_000 + GLINT_ROUND / 3, Rect::default());
            let round = drawn(&theme, area, 1_000 + GLINT_ROUND, Rect::default());
            assert_eq!(first, round, "{}", theme.name);
            assert_ne!(first, later, "{}", theme.name);
            for (one, other) in first.content().iter().zip(later.content()) {
                assert_eq!(one.symbol(), other.symbol());
            }
        }
    }

    /// A pane too small to hang a frame and a painting in, or a pane all of
    /// which is covered, is left blank.
    #[test]
    fn a_pane_with_no_room_for_the_portrait_is_left_blank() {
        for (area, over) in [
            (Rect::new(0, 0, 6, 5), Rect::default()),
            (Rect::new(0, 0, 40, 3), Rect::default()),
            (Rect::new(0, 0, 40, 20), Rect::new(0, 0, 40, 20)),
        ] {
            assert_eq!(hung(&drawn(&CLASSIC, area, 0, over)), None, "{area:?}");
        }
    }

    /// Every letter of the painting names a patch, and every row is as wide
    /// as the painting.
    #[test]
    fn every_patch_of_the_painting_is_one_the_table_holds() {
        for (y, row) in painting::ROWS.iter().enumerate() {
            assert_eq!(row.len(), painting::WIDTH, "row {y}");
            for letter in row.bytes() {
                assert!(painting::index(letter).is_some(), "{letter} in row {y}");
            }
        }
        for patch in painting::PATCHES {
            assert_ne!(patch.named, Color::Blue, "{patch:?}");
            assert!(
                patch.glyph.is_ascii_graphic() || patch.glyph == ' ',
                "{patch:?}"
            );
        }
    }
}
