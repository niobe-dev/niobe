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
use crate::work::{self, Work};
use std::collections::{BTreeMap, BTreeSet};

use crate::event::{
    AgentId, AgentOutcome, Billing, ChangeScope, CompactTrigger, Context, Event, Mode, ModelOption,
    OPERATOR_SHELL, SessionMeta, SlashCommand, TokenCounts, ToolCallId, ToolOutcome, Usage,
    UsageWindow, UsageWindows,
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
    /// Those records, per model.
    ///
    /// A backend that reports money once per turn leaves the turn priced by
    /// nothing while it runs. These are the tokens no reported figure covers,
    /// in the shape a price table takes, so a consumer that has one can show
    /// what the turn is costing instead of showing nothing. The fold itself
    /// prices nothing: an invented number here would be indistinguishable
    /// from a measured one.
    ///
    /// A model is in the map only while it is owed for.
    pub unsettled: BTreeMap<String, Owed>,
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
}

/// The records of one model that no reported cost covers.
///
/// Each record is kept as the backend reported it, not only summed, because
/// what a request costs can turn on the request itself: a provider bills a
/// prompt past a long-context threshold at dearer rates, and a sum of short
/// prompts read as one request would cross that threshold where none of them
/// did.
#[derive(Debug, Clone, PartialEq)]
pub struct Owed {
    total: Usage,
    records: Vec<Usage>,
}

impl Owed {
    fn new(model: &str) -> Self {
        Self {
            total: Usage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                cache_write_1h: 0,
                reasoning: 0,
                model: model.to_owned(),
                cost_usd: None,
                settles_model: false,
                fast: false,
            },
            records: Vec::new(),
        }
    }

    fn push(&mut self, usage: &Usage) {
        let total = &mut self.total;
        total.input = total.input.saturating_add(usage.input);
        total.output = total.output.saturating_add(usage.output);
        total.cache_read = total.cache_read.saturating_add(usage.cache_read);
        total.cache_write = total.cache_write.saturating_add(usage.cache_write);
        total.cache_write_1h = total.cache_write_1h.saturating_add(usage.cache_write_1h);
        total.reasoning = total.reasoning.saturating_add(usage.reasoning);
        self.records.push(usage.clone());
    }

    /// Every token owed for, summed. Its `cost_usd` is always `None` — it is
    /// what is *not* accounted for.
    ///
    /// For counting tokens, not for pricing: a sum of several requests priced
    /// as one can land in a rate none of them was billed at. Price
    /// [`Owed::records`] one at a time.
    pub fn total(&self) -> &Usage {
        &self.total
    }

    /// The records owed for, in the order they were folded, each as its
    /// backend reported it. Never empty.
    pub fn records(&self) -> &[Usage] {
        &self.records
    }
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
        bump(&mut self.records);
        self.tokens_by_model
            .entry(usage.model.clone())
            .and_modify(|spent| *spent = spent.saturating_add(usage.tokens()))
            .or_insert_with(|| usage.tokens());

        // A settlement is read before its own cost is taken, so that a record
        // both settling the model and carrying tokens of its own — which the
        // Claude bridge emits for tokens no message reported — leaves nothing
        // owed rather than owing for itself.
        let cost = reported_cost(usage);
        if usage.settles_model && cost.is_some() {
            self.clear_unsettled(&usage.model);
        }

        match cost {
            // Each cost is finite, and so is what they add up to: a sum of
            // two that overflows stops at the largest there is rather than
            // drawing as infinite money.
            Some(cost) => {
                self.reported_cost_usd = finite_sum(self.reported_cost_usd, cost);
                let by_model = self
                    .reported_cost_by_model
                    .entry(usage.model.clone())
                    .or_default();
                *by_model = finite_sum(*by_model, cost);
            }
            None => self.owe(usage),
        }
    }

    /// Records tokens that no reported cost covers.
    fn owe(&mut self, usage: &Usage) {
        bump(&mut self.records_unsettled);
        self.unsettled
            .entry(usage.model.clone())
            .or_insert_with(|| Owed::new(&usage.model))
            .push(usage);
    }

    /// Forgets what `model` was owed for, because a cost has now covered it.
    fn clear_unsettled(&mut self, model: &str) {
        if let Some(covered) = self.unsettled.remove(model) {
            let covered = u64::try_from(covered.records.len()).unwrap_or(u64::MAX);
            self.records_unsettled = self.records_unsettled.saturating_sub(covered);
        }
    }
}

/// The money a record reports, where it is a sum of money at all.
///
/// A cost that is not finite, or is below nothing, is not a figure anything
/// on screen can stand behind: added in, a `NaN` poisons every total after it
/// and a negative one quietly takes spend away. Such a record is folded as one
/// that reported no money, so its tokens are owed for like any other's.
fn reported_cost(usage: &Usage) -> Option<f64> {
    usage
        .cost_usd
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
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
    /// Finished calls by the kind of work they did ([`crate::work`]). Every
    /// finished call is under exactly one kind, so the kinds' calls add up to
    /// [`ToolTotals::finished`] and their failures to [`ToolTotals::failed`].
    pub by_work: BTreeMap<Work, WorkTotals>,
    /// Calls the session or their turn ended under: still running when a
    /// fatal error ended the backend, or when the backend ended the turn
    /// without reporting their end, so no end will arrive for them. Neither
    /// finished nor failed — the agent did not fail them, the session stopped
    /// them.
    pub interrupted: u64,
}

/// The calls of one kind of work.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkTotals {
    /// Calls that finished, however they finished.
    pub calls: u64,
    /// Of those, the calls that reported failure.
    pub failed: u64,
    /// Of those, the calls the operator denied.
    pub denied: u64,
    /// How many of the calls each program or tool took part in: once per
    /// call however often the call ran it, so a pipeline of two `grep`s is
    /// one call that used `grep`.
    pub specifics: BTreeMap<String, u64>,
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
        let counts = whole.and_then(|whole| test_run::counts(command, whole, exit_code));
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
    ///
    /// `None` where no usage record landed in that span — a turn interrupted
    /// or failed before its first message was reported nothing, and a zero
    /// would read as a measurement. `Some(0)` is a turn whose records said it
    /// spent none.
    pub tokens: Option<u64>,
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
    /// Whether the turn was cut off rather than ended by the backend: the
    /// session failed under it, or the process running it stopped.
    pub cut: bool,
    /// What started the compaction of the conversation the turn ran, where it
    /// ran one: a turn the operator opened to compact, or one the backend
    /// compacted part-way through.
    pub compacted: Option<CompactTrigger>,
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
    /// Whether the session has been billed more than one way: resumed under
    /// a profile billed differently from the one it was recorded under.
    billing_changed: bool,
    /// The usage folded while the session was billed by use.
    metered: Totals,
    /// The main agent's last request. `None` until one has been reported.
    context: Option<Context>,
    tools: ToolTotals,
    in_flight_tools: BTreeMap<ToolCallId, String>,
    /// The calls whose end has been counted, so that an end said again for
    /// one of them is not a second call.
    ended_tools: BTreeSet<ToolCallId>,
    /// Whether the backend is still answering the last prompt.
    turn_running: bool,
    /// Whether the backend is compacting the conversation now.
    compacting: bool,
    /// What started a compaction since the last turn ended, for the record
    /// of the turn it ran in.
    compacted: Option<CompactTrigger>,
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
    /// Whether the last event folded ended a turn — its end, or a fatal
    /// error — which an end straight after it repeats rather than closes a
    /// turn of its own.
    ended_last: bool,
    /// Whether a usage record has landed since the last turn began or ended,
    /// which is the span a turn's tokens are counted over.
    usage_reported: bool,
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
    /// The latest test run, which replaces the one before it: an earlier
    /// run's result says nothing about the code as it is now.
    test_run: Option<TestRunRecord>,
    agents_spawned: u64,
    agents_completed: u64,
    agents_failed: u64,
    agents_cancelled: u64,
    /// Sub-agents still running when a fatal error ended the session.
    agents_interrupted: u64,
    running_agents: BTreeSet<AgentId>,
    /// The sub-agents whose end has been counted, by their exit or by the
    /// session ending under them, so that an exit said after it is not a
    /// second end.
    ended_agents: BTreeSet<AgentId>,
    peak_running_agents: u64,
    errors: u64,
    /// What the backend's own turn totals carried beyond what the turns'
    /// messages reported, summed over the session.
    beyond_messages: TokenCounts,
    fatal_error: Option<String>,
    /// The commands the backend last said it runs from a prompt.
    commands: Vec<SlashCommand>,
    /// The models the backend last said it offers.
    models: Vec<ModelOption>,
}

