// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A tool call in the transcript, drawn as a row of a table: its glyph, the
//! tool's name, the tag of the sub-agent that made it, what it does, and on
//! the right what it cost. A sub-agent's words are a row of the same table.
//!
//! The name and tag columns are as wide as the transcript's widest, so the
//! rows line up and the eye can run down what each call does and what it
//! cost; rows with nothing under them are drawn on consecutive lines. What
//! the call does is the column that gives way first, because it is the one
//! the operator can find again in the diff or the Changes pane. The cost is
//! never cut short of its figure.
//!
//! What the cost column says depends on what the backend reported about the
//! call, not on which tool it was: the lines a change added and removed, the
//! status a command exited with, and otherwise the bytes it returned — which
//! every finished call has. Each carries how long it ran where this shell's
//! clock saw both ends. A call that did not succeed says so instead, and draws
//! the backend's reason for it under the row, or that it gave none. A call a
//! standing rule let through says `rule` after its cost, where it has no diff
//! to say it under.
//!
//! A call that ran the tests says under its row what the run reported: its
//! counts where its output held the whole run, beside a bar of them, and
//! otherwise that the result was not read — never a number the output did
//! not give. While it runs it says how long it has run, against the last
//! whole run of the same command where there was one — timed from when it was
//! allowed, and not drawn at all while a question about it waits.
//!
//! A run of calls to the same tool is one group: a row with the run's summed
//! figures, and a row for each call under it unless the operator has folded
//! the runs away.

use std::time::Duration;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::app::{Call, Entry, EntryKind, Gate, human_bytes};
use crate::clock::{self, Stamp};
use crate::text;
use crate::theme::Theme;
use crate::ui::count;
use niobe_core::event::ToolOutcome;
use niobe_core::session::TestRunRecord;
use niobe_core::test_run::FailedTests;

/// How much of a tool-call entry the operator has asked to see, and the
/// columns every row is drawn in: what holds for the whole transcript.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(crate) struct Detail {
    /// Every run of calls is drawn as its group row alone.
    pub(crate) folded: bool,
    /// Every diff is drawn whole rather than cut at
    /// [`crate::hunks::MAX_ROWS`].
    pub(crate) diffs_open: bool,
    /// How wide the name and agent columns are drawn.
    pub(crate) columns: Columns,
    /// Whether the sub-agents' rows are drawn under a heading per agent,
    /// which names the agent in place of every row's tag.
    pub(crate) grouped: bool,
    /// The moment being drawn, which a test run in progress is timed
    /// against. `None` where the shell has not read its clock.
    pub(crate) now: Option<Stamp>,
}

/// How wide the tool's name and the sub-agent's tag are drawn, so that what
/// the calls do starts in one column down the whole transcript.
///
/// Each is as wide as the widest it holds, rather than as wide as the widest
/// it could: a session that only reads and runs commands draws its names in
/// four cells, and one no sub-agent worked in has no agent column at all.
/// A new, wider name lays the transcript out again, which is rare — the set
/// of tools a session calls settles early.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(crate) struct Columns {
    pub(crate) name: usize,
    pub(crate) agent: usize,
}

impl Columns {
    /// The columns `entries` need.
    pub(crate) fn of(entries: &[Entry]) -> Self {
        let name = entries
            .iter()
            .filter(|entry| !entry.calls.is_empty())
            .map(|entry| text::width(&head(entry)))
            .max()
            .unwrap_or(0)
            .clamp(NAME_LEAST, NAME_MOST);
        let agent = entries
            .iter()
            .filter(|entry| !entry.calls.is_empty() || entry.kind == EntryKind::SubAgent)
            .filter_map(|entry| entry.agent.as_deref())
            .map(text::width)
            .max()
            .unwrap_or(0)
            .min(crate::tags::TAG_MAX);
        Columns { name, agent }
    }
}

/// The glyph and the space after it, which every row of the transcript starts
/// with.
const GUTTER: usize = 2;

/// The narrowest the name column is drawn: `Bash`, `Read` and `Edit` fit.
const NAME_LEAST: usize = 4;

/// The widest the name column is drawn. A longer name — an MCP tool's, with
/// its server in front — is cut rather than pushing what every other row
/// does out of sight.
const NAME_MOST: usize = 14;

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
        calls if !detail.diffs_open && all_memory(calls) => {
            remembered(entry, calls, width, detail, colour, theme)
        }
        [call] => single(entry, call, width, detail, colour, theme),
        calls => group(entry, calls, width, detail, colour, theme),
    };
    lines.push(Line::from(""));
    lines
}

/// A call on its own: its row, then the reason it failed or the lines it
/// changed.
fn single(
    entry: &Entry,
    call: &Call,
    width: usize,
    detail: Detail,
    colour: Color,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let (glyph, glyph_colour) = match (call.failed(), call.running()) {
        (true, _) => ("✗", colour),
        (false, true) => (RUNNING, theme.hot),
        (false, false) => ("⚙", colour),
    };
    let mut lines = vec![row(
        (glyph, glyph_colour),
        entry.head.clone(),
        Doing {
            agent: entry.agent.as_deref().filter(|_| !detail.grouped),
            what: &call.what,
        },
        result(call, theme),
        width,
        detail.columns,
        colour,
        theme,
    )];
    lines.extend(under(call, " ".repeat(GUTTER), width, detail, theme));
    lines
}

/// Whether every call of an entry changed one of the agent's own notes, and
/// nothing else.
fn all_memory(calls: &[Call]) -> bool {
    !calls.is_empty() && calls.iter().all(|call| call.memory)
}

/// What the agent's own notes changed by a call are drawn as, in place of
/// what the call does: that it updated its memory, and which note.
const MEMORY: &str = "updated memory";

