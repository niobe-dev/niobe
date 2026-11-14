// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The starfield `classic` shows in the session pane before anything has
//! happened in it: stars rushing out of the middle of the pane toward the
//! operator, as the screen savers and demos of the DOS years flew through
//! them.
//!
//! Each star keeps a direction out of the middle and comes in from far off
//! to pass the screen, then starts far off again. Its place on screen is its
//! direction over its depth, so a far star creeps near the middle and a near
//! one races to the edge; it grows from a point to a bright star as it nears,
//! and the nearest leave a streak back toward where they were.
//!
//! The pane is `classic`'s DOS blue, and the stars are drawn in the theme's
//! own light colours on it: the furthest in its secondary grey, then its body
//! text's light cyan, the nearest in white.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use super::{faded, key, rgb};
use crate::theme::Theme;

/// How far off a star starts, in units of depth.
const DEEP: i64 = 1000;

/// The nearest a star comes before it starts again far off.
const NEAREST: i64 = 40;

/// How many units of depth a star comes nearer a second: the trip from
/// [`DEEP`] to [`NEAREST`] takes about four seconds.
const SPEED: i64 = 240;

/// How far out of the middle a star is drawn at depth `d`, as a share of its
/// direction: `LENS / d`, so the stars furthest off already spread over most
/// of the pane, and a star is off it well before it is near unless it is
/// headed close to the middle.
const LENS: i64 = 800;

/// How many cells a near star's streak runs back toward where it was, at
/// the most: a third of its depth ago, which the nearest cover in a blink.
const STREAK: i64 = 3;

/// How many cells of the pane each star has to itself.
const CELLS_A_STAR: u32 = 11;

/// What a star looks like and how it is drawn, by how near it is: furthest
/// first.
const GLYPHS: [char; 5] = ['.', '·', '+', '*', '*'];

/// How far out of the middle toward the pane's edge a star has to have
/// flown, in thousandths of the way, to be drawn as each of [`GLYPHS`]: every
/// star brightens on its way out, whichever way it is headed, as it would
/// were the pane a window it flies past.
const FLOWN: [i64; 5] = [0, 300, 520, 720, 880];

/// One star as it stands at a moment.
struct Star {
    /// Where it points out of the middle, in thousandths of the way to the
    /// pane's edge on each axis.
    across: i64,
    down: i64,
    depth: i64,
}

impl Star {
    fn at(seed: u64, n: u64, millis: u64) -> Self {
        let key = key(seed, 0x57A2, n);
        let (across, down) = direction(key);
        let span = DEEP - NEAREST;
        let travelled = i64::try_from(millis)
            .unwrap_or(i64::MAX)
            .saturating_mul(SPEED)
            / 1000;
        let start = i64::try_from((key >> 40) % span.unsigned_abs()).unwrap_or(0);
        Self {
            across,
            down,
            depth: DEEP - (travelled + start).rem_euclid(span),
        }
    }

    /// Where on screen the star stands at `depth`, from the middle of the
    /// pane, in cells, which can be off the pane.
    fn place(&self, depth: i64, half: (i64, i64)) -> (i64, i64) {
        let out = |toward: i64, half: i64| toward * half * LENS / depth.max(1) / 1000;
        (out(self.across, half.0), out(self.down, half.1))
    }

    /// Which of [`GLYPHS`] the star is drawn as.
    fn nearness(&self) -> usize {
        let out = self.across.abs().max(self.down.abs()) * LENS / self.depth.max(1);
        FLOWN.iter().rposition(|bound| out >= *bound).unwrap_or(0)
    }
}

/// A direction out of the middle, each axis from -1000 to 1000, none so
/// near the middle that its star would sit there its whole way.
fn direction(key: u64) -> (i64, i64) {
    let axis = |bits: u64| i64::try_from(bits % 2001).unwrap_or(0) - 1000;
    let (across, down) = (axis(key), axis(key >> 16));
    if across.abs().max(down.abs()) >= 160 {
        return (across, down);
    }
    let out = match across < 0 {
        true => -400,
        false => 400,
    };
    (across + out, down)
}

/// The styles the stars are drawn in, furthest first, and their streaks'.
struct Ink {
    stars: [Style; 5],
    streak: Style,
}

