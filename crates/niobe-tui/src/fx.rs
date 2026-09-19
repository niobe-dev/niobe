// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What the desktop does behind the panes while a turn is running.
//!
//! The panes are opaque, so the only part of the desktop still visible once
//! they are drawn is the column between the session pane and the right-hand
//! stack. That column is what this module fills, one character per cell, in
//! the theme's own colours — a terminal cannot fade, blur or move by half a
//! cell, so the motion is whole characters moving one row at a time.
//!
//! Every frame is a pure function of the theme, the strip's height and how
//! long the turn has been running. Nothing here holds state and nothing here
//! is random: the same turn, timed the same, draws the same thing, which is
//! what lets a test assert a frame instead of merely that something moved.

use ratatui::style::Color;

use crate::theme::{Motion, Theme};

/// One cell of the desktop's motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mote {
    /// Rows from the top of the strip.
    pub row: u16,
    /// The character drawn there.
    pub symbol: char,
    /// What colour to draw it in.
    pub colour: Color,
}

/// How long one frame of the motion lasts.
pub const FRAME: std::time::Duration = std::time::Duration::from_millis(100);

/// The frame a turn that has run for `elapsed` is on.
pub fn frame_at(elapsed: std::time::Duration) -> u64 {
    u64::try_from(elapsed.as_millis() / FRAME.as_millis()).unwrap_or(0)
}

/// The strip of desktop between the panes, `height` rows tall, on `frame`.
pub fn column(theme: &Theme, height: u16, frame: u64) -> Vec<Mote> {
    if height == 0 {
        return Vec::new();
    }
    match theme.motion {
        Motion::Still => Vec::new(),
        Motion::Rain => rain(theme, height, frame),
        Motion::Drift => drift(theme, height, frame),
        Motion::Sweep => sweep(theme, height, frame),
    }
}

/// The glyphs the rain falls in: half-width katakana and digits, every one of
/// them one cell wide, so a trail cannot push the pane beside it sideways.
const GLYPHS: [char; 26] = [
    'ｦ', 'ｧ', 'ｨ', 'ｩ', 'ｪ', 'ｫ', 'ｬ', 'ｭ', 'ｮ', 'ｯ', 'ｰ', 'ｱ', 'ｲ', 'ｳ', 'ｴ', 'ｵ', '0', '1', '2',
    '3', '4', '5', '6', '7', '8', '9',
];

/// How many cells fall behind the head of the rain.
const TRAIL: u64 = 8;

/// Glyphs falling one row per frame, the head bright and the trail behind it
/// dim, re-entering at the top once the head has fallen out of the bottom.
///
/// The head is the only cell in the accent colour: a trail all one colour
/// reads as a line rather than as something falling.
fn rain(theme: &Theme, height: u16, frame: u64) -> Vec<Mote> {
    let rows = u64::from(height);
    let head = frame % (rows + TRAIL - 1);
    (0..TRAIL)
        .filter_map(|behind| {
            let row = head.checked_sub(behind).filter(|row| *row < rows)?;
            Some(Mote {
                row: u16::try_from(row).unwrap_or(0),
                // The glyph a cell shows changes faster than the rain falls,
                // which is what makes a trail flicker rather than slide.
                symbol: GLYPHS[usize::try_from(scramble(row, frame / 2) % 26).unwrap_or(0)],
                colour: match behind {
                    0 => theme.fx,
                    _ => theme.dim,
                },
            })
        })
        .collect()
}

/// The words the cyber theme drifts through the gutter. They are what the
/// shell is about — what the session spends, what it runs and what it changes
/// — written down the strip one letter per row.
const WORDS: [&str; 6] = ["NIOBE", "AGENT", "TOKENS", "SHELL", "EDIT", "COST"];

/// Blank rows between one drifting word and the next.
const WORD_GAP: u64 = 4;

/// Words drifting down the strip, with a mark that travels through them.
///
/// The words move at half the speed of the mark, so the strip has two things
/// going at once in one column, which is what keeps a single column from
/// reading as a progress bar.
fn drift(theme: &Theme, height: u16, frame: u64) -> Vec<Mote> {
    let tape: u64 = WORDS
        .iter()
        .map(|word| word.chars().count() as u64 + WORD_GAP)
        .sum();
    let offset = frame / 2;

    let mut motes: Vec<Mote> = (0..u64::from(height))
        .filter_map(|row| {
            let (symbol, word) = letter_at(tape.checked_sub(offset % tape)? + row, tape)?;
            Some(Mote {
                row: u16::try_from(row).unwrap_or(0),
                symbol,
                // Every other word in the accent, so the strip has the two
                // colours the theme is built on rather than one of them.
                colour: match word % 2 {
                    0 => theme.dim,
                    _ => theme.fx,
                },
            })
        })
        .collect();

    // The mark travels faster than the words and is drawn over them, and it
    // leaves the strip for a moment at the bottom before coming round.
    let mark = frame % (u64::from(height) + WORD_GAP);
    if let Ok(mark) = u16::try_from(mark)
        && mark < height
    {
        motes.retain(|mote| mote.row != mark);
        motes.push(Mote {
            row: mark,
            symbol: '─',
            colour: theme.fx,
        });
    }
    motes
}