/// Calls that changed nothing but the agent's own notes, as one row:
/// `◆ Edit ×2  updated memory · notes.md, MEMORY.md  +2 −2`.
///
/// The notes are how the agent works, not what is being built, so their
/// lines are not drawn in the transcript; Ctrl+T draws them as any other
/// change, for whoever wants to read what the agent wrote down.
fn remembered(
    entry: &Entry,
    calls: &[Call],
    width: usize,
    detail: Detail,
    colour: Color,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut names: Vec<&str> = Vec::new();
    for call in calls {
        let name = file_name(&call.what);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let what = format!("{MEMORY} · {}", names.join(", "));
    let result = match calls {
        [call] => result(call, theme),
        calls => group_result(calls, theme),
    };
    vec![row(
        ("◆", colour),
        head(entry),
        Doing {
            agent: entry.agent.as_deref().filter(|_| !detail.grouped),
            what: &what,
        },
        result,
        width,
        detail.columns,
        colour,
        theme,
    )]
}

/// The last part of a path, which is all a note's row names: the agent's
/// notes live outside the repository, and the directory they share says
/// nothing the row does not.
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

/// What a call of a group is said to do: what the backend read it as, or,
/// for one that changed the agent's own notes while the diffs are shut,
/// that it updated its memory and which note.
fn doing(call: &Call, detail: Detail) -> String {
    match call.memory && !detail.diffs_open {
        true => format!("{MEMORY} · {}", file_name(&call.what)),
        false => call.what.clone(),
    }
}

/// A run of calls: the group's row, then the calls under it unless the runs
/// are folded.
fn group(
    entry: &Entry,
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
        .map(|call| doing(call, detail))
        .collect::<Vec<_>>()
        .join(", ");
    let mut lines = vec![row(
        (glyph, colour),
        head(entry),
        Doing {
            agent: entry.agent.as_deref().filter(|_| !detail.grouped),
            what: &what,
        },
        group_result(calls, theme),
        width,
        detail.columns,
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
        lines.push(child(branch, call, width, detail, theme));
        lines.extend(under(call, format!("{indent}{stem}"), width, detail, theme));
    }
    lines
}

/// One call of a group: hung from the group's row, what it does, and its own
/// cost on the right.
fn child(branch: &str, call: &Call, width: usize, detail: Detail, theme: &Theme) -> Line<'static> {
    let colour = match call.failed() {
        true => theme.del,
        false => theme.fg,
    };
    let lead = " ".repeat(GUTTER);
    let result = result(call, theme);
    let room = width
        .saturating_sub(GUTTER + text::width(branch) + GAP)
        .saturating_sub(spans_width(&result));
    let what = text::truncate(&doing(call, detail), room);
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
        .as_ref()
        .filter(|run| !call.failed() || run.counts.is_some() || run.failed);
    let progress = call
        .running()
        .then_some(call)
        .filter(|call| call.testing && !call.asked)
        .map(|call| progress_line(call, detail.now, room, theme));
    let body = match (&call.printed, tested, call.failed(), &call.change) {
        (Some(printed), _, _, _) => {
            let mut lines: Vec<_> = tested
                .map(|run| test_line(run, room, theme))
                .into_iter()
                .collect();
            lines.extend(printed_lines(call, printed, room, theme));
            lines
        }
        (None, Some(run), _, _) => vec![test_line(run, room, theme)],
        (None, None, true, _) => vec![reason(call, room, theme)],
        // The agent's own notes keep their lines behind Ctrl+T.
        (None, None, false, Some(_)) if call.memory && !detail.diffs_open => Vec::new(),
        (None, None, false, Some(change)) => {
            crate::hunks::lines(change, room, detail.diffs_open, theme)
        }
        (None, None, false, None) => Vec::new(),
    };
    progress
        .into_iter()
        .chain(body)
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
    let above = match (printed.cut, printed.above) {
        (false, 0) => None,
        (false, above) => Some(format!("… {above} lines above")),
        // What was not kept was not counted, so the count is a floor.
        (true, 0) => Some("… more above".to_owned()),
        (true, above) => Some(format!("… at least {above} lines above")),
    };
    if let Some(above) = above {
        lines.push(Line::from(Span::styled(above, dim.italic())));
    }
    // Wrapped rather than cut at the edge: what a command printed is read
    // here and nowhere else in the shell, and a path cut at the pane's edge
    // has nothing that opens it.
    lines.extend(printed.tail.iter().flat_map(|line| {
        let mut parts = text::wrap(line, room.max(1));
        if parts.is_empty() {
            // A blank line is a line of what was printed too.
            parts.push(String::new());
        }
        parts
            .into_iter()
            .map(|part| Line::from(Span::styled(part, Style::new().fg(theme.fg))))
    }));
    if lines.is_empty() && !call.running() {
        lines.push(Line::from(Span::styled("printed nothing", dim.italic())));
    }
    lines
}

/// What a test run reported, hung under its call: `637 passed · 0 failed`,
/// with the failures in the failure colour where there are any, and the first
/// test a failing binary listed, where a list was left.
///
/// Where the room runs out, the ignored go first, then the passed and then
/// the failing test's name: the failures are what the line is for. A run
/// whose counts were not read says that, and whether it is still known to
/// have failed.
fn test_line(run: &TestRunRecord, room: usize, theme: &Theme) -> Line<'static> {
    let dim = Style::new().fg(theme.dim);
    let named = failing_within(&run.failures, usize::MAX)
        .map(|said| (NAMED, Span::styled(said, theme_del(theme))));
    let mut figures = match run.counts {
        Some(counts) => {
            let (passed, failed) = match counts.failing() {
                true => (Style::new().fg(theme.fg), theme_del(theme).bold()),
                false => (Style::new().fg(theme.add), dim),
            };
            let mut figures = vec![
                (2, Span::styled(format!("{} passed", counts.passed), passed)),
                (0, Span::styled(format!("{} failed", counts.failed), failed)),
            ];
            figures.extend(named);
            if counts.ignored > 0 {
                figures.push((3, Span::styled(format!("{} ignored", counts.ignored), dim)));
            }
            figures
        }
        None if run.failed => {
            let mut figures = vec![(0, Span::styled("tests failed", theme_del(theme).bold()))];
            figures.extend(named);
            figures.push((2, Span::styled("counts not read", dim)));
            figures
        }
        None => vec![(0, Span::styled("test result not read", dim.italic()))],
    };
    let room = room.saturating_sub(text::width(LAST_BRANCH));
    let cells = bar_cells(room, figures_width(&figures));
    let bar = run
        .counts
        .filter(|_| cells > 0)
        .map(|counts| result_bar(counts, cells, theme));
    let room = match bar {
        Some(_) => room.saturating_sub(cells + text::width(BAR_GAP)),
        None => room,
    };
    while figures.len() > 1 && figures_width(&figures) > room {
        let Some(least) = (0..figures.len()).max_by_key(|&at| figures[at].0) else {
            break;
        };
        // The name gives up its own end before the figure goes: which binary
        // it is from is what keeps it from reading as the run's whole list.
        let excess = figures_width(&figures).saturating_sub(room);
        let shortened = (figures[least].0 == NAMED)
            .then(|| {
                let columns = text::width(&figures[least].1.content).saturating_sub(excess);
                failing_within(&run.failures, columns)
            })
            .flatten();
        match shortened {
            Some(said) => {
                figures[least].1.content = said.into();
                break;
            }
            None => {
                figures.remove(least);
            }
        }
    }
    let mut spans = vec![Span::styled(LAST_BRANCH, dim)];
    if let Some(bar) = bar {
        spans.extend(bar);
        spans.push(Span::raw(BAR_GAP));
    }
    for (at, (_, figure)) in figures.into_iter().enumerate() {
        if at > 0 {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(figure);
    }
    Line::from(spans)
}

/// How many cells a test run's bar takes in `room` beside `said` columns of
/// text: the whole bar where both fit, half of it where only that does, and
/// none where the bar would cost the text a single column. The bar draws what
/// the words already say, so it is the first thing a narrow line gives up.
fn bar_cells(room: usize, said: usize) -> usize {
    [TEST_BAR, TEST_BAR / 2]
        .into_iter()
        .find(|cells| cells + text::width(BAR_GAP) + said <= room)
        .unwrap_or(0)
}

/// How wide a test run's bar is drawn where there is room for all of it.
const TEST_BAR: usize = 20;

/// What stands between a test run's bar and what is written beside it.
const BAR_GAP: &str = "  ";

/// A bar's cell, filled.
const BAR_FILLED: &str = "━";

/// A bar's cell, not filled yet.
const BAR_EMPTY: &str = "─";

/// The run's tests as `cells` cells: passed in the colour of an addition,
/// failed in the failure colour and ignored dim, each in proportion.
///
/// A failure is never rounded away: however many tests passed, one that
/// failed keeps a cell, because a bar all of one colour reads as a run that
/// all passed. A run of no tests has no bar to draw.
fn result_bar(counts: niobe_core::TestCounts, cells: usize, theme: &Theme) -> Vec<Span<'static>> {
    let total = counts
        .passed
        .saturating_add(counts.failed)
        .saturating_add(counts.ignored);
    if total == 0 {
        return Vec::new();
    }
    let share = |count: u64| -> usize {
        let exact = u128::from(count) * cells as u128 / u128::from(total);
        let at_least = usize::from(count > 0);
        usize::try_from(exact).unwrap_or(cells).max(at_least)
    };
    let failed = share(counts.failed).min(cells);
    let ignored = share(counts.ignored).min(cells - failed);
    let passed = cells - failed - ignored;
    [
        (passed, theme.add),
        (failed, theme.del),
        (ignored, theme.dim),
    ]
    .into_iter()
    .filter(|(cells, _)| *cells > 0)
    .map(|(cells, colour)| Span::styled(BAR_FILLED.repeat(cells), Style::new().fg(colour)))
    .collect()
}

