// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A tool call in the transcript, drawn as a row of a table: its glyph, the
//! tool's name in a column of fixed width, what it does, and on the right what
//! it cost.
//!
//! The name column is fixed so that the rows line up and the eye can run down
//! the costs; what the call does is the column that gives way first, because
//! it is the one the operator can find again in the diff or the Changes pane.
//! The cost is never cut short of its figure.
//!
//! What the cost column says depends on what the backend reported about the
//! call, not on which tool it was: the lines a change added and removed, the
//! status a command exited with, and otherwise the bytes it returned — which
//! every finished call has. Each carries how long it ran where this shell's
//! clock saw both ends. A call that did not succeed says so instead, and draws
//! the backend's reason for it under the row, or that it gave none.
//!
//! A call that ran the tests says under its row what the run reported: its
//! counts where its output held the whole run, and otherwise that the result
//! was not read — never a number the output did not give.
//!
//! A run of calls to the same tool is one group: a row with the run's summed
//! figures, and a row for each call under it unless the operator has folded
//! the runs away.

use std::time::Duration;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::app::{Call, Entry, human_bytes};
use crate::text;
use crate::theme::Theme;
use crate::ui::count;
use niobe_core::event::ToolOutcome;
use niobe_core::session::TestRunRecord;

/// How much of a tool-call entry the operator has asked to see: the two
/// switches that hold for the whole transcript.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(crate) struct Detail {
    /// Every run of calls is drawn as its group row alone.
    pub(crate) folded: bool,
    /// Every diff is drawn whole rather than cut at
    /// [`crate::hunks::MAX_ROWS`].
    pub(crate) diffs_open: bool,
}

/// The glyph and the space after it, which every row of the transcript starts
/// with.
const GUTTER: usize = 2;

/// How wide the tool's name is drawn, so that the columns after it line up
/// down the transcript: wide enough for `Notion·query-data-sources`, the
/// longest name a common MCP server gives a tool.
const NAME_COLUMN: usize = 26;

/// The space between one column and the next.
const GAP: usize = 2;

/// Where a group's rows hang from.
const BRANCH: &str = "├ ";
const LAST_BRANCH: &str = "└ ";
const STEM: &str = "│ ";

/// The rows a tool-call entry is drawn as, `width` cells wide, the blank line
/// after it included.
pub(crate) fn lines(
    entry: &Entry,
    width: usize,
    detail: Detail,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let colour = entry.kind.colour(theme);
    let mut lines = match entry.calls.as_slice() {
        [call] => single(&entry.head, call, width, detail, colour, theme),
        calls => group(&entry.head, calls, width, detail, colour, theme),
    };
    lines.push(Line::from(""));
    lines
}

/// A call on its own: its row, then the reason it failed or the lines it
/// changed.
fn single(
    head: &str,
    call: &Call,
    width: usize,
    detail: Detail,
    colour: Color,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let glyph = match call.failed() {
        true => "✗",
        false => "⚙",
    };
    let mut lines = vec![row(
        glyph,
        head.to_owned(),
        &call.what,
        result(call, theme),
        width,
        colour,
        theme,
    )];
    lines.extend(under(call, " ".repeat(GUTTER), width, detail, theme));
    lines
}

