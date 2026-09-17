// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Turning the CLI's stream-json into [`Event`]s.
//!
//! [`Translator`] is fed one line at a time and answers with the events that
//! line produced. It holds the little state the protocol makes unavoidable —
//! which model the message in flight runs on, what a `tool_use` id was called,
//! how many tokens each model has been reported for — and nothing else, so the
//! same translator can be driven by a live subprocess or by a recorded log.
//!
//! # What the recording taught, and what the protocol costs
//!
//! Four properties of the stream are not obvious and are the reason this file
//! is not a `match` over message types:
//!
//! * **`assistant` messages repeat their `usage`.** The CLI splits one API
//!   response into several `assistant` lines — one per run of content blocks —
//!   and stamps the same `message.usage` on each. Summing them double-counts.
//! * **`assistant` `usage` is also a mid-stream snapshot.** Its
//!   `output_tokens` is whatever had been produced when that line was written,
//!   not what the message finished at. On a recorded two-turn session it read
//!   17 output tokens against the 305 the turn actually billed. The count that
//!   reconciles is the one on the `message_delta` stream event, which is why
//!   `--include-partial-messages` is not optional for this bridge and why
//!   usage is folded from there and nowhere else.
//! * **`result` totals are cumulative for the session, not for the turn.**
//!   `modelUsage` and `total_cost_usd` grow across turns, so they are read as
//!   running totals and reported as the difference from the last turn's.
//!   `result.usage`, alone among them, is the turn's own.
//! * **A refusal is reported twice.** `system`/`permission_denied` announces
//!   it between the call and the result it comes back as, and the closing
//!   `result` lists it again. Counting both doubles every denial; reading the
//!   first is also the only way to tell a call that was not allowed to run
//!   from a tool that broke, because the model is told about both as errors.
//!
//! # What is recognised and not yet translated
//!
//! * `rate_limit_event` carries how much of the five-hour and seven-day
//!   windows is gone. Nothing in the event model holds a usage window, and a
//!   shape invented here would be fixed before the status line that reads it
//!   exists. It is recognised so that it is not reported as unknown.
//! * `system`/`init` carries the CLI's version, its tool list and the state of
//!   each MCP server. [`SessionMeta`] carries the model and the CLI's own
//!   session id; the rest is not surfaced, because no pane reads it and
//!   widening the shared vocabulary for figures nothing draws would be a
//!   change nobody could see.
//! * A sub-agent's `parent_tool_use_id` says which `Task` call a message
//!   belongs to. Its tool calls and its tokens are folded in — they are work
//!   done and money spent — but attributing each line of the transcript to the
//!   agent that wrote it needs a pane that can show two agents at once.

use std::collections::BTreeMap;

use niobe_core::event::{
    AgentId, AgentOutcome, Backend, CostBasis, Event, PermissionDecision, SessionMeta, ToolCallId,
    ToolOutcome, Usage,
};

use crate::wire;

/// The tool whose call is a sub-agent rather than an action.
const TASK_TOOL: &str = "Task";

/// How far apart two figures for the same money may be before the difference
/// is reported, in USD.
///
/// The CLI sums its per-model costs in floating point and prints the sum
/// separately, so the two disagree in the last bits. Half a hundredth of a
/// cent is below anything a screen shows and far above that error.
const COST_TOLERANCE_USD: f64 = 0.000_05;

/// Token counts, summed the way both sides of a reconciliation sum them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

impl Counts {
    fn add(&mut self, other: Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
    }

    /// What `other` has that this does not. Saturating: a total that went
    /// backwards is read as nothing new rather than as a negative count.
    fn beyond(self, other: Self) -> Self {
        Self {
            input: other.input.saturating_sub(self.input),
            output: other.output.saturating_sub(self.output),
            cache_read: other.cache_read.saturating_sub(self.cache_read),
            cache_write: other.cache_write.saturating_sub(self.cache_write),
        }
    }

    fn is_empty(self) -> bool {
        self == Self::default()
    }
}

impl From<&wire::Usage> for Counts {
    fn from(usage: &wire::Usage) -> Self {
        Self {
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_read: usage.cache_read_input_tokens,
            cache_write: usage.cache_creation_input_tokens,
        }
    }
}

