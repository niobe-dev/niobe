// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The rain the session pane shows before anything has happened in it.
//!
//! Every other column is a lane down which drops fall. A drop is a bright
//! head that writes a glyph into each cell it passes; the glyphs it leaves
//! stay where they were written and fade behind it over the drop's length,
//! so what moves is the head and the fade, never the glyphs. A few of the
//! glyphs a trail holds change where they stand, and the head's changes with
//! every frame. Lanes fall at speeds of their own, and some are further off:
//! slower and dimmer, with no white head.
//!
//! Nothing is kept between frames. Each cell is worked out from the seed,
//! the moment it is drawn at and where the cell is, so a resize needs
//! nothing redone, and two draws at one moment draw one frame.
//!
//! The ink is the theme's accent, [`Theme::hot`], faded toward the pane's
//! own colour, with the head drawn toward white. The accent is green in
//! [`crate::theme::NEO`] and [`crate::theme::CYBER`] alone, so those are the
//! themes the rain is green in; every other theme rains in its own accent.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::theme::Theme;

/// The half-width katakana the rain is mostly written in, `ｦ` to `ﾝ`: one
/// cell wide each, as every glyph here has to be. The two sound marks after
/// them are left out, since a terminal can join them to the glyph before.
const KATAKANA: (u32, u32) = (0xFF66, 0xFF9D);

/// The digits and marks written among the katakana.
const MARKS: [char; 22] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', ':', '.', '"', '=', '*', '+', '-', '<', '>',
    '¦', '|', 'Z',
];

/// How many shades a trail fades through, brightest first.
const SHADES: usize = 8;

/// How much of the accent each shade keeps over the pane's colour, in percent.
const FADE: [u32; SHADES] = [100, 86, 72, 60, 48, 37, 27, 18];

/// How many shades dimmer a lane further off is drawn.
const FAR_OFF: usize = 3;

/// The share of glyphs, in percent, that change where they stand rather than
/// keep the one their drop wrote.
const RESTLESS: u64 = 12;

/// How often the head's glyph changes, in milliseconds.
const HEAD_CHANGES: u64 = 70;

/// The styles the rain is drawn in, worked out once a frame from the theme.
struct Ink {
    /// The head of a near lane: the accent taken most of the way to white.
    head: Style,
    /// The cell just behind a near lane's head, still lit by it.
    neck: Style,
    /// The trail, brightest first.
    trail: [Style; SHADES],
}

