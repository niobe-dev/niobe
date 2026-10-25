// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The fold read as a state machine: thousands of random event sequences a
//! backend could plausibly produce — ends said twice or late, exits with no
//! spawn, a clear in the middle of a turn, a session left and taken up again —
//! folded one event at a time, with the figures every pane derives from
//! checked after each against what must hold whatever arrived.
//!
//! The expected figures are kept by a model in this file, counted from the
//! events themselves, so a fold that drifts cannot agree with itself. The
//! sequences are drawn from a seeded generator, so a failure names the seed
//! that reproduces it.

use niobe_core::catch_up::{CatchUp, Mark};
use niobe_core::event::{
    AgentId, AgentOutcome, Billing, ChangeScope, CompactTrigger, Context, Event, Mode,
    OPERATOR_SHELL, PermissionDecision, ToolCallId, ToolOutcome, Usage, UsageWindow, UsageWindows,
};
use niobe_core::session::SessionState;

/// A small xorshift generator: the suite takes no dependency for this, and a
/// seed is all a failure needs to be replayed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

const CALLS: &[&str] = &["c1", "c2", "c3", "c4", "c5"];
const AGENTS: &[&str] = &["a1", "a2", "a3"];
const MODELS: &[&str] = &["opus", "sonnet", "haiku"];
const TOOLS: &[&str] = &["Read", "Edit", "Bash", "Task", OPERATOR_SHELL];

fn usage(rng: &mut Rng) -> Usage {
    let cost = match rng.below(6) {
        0..=2 => None,
        3 => Some(0.25),
        4 => Some(f64::NAN),
        _ => Some(1.5),
    };
    let settles = match rng.below(3) {
        0 => None,
        1 => Some(false),
        _ => Some(true),
    };
    let cache_write = rng.below(50);
    Usage {
        input: rng.below(1000),
        output: rng.below(500),
        cache_read: rng.below(2000),
        cache_write,
        cache_write_1h: rng.chance(50).then_some(cache_write / 2),
        reasoning: rng.below(30),
        model: rng.pick(MODELS).to_owned(),
        cost_usd: cost,
        settles_model: settles,
        fast: rng.chance(10),
    }
}

fn windows(rng: &mut Rng) -> UsageWindows {
    let window = |rng: &mut Rng| UsageWindow {
        utilization: rng.below(100) as f64 / 100.0,
        resets_at: rng.chance(80).then_some(1_000 + rng.below(2)),
    };
    UsageWindows {
        five_hour: rng.chance(70).then(|| window(rng)),
        seven_day: rng.chance(50).then(|| window(rng)),
        using_overage: rng.chance(10),
        ..UsageWindows::default()
    }
}

fn agent(rng: &mut Rng) -> Option<AgentId> {
    rng.chance(40).then(|| AgentId::new(rng.pick(AGENTS)))
}

