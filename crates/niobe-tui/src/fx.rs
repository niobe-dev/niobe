// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What the desktop does behind the panes while a turn is running.
//!
//! The motion is a picture of the whole screen. The panes are opaque and
//! cover it, so what reaches the operator is the desktop they leave
//! uncovered; the drawing code asks this module only for those cells, which
//! is why a frame is answered cell by cell ([`Field::at`]) rather than
//! painted into a buffer and then covered.
//!
//! Every frame is a pure function of the theme, the screen's size and how
//! long the turn has been running. Nothing here holds state and nothing here
//! is random — where the motion looks random, a hash of the cell's position
//! stands in — so the same turn, timed the same, draws the same thing, which
//! is what lets a test assert a frame instead of merely that something moved.
//!
//! A terminal cannot fade, blur or move by half a cell, so the motion is
//! whole characters moving one cell at a time, in the four tones the theme
//! gives it ([`Theme::fx`]). The scanlines a screen could lay over the whole
//! picture are not drawn: a translucent stripe has no cell to be drawn in,
//! and dimming every other row of the desktop instead would dim the motion
//! in the rows it fell through, which reads as flicker rather than as glass.

use std::time::Duration;

use ratatui::style::Color;

use crate::theme::{Motion, Theme};

/// One cell of the motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    /// The character drawn there.
    pub symbol: char,
    /// What colour to draw it in: one of the theme's [`Theme::fx`] tones.
    pub colour: Color,
}

/// The frame of `motion` a turn that has run for `elapsed` is on.
pub fn frame_at(motion: Motion, elapsed: Duration) -> u64 {
    let length = motion.frame().as_millis().max(1);
    u64::try_from(elapsed.as_millis() / length).unwrap_or(u64::MAX)
}

/// One frame of the motion over a screen of a given size.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    motion: Motion,
    tones: [Color; 4],
    width: i64,
    height: i64,
    frame: u64,
}

impl Field {
    /// Frame `frame` of `theme`'s motion over a screen `width` by `height`.
    pub fn new(theme: &Theme, width: u16, height: u16, frame: u64) -> Self {
        Self {
            motion: theme.motion,
            tones: theme.fx,
            width: i64::from(width),
            height: i64::from(height),
            frame,
        }
    }

    /// What the motion draws at column `x`, row `y` of the screen, or `None`
    /// where the cell is left as the desktop.
    pub fn at(&self, x: u16, y: u16) -> Option<Glyph> {
        let (x, y) = (i64::from(x), i64::from(y));
        // Too small a screen for any motion to read as one; the same floor
        // the design has.
        if x >= self.width || y >= self.height || self.width < 4 || self.height < 4 {
            return None;
        }
        let (symbol, tone) = match self.motion {
            Motion::Still => None,
            Motion::Rain => self.rain(x, y),
            Motion::Drift => self.drift(x, y),
            Motion::Sweep => self.sweep(x, y),
        }?;
        Some(Glyph {
            symbol,
            colour: self.tones[tone],
        })
    }

    /// The frame number as a signed count, for the arithmetic that runs
    /// positions backwards.
    fn tick(&self) -> i64 {
        i64::try_from(self.frame).unwrap_or(i64::MAX)
    }

    /// A drop falling down every column, a twelve-cell trail behind it that
    /// fades from the brightest tone at the head to the faintest at the tail.
    ///
    /// Every third column falls at half speed, so the columns never fall in
    /// step. Every column rains, where a full screen of it would read as well
    /// with every other: the desktop the panes leave may be a single column,
    /// and it has to be one that rains.
    fn rain(&self, x: i64, y: i64) -> Option<(char, usize)> {
        let rows = self.height.unsigned_abs();
        let column = x.unsigned_abs();
        let steps = match column % 3 {
            0 => self.frame / 2,
            _ => self.frame,
        };
        // A drop that has fallen out of the bottom waits a while before the
        // next one enters at the top, and how long is the column's own. No
        // longer than a quarter of the screen: where the only desktop the
        // panes leave is a few single columns, a long wait is a long stretch
        // of a running turn that looks like a stopped one.
        let pause = scramble(column, 1) % (rows / 4 + 1);
        let lap = rows + RAIN_TRAIL + pause;
        let at = steps.wrapping_add(scramble(column, 2)) % lap;
        let head = i64::try_from(at).ok()? - i64::try_from(pause).ok()?;
        let behind = head - y;
        let tone = match behind {
            0 => 0,
            1..=2 => 1,
            3..=7 => 2,
            8..RAIN_TRAIL_CELLS => 3,
            _ => return None,
        };
        Some((rain_glyph(column, y.unsigned_abs(), self.frame, rows), tone))
    }