/// `testing · 18s` under a test run still going, or, where the same command
/// ran to the end earlier in the session, a bar of this run's time against
/// that one's: `━━━━━━──── testing · 20s of the last run's 40s`.
///
/// The backend says nothing about a run until it ends, so the bar is not the
/// run's progress and does not claim to be: it is the clock against the last
/// run, named as such, and full where this run has gone on past it. Where the
/// shell has not read its clock there is no time to give, and it says only
/// that the tests are running.
fn progress_line(call: &Call, now: Option<Stamp>, room: usize, theme: &Theme) -> Line<'static> {
    let dim = Style::new().fg(theme.dim);
    let mut spans = vec![Span::styled(LAST_BRANCH, dim)];
    let Some(ran) = now
        .zip(call.began())
        .and_then(|(now, began)| now.since(began))
    else {
        spans.push(Span::styled("testing", dim));
        return Line::from(spans);
    };
    let room = room.saturating_sub(text::width(LAST_BRANCH));
    let said = match call.last_run {
        Some(last) if ran > last => format!(
            "testing · {}, past the last run's {}",
            clock::spent(ran),
            clock::took(last)
        ),
        Some(last) => format!(
            "testing · {} of the last run's {}",
            clock::spent(ran),
            clock::took(last)
        ),
        None => format!("testing · {}", clock::spent(ran)),
    };
    let cells = bar_cells(room, text::width(&said));
    if let Some(last) = call.last_run.filter(|_| cells > 0) {
        let filled = match last.as_millis() {
            0 => cells,
            last => usize::try_from(ran.as_millis().saturating_mul(cells as u128) / last)
                .unwrap_or(cells)
                .min(cells),
        };
        spans.push(Span::styled(
            BAR_FILLED.repeat(filled),
            Style::new().fg(theme.hot),
        ));
        spans.push(Span::styled(BAR_EMPTY.repeat(cells - filled), dim));
        spans.push(Span::raw(BAR_GAP));
    }
    let left = room.saturating_sub(spans_width(&spans[1..]));
    spans.push(Span::styled(text::truncate(&said, left), dim));
    Line::from(spans)
}

/// Where the failing test's name ranks among a test line's figures: after the
/// failures, ahead of everything else.
const NAMED: u8 = 1;

/// The shortest a failing test's name is cut to before it is given up: fewer
/// columns than this name nothing.
const NAME_AT_LEAST: usize = 8;

/// `tests::wrong in --lib`, `tests::wrong +2 in --lib` where the binary
/// listed more, or `tests::wrong +3 in 2 binaries` where more than one binary
/// listed its failures — the first test named, and whose lists they were — in
/// `columns` at most. The name is cut to fit and the rest is kept whole;
/// `None` where that leaves less than [`NAME_AT_LEAST`] of the name, or where
/// no list was left.
fn failing_within(failures: &[FailedTests], columns: usize) -> Option<String> {
    let first = failures.first()?.tests.first()?;
    let named = failures
        .iter()
        .fold(0usize, |named, list| named.saturating_add(list.tests.len()));
    let more = match named.saturating_sub(1) {
        0 => String::new(),
        more => format!(" +{more}"),
    };
    let whose = match failures {
        [only] => format!("{more} in {}", only.binary),
        lists => format!("{more} in {} binaries", lists.len()),
    };
    let room = columns.saturating_sub(text::width(&whose));
    (room >= NAME_AT_LEAST.min(text::width(first)))
        .then(|| format!("{}{whose}", text::truncate(first, room)))
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
                Some(ToolOutcome::Interrupted) => "stopped by the operator",
                Some(ToolOutcome::Failed | ToolOutcome::Ok) | None => {
                    "the call failed and the backend said nothing about why"
                }
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

/// The row itself: the glyph, the name in its column, the sub-agent's tag in
/// its own, what the call does in whatever is left, and the cost on the
/// right.
///
/// What the call does gives way first. On a pane too narrow for the columns
/// and the cost together the agent column goes, then the name gives way, and
/// the cost never does. A row the session's own agent made leaves the agent
/// column blank, so what every call does still starts in one column.
#[allow(clippy::too_many_arguments)] // Each is one column of the row; a struct would only name them again.
fn row(
    (glyph, glyph_colour): (&str, Color),
    name: String,
    doing: Doing<'_>,
    result: Vec<Span<'static>>,
    width: usize,
    columns: Columns,
    colour: Color,
    theme: &Theme,
) -> Line<'static> {
    let cost = spans_width(&result);
    let room = width.saturating_sub(GUTTER + cost + GAP);
    let name_column = columns.name.max(NAME_LEAST).min(room);
    let name = text::truncate(&name, name_column);
    let left = room.saturating_sub(name_column + 1);
    let agent_column = match columns.agent > 0 && left >= columns.agent + 1 + WHAT_LEAST {
        true => columns.agent,
        false => 0,
    };
    let what_room = match agent_column {
        0 => left,
        _ => left.saturating_sub(agent_column + 1),
    };
    let what = text::truncate(doing.what, what_room);
    let gap = what_room
        .saturating_sub(text::width(&what))
        .saturating_add(GAP);

    let mut spans = vec![
        Span::styled(format!("{glyph} "), Style::new().fg(glyph_colour).bold()),
        Span::styled(text::pad(&name, name_column), Style::new().fg(colour)),
        Span::raw(" "),
    ];
    if agent_column > 0 {
        let tag = text::truncate(doing.agent.unwrap_or_default(), agent_column);
        spans.push(Span::styled(
            format!("{} ", text::pad(&tag, agent_column)),
            Style::new().fg(theme.dim),
        ));
    }
    spans.push(Span::styled(what, Style::new().fg(theme.fg)));
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(result);
    Line::from(spans)
}

