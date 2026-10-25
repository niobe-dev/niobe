// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! `niobe overview` over a store holding two recorded sessions: the page it
//! prints, byte for byte, against `tests/snapshots/overview.txt`, and the
//! counts in it against what `jq` reads off the same recordings.
//!
//! After an intentional change to the page, rewrite the picture with
//! `UPDATE_SNAPSHOTS=1 cargo test -p niobe-cli --test overview` and read the
//! diff before accepting it.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, UNIX_EPOCH};

use niobe_core::session::SessionState;
use niobe_store::{Store, read_log};

const FIXTURES: &str = "../niobe-core/tests/fixtures";

/// Noon UTC on 18 September 2026, the day the fixture was recorded.
const RECORDED_ON_MS: u64 = 1_789_732_800_000;

/// A repository named `catalog` holding the recorded session and the one the
/// producer dropped events from, in that order.
fn repository() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    let root = dir.path().join("catalog");
    std::fs::create_dir_all(root.join(".git")).expect("a .git directory can be made");
    std::fs::create_dir(root.join(".niobe")).expect("a .niobe directory can be made");
    let store = Store::open(&root.join(".niobe").join("sessions.db"))
        .expect("a store opens in a temporary directory");
    for log in ["claude-session.jsonl", "producer-gaps.jsonl"] {
        let session = store.create_session().expect("a session is created");
        let text = std::fs::read_to_string(fixtures().join(log)).expect("the fixture reads");
        for event in read_log(&text).expect("the fixture parses") {
            store
                .append_at(
                    session,
                    &event,
                    UNIX_EPOCH + Duration::from_millis(RECORDED_ON_MS),
                )
                .expect("an append succeeds");
        }
    }
    (dir, root)
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES)
}

fn overview(root: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_niobe"))
        .arg("overview")
        .current_dir(root)
        .env("XDG_CONFIG_HOME", root.join("no-user-config-here"))
        .env("CLAUDE_CONFIG_DIR", root.join("no-claude-sessions-here"))
        .env_remove("HOME")
        .output()
        .expect("the niobe binary runs")
}

fn printed(root: &Path) -> String {
    let output = overview(root);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("the page is UTF-8")
}

#[test]
fn the_overview_of_two_recorded_sessions_matches_its_picture() {
    let (_dir, root) = repository();
    let page = printed(&root);

    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/overview.txt");
    if niobe_tui::snapshots::updating().unwrap_or_else(|refused| panic!("{refused}")) {
        std::fs::write(&path, &page).expect("the snapshot can be written");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "no snapshot at {}: {error}\nrun `UPDATE_SNAPSHOTS=1 cargo test` to write one",
            path.display()
        )
    });
    assert_eq!(page, expected, "the overview moved from its picture");
}

/// The counts `jq` reads off the two recordings, with the program in
/// `crates/niobe-core/tests/fixtures/README.md`: the calls ended per tool
/// and those of them that failed, and the one path changed, at +20 −4 and
/// +1 −0.
#[test]
fn the_overviews_tools_and_paths_are_the_ones_the_recordings_hold() {
    let (_dir, root) = repository();
    let page = printed(&root);

    for row in [
        "| Bash | 12 | 3 |",
        "| Read | 9 | 0 |",
        "| Agent | 3 | 0 |",
        "| Edit | 2 | 0 |",
        "| Write | 1 | 0 |",
        "| catalog/fetch.py | 21 | 4 | 1 |",
    ] {
        assert!(page.contains(row), "{row} is not in\n{page}");
    }
}

