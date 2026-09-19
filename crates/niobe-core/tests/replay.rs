// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Replay: recorded logs folded into [`SessionState`], with the derived
//! totals asserted.
//!
//! The expected numbers below were not produced by this crate. They were
//! derived from the committed fixtures with `jq`, as
//! `crates/niobe-core/tests/fixtures/README.md` records, so a bug in the fold
//! cannot quietly agree with itself.

use std::time::Instant;

use niobe_core::event::{AgentOutcome, Backend, Event, ToolOutcome};
use niobe_core::session::SessionState;

/// A recorded `claude` bridge session, one JSON object per line.
const SESSION: &str = include_str!("fixtures/claude-session.jsonl");

/// The events the shared model defines that the recording above does not
/// carry, because nothing produces them today.
const GAPS: &str = include_str!("fixtures/producer-gaps.jsonl");

/// The recorded session must replay in under this many milliseconds.
///
/// This test stays in the binary it shares with the ones around it, where the
/// shell redraw needed one of its own. Timed on four two-vCPU CI runners, the
/// fold of the 200-event log this replaced took a median of 0.05 ms with those
/// siblings running in parallel and 0.06 ms alone, and the slowest single fold
/// of the 80 timed was 0.18 ms. A budget this far above the measurement cannot
/// be reached by a scheduler taking the core away.
const REPLAY_BUDGET_MS: u128 = 50;

fn events_of(log: &str) -> Vec<Event> {
    log.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line {} is not an Event: {e}", i + 1))
        })
        .collect()
}

fn recorded_events() -> Vec<Event> {
    events_of(SESSION)
}

#[test]
fn the_recorded_session_is_six_hundred_and_thirty_one_events() {
    assert_eq!(recorded_events().len(), 631);
}