/// The least of what a call does the agent column is kept beside: fewer
/// cells than this and the column goes, because a row that names who made
/// the call and not what it was says nothing about the call.
const WHAT_LEAST: usize = 8;

/// The glyph a call still running is drawn with.
const RUNNING: &str = "⠋";

/// What a tool-call entry is called in the name column: the tool, and how
/// many calls a run of them holds.
fn head(entry: &Entry) -> String {
    match entry.calls.len() {
        0 | 1 => entry.head.clone(),
        calls => format!("{} ×{calls}", entry.head),
    }
}

/// A sub-agent's words, as a row among its calls: `↳`, its tag across the
/// name and agent columns, what it said where what a call does is drawn, and
/// `says` where a call's cost is. What it said wraps under itself, so a long
/// answer keeps the column its first line started in.
pub(crate) fn said(
    entry: &Entry,
    width: usize,
    detail: Detail,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let columns = detail.columns;
    let tag = entry.agent.as_deref().unwrap_or(&entry.head);
    let span = columns.name.max(NAME_LEAST) + 1 + columns.agent;
    let lead = (span.max(text::width(tag)) + 1).min(width / 2);
    let room = width.saturating_sub(GUTTER + lead + GAP + text::width(SAYS));
    let body = crate::markdown::render(entry.body.trim_end(), room.max(1), theme);
    let tag = text::truncate(tag, lead.saturating_sub(1));
    let indent = " ".repeat(GUTTER + lead);

    let mut lines = Vec::new();
    for (at, line) in body.into_iter().enumerate() {
        let mut spans = match at {
            0 => vec![
                Span::styled(
                    format!("{} ", EntryKind::SubAgent.glyph()),
                    Style::new().fg(theme.agent).bold(),
                ),
                Span::styled(text::pad(&tag, lead), Style::new().fg(theme.agent).bold()),
            ],
            _ => vec![Span::raw(indent.clone())],
        };
        let drawn = spans_width(&line.spans);
        spans.extend(line.spans);
        if at == 0 {
            spans.push(Span::raw(" ".repeat(room.saturating_sub(drawn) + GAP)));
            spans.push(Span::styled(SAYS, Style::new().fg(theme.dim)));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(""));
    lines
}

/// The heading a sub-agent's rows are grouped under: `▾`, its tag across
/// the name and agent columns, its task where what a call does is drawn, and
/// how many calls the rows under it hold where a call's cost is.
pub(crate) fn heading(
    tag: &str,
    task: &str,
    calls: usize,
    width: usize,
    detail: Detail,
    theme: &Theme,
) -> Line<'static> {
    let columns = detail.columns;
    let span = columns.name.max(NAME_LEAST) + 1 + columns.agent;
    let lead = (span.max(text::width(tag)) + 1).min(width / 2);
    let count = match calls {
        1 => "1 call".to_owned(),
        calls => format!("{calls} calls"),
    };
    let room = width.saturating_sub(GUTTER + lead + GAP + text::width(&count));
    let task = text::truncate(task, room);
    let gap = room.saturating_sub(text::width(&task)) + GAP;
    let tag = text::truncate(tag, lead.saturating_sub(1));
    Line::from(vec![
        Span::styled("▾ ", Style::new().fg(theme.title).bold()),
        Span::styled(text::pad(&tag, lead), Style::new().fg(theme.title).bold()),
        Span::styled(task, Style::new().fg(theme.dim)),
        Span::raw(" ".repeat(gap)),
        Span::styled(count, Style::new().fg(theme.dim)),
    ])
}

/// What stands where a call's cost would, on a sub-agent's words.
const SAYS: &str = "says";

/// What a row says the call does, and which sub-agent made it, where one did.
#[derive(Clone, Copy)]
struct Doing<'a> {
    /// The sub-agent's name, or `None` for the session's own call.
    agent: Option<&'a str>,
    /// What the call does, in one line.
    what: &'a str,
}

/// What stands between an agent's name and what its call does.
const AGENT_MARK: &str = " › ";

/// A sub-agent's name and the mark after it, fitted into the `room` it shares
/// with what the call does.
///
/// The name is what tells two agents' interleaved rows apart, so it keeps
/// half the room however long what the call does is, and all of the room
/// what the call does leaves. Where there is no room for a letter of it and
/// the mark, it is dropped whole rather than left as a mark after an
/// ellipsis.
pub(crate) fn agent_tag(agent: &str, what: &str, room: usize) -> String {
    let mark = text::width(AGENT_MARK);
    let half = room.saturating_sub(mark) / 2;
    let left = room.saturating_sub(mark + text::width(what));
    let name = text::truncate(agent, half.max(left));
    match name.as_str() {
        "" | "…" => String::new(),
        _ => format!("{name}{AGENT_MARK}"),
    }
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| text::width(&span.content)).sum()
}

/// What a call the session or its turn ended under says in place of its
/// cost: it has none, and it did not fail.
const CUT_SHORT: &str = "cut short";

/// What one call cost, as the backend and the clock reported it.
fn result(call: &Call, theme: &Theme) -> Vec<Span<'static>> {
    let dim = Style::new().fg(theme.dim);
    let Some(outcome) = call.outcome else {
        return vec![match call.interrupted {
            true => Span::styled(CUT_SHORT, dim),
            false => Span::styled("running", Style::new().fg(theme.hot)),
        }];
    };
    let mut spans = match (outcome, call.exit_code, call.lines) {
        (ToolOutcome::Denied, _, _) => return vec![Span::styled("denied", theme_del(theme))],
        (ToolOutcome::Interrupted, _, _) => vec![Span::styled("stopped", dim)],
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
        spans.push(Span::styled(format!(" · {}", clock::took(took)), dim));
    }
    if call.gate == Some(Gate::Rule) && call.change.is_none() {
        spans.push(Span::styled(format!(" · {RULE_MARK}"), dim));
    }
    spans
}

/// What a call a standing rule let through says after its cost. A call with
/// a diff says who let it through under the diff instead; one without has
/// only its row, and a command run by a rule reads the same there as one the
/// operator allowed by hand.
const RULE_MARK: &str = "rule";

