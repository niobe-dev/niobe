// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The sheet that tells the operator what happened while they looked away,
//! in words rather than counts: what the agent last said and the decisions it
//! stated, the files it changed, what failed, how far the five-hour window
//! moved, and what waits on them. Nothing on it is a model's summary: the
//! reply is the agent's own, and the rest is what the fold keeps (see
//! [`niobe_core::catch_up`]).
//!
//! Counts that say nothing to someone coming back — which turns ended, how
//! many questions a rule answered, how many sub-agents came and went — are
//! left off: they decide whether there is anything to tell, and the reply
//! says what came of them.

use niobe_core::catch_up::CatchUp;

use crate::app::Sheet;
use crate::text;

/// What the sheet is called, on its top edge.
pub const TITLE: &str = "Since you last looked";

/// What the shell says where nothing has happened since the operator last
/// looked, in place of a sheet with nothing on it.
pub const NOTHING: &str = "nothing has happened since you last looked";

/// How many lines of the agent's reply the sheet quotes. A reply's opening
/// is where it says what it did; the rest is a key away in the transcript.
pub const REPLY_LINES: usize = 12;

/// How many columns of one line of the reply the sheet quotes, so a reply
/// that is one vast line does not fill the sheet on its own.
const REPLY_COLUMNS: usize = 600;

/// The row under a reply the sheet did not quote whole.
pub const MORE_IN_TRANSCRIPT: &str = "  … the rest is in the transcript";

/// What the sheet says beside the catch-up, which the shell knows and the
/// fold does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context<'a> {
    /// What the agent is called, which its reply is headed with.
    pub agent: &'a str,
    /// Each question waiting on the operator, as the tool and what it acts on.
    pub waiting: Vec<String>,
    /// The error that ended the session, where one did.
    pub ended: Option<&'a str>,
}

/// The sheet for `caught`, or `None` where nothing on it would be worth
/// reading.
pub fn sheet(caught: &CatchUp, context: &Context) -> Option<Sheet> {
    let mut rows = Vec::new();
    if let Some(reply) = &caught.reply {
        rows.push(format!("{} said:", text::controls_shown(context.agent)));
        rows.extend(quoted(reply));
    }
    if !caught.decisions.is_empty() {
        rows.push("decided:".to_owned());
        rows.extend(
            caught
                .decisions
                .iter()
                .map(|decision| format!("  · {}", text::controls_shown(&decision.summary))),
        );
    }
    if !caught.files.is_empty() {
        rows.push(format!(
            "changed {}:",
            counted(caught.files.len(), "file", "files")
        ));
        rows.extend(caught.files.iter().map(|file| {
            format!(
                "  {}  {} {}",
                text::controls_shown(&file.path),
                crate::ui::count('+', file.added, file.added_stated()),
                crate::ui::count('−', file.removed, file.removed_stated()),
            )
        }));
    }
    match caught.tests_failed {
        0 => {}
        1 => rows.push("a test run failed".to_owned()),
        runs => rows.push(format!("{runs} test runs failed")),
    }
    match caught.errors {
        0 => {}
        1 => rows.push("the backend reported an error".to_owned()),
        errors => rows.push(format!("the backend reported {errors} errors")),
    }
    if caught.turns.iter().any(|turn| turn.cut) {
        rows.push("a turn was cut off before it ended".to_owned());
    }
    if let Some(share) = caught.five_hour {
        rows.push(match crate::app::percent(share) {
            0 => "the five-hour window moved under one point".to_owned(),
            points => format!("the five-hour window moved {points} points"),
        });
    }
    rows.extend(
        context
            .waiting
            .iter()
            .map(|question| format!("waiting on you: {}", text::controls_shown(question))),
    );
    if let Some(error) = context.ended {
        rows.push(format!(
            "the session ended: {}",
            text::controls_shown(error)
        ));
    }
    (!rows.is_empty()).then(|| Sheet {
        title: TITLE.to_owned(),
        rows,
        link: None,
        scroll: 0,
    })
}

/// The opening of `reply`, indented under its head: its lines as written,
/// a run of blank ones as one, cut at [`REPLY_LINES`] with a row that says
/// where the rest is.
fn quoted(reply: &str) -> Vec<String> {
    let mut lines: Vec<&str> = Vec::new();
    for line in reply.trim().lines() {
        let blank = line.trim().is_empty();
        if !(blank && lines.last().is_some_and(|last| last.trim().is_empty())) {
            lines.push(line);
        }
        // One past what is quoted says there is more, and a reply of a
        // million lines is read no further.
        if lines.len() > REPLY_LINES {
            break;
        }
    }
    let cut = lines.len() > REPLY_LINES;
    let mut rows: Vec<String> = lines
        .into_iter()
        .take(REPLY_LINES)
        .map(|line| match line.trim().is_empty() {
            true => String::new(),
            false => {
                let line = text::truncate(&text::expand_tabs(line), REPLY_COLUMNS);
                format!("  {}", text::controls_shown(&line))
            }
        })
        .collect();
    if cut {
        rows.push(MORE_IN_TRANSCRIPT.to_owned());
    }
    rows
}