/// The letter at `at` on the endless tape of words, and which word it belongs
/// to. A position in one of the gaps between words is no letter at all.
fn letter_at(at: u64, tape: u64) -> Option<(char, usize)> {
    let mut at = at % tape;
    for (which, word) in WORDS.iter().enumerate() {
        let len = word.chars().count() as u64;
        if at < len {
            return word
                .chars()
                .nth(usize::try_from(at).ok()?)
                .map(|c| (c, which));
        }
        at -= len.min(at);
        if at < WORD_GAP {
            return None;
        }
        at -= WORD_GAP;
    }
    None
}

/// How many cells trail the sweep.
const SWEEP_TRAIL: u64 = 3;

/// One mote travelling down the strip, trailing off behind it: the quietest
/// of the motions, for the theme whose chrome is meant to stay out of the way.
fn sweep(theme: &Theme, height: u16, frame: u64) -> Vec<Mote> {
    let rows = u64::from(height);
    let head = frame % (rows + SWEEP_TRAIL - 1);
    (0..SWEEP_TRAIL)
        .filter_map(|behind| {
            let row = head.checked_sub(behind).filter(|row| *row < rows)?;
            Some(Mote {
                row: u16::try_from(row).unwrap_or(0),
                symbol: match behind {
                    0 => '\u{25cf}',
                    _ => '\u{00b7}',
                },
                colour: match behind {
                    0 => theme.fx,
                    _ => theme.dim,
                },
            })
        })
        .collect()
}

/// A number that looks unrelated to the ones beside it, so the rain flickers
/// without a random source a test could not pin down.
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
    use crate::theme::{CLASSIC, THEMES};

    const HEIGHT: u16 = 28;

    /// Where the motion is drawn is one column between two panes. A mote
    /// outside it would be drawn over a pane, which is content.
    #[test]
    fn every_mote_lands_inside_the_strip_it_was_asked_for() {
        for theme in THEMES {
            for frame in 0..200 {
                for mote in column(&theme, HEIGHT, frame) {
                    assert!(mote.row < HEIGHT, "{}: row {}", theme.name, mote.row);
                }
            }
        }
    }

    /// The whole point of the motion is that it moves: a strip that draws the
    /// same frame twice running says nothing about whether the turn is alive.
    #[test]
    fn a_theme_that_moves_draws_something_different_from_one_frame_to_the_next() {
        for theme in THEMES {
            if theme.motion == Motion::Still {
                continue;
            }
            let frames: Vec<Vec<Mote>> = (0..40).map(|f| column(&theme, HEIGHT, f)).collect();
            assert!(
                frames.iter().all(|frame| !frame.is_empty()),
                "{}: a frame of the motion was blank",
                theme.name
            );
            let moved = frames.windows(2).filter(|pair| pair[0] != pair[1]).count();
            assert!(
                moved >= 30,
                "{}: only {moved} of 39 frames changed",
                theme.name
            );
        }
    }

    /// A theme that does not animate draws nothing at all, rather than
    /// something that happens to sit still.
    #[test]
    fn a_still_theme_leaves_the_desktop_alone() {
        assert_eq!(CLASSIC.motion, Motion::Still);
        for frame in 0..200 {
            assert_eq!(column(&CLASSIC, HEIGHT, frame), Vec::new());
        }
    }

    /// The motion is drawn in the theme, not in colours of its own: a strip in
    /// a colour the palette does not contain is a strip the operator's own
    /// terminal scheme cannot touch.
    #[test]
    fn nothing_is_drawn_in_a_colour_the_theme_does_not_name() {
        for theme in THEMES {
            for frame in 0..200 {
                for mote in column(&theme, HEIGHT, frame) {
                    assert!(
                        mote.colour == theme.fx || mote.colour == theme.dim,
                        "{}: {:?}",
                        theme.name,
                        mote.colour
                    );
                }
            }
        }
    }

    /// The same turn, timed the same, draws the same thing. A frame that
    /// depended on anything but its arguments could not be asserted at all.
    #[test]
    fn the_same_frame_is_the_same_picture_however_often_it_is_asked_for() {
        for theme in THEMES {
            for frame in [0, 1, 7, 99, 1_000] {
                assert_eq!(column(&theme, HEIGHT, frame), column(&theme, HEIGHT, frame));
            }
        }
    }

    /// A strip with no room draws nothing rather than panicking on the
    /// arithmetic that spaces the motion out over it.
    #[test]
    fn a_strip_with_no_rows_draws_nothing() {
        for theme in THEMES {
            for height in [0, 1] {
                for frame in 0..40 {
                    assert!(
                        column(&theme, height, frame).len() <= usize::from(height),
                        "{}: {height} rows",
                        theme.name
                    );
                }
            }
        }
    }

    /// The frame number comes from the clock the event loop hands in, so that
    /// the motion runs at the same speed whatever the loop's own tick is.
    #[test]
    fn the_frame_advances_once_every_tenth_of_a_second() {
        use std::time::Duration;
        assert_eq!(frame_at(Duration::ZERO), 0);
        assert_eq!(frame_at(Duration::from_millis(99)), 0);
        assert_eq!(frame_at(Duration::from_millis(100)), 1);
        assert_eq!(frame_at(Duration::from_secs(1)), 10);
        assert_eq!(frame_at(Duration::from_secs(3600)), 36_000);
    }
}
