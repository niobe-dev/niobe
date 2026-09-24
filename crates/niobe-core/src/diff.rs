// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The line arithmetic every backend's file changes are counted with.
//!
//! A bridge knows what its CLI replaced with what; it does not get to decide
//! what a changed line is. Both bridges count through [`lines_changed`], so a
//! session cannot report one figure on one backend and another figure on the
//! other for the same edit.
//!
//! The counts are the ones `git diff --numstat` prints for the same
//! replacement: the minimal number of lines that have to be removed and added
//! to turn one text into the other. That is the longest common subsequence of
//! the two line sequences, which is what a diff's own minimality means — so
//! the pane's `+2 −1` is a figure the operator can check against `git`.
//!
//! A [`Hunk`] is the other half: not how many lines changed but which, as the
//! backend reported them. It is kept here rather than in a bridge so that the
//! transcript draws one shape of diff whichever backend made the change.

use serde::{Deserialize, Serialize};

/// Lines added and removed by replacing `before` with `after`, as
/// `(added, removed)`.
///
/// A trailing newline is a terminator, not a line: `"a\nb\n"` and `"a\nb"` are
/// both two lines, which is how `git` counts them too.
///
/// `None` where the two texts are too large to compare exactly. The comparison
/// is quadratic in the number of lines that differ, so a replacement of
/// thousands of lines by thousands of others is refused rather than allowed to
/// stall a redraw. A caller reports that as a change of an unstated size — an
/// estimate here would be a number nobody could check.
pub fn lines_changed(before: &str, after: &str) -> Option<(u64, u64)> {
    let before: Vec<&str> = before.lines().collect();
    let after: Vec<&str> = after.lines().collect();

    // The lines that match at either end are common to both texts whatever the
    // middle does, so trimming them is free and leaves the quadratic step with
    // only the part that actually differs.
    let head = common_prefix(&before, &after);
    let tail = common_suffix(&before[head..], &after[head..]);
    let before = &before[head..before.len() - tail];
    let after = &after[head..after.len() - tail];

    let common = common_lines(before, after)?;
    Some((
        (after.len() - common) as u64,
        (before.len() - common) as u64,
    ))
}

/// The most cells the longest-common-subsequence table may be walked.
///
/// Four million is a few milliseconds, which a replacement made between two
/// frames can afford. Above it the change is reported as one whose size the
/// backend did not state, because a redraw that waits on a diff is a redraw
/// the operator watches stall.
const MAX_CELLS: usize = 4_000_000;