impl From<&wire::ModelUsage> for Counts {
    fn from(usage: &wire::ModelUsage) -> Self {
        Self {
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_read: usage.cache_read_input_tokens,
            cache_write: usage.cache_creation_input_tokens,
        }
    }
}

/// What has been reported for one model so far in this session.
#[derive(Debug, Clone, Copy, Default)]
struct Reported {
    tokens: Counts,
    cost_usd: f64,
}

/// Translates the CLI's stream-json into the shared event model.
#[derive(Debug)]
pub struct Translator {
    profile: String,
    model: Option<String>,
    backend_session: Option<String>,
    /// The model of the message in flight, per stream: the main session is
    /// `None` and each sub-agent is its `Task` call's id. `message_delta`
    /// carries the turn's authoritative usage and no model, so the model is
    /// remembered from the `message_start` that opened the same stream.
    in_flight: BTreeMap<Option<String>, String>,
    /// What each outstanding `tool_use` id was called and called with, so that
    /// the `tool_result` can repeat both without the consumer holding state.
    tool_calls: BTreeMap<String, (String, String)>,
    /// The `tool_use` ids that are sub-agents rather than actions.
    agents: BTreeMap<String, ()>,
    /// The `tool_use` ids the CLI refused and this bridge has already
    /// reported, so that the closing `result`'s list of the same refusals is
    /// not counted a second time.
    denied: BTreeMap<String, ()>,
    /// Per-message usage since the last `result`, for the turn reconciliation.
    turn: Counts,
    /// Per model, everything reported for it so far this session.
    reported: BTreeMap<String, Reported>,
}

