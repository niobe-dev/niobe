// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The fold read as a state machine: thousands of random event sequences a
//! backend could plausibly produce — ends said twice or late, exits with no
//! spawn, a clear in the middle of a turn, a session left and taken up again —
//! folded one event at a time, with the figures every pane derives from
//! checked after each against what must hold whatever arrived. Background
//! commands and sub-agents run across turns and clears, usage lands after a
//! turn's end, prompts reach no backend, and a session is left, or ends on a
//! fatal error, between any two events.
//!
//! The expected figures are kept by a model in this file, counted from the
//! events themselves, so a fold that drifts cannot agree with itself. The
//! sequences are drawn from a seeded generator, so a failure names the seed
//! that reproduces it.

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

/// How many kinds of event [`event`] draws from.
const KINDS: u64 = 40;

fn event(rng: &mut Rng, kinds: &[u64]) -> Event {
    let call = ToolCallId::new(rng.pick(CALLS));
    match kinds[rng.below(kinds.len() as u64) as usize] {
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
                answers: Vec::new(),
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
        28 => match rng.below(4) {
            0 => Event::ModeSelected { mode: Mode::Plan },
            1 => Event::ModeUnknown,
            2 => Event::TurnFollows,
            _ => Event::Looked,
        },
        29 => Event::Notice {
            message: "note".to_owned(),
        },
        30 | 31 => Event::BackgroundCommandStarted { id: call },
        32 | 33 => Event::BackgroundCommandEnded { id: call },
        34 => Event::PromptNotSent {
            text: "go".to_owned(),
            why: None,
        },
        35 => Event::TurnFollows,
        36 => Event::CompactionEnded,
        37 => Event::SessionLeft,
        38 => Event::Error {
            message: "boom".to_owned(),
            fatal: true,
        },
        _ => Event::Cleared,
    }
}

/// A stream drawn from every kind of event, or, for two seeds in three, from
/// a few kinds and the prompt, end and usage that make turns of them: the
/// shapes a rare kind makes only beside another rare one — a clear while a
/// sub-agent runs and the turn it ends — are drawn often enough to be met.
fn sequence(seed: u64, len: usize) -> Vec<Event> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let kinds: Vec<u64> = match seed % 3 {
        0 => (0..KINDS).collect(),
        _ => (0..KINDS)
            .filter(|_| rng.chance(30))
            .chain([0, 11, 14])
            .collect(),
    };
    (0..len).map(|_| event(&mut rng, &kinds)).collect()
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
    /// Tokens per model since the last clear, as the records named it.
    by_model: std::collections::BTreeMap<String, u64>,
    /// How the session was last said to be billed, which a clear keeps.
    billing: Option<Billing>,
    /// Whether a sub-agent wrote or called anything since the last record
    /// that settled a cost which is a sum of money, or since the last clear.
    agent_unbilled: bool,
    /// Whether a process ended with an agent's work unbilled, which no later
    /// bill takes in, since the last clear.
    agent_stranded: bool,
    /// Per model, the records that reported no money and no settlement has
    /// covered since: how many a process that ended left, and how many in all.
    owed: std::collections::BTreeMap<String, (u64, u64)>,
    /// Tokens folded while the session was last said to be billed by use.
    metered: u64,
    /// The last request's prompt and model, or a compaction's floor.
    context: Option<(u64, String)>,
    /// The five-hour window as last reported, which a clear keeps.
    five_hour: Option<UsageWindow>,
    /// The window where the running turn began, where a prompt opened it.
    began_at: Option<Option<UsageWindow>>,
    /// The window where the last turn ended.
    ended_at: Option<UsageWindow>,
    /// Whether any window was reported since the running turn began or the
    /// last one ended.
    window_said: bool,
    /// Whether the session was left since the five-hour window was last
    /// reported, which a clear keeps: no turn is measured from that level.
    left: bool,
    /// Lines added and removed and changes made, per project file.
    lines: std::collections::BTreeMap<String, (u64, u64, u64)>,
    /// The calls running, each with its tool and the agent that made it.
    running_calls: std::collections::BTreeMap<ToolCallId, (String, Option<AgentId>)>,
    /// The sub-agents running.
    running_agents: std::collections::BTreeSet<AgentId>,
    /// What started the last compaction since the last turn was recorded.
    compacted: Option<CompactTrigger>,
}

