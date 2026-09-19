// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The `claude` CLI's stream-json protocol, as Rust types.
//!
//! Everything here is vendor shape and stays inside this crate: nothing in
//! this module is public, so no field of the CLI's protocol can reach the TUI
//! or the ledger (`cargo xtask layering` enforces the crate half of that; this
//! module's privacy enforces the rest).
//!
//! Every type ignores fields it does not name, and every field the CLI may
//! omit is optional or defaulted. That is deliberate: the CLI ships new keys
//! with new versions, and a bridge that refuses a message because it grew a
//! field is a bridge that stops working on the next release. A message shape
//! that changed so far that it no longer parses becomes a warning entry rather
//! than a failure — see [`crate::translate`].

use serde::Deserialize;

/// One line of the CLI's standard output.
///
/// [`Message::Unknown`] is the whole point of the enum: an unrecognised `type`
/// is a value, not a parse error, so the translator can report it and carry on.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Message {
    /// The CLI talking about the session rather than about the conversation.
    System(System),
    /// A message from the model, complete.
    Assistant(Envelope),
    /// A turn from the operator, or a tool result the CLI is feeding back.
    User(Envelope),
    /// A fragment of a message, from `--include-partial-messages`.
    StreamEvent(StreamEvent),
    /// The CLI asking Niobe something over the stdio control channel.
    ControlRequest(ControlRequest),
    /// The CLI answering something Niobe asked it.
    ControlResponse(ControlResponse),
    /// How much of the plan's usage windows is gone.
    RateLimitEvent(RateLimit),
    /// The end of a turn, with the turn's totals.
    Result(Outcome),
    /// A `type` this bridge does not know.
    #[serde(other)]
    Unknown,
}

/// The `type` of a line, read on its own so that an unrecognised or unparsable
/// message can be named in the warning it becomes.
#[derive(Debug, Deserialize)]
pub(crate) struct Tag {
    #[serde(rename = "type")]
    pub(crate) kind: Option<String>,
}

/// `system`, of whichever `subtype`.
#[derive(Debug, Deserialize)]
pub(crate) struct System {
    pub(crate) subtype: Option<String>,
    pub(crate) model: Option<String>,
    /// On `init`: which release of the CLI is on the other end. Optional
    /// because a release that stopped naming itself is a release Niobe can say
    /// nothing about, not one to refuse.
    pub(crate) claude_code_version: Option<String>,
    /// On `init`: how the CLI is gating tool calls, in its own spelling.
    #[serde(rename = "permissionMode")]
    pub(crate) permission_mode: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) compact_metadata: Option<CompactMetadata>,
    /// On `permission_denied`: the call that was refused.
    pub(crate) tool_name: Option<String>,
    pub(crate) tool_use_id: Option<String>,
}

/// What the CLI says about a context compaction.
#[derive(Debug, Deserialize)]
pub(crate) struct CompactMetadata {
    pub(crate) trigger: Option<String>,
    pub(crate) pre_tokens: Option<u64>,
}

/// An `assistant` or `user` line.
///
/// The envelope also carries `parent_tool_use_id`, naming the `Task` call
/// whose sub-agent produced the message. It is not read here: a complete
/// message is folded in the same way whichever agent wrote it, and only the
/// fragments of one need to be told apart — see [`StreamEvent`].
#[derive(Debug, Deserialize)]
pub(crate) struct Envelope {
    pub(crate) message: ApiMessage,
}

/// The message itself.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiMessage {
    pub(crate) content: Option<Content>,
}

/// A message's content, which the CLI writes as a bare string for a plain turn
/// and as blocks for anything else.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

/// One content block.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Block {
    Text {
        #[serde(default)]
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Option<Content>,
        is_error: Option<bool>,
    },
    /// Thinking, redacted thinking, images: blocks with no place in the
    /// transcript, kept as a variant so that they parse rather than failing the
    /// message they are in.
    #[serde(other)]
    Other,
}

/// A `stream_event` line.
#[derive(Debug, Deserialize)]
pub(crate) struct StreamEvent {
    pub(crate) event: StreamBody,
    pub(crate) parent_tool_use_id: Option<String>,
}

/// What a `stream_event` carries.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum StreamBody {
    MessageStart {
        message: StartMessage,
    },
    MessageDelta {
        usage: Option<Usage>,
    },
    ContentBlockDelta {
        delta: Delta,
    },
    #[serde(other)]
    Other,
}

/// The head of a message: the only place the model it runs on is named.
#[derive(Debug, Deserialize)]
pub(crate) struct StartMessage {
    pub(crate) model: Option<String>,
}

/// A fragment of one content block.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Delta {
    TextDelta {
        text: String,
    },
    /// Thinking, signatures and the JSON of a tool call's arguments as it is
    /// typed: fragments with nowhere to go in the transcript.
    #[serde(other)]
    Other,
}

