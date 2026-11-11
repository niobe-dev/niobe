// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The goal a session's operator set with `/goal`, as the CLI reports it.
//!
//! The CLI checks a goal each time a turn would end, with a model of its own
//! asked whether the conversation meets the condition. Its stream says when a
//! goal is set or cleared, in the reply it writes to the command, and says
//! each check that found it not met, in the message it continues the turn
//! with. A check that found it met ends the turn and is written to the
//! stream as nothing: only the CLI's transcript has it, as a `goal_status`
//! attachment, which is written before the turn's `result` reaches the
//! stream (Claude Code 2.1.322, measured on 11 November 2026). So where a
//! turn ends while a goal is set, the end of that transcript is read for the
//! last check.
//!
//! A turn the operator stopped, or one that failed, ends with no check at
//! all, and the goal is still set: the transcript's last check is then the
//! one before, which found it not met, and nothing is said.
//!
//! A conversation the CLI resumes has its goal restored with it, which the
//! stream does not say either; the same transcript's last goal record says
//! which goal is still set, as long as it is in the last mebibyte.

use niobe_core::GoalReport;
use serde::Deserialize;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// How the CLI's reply to `/goal <condition>` opens.
const SET: &str = "Goal set: ";

/// How the CLI's reply to `/goal clear` opens, where a goal was set.
const CLEARED: &str = "Goal cleared: ";

/// How the message the CLI continues a turn with after a stop hook opens.
/// The goal's check is a stop hook of the CLI's own, named by its condition
/// in brackets.
const FEEDBACK: &str = "Stop hook feedback:\n";

/// How much of the transcript's end is read for the last check. What the CLI
/// writes after the check and before the turn's `result` is a line or two of
/// its own bookkeeping; the rest of this is room for a long reason.
const TAIL: u64 = 1024 * 1024;

/// What the CLI's own reply `said` reports of the goal: one set, with its
/// condition, or one cleared.
pub(crate) fn replied(said: &str) -> Option<GoalReport> {
    if let Some(condition) = said.strip_prefix(SET) {
        return Some(GoalReport::Set {
            condition: condition.to_owned(),
        });
    }
    said.strip_prefix(CLEARED).map(|_| GoalReport::Cleared)
}

/// The reason a check of the goal set with `condition` found it not met,
/// where `said` is the message the CLI continued a turn with after one.
pub(crate) fn not_met(said: &str, condition: &str) -> Option<String> {
    let hooks = said.strip_prefix(FEEDBACK)?;
    let named = format!("[{condition}]: ");
    let at = hooks.find(&named)?;
    Some(hooks[at + named.len()..].trim_end().to_owned())
}

/// The last check the transcript at `path` records, where it is one that
/// found the goal set with `condition` met. `None` where the file cannot be
/// read, is not a regular file, or its last check found the goal not met or
/// was of another goal.
pub(crate) fn met(path: &Path, condition: &str) -> Option<GoalReport> {
    let last = last_status(path)?;
    (last.met && !last.sentinel && last.condition.as_deref() == Some(condition)).then_some(
        GoalReport::Met {
            reason: last.reason,
        },
    )
}

/// The condition of the goal still set where the transcript at `path`
/// ends, which the CLI restores with the conversation it resumes and says
/// nothing of on its stream. `None` where the last goal record in the
/// transcript's last [`TAIL`] bytes ended its goal, or there is none there.
pub(crate) fn pursued(path: &Path) -> Option<String> {
    let last = last_status(path)?;
    (!last.met).then_some(last.condition).flatten()
}

/// The last `goal_status` the transcript at `path` holds in its last
/// [`TAIL`] bytes.
fn last_status(path: &Path) -> Option<Status> {
    tail(path)?
        .lines()
        .rev()
        .filter(|line| line.contains("\"goal_status\""))
        .filter_map(|line| serde_json::from_str::<Record>(line).ok())
        .filter_map(|record| record.attachment)
        .find(|status| status.kind == "goal_status")
}

/// The last [`TAIL`] bytes of the regular file at `path`, from the first
/// whole line in them, as text.
fn tail(path: &Path) -> Option<String> {
    let mut file = crate::spilled::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    let from = metadata.len().saturating_sub(TAIL);
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.take(TAIL).read_to_end(&mut bytes).ok()?;
    let whole = match from {
        0 => &bytes[..],
        _ => {
            let first = bytes.iter().position(|byte| *byte == b'\n')?;
            &bytes[first + 1..]
        }
    };
    Some(String::from_utf8_lossy(whole).into_owned())
}

/// A transcript line, read for the attachment it carries.
#[derive(Deserialize)]
struct Record {
    #[serde(default)]
    attachment: Option<Status>,
}

