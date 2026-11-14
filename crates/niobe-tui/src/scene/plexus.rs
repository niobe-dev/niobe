// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The plexus `modern` shows in the session pane before anything has
//! happened in it: points drifting about the pane, each joined to the ones
//! near it by a line that fades as they part, and now and then a pulse
//! running along a line from one to the other.
//!
//! The lines are drawn in braille, whose 256 glyphs are every pattern of a
//! cell's two columns of four dots, so a line runs at eight times the cells'
//! resolution. A dot is about as wide as it is tall in a terminal's cell, so
//! distance is measured in dots. A point is a disc in the cell its dot falls
//! in. A cell has one colour, so where two things fall in one cell the
//! brighter colours it: a pulse over a line, and a point over both.
//!
//! The lines are the theme's accent faded toward the pane, and the points
//! take the colours an editor highlights code in: most the accent, the rest
//! the theme's added-line, tool and operator colours.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use super::{darker, faded, key};
use crate::theme::Theme;

/// How many cells of the pane each point has to itself.
const CELLS_A_POINT: u32 = 75;

/// How many dots across a point drifts a second, at the slowest and the
/// fastest.
const DRIFT: (u64, u64) = (2, 7);

/// How long a pulse takes to run the length of a line, in milliseconds.
const PULSE: u64 = 1_400;

/// One line in how many carries a pulse.
const PULSING: u64 = 5;

/// The braille bit of the dot in column `dx` and row `dy` of a cell.
const BRAILLE: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

/// One point where it stands at a moment, in dots from the area's corner,
/// and which of the colours it is drawn in.
#[derive(Debug, Clone, Copy)]
struct Point {
    x: i64,
    y: i64,
    hue: usize,
}

/// What a cell holds, brightest last, so the brightest thing in a cell is
/// the one it is coloured by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Lit {
    Nothing,
    Faint,
    Line,
    Near,
    Pulse,
    Point(usize),
}

/// The braille dots of the area and what lit each cell.
struct Canvas {
    width: i64,
    height: i64,
    dots: Vec<u8>,
    lit: Vec<Lit>,
}

/// What a point is drawn as, in the cell its dot falls in.
const POINT: char = '●';

impl Canvas {
    fn new(area: Rect) -> Self {
        let cells = usize::from(area.width) * usize::from(area.height);
        Self {
            width: i64::from(area.width) * 2,
            height: i64::from(area.height) * 4,
            dots: vec![0; cells],
            lit: vec![Lit::Nothing; cells],
        }
    }

    /// The dot at `(x, y)` set and its cell lit by `lit` where nothing
    /// brighter lit it, if the dot is on the canvas.
    fn dot(&mut self, x: i64, y: i64, lit: Lit) {
        if !(0..self.width).contains(&x) || !(0..self.height).contains(&y) {
            return;
        }
        let Ok(cell) = usize::try_from(y / 4 * (self.width / 2) + x / 2) else {
            return;
        };
        let (Ok(dx), Ok(dy)) = (usize::try_from(x % 2), usize::try_from(y % 4)) else {
            return;
        };
        if let (Some(dots), Some(was)) = (self.dots.get_mut(cell), self.lit.get_mut(cell)) {
            *dots |= BRAILLE[dx][dy];
            *was = (*was).max(lit);
        }
    }

    /// The cell `point`'s dot falls in marked as holding it, if the dot is
    /// on the canvas.
    fn point(&mut self, point: Point) {
        self.dot(point.x, point.y, Lit::Point(point.hue));
    }

    /// The dots from `a` to `b`, every `every`th of them, as Bresenham steps.
    fn line(&mut self, a: Point, b: Point, every: i64, lit: Lit) {
        let (dx, dy) = ((b.x - a.x).abs(), -(b.y - a.y).abs());
        let (sx, sy) = ((b.x - a.x).signum(), (b.y - a.y).signum());
        let (mut x, mut y, mut error) = (a.x, a.y, dx + dy);
        for step in 0.. {
            if step % every == 0 {
                self.dot(x, y, lit);
            }
            if x == b.x && y == b.y {
                break;
            }
            let twice = 2 * error;
            if twice >= dy {
                error += dy;
                x += sx;
            }
            if twice <= dx {
                error += dx;
                y += sy;
            }
        }
    }
}

/// Where the `n`th point stands `millis` in, on a canvas `width` by
/// `height` dots: drifting in a straight line and turning back off each
/// edge, so it is always on the canvas.
fn point(seed: u64, n: u64, millis: u64, width: i64, height: i64) -> Point {
    let key = key(seed, 0x91E5, n);
    let speed = |bits: u64| DRIFT.0 + bits % (DRIFT.1 - DRIFT.0 + 1);
    let along = |start: u64, bits: u64, span: i64| {
        let travelled = millis.saturating_mul(speed(bits)) / 1000;
        bounce(start.wrapping_add(travelled), span)
    };
    let hue = match key % 10 {
        0..=6 => 0,
        7 => 1,
        8 => 2,
        _ => 3,
    };
    Point {
        x: along(key >> 8, key >> 4, width),
        y: along(key >> 32, key >> 6, height),
        hue,
    }
}