/// How many lines the two sequences have in common, in order.
fn common_lines(before: &[&str], after: &[&str]) -> Option<usize> {
    if before.is_empty() || after.is_empty() {
        return Some(0);
    }
    if before.len().checked_mul(after.len())? > MAX_CELLS {
        return None;
    }

    // Only the length of the longest common subsequence is wanted, not the
    // subsequence itself, so two rows of the table are enough and the shorter
    // side is the one held in memory.
    let (outer, inner) = match before.len() >= after.len() {
        true => (before, after),
        false => (after, before),
    };

    let mut previous = vec![0usize; inner.len() + 1];
    let mut current = vec![0usize; inner.len() + 1];
    for outer_line in outer {
        for (j, inner_line) in inner.iter().enumerate() {
            current[j + 1] = match outer_line == inner_line {
                true => previous[j] + 1,
                false => current[j].max(previous[j + 1]),
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous.last().copied()
}

fn common_prefix(before: &[&str], after: &[&str]) -> usize {
    before.iter().zip(after).take_while(|(a, b)| a == b).count()
}

fn common_suffix(before: &[&str], after: &[&str]) -> usize {
    before
        .iter()
        .rev()
        .zip(after.iter().rev())
        .take_while(|(a, b)| a == b)
        .count()
}

/// One line of a [`Hunk`], and which side of the change it is on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Line {
    /// Unchanged: in the file before the change and after it.
    Context(String),
    /// In the file before the change and not after it.
    Removed(String),
    /// In the file after the change and not before it.
    Added(String),
}

impl Line {
    /// The line's text, without its terminator.
    pub fn text(&self) -> &str {
        match self {
            Self::Context(text) | Self::Removed(text) | Self::Added(text) => text,
        }
    }
}

/// A run of changed lines and the unchanged lines around them, placed in the
/// file: what a unified diff prints under one `@@` header.
///
/// Built only from what a backend reported about the change. A backend that
/// did not send the lines produces no hunk at all — never one rebuilt from the
/// file on disk, which by the time anyone reads it may hold a later edit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Hunk {
    old_start: u64,
    new_start: u64,
    lines: Vec<Line>,
}

impl Hunk {
    /// A hunk whose lines agree with the counts its header states, or `None`
    /// where they do not.
    ///
    /// `old_start` and `new_start` are one-based line numbers, as a unified
    /// diff writes them; `old_len` and `new_len` are how many lines the hunk
    /// says it spans on each side. A header and a body that disagree mean the
    /// report was cut or misread, and a diff drawn from it would number lines
    /// that are not where it says — so it is refused rather than drawn.
    pub fn checked(
        old_start: u64,
        old_len: u64,
        new_start: u64,
        new_len: u64,
        lines: Vec<Line>,
    ) -> Option<Self> {
        let hunk = Self {
            old_start,
            new_start,
            lines,
        };
        let (old, new) = hunk.spans();
        (old == old_len && new == new_len).then_some(hunk)
    }

    /// The whole of a file that did not exist before, as one hunk of added
    /// lines. `None` for an empty file, which has no line to show.
    pub fn created(content: &str) -> Option<Self> {
        let lines: Vec<Line> = content.lines().map(|l| Line::Added(l.to_owned())).collect();
        (!lines.is_empty()).then_some(Self {
            old_start: 0,
            new_start: 1,
            lines,
        })
    }

    /// The first line the hunk covers in the file before the change.
    pub fn old_start(&self) -> u64 {
        self.old_start
    }

    /// The first line the hunk covers in the file after the change.
    pub fn new_start(&self) -> u64 {
        self.new_start
    }

    /// The lines, in the order a unified diff prints them.
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// How many lines the hunk spans before the change and after it.
    fn spans(&self) -> (u64, u64) {
        self.lines
            .iter()
            .fold((0u64, 0u64), |(old, new), line| match line {
                Line::Context(_) => (old.saturating_add(1), new.saturating_add(1)),
                Line::Removed(_) => (old.saturating_add(1), new),
                Line::Added(_) => (old, new.saturating_add(1)),
            })
    }

    /// The hunk's two sides as texts, for [`lines_changed`].
    fn sides(&self) -> (String, String) {
        let mut before = String::new();
        let mut after = String::new();
        for line in &self.lines {
            let (to_before, to_after) = match line {
                Line::Context(_) => (true, true),
                Line::Removed(_) => (true, false),
                Line::Added(_) => (false, true),
            };
            for (wanted, side) in [(to_before, &mut before), (to_after, &mut after)] {
                if wanted {
                    side.push_str(line.text());
                    side.push('\n');
                }
            }
        }
        (before, after)
    }
}

/// Lines added and removed across `hunks`, as `(added, removed)`, counted
/// through [`lines_changed`] one hunk at a time so that a change reported as
/// hunks is counted by the same arithmetic as one reported as two texts.
///
/// `None` where there are no hunks, or where one of them is too large to
/// compare exactly.
pub fn hunks_changed(hunks: &[Hunk]) -> Option<(u64, u64)> {
    if hunks.is_empty() {
        return None;
    }
    hunks
        .iter()
        .try_fold((0u64, 0u64), |(added, removed), hunk| {
            let (before, after) = hunk.sides();
            let (a, r) = lines_changed(&before, &after)?;
            Some((added.saturating_add(a), removed.saturating_add(r)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(text: &str) -> Line {
        Line::Context(text.to_owned())
    }
    fn removed(text: &str) -> Line {
        Line::Removed(text.to_owned())
    }
    fn added(text: &str) -> Line {
        Line::Added(text.to_owned())
    }

    #[test]
    fn a_hunk_whose_lines_match_its_header_is_kept() {
        let hunk = Hunk::checked(
            1,
            2,
            1,
            2,
            vec![context("alpha"), removed("beta"), added("gamma")],
        )
        .expect("two lines each side, as the header says");
        assert_eq!(hunk.old_start(), 1);
        assert_eq!(hunk.new_start(), 1);
        assert_eq!(hunk.lines().len(), 3);
    }

    /// A body cut short would number every line after the cut wrongly.
    #[test]
    fn a_hunk_whose_lines_disagree_with_its_header_is_refused() {
        assert_eq!(
            Hunk::checked(
                1,
                3,
                1,
                2,
                vec![context("alpha"), removed("beta"), added("gamma")]
            ),
            None
        );
        assert_eq!(
            Hunk::checked(
                1,
                2,
                1,
                1,
                vec![context("alpha"), removed("beta"), added("gamma")]
            ),
            None
        );
    }

    #[test]
    fn a_created_file_is_one_hunk_of_added_lines_and_an_empty_one_is_none() {
        let hunk = Hunk::created("one\ntwo\n").expect("two lines");
        assert_eq!(hunk.old_start(), 0);
        assert_eq!(hunk.new_start(), 1);
        assert_eq!(hunk.lines(), &[added("one"), added("two")]);
        assert_eq!(Hunk::created(""), None);
    }

    /// The same edit counts the same whether the backend sent the two texts
    /// or the hunks, which is what lets a replacement made everywhere in a
    /// file be counted at all.
    #[test]
    fn hunks_are_counted_by_the_same_arithmetic_as_two_texts() {
        let hunks = [
            Hunk::checked(
                5,
                3,
                5,
                3,
                vec![context("a"), removed("old()"), added("new()"), context("b")],
            )
            .expect("consistent"),
            Hunk::checked(
                20,
                1,
                20,
                2,
                vec![removed("old()"), added("new()"), added("more()")],
            )
            .expect("consistent"),
        ];
        assert_eq!(hunks_changed(&hunks), Some((3, 2)));
        assert_eq!(hunks_changed(&[]), None);
    }

    #[test]
    fn a_line_replaced_by_two_reads_as_two_added_and_one_removed() {
        assert_eq!(lines_changed("gamma", "gamma one\ngamma two"), Some((2, 1)));
    }

    #[test]
    fn a_trailing_newline_terminates_the_last_line_rather_than_starting_one() {
        assert_eq!(lines_changed("a\nb\n", "a\nb"), Some((0, 0)));
        assert_eq!(lines_changed("", "one line\n"), Some((1, 0)));
        assert_eq!(lines_changed("one line", ""), Some((0, 1)));
    }

    #[test]
    fn unchanged_text_changes_nothing() {
        assert_eq!(lines_changed("a\nb\nc", "a\nb\nc"), Some((0, 0)));
        assert_eq!(lines_changed("", ""), Some((0, 0)));
    }

    /// The point of the longest common subsequence: a replacement that keeps
    /// lines in the middle is not counted as replacing all of them.
    #[test]
    fn lines_kept_in_the_middle_are_not_counted_as_changed() {
        assert_eq!(
            lines_changed("keep\ndrop\nkeep too", "keep\nkeep too"),
            Some((0, 1))
        );
        assert_eq!(
            lines_changed("one\ntwo\nthree", "one\ninserted\ntwo\nthree"),
            Some((1, 0))
        );
    }

    #[test]
    fn a_moved_line_is_one_removal_and_one_addition() {
        assert_eq!(lines_changed("a\nb\nc", "b\nc\na"), Some((1, 1)));
    }

    #[test]
    fn a_replacement_too_large_to_compare_exactly_is_refused_not_guessed() {
        let before: String = (0..3_000)
            .map(|i| format!("before {i}\n"))
            .collect::<String>();
        let after: String = (0..3_000)
            .map(|i| format!("after {i}\n"))
            .collect::<String>();
        assert_eq!(lines_changed(&before, &after), None);
    }

    /// The trim is what keeps a large file with a small edit inside the cap,
    /// so a big file is not reported as a change of unstated size.
    #[test]
    fn a_small_edit_inside_a_large_file_is_still_counted() {
        let before: String = (0..5_000).map(|i| format!("line {i}\n")).collect();
        let after = before.replace("line 2500\n", "line 2500\nline 2500b\n");
        assert_eq!(lines_changed(&before, &after), Some((1, 0)));
    }
}