fn event(rng: &mut Rng) -> Event {
    let call = ToolCallId::new(rng.pick(CALLS));
    match rng.below(30) {
        0..=2 => Event::UserMessage {
            text: "go".to_owned(),
        },
        3 => Event::AssistantDelta {
            text: "part".to_owned(),
        },
        4 => Event::AssistantMessage {
            text: "DECISION: keep it\nmore".to_owned(),
            agent: agent(rng),
        },
        5..=7 => Event::ToolCallStart {
            id: call,
            name: rng.pick(TOOLS).to_owned(),
            input: "{}".to_owned(),
            summary: rng.chance(50).then(|| "cargo test".to_owned()),
            agent: agent(rng),
        },
        8..=10 => Event::ToolCallEnd {
            id: call,
            name: rng.pick(TOOLS).to_owned(),
            input: "{}".to_owned(),
            output: "out".to_owned(),
            bytes: rng.below(100),
            outcome: match rng.below(4) {
                0 => ToolOutcome::Ok,
                1 => ToolOutcome::Failed,
                2 => ToolOutcome::Denied,
                _ => ToolOutcome::Interrupted,
            },
            summary: None,
            exit_code: None,
            error: None,
            command: rng.chance(30).then(|| "git status".to_owned()),
            untrimmed: None,
        },
        11..=13 => Event::TurnEnded,
        14..=16 => Event::Usage(usage(rng)),
        17 => Event::UsageWindows(windows(rng)),
        18 => Event::Billing {
            billing: match rng.chance(50) {
                true => Billing::Plan,
                false => Billing::Metered,
            },
        },
        19 => match rng.below(4) {
            0 => Event::Cleared,
            1 => Event::Compacted {
                trigger: CompactTrigger::Auto,
                before: None,
                after: rng.chance(50).then_some(500),
            },
            2 => Event::CompactionStarted,
            _ => Event::Context(Context {
                tokens: rng.below(10_000),
                model: rng.pick(MODELS).to_owned(),
                window: Some(200_000),
            }),
        },
        20 => match rng.below(3) {
            0 => Event::SessionLeft,
            1 => Event::Error {
                message: "boom".to_owned(),
                fatal: true,
            },
            _ => Event::Error {
                message: "hiccup".to_owned(),
                fatal: false,
            },
        },
        21 | 22 => Event::PermissionRequest {
            id: call,
            tool: "Bash".to_owned(),
            input: "{}".to_owned(),
            target: None,
            proposal: None,
            reason: None,
            agent: agent(rng),
        },
        23 => match rng.below(3) {
            0 => Event::PermissionWithdrawn { id: call },
            _ => Event::PermissionResponse {
                id: call,
                decision: match rng.below(3) {
                    0 => PermissionDecision::Allow,
                    1 => PermissionDecision::AllowByRule,
                    _ => PermissionDecision::Deny,
                },
                message: None,
            },
        },
        24 | 25 => Event::AgentSpawn {
            id: AgentId::new(rng.pick(AGENTS)),
            parent: agent(rng),
            kind: None,
            label: "help".to_owned(),
        },
        26 => Event::AgentExit {
            id: AgentId::new(rng.pick(AGENTS)),
            outcome: match rng.below(3) {
                0 => AgentOutcome::Completed,
                1 => AgentOutcome::Failed,
                _ => AgentOutcome::Cancelled,
            },
        },
        27 => Event::FileChange {
            path: rng.pick(&["a.rs", "b.rs"]).to_owned(),
            added: rng.chance(80).then(|| rng.below(20)),
            removed: rng.chance(80).then(|| rng.below(20)),
            hunks: Vec::new(),
            scope: match rng.chance(80) {
                true => ChangeScope::Project,
                false => ChangeScope::AgentMemory,
            },
        },
        28 => match rng.below(3) {
            0 => Event::ModeSelected { mode: Mode::Plan },
            1 => Event::ModeUnknown,
            _ => Event::Looked,
        },
        _ => Event::Notice {
            message: "note".to_owned(),
        },
    }
}

fn sequence(seed: u64, len: usize) -> Vec<Event> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    (0..len).map(|_| event(&mut rng)).collect()
}

/// The figures counted from the events alone, for the conversation since the
/// last clear.
#[derive(Default)]
struct Model {
    usage_records: u64,
    /// Tokens folded while no turn ran and since the last turn ended, which
    /// a turn ended without a prompt is counted from.
    between: u64,
    /// Tokens folded since the prompt that opened the running turn.
    in_turn: u64,
    /// Tokens that fell between turns and were never any turn's.
    no_turn: u64,
    project_changes: u64,
    /// Exits of agents neither running nor already ended, which the fold
    /// counts as agents that ended though nothing said they started.
    orphan_exits: u64,
    /// The agents whose end has been counted, by an exit or by the session
    /// ending under them.
    ended: std::collections::BTreeSet<AgentId>,
    /// Whether the turn that typed the last clear may still be running: no
    /// end of any kind has come since the clear. A prompt sent then joins it.
    clear_open: bool,
}

fn tokens(usage: &Usage) -> u64 {
    usage.input + usage.output + usage.cache_read + usage.cache_write + usage.reasoning
}