/// `travelled` along a span `span` long that turns back off each end.
fn bounce(travelled: u64, span: i64) -> i64 {
    let Ok(span) = u64::try_from(span.max(1) - 1) else {
        return 0;
    };
    if span == 0 {
        return 0;
    }
    let at = travelled % (2 * span);
    i64::try_from(match at < span {
        true => at,
        false => 2 * span - at,
    })
    .unwrap_or(0)
}

/// How far apart two points may be and still be joined: about as far as a
/// point is from its neighbours on average, so each is joined to a few
/// whatever the pane's size.
fn reach(width: i64, height: i64, count: i64) -> i64 {
    isqrt(width * height / count.max(1)) * 6 / 5
}

/// The whole square root of `n`, and none of a number below zero.
fn isqrt(n: i64) -> i64 {
    n.max(0).isqrt()
}

/// The styles the plexus is drawn in.
struct Ink {
    faint: Style,
    line: Style,
    near: Style,
    pulse: Style,
    points: [Style; 4],
}

impl Ink {
    fn of(theme: &Theme) -> Self {
        let paper = Style::new().bg(theme.pane_bg);
        let accent = theme.hot;
        Self {
            faint: paper.fg(faded(accent, theme.pane_bg, 40, Color::DarkGray)),
            line: paper.fg(faded(accent, theme.pane_bg, 65, darker(accent))),
            near: paper.fg(faded(accent, theme.pane_bg, 90, accent)),
            pulse: paper.fg(theme.fg).bold(),
            points: [
                paper.fg(accent).bold(),
                paper.fg(theme.add).bold(),
                paper.fg(theme.tool).bold(),
                paper.fg(theme.user).bold(),
            ],
        }
    }

    fn of_lit(&self, lit: Lit) -> Style {
        match lit {
            Lit::Nothing | Lit::Faint => self.faint,
            Lit::Line => self.line,
            Lit::Near => self.near,
            Lit::Pulse => self.pulse,
            Lit::Point(hue) => self.points[hue.min(self.points.len() - 1)],
        }
    }
}

/// Draws the plexus into `area` of `buffer`, cleared to the pane's colour,
/// as it stands `millis` after the origin, from `seed`.
pub(super) fn draw(buffer: &mut Buffer, area: Rect, millis: u64, seed: u64, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let mut canvas = Canvas::new(area);
    let count = (u32::from(area.width) * u32::from(area.height) / CELLS_A_POINT).clamp(6, 80);
    let points: Vec<Point> = (0..u64::from(count))
        .map(|n| point(seed, n, millis, canvas.width, canvas.height))
        .collect();
    let reach = reach(canvas.width, canvas.height, i64::from(count));
    join(&mut canvas, &points, reach, seed, millis);
    for point in &points {
        canvas.point(*point);
    }
    paint(buffer, area, &canvas, &Ink::of(theme));
}

/// Every two points nearer than `reach` joined by a line that is fainter
/// and sparser the further apart they are, some with a pulse on its way
/// along.
fn join(canvas: &mut Canvas, points: &[Point], reach: i64, seed: u64, millis: u64) {
    for (i, a) in points.iter().enumerate() {
        for (j, b) in points.iter().enumerate().skip(i + 1) {
            let apart = isqrt((a.x - b.x).pow(2) + (a.y - b.y).pow(2));
            if apart >= reach {
                continue;
            }
            let (every, lit) = match apart * 3 / reach.max(1) {
                0 => (1, Lit::Near),
                1 => (1, Lit::Line),
                _ => (2, Lit::Faint),
            };
            canvas.line(*a, *b, every, lit);
            let edge = key(seed, 0x9015E, ((i as u64) << 32) | j as u64);
            if edge.is_multiple_of(PULSING) {
                let along = i64::try_from((millis + edge % PULSE) % PULSE).unwrap_or(0);
                let span = i64::try_from(PULSE).unwrap_or(1);
                let x = a.x + (b.x - a.x) * along / span;
                let y = a.y + (b.y - a.y) * along / span;
                canvas.dot(x, y, Lit::Pulse);
                canvas.dot(x + 1, y, Lit::Pulse);
            }
        }
    }
}