impl Ink {
    fn of(theme: &Theme) -> Self {
        let paper = Style::new().bg(theme.pane_bg);
        match (rgb(theme.hot), rgb(theme.pane_bg)) {
            (Some(accent), behind) => {
                let behind = behind.unwrap_or((0, 0, 0));
                let white = (255, 255, 255);
                Self {
                    head: paper.fg(blend(white, accent, 80)).bold(),
                    neck: paper.fg(blend(white, accent, 35)).bold(),
                    trail: FADE.map(|percent| paper.fg(blend(accent, behind, percent))),
                }
            }
            (None, _) => {
                let bright = paper.fg(theme.hot);
                let dark = paper.fg(darker(theme.hot));
                let faint = dark.add_modifier(Modifier::DIM);
                Self {
                    head: paper.fg(Color::White).bold(),
                    neck: bright.bold(),
                    trail: [bright, bright, bright, dark, dark, dark, faint, faint],
                }
            }
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

/// The darker of a bright one of the sixteen, which is where a trail fades
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

/// One lane: how fast it falls and how its drops are spaced.
struct Lane {
    seed: u64,
    /// Whether it is further off: slower, dimmer and with no white head.
    far: bool,
    /// How fast its drops fall, in thousandths of a row a millisecond, which
    /// is rows a second.
    speed: u64,
    /// How many rows apart its drops start, a thousandth of a row at a time.
    spacing: u64,
    /// Where on its way the lane was when the rain began.
    phase: u64,
}

impl Lane {
    fn new(seed: u64, column: u64, height: u64) -> Self {
        let seed = mix(seed ^ column.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let far = seed % 100 < 30;
        let speed = 5 + (seed >> 8) % 12;
        let speed = match far {
            true => speed * 3 / 5,
            false => speed,
        };
        let spacing = (height + (seed >> 16) % (height + 1) + 4) * 1000;
        Self {
            seed,
            far,
            speed: speed.max(3),
            spacing,
            phase: (seed >> 32) % spacing,
        }
    }

    /// The drops on screen after `millis`: for each, the thousandth of a row
    /// its head has reached, which can be past the bottom while its trail is
    /// still on screen, how long the trail is in thousandths of a row, and
    /// which drop it is, which the glyphs it writes are drawn from.
    fn drops(&self, millis: u64, height: u64) -> impl Iterator<Item = Drop> + '_ {
        // Three spacings in, so that the drops before the first are on their
        // way too and the pane does not open empty.
        let travelled = millis
            .saturating_mul(self.speed)
            .saturating_add(self.phase)
            .saturating_add(3 * self.spacing);
        let latest = travelled / self.spacing;
        (latest.saturating_sub(3)..=latest).filter_map(move |number| {
            let key = mix(self.seed ^ number.wrapping_mul(0xD1B5_4A32_D192_ED03));
            let delay = key % (self.spacing / 2);
            let head = travelled
                .checked_sub(number * self.spacing)?
                .checked_sub(delay)?;
            let length = (height / 3 + (key >> 20) % (height + 1)).max(4) * 1000;
            (head < (height * 1000).saturating_add(length)).then_some(Drop { head, length, key })
        })
    }
}

/// One drop falling down a lane.
struct Drop {
    head: u64,
    length: u64,
    key: u64,
}

/// Draws the rain into `area` of `buffer` as it stands at `clock`, a moment
/// counted from any fixed origin — the shell counts from the epoch — from
/// `seed`, in `theme`'s accent over its pane colour. Every cell of the area is
/// drawn, a blank on the pane's colour where no drop lights it.
pub(crate) fn draw(buffer: &mut Buffer, area: Rect, clock: Duration, seed: u64, theme: &Theme) {
    // Reset whole rather than styled over: a style laid on a cell adds its
    // modifiers to the ones already there, and a dim or bold left by what
    // was drawn before would show through.
    let blank = Style::new().fg(theme.fg).bg(theme.pane_bg);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buffer[(x, y)].reset();
            buffer[(x, y)].set_style(blank);
        }
    }
    let ink = Ink::of(theme);
    let millis = u64::try_from(clock.as_millis()).unwrap_or(u64::MAX);
    let height = u64::from(area.height);
    for x in (area.left().saturating_add(1)..area.right()).step_by(2) {
        let lane = Lane::new(seed, u64::from(x - area.left()), height);
        for drop in lane.drops(millis, height) {
            draw_drop(buffer, area, x, &lane, &drop, millis, &ink);
        }
    }
}

/// One drop's head and trail in column `x`, over whatever an earlier drop of
/// the lane left there only where this one is the brighter.
fn draw_drop(
    buffer: &mut Buffer,
    area: Rect,
    x: u16,
    lane: &Lane,
    drop: &Drop,
    millis: u64,
    ink: &Ink,
) {
    let head_row = drop.head / 1000;
    let tail_row = drop.head.saturating_sub(drop.length) / 1000;
    for row in tail_row..=head_row {
        let Some(y) = u16::try_from(row)
            .ok()
            .and_then(|row| area.top().checked_add(row))
            .filter(|y| *y < area.bottom())
        else {
            continue;
        };
        let age = drop.head - row * 1000;
        if age >= drop.length {
            continue;
        }
        let (style, shade) = shade(ink, lane.far, age, drop.length);
        let cell = &mut buffer[(x, y)];
        if cell.symbol() != " " && cell_shade(cell, ink) < shade {
            continue;
        }
        let glyph = glyph(lane, drop, row, millis, age < 1000);
        cell.reset();
        cell.set_char(glyph).set_style(style);
    }
}

/// The style a glyph `age` thousandths of a row behind its head is drawn in,
/// and how dim that is: `0` for the head, rising to the trail's end.
fn shade(ink: &Ink, far: bool, age: u64, length: u64) -> (Style, usize) {
    let fade = usize::try_from(age.saturating_mul(SHADES as u64) / length.max(1)).unwrap_or(0);
    match (far, age / 1000) {
        (false, 0) => (ink.head, 0),
        (false, 1) => (ink.neck, 1),
        (true, 0) => (ink.trail[FAR_OFF - 1], FAR_OFF + 1),
        _ => {
            let at = match far {
                true => fade + FAR_OFF,
                false => fade,
            }
            .min(SHADES - 1);
            (ink.trail[at], at + 2)
        }
    }
}

/// How dim the glyph already in `cell` is drawn, as [`shade`] counts.
fn cell_shade(cell: &ratatui::buffer::Cell, ink: &Ink) -> usize {
    let drawn = cell.style();
    if drawn == ink.head {
        return 0;
    }
    if drawn == ink.neck {
        return 1;
    }
    ink.trail
        .iter()
        .position(|style| *style == drawn)
        .map_or(SHADES + 2, |at| at + 2)
}

/// The glyph a drop wrote at `row`: the one it wrote as it passed, unless the
/// cell is one of the few that keep changing, or the head is on it.
fn glyph(lane: &Lane, drop: &Drop, row: u64, millis: u64, head: bool) -> char {
    let cell = mix(drop.key ^ row.wrapping_mul(0xA24B_AED4_963E_E407));
    let changes = match head {
        true => millis / HEAD_CHANGES,
        false if cell % 100 < RESTLESS => millis / (90 + (cell >> 8) % 400),
        false => 0,
    };
    pick(mix(cell ^ changes ^ lane.seed))
}

/// The glyph `n` names: katakana mostly, as the rain is, and the marks.
fn pick(n: u64) -> char {
    let katakana = u64::from(KATAKANA.1 - KATAKANA.0 + 1);
    let all = katakana + MARKS.len() as u64;
    let at = n % all;
    match at < katakana {
        true => u32::try_from(at)
            .ok()
            .and_then(|at| char::from_u32(KATAKANA.0 + at))
            .unwrap_or('0'),
        false => MARKS[usize::try_from(at - katakana).unwrap_or(0) % MARKS.len()],
    }
}

/// SplitMix64's finaliser: a well-spread number from any other.
fn mix(n: u64) -> u64 {
    let n = (n ^ (n >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let n = (n ^ (n >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    n ^ (n >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::theme::{CLASSIC, CYBER, Depth, MODERN, NEO};

    fn rained(theme: &Theme, millis: u64) -> Buffer {
        let area = Rect::new(0, 0, 80, 24);
        let mut buffer = Buffer::empty(area);
        draw(&mut buffer, area, Duration::from_millis(millis), 7, theme);
        buffer
    }

    fn glyphs(buffer: &Buffer) -> usize {
        buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() != " ")
            .count()
    }

    #[test]
    fn the_rain_is_already_falling_when_it_begins_and_moves_after() {
        let first = rained(&NEO, 0);
        let lit = glyphs(&first);
        assert!(
            lit > 80 * 24 / 10,
            "{lit} cells lit of {}: the pane should not open empty",
            80 * 24
        );
        assert!(
            lit < 80 * 24 / 2,
            "{lit} cells lit: half the columns are gaps"
        );
        assert_ne!(first, rained(&NEO, 500), "half a second on, it has moved");
        assert_eq!(first, rained(&NEO, 0), "one moment draws one frame");
    }

    /// Drawn over a pane that held dim and bold text, every cell is in a
    /// style of the rain's own: nothing left under it shows through.
    #[test]
    fn the_rain_leaves_nothing_of_what_was_drawn_before() {
        let area = Rect::new(0, 0, 80, 24);
        let mut buffer = Buffer::empty(area);
        buffer.set_style(area, Style::new().fg(Color::White).dim().italic());
        draw(&mut buffer, area, Duration::from_millis(900), 7, &CYBER);
        let ink = Ink::of(&CYBER);
        let blank = Style::new().fg(CYBER.fg).bg(CYBER.pane_bg);
        let mut styles: Vec<Style> = vec![blank, ink.head, ink.neck];
        styles.extend(ink.trail);
        for cell in buffer.content() {
            let drawn = cell.style();
            assert!(
                styles.iter().any(|style| {
                    style.fg.unwrap_or(Color::Reset) == drawn.fg.unwrap_or(Color::Reset)
                        && style.bg == drawn.bg
                        && style.add_modifier == drawn.add_modifier
                }),
                "{drawn:?}"
            );
        }
    }

    #[test]
    fn a_column_between_lanes_is_never_written() {
        let buffer = rained(&NEO, 1_234);
        for y in 0..24 {
            for x in (0..80).step_by(2) {
                assert_eq!(buffer[(x, y)].symbol(), " ", "({x}, {y})");
            }
        }
    }

    #[test]
    fn every_glyph_the_rain_writes_is_one_cell_wide() {
        for n in 0..10_000 {
            let glyph = pick(mix(n)).to_string();
            assert_eq!(crate::text::width(&glyph), 1, "{glyph:?}");
        }
    }

    #[test]
    fn a_head_falls_down_its_lane_at_the_lanes_speed() {
        let lane = Lane::new(7, 3, 24);
        let before: Vec<Drop> = lane.drops(10_000, 24).collect();
        let after: Vec<Drop> = lane.drops(10_050, 24).collect();
        let moved: Vec<u64> = after
            .iter()
            .filter_map(|drop| {
                let was = before.iter().find(|was| was.key == drop.key)?;
                Some(drop.head - was.head)
            })
            .collect();
        assert!(!moved.is_empty(), "a drop on screen is on screen 50 ms on");
        assert!(
            moved.iter().all(|rows| *rows == lane.speed * 50),
            "{moved:?} thousandths of a row in 50 ms at {} rows a second",
            lane.speed
        );
    }

    /// Green is the accent of the two Matrix themes alone, so the rain is
    /// green there and in the theme's own colour everywhere else.
    #[test]
    fn the_rain_is_green_in_neo_and_cyber_and_the_accent_elsewhere() {
        let greenest = |theme: &Theme| {
            let ink = Ink::of(&theme.at(Depth::TrueColour));
            let Some(Color::Rgb(r, g, b)) = ink.trail[0].fg else {
                panic!("{}: a truecolour trail is drawn in a value", theme.name)
            };
            g > r && g > b
        };
        assert!(greenest(&NEO));
        assert!(greenest(&CYBER));
        assert!(!greenest(&MODERN));
        let sixteen = Ink::of(&CLASSIC);
        assert_eq!(sixteen.trail[0].fg, Some(CLASSIC.hot));
        assert_eq!(sixteen.trail[3].fg, Some(Color::Yellow));
        assert_eq!(Ink::of(&NEO).trail[0].fg, Some(Color::LightGreen));
    }

    #[test]
    fn a_trail_fades_from_its_head_to_its_end() {
        let ink = Ink::of(&NEO.at(Depth::TrueColour));
        let green = |style: Style| match style.fg {
            Some(Color::Rgb(_, g, _)) => g,
            other => panic!("{other:?}"),
        };
        let shades: Vec<u8> = (2..20)
            .map(|row| green(shade(&ink, false, row * 1000, 20_000).0))
            .collect();
        assert!(
            shades.windows(2).all(|pair| pair[0] >= pair[1]),
            "{shades:?}"
        );
        assert!(shades[0] > shades[shades.len() - 1], "{shades:?}");
        assert_eq!(shade(&ink, false, 0, 20_000).0, ink.head);
        assert_ne!(
            shade(&ink, true, 0, 20_000).0,
            ink.head,
            "a lane further off has no white head"
        );
    }
}