/// What a run of calls cost, summed: the lines its changes added and removed
/// where every call that succeeded changed a file, and otherwise the bytes
/// every call returned; how many failed, how many were refused, and how many
/// the session ended under; and how long they ran.
///
/// A sum is marked `≥` where one of the calls in it had no figure to add, by
/// [`count`]'s rule — and a call that has not finished has none yet.
fn group_result(calls: &[Call], theme: &Theme) -> Vec<Span<'static>> {
    let dim = Style::new().fg(theme.dim);
    if calls.iter().any(Call::running) {
        return vec![Span::styled("running", Style::new().fg(theme.hot))];
    }
    let succeeded: Vec<&Call> = calls
        .iter()
        .filter(|call| !call.failed() && !call.interrupted)
        .collect();
    let ended = |outcome: ToolOutcome| {
        calls
            .iter()
            .filter(|call| call.outcome == Some(outcome))
            .count()
    };
    let cut = calls.iter().filter(|call| call.interrupted).count();

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
    // Where nothing in the run succeeded, what became of its calls is the
    // whole of what it says: it returned nothing worth a figure.
    let mut follows = !succeeded.is_empty();
    for (count, said, style) in [
        (cut, CUT_SHORT, dim),
        (ended(ToolOutcome::Failed), "failed", theme_del(theme)),
        (ended(ToolOutcome::Denied), "denied", theme_del(theme)),
    ] {
        if count == 0 {
            continue;
        }
        let said = match count == calls.len() {
            true => format!("{said} ×{count}"),
            false => format!("{count} {said}"),
        };
        match follows {
            true => spans.push(Span::styled(format!(" · {said}"), style)),
            false => spans = vec![Span::styled(said, style)],
        }
        follows = true;
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
        true => clock::took(total),
        false => format!("≥{}", clock::took(total)),
    })
}

