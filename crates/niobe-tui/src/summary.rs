// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The session's own tool calls between two things it said, drawn as one
//! line that says what they did: `Read 3 files, searched for 2 patterns,
//! edited 1 file +12 −3, ran 2 shell commands, 41 tests passed`.
//!
//! The transcript is read for what the agent says and decides; the calls it
//! makes on the way are the how, and drawn a row each they bury the what.
//! So, unless the operator asks for every call (Ctrl+O), a run of calls
//! that went as they should is counted on a dim line of its own, and only
//! what asks to be read stays a row: a call that failed or was refused, one
//! a question about is waiting on, one the operator ran with `!`, and a
//! hand-off to a sub-agent, whose own words and calls are left to its row
//! and to the view narrowed to it.
//!
//! Every count is of the calls the line stands for; a figure a call did not
//! report is left out of the line rather than read as a zero. The lines the
//! edits added and removed are summed as the run's row sums them under
//! Ctrl+O ([`crate::ui::count`]'s rule): a side every edit stated is the
//! whole, `+≥6` where some edit left it unstated is at least that much, and a
//! side no edit stated a line of is left out.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{Call, Entry, EntryKind};
use crate::text;
use crate::theme::Theme;
use niobe_core::event::{OPERATOR_SHELL, ToolOutcome};
use niobe_core::work::{self, Deed, Work};

/// How far the line is set in from the transcript's edge: where an entry's
/// body starts, under its glyph.
const INDENT: usize = 2;

/// Whether `entry` is counted on a summary line rather than drawn as rows of
/// its own: one or more of the session's own calls, none of which asks to
/// be read — see the module's notes.
pub(crate) fn summed(entry: &Entry) -> bool {
    entry.kind == EntryKind::Tool
        && entry.agent.is_none()
        && entry.head != OPERATOR_SHELL
        && !entry.calls.is_empty()
        && work::of_call(&entry.head, None, None, "").work != Work::Delegating
        && entry.calls.iter().all(went_as_it_should)
}

/// Whether a call ran, or is running, without anything the operator has to
/// read about it.
fn went_as_it_should(call: &Call) -> bool {
    let outcome = match call.outcome {
        None => !call.interrupted,
        Some(ToolOutcome::Ok) => true,
        Some(ToolOutcome::Failed | ToolOutcome::Denied | ToolOutcome::Interrupted) => false,
    };
    outcome && !call.asked && call.answer.is_none() && call.error.is_none()
}

/// What one call is counted as on the line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tally {
    Did(Deed),
    /// An edit of the notes the agent keeps for itself.
    Memory,
    Shell,
    /// A tool of an MCP server, by the server's name.
    Server(String),
    /// Any other tool, by its own name.
    Tool(String),
}

impl Tally {
    fn of(tool: &str, call: &Call) -> Self {
        if call.memory {
            return Self::Memory;
        }
        if work::command_of(tool, call.command.as_deref(), Some(&call.what)).is_some() {
            return Self::Shell;
        }
        if let Some(deed) = work::deed(tool) {
            return Self::Did(deed);
        }
        if let Some(server) = crate::app::label_server(tool) {
            return Self::Server(server.to_owned());
        }
        match work::of_call(tool, None, None, "").work {
            Work::Server(server) => Self::Server(server),
            Work::Exploring
            | Work::Editing
            | Work::Building
            | Work::VersionControl
            | Work::Web
            | Work::Delegating
            | Work::Housekeeping
            | Work::Other => Self::Tool(tool.to_owned()),
        }
    }
}

/// The calls counted under one [`Tally`].
#[derive(Debug)]
struct Counted<'a> {
    calls: usize,
    /// What each call was about, for the counts of distinct files. A set,
    /// since a long run edits thousands and the line is said every frame.
    what: std::collections::HashSet<&'a str>,
    /// The lines the calls added, summed, and whether every one stated it.
    added: (u64, bool),
    /// The lines the calls removed, summed, and whether every one stated it.
    removed: (u64, bool),
}

impl Default for Counted<'_> {
    fn default() -> Self {
        Self {
            calls: 0,
            what: std::collections::HashSet::new(),
            added: crate::calls::NOTHING_SUMMED,
            removed: crate::calls::NOTHING_SUMMED,
        }
    }
}

