// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The night city `cyber` shows in the session pane before anything has
//! happened in it: two rows of towers sliding past at their own depths under
//! a sky that glows toward the horizon, lit windows going on and off, neon
//! signs that flicker, red lights blinking on the masts and craft crossing
//! between the towers.
//!
//! The far row is a haze of towers that drifts by slowly; the near row
//! passes faster, each tower drawn in a line of its own colour with a grid of
//! windows, some carrying a sign down their face or a word on their roof.
//! The city is laid out along a street that never ends: every stretch of it
//! is worked out from the seed and where it is, so the scene keeps nothing
//! and scrolls by moving where the pane looks.
//!
//! A near tower is a block rather than a flat front: the pane looks at the
//! street from its middle, so a tower left of that shows its right side and
//! one right of it its left, deeper the further out it stands, its top edge
//! sloping away toward the horizon. As the street slides by, each side
//! narrows, vanishes as its tower passes the middle and opens on the other
//! hand — the turn a passer-by sees, which is what makes the row read as
//! depth rather than as cut-outs.
//!
//! Both rows step often rather than far: a whole column a second reads as a
//! jolt, so the near row moves a column every fifth of a second, and the far
//! row's edges, where the terminal draws 24-bit colour, move by eighths of a
//! cell between its steps.
//!
//! On a terminal that draws 24-bit colour the sky fades from the pane's
//! black to the theme's magenta at the horizon and the towers are solid
//! silhouettes. In the sixteen names the sky stays black, the far towers are
//! a shaded magenta haze and the near ones are drawn by their outline and
//! their windows, which is what reads on black.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use super::{blend, darker, faded, key, mix, rgb};
use crate::theme::Theme;

/// A row of towers: how wide a plot each stands on, how tall they are, and
/// how fast the row slides past.
struct Row {
    salt: u64,
    /// How many columns each plot is wide.
    plot: i64,
    /// How tall a tower is, as a share of the pane in percent, at the least
    /// and the most.
    tall: (i64, i64),
    /// How long the row takes to slide one column, in milliseconds.
    millis_a_column: u64,
    /// One plot in how many is left empty, where any is: a gap the row
    /// behind shows through.
    vacant: Option<u64>,
}

/// The towers far off: narrow, a haze, sliding by slowly.
const FAR: Row = Row {
    salt: 0xFA2,
    plot: 6,
    tall: (36, 72),
    millis_a_column: 660,
    vacant: None,
};

/// The towers near: wider, outlined and lit, passing faster.
const NEAR: Row = Row {
    salt: 0x4EA2,
    plot: 12,
    tall: (24, 80),
    millis_a_column: 220,
    vacant: Some(7),
};

/// The words a roof's sign can carry.
const WORDS: [&str; 9] = [
    "BAR",
    "HOTEL",
    "24H",
    "NOODLE",
    "ﾗｰﾒﾝ",
    "ﾎﾃﾙ",
    "OPEN",
    "CAFE",
    "NIOBE",
];

/// How many columns deep a near tower's side shows at the most: at the
/// pane's edge, furthest from the middle it is seen from.
const DEEPEST_SIDE: i64 = 5;

/// The blocks that fill a cell from its left by one eighth to seven.
const LEFT_EIGHTHS: [char; 7] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉'];

/// The half-width katakana a sign down a tower's face is written in: one
/// cell each, without the two sound marks a terminal can join to the glyph
/// before them.
const KATAKANA: (u32, u32) = (0xFF66, 0xFF9D);

/// What stands on a near tower's roof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Roof {
    Flat,
    /// A mast this many rows tall, with a light blinking at its top.
    Mast(i64),
    /// A sign with one of [`WORDS`] on it.
    Sign(usize),
}

/// One tower where it stands along the street, in columns from where the
/// street starts, and in rows from the top of the pane.
#[derive(Debug, Clone, Copy)]
struct Tower {
    key: u64,
    left: i64,
    right: i64,
    top: i64,
    /// How many rows at the top are narrower, by two columns each side.
    setback: i64,
    roof: Roof,
    /// Which of the neon colours its windows and signs are mostly lit in.
    tint: usize,
    /// How many of its windows are lit at a time, in percent.
    lit: u64,
    /// How many rows its sign runs down its face, below any setback, where
    /// it has one.
    sign: Option<i64>,
}

impl Row {
    /// How far along the street the pane looks `millis` in.
    fn offset(&self, millis: u64) -> i64 {
        i64::try_from(millis / self.millis_a_column).unwrap_or(i64::MAX / 2)
    }

