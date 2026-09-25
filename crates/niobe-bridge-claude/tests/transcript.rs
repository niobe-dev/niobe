// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A recorded `claude` transcript listed, folded and priced.
//!
//! The expected numbers are the ones written into
//! `tests/fixtures/README.md` and derivable from the fixture with `jq`, not
//! numbers this crate produced, so a bug in the fold cannot agree with itself.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::path::{Path, PathBuf};

use niobe_bridge_claude::transcript;
use niobe_core::event::{Backend, CostBasis, Event, Mode, SessionMeta, ToolOutcome};
use niobe_core::session::SessionState;
use niobe_core::test_run::TestCounts;

/// The session in the fixture, as the CLI names it.
const SESSION: &str = "2f6c1e10-8f4b-4d2a-9c3e-7a5b0d1e6f42";

/// A directory of transcripts in the shape the CLI writes one.
fn transcripts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcripts")
}

/// Every event the fixture folds to, in order, for a session in `/repo`.
fn folded() -> Vec<Event> {
    transcript::events(
        &transcripts().join(format!("{SESSION}.jsonl")),
        "max",
        Path::new("/repo"),
    )
    .expect("the transcript reads")
}

fn usage_records(events: &[Event]) -> Vec<&niobe_core::Usage> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Usage(usage) => Some(usage),
            _ => None,
        })
        .collect()
}

fn warnings(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Error { message, fatal } => {
                assert!(!fatal, "a record the bridge could not read ended the fold");
                Some(message.as_str())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_transcript_is_listed_by_the_id_that_continues_it_and_by_what_was_asked() {
    let listed = transcript::list(&transcripts()).expect("the directory lists");

    let [only] = listed.as_slice() else {
        panic!("one transcript: {listed:?}");
    };
    assert_eq!(only.id, SESSION);
    assert_eq!(
        only.first_prompt.as_deref(),
        Some("add an etag to the catalog response"),
        "the `/clear` the CLI expanded was listed as though the operator had typed it"
    );
}

#[test]
fn a_transcript_folds_into_what_the_session_said_and_did() {
    let events = folded();
    let said: Vec<&Event> = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::UserMessage { .. }
                    | Event::AssistantMessage { .. }
                    | Event::ToolCallEnd { .. }
                    | Event::FileChange { .. }
            )
        })
        .collect();

    let [first, second, reply, call, change, closing] = said.as_slice() else {
        panic!("the turns, the call and the change: {said:?}");
    };
    // A turn the CLI wrote for itself is still a turn the model was given, so
    // the history carries it; only the session list passes over it.
    assert!(
        matches!(first, Event::UserMessage { text } if text.starts_with("<command-name>/clear")),
    );
    assert_eq!(
        *second,
        &Event::UserMessage {
            text: "add an etag to the catalog response".to_owned()
        }
    );
    assert_eq!(
        *reply,
        &Event::AssistantMessage {
            text: "I will add the header.".to_owned()
        }
    );
    let Event::ToolCallEnd { name, outcome, .. } = call else {
        panic!("the Edit call ended: {call:?}");
    };
    assert_eq!(name, "Edit");
    assert_eq!(*outcome, ToolOutcome::Ok);
    assert_eq!(
        *change,
        &Event::FileChange {
            // Named the way `git` names it, because the fold was told where
            // the session ran.
            path: "catalog/fetch.ts".to_owned(),
            added: Some(3),
            removed: Some(1),
            hunks: Vec::new(),
        }
    );
    assert!(matches!(closing, Event::AssistantMessage { .. }));
}