    /// Words drifting along every column, up in one and down in the next, and
    /// a rule sweeping down the screen through them, dotting a wake behind it
    /// where the words are not.
    ///
    /// Every column carries words, for the reason every column rains.
    fn drift(&self, x: i64, y: i64) -> Option<(char, usize)> {
        if let Some(letter) = self.drift_letter(x, y) {
            return Some(letter);
        }
        let rule = self.tick() % (self.height + 4) - 2;
        match y {
            _ if y == rule => Some(('─', 0)),
            _ if y == rule - 1 && x % 2 == 0 => Some(('·', 2)),
            _ => None,
        }
    }

    /// The letter of a drifting word at `x`, `y`, if one is there.
    fn drift_letter(&self, x: i64, y: i64) -> Option<(char, usize)> {
        let column = x.unsigned_abs();
        let direction = match column % 2 {
            1 => 1,
            _ => -1,
        };
        let start = i64::try_from(scramble(column, 3) % self.height.unsigned_abs()).ok()?;
        // The words run off both ends of the screen and come back round, so
        // the loop is longer than the screen by the ten rows either side.
        let lap = self.height + 20;
        let mut top = (start + direction * (self.tick() % lap)).rem_euclid(lap) - 10;
        let first = usize::try_from(column % WORDS.len() as u64).ok()?;
        for word in WORDS.iter().cycle().skip(first).take(WORDS_PER_COLUMN) {
            if top >= self.height + 12 {
                break;
            }
            let len = i64::try_from(word.len()).ok()?;
            if (top..top + len).contains(&y) {
                let letter = word.chars().nth(usize::try_from(y - top).ok()?)?;
                let tone = match column % 3 {
                    1 => 1,
                    _ => 3,
                };
                return Some((letter, tone));
            }
            top += len + WORD_GAP;
        }
        None
    }

    /// A radar in the bottom-right corner — a ring of dots, a ray turning one
    /// eighth of the way round each frame, and the word `working` under it —
    /// and a scanline travelling down the screen behind everything.
    fn sweep(&self, x: i64, y: i64) -> Option<(char, usize)> {
        let (cx, cy) = (self.width - 8, self.height - 6);
        let turn = usize::try_from(self.frame % 8).ok()?;

        let caption = cy + 5;
        if caption < self.height && y == caption {
            let at = x - (cx - 4);
            let spin = SPINNER[usize::try_from(self.frame % SPINNER.len() as u64).ok()?];
            let letter = match at {
                0 => Some(spin),
                _ => usize::try_from(at - 1)
                    .ok()
                    .and_then(|at| CAPTION.chars().nth(at)),
            };
            if let Some(letter) = letter {
                return Some((letter, 1));
            }
        }
        if (x, y) == (cx, cy) {
            return Some(('+', 0));
        }
        let (dx, dy, ray) = RAYS[turn];
        if (1..=3).any(|k| (x, y) == (cx + dx * k * 2, cy + dy * k)) {
            return Some((ray, 1));
        }
        if on_ring(x - cx, y - cy) {
            return Some(('·', 2));
        }
        if y == self.tick() % self.height {
            return Some(('·', 3));
        }
        None
    }
}

/// How many cells of a drop's trail are drawn, the head included.
const RAIN_TRAIL_CELLS: i64 = 12;