    /// How far along the street the pane looks `millis` in, in eighths of a
    /// column.
    fn eighths(&self, millis: u64) -> i64 {
        i64::try_from(millis.saturating_mul(8) / self.millis_a_column).unwrap_or(i64::MAX / 2)
    }

    /// The tower on plot `plot`, on a pane `height` rows tall; `None` for
    /// an empty plot.
    fn tower(&self, seed: u64, plot: i64, height: i64) -> Option<Tower> {
        let key = key(seed, self.salt, plot.cast_unsigned());
        if self.vacant.is_some_and(|one_in| key.is_multiple_of(one_in)) {
            return None;
        }
        let pick =
            |bits: u64, span: i64| i64::try_from(bits % span.max(1).unsigned_abs()).unwrap_or(0);
        let start = plot.saturating_mul(self.plot);
        let left = start + pick(key >> 3, self.plot / 4 + 1);
        let right = start + self.plot - pick(key >> 7, self.plot / 4 + 1);
        let tall = self.tall.0 + pick(key >> 11, self.tall.1 - self.tall.0 + 1);
        let rows = (height * tall / 100).max(3).min(height);
        let width = right - left;
        let roof = match (key >> 19) % 6 {
            0 | 1 => Roof::Mast(2 + pick(key >> 23, 3)),
            2 if width >= 6 => Roof::Sign(usize::try_from(key >> 23).unwrap_or(0) % WORDS.len()),
            _ => Roof::Flat,
        };
        let setback = match (key >> 27).is_multiple_of(3) && width >= 8 && rows >= 8 {
            true => (rows / 4).max(2),
            false => 0,
        };
        let below_setback = rows - setback;
        let sign = ((key >> 31).is_multiple_of(3) && width >= 6 && below_setback >= 9)
            .then(|| (3 + pick(key >> 33, 4)).min(below_setback - 4));
        Some(Tower {
            key,
            left,
            right,
            top: height - rows,
            setback,
            roof,
            tint: usize::try_from((key >> 37) % 4).unwrap_or(0),
            lit: 25 + (key >> 41) % 45,
            sign,
        })
    }

    /// The tower standing at column `at` of the street, if one does.
    fn at(&self, seed: u64, at: i64, height: i64) -> Option<Tower> {
        self.tower(seed, at.div_euclid(self.plot), height)
            .filter(|tower| (tower.left..tower.right).contains(&at))
    }
}

impl Tower {
    /// Whether the tower takes up column `x`, row `y`: below its roof, and
    /// inside its setback at the top.
    fn holds(&self, x: i64, y: i64) -> bool {
        let inset = match y < self.top + self.setback {
            true => 2,
            false => 0,
        };
        y >= self.top && x >= self.left + inset && x < self.right - inset
    }

    /// The column of the middle of its roof, where a mast stands and a sign
    /// is centred.
    fn middle(&self) -> i64 {
        (self.left + self.right) / 2
    }

    /// The column its sign runs down: just inside its right wall.
    fn sign_column(&self) -> i64 {
        self.right - 2
    }

    /// The blocks it is built of, as their first column, the column after
    /// their last and the row their top is on: its body, and the narrower
    /// block its setback stands on that, where it has one.
    fn blocks(&self) -> [Option<(i64, i64, i64)>; 2] {
        [
            Some((self.left, self.right, self.top + self.setback)),
            (self.setback > 0).then_some((self.left + 2, self.right - 2, self.top)),
        ]
    }

    /// How many columns deep its side shows from `view`: none straight ahead,
    /// [`DEEPEST_SIDE`] at the pane's edge.
    fn depth(&self, view: View) -> i64 {
        let off = (self.middle() - view.centre).abs();
        let width = view.width.max(1);
        ((off * 2 * DEEPEST_SIDE + width / 2) / width).min(DEEPEST_SIDE)
    }

    /// Where its side shows at column `x` of the street, seen from `view`:
    /// how many columns out from its front `x` is, from nought, and the
    /// half-row the side's top edge is at there, counted from the pane's top
    /// in halves of a row. The edge drops half a row a column away from the
    /// front, which on a cell twice as tall as it is wide slopes it at the
    /// angle a roof's edge recedes at.
    fn side(&self, x: i64, view: View) -> Option<(i64, i64)> {
        let depth = self.depth(view);
        let rightward = self.middle() < view.centre;
        self.blocks()
            .into_iter()
            .flatten()
            .filter_map(|(left, right, top)| {
                let out = match rightward {
                    true => x - right,
                    false => left - 1 - x,
                };
                (0..depth)
                    .contains(&out)
                    .then_some((out, 2 * top + out + 1))
            })
            .min_by_key(|(_, top)| *top)
    }
}