/// A run of calls: the group's row, then the calls under it unless the runs
/// are folded.
fn group(
    head: &str,
    calls: &[Call],
    width: usize,
    detail: Detail,
    colour: Color,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let glyph = match detail.folded {
        true => "▸",
        false => "▾",
    };
    let what = calls
        .iter()
        .map(|call| call.what.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let name = format!("{head} ×{}", calls.len());
    let mut lines = vec![row(
        glyph,
        name,
        &what,
        group_result(calls, theme),
        width,
        colour,
        theme,
    )];
    if detail.folded {
        return lines;
    }

    let indent = " ".repeat(GUTTER);
    for (at, call) in calls.iter().enumerate() {
        let last = at + 1 == calls.len();
        let branch = if last { LAST_BRANCH } else { BRANCH };
        let stem = if last { "  " } else { STEM };
        lines.push(child(branch, call, width, theme));
        lines.extend(under(call, format!("{indent}{stem}"), width, detail, theme));
    }
    lines
}

/// One call of a group: hung from the group's row, what it does, and its own
/// cost on the right.
fn child(branch: &str, call: &Call, width: usize, theme: &Theme) -> Line<'static> {
    let colour = match call.failed() {
        true => theme.del,
        false => theme.fg,
    };
    let lead = " ".repeat(GUTTER);
    let result = result(call, theme);
    let room = width
        .saturating_sub(GUTTER + text::width(branch) + GAP)
        .saturating_sub(spans_width(&result));
    let what = text::truncate(&call.what, room);
    let gap = room.saturating_sub(text::width(&what)) + GAP;

    let mut spans = vec![
        Span::raw(lead),
        Span::styled(branch.to_owned(), Style::new().fg(theme.dim)),
        Span::styled(what, Style::new().fg(colour)),
        Span::raw(" ".repeat(gap)),
    ];
    spans.extend(result);
    Line::from(spans)
}