/// `+8 −6`, each side in its colour, with [`count`]'s marks for a side some
/// call did not state.
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
            agent: None,
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
            columns: Columns::of(app.entries()),
            grouped: false,
            now: None,
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

    /// A command a standing rule let through says so on its row, which is
    /// the only place a call with no diff can: the operator did not see it
    /// asked. One the operator allowed carries no mark.
    #[test]
    fn a_command_a_standing_rule_let_through_is_marked_on_its_row() {
        for (decision, marked) in [
            (niobe_core::event::PermissionDecision::AllowByRule, true),
            (niobe_core::event::PermissionDecision::Allow, false),
        ] {
            let mut app = app();
            app.apply(&start("t1", "Bash"));
            app.apply(&Event::PermissionResponse {
                id: "t1".into(),
                decision,
                message: None,
            });
            let mut ended = end("t1", "Bash", ToolOutcome::Ok, None);
            if let Event::ToolCallEnd { exit_code, .. } = &mut ended {
                *exit_code = Some(0);
            }
            app.apply(&ended);

            let rows = drawn(&app, false);
            assert_eq!(
                rows[0].ends_with("exit 0 · rule"),
                marked,
                "{decision:?}: {rows:?}"
            );
            assert!(rows[0].contains("exit 0"), "{rows:?}");
        }
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

    /// In a run of edits where only some were to the agent's own notes, the
    /// note's row says so and keeps its lines behind Ctrl+T, and the
    /// project's edit is drawn as ever.
    #[test]
    fn a_note_among_the_projects_edits_is_named_as_memory_and_drawn_without_its_lines() {
        use niobe_core::event::ChangeScope;

        let mut app = app();
        for (id, path, scope) in [
            ("t1", "src/lib.rs", ChangeScope::Project),
            ("t2", "/notes/memory/MEMORY.md", ChangeScope::AgentMemory),
        ] {
            app.apply(&start(id, "Edit"));
            app.apply(&end(id, "Edit", ToolOutcome::Ok, None));
            app.apply(&Event::FileChange {
                path: path.to_owned(),
                added: Some(1),
                removed: Some(1),
                hunks: vec![
                    niobe_core::diff::Hunk::checked(
                        1,
                        1,
                        1,
                        1,
                        vec![
                            niobe_core::diff::Line::Removed(format!("old {id}")),
                            niobe_core::diff::Line::Added(format!("new {id}")),
                        ],
                    )
                    .expect("one line each side"),
                ],
                scope,
            });
        }

        let rows = drawn(&app, false);
        let said = rows.join("\n");
        assert!(rows[0].starts_with("▾ Edit ×2"), "{said}");
        assert!(
            said.contains("new t1"),
            "the project's edit lost its lines:\n{said}"
        );
        assert!(
            !said.contains("new t2"),
            "the note's lines are drawn:\n{said}"
        );
        let note = rows
            .iter()
            .find(|row| row.contains("└ "))
            .expect("the note hangs last");
        // The test's calls are read as their ids, as a backend's summary.
        assert!(note.contains("updated memory · t2"), "{said}");
        assert!(rows[0].contains("t1, updated memory · t2"), "{said}");
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
                scope: niobe_core::event::ChangeScope::Project,
            });
        }

        let rows = drawn(&app, false);
        assert!(rows[0].ends_with("+8 −≥3"), "{rows:?}");
        assert!(rows[1].ends_with("+4 —"), "{rows:?}");
        assert!(rows[2].ends_with("+4 −3"), "{rows:?}");
    }

    fn spawn(app: &mut App, id: &str, label: &str) {
        app.apply(&Event::AgentSpawn {
            id: niobe_core::event::AgentId::new(id),
            parent: None,
            kind: None,
            label: label.to_owned(),
        });
    }

    fn start_by(id: &str, name: &str, agent: &str) -> Event {
        Event::ToolCallStart {
            id: id.into(),
            name: name.to_owned(),
            input: String::new(),
            summary: Some(format!("catalog/{id}.py")),
            agent: Some(niobe_core::event::AgentId::new(agent)),
        }
    }

    #[test]
    fn a_sub_agents_call_names_the_agent_by_its_tag_in_a_column_of_its_own() {
        let mut app = app();
        spawn(&mut app, "toolu_a", "quick-lookup: Summarize");
        app.apply(&start_by("t1", "Read", "toolu_a"));

        let rows = drawn(&app, false);
        assert!(
            rows[0].starts_with("⠋ Read quick catalog/t1.py "),
            "{rows:?}"
        );
        assert!(rows[0].ends_with("running"), "{rows:?}");
        assert_eq!(rows[0].chars().count(), 80, "{rows:?}");
    }

    /// Each row of `app`'s transcript at 80 columns, one string a row.
    fn every_row(app: &App) -> Vec<String> {
        let detail = Detail {
            columns: Columns::of(app.entries()),
            ..Detail::default()
        };
        app.entries()
            .iter()
            .flat_map(|entry| lines(entry, 80, detail, &Theme::default()))
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    /// The cell `needle` starts at in `row`.
    fn cell_of(row: &str, needle: &str) -> Option<usize> {
        row.find(needle).map(|at| text::width(&row[..at]))
    }

    #[test]
    fn what_a_call_does_starts_in_one_column_for_a_wide_tool_name_and_agent_tag() {
        let mut app = app();
        app.apply(&Event::AgentSpawn {
            id: niobe_core::event::AgentId::new("toolu_a"),
            parent: None,
            kind: Some("漢字漢字-writer".to_owned()),
            label: "漢字漢字-writer: 書く".to_owned(),
        });
        app.apply(&start_by("t1", "mcp__漢字サーバー__ツール", "toolu_a"));
        app.apply(&Event::ToolCallStart {
            id: "t2".into(),
            name: "Read".to_owned(),
            input: String::new(),
            summary: Some("catalog/t2.py".to_owned()),
            agent: None,
        });

        let rows = every_row(&app);
        let rows: Vec<&String> = rows.iter().filter(|row| !row.is_empty()).collect();
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(cell_of(rows[0], "catalog/t1.py"), Some(26), "{rows:?}");
        assert_eq!(cell_of(rows[1], "catalog/t2.py"), Some(26), "{rows:?}");
        assert_eq!(cell_of(rows[0], "漢字漢字"), Some(17), "{rows:?}");
        for row in rows {
            assert_eq!(text::width(row), 80, "{row:?}");
            assert!(row.ends_with("running"), "{row:?}");
        }
    }

    #[test]
    fn a_wide_agent_tag_keeps_its_words_and_its_heading_in_their_columns() {
        let mut app = app();
        app.apply(&Event::AgentSpawn {
            id: niobe_core::event::AgentId::new("toolu_a"),
            parent: None,
            kind: Some("漢字漢字-writer".to_owned()),
            label: "漢字漢字-writer: 書く".to_owned(),
        });
        app.apply(&start_by("t1", "Read", "toolu_a"));
        app.apply(&Event::AssistantMessage {
            text: "Done.".into(),
            agent: Some(niobe_core::event::AgentId::new("toolu_a")),
        });

        let detail = Detail {
            columns: Columns::of(app.entries()),
            ..Detail::default()
        };
        let theme = Theme::default();
        let words: String = said(&app.entries()[1], 60, detail, &theme)[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(cell_of(&words, "Done."), Some(16), "{words:?}");
        assert_eq!(text::width(&words), 60, "{words:?}");
        let head: String = heading("漢字漢字", "書く", 1, 60, detail, &theme)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(cell_of(&head, "書く"), Some(16), "{head:?}");
        assert_eq!(text::width(&head), 60, "{head:?}");
    }

    #[test]
    fn the_columns_are_as_wide_as_the_widest_name_and_tag_the_transcript_holds() {
        let mut app = app();
        app.apply(&start("t1", "Read"));
        assert_eq!(
            Columns::of(app.entries()),
            Columns {
                name: NAME_LEAST,
                agent: 0
            },
            "no agent made a call, so there is no agent column"
        );

        spawn(&mut app, "toolu_a", "quick-lookup: Summarize");
        app.apply(&start_by("t2", "TodoWrite", "toolu_a"));
        app.apply(&start(
            "t3",
            "mcp__claude_ai_Notion__notion-query-data-sources",
        ));
        assert_eq!(
            Columns::of(app.entries()),
            Columns {
                name: NAME_MOST,
                agent: text::width("quick")
            }
        );
    }

    #[test]
    fn a_sub_agents_words_are_a_row_under_its_tag_that_wraps_in_its_own_column() {
        let mut app = app();
        spawn(&mut app, "toolu_a", "quick-lookup: Summarize");
        app.apply(&start_by("t1", "Read", "toolu_a"));
        app.apply(&Event::AssistantMessage {
            text: "The stub is in down mode still, and the probe reads it as an outage.".into(),
            agent: Some(niobe_core::event::AgentId::new("toolu_a")),
        });

        let detail = Detail {
            columns: Columns::of(app.entries()),
            ..Detail::default()
        };
        let rows: Vec<String> = said(&app.entries()[1], 60, detail, &Theme::default())
            .into_iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        assert!(
            rows[0].starts_with("↳ quick      The stub is in"),
            "{rows:?}"
        );
        assert!(rows[0].ends_with("says"), "{rows:?}");
        assert_eq!(text::width(&rows[0]), 60, "{rows:?}");
        let column = "↳ quick      ".chars().count();
        assert!(
            rows[1].starts_with(&" ".repeat(column)) && !rows[1][column..].starts_with(' '),
            "the words wrap under their own first line: {rows:?}"
        );
    }

    #[test]
    fn two_agents_calls_to_one_tool_are_not_one_group() {
        let mut app = app();
        spawn(&mut app, "toolu_a", "Review fetch");
        spawn(&mut app, "toolu_b", "Review cache");
        app.apply(&start_by("t1", "Read", "toolu_a"));
        app.apply(&start_by("t2", "Read", "toolu_b"));
        app.apply(&start_by("t3", "Read", "toolu_b"));

        let heads: Vec<(&str, Option<&str>, usize)> = app
            .entries()
            .iter()
            .map(|entry| {
                (
                    entry.head.as_str(),
                    entry.agent.as_deref(),
                    entry.calls.len(),
                )
            })
            .collect();
        assert_eq!(
            heads,
            [("Read", Some("fetch"), 1), ("Read", Some("cache"), 2)]
        );
    }

    #[test]
    fn an_agents_name_keeps_half_the_room_and_gives_way_whole_where_none_is_left() {
        let long = "x".repeat(60);
        assert_eq!(agent_tag("Review fetch", &long, 40), "Review fetch › ");
        assert_eq!(
            text::width(&agent_tag(&"n".repeat(50), &long, 40)),
            18 + text::width(AGENT_MARK),
            "a long name took more than half the room from what the call does"
        );
        assert_eq!(
            agent_tag(&"n".repeat(50), "short", 40),
            format!("{}…{AGENT_MARK}", "n".repeat(31)),
            "the room a short call leaves went unused"
        );
        assert_eq!(agent_tag("Review fetch", &long, 3), "");
        assert_eq!(agent_tag("Review fetch", &long, 5), "");
    }

    #[test]
    fn a_call_still_running_says_so_in_place_of_a_cost() {
        let mut app = app();
        app.apply(&start("t1", "Bash"));

        assert!(drawn(&app, false)[0].ends_with("running"));
    }

    #[test]
    fn a_call_the_session_ended_under_says_it_was_cut_short() {
        let fatal = Event::Error {
            message: "the `claude` session ended: exit status 1".to_owned(),
            fatal: true,
        };
        let mut alone = app();
        alone.apply(&start("t1", "Bash"));
        alone.apply(&fatal);
        let rows = drawn(&alone, false);
        assert!(rows[0].ends_with("cut short"), "{rows:?}");

        let mut run = app();
        run.apply(&start("t1", "Read"));
        run.apply(&end("t1", "Read", ToolOutcome::Ok, None));
        run.apply(&start("t2", "Read"));
        run.apply(&fatal);
        let rows = drawn(&run, true);
        assert!(!rows[0].contains("running"), "{rows:?}");
        assert!(rows[0].contains("1 cut short"), "{rows:?}");
        assert!(!rows[0].contains("failed"), "{rows:?}");
    }

    #[test]
    fn a_call_its_turn_ended_without_says_it_was_cut_short() {
        let mut app = app();
        app.apply(&start("t1", "Bash"));
        app.apply(&Event::TurnEnded);

        let rows = drawn(&app, false);
        assert!(rows[0].ends_with("cut short"), "{rows:?}");
    }

    #[test]
    fn a_run_counts_a_refused_call_as_denied_rather_than_failed() {
        let mut app = app();
        app.apply(&start("t1", "Bash"));
        app.apply(&end("t1", "Bash", ToolOutcome::Ok, None));
        app.apply(&start("t2", "Bash"));
        app.apply(&end("t2", "Bash", ToolOutcome::Denied, None));

        let rows = drawn(&app, true);
        assert!(rows[0].ends_with("2.0 kB · 1 denied"), "{rows:?}");
        assert!(!rows[0].contains("failed"), "{rows:?}");
    }

    #[test]
    fn a_run_of_refused_calls_reads_as_denied_and_a_failure_beside_them_as_failed() {
        let mut refused = app();
        for id in ["t1", "t2"] {
            refused.apply(&start(id, "Bash"));
            refused.apply(&end(id, "Bash", ToolOutcome::Denied, None));
        }
        assert!(drawn(&refused, true)[0].ends_with("denied ×2"));

        let mut mixed = app();
        mixed.apply(&start("t1", "Bash"));
        mixed.apply(&end("t1", "Bash", ToolOutcome::Failed, Some("gone")));
        mixed.apply(&start("t2", "Bash"));
        mixed.apply(&end("t2", "Bash", ToolOutcome::Denied, None));
        assert!(drawn(&mixed, true)[0].ends_with("1 failed · 1 denied"));
    }

    /// A `cargo test` call that ended with `status`, and the run it reported.
    fn tested(app: &mut App, status: i32, counts: Option<niobe_core::TestCounts>, failed: bool) {
        tested_naming(app, status, counts, failed, Vec::new());
    }

    /// The same, with the tests each failing binary named.
    fn tested_naming(
        app: &mut App,
        status: i32,
        counts: Option<niobe_core::TestCounts>,
        failed: bool,
        failures: Vec<niobe_core::FailedTests>,
    ) {
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
            failures,
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
        assert_eq!(
            rows[1].trim(),
            "└ ━━━━━━━━━━━━━━━━━━━━  637 passed · 0 failed · 2 ignored"
        );
    }

    #[test]
    fn a_failing_test_run_shows_its_failures_in_the_failure_colour_in_place_of_its_status() {
        let mut app = app();
        tested(&mut app, 101, counts(630, 7, 0), false);

        let theme = Theme::default();
        let detail = Detail::default();
        let lines = lines(&app.entries()[0], 80, detail, &theme);
        let under: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            under.trim(),
            "└ ━━━━━━━━━━━━━━━━━━━━  630 passed · 7 failed"
        );
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

    fn named(binary: &str, tests: &[&str]) -> Vec<niobe_core::FailedTests> {
        vec![niobe_core::FailedTests {
            binary: binary.to_owned(),
            tests: tests.iter().map(|&test| test.to_owned()).collect(),
        }]
    }

    const STATEMENT: [&str; 2] = [
        "a_statement_line_037_rounds_like_the_ledger",
        "a_statement_line_088_rounds_like_the_ledger",
    ];

    #[test]
    fn a_failed_run_names_the_first_test_its_failing_binary_listed_and_that_binary() {
        let mut app = app();
        tested_naming(
            &mut app,
            101,
            None,
            true,
            named("--test statement", &STATEMENT),
        );

        let under = |width| {
            let line = lines(
                &app.entries()[0],
                width,
                Detail::default(),
                &Theme::default(),
            )
            .swap_remove(1);
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .trim()
                .to_owned()
        };
        assert_eq!(
            under(120),
            "└ tests failed · a_statement_line_037_rounds_like_the_ledger +1 in --test statement \
             · counts not read"
        );
        assert_eq!(
            under(80),
            "└ tests failed · a_statement_line_037_rounds_like_the_… +1 in --test statement",
            "the name gives up its end, and never whose list it was"
        );
        assert_eq!(under(40), "└ tests failed");
    }

    #[test]
    fn a_counted_failing_run_names_what_failed_ahead_of_what_passed() {
        let mut app = app();
        tested_naming(
            &mut app,
            101,
            counts(3, 1, 1),
            true,
            named("--lib", &["tests::wrong"]),
        );

        assert_eq!(
            drawn(&app, false)[1].trim(),
            "└ ━━━━━━━━━━  3 passed · 1 failed · tests::wrong in --lib · 1 ignored"
        );
        let line =
            lines(&app.entries()[0], 36, Detail::default(), &Theme::default()).swap_remove(1);
        let under: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(under.trim(), "└ 1 failed · tests::wrong in --lib");
    }

    #[test]
    fn a_run_that_failed_in_several_binaries_names_its_first_test_and_how_many_binaries() {
        let mut app = app();
        let mut failures = named("--lib", &["tests::wrong"]);
        failures.extend(named("--test statement", &STATEMENT));
        tested_naming(&mut app, 101, counts(4, 3, 0), true, failures);

        assert_eq!(
            drawn(&app, false)[1].trim(),
            "└ ━━━━━━━━━━━━━━━━━━━━  4 passed · 3 failed · tests::wrong +2 in 2 binaries"
        );
    }

    #[test]
    fn a_tailed_run_that_named_a_failing_test_says_it_failed_with_no_count() {
        // `cargo test 2>&1 | tail -30`: the status is tail's, the list is whole.
        let mut app = app();
        tested_naming(&mut app, 0, None, true, named("--lib", &["tests::wrong"]));

        let rows = drawn(&app, false);
        assert_eq!(
            rows[1].trim(),
            "└ tests failed · tests::wrong in --lib · counts not read"
        );
    }

    #[test]
    fn a_named_failure_is_drawn_in_the_failure_colour() {
        let mut app = app();
        tested_naming(&mut app, 101, None, true, named("--lib", &["tests::wrong"]));

        let theme = Theme::default();
        let line = lines(&app.entries()[0], 80, Detail::default(), &theme).swap_remove(1);
        let name = line
            .spans
            .iter()
            .find(|span| span.content.starts_with("tests::wrong"))
            .expect("the name is a span of its own");
        assert_eq!(name.style.fg, Some(theme.del));
    }

    #[test]
    fn a_line_a_command_printed_wider_than_the_pane_is_drawn_whole() {
        let long = format!("/private/tmp/{}/deep", "segment".repeat(12));
        let mut call = Call::started("pwd".to_owned(), None);
        call.outcome = Some(ToolOutcome::Ok);
        call.printed = Some(crate::app::Printed {
            above: 0,
            cut: false,
            tail: vec![long.clone(), String::new(), "after".to_owned()],
        });

        let drawn: Vec<String> = printed_lines(
            &call,
            call.printed.as_ref().expect("set"),
            30,
            &Theme::default(),
        )
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();

        assert_eq!(drawn.concat().replace("after", ""), long, "{drawn:?}");
        assert!(
            drawn.iter().all(|line| text::width(line) <= 30),
            "{drawn:?}"
        );
        assert!(
            drawn.iter().any(String::is_empty),
            "the blank line was lost"
        );
    }

    /// The rows of a command the operator ran that printed `output`, of
    /// `bytes` in all.
    fn printed_rows(output: &str, bytes: u64) -> Vec<String> {
        let mut app = app();
        app.apply(&start("op1", crate::shell::OPERATOR_SHELL));
        app.apply(&Event::ToolCallEnd {
            id: "op1".into(),
            name: crate::shell::OPERATOR_SHELL.to_owned(),
            input: "yes | head -c 50000000".to_owned(),
            output: output.to_owned(),
            bytes,
            outcome: ToolOutcome::Ok,
            summary: None,
            exit_code: Some(0),
            error: None,
        });
        drawn(&app, false)
    }

    #[test]
    fn the_lines_above_a_commands_whole_output_are_counted() {
        let output = "y\n".repeat(20);

        let rows = printed_rows(&output, output.len() as u64);

        assert_eq!(rows[1].trim(), "… 8 lines above", "{rows:?}");
    }

    #[test]
    fn the_lines_above_the_kept_end_of_a_longer_output_are_a_floor() {
        let kept = "y\n".repeat(20);

        assert_eq!(
            printed_rows(&kept, 50_000_000)[1].trim(),
            "… at least 8 lines above"
        );
        assert_eq!(
            printed_rows(&"y\n".repeat(12), 50_000_000)[1].trim(),
            "… more above"
        );
    }

    fn second(seconds: u64) -> Stamp {
        Stamp::new(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds), None)
    }

    /// `cargo test` started at `from` seconds, ending at `to` with every test
    /// passed where it ends.
    fn cargo_test(app: &mut App, id: &str, from: u64, to: Option<u64>) {
        app.apply_at(
            &Event::ToolCallStart {
                id: id.into(),
                name: "Bash".to_owned(),
                input: r#"{"command":"cargo test"}"#.to_owned(),
                summary: Some("cargo test".to_owned()),
                agent: None,
            },
            second(from),
        );
        let Some(to) = to else {
            return;
        };
        app.apply_at(
            &Event::ToolCallEnd {
                id: id.into(),
                name: "Bash".to_owned(),
                input: String::new(),
                output: String::new(),
                bytes: 1_024,
                outcome: ToolOutcome::Ok,
                summary: Some("cargo test".to_owned()),
                exit_code: Some(0),
                error: None,
            },
            second(to),
        );
        app.apply_at(
            &Event::TestRun {
                id: id.into(),
                counts: counts(12, 0, 0),
                exit_code: Some(0),
                failed: false,
                failures: Vec::new(),
            },
            second(to),
        );
    }

    /// The last entry of `app` as the transcript draws it at `now`.
    fn drawn_at(app: &App, now: u64) -> Vec<Line<'static>> {
        let detail = Detail {
            columns: Columns::of(app.entries()),
            now: Some(second(now)),
            ..Detail::default()
        };
        let entry = app.entries().last().expect("there is an entry");
        lines(entry, 80, detail, &Theme::default())
    }

    fn text_of(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_test_run_in_progress_says_how_long_it_has_run() {
        let mut app = app();
        cargo_test(&mut app, "t1", 100, None);

        let rows = drawn_at(&app, 118);
        assert!(
            text_of(&rows[0]).ends_with("running"),
            "{:?}",
            text_of(&rows[0])
        );
        assert_eq!(text_of(&rows[1]).trim(), "└ testing · 18s");
        assert_eq!(
            text_of(&drawn_at(&app, 119)[1]).trim(),
            "└ testing · 19s",
            "the time moves with the clock"
        );
    }

    /// The backend reports no progress through a run, so the bar is this run's
    /// time against the last whole run of the same command, and says so.
    #[test]
    fn a_test_run_in_progress_is_drawn_against_the_last_run_of_its_command() {
        let mut app = app();
        cargo_test(&mut app, "t1", 0, Some(40));
        cargo_test(&mut app, "t2", 100, None);

        // The two calls are one run of Bash calls, and the line hangs under
        // the second.
        let testing = |now| -> Line<'static> {
            drawn_at(&app, now)
                .into_iter()
                .find(|line| text_of(line).contains("testing"))
                .expect("the run in progress says so")
        };
        let half = testing(120);
        assert_eq!(
            text_of(&half).trim(),
            "└ ━━━━━━━━━━──────────  testing · 20s of the last run's 40s"
        );
        let theme = Theme::default();
        let filled = half
            .spans
            .iter()
            .find(|span| span.content.contains('━'))
            .expect("half the bar is filled");
        assert_eq!(filled.style.fg, Some(theme.hot));

        assert_eq!(
            text_of(&testing(150)).trim(),
            "└ ━━━━━━━━━━━━━━━━━━━━  testing · 50s, past the last run's 40s"
        );
    }

    /// A run waiting on a question has not started its tests: the time it
    /// waits is the operator's, and a bar of it against the last run would
    /// claim a run nobody measured.
    #[test]
    fn a_test_run_waiting_on_a_question_is_not_drawn_as_testing() {
        let mut app = app();
        cargo_test(&mut app, "t1", 0, Some(40));
        cargo_test(&mut app, "t2", 100, None);
        app.apply_at(
            &Event::PermissionRequest {
                id: "t2".into(),
                tool: "Bash".to_owned(),
                input: r#"{"command":"cargo test"}"#.to_owned(),
                target: Some("cargo test".to_owned()),
                agent: None,
            },
            second(100),
        );

        let waiting = drawn_at(&app, 130);
        assert!(
            !waiting.iter().any(|line| text_of(line).contains("testing")),
            "{:?}",
            waiting.iter().map(text_of).collect::<Vec<_>>()
        );

        app.apply_at(
            &Event::PermissionResponse {
                id: "t2".into(),
                decision: niobe_core::event::PermissionDecision::Allow,
                message: None,
            },
            second(130),
        );
        let running = drawn_at(&app, 140)
            .into_iter()
            .find(|line| text_of(line).contains("testing"))
            .expect("the run says so once it is allowed");
        assert_eq!(
            text_of(&running).trim(),
            "└ ━━━━━───────────────  testing · 10s of the last run's 40s"
        );
    }

    #[test]
    fn a_finished_test_run_draws_its_counts_as_a_bar_beside_them() {
        let mut app = app();
        tested(&mut app, 101, counts(15, 4, 1), false);

        let theme = Theme::default();
        let rows = lines(&app.entries()[0], 80, Detail::default(), &theme);
        assert_eq!(
            text_of(&rows[1]).trim(),
            "└ ━━━━━━━━━━━━━━━━━━━━  15 passed · 4 failed · 1 ignored"
        );
        let cells = |colour| -> usize {
            rows[1]
                .spans
                .iter()
                .filter(|span| span.content.contains('━') && span.style.fg == Some(colour))
                .map(|span| span.content.chars().count())
                .sum()
        };
        assert_eq!(
            (cells(theme.add), cells(theme.del), cells(theme.dim)),
            (15, 4, 1),
            "a cell for every test of twenty"
        );
    }

    #[test]
    fn a_failure_keeps_a_cell_of_the_bar_however_many_passed() {
        let mut app = app();
        tested(&mut app, 101, counts(5_000, 1, 0), false);

        let theme = Theme::default();
        let rows = lines(&app.entries()[0], 80, Detail::default(), &theme);
        assert!(
            rows[1]
                .spans
                .iter()
                .any(|span| span.content.contains('━') && span.style.fg == Some(theme.del)),
            "{:?}",
            rows[1]
        );
    }
}
