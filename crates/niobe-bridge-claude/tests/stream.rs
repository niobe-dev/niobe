// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A recorded `claude` stream translated and folded.
//!
//! The expected numbers are the ones written into
//! `tests/fixtures/README.md` and derivable from the fixture with `jq`, not
//! numbers this crate produced, so a bug in the translation cannot agree with
//! itself.

use niobe_bridge_claude::Translator;
use niobe_core::event::{AgentOutcome, Backend, CostBasis, Event, PermissionDecision, ToolOutcome};
use niobe_core::session::SessionState;

/// A two-turn session as the CLI prints it.
const FIXTURE: &str = include_str!("fixtures/stream-json.jsonl");

/// Every event the fixture translates to, in order.
fn translated() -> Vec<Event> {
    let mut translator = Translator::new("max");
    FIXTURE
        .lines()
        .filter(|line| !line.trim().is_empty())
        .flat_map(|line| translator.line(line))
        .collect()
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
                assert!(
                    !fatal,
                    "a message the bridge could not read ended the session"
                );
                Some(message.as_str())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_recorded_stream_folds_into_the_totals_the_cli_reported() {
    let state = SessionState::replay(&translated());
    let totals = state.totals();

    // The six message_delta records, plus the tokens `modelUsage` reported for
    // a model that never produced a message of its own.
    assert_eq!(totals.input, 9 + 900);
    assert_eq!(totals.output, 115 + 10);
    assert_eq!(totals.cache_read, 5_700);
    assert_eq!(totals.cache_write, 185);
    assert_eq!(
        totals.reasoning, 0,
        "thinking tokens are a share of the output"
    );

    // Six per-message records with no money on them, and one cost record per
    // model per turn that had something new to report.
    assert_eq!(totals.records, 9);
    assert_eq!(totals.records_without_cost, 6);
    assert!(
        (totals.reported_cost_usd - 0.091).abs() < 1e-9,
        "the session cost {} rather than the 0.091 the CLI reported",
        totals.reported_cost_usd
    );
}

#[test]
fn the_per_message_tokens_add_up_to_what_the_cli_reported_for_the_turn() {
    // The bridge checks this itself on every `result` and says so when it
    // fails, so the absence of that warning is the assertion.
    let events = translated();
    let complaints: Vec<&str> = warnings(&events)
        .into_iter()
        .filter(|w| w.contains("do not add up"))
        .collect();

    assert!(complaints.is_empty(), "{complaints:?}");
}

#[test]
fn usage_is_folded_from_the_stream_events_not_from_the_assistant_snapshot() {
    // `msg_1` carries `usage.output_tokens: 2` on its `assistant` line — the
    // count at the moment that line was written — and finished at 40. Folding
    // the snapshot is the bug this asserts against.
    let events = translated();
    let first = usage_records(&events)
        .first()
        .copied()
        .cloned()
        .expect("the first message produced a usage record");

    assert_eq!(first.output, 40);
    assert_eq!(first.input, 3);
    assert_eq!(first.cache_read, 1_000);
    assert_eq!(first.cache_write, 100);
    assert_eq!(
        first.cache_write_1h, 100,
        "the CLI bought the hour, not the five minutes"
    );
    assert_eq!(first.model, "claude-sonnet-5");
}

#[test]
fn a_plan_backends_cost_is_api_equivalent_and_never_measured() {
    let events = translated();
    let priced: Vec<_> = usage_records(&events)
        .into_iter()
        .filter(|usage| usage.cost_usd.is_some())
        .collect();

    assert_eq!(
        priced.len(),
        3,
        "one per model per turn that cost something new"
    );
    for usage in &priced {
        assert_eq!(
            usage.cost_basis,
            Some(CostBasis::ApiEquivalent),
            "{} was priced as {:?}",
            usage.model,
            usage.cost_basis
        );
    }
    assert!(
        usage_records(&events)
            .iter()
            .all(|usage| usage.cost_basis != Some(CostBasis::Measured)),
        "a figure the CLI computed from list prices was stored as money that moved"
    );
}

#[test]
fn a_session_cost_that_runs_on_is_reported_as_what_the_turn_added() {
    let events = translated();
    let sonnet: Vec<f64> = usage_records(&events)
        .into_iter()
        .filter(|usage| usage.model == "claude-sonnet-5")
        .filter_map(|usage| usage.cost_usd)
        .collect();

    // `modelUsage` ran 0.05 then 0.09 for the session, so the turns cost 0.05
    // and 0.04. Adding the two totals would have reported 0.14.
    assert_eq!(sonnet.len(), 2);
    assert!((sonnet[0] - 0.05).abs() < 1e-9, "{sonnet:?}");
    assert!((sonnet[1] - 0.04).abs() < 1e-9, "{sonnet:?}");
}

#[test]
fn a_message_type_the_bridge_does_not_know_is_a_warning_and_not_a_crash() {
    let events = translated();
    let complaints = warnings(&events);

    assert!(
        complaints
            .iter()
            .any(|w| w.contains("a_message_type_from_a_later_version")),
        "{complaints:?}"
    );
    assert!(
        complaints.iter().any(|w| w.contains("could not read")),
        "the line that is not JSON at all: {complaints:?}"
    );
    assert_eq!(complaints.len(), 2, "{complaints:?}");
}

#[test]
fn what_the_session_ran_comes_from_the_cli_not_from_the_profile() {
    let events = translated();
    let Some(Event::SessionMeta(meta)) = events.first() else {
        panic!("the first event says what is running: {:?}", events.first());
    };

    assert_eq!(meta.backend, Backend::Claude);
    assert_eq!(meta.profile, "max");
    assert_eq!(meta.model, "claude-sonnet-5");
    assert_eq!(meta.backend_session.as_deref(), Some("s-1"));
}

#[test]
fn tool_calls_and_their_results_reach_the_timeline_in_full() {
    let events = translated();
    let state = SessionState::replay(&events);
    let tools = state.tools();

    assert_eq!(tools.started, 3);
    assert_eq!(tools.finished, 3);
    assert_eq!(
        tools.denied, 1,
        "a call the CLI refused read as a tool that broke rather than one that was not allowed"
    );
    assert_eq!(tools.failed, 0);
    assert_eq!(tools.unmatched_ends, 0);
    assert_eq!(tools.by_name.get("Read"), Some(&1));
    assert_eq!(tools.by_name.get("Bash"), Some(&1));
    assert_eq!(tools.by_name.get("Task"), Some(&1));

    let read = events
        .iter()
        .find_map(|event| match event {
            Event::ToolCallEnd { name, .. } if name == "Read" => Some(event.clone()),
            _ => None,
        })
        .expect("the Read call finished");
    let Event::ToolCallEnd {
        input,
        output,
        bytes,
        outcome,
        ..
    } = read
    else {
        panic!("a tool call end");
    };
    assert_eq!(input, r#"{"file_path":"/repo/notes.txt"}"#);
    assert_eq!(output, "line one\nline two");
    assert_eq!(bytes, 17);
    assert_eq!(outcome, ToolOutcome::Ok);
}

#[test]
fn a_task_call_is_a_sub_agent_as_well_as_a_tool_call() {
    let events = translated();
    let state = SessionState::replay(&events);

    assert_eq!(state.agents_spawned(), 1);
    assert_eq!(state.agents_completed(), 1);
    assert!(state.running_agents().is_empty());
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::AgentSpawn { label, .. } if label == "check the tests"
        )),
        "the sub-agent was named by what it was asked to do"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::AgentExit { outcome, .. } if *outcome == AgentOutcome::Completed))
    );
}