impl Translator {
    /// A translator for a session running under `profile`.
    pub fn new(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
            model: None,
            backend_session: None,
            in_flight: BTreeMap::new(),
            tool_calls: BTreeMap::new(),
            agents: BTreeMap::new(),
            denied: BTreeMap::new(),
            turn: Counts::default(),
            reported: BTreeMap::new(),
        }
    }

    /// The id the CLI calls this session by, once it has said.
    pub fn backend_session(&self) -> Option<&str> {
        self.backend_session.as_deref()
    }

    /// The events one line of the CLI's standard output produced.
    ///
    /// A line that does not parse, and a line whose `type` this bridge does not
    /// know, become a warning entry: the CLI ships new shapes with new
    /// versions, and a bridge that dies on one takes the session with it.
    pub fn line(&mut self, line: &str) -> Vec<Event> {
        let mut out = Vec::new();
        match serde_json::from_str::<wire::Message>(line) {
            Ok(wire::Message::System(system)) => self.system(system, &mut out),
            Ok(wire::Message::Assistant(envelope)) => self.assistant(envelope, &mut out),
            Ok(wire::Message::User(envelope)) => self.user(envelope, &mut out),
            Ok(wire::Message::StreamEvent(event)) => self.stream(event, &mut out),
            Ok(wire::Message::ControlRequest(request)) => self.control(request, &mut out),
            Ok(wire::Message::Result(outcome)) => self.result(outcome, &mut out),
            // Recognised, and carried by nothing in the event model yet; see
            // the module documentation.
            Ok(wire::Message::ControlResponse(_) | wire::Message::RateLimitEvent(_)) => {}
            Ok(wire::Message::Unknown) => out.push(warn(format!(
                "the CLI sent a message of type `{}`, which this version of Niobe does not \
                 know how to read. It was not counted.",
                kind_of(line)
            ))),
            Err(error) => out.push(warn(format!(
                "the CLI sent a `{}` message this version of Niobe could not read, so it was \
                 not counted: {error}",
                kind_of(line)
            ))),
        }
        out
    }

    fn system(&mut self, system: wire::System, out: &mut Vec<Event>) {
        match system.subtype.as_deref() {
            Some("init") => {
                if let Some(id) = system.session_id {
                    self.backend_session = Some(id);
                }
                if let Some(model) = system.model {
                    self.set_model(model, out);
                }
            }
            Some("compact_boundary") => {
                let metadata = system.compact_metadata.unwrap_or(wire::CompactMetadata {
                    trigger: None,
                    pre_tokens: None,
                });
                let trigger = metadata.trigger.unwrap_or_else(|| "unstated".to_owned());
                out.push(Event::Notice {
                    message: match metadata.pre_tokens {
                        Some(tokens) => format!(
                            "the context was compacted ({trigger}); it held {tokens} tokens \
                             before. Everything after this point is priced against a shorter \
                             prompt."
                        ),
                        None => format!("the context was compacted ({trigger})."),
                    },
                });
            }
            Some("permission_denied") => self.denied(system, out),
            // The CLI's own request state — `requesting`, and whatever it adds
            // next. It says what the process is doing, not what the session is,
            // and the shell already shows that a turn is in flight.
            Some("status") => {}
            // A running guess at the thinking tokens of the message being
            // produced (`estimated_tokens`, and the step since the last one).
            // Deliberately not folded: it is an estimate, the measured count
            // arrives with the message, and it is already a share of the
            // output tokens the turn is billed for. Counting it would put a
            // guess into a total the whole product promises is measured.
            Some("thinking_tokens") => {}
            other => out.push(warn(format!(
                "the CLI sent a system message of subtype `{}`, which this version of Niobe \
                 does not know how to read.",
                other.unwrap_or("(none)")
            ))),
        }
    }

    /// Records a call the CLI refused, at the moment it refuses it.
    ///
    /// The refusal arrives between the `tool_use` that asked and the
    /// `tool_result` that carries the refusal back to the model, so the call
    /// can be shown as denied rather than as failed — which is the difference
    /// between a tool that broke and a tool that was not allowed to run. The
    /// closing `result` lists the same refusals again; [`Translator::result`]
    /// reads this to know which it has already reported.
    fn denied(&mut self, system: wire::System, out: &mut Vec<Event>) {
        let Some(id) = system.tool_use_id else { return };
        let tool = system
            .tool_name
            .or_else(|| self.tool_calls.get(&id).map(|(name, _)| name.clone()))
            .unwrap_or_default();
        let input = self
            .tool_calls
            .get(&id)
            .map(|(_, input)| input.clone())
            .unwrap_or_default();

        self.denied.insert(id.clone(), ());
        let id = ToolCallId::new(id);
        out.push(Event::PermissionRequest {
            id: id.clone(),
            tool,
            input,
        });
        out.push(Event::PermissionResponse {
            id,
            decision: PermissionDecision::Deny,
        });
    }

    /// Records the model the session is on, producing a fresh [`SessionMeta`]
    /// whenever it changes — which is how a routing decision reaches the
    /// status line.
    fn set_model(&mut self, model: String, out: &mut Vec<Event>) {
        if self.model.as_deref() == Some(model.as_str()) {
            return;
        }
        self.model = Some(model.clone());
        out.push(Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: self.profile.clone(),
            model,
            backend_session: self.backend_session.clone(),
        }));
    }

    fn assistant(&mut self, envelope: wire::Envelope, out: &mut Vec<Event>) {
        let blocks = match envelope.message.content {
            Some(wire::Content::Blocks(blocks)) => blocks,
            Some(wire::Content::Text(text)) => {
                out.push(Event::AssistantMessage { text });
                return;
            }
            None => return,
        };

        let mut text = String::new();
        let mut calls = Vec::new();
        for block in blocks {
            match block {
                wire::Block::Text { text: fragment } => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&fragment);
                }
                wire::Block::ToolUse { id, name, input } => {
                    let rendered = render(&input);
                    self.tool_calls
                        .insert(id.clone(), (name.clone(), rendered.clone()));
                    if name == TASK_TOOL {
                        self.agents.insert(id.clone(), ());
                        calls.push(Event::AgentSpawn {
                            id: AgentId::new(id.clone()),
                            // Which agent spawned this one is the sub-agent
                            // pane's question; the stream says only that the
                            // session did.
                            parent: None,
                            label: label_of(&input).unwrap_or_else(|| rendered.clone()),
                        });
                    }
                    calls.push(Event::ToolCallStart {
                        id: ToolCallId::new(id),
                        name,
                        input: rendered,
                    });
                }
                // A tool result never rides on an assistant message, and
                // thinking has nowhere to go in a transcript that shows what
                // the session did.
                wire::Block::ToolResult { .. } | wire::Block::Other => {}
            }
        }

        // The text first: it is what the model said before it acted, and the
        // transcript reads in that order.
        if !text.is_empty() {
            out.push(Event::AssistantMessage { text });
        }
        out.append(&mut calls);
    }

    fn user(&mut self, envelope: wire::Envelope, out: &mut Vec<Event>) {
        let Some(wire::Content::Blocks(blocks)) = envelope.message.content else {
            // A `user` message with plain text is the turn Niobe itself wrote,
            // echoed back. The shell already has it; folding it again would
            // show every prompt twice.
            return;
        };

        for block in blocks {
            let wire::Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = block
            else {
                continue;
            };

            let output = content.map(render_content).unwrap_or_default();
            let bytes = output.len() as u64;
            let (name, input) = match self.tool_calls.remove(&tool_use_id) {
                Some(call) => call,
                None => {
                    out.push(warn(format!(
                        "the CLI returned a result for tool call `{tool_use_id}`, which it never \
                         announced. The call is counted; what it was called with is lost."
                    )));
                    (String::new(), String::new())
                }
            };
            // A refusal reaches the model as an error, so the result alone
            // cannot tell a tool that broke from one that was not allowed to
            // run. The CLI says which in the `permission_denied` it sends
            // first, and that is what this reads.
            let outcome = match (
                is_error.unwrap_or(false),
                self.denied.contains_key(&tool_use_id),
            ) {
                (_, true) => ToolOutcome::Denied,
                (true, false) => ToolOutcome::Failed,
                (false, false) => ToolOutcome::Ok,
            };

            if self.agents.remove(&tool_use_id).is_some() {
                out.push(Event::AgentExit {
                    id: AgentId::new(tool_use_id.clone()),
                    outcome: match outcome {
                        ToolOutcome::Ok => AgentOutcome::Completed,
                        ToolOutcome::Failed | ToolOutcome::Denied => AgentOutcome::Failed,
                    },
                });
            }

            out.push(Event::ToolCallEnd {
                id: ToolCallId::new(tool_use_id),
                name,
                input,
                output,
                bytes,
                outcome,
            });
        }
    }

    fn stream(&mut self, event: wire::StreamEvent, out: &mut Vec<Event>) {
        let stream = event.parent_tool_use_id;
        match event.event {
            wire::StreamBody::MessageStart { message } => {
                let Some(model) = message.model else { return };
                if stream.is_none() {
                    self.set_model(model.clone(), out);
                }
                self.in_flight.insert(stream, model);
            }

            wire::StreamBody::MessageDelta { usage } => {
                let Some(usage) = usage else { return };
                let model = self
                    .in_flight
                    .remove(&stream)
                    .or_else(|| self.model.clone())
                    .unwrap_or_default();
                let counts = Counts::from(&usage);
                self.turn.add(counts);
                self.reported
                    .entry(model.clone())
                    .or_default()
                    .tokens
                    .add(counts);
                out.push(Event::Usage(Usage {
                    input: counts.input,
                    output: counts.output,
                    cache_read: counts.cache_read,
                    cache_write: counts.cache_write,
                    cache_write_1h: usage.cache_write_1h(),
                    // `output_tokens_details.thinking_tokens` is a share of
                    // `output_tokens`, not a count beside it. Reporting it as
                    // reasoning tokens would count those tokens twice, in the
                    // total and again in the bill.
                    reasoning: 0,
                    model,
                    // The CLI reports no money per message; the turn's
                    // `result` does, for the session so far.
                    cost_usd: None,
                    cost_basis: None,
                }));
            }

            // A sub-agent's text is folded in when the message is complete
            // rather than as it streams: two agents streaming into one
            // transcript would write through each other's paragraphs, and
            // telling them apart on screen needs a pane that shows both.
            wire::StreamBody::ContentBlockDelta {
                delta: wire::Delta::TextDelta { text },
            } if stream.is_none() => out.push(Event::AssistantDelta { text }),

            wire::StreamBody::ContentBlockDelta { .. } | wire::StreamBody::Other => {}
        }
    }

    fn control(&mut self, request: wire::ControlRequest, out: &mut Vec<Event>) {
        let Some(body) = request.request else { return };
        if body.subtype.as_deref() != Some("can_use_tool") {
            return;
        }
        let id = body.tool_use_id.unwrap_or_default();
        out.push(Event::PermissionRequest {
            id: ToolCallId::new(id),
            tool: body.tool_name.unwrap_or_default(),
            input: body.input.as_ref().map(render).unwrap_or_default(),
        });
    }

    fn result(&mut self, outcome: wire::Outcome, out: &mut Vec<Event>) {
        self.reconcile_turn(outcome.usage.as_ref(), out);
        self.report_cost(&outcome, out);

        // The closing line lists every call the turn refused, including the
        // ones already reported as they happened. Anything left is a refusal
        // the CLI did not announce — recorded here, late, rather than lost.
        for denial in outcome.permission_denials {
            let id = denial.tool_use_id.unwrap_or_default();
            if self.denied.insert(id.clone(), ()).is_some() {
                continue;
            }
            let id = ToolCallId::new(id);
            out.push(Event::PermissionRequest {
                id: id.clone(),
                tool: denial.tool_name.unwrap_or_default(),
                input: denial.tool_input.as_ref().map(render).unwrap_or_default(),
            });
            out.push(Event::PermissionResponse {
                id,
                decision: PermissionDecision::Deny,
            });
        }

        if outcome.is_error || outcome.subtype.as_deref() != Some("success") {
            out.push(Event::Error {
                message: format!(
                    "the turn ended as `{}`: {}",
                    outcome.subtype.as_deref().unwrap_or("(no subtype)"),
                    outcome
                        .result
                        .as_deref()
                        .unwrap_or("the CLI gave no reason")
                ),
                // The subprocess is still there with its stdin open, so the
                // session goes on; it is this turn that failed.
                fatal: false,
            });
        }
    }

    /// Checks the turn's own total against what the per-message records added
    /// up to, and says so when they differ.
    ///
    /// A silent correction here would be the product gone: the per-message
    /// records are what every pane and every price is derived from, so a
    /// difference between them and the CLI's own figure is something the
    /// operator has to be able to see.
    fn reconcile_turn(&mut self, usage: Option<&wire::Usage>, out: &mut Vec<Event>) {
        let summed = std::mem::take(&mut self.turn);
        let Some(reported) = usage.map(Counts::from) else {
            return;
        };
        if reported != summed {
            out.push(warn(format!(
                "the turn's per-message tokens do not add up to what the CLI reported for the \
                 turn. Per message: {} in, {} out, {} cache read, {} cache write. Reported: \
                 {} in, {} out, {} cache read, {} cache write.",
                summed.input,
                summed.output,
                summed.cache_read,
                summed.cache_write,
                reported.input,
                reported.output,
                reported.cache_read,
                reported.cache_write,
            )));
        }
    }

    /// Reports what the session has cost since the last turn.
    ///
    /// `modelUsage` is the session's running total per model, so each turn
    /// reports the difference. The tokens it carries are almost always already
    /// in the per-message records — the difference is what the CLI spent on
    /// models that never produced a message of their own, such as the small
    /// model it summarises with — and those are reported here or nowhere.
    fn report_cost(&mut self, outcome: &wire::Outcome, out: &mut Vec<Event>) {
        if outcome.model_usage.is_empty() {
            self.report_session_cost(outcome, out);
            return;
        }

        let mut costs = 0.0;
        for (model, usage) in &outcome.model_usage {
            if let Some(basis) = &usage.cost_basis
                && basis != "list"
            {
                out.push(warn(format!(
                    "the CLI priced {model} on a basis this version of Niobe does not know \
                     (`{basis}`). The figure is still labelled API-equivalent, never measured."
                )));
            }
            let cost_now = usage.cost_usd.unwrap_or_default();
            costs += cost_now;

            let seen = self.reported.entry(model.clone()).or_default();
            let tokens = seen.tokens.beyond(Counts::from(usage));
            let spent = cost_now - seen.cost_usd;
            if tokens.is_empty() && spent <= 0.0 {
                continue;
            }
            seen.tokens.add(tokens);
            seen.cost_usd = cost_now;
            out.push(self.cost_record(model.clone(), tokens, spent));
        }

        if let Some(total) = outcome.total_cost_usd
            && (total - costs).abs() > COST_TOLERANCE_USD
        {
            out.push(warn(format!(
                "the CLI's per-model costs add up to ${costs:.6}, and it reported \
                 ${total:.6} for the session. The per-model figures are what was counted."
            )));
        }
    }

    /// The fallback for a `result` that priced the session without saying how
    /// it split by model: one record, against the model the session is on.
    fn report_session_cost(&mut self, outcome: &wire::Outcome, out: &mut Vec<Event>) {
        let Some(total) = outcome.total_cost_usd else {
            return;
        };
        let model = self.model.clone().unwrap_or_default();
        let seen = self.reported.entry(model.clone()).or_default();
        let spent = total - seen.cost_usd;
        if spent <= 0.0 {
            return;
        }
        seen.cost_usd = total;
        out.push(self.cost_record(model, Counts::default(), spent));
    }

    /// A usage record carrying what a turn cost, and whatever tokens the
    /// per-message records had not already accounted for.
    ///
    /// Always API-equivalent, never measured: the CLI computes every figure it
    /// prints from published list prices, whatever login is behind it. Money
    /// that moved is on the provider's invoice, not in this stream, and
    /// labelling a list price as a measurement is the one thing this product
    /// cannot do.
    fn cost_record(&self, model: String, tokens: Counts, spent: f64) -> Event {
        Event::Usage(Usage {
            input: tokens.input,
            output: tokens.output,
            cache_read: tokens.cache_read,
            cache_write: tokens.cache_write,
            // `modelUsage` reports no lifetime split for its cache writes.
            cache_write_1h: 0,
            reasoning: 0,
            model,
            // A difference of zero is not a report that the turn was free.
            cost_usd: (spent > 0.0).then_some(spent),
            cost_basis: (spent > 0.0).then_some(CostBasis::ApiEquivalent),
        })
    }
}

