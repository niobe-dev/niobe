// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The starfield `classic` shows in the session pane before anything has
//! happened in it: the DOS screensaver's flight through the stars, on black
//! space inside the pane's blue frame, as a screensaver blanked the screen.
//!
//! Each star is a point in front of the viewer, `x` and `y` across the view
//! and `z` its depth, projected onto the pane by dividing by its depth —
//! `column = centre + x / z × half the width`, and the same for rows — so a
//! star born far off appears near the centre and races out to an edge as its
//! depth runs down. Its depth runs down at one pace from [`FAR`] to nothing
//! over [`FLIGHT`] milliseconds, and it is born again far off at a new place
//! across the view, so the field never thins. Each star starts its flight at
//! a moment of its own, so the field is full from the first frame.
//!
//! A star is drawn brighter and larger the nearer it is: a dark grey dot
//! far off, a grey one nearer and a white star close by, and in between, on
//! a terminal that draws 24-bit colour, a few shades of grey between the
//! space and the white.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use super::{faded, key};
use crate::theme::Theme;

/// How deep a star is when it is born, in half the view's width at depth
/// one.
const FAR: f64 = 4.0;

/// How far across the view a star may be born, in the same measure: half
/// its depth, so the stars just born show over the middle half of the pane
/// and only those born nearest its centre come close before they pass.
const SPREAD: f64 = FAR / 2.0;

/// How long a star takes to come from [`FAR`] to the viewer, in
/// milliseconds.
const FLIGHT: u64 = 7_000;

/// How many cells of the pane there are to each star in flight. Most of
/// them have left the pane by the end of their flight, so the pane shows a
/// fraction of these at a time.
const CELLS_PER_STAR: usize = 4;

/// How many shades of grey a star brightens through on a terminal that
/// draws 24-bit colour.
const SHADES: u32 = 6;

/// How much of its flight a star is too far off to be seen, in hundredths:
/// stars that far off would crowd the middle of the pane, since every star
/// is born there.
const UNSEEN: f64 = 0.1;

/// A star at one moment: across and down the view, from -1 to 1 at depth
/// one, and its depth, from [`FAR`] down to nearly nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Star {
    x: f64,
    y: f64,
    z: f64,
}

/// Star `n` of the field `seed` sows, as it stands `millis` after the
/// origin.
fn star(seed: u64, n: u64, millis: u64) -> Star {
    let own = key(seed, 0x57A2, n);
    let flown = millis.saturating_add(own % FLIGHT);
    let (lap, into) = (flown / FLIGHT, flown % FLIGHT);
    let place = key(own, 0x1A9, lap);
    // Never quite at the viewer: `into` stops one short of a whole flight.
    let left = (FLIGHT - into) as f64 / FLIGHT as f64;
    // Anywhere in the round of the view, evenly: a square's corners would
    // show as a box of stars about the centre.
    let (radius, turn) = (fraction(place).sqrt(), fraction(place.rotate_left(32)));
    let (sin, cos) = (turn * std::f64::consts::TAU).sin_cos();
    Star {
        x: SPREAD * radius * cos,
        y: SPREAD * radius * sin,
        z: FAR * left,
    }
}

/// The high bits of `bits` as a number from 0 up to 1.
fn fraction(bits: u64) -> f64 {
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

/// The cell of `area` `star` is projected onto, or `None` where it has flown
/// past the pane's edge.
fn projected(star: Star, area: Rect) -> Option<(u16, u16)> {
    let half = (f64::from(area.width) / 2.0, f64::from(area.height) / 2.0);
    let column = half.0 + star.x / star.z * half.0;
    let row = half.1 + star.y / star.z * half.1;
    let inside = |at: f64, span: u16| (0.0..f64::from(span)).contains(&at);
    if !inside(column, area.width) || !inside(row, area.height) {
        return None;
    }
    // Both are inside the pane, whose sides fit a `u16`.
    Some((area.x + column as u16, area.y + row as u16))
}

/// How near a star is, as the glyph and the named colour it is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Nearness {
    Far,
    Middling,
    Close,
}

impl Nearness {
    fn of(star: Star) -> Self {
        match star.z {
            z if z > FAR / 2.0 => Nearness::Far,
            z if z > FAR / 4.0 => Nearness::Middling,
            _ => Nearness::Close,
        }
    }