/// `count` things, in the words a count of them is said in.
fn counted(count: usize, one: &str, many: &str) -> String {
    match count {
        1 => format!("1 {one}"),
        _ => format!("{count} {many}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use niobe_core::session::{DecisionRecord, FileChanges, TurnRecord};

    fn context() -> Context<'static> {
        Context {
            agent: "claude",
            waiting: Vec::new(),
            ended: None,
        }
    }

    fn turn(cut: bool) -> TurnRecord {
        TurnRecord {
            number: 20,
            tokens: Some(1_000),
            five_hour_share: None,
            cut,
            compacted: None,
        }
    }

    fn rows(caught: &CatchUp, context: &Context) -> Vec<String> {
        sheet(caught, context)
            .map(|sheet| sheet.rows)
            .unwrap_or_default()
    }

    #[test]
    fn the_sheet_says_what_the_agent_said_decided_changed_and_asks_in_words() {
        let caught = CatchUp {
            turns: vec![turn(false); 30],
            reply: Some("The gate is green.\n\n\nCommitted; not pushed.".to_owned()),
            decisions: vec![DecisionRecord {
                summary: "kept the parser".to_owned(),
                agent: None,
            }],
            files: vec![FileChanges {
                path: "src/lib.rs".to_owned(),
                added: 6,
                removed: 2,
                added_unstated: 0,
                removed_unstated: 0,
                changes: 1,
                why: None,
            }],
            tests_failed: 1,
            agents_started: 10,
            agents_finished: 10,
            asked: 281,
            waiting: 1,
            ..CatchUp::default()
        };
        let context = Context {
            waiting: vec!["Monitor until grep -q exit".to_owned()],
            ..context()
        };
        assert_eq!(
            rows(&caught, &context),
            [
                "claude said:",
                "  The gate is green.",
                "",
                "  Committed; not pushed.",
                "decided:",
                "  · kept the parser",
                "changed 1 file:",
                "  src/lib.rs  +6 −2",
                "a test run failed",
                "waiting on you: Monitor until grep -q exit",
            ]
        );
    }

    #[test]
    fn counts_with_nothing_to_say_make_no_sheet() {
        let caught = CatchUp {
            turns: vec![turn(false); 3],
            agents_started: 2,
            agents_finished: 2,
            asked: 7,
            ..CatchUp::default()
        };
        assert_eq!(sheet(&caught, &context()), None);
    }

    #[test]
    fn a_long_reply_is_cut_and_says_where_the_rest_is() {
        let reply: Vec<String> = (1..=40).map(|line| format!("line {line}")).collect();
        let caught = CatchUp {
            reply: Some(reply.join("\n")),
            ..CatchUp::default()
        };
        let rows = rows(&caught, &context());
        assert_eq!(rows.len(), 1 + REPLY_LINES + 1, "{rows:?}");
        assert_eq!(rows[REPLY_LINES], format!("  line {REPLY_LINES}"));
        assert_eq!(rows.last().map(String::as_str), Some(MORE_IN_TRANSCRIPT));
    }

    #[test]
    fn a_reply_holding_an_escape_shows_it_rather_than_sending_it() {
        let caught = CatchUp {
            reply: Some("safe\u{1b}[8m hidden\tafter".to_owned()),
            ..CatchUp::default()
        };
        let rows = rows(&caught, &context());
        assert!(
            !rows
                .iter()
                .any(|row| row.contains('\u{1b}') || row.contains('\t')),
            "{rows:?}"
        );
    }

    #[test]
    fn a_cut_turn_an_error_and_an_ended_session_are_said_once_each() {
        let caught = CatchUp {
            turns: vec![turn(true)],
            errors: 2,
            ..CatchUp::default()
        };
        let context = Context {
            ended: Some("the CLI exited"),
            ..context()
        };
        assert_eq!(
            rows(&caught, &context),
            [
                "the backend reported 2 errors",
                "a turn was cut off before it ended",
                "the session ended: the CLI exited",
            ]
        );
    }

    #[test]
    fn a_window_moved_under_a_point_says_so_rather_than_zero() {
        let caught = CatchUp {
            five_hour: Some(0.004),
            ..CatchUp::default()
        };
        assert_eq!(
            rows(&caught, &context()),
            ["the five-hour window moved under one point"]
        );
    }
}