/// Where the pane looks at the near row from: the street column at its
/// middle, and how wide it is.
#[derive(Debug, Clone, Copy)]
struct View {
    centre: i64,
    width: i64,
}

/// The colours the city is drawn in.
struct Ink {
    deep: bool,
    pane: Color,
    /// The neon colours a window or sign is lit in.
    neon: [Color; 4],
    far: Style,
    /// The far haze's colour where the terminal draws 24-bit colour, which
    /// an edge between two eighths of a cell is drawn in.
    far_colour: Color,
    far_window: Option<Style>,
    near: Color,
    /// A near tower's side in the sixteen names: a grey stipple.
    stipple: Style,
    /// A near tower's side where the terminal draws 24-bit colour, by the
    /// neon colour the tower is lit in: dark, washed with the glow of its own
    /// signs, so that it reads as a face turned another way from the front.
    sides: [Color; 4],
    dark_window: Option<Style>,
    stars: Style,
    /// A mast and its light, over whatever sky is behind them.
    mast: Style,
    beacon: Style,
    craft: [Style; 3],
}

impl Ink {
    fn of(theme: &Theme) -> Self {
        let pane = theme.pane_bg;
        let deep = rgb(pane).is_some();
        let far = faded(theme.title, pane, 14, Color::Magenta);
        let near = faded(theme.tool, pane, 12, pane);
        let paper = Style::new().bg(pane);
        Self {
            deep,
            pane,
            neon: [theme.warn, theme.command, theme.title, theme.fg],
            far: match deep {
                true => Style::new().bg(far),
                false => paper.fg(Color::Magenta),
            },
            far_colour: far,
            far_window: deep.then(|| Style::new().bg(far).fg(faded(theme.warn, far, 45, far))),
            near,
            stipple: paper.fg(Color::DarkGray),
            sides: [theme.warn, theme.command, theme.title, theme.fg]
                .map(|neon| faded(neon, pane, 9, Color::DarkGray)),
            dark_window: deep.then(|| Style::new().bg(near).fg(faded(theme.fg, near, 12, near))),
            stars: paper.fg(faded(theme.fg, pane, 55, darker(theme.fg))),
            mast: Style::new().fg(faded(theme.fg, pane, 30, Color::DarkGray)),
            beacon: Style::new().fg(theme.del).bold(),
            craft: [
                Style::new().fg(theme.del),
                Style::new().fg(faded(theme.fg, pane, 60, theme.fg)),
                Style::new().fg(theme.warn).bold(),
            ],
        }
    }

    /// The sky at row `y` of a pane `height` rows tall: the pane's colour at
    /// the top, glowing toward the theme's magenta at the horizon where the
    /// terminal can draw a shade between.
    fn sky(&self, theme: &Theme, y: i64, height: i64) -> Color {
        let down = u32::try_from(y.max(0) * 100 / height.max(1)).unwrap_or(0);
        match (rgb(theme.title), rgb(self.pane)) {
            (Some(glow), Some(pane)) => blend(glow, pane, down * down * 28 / 10_000),
            _ => self.pane,
        }
    }

    /// A neon colour as it stands while its sign is off: faded to the tower
    /// it is on, or the darker of the sixteen.
    fn unlit(&self, colour: Color) -> Color {
        faded(colour, self.near, 30, darker(colour))
    }
}

/// Draws the city into `area` of `buffer`, cleared to the pane's colour, as
/// it stands `millis` after the origin, from `seed`.
pub(super) fn draw(buffer: &mut Buffer, area: Rect, millis: u64, seed: u64, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let ink = Ink::of(theme);
    let height = i64::from(area.height);
    let width = i64::from(area.width);
    let (far, near) = (FAR.eighths(millis), NEAR.offset(millis));
    for x in 0..width {
        for y in 0..height {
            let sky = ink.sky(theme, y, height);
            let (glyph, style) =
                star(seed, x, y, height, millis, &ink).unwrap_or((' ', Style::new().bg(sky)));
            put(buffer, area, x, y, glyph, style.bg(sky));
        }
        far_column(buffer, area, (seed, x, far), theme, &ink);
    }
    craft(buffer, area, seed, millis, &ink);
    let view = View {
        centre: near + width / 2,
        width,
    };
    for x in 0..width {
        let street = x + near;
        let plot = street.div_euclid(NEAR.plot);
        let [before, here, after] =
            [plot - 1, plot, plot + 1].map(|plot| NEAR.tower(seed, plot, height));
        for y in 0..height {
            let front = here
                .as_ref()
                .and_then(|tower| near_cell(tower, street, y, millis, &ink));
            let drawn = front.or_else(|| {
                let behind = buffer_bg(buffer, area, x, y);
                [before, here, after]
                    .iter()
                    .flatten()
                    .filter_map(|tower| tower.side(street, view).map(|side| (tower, side)))
                    .min_by_key(|(_, (out, _))| *out)
                    .and_then(|(tower, side)| {
                        side_cell(tower, (street, y), side, millis, &ink, behind)
                    })
            });
            if let Some((glyph, style)) = drawn {
                put(buffer, area, x, y, glyph, style);
            }
        }
    }
}

