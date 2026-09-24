// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A file change drawn as the lines that changed, under the call that made it.
//!
//! Each row is a line number, a one-cell sign — `-`, `+`, or blank for an
//! unchanged line — and the line itself, on the diff's own background. The
//! two sides are numbered independently, as a unified diff numbers them: a
//! removed line carries its number in the file before the change, an added or
//! unchanged line its number after. The sign is what tells the rows apart,
//! so the diff still reads on a terminal with no colour.
//!
//! No syntax highlighting. A highlighter is a dependency the release binary's
//! size budget pays for and a grammar per language, and the sign and the
//! line's colour already say what changed; what a keyword is, the operator
//! can read.

use niobe_core::diff::{Hunk, Line as DiffLine};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{Change, Gate};
use crate::text;
use crate::theme::Theme;

/// How many unchanged lines a diff keeps on each side of a change.
///
/// Two, where `git` and the backends keep three: three lines are what a patch
/// needs to find its place in a file that has moved, and a reader needs only
/// enough to see where the change sits. Every line kept is a row of a pane
/// that also has to show the rest of the session. Runs of unchanged lines
/// longer than this, between two changes, are drawn as one `…` row.
pub const CONTEXT_LINES: usize = 2;

/// The most rows of lines one diff draws before it stops and says how many it
/// left out.
///
/// A file written from scratch is a diff of every line in it, and a transcript
/// that drew all of them would bury the session under one call. Twenty rows
/// hold every edit of ordinary size in full — a change and its context on
/// each side — and cap the one that is not at less than a screen.
pub const MAX_ROWS: usize = 20;

/// One row of a diff before it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row<'a> {
    /// A line, with the number the diff shows beside it.
    Line(u64, &'a DiffLine),
    /// Unchanged lines left out between two that are shown.
    Elided,
}

/// The rows `hunks` draw as: each line numbered, the unchanged lines more than
/// [`CONTEXT_LINES`] from a change dropped, and one [`Row::Elided`] wherever
/// lines were dropped between two that are kept — including between hunks,
/// which the backend already separated for that reason.
fn rows(hunks: &[Hunk]) -> Vec<Row<'_>> {
    let mut rows = Vec::new();
    for hunk in hunks {
        let lines = hunk.lines();
        let near = near_a_change(lines);
        let (mut old, mut new) = (hunk.old_start(), hunk.new_start());
        let mut dropped = !rows.is_empty();
        for (line, keep) in lines.iter().zip(near) {
            let number = match line {
                DiffLine::Context(_) => new,
                DiffLine::Removed(_) => old,
                DiffLine::Added(_) => new,
            };
            match line {
                DiffLine::Context(_) => {
                    old = old.saturating_add(1);
                    new = new.saturating_add(1);
                }
                DiffLine::Removed(_) => old = old.saturating_add(1),
                DiffLine::Added(_) => new = new.saturating_add(1),
            }
            if !keep {
                dropped = true;
                continue;
            }
            if dropped && !rows.is_empty() {
                rows.push(Row::Elided);
            }
            dropped = false;
            rows.push(Row::Line(number, line));
        }
    }
    rows
}

/// For each line, whether it is a change or within [`CONTEXT_LINES`] of one.
fn near_a_change(lines: &[DiffLine]) -> Vec<bool> {
    let changed: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !matches!(line, DiffLine::Context(_)))
        .map(|(at, _)| at)
        .collect();
    (0..lines.len())
        .map(|at| {
            changed
                .iter()
                .any(|&change| at.abs_diff(change) <= CONTEXT_LINES)
        })
        .collect()
}

/// A change's rows, fitted to `width` cells, and the row under them that says
/// how the call was let through.
pub(crate) fn lines(change: &Change, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let rows = rows(change.hunks());
    let number_width = rows
        .iter()
        .filter_map(|row| match row {
            Row::Line(number, _) => Some(number.to_string().len()),
            Row::Elided => None,
        })
        .max()
        .unwrap_or(1);

    let mut lines: Vec<Line<'static>> = rows
        .iter()
        .take(MAX_ROWS)
        .map(|row| draw_row(*row, number_width, width, theme))
        .collect();
    let left_out = rows.len().saturating_sub(MAX_ROWS);
    if left_out > 0 {
        let said = format!("… {left_out} more {}", plural(left_out, "row", "rows"));
        lines.push(Line::from(Span::styled(
            pad(&text::truncate(&said, width), width),
            Style::new().fg(theme.dim).bg(theme.diff_bg),
        )));
    }
    if let Some(gate) = change.gate() {
        lines.push(footer(gate, width, theme));
    }
    lines
}

