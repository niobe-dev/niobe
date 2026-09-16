// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The `niobe` binary, run as a process.
//!
//! Standard output is a pipe here, so `replay` and `--resume` print the fold
//! instead of opening the shell. That is what lets a test compare a session
//! read back from the store with the log it was recorded from, through the
//! same binary an operator runs.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use niobe_store::{Store, read_log};

const FIXTURE: &str = "../niobe-core/tests/fixtures/session-200.jsonl";

/// The fixture must be read and folded in under this many milliseconds.
const REPLAY_BUDGET_MS: f64 = 50.0;

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)
}

fn niobe(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_niobe"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("the niobe binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr is UTF-8")
}

/// A directory that is the root of a repository, with the fixture recorded as
/// its first session.
fn repo_with_the_fixture_recorded() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    std::fs::create_dir(dir.path().join(".git")).expect("a .git directory can be made");
    std::fs::create_dir(dir.path().join(".niobe")).expect("a .niobe directory can be made");

    let store = Store::open(&dir.path().join(".niobe").join("sessions.db")).expect("store");
    let session = store.create_session().expect("a session is created");
    let text = std::fs::read_to_string(fixture_path()).expect("the fixture reads");
    for event in read_log(&text).expect("the fixture parses") {
        store.append(session, &event).expect("an append succeeds");
    }
    dir
}

/// The summary without its first line, which names where the events came from
/// and how long they took.
fn body(summary: &str) -> Vec<&str> {
    summary.lines().skip(1).collect()
}

#[test]
fn replaying_the_fixture_prints_its_totals_inside_the_budget() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = fixture_path();
    let output = niobe(here, &["replay", fixture.to_str().expect("a UTF-8 path")]);
    let out = stdout(&output);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(out.contains("122,554"), "{out}");
    assert!(out.contains("≥$2.47"), "{out}");
    assert!(
        out.contains("4 of 14 usage records reported no cost"),
        "{out}"
    );

    let millis: f64 = out
        .lines()
        .next()
        .and_then(|header| header.split(" in ").nth(1))
        .and_then(|rest| rest.strip_suffix(" ms"))
        .and_then(|ms| ms.parse().ok())
        .unwrap_or_else(|| panic!("no timing in the header: {out}"));
    assert!(
        millis < REPLAY_BUDGET_MS,
        "replay took {millis} ms, over the {REPLAY_BUDGET_MS} ms budget"
    );
}

#[test]
fn a_resumed_session_shows_the_totals_the_log_it_recorded_folds_to() {
    let repo = repo_with_the_fixture_recorded();
    let fixture = fixture_path();

    let resumed = niobe(repo.path(), &["--resume", "1"]);
    let replayed = niobe(
        repo.path(),
        &["replay", fixture.to_str().expect("a UTF-8 path")],
    );

    assert!(resumed.status.success(), "{}", stderr(&resumed));
    let resumed = stdout(&resumed);
    assert!(resumed.starts_with("session 1 · 200 events"), "{resumed}");
    assert_eq!(body(&resumed), body(&stdout(&replayed)));
}

#[test]
fn the_session_list_shows_what_the_store_holds() {
    let repo = repo_with_the_fixture_recorded();
    let output = niobe(repo.path(), &["sessions"]);
    let out = stdout(&output);

    assert!(output.status.success(), "{}", stderr(&output));
    let row = out.lines().nth(1).unwrap_or_default();
    assert!(row.trim_start().starts_with("1 "), "{out}");
    assert!(row.contains(" 200 "), "{out}");
    assert!(row.contains("turn 1: keep going on the etag work"), "{out}");
}

#[test]
fn the_session_list_is_the_same_from_a_subdirectory_of_the_repository() {
    let repo = repo_with_the_fixture_recorded();
    let nested = repo.path().join("src").join("deep");
    std::fs::create_dir_all(&nested).expect("a nested directory can be made");

    let output = niobe(&nested, &["sessions"]);
    assert!(stdout(&output).contains("turn 1:"), "{}", stdout(&output));
    assert!(!nested.join(".niobe").exists());
}

#[test]
fn resuming_a_session_that_does_not_exist_says_which_and_fails() {
    let repo = repo_with_the_fixture_recorded();
    let output = niobe(repo.path(), &["--resume", "9"]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("no session 9"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn listing_sessions_where_nothing_was_recorded_creates_nothing() {
    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    std::fs::create_dir(dir.path().join(".git")).expect("a .git directory can be made");

    let output = niobe(dir.path(), &["sessions"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).starts_with("no sessions recorded"),
        "{}",
        stdout(&output)
    );
    assert!(!dir.path().join(".niobe").exists());
}

#[test]
fn a_log_with_a_bad_line_names_the_line() {
    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    let log = dir.path().join("bad.jsonl");
    std::fs::write(
        &log,
        "{\"type\":\"user_message\",\"text\":\"hi\"}\nnot json\n",
    )
    .expect("the log is written");

    let output = niobe(dir.path(), &["replay", "bad.jsonl"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("line 2"), "{}", stderr(&output));
}
