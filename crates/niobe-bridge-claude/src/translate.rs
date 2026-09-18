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
//!   windows is gone, which is the budget on a flat-rate plan; it becomes
//!   [`Event::UsageWindows`]. Its `status`, `rateLimitType` and `resetsAt`
//!   restate whichever window the CLI is closest to and are not read.
//! * `system`/`init` carries the CLI's version, its tool list and the state of
//!   each MCP server. [`SessionMeta`] carries the model and the CLI's own
//!   session id, and the mode it reports becomes [`Event::ModeSelected`]; the
//!   rest is not surfaced, because no pane reads it and widening the shared
//!   vocabulary for figures nothing draws would be a change nobody could see.
//! * A sub-agent's `parent_tool_use_id` says which `Task` call a message
//!   belongs to. Its tool calls and its tokens are folded in — they are work
//!   done and money spent — but attributing each line of the transcript to the
//!   agent that wrote it needs a pane that can show two agents at once.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use niobe_core::diff;
use niobe_core::event::{
    AgentId, AgentOutcome, Backend, CostBasis, Event, Mode, PermissionDecision, SessionMeta,
    ToolCallId, ToolOutcome, Usage, UsageWindow, UsageWindows,
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

/// What a `tool_use` id was called, called with, and acts on.
///
/// Kept whole so that the `tool_result` can repeat it without the consumer
/// holding state, and so that a refusal the CLI announces after the call can
/// be reported with the same arguments the call was made with.
#[derive(Debug, Clone)]
struct Call {
    name: String,
    input: String,
    target: Option<String>,
    /// The arguments as the CLI sent them. The rendered `input` is for a
    /// human; counting what an edit changed needs the fields themselves.
    arguments: serde_json::Value,
}

/// A permission prompt the CLI is waiting on an answer to.
///
/// The tool call is what the shell shows and answers about; `request_id` is
/// what the CLI addresses the answer by, and `input` is echoed back with an
/// approval because the protocol carries the approved arguments rather than a
/// bare yes. Niobe approves a call, it never rewrites one, so what goes back
/// is exactly what came.
#[derive(Debug, Clone)]
pub struct Asked {
    /// The call the CLI is asking about.
    pub id: ToolCallId,
    /// The id an answer is addressed to.
    pub request_id: String,
    /// The arguments the call would run with, as the CLI sent them.
    pub input: serde_json::Value,
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
    /// Where the session runs, so that a file it changed is named the way the
    /// operator and `git` name it rather than by its absolute path. `None`
    /// where the caller did not say, and a path is then left as the CLI gave
    /// it: a prefix guessed here would shorten a path to something that is not
    /// the file.
    cwd: Option<PathBuf>,
    model: Option<String>,
    backend_session: Option<String>,
    /// The model of the message in flight, per stream: the main session is
    /// `None` and each sub-agent is its `Task` call's id. `message_delta`
    /// carries the turn's authoritative usage and no model, so the model is
    /// remembered from the `message_start` that opened the same stream.
    in_flight: BTreeMap<Option<String>, String>,
    /// What each outstanding `tool_use` id was called and called with, so that
    /// the `tool_result` can repeat both without the consumer holding state.
    tool_calls: BTreeMap<String, Call>,
    /// The permission prompts read since the last drain, for whatever owns the
    /// CLI's standard input to answer.
    asked: Vec<Asked>,
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
            cwd: None,
            model: None,
            backend_session: None,
            in_flight: BTreeMap::new(),
            tool_calls: BTreeMap::new(),
            asked: Vec::new(),
            agents: BTreeMap::new(),
            denied: BTreeMap::new(),
            turn: Counts::default(),
            reported: BTreeMap::new(),
        }
    }

    /// The same translator, told where the session runs, so that the files it
    /// reports changed are named relative to it.
    #[must_use]
    pub fn in_dir(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// The same translator, told what the CLI calls the session before it has
    /// said so itself.
    ///
    /// A live session learns the id from the `init` message the CLI opens
    /// with; a transcript read off disk is named by the file it is in, and
    /// there is no `init` in it.
    #[must_use]
    pub fn of_session(mut self, id: impl Into<String>) -> Self {
        self.backend_session = Some(id.into());
        self
    }

    /// The id the CLI calls this session by, once it has said.
    pub fn backend_session(&self) -> Option<&str> {
        self.backend_session.as_deref()
    }

    /// Records that a call was refused here, over the control channel.
    ///
    /// The CLI feeds a refusal back to the model as a tool result with an
    /// error on it, which is indistinguishable from a tool that broke, and it
    /// lists the refusal again only in the turn's closing `result` — by which
    /// time the call has already been shown as failed. Told at the moment the
    /// answer goes out, the translator reads that result as the denial it is,
    /// and does not report the closing list as a second refusal.
    pub fn refused(&mut self, id: &ToolCallId) {
        self.denied.insert(id.as_str().to_owned(), ());
    }

    /// The permission prompts read since the last call, oldest first.
    ///
    /// Separate from the events because an answer is addressed by an id the
    /// event model has no room for and no use for: the shell answers about a
    /// tool call, and only whatever holds the CLI's standard input needs to
    /// know what the CLI calls the question.
    pub fn take_asked(&mut self) -> Vec<Asked> {
        std::mem::take(&mut self.asked)
    }

    /// The events one line of the CLI's standard output produced.
    ///
    /// A line that does not parse, and a line whose `type` this bridge does not
    /// know, become a warning entry: the CLI ships new shapes with new
    /// versions, and a bridge that dies on one takes the session with it.
    pub fn line(&mut self, line: &str) -> Vec<Event> {
        match serde_json::from_str::<wire::Message>(line) {
            Ok(wire::Message::Unknown) => vec![warn(format!(
                "the CLI sent a message of type `{}`, which this version of Niobe does not \
                 know how to read. It was not counted.",
                kind_of(line)
            ))],
            Ok(message) => self.message(message),
            Err(error) => vec![warn(format!(
                "the CLI sent a `{}` message this version of Niobe could not read, so it was \
                 not counted: {error}",
                kind_of(line)
            ))],
        }
    }

    /// The events one of the CLI's messages produced.
    ///
    /// Split from [`Translator::line`] because a message reaches Niobe two
    /// ways: off the live stream, a line at a time, and out of the transcript
    /// the CLI keeps for itself, which carries the same messages in envelopes
    /// of its own. Both fold through here, so the two cannot drift.
    pub(crate) fn message(&mut self, message: wire::Message) -> Vec<Event> {
        let mut out = Vec::new();
        match message {
            wire::Message::System(system) => self.system(system, &mut out),
            wire::Message::Assistant(envelope) => self.assistant(envelope, &mut out),
            wire::Message::User(envelope) => self.user(envelope, &mut out),
            wire::Message::StreamEvent(event) => self.stream(event, &mut out),
            wire::Message::ControlRequest(request) => self.control(request, &mut out),
            wire::Message::Result(outcome) => self.result(outcome, &mut out),
            wire::Message::ControlResponse(response) => self.answered(response, &mut out),
            wire::Message::RateLimitEvent(event) => rate_limit(event, &mut out),
            // Only the line a message arrived on says what type it was, so
            // whoever read that line is the one that can report it.
            wire::Message::Unknown => {}
        }
        out
    }

    /// Reports a request of Niobe's own that the CLI refused.
    ///
    /// Only this side asks the CLI anything, so every `control_response` is an
    /// answer to a request made here — a mode or a model the session was asked
    /// to move to. A refusal left unreported would leave the status line
    /// showing a change that never happened.
    fn answered(&mut self, response: wire::ControlResponse, out: &mut Vec<Event>) {
        let Some(outcome) = response.response else {
            return;
        };
        if outcome.subtype.as_deref() == Some("success") {
            return;
        }
        out.push(warn(format!(
            "the CLI refused a change Niobe asked for, so the session is running as it was: {}",
            outcome.error.as_deref().unwrap_or("the CLI gave no reason")
        )));
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
                if let Some(mode) = system.permission_mode {
                    match read_mode(&mode) {
                        Some(mode) => out.push(Event::ModeSelected { mode }),
                        // The CLI gates calls in ways Niobe has no word for —
                        // `acceptEdits`, `bypassPermissions`, `dontAsk` — and
                        // a profile can ask for one through its own arguments.
                        // Naming it beats showing a mode the session is not in.
                        None => out.push(warn(format!(
                            "the CLI is gating tool calls as `{mode}`, which this version of \
                             Niobe does not model. The status line shows no mode until the \
                             session is moved to one it does."
                        ))),
                    }
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
        // Already reported: the operator refused it here, or the CLI has said
        // so once. A second report would count the refusal twice.
        if self.denied.contains_key(&id) {
            return;
        }
        let call = self.tool_calls.get(&id);
        let tool = system
            .tool_name
            .or_else(|| call.map(|call| call.name.clone()))
            .unwrap_or_default();
        let input = call.map(|call| call.input.clone()).unwrap_or_default();
        let target = call.and_then(|call| call.target.clone());

        self.denied.insert(id.clone(), ());
        let id = ToolCallId::new(id);
        out.push(Event::PermissionRequest {
            id: id.clone(),
            tool,
            input,
            target,
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
                    self.tool_calls.insert(
                        id.clone(),
                        Call {
                            name: name.clone(),
                            input: rendered.clone(),
                            target: target_of(&input),
                            arguments: input.clone(),
                        },
                    );
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
            let (name, input, arguments) = match self.tool_calls.remove(&tool_use_id) {
                Some(call) => (call.name, call.input, call.arguments),
                None => {
                    out.push(warn(format!(
                        "the CLI returned a result for tool call `{tool_use_id}`, which it never \
                         announced. The call is counted; what it was called with is lost."
                    )));
                    (String::new(), String::new(), serde_json::Value::Null)
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

            // Only a call that ran changed anything: a refused or broken edit
            // would otherwise put a file in the change set that is not in the
            // diff.
            let change = match outcome {
                ToolOutcome::Ok => self.file_change(&name, &arguments, &output),
                ToolOutcome::Failed | ToolOutcome::Denied => None,
            };

            out.push(Event::ToolCallEnd {
                id: ToolCallId::new(tool_use_id),
                name,
                input,
                output,
                bytes,
                outcome,
            });
            out.extend(change);
        }
    }

    /// What a finished call did to a file, where it was a call that edits one.
    ///
    /// The counts come from the call's own arguments and from nothing else.
    /// Two cases the arguments do not settle are reported as unstated rather
    /// than filled in:
    ///
    /// * **A replacement the CLI applied everywhere.** `replace_all` says the
    ///   CLI matched `old_string` as many times as it appears in the file, and
    ///   the file is not in the stream. Counting one occurrence would under-
    ///   report every further one: measured on this machine, a `replace_all`
    ///   of three occurrences shows as `3 3` in `git diff --numstat` while the
    ///   call describes one.
    /// * **A `Write` over a file that already existed.** The call carries what
    ///   the file becomes and never what it was, so the lines it dropped are
    ///   not in the stream at all.
    fn file_change(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        output: &str,
    ) -> Option<Event> {
        let (path, added, removed) = match name {
            EDIT_TOOL => {
                let path = string_at(arguments, "file_path")?;
                let counts = match arguments
                    .get("replace_all")
                    .and_then(serde_json::Value::as_bool)
                {
                    Some(true) => None,
                    Some(false) | None => diff::lines_changed(
                        string_at(arguments, "old_string").unwrap_or_default(),
                        string_at(arguments, "new_string").unwrap_or_default(),
                    ),
                };
                match counts {
                    Some((added, removed)) => (path, Some(added), Some(removed)),
                    None => (path, None, None),
                }
            }

            WRITE_TOOL => {
                let path = string_at(arguments, "file_path")?;
                let written = string_at(arguments, "content").unwrap_or_default();
                let added = written.lines().count() as u64;
                // The CLI says which of the two it did, and its answer is read
                // narrowly on purpose: a wording it no longer uses leaves the
                // removal unstated, which is a figure the pane marks, where
                // guessing "nothing was there" would be a zero that is wrong.
                let removed = output.starts_with(CREATED_PREFIX).then_some(0);
                (path, Some(added), removed)
            }

            // A notebook is edited by cell, so the call says which cell and
            // never how many lines. That the file changed is still worth
            // showing; how much it changed, this call cannot say.
            NOTEBOOK_TOOL => (string_at(arguments, "notebook_path")?, None, None),

            _ => return None,
        };

        Some(Event::FileChange {
            path: self.relative(path),
            added,
            removed,
        })
    }

    /// A path as the operator reads it: relative to where the session runs,
    /// which is how `git` names the same file, and left as the CLI gave it
    /// where it is somewhere else entirely.
    fn relative(&self, path: &str) -> String {
        let Some(cwd) = self.cwd.as_deref() else {
            return path.to_owned();
        };
        Path::new(path)
            .strip_prefix(cwd)
            .ok()
            .and_then(Path::to_str)
            .unwrap_or(path)
            .to_owned()
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

    /// Records a prompt the CLI is waiting on, and asks the session it.
    ///
    /// A request with no `request_id` is reported as a prompt and never
    /// queued to be answered: there is nothing to address an answer to, and
    /// queueing it would leave the shell waiting on a modal whose reply goes
    /// nowhere. A `can_use_tool` is the only subtype this bridge answers;
    /// anything else the CLI asks is left alone rather than guessed at, and
    /// the CLI falls back to its own handling.
    fn control(&mut self, request: wire::ControlRequest, out: &mut Vec<Event>) {
        let Some(body) = request.request else { return };
        if body.subtype.as_deref() != Some("can_use_tool") {
            return;
        }
        let id = ToolCallId::new(body.tool_use_id.unwrap_or_default());
        if let Some(request_id) = request.request_id {
            self.asked.push(Asked {
                id: id.clone(),
                request_id,
                input: body.input.clone().unwrap_or(serde_json::Value::Null),
            });
        }
        out.push(Event::PermissionRequest {
            id,
            tool: body.tool_name.unwrap_or_default(),
            input: body.input.as_ref().map(render).unwrap_or_default(),
            target: body.input.as_ref().and_then(target_of),
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
                target: denial.tool_input.as_ref().and_then(target_of),
            });
            out.push(Event::PermissionResponse {
                id,
                decision: PermissionDecision::Deny,
            });
        }

        if outcome.is_error || outcome.subtype.as_deref() != Some("success") {
            // `result` carries the reason for most failures and `errors` for
            // the rest — a budget the CLI stopped on says why only there — so
            // both are reported rather than whichever one was looked at first.
            let mut said: Vec<String> = outcome.result.into_iter().collect();
            said.extend(outcome.errors);
            out.push(Event::Error {
                message: format!(
                    "the turn ended as `{}`: {}",
                    outcome.subtype.as_deref().unwrap_or("(no subtype)"),
                    match said.is_empty() {
                        true => "the CLI gave no reason".to_owned(),
                        false => said.join(" · "),
                    }
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
    ///
    /// A turn the CLI gave no total for is not a disagreement. It closes a
    /// turn it stopped on a budget with an all-zero `usage` block, which says
    /// the same as leaving it out: there is nothing on the other side to
    /// compare the per-message records against, and reporting that as a
    /// mismatch would put a warning in front of the operator on every budget
    /// stop for numbers nobody contradicted.
    fn reconcile_turn(&mut self, usage: Option<&wire::Usage>, out: &mut Vec<Event>) {
        let summed = std::mem::take(&mut self.turn);
        let Some(reported) = usage.map(Counts::from) else {
            return;
        };
        if reported.is_empty() {
            return;
        }
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

/// The mode the CLI reports, as one of the three Niobe models.
///
/// The CLI's `default` and `manual` both mean "stop and ask about anything a
/// standing rule does not already allow", which is Niobe's `ask`. The rest —
/// `acceptEdits`, `bypassPermissions`, `dontAsk` — are modes the shell has no
/// word for, and a wrong word for one is worse than none.
pub(crate) fn read_mode(mode: &str) -> Option<Mode> {
    match mode {
        "plan" => Some(Mode::Plan),
        "default" | "manual" => Some(Mode::Ask),
        "auto" => Some(Mode::Auto),
        _ => None,
    }
}

/// A non-fatal entry: something the session should show and go on from.
pub(crate) fn warn(message: String) -> Event {
    Event::Error {
        message,
        fatal: false,
    }
}

/// The `type` of a line, for a message that could not be read as one. Read
/// separately and only on this path, so that the common case parses once.
pub(crate) fn kind_of(line: &str) -> String {
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

/// The CLI's tools that change a file, as its `init` message lists them.
///
/// Read off the tool list of Claude Code 2.1.277 on 18 September 2026: `Edit`,
/// `Write` and `NotebookEdit` are all of them. A tool that is not here changes
/// no file as far as the changes pane is concerned — a `Bash` call that runs
/// `sed` does change one, and nothing in the stream says which or by how much,
/// so nothing is claimed about it.
const EDIT_TOOL: &str = "Edit";
const WRITE_TOOL: &str = "Write";
const NOTEBOOK_TOOL: &str = "NotebookEdit";

/// How the CLI opens the result of a `Write` that made a file that was not
/// there, as against one that replaced a file that was.
const CREATED_PREFIX: &str = "File created successfully at:";

/// A string argument, where the arguments are an object that carries it.
fn string_at<'a>(arguments: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(serde_json::Value::as_str)
}

/// Keys a tool's arguments name the thing it acts on by, in the order they
/// are looked for.
///
/// The list is the vendor's, read off the tools the CLI ships: a shell command
/// runs `command`, the file tools take `file_path` or `notebook_path`, the
/// search tools a `pattern` and a `path`, the web tools a `url`. Where none of
/// them is there the call has no target, and a standing answer about it can
/// only be about the whole tool — which is the honest answer, because a target
/// invented here would be one the operator never approved.
const TARGET_KEYS: [&str; 6] = [
    "command",
    "file_path",
    "notebook_path",
    "path",
    "url",
    "pattern",
];

/// The one thing a call acts on, where its arguments name it.
fn target_of(input: &serde_json::Value) -> Option<String> {
    if let serde_json::Value::String(text) = input {
        return Some(text.clone());
    }
    TARGET_KEYS
        .iter()
        .find_map(|key| input.get(key).and_then(serde_json::Value::as_str))
        .map(str::to_owned)
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

/// The windows one `rate_limit_event` reported.
///
/// A free function rather than a method: the message says what is left of the
/// plan, which is a level the CLI measured and not something the translator
/// accumulates, so there is no state for it to touch. A message with no window
/// in it produces nothing — a window reported as zero would claim an untouched
/// plan, which is a different thing from a CLI that said nothing.
fn rate_limit(event: wire::RateLimit, out: &mut Vec<Event>) {
    let Some(info) = event.rate_limit_info else {
        return;
    };
    let windows = info.unified_windows.unwrap_or(wire::UnifiedWindows {
        five_hour: None,
        seven_day: None,
    });
    let windows = UsageWindows {
        five_hour: read_window(windows.five_hour),
        seven_day: read_window(windows.seven_day),
        using_overage: info.is_using_overage,
    };
    if windows.is_empty() {
        return;
    }
    out.push(Event::UsageWindows(windows));
}

/// One window, kept only where the CLI measured it.
fn read_window(window: Option<wire::Window>) -> Option<UsageWindow> {
    let window = window?;
    Some(UsageWindow {
        utilization: window.utilization?,
        resets_at: window.resets_at,
    })
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
    fn a_calls_target_is_whichever_argument_names_what_it_acts_on() {
        let target =
            |json: &str| target_of(&serde_json::from_str(json).expect("the arguments parse"));

        assert_eq!(
            target(r#"{"command":"cargo test"}"#).as_deref(),
            Some("cargo test")
        );
        assert_eq!(
            target(r#"{"file_path":"/repo/a.rs"}"#).as_deref(),
            Some("/repo/a.rs")
        );
        assert_eq!(
            target(r#"{"url":"https://example.test"}"#).as_deref(),
            Some("https://example.test")
        );
        assert_eq!(target(r#""ls -la""#).as_deref(), Some("ls -la"));
        // The command wins over the path a shell call also carries, so a
        // standing answer is about what ran rather than where it ran.
        assert_eq!(
            target(r#"{"path":"/repo","command":"cargo test"}"#).as_deref(),
            Some("cargo test")
        );
        // Nothing here names a thing the call acts on, and inventing one
        // would put a target in front of the operator that they never saw.
        assert_eq!(target(r#"{"todos":[]}"#), None);
        assert_eq!(target("null"), None);
    }

    #[test]
    fn a_prompt_with_nothing_to_address_an_answer_to_is_shown_and_not_queued() {
        let mut translator = translator();

        let events = translator.line(
            r#"{"type":"control_request","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"},"tool_use_id":"toolu_1"}}"#,
        );

        assert!(
            matches!(events.as_slice(), [Event::PermissionRequest { .. }]),
            "{events:?}"
        );
        assert!(
            translator.take_asked().is_empty(),
            "the shell would have waited on a modal whose answer goes nowhere"
        );
    }

    #[test]
    fn a_control_request_this_bridge_does_not_answer_is_left_to_the_cli() {
        let mut translator = translator();

        let events = translator.line(
            r#"{"type":"control_request","request_id":"c9","request":{"subtype":"initialize"}}"#,
        );

        assert!(events.is_empty(), "{events:?}");
        assert!(translator.take_asked().is_empty());
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
            Event::PermissionRequest {
                id,
                tool,
                input,
                target,
            },
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
        assert_eq!(
            target.as_deref(),
            Some("rm -rf build"),
            "a refusal the operator may want a standing answer about lost its target"
        );
        assert_eq!(*decision, PermissionDecision::Deny);
        assert!(
            result.is_empty(),
            "the closing result reported the same refusal again: {result:?}"
        );
    }

    #[test]
    fn a_call_refused_here_reads_as_denied_and_is_not_reported_twice() {
        let mut translator = translator();
        translator.line(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"rm -rf build"}}]}}"#,
        );

        // The operator refused it over the control channel. The CLI knows
        // nothing of that yet; it just hands the model an error.
        translator.refused(&ToolCallId::new("toolu_1"));
        let back = translator.line(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":true,"content":"The operator denied this call in Niobe."}]}}"#,
        );

        let [Event::ToolCallEnd { outcome, .. }] = back.as_slice() else {
            panic!("the call ended: {back:?}");
        };
        assert_eq!(
            *outcome,
            ToolOutcome::Denied,
            "a call that was not allowed to run read as a tool that broke"
        );

        // The closing `result` lists it, and the CLI may announce it too;
        // neither is a second refusal.
        assert!(
            translator
                .line(
                    r#"{"type":"system","subtype":"permission_denied","tool_name":"Bash","tool_use_id":"toolu_1"}"#
                )
                .is_empty()
        );
        assert!(
            translator
                .line(
                    r#"{"type":"result","subtype":"success","permission_denials":[{"tool_name":"Bash","tool_use_id":"toolu_1"}]}"#
                )
                .is_empty()
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
    fn the_mode_the_cli_started_in_reaches_the_session() {
        let mut translator = Translator::new("max");

        let events = translator.line(
            r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5","permissionMode":"default"}"#,
        );

        assert!(
            events.contains(&Event::ModeSelected { mode: Mode::Ask }),
            "{events:?}"
        );
    }

    #[test]
    fn every_mode_niobe_models_is_read_back_from_the_cli_spelling() {
        for (spelt, mode) in [
            ("plan", Mode::Plan),
            ("default", Mode::Ask),
            ("manual", Mode::Ask),
            ("auto", Mode::Auto),
        ] {
            let mut translator = Translator::new("max");
            let events = translator.line(&format!(
                r#"{{"type":"system","subtype":"init","permissionMode":"{spelt}"}}"#
            ));
            assert_eq!(
                events,
                [Event::ModeSelected { mode }],
                "`{spelt}` did not read as {mode}"
            );
        }
    }

    #[test]
    fn a_permission_mode_niobe_does_not_model_is_reported_rather_than_guessed_at() {
        let mut translator = Translator::new("max");

        let events = translator
            .line(r#"{"type":"system","subtype":"init","permissionMode":"bypassPermissions"}"#);

        let said = warnings(&events);
        assert_eq!(said.len(), 1, "{events:?}");
        assert!(said[0].contains("`bypassPermissions`"), "{said:?}");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::ModeSelected { .. })),
            "a mode the shell cannot show was shown anyway: {events:?}"
        );
    }

    #[test]
    fn a_request_niobe_made_that_the_cli_refused_is_reported_rather_than_dropped() {
        let mut translator = translator();

        let refused = translator.line(
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"niobe-2","error":"Cannot set permission mode: must be one of acceptEdits, auto, bypassPermissions, default, dontAsk, plan"}}"#,
        );
        let accepted = translator.line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-1"}}"#,
        );

        let said = warnings(&refused);
        assert_eq!(said.len(), 1, "{refused:?}");
        assert!(said[0].contains("Cannot set permission mode"), "{said:?}");
        assert!(
            accepted.is_empty(),
            "a request the CLI took was reported: {accepted:?}"
        );
    }

    #[test]
    fn a_turn_the_cli_gave_no_total_for_is_not_reported_as_one_that_does_not_add_up() {
        let mut translator = translator();
        translator.line(&delta(2, 10));

        // What a budget stop closes a turn with: the per-model figures are
        // there, and the turn's own `usage` block is all zeros.
        let events = translator.line(
            r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"errors":["Reached maximum budget ($0.02)"],"usage":{"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}"#,
        );

        let said = warnings(&events);
        assert!(
            !said.iter().any(|line| line.contains("do not add up")),
            "a turn the CLI totalled nothing for was reported as a mismatch: {said:?}"
        );
        assert_eq!(said.len(), 1, "{said:?}");
    }

    #[test]
    fn a_turn_the_budget_stopped_is_reported_in_the_clis_own_words() {
        let mut translator = translator();

        let events = translator.line(
            r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"errors":["Reached maximum budget ($0.02)"]}"#,
        );

        let said = warnings(&events);
        assert_eq!(said.len(), 1, "{events:?}");
        assert!(said[0].contains("error_max_budget_usd"), "{said:?}");
        assert!(
            said[0].contains("Reached maximum budget ($0.02)"),
            "the operator was not told why the session stopped: {said:?}"
        );
    }

    /// The shape is the one the CLI printed on 17 September 2026 (Claude Code
    /// 2.1.274); the figures are the fixture's.
    #[test]
    fn a_rate_limit_event_becomes_the_windows_it_reported() {
        let mut translator = translator();

        let events = translator.line(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1789779600,"rateLimitType":"five_hour","overageStatus":"rejected","overageDisabledReason":"out_of_credits","isUsingOverage":false,"unifiedWindows":{"five_hour":{"utilization":0.62,"resetsAt":1789779600},"seven_day":{"utilization":0.18,"resetsAt":1790118000}}}}"#,
        );

        assert_eq!(
            events,
            [Event::UsageWindows(UsageWindows {
                five_hour: Some(UsageWindow {
                    utilization: 0.62,
                    resets_at: Some(1_789_779_600),
                }),
                seven_day: Some(UsageWindow {
                    utilization: 0.18,
                    resets_at: Some(1_790_118_000),
                }),
                using_overage: false,
            })]
        );
    }

    #[test]
    fn a_plan_spending_beyond_its_flat_fee_says_so() {
        let mut translator = translator();

        let events = translator.line(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","isUsingOverage":true,"unifiedWindows":{"five_hour":{"utilization":1.0,"resetsAt":1789779600},"seven_day":{"utilization":0.91,"resetsAt":1790118000}}}}"#,
        );

        let [Event::UsageWindows(windows)] = events.as_slice() else {
            panic!("one usage-windows event: {events:?}");
        };
        assert!(
            windows.using_overage,
            "the plan is spending real money and nothing said so"
        );
    }

    /// The windows are the only part of the message Niobe reads. A version
    /// that reports a rate limit without them has nothing to show, and showing
    /// nothing is not the same as reporting a window at zero.
    #[test]
    fn a_rate_limit_with_no_windows_in_it_reports_nothing() {
        let mut translator = translator();

        for line in [
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#,
            r#"{"type":"rate_limit_event"}"#,
            r#"{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{}}}"#,
        ] {
            assert!(translator.line(line).is_empty(), "{line}");
        }
    }

    /// The CLI reports the windows several times a turn, each a fresh level
    /// rather than an increment, so every report is passed on and the fold is
    /// what keeps only the last.
    #[test]
    fn every_report_of_a_window_is_passed_on() {
        let mut translator = translator();
        let line = |used: &str| {
            format!(
                r#"{{"type":"rate_limit_event","rate_limit_info":{{"unifiedWindows":{{"five_hour":{{"utilization":{used}}}}}}}}}"#
            )
        };

        let first = translator.line(&line("0.14"));
        let second = translator.line(&line("0.15"));

        assert_eq!(
            first,
            [Event::UsageWindows(UsageWindows {
                five_hour: Some(UsageWindow {
                    utilization: 0.14,
                    resets_at: None,
                }),
                seven_day: None,
                using_overage: false,
            })],
            "a window without a reset time was dropped or given one"
        );
        assert_eq!(second.len(), 1, "{second:?}");
    }

    /// The shapes are those the CLI printed on 18 September 2026 (Claude Code
    /// 2.1.277): an `Edit` carries `old_string`, `new_string` and
    /// `replace_all`, a `Write` carries `content`, and the result of each is
    /// one line of the CLI's own prose.
    fn call(id: &str, name: &str, arguments: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"{id}","name":"{name}","input":{arguments}}}]}}}}"#
        )
    }

    fn result(id: &str, content: &str, is_error: bool) -> String {
        let content = serde_json::Value::String(content.to_owned());
        format!(
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"{id}","is_error":{is_error},"content":{content}}}]}}}}"#
        )
    }

    const UPDATED: &str = "The file /repo/notes.txt has been updated successfully. \
                           (file state is current in your context — no need to Read it back)";

    /// Everything the events say about files that changed.
    fn changes(events: &[Event]) -> Vec<(String, Option<u64>, Option<u64>)> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::FileChange {
                    path,
                    added,
                    removed,
                } => Some((path.clone(), *added, *removed)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_edit_is_counted_from_the_text_it_replaced_and_the_text_it_wrote() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Edit",
            r#"{"file_path":"/repo/notes.txt","old_string":"gamma","new_string":"gamma one\ngamma two","replace_all":false}"#,
        ));

        let events = translator.line(&result("t1", UPDATED, false));

        assert_eq!(
            changes(&events),
            vec![("notes.txt".to_owned(), Some(2), Some(1))],
            "{events:?}"
        );
    }

    /// Measured on this machine: replacing three occurrences of one line shows
    /// as `3 3` in `git diff --numstat`, and the call describes one of them.
    /// Reporting that one would under-report the other two.
    #[test]
    fn a_replacement_the_cli_applied_everywhere_states_no_count() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Edit",
            r#"{"file_path":"/repo/rep.txt","old_string":"foo","new_string":"bar","replace_all":true}"#,
        ));

        let events = translator.line(&result(
            "t1",
            "The file /repo/rep.txt has been updated. All occurrences were successfully replaced.",
            false,
        ));

        assert_eq!(
            changes(&events),
            vec![("rep.txt".to_owned(), None, None)],
            "a count the call cannot support was reported: {events:?}"
        );
    }

    #[test]
    fn a_written_file_that_was_not_there_removed_nothing() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Write",
            r##"{"file_path":"/repo/fresh.py","content":"# comment\ndef foo():\n    return None\n"}"##,
        ));

        let events = translator.line(&result(
            "t1",
            "File created successfully at: /repo/fresh.py (file state is current in your context)",
            false,
        ));

        assert_eq!(
            changes(&events),
            vec![("fresh.py".to_owned(), Some(3), Some(0))],
            "{events:?}"
        );
    }

    /// A `Write` carries what the file becomes and never what it was, so the
    /// lines it dropped are not in the stream. Measured: overwriting a
    /// three-line file with one line shows as `1 3`, and the call says `1`.
    #[test]
    fn a_written_file_that_was_already_there_states_no_removal() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Write",
            r#"{"file_path":"/repo/doomed.txt","content":"gone"}"#,
        ));

        let events = translator.line(&result(
            "t1",
            "The file /repo/doomed.txt has been updated successfully.",
            false,
        ));

        assert_eq!(
            changes(&events),
            vec![("doomed.txt".to_owned(), Some(1), None)],
            "{events:?}"
        );
    }

    #[test]
    fn a_notebook_changed_by_cell_says_it_changed_and_not_by_how_much() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "NotebookEdit",
            r#"{"notebook_path":"/repo/run.ipynb","cell_id":"c2","new_source":"import io"}"#,
        ));

        let events = translator.line(&result("t1", "Updated cell c2", false));

        assert_eq!(changes(&events), vec![("run.ipynb".to_owned(), None, None)]);
    }

    #[test]
    fn an_edit_that_did_not_run_changed_no_file() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Write",
            r#"{"file_path":"/repo/doomed.txt","content":"gone"}"#,
        ));

        let events = translator.line(&result(
            "t1",
            "<tool_use_error>File has not been read yet. Read it first before writing to it.\
             </tool_use_error>",
            true,
        ));

        assert!(
            changes(&events).is_empty(),
            "a call that failed put a file in the change set: {events:?}"
        );
    }

    #[test]
    fn a_call_that_edits_nothing_changes_no_file() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call("t1", "Read", r#"{"file_path":"/repo/notes.txt"}"#));

        let events = translator.line(&result("t1", "1\tline one", false));

        assert!(changes(&events).is_empty(), "{events:?}");
    }

    #[test]
    fn a_file_outside_the_session_directory_keeps_the_path_the_cli_gave() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Edit",
            r#"{"file_path":"/etc/hosts","old_string":"a","new_string":"b"}"#,
        ));

        let events = translator.line(&result("t1", UPDATED, false));

        assert_eq!(
            changes(&events),
            vec![("/etc/hosts".to_owned(), Some(1), Some(1))]
        );
    }
}
