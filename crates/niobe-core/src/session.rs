// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Session state, derived from the event stream and from nothing else.
//!
//! [`SessionState`] is a fold over [`Event`]s: feed it a live channel or a
//! recorded log and it ends in the same place. That is what makes the replay
//! test meaningful — the totals the TUI shows are the totals a fixture can
//! assert.
//!
//! What this type does *not* do is price anything. It sums the money backends
//! reported and says which tokens no reported figure covers, so the ledger can
//! label what it derives measured, API-equivalent or unpriced. A number
//! invented here would be indistinguishable from a measured one, which is the
//! product gone.
//!
//! "Covers" is the fold's own bookkeeping, and it is what keeps a fully
//! reported session from reading as a floor forever. A backend that reports
//! tokens per message and money once per turn — the `claude` CLI does — closes
//! the turn with a figure for everything it has billed under that model so
//! far. That record says so ([`crate::event::Usage::settles_model`]), and the
//! records it covers stop being owed for.

use crate::test_run::{self, FailedTests, TestCounts};
use std::collections::{BTreeMap, BTreeSet};

use crate::event::{
    AgentId, AgentOutcome, Billing, CheckpointId, Context, Event, Mode, SessionMeta, SlashCommand,
    ToolCallId, ToolOutcome, Usage, UsageWindow, UsageWindows,
};

/// Token and cost totals, summed from every [`Event::Usage`] in the stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Totals {
    /// Input tokens billed at the full rate.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Input tokens served from the prompt cache.
    pub cache_read: u64,
    /// Input tokens written into the prompt cache.
    pub cache_write: u64,
    /// Of [`Totals::cache_write`], the tokens written to live for an hour
    /// rather than the default five minutes. A share of the writes, not more
    /// of them, so it is never added to [`Totals::tokens`]: providers bill the
    /// hour at a higher rate, and a consumer that prices the session needs the
    /// share to price it at all.
    pub cache_write_1h: u64,
    /// Reasoning tokens.
    pub reasoning: u64,
    /// How many usage records were folded in.
    pub records: u64,
    /// The sum of the costs backends actually reported, in USD.
    pub reported_cost_usd: f64,
    /// How many usage records carried no cost and have not since been settled
    /// by one that did. While this is non-zero,
    /// [`Totals::reported_cost_usd`] is a floor, not the session's cost.
    pub records_unsettled: u64,
    /// The tokens those records carried, summed per model.
    ///
    /// A backend that reports money once per turn leaves the turn priced by
    /// nothing while it runs. These are the tokens no reported figure covers,
    /// in the shape a price table takes, so a consumer that has one can show
    /// what the turn is costing instead of showing nothing. The fold itself
    /// prices nothing: an invented number here would be indistinguishable
    /// from a measured one.
    ///
    /// A model is in the map only while it is owed for, and the record's
    /// `cost_usd` is always `None` — it is what is *not* accounted for.
    pub unsettled: BTreeMap<String, Usage>,
    /// Every token the session spent, summed per model, on the same definition
    /// [`Totals::tokens`] uses — so the map adds up to that total exactly.
    ///
    /// Cumulative, and untouched by settlement: a model whose cost has landed
    /// has not stopped having spent the tokens. That is what separates this
    /// from [`Totals::unsettled`], which empties as money arrives and so can
    /// only answer what is still owed for, never who spent what.
    ///
    /// The key is the model id the backend reported, unaltered. Two ids that
    /// bill apart are two entries, even where they read alike.
    pub tokens_by_model: BTreeMap<String, u64>,
    /// The costs backends reported, summed per model, so that they add up to
    /// [`Totals::reported_cost_usd`].
    ///
    /// A model is in the map only once a cost was reported for it. One that
    /// spent tokens and was billed nothing is absent rather than at zero: what
    /// it cost is unknown, and a consumer prices it or says so.
    pub reported_cost_by_model: BTreeMap<String, f64>,
    /// How many records each model in [`Totals::unsettled`] is owed for, so
    /// that a settlement takes exactly its own model's share back out of
    /// [`Totals::records_unsettled`]. Private because it is the bookkeeping
    /// behind that count rather than a figure of its own.
    unsettled_records: BTreeMap<String, u64>,
}

impl Totals {
    /// Every token, cache traffic included.
    pub fn tokens(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
            .saturating_add(self.reasoning)
    }

    /// Whether a reported cost covers every usage record, i.e. whether
    /// [`Totals::reported_cost_usd`] is the whole bill and may be shown as a
    /// measurement rather than a floor.
    pub fn cost_fully_reported(&self) -> bool {
        self.records_unsettled == 0
    }

    fn add(&mut self, usage: &Usage) {
        self.input = self.input.saturating_add(usage.input);
        self.output = self.output.saturating_add(usage.output);
        self.cache_read = self.cache_read.saturating_add(usage.cache_read);
        self.cache_write = self.cache_write.saturating_add(usage.cache_write);
        self.cache_write_1h = self.cache_write_1h.saturating_add(usage.cache_write_1h);
        self.reasoning = self.reasoning.saturating_add(usage.reasoning);
        self.records += 1;
        self.tokens_by_model
            .entry(usage.model.clone())
            .and_modify(|spent| *spent = spent.saturating_add(usage.tokens()))
            .or_insert_with(|| usage.tokens());

        // A settlement is read before its own cost is taken, so that a record
        // both settling the model and carrying tokens of its own — which the
        // Claude bridge emits for tokens no message reported — leaves nothing
        // owed rather than owing for itself.
        if usage.settles_model && usage.cost_usd.is_some() {
            self.clear_unsettled(&usage.model);
        }

        match usage.cost_usd {
            Some(cost) => {
                self.reported_cost_usd += cost;
                *self
                    .reported_cost_by_model
                    .entry(usage.model.clone())
                    .or_default() += cost;
            }
            None => self.owe(usage),
        }
    }

    /// Records tokens that no reported cost covers.
    fn owe(&mut self, usage: &Usage) {
        self.records_unsettled += 1;
        let owed = self
            .unsettled
            .entry(usage.model.clone())
            .or_insert_with(|| Usage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                cache_write_1h: 0,
                reasoning: 0,
                model: usage.model.clone(),
                cost_usd: None,
                cost_basis: None,
                settles_model: false,
            });
        owed.input = owed.input.saturating_add(usage.input);
        owed.output = owed.output.saturating_add(usage.output);
        owed.cache_read = owed.cache_read.saturating_add(usage.cache_read);
        owed.cache_write = owed.cache_write.saturating_add(usage.cache_write);
        owed.cache_write_1h = owed.cache_write_1h.saturating_add(usage.cache_write_1h);
        owed.reasoning = owed.reasoning.saturating_add(usage.reasoning);
        self.unsettled_records
            .entry(usage.model.clone())
            .and_modify(|n| *n = n.saturating_add(1))
            .or_insert(1);
    }

    /// Forgets what `model` was owed for, because a cost has now covered it.
    fn clear_unsettled(&mut self, model: &str) {
        self.unsettled.remove(model);
        if let Some(covered) = self.unsettled_records.remove(model) {
            self.records_unsettled = self.records_unsettled.saturating_sub(covered);
        }
    }
}

/// Tool call counters, for the tools pane and for waste accounting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolTotals {
    /// Calls that started.
    pub started: u64,
    /// Calls that finished, however they finished.
    pub finished: u64,
    /// Calls that reported failure.
    pub failed: u64,
    /// Calls the operator denied.
    pub denied: u64,
    /// Bytes of tool output, before trimming.
    pub output_bytes: u64,
    /// Finished calls per tool name.
    pub by_name: BTreeMap<String, u64>,
    /// Failed calls per tool name, for a pane that says which tool the
    /// failures belong to. A tool with no failures has no entry rather than a
    /// zero, so a caller cannot read an absent count as a measured none.
    pub failed_by_name: BTreeMap<String, u64>,
    /// Ends that arrived without a matching start. A non-zero count means the
    /// producer is dropping events, so it is surfaced rather than swallowed.
    pub unmatched_ends: u64,
}

/// A recorded `decide` event, kept in order for the changes pane and for the
/// commit and PR bodies it is exported into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRecord {
    /// One line.
    pub summary: String,
    /// The reasoning, where there was more of it.
    pub rationale: Option<String>,
    /// What was considered and rejected.
    pub rejected: Vec<String>,
}