/// What is drawn under a call's row: what its test run reported, why it
/// failed, or the lines it changed, each line led by `lead`.
///
/// A test run that is known to have failed says so in place of the reason
/// the call failed, which is only the status the row already shows. One that
/// failed without its tests failing — a build that did not compile — keeps
/// the reason, which is the part that says what went wrong.
fn under(
    call: &Call,
    lead: String,
    width: usize,
    detail: Detail,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let room = width.saturating_sub(text::width(&lead));
    let tested = call
        .tested
        .filter(|run| !call.failed() || run.counts.is_some() || run.failed);
    let body = match (&call.printed, tested, call.failed(), &call.change) {
        (Some(printed), _, _, _) => {
            let mut lines: Vec<_> = tested
                .map(|run| test_line(&run, room, theme))
                .into_iter()
                .collect();
            lines.extend(printed_lines(call, printed, room, theme));
            lines
        }
        (None, Some(run), _, _) => vec![test_line(&run, room, theme)],
        (None, None, true, _) => vec![reason(call, room, theme)],
        (None, None, false, Some(change)) => {
            crate::hunks::lines(change, room, detail.diffs_open, theme)
        }
        (None, None, false, None) => Vec::new(),
    };
    body.into_iter()
        .map(|line| {
            let mut spans = vec![Span::raw(lead.clone())];
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect()
}

/// What a command the operator ran printed, the last lines of it, under its
/// row; and above them why it ended without an exit of its own, where it did.
///
/// A command that failed by exiting non-zero says so in its row, and what it
/// printed is the reason — so no reason line is made up for it.
fn printed_lines(
    call: &Call,
    printed: &crate::app::Printed,
    room: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let dim = Style::new().fg(theme.dim);
    let mut lines = Vec::new();
    if call.error.is_some() {
        lines.push(reason(call, room, theme));
    }
    if printed.above > 0 {
        lines.push(Line::from(Span::styled(
            format!("… {} lines above", printed.above),
            dim.italic(),
        )));
    }
    lines.extend(printed.tail.iter().map(|line| {
        Line::from(Span::styled(
            text::truncate(line, room),
            Style::new().fg(theme.fg),
        ))
    }));
    if lines.is_empty() && !call.running() {
        lines.push(Line::from(Span::styled("printed nothing", dim.italic())));
    }
    lines
}

/// What a test run reported, hung under its call: `637 passed · 0 failed`,
/// with the failures in the failure colour where there are any.
///
/// Where the room runs out, the ignored go first and then the passed: the
/// failures are what the line is for. A run whose counts were not read says
/// that, and whether it is still known to have failed.
fn test_line(run: &TestRunRecord, room: usize, theme: &Theme) -> Line<'static> {
    let dim = Style::new().fg(theme.dim);
    let mut figures = match run.counts {
        Some(counts) => {
            let (passed, failed) = match counts.failing() {
                true => (Style::new().fg(theme.fg), theme_del(theme).bold()),
                false => (Style::new().fg(theme.add), dim),
            };
            let mut figures = vec![
                (1, Span::styled(format!("{} passed", counts.passed), passed)),
                (0, Span::styled(format!("{} failed", counts.failed), failed)),
            ];
            if counts.ignored > 0 {
                figures.push((2, Span::styled(format!("{} ignored", counts.ignored), dim)));
            }
            figures
        }
        None if run.failed => vec![
            (0, Span::styled("tests failed", theme_del(theme).bold())),
            (1, Span::styled("counts not read", dim)),
        ],
        None => vec![(0, Span::styled("test result not read", dim.italic()))],
    };
    let room = room.saturating_sub(text::width(LAST_BRANCH));
    while figures.len() > 1 && figures_width(&figures) > room {
        if let Some(least) = (0..figures.len()).max_by_key(|&at| figures[at].0) {
            figures.remove(least);
        }
    }
    let mut spans = vec![Span::styled(LAST_BRANCH, dim)];
    for (at, (_, figure)) in figures.into_iter().enumerate() {
        if at > 0 {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(figure);
    }
    Line::from(spans)
}

/// How wide a row of figures is drawn, with the separators between them.
fn figures_width(figures: &[(u8, Span<'static>)]) -> usize {
    let separators = figures.len().saturating_sub(1) * text::width(" · ");
    figures
        .iter()
        .map(|(_, span)| text::width(&span.content))
        .sum::<usize>()
        + separators
}

/// The first line of the backend's reason for a call that did not succeed,
/// or that it gave none.
fn reason(call: &Call, room: usize, theme: &Theme) -> Line<'static> {
    let (said, style) = match call.error.as_deref().and_then(first_line) {
        Some(said) => (said.to_owned(), Style::new().fg(theme.fg)),
        None => (
            match call.outcome {
                Some(ToolOutcome::Denied) => "not allowed to run; no reason was given",
                _ => "the call failed and the backend said nothing about why",
            }
            .to_owned(),
            Style::new().fg(theme.dim).italic(),
        ),
    };
    let room = room.saturating_sub(text::width(LAST_BRANCH));
    Line::from(vec![
        Span::styled(LAST_BRANCH, Style::new().fg(theme.dim)),
        Span::styled(text::truncate(&said, room), style),
    ])
}

fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

/// The row itself: the glyph, the name in its column, what the call does in
/// whatever is left, and the cost on the right.
///
/// What the call does gives way first. Only on a pane too narrow for the name
/// column and the cost together does the name give way, and the cost never
/// does.
fn row(
    glyph: &str,
    name: String,
    what: &str,
    result: Vec<Span<'static>>,
    width: usize,
    colour: Color,
    theme: &Theme,
) -> Line<'static> {
    let cost = spans_width(&result);
    let room = width.saturating_sub(GUTTER + cost + GAP);
    let name_column = NAME_COLUMN.min(room);
    let name = text::truncate(&name, name_column);
    let what_room = room.saturating_sub(name_column + 1);
    let what = text::truncate(what, what_room);
    let gap = room
        .saturating_sub(name_column + 1 + text::width(&what))
        .saturating_add(GAP);

    let mut spans = vec![
        Span::styled(format!("{glyph} "), Style::new().fg(colour).bold()),
        Span::styled(
            format!("{name:<name_column$}"),
            Style::new().fg(colour).bold(),
        ),
        Span::raw(" "),
        Span::styled(what, Style::new().fg(theme.dim)),
        Span::raw(" ".repeat(gap)),
    ];
    spans.extend(result);
    Line::from(spans)
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| text::width(&span.content)).sum()
}

/// What one call cost, as the backend and the clock reported it.
fn result(call: &Call, theme: &Theme) -> Vec<Span<'static>> {
    let dim = Style::new().fg(theme.dim);
    let Some(outcome) = call.outcome else {
        return vec![Span::styled("running", dim)];
    };
    let mut spans = match (outcome, call.exit_code, call.lines) {
        (ToolOutcome::Denied, _, _) => return vec![Span::styled("denied", theme_del(theme))],
        (ToolOutcome::Failed, Some(status), _) => {
            vec![Span::styled(format!("exit {status}"), theme_del(theme))]
        }
        (ToolOutcome::Failed, None, _) => vec![Span::styled("failed", theme_del(theme))],
        (ToolOutcome::Ok, _, Some((added, removed))) => diffstat(
            (added.unwrap_or(0), added.is_some()),
            (removed.unwrap_or(0), removed.is_some()),
            theme,
        ),
        (ToolOutcome::Ok, Some(status), None) => vec![Span::styled(format!("exit {status}"), dim)],
        (ToolOutcome::Ok, None, None) => {
            vec![Span::styled(human_bytes(call.bytes.unwrap_or(0)), dim)]
        }
    };
    if let Some(took) = call.took {
        spans.push(Span::styled(format!(" · {}", human_duration(took)), dim));
    }
    spans
}