/// `--last 1` reads the newer of the two sessions alone, and the page says
/// what it was narrowed to.
#[test]
fn the_overview_narrowed_to_the_newest_session_reads_that_one_alone() {
    let (_dir, root) = repository();
    let output = Command::new(env!("CARGO_BIN_EXE_niobe"))
        .args(["overview", "--last", "1"])
        .current_dir(&root)
        .env("XDG_CONFIG_HOME", root.join("no-user-config-here"))
        .env("CLAUDE_CONFIG_DIR", root.join("no-claude-sessions-here"))
        .env_remove("HOME")
        .output()
        .expect("the niobe binary runs");
    let page = String::from_utf8(output.stdout).expect("the page is UTF-8");

    assert!(page.contains("1 session recorded"), "{page}");
    assert!(
        page.contains("Read: the newest 1 session. `niobe overview` alone reads every one."),
        "{page}"
    );
    assert!(page.contains("Session 2:"), "{page}");
    assert!(!page.contains("Session 1:"), "{page}");
}

/// The page's totals are the sums of each recording folded on its own, here
/// and not by the binary: the store and the page between them lose nothing.
#[test]
fn the_overviews_totals_are_the_sums_of_each_session_folded_alone() {
    let (_dir, root) = repository();
    let page = printed(&root);

    let folds: Vec<SessionState> = ["claude-session.jsonl", "producer-gaps.jsonl"]
        .iter()
        .map(|log| {
            let text = std::fs::read_to_string(fixtures().join(log)).expect("the fixture reads");
            SessionState::replay(&read_log(&text).expect("the fixture parses"))
        })
        .collect();
    let sum = |of: fn(&SessionState) -> u64| folds.iter().map(of).sum::<u64>();
    assert_eq!(
        sum(|s| s.totals().cache_write_unsplit),
        0,
        "every record in both recordings split its writes"
    );
    let grouped = |n: u64| {
        let digits = n.to_string();
        let mut out = String::new();
        for (i, digit) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                out.push(',');
            }
            out.push(digit);
        }
        out
    };

    for line in [
        format!(
            "- {} tokens: {} in · {} out · {} cache read · {} cache write ({} for an hour).",
            grouped(sum(|s| s.totals().tokens())),
            grouped(sum(|s| s.totals().input)),
            grouped(sum(|s| s.totals().output)),
            grouped(sum(|s| s.totals().cache_read)),
            grouped(sum(|s| s.totals().cache_write)),
            grouped(sum(|s| s.totals().cache_write_1h)),
        ),
        format!(
            "- {} prompts sent, {} replies from the agent.",
            sum(SessionState::user_messages),
            sum(SessionState::assistant_messages)
        ),
        // `jq -s '[.[] | select(.type == "tool_call_end") | .outcome] |
        // group_by(.) | map({(.[0]): length}) | add' claude-session.jsonl
        // producer-gaps.jsonl` reads 23 ok, 3 failed and 1 denied: the denied
        // call did not run, so it is no call that finished without failing.
        "- 23 of 27 tool calls finished without failing.".to_owned(),
        format!("- {} errors reported.", sum(SessionState::errors)),
    ] {
        assert!(page.contains(&line), "{line} is not in\n{page}");
    }
}

#[test]
fn a_file_the_sessions_changed_that_git_still_lists_as_changed_is_left_open() {
    let (_dir, root) = repository();
    std::fs::remove_dir_all(root.join(".git")).expect("the stand-in .git is removed");
    let init = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .expect("git runs");
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    std::fs::create_dir_all(root.join("catalog")).expect("a directory can be made");
    std::fs::write(root.join("catalog/fetch.py"), "print()\n").expect("a file can be written");
    std::fs::write(root.join("unrelated.txt"), "x\n").expect("a file can be written");

    let page = printed(&root);

    assert!(
        page.contains("- 1 file the sessions changed is not committed: `catalog/fetch.py`\n"),
        "{page}"
    );
    assert!(!page.contains("unrelated.txt"), "{page}");
}

#[test]
fn a_repository_with_nothing_recorded_says_so_in_one_line() {
    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    let root = dir.path().join("empty");
    std::fs::create_dir_all(root.join(".git")).expect("a .git directory can be made");

    assert_eq!(
        printed(&root),
        "# What has been built in empty\n\nNo sessions recorded in this repository.\n"
    );
}