fn tokens(usage: &Usage) -> u64 {
    usage.input + usage.output + usage.cache_read + usage.cache_write + usage.reasoning
}

impl Model {
    /// Moves the model on by `event`, given the fold as it was before it.
    fn before(&mut self, before: &SessionState, event: &Event) {
        match event {
            Event::Cleared => {
                let agents = std::mem::take(&mut self.running_agents);
                let mut calls = std::mem::take(&mut self.running_calls);
                calls.retain(|_, (name, agent)| {
                    name == OPERATOR_SHELL || agent.as_ref().is_some_and(|a| agents.contains(a))
                });
                *self = Model {
                    billing: self.billing,
                    five_hour: self.five_hour,
                    left: self.left,
                    running_calls: calls,
                    running_agents: agents,
                    ..Model::default()
                };
                self.clear_open = true;
            }
            Event::Billing { billing } => self.billing = Some(*billing),
            Event::Context(context) => {
                self.context = Some((context.tokens, context.model.clone()));
            }
            Event::Compacted { after, trigger, .. } => {
                self.compacted = Some(*trigger);
                let last = self.context.take();
                self.context = last.zip(*after).map(|((_, model), after)| (after, model));
            }
            Event::UsageWindows(windows) if !windows.is_empty() => {
                self.five_hour = windows.five_hour;
                self.window_said = true;
                self.left &= windows.five_hour.is_none();
            }
            Event::AgentExit { id, .. } if self.ended.contains(id) => {}
            Event::AgentExit { id, .. } => {
                if !before.running_agents().contains(id) {
                    self.orphan_exits += 1;
                }
                self.ended.insert(id.clone());
                self.running_agents.remove(id);
                self.running_calls
                    .retain(|_, (_, agent)| agent.as_ref() != Some(id));
            }
            Event::AgentSpawn { id, .. } => {
                self.ended.remove(id);
                self.running_agents.insert(id.clone());
            }
            Event::AssistantMessage { agent: Some(_), .. } => self.agent_unbilled = true,
            Event::ToolCallStart {
                id, name, agent, ..
            } => {
                self.agent_unbilled |= agent.is_some();
                let agent = agent
                    .clone()
                    .or_else(|| self.running_calls.get(id).and_then(|(_, a)| a.clone()));
                self.running_calls.insert(id.clone(), (name.clone(), agent));
            }
            Event::ToolCallEnd { id, .. } => {
                self.running_calls.remove(id);
            }
            Event::SessionLeft | Event::Error { fatal: true, .. } => {
                self.running_calls
                    .retain(|_, (name, _)| name == OPERATOR_SHELL);
                self.running_agents.clear();
                self.ended.extend(before.running_agents().iter().cloned());
                self.clear_open = false;
                self.left |= matches!(event, Event::SessionLeft);
                self.agent_stranded |= self.agent_unbilled;
                for (stranded, all) in self.owed.values_mut() {
                    *stranded = *all;
                }
            }
            Event::TurnEnded => {
                self.clear_open = false;
                self.close_turn_calls();
            }
            Event::UserMessage { .. } if !before.turn_running() && !self.clear_open => {
                self.began_at = Some(self.five_hour.filter(|_| !self.left));
                self.window_said = false;
                self.no_turn += self.between;
                self.between = 0;
                self.in_turn = 0;
            }
            Event::Usage(usage) => {
                let money = usage
                    .cost_usd
                    .is_some_and(|cost| cost.is_finite() && cost >= 0.0);
                if usage.settles_model == Some(true) && money {
                    self.agent_unbilled = self.agent_stranded;
                    if let Some((stranded, all)) = self.owed.get_mut(&usage.model) {
                        *all = *stranded;
                    }
                }
                if !money {
                    self.owed.entry(usage.model.clone()).or_default().1 += 1;
                }
                *self.by_model.entry(usage.model.clone()).or_default() += tokens(usage);
                if self.billing == Some(Billing::Metered) {
                    self.metered += tokens(usage);
                }
                self.usage_records += 1;
                match before.turn_running() {
                    true => self.in_turn += tokens(usage),
                    false => self.between += tokens(usage),
                }
            }
            Event::FileChange {
                path,
                added,
                removed,
                scope: ChangeScope::Project,
                ..
            } => {
                self.project_changes += 1;
                let file = self.lines.entry(path.clone()).or_default();
                file.0 += added.unwrap_or(0);
                file.1 += removed.unwrap_or(0);
                file.2 += 1;
            }
            _ => {}
        }
    }