/// A `goal_status` attachment: one the CLI writes as a goal is set or
/// cleared, marked `sentinel`, and one for every check, with its verdict and
/// the checking model's reason.
#[derive(Deserialize)]
pub(crate) struct Status {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    met: bool,
    #[serde(default)]
    sentinel: bool,
    #[serde(default)]
    condition: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

impl Status {
    /// What the attachment `attached` reports, where it is a `goal_status`:
    /// a goal set or cleared, or a check's verdict.
    ///
    /// A check that found the goal not met and gave no reason is passed
    /// over: the CLI writes one with every such check.
    pub(crate) fn read(attached: &serde_json::Value) -> Option<GoalReport> {
        let status = Self::deserialize(attached).ok()?;
        if status.kind != "goal_status" {
            return None;
        }
        match (status.sentinel, status.met) {
            (true, false) => status
                .condition
                .map(|condition| GoalReport::Set { condition }),
            (true, true) => Some(GoalReport::Cleared),
            (false, true) => Some(GoalReport::Met {
                reason: status.reason,
            }),
            (false, false) => status.reason.map(|reason| GoalReport::NotMet { reason }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn the_clis_reply_to_the_command_says_a_goal_was_set_or_cleared() {
        assert_eq!(
            replied("Goal set: every test passes"),
            Some(GoalReport::Set {
                condition: "every test passes".to_owned()
            })
        );
        assert_eq!(
            replied("Goal cleared: every test passes"),
            Some(GoalReport::Cleared)
        );
        assert_eq!(replied("No goal set. Usage: `/goal <condition>`"), None);
        assert_eq!(replied("Goal active: every test passes (6 turns)"), None);
    }

    #[test]
    fn a_stop_hooks_feedback_is_a_check_only_where_it_names_the_goal() {
        let said = "Stop hook feedback:\n[every test passes]: Two tests fail.\n";
        assert_eq!(
            not_met(said, "every test passes").as_deref(),
            Some("Two tests fail.")
        );
        assert_eq!(not_met(said, "the docs build"), None);
        assert_eq!(
            not_met("[every test passes]: Two tests fail.", "every test passes"),
            None
        );
    }

    fn transcript(lines: &[&str]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("a temporary file can be made");
        for line in lines {
            writeln!(file, "{line}").expect("the temporary file is writable");
        }
        file
    }

    const SET_LINE: &str = r#"{"type":"attachment","attachment":{"type":"goal_status","met":false,"sentinel":true,"condition":"two replies"}}"#;
    const NOT_MET_LINE: &str = r#"{"type":"attachment","attachment":{"type":"goal_status","met":false,"condition":"two replies","reason":"Only one so far."}}"#;
    const MET_LINE: &str = r#"{"type":"attachment","attachment":{"type":"goal_status","met":true,"condition":"two replies","reason":"Both were sent.","iterations":2,"durationMs":2536,"tokens":274}}"#;
    const AFTER: &str = r#"{"type":"system","subtype":"stop_hook_summary"}"#;

    #[test]
    fn the_transcripts_last_check_says_the_goal_was_met() {
        let file = transcript(&[SET_LINE, NOT_MET_LINE, MET_LINE, AFTER]);
        assert_eq!(
            met(file.path(), "two replies"),
            Some(GoalReport::Met {
                reason: Some("Both were sent.".to_owned())
            })
        );
        assert_eq!(met(file.path(), "three replies"), None);
    }

    #[test]
    fn a_turn_stopped_before_a_check_found_the_goal_met_says_nothing() {
        let file = transcript(&[SET_LINE, NOT_MET_LINE, AFTER]);
        assert_eq!(met(file.path(), "two replies"), None);
        let file = transcript(&[MET_LINE, SET_LINE]);
        assert_eq!(met(file.path(), "two replies"), None);
    }

    #[test]
    fn only_the_end_of_a_long_transcript_is_read_from_a_whole_line() {
        let filler = format!(r#"{{"type":"user","pad":"{}"}}"#, "x".repeat(4096));
        let mut lines = vec![MET_LINE];
        lines.extend(std::iter::repeat_n(filler.as_str(), 300));
        lines.push(NOT_MET_LINE);
        lines.push(MET_LINE);
        let file = transcript(&lines);
        assert!(std::fs::metadata(file.path()).expect("written").len() > TAIL);
        assert!(met(file.path(), "two replies").is_some());

        let mut early = vec![MET_LINE];
        early.extend(std::iter::repeat_n(filler.as_str(), 300));
        let file = transcript(&early);
        assert_eq!(met(file.path(), "two replies"), None);
    }

    #[test]
    fn a_goal_still_set_where_the_transcript_ends_is_the_one_a_resume_restores() {
        let set = transcript(&[SET_LINE, AFTER]);
        assert_eq!(pursued(set.path()).as_deref(), Some("two replies"));
        let checked = transcript(&[SET_LINE, NOT_MET_LINE, AFTER]);
        assert_eq!(pursued(checked.path()).as_deref(), Some("two replies"));
        let met = transcript(&[SET_LINE, NOT_MET_LINE, MET_LINE]);
        assert_eq!(pursued(met.path()), None);
        let cleared = transcript(&[
            SET_LINE,
            r#"{"type":"attachment","attachment":{"type":"goal_status","met":true,"sentinel":true}}"#,
        ]);
        assert_eq!(pursued(cleared.path()), None);
        assert_eq!(pursued(transcript(&[AFTER]).path()), None);
    }

    #[test]
    fn what_is_not_a_regular_file_is_not_read() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        assert_eq!(met(dir.path(), "two replies"), None);
        assert_eq!(met(&dir.path().join("missing.jsonl"), "two replies"), None);
    }

    #[test]
    fn each_goal_status_reads_as_what_it_reports() {
        let read = |line: &str| {
            let record: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
            Status::read(&record["attachment"])
        };
        assert_eq!(
            read(SET_LINE),
            Some(GoalReport::Set {
                condition: "two replies".to_owned()
            })
        );
        assert_eq!(
            read(NOT_MET_LINE),
            Some(GoalReport::NotMet {
                reason: "Only one so far.".to_owned()
            })
        );
        assert_eq!(
            read(MET_LINE),
            Some(GoalReport::Met {
                reason: Some("Both were sent.".to_owned())
            })
        );
        assert_eq!(
            read(r#"{"attachment":{"type":"goal_status","met":true,"sentinel":true}}"#),
            Some(GoalReport::Cleared)
        );
        assert_eq!(
            read(r#"{"attachment":{"type":"date","date":"2026-11-11"}}"#),
            None
        );
    }
}