/// What a run of calls cost, summed: the lines its changes added and removed
/// where every call that succeeded changed a file, and otherwise the bytes
/// every call returned; how many failed; and how long they ran.
///
/// A sum is marked `≥` where one of the calls in it had no figure to add, the
/// same way the Changes pane marks a file some call did not count — and a
/// call that has not finished has none yet.
fn group_result(calls: &[Call], theme: &Theme) -> Vec<Span<'static>> {
    let dim = Style::new().fg(theme.dim);
    if calls.iter().any(Call::running) {
        return vec![Span::styled("running", dim)];
    }
    let succeeded: Vec<&Call> = calls.iter().filter(|call| !call.failed()).collect();
    let failed = calls.len() - succeeded.len();

    let mut spans = match succeeded
        .iter()
        .map(|call| call.lines)
        .collect::<Option<Vec<_>>>()
    {
        Some(lines) if !lines.is_empty() => diffstat(
            summed(lines.iter().map(|(added, _)| *added)),
            summed(lines.iter().map(|(_, removed)| *removed)),
            theme,
        ),
        _ => {
            let bytes = calls.iter().fold(0u64, |sum, call| {
                sum.saturating_add(call.bytes.unwrap_or(0))
            });
            vec![Span::styled(human_bytes(bytes), dim)]
        }
    };
    if failed > 0 {
        let said = match failed == calls.len() {
            true => format!("failed ×{failed}"),
            false => format!("{failed} failed"),
        };
        match succeeded.is_empty() {
            true => spans = vec![Span::styled(said, theme_del(theme))],
            false => spans.push(Span::styled(format!(" · {said}"), theme_del(theme))),
        }
    }
    if let Some(took) = summed_time(calls) {
        spans.push(Span::styled(format!(" · {took}"), dim));
    }
    spans
}

/// One side of a run's line counts, summed, and whether every call stated
/// it.
fn summed(sides: impl Iterator<Item = Option<u64>>) -> (u64, bool) {
    sides.fold((0, true), |(sum, stated), side| {
        (
            sum.saturating_add(side.unwrap_or(0)),
            stated && side.is_some(),
        )
    })
}

/// How long a run of calls ran, all told: `≥` where the clock missed an end
/// of one of them, and `None` where it missed every one.
fn summed_time(calls: &[Call]) -> Option<String> {
    let timed: Vec<Duration> = calls.iter().filter_map(|call| call.took).collect();
    if timed.is_empty() {
        return None;
    }
    let total = timed.iter().sum();
    Some(match timed.len() == calls.len() {
        true => human_duration(total),
        false => format!("≥{}", human_duration(total)),
    })
}

/// `+8 −6`, each side in its colour, with the Changes pane's marks for a side
/// some call did not state.
fn diffstat(added: (u64, bool), removed: (u64, bool), theme: &Theme) -> Vec<Span<'static>> {
    vec![
        Span::styled(count('+', added.0, added.1), Style::new().fg(theme.add)),
        Span::raw(" "),
        Span::styled(count('−', removed.0, removed.1), Style::new().fg(theme.del)),
    ]
}

fn theme_del(theme: &Theme) -> Style {
    Style::new().fg(theme.del)
}

