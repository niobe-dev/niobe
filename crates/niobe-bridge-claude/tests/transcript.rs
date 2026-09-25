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