impl SessionState {
    /// What the backend's turn totals reported beyond what the turns'
    /// messages did, summed over the session: tokens the session's totals
    /// hold only where a bill carried them, and no message accounts for.
    pub fn beyond_messages(&self) -> &TokenCounts {
        &self.beyond_messages
    }

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
    /// a silently ignored event is a total that cannot be defended. What
    /// changes nothing is an event said again: a turn's end straight after
    /// the end it repeats, and a call's end or an agent's exit once that
    /// call or agent has ended.
    pub fn apply(&mut self, event: &Event) {
        self.fold(event);
        self.ended_last = matches!(
            event,
            Event::TurnEnded | Event::Error { fatal: true, .. } | Event::SessionLeft
        );
    }

    fn fold(&mut self, event: &Event) {
        match event {
            Event::SessionMeta(meta) => {
                self.model = Some(meta.model.clone());
                self.meta = Some(meta.clone());
            }

            Event::ModeSelected { mode } => self.mode = Some(*mode),

            // The operator's choice stands until the backend says what it
            // resolved that to, which is the next `SessionMeta`, or refuses
            // it. Showing the model it has moved off instead would tell the
            // operator their choice did not land.
            Event::ModelSelected { model } => self.model = Some(model.clone()),

            // Back to what the backend last reported running, or to no model
            // where it has reported none: the choice it refused never ran.
            Event::ModelRefused => {
                self.model = self.meta.as_ref().map(|meta| meta.model.clone());
            }

            Event::Titled { title } => self.title = Some(title.clone()),

            // A prompt sent while a turn runs joins it: the turn began with
            // the first one.
            Event::UserMessage { text } => {
                bump(&mut self.user_messages);
                if self.first_prompt.is_none() {
                    self.first_prompt = Some(text.clone());
                }
                if !self.turn_running {
                    self.turn_began = Some(self.mark());
                    self.window_reported = false;
                    self.usage_reported = false;
                }
                self.turn_running = true;
            }

            // An end with no turn running is still a turn, one the fold did
            // not see begin — a command the CLI answers itself sends no
            // prompt — as long as anything came between it and the last end.
            // With nothing between, it is the last turn's end said again,
            // and a turn recorded for it would be one nobody ran.
            Event::TurnEnded if self.ended_last => {}
            Event::TurnEnded => self.end_turn(false),

            Event::AssistantDelta { text } => self.pending_assistant.push_str(text),

            Event::AssistantMessage { text, agent: None } => {
                bump(&mut self.assistant_messages);
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
                // A start said again for a call already running is one call;
                // one for a call that has ended starts it again.
                self.ended_tools.remove(id);
                if self
                    .in_flight_tools
                    .insert(id.clone(), name.clone())
                    .is_none()
                {
                    bump(&mut self.tools.started);
                }
                if let Some(agent) = agent {
                    self.agent_calls.insert(id.clone(), agent.clone());
                }
            }

            // An end said again for a call already ended is the same call
            // ending, as a start said twice is one start.
            Event::ToolCallEnd { id, .. } if self.ended_tools.contains(id) => {}
            Event::ToolCallEnd {
                id,
                name,
                input,
                bytes,
                outcome,
                summary,
                command,
                ..
            } => {
                self.ended_tools.insert(id.clone());
                bump(&mut self.tools.finished);
                self.tools.output_bytes = self.tools.output_bytes.saturating_add(*bytes);
                bump(self.tools.by_name.entry(name.clone()).or_default());
                let reading = work::of_call(name, command.as_deref(), summary.as_deref(), input);
                let kind = self.tools.by_work.entry(reading.work).or_default();
                bump(&mut kind.calls);
                for specific in reading.specifics {
                    bump(kind.specifics.entry(specific).or_default());
                }
                match outcome {
                    ToolOutcome::Ok => {}
                    ToolOutcome::Failed => {
                        bump(&mut self.tools.failed);
                        bump(self.tools.failed_by_name.entry(name.clone()).or_default());
                        bump(&mut kind.failed);
                    }
                    ToolOutcome::Denied => {
                        bump(&mut self.tools.denied);
                        bump(&mut kind.denied);
                    }
                    // The operator's own stop: the tool neither failed nor
                    // was refused, so it counts against neither.
                    ToolOutcome::Interrupted => {}
                }
                if self.in_flight_tools.remove(id).is_none() {
                    bump(&mut self.tools.unmatched_ends);
                }
                self.ended_by = self.agent_calls.remove(id);
            }

            Event::Usage(usage) => {
                self.totals.add(usage);
                if self.billing == Some(Billing::Metered) {
                    self.metered.add(usage);
                }
                self.usage_reported = true;
            }

            // The last report replaces the one before it: a window is a level,
            // not a quantity, so summing two reports of it would be nonsense.
            // A report that names no window says only whether the plan is
            // spending beyond its fee; the levels before it still stand.
            Event::UsageWindows(windows) => {
                self.usage_windows = match (windows.is_empty(), self.usage_windows) {
                    (true, Some(before)) => Some(UsageWindows {
                        using_overage: windows.using_overage,
                        ..before
                    }),
                    _ => Some(*windows),
                };
                self.window_reported |= !windows.is_empty();
            }

            // The last report stands for what the session is billed from here
            // on; what was spent before a change was spent the other way.
            Event::Billing { billing } => {
                if self.billing.is_some_and(|before| before != *billing) {
                    self.billing_changed = true;
                }
                self.billing = Some(*billing);
            }

            // A level again, and the last one stands — including after a
            // compaction, when it drops: a high-water mark would keep showing
            // a context the model no longer has.
            Event::Context(context) => self.context = Some(context.clone()),

            // Nothing has measured the conversation that starts here, and the
            // last figure is of one the model no longer has: no figure is
            // what is true until the next request reports one.
            // A conversation started over is not one being compacted.
            Event::Cleared => {
                self.context = None;
                self.compacting = false;
            }

            // As a fatal error does, without being one: the process stopped,
            // and whatever it was running will report nothing more.
            Event::SessionLeft => {
                self.pending_permissions.clear();
                self.pending_assistant.clear();
                self.interrupt_backend_work();
                if self.turn_running {
                    self.end_turn(true);
                }
            }

            Event::PermissionRequest { id, .. } => {
                bump(&mut self.permission_requests);
                self.pending_permissions.insert(id.clone());
            }

            Event::PermissionWithdrawn { id } => {
                self.pending_permissions.remove(id);
            }

            Event::PermissionResponse { id, decision, .. } => {
                self.pending_permissions.remove(id);
                if !decision.allowed() {
                    bump(&mut self.permissions_denied);
                }
            }

            Event::FileChange {
                path,
                added,
                removed,
                scope,
                ..
            } => match scope {
                ChangeScope::Project => self.change_file(path, *added, *removed),
                // The agent's own notes are not what is being built: the
                // session's files and their line counts are the project's.
                ChangeScope::AgentMemory => {}
            },

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

            // A spawn said again for an agent already running is the same
            // agent, as an exit said twice is one exit. One for an agent that
            // has ended runs it again, and its next exit is that run's.
            Event::AgentSpawn { id, .. } => {
                self.ended_agents.remove(id);
                if self.running_agents.insert(id.clone()) {
                    bump(&mut self.agents_spawned);
                }
                let running = self.running_agents.len() as u64;
                self.peak_running_agents = self.peak_running_agents.max(running);
            }

            // A sub-agent's own figures are what a pane draws beside it. Its
            // context size is not a spend, and its tokens are already in the
            // session's usage records, so nothing here counts them again.
            Event::AgentProgress { .. } => {}

            // An exit without a spawn is still an agent that ended. One the
            // backend's end already counted as interrupted did not also
            // complete, and an exit reported twice is one exit.
            Event::AgentExit { id, .. } if self.ended_agents.contains(id) => {}
            Event::AgentExit { id, outcome } => {
                self.running_agents.remove(id);
                self.ended_agents.insert(id.clone());
                match outcome {
                    AgentOutcome::Completed => bump(&mut self.agents_completed),
                    AgentOutcome::Failed => bump(&mut self.agents_failed),
                    AgentOutcome::Cancelled => bump(&mut self.agents_cancelled),
                }
            }

            Event::Error { message, fatal } => {
                bump(&mut self.errors);
                if *fatal {
                    self.fatal_error = Some(message.clone());
                    // Nothing is left to take an answer: a prompt the session
                    // ended on was asked and never answered, and waits on no
                    // one now.
                    self.pending_permissions.clear();
                    self.pending_assistant.clear();
                    self.interrupt_backend_work();
                    // The turn it cut short spent what it spent, and is
                    // recorded with it; with no turn running there is none
                    // to end.
                    if self.turn_running {
                        self.end_turn(true);
                    }
                }
            }

            // A notice changes nothing that is counted: it explains the
            // numbers around it, and the transcript is where it is read.
            Event::Notice { .. } => {}
            // Kept apart rather than added to the totals: where the backend
            // bills what its messages did not report, the turn's bill already
            // carries it, and adding it here would count it twice.
            Event::TurnTotalDiffers {
                per_message,
                reported,
                ..
            } => {
                self.beyond_messages = self.beyond_messages.plus(&per_message.short_of(reported));
            }
            Event::Commands { commands } => self.commands = commands.clone(),
            Event::CompactionStarted => self.compacting = true,
            Event::CompactionEnded => self.compacting = false,
            // The boundary is the compaction done, which a backend may report
            // without having said it ended.
            Event::Compacted { trigger, .. } => {
                self.compacting = false;
                self.compacted = Some(*trigger);
            }
            Event::Models { models } => self.models = models.clone(),
        }
    }