/// [`RAIN_TRAIL_CELLS`] as a count of rows.
const RAIN_TRAIL: u64 = RAIN_TRAIL_CELLS.unsigned_abs();

/// The glyphs the rain falls in: half-width katakana and digits, every one of
/// them one cell wide, so a trail cannot push the pane beside it sideways.
const GLYPHS: [char; 46] = [
    'ｦ', 'ｧ', 'ｨ', 'ｩ', 'ｪ', 'ｫ', 'ｬ', 'ｭ', 'ｮ', 'ｯ', 'ｰ', 'ｱ', 'ｲ', 'ｳ', 'ｴ', 'ｵ', 'ｶ', 'ｷ', 'ｸ',
    'ｹ', 'ｺ', 'ｻ', 'ｼ', 'ｽ', 'ｾ', 'ｿ', 'ﾀ', 'ﾁ', 'ﾂ', 'ﾃ', 'ﾄ', 'ﾅ', 'ﾆ', 'ﾇ', 'ﾈ', 'ﾉ', '0', '1',
    '2', '3', '4', '5', '6', '7', '8', '9',
];

/// The glyph the rain shows at a cell on a frame.
///
/// A cell keeps its glyph for as many frames as the screen has rows and then
/// changes, each cell at its own moment, so the rain flickers a little as it
/// falls without every glyph on screen changing at once.
fn rain_glyph(column: u64, row: u64, frame: u64, rows: u64) -> char {
    let cell = column << 16 | row;
    let epoch = frame.wrapping_add(scramble(cell, 4) % rows.max(1)) / rows.max(1);
    GLYPHS[usize::try_from(scramble(cell, epoch) % GLYPHS.len() as u64).unwrap_or(0)]
}

/// The words the drift is made of: what the shell is about — the agent, the
/// shell, the branch — and the kind of thing a session changes. No figure is
/// among them: a number on this screen that nobody measured is the one thing
/// the shell never draws, and a cost drifting past behind the panes is still
/// a cost on screen.
const WORDS: [&str; 8] = [
    "ETAG", "LRU", "TSC", "CACHE", "AGENT", "MAIN", "NIOBE", "SHELL",
];

/// How many words one column carries before it comes round again.
const WORDS_PER_COLUMN: usize = 5;

/// Blank rows between one drifting word and the next.
const WORD_GAP: i64 = 3;

/// The eight positions of the radar's ray, one per frame, as the step it
/// takes from the centre and the line it is drawn in. A step across is two
/// columns, because a cell is about twice as tall as it is wide.
const RAYS: [(i64, i64, char); 8] = [
    (1, 0, '─'),
    (1, 1, '╲'),
    (0, 1, '│'),
    (-1, 1, '╱'),
    (-1, 0, '─'),
    (-1, -1, '╲'),
    (0, -1, '│'),
    (1, -1, '╱'),
];

/// The braille spinner in front of the radar's caption.
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// What the radar says under it, after the spinner.
const CAPTION: &str = " working";

/// Whether a cell `dx` columns and `dy` rows from the radar's centre is on
/// its ring: the dots whose distance rounds to three, with a step across
/// (two columns) weighted at 0.35 of a step down, squared, as the design
/// weights it. That draws the ring as its top and bottom three rows out, and
/// its four far corners two rows out. In hundredths, so the test is exact: a
/// distance that rounds to three is one whose square is between 2.5² and 3.5².
fn on_ring(dx: i64, dy: i64) -> bool {
    if dx % 2 != 0 || !(-6..=6).contains(&dx) || !(-3..=3).contains(&dy) {
        return false;
    }
    let across = dx / 2;
    let distance = 35 * across * across + 100 * dy * dy;
    (625..1225).contains(&distance)
}

