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
//! reported and counts the records that reported none, so the ledger can label
//! a figure measured, API-equivalent or unpriced. A number invented here would
//! be indistinguishable from a measured one, which is the product gone.

use std::collections::{BTreeMap, BTreeSet};

use crate::event::{
    AgentId, AgentOutcome, CheckpointId, Event, Mode, SessionMeta, ToolCallId, ToolOutcome, Usage,
    UsageWindows,
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
    /// How many usage records carried no cost. While this is non-zero,
    /// [`Totals::reported_cost_usd`] is a floor, not the session's cost.
    pub records_without_cost: u64,
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

    /// Whether every usage record came with a cost, i.e. whether
    /// [`Totals::reported_cost_usd`] is the whole bill and may be shown as a
    /// measurement rather than a floor.
    pub fn cost_fully_reported(&self) -> bool {
        self.records_without_cost == 0
    }

    fn add(&mut self, usage: &Usage) {
        self.input = self.input.saturating_add(usage.input);
        self.output = self.output.saturating_add(usage.output);
        self.cache_read = self.cache_read.saturating_add(usage.cache_read);
        self.cache_write = self.cache_write.saturating_add(usage.cache_write);
        self.cache_write_1h = self.cache_write_1h.saturating_add(usage.cache_write_1h);
        self.reasoning = self.reasoning.saturating_add(usage.reasoning);
        self.records += 1;
        match usage.cost_usd {
            Some(cost) => self.reported_cost_usd += cost,
            None => self.records_without_cost += 1,
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
    tools: ToolTotals,
    in_flight_tools: BTreeMap<ToolCallId, String>,
    /// Whether the backend is still answering the last prompt.
    turn_running: bool,
    user_messages: u64,
    assistant_messages: u64,
    pending_assistant: String,
    last_assistant: Option<String>,
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
    agents_spawned: u64,
    agents_completed: u64,
    agents_failed: u64,
    agents_cancelled: u64,
    running_agents: BTreeSet<AgentId>,
    peak_running_agents: u64,
    errors: u64,
    fatal_error: Option<String>,
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

            Event::UserMessage { .. } => {
                self.user_messages += 1;
                self.turn_running = true;
            }

            Event::TurnEnded => self.turn_running = false,

            Event::AssistantDelta { text } => self.pending_assistant.push_str(text),

            Event::AssistantMessage { text } => {
                self.assistant_messages += 1;
                self.pending_assistant.clear();
                self.last_assistant = Some(text.clone());
            }

            Event::ToolCallStart { id, name, .. } => {
                self.tools.started += 1;
                self.in_flight_tools.insert(id.clone(), name.clone());
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
                    ToolOutcome::Failed => self.tools.failed += 1,
                    ToolOutcome::Denied => self.tools.denied += 1,
                }
                if self.in_flight_tools.remove(id).is_none() {
                    self.tools.unmatched_ends += 1;
                }
            }

            Event::Usage(usage) => self.totals.add(usage),

            // The last report replaces the one before it: a window is a level,
            // not a quantity, so summing two reports of it would be nonsense.
            Event::UsageWindows(windows) => self.usage_windows = Some(*windows),

            Event::PermissionRequest { id, .. } => {
                self.permission_requests += 1;
                self.pending_permissions.insert(id.clone());
            }

            Event::PermissionResponse { id, decision } => {
                self.pending_permissions.remove(id);
                if !decision.allowed() {
                    self.permissions_denied += 1;
                }
            }

            Event::FileChange {
                path,
                added,
                removed,
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

            Event::AgentSpawn { id, .. } => {
                self.agents_spawned += 1;
                self.running_agents.insert(id.clone());
                let running = self.running_agents.len() as u64;
                self.peak_running_agents = self.peak_running_agents.max(running);
            }

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
                    self.turn_running = false;
                }
            }

            // A notice changes nothing that is counted: it explains the
            // numbers around it, and the transcript is where it is read.
            Event::Notice { .. } => {}
        }
    }

    /// Folds one file change in, against the file's running totals.
    ///
    /// The "why" is taken here rather than carried on the event: the fold is
    /// what knows the order the stream arrived in, and every backend gets the
    /// same rule for free — the last thing the model said before this call is
    /// the last [`Event::AssistantMessage`] the fold saw, because nothing else
    /// speaks between a call and its result.
    fn change_file(&mut self, path: &str, added: Option<u64>, removed: Option<u64>) {
        let why = self
            .last_assistant
            .as_deref()
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

    /// How many prompts the operator sent.
    pub fn user_messages(&self) -> u64 {
        self.user_messages
    }

    /// How many replies the assistant completed.
    pub fn assistant_messages(&self) -> u64 {
        self.assistant_messages
    }

    /// The reply currently streaming, assembled from deltas. Empty between
    /// messages.
    pub fn pending_assistant(&self) -> &str {
        &self.pending_assistant
    }

    /// The last completed reply.
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

    /// The decisions log, in order.
    pub fn decisions(&self) -> &[DecisionRecord] {
        &self.decisions
    }

    /// The checkpoints taken, in order.
    pub fn checkpoints(&self) -> &[CheckpointRecord] {
        &self.checkpoints
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

    /// The error that ended the session, if one did.
    pub fn fatal_error(&self) -> Option<&str> {
        self.fatal_error.as_deref()
    }
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
    use crate::event::{Backend, Mode, PermissionDecision};

    fn usage(input: u64, output: u64, cost: Option<f64>) -> Event {
        Event::Usage(Usage {
            input,
            output,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 0,
            model: "opus-5".to_owned(),
            cost_usd: cost,
            cost_basis: None,
        })
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
        assert_eq!(totals.records_without_cost, 1);
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
        });
        state.apply(&Event::ToolCallEnd {
            id: "t9".into(),
            name: "Bash".to_owned(),
            input: "npm test".to_owned(),
            output: "1 failing".to_owned(),
            bytes: 128,
            outcome: ToolOutcome::Failed,
            summary: None,
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
        }
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
            },
            changed("src/fetch.rs", Some(9), Some(0)),
            Event::AssistantMessage {
                text: "  Short-circuiting the 304 path.  ".to_owned(),
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
            },
            changed("src/fetch.rs", Some(9), Some(0)),
            Event::AssistantMessage {
                text: "Short-circuiting the 304 path.".to_owned(),
            },
            changed("src/fetch.rs", Some(4), Some(1)),
        ]);

        assert_eq!(
            state.files()[0].why.as_deref(),
            Some("Short-circuiting the 304 path.")
        );
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
}