/// What the calls of `entries` did, as the sentence their line says: each
/// kind of thing in the order the calls first did it, then what the last
/// test run among them reported, then what is still running.
pub(crate) fn sentence<'a>(entries: impl IntoIterator<Item = &'a Entry>) -> String {
    let mut tallies: Vec<(Tally, Counted<'a>)> = Vec::new();
    let mut tested = None;
    let mut running = None;
    for entry in entries {
        for call in &entry.calls {
            if call.running() {
                running = Some(call.programs.as_deref().unwrap_or(&call.what));
                continue;
            }
            if let Some(run) = &call.tested {
                tested = Some(run);
            }
            let tally = Tally::of(&entry.head, call);
            let at = match tallies.iter().position(|(seen, _)| *seen == tally) {
                Some(at) => at,
                None => {
                    tallies.push((tally, Counted::default()));
                    tallies.len() - 1
                }
            };
            if let Some((_, counted)) = tallies.get_mut(at) {
                count(counted, call);
            }
        }
    }
    let mut said: Vec<String> = tallies
        .iter()
        .map(|(tally, counted)| phrase(tally, counted))
        .collect();
    said.extend(tested.and_then(test_phrase));
    if let Some(what) = running {
        said.push(format!(
            "running {}…",
            text::truncate(what.trim(), RUNNING_MOST)
        ));
    }
    capitalised(&said.join(", "))
}

/// How much of what a running call does the line names.
const RUNNING_MOST: usize = 60;

/// Counts `call` in `counted`. A call whose change was never reported states
/// neither side, since what it changed is unknown rather than nothing.
fn count<'a>(counted: &mut Counted<'a>, call: &'a Call) {
    counted.calls = counted.calls.saturating_add(1);
    counted.what.insert(&call.what);
    let (added, removed) = call.lines.unwrap_or((None, None));
    counted.added = crate::calls::summed_with(counted.added, added);
    counted.removed = crate::calls::summed_with(counted.removed, removed);
}

/// One side of the lines edits changed, as the sentence says it after a
/// space: [`crate::ui::count`]'s reading, but for the em dash it draws where
/// no call stated a line of the side, which a sentence leaves out instead.
fn lines_side(sign: char, (lines, stated): (u64, bool)) -> Option<String> {
    match (stated, lines) {
        (false, 0) => None,
        (stated, lines) => Some(format!(" {}", crate::ui::count(sign, lines, stated))),
    }
}

fn phrase(tally: &Tally, counted: &Counted<'_>) -> String {
    let calls = counted.calls;
    let files = counted.what.len();
    match tally {
        Tally::Did(Deed::Read) => format!("read {}", plural(files, "file", "files")),
        Tally::Did(Deed::Searched) => {
            format!("searched for {}", plural(calls, "pattern", "patterns"))
        }
        Tally::Did(Deed::Listed) => {
            format!("listed {}", plural(calls, "directory", "directories"))
        }
        Tally::Did(Deed::Edited) => {
            let mut said = format!("edited {}", plural(files, "file", "files"));
            said.extend(lines_side('+', counted.added));
            said.extend(lines_side('−', counted.removed));
            said
        }
        Tally::Did(Deed::Fetched) => format!("fetched {}", plural(calls, "page", "pages")),
        Tally::Did(Deed::SearchedWeb) => match calls {
            1 => "searched the web".to_owned(),
            calls => format!("searched the web {calls} times"),
        },
        Tally::Memory => "updated memory".to_owned(),
        Tally::Shell => format!("ran {}", plural(calls, "shell command", "shell commands")),
        Tally::Server(name) | Tally::Tool(name) => match calls {
            1 => format!("used {name}"),
            calls => format!("used {name} {calls} times"),
        },
    }
}