/// The canvas's cells written into `area` of `buffer`, each in the style of
/// the brightest thing in it.
fn paint(buffer: &mut Buffer, area: Rect, canvas: &Canvas, ink: &Ink) {
    let columns = usize::from(area.width);
    for (at, (dots, lit)) in canvas.dots.iter().zip(&canvas.lit).enumerate() {
        if *dots == 0 {
            continue;
        }
        let (Ok(x), Ok(y)) = (u16::try_from(at % columns), u16::try_from(at / columns)) else {
            continue;
        };
        let glyph = match lit {
            Lit::Point(_) => POINT,
            Lit::Nothing | Lit::Faint | Lit::Line | Lit::Near | Lit::Pulse => {
                char::from_u32(0x2800 + u32::from(*dots)).unwrap_or(' ')
            }
        };
        buffer[(area.x + x, area.y + y)]
            .set_char(glyph)
            .set_style(ink.of_lit(*lit));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::theme::{Depth, MODERN};

    #[test]
    fn a_point_drifts_and_never_leaves_the_canvas() {
        for n in 0..100 {
            let start = point(7, n, 0, 160, 96);
            let moved = (1..=600).any(|second| {
                let at = point(7, n, second * 1000, 160, 96);
                assert!(
                    (0..160).contains(&at.x) && (0..96).contains(&at.y),
                    "{at:?}"
                );
                (at.x, at.y) != (start.x, start.y)
            });
            assert!(moved, "point {n} stood still");
        }
    }

    #[test]
    fn a_point_turns_back_off_an_edge() {
        assert_eq!(bounce(0, 10), 0);
        assert_eq!(bounce(9, 10), 9);
        assert_eq!(bounce(10, 10), 8);
        assert_eq!(bounce(18, 10), 0);
        assert_eq!(bounce(5, 1), 0);
        assert_eq!(bounce(5, 0), 0);
    }

    #[test]
    fn two_points_within_reach_are_joined_and_two_beyond_it_are_not() {
        let area = Rect::new(0, 0, 40, 10);
        let a = Point { x: 4, y: 4, hue: 0 };
        let near = Point { x: 20, ..a };
        let far = Point { x: 60, ..a };
        let mut canvas = Canvas::new(area);
        join(&mut canvas, &[a, near], 30, 1, 0);
        assert!(canvas.lit[42..=50].iter().all(|lit| *lit >= Lit::Line));
        let mut canvas = Canvas::new(area);
        join(&mut canvas, &[a, far], 30, 1, 0);
        assert!(canvas.dots.iter().all(|dots| *dots == 0));
    }

    #[test]
    fn a_line_is_fainter_and_sparser_the_further_apart_its_points_are() {
        let area = Rect::new(0, 0, 60, 4);
        let dots_between = |apart: i64| {
            let a = Point { x: 0, y: 0, hue: 0 };
            let mut canvas = Canvas::new(area);
            join(&mut canvas, &[a, Point { x: apart, ..a }], 90, 99, 0);
            let set: u32 = canvas.dots.iter().map(|dots| dots.count_ones()).sum();
            (set, canvas.lit.iter().copied().max())
        };
        assert_eq!(dots_between(20).1, Some(Lit::Near));
        assert_eq!(dots_between(40).1, Some(Lit::Line));
        let (faint, lit) = dots_between(80);
        assert_eq!(lit, Some(Lit::Faint));
        assert!(
            faint <= 43,
            "{faint} dots on a dotted line 81 long, a pulse among them"
        );
    }

    #[test]
    fn a_line_sets_the_braille_dot_under_each_step() {
        let mut canvas = Canvas::new(Rect::new(0, 0, 2, 1));
        let a = Point { x: 0, y: 0, hue: 0 };
        canvas.line(a, Point { x: 3, y: 3, ..a }, 1, Lit::Line);
        assert_eq!(canvas.dots, vec![0x01 | 0x10, 0x04 | 0x80]);
    }

    #[test]
    fn each_point_is_joined_to_a_few_others_whatever_the_panes_size() {
        for (width, height) in [(80_u16, 24_u16), (200, 60), (120, 30)] {
            let canvas = Canvas::new(Rect::new(0, 0, width, height));
            let count = (u32::from(width) * u32::from(height) / CELLS_A_POINT).clamp(6, 80);
            let points: Vec<Point> = (0..u64::from(count))
                .map(|n| point(5, n, 30_000, canvas.width, canvas.height))
                .collect();
            let reach = reach(canvas.width, canvas.height, i64::from(count));
            let joined = points
                .iter()
                .enumerate()
                .flat_map(|(i, a)| points[i + 1..].iter().map(move |b| (a, b)))
                .filter(|(a, b)| isqrt((a.x - b.x).pow(2) + (a.y - b.y).pow(2)) < reach)
                .count();
            let each = joined as f64 * 2.0 / f64::from(count);
            assert!((1.5..=8.0).contains(&each), "{width}x{height}: {each} each");
        }
    }

    #[test]
    fn the_lines_are_the_accent_and_the_points_the_syntax_colours() {
        let theme = MODERN.at(Depth::TrueColour);
        let ink = Ink::of(&theme);
        assert_eq!(
            ink.near.fg,
            Some(faded(theme.hot, theme.pane_bg, 90, theme.hot))
        );
        assert_eq!(ink.points[1].fg, Some(theme.add));
        let sixteen = Ink::of(&MODERN);
        assert_eq!(sixteen.near.fg, Some(Color::LightBlue));
        assert_eq!(sixteen.line.fg, Some(Color::Blue));
        assert_eq!(sixteen.faint.fg, Some(Color::DarkGray));
    }

    #[test]
    fn the_whole_square_root_is_exact_at_the_squares() {
        for n in 0..2_000 {
            assert_eq!(isqrt(n * n), n);
            assert_eq!(isqrt(n * n + 2 * n), n);
        }
        assert_eq!(isqrt(-4), 0);
    }
}