#[test]
fn a_recorded_log_replays_into_the_derived_totals() {
    let events = recorded_events();
    let state = SessionState::replay(&events);

    let meta = state.meta().expect("the log opens with session meta");
    assert_eq!(meta.backend, Backend::Claude);
    assert_eq!(meta.profile, "max");

    // Tokens and money, summed from the 25 usage records.
    let totals = state.totals();
    assert_eq!(totals.input, 120);
    assert_eq!(totals.output, 19_606);
    assert_eq!(totals.cache_read, 460_375);
    assert_eq!(totals.cache_write, 82_887);
    // Every cache write a message reported was bought for an hour. The eleven
    // records from the closing `modelUsage` carry no split, so their writes
    // are not counted here.
    assert_eq!(totals.cache_write_1h, 40_054);
    assert_eq!(totals.reasoning, 0);
    assert_eq!(totals.tokens(), 562_988);
    assert_eq!(totals.records, 25);

    // Fourteen of those records are the per-message counts, which the CLI
    // reports without money; the eleven that carry a cost are the ones the
    // closing `result`s priced. The session's cost is therefore a floor, and
    // the state says so rather than presenting it as the bill.
    assert_eq!(totals.records_without_cost, 14);
    assert!(!totals.cost_fully_reported());
    assert!((totals.reported_cost_usd - 0.865_523_95).abs() < 1e-9);

    // Tool calls. Three failed — a test module that does not exist, a shell
    // glob the shell refused, and a sub-agent's probe that exited non-zero —
    // and one the operator refused.
    let tools = state.tools();
    assert_eq!(tools.started, 26);
    assert_eq!(tools.finished, 26);
    assert_eq!(tools.failed, 3);
    assert_eq!(tools.denied, 1);
    assert_eq!(tools.unmatched_ends, 0);
    assert_eq!(tools.output_bytes, 19_154);
    assert_eq!(tools.by_name["Read"], 8);
    assert_eq!(tools.by_name["Bash"], 12);
    assert_eq!(tools.by_name["Agent"], 3);
    assert_eq!(tools.by_name["Edit"], 2);
    assert_eq!(tools.by_name["Write"], 1);
    assert!(state.in_flight_tools().is_empty());

    // Three sub-agents, each launched in the background and each ended by the
    // CLI's word that it stopped; the two asked for in parallel ran together.
    assert_eq!(state.agents_spawned(), 3);
    assert_eq!(state.agents_cancelled(), 0);
    assert_eq!(state.peak_running_agents(), 2);
    assert!(state.running_agents().is_empty());

    // Permissions: eight prompts, one of them refused, all of them answered.
    assert_eq!(state.permission_requests(), 8);
    assert_eq!(state.permissions_denied(), 1);
    assert!(state.pending_permissions().is_empty());

    // Nothing produces a decision or a checkpoint yet, so an ordinary session
    // carries none. `producer-gaps.jsonl` is where the fold's handling of them
    // is held.
    assert!(state.decisions().is_empty());
    assert!(state.checkpoints().is_empty());

    // Messages and the entries the bridge could not read.
    assert_eq!(state.user_messages(), 6);
    assert_eq!(state.assistant_messages(), 15);
    assert_eq!(state.errors(), 28);
    assert_eq!(state.fatal_error(), None);
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
fn the_recorded_session_replays_inside_the_budget() {
    let events = recorded_events();

    let started = Instant::now();
    let state = SessionState::replay(&events);
    let elapsed = started.elapsed();

    assert_eq!(state.totals().records, 25);
    assert!(
        elapsed.as_millis() < REPLAY_BUDGET_MS,
        "replay took {elapsed:?}, over the {REPLAY_BUDGET_MS} ms budget"
    );
}

#[test]
fn every_event_in_the_logs_round_trips_through_its_wire_form() {
    for (name, log) in [("claude-session", SESSION), ("producer-gaps", GAPS)] {
        for (i, event) in events_of(log).iter().enumerate() {
            let json = serde_json::to_string(event).expect("an Event serializes");
            let back: Event = serde_json::from_str(&json).expect("an Event deserializes");
            assert_eq!(*event, back, "{name} event {} did not round trip", i + 1);
        }
    }
}

#[test]
fn the_recording_exercises_the_awkward_cases_a_live_session_has() {
    let events = recorded_events();
    let has = |f: &dyn Fn(&Event) -> bool| events.iter().any(f);

    // A per-message count the CLI reported without money: the session's cost
    // is a floor while any of these are in it.
    assert!(has(
        &|e| matches!(e, Event::Usage(u) if u.cost_usd.is_none())
    ));
    // A model the session was moved to partway through, and one that produced
    // no message of its own and arrived only in the closing `modelUsage` —
    // billed under an id no message in the session ever named.
    assert!(has(
        &|e| matches!(e, Event::Usage(u) if u.model == "claude-haiku-4-5-20251001")
    ));
    assert!(has(
        &|e| matches!(e, Event::Usage(u) if u.model == "claude-opus-5[1m]")
    ));
    assert!(has(&|e| matches!(e, Event::ModelSelected { .. })));

    assert!(has(
        &|e| matches!(e, Event::ToolCallEnd { outcome, .. } if *outcome == ToolOutcome::Denied)
    ));
    assert!(has(
        &|e| matches!(e, Event::ToolCallEnd { outcome, .. } if *outcome == ToolOutcome::Failed)
    ));
    assert!(has(&|e| matches!(e, Event::Error { fatal: false, .. })));
    assert!(has(&|e| matches!(e, Event::UsageWindows(_))));
    assert!(has(&|e| matches!(e, Event::FileChange { .. })));
}

#[test]
fn the_gaps_log_exercises_what_no_producer_emits() {
    let events = events_of(GAPS);
    let state = SessionState::replay(&events);

    // A `tool_call_end` whose start the producer dropped is counted, not
    // swallowed: a lossy recording is not a session with fewer calls in it.
    assert_eq!(state.tools().unmatched_ends, 1);
    assert_eq!(state.tools().finished, 1);
    assert_eq!(state.tools().started, 0);

    assert_eq!(state.decisions().len(), 1);
    assert_eq!(state.checkpoints().len(), 1);

    // Two agents, one killed and one still running when the log ends.
    assert_eq!(state.agents_spawned(), 2);
    assert_eq!(state.agents_cancelled(), 1);
    assert_eq!(state.peak_running_agents(), 2);
    assert_eq!(state.running_agents().len(), 1);

    assert_eq!(
        state.fatal_error(),
        Some("the `claude` session ended: exit status 1")
    );
    assert!(events.iter().any(
        |e| matches!(e, Event::AgentExit { outcome, .. } if *outcome == AgentOutcome::Cancelled)
    ));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::AgentSpawn { parent, .. } if parent.is_some()))
    );
}