/// The far towers at column `x` of the pane, with the row `far` eighths of a
/// column along the street.
///
/// Where the terminal draws 24-bit colour a cell an edge crosses is filled
/// from its left by as many eighths as lie on the one side of it, so the
/// edges glide between the row's steps; in the sixteen names a cell is a
/// tower's or the sky's whole, and the row steps a column at a time.
fn far_column(
    buffer: &mut Buffer,
    area: Rect,
    (seed, x, far): (u64, i64, i64),
    theme: &Theme,
    ink: &Ink,
) {
    let height = i64::from(area.height);
    let (along, part) = (far.div_euclid(8), far.rem_euclid(8));
    let here = FAR.at(seed, x + along, height);
    if !ink.deep || part == 0 {
        if let Some(tower) = here {
            for y in tower.top..height {
                let (glyph, style) = far_cell(&tower, x + along, y, ink);
                put(buffer, area, x, y, glyph, style);
            }
        }
        return;
    }
    let next = FAR.at(seed, x + along + 1, height);
    // The cell holds the last `8 - part` eighths of the column it starts in
    // and the first `part` of the one after.
    let first = LEFT_EIGHTHS[usize::try_from(7 - part).unwrap_or(0)];
    for y in 0..height {
        let stands = |tower: &Option<Tower>| tower.as_ref().is_some_and(|tower| y >= tower.top);
        let sky = ink.sky(theme, y, height);
        let haze = ink.far_colour;
        let (glyph, style) = match (stands(&here), stands(&next)) {
            (false, false) => continue,
            (true, true) => {
                let (tower, at) = match part < 4 {
                    true => (here, x + along),
                    false => (next, x + along + 1),
                };
                match tower {
                    Some(tower) => far_cell(&tower, at, y, ink),
                    None => continue,
                }
            }
            (true, false) => (first, Style::new().fg(haze).bg(sky)),
            (false, true) => (first, Style::new().fg(sky).bg(haze)),
        };
        // A glow that has reached the haze's own shade leaves no edge to
        // draw, and a glyph in the colour it stands on is one nobody sees.
        let (glyph, style) = match style.fg.is_some() && style.fg == style.bg {
            true => (' ', Style::new().bg(haze)),
            false => (glyph, style),
        };
        put(buffer, area, x, y, glyph, style);
    }
}

/// A star in the top half of the sky at `(x, y)`, twinkling, if one is
/// there.
fn star(seed: u64, x: i64, y: i64, height: i64, millis: u64, ink: &Ink) -> Option<(char, Style)> {
    if y * 2 >= height {
        return None;
    }
    let key = key(seed, 0x57A2, (x.cast_unsigned() << 16) ^ y.cast_unsigned());
    if key % 1000 >= 14 {
        return None;
    }
    let beat = (millis + (key >> 10) % 5_000) / (700 + (key >> 24) % 1_500);
    let glyph = match mix(key ^ beat) % 6 {
        0 => '·',
        1 => '+',
        _ => '.',
    };
    Some((glyph, ink.stars))
}

/// What a far tower shows at column `x` of the street, row `y`: its haze,
/// and now and then a dim window where the terminal can draw one.
fn far_cell(tower: &Tower, x: i64, y: i64, ink: &Ink) -> (char, Style) {
    let window = (x - tower.left) % 2 == 1
        && (y - tower.top) % 3 == 1
        && key(
            tower.key,
            0xF1,
            (x.cast_unsigned() << 20) ^ y.cast_unsigned(),
        )
        .is_multiple_of(5);
    match (ink.deep, window, ink.far_window) {
        (true, true, Some(style)) => ('·', style),
        (true, _, _) => (' ', ink.far),
        (false, _, _) => ('░', ink.far),
    }
}

/// What a near tower shows at column `x` of the street, row `y`, if
/// anything: what stands on its roof, its outline, its sign or a window.
fn near_cell(tower: &Tower, x: i64, y: i64, millis: u64, ink: &Ink) -> Option<(char, Style)> {
    if y < tower.top {
        return roof(tower, x, y, millis, ink);
    }
    if !tower.holds(x, y) {
        return None;
    }
    let body = Style::new().bg(ink.near);
    let outline = body.fg(faded(ink.neon[tower.tint], ink.near, 32, Color::DarkGray));
    if let Some(edge) = edge(tower, x, y) {
        return Some((edge, outline));
    }
    if let Some(sign) = sign(tower, x, y, millis, ink) {
        return Some(sign);
    }
    let from = (x - tower.left, y - tower.top);
    if from.0 % 2 != 0 || from.1 < 2 || from.1 % 2 != 0 {
        return Some((' ', body));
    }
    Some(window(tower, x, y, millis, ink).unwrap_or((' ', body)))
}

