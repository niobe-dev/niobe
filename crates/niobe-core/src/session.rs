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
    AgentId, AgentOutcome, CheckpointId, Event, SessionMeta, ToolCallId, ToolOutcome, Usage,
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
    totals: Totals,
    tools: ToolTotals,
    in_flight_tools: BTreeMap<ToolCallId, String>,
    user_messages: u64,
    assistant_messages: u64,
    pending_assistant: String,
    last_assistant: Option<String>,
    permission_requests: u64,
    permissions_denied: u64,
    pending_permissions: BTreeSet<ToolCallId>,
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
            Event::SessionMeta(meta) => self.meta = Some(meta.clone()),

            Event::UserMessage { .. } => self.user_messages += 1,

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
                }
            }

            // A notice changes nothing that is counted: it explains the
            // numbers around it, and the transcript is where it is read.
            Event::Notice { .. } => {}
        }
    }

    /// What is running, once the backend has said.
    pub fn meta(&self) -> Option<&SessionMeta> {
        self.meta.as_ref()
    }

    /// Token and cost totals.
    pub fn totals(&self) -> &Totals {
        &self.totals
    }

    /// Tool call counters.
    pub fn tools(&self) -> &ToolTotals {
        &self.tools
    }

    /// Tool calls that started and have not ended, by name.
    pub fn in_flight_tools(&self) -> &BTreeMap<ToolCallId, String> {
        &self.in_flight_tools
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Backend, PermissionDecision};

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
    fn tool_calls_pair_up_and_the_unpaired_ones_are_visible() {
        let mut state = SessionState::new();
        state.apply(&Event::ToolCallStart {
            id: "t1".into(),
            name: "Read".to_owned(),
            input: "fetch.ts".to_owned(),
        });
        assert_eq!(state.in_flight_tools().len(), 1);

        state.apply(&Event::ToolCallEnd {
            id: "t1".into(),
            name: "Read".to_owned(),
            input: "fetch.ts".to_owned(),
            output: "212 lines".to_owned(),
            bytes: 4_096,
            outcome: ToolOutcome::Ok,
        });
        state.apply(&Event::ToolCallEnd {
            id: "t9".into(),
            name: "Bash".to_owned(),
            input: "npm test".to_owned(),
            output: "1 failing".to_owned(),
            bytes: 128,
            outcome: ToolOutcome::Failed,
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
            input: "rm -rf build".to_owned(),
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