/// What a session did to one file: how much of it changed, and the model's own
/// words about why.
///
/// [`FileChanges::added`] and [`FileChanges::removed`] sum the calls that said
/// how many lines they changed. Where a call did not say,
/// [`FileChanges::added_unstated`] or [`FileChanges::removed_unstated`] counts
/// it and that side of the figure is a floor — the file changed by at least
/// this much, and by how much more the backend did not report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChanges {
    /// The file, as the backend named it.
    pub path: String,
    /// Lines added by the calls that said how many.
    pub added: u64,
    /// Lines removed by the calls that said how many.
    pub removed: u64,
    /// Calls that changed this file without saying how many lines they added.
    pub added_unstated: u64,
    /// Calls that changed this file without saying how many they removed.
    pub removed_unstated: u64,
    /// How many calls changed this file.
    pub changes: u64,
    /// What the model said just before the last change to this file, lifted
    /// from its own prose.
    ///
    /// A heuristic, and shown as one: it is the sentence that happened to
    /// precede the call, not a claim about intent. The most recent one rather
    /// than the first, because the pane redraws as the session runs and the
    /// line beside a file that has just changed should explain the change the
    /// operator watched, not one from ten turns ago.
    pub why: Option<String>,
}

impl FileChanges {
    /// Whether every call that changed this file said how many lines it added,
    /// i.e. whether [`FileChanges::added`] is the whole figure rather than a
    /// floor.
    pub fn added_stated(&self) -> bool {
        self.added_unstated == 0
    }

    /// Whether every call that changed this file said how many lines it
    /// removed.
    pub fn removed_stated(&self) -> bool {
        self.removed_unstated == 0
    }
}

/// A restore point the undo list can return to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRecord {
    /// Names the point.
    pub id: CheckpointId,
    /// What was about to happen when it was taken.
    pub label: String,
}

/// A test run the session made, as it reported itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TestRunRecord {
    /// What its summary counted. `None` where its output did not hold the
    /// whole run, so its result was not read.
    pub counts: Option<TestCounts>,
    /// The status the command exited with, where the backend reported one.
    pub exit_code: Option<i32>,
    /// Whether the run is known to have failed after its tests started,
    /// counted or not. A run whose counts say a test failed, or that named a
    /// failing test, always has.
    pub failed: bool,
    /// The tests each failing binary named, in the order they ran, where
    /// its whole list was left: each is its binary's, and together they are
    /// never known to be the run's whole list.
    pub failures: Vec<FailedTests>,
}

impl TestRunRecord {
    /// The run an [`Event::TestRun`] reported, with `failed` set wherever its
    /// counts say a test failed or it named one, whatever the event said.
    pub fn new(
        counts: Option<TestCounts>,
        exit_code: Option<i32>,
        failed: bool,
        failures: Vec<FailedTests>,
    ) -> Self {
        Self {
            counts,
            exit_code,
            failed: failed || counts.is_some_and(|counts| counts.failing()) || !failures.is_empty(),
            failures,
        }
    }

    /// What a shell command that ran `cargo test` reported, read out of what
    /// it printed, so that every backend reads a run the same way.
    ///
    /// `output` is what the backend handed over, cut or whole; `whole` is the
    /// whole of it, where the backend can tell it has that, which is the only
    /// output the counts are read from. Which tests failed is read from the
    /// whole output where there is one and from what is left otherwise, and
    /// whether the run failed from what is left: the start of a run is what
    /// survives a cut from the end.
    pub fn read(command: &str, output: &str, whole: Option<&str>, exit_code: Option<i32>) -> Self {
        let counts = whole.and_then(|whole| test_run::counts(whole, exit_code));
        let failed = match counts {
            Some(counts) => counts.failing(),
            None => test_run::failed(command, output, exit_code),
        };
        let failures = test_run::failures(whole.unwrap_or(output));
        Self::new(counts, exit_code, failed, failures)
    }
}

/// One finished turn's own figures: what it spent between the prompt that
/// opened it and the end that closed it.
///
/// No time is in here, because the event model carries none: how long a turn
/// took is read off whatever clock the consumer stamped the two ends with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TurnRecord {
    /// Which turn this was, counted from one in the order turns ended.
    pub number: u64,
    /// Every token the turn spent, cache traffic included, on the definition
    /// [`Totals::tokens`] uses: the session's total when the turn ended less
    /// its total when the turn began.
    pub tokens: u64,
    /// How far the five-hour window moved across the turn, as a share of the
    /// window: `0.01` is one percent of it.
    ///
    /// The difference between the level last reported before the turn began
    /// and the level last reported by its end. `None` wherever that is not a
    /// measurement: nothing was reported before the turn, nothing was
    /// reported during it — the level at its end would be the one it began
    /// with, read again — either report left the window out or gave no reset,
    /// the reset moved so the window started over in between, or the level
    /// went down. Never a zero standing in for any of those.
    ///
    /// It is the turn's own to within one request and one point. The `claude`
    /// CLI reports the level with an API response, read from the response's
    /// headers, so a report cannot count the output of the request it came
    /// with: both ends lag by that one request, which shifts a request's
    /// worth from each turn to the next rather than a turn's. And the CLI
    /// reports only when the level moves a whole point, so a turn that moved
    /// it less is reported nothing and has no share, and one that is drawn as
    /// a point may have spent less than a point that crossed one.
    ///
    /// The window is the account's, not the session's: anything else the
    /// account ran during the turn moved it too.
    pub five_hour_share: Option<f64>,
}

/// Where a turn began: what the session had spent, and the window as last
/// reported, at that moment.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct TurnMark {
    tokens: u64,
    five_hour: Option<UsageWindow>,
}

/// Everything derivable from a session's events.
///
/// Built by folding [`SessionState::apply`] over a stream, or in one go with
/// [`SessionState::replay`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionState {
    meta: Option<SessionMeta>,
    mode: Option<Mode>,
    /// What the session is on: what the backend last said it was running, or
    /// the model the operator has since chosen and it has not yet confirmed.
    model: Option<String>,
    totals: Totals,
    /// What the backend last said was left of the plan's usage windows.
    /// `None` until one has said: a metered profile never reports these, and
    /// neither does a backend version that does not emit them.
    usage_windows: Option<UsageWindows>,
    /// How the session is billed, as last reported. `None` until something
    /// has said — never assumed from the backend's name.
    billing: Option<Billing>,
    /// The main agent's last request. `None` until one has been reported.
    context: Option<Context>,
    tools: ToolTotals,
    in_flight_tools: BTreeMap<ToolCallId, String>,
    /// Whether the backend is still answering the last prompt.
    turn_running: bool,
    /// Every turn that has ended, oldest first.
    turns: Vec<TurnRecord>,
    /// Where the running turn began. `None` between turns, and for a turn the
    /// fold never saw begin.
    turn_began: Option<TurnMark>,
    /// Where the last turn ended, which is where a turn that ends without
    /// having been seen to begin is counted from.
    turn_last_ended: TurnMark,
    /// Whether a window has been reported since the last turn began or ended.
    window_reported: bool,
    user_messages: u64,
    /// The last title the backend gave the session.
    title: Option<String>,
    /// The first thing the operator said, which captions a session the
    /// backend gave no title.
    first_prompt: Option<String>,
    assistant_messages: u64,
    pending_assistant: String,
    last_assistant: Option<String>,
    /// What each sub-agent last said, by agent: the why of a file that agent
    /// changes, which the session's own words are not.
    agents_said: BTreeMap<AgentId, String>,
    /// The sub-agent that made each call still running, where one did.
    agent_calls: BTreeMap<ToolCallId, AgentId>,
    /// The sub-agent that made the call that ended last, or `None` for the
    /// session's own. A file change arrives directly after the end of the call
    /// that made it, so this is whose change it is.
    ended_by: Option<AgentId>,
    permission_requests: u64,
    permissions_denied: u64,
    pending_permissions: BTreeSet<ToolCallId>,
    /// One entry per file changed, in the order the session first changed
    /// them. Stable rather than sorted: the pane redraws on every event, and a
    /// list that reorders itself under the operator is a list nobody can read.
    files: Vec<FileChanges>,
    files_at: BTreeMap<String, usize>,
    decisions: Vec<DecisionRecord>,
    checkpoints: Vec<CheckpointRecord>,
    /// The latest test run, which replaces the one before it: an earlier
    /// run's result says nothing about the code as it is now.
    test_run: Option<TestRunRecord>,
    agents_spawned: u64,
    agents_completed: u64,
    agents_failed: u64,
    agents_cancelled: u64,
    running_agents: BTreeSet<AgentId>,
    peak_running_agents: u64,
    errors: u64,
    fatal_error: Option<String>,
    /// The commands the backend last said it runs from a prompt.
    commands: Vec<SlashCommand>,
}