    fn glyph(self) -> char {
        match self {
            Nearness::Far => '.',
            Nearness::Middling => '+',
            Nearness::Close => '*',
        }
    }
}

/// The colours the field is drawn in, from the theme: the black its shadows
/// are, and its dark grey, grey and white for the stars.
struct Ink {
    space: Color,
    far: Color,
    middling: Color,
    close: Color,
}

impl Ink {
    fn of(theme: &Theme) -> Self {
        Self {
            space: theme.shadow,
            far: theme.shadow_fg,
            middling: theme.dim,
            close: theme.title,
        }
    }

    /// What `star` is drawn in, or `None` while it is too far off to be
    /// seen: on a terminal of 24-bit colour, the white laid over the space by
    /// how far the star has come since it could be seen, rounded to one of
    /// [`SHADES`]; otherwise the named colour of how near it is.
    fn colour(&self, star: Star) -> Option<Color> {
        let come = (1.0 - star.z / FAR - UNSEEN) / (1.0 - UNSEEN);
        if come <= 0.0 {
            return None;
        }
        let named = match Nearness::of(star) {
            Nearness::Far => self.far,
            Nearness::Middling => self.middling,
            Nearness::Close => self.close,
        };
        // A share of what is left of the flight, so from 1 to `SHADES`.
        let shade = (come * f64::from(SHADES)).ceil() as u32;
        let percent = (shade * 100 / SHADES).clamp(100 / SHADES, 100);
        Some(faded(self.close, self.space, percent, named)).filter(|ink| *ink != self.space)
    }
}

