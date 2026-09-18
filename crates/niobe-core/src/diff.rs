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

#[cfg(test)]
mod tests {
    use super::*;

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
