// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The event model: the one type every backend produces into.
//!
//! A backend — the `claude` bridge, the `codex` bridge, a native engine —
//! produces [`Event`]s on a channel. The TUI and the ledger consume them and
//! never learn which backend was running: that is what keeps a second backend
//! cheap, and it is the load-bearing decision of the architecture.
//!
//! Two properties are deliberate:
//!
//! * **No backend-specific types.** Nothing here names a vendor wire format. A
//!   bridge translates its own protocol into these variants and keeps its own
//!   types to itself.
//! * **No timestamps and no sequence numbers.** An `Event` is what happened,
//!   not when it was recorded. Whatever persists a stream stores each event in
//!   an append-only record that carries the sequence number and the wall clock,
//!   so that the same `Event` can be replayed from a live channel or from disk
//!   without two shapes existing. The alternative — an envelope struct with
//!   `at` and `seq` on it — was rejected because it forces every producer to
//!   invent a clock and every test fixture to carry one.

use serde::{Deserialize, Serialize};

/// Declares a string newtype used to correlate events with each other.
///
/// The ids are opaque: a backend supplies whatever it uses (a UUID, a counter,
/// a tool-use id from a vendor protocol) and Niobe only ever compares them.
macro_rules! id_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wraps an id produced by a backend.
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            /// The id as the backend spelled it.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(id: &str) -> Self {
                Self::new(id)
            }
        }

        impl From<String> for $name {
            fn from(id: String) -> Self {
                Self(id)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_newtype! {
    /// Correlates [`Event::ToolCallStart`] with [`Event::ToolCallEnd`], and a
    /// permission exchange with the call it gates.
    ToolCallId
}

id_newtype! {
    /// Identifies a sub-agent across its spawn and its exit.
    AgentId
}

id_newtype! {
    /// Identifies a checkpoint so that a rewind can name what it returns to.
    CheckpointId
}

/// Which engine produced the event stream.
///
/// A tag, not a type: the variants let the UI say what is running and let the
/// ledger pick a rate card. Nothing backend-specific travels with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// The official `claude` CLI, driven as a subprocess.
    Claude,
    /// The official `codex` CLI, driven as a subprocess.
    Codex,
    /// Niobe's own agent loop against a provider API.
    Native,
}

impl Backend {
    /// The name shown in the status line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Native => "native",
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a session is running: which backend, under which profile, on which
/// model. Produced once, first, by every backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    /// The engine producing this stream.
    pub backend: Backend,
    /// The named profile that selected the backend and its credentials.
    pub profile: String,
    /// The model the session starts on. A routing decision may move it, in
    /// which case the backend produces a fresh `SessionMeta`.
    pub model: String,
}

/// Token counts and, when the backend reports one, the billed cost of a single
/// unit of work.
///
/// `cost_usd` is `None` whenever the backend did not report money. Niobe never
/// fills that in here: a derived figure is the ledger's job and carries a
/// provenance label with it, so a guess can never be shown as a measurement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens billed at the full rate (cache reads and writes excluded).
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Input tokens served from the prompt cache.
    pub cache_read: u64,
    /// Input tokens written into the prompt cache, whatever their lifetime.
    pub cache_write: u64,
    /// Of [`Usage::cache_write`], the tokens written to live for an hour
    /// rather than the default five minutes. A provider that offers both bills
    /// the hour at a higher rate, so a cost derived from the total alone would
    /// be wrong in whichever direction it guessed. Zero where the backend does
    /// not report the split, and in records written before it was kept.
    #[serde(default)]
    pub cache_write_1h: u64,
    /// Reasoning tokens, where the backend reports them separately.
    pub reasoning: u64,
    /// The model these counts were billed against.
    pub model: String,
    /// The cost the backend reported, in USD. `None` means "not reported",
    /// never "zero".
    pub cost_usd: Option<f64>,
}

impl Usage {
    /// Every token in this record, cache traffic included.
    pub fn tokens(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
            .saturating_add(self.reasoning)
    }
}

/// How a tool call ended. Drives waste accounting: a failed call is spend with
/// nothing to show for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    /// The tool ran and reported success.
    Ok,
    /// The tool ran and failed, or could not run.
    Failed,
    /// The user denied the permission request that gated the call.
    Denied,
}

/// The answer to a permission request.
///
/// Rule scoping — "always this tool" against "always this pattern" — belongs
/// to whatever answers the prompt; the stream only records what was decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    /// Allowed this once.
    Allow,
    /// Allowed, and a rule was stored so the prompt does not return.
    AllowAlways,
    /// Denied.
    Deny,
}

impl PermissionDecision {
    /// Whether the call was let through.
    pub fn allowed(self) -> bool {
        matches!(self, Self::Allow | Self::AllowAlways)
    }
}

/// How a sub-agent finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOutcome {
    /// Finished its task.
    Completed,
    /// Failed.
    Failed,
    /// Killed from the parallel pane, or by a budget.
    Cancelled,
}