impl SessionState {
    /// An empty session.
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds a whole stream in one call.
    pub fn replay<'a>(events: impl IntoIterator<Item = &'a Event>) -> Self {
        let mut state = Self::new();
        for event in events {
            state.apply(event);
        }
        state
    }

    /// Folds one event in.
    ///
    /// Every case is total: an end without a start, an exit without a spawn, a
    /// response without a request are all counted rather than dropped, because
    /// a silently ignored event is a total that cannot be defended.
    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::SessionMeta(meta) => {
                self.model = Some(meta.model.clone());
                self.meta = Some(meta.clone());
            }

            Event::ModeSelected { mode } => self.mode = Some(*mode),

            // The operator's choice stands until the backend says what it
            // resolved that to, which is the next `SessionMeta`. Showing the
            // model it has moved off instead would tell the operator their
            // choice did not land.
            Event::ModelSelected { model } => self.model = Some(model.clone()),

            // A prompt sent while a turn runs joins it: the turn began with
            // the first one.
            Event::Titled { title } => self.title = Some(title.clone()),

            Event::UserMessage { text } => {
                self.user_messages += 1;
                if self.first_prompt.is_none() {
                    self.first_prompt = Some(text.clone());
                }
                if !self.turn_running {
                    self.turn_began = Some(self.mark());
                    self.window_reported = false;
                }
                self.turn_running = true;
            }

            Event::TurnEnded => self.end_turn(),

            Event::AssistantDelta { text } => self.pending_assistant.push_str(text),

            Event::AssistantMessage { text, agent: None } => {
                self.assistant_messages += 1;
                self.pending_assistant.clear();
                self.last_assistant = Some(text.clone());
            }
            Event::AssistantMessage {
                text,
                agent: Some(agent),
            } => {
                self.agents_said.insert(agent.clone(), text.clone());
            }

            Event::ToolCallStart {
                id, name, agent, ..
            } => {
                self.tools.started += 1;
                self.in_flight_tools.insert(id.clone(), name.clone());
                if let Some(agent) = agent {
                    self.agent_calls.insert(id.clone(), agent.clone());
                }
            }

            Event::ToolCallEnd {
                id,
                name,
                bytes,
                outcome,
                ..
            } => {
                self.tools.finished += 1;
                self.tools.output_bytes = self.tools.output_bytes.saturating_add(*bytes);
                *self.tools.by_name.entry(name.clone()).or_default() += 1;
                match outcome {
                    ToolOutcome::Ok => {}
                    ToolOutcome::Failed => {
                        self.tools.failed += 1;
                        *self.tools.failed_by_name.entry(name.clone()).or_default() += 1;
                    }
                    ToolOutcome::Denied => self.tools.denied += 1,
                }
                if self.in_flight_tools.remove(id).is_none() {
                    self.tools.unmatched_ends += 1;
                }
                self.ended_by = self.agent_calls.remove(id);
            }

            Event::Usage(usage) => self.totals.add(usage),

            // The last report replaces the one before it: a window is a level,
            // not a quantity, so summing two reports of it would be nonsense.
            Event::UsageWindows(windows) => {
                self.usage_windows = Some(*windows);
                self.window_reported = true;
            }

            Event::Billing { billing } => self.billing = Some(*billing),

            // A level again, and the last one stands — including after a
            // compaction, when it drops: a high-water mark would keep showing
            // a context the model no longer has.
            Event::Context(context) => self.context = Some(context.clone()),

            Event::PermissionRequest { id, .. } => {
                self.permission_requests += 1;
                self.pending_permissions.insert(id.clone());
            }

            Event::PermissionResponse { id, decision, .. } => {
                self.pending_permissions.remove(id);
                if !decision.allowed() {
                    self.permissions_denied += 1;
                }
            }

            Event::FileChange {
                path,
                added,
                removed,
                ..
            } => self.change_file(path, *added, *removed),

            Event::Decision {
                summary,
                rationale,
                rejected,
            } => self.decisions.push(DecisionRecord {
                summary: summary.clone(),
                rationale: rationale.clone(),
                rejected: rejected.clone(),
            }),

            Event::Checkpoint { id, label } => self.checkpoints.push(CheckpointRecord {
                id: id.clone(),
                label: label.clone(),
            }),

            Event::TestRun {
                counts,
                exit_code,
                failed,
                failures,
                ..
            } => {
                self.test_run = Some(TestRunRecord::new(
                    *counts,
                    *exit_code,
                    *failed,
                    failures.clone(),
                ));
            }

            Event::AgentSpawn { id, .. } => {
                self.agents_spawned += 1;
                self.running_agents.insert(id.clone());
                let running = self.running_agents.len() as u64;
                self.peak_running_agents = self.peak_running_agents.max(running);
            }

            // A sub-agent's own figures are what a pane draws beside it. Its
            // context size is not a spend, and its tokens are already in the
            // session's usage records, so nothing here counts them again.
            Event::AgentProgress { .. } => {}

            Event::AgentExit { id, outcome } => {
                self.running_agents.remove(id);
                match outcome {
                    AgentOutcome::Completed => self.agents_completed += 1,
                    AgentOutcome::Failed => self.agents_failed += 1,
                    AgentOutcome::Cancelled => self.agents_cancelled += 1,
                }
            }

            Event::Error { message, fatal } => {
                self.errors += 1;
                if *fatal {
                    self.fatal_error = Some(message.clone());
                    // The turn it cut short spent what it spent, and is
                    // recorded with it; with no turn running there is none
                    // to end.
                    if self.turn_running {
                        self.end_turn();
                    }
                }
            }

            // A notice changes nothing that is counted: it explains the
            // numbers around it, and the transcript is where it is read.
            Event::Notice { .. } => {}
            Event::Commands { commands } => self.commands = commands.clone(),
        }
    }

    /// Where the session stands now, as a turn beginning or ending here would
    /// be measured from.
    fn mark(&self) -> TurnMark {
        TurnMark {
            tokens: self.totals.tokens(),
            five_hour: self.usage_windows.and_then(|windows| windows.five_hour),
        }
    }

    /// Ends the running turn, and records what it spent.
    fn end_turn(&mut self) {
        let ended = self.mark();
        let began = self.turn_began.take().unwrap_or(self.turn_last_ended);
        let five_hour_share = match self.window_reported {
            true => window_share(began.five_hour, ended.five_hour),
            false => None,
        };
        self.turns.push(TurnRecord {
            number: self.turns.len() as u64 + 1,
            tokens: ended.tokens.saturating_sub(began.tokens),
            five_hour_share,
        });
        self.turn_last_ended = ended;
        self.window_reported = false;
        self.turn_running = false;
    }

    /// Folds one file change in, against the file's running totals.
    ///
    /// The "why" is taken here rather than carried on the event: the fold is
    /// what knows the order the stream arrived in, and every backend gets the
    /// same rule for free — the last thing the model said before this call is
    /// the last [`Event::AssistantMessage`] the fold saw from the agent that
    /// made the call, because nothing else that agent says comes between a
    /// call and its result. Another agent's words can, while agents run at
    /// once, which is why each agent's are kept apart.
    fn change_file(&mut self, path: &str, added: Option<u64>, removed: Option<u64>) {
        let said = match &self.ended_by {
            Some(agent) => self.agents_said.get(agent),
            None => self.last_assistant.as_ref(),
        };
        let why = said
            .map(String::as_str)
            .and_then(first_line)
            .map(str::to_owned);

        let at = match self.files_at.get(path) {
            Some(at) => *at,
            None => {
                self.files_at.insert(path.to_owned(), self.files.len());
                self.files.push(FileChanges {
                    path: path.to_owned(),
                    added: 0,
                    removed: 0,
                    added_unstated: 0,
                    removed_unstated: 0,
                    changes: 0,
                    why: None,
                });
                self.files.len() - 1
            }
        };

        let Some(file) = self.files.get_mut(at) else {
            return;
        };
        file.changes += 1;
        match added {
            Some(lines) => file.added = file.added.saturating_add(lines),
            None => file.added_unstated += 1,
        }
        match removed {
            Some(lines) => file.removed = file.removed.saturating_add(lines),
            None => file.removed_unstated += 1,
        }
        // A change the model said nothing before keeps whatever it said before
        // the last one: an explanation that disappeared on the second edit to
        // the same file would read as the file having no reason to be there.
        if why.is_some() {
            file.why = why;
        }
    }

    /// The files the session changed, in the order it first changed them.
    pub fn files(&self) -> &[FileChanges] {
        &self.files
    }

    /// What is running, once the backend has said.
    pub fn meta(&self) -> Option<&SessionMeta> {
        self.meta.as_ref()
    }

    /// How tool calls are gated, once something has said.
    pub fn mode(&self) -> Option<Mode> {
        self.mode
    }

    /// The model the session is on: what the backend last reported, or the one
    /// the operator has since chosen and it has not yet confirmed.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Token and cost totals.
    pub fn totals(&self) -> &Totals {
        &self.totals
    }

    /// What is left of the plan's usage windows, once a backend has said.
    ///
    /// `None` means nothing reported any — a metered profile, or a backend
    /// that does not emit them — and whatever shows these shows nothing at
    /// all rather than a zero, which would read as a window untouched.
    pub fn usage_windows(&self) -> Option<&UsageWindows> {
        self.usage_windows.as_ref()
    }

    /// How the session is billed, as the backend or the profile last said.
    /// `None` where nothing has, which a consumer shows as not knowing rather
    /// than as either mode.
    pub fn billing(&self) -> Option<Billing> {
        self.billing
    }

    /// The prompt the main agent's last request sent, and the window it went
    /// into where the backend said. `None` until a request has been reported,
    /// which is not an empty context: nothing has been measured.
    pub fn context(&self) -> Option<&Context> {
        self.context.as_ref()
    }

    /// Tool call counters.
    pub fn tools(&self) -> &ToolTotals {
        &self.tools
    }

    /// Tool calls that started and have not ended, by name.
    pub fn in_flight_tools(&self) -> &BTreeMap<ToolCallId, String> {
        &self.in_flight_tools
    }

    /// Whether the backend is still answering the last prompt: from the prompt
    /// until [`Event::TurnEnded`], or a fatal error ends the session under it.
    ///
    /// A log recorded by a backend that never reports a turn's end leaves its
    /// last turn running here; whether that turn is running *now* is a question
    /// about a live process, which a fold of a record cannot answer.
    pub fn turn_running(&self) -> bool {
        self.turn_running
    }

    /// Every turn that has ended, oldest first. A turn still running is not
    /// in it: its figures are not final until it ends.
    pub fn turns(&self) -> &[TurnRecord] {
        &self.turns
    }

    /// How many prompts the operator sent.
    pub fn user_messages(&self) -> u64 {
        self.user_messages
    }

    /// How many replies the assistant completed. A sub-agent's messages are
    /// its own conversation with the session, not replies, and are not
    /// counted.
    pub fn assistant_messages(&self) -> u64 {
        self.assistant_messages
    }

    /// The reply currently streaming, assembled from deltas. Empty between
    /// messages.
    pub fn pending_assistant(&self) -> &str {
        &self.pending_assistant
    }

    /// The last completed reply: the session's own, never a sub-agent's.
    pub fn last_assistant(&self) -> Option<&str> {
        self.last_assistant.as_deref()
    }

    /// How many permission prompts were raised.
    pub fn permission_requests(&self) -> u64 {
        self.permission_requests
    }

    /// How many were denied.
    pub fn permissions_denied(&self) -> u64 {
        self.permissions_denied
    }

    /// Requests still waiting on the operator.
    pub fn pending_permissions(&self) -> &BTreeSet<ToolCallId> {
        &self.pending_permissions
    }

    /// What the session is about, on one line: the last title the backend
    /// gave it, or else the first line of the operator's first message.
    ///
    /// A title follows the backend, which may re-title a session; a caption
    /// taken from the first message is written once and stays, because what
    /// a session was started to do does not change when the talk moves on.
    /// `None` where there is neither — a session nobody has spoken in — or
    /// where what there is holds no words.
    pub fn caption(&self) -> Option<String> {
        let words = |text: &str| {
            let line = text.lines().find(|line| !line.trim().is_empty())?;
            Some(line.split_whitespace().collect::<Vec<_>>().join(" "))
        };
        self.title
            .as_deref()
            .and_then(words)
            .or_else(|| self.first_prompt.as_deref().and_then(words))
    }

    /// The decisions log, in order.
    pub fn decisions(&self) -> &[DecisionRecord] {
        &self.decisions
    }

    /// The checkpoints taken, in order.
    pub fn checkpoints(&self) -> &[CheckpointRecord] {
        &self.checkpoints
    }

    /// The latest test run the session made. `None` where it has made none,
    /// which is not a run of no tests.
    pub fn test_run(&self) -> Option<&TestRunRecord> {
        self.test_run.as_ref()
    }

    /// How many sub-agents were spawned.
    pub fn agents_spawned(&self) -> u64 {
        self.agents_spawned
    }

    /// Sub-agents that finished their task.
    pub fn agents_completed(&self) -> u64 {
        self.agents_completed
    }

    /// Sub-agents that failed.
    pub fn agents_failed(&self) -> u64 {
        self.agents_failed
    }

    /// Sub-agents that were killed or hit a budget.
    pub fn agents_cancelled(&self) -> u64 {
        self.agents_cancelled
    }

    /// Sub-agents running now.
    pub fn running_agents(&self) -> &BTreeSet<AgentId> {
        &self.running_agents
    }

    /// The most sub-agents that ran at once.
    pub fn peak_running_agents(&self) -> u64 {
        self.peak_running_agents
    }

    /// How many errors were reported.
    pub fn errors(&self) -> u64 {
        self.errors
    }

    /// The commands the backend last said it runs from a prompt, in its
    /// order. Empty until it has said, which is not a backend with none.
    pub fn commands(&self) -> &[SlashCommand] {
        &self.commands
    }

    /// The error that ended the session, if one did.
    pub fn fatal_error(&self) -> Option<&str> {
        self.fatal_error.as_deref()
    }
}

