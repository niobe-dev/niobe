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