/// Everything that can happen in a session.
///
/// Serialized internally tagged, so a recorded log is one JSON object per line
/// with a `type` field, readable without the schema in hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// What is running. Produced first, and again whenever the model changes.
    SessionMeta(SessionMeta),

    /// The operator said something.
    UserMessage {
        /// The prompt as typed.
        text: String,
    },

    /// A fragment of the assistant's reply, as it streams.
    AssistantDelta {
        /// The fragment. Consumers append; they do not replace.
        text: String,
    },

    /// The assistant's reply, complete. Carries the whole message, so a
    /// consumer that joined late or dropped deltas still ends up correct.
    AssistantMessage {
        /// The finished message.
        text: String,
    },

    /// A tool call started.
    ToolCallStart {
        /// Correlates with the matching [`Event::ToolCallEnd`].
        id: ToolCallId,
        /// Tool name, as the backend spells it: `Read`, `Bash`, `Edit`.
        name: String,
        /// The arguments, rendered for display and for attribution.
        input: String,
    },

    /// A tool call finished.
    ///
    /// `name` and `input` repeat what the start carried. The duplication is
    /// deliberate: the ledger attributes a call to a file and a category from
    /// this one event, without holding open state, and a consumer that missed
    /// the start still accounts for the call.
    ToolCallEnd {
        /// The id from the matching [`Event::ToolCallStart`].
        id: ToolCallId,
        /// Tool name.
        name: String,
        /// The arguments the call ran with.
        input: String,
        /// What the tool returned, as it will be fed back to the model.
        output: String,
        /// Size of `output` in bytes, before any trimming. This is the number
        /// any savings from trimming tool output are measured against.
        bytes: u64,
        /// Success, failure or denial.
        outcome: ToolOutcome,
    },

    /// Tokens and, when reported, money.
    Usage(Usage),

    /// A tool call is waiting on the operator.
    PermissionRequest {
        /// The call being gated.
        id: ToolCallId,
        /// Tool name.
        tool: String,
        /// The arguments the operator is approving.
        input: String,
    },

    /// The operator answered a permission request.
    PermissionResponse {
        /// The call that was gated.
        id: ToolCallId,
        /// What was decided.
        decision: PermissionDecision,
    },

    /// A structured `decide` record: why a plan, a model, a file or a declined
    /// scope was chosen. Surfaced in the changes pane and exported into commit
    /// and PR bodies.
    Decision {
        /// One line, as it appears in the pane.
        summary: String,
        /// The reasoning, where there is more of it than fits on one line.
        rationale: Option<String>,
        /// What was considered and rejected.
        rejected: Vec<String>,
    },

    /// A restore point, taken before an edit so that a rewind has somewhere to
    /// land.
    Checkpoint {
        /// Names the point a rewind returns to.
        id: CheckpointId,
        /// What was about to happen, for the undo list.
        label: String,
    },

    /// A sub-agent started.
    AgentSpawn {
        /// The new agent.
        id: AgentId,
        /// The agent that spawned it, if it was not the session itself.
        parent: Option<AgentId>,
        /// What it was spawned to do, for the parallel pane.
        label: String,
    },

    /// A sub-agent finished.
    AgentExit {
        /// The agent that finished.
        id: AgentId,
        /// How it finished.
        outcome: AgentOutcome,
    },

    /// Something went wrong.
    Error {
        /// The message, as it will be shown.
        message: String,
        /// Whether the session ended with it.
        fatal: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_tokens_counts_cache_traffic() {
        let usage = Usage {
            input: 10,
            output: 20,
            cache_read: 30,
            cache_write: 40,
            cache_write_1h: 0,
            reasoning: 50,
            model: "opus-5".to_owned(),
            cost_usd: None,
        };
        assert_eq!(usage.tokens(), 150);
    }

    #[test]
    fn one_hour_cache_writes_are_a_share_of_cache_write_not_more_tokens() {
        let usage = Usage {
            input: 10,
            output: 20,
            cache_read: 30,
            cache_write: 40,
            cache_write_1h: 25,
            reasoning: 50,
            model: "opus-5".to_owned(),
            cost_usd: None,
        };
        assert_eq!(usage.tokens(), 150);
    }

    #[test]
    fn a_usage_record_without_one_hour_writes_reads_them_as_zero() {
        let json = r#"{"type":"usage","input":1,"output":2,"cache_read":3,"cache_write":4,"reasoning":0,"model":"opus-5","cost_usd":null}"#;
        let event: Event = serde_json::from_str(json).expect("a record from before the field");
        let Event::Usage(usage) = event else {
            panic!("the record is a usage record: {event:?}");
        };
        assert_eq!(usage.cache_write, 4);
        assert_eq!(usage.cache_write_1h, 0);
    }

    #[test]
    fn an_unreported_cost_is_none_not_zero() {
        let usage = Usage {
            input: 1,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 0,
            model: "gpt-5-codex".to_owned(),
            cost_usd: None,
        };
        assert!(usage.cost_usd.is_none());
    }

    #[test]
    fn ids_are_opaque_strings() {
        let id = ToolCallId::from("toolu_01");
        assert_eq!(id.as_str(), "toolu_01");
        assert_eq!(id.to_string(), "toolu_01");
        assert_eq!(id, ToolCallId::new(String::from("toolu_01")));
    }

    #[test]
    fn allowed_covers_both_allow_variants() {
        assert!(PermissionDecision::Allow.allowed());
        assert!(PermissionDecision::AllowAlways.allowed());
        assert!(!PermissionDecision::Deny.allowed());
    }
}