/// Token counts, as every message and every turn reports them.
#[derive(Debug, Default, Clone, Deserialize)]
pub(crate) struct Usage {
    #[serde(default)]
    pub(crate) input_tokens: u64,
    #[serde(default)]
    pub(crate) output_tokens: u64,
    #[serde(default)]
    pub(crate) cache_read_input_tokens: u64,
    #[serde(default)]
    pub(crate) cache_creation_input_tokens: u64,
    pub(crate) cache_creation: Option<CacheCreation>,
}

impl Usage {
    /// Of the cache writes, the ones bought for an hour rather than for the
    /// default five minutes. Zero where the CLI reported no split.
    pub(crate) fn cache_write_1h(&self) -> u64 {
        self.cache_creation
            .as_ref()
            .map_or(0, |c| c.ephemeral_1h_input_tokens)
    }
}

/// How the cache writes of a message split by lifetime.
#[derive(Debug, Default, Clone, Deserialize)]
pub(crate) struct CacheCreation {
    #[serde(default)]
    pub(crate) ephemeral_1h_input_tokens: u64,
}

/// A `rate_limit_event`: how much of the plan's metered windows is gone.
///
/// The message also carries `status`, `rateLimitType`, `resetsAt`,
/// `overageStatus` and `overageDisabledReason`. Only the windows and the
/// overage flag are read: the rest restates the window the CLI happens to be
/// closest to, and a status word nothing draws would widen the shared
/// vocabulary for a figure no pane shows.
#[derive(Debug, Deserialize)]
pub(crate) struct RateLimit {
    pub(crate) rate_limit_info: Option<RateLimitInfo>,
}

/// The body of a `rate_limit_event`.
#[derive(Debug, Deserialize)]
pub(crate) struct RateLimitInfo {
    #[serde(rename = "unifiedWindows")]
    pub(crate) unified_windows: Option<UnifiedWindows>,
    /// Whether the plan has started spending beyond its flat fee. Absent on a
    /// CLI version that does not report it, which is read as "did not say"
    /// rather than as a promise that nothing extra is being charged.
    #[serde(rename = "isUsingOverage", default)]
    pub(crate) is_using_overage: bool,
}

/// The two windows a plan is metered against.
#[derive(Debug, Deserialize)]
pub(crate) struct UnifiedWindows {
    pub(crate) five_hour: Option<Window>,
    pub(crate) seven_day: Option<Window>,
}

/// One window: how much of it is gone, and when it starts over.
#[derive(Debug, Deserialize)]
pub(crate) struct Window {
    /// The share used, 0–1. Absent from a window the CLI named without
    /// measuring, which is a window this bridge reports nothing for.
    pub(crate) utilization: Option<f64>,
    #[serde(rename = "resetsAt")]
    pub(crate) resets_at: Option<u64>,
}

/// A `control_request`: the CLI asking Niobe to decide something.
///
/// `request_id` is what an answer is addressed to, and it is the CLI's own id
/// rather than the tool call's: the two are carried separately because one
/// call can be asked about more than once across a resumed session.
#[derive(Debug, Deserialize)]
pub(crate) struct ControlRequest {
    pub(crate) request_id: Option<String>,
    pub(crate) request: Option<ControlBody>,
}

/// What a `control_request` is asking.
#[derive(Debug, Deserialize)]
pub(crate) struct ControlBody {
    pub(crate) subtype: Option<String>,
    pub(crate) tool_name: Option<String>,
    pub(crate) tool_use_id: Option<String>,
    pub(crate) input: Option<serde_json::Value>,
}

/// A `control_response`: the CLI answering a request Niobe made.
///
/// Every one of these answers something this side asked — the CLI's own
/// questions arrive as `control_request` and are answered the other way — so a
/// failure here is a request of Niobe's that did not take effect.
#[derive(Debug, Deserialize)]
pub(crate) struct ControlResponse {
    pub(crate) response: Option<ControlOutcome>,
}

/// How a request Niobe made ended.
#[derive(Debug, Deserialize)]
pub(crate) struct ControlOutcome {
    pub(crate) subtype: Option<String>,
    pub(crate) error: Option<String>,
}

/// The `result` line that closes a turn.
///
/// Its numbers are the CLI's own running totals for the session, not the
/// turn's: see [`crate::translate`], which is where that costs something.
#[derive(Debug, Deserialize)]
pub(crate) struct Outcome {
    pub(crate) subtype: Option<String>,
    #[serde(default)]
    pub(crate) is_error: bool,
    pub(crate) usage: Option<Usage>,
    #[serde(rename = "modelUsage", default)]
    pub(crate) model_usage: std::collections::BTreeMap<String, ModelUsage>,
    pub(crate) total_cost_usd: Option<f64>,
    #[serde(default)]
    pub(crate) permission_denials: Vec<Denial>,
    pub(crate) result: Option<String>,
    /// Why the turn ended, where the CLI says so apart from `result` — a
    /// budget it stopped on says it here and leaves `result` out.
    #[serde(default)]
    pub(crate) errors: Vec<String>,
}