#[test]
fn each_decision_a_reply_stated_is_listed_at_the_time_its_reply_was_stored() {
    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    let root = dir.path().join("catalog");
    std::fs::create_dir_all(root.join(".git")).expect("a .git directory can be made");
    std::fs::create_dir(root.join(".niobe")).expect("a .niobe directory can be made");
    let store = Store::open(&root.join(".niobe").join("sessions.db"))
        .expect("a store opens in a temporary directory");
    let session = store.create_session().expect("a session is created");
    let said = |text: &str| niobe_core::Event::AssistantMessage {
        text: text.to_owned(),
        agent: None,
    };
    let at = |minutes: u64| UNIX_EPOCH + Duration::from_millis(RECORDED_ON_MS + minutes * 60_000);
    for (event, minutes) in [
        (
            niobe_core::Event::UserMessage {
                text: "add etags".to_owned(),
            },
            0,
        ),
        (said("DECISION: reused the LRU rather than a second map"), 3),
        (said("Done.\nDECISION: left the CSV exporter alone"), 7),
    ] {
        store
            .append_at(session, &event, at(minutes))
            .expect("an append succeeds");
    }

    let page = printed(&root);

    assert!(
        page.contains(
            "## Decisions, as the agents said\n\n\
             As each session's agent stated them in its replies: its own account of what it \
             chose, which nothing here checked. Newest first.\n\n\
             - 2026-09-18 12:07 UTC, session 1: left the CSV exporter alone\n\
             - 2026-09-18 12:03 UTC, session 1: reused the LRU rather than a second map\n"
        ),
        "{page}"
    );
}

/// A clear starts a session of its own in the store, and the prompt that typed
/// it is the last event of the session before: that session's turn ended in
/// the next one, so it is not left open. Another shell's session in between,
/// which did stop in the middle of a turn, still is.
#[test]
fn a_session_that_ended_on_a_clear_is_not_left_open_in_the_middle_of_a_turn() {
    use niobe_core::Event;

    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    let root = dir.path().join("catalog");
    std::fs::create_dir_all(root.join(".git")).expect("a .git directory can be made");
    std::fs::create_dir(root.join(".niobe")).expect("a .niobe directory can be made");
    let store = Store::open(&root.join(".niobe").join("sessions.db"))
        .expect("a store opens in a temporary directory");
    let prompt = |text: &str| Event::UserMessage {
        text: text.to_owned(),
    };
    let said = |text: &str| Event::AssistantMessage {
        text: text.to_owned(),
        agent: None,
    };
    let at = |minutes: u64| UNIX_EPOCH + Duration::from_millis(RECORDED_ON_MS + minutes * 60_000);
    let cleared = store.create_session().expect("a session is created");
    let stopped = store.create_session().expect("a session is created");
    let after = store.create_session().expect("a session is created");
    let record = |session, events: Vec<(Event, u64)>| {
        for (event, minutes) in events {
            store
                .append_at(session, &event, at(minutes))
                .expect("an append succeeds");
        }
    };
    record(
        cleared,
        vec![
            (prompt("add etags"), 0),
            (said("Done."), 1),
            (Event::TurnEnded, 1),
            (prompt("/clear"), 3),
        ],
    );
    record(stopped, vec![(prompt("rename the module"), 2)]);
    record(
        after,
        vec![
            (Event::Cleared, 3),
            (Event::TurnEnded, 3),
            (prompt("now the docs"), 4),
            (said("Done."), 5),
            (Event::TurnEnded, 5),
        ],
    );

    let page = printed(&root);

    assert!(
        page.contains(
            "## Left open\n\n\
             - Session 2: stopped in the middle of a turn — `niobe --resume 2`\n\
             - Whether what the sessions changed is committed could not be read from git.\n"
        ),
        "{page}"
    );
}