    /// Drops the calls an ended turn leaves no one to end, whether or not it
    /// is recorded as a turn: all but the operator's commands and the calls
    /// of sub-agents still running.
    fn close_turn_calls(&mut self) {
        let agents = &self.running_agents;
        self.running_calls.retain(|_, (name, agent)| {
            name == OPERATOR_SHELL || agent.as_ref().is_some_and(|a| agents.contains(a))
        });
    }

    /// Takes in a turn the fold has just recorded, and says what it spent.
    fn turn_recorded(&mut self) -> u64 {
        self.close_turn_calls();
        let spent = self.between + self.in_turn;
        self.between = 0;
        self.in_turn = 0;
        spent
    }

    /// Takes in the window share of a turn the fold has just recorded, and
    /// says what it should be: the level at its end less the level where it
    /// began, of one window, where one was reported in between.
    fn share_recorded(&mut self) -> Option<f64> {
        let began = self.began_at.take().unwrap_or(self.ended_at);
        let ended = self.five_hour;
        let share = match (self.window_said, began, ended) {
            (true, Some(before), Some(after))
                if before.resets_at.is_some()
                    && before.resets_at == after.resets_at
                    && after.utilization >= before.utilization =>
            {
                Some(after.utilization - before.utilization)
            }
            _ => None,
        };
        self.ended_at = ended.filter(|_| !self.left);
        self.window_said = false;
        share
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
    let owed: u64 = model.owed.values().map(|(_, all)| all).sum();
    assert_eq!(
        totals.records_unsettled,
        owed,
        "owed by the events, {}",
        why()
    );
    assert_eq!(
        totals.cost_fully_reported(),
        totals.records_unsettled == 0 && !model.agent_unbilled,
        "whole while an agent's work is unbilled, {}",
        why()
    );
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
        tools.denied_by_name.values().sum::<u64>(),
        tools.denied,
        "{}",
        why()
    );
    assert_eq!(
        tools.stopped_by_name.values().sum::<u64>(),
        tools.stopped,
        "{}",
        why()
    );
    assert_eq!(
        tools.by_work.values().map(|w| w.stopped).sum::<u64>(),
        tools.stopped,
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
    for file in state.files() {
        assert_eq!(
            Some(&(file.added, file.removed, file.changes)),
            model.lines.get(&file.path),
            "lines of {}, {}",
            file.path,
            why()
        );
    }
    assert_eq!(state.files().len(), model.lines.len(), "files, {}", why());
    assert_eq!(
        state.running_agents(),
        &model.running_agents,
        "running agents, {}",
        why()
    );
    assert_eq!(
        state.in_flight_tools().keys().collect::<Vec<_>>(),
        model.running_calls.keys().collect::<Vec<_>>(),
        "running calls, {}",
        why()
    );
    assert_eq!(
        totals.tokens_by_model,
        model.by_model,
        "tokens per model, {}",
        why()
    );
    assert_eq!(
        state.metered_totals().tokens(),
        model.metered,
        "metered tokens, {}",
        why()
    );
    assert_eq!(
        state.context().map(|c| (c.tokens, c.model.clone())),
        model.context,
        "context, {}",
        why()
    );
    assert!(
        state.pending_permissions().len() as u64 <= state.permission_requests(),
        "{}",
        why()
    );
}

/// What the events say is open, and how many turns they can have made, since
/// the last clear.
#[derive(Default)]
struct Open {
    /// The commands the backend moved to the background and has not said
    /// ended, which go on across a turn's end and a clear.
    background: std::collections::BTreeSet<ToolCallId>,
    /// The agent each call was last started by, where one was.
    callers: std::collections::BTreeMap<ToolCallId, Option<AgentId>>,
    /// Ends of any kind — the backend's, the process leaving, a fatal error.
    ends: usize,
    /// Prompts that opened a turn: sent while none ran or was clearing.
    prompted: usize,
    /// Whether the event being folded ends a turn that was running or
    /// clearing, rather than repeating an end.
    ending: bool,
}

/// The counts that only a clear starts over.
fn counters(state: &SessionState) -> [u64; 12] {
    let tools = state.tools();
    [
        state.totals().tokens(),
        state.totals().records,
        state.turns().len() as u64,
        tools.started,
        tools.finished,
        state.agents_spawned(),
        state.agents_completed()
            + state.agents_failed()
            + state.agents_cancelled()
            + state.agents_interrupted(),
        state.errors(),
        state.user_messages(),
        state.permission_requests(),
        state.files().iter().map(|f| f.changes).sum(),
        state.decisions().len() as u64,
    ]
}

impl Open {
    fn before(&mut self, before: &SessionState, event: &Event) {
        self.ending =
            matches!(event, Event::TurnEnded) && (before.turn_running() || before.clearing());
        match event {
            Event::Cleared => {
                let background = std::mem::take(&mut self.background);
                *self = Open {
                    background,
                    callers: std::mem::take(&mut self.callers),
                    ..Open::default()
                };
            }
            Event::ToolCallStart { id, agent, .. } => {
                if agent.is_some() || !before.in_flight_tools().contains_key(id) {
                    self.callers.insert(id.clone(), agent.clone());
                }
            }
            Event::BackgroundCommandStarted { id } => {
                self.background.insert(id.clone());
            }
            Event::BackgroundCommandEnded { id } => {
                self.background.remove(id);
            }
            Event::UserMessage { .. } if !before.turn_running() && !before.clearing() => {
                self.prompted += 1;
            }
            _ => {}
        }
        if matches!(
            event,
            Event::TurnEnded | Event::SessionLeft | Event::Error { fatal: true, .. }
        ) {
            self.ends += 1;
        }
        if matches!(event, Event::SessionLeft | Event::Error { fatal: true, .. }) {
            self.background.clear();
        }
    }