#[test]
fn a_call_the_cli_refused_is_recorded_once_though_it_is_reported_twice() {
    // The CLI announces the refusal as it happens and lists it again in the
    // turn's closing `result`. Counting both would double every denial.
    let events = translated();
    let state = SessionState::replay(&events);

    assert_eq!(state.permission_requests(), 1);
    assert_eq!(state.permissions_denied(), 1);
    assert!(events.iter().any(|event| matches!(
        event,
        Event::PermissionResponse { decision, .. } if *decision == PermissionDecision::Deny
    )));
}

#[test]
fn a_compaction_is_a_notice_rather_than_a_failure() {
    let events = translated();
    let notices: Vec<&String> = events
        .iter()
        .filter_map(|event| match event {
            Event::Notice { message } => Some(message),
            _ => None,
        })
        .collect();

    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("compacted"), "{notices:?}");
    assert!(notices[0].contains("41000"), "{notices:?}");
}

#[test]
fn the_transcript_reads_as_the_session_happened() {
    let events = translated();
    let state = SessionState::replay(&events);

    assert_eq!(state.assistant_messages(), 3);
    assert_eq!(state.last_assistant(), Some("Done."));
    assert_eq!(
        state.errors(),
        2,
        "the two unreadable lines, and nothing else"
    );
    assert_eq!(state.fatal_error(), None);
}