/// What a test run reported, where it said anything: its counts where its
/// output held the whole run, or that it failed where only that is known.
fn test_phrase(run: &niobe_core::session::TestRunRecord) -> Option<String> {
    match run.counts {
        Some(counts) if counts.failed > 0 => Some(format!(
            "{} of {} failed",
            counts.failed,
            plural(
                usize::try_from(counts.passed.saturating_add(counts.failed)).unwrap_or(usize::MAX),
                "test",
                "tests"
            )
        )),
        Some(counts) => Some(format!(
            "{} passed",
            plural(
                usize::try_from(counts.passed).unwrap_or(usize::MAX),
                "test",
                "tests"
            )
        )),
        None if run.failed => Some("a test run failed".to_owned()),
        None => None,
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    match count {
        1 => format!("1 {one}"),
        count => format!("{count} {many}"),
    }
}

fn capitalised(said: &str) -> String {
    let mut chars = said.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The summary line of `sentence`, wrapped to the pane `width` cells wide,
/// dim, and the blank line after it that every entry has.
pub(crate) fn lines(sentence: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let indent = " ".repeat(INDENT);
    let mut lines: Vec<Line<'static>> = text::wrap(sentence, width.saturating_sub(INDENT).max(1))
        .into_iter()
        .map(|wrapped| {
            Line::from(vec![
                Span::raw(indent.clone()),
                Span::styled(wrapped, Style::new().fg(theme.dim)),
            ])
        })
        .collect();
    lines.push(Line::from(""));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use niobe_core::session::TestRunRecord;
    use niobe_core::test_run::TestCounts;

    fn ended(what: &str) -> Call {
        let mut call = Call::started(what.to_owned(), None);
        call.outcome = Some(ToolOutcome::Ok);
        call
    }

    fn calls(tool: &str, calls: Vec<Call>) -> Entry {
        Entry {
            queued: false,
            kind: EntryKind::Tool,
            head: tool.to_owned(),
            meta: String::new(),
            body: String::new(),
            streaming: false,
            at: None,
            calls,
            agent: None,
            attention: niobe_core::attention::Attention::Routine,
        }
    }

    fn shell(command: &str) -> Call {
        let mut call = ended(command);
        call.command = Some(command.to_owned());
        call.programs = Some(command.split(' ').next().unwrap_or_default().to_owned());
        call
    }

    #[test]
    fn a_run_of_calls_reads_as_what_they_did_in_the_order_they_first_did_it() {
        let entries = [
            calls("Grep", vec![ended("etag"), ended("If-None-Match")]),
            calls("Read", vec![ended("fetch.py"), ended("cache.py")]),
            calls("Bash", vec![shell("ls catalog")]),
            calls("Read", vec![ended("fetch.py")]),
            calls("Glob", vec![ended("**/*.py")]),
        ];
        assert_eq!(
            sentence(&entries),
            "Searched for 3 patterns, read 2 files, ran 1 shell command"
        );
    }

    #[test]
    fn edits_count_the_files_they_changed_and_sum_the_lines_each_reported() {
        let mut first = ended("fetch.py");
        first.lines = Some((Some(10), Some(2)));
        let mut again = ended("fetch.py");
        again.lines = Some((Some(2), Some(1)));
        let mut other = ended("cache.py");
        other.lines = Some((Some(30), Some(0)));
        let entries = [
            calls("Edit", vec![first, again]),
            calls("Write", vec![other]),
        ];
        assert_eq!(sentence(&entries), "Edited 2 files +42 −3");
    }

    /// A side one of the edits left unstated sums to at least what the
    /// others stated, as the run's row reads it under Ctrl+O, and never as
    /// though it were the whole.
    #[test]
    fn a_side_some_edit_left_unstated_sums_to_a_floor() {
        let mut first = ended("fetch.py");
        first.lines = Some((Some(12), Some(3)));
        let mut other = ended("cache.py");
        other.lines = Some((Some(30), None));
        let entries = [calls("Edit", vec![first]), calls("Write", vec![other])];
        assert_eq!(sentence(&entries), "Edited 2 files +42 −≥3");
    }

    /// A notebook's edit never says how many lines it changed, so beside an
    /// edit that did, both sides are floors.
    #[test]
    fn a_notebook_edit_beside_an_edit_that_counted_its_lines_leaves_both_sides_floors() {
        let mut edit = ended("fetch.py");
        edit.lines = Some((Some(6), Some(2)));
        let mut notebook = ended("analysis.ipynb");
        notebook.lines = Some((None, None));
        let entries = [
            calls("Edit", vec![edit]),
            calls("NotebookEdit", vec![notebook]),
        ];
        assert_eq!(sentence(&entries), "Edited 2 files +≥6 −≥2");
    }

    /// An edit whose change was never reported at all leaves the sum of the
    /// others a floor too: what it changed is unknown, not nothing.
    #[test]
    fn an_edit_with_no_change_reported_leaves_the_others_sum_a_floor() {
        let mut edit = ended("fetch.py");
        edit.lines = Some((Some(6), Some(2)));
        let entries = [calls("Edit", vec![edit, ended("cache.py")])];
        assert_eq!(sentence(&entries), "Edited 2 files +≥6 −≥2");
    }

    #[test]
    fn a_notebook_edit_alone_says_no_figure_for_its_lines() {
        let mut notebook = ended("analysis.ipynb");
        notebook.lines = Some((None, None));
        let entries = [calls("NotebookEdit", vec![notebook])];
        assert_eq!(sentence(&entries), "Edited 1 file");
    }

    #[test]
    fn an_edit_whose_lines_no_one_reported_says_no_figure_for_them() {
        let entries = [calls("Edit", vec![ended("fetch.py")])];
        assert_eq!(sentence(&entries), "Edited 1 file");
    }

    #[test]
    fn the_last_test_run_says_what_it_counted() {
        let mut first = shell("cargo test");
        first.tested = Some(TestRunRecord {
            counts: Some(TestCounts {
                passed: 40,
                failed: 1,
                ignored: 0,
                suites: 1,
            }),
            exit_code: Some(101),
            failed: true,
            failures: Vec::new(),
        });
        let mut last = shell("cargo test");
        last.tested = Some(TestRunRecord {
            counts: Some(TestCounts {
                passed: 41,
                failed: 0,
                ignored: 0,
                suites: 1,
            }),
            exit_code: Some(0),
            failed: false,
            failures: Vec::new(),
        });
        let entries = [calls("Bash", vec![first.clone(), last])];
        assert_eq!(sentence(&entries), "Ran 2 shell commands, 41 tests passed");
        let entries = [calls("Bash", vec![first])];
        assert_eq!(
            sentence(&entries),
            "Ran 1 shell command, 1 of 41 tests failed"
        );
    }

    #[test]
    fn a_test_run_whose_output_was_not_read_claims_no_result() {
        let mut run = shell("cargo test");
        run.tested = Some(TestRunRecord {
            counts: None,
            exit_code: None,
            failed: false,
            failures: Vec::new(),
        });
        assert_eq!(sentence(&[calls("Bash", vec![run])]), "Ran 1 shell command");
    }

    #[test]
    fn a_call_still_running_is_named_last_and_counted_once_it_ends() {
        let running = Call::started("catalog/etag.py".to_owned(), None);
        let entries = [calls("Read", vec![ended("fetch.py"), running])];
        assert_eq!(sentence(&entries), "Read 1 file, running catalog/etag.py…");
        let alone = [calls("Read", vec![Call::started("a.py".to_owned(), None)])];
        assert_eq!(sentence(&alone), "Running a.py…");
    }

    #[test]
    fn other_tools_are_named_and_a_server_by_its_name() {
        let entries = [
            calls("TodoWrite", vec![ended("3 tasks"), ended("3 tasks")]),
            calls(
                &crate::app::tool_label("mcp__claude_ai_Notion__notion-search"),
                vec![ended("q")],
            ),
            calls(
                &crate::app::tool_label("mcp__claude_ai_Notion__notion-fetch"),
                vec![ended("page")],
            ),
            calls("WebFetch", vec![ended("https://example.com")]),
            calls("WebSearch", vec![ended("etag"), ended("304")]),
        ];
        assert_eq!(
            sentence(&entries),
            "Used TodoWrite 2 times, used Notion 2 times, fetched 1 page, searched the web 2 times"
        );
    }

    #[test]
    fn an_edit_of_the_agents_own_notes_reads_as_updating_its_memory() {
        let mut note = ended("memory/notes.md");
        note.memory = true;
        note.lines = Some((Some(3), Some(0)));
        assert_eq!(sentence(&[calls("Edit", vec![note])]), "Updated memory");
    }

    #[test]
    fn what_asks_to_be_read_is_not_summed() {
        assert!(summed(&calls("Read", vec![ended("a.py")])));
        assert!(summed(&calls(
            "Read",
            vec![Call::started("a.py".into(), None)]
        )));

        let mut failed = ended("a.py");
        failed.outcome = Some(ToolOutcome::Failed);
        let mut denied = ended("a.py");
        denied.outcome = Some(ToolOutcome::Denied);
        let mut stopped = ended("a.py");
        stopped.outcome = Some(ToolOutcome::Interrupted);
        let mut cut = Call::started("a.py".into(), None);
        cut.interrupted = true;
        let mut asked = Call::started("a.py".into(), None);
        asked.asked = true;
        let mut answered = ended("a.py");
        answered.answer = Some("do it another way".into());
        for call in [failed, denied, stopped, cut, asked, answered] {
            assert!(
                !summed(&calls("Read", vec![ended("b.py"), call.clone()])),
                "{call:?}"
            );
        }

        assert!(!summed(&calls("Agent", vec![ended("review fetch.py")])));
        assert!(!summed(&calls(OPERATOR_SHELL, vec![ended("ls")])));
        let mut theirs = calls("Read", vec![ended("a.py")]);
        theirs.agent = Some("deep1".into());
        assert!(!summed(&theirs));
        let mut said = calls("Read", vec![ended("a.py")]);
        said.kind = EntryKind::Agent;
        assert!(!summed(&said));
    }

    #[test]
    fn the_line_is_set_in_dim_and_wraps_without_losing_a_word() {
        let theme = Theme::default();
        let lines = lines("Read 3 files, searched for 2 patterns", 20, &theme);
        let read: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        assert_eq!(
            read,
            ["  Read 3 files,", "  searched for 2", "  patterns", ""]
        );
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| { span.content.trim().is_empty() || span.style.fg == Some(theme.dim) })
        );
    }
}