/// What one model has cost the session so far.
#[derive(Debug, Deserialize)]
pub(crate) struct ModelUsage {
    #[serde(rename = "inputTokens", default)]
    pub(crate) input_tokens: u64,
    #[serde(rename = "outputTokens", default)]
    pub(crate) output_tokens: u64,
    #[serde(rename = "cacheReadInputTokens", default)]
    pub(crate) cache_read_input_tokens: u64,
    #[serde(rename = "cacheCreationInputTokens", default)]
    pub(crate) cache_creation_input_tokens: u64,
    #[serde(rename = "costUSD")]
    pub(crate) cost_usd: Option<f64>,
    /// `"list"` where the figure was computed from published prices rather
    /// than billed. A subscription plan reports exactly that.
    #[serde(rename = "costBasis")]
    pub(crate) cost_basis: Option<String>,
}

/// One call the CLI refused to make.
#[derive(Debug, Deserialize)]
pub(crate) struct Denial {
    pub(crate) tool_name: Option<String>,
    pub(crate) tool_use_id: Option<String>,
    pub(crate) tool_input: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_type_this_bridge_does_not_know_parses_as_unknown() {
        let message: Message = serde_json::from_str(r#"{"type":"teleport","to":"mars"}"#)
            .expect("an unknown type is a value, not a parse error");

        assert!(matches!(message, Message::Unknown));
    }

    #[test]
    fn a_known_message_that_grew_a_field_still_parses() {
        let message: Message = serde_json::from_str(
            r#"{"type":"system","subtype":"init","model":"opus-5","a_key_from_a_later_version":7}"#,
        )
        .expect("an unknown field is ignored");

        let Message::System(system) = message else {
            panic!("a system message");
        };
        assert_eq!(system.model.as_deref(), Some("opus-5"));
    }

    #[test]
    fn a_turn_written_as_a_bare_string_parses_like_one_written_as_blocks() {
        let string: Message =
            serde_json::from_str(r#"{"type":"user","message":{"role":"user","content":"hi"}}"#)
                .expect("a bare string turn");
        let blocks: Message = serde_json::from_str(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#,
        )
        .expect("a block turn");

        let (Message::User(string), Message::User(blocks)) = (string, blocks) else {
            panic!("both are user messages");
        };
        assert!(matches!(string.message.content, Some(Content::Text(_))));
        assert!(matches!(blocks.message.content, Some(Content::Blocks(_))));
    }

    #[test]
    fn cache_writes_without_a_reported_split_are_no_one_hour_writes() {
        let usage: Usage = serde_json::from_str(r#"{"cache_creation_input_tokens":99}"#)
            .expect("usage without a split");

        assert_eq!(usage.cache_creation_input_tokens, 99);
        assert_eq!(usage.cache_write_1h(), 0);
    }

    #[test]
    fn the_one_hour_share_is_read_past_the_five_minute_one_beside_it() {
        let usage: Usage = serde_json::from_str(
            r#"{"cache_creation_input_tokens":90,"cache_creation":{"ephemeral_1h_input_tokens":60,"ephemeral_5m_input_tokens":30}}"#,
        )
        .expect("usage with both lifetimes");

        assert_eq!(usage.cache_creation_input_tokens, 90);
        assert_eq!(usage.cache_write_1h(), 60);
    }

    #[test]
    fn a_window_the_cli_named_without_measuring_carries_no_share() {
        let message: Message = serde_json::from_str(
            r#"{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"five_hour":{"resetsAt":1789779600}}}}"#,
        )
        .expect("a rate limit event");

        let Message::RateLimitEvent(event) = message else {
            panic!("a rate limit event");
        };
        let info = event.rate_limit_info.expect("the event carries a body");
        let windows = info.unified_windows.expect("the body carries its windows");
        let five_hour = windows.five_hour.expect("the five-hour window is named");
        assert_eq!(five_hour.utilization, None);
        assert_eq!(five_hour.resets_at, Some(1_789_779_600));
        assert!(windows.seven_day.is_none());
        assert!(!info.is_using_overage);
    }

    #[test]
    fn a_permission_request_is_read_from_inside_its_request() {
        let message: Message = serde_json::from_str(
            r#"{"type":"control_request","request_id":"c1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"echo one"},"tool_use_id":"toolu_1"}}"#,
        )
        .expect("a control request");

        let Message::ControlRequest(request) = message else {
            panic!("a control request");
        };
        assert_eq!(request.request_id.as_deref(), Some("c1"));
        let body = request.request.expect("the request carries a body");
        assert_eq!(body.subtype.as_deref(), Some("can_use_tool"));
        assert_eq!(body.tool_name.as_deref(), Some("Bash"));
        assert_eq!(body.tool_use_id.as_deref(), Some("toolu_1"));
    }
}