    /// Whether `id` is a call a sub-agent still running made.
    fn agents(&self, state: &SessionState, id: &ToolCallId) -> bool {
        self.callers
            .get(id)
            .cloned()
            .flatten()
            .is_some_and(|agent| state.running_agents().contains(&agent))
    }

    fn check(&self, why: &str, before: [u64; 12], event: &Event, state: &SessionState) {
        assert_eq!(
            state.background_commands(),
            &self.background,
            "background, {why}"
        );
        if !matches!(event, Event::Cleared) {
            let after = counters(state);
            for (i, (was, is)) in before.iter().zip(after).enumerate() {
                assert!(is >= *was, "counter {i} went from {was} to {is}, {why}");
            }
        }
        assert!(state.turns().len() <= self.ends, "turns no end made, {why}");
        assert!(
            self.prompted
                <= state.turns().len() + usize::from(state.turn_running() || state.clearing()),
            "a prompted turn neither running nor recorded, {why}"
        );
        if matches!(event, Event::SessionLeft | Event::Error { fatal: true, .. }) {
            assert!(!state.turn_running() && !state.clearing(), "{why}");
            assert!(state.running_agents().is_empty(), "{why}");
            assert!(state.pending_permissions().is_empty(), "{why}");
            assert!(
                state
                    .in_flight_tools()
                    .values()
                    .all(|n| n == OPERATOR_SHELL),
                "{why}"
            );
        }
        // Whether or not it was recorded as a turn — the one that typed a
        // clear is not — a turn the backend ended leaves nothing of its own
        // running or waiting: only the operator's commands and what the
        // sub-agents still running have under way.
        if self.ending {
            assert!(!state.turn_running() && !state.clearing(), "{why}");
            for (id, name) in state.in_flight_tools() {
                assert!(
                    name == OPERATOR_SHELL || self.agents(state, id),
                    "call {id:?} running past its turn's end, {why}"
                );
            }
            for id in state.pending_permissions() {
                assert!(
                    state.in_flight_tools().contains_key(id) && self.agents(state, id),
                    "question {id:?} waiting past its turn's end, {why}"
                );
            }
            assert_eq!(state.pending_assistant(), "", "{why}");
            assert!(!state.compacting(), "{why}");
        }
    }
}

fn fold_checked(seed: u64, events: &[Event]) -> SessionState {
    let mut state = SessionState::new();
    let mut model = Model::default();
    let mut open = Open::default();
    for (at, event) in events.iter().enumerate() {
        model.before(&state, event);
        open.before(&state, event);
        let counted = counters(&state);
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
            let compacted = model.compacted.take();
            assert_eq!(
                state.turns()[turns].compacted,
                compacted,
                "the compaction of the turn recorded at event {at} of seed {seed} ({event:?})"
            );
            assert_eq!(
                state.turns()[turns].cut,
                !matches!(event, Event::TurnEnded),
                "seed {seed} event {at}"
            );
            let share = model.share_recorded();
            assert_eq!(
                state.turns()[turns].five_hour_share,
                share,
                "the window share of the turn recorded at event {at} of seed {seed} ({event:?})"
            );
        }
        // Whatever turn the leave cut was measured to the level before it;
        // a turn after it, prompted or not, is measured from nothing.
        if matches!(event, Event::SessionLeft) {
            model.ended_at = None;
        }
        check(seed, at, event, &state, &model);
        open.check(
            &format!("seed {seed}, after event {at} ({event:?})"),
            counted,
            event,
            &state,
        );
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

/// The store keeps the conversation after a clear as a session of its own
/// that opens on the clear, without the prompt that typed it, so the turns
/// read back from there are the ones the live fold counted after it.
#[test]
fn a_conversation_read_back_from_its_clear_has_the_turns_and_totals_it_had_live() {
    for seed in 0..SEEDS {
        let events = sequence(seed, LENGTH);
        let clears: Vec<usize> = (0..events.len())
            .filter(|&at| matches!(events[at], Event::Cleared))
            .collect();
        for (i, &at) in clears.iter().enumerate() {
            let next = clears.get(i + 1).copied().unwrap_or(events.len());
            let live = SessionState::replay(&events[..next]);
            let read_back = SessionState::replay(&events[at..next]);
            let why = format!("seed {seed}, cleared at event {at} until {next}");
            let turns = |state: &SessionState| {
                state
                    .turns()
                    .iter()
                    .map(|turn| (turn.number, turn.tokens, turn.cut, turn.compacted))
                    .collect::<Vec<_>>()
            };
            let files = |state: &SessionState| {
                state
                    .files()
                    .iter()
                    .map(|f| (f.path.clone(), f.added, f.removed, f.changes))
                    .collect::<Vec<_>>()
            };
            assert_eq!(turns(&read_back), turns(&live), "{why}");
            assert_eq!(read_back.totals().tokens(), live.totals().tokens(), "{why}");
            assert_eq!(read_back.totals().records, live.totals().records, "{why}");
            assert_eq!(read_back.user_messages(), live.user_messages(), "{why}");
            assert_eq!(read_back.errors(), live.errors(), "{why}");
            assert_eq!(read_back.decisions(), live.decisions(), "{why}");
            assert_eq!(files(&read_back), files(&live), "{why}");
            assert_eq!(read_back.clearing(), live.clearing(), "{why}");
            assert_eq!(read_back.fatal_error(), live.fatal_error(), "{why}");
        }
    }
}
