// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The line arithmetic every backend's file changes are counted with.
//!
//! A bridge knows what its CLI replaced with what; it does not get to decide
//! what a changed line is. Both bridges count through this module, so a
//! session cannot report one figure on one backend and another figure on the
//! other for the same edit.
//!
//! The counts are the ones `git diff --numstat` prints for the same
//! replacement: the minimal number of lines that have to be removed and added
//! to turn one text into the other, a line being its text and its terminator
//! both. That is the longest common subsequence of the two line sequences,
//! which is what a diff's own minimality means — so the pane's `+2 −1` is a
//! figure the operator can check against `git`.
//!
//! A [`Hunk`] is the other half: not how many lines changed but which, as the
//! backend reported them. It is kept here rather than in a bridge so that the
//! transcript draws one shape of diff whichever backend made the change.

use serde::{Deserialize, Serialize};

/// Lines added and removed by replacing `before` with `after`, as
/// `(added, removed)`, where the two are the whole of a file before and after.
///
/// A line is compared with its terminator, as `git` compares it: a line whose
/// `\r\n` became `\n` is a line removed and a line added, and so is a last
/// line that gained or lost its newline. A trailing newline still ends the
/// last line rather than starting another, so `"a\nb\n"` and `"a\nb"` are
/// both two lines.
///
/// `None` where either text is binary by git's reading — a nul in its first
/// 8000 bytes — since git states no line counts for one either, and where the
/// two differ too much to compare within a redraw. A caller reports either as
/// a change of an unstated size — an estimate here would be a number nobody
/// could check.
pub fn lines_changed(before: &str, after: &str) -> Option<(u64, u64)> {
    if is_binary(before) || is_binary(after) {
        return None;
    }
    let before: Vec<&str> = before.split_inclusive('\n').collect();
    let after: Vec<&str> = after.split_inclusive('\n').collect();

    // The lines that match at either end are common to both texts whatever the
    // middle does, so trimming them is free and leaves the diff with only the
    // part that actually differs.
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

/// Lines added and removed by replacing `old` with `new` somewhere inside a
/// file, as `(added, removed)`: what an edit that names the text it replaced
/// changes.
///
/// Whatever follows the replaced text in the file follows both sides alike, so
/// the last line of each is continued by the same text rather than ended where
/// the argument ends. It is counted as a line the file ends with a newline,
/// which is exact wherever the replacement stops at the end of a line — the
/// usual case, since an edit is usually of whole lines.
pub fn replacement_changed(old: &str, new: &str) -> Option<(u64, u64)> {
    lines_changed(&format!("{old}\n"), &format!("{new}\n"))
}

/// How far into a text `git` looks for a nul before calling it binary.
const BINARY_PROBE: usize = 8_000;

fn is_binary(text: &str) -> bool {
    text.as_bytes()
        .iter()
        .take(BINARY_PROBE)
        .any(|&byte| byte == 0)
}

/// The most steps the diff may take before the change is refused.
///
/// A step is one diagonal tried or one matching line followed, so the budget
/// grows with how much the texts differ times how long they are, not with the
/// size of the file alone: a two-line change in a fifty-thousand-line file is
/// a few hundred thousand steps, and seven hundred lines replaced by seven
/// hundred others are just inside the million. A million is under ten
/// milliseconds in a release build, which a replacement made between two
/// frames can afford. Above it the change is reported as one whose size the backend did not
/// state, because a redraw that waits on a diff is a redraw the operator
/// watches stall.
const MAX_STEPS: usize = 1_000_000;

/// How many lines the two sequences have in common, in order, or `None`
/// where finding out would take more than [`MAX_STEPS`].
///
/// Myers' greedy diff: it finds the fewest lines that must be removed and
/// added, `d`, by trying every edit script of length 0, 1, 2 … in turn, so
/// its work grows with `d` rather than with the product of the lengths. The
/// longest common subsequence is what the two lengths share once those `d`
/// lines are set aside.
fn common_lines(before: &[&str], after: &[&str]) -> Option<usize> {
    let (n, m) = (before.len(), after.len());
    if n == 0 || m == 0 {
        return Some(0);
    }

    // Diagonal `k` holds the points where the line of `before` minus the line
    // of `after` is `k - offset`; the shift keeps it an index. `furthest[k]`
    // is how far along `before` the best script of the current length
    // reaches on that diagonal.
    let offset = n + m;
    let mut furthest = vec![0usize; 2 * offset + 2];
    let mut steps = 0usize;
    for d in 0..=offset {
        for k in (offset - d..=offset + d).step_by(2) {
            let down = k == offset - d || (k != offset + d && furthest[k - 1] < furthest[k + 1]);
            let mut x = match down {
                true => furthest[k + 1],
                false => furthest[k - 1] + 1,
            };
            let mut y = (x + offset).checked_sub(k)?;
            while x < n && y < m && before[x] == after[y] {
                x += 1;
                y += 1;
                steps += 1;
            }
            furthest[k] = x;
            if x >= n && y >= m {
                return Some((n + m - d) / 2);
            }
            steps += 1;
            if steps > MAX_STEPS {
                return None;
            }
        }
    }
    None
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
}

/// Lines added and removed across `hunks`, as `(added, removed)`.
///
/// A hunk is already a diff, so its own markers are the count: a line the
/// backend removed and added again differs in something the hunk's text does
/// not carry — its terminator, or the newline a last line gained or lost —
/// and that is a change `git` counts too.
///
/// `None` where there are no hunks.
pub fn hunks_changed(hunks: &[Hunk]) -> Option<(u64, u64)> {
    if hunks.is_empty() {
        return None;
    }
    Some(hunks.iter().flat_map(Hunk::lines).fold(
        (0u64, 0u64),
        |(added, removed), line| match line {
            Line::Context(_) => (added, removed),
            Line::Removed(_) => (added, removed.saturating_add(1)),
            Line::Added(_) => (added.saturating_add(1), removed),
        },
    ))
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
        assert_eq!(lines_changed("", "one line\n"), Some((1, 0)));
        assert_eq!(lines_changed("one line", ""), Some((0, 1)));
        assert_eq!(lines_changed("a\nb\n", "a\nb\nc\n"), Some((1, 0)));
    }

    /// Each row was measured with `git diff --no-index --numstat` on two files
    /// holding exactly these bytes; `None` is the `-  -` git prints for a
    /// binary file.
    #[test]
    fn line_ending_final_newline_and_binary_changes_count_as_git_counts_them() {
        let cases = [
            ("crlf to lf", "a\r\nb\r\nc\r\n", "a\nb\nc\n", Some((3, 3))),
            ("lf to crlf", "a\nb\n", "a\r\nb\r\n", Some((2, 2))),
            ("final newline dropped", "a\nb\n", "a\nb", Some((1, 1))),
            ("final newline added", "a\nb", "a\nb\n", Some((1, 1))),
            ("content with a nul", "a\nb\n", "a\n\0b\n", None),
        ];
        for (case, before, after, git) in cases {
            assert_eq!(lines_changed(before, after), git, "{case}");
        }
    }

    /// A nul past the first 8000 bytes is not where git looks, so the file is
    /// still text to it.
    #[test]
    fn a_nul_past_where_git_looks_leaves_the_file_text() {
        let before = format!("{}\n", "x".repeat(8_000));
        let after = format!("{before}\0\n");
        assert_eq!(lines_changed(&before, &after), Some((1, 0)));
    }

    /// Two changes far apart leave the whole file between them to compare, and
    /// that is still cheap when the files are mostly the same.
    #[test]
    fn two_changes_far_apart_in_a_large_file_are_counted() {
        let before: String = (0..50_000).map(|i| format!("line {i}\n")).collect();
        let after = before.replacen("line 0\n", "line zero\n", 1).replacen(
            "line 49999\n",
            "line last\n",
            1,
        );
        assert_eq!(lines_changed(&before, &after), Some((2, 2)));
    }

    /// An edit's two texts sit in a file whose text after them is the same on
    /// both sides, so their last lines are not told apart by a terminator the
    /// file supplies.
    #[test]
    fn a_replacement_inside_a_file_reads_its_last_lines_as_continued_by_the_file() {
        assert_eq!(replacement_changed("a\nb", "a\nb\nc"), Some((1, 0)));
        assert_eq!(
            replacement_changed("gamma", "gamma one\ngamma two"),
            Some((2, 1))
        );
        assert_eq!(replacement_changed("drop me\n", ""), Some((0, 1)));
        assert_eq!(replacement_changed("a\r\nb", "a\nb"), Some((1, 1)));
        assert_eq!(replacement_changed("a\n\0", "a\n"), None);
    }

    /// The patch's own markers are the diff: a line removed and added again
    /// with only its terminator changed is a change git counts.
    #[test]
    fn a_hunk_that_rewrites_a_line_unchanged_but_for_its_end_counts_it() {
        let hunk = Hunk::checked(1, 2, 1, 2, vec![context("a"), removed("b"), added("b")])
            .expect("consistent");
        assert_eq!(hunks_changed(&[hunk]), Some((1, 1)));
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

    /// The longest common subsequence by the whole table, which is too slow
    /// to ship and too plain to be wrong.
    fn common_by_table(before: &[&str], after: &[&str]) -> usize {
        let mut table = vec![vec![0usize; after.len() + 1]; before.len() + 1];
        for (i, b) in before.iter().enumerate() {
            for (j, a) in after.iter().enumerate() {
                table[i + 1][j + 1] = match b == a {
                    true => table[i][j] + 1,
                    false => table[i][j + 1].max(table[i + 1][j]),
                };
            }
        }
        table[before.len()][after.len()]
    }

    /// Every pair of short sequences over three lines, which covers each way
    /// a script can run into the edge of the grid.
    #[test]
    fn the_diff_finds_the_same_common_lines_as_the_whole_table() {
        let alphabet = ["a", "b", "c"];
        let sequences: Vec<Vec<&str>> = (0..=4u32)
            .flat_map(|len| {
                (0..3usize.pow(len)).map(move |mut code| {
                    (0..len)
                        .map(|_| {
                            let line = alphabet[code % 3];
                            code /= 3;
                            line
                        })
                        .collect()
                })
            })
            .collect();
        for before in &sequences {
            for after in &sequences {
                assert_eq!(
                    common_lines(before, after),
                    Some(common_by_table(before, after)),
                    "{before:?} against {after:?}"
                );
            }
        }
    }

    #[test]
    fn a_moved_line_is_one_removal_and_one_addition() {
        assert_eq!(lines_changed("a\nb\nc\n", "b\nc\na\n"), Some((1, 1)));
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