/// A non-fatal entry: something the session should show and go on from.
fn warn(message: String) -> Event {
    Event::Error {
        message,
        fatal: false,
    }
}

/// The `type` of a line, for a message that could not be read as one. Read
/// separately and only on this path, so that the common case parses once.
fn kind_of(line: &str) -> String {
    serde_json::from_str::<wire::Tag>(line)
        .ok()
        .and_then(|tag| tag.kind)
        .unwrap_or_else(|| "(untyped)".to_owned())
}

/// A tool call's arguments, on one line, as they are shown and attributed.
///
/// A string argument is shown as itself rather than as a quoted JSON string:
/// the common call is a command or a path, and `"ls -la"` reads worse than
/// `ls -la` for no gain.
fn render(input: &serde_json::Value) -> String {
    match input {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The text of a tool result, which the CLI writes either as a string or as
/// the content blocks a tool returned.
fn render_content(content: wire::Content) -> String {
    match content {
        wire::Content::Text(text) => text,
        wire::Content::Blocks(blocks) => blocks
            .into_iter()
            .filter_map(|block| match block {
                wire::Block::Text { text } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// What a `Task` call was spawned to do, as the parallel pane will name it.
fn label_of(input: &serde_json::Value) -> Option<String> {
    input
        .get("description")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translator() -> Translator {
        let mut translator = Translator::new("max");
        translator
            .line(r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5"}"#);
        translator
    }

    /// One `message_delta`, which is where the counts that reconcile live.
    fn delta(input: u64, output: u64) -> String {
        format!(
            r#"{{"type":"stream_event","event":{{"type":"message_delta","usage":{{"input_tokens":{input},"output_tokens":{output}}}}}}}"#
        )
    }

    fn warnings(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Error { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_turn_whose_messages_do_not_add_up_is_reported_rather_than_corrected() {
        let mut translator = translator();
        translator.line(&delta(2, 10));

        let events = translator.line(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":2,"output_tokens":99}}"#,
        );

        let said = warnings(&events);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("do not add up"), "{said:?}");
        assert!(said[0].contains("10 out"), "the summed figure: {said:?}");
        assert!(said[0].contains("99 out"), "the reported figure: {said:?}");
        assert!(
            events.iter().all(|event| !matches!(event, Event::Usage(_))),
            "a mismatch invented a record to close the gap"
        );
    }

    #[test]
    fn the_turns_tally_starts_again_at_every_result() {
        let mut translator = translator();
        translator.line(&delta(2, 10));
        translator.line(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":2,"output_tokens":10}}"#,
        );

        translator.line(&delta(3, 20));
        let events = translator.line(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":3,"output_tokens":20}}"#,
        );

        assert!(
            warnings(&events).is_empty(),
            "the second turn was measured against the first"
        );
    }

    #[test]
    fn a_model_the_session_moves_to_is_announced_once() {
        let mut translator = translator();

        let moved = translator.line(
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"sonnet-5"}}}"#,
        );
        let again = translator.line(
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"sonnet-5"}}}"#,
        );

        assert!(matches!(
            moved.as_slice(),
            [Event::SessionMeta(meta)] if meta.model == "sonnet-5"
        ));
        assert!(
            again.is_empty(),
            "the same model was announced twice: {again:?}"
        );
    }

    #[test]
    fn a_sub_agents_model_does_not_become_the_sessions() {
        let mut translator = translator();

        let events = translator.line(
            r#"{"type":"stream_event","parent_tool_use_id":"toolu_task","event":{"type":"message_start","message":{"model":"haiku-4-5"}}}"#,
        );

        assert!(events.is_empty(), "{events:?}");
        assert_eq!(translator.model.as_deref(), Some("opus-5"));
    }

    #[test]
    fn a_sub_agents_tokens_are_billed_to_the_model_that_produced_them() {
        let mut translator = translator();
        translator.line(
            r#"{"type":"stream_event","parent_tool_use_id":"toolu_task","event":{"type":"message_start","message":{"model":"haiku-4-5"}}}"#,
        );

        let events = translator.line(
            r#"{"type":"stream_event","parent_tool_use_id":"toolu_task","event":{"type":"message_delta","usage":{"input_tokens":5,"output_tokens":7}}}"#,
        );

        let [Event::Usage(usage)] = events.as_slice() else {
            panic!("one usage record: {events:?}");
        };
        assert_eq!(usage.model, "haiku-4-5");
        assert_eq!(usage.output, 7);
    }

    #[test]
    fn a_pricing_basis_the_bridge_does_not_know_is_reported_and_still_not_called_measured() {
        let mut translator = translator();

        let events = translator.line(
            r#"{"type":"result","subtype":"success","modelUsage":{"opus-5":{"costUSD":0.5,"costBasis":"billed"}},"total_cost_usd":0.5}"#,
        );

        let said = warnings(&events);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("`billed`"), "{said:?}");
        let [_, Event::Usage(usage)] = events.as_slice() else {
            panic!("a warning and a cost record: {events:?}");
        };
        assert_eq!(usage.cost_basis, Some(CostBasis::ApiEquivalent));
    }

    #[test]
    fn a_session_total_that_disagrees_with_its_parts_is_reported() {
        let mut translator = translator();

        let events = translator.line(
            r#"{"type":"result","subtype":"success","modelUsage":{"opus-5":{"costUSD":0.5,"costBasis":"list"}},"total_cost_usd":0.9}"#,
        );

        let said = warnings(&events);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("add up to"), "{said:?}");
    }

    #[test]
    fn a_result_that_priced_the_session_without_splitting_it_by_model_still_reports_the_cost() {
        let mut translator = translator();

        let first =
            translator.line(r#"{"type":"result","subtype":"success","total_cost_usd":0.25}"#);
        let second =
            translator.line(r#"{"type":"result","subtype":"success","total_cost_usd":0.40}"#);

        let [Event::Usage(first)] = first.as_slice() else {
            panic!("one cost record: {first:?}");
        };
        let [Event::Usage(second)] = second.as_slice() else {
            panic!("one cost record: {second:?}");
        };
        assert_eq!(first.model, "opus-5");
        assert!((first.cost_usd.unwrap_or_default() - 0.25).abs() < 1e-9);
        assert!(
            (second.cost_usd.unwrap_or_default() - 0.15).abs() < 1e-9,
            "the running total was reported again instead of what the turn added"
        );
    }

    #[test]
    fn a_turn_that_failed_does_not_end_the_session() {
        let mut translator = translator();

        let events = translator.line(
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"the provider refused"}"#,
        );

        let [Event::Error { message, fatal }] = events.as_slice() else {
            panic!("one error: {events:?}");
        };
        assert!(!fatal, "a failed turn ended the session");
        assert!(message.contains("error_during_execution"), "{message}");
        assert!(message.contains("the provider refused"), "{message}");
    }

    #[test]
    fn a_result_for_a_call_that_was_never_announced_is_counted_and_reported() {
        let mut translator = translator();

        let events = translator.line(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_gone","content":"four"}]}}"#,
        );

        let [
            Event::Error { message, .. },
            Event::ToolCallEnd { id, bytes, .. },
        ] = events.as_slice()
        else {
            panic!("a warning and the call it could not name: {events:?}");
        };
        assert!(message.contains("toolu_gone"), "{message}");
        assert_eq!(id.as_str(), "toolu_gone");
        assert_eq!(*bytes, 4, "the call is still accounted for");
    }

    #[test]
    fn a_turn_niobe_wrote_is_not_folded_in_again_when_the_cli_echoes_it() {
        let mut translator = translator();

        let events = translator
            .line(r#"{"type":"user","message":{"role":"user","content":"do the thing"}}"#);

        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn a_system_subtype_the_bridge_does_not_know_is_a_warning_and_not_a_crash() {
        let mut translator = translator();

        let events = translator.line(r#"{"type":"system","subtype":"weather","outlook":"fine"}"#);

        let said = warnings(&events);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("`weather`"), "{said:?}");
    }

    #[test]
    fn a_running_guess_at_the_thinking_tokens_is_recognised_and_never_counted() {
        let mut translator = translator();

        let events = translator.line(
            r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":136,"estimated_tokens_delta":86}"#,
        );

        // `estimated_tokens` is a guess, and the measured count arrives with
        // the message as a share of its output tokens. Folding it would put a
        // guess into a total that is meant to be measured, and count those
        // tokens twice besides.
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn a_refusal_is_recorded_when_it_happens_and_not_again_at_the_end_of_the_turn() {
        let mut translator = translator();
        translator.line(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"rm -rf build"}}]}}"#,
        );

        let announced = translator.line(
            r#"{"type":"system","subtype":"permission_denied","tool_name":"Bash","tool_use_id":"toolu_1","message":"This command requires approval"}"#,
        );
        let result = translator.line(
            r#"{"type":"result","subtype":"success","permission_denials":[{"tool_name":"Bash","tool_use_id":"toolu_1"}]}"#,
        );

        let [
            Event::PermissionRequest { id, tool, input },
            Event::PermissionResponse { decision, .. },
        ] = announced.as_slice()
        else {
            panic!("the refusal and its answer: {announced:?}");
        };
        assert_eq!(id.as_str(), "toolu_1");
        assert_eq!(tool, "Bash");
        assert_eq!(
            input, r#"{"command":"rm -rf build"}"#,
            "the call it refused"
        );
        assert_eq!(*decision, PermissionDecision::Deny);
        assert!(
            result.is_empty(),
            "the closing result reported the same refusal again: {result:?}"
        );
    }

    #[test]
    fn a_call_that_was_refused_is_denied_rather_than_failed() {
        let mut translator = translator();
        translator.line(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"rm -rf build"}}]}}"#,
        );
        translator.line(
            r#"{"type":"system","subtype":"permission_denied","tool_name":"Bash","tool_use_id":"toolu_1"}"#,
        );

        // The refusal reaches the model as an error, so the result alone
        // cannot tell a tool that broke from one that was not allowed to run.
        let events = translator.line(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":true,"content":"This command requires approval"}]}}"#,
        );

        let [Event::ToolCallEnd { outcome, .. }] = events.as_slice() else {
            panic!("the call ending: {events:?}");
        };
        assert_eq!(*outcome, ToolOutcome::Denied);
    }

    #[test]
    fn a_refusal_the_cli_only_lists_at_the_end_is_still_recorded() {
        let mut translator = translator();

        let events = translator.line(
            r#"{"type":"result","subtype":"success","permission_denials":[{"tool_name":"Write","tool_use_id":"toolu_9","tool_input":{"file_path":"/etc/hosts"}}]}"#,
        );

        let [
            Event::PermissionRequest { tool, input, .. },
            Event::PermissionResponse { .. },
        ] = events.as_slice()
        else {
            panic!("the refusal and its answer: {events:?}");
        };
        assert_eq!(tool, "Write");
        assert_eq!(input, r#"{"file_path":"/etc/hosts"}"#);
    }

    #[test]
    fn a_usage_window_is_recognised_rather_than_reported_as_unknown() {
        let mut translator = translator();

        let events = translator
            .line(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#);

        assert!(events.is_empty(), "{events:?}");
    }
}
