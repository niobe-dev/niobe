// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The sheet that tells the operator what happened while they looked away,
//! in the words of the session's own figures: nothing on it is anyone's
//! summary, every line is a count the fold keeps (see
//! [`niobe_core::catch_up`]).

use niobe_core::catch_up::CatchUp;

use crate::app::Sheet;

/// What the sheet is called, on its top edge.
pub const TITLE: &str = "Since you last looked";

/// What the shell says where nothing has happened since the operator last
/// looked, in place of a sheet with nothing on it.
pub const NOTHING: &str = "nothing has happened since you last looked";

/// The sheet for `caught`, with `now` — what the session is doing at this
/// moment, or `None` where it is doing nothing — as its last row.
pub fn sheet(caught: &CatchUp, now: Option<String>) -> Sheet {
    let mut rows = Vec::new();
    if !caught.turns.is_empty() {
        let numbers: Vec<String> = caught
            .turns
            .iter()
            .map(|turn| turn.number.to_string())
            .collect();
        rows.push(format!(
            "{} ended: {}",
            counted(caught.turns.len(), "turn", "turns"),
            numbers.join(", ")
        ));
    }
    if let Some(share) = caught.five_hour {
        rows.push(match crate::app::percent(share) {
            0 => "the five-hour window moved under one point".to_owned(),
            points => format!("the five-hour window moved {points} points"),
        });
    }
    if !caught.files.is_empty() {
        rows.push(format!(
            "{} changed:",
            counted(caught.files.len(), "file", "files")
        ));
        rows.extend(caught.files.iter().map(|file| {
            format!(
                "  {}  {} {}",
                file.path,
                crate::ui::count('+', file.added, file.added_stated()),
                crate::ui::count('−', file.removed, file.removed_stated()),
            )
        }));
    }
    if caught.agents_started > 0 || caught.agents_finished > 0 {
        rows.push(format!(
            "sub-agents: {} started, {} finished",
            caught.agents_started, caught.agents_finished
        ));
    }
    if caught.errors > 0 {
        rows.push(format!(
            "{} reported",
            counted(caught.errors as usize, "error", "errors")
        ));
    }
    if caught.asked > 0 {
        rows.push(format!(
            "{} asked",
            counted(caught.asked as usize, "question", "questions")
        ));
    }
    match caught.waiting {
        0 => {}
        1 => rows.push("1 question is waiting on you".to_owned()),
        waiting => rows.push(format!("{waiting} questions are waiting on you")),
    }
    rows.push(match now {
        Some(doing) => format!("now: {doing}"),
        None => "now: idle".to_owned(),
    });
    Sheet {
        title: TITLE.to_owned(),
        rows,
        link: None,
        scroll: 0,
    }
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
    use niobe_core::session::FileChanges;

    #[test]
    fn each_thing_that_happened_is_a_row_and_what_did_not_is_left_out() {
        let caught = CatchUp {
            files: vec![FileChanges {
                path: "src/lib.rs".to_owned(),
                added: 6,
                removed: 2,
                added_unstated: 0,
                removed_unstated: 0,
                changes: 1,
                why: None,
            }],
            agents_started: 2,
            agents_finished: 1,
            waiting: 1,
            asked: 1,
            ..CatchUp::default()
        };
        assert_eq!(
            sheet(&caught, Some("running Bash  cargo test".to_owned())).rows,
            [
                "1 file changed:",
                "  src/lib.rs  +6 −2",
                "sub-agents: 2 started, 1 finished",
                "1 question asked",
                "1 question is waiting on you",
                "now: running Bash  cargo test",
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
            sheet(&caught, None).rows,
            ["the five-hour window moved under one point", "now: idle"]
        );
    }
}