impl Ink {
    fn of(theme: &Theme) -> Self {
        let paper = Style::new().bg(theme.pane_bg);
        let white = match rgb(theme.pane_bg) {
            Some(_) => Color::Rgb(255, 255, 255),
            None => Color::White,
        };
        let far = faded(theme.dim, theme.pane_bg, 55, theme.dim);
        Self {
            stars: [
                paper.fg(far),
                paper.fg(theme.dim),
                paper.fg(theme.fg),
                paper.fg(white),
                paper.fg(white).bold(),
            ],
            streak: paper.fg(faded(theme.fg, theme.pane_bg, 45, theme.dim)),
        }
    }
}

/// Draws the starfield into `area` of `buffer`, cleared to the pane's
/// colour, as it stands `millis` after the origin, from `seed`.
pub(super) fn draw(buffer: &mut Buffer, area: Rect, millis: u64, seed: u64, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let ink = Ink::of(theme);
    let count = (u32::from(area.width) * u32::from(area.height) / CELLS_A_STAR).clamp(12, 1200);
    let mut stars: Vec<Star> = (0..u64::from(count))
        .map(|n| Star::at(seed, n, millis))
        .collect();
    // The furthest first, so a nearer star is drawn over one behind it.
    stars.sort_by_key(|star| std::cmp::Reverse(star.depth));
    let half = (i64::from(area.width) / 2, i64::from(area.height) / 2);
    let middle = (i64::from(area.x) + half.0, i64::from(area.y) + half.1);
    for star in &stars {
        let nearness = star.nearness();
        let (x, y) = star.place(star.depth, half);
        if !on(area, (middle.0 + x, middle.1 + y)) {
            continue;
        }
        if nearness == GLYPHS.len() - 1 {
            let from = star.place(star.depth + star.depth / 3, half);
            streak(buffer, area, middle, from, (x, y), ink.streak);
        }
        put(
            buffer,
            area,
            (middle.0 + x, middle.1 + y),
            GLYPHS[nearness],
            ink.stars[nearness],
        );
    }
}

/// The cells from `from` to just short of `to`, both from `middle`, no more
/// than [`STREAK`] of them, marked as where a star has just been: a line
/// along the way it flies, as the warp of a DOS demo draws one.
fn streak(
    buffer: &mut Buffer,
    area: Rect,
    middle: (i64, i64),
    from: (i64, i64),
    to: (i64, i64),
    style: Style,
) {
    let (across, down) = (to.0 - from.0, to.1 - from.1);
    let steps = across.abs().max(down.abs());
    let glyph = slant(across, down);
    // Counted back from the star, so the cell beside it is always one of
    // the streak's however the division rounds.
    for back in 1..=steps.min(STREAK) {
        let x = to.0 - across * back / steps;
        let y = to.1 - down * back / steps;
        put(buffer, area, (middle.0 + x, middle.1 + y), glyph, style);
    }
}

/// The line that runs most nearly `across` cells and `down` rows, a row
/// being about twice as tall as a cell is wide.
fn slant(across: i64, down: i64) -> char {
    let (wide, tall) = (across.abs(), down.abs() * 2);
    match (
        wide >= tall * 2,
        tall >= wide * 2,
        (across < 0) == (down < 0),
    ) {
        (true, _, _) => '-',
        (_, true, _) => '|',
        (false, false, true) => '\\',
        (false, false, false) => '/',
    }
}

/// `glyph` in `style` at `(x, y)`, where that is inside `area`.
fn put(buffer: &mut Buffer, area: Rect, at: (i64, i64), glyph: char, style: Style) {
    if let Some(at) = cell(area, at) {
        buffer[at].set_char(glyph).set_style(style);
    }
}

/// Whether `(x, y)` is inside `area`.
fn on(area: Rect, at: (i64, i64)) -> bool {
    cell(area, at).is_some()
}