/// One row: the number right-aligned in its column, the sign, and the line
/// cut to what is left, padded so the row's background reaches the edge.
///
/// A line too long for the pane is cut rather than wrapped, so a line of the
/// file is always one row and one number.
fn draw_row(row: Row<'_>, number_width: usize, width: usize, theme: &Theme) -> Line<'static> {
    let Row::Line(number, line) = row else {
        let gutter = " ".repeat(number_width + 1);
        return Line::from(vec![
            Span::styled(gutter, Style::new().bg(theme.diff_bg)),
            Span::styled(
                pad("…", width.saturating_sub(number_width + 1)),
                Style::new().fg(theme.dim).bg(theme.diff_bg),
            ),
        ]);
    };
    let (sign, number_colour, text_colour, bg) = match line {
        DiffLine::Context(_) => (" ", theme.dim, theme.fg, theme.diff_bg),
        DiffLine::Removed(_) => ("-", theme.del, theme.del, theme.del_bg),
        DiffLine::Added(_) => ("+", theme.add, theme.add, theme.add_bg),
    };
    let room = width.saturating_sub(number_width + 3);
    let shown = text::truncate(&expand_tabs(line.text()), room);
    Line::from(vec![
        Span::styled(
            format!("{number:>number_width$} "),
            Style::new().fg(number_colour).bg(bg),
        ),
        Span::styled(sign, Style::new().fg(number_colour).bg(bg).bold()),
        Span::styled(" ", Style::new().bg(bg)),
        Span::styled(pad(&shown, room), Style::new().fg(text_colour).bg(bg)),
    ])
    .style(Style::new().bg(bg))
}

/// `└ allowed by you`: who let the call through, in the words of what
/// actually happened.
fn footer(gate: Gate, width: usize, theme: &Theme) -> Line<'static> {
    let said = match gate {
        Gate::Operator => "allowed by you".to_owned(),
        Gate::OperatorAlways => "allowed by you, with a standing rule saved".to_owned(),
        Gate::Rule => "allowed by a standing rule".to_owned(),
        Gate::Unasked(mode) => format!("not asked · {mode} mode"),
    };
    Line::from(vec![
        Span::styled("└ ", Style::new().fg(theme.dim)),
        Span::styled(
            text::truncate(&said, width.saturating_sub(2)),
            Style::new().fg(theme.dim),
        ),
    ])
}

/// Tabs as four spaces: a tab's width is the terminal's to decide, and a row
/// whose width the shell cannot count is a row it cannot cut or pad.
fn expand_tabs(line: &str) -> String {
    line.replace('\t', "    ")
}

/// `text` padded with spaces to `width` cells.
fn pad(shown: &str, width: usize) -> String {
    let fill = width.saturating_sub(text::width(shown));
    format!("{shown}{}", " ".repeat(fill))
}

fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    match count {
        1 => one,
        _ => many,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::CLASSIC;
    use niobe_core::event::Mode;
    use ratatui::style::Color;

    /// The colour a row of the diff is drawn on.
    fn background(line: &Line<'_>) -> Option<Color> {
        line.spans.first().and_then(|span| span.style.bg)
    }

    fn context(text: &str) -> DiffLine {
        DiffLine::Context(text.to_owned())
    }
    fn removed(text: &str) -> DiffLine {
        DiffLine::Removed(text.to_owned())
    }
    fn added(text: &str) -> DiffLine {
        DiffLine::Added(text.to_owned())
    }

    fn hunk(old: u64, new: u64, lines: Vec<DiffLine>) -> Hunk {
        let olds = lines
            .iter()
            .filter(|l| !matches!(l, DiffLine::Added(_)))
            .count() as u64;
        let news = lines
            .iter()
            .filter(|l| !matches!(l, DiffLine::Removed(_)))
            .count() as u64;
        Hunk::checked(old, olds, new, news, lines).expect("built to agree with itself")
    }

    fn drawn(change: &Change, width: usize) -> Vec<String> {
        lines(change, width, &CLASSIC)
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn change(hunks: Vec<Hunk>, gate: Option<Gate>) -> Change {
        Change::new(hunks, gate)
    }

    /// The mock's own example: two lines replaced by five, numbered on each
    /// side as a unified diff numbers them.
    #[test]
    fn removed_and_added_lines_are_numbered_each_on_their_own_side() {
        let diff = change(
            vec![hunk(
                40,
                40,
                vec![
                    context("fn label_for() {"),
                    context("    let unsettled = 0;"),
                    removed("    old one"),
                    removed("    old two"),
                    added("    new one"),
                    added("    new two"),
                    added("    new three"),
                    context("}"),
                ],
            )],
            None,
        );

        let rows = drawn(&diff, 30);

        assert_eq!(
            rows,
            vec![
                "40   fn label_for() {         ",
                "41       let unsettled = 0;   ",
                "42 -     old one              ",
                "43 -     old two              ",
                "42 +     new one              ",
                "43 +     new two              ",
                "44 +     new three            ",
                "45   }                        ",
            ]
        );
    }

    #[test]
    fn unchanged_lines_far_from_a_change_are_one_elision_row() {
        let diff = change(
            vec![hunk(
                1,
                1,
                vec![
                    removed("a"),
                    added("A"),
                    context("1"),
                    context("2"),
                    context("3"),
                    context("4"),
                    context("5"),
                    removed("b"),
                    added("B"),
                ],
            )],
            None,
        );

        let rows = drawn(&diff, 12);

        assert_eq!(
            rows,
            vec![
                "1 - a       ",
                "1 + A       ",
                "2   1       ",
                "3   2       ",
                "  …         ",
                "5   4       ",
                "6   5       ",
                "7 - b       ",
                "7 + B       ",
            ]
        );
    }

    #[test]
    fn two_hunks_are_separated_by_an_elision_row_and_numbered_from_their_own_start() {
        let diff = change(
            vec![
                hunk(3, 3, vec![removed("x"), added("y")]),
                hunk(98, 98, vec![context("keep"), removed("p"), added("q")]),
            ],
            None,
        );

        let rows = drawn(&diff, 12);

        assert_eq!(
            rows,
            vec![
                " 3 - x      ",
                " 3 + y      ",
                "   …        ",
                "98   keep   ",
                "99 - p      ",
                "99 + q      ",
            ]
        );
    }

    /// A row keeps its number and its sign whatever the width; the line is
    /// what gives way, and it never wraps onto a row of its own.
    #[test]
    fn a_long_line_is_cut_and_keeps_one_number() {
        let diff = change(
            vec![hunk(7, 7, vec![added("a line far too long for the pane")])],
            None,
        );

        assert_eq!(drawn(&diff, 16), vec!["7 + a line far …"]);
    }

    #[test]
    fn every_row_has_a_sign_cell_of_one_column_even_where_it_is_blank() {
        let diff = change(
            vec![hunk(
                1,
                1,
                vec![context("same"), removed("\tgone"), added("new")],
            )],
            None,
        );

        for row in drawn(&diff, 20) {
            let sign = row
                .chars()
                .nth(2)
                .expect("a number, a space, then the sign");
            assert!([' ', '-', '+'].contains(&sign), "{row:?}");
            assert_eq!(text::width(&row), 20, "{row:?}");
        }
    }

    #[test]
    fn a_diff_longer_than_the_cap_says_how_many_rows_it_left_out() {
        let created = Hunk::created(&(1..=30).map(|n| format!("line {n}\n")).collect::<String>())
            .expect("thirty lines");

        let rows = drawn(&change(vec![created], None), 20);

        assert_eq!(rows.len(), MAX_ROWS + 1);
        assert_eq!(rows[0].trim_end(), " 1 + line 1");
        assert_eq!(rows[MAX_ROWS].trim_end(), "… 10 more rows");
    }

    #[test]
    fn the_footer_says_who_let_the_call_through() {
        let one = || vec![hunk(1, 1, vec![removed("a"), added("b")])];
        let footer = |gate| drawn(&change(one(), Some(gate)), 60).pop();

        assert_eq!(footer(Gate::Operator).as_deref(), Some("└ allowed by you"));
        assert_eq!(
            footer(Gate::OperatorAlways).as_deref(),
            Some("└ allowed by you, with a standing rule saved")
        );
        assert_eq!(
            footer(Gate::Rule).as_deref(),
            Some("└ allowed by a standing rule")
        );
        assert_eq!(
            footer(Gate::Unasked(Mode::Auto)).as_deref(),
            Some("└ not asked · auto mode")
        );
    }

    #[test]
    fn a_change_nobody_can_say_was_gated_has_no_footer() {
        let rows = drawn(&change(vec![hunk(1, 1, vec![added("b")])], None), 20);
        assert_eq!(rows, vec!["1 + b               "]);
    }

    #[test]
    fn the_diff_is_drawn_on_its_own_background_and_not_the_panes() {
        let diff = change(vec![hunk(1, 1, vec![context("a"), added("b")])], None);
        for line in lines(&diff, 20, &CLASSIC) {
            assert_eq!(background(&line), Some(CLASSIC.diff_bg));
            assert_ne!(background(&line), Some(CLASSIC.pane_bg));
        }
    }
}