/// Draws the field into `area` of `buffer`, on the theme's black, as it
/// stands `millis` after the origin, from `seed`.
pub(super) fn draw(buffer: &mut Buffer, area: Rect, millis: u64, seed: u64, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let ink = Ink::of(theme);
    let space = Style::new().bg(ink.space);
    buffer.set_style(area, space);
    let count = usize::from(area.width) * usize::from(area.height) / CELLS_PER_STAR;
    let mut shown: Vec<(Star, (u16, u16))> = (0..count as u64)
        .map(|n| star(seed, n, millis))
        .filter_map(|star| Some((star, projected(star, area)?)))
        .collect();
    // The farthest first, so a nearer star in the same cell is drawn over it.
    shown.sort_by(|(one, _), (other, _)| other.z.total_cmp(&one.z));
    for (star, at) in shown {
        let Some(colour) = ink.colour(star) else {
            continue;
        };
        buffer[at]
            .set_char(Nearness::of(star).glyph())
            .set_style(space.fg(colour));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::theme::{CLASSIC, Depth};

    const AREA: Rect = Rect::new(0, 0, 80, 24);

    fn drawn(theme: &Theme, millis: u64) -> Buffer {
        let mut buffer = Buffer::empty(AREA);
        draw(&mut buffer, AREA, millis, 7, theme);
        buffer
    }

    fn from_the_centre(at: (u16, u16)) -> f64 {
        let x = (f64::from(at.0) + 0.5 - 40.0) / 40.0;
        let y = (f64::from(at.1) + 0.5 - 12.0) / 12.0;
        x.hypot(y)
    }

    /// Over one flight a star only ever comes nearer and moves out from the
    /// centre, until it leaves the pane, and grows brighter as it comes.
    #[test]
    fn a_star_flies_out_from_the_centre_and_brightens_as_it_comes() {
        for n in 0..50 {
            let born = star(7, n, 0);
            let first = FLIGHT - (key(7, 0x57A2, n) % FLIGHT);
            let mut last: Option<(f64, f64, Nearness)> = None;
            for millis in (first..first + FLIGHT).step_by(40) {
                let now = star(7, n, millis);
                assert_ne!((now.x, now.y), (born.x, born.y), "born again elsewhere");
                let Some(at) = projected(now, AREA) else {
                    // Gone past the edge, it does not come back this flight.
                    assert!(
                        (millis..first + FLIGHT).step_by(40).all(|later| projected(
                            star(7, n, later),
                            AREA
                        )
                        .is_none()),
                        "star {n} came back at {millis}"
                    );
                    break;
                };
                let here = (now.z, from_the_centre(at), Nearness::of(now));
                if let Some((z, out, nearness)) = last {
                    assert!(here.0 < z, "star {n} went back at {millis}");
                    assert!(here.1 >= out - 0.15, "star {n} came in at {millis}");
                    assert!(here.2 >= nearness, "star {n} dimmed at {millis}");
                }
                last = Some(here);
            }
            assert!(last.is_some(), "star {n} was never on the pane");
        }
    }

    /// The field is drawn on black, in the dark grey, grey and white that
    /// say how near each star is, and each of them is to be seen.
    #[test]
    fn classic_flies_through_black_space_past_dark_grey_grey_and_white_stars() {
        let buffer = drawn(&CLASSIC, 30_000);
        let mut seen = std::collections::BTreeSet::new();
        for cell in buffer.content() {
            assert_eq!(cell.bg, Color::Black, "{cell:?}");
            match cell.symbol() {
                " " => {}
                "." => assert_eq!(cell.fg, Color::DarkGray, "{cell:?}"),
                "+" => assert_eq!(cell.fg, Color::Gray, "{cell:?}"),
                "*" => assert_eq!(cell.fg, Color::White, "{cell:?}"),
                other => panic!("{other:?} is not a star"),
            }
            seen.insert(cell.symbol().to_owned());
        }
        assert_eq!(seen.len(), 4, "{seen:?}");
    }

    /// On a terminal of 24-bit colour a star brightens through greys from
    /// the space to white, a few shades, never one the space itself.
    #[test]
    fn a_deep_terminal_brightens_each_star_through_a_few_greys() {
        let deep = CLASSIC.at(Depth::TrueColour);
        let mut greys = std::collections::BTreeSet::new();
        for millis in [0, 2_500, 30_000] {
            for cell in drawn(&deep, millis).content() {
                assert_eq!(cell.bg, Color::Rgb(0, 0, 0), "{cell:?}");
                if cell.symbol() != " " {
                    let Color::Rgb(r, g, b) = cell.fg else {
                        panic!("{cell:?} is a named colour on a deep terminal");
                    };
                    assert!(r == g && g == b && r > 0, "{cell:?}");
                    greys.insert(r);
                }
            }
        }
        assert!(greys.contains(&255), "{greys:?}");
        assert!((3..=SHADES as usize).contains(&greys.len()), "{greys:?}");
    }

    /// The stars are all round the centre, as they are flown through, not
    /// bunched to one side of it.
    #[test]
    fn the_stars_are_spread_all_round_the_centre() {
        let buffer = drawn(&CLASSIC, 12_345);
        let mut quarters = [0usize; 4];
        for y in 0..24u16 {
            for x in 0..80u16 {
                if buffer[(x, y)].symbol() != " " {
                    quarters[usize::from(x >= 40) + 2 * usize::from(y >= 12)] += 1;
                }
            }
        }
        let total: usize = quarters.iter().sum();
        assert!(total > 40, "{quarters:?}");
        for quarter in quarters {
            assert!(quarter * 8 > total, "{quarters:?}");
        }
    }

    /// Where two stars fall in one cell, the nearer is the one drawn.
    #[test]
    fn the_nearer_of_two_stars_in_one_cell_is_the_one_drawn() {
        let area = Rect::new(0, 0, 6, 4);
        for millis in (0..20_000).step_by(250) {
            let mut buffer = Buffer::empty(area);
            draw(&mut buffer, area, millis, 11, &CLASSIC);
            let count = 6 * 4 / CELLS_PER_STAR as u64;
            for y in 0..4u16 {
                for x in 0..6u16 {
                    let ink = Ink::of(&CLASSIC);
                    let nearest = (0..count)
                        .map(|n| star(11, n, millis))
                        .filter(|star| ink.colour(*star).is_some())
                        .filter(|star| projected(*star, area) == Some((x, y)))
                        .max_by(|one, other| other.z.total_cmp(&one.z));
                    let glyph = nearest.map_or(' ', |star| Nearness::of(star).glyph());
                    assert_eq!(buffer[(x, y)].symbol(), glyph.to_string(), "({x}, {y})");
                }
            }
        }
    }
}