/// The line a tower's outline draws at `(x, y)`, if the cell is on it: a
/// wall where the cell beside it is not the tower, a roof where the cell
/// above it is not, a corner where both, and an inside corner where a
/// setback steps in.
fn edge(tower: &Tower, x: i64, y: i64) -> Option<char> {
    let (up, left, right) = (
        tower.holds(x, y - 1),
        tower.holds(x - 1, y),
        tower.holds(x + 1, y),
    );
    match (up, left, right) {
        (false, false, _) => Some('┌'),
        (false, _, false) => Some('┐'),
        (false, _, _) => Some('─'),
        (true, false, _) | (true, _, false) => Some('│'),
        (true, true, true) if !tower.holds(x - 1, y - 1) => Some('┘'),
        (true, true, true) if !tower.holds(x + 1, y - 1) => Some('└'),
        (true, true, true) => None,
    }
}

/// A window at `(x, y)`: lit in the tower's colour mostly, going on and off
/// at its own slow pace, or dark where the terminal can draw a dark one.
fn window(tower: &Tower, x: i64, y: i64, millis: u64, ink: &Ink) -> Option<(char, Style)> {
    match lit(tower, x, y, millis, ink) {
        Some(colour) => Some(('▪', Style::new().bg(ink.near).fg(colour))),
        None => ink.dark_window.map(|style| ('▪', style)),
    }
}

/// The colour the window at `(x, y)` is lit in at `millis`, or `None` while
/// it is dark.
fn lit(tower: &Tower, x: i64, y: i64, millis: u64, ink: &Ink) -> Option<Color> {
    let key = key(
        tower.key,
        0x317D,
        (x.cast_unsigned() << 20) ^ y.cast_unsigned(),
    );
    let pace = 9_000 + (key >> 8) % 40_000;
    let spell = (millis + (key >> 24) % pace) / pace;
    let roll = mix(key ^ spell);
    if roll % 100 >= tower.lit {
        return None;
    }
    let tint = match (roll >> 8) % 10 {
        0..=6 => tower.tint,
        other => usize::try_from(other).unwrap_or(0) % ink.neon.len(),
    };
    Some(ink.neon[tint])
}

/// What a near tower's side shows at `(x, y)` of the street, where
/// [`Tower::side`] put it `out` columns from the front with its top edge at
/// half-row `top`, over `behind`, the colour already drawn there: the upper
/// half of a cell the edge halves, its shade below, and a window lit now and
/// then on every other column, dimmer than the front's for facing away.
fn side_cell(
    tower: &Tower,
    (x, y): (i64, i64),
    (out, top): (i64, i64),
    millis: u64,
    ink: &Ink,
    behind: Color,
) -> Option<(char, Style)> {
    if 2 * y + 1 < top {
        return None;
    }
    let shade = ink.sides[tower.tint % ink.sides.len()];
    if 2 * y + 1 == top {
        // The sixteen names have nothing to fill half a cell with that would
        // not read as a block of its own, so the edge steps there instead.
        return (ink.deep && behind != shade).then(|| ('▄', Style::new().fg(shade).bg(behind)));
    }
    if !ink.deep {
        return Some(('░', ink.stipple));
    }
    let face = Style::new().bg(shade);
    let row = y - tower.top;
    let window = (out % 2 == 1 && row >= 2 && row % 2 == 0)
        .then(|| lit(tower, x.wrapping_add(0x51DE), y, millis, ink))
        .flatten();
    Some(match window {
        Some(colour) => ('▪', face.fg(faded(colour, shade, 45, colour))),
        None => (' ', face),
    })
}

/// The sign running down a tower's face at `(x, y)`, if it has one there:
/// katakana in neon that flickers off now and then.
fn sign(tower: &Tower, x: i64, y: i64, millis: u64, ink: &Ink) -> Option<(char, Style)> {
    let rows = tower.sign?;
    let down = y - tower.top - tower.setback - 2;
    if x != tower.sign_column() || !(0..rows).contains(&down) {
        return None;
    }
    let colour = ink.neon[(tower.tint + 1) % ink.neon.len()];
    let letter = key(tower.key, 0x5167, down.cast_unsigned());
    let span = u64::from(KATAKANA.1 - KATAKANA.0 + 1);
    let glyph = u32::try_from(letter % span)
        .ok()
        .and_then(|at| char::from_u32(KATAKANA.0 + at))
        .unwrap_or('ｦ');
    Some((glyph, neon(tower, colour, millis, ink).bg(ink.near)))
}