    /// Records every call and sub-agent the backend had running as cut short
    /// by its end: none of them will report an end now, and one counted as
    /// running for good is a session the panes say is still working.
    ///
    /// A command the operator ran with `!` is not the backend's, keeps
    /// running, and ends with a report of its own.
    fn interrupt_backend_work(&mut self) {
        let cut: Vec<ToolCallId> = self
            .in_flight_tools
            .iter()
            .filter(|(_, name)| name.as_str() != OPERATOR_SHELL)
            .map(|(id, _)| id.clone())
            .collect();
        self.cut_calls(cut);
        let agents = std::mem::take(&mut self.running_agents);
        self.agents_interrupted = self.agents_interrupted.saturating_add(agents.len() as u64);
        self.ended_agents.extend(agents);
    }

    /// Records every call the ending turn left running as cut short: the
    /// backend has said the turn is over, and an end it did not send before
    /// that — a call whose permission prompt the stop withdrew — it will not
    /// send after it.
    ///
    /// A sub-agent still running goes on past the turn that spawned it, in
    /// the background, and its calls with it; so does a command the operator
    /// ran with `!`.
    fn interrupt_what_the_turn_left(&mut self) {
        let cut: Vec<ToolCallId> = self
            .in_flight_tools
            .iter()
            .filter(|(id, name)| {
                name.as_str() != OPERATOR_SHELL
                    && !self
                        .agent_calls
                        .get(*id)
                        .is_some_and(|agent| self.running_agents.contains(agent))
            })
            .map(|(id, _)| id.clone())
            .collect();
        self.cut_calls(cut);
    }