#[test]
fn a_transcript_is_captioned_by_the_title_the_cli_gave_it() {
    let events = folded();

    let titles: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::Titled { title } => Some(title.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(titles, ["Etag on the catalog response"]);
    assert_eq!(
        SessionState::replay(&events).caption().as_deref(),
        Some("Etag on the catalog response")
    );
}

#[test]
fn a_transcript_says_what_it_ran_as_and_what_it_ran_on() {
    let events = folded();

    assert_eq!(
        events.first(),
        Some(&Event::ModeSelected { mode: Mode::Plan }),
        "{events:?}"
    );
    let meta = events.iter().find_map(|event| match event {
        Event::SessionMeta(meta) => Some(meta),
        _ => None,
    });
    assert_eq!(
        meta,
        Some(&SessionMeta {
            backend: Backend::Claude,
            profile: "max".to_owned(),
            model: "claude-sonnet-5".to_owned(),
            // The id the file is named by, which is the only thing that can
            // hand the conversation back to the CLI.
            backend_session: Some(SESSION.to_owned()),
        })
    );
}

#[test]
fn a_priced_session_is_counted_from_the_accounting_the_cli_closed_it_with() {
    let events = folded();
    let records = usage_records(&events);

    // The session's messages name `claude-sonnet-5` and the CLI billed it as
    // `claude-sonnet-5[1m]`, which is a different rate for the same work. One
    // record per model it billed, and not one per message as well: adding both
    // would report every token of the session twice, and reading one id as the
    // other would be a guess about a price.
    let named: Vec<&str> = records.iter().map(|usage| usage.model.as_str()).collect();
    assert_eq!(named, ["claude-haiku-4-5", "claude-sonnet-5[1m]"]);
    assert!(
        records.iter().all(|usage| usage.cost_usd.is_some()),
        "the CLI closed the session with what it cost, so nothing here is a floor: {records:?}"
    );
}

#[test]
fn an_imported_session_folds_into_the_totals_the_cli_recorded() {
    let state = SessionState::replay(&folded());
    let totals = state.totals();

    // Exactly what the closing `cost-state` holds, across both models.
    assert_eq!(totals.input, 5 + 900);
    assert_eq!(totals.output, 65 + 10);
    assert_eq!(totals.cache_read, 2_100);
    assert_eq!(totals.cache_write, 150);
    assert_eq!(totals.reasoning, 0);

    assert_eq!(totals.records, 2);
    assert_eq!(
        totals.records_unsettled, 0,
        "the CLI recorded what the session cost, so the figure is not a floor"
    );
    assert!(
        (totals.reported_cost_usd - 0.051).abs() < 1e-9,
        "the session cost {} rather than the 0.051 the CLI recorded",
        totals.reported_cost_usd
    );
}

#[test]
fn an_imported_cost_is_api_equivalent_and_never_measured() {
    let events = folded();
    let priced: Vec<_> = usage_records(&events)
        .into_iter()
        .filter(|usage| usage.cost_usd.is_some())
        .collect();

    assert_eq!(priced.len(), 2, "one per model the session spent on");
    for usage in &priced {
        assert_eq!(
            usage.cost_basis,
            Some(CostBasis::ApiEquivalent),
            "{} was priced as {:?}",
            usage.model,
            usage.cost_basis
        );
    }
}

#[test]
fn a_record_this_version_cannot_read_is_shown_and_not_a_lost_history() {
    let events = folded();

    // A record type nobody has read is a notice: the CLI wrote it, and
    // nothing in it says anything failed.
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::Notice { message } if message.contains("teleport")
        )),
        "{events:?}"
    );
    // A line that is not JSON at all is something that could not be read.
    let complaints = warnings(&events);
    assert_eq!(complaints.len(), 1, "{complaints:?}");
    assert!(complaints[0].contains("could not read"), "{complaints:?}");
}

/// A `cargo test --workspace` whose output the CLI saved to a file, as its own
/// transcript records the call and the result: Claude Code 2.1.282, with the
/// records cut down to what the bridge reads, the preview shortened to its
/// first lines and the report's copy of the output left out. `{saved}` and
/// `{size}` stand for where the file is and how many bytes the CLI said it
/// wrote.
const SAVED_RUN: [&str; 2] = [
    r#"{"type":"assistant","message":{"id":"msg_011CfQUVCNkDYs9dh6ZLTxNa","model":"claude-opus-5-5","role":"assistant","content":[{"type":"tool_use","id":"toolu_01JeJeYUUENSCaXAuZhZP2qH","name":"Bash","input":{"command":"cargo test --workspace","description":"Run workspace tests","timeout":600000}}]}}"#,
    r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_01JeJeYUUENSCaXAuZhZP2qH","type":"tool_result","content":"<persisted-output>\nOutput too large (86KB). Full output saved to: {saved}\n\nPreview (first 2KB):\n    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.60s\n     Running unittests src/lib.rs (target/debug/deps/niobe_bridge_claude-6eb70ea3aac9fd8d)\n\nrunning 151 tests\n</persisted-output>","is_error":false}]},"toolUseResult":{"stdout":"","stderr":"","interrupted":false,"isImage":false,"noOutputExpected":false,"persistedOutputPath":"{saved}","persistedOutputSize":{size}}}"#,
];

/// The bytes the CLI reported writing for that run, which is the size of the
/// file it saved.
const SAVED_BYTES: u64 = 88_059;

/// The file the CLI saved that run to, recorded whole.
fn saved_run() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tool-results/cargo-test-workspace.txt")
}