/// How far a window moved between two reports of it, where the two are
/// reports of the same window: both give the moment it starts over, and it is
/// the same moment. A level that went down is a window that started over
/// without saying so, and is no measurement of what was spent.
fn window_share(before: Option<UsageWindow>, after: Option<UsageWindow>) -> Option<f64> {
    let (before, after) = (before?, after?);
    if before.resets_at.is_none() || before.resets_at != after.resets_at {
        return None;
    }
    let moved = after.utilization - before.utilization;
    (moved.is_finite() && moved >= 0.0).then_some(moved)
}

/// The first line of a message that has anything on it.
///
/// A model opens a turn with a sentence and then goes on; that sentence is the
/// whole of what the pane has room for, and cutting it anywhere else would put
/// half a thought beside a file.
fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{AgentId, Backend, Mode, PermissionDecision, SlashCommand, UsageWindow};

    fn usage(input: u64, output: u64, cost: Option<f64>) -> Event {
        on_model("opus-5", input, output, cost)
    }

    fn on_model(model: &str, input: u64, output: u64, cost: Option<f64>) -> Event {
        Event::Usage(Usage {
            input,
            output,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 0,
            model: model.to_owned(),
            cost_usd: cost,
            cost_basis: None,
            settles_model: false,
        })
    }

    /// What the Claude bridge emits at a turn's end: the money for every token
    /// reported under the model so far, and whatever tokens the per-message
    /// records had not already carried.
    fn settlement(model: &str, cost: f64) -> Event {
        let Event::Usage(mut usage) = on_model(model, 0, 0, Some(cost)) else {
            unreachable!("on_model builds a usage event")
        };
        usage.settles_model = true;
        Event::Usage(usage)
    }

    #[test]
    fn a_settlement_prices_the_records_it_covers() {
        // The CLI reports tokens per message and money once per turn, in a
        // record that prices every token reported under the model so far. The
        // message records are not unpriced once it lands; they are paid for.
        let state = SessionState::replay(&[
            usage(100, 10, None),
            usage(200, 20, None),
            settlement("opus-5", 0.75),
        ]);

        let totals = state.totals();
        assert_eq!(totals.records_unsettled, 0);
        assert!(totals.cost_fully_reported());
        assert!((totals.reported_cost_usd - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn a_settlement_leaves_another_models_records_unsettled() {
        let state = SessionState::replay(&[
            on_model("opus-5", 100, 10, None),
            on_model("haiku-4-5", 300, 30, None),
            settlement("opus-5", 0.75),
        ]);

        let totals = state.totals();
        assert_eq!(totals.records_unsettled, 1);
        assert!(!totals.cost_fully_reported());
        let owed = totals
            .unsettled
            .get("haiku-4-5")
            .expect("haiku is owed for");
        assert_eq!((owed.input, owed.output), (300, 30));
        assert!(!totals.unsettled.contains_key("opus-5"));
    }

    #[test]
    fn tokens_reported_after_a_settlement_are_unsettled_again() {
        let state = SessionState::replay(&[
            usage(100, 10, None),
            settlement("opus-5", 0.75),
            usage(200, 20, None),
        ]);

        let totals = state.totals();
        assert_eq!(totals.records_unsettled, 1);
        assert!(!totals.cost_fully_reported());
        let owed = totals
            .unsettled
            .get("opus-5")
            .expect("the new turn is owed for");
        assert_eq!((owed.input, owed.output), (200, 20));
    }

    /// What each model was billed is kept apart, so that a pane can price a
    /// model's row without dividing the session's bill by a share of tokens —
    /// two models bill at different rates, and a split by tokens would
    /// invent both figures.
    #[test]
    fn reported_cost_is_kept_per_model() {
        let state = SessionState::replay(&[
            on_model("opus-5", 100, 10, Some(0.50)),
            on_model("haiku-4-5", 300, 30, None),
            settlement("opus-5", 0.25),
        ]);

        let totals = state.totals();
        let opus = totals.reported_cost_by_model.get("opus-5").copied();
        assert!(
            opus.is_some_and(|cost| (cost - 0.75).abs() < 1e-9),
            "{opus:?}"
        );
        // A model nothing billed has no figure at all, not a zero.
        assert_eq!(totals.reported_cost_by_model.get("haiku-4-5"), None);
    }

    #[test]
    fn nothing_says_how_a_session_is_billed_until_a_backend_does() {
        let mut state = SessionState::new();
        assert_eq!(state.billing(), None, "a billing mode was guessed");

        state.apply(&Event::Billing {
            billing: Billing::Plan,
        });
        state.apply(&Event::Billing {
            billing: Billing::Metered,
        });
        assert_eq!(state.billing(), Some(Billing::Metered));
    }

    /// Who spent the session's tokens is a different question from what is
    /// still owed for: a model whose cost has landed has not stopped having
    /// spent them. `unsettled` empties as money arrives, so it cannot answer
    /// the first question and the fold keeps a cumulative map beside it.
    #[test]
    fn tokens_are_kept_per_model_whether_or_not_the_cost_has_settled() {
        let state = SessionState::replay(&[
            on_model("opus-5", 100, 10, None),
            on_model("haiku-4-5", 300, 30, None),
            settlement("opus-5", 0.75),
        ]);

        let totals = state.totals();
        assert_eq!(totals.tokens_by_model.get("opus-5"), Some(&110));
        assert_eq!(totals.tokens_by_model.get("haiku-4-5"), Some(&330));
        // The settled model is gone from what is owed for and still counted
        // among what was spent.
        assert!(!totals.unsettled.contains_key("opus-5"));
    }

    /// Every token the session counts belongs to exactly one model, so the map
    /// adds up to the whole of [`Totals::tokens`]. The per-model rows in the
    /// shell are shares of that total; a map that did not sum to it would draw
    /// shares that do not either.
    #[test]
    fn the_per_model_tokens_sum_to_the_sessions_own_total() {
        let state = SessionState::replay(&[
            Event::Usage(Usage {
                input: 2_000,
                output: 300,
                cache_read: 18_000,
                cache_write: 900,
                cache_write_1h: 400,
                reasoning: 120,
                model: "opus-5".to_owned(),
                cost_usd: Some(0.04),
                cost_basis: None,
                settles_model: false,
            }),
            on_model("haiku-4-5", 300, 30, None),
        ]);

        let totals = state.totals();
        // 2,000 + 300 + 18,000 + 900 + 120 for opus; the hour is a share of
        // the writes, not more of them.
        assert_eq!(totals.tokens_by_model.get("opus-5"), Some(&21_320));
        assert_eq!(
            totals.tokens_by_model.values().sum::<u64>(),
            totals.tokens()
        );
    }

    #[test]
    fn totals_sum_reported_cost_and_count_what_was_not_reported() {
        let state = SessionState::replay(&[
            usage(100, 10, Some(0.25)),
            usage(200, 20, None),
            usage(300, 30, Some(0.50)),
        ]);

        let totals = state.totals();
        assert_eq!(totals.input, 600);
        assert_eq!(totals.output, 60);
        assert_eq!(totals.tokens(), 660);
        assert_eq!(totals.records, 3);
        // A record that prices itself settles nothing else, so the one that
        // reported no cost is still owed for.
        assert_eq!(totals.records_unsettled, 1);
        assert!((totals.reported_cost_usd - 0.75).abs() < f64::EPSILON);
        assert!(!totals.cost_fully_reported());
    }

    /// A cache write bought for an hour costs twice the input rate where five
    /// minutes costs 1.25×, so the share is what a session is priced from. The
    /// fold has to carry it: a total that drops it prices every write at the
    /// cheaper rate and reports a bill nobody was sent.
    #[test]
    fn one_hour_cache_writes_are_summed_as_a_share_of_the_writes_not_beside_them() {
        let write = |cache_write, cache_write_1h| {
            Event::Usage(Usage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write,
                cache_write_1h,
                reasoning: 0,
                model: "opus-5".to_owned(),
                cost_usd: None,
                cost_basis: None,
                settles_model: false,
            })
        };
        let state = SessionState::replay(&[write(100, 100), write(50, 0), write(10, 4)]);

        let totals = state.totals();
        assert_eq!(totals.cache_write, 160);
        assert_eq!(totals.cache_write_1h, 104);
        assert_eq!(
            totals.tokens(),
            160,
            "the one-hour writes are already in the write total"
        );
    }

    #[test]
    fn deltas_assemble_and_a_complete_message_replaces_them() {
        let mut state = SessionState::new();
        state.apply(&Event::AssistantDelta {
            text: "Reading ".to_owned(),
        });
        state.apply(&Event::AssistantDelta {
            text: "fetch.ts".to_owned(),
        });
        assert_eq!(state.pending_assistant(), "Reading fetch.ts");

        state.apply(&Event::AssistantMessage {
            text: "Reading fetch.ts and its callers.".to_owned(),
            agent: None,
        });
        assert_eq!(state.pending_assistant(), "");
        assert_eq!(
            state.last_assistant(),
            Some("Reading fetch.ts and its callers.")
        );
        assert_eq!(state.assistant_messages(), 1);
    }

    #[test]
    fn a_turn_runs_from_the_prompt_until_the_backend_says_it_ended() {
        let mut state = SessionState::new();
        assert!(!state.turn_running());

        state.apply(&Event::UserMessage {
            text: "fix the test".to_owned(),
        });
        assert!(state.turn_running());
        state.apply(&Event::AssistantMessage {
            text: "Reading it first.".to_owned(),
            agent: None,
        });
        assert!(state.turn_running(), "a reply mid-turn is not its end");

        state.apply(&Event::TurnEnded);
        assert!(!state.turn_running());
    }

    #[test]
    fn a_fatal_error_ends_the_turn_and_a_recoverable_one_does_not() {
        let mut state = SessionState::new();
        state.apply(&Event::UserMessage {
            text: "go".to_owned(),
        });
        state.apply(&Event::Error {
            message: "retrying".to_owned(),
            fatal: false,
        });
        assert!(state.turn_running());
        state.apply(&Event::Error {
            message: "the CLI exited".to_owned(),
            fatal: true,
        });
        assert!(!state.turn_running());
    }

    #[test]
    fn tool_calls_pair_up_and_the_unpaired_ones_are_visible() {
        let mut state = SessionState::new();
        state.apply(&Event::ToolCallStart {
            id: "t1".into(),
            name: "Read".to_owned(),
            input: "fetch.ts".to_owned(),
            summary: None,
            agent: None,
        });
        assert_eq!(state.in_flight_tools().len(), 1);

        state.apply(&Event::ToolCallEnd {
            id: "t1".into(),
            name: "Read".to_owned(),
            input: "fetch.ts".to_owned(),
            output: "212 lines".to_owned(),
            bytes: 4_096,
            outcome: ToolOutcome::Ok,
            summary: None,
            exit_code: None,
            error: None,
        });
        state.apply(&Event::ToolCallEnd {
            id: "t9".into(),
            name: "Bash".to_owned(),
            input: "npm test".to_owned(),
            output: "1 failing".to_owned(),
            bytes: 128,
            outcome: ToolOutcome::Failed,
            summary: None,
            exit_code: None,
            error: None,
        });

        let tools = state.tools();
        assert!(state.in_flight_tools().is_empty());
        assert_eq!(tools.started, 1);
        assert_eq!(tools.finished, 2);
        assert_eq!(tools.failed, 1);
        assert_eq!(tools.unmatched_ends, 1);
        assert_eq!(tools.output_bytes, 4_224);
        assert_eq!(tools.by_name["Read"], 1);
        assert_eq!(tools.by_name["Bash"], 1);
        // The failure belongs to the tool that failed, and the tool that did
        // not fail has no entry at all.
        assert_eq!(tools.failed_by_name["Bash"], 1);
        assert_eq!(tools.failed_by_name.get("Read"), None);
    }

    #[test]
    fn denied_permissions_are_counted_and_stop_pending() {
        let mut state = SessionState::new();
        state.apply(&Event::PermissionRequest {
            id: "t1".into(),
            tool: "Bash".to_owned(),
            input: r#"{"command":"rm -rf build"}"#.to_owned(),
            target: Some("rm -rf build".to_owned()),
        });
        assert_eq!(state.pending_permissions().len(), 1);

        state.apply(&Event::PermissionResponse {
            id: "t1".into(),
            decision: PermissionDecision::Deny,
            message: None,
        });
        assert!(state.pending_permissions().is_empty());
        assert_eq!(state.permission_requests(), 1);
        assert_eq!(state.permissions_denied(), 1);
    }

    #[test]
    fn parallel_agents_report_their_peak() {
        let mut state = SessionState::new();
        for id in ["a1", "a2", "a3"] {
            state.apply(&Event::AgentSpawn {
                id: id.into(),
                parent: None,
                label: "test-writer".to_owned(),
            });
        }
        state.apply(&Event::AgentExit {
            id: "a1".into(),
            outcome: AgentOutcome::Completed,
        });
        state.apply(&Event::AgentExit {
            id: "a2".into(),
            outcome: AgentOutcome::Cancelled,
        });

        assert_eq!(state.agents_spawned(), 3);
        assert_eq!(state.agents_completed(), 1);
        assert_eq!(state.agents_cancelled(), 1);
        assert_eq!(state.running_agents().len(), 1);
        assert_eq!(state.peak_running_agents(), 3);
    }

    #[test]
    fn meta_is_taken_from_the_stream_and_updated_by_it() {
        let mut state = SessionState::new();
        assert!(state.meta().is_none());

        state.apply(&Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "default".to_owned(),
            model: "opus-5".to_owned(),
            backend_session: None,
        }));
        state.apply(&Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "default".to_owned(),
            model: "sonnet-5".to_owned(),
            backend_session: None,
        }));

        let meta = state.meta().expect("the stream carried session meta");
        assert_eq!(meta.backend, Backend::Claude);
        assert_eq!(meta.model, "sonnet-5");
    }

    #[test]
    fn a_fatal_error_is_kept_apart_from_the_count() {
        let mut state = SessionState::new();
        state.apply(&Event::Error {
            message: "tool timed out".to_owned(),
            fatal: false,
        });
        state.apply(&Event::Error {
            message: "backend exited".to_owned(),
            fatal: true,
        });
        assert_eq!(state.errors(), 2);
        assert_eq!(state.fatal_error(), Some("backend exited"));
    }

    #[test]
    fn the_mode_a_session_is_in_is_the_last_one_chosen() {
        let mut state = SessionState::new();
        assert_eq!(state.mode(), None, "a session invented a mode nobody set");

        state.apply(&Event::ModeSelected { mode: Mode::Ask });
        state.apply(&Event::ModeSelected { mode: Mode::Plan });

        assert_eq!(state.mode(), Some(Mode::Plan));
    }

    #[test]
    fn the_model_the_operator_chose_stands_until_the_backend_says_what_it_ran() {
        let mut state = SessionState::new();
        state.apply(&Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "max".to_owned(),
            model: "claude-opus-5".to_owned(),
            backend_session: None,
        }));
        assert_eq!(state.model(), Some("claude-opus-5"));

        // The operator asks for one by the alias the backend takes; the
        // backend answers later with whatever id it resolved that to.
        state.apply(&Event::ModelSelected {
            model: "haiku".to_owned(),
        });
        assert_eq!(state.model(), Some("haiku"));

        state.apply(&Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "max".to_owned(),
            model: "claude-haiku-4-5-20251001".to_owned(),
            backend_session: None,
        }));
        assert_eq!(state.model(), Some("claude-haiku-4-5-20251001"));
    }

    fn changed(path: &str, added: Option<u64>, removed: Option<u64>) -> Event {
        Event::FileChange {
            path: path.to_owned(),
            added,
            removed,
            hunks: Vec::new(),
        }
    }

    fn tested(counts: Option<TestCounts>, exit_code: Option<i32>) -> Event {
        Event::TestRun {
            id: ToolCallId::new("t"),
            counts,
            exit_code,
            failed: false,
            failures: Vec::new(),
        }
    }

    #[test]
    fn the_latest_test_run_is_the_one_the_session_reports() {
        assert_eq!(SessionState::replay(&[]).test_run(), None);

        let failing = TestCounts {
            passed: 3,
            failed: 1,
            ignored: 0,
            suites: 1,
        };
        let state =
            SessionState::replay(&[tested(Some(failing), Some(101)), tested(None, Some(0))]);
        assert_eq!(
            state.test_run(),
            Some(&TestRunRecord {
                counts: None,
                exit_code: Some(0),
                failed: false,
                failures: Vec::new(),
            }),
            "an earlier run's counts are not carried over a run that was not read"
        );
    }

    #[test]
    fn a_run_whose_counts_say_it_failed_failed_whatever_the_backend_said() {
        let failing = TestCounts {
            passed: 3,
            failed: 1,
            ignored: 0,
            suites: 1,
        };
        let recorded = SessionState::replay(&[tested(Some(failing), Some(101))]);
        assert_eq!(recorded.test_run().map(|run| run.failed), Some(true));
    }

    #[test]
    fn a_run_known_to_have_failed_is_kept_as_failed_without_counts() {
        let cut = Event::TestRun {
            id: ToolCallId::new("t"),
            counts: None,
            exit_code: Some(101),
            failed: true,
            failures: Vec::new(),
        };
        assert_eq!(
            SessionState::replay(&[cut]).test_run(),
            Some(&TestRunRecord {
                counts: None,
                exit_code: Some(101),
                failed: true,
                failures: Vec::new(),
            })
        );
    }

    #[test]
    fn the_tests_a_failing_run_named_are_kept_with_it() {
        let named = FailedTests {
            binary: "--test statement".to_owned(),
            tests: vec!["a_line_rounds".to_owned()],
        };
        let cut = Event::TestRun {
            id: ToolCallId::new("t"),
            counts: None,
            exit_code: Some(101),
            failed: true,
            failures: vec![named.clone()],
        };
        let state = SessionState::replay(&[cut]);
        assert_eq!(
            state.test_run().map(|run| run.failures.as_slice()),
            Some(&[named][..])
        );
    }

    #[test]
    fn a_run_that_named_a_failing_test_failed_whatever_its_status_said() {
        // `cargo test 2>&1 | tail -8` of a failing run: the status is tail's,
        // and the list left in the tail proves itself.
        let tail = "failures:\n    tests::wrong\n\n\
                    test result: FAILED. 3 passed; 1 failed; 1 ignored; 0 measured; \
                    0 filtered out; finished in 0.00s\n\n\
                    error: test failed, to rerun pass `--lib`\n";
        let run = TestRunRecord::read("cargo test 2>&1 | tail -8", tail, None, Some(0));
        assert_eq!(run.counts, None, "a tail holds no count");
        assert!(run.failed);
        assert_eq!(
            run.failures,
            [FailedTests {
                binary: "--lib".to_owned(),
                tests: vec!["tests::wrong".to_owned()],
            }]
        );
    }

    #[test]
    fn a_passing_tail_is_a_run_whose_result_was_not_read() {
        let tail = "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; \
                    0 filtered out; finished in 0.07s\n";
        let run = TestRunRecord::read("cargo test | tail -1", tail, None, Some(0));
        assert_eq!((run.counts, run.failed), (None, false));
        assert!(run.failures.is_empty());
    }

    #[test]
    fn a_files_changes_add_up_across_the_calls_that_made_them() {
        let state = SessionState::replay(&[
            changed("src/fetch.rs", Some(12), Some(3)),
            changed("src/cache.rs", Some(4), Some(0)),
            changed("src/fetch.rs", Some(1), Some(7)),
        ]);

        let files = state.files();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/fetch.rs", "files lost their order");
        assert_eq!((files[0].added, files[0].removed), (13, 10));
        assert_eq!(files[0].changes, 2);
        assert!(files[0].added_stated() && files[0].removed_stated());
        assert_eq!((files[1].added, files[1].removed), (4, 0));
    }

    #[test]
    fn a_change_that_did_not_say_how_much_it_removed_leaves_that_side_a_floor() {
        let state = SessionState::replay(&[
            changed("doomed.txt", Some(1), None),
            changed("doomed.txt", Some(2), Some(2)),
        ]);

        let file = &state.files()[0];
        assert_eq!(file.added, 3);
        assert_eq!(file.removed, 2, "an unstated removal was counted as lines");
        assert!(file.added_stated());
        assert!(
            !file.removed_stated(),
            "a figure the backend never gave was shown as the whole of it"
        );
        assert_eq!(file.removed_unstated, 1);
    }

    #[test]
    fn a_change_of_wholly_unstated_size_is_still_a_file_the_session_changed() {
        let state = SessionState::replay(&[changed("notes.ipynb", None, None)]);

        let file = &state.files()[0];
        assert_eq!((file.added, file.removed), (0, 0));
        assert_eq!(file.changes, 1);
        assert!(!file.added_stated() && !file.removed_stated());
    }

    #[test]
    fn the_why_beside_a_file_is_the_last_thing_the_model_said_before_changing_it() {
        let state = SessionState::replay(&[
            Event::AssistantMessage {
                text: "Adding the etag header.\nThen the cache lookup.".to_owned(),
                agent: None,
            },
            changed("src/fetch.rs", Some(9), Some(0)),
            Event::AssistantMessage {
                text: "  Short-circuiting the 304 path.  ".to_owned(),
                agent: None,
            },
            changed("src/cache.rs", Some(2), Some(0)),
        ]);

        assert_eq!(
            state.files()[1].why.as_deref(),
            Some("Short-circuiting the 304 path."),
            "the sentence beside a file was neither its first line nor trimmed"
        );
        assert_eq!(
            state.files()[0].why.as_deref(),
            Some("Adding the etag header."),
            "only the first line of a message belongs on one line beside a file"
        );
    }

    /// The pane redraws as the session runs, so a file changed twice carries
    /// the explanation of the change the operator has just watched.
    #[test]
    fn a_file_changed_again_takes_the_newer_explanation() {
        let state = SessionState::replay(&[
            Event::AssistantMessage {
                text: "Adding the etag header.".to_owned(),
                agent: None,
            },
            changed("src/fetch.rs", Some(9), Some(0)),
            Event::AssistantMessage {
                text: "Short-circuiting the 304 path.".to_owned(),
                agent: None,
            },
            changed("src/fetch.rs", Some(4), Some(1)),
        ]);

        assert_eq!(
            state.files()[0].why.as_deref(),
            Some("Short-circuiting the 304 path.")
        );
    }

    /// A call and its end, made by `agent`, that changed `path`.
    fn edited_by(agent: Option<&str>, id: &str, path: &str) -> [Event; 3] {
        [
            Event::ToolCallStart {
                id: ToolCallId::new(id),
                name: "Edit".to_owned(),
                input: String::new(),
                summary: None,
                agent: agent.map(AgentId::new),
            },
            Event::ToolCallEnd {
                id: ToolCallId::new(id),
                name: "Edit".to_owned(),
                input: String::new(),
                output: String::new(),
                bytes: 0,
                outcome: ToolOutcome::Ok,
                summary: None,
                exit_code: None,
                error: None,
            },
            changed(path, Some(1), Some(1)),
        ]
    }

    /// Two agents at work at once: each file's why is what the agent that
    /// changed it said last, never the other's words that happened to land
    /// in between.
    #[test]
    fn the_why_beside_a_file_is_what_the_agent_that_changed_it_said() {
        let said = |text: &str, agent: Option<&str>| Event::AssistantMessage {
            text: text.to_owned(),
            agent: agent.map(AgentId::new),
        };
        let mut events = vec![
            said("Fixing the session's header.", None),
            said("Let me check how eviction is triggered.", Some("toolu_a")),
        ];
        events.extend(edited_by(None, "t1", "src/fetch.rs"));
        events.push(said("Evicting on write, not on read.", Some("toolu_a")));
        events.extend(edited_by(Some("toolu_a"), "t2", "src/cache.rs"));
        let state = SessionState::replay(&events);

        let why = |at: usize| state.files()[at].why.as_deref();
        assert_eq!(why(0), Some("Fixing the session's header."));
        assert_eq!(why(1), Some("Evicting on write, not on read."));
        assert_eq!(
            state.last_assistant(),
            Some("Fixing the session's header."),
            "a sub-agent's words were taken for the session's reply"
        );
        assert_eq!(state.assistant_messages(), 1);
    }

    /// A sub-agent's message never streams, so it does not finish the reply
    /// the session is streaming.
    #[test]
    fn a_sub_agents_message_leaves_the_sessions_streaming_reply_alone() {
        let state = SessionState::replay(&[
            Event::AssistantDelta {
                text: "Reading fetch".to_owned(),
            },
            Event::AssistantMessage {
                text: "Found it.".to_owned(),
                agent: Some(AgentId::new("toolu_a")),
            },
        ]);
        assert_eq!(state.pending_assistant(), "Reading fetch");
    }

    #[test]
    fn a_file_changed_before_the_model_said_anything_has_no_why_invented_for_it() {
        let state = SessionState::replay(&[changed("src/fetch.rs", Some(1), Some(1))]);
        assert_eq!(state.files()[0].why, None);
    }

    #[test]
    fn the_usage_windows_a_session_shows_are_the_last_ones_reported() {
        use crate::event::{UsageWindow, UsageWindows};

        let mut state = SessionState::new();
        assert_eq!(
            state.usage_windows(),
            None,
            "a session invented a usage window nobody reported"
        );

        for used in [0.14, 0.15] {
            state.apply(&Event::UsageWindows(UsageWindows {
                five_hour: Some(UsageWindow {
                    utilization: used,
                    resets_at: Some(1_789_779_600),
                }),
                seven_day: Some(UsageWindow {
                    utilization: 0.36,
                    resets_at: Some(1_790_118_000),
                }),
                using_overage: false,
            }));
        }

        let windows = state.usage_windows().expect("two reports arrived");
        assert_eq!(
            windows.five_hour.map(|w| w.utilization),
            Some(0.15),
            "two reports of the same window were added together"
        );
        assert_eq!(
            windows.five_hour.and_then(|w| w.resets_at),
            Some(1_789_779_600)
        );
        assert!(!windows.using_overage);
    }

    fn context(tokens: u64) -> Event {
        Event::Context(crate::event::Context {
            tokens,
            model: "opus-5".to_owned(),
            window: Some(1_000_000),
        })
    }

    #[test]
    fn a_session_that_has_sent_nothing_has_no_context() {
        assert_eq!(SessionState::new().context(), None);
    }

    /// A compaction shrinks the next prompt, and the fold follows it down:
    /// the context is the last request's, never the largest one's.
    #[test]
    fn the_context_is_the_last_requests_even_when_it_shrank() {
        let state = SessionState::replay(&[
            context(150_000),
            Event::Notice {
                message: "the context was compacted (auto).".to_owned(),
            },
            context(20_000),
        ]);
        assert_eq!(state.context().map(|c| c.tokens), Some(20_000));
    }

    fn said(text: &str) -> Event {
        Event::UserMessage {
            text: text.to_owned(),
        }
    }

    fn titled(title: &str) -> Event {
        Event::Titled {
            title: title.to_owned(),
        }
    }

    #[test]
    fn a_session_nobody_has_spoken_in_has_no_caption() {
        assert_eq!(SessionState::new().caption(), None);
    }

    #[test]
    fn an_untitled_session_is_captioned_by_the_first_line_of_its_first_prompt() {
        let state = SessionState::replay(&[
            said("\n  Fix the   retry\tloop\nin catalog/fetch.ts"),
            Event::TurnEnded,
            said("now the tests"),
        ]);

        assert_eq!(state.caption().as_deref(), Some("Fix the retry loop"));
    }

    #[test]
    fn the_backends_latest_title_captions_the_session_over_the_first_prompt() {
        let state = SessionState::replay(&[
            said("fix it"),
            titled("Interstellar objects in catalog"),
            titled("Interstellar objects search"),
        ]);

        assert_eq!(
            state.caption().as_deref(),
            Some("Interstellar objects search")
        );
    }

    #[test]
    fn a_title_or_prompt_of_nothing_but_whitespace_is_no_caption() {
        assert_eq!(SessionState::replay(&[said(" \n\t")]).caption(), None);
        assert_eq!(
            SessionState::replay(&[titled("  "), said("Etag support")])
                .caption()
                .as_deref(),
            Some("Etag support")
        );
    }

    #[test]
    fn replaying_the_same_events_twice_lands_in_the_same_place() {
        let events = vec![
            Event::UserMessage {
                text: "add etags".to_owned(),
            },
            usage(1_000, 100, Some(0.04)),
            Event::Decision {
                summary: "Reuse the existing LRU".to_owned(),
                rationale: None,
                rejected: vec!["A second Map".to_owned()],
            },
        ];
        assert_eq!(SessionState::replay(&events), SessionState::replay(&events));
    }

    fn prompt() -> Event {
        Event::UserMessage {
            text: "go on".to_owned(),
        }
    }

    /// A usage record with cache traffic in it, so that a turn's tokens are
    /// seen to count every kind, as the session's total does.
    fn cached(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Event {
        Event::Usage(Usage {
            input,
            output,
            cache_read,
            cache_write,
            cache_write_1h: 0,
            reasoning: 0,
            model: "opus-5".to_owned(),
            cost_usd: None,
            cost_basis: None,
            settles_model: false,
        })
    }

    fn five_hour(used: f64, resets_at: Option<u64>) -> Event {
        Event::UsageWindows(UsageWindows {
            five_hour: Some(UsageWindow {
                utilization: used,
                resets_at,
            }),
            seven_day: None,
            using_overage: false,
        })
    }

    const RESET: Option<u64> = Some(1_789_779_600);

    #[test]
    fn each_turn_counts_only_the_tokens_it_spent() {
        // Turn one: 1 200 + 300 + 8 000 + 500 = 10 000 and 20 + 5 = 25, so
        // 10 025. Turn two: 40 + 7 + 9 000 = 9 047. Summed by hand, not by
        // the fold: the session's total is 19 072 and neither turn is it.
        let state = SessionState::replay(&[
            prompt(),
            cached(1_200, 300, 8_000, 500),
            cached(20, 5, 0, 0),
            Event::TurnEnded,
            prompt(),
            cached(40, 7, 9_000, 0),
            Event::TurnEnded,
        ]);

        let turns = state.turns();
        assert_eq!(turns.len(), 2);
        assert_eq!((turns[0].number, turns[0].tokens), (1, 10_025));
        assert_eq!((turns[1].number, turns[1].tokens), (2, 9_047));
        assert_eq!(state.totals().tokens(), 19_072);
    }

    #[test]
    fn a_turn_still_running_has_no_record() {
        let state = SessionState::replay(&[prompt(), cached(100, 10, 0, 0)]);
        assert!(state.turns().is_empty(), "{:?}", state.turns());
    }

    #[test]
    fn a_turn_a_fatal_error_ended_is_recorded_with_its_numbers() {
        let state = SessionState::replay(&[
            prompt(),
            cached(100, 10, 0, 0),
            Event::Error {
                message: "the CLI exited".to_owned(),
                fatal: true,
            },
        ]);
        assert_eq!(state.turns().len(), 1);
        assert_eq!(state.turns()[0].tokens, 110);
    }

    #[test]
    fn a_fatal_error_between_turns_closes_no_turn() {
        let state = SessionState::replay(&[
            prompt(),
            Event::TurnEnded,
            Event::Error {
                message: "the CLI exited".to_owned(),
                fatal: true,
            },
        ]);
        assert_eq!(state.turns().len(), 1, "{:?}", state.turns());
    }

    /// The window moved from 14 % to 15 % across turn two, and the turn is
    /// what it moved across. Turn one has no report before it, so it has no
    /// share rather than a share of everything reported so far.
    #[test]
    fn a_turns_window_share_is_what_the_window_moved_across_it() {
        let state = SessionState::replay(&[
            prompt(),
            five_hour(0.14, RESET),
            Event::TurnEnded,
            prompt(),
            five_hour(0.15, RESET),
            Event::TurnEnded,
        ]);
        let turns = state.turns();
        assert_eq!(turns[0].five_hour_share, None);
        let share = turns[1].five_hour_share.expect("both ends were reported");
        assert!((share - 0.01).abs() < 1e-9, "{share}");
    }

    /// Five turns as the `claude` CLI reported them live: a report at the end
    /// of a request, and only when the level moved a whole point. Turn one
    /// moved it 14 → 15 but nothing was reported before it; turn two
    /// 15 → 16 → 17; turn three 17 → 18; turn four made four requests and
    /// moved it under a point, so nothing was reported; turn five 18 → 19.
    /// The shares that are measured add up to the session's 15 → 19.
    #[test]
    fn several_reports_in_a_turn_measure_it_from_the_last_before_to_the_last_in_it() {
        let state = SessionState::replay(&[
            prompt(),
            cached(2, 88, 28_859, 0),
            five_hour(0.14, RESET),
            cached(2, 249, 117_066, 458),
            five_hour(0.15, RESET),
            Event::TurnEnded,
            prompt(),
            five_hour(0.16, RESET),
            five_hour(0.17, RESET),
            Event::TurnEnded,
            prompt(),
            five_hour(0.18, RESET),
            Event::TurnEnded,
            prompt(),
            cached(2, 90, 117_524, 427),
            Event::TurnEnded,
            prompt(),
            five_hour(0.19, RESET),
            Event::TurnEnded,
        ]);

        let shares: Vec<Option<u64>> = state
            .turns()
            .iter()
            .map(|turn| turn.five_hour_share.map(|s| (s * 100.0).round() as u64))
            .collect();
        assert_eq!(shares, [None, Some(2), Some(1), None, Some(1)]);
    }

    #[test]
    fn a_turn_the_window_was_not_reported_in_has_no_share() {
        let state = SessionState::replay(&[
            prompt(),
            five_hour(0.14, RESET),
            Event::TurnEnded,
            prompt(),
            cached(100, 10, 0, 0),
            Event::TurnEnded,
        ]);
        assert_eq!(
            state.turns()[1].five_hour_share,
            None,
            "a level nobody reported during the turn was read as its end"
        );
    }

    #[test]
    fn a_window_that_started_over_during_the_turn_gives_no_share() {
        let state = SessionState::replay(&[
            prompt(),
            five_hour(0.02, RESET),
            Event::TurnEnded,
            prompt(),
            five_hour(0.05, Some(1_789_797_600)),
            Event::TurnEnded,
        ]);
        assert_eq!(state.turns()[1].five_hour_share, None);
    }

    #[test]
    fn a_window_reported_without_its_reset_gives_no_share() {
        let state = SessionState::replay(&[
            prompt(),
            five_hour(0.02, None),
            Event::TurnEnded,
            prompt(),
            five_hour(0.05, None),
            Event::TurnEnded,
        ]);
        assert_eq!(
            state.turns()[1].five_hour_share,
            None,
            "a window with no reset cannot be told from one that started over"
        );
    }

    #[test]
    fn a_turn_whose_report_dropped_the_five_hour_window_has_no_share() {
        let state = SessionState::replay(&[
            prompt(),
            five_hour(0.14, RESET),
            Event::TurnEnded,
            prompt(),
            Event::UsageWindows(UsageWindows {
                five_hour: None,
                seven_day: Some(UsageWindow {
                    utilization: 0.4,
                    resets_at: RESET,
                }),
                using_overage: false,
            }),
            Event::TurnEnded,
        ]);
        assert_eq!(state.turns()[1].five_hour_share, None);
    }

    /// A backend that ends a turn it was never seen to start — a second
    /// prompt queued behind the first, a record that begins mid-turn — has
    /// its tokens counted from the end of the turn before, so no token is in
    /// two turns.
    #[test]
    fn an_end_with_no_prompt_before_it_counts_from_the_last_end() {
        let state = SessionState::replay(&[
            prompt(),
            cached(100, 0, 0, 0),
            Event::TurnEnded,
            cached(30, 0, 0, 0),
            Event::TurnEnded,
        ]);
        let tokens: Vec<u64> = state.turns().iter().map(|turn| turn.tokens).collect();
        assert_eq!(tokens, [100, 30]);
    }

    fn command(name: &str) -> SlashCommand {
        SlashCommand {
            name: name.to_owned(),
            description: format!("what /{name} does"),
            argument_hint: None,
        }
    }

    #[test]
    fn the_last_command_list_the_backend_sent_replaces_the_one_before() {
        let state = SessionState::replay(&[
            Event::Commands {
                commands: vec![command("compact"), command("context")],
            },
            prompt(),
            Event::Commands {
                commands: vec![command("review_changes (MCP)")],
            },
        ]);
        let names: Vec<&str> = state.commands().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["review_changes (MCP)"]);
        assert!(SessionState::new().commands().is_empty());
    }
}