/// A number that looks unrelated to the ones beside it, so the motion looks
/// scattered without a random source a test could not pin down.
fn scramble(a: u64, b: u64) -> u64 {
    let mut n = a
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(b.wrapping_mul(0xbf58_476d_1ce4_e5b9));
    n ^= n >> 29;
    n = n.wrapping_mul(0x94d0_49bb_1331_11eb);
    n ^ (n >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{CLASSIC, CYBER, Depth, MODERN, NEO, THEMES};

    const WIDTH: u16 = 60;
    const HEIGHT: u16 = 24;

    /// Every cell of one frame, a row per line, with a space where the
    /// desktop is left alone.
    fn picture(theme: &Theme, frame: u64) -> Vec<String> {
        let field = Field::new(theme, WIDTH, HEIGHT, frame);
        (0..HEIGHT)
            .map(|y| {
                (0..WIDTH)
                    .map(|x| field.at(x, y).map_or(' ', |glyph| glyph.symbol))
                    .collect()
            })
            .collect()
    }

    /// Column `x` of a picture, top to bottom.
    fn column_of(picture: &[String], x: usize) -> String {
        picture
            .iter()
            .map(|row| row.chars().nth(x).unwrap_or(' '))
            .collect()
    }

    fn every_palette() -> Vec<Theme> {
        THEMES
            .into_iter()
            .flat_map(|t| [t.at(Depth::Sixteen), t.at(Depth::TrueColour)])
            .collect()
    }

    /// Nothing is drawn outside the screen it was asked for: a glyph off the
    /// edge of the field would be written into a buffer at a cell that is not
    /// there.
    #[test]
    fn a_cell_off_the_screen_is_never_drawn() {
        for theme in THEMES {
            for frame in 0..50 {
                let field = Field::new(&theme, WIDTH, HEIGHT, frame);
                for (x, y) in [(WIDTH, 0), (0, HEIGHT), (WIDTH + 5, HEIGHT + 5)] {
                    assert_eq!(field.at(x, y), None, "{}", theme.name);
                }
            }
        }
    }

    /// The whole point of the motion is that it moves: a desktop that draws
    /// the same frame twice running says nothing about whether the turn is
    /// alive.
    #[test]
    fn a_theme_that_moves_draws_something_different_from_one_frame_to_the_next() {
        for theme in THEMES {
            if theme.motion == Motion::Still {
                continue;
            }
            let frames: Vec<Vec<String>> = (0..40).map(|f| picture(&theme, f)).collect();
            let moved = frames.windows(2).filter(|pair| pair[0] != pair[1]).count();
            assert_eq!(
                moved, 39,
                "{}: only {moved} of 39 frames changed",
                theme.name
            );
        }
    }

    /// Only a column at each edge and the one between the panes may be
    /// uncovered, so the motion has to show in any one column, not just
    /// somewhere on the screen.
    #[test]
    fn every_column_of_a_moving_theme_shows_the_motion_within_a_lap() {
        for theme in THEMES {
            if theme.motion == Motion::Still {
                continue;
            }
            let frames: Vec<Vec<String>> = (0..120).map(|f| picture(&theme, f)).collect();
            for x in 0..usize::from(WIDTH) {
                assert!(
                    frames.iter().any(|p| column_of(p, x).trim() != ""),
                    "{}: column {x} never moved",
                    theme.name
                );
            }
        }
    }

    /// A theme that does not animate draws nothing at all, rather than
    /// something that happens to sit still.
    #[test]
    fn a_still_theme_leaves_the_desktop_alone() {
        assert_eq!(CLASSIC.motion, Motion::Still);
        for frame in 0..200 {
            assert!(
                picture(&CLASSIC, frame)
                    .iter()
                    .all(|row| row.trim().is_empty())
            );
        }
    }

    /// The motion is drawn in the theme, not in colours of its own: a glyph
    /// in a colour the palette does not contain is one the operator's own
    /// terminal scheme cannot touch.
    #[test]
    fn nothing_is_drawn_in_a_colour_the_theme_does_not_name() {
        for theme in every_palette() {
            for frame in 0..60 {
                let field = Field::new(&theme, WIDTH, HEIGHT, frame);
                for y in 0..HEIGHT {
                    for x in 0..WIDTH {
                        if let Some(glyph) = field.at(x, y) {
                            assert!(theme.fx.contains(&glyph.colour), "{}", theme.name);
                        }
                    }
                }
            }
        }
    }

    /// The same turn, timed the same, draws the same thing. A frame that
    /// depended on anything but its arguments could not be asserted at all.
    #[test]
    fn the_same_frame_is_the_same_picture_however_often_it_is_asked_for() {
        for theme in THEMES {
            for frame in [0, 1, 7, 99, 1_000, u64::MAX] {
                assert_eq!(picture(&theme, frame), picture(&theme, frame));
            }
        }
    }

    /// A screen too small for a motion to read as one draws nothing, rather
    /// than panicking on the arithmetic that lays the motion out over it.
    #[test]
    fn a_screen_with_no_room_draws_nothing() {
        for theme in THEMES {
            for (width, height) in [(0, 0), (1, 1), (3, 40), (40, 3)] {
                for frame in 0..40 {
                    let field = Field::new(&theme, width, height, frame);
                    for y in 0..height {
                        for x in 0..width {
                            assert_eq!(field.at(x, y), None, "{} {width}x{height}", theme.name);
                        }
                    }
                }
            }
        }
    }

    /// Each column of the rain is one drop at a time: a run of at most twelve
    /// cells with the brightest tone at the bottom, where the head is, and
    /// the faintest furthest behind it.
    #[test]
    fn a_drop_of_rain_is_a_run_of_twelve_brightest_at_the_head() {
        for frame in 0..200 {
            let field = Field::new(&NEO, WIDTH, HEIGHT, frame);
            for x in 0..WIDTH {
                let lit: Vec<(u16, Color)> = (0..HEIGHT)
                    .filter_map(|y| field.at(x, y).map(|g| (y, g.colour)))
                    .collect();
                assert!(lit.len() <= 12, "column {x}: {} cells", lit.len());
                if let (Some(first), Some(last)) = (lit.first(), lit.last()) {
                    assert_eq!(usize::from(last.0 - first.0) + 1, lit.len(), "a gap in {x}");
                    let head = usize::from(first.0) + lit.len() - 1;
                    // The head is the lowest cell unless it has fallen out of
                    // the bottom, in which case the brightest tone is off
                    // screen too.
                    if head < usize::from(HEIGHT - 1) {
                        assert_eq!(last.1, NEO.fx[0], "column {x}, frame {frame}");
                    }
                }
            }
        }
    }

    /// The rain falls: a drop's head is one row lower on the next frame, or
    /// two frames later in the columns that fall at half speed.
    #[test]
    fn the_rain_falls_a_row_a_frame_and_every_third_column_at_half_speed() {
        let head = |x: u16, frame: u64| {
            let field = Field::new(&NEO, WIDTH, HEIGHT, frame);
            (0..HEIGHT).find(|&y| field.at(x, y).is_some_and(|g| g.colour == NEO.fx[0]))
        };
        let (fast, slow) = (1, 3);
        let frame = (0..500)
            .find(|&f| head(fast, f).is_some_and(|y| y < 10))
            .expect("the drop enters the screen within a lap");
        let at = head(fast, frame).expect("found above");
        assert_eq!(head(fast, frame + 1), Some(at + 1));

        let frame = (0..500)
            .step_by(2)
            .find(|&f| head(slow, f).is_some_and(|y| y < 10))
            .expect("the drop enters the screen within a lap");
        let at = head(slow, frame).expect("found above");
        assert_eq!(head(slow, frame + 1), Some(at));
        assert_eq!(head(slow, frame + 2), Some(at + 1));
    }

    /// A drifting column reads, top to bottom, as whole words and the rule
    /// and its wake: every run of letters in it is a word the drift carries
    /// or the part of one the screen's edge cuts.
    #[test]
    fn a_drifting_column_reads_as_the_words_it_carries() {
        for frame in 0..60 {
            let picture = picture(&CYBER, frame);
            for x in 0..usize::from(WIDTH) {
                let column = column_of(&picture, x);
                let runs = column
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .filter(|run| !run.is_empty());
                for run in runs {
                    assert!(
                        WORDS.iter().any(|word| word.contains(run)),
                        "column {x}, frame {frame}: {run:?} in {column:?}"
                    );
                }
            }
        }
    }

    /// The rule sweeps down the screen a row a frame, across every column no
    /// word is in, and comes round again.
    #[test]
    fn the_rule_sweeps_down_the_screen_a_row_a_frame() {
        for frame in 0..u64::from(HEIGHT) {
            let rule = usize::try_from(frame % u64::from(HEIGHT + 4)).expect("small");
            let Some(row) = rule.checked_sub(2) else {
                continue;
            };
            let picture = picture(&CYBER, frame);
            let drawn = picture[row].chars().filter(|&c| c == '─').count();
            let letters = picture[row]
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .count();
            assert_eq!(
                drawn + letters,
                usize::from(WIDTH),
                "frame {frame}: {}",
                picture[row]
            );
        }
    }

    /// The radar's centre, its ray and its caption are where the design puts
    /// them, and the ray turns one eighth of the way round each frame.
    #[test]
    fn the_radar_turns_an_eighth_a_frame_about_its_centre() {
        let (cx, cy) = (usize::from(WIDTH) - 8, usize::from(HEIGHT) - 6);
        let at = |p: &[String], x: usize, y: usize| p[y].chars().nth(x);

        let first = picture(&MODERN, 0);
        assert_eq!(at(&first, cx, cy), Some('+'));
        for k in 1..=3 {
            assert_eq!(at(&first, cx + 2 * k, cy), Some('─'), "ray east, step {k}");
        }
        let caption: String = first[cy + 5].chars().skip(cx - 4).take(9).collect();
        assert_eq!(caption, "⠋ working");

        let second = picture(&MODERN, 1);
        for k in 1..=3 {
            assert_eq!(at(&second, cx + 2 * k, cy + k), Some('╲'), "ray south-east");
        }
        assert_eq!(picture(&MODERN, 8)[cy], first[cy]);
    }

    /// The ring is the design's: a row of seven dots three rows above and
    /// below the centre, and the four far corners two rows out.
    #[test]
    fn the_radar_ring_is_its_top_and_bottom_and_its_far_corners() {
        for dx in [-6, -4, -2, 0, 2, 4, 6] {
            assert!(on_ring(dx, 3) && on_ring(dx, -3), "{dx}");
        }
        for (dx, dy) in [(6, 2), (-6, 2), (6, -2), (-6, -2)] {
            assert!(on_ring(dx, dy), "{dx}, {dy}");
        }
        assert!(!on_ring(0, 0) && !on_ring(6, 1) && !on_ring(4, 2) && !on_ring(1, 3));
    }

    /// The scanline is a row of dots a row lower each frame, behind the
    /// radar, wrapping at the bottom.
    #[test]
    fn the_scanline_travels_down_the_screen_and_wraps() {
        for frame in [0, 5, u64::from(HEIGHT) + 2] {
            let row = usize::try_from(frame % u64::from(HEIGHT)).expect("small");
            let line = &picture(&MODERN, frame)[row];
            assert!(
                line.chars().take(20).all(|c| c == '·'),
                "frame {frame}: {line}"
            );
        }
    }

    /// The frame number comes from the clock the event loop hands in, so the
    /// motion runs at its own pace whatever the loop's own tick is.
    #[test]
    fn the_frame_advances_at_the_pace_of_the_motion() {
        assert_eq!(frame_at(Motion::Rain, Duration::ZERO), 0);
        assert_eq!(frame_at(Motion::Rain, Duration::from_millis(89)), 0);
        assert_eq!(frame_at(Motion::Rain, Duration::from_millis(90)), 1);
        assert_eq!(frame_at(Motion::Drift, Duration::from_millis(1_200)), 10);
        assert_eq!(frame_at(Motion::Sweep, Duration::from_millis(1_100)), 10);
        assert_eq!(frame_at(Motion::Sweep, Duration::from_secs(3_600)), 32_727);
    }
}