/// What stands above a tower's roof at `(x, y)`: a mast with its light, or
/// the sign with its word.
fn roof(tower: &Tower, x: i64, y: i64, millis: u64, ink: &Ink) -> Option<(char, Style)> {
    let above = tower.top - y;
    match tower.roof {
        Roof::Flat => None,
        Roof::Mast(tall) if x == tower.middle() && above <= tall => {
            if above < tall {
                return Some(('│', ink.mast));
            }
            let blink = (millis + tower.key % 1_000) % 1_600 < 800;
            Some(match blink {
                true => ('*', ink.beacon),
                false => ('·', ink.mast),
            })
        }
        Roof::Mast(_) => None,
        Roof::Sign(word) if above == 1 => {
            let word = WORDS.get(word)?;
            let length = i64::try_from(word.chars().count()).unwrap_or(0);
            let start = tower.middle() - length / 2;
            let at = usize::try_from(x - start).ok()?;
            let glyph = word.chars().nth(at)?;
            let colour = ink.neon[(tower.tint + 2) % ink.neon.len()];
            Some((glyph, neon(tower, colour, millis, ink)))
        }
        Roof::Sign(_) => None,
    }
}

/// A neon colour as a tower's sign shows it at `millis`: lit, but for the
/// moments it flickers off, which are more for a sign whose tube is going.
/// It sets no background, so a sign on a roof keeps the sky behind it.
fn neon(tower: &Tower, colour: Color, millis: u64, ink: &Ink) -> Style {
    let failing = (tower.key >> 45).is_multiple_of(4);
    let beat = (millis / 60).wrapping_add(tower.key >> 13);
    let off = match failing {
        true => mix(beat / 3) % 9 < 2,
        false => beat % 97 < 2,
    };
    match off {
        true => Style::new().fg(ink.unlit(colour)),
        false => Style::new().fg(colour).bold(),
    }
}

/// The craft crossing the sky between the rows of towers: a light at the
/// front, a body and a red light behind, each at a height and speed of its
/// own, going one way or the other.
fn craft(buffer: &mut Buffer, area: Rect, seed: u64, millis: u64, ink: &Ink) {
    let (width, height) = (i64::from(area.width), i64::from(area.height));
    let count = (area.width / 26).clamp(1, 8);
    for n in 0..u64::from(count) {
        let key = key(seed, 0xC2AF, n);
        let lane = height * (12 + i64::try_from((key >> 4) % 44).unwrap_or(0)) / 100;
        let speed = 4 + (key >> 12) % 10;
        let span = (width + 8).cast_unsigned();
        let along = i64::try_from((millis.saturating_mul(speed) / 1000 + (key >> 20)) % span)
            .unwrap_or(0)
            - 4;
        let rightward = key.is_multiple_of(2);
        let (x, step) = match rightward {
            true => (along, -1),
            false => (width - 1 - along, 1),
        };
        // The front first, then the body and the tail behind it.
        for (part, glyph) in ['>', '=', '-'].into_iter().enumerate() {
            let glyph = match (rightward, glyph) {
                (false, '>') => '<',
                (_, other) => other,
            };
            let at = x + step * i64::try_from(part).unwrap_or(0);
            let style = ink.craft[2 - part];
            let sky = buffer_bg(buffer, area, at, lane);
            put(buffer, area, at, lane, glyph, style.bg(sky));
        }
    }
}

/// The background already drawn at `(x, y)` of `area`, so a craft keeps the
/// sky's glow behind it.
fn buffer_bg(buffer: &Buffer, area: Rect, x: i64, y: i64) -> Color {
    cell_at(area, x, y).map_or(Color::Reset, |(x, y)| buffer[(x, y)].bg)
}

/// The buffer position of `(x, y)` counted from `area`'s corner, if it is
/// inside the area.
fn cell_at(area: Rect, x: i64, y: i64) -> Option<(u16, u16)> {
    let x = u16::try_from(x).ok().filter(|x| *x < area.width)?;
    let y = u16::try_from(y).ok().filter(|y| *y < area.height)?;
    Some((area.x + x, area.y + y))
}