/// A duration, short enough for the cost column: tenths of a second under
/// ten seconds, whole seconds under a minute, minutes and seconds after.
fn human_duration(took: Duration) -> String {
    match took.as_secs() {
        0..10 => format!("{:.1}s", took.as_secs_f64()),
        10..60 => format!("{}s", took.as_secs()),
        secs => format!("{}m{:02}s", secs / 60, secs % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Repo};
    use crate::clock::Stamp;
    use niobe_core::event::Event;
    use std::time::{Duration, SystemTime};

    fn at(ms: u64) -> Stamp {
        Stamp::new(SystemTime::UNIX_EPOCH + Duration::from_millis(ms), None)
    }

    fn start(id: &str, name: &str) -> Event {
        Event::ToolCallStart {
            id: id.into(),
            name: name.to_owned(),
            input: String::new(),
            summary: Some(id.to_owned()),
        }
    }

    fn end(id: &str, name: &str, outcome: ToolOutcome, error: Option<&str>) -> Event {
        Event::ToolCallEnd {
            id: id.into(),
            name: name.to_owned(),
            input: String::new(),
            output: String::new(),
            bytes: 1_024,
            outcome,
            summary: Some(id.to_owned()),
            exit_code: None,
            error: error.map(str::to_owned),
        }
    }

    /// The first entry of `app` as the transcript draws it, one string a row.
    fn drawn(app: &App, folded: bool) -> Vec<String> {
        let detail = Detail {
            folded,
            diffs_open: false,
        };
        lines(&app.entries()[0], 80, detail, &Theme::default())
            .into_iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    fn app() -> App {
        App::new(Repo::default())
    }

    #[test]
    fn a_failure_with_no_reason_says_the_backend_gave_none() {
        let mut app = app();
        app.apply(&start("t1", "Read"));
        app.apply(&end("t1", "Read", ToolOutcome::Failed, None));

        let rows = drawn(&app, false);
        assert!(rows[0].starts_with("✗ Read"), "{rows:?}");
        assert!(rows[0].ends_with("failed"), "{rows:?}");
        assert_eq!(
            rows[1].trim(),
            "└ the call failed and the backend said nothing about why"
        );
    }

    #[test]
    fn a_failure_shows_the_first_line_of_its_reason() {
        let mut app = app();
        app.apply(&start("t1", "Read"));
        app.apply(&end(
            "t1",
            "Read",
            ToolOutcome::Failed,
            Some("\nFile does not exist.\nsecond line"),
        ));

        assert_eq!(drawn(&app, false)[1].trim(), "└ File does not exist.");
    }

    #[test]
    fn a_run_sums_its_figures_and_marks_a_time_one_call_has_none_of() {
        let mut app = app();
        app.apply_at(&start("t1", "Read"), at(0));
        app.apply_at(&start("t2", "Read"), at(0));
        app.apply_at(&end("t1", "Read", ToolOutcome::Ok, None), at(1_500));
        // The second end came with no clock behind it.
        app.apply(&end("t2", "Read", ToolOutcome::Failed, Some("gone")));

        let rows = drawn(&app, false);
        assert!(rows[0].starts_with("▾ Read ×2"), "{rows:?}");
        assert!(rows[0].ends_with("2.0 kB · 1 failed · ≥1.5s"), "{rows:?}");
        assert!(rows[1].ends_with("1.0 kB · 1.5s"), "{rows:?}");
        assert!(rows[2].ends_with("failed"), "{rows:?}");
        assert_eq!(
            rows[3].trim(),
            "└ gone",
            "the failed call's reason hangs under it"
        );
        assert_eq!(drawn(&app, true).len(), 2, "folded, the group row alone");
    }

    #[test]
    fn a_change_whose_removal_went_unstated_reads_as_a_dash_and_its_sum_as_a_floor() {
        let mut app = app();
        for (id, removed) in [("t1", None), ("t2", Some(3))] {
            app.apply(&start(id, "Write"));
            app.apply(&end(id, "Write", ToolOutcome::Ok, None));
            app.apply(&Event::FileChange {
                path: format!("{id}.rs"),
                added: Some(4),
                removed,
                hunks: Vec::new(),
            });
        }

        let rows = drawn(&app, false);
        assert!(rows[0].ends_with("+8 −≥3"), "{rows:?}");
        assert!(rows[1].ends_with("+4 —"), "{rows:?}");
        assert!(rows[2].ends_with("+4 −3"), "{rows:?}");
    }

    #[test]
    fn a_call_still_running_says_so_in_place_of_a_cost() {
        let mut app = app();
        app.apply(&start("t1", "Bash"));

        assert!(drawn(&app, false)[0].ends_with("running"));
    }

    /// A `cargo test` call that ended with `status`, and the run it reported.
    fn tested(app: &mut App, status: i32, counts: Option<niobe_core::TestCounts>, failed: bool) {
        let outcome = match status {
            0 => ToolOutcome::Ok,
            _ => ToolOutcome::Failed,
        };
        app.apply(&start("t1", "Bash"));
        app.apply(&Event::ToolCallEnd {
            id: "t1".into(),
            name: "Bash".to_owned(),
            input: String::new(),
            output: String::new(),
            bytes: 1_024,
            outcome,
            summary: Some("cargo test".to_owned()),
            exit_code: Some(status),
            error: (status != 0).then(|| format!("Exit code {status}")),
        });
        app.apply(&Event::TestRun {
            id: "t1".into(),
            counts,
            exit_code: Some(status),
            failed,
        });
    }

    fn counts(passed: u64, failed: u64, ignored: u64) -> Option<niobe_core::TestCounts> {
        Some(niobe_core::TestCounts {
            passed,
            failed,
            ignored,
            suites: 3,
        })
    }

    #[test]
    fn a_test_run_whose_result_was_read_shows_its_counts_under_its_call() {
        let mut app = app();
        tested(&mut app, 0, counts(637, 0, 2), false);

        let rows = drawn(&app, false);
        assert!(rows[0].starts_with("⚙ Bash"), "{rows:?}");
        assert_eq!(rows[1].trim(), "└ 637 passed · 0 failed · 2 ignored");
    }

    #[test]
    fn a_failing_test_run_shows_its_failures_in_the_failure_colour_in_place_of_its_status() {
        let mut app = app();
        tested(&mut app, 101, counts(630, 7, 0), false);

        let theme = Theme::default();
        let detail = Detail::default();
        let lines = lines(&app.entries()[0], 80, detail, &theme);
        let under: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(under.trim(), "└ 630 passed · 7 failed");
        let failed = lines[1]
            .spans
            .iter()
            .find(|span| span.content == "7 failed")
            .expect("the failures are a span of their own");
        assert_eq!(failed.style.fg, Some(theme.del));
        assert!(!under.contains("Exit code"), "the counts say why it failed");
    }

    #[test]
    fn a_test_run_whose_result_was_not_read_says_so_and_gives_no_number() {
        let mut app = app();
        tested(&mut app, 0, None, false);

        let rows = drawn(&app, false);
        assert_eq!(rows[1].trim(), "└ test result not read");
        assert!(
            !rows[1].chars().any(|c| c.is_ascii_digit()),
            "a filtered run has no count: {rows:?}"
        );
    }

    #[test]
    fn a_test_run_known_to_have_failed_uncounted_says_so_without_a_count() {
        let mut app = app();
        tested(&mut app, 101, None, true);

        assert_eq!(
            drawn(&app, false)[1].trim(),
            "└ tests failed · counts not read"
        );
    }

    #[test]
    fn a_test_run_whose_build_failed_keeps_the_reason_the_call_failed() {
        let mut app = app();
        tested(&mut app, 101, None, false);

        assert_eq!(drawn(&app, false)[1].trim(), "└ Exit code 101");
    }

    #[test]
    fn a_test_run_on_a_narrow_pane_gives_up_whole_figures_and_keeps_its_failures() {
        let mut app = app();
        tested(&mut app, 101, counts(630, 7, 1), false);

        let line =
            lines(&app.entries()[0], 28, Detail::default(), &Theme::default()).swap_remove(1);
        let under: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(under.trim(), "└ 630 passed · 7 failed");
    }

    #[test]
    fn a_duration_reads_in_the_unit_that_says_anything() {
        assert_eq!(human_duration(Duration::from_millis(300)), "0.3s");
        assert_eq!(human_duration(Duration::from_millis(9_940)), "9.9s");
        assert_eq!(human_duration(Duration::from_millis(12_600)), "12s");
        assert_eq!(human_duration(Duration::from_secs(125)), "2m05s");
    }
}
