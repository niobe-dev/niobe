// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Replay: a recorded log folded into [`SessionState`], with the derived
//! totals asserted.
//!
//! The expected numbers below were not produced by this crate. They were
//! derived from the committed fixture with `jq`, as
//! `crates/niobe-core/tests/fixtures/README.md` records, so a bug in the fold
//! cannot quietly agree with itself.

use std::time::Instant;

use niobe_core::event::{AgentOutcome, Backend, Event, ToolOutcome};
use niobe_core::session::SessionState;

/// The recorded log, 200 events, one JSON object per line.
const FIXTURE: &str = include_str!("fixtures/session-200.jsonl");

/// The 200-event fixture must replay in under this many milliseconds.
const REPLAY_BUDGET_MS: u128 = 50;

fn recorded_events() -> Vec<Event> {
    FIXTURE
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line {} is not an Event: {e}", i + 1))
        })
        .collect()
}

#[test]
fn the_fixture_is_two_hundred_events() {
    assert_eq!(recorded_events().len(), 200);
}

#[test]
fn a_recorded_log_replays_into_the_derived_totals() {
    let events = recorded_events();
    let state = SessionState::replay(&events);

    let meta = state.meta().expect("the log opens with session meta");
    assert_eq!(meta.backend, Backend::Claude);
    assert_eq!(meta.profile, "default");

    // Tokens and money, summed from the 14 usage records.
    let totals = state.totals();
    assert_eq!(totals.input, 25_500);
    assert_eq!(totals.output, 3_370);
    assert_eq!(totals.cache_read, 86_300);
    assert_eq!(totals.cache_write, 1_560);
    assert_eq!(totals.reasoning, 5_824);
    assert_eq!(totals.tokens(), 122_554);
    assert_eq!(totals.records, 14);

    // Four of those records carried no cost, so the reported figure is a floor
    // and the state says so rather than presenting it as the session's bill.
    assert_eq!(totals.records_without_cost, 4);
    assert!(!totals.cost_fully_reported());
    assert!((totals.reported_cost_usd - 2.47).abs() < 1e-9);

    // Tool calls. One end arrives without its start — a recording of a
    // producer that dropped an event — and is counted rather than swallowed.
    let tools = state.tools();
    assert_eq!(tools.started, 39);
    assert_eq!(tools.finished, 40);
    assert_eq!(tools.failed, 4);
    assert_eq!(tools.denied, 2);
    assert_eq!(tools.unmatched_ends, 1);
    assert_eq!(tools.output_bytes, 92_944);
    assert_eq!(tools.by_name["Read"], 15);
    assert_eq!(tools.by_name["Bash"], 10);
    assert_eq!(tools.by_name["Grep"], 5);
    assert_eq!(tools.by_name["Edit"], 5);
    assert_eq!(tools.by_name["Write"], 5);
    assert!(state.in_flight_tools().is_empty());

    // Permissions, decisions, checkpoints.
    assert_eq!(state.permission_requests(), 10);
    assert_eq!(state.permissions_denied(), 2);
    assert!(state.pending_permissions().is_empty());
    assert_eq!(state.decisions().len(), 8);
    assert_eq!(state.checkpoints().len(), 7);
    assert_eq!(
        state.decisions().last().map(|d| d.summary.as_str()),
        Some("Skipped the image proxy - out of scope, asked first")
    );

    // Sub-agents: three ran at once at the peak, two were still running when
    // the recording was cut.
    assert_eq!(state.agents_spawned(), 8);
    assert_eq!(state.agents_completed(), 3);
    assert_eq!(state.agents_failed(), 2);
    assert_eq!(state.agents_cancelled(), 1);
    assert_eq!(state.peak_running_agents(), 3);
    assert_eq!(state.running_agents().len(), 2);

    // Messages and the error that ended it.
    assert_eq!(state.user_messages(), 13);
    assert_eq!(state.assistant_messages(), 14);
    assert_eq!(state.pending_assistant(), "");
    assert_eq!(
        state.last_assistant(),
        Some("Done. Etags cached in the LRU; 304s short-circuit.")
    );
    assert_eq!(state.errors(), 4);
    assert_eq!(
        state.fatal_error(),
        Some("backend exited after the final turn")
    );
}

#[test]
fn replaying_event_by_event_matches_replaying_the_whole_log() {
    let events = recorded_events();

    let mut incremental = SessionState::new();
    for event in &events {
        incremental.apply(event);
    }

    assert_eq!(incremental, SessionState::replay(&events));
}

#[test]
fn two_hundred_events_replay_inside_the_budget() {
    let events = recorded_events();

    let started = Instant::now();
    let state = SessionState::replay(&events);
    let elapsed = started.elapsed();

    assert_eq!(state.totals().records, 14);
    assert!(
        elapsed.as_millis() < REPLAY_BUDGET_MS,
        "replay took {elapsed:?}, over the {REPLAY_BUDGET_MS} ms budget"
    );
}

#[test]
fn every_event_in_the_log_round_trips_through_its_wire_form() {
    for (i, event) in recorded_events().iter().enumerate() {
        let json = serde_json::to_string(event).expect("an Event serializes");
        let back: Event = serde_json::from_str(&json).expect("an Event deserializes");
        assert_eq!(*event, back, "event {} did not round trip", i + 1);
    }
}

#[test]
fn the_log_exercises_the_awkward_variants() {
    let events = recorded_events();

    let has = |f: &dyn Fn(&Event) -> bool| events.iter().any(f);

    assert!(has(
        &|e| matches!(e, Event::Usage(u) if u.cost_usd.is_none())
    ));
    assert!(has(
        &|e| matches!(e, Event::Usage(u) if u.model == "sonnet-5")
    ));
    assert!(has(
        &|e| matches!(e, Event::ToolCallEnd { outcome, .. } if *outcome == ToolOutcome::Denied)
    ));
    assert!(has(
        &|e| matches!(e, Event::ToolCallEnd { outcome, .. } if *outcome == ToolOutcome::Failed)
    ));
    assert!(has(
        &|e| matches!(e, Event::AgentExit { outcome, .. } if *outcome == AgentOutcome::Cancelled)
    ));
    assert!(has(&|e| matches!(e, Event::Error { fatal: true, .. })));
    assert!(has(
        &|e| matches!(e, Event::AgentSpawn { parent, .. } if parent.is_some())
    ));
}