/// `glyph` in `style` at `(x, y)` counted from `area`'s corner, where that is
/// inside the area.
fn put(buffer: &mut Buffer, area: Rect, x: i64, y: i64, glyph: char, style: Style) {
    if let Some(at) = cell_at(area, x, y) {
        buffer[at].set_char(glyph).set_style(style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use crate::theme::{CYBER, Depth};

    fn city(theme: &Theme, millis: u64) -> Buffer {
        let area = Rect::new(0, 0, 120, 30);
        let mut buffer = Buffer::empty(area);
        super::super::draw(&mut buffer, area, Duration::from_millis(millis), 7, theme);
        buffer
    }

    fn row(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn the_near_towers_pass_faster_than_the_far_ones_and_both_keep_moving() {
        for millis in [0, 10_000, 3_600_000] {
            let later = millis + 60_000;
            let far = FAR.offset(later) - FAR.offset(millis);
            let near = NEAR.offset(later) - NEAR.offset(millis);
            assert!(far > 0, "the far towers stood still");
            assert!(near > far * 2, "{near} columns near against {far} far");
        }
    }

    /// A column a second reads as a jolt rather than as motion, and a pane
    /// crossed in a few seconds as a rush.
    #[test]
    fn the_near_row_steps_several_times_a_second_and_takes_its_time_crossing() {
        let steps = NEAR.offset(61_000) - NEAR.offset(60_000);
        assert!((4..=6).contains(&steps), "{steps} columns a second");
        let crossing = 120 * NEAR.millis_a_column;
        assert!(
            crossing >= 20_000,
            "an 120-column pane crossed in {crossing} ms"
        );
    }

    /// Between two of the far row's steps its edges move by eighths of a
    /// cell, where the terminal draws 24-bit colour; in the sixteen names a
    /// cell is the sky's or a tower's whole.
    #[test]
    fn the_far_edges_glide_by_eighths_between_the_rows_steps() {
        let eighth = |glyph: &str| {
            glyph
                .chars()
                .next()
                .is_some_and(|c| LEFT_EIGHTHS.contains(&c))
        };
        let deep = CYBER.at(Depth::TrueColour);
        let step = FAR.millis_a_column;
        let (between, on) = (60 * step + step * 3 / 8, 60 * step);
        assert_eq!(FAR.offset(between), FAR.offset(on));
        let glides = city(&deep, between);
        assert!(glides.content().iter().any(|cell| eighth(cell.symbol())));
        assert_ne!(glides, city(&deep, on), "nothing moved between the steps");
        assert!(
            !city(&deep, on)
                .content()
                .iter()
                .any(|cell| eighth(cell.symbol()))
        );
        assert!(
            !city(&CYBER, between)
                .content()
                .iter()
                .any(|cell| eighth(cell.symbol()))
        );
    }

    /// The pane looks from its middle: a tower left of it shows its right
    /// side, one right of it its left, deeper the further out it stands, and
    /// one straight ahead none.
    #[test]
    fn a_tower_shows_the_side_that_faces_the_middle_deeper_toward_the_edge() {
        let at = |left: i64| Tower {
            key: 5,
            left,
            right: left + 8,
            top: 10,
            setback: 0,
            roof: Roof::Flat,
            tint: 0,
            lit: 50,
            sign: None,
        };
        let view = View {
            centre: 60,
            width: 120,
        };
        let far_left = at(0);
        assert_eq!(far_left.side(8, view), Some((0, 21)));
        assert_eq!(far_left.side(-1, view), None, "the left side faces away");
        assert_eq!(far_left.depth(view), DEEPEST_SIDE);
        let near_left = at(36);
        assert!(near_left.depth(view) < far_left.depth(view));
        assert_eq!(at(56).side(64, view), None, "straight ahead");
        let right = at(100);
        assert_eq!(right.side(99, view), Some((0, 21)));
        assert_eq!(right.side(108, view), None);
        // The top edge drops half a row for each column away from the front.
        assert_eq!(far_left.side(10, view), Some((2, 23)));
    }

    /// A side shows where no tower's front stands, and the front of the
    /// tower beside it hides what of it would reach that far.
    #[test]
    fn a_side_shows_in_the_gap_beside_its_tower_and_never_over_a_front() {
        let deep = CYBER.at(Depth::TrueColour);
        let buffer = city(&deep, 33_000);
        let ink = Ink::of(&deep);
        let shaded = buffer
            .content()
            .iter()
            .filter(|cell| ink.sides.contains(&cell.bg))
            .count();
        assert!(shaded > 30, "{shaded} cells of side");
        let near = NEAR.offset(33_000);
        let view = View {
            centre: near + 60,
            width: 120,
        };
        for x in 0..120 {
            let street = x + near;
            let Some(tower) = NEAR.at(7, street, 30) else {
                continue;
            };
            for y in (tower.top + tower.setback)..30 {
                let cell = &buffer[(
                    u16::try_from(x).expect("on the pane"),
                    u16::try_from(y).expect("on the pane"),
                )];
                assert!(
                    !ink.sides.contains(&cell.bg) || tower.side(street, view).is_some(),
                    "({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn the_street_is_built_up_with_only_a_few_gaps_between_the_towers() {
        for millis in [0, 50_000, 3_600_000] {
            let (far, near) = (FAR.offset(millis), NEAR.offset(millis));
            let view = View {
                centre: near + 100,
                width: 200,
            };
            let side = |street: i64| {
                let plot = street.div_euclid(NEAR.plot);
                (plot - 1..=plot + 1)
                    .filter_map(|plot| NEAR.tower(7, plot, 30))
                    .any(|tower| tower.side(street, view).is_some())
            };
            let open = (0..200)
                .filter(|x| {
                    FAR.at(7, x + far, 30).is_none()
                        && NEAR.at(7, x + near, 30).is_none()
                        && !side(x + near)
                })
                .count();
            assert!(open < 20, "{open} columns of 200 with no tower");
        }
    }

    #[test]
    fn the_top_of_the_sky_holds_only_stars_and_craft() {
        for millis in [0, 7_000, 90_000, 1_000_000] {
            let top = row(&city(&CYBER, millis), 0);
            assert!(
                top.chars().all(|glyph| " .·+<>=-".contains(glyph)),
                "{top:?}"
            );
        }
    }

    #[test]
    fn a_window_goes_on_and_off_at_a_slow_pace_of_its_own() {
        let tower = Tower {
            key: 99,
            left: 0,
            right: 40,
            top: 0,
            setback: 0,
            roof: Roof::Flat,
            tint: 0,
            lit: 50,
            sign: None,
        };
        let ink = Ink::of(&CYBER);
        let lit = |x: i64, millis: u64| window(&tower, x, 4, millis, &ink).is_some();
        let windows = 0..400;
        let at = |millis: u64| windows.clone().filter(|x| lit(*x, millis)).count();
        let share = at(60_000);
        assert!((120..=280).contains(&share), "{share} of 400 lit at half");
        let changed = |gap: u64| {
            windows
                .clone()
                .filter(|x| lit(*x, 60_000) != lit(*x, 60_000 + gap))
                .count()
        };
        assert!(
            changed(100) <= 4,
            "{} changed in a tenth of a second",
            changed(100)
        );
        assert!(
            changed(120_000) >= 40,
            "{} changed in two minutes",
            changed(120_000)
        );
    }

    #[test]
    fn a_tower_is_outlined_with_its_corners_and_walls() {
        let tower = Tower {
            key: 1,
            left: 0,
            right: 10,
            top: 2,
            setback: 3,
            roof: Roof::Flat,
            tint: 0,
            lit: 50,
            sign: None,
        };
        assert_eq!(edge(&tower, 2, 2), Some('┌'));
        assert_eq!(edge(&tower, 7, 2), Some('┐'));
        assert_eq!(edge(&tower, 4, 2), Some('─'));
        assert_eq!(edge(&tower, 2, 3), Some('│'));
        assert_eq!(edge(&tower, 0, 5), Some('┌'));
        assert_eq!(edge(&tower, 2, 5), Some('┘'));
        assert_eq!(edge(&tower, 7, 5), Some('└'));
        assert_eq!(edge(&tower, 4, 6), None);
    }

    #[test]
    fn neon_lights_the_city_in_the_themes_own_colours() {
        let sixteen = city(&CYBER, 33_000);
        let colours: Vec<Color> = sixteen.content().iter().map(|cell| cell.fg).collect();
        for neon in [CYBER.warn, CYBER.command, CYBER.title] {
            assert!(colours.contains(&neon), "{neon:?} is nowhere");
        }
        let deep = CYBER.at(Depth::TrueColour);
        let glow = city(&deep, 33_000);
        let sky = |y: u16| glow[(0, y)].bg;
        assert_ne!(sky(0), sky(14), "the sky glows toward the horizon");
    }

    #[test]
    fn a_roof_sign_spells_its_word_centred_on_the_roof() {
        let tower = Tower {
            key: 0,
            left: 10,
            right: 20,
            top: 8,
            setback: 0,
            roof: Roof::Sign(1),
            tint: 0,
            lit: 50,
            sign: None,
        };
        let ink = Ink::of(&CYBER);
        let word: String = (10..20)
            .filter_map(|x| roof(&tower, x, 7, 0, &ink).map(|(glyph, _)| glyph))
            .collect();
        assert_eq!(word, "HOTEL");
        assert_eq!(roof(&tower, 15, 6, 0, &ink), None);
    }
}