/// The test runs a transcript of [`SAVED_RUN`] reports, with its report
/// naming `saved` and `size`.
fn saved_test_runs(saved: &Path, size: u64) -> Vec<(Option<TestCounts>, Option<i32>)> {
    let dir = tempfile::tempdir().expect("a temporary directory can be made");
    let path = dir
        .path()
        .join("f9e80817-ed99-446a-ad84-62e8d1cc9018.jsonl");
    let saved = saved.to_str().expect("the fixture path is UTF-8");
    let text: String = SAVED_RUN
        .iter()
        .map(|record| {
            record
                .replace("{saved}", saved)
                .replace("{size}", &size.to_string())
                + "\n"
        })
        .collect();
    std::fs::write(&path, text).expect("the temporary directory is writable");

    transcript::events(&path, "max", Path::new("/repo"))
        .expect("the transcript reads")
        .into_iter()
        .filter_map(|event| match event {
            Event::TestRun {
                counts, exit_code, ..
            } => Some((counts, exit_code)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_test_run_the_cli_saved_to_a_file_is_counted_from_the_file() {
    let runs = saved_test_runs(&saved_run(), SAVED_BYTES);

    let counts = TestCounts {
        passed: 1040,
        failed: 0,
        ignored: 0,
        suites: 32,
    };
    assert_eq!(runs, [(Some(counts), Some(0))]);
}

/// A failing `cargo test --workspace` whose output the CLI cut, as its own
/// transcript records the call and the result: Claude Code 2.1.282, with the
/// records cut down to what the bridge reads. `{cut}` stands for the result
/// the CLI handed the model, which is the fixture
/// `cargo-test-workspace-cut.txt`, byte for byte.
const CUT_RUN: [&str; 2] = [
    r#"{"type":"assistant","message":{"id":"msg_011CfQUDWDMVUkNFbtZKYet1","model":"claude-opus-5-5","role":"assistant","content":[{"type":"tool_use","id":"toolu_01Gbjs71jH54b1JBB9hdcxBr","name":"Bash","input":{"command":"cargo test --workspace","description":"Run workspace tests","timeout":600000}}]}}"#,
    r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":{cut},"is_error":true,"tool_use_id":"toolu_01Gbjs71jH54b1JBB9hdcxBr"}]}}"#,
];

/// The test run a transcript of [`CUT_RUN`] reports, with `cut` as the
/// result, as its counts, its exit status and whether it failed.
fn cut_test_runs(cut: &str) -> Vec<(Option<TestCounts>, Option<i32>, bool)> {
    let dir = tempfile::tempdir().expect("a temporary directory can be made");
    let path = dir
        .path()
        .join("762e36f1-b86c-4ff7-a551-056eeb6ece36.jsonl");
    let cut = serde_json::Value::String(cut.to_owned()).to_string();
    let text: String = CUT_RUN
        .iter()
        .map(|record| record.replace("{cut}", &cut) + "\n")
        .collect();
    std::fs::write(&path, text).expect("the temporary directory is writable");

    transcript::events(&path, "max", Path::new("/repo"))
        .expect("the transcript reads")
        .into_iter()
        .filter_map(|event| match event {
            Event::TestRun {
                counts,
                exit_code,
                failed,
                ..
            } => Some((counts, exit_code, failed)),
            _ => None,
        })
        .collect()
}

fn recorded_cut() -> String {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cargo-test-workspace-cut.txt");
    std::fs::read_to_string(path).expect("the fixture is there")
}

#[test]
fn a_failing_run_the_cli_cut_is_failed_and_counts_nothing() {
    let cut = recorded_cut();
    assert_eq!(
        cut.chars().count(),
        10_040,
        "the result the CLI handed over"
    );

    assert_eq!(cut_test_runs(&cut), [(None, Some(101), true)]);
}

#[test]
fn a_cut_run_that_shows_no_test_binary_starting_is_not_failed() {
    // The same result stopped where the first test binary would have
    // started: exit 101 with nothing to say a test ran.
    let cut = recorded_cut();
    let (built, _) = cut
        .split_once("     Running unittests")
        .expect("the recording starts a test binary");

    assert_eq!(cut_test_runs(built), [(None, Some(101), false)]);
}

#[test]
fn a_saved_test_run_whose_file_is_gone_or_changed_is_not_read() {
    let gone = saved_run().with_file_name("b8u0w4de3.txt");

    assert_eq!(saved_test_runs(&gone, SAVED_BYTES), [(None, Some(0))]);
    assert_eq!(
        saved_test_runs(&saved_run(), SAVED_BYTES - 1),
        [(None, Some(0))],
        "the file holds more than the CLI wrote"
    );
}