    /// Records each of `cut` as cut short: no end will arrive for it.
    fn cut_calls(&mut self, cut: Vec<ToolCallId>) {
        for id in cut {
            self.in_flight_tools.remove(&id);
            self.agent_calls.remove(&id);
            bump(&mut self.tools.interrupted);
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
    /// Records the running turn as ended, and whether it was `cut` off rather
    /// than ended by the backend.
    fn end_turn(&mut self, cut: bool) {
        // Nothing more comes until the next prompt, so a question the turn
        // left open waits on nobody: an answer to it would reach nothing. A
        // reply it left half-streamed is not the start of the next one.
        self.pending_permissions.clear();
        self.pending_assistant.clear();
        self.interrupt_what_the_turn_left();
        self.compacting = false;
        let ended = self.mark();
        let began = self.turn_began.take().unwrap_or(self.turn_last_ended);
        let five_hour_share = match self.window_reported {
            true => window_share(began.five_hour, ended.five_hour),
            false => None,
        };
        self.turns.push(TurnRecord {
            number: self.turns.len() as u64 + 1,
            tokens: self
                .usage_reported
                .then(|| ended.tokens.saturating_sub(began.tokens)),
            five_hour_share,
            cut,
            compacted: self.compacted.take(),
        });
        self.turn_last_ended = ended;
        self.window_reported = false;
        self.usage_reported = false;
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
        bump(&mut file.changes);
        match added {
            Some(lines) => file.added = file.added.saturating_add(lines),
            None => bump(&mut file.added_unstated),
        }
        match removed {
            Some(lines) => file.removed = file.removed.saturating_add(lines),
            None => bump(&mut file.removed_unstated),
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

    /// Whether the session has been billed more than one way, so that its
    /// cost is part list-price work on a plan and part money spent: the
    /// money is in [`SessionState::metered_totals`].
    pub fn billing_changed(&self) -> bool {
        self.billing_changed
    }

    /// Token and cost totals of the usage folded while the session was billed
    /// by use: of a session billed both ways, the part whose cost is money
    /// spent. A plan's part is left out, because its figure is what the work
    /// would have cost on the API and not what anyone paid.
    ///
    /// Usage is taken to be billed the way the session was last said to be
    /// when it arrived, so a record folded before anything said is in no
    /// part. A turn's cost settles only the records of its own part.
    pub fn metered_totals(&self) -> &Totals {
        &self.metered
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

    /// Tool calls that started and have not ended, by name. A fatal error
    /// leaves only the operator's own commands here: it cuts the backend's
    /// short ([`ToolTotals::interrupted`]).
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

    /// Whether the backend is compacting the conversation: between its word
    /// that it began and its word that it stopped, or the end of the turn it
    /// was compacting in.
    pub fn compacting(&self) -> bool {
        self.compacting
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

    /// The title the backend last gave the session. `None` where it has
    /// given none.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
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

    /// Sub-agents that were still running when a fatal error ended the
    /// session. Kept apart from the cancelled and the failed: nobody stopped
    /// them and they did not fail, the session ended under them.
    pub fn agents_interrupted(&self) -> u64 {
        self.agents_interrupted
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

    /// The models the backend last said it offers, in its order. Empty until
    /// it has said, which is not a backend with none.
    pub fn models(&self) -> &[ModelOption] {
        &self.models
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

/// Adds one to a counter, stopping at its largest value rather than wrapping:
/// a count that wrapped would read as a session that did almost nothing.
fn bump(counter: &mut u64) {
    *counter = counter.saturating_add(1);
}

/// `a + b`, stopped at the largest finite value where it would overflow.
fn finite_sum(a: f64, b: f64) -> f64 {
    let sum = a + b;
    match sum.is_finite() {
        true => sum,
        false => f64::MAX,
    }
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
            settles_model: false,
            fast: false,
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
    fn a_cost_that_is_not_a_sum_of_money_is_taken_as_not_reported() {
        for nonsense in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.5] {
            let state = SessionState::replay(&[
                usage(100, 10, Some(0.25)),
                usage(200, 20, Some(nonsense)),
                settlement("opus-5", nonsense),
            ]);

            let totals = state.totals();
            assert!(
                (totals.reported_cost_usd - 0.25).abs() < f64::EPSILON,
                "{nonsense}: {}",
                totals.reported_cost_usd
            );
            assert_eq!(totals.records_unsettled, 2, "{nonsense}");
            assert!(!totals.cost_fully_reported(), "{nonsense}");
            assert!(
                totals
                    .reported_cost_by_model
                    .values()
                    .all(|cost| (cost - 0.25).abs() < f64::EPSILON),
                "{nonsense}"
            );
        }
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
            .expect("haiku is owed for")
            .total();
        assert_eq!((owed.input, owed.output), (300, 30));
        assert!(!totals.unsettled.contains_key("opus-5"));
    }

    /// Two models each settled by their own record leave nothing owed,
    /// whichever settles first, and each keeps its own bill.
    #[test]
    fn two_models_settle_in_either_order() {
        for (first, second) in [("opus-5", "haiku-4-5"), ("haiku-4-5", "opus-5")] {
            let owing = [
                on_model("opus-5", 100, 10, None),
                on_model("haiku-4-5", 300, 30, None),
                on_model("opus-5", 200, 20, None),
            ];
            let halfway = SessionState::replay(owing.iter().chain([&settlement(first, 0.5)]));
            let owed: Vec<&String> = halfway.totals().unsettled.keys().collect();
            assert_eq!(owed, [second], "{first} settled first");
            let expected = if second == "opus-5" { 2 } else { 1 };
            assert_eq!(
                halfway.totals().records_unsettled,
                expected,
                "{first} first"
            );

            let settled = SessionState::replay(
                owing
                    .iter()
                    .chain([&settlement(first, 0.5), &settlement(second, 0.25)]),
            );
            let totals = settled.totals();
            assert_eq!(totals.records_unsettled, 0, "{first} first");
            assert!(totals.unsettled.is_empty(), "{first} first");
            assert!((totals.reported_cost_by_model[first] - 0.5).abs() < 1e-9);
            assert!((totals.reported_cost_by_model[second] - 0.25).abs() < 1e-9);
        }
    }

    /// A settlement for a model nothing is owed for takes nothing off what
    /// another model owes: the count of records owed for never goes below
    /// the records that are.
    #[test]
    fn a_settlement_with_nothing_owed_leaves_what_is_owed_alone() {
        let state = SessionState::replay(&[
            on_model("haiku-4-5", 300, 30, None),
            on_model("haiku-4-5", 300, 30, None),
            settlement("opus-5", 0.75),
            settlement("opus-5", 0.75),
        ]);

        let totals = state.totals();
        assert_eq!(totals.records_unsettled, 2);
        assert_eq!(totals.unsettled["haiku-4-5"].records().len(), 2);
        assert!((totals.reported_cost_usd - 1.5).abs() < 1e-9);
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
            .expect("the new turn is owed for")
            .total();
        assert_eq!((owed.input, owed.output), (200, 20));
    }

    #[test]
    fn each_record_owed_for_is_kept_as_it_was_reported_and_summed() {
        // A long-context rate is decided per request, so a consumer pricing
        // what is owed needs each request's own prompt, not only their sum.
        let state = SessionState::replay(&[
            usage(100, 10, None),
            usage(200, 20, None),
            on_model("haiku-4-5", 300, 30, None),
        ]);

        let owed = &state.totals().unsettled["opus-5"];
        let each: Vec<_> = owed
            .records()
            .iter()
            .map(|record| (record.input, record.output))
            .collect();
        assert_eq!(each, [(100, 10), (200, 20)]);
        assert_eq!((owed.total().input, owed.total().output), (300, 30));
        assert_eq!(owed.total().model, "opus-5");
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

    #[test]
    fn what_a_session_billed_both_ways_spent_by_use_is_kept_apart() {
        for [first, then] in [
            [Billing::Plan, Billing::Metered],
            [Billing::Metered, Billing::Plan],
        ] {
            let mut events = vec![Event::Billing { billing: first }];
            events.extend([usage(100, 10, None), settlement("opus-5", 0.25)]);
            events.push(Event::Billing { billing: then });
            events.extend([usage(200, 20, None), settlement("opus-5", 0.5)]);
            let state = SessionState::replay(&events);

            let metered = state.metered_totals();
            let (tokens, cost) = match first {
                Billing::Metered => (110, 0.25),
                Billing::Plan => (220, 0.5),
            };
            assert_eq!(metered.tokens(), tokens, "{first:?} then {then:?}");
            assert_eq!(metered.records, 2, "{first:?} then {then:?}");
            assert!(metered.cost_fully_reported(), "{first:?} then {then:?}");
            assert!(
                (metered.reported_cost_usd - cost).abs() < 1e-9,
                "{first:?} then {then:?}: {}",
                metered.reported_cost_usd
            );
            assert_eq!(state.totals().tokens(), 330, "{first:?} then {then:?}");
        }
    }

    #[test]
    fn a_session_never_billed_by_use_spent_nothing_by_use() {
        for billing in [None, Some(Billing::Plan)] {
            let mut events: Vec<Event> = billing
                .map(|billing| Event::Billing { billing })
                .into_iter()
                .collect();
            events.extend([usage(100, 10, None), settlement("opus-5", 0.25)]);
            let state = SessionState::replay(&events);

            assert_eq!(state.metered_totals(), &Totals::default(), "{billing:?}");
        }
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
                settles_model: false,
                fast: false,
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
                settles_model: false,
                fast: false,
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
            command: None,
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
            command: None,
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
            agent: None,
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

    fn asked(id: &str) -> Event {
        Event::PermissionRequest {
            id: id.into(),
            tool: "Bash".to_owned(),
            input: r#"{"command":"ls"}"#.to_owned(),
            target: Some("ls".to_owned()),
            agent: None,
        }
    }

    #[test]
    fn a_prompt_left_open_when_the_turn_ends_waits_on_nobody() {
        let state = SessionState::replay(&[
            Event::UserMessage {
                text: "go".to_owned(),
            },
            asked("a"),
            Event::TurnEnded,
            Event::UserMessage {
                text: "again".to_owned(),
            },
        ]);

        assert!(state.pending_permissions().is_empty());
        assert_eq!(state.permission_requests(), 1);
    }

    #[test]
    fn a_withdrawn_prompt_waits_on_nobody_and_was_not_denied() {
        let state = SessionState::replay(&[
            asked("a"),
            asked("b"),
            Event::PermissionWithdrawn { id: "a".into() },
        ]);

        assert_eq!(
            state.pending_permissions().iter().collect::<Vec<_>>(),
            [&ToolCallId::from("b")]
        );
        assert_eq!(state.permissions_denied(), 0);
    }

    #[test]
    fn a_fatal_error_leaves_no_prompt_waiting_and_a_recoverable_one_does() {
        let mut state = SessionState::new();
        state.apply(&Event::PermissionRequest {
            id: "t1".into(),
            tool: "Bash".to_owned(),
            input: r#"{"command":"ls"}"#.to_owned(),
            target: Some("ls".to_owned()),
            agent: None,
        });
        state.apply(&Event::Error {
            message: "retrying".to_owned(),
            fatal: false,
        });
        assert_eq!(state.pending_permissions().len(), 1);

        state.apply(&Event::Error {
            message: "the CLI exited".to_owned(),
            fatal: true,
        });

        assert!(state.pending_permissions().is_empty());
        // The prompt was asked, and never answered: neither a request nor a
        // refusal is taken back.
        assert_eq!(state.permission_requests(), 1);
        assert_eq!(state.permissions_denied(), 0);
    }

    fn start(id: &str, name: &str, agent: Option<&str>) -> Event {
        Event::ToolCallStart {
            id: id.into(),
            name: name.to_owned(),
            input: String::new(),
            summary: None,
            agent: agent.map(Into::into),
        }
    }

    #[test]
    fn a_spawn_or_a_start_said_twice_is_one_agent_and_one_call() {
        let mut state = SessionState::new();
        for _ in 0..2 {
            state.apply(&Event::AgentSpawn {
                id: "a1".into(),
                parent: None,
                kind: None,
                label: "test-writer".to_owned(),
            });
            state.apply(&start("t1", "Bash", None));
        }

        assert_eq!(state.agents_spawned(), 1);
        assert_eq!(state.tools().started, 1);
    }

    fn end(id: &str, name: &str) -> Event {
        Event::ToolCallEnd {
            id: id.into(),
            name: name.to_owned(),
            input: String::new(),
            output: String::new(),
            bytes: 64,
            outcome: ToolOutcome::Failed,
            summary: None,
            exit_code: None,
            error: None,
            command: None,
        }
    }

    #[test]
    fn an_end_said_twice_is_one_finished_call() {
        let state = SessionState::replay(&[
            start("t1", "Bash", None),
            end("t1", "Bash"),
            end("t1", "Bash"),
            end("t9", "Read"),
            end("t9", "Read"),
        ]);

        let tools = state.tools();
        assert_eq!(tools.started, 1);
        assert_eq!(tools.finished, 2);
        assert_eq!(tools.unmatched_ends, 1);
        assert_eq!(tools.failed, 2);
        assert_eq!(tools.output_bytes, 128);
        assert_eq!(tools.by_name["Bash"], 1);
        assert_eq!(tools.by_name["Read"], 1);
        assert_eq!(tools.failed_by_name["Bash"], 1);
    }

    #[test]
    fn a_call_started_again_after_its_end_ends_again() {
        let state = SessionState::replay(&[
            start("t1", "Bash", None),
            end("t1", "Bash"),
            start("t1", "Bash", None),
            end("t1", "Bash"),
        ]);

        let tools = state.tools();
        assert_eq!(tools.started, 2);
        assert_eq!(tools.finished, 2);
        assert_eq!(tools.unmatched_ends, 0);
        assert!(state.in_flight_tools().is_empty());
    }

    fn exit(id: &str, outcome: AgentOutcome) -> Event {
        Event::AgentExit {
            id: id.into(),
            outcome,
        }
    }

    #[test]
    fn an_exit_without_a_spawn_is_counted_and_an_exit_said_again_is_not() {
        let state = SessionState::replay(&[
            exit("a1", AgentOutcome::Completed),
            exit("a1", AgentOutcome::Completed),
            exit("a2", AgentOutcome::Failed),
        ]);

        assert_eq!(state.agents_spawned(), 0);
        assert_eq!(state.agents_completed(), 1);
        assert_eq!(state.agents_failed(), 1);
        assert!(state.running_agents().is_empty());
    }

    #[test]
    fn an_agent_the_session_ended_under_does_not_also_exit() {
        let state = SessionState::replay(&[
            Event::AgentSpawn {
                id: "a1".into(),
                parent: None,
                kind: None,
                label: "test-writer".to_owned(),
            },
            Event::SessionLeft,
            exit("a1", AgentOutcome::Completed),
        ]);

        assert_eq!(state.agents_spawned(), 1);
        assert_eq!(state.agents_interrupted(), 1);
        assert_eq!(state.agents_completed(), 0);
    }

    #[test]
    fn a_fatal_error_records_the_calls_it_cut_short_and_leaves_none_running() {
        let mut state = SessionState::new();
        state.apply(&Event::AgentSpawn {
            id: "a1".into(),
            parent: None,
            kind: None,
            label: "test-writer".to_owned(),
        });
        state.apply(&start("t1", "Bash", None));
        state.apply(&start("t2", "Read", Some("a1")));
        state.apply(&Event::Error {
            message: "retrying".to_owned(),
            fatal: false,
        });
        assert_eq!(state.in_flight_tools().len(), 2);

        state.apply(&Event::Error {
            message: "the CLI exited".to_owned(),
            fatal: true,
        });

        let tools = state.tools();
        assert!(state.in_flight_tools().is_empty());
        assert_eq!(tools.interrupted, 2);
        // Cut short is not an end the backend reported, and not a failure
        // the agent made.
        assert_eq!(tools.finished, 0);
        assert_eq!(tools.failed, 0);
        assert_eq!(tools.unmatched_ends, 0);
    }

    #[test]
    fn a_turn_that_ends_before_a_call_does_records_the_call_as_cut_short() {
        let mut state = SessionState::new();
        state.apply(&start("t1", "Bash", None));

        state.apply(&Event::TurnEnded);

        let tools = state.tools();
        assert!(state.in_flight_tools().is_empty());
        assert_eq!(tools.interrupted, 1);
        assert_eq!(tools.finished, 0);
        assert_eq!(tools.failed, 0);
    }

    #[test]
    fn a_turn_end_leaves_a_running_sub_agents_calls_and_the_operators_commands_running() {
        let mut state = SessionState::new();
        for id in ["a1", "a2"] {
            state.apply(&Event::AgentSpawn {
                id: id.into(),
                parent: None,
                kind: None,
                label: "explorer".to_owned(),
            });
        }
        state.apply(&start("t1", "Read", Some("a1")));
        state.apply(&start("t2", "Read", Some("a2")));
        state.apply(&Event::AgentExit {
            id: "a2".into(),
            outcome: AgentOutcome::Completed,
        });
        state.apply(&start("op1", OPERATOR_SHELL, None));

        state.apply(&Event::TurnEnded);

        assert_eq!(
            state.in_flight_tools().keys().collect::<Vec<_>>(),
            [&ToolCallId::from("op1"), &ToolCallId::from("t1")],
            "an agent working in the background and the operator's command go on"
        );
        assert_eq!(state.tools().interrupted, 1);
        assert_eq!(state.running_agents().len(), 1);
        assert_eq!(state.agents_interrupted(), 0);
    }

    #[test]
    fn a_fatal_error_leaves_the_operators_own_commands_running() {
        let mut state = SessionState::new();
        state.apply(&start("t1", "Bash", None));
        state.apply(&start("op1", OPERATOR_SHELL, None));

        state.apply(&Event::Error {
            message: "the CLI exited".to_owned(),
            fatal: true,
        });

        assert_eq!(
            state.in_flight_tools().keys().collect::<Vec<_>>(),
            [&ToolCallId::from("op1")],
            "the backend ending does not end what the operator ran"
        );
        assert_eq!(state.tools().interrupted, 1);

        state.apply(&Event::ToolCallEnd {
            id: "op1".into(),
            name: OPERATOR_SHELL.to_owned(),
            input: "git status".to_owned(),
            output: String::new(),
            bytes: 0,
            outcome: ToolOutcome::Ok,
            summary: None,
            exit_code: Some(0),
            error: None,
            command: None,
        });
        assert!(state.in_flight_tools().is_empty());
        assert_eq!(state.tools().unmatched_ends, 0);
    }

    #[test]
    fn a_fatal_error_leaves_no_agent_running_and_counts_none_as_failed() {
        let mut state = SessionState::new();
        for id in ["a1", "a2"] {
            state.apply(&Event::AgentSpawn {
                id: id.into(),
                parent: None,
                kind: None,
                label: "test-writer".to_owned(),
            });
        }
        state.apply(&Event::AgentExit {
            id: "a1".into(),
            outcome: AgentOutcome::Completed,
        });

        state.apply(&Event::Error {
            message: "the CLI exited".to_owned(),
            fatal: true,
        });

        assert!(state.running_agents().is_empty());
        assert_eq!(state.agents_interrupted(), 1);
        assert_eq!(state.agents_completed(), 1);
        assert_eq!(state.agents_failed(), 0);
        assert_eq!(state.agents_cancelled(), 0);
        assert_eq!(state.peak_running_agents(), 2);
    }

    #[test]
    fn parallel_agents_report_their_peak() {
        let mut state = SessionState::new();
        for id in ["a1", "a2", "a3"] {
            state.apply(&Event::AgentSpawn {
                id: id.into(),
                parent: None,
                kind: None,
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

    #[test]
    fn a_refused_model_gives_way_to_the_one_the_backend_last_reported() {
        let mut state = SessionState::new();
        state.apply(&Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "max".to_owned(),
            model: "claude-opus-5".to_owned(),
            backend_session: None,
        }));
        state.apply(&Event::ModelSelected {
            model: "no-such-model".to_owned(),
        });
        assert_eq!(state.model(), Some("no-such-model"));

        state.apply(&Event::ModelRefused);

        assert_eq!(state.model(), Some("claude-opus-5"));
    }

    #[test]
    fn a_model_refused_before_the_backend_reported_one_leaves_no_model() {
        let mut state = SessionState::new();
        state.apply(&Event::ModelSelected {
            model: "no-such-model".to_owned(),
        });

        state.apply(&Event::ModelRefused);

        assert_eq!(state.model(), None);
    }

    fn changed(path: &str, added: Option<u64>, removed: Option<u64>) -> Event {
        Event::FileChange {
            path: path.to_owned(),
            added,
            removed,
            hunks: Vec::new(),
            scope: ChangeScope::Project,
        }
    }

    #[test]
    fn a_change_to_the_agents_own_notes_is_left_out_of_the_files_the_session_changed() {
        let mut state = SessionState::new();
        state.apply(&changed("src/lib.rs", Some(3), Some(1)));
        state.apply(&Event::FileChange {
            path: "/home/me/.claude/projects/-w-app/memory/MEMORY.md".to_owned(),
            added: Some(2),
            removed: Some(1),
            hunks: Vec::new(),
            scope: ChangeScope::AgentMemory,
        });

        let files: Vec<_> = state
            .files()
            .iter()
            .map(|file| (file.path.as_str(), file.added, file.removed))
            .collect();
        assert_eq!(files, [("src/lib.rs", 3, 1)]);
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
                command: None,
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

    /// A sub-agent that said nothing before editing a file the session had
    /// explained leaves the explanation where it was.
    #[test]
    fn a_file_changed_again_with_nothing_said_keeps_the_explanation_it_had() {
        let mut events = vec![Event::AssistantMessage {
            text: "Adding the etag header.".to_owned(),
            agent: None,
        }];
        events.extend(edited_by(None, "t1", "src/fetch.rs"));
        events.extend(edited_by(Some("toolu_a"), "t2", "src/fetch.rs"));
        let state = SessionState::replay(&events);

        assert_eq!(state.files()[0].changes, 2);
        assert_eq!(
            state.files()[0].why.as_deref(),
            Some("Adding the etag header.")
        );
    }

    #[test]
    fn a_file_changed_before_the_model_said_anything_has_no_why_invented_for_it() {
        let state = SessionState::replay(&[changed("src/fetch.rs", Some(1), Some(1))]);
        assert_eq!(state.files()[0].why, None);
    }

    /// A report that names no window says only whether the plan is spending
    /// beyond its fee; the levels last reported still stand.
    #[test]
    fn a_report_of_overage_alone_keeps_the_levels_before_it() {
        use crate::event::{UsageWindow, UsageWindows};
        let level = Some(UsageWindow {
            utilization: 0.97,
            resets_at: Some(1_789_779_600),
        });
        let mut state = SessionState::new();
        state.apply(&Event::UsageWindows(UsageWindows {
            five_hour: level,
            seven_day: None,
            using_overage: false,
        }));

        state.apply(&Event::UsageWindows(UsageWindows {
            five_hour: None,
            seven_day: None,
            using_overage: true,
        }));

        assert_eq!(
            state.usage_windows().copied(),
            Some(UsageWindows {
                five_hour: level,
                seven_day: None,
                using_overage: true,
            })
        );
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

    #[test]
    fn a_cleared_conversation_has_no_context_until_its_first_request() {
        let state = SessionState::replay(&[context(150_000), Event::Cleared]);
        assert_eq!(state.context(), None);

        let state = SessionState::replay(&[context(150_000), Event::Cleared, context(9_000)]);
        assert_eq!(state.context().map(|c| c.tokens), Some(9_000));
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

    /// Folding is a function of the events alone: the same events folded in
    /// two runs, into a state that has already folded the first of them,
    /// land where folding them all in one does.
    #[test]
    fn folding_events_in_two_runs_lands_where_folding_them_in_one_does() {
        let events = vec![
            Event::UserMessage {
                text: "add etags".to_owned(),
            },
            usage(1_000, 100, Some(0.04)),
            Event::TurnEnded,
            Event::UserMessage {
                text: "and the tests".to_owned(),
            },
            usage(500, 50, None),
        ];
        let (first, second) = events.split_at(3);
        let mut state = SessionState::replay(first);
        for event in second {
            state.apply(event);
        }

        assert_eq!(state, SessionState::replay(&events));
        assert_eq!(state.turns().len(), 1);
        assert_eq!(state.totals().records, 2);
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
            settles_model: false,
            fast: false,
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
        assert_eq!((turns[0].number, turns[0].tokens), (1, Some(10_025)));
        assert_eq!((turns[1].number, turns[1].tokens), (2, Some(9_047)));
        assert_eq!(state.totals().tokens(), 19_072);
    }

    /// A turn no usage record landed in — interrupted, or failed before the
    /// first message — has no token figure, which is not the same as one
    /// that reported spending none.
    #[test]
    fn a_turn_nothing_reported_usage_for_has_no_token_figure() {
        let state = SessionState::replay(&[
            prompt(),
            cached(100, 10, 0, 0),
            Event::TurnEnded,
            prompt(),
            Event::TurnEnded,
            prompt(),
            cached(0, 0, 0, 0),
            Event::TurnEnded,
        ]);
        let tokens: Vec<Option<u64>> = state.turns().iter().map(|turn| turn.tokens).collect();
        assert_eq!(tokens, [Some(110), None, Some(0)]);
    }

    /// Whether a turn's tokens were measured is decided over the same span
    /// its tokens are counted over: from the prompt that opened it where the
    /// fold saw one, and from the end of the turn before where it did not.
    #[test]
    fn a_turns_tokens_are_measured_over_the_span_they_are_counted_over() {
        let state = SessionState::replay(&[
            prompt(),
            Event::TurnEnded,
            cached(30, 0, 0, 0),
            Event::TurnEnded,
            cached(20, 0, 0, 0),
            prompt(),
            Event::TurnEnded,
        ]);
        let tokens: Vec<Option<u64>> = state.turns().iter().map(|turn| turn.tokens).collect();
        assert_eq!(tokens, [None, Some(30), None]);
    }

    #[test]
    fn a_reply_left_half_streamed_by_its_turn_is_not_the_start_of_the_next() {
        let state = SessionState::replay(&[
            prompt(),
            Event::AssistantDelta {
                text: "Hello wor".to_owned(),
            },
            Event::TurnEnded,
            prompt(),
            Event::AssistantDelta {
                text: "New".to_owned(),
            },
        ]);
        assert_eq!(state.pending_assistant(), "New");

        let ended = SessionState::replay(&[
            prompt(),
            Event::AssistantDelta {
                text: "cut".to_owned(),
            },
            Event::Error {
                message: "the CLI exited".to_owned(),
                fatal: true,
            },
        ]);
        assert_eq!(ended.pending_assistant(), "");
    }

    #[test]
    fn an_agent_the_end_interrupted_does_not_also_complete() {
        let state = SessionState::replay(&[
            Event::AgentSpawn {
                id: "a1".into(),
                parent: None,
                kind: None,
                label: "review".to_owned(),
            },
            Event::Error {
                message: "the CLI exited".to_owned(),
                fatal: true,
            },
            Event::AgentExit {
                id: "a1".into(),
                outcome: AgentOutcome::Completed,
            },
        ]);

        assert_eq!(state.agents_spawned(), 1);
        assert_eq!(state.agents_interrupted(), 1);
        assert_eq!(state.agents_completed(), 0);
    }

    #[test]
    fn token_totals_stop_at_their_largest_value_rather_than_wrapping() {
        let huge = usage(u64::MAX, u64::MAX, None);
        let state = SessionState::replay(&[huge.clone(), huge]);

        assert_eq!(state.totals().input, u64::MAX);
        assert_eq!(state.totals().output, u64::MAX);
        assert_eq!(state.totals().tokens(), u64::MAX);
        assert_eq!(state.totals().records, 2);
    }

    /// A backend that says a turn ended twice in a row has ended one turn,
    /// so the second records no turn of its own. What makes it a repeat is
    /// that nothing came between the two: any event between them, a notice
    /// included, makes the second the end of a turn the fold did not see
    /// begin (see [`only_an_end_straight_after_an_end_is_a_repeat`]).
    #[test]
    fn a_turn_end_said_again_straight_after_an_end_records_no_turn() {
        let state = SessionState::replay(&[
            prompt(),
            usage(100, 0, None),
            Event::TurnEnded,
            Event::TurnEnded,
        ]);
        let turns: Vec<(u64, Option<u64>)> = state
            .turns()
            .iter()
            .map(|turn| (turn.number, turn.tokens))
            .collect();
        assert_eq!(turns, [(1, Some(100))]);
    }

    /// The same holds for an end said after a fatal error already ended the
    /// turn; and an end the fold never saw begin, with a reply before it, is
    /// a turn — a command the CLI answers itself sends no prompt.
    #[test]
    fn only_an_end_straight_after_an_end_is_a_repeat() {
        let state = SessionState::replay(&[
            prompt(),
            Event::Error {
                message: "the CLI exited".to_owned(),
                fatal: true,
            },
            Event::TurnEnded,
            Event::AssistantMessage {
                text: "## Context Usage".to_owned(),
                agent: None,
            },
            Event::TurnEnded,
            Event::TurnEnded,
        ]);
        assert_eq!(state.turns().len(), 2, "{:?}", state.turns());
    }

    #[test]
    fn a_turn_still_running_has_no_record() {
        let state = SessionState::replay(&[prompt(), cached(100, 10, 0, 0)]);
        assert!(state.turns().is_empty(), "{:?}", state.turns());
    }

    #[test]
    fn what_a_turn_total_carried_beyond_its_messages_is_kept_apart_from_the_totals() {
        let counts = |input, output, cache_read, cache_write| TokenCounts {
            input,
            output,
            cache_read,
            cache_write,
        };
        let differs = |per_message, reported| Event::TurnTotalDiffers {
            per_message,
            reported,
            backend_session: Some("cli-session".to_owned()),
            unfinished: vec!["msg_1".to_owned()],
        };

        let state = SessionState::replay(&[
            differs(
                counts(38, 19_386, 1_832_928, 117_517),
                counts(40, 19_394, 1_950_998, 120_328),
            ),
            // A turn whose messages reported more than its total adds nothing:
            // there is nothing beyond the messages to account for.
            differs(counts(5, 5, 5, 5), counts(4, 4, 4, 4)),
        ]);

        assert_eq!(*state.beyond_messages(), counts(2, 8, 118_070, 2_811));
        assert_eq!(state.totals().tokens(), 0);
        assert_eq!(state.errors(), 0);
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
        assert_eq!(state.turns()[0].tokens, Some(110));
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
    fn a_turn_that_heard_only_of_overage_has_no_share() {
        let state = SessionState::replay(&[
            prompt(),
            five_hour(0.14, RESET),
            Event::TurnEnded,
            prompt(),
            Event::UsageWindows(UsageWindows {
                five_hour: None,
                seven_day: None,
                using_overage: true,
            }),
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

    /// The level falling within one reset is no measure of what the turn
    /// spent, and a share of nothing would read as a measured one.
    #[test]
    fn a_level_that_fell_within_one_reset_gives_no_share() {
        let state = SessionState::replay(&[
            prompt(),
            five_hour(0.15, RESET),
            Event::TurnEnded,
            prompt(),
            five_hour(0.14, RESET),
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
        let tokens: Vec<Option<u64>> = state.turns().iter().map(|turn| turn.tokens).collect();
        assert_eq!(tokens, [Some(100), Some(30)]);
    }

    /// What arrives between an end and the next prompt is in neither turn:
    /// a turn the fold saw begin is measured from its prompt, not from the
    /// end of the turn before.
    #[test]
    fn a_turn_with_a_prompt_counts_from_the_prompt_and_not_from_the_last_end() {
        let state = SessionState::replay(&[
            prompt(),
            cached(100, 0, 0, 0),
            Event::TurnEnded,
            cached(20, 0, 0, 0),
            prompt(),
            cached(5, 0, 0, 0),
            Event::TurnEnded,
        ]);
        let tokens: Vec<Option<u64>> = state.turns().iter().map(|turn| turn.tokens).collect();
        assert_eq!(tokens, [Some(100), Some(5)]);
    }

    /// The same for a report of the window: a level reported between turns
    /// is where the next turn began, not part of what it moved.
    #[test]
    fn a_window_reported_between_turns_is_where_the_next_one_began() {
        let state = SessionState::replay(&[
            prompt(),
            five_hour(0.14, RESET),
            Event::TurnEnded,
            five_hour(0.16, RESET),
            prompt(),
            five_hour(0.17, RESET),
            Event::TurnEnded,
        ]);
        let share = state.turns()[1]
            .five_hour_share
            .expect("both ends were reported");
        assert!((share - 0.01).abs() < 1e-9, "{share}");
    }

    fn command(name: &str) -> SlashCommand {
        SlashCommand {
            name: name.to_owned(),
            description: format!("what /{name} does"),
            argument_hint: None,
            mid_prompt: false,
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

    #[test]
    fn a_turn_the_session_left_running_is_cut_and_the_next_prompt_opens_its_own() {
        let said = |text: &str| Event::UserMessage {
            text: text.to_owned(),
        };
        let state = SessionState::replay(&[
            said("slow"),
            Event::AssistantDelta {
                text: "tick 0".to_owned(),
            },
            Event::SessionLeft,
            said("again"),
            Event::TurnEnded,
        ]);

        let cut: Vec<bool> = state.turns().iter().map(|turn| turn.cut).collect();
        assert_eq!(cut, [true, false]);
        assert!(!state.turn_running());
    }

    #[test]
    fn a_session_left_with_no_turn_running_ends_no_turn() {
        let state = SessionState::replay(&[
            Event::UserMessage {
                text: "go".to_owned(),
            },
            Event::TurnEnded,
            Event::SessionLeft,
        ]);

        assert_eq!(state.turns().len(), 1);
    }

    #[test]
    fn a_session_billed_one_way_and_then_the_other_says_so() {
        let billed = |billing| Event::Billing { billing };
        let once = SessionState::replay(&[billed(Billing::Plan), billed(Billing::Plan)]);
        assert!(!once.billing_changed());

        let twice = SessionState::replay(&[billed(Billing::Plan), billed(Billing::Metered)]);
        assert!(twice.billing_changed());
        assert_eq!(twice.billing(), Some(Billing::Metered));
    }

    #[test]
    fn costs_that_add_up_past_what_a_float_holds_stay_finite() {
        let costly = || {
            Event::Usage(Usage {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                cache_write_1h: 0,
                reasoning: 0,
                model: "opus-5".to_owned(),
                cost_usd: Some(1e308),
                settles_model: false,
                fast: false,
            })
        };
        let state = SessionState::replay(&[costly(), costly()]);

        assert!(state.totals().reported_cost_usd.is_finite());
    }

    #[test]
    fn a_compaction_is_a_phase_from_its_start_to_its_end() {
        let mut state = SessionState::new();
        state.apply(&Event::UserMessage {
            text: "/compact".to_owned(),
        });
        assert!(!state.compacting());

        state.apply(&Event::CompactionStarted);
        assert!(state.compacting());

        state.apply(&Event::CompactionEnded);
        assert!(!state.compacting());
    }

    #[test]
    fn a_compaction_still_running_when_its_turn_ends_ends_with_it() {
        for end in [
            Event::TurnEnded,
            Event::SessionLeft,
            Event::Cleared,
            Event::Error {
                message: "gone".to_owned(),
                fatal: true,
            },
        ] {
            let state = SessionState::replay(&[
                Event::UserMessage {
                    text: "/compact".to_owned(),
                },
                Event::CompactionStarted,
                end.clone(),
            ]);
            assert!(!state.compacting(), "{end:?}");
        }
    }

    #[test]
    fn a_turn_that_compacted_the_conversation_is_recorded_with_what_started_it() {
        for trigger in [
            CompactTrigger::Manual,
            CompactTrigger::Auto,
            CompactTrigger::Unstated,
        ] {
            let state = SessionState::replay(&[
                Event::UserMessage {
                    text: "go".to_owned(),
                },
                Event::CompactionStarted,
                Event::CompactionEnded,
                Event::Compacted {
                    trigger,
                    before: Some(23_193),
                },
                Event::TurnEnded,
                Event::UserMessage {
                    text: "again".to_owned(),
                },
                Event::TurnEnded,
            ]);
            let compacted: Vec<_> = state.turns().iter().map(|turn| turn.compacted).collect();
            assert_eq!(compacted, [Some(trigger), None]);
        }
    }

    fn ended(id: &str, name: &str, command: Option<&str>, outcome: ToolOutcome) -> [Event; 2] {
        [
            Event::ToolCallStart {
                id: id.into(),
                name: name.to_owned(),
                input: "{}".to_owned(),
                summary: None,
                agent: None,
            },
            Event::ToolCallEnd {
                id: id.into(),
                name: name.to_owned(),
                input: "{}".to_owned(),
                output: String::new(),
                bytes: 0,
                outcome,
                summary: None,
                exit_code: None,
                error: None,
                command: command.map(str::to_owned),
            },
        ]
    }

    #[test]
    fn every_finished_call_is_counted_under_one_kind_of_work_with_what_did_it() {
        let events: Vec<Event> = [
            ended("a", "Bash", Some("grep -n x f | head"), ToolOutcome::Ok),
            ended("b", "Bash", Some("grep y f"), ToolOutcome::Ok),
            ended("c", "Read", None, ToolOutcome::Ok),
            ended("d", "Bash", Some("cargo test"), ToolOutcome::Failed),
            ended("e", "Bash", Some("rm -rf build"), ToolOutcome::Denied),
            ended(
                "f",
                "mcp__claude_ai_Notion__notion-fetch",
                None,
                ToolOutcome::Ok,
            ),
        ]
        .into_iter()
        .flatten()
        .collect();
        let state = SessionState::replay(&events);
        let tools = state.tools();

        let exploring = &tools.by_work[&Work::Exploring];
        assert_eq!(exploring.calls, 3);
        assert_eq!(
            exploring.specifics,
            BTreeMap::from([
                ("grep".to_owned(), 2),
                ("head".to_owned(), 1),
                ("Read".to_owned(), 1),
            ])
        );
        let building = &tools.by_work[&Work::Building];
        assert_eq!((building.calls, building.failed), (1, 1));
        let editing = &tools.by_work[&Work::Editing];
        assert_eq!((editing.calls, editing.denied), (1, 1));
        assert_eq!(tools.by_work[&Work::Server("Notion".to_owned())].calls, 1);

        let calls: u64 = tools.by_work.values().map(|kind| kind.calls).sum();
        let failed: u64 = tools.by_work.values().map(|kind| kind.failed).sum();
        let denied: u64 = tools.by_work.values().map(|kind| kind.denied).sum();
        assert_eq!(
            (calls, failed, denied),
            (tools.finished, tools.failed, tools.denied)
        );
    }

    #[test]
    fn an_end_said_again_is_not_a_second_call_of_its_kind() {
        let [start, end] = ended("a", "Bash", Some("ls"), ToolOutcome::Ok);
        let state = SessionState::replay(&[start, end.clone(), end]);
        assert_eq!(state.tools().by_work[&Work::Exploring].calls, 1);
    }
}