/// The buffer position `(x, y)` is, where that is inside `area`.
fn cell(area: Rect, (x, y): (i64, i64)) -> Option<(u16, u16)> {
    let inside =
        |at: i64, from: u16, to: u16| u16::try_from(at).ok().filter(|at| (from..to).contains(at));
    Some((
        inside(x, area.left(), area.right())?,
        inside(y, area.top(), area.bottom())?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::theme::CLASSIC;

    fn distance((x, y): (i64, i64)) -> i64 {
        x * x + 4 * y * y
    }

    #[test]
    fn a_star_comes_nearer_and_flies_out_of_the_middle_as_it_does() {
        let half = (40, 12);
        for n in 0..200 {
            let before = Star::at(7, n, 10_000);
            let after = Star::at(7, n, 10_100);
            if after.depth > before.depth {
                continue;
            }
            assert_eq!(before.depth - after.depth, SPEED / 10, "star {n}");
            assert!(
                distance(after.place(after.depth, half))
                    >= distance(before.place(before.depth, half)),
                "star {n} moved toward the middle"
            );
        }
    }

    #[test]
    fn a_star_that_has_passed_starts_again_far_off() {
        let span = u64::try_from((DEEP - NEAREST) * 1000 / SPEED).expect("the span is positive");
        for n in 0..50 {
            let star = Star::at(3, n, 0);
            let again = Star::at(3, n, span);
            assert!((star.depth - again.depth).abs() <= 1, "star {n}");
            assert!(
                (NEAREST..=DEEP).contains(&star.depth),
                "{} is outside the field",
                star.depth
            );
        }
    }

    #[test]
    fn a_star_grows_brighter_as_it_nears() {
        let star = |depth| Star {
            across: 200,
            down: -150,
            depth,
        };
        let nearness: Vec<usize> = (NEAREST..=DEEP)
            .rev()
            .step_by(10)
            .map(|depth| star(depth).nearness())
            .collect();
        assert!(nearness.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(nearness.first(), Some(&0));
        assert_eq!(nearness.last(), Some(&(GLYPHS.len() - 1)));
    }

    #[test]
    fn no_star_sits_in_the_middle_its_whole_way() {
        for n in 0..10_000 {
            let (across, down) = direction(key(11, 0x57A2, n));
            assert!(across.abs().max(down.abs()) >= 160, "{across}, {down}");
            assert!(across.abs() <= 1000 && down.abs() <= 1000);
        }
    }

    /// A streak that runs nearly level ends in the cell beside its star on
    /// the star's own row, and one that runs nearly upright in the cell
    /// above or below it, however the division rounds.
    #[test]
    fn a_streak_ends_beside_its_star_however_shallow_its_slope() {
        let area = Rect::new(0, 0, 80, 24);
        let style = Style::new().fg(Color::Gray);
        for (from, to, beside) in [
            ((0, 0), (30, -1), (-1, 0)),
            ((0, 3), (-25, 2), (1, 0)),
            ((5, 0), (6, 9), (0, -1)),
        ] {
            let mut buffer = Buffer::empty(area);
            streak(&mut buffer, area, (40, 12), from, to, style);
            let x = u16::try_from(40 + to.0 + beside.0).expect("on the area");
            let y = u16::try_from(12 + to.1 + beside.1).expect("on the area");
            assert_ne!(
                buffer[(x, y)].symbol(),
                " ",
                "the streak from {from:?} to {to:?} leaves its star"
            );
        }
    }

    #[test]
    fn a_streak_runs_the_way_its_star_flies() {
        assert_eq!(slant(6, 0), '-');
        assert_eq!(slant(-6, 1), '-');
        assert_eq!(slant(0, 3), '|');
        assert_eq!(slant(1, -4), '|');
        assert_eq!(slant(4, 2), '\\');
        assert_eq!(slant(-4, -2), '\\');
        assert_eq!(slant(4, -2), '/');
        assert_eq!(slant(-4, 2), '/');
    }

    #[test]
    fn a_streak_is_no_longer_than_its_bound() {
        let area = Rect::new(0, 0, 80, 24);
        let mut buffer = Buffer::empty(area);
        let style = Style::new().fg(Color::Gray);
        streak(&mut buffer, area, (40, 12), (0, 0), (30, 0), style);
        let marked = buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() == "-")
            .count();
        assert_eq!(marked, usize::try_from(STREAK).expect("the bound is small"));
        assert_eq!(
            buffer[(69, 12)].symbol(),
            "-",
            "the streak ends by its star"
        );
        assert_eq!(
            buffer[(70, 12)].symbol(),
            " ",
            "the star's own cell is its own"
        );
    }

    #[test]
    fn the_stars_are_drawn_in_classics_light_colours_on_its_blue() {
        let area = Rect::new(0, 0, 80, 24);
        let mut buffer = Buffer::empty(area);
        super::super::draw(
            &mut buffer,
            area,
            std::time::Duration::from_millis(5_000),
            7,
            &CLASSIC,
        );
        let lit: Vec<_> = buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() != " ")
            .collect();
        assert!(lit.len() > 40, "{} stars drawn", lit.len());
        for cell in lit {
            assert_eq!(cell.bg, Color::Blue);
            assert!(
                [Color::Gray, Color::LightCyan, Color::White].contains(&cell.fg),
                "{:?}",
                cell.fg
            );
        }
    }
}