impl Model {
    /// Moves the model on by `event`, given the fold as it was before it.
    fn before(&mut self, before: &SessionState, event: &Event) {
        match event {
            Event::Cleared => {
                *self = Model::default();
                self.clear_open = true;
            }
            Event::AgentExit { id, .. }
                if !before.running_agents().contains(id) && !self.ended.contains(id) =>
            {
                self.orphan_exits += 1;
                self.ended.insert(id.clone());
            }
            Event::AgentExit { id, .. } => {
                self.ended.insert(id.clone());
            }
            Event::AgentSpawn { id, .. } => {
                self.ended.remove(id);
            }
            Event::SessionLeft | Event::Error { fatal: true, .. } => {
                self.ended.extend(before.running_agents().iter().cloned());
                self.clear_open = false;
            }
            Event::TurnEnded => self.clear_open = false,
            Event::UserMessage { .. } if !before.turn_running() && !self.clear_open => {
                self.no_turn += self.between;
                self.between = 0;
                self.in_turn = 0;
            }
            Event::Usage(usage) => {
                self.usage_records += 1;
                match before.turn_running() {
                    true => self.in_turn += tokens(usage),
                    false => self.between += tokens(usage),
                }
            }
            Event::FileChange {
                scope: ChangeScope::Project,
                ..
            } => self.project_changes += 1,
            _ => {}
        }
    }

    /// Takes in a turn the fold has just recorded, and says what it spent.
    fn turn_recorded(&mut self) -> u64 {
        let spent = self.between + self.in_turn;
        self.between = 0;
        self.in_turn = 0;
        spent
    }
}

fn check(seed: u64, at: usize, event: &Event, state: &SessionState, model: &Model) {
    let why = || format!("seed {seed}, after event {at} ({event:?})");
    let totals = state.totals();

    assert_eq!(totals.records, model.usage_records, "{}", why());
    let by_model: u64 = totals.tokens_by_model.values().sum();
    assert_eq!(by_model, totals.tokens(), "per-model tokens, {}", why());
    let cost_by_model: f64 = totals.reported_cost_by_model.values().sum();
    assert!(
        (cost_by_model - totals.reported_cost_usd).abs() < 1e-9,
        "per-model cost, {}",
        why()
    );
    let owed: usize = totals.unsettled.values().map(|o| o.records().len()).sum();
    assert_eq!(owed as u64, totals.records_unsettled, "owed, {}", why());
    assert!(
        totals.unsettled.values().all(|o| !o.records().is_empty()),
        "an owed model with no records, {}",
        why()
    );
    assert!(
        totals.cache_write_1h + totals.cache_write_unsplit <= totals.cache_write,
        "one-hour share beyond the writes, {}",
        why()
    );
    assert!(
        state.metered_totals().tokens() <= totals.tokens(),
        "metered beyond the whole, {}",
        why()
    );

    let turned: u64 = state.turns().iter().filter_map(|t| t.tokens).sum();
    assert_eq!(
        turned + model.no_turn + model.between + model.in_turn,
        totals.tokens(),
        "turns plus what no turn holds, {}",
        why()
    );
    for (i, turn) in state.turns().iter().enumerate() {
        assert_eq!(turn.number, i as u64 + 1, "turn numbers, {}", why());
        if let Some(share) = turn.five_hour_share {
            assert!(share >= 0.0, "negative window share, {}", why());
        }
    }

    let tools = state.tools();
    assert_eq!(
        tools.by_name.values().sum::<u64>(),
        tools.finished,
        "{}",
        why()
    );
    assert_eq!(
        tools.by_work.values().map(|w| w.calls).sum::<u64>(),
        tools.finished,
        "{}",
        why()
    );
    assert_eq!(
        tools.failed_by_name.values().sum::<u64>(),
        tools.failed,
        "{}",
        why()
    );
    assert_eq!(
        tools.by_work.values().map(|w| w.failed).sum::<u64>(),
        tools.failed,
        "{}",
        why()
    );
    assert_eq!(
        tools.succeeded() + tools.failed + tools.denied + tools.stopped,
        tools.finished,
        "outcomes, {}",
        why()
    );
    assert_eq!(
        tools.started + tools.unmatched_ends,
        tools.finished + state.in_flight_tools().len() as u64 + tools.interrupted,
        "every call started is running, finished or cut, {}",
        why()
    );

    let ended = state.agents_completed()
        + state.agents_failed()
        + state.agents_cancelled()
        + state.agents_interrupted();
    assert!(
        state.running_agents().len() as u64 <= state.agents_spawned(),
        "{}",
        why()
    );
    assert!(
        state.peak_running_agents() >= state.running_agents().len() as u64,
        "{}",
        why()
    );
    assert_eq!(
        ended + state.running_agents().len() as u64,
        state.agents_spawned() + model.orphan_exits,
        "every agent spawned is running or ended once, {}",
        why()
    );

    let changes: u64 = state.files().iter().map(|f| f.changes).sum();
    assert_eq!(changes, model.project_changes, "file changes, {}", why());
    assert!(
        state.pending_permissions().len() as u64 <= state.permission_requests(),
        "{}",
        why()
    );
}

fn fold_checked(seed: u64, events: &[Event]) -> SessionState {
    let mut state = SessionState::new();
    let mut model = Model::default();
    for (at, event) in events.iter().enumerate() {
        model.before(&state, event);
        let turns = state.turns().len();
        let cleared = matches!(event, Event::Cleared);
        state.apply(event);
        if !cleared && state.turns().len() > turns {
            assert_eq!(state.turns().len(), turns + 1, "seed {seed}, event {at}");
            let spent = model.turn_recorded();
            let recorded = state.turns()[turns].tokens.unwrap_or(0);
            assert_eq!(
                recorded, spent,
                "the turn recorded at event {at} of seed {seed} ({event:?})"
            );
        }
        check(seed, at, event, &state, &model);
    }
    state
}

const SEEDS: u64 = 3_000;
const LENGTH: usize = 120;

#[test]
fn every_figure_holds_after_every_event_of_a_plausible_stream() {
    for seed in 0..SEEDS {
        fold_checked(seed, &sequence(seed, LENGTH));
    }
}

#[test]
fn a_fold_resumed_from_any_point_ends_where_one_fold_does() {
    for seed in 0..SEEDS / 4 {
        let events = sequence(seed, LENGTH);
        let whole = SessionState::replay(&events);
        let split = (seed as usize * 7) % events.len();
        let mut resumed = SessionState::replay(&events[..split]).clone();
        for event in &events[split..] {
            resumed.apply(event);
        }
        // Compared as printed: a cost that is not a number, which the fold
        // must take as unreported, is kept in the records as it came, and
        // `NaN` is never equal to itself.
        assert_eq!(
            format!("{resumed:?}"),
            format!("{whole:?}"),
            "seed {seed} split at {split}"
        );
    }
}

#[test]
fn what_happened_since_a_look_is_what_the_events_after_it_say() {
    for seed in 0..SEEDS {
        let events = sequence(seed, LENGTH);
        let split = (seed as usize * 13) % events.len();
        let after = &events[split..];
        if after.iter().any(|e| matches!(e, Event::Cleared)) {
            continue;
        }
        let looked = SessionState::replay(&events[..split]);
        let now = SessionState::replay(&events);
        let since = CatchUp::since(&Mark::of(&looked), &now);
        let errors = after
            .iter()
            .filter(|e| matches!(e, Event::Error { .. }))
            .count() as u64;
        let asked = after
            .iter()
            .filter(|e| matches!(e, Event::PermissionRequest { .. }))
            .count() as u64;
        assert_eq!(since.errors, errors, "seed {seed} split at {split}");
        assert_eq!(since.asked, asked, "seed {seed} split at {split}");
        assert_eq!(
            since.turns.len(),
            now.turns().len() - looked.turns().len(),
            "seed {seed}"
        );
    }
}

/// The store keeps the conversation after a clear as a session of its own
/// that opens on the clear, without the prompt that typed it, so the turns
/// read back from there are the ones the live fold counted after it.
#[test]
fn a_conversation_read_back_from_its_clear_has_the_turns_and_totals_it_had_live() {
    for seed in 0..SEEDS {
        let events = sequence(seed, LENGTH);
        let Some(at) = events.iter().position(|e| matches!(e, Event::Cleared)) else {
            continue;
        };
        let live = SessionState::replay(&events);
        let read_back = SessionState::replay(&events[at..]);
        let turns = |state: &SessionState| {
            state
                .turns()
                .iter()
                .map(|turn| (turn.number, turn.tokens, turn.cut))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            turns(&read_back),
            turns(&live),
            "seed {seed}, cleared at event {at}"
        );
        assert_eq!(
            read_back.totals().tokens(),
            live.totals().tokens(),
            "seed {seed}, cleared at event {at}"
        );
    }
}
