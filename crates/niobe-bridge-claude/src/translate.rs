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
//! Six properties of the stream are not obvious and are the reason this file
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
//! * **`/clear` restarts those running totals.** The CLI announces it with a
//!   `conversation_reset`, and the first `result` after it reports the new
//!   conversation from zero, under a `session_id` the next `init` gives. The
//!   difference is taken from there again, or the new conversation is billed
//!   only for what it spent beyond the old one.
//! * **A refusal is reported twice.** `system`/`permission_denied` announces
//!   it between the call and the result it comes back as, and the closing
//!   `result` lists it again. Counting both doubles every denial; reading the
//!   first is also the only way to tell a call that was not allowed to run
//!   from a tool that broke, because the model is told about both as errors.
//!   That holds for a call the CLI refuses by itself. A call refused over the
//!   control channel gets no `permission_denied` at all, so the driver tells
//!   the translator about it instead ([`Translator::refused`]).
//! * **A sub-agent's call returns before the sub-agent is done.** The CLI
//!   runs a sub-agent in the background unless told otherwise, answers the
//!   call at once with a `tool_use_result` whose `status` is `async_launched`,
//!   and says the agent stopped only later, in a `system`/`task_notification`
//!   naming the call. Ending the agent at its call's result would show every
//!   one of them finished while it is still working. Measured across the
//!   CLI's own transcripts on the machine this was written on: 465 of 477
//!   sub-agent calls were launched that way.
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
//! * A sub-agent's own figures are what the CLI reports for that agent alone,
//!   and they become [`Event::AgentProgress`]: the model its messages name,
//!   the step `system`/`task_progress` says it is on, the first line of the
//!   answer its `task_notification` carries, and the `usage.total_tokens` of
//!   both. That count is one number with no model and no split, and it follows
//!   the size of the agent's conversation rather than summing what it was
//!   billed, so it is carried as a size and never priced.
//! * A sub-agent's `parent_tool_use_id` says which sub-agent call a message
//!   belongs to. Its tool calls and its finished messages carry that agent,
//!   and its tokens are folded in under the model that produced them. Its
//!   text is not streamed: two agents' fragments arriving interleaved would
//!   have to be reassembled per agent, and the finished message is the same
//!   words.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use niobe_core::diff::{self, Hunk, Line};
use niobe_core::event::{
    AgentId, AgentOutcome, Backend, Billing, Context, CostBasis, Event, Mode, PermissionDecision,
    SessionMeta, SlashCommand, ToolCallId, ToolOutcome, Usage, UsageWindow, UsageWindows,
};
use niobe_core::session::TestRunRecord;
use niobe_core::test_run;

use crate::conformance;
use crate::wire;

/// The names of the tool whose call is a sub-agent rather than an action.
///
/// Claude Code registers the tool as `Agent` with `Task` as its alias, and
/// counts a call under either name as a sub-agent it spawned. Measured on the
/// machine this was written on, 19 September 2026: 477 sub-agent calls in the
/// CLI's own transcripts, from twenty-one releases between 2.1.231 and
/// 2.1.278, every one of them `Agent`. `Task` is kept because the CLI still
/// accepts it and still lists it in `init`, so a model that reads the list
/// can call it.
const AGENT_TOOLS: [&str; 2] = ["Agent", "Task"];

/// How far apart two figures for the same money may be before the difference
/// is reported, in USD.
///
/// The CLI sums its per-model costs in floating point and prints the sum
/// separately, so the two disagree in the last bits. Half a hundredth of a
/// cent is below anything a screen shows and far above that error.
const COST_TOLERANCE_USD: f64 = 0.000_05;

/// Whether a sub-agent this session spawned is still working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Running {
    Yes,
    No,
}

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

    /// The smaller of the two, field by field.
    fn least(self, other: Self) -> Self {
        Self {
            input: self.input.min(other.input),
            output: self.output.min(other.output),
            cache_read: self.cache_read.min(other.cache_read),
            cache_write: self.cache_write.min(other.cache_write),
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
    /// The call in one line, for the transcript.
    summary: Option<String>,
    /// The arguments as the CLI sent them. The rendered `input` is for a
    /// human; counting what an edit changed needs the fields themselves.
    arguments: serde_json::Value,
    /// The sub-agent that made the call, or `None` for the session's own: a
    /// prompt about the call is that agent's.
    agent: Option<AgentId>,
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
    /// The session id the last [`Event::SessionMeta`] named, so that a new one
    /// goes out when the CLI moves to another conversation on the same model,
    /// as it does after `/clear`.
    announced_session: Option<String>,
    /// The commands the CLI lists and has said it will not run in this
    /// process, which are left out of every list it sends.
    unavailable: Vec<String>,
    /// The model of the message in flight, per stream: the main session is
    /// `None` and each sub-agent is the id of the call that spawned it. `message_delta`
    /// carries the turn's authoritative usage and no model, so the model is
    /// remembered from the `message_start` that opened the same stream.
    in_flight: BTreeMap<Option<String>, String>,
    /// What each outstanding `tool_use` id was called and called with, so that
    /// the `tool_result` can repeat both without the consumer holding state.
    tool_calls: BTreeMap<String, Call>,
    /// The sub-agent behind each call that has ended since the last `result`,
    /// by call id: the closing line can list a refusal of one of them after
    /// its `tool_result` took the call out of `tool_calls`, and the refusal is
    /// that agent's.
    ended_agents: BTreeMap<String, AgentId>,
    /// The permission prompts read since the last drain, for whatever owns the
    /// CLI's standard input to answer.
    asked: Vec<Asked>,
    /// The sub-agents this session has spawned, by the id of the call that
    /// spawned each, and whether each is still running.
    agents: BTreeMap<String, Running>,
    /// The model each sub-agent was last reported answering with, by the id of
    /// the call that spawned it, so that a model is reported when it changes
    /// rather than with every message the agent writes.
    agent_models: BTreeMap<String, String>,
    /// What each sub-agent's own transcript says about it, by the id of the
    /// call that spawned it, where the session is read back rather than
    /// watched. Empty on a live session, which hears the same from the stream.
    recorded_agents: BTreeMap<String, RecordedAgent>,
    /// The `tool_use` ids the CLI refused and this bridge has already
    /// reported, so that the closing `result`'s list of the same refusals is
    /// not counted a second time.
    denied: BTreeMap<String, ()>,
    /// Per-message usage since the last `result`, for the turn reconciliation.
    turn: Counts,
    /// Per model, everything reported for it so far this session.
    reported: BTreeMap<String, Reported>,
    /// The context window the CLI last reported for each model id, from the
    /// `modelUsage` of its `result`s.
    windows: BTreeMap<String, u64>,
    /// The main agent's last request, as last reported, so that a window that
    /// arrives after it can be reported with it.
    context: Option<Context>,
    /// Whether the CLI's release has been read off the first `init`. The CLI
    /// writes `init` again at the start of every turn — Claude Code 2.1.282
    /// was recorded doing so for each prompt on its standard input — and the
    /// release it reports there has not changed.
    checked_release: bool,
    /// How the profile says the session is billed, which stands over anything
    /// the stream suggests. `None` where the profile left it to the stream.
    billed_as: Option<Billing>,
    /// Where the CLI's credential came from, off its `init`. `None` where it
    /// has not said, which is not the same as `none`.
    api_key_source: Option<String>,
    /// The billing last reported, so that it is reported when it changes
    /// rather than with every turn.
    billing: Option<Billing>,
    /// What reads a shell result the CLI saved to a file, where the caller
    /// handed one in. `None` leaves every such result unread.
    read_spilled: Option<ReadSpilled>,
}

/// What a sub-agent's own transcript says about it that its messages, folded
/// in where they happened, do not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RecordedAgent {
    /// The text of its last message, where that message called no tool after
    /// it: what it answered, and nothing where it stopped mid-step.
    pub(crate) answer: Option<String>,
}

/// Reads the whole of a shell result the CLI saved to a file for being too
/// large: the file at the path the CLI named, as long as it still holds the
/// number of bytes the CLI said it wrote. `None` where it does not, or cannot
/// be read.
///
/// Handed in rather than done here, so that the translator does no I/O of its
/// own and a recorded log exercises exactly the code a live session runs.
pub(crate) type ReadSpilled = fn(&Path, u64) -> Option<String>;

impl Translator {
    /// A translator for a session running under `profile`.
    pub fn new(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
            cwd: None,
            model: None,
            backend_session: None,
            announced_session: None,
            unavailable: Vec::new(),
            in_flight: BTreeMap::new(),
            tool_calls: BTreeMap::new(),
            ended_agents: BTreeMap::new(),
            asked: Vec::new(),
            agents: BTreeMap::new(),
            agent_models: BTreeMap::new(),
            recorded_agents: BTreeMap::new(),
            denied: BTreeMap::new(),
            turn: Counts::default(),
            reported: BTreeMap::new(),
            windows: BTreeMap::new(),
            context: None,
            checked_release: false,
            billed_as: None,
            api_key_source: None,
            billing: None,
            read_spilled: None,
        }
    }

    /// The same translator, told how the session is billed by the profile it
    /// runs under.
    ///
    /// Taken over what the stream suggests: the stream shows which API served
    /// a request and where its key came from, and a seat billed by use signs
    /// in exactly as a flat-rate plan does.
    #[must_use]
    pub fn billed_as(mut self, billing: Billing) -> Self {
        self.billed_as = Some(billing);
        self
    }

    /// The same translator, told where the session runs, so that the files it
    /// reports changed are named relative to it.
    #[must_use]
    pub fn in_dir(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// The same translator, reading a shell result the CLI saved to a file
    /// with `read`.
    ///
    /// Only a `cargo test` run's result is read, and only once, when its call
    /// ends: the CLI hands the model the first 2 KB of an output past about
    /// 30 KB, and a whole workspace's test run is past that.
    #[must_use]
    pub(crate) fn reading_spilled_with(mut self, read: ReadSpilled) -> Self {
        self.read_spilled = Some(read);
        self
    }

    /// The same translator, told what each sub-agent's own transcript says
    /// about it, by the id of the call that spawned it.
    ///
    /// A session read back from the CLI's transcripts has no notification
    /// carrying an agent's answer: the session's file says only that the
    /// agent was launched and that it stopped. The agent's own file says
    /// what it answered, which is reported where the stream would have said
    /// it — just before the end.
    #[must_use]
    pub(crate) fn knowing_agents(mut self, recorded: BTreeMap<String, RecordedAgent>) -> Self {
        self.recorded_agents = recorded;
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
    ///
    /// This is the only thing that marks such a call denied. Recorded against
    /// Claude Code 2.1.278: the CLI sends no `system`/`permission_denied` for a
    /// call refused over the control channel, only for one it refuses by its
    /// own rules.
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
    /// A line whose `type` this bridge does not know becomes a notice, and a
    /// line that does not parse becomes a warning entry: the CLI ships new
    /// shapes with new versions, and a bridge that dies on one takes the
    /// session with it. Only the second is a failure — the first is a shape
    /// nobody has read yet, which the operator should see without being told
    /// that something went wrong.
    pub fn line(&mut self, line: &str) -> Vec<Event> {
        match serde_json::from_str::<wire::Message>(line) {
            Ok(wire::Message::Unknown) => vec![unread(format!(
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
            wire::Message::ConversationReset(_) => self.reset(&mut out),
            // Only the line a message arrived on says what type it was, so
            // whoever read that line is the one that can report it.
            wire::Message::Unknown => {}
        }
        out
    }

    /// Reads the CLI's answer to a request of Niobe's own: the slash commands
    /// the answer to `initialize` lists, or the refusal of a change.
    ///
    /// Only this side asks the CLI anything, so every `control_response` is an
    /// answer to a request made here — what the CLI offers, or a mode or a
    /// model the session was asked to move to. A refusal left unreported would
    /// leave the shell showing a change that never happened.
    fn answered(&mut self, response: wire::ControlResponse, out: &mut Vec<Event>) {
        let Some(outcome) = response.response else {
            return;
        };
        if outcome.subtype.as_deref() == Some("success") {
            let Some(answer) = outcome.response else {
                return;
            };
            if answer.fast_mode_disabled_reason.is_some() {
                self.unavailable.push("fast".to_owned());
            }
            if let Some(commands) = answer.commands {
                out.push(self.listed(commands));
            }
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
                self.check_release(system.claude_code_version.as_deref(), out);
                if let Some(id) = system.session_id {
                    self.backend_session = Some(id);
                }
                if let Some(model) = system.model {
                    self.set_model(model, out);
                }
                if system.api_key_source.is_some() {
                    self.api_key_source = system.api_key_source;
                }
                self.report_billing(None, out);
                if let Some(mode) = system.permission_mode {
                    match read_mode(&mode) {
                        Some(mode) => out.push(Event::ModeSelected { mode }),
                        // The CLI gates calls in ways Niobe has no word for —
                        // `acceptEdits`, `bypassPermissions`, `dontAsk` — and
                        // a profile can ask for one through its own arguments.
                        // Naming it beats showing a mode the session is not in.
                        None => out.push(warn(format!(
                            "the CLI is gating tool calls as `{mode}`, which this version of \
                             Niobe does not model. No mode is reported until the session is \
                             moved to one it does."
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
            Some("task_notification") => self.task_notification(system, out),
            Some("task_progress") => self.task_progress(system, out),
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
            // The CLI's bookkeeping for the work it runs in the background, as
            // Claude Code 2.1.278 reports it. Passed over, each for its own
            // reason:
            //
            // * `task_started` restates the call that started the task, which
            //   is already a spawn for a sub-agent and a tool call for anything
            //   else.
            // * `task_updated` patches the task's status by the CLI's own task
            //   id and names no call. The same end arrives as a
            //   `task_notification` that does, and that is the one read.
            // * `background_tasks_changed` lists what is running now, which is
            //   what the spawns and exits already fold to.
            Some("task_started" | "task_updated" | "background_tasks_changed") => {}
            // The CLI's whole slash-command list, pushed when it changes after
            // the process started — an MCP server's prompts arriving late, a
            // skill found mid-session — which can be before `init`, and before
            // the answer to `initialize` that carries the list as it started.
            // Both are the whole list as it stood when sent, so whichever
            // arrives last is the list.
            Some("commands_changed") => {
                if let Some(commands) = system.commands {
                    out.push(self.listed(commands));
                }
            }
            other => out.push(unread(format!(
                "the CLI sent a system message of subtype `{}`, which this version of Niobe \
                 does not know how to read.",
                other.unwrap_or("(none)")
            ))),
        }
    }

    /// Whether `id` is the call that spawned one of this session's
    /// sub-agents, running or not.
    fn is_agent(&self, id: Option<&str>) -> bool {
        id.is_some_and(|id| self.agents.contains_key(id))
    }

    /// The CLI's word, on the live stream, that something it ran in the
    /// background has stopped.
    ///
    /// Only a sub-agent's end is read. A background command's is passed
    /// over: its call already ended when the command was put in the
    /// background, the model reads the output for itself, and the transcript
    /// the CLI writes of the same session passes it over too, so a session
    /// read back folds to what it showed live.
    ///
    /// What the agent answered and how large its conversation ended up are
    /// reported before its end, because nothing is reported about an agent
    /// after it.
    fn task_notification(&mut self, system: wire::System, out: &mut Vec<Event>) {
        let Some(id) = system.tool_use_id else { return };
        let answer = system.summary.as_deref().and_then(opening_line);
        self.agent_progress(&id, None, total_tokens(system.usage.as_ref()), answer, out);
        out.append(&mut self.task_stopped(&id, system.status.as_deref()));
    }

    /// The CLI's word on a sub-agent it is running: the step it is on and how
    /// large its conversation has grown. Passed over for a task that is not a
    /// sub-agent this session spawned and is still running.
    fn task_progress(&mut self, system: wire::System, out: &mut Vec<Event>) {
        let Some(id) = system.tool_use_id else { return };
        let step = system.description.as_deref().and_then(opening_line);
        self.agent_progress(&id, None, total_tokens(system.usage.as_ref()), step, out);
    }

    /// Records the model a sub-agent's own message names, and reports it the
    /// first time it is named and whenever it changes.
    fn agent_model(&mut self, id: &str, model: String, out: &mut Vec<Event>) {
        if !self.is_running(id) || self.agent_models.get(id) == Some(&model) {
            return;
        }
        self.agent_models.insert(id.to_owned(), model.clone());
        self.agent_progress(id, Some(model), None, None, out);
    }

    /// Reports what is known about the running sub-agent spawned by call
    /// `id`, where anything is.
    fn agent_progress(
        &self,
        id: &str,
        model: Option<String>,
        context_tokens: Option<u64>,
        latest: Option<String>,
        out: &mut Vec<Event>,
    ) {
        if !self.is_running(id) || (model.is_none() && context_tokens.is_none() && latest.is_none())
        {
            return;
        }
        out.push(Event::AgentProgress {
            id: AgentId::new(id.to_owned()),
            model,
            context_tokens,
            latest,
        });
    }

    /// Whether call `id` spawned a sub-agent that has not ended.
    fn is_running(&self, id: &str) -> bool {
        self.agents.get(id) == Some(&Running::Yes)
    }

    /// The events for the CLI saying that the background task call `id`
    /// started has stopped, with `status` in the CLI's spelling.
    ///
    /// Nothing, where `id` spawned no sub-agent: the CLI notifies about
    /// background commands the same way. Nothing either for a sub-agent that
    /// has already ended — one that is sent another message runs again and
    /// notifies again, and only the first end is one this session saw begin.
    ///
    /// The live stream spells the statuses `completed`, `failed` and
    /// `stopped`, which are the CLI's own schema for the message; its
    /// transcripts also say `killed` for a task stopped from outside. Both
    /// read as cancelled.
    pub(crate) fn task_stopped(&mut self, id: &str, status: Option<&str>) -> Vec<Event> {
        let mut out = Vec::new();
        if !self.is_agent(Some(id)) {
            return out;
        }
        let outcome = match status {
            Some("completed") => AgentOutcome::Completed,
            Some("failed") => AgentOutcome::Failed,
            Some("stopped" | "killed") => AgentOutcome::Cancelled,
            other => {
                out.push(warn(format!(
                    "the CLI reported sub-agent `{id}` as `{}`, which this version of Niobe \
                     does not read as an end, so it is still shown as running.",
                    other.unwrap_or("(no status)")
                )));
                return out;
            }
        };
        self.agent_ended(id, outcome, &mut out);
        out
    }

    /// Ends the sub-agent spawned by call `id`, once, if the call spawned one.
    ///
    /// Where the agent's own transcript holds its answer, a completed agent
    /// reports it first, as the live stream's notification does. An agent
    /// that failed or was stopped has no answer: its last words are whatever
    /// it was saying when it was cut off, which may be what it meant to do
    /// next.
    fn agent_ended(&mut self, id: &str, outcome: AgentOutcome, out: &mut Vec<Event>) {
        if !self.is_running(id) {
            return;
        }
        if outcome == AgentOutcome::Completed {
            let answer = self
                .recorded_agents
                .get(id)
                .and_then(|recorded| recorded.answer.as_deref())
                .and_then(opening_line);
            self.agent_progress(id, None, None, answer, out);
        }
        self.agents.insert(id.to_owned(), Running::No);
        out.push(Event::AgentExit {
            id: AgentId::new(id.to_owned()),
            outcome,
        });
    }

    /// Says, once a session, when the CLI on the other end is a release these
    /// recordings do not cover.
    ///
    /// The warning is for what a version check can catch and a message-by-
    /// message check cannot: a type the bridge does not know announces itself
    /// when it arrives, but a count that moved to another message, or a field
    /// that kept its name and changed its meaning, arrives looking exactly
    /// like one that did not — and would be folded into a total the product
    /// promises is measured. A release that says nothing about itself is left
    /// alone: a warning that named no version would say nothing the operator
    /// could act on.
    ///
    /// The CLI writes `init` again whenever the session moves model, so this
    /// reports the first one and nothing after it.
    fn check_release(&mut self, version: Option<&str>, out: &mut Vec<Event>) {
        if self.checked_release {
            return;
        }
        self.checked_release = true;
        let Some(version) = version else { return };
        if conformance::recorded(version) {
            return;
        }
        out.push(warn(conformance::unrecorded(version)));
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
        let agent = call.and_then(|call| call.agent.clone());

        self.denied.insert(id.clone(), ());
        let id = ToolCallId::new(id);
        out.push(Event::PermissionRequest {
            id: id.clone(),
            tool,
            input,
            target,
            agent,
        });
        out.push(Event::PermissionResponse {
            id,
            decision: PermissionDecision::Deny,
            message: None,
        });
    }

    /// Records the model the session is on, producing a fresh [`SessionMeta`]
    /// whenever it changes — which is how a routing decision reaches the
    /// menu row.
    /// The list the CLI sent, less what it has said it will not run here.
    ///
    /// A command the CLI lists and then refuses in every session this bridge
    /// drives is one the operator would pick only to be told no.
    fn listed(&self, commands: Vec<wire::Command>) -> Event {
        listed_as_offered(commands, &self.unavailable)
    }

    /// The conversation started over: the CLI's running totals restart at
    /// zero with it, and its context is gone.
    ///
    /// Recorded from Claude Code 2.1.282: the first `result` after `/clear`
    /// reported that turn's cost alone. Measured against the totals from
    /// before, the new conversation would be billed only for what it spent
    /// beyond the old one — $0.0165 for a turn that cost $0.0424 — so what has
    /// been reported per model is forgotten here, and nothing already counted
    /// is counted again: the CLI's totals from here on hold nothing from
    /// before.
    fn reset(&mut self, out: &mut Vec<Event>) {
        self.reported.clear();
        self.turn = Counts::default();
        self.context = None;
        out.push(Event::Cleared);
    }

    /// Says what is running, where the model or the conversation has changed
    /// since it was last said.
    fn set_model(&mut self, model: String, out: &mut Vec<Event>) {
        if self.model.as_deref() == Some(model.as_str())
            && self.announced_session == self.backend_session
        {
            return;
        }
        self.model = Some(model.clone());
        self.announced_session = self.backend_session.clone();
        out.push(Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: self.profile.clone(),
            model,
            backend_session: self.backend_session.clone(),
        }));
    }

    fn assistant(&mut self, envelope: wire::Envelope, out: &mut Vec<Event>) {
        if let (Some(agent), Some(model)) = (
            envelope.parent_tool_use_id.as_deref(),
            envelope.message.model.clone(),
        ) {
            self.agent_model(agent, model, out);
        }
        // A message that names the sub-agent call it belongs to is that
        // agent's: its words and its calls are the agent's, not the session's.
        let agent = envelope.parent_tool_use_id.map(AgentId::new);
        let blocks = match envelope.message.content {
            Some(wire::Content::Blocks(blocks)) => blocks,
            Some(wire::Content::Text(text)) => {
                out.push(Event::AssistantMessage { text, agent });
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
                    let summary = self.summary_of(&name, &input);
                    self.tool_calls.insert(
                        id.clone(),
                        Call {
                            name: name.clone(),
                            input: rendered.clone(),
                            target: target_of(&input),
                            summary: summary.clone(),
                            arguments: input.clone(),
                            agent: agent.clone(),
                        },
                    );
                    if AGENT_TOOLS.contains(&name.as_str()) {
                        self.agents.insert(id.clone(), Running::Yes);
                        calls.push(Event::AgentSpawn {
                            id: AgentId::new(id.clone()),
                            parent: agent.clone(),
                            label: label_of(&input).unwrap_or_else(|| rendered.clone()),
                        });
                    }
                    calls.push(Event::ToolCallStart {
                        id: ToolCallId::new(id),
                        name,
                        input: rendered,
                        summary,
                        agent: agent.clone(),
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
            out.push(Event::AssistantMessage { text, agent });
        }
        out.append(&mut calls);
    }

    fn user(&mut self, envelope: wire::Envelope, out: &mut Vec<Event>) {
        let background = envelope.launched_in_background();
        let reported = envelope.tool_use_result;
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
            let (name, input, summary, arguments) = match self.tool_calls.remove(&tool_use_id) {
                Some(call) => {
                    if let Some(agent) = call.agent {
                        self.ended_agents.insert(tool_use_id.clone(), agent);
                    }
                    (call.name, call.input, call.summary, call.arguments)
                }
                None => {
                    out.push(warn(format!(
                        "the CLI returned a result for tool call `{tool_use_id}`, which it never \
                         announced. The call is counted; what it was called with is lost."
                    )));
                    (String::new(), String::new(), None, serde_json::Value::Null)
                }
            };
            // A refusal reaches the model as an error, so the result alone
            // cannot tell a tool that broke from one that was not allowed to
            // run. What says which came first: the `permission_denied` the CLI
            // sends for a refusal of its own, or the driver's word for one
            // made over the control channel.
            let outcome = match (
                is_error.unwrap_or(false),
                self.denied.contains_key(&tool_use_id),
            ) {
                (_, true) => ToolOutcome::Denied,
                (true, false) => ToolOutcome::Failed,
                (false, false) => ToolOutcome::Ok,
            };

            // A sub-agent the CLI ran in the background is still working when
            // its call returns — the result says only that it was launched —
            // and its end arrives later as a `task_notification`. One that ran
            // in the foreground, or never started, ends with its call.
            let background = outcome == ToolOutcome::Ok && background;
            if !background {
                self.agent_ended(
                    &tool_use_id,
                    match outcome {
                        ToolOutcome::Ok => AgentOutcome::Completed,
                        ToolOutcome::Failed | ToolOutcome::Denied => AgentOutcome::Failed,
                    },
                    out,
                );
            }

            // Only a call that ran changed anything: a refused or broken edit
            // would otherwise put a file in the change set that is not in the
            // diff.
            let change = match outcome {
                ToolOutcome::Ok => self.file_change(&name, &arguments, &output, reported.as_ref()),
                ToolOutcome::Failed | ToolOutcome::Denied => None,
            };
            let exit_code = exit_code(&name, outcome, &output, reported.as_ref());
            let error = match outcome {
                ToolOutcome::Ok => None,
                ToolOutcome::Failed | ToolOutcome::Denied => failure_reason(&output, exit_code),
            };

            let tested = test_run(
                &name,
                &arguments,
                outcome,
                &output,
                reported.as_ref(),
                exit_code,
                self.read_spilled,
            );
            let id = ToolCallId::new(tool_use_id);
            let tested = tested.map(|run| Event::TestRun {
                id: id.clone(),
                counts: run.counts,
                exit_code: run.exit_code,
                failed: run.failed,
                failures: run.failures,
            });
            out.push(Event::ToolCallEnd {
                id,
                name,
                input,
                output,
                bytes,
                outcome,
                summary,
                exit_code,
                error,
            });
            out.extend(change);
            out.extend(tested);
        }
    }

    /// What a finished call did to a file, where it was a call that edits one.
    ///
    /// The counts come from the call's own arguments where those settle them,
    /// and otherwise from the diff the CLI reported beside the result. Two
    /// cases the arguments do not settle:
    ///
    /// * **A replacement the CLI applied everywhere.** `replace_all` says the
    ///   CLI matched `old_string` as many times as it appears in the file, and
    ///   the file is not in the arguments. Counting one occurrence would
    ///   under-report every further one: measured on this machine, a
    ///   `replace_all` of three occurrences shows as `3 3` in
    ///   `git diff --numstat` while the call describes one.
    /// * **A `Write` over a file that already existed.** The call carries what
    ///   the file becomes and never what it was.
    ///
    /// The report's patch is of the whole file, so it settles both. Where no
    /// usable patch was reported the counts stay unstated rather than filled
    /// in.
    fn file_change(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        output: &str,
        reported: Option<&serde_json::Value>,
    ) -> Option<Event> {
        let (path, added, removed, hunks) = match name {
            EDIT_TOOL => {
                let path = string_at(arguments, "file_path")?;
                let hunks = reported_hunks(reported, path);
                let counts = match arguments
                    .get("replace_all")
                    .and_then(serde_json::Value::as_bool)
                {
                    Some(true) => diff::hunks_changed(&hunks),
                    Some(false) | None => diff::lines_changed(
                        string_at(arguments, "old_string").unwrap_or_default(),
                        string_at(arguments, "new_string").unwrap_or_default(),
                    ),
                };
                match counts {
                    Some((added, removed)) => (path, Some(added), Some(removed), hunks),
                    None => (path, None, None, hunks),
                }
            }

            WRITE_TOOL => {
                let path = string_at(arguments, "file_path")?;
                let written = string_at(arguments, "content").unwrap_or_default();
                // The CLI says which of the two it did, and its answer is read
                // narrowly on purpose: a wording it no longer uses leaves the
                // removal unstated, which is a figure the pane marks, where
                // guessing "nothing was there" would be a zero that is wrong.
                let created = output.starts_with(CREATED_PREFIX);
                let hunks = match created {
                    true => created_hunk(reported, path),
                    false => reported_hunks(reported, path),
                };
                match (created, diff::hunks_changed(&hunks)) {
                    (false, Some((added, removed))) => (path, Some(added), Some(removed), hunks),
                    (true, _) | (false, None) => (
                        path,
                        Some(written.lines().count() as u64),
                        created.then_some(0),
                        hunks,
                    ),
                }
            }

            // A notebook is edited by cell, so the call says which cell and
            // never how many lines. That the file changed is still worth
            // showing; how much it changed, this call cannot say.
            NOTEBOOK_TOOL => (
                string_at(arguments, "notebook_path")?,
                None,
                None,
                Vec::new(),
            ),

            _ => return None,
        };

        Some(Event::FileChange {
            path: self.relative(path),
            added,
            removed,
            hunks,
        })
    }

    /// A call in the words a person would use for it: what a shell command
    /// runs, which file a file tool reads or writes, what a search looks for.
    ///
    /// Built from the CLI's own tool schemas, which is why it is worded here
    /// and not in the shell. A tool this does not know — an MCP server's, a
    /// new one — is read as its scalar arguments, `key: value`, with nested
    /// values reduced to their shape, so it still reads as words rather than
    /// as JSON. Arguments that say nothing (an empty object) give `None`.
    fn summary_of(&self, name: &str, input: &serde_json::Value) -> Option<String> {
        let field = |key| input.get(key).and_then(serde_json::Value::as_str);
        let path = |key| field(key).map(|path| self.relative(path));
        match name {
            "Bash" => field("command").map(first_line),
            "Read" | "Edit" | "MultiEdit" | "Write" => path("file_path"),
            "NotebookEdit" => path("notebook_path"),
            "Grep" | "Glob" => field("pattern").map(|pattern| match path("path") {
                Some(within) => format!("{pattern} in {within}"),
                None => pattern.to_owned(),
            }),
            "WebFetch" => field("url").map(str::to_owned),
            "WebSearch" | "ToolSearch" => field("query").map(str::to_owned),
            _ if AGENT_TOOLS.contains(&name) => field("description").map(str::to_owned),
            _ => scalars_of(input),
        }
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
                let windowed = self
                    .model
                    .clone()
                    .filter(|session| is_window_of(session, &model));
                // A main-agent message naming the family of the session's
                // model is billed under the session's id, which carries the
                // window, so its tokens are filed there. Filed under the
                // family, they would be owed for until a cost under that id
                // settled them, and none ever does: the turn's cost is
                // reported under the billed id, and the shell would price the
                // same tokens a second time on top of it.
                let billed = match (stream.as_deref(), windowed) {
                    (None, Some(session)) => session,
                    (None, None) => {
                        self.set_model(model.clone(), out);
                        model
                    }
                    (Some(agent), _) => {
                        self.agent_model(agent, model.clone(), out);
                        model
                    }
                };
                self.in_flight.insert(stream, billed);
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
                    // `result` does, for the session so far, and settles this
                    // record along with it.
                    cost_usd: None,
                    cost_basis: None,
                    settles_model: false,
                }));
                if stream.is_none() {
                    self.report_context(usage.last_prompt(), out);
                }
            }

            // A sub-agent's text is folded in when the message is complete
            // rather than as it streams: two agents streaming at once would
            // write through each other's paragraphs, and the finished message,
            // which names its agent, carries the same words.
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
        let call_id = body.tool_use_id.unwrap_or_default();
        // The request names the agent too, but by the CLI's own id for it,
        // which nothing else on the stream uses; the call it gates names the
        // agent the way its calls and messages do.
        let agent = self
            .tool_calls
            .get(&call_id)
            .and_then(|call| call.agent.clone());
        let id = ToolCallId::new(call_id);
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
            agent,
        });
    }

    fn result(&mut self, outcome: wire::Outcome, out: &mut Vec<Event>) {
        self.reconcile_turn(outcome.usage.as_ref(), out);
        self.report_billing(Some(&outcome.model_usage), out);
        self.report_cost(&outcome, out);
        self.learn_windows(&outcome.model_usage, out);

        // The closing line lists every call the turn refused, including the
        // ones already reported as they happened. Anything left is a refusal
        // the CLI did not announce — recorded here, late, rather than lost.
        for denial in outcome.permission_denials {
            let id = denial.tool_use_id.unwrap_or_default();
            if self.denied.insert(id.clone(), ()).is_some() {
                continue;
            }
            let agent = self
                .tool_calls
                .get(&id)
                .and_then(|call| call.agent.clone())
                .or_else(|| self.ended_agents.get(&id).cloned());
            let id = ToolCallId::new(id);
            out.push(Event::PermissionRequest {
                id: id.clone(),
                tool: denial.tool_name.unwrap_or_default(),
                input: denial.tool_input.as_ref().map(render).unwrap_or_default(),
                target: denial.tool_input.as_ref().and_then(target_of),
                agent,
            });
            out.push(Event::PermissionResponse {
                id,
                decision: PermissionDecision::Deny,
                message: None,
            });
        }
        self.ended_agents.clear();

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

        // Last, after everything the line reports: the shell stops showing
        // the session as working when it reads this, and a figure that came
        // after it would land in a turn the operator saw end.
        out.push(Event::TurnEnded);
    }

    /// Reports the prompt of a request the main agent made, against the
    /// window the CLI last reported for the model the session is on.
    ///
    /// Keyed by the session's model rather than the message's: the messages of
    /// a session with its 1M window selected name the family, and the window
    /// belongs to the id with the suffix.
    fn report_context(&mut self, tokens: u64, out: &mut Vec<Event>) {
        let model = self.model.clone().unwrap_or_default();
        let context = Context {
            tokens,
            window: self.windows.get(&model).copied(),
            model,
        };
        self.context = Some(context.clone());
        out.push(Event::Context(context));
    }

    /// Keeps the window each model's `modelUsage` entry reports, and restates
    /// the last request against it where that changes the session's.
    ///
    /// The window arrives with the first `result` and the request it sizes
    /// went before it, so without the restatement the first turn's meter would
    /// wait for the second turn's first message to learn how big the window is.
    fn learn_windows(
        &mut self,
        model_usage: &BTreeMap<String, wire::ModelUsage>,
        out: &mut Vec<Event>,
    ) {
        for (model, usage) in model_usage {
            if let Some(window) = usage.context_window {
                self.windows.insert(model.clone(), window);
            }
        }
        let Some(last) = &self.context else { return };
        let window = self.windows.get(&last.model).copied();
        if window != last.window {
            let tokens = last.tokens;
            self.report_context(tokens, out);
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

    /// Reports how the session is billed, where that is known and has
    /// changed: what the profile said, or else what the credential and the
    /// providers that served the turn's models say.
    fn report_billing(
        &mut self,
        served: Option<&BTreeMap<String, wire::ModelUsage>>,
        out: &mut Vec<Event>,
    ) {
        let billing = self.billed_as.or_else(|| {
            billing_of(
                self.api_key_source.as_deref(),
                served
                    .into_iter()
                    .flat_map(|served| served.values())
                    .filter_map(|usage| usage.provider.as_deref()),
            )
        });
        if let Some(billing) = billing
            && self.billing != Some(billing)
        {
            self.billing = Some(billing);
            out.push(Event::Billing { billing });
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
        let moved = self.file_messages_under_their_bill(&outcome.model_usage);

        let mut costs = 0.0;
        let mut settled = Vec::new();
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
            if spent > 0.0 {
                settled.push(model.clone());
            }
            out.push(self.cost_record(model.clone(), tokens, spent));
        }
        for model in moved.into_iter().filter(|model| !settled.contains(model)) {
            out.push(covered_elsewhere(model));
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

    /// Moves what the messages reported under one id to the id the CLI billed
    /// them under, where `modelUsage` says the two are the same model, and
    /// returns the ids that gave anything up.
    ///
    /// A session on a model with its 1M-token window selected names the model
    /// as the family in every `message_start` — `claude-opus-5` — and bills it
    /// as `claude-opus-5[1m]`, naming the family in `canonicalModel`. Recorded
    /// live on Claude Code 2.1.278. Reconciled by id, the two have nothing in
    /// common, and every token the messages carried is reported again.
    ///
    /// The main agent's messages are filed under the session's id as they
    /// arrive, where `init` named it. What is left for this is a message the
    /// session's id does not account for: a sub-agent's, or one of a turn
    /// after `/model` moved the session onto the window before any `init` said
    /// so.
    ///
    /// Where the family is billed beside its window in the same `result`, a
    /// message's name cannot say which of the two it belongs to, so the bill
    /// does: an id that was filed more than it grew gives the difference to
    /// an id of the same model that grew more than was filed under it. Moving
    /// everything filed under the family instead would report the family's
    /// own growth again, and moving nothing would report the window's.
    fn file_messages_under_their_bill(
        &mut self,
        model_usage: &BTreeMap<String, wire::ModelUsage>,
    ) -> Vec<String> {
        let family_of = |(id, usage): (&String, &wire::ModelUsage)| {
            usage.canonical_model.clone().unwrap_or_else(|| id.clone())
        };
        let families: Vec<String> = model_usage.iter().map(family_of).collect();
        let mut moved = Vec::new();
        for family in families {
            let bills: BTreeMap<String, Counts> = model_usage
                .iter()
                .filter(|entry| family_of(*entry) == family)
                .map(|(id, usage)| (id.clone(), Counts::from(usage)))
                .collect();
            let mut spare: BTreeMap<String, Counts> = bills
                .iter()
                .map(|(id, bill)| (id.clone(), bill.beyond(self.filed(id))))
                .collect();
            spare
                .entry(family.clone())
                .or_insert_with(|| self.filed(&family));
            for (target, bill) in &bills {
                let mut short = self.filed(target).beyond(*bill);
                for (source, left) in &mut spare {
                    let take = left.least(short);
                    if source == target || take.is_empty() {
                        continue;
                    }
                    *left = take.beyond(*left);
                    short = take.beyond(short);
                    self.shift(source, target, take);
                    if !moved.contains(source) {
                        moved.push(source.clone());
                    }
                }
            }
        }
        moved
    }

    /// The tokens reported under `model` so far this session.
    fn filed(&self, model: &str) -> Counts {
        self.reported
            .get(model)
            .map(|seen| seen.tokens)
            .unwrap_or_default()
    }

    /// Moves `tokens` from what was reported under one id to another's.
    fn shift(&mut self, from: &str, to: &str, tokens: Counts) {
        let source = self.reported.entry(from.to_owned()).or_default();
        source.tokens = tokens.beyond(source.tokens);
        self.reported
            .entry(to.to_owned())
            .or_default()
            .tokens
            .add(tokens);
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
            // `spent` is what the CLI has billed under this model less what it
            // had already billed, and the tokens above are those no message
            // reported. Between them the two cover every record emitted under
            // the model so far, so this figure settles them.
            settles_model: true,
        })
    }
}

/// A record settling what went out under `model` that the CLI billed under
/// another id of the same model, whose cost covers it.
///
/// The cost is the CLI's own: nothing more was billed under `model`, and the
/// money for these tokens is in the other id's record. Left unsettled, they
/// would be priced by the shell on top of that money.
fn covered_elsewhere(model: String) -> Event {
    Event::Usage(Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: 0,
        reasoning: 0,
        model,
        cost_usd: Some(0.0),
        cost_basis: Some(CostBasis::ApiEquivalent),
        settles_model: true,
    })
}

/// How a session is billed, from where its credential came from and the
/// providers that served its models. `None` where the two do not settle it.
///
/// An API key is billed per request whoever serves it. Without one, a model
/// served by anything but Anthropic's own API is a cloud or a gateway account,
/// billed per request too. What is left — no key, Anthropic's API — is a
/// claude.ai login, which is a plan; but only where the CLI said there was no
/// key, because the same provider behind a credential nobody named could be a
/// bearer token billed like a key.
///
/// The spellings are the CLI's own, from the schema of Claude Code 2.1.282.
fn billing_of<'a>(
    api_key_source: Option<&str>,
    providers: impl IntoIterator<Item = &'a str>,
) -> Option<Billing> {
    if api_key_source.is_some_and(|source| source != "none") {
        return Some(Billing::Metered);
    }
    let mut first_party = false;
    for provider in providers {
        match provider {
            "firstParty" => first_party = true,
            _ => return Some(Billing::Metered),
        }
    }
    (first_party && api_key_source == Some("none")).then_some(Billing::Plan)
}

/// Whether `session` is `model` with a context window selected, as in
/// `claude-opus-5[1m]` over `claude-opus-5`.
///
/// The CLI's `init` names the session by the first and its messages by the
/// second, so a message naming the family is not the session moving model.
fn is_window_of(session: &str, model: &str) -> bool {
    session
        .strip_prefix(model)
        .is_some_and(|window| window.starts_with('[') && window.ends_with(']'))
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

/// The entry for a shape this bridge has not read: visible, so that a shape a
/// new release added cannot pass unnoticed, and a notice, because the CLI did
/// not say that anything failed.
pub(crate) fn unread(message: String) -> Event {
    Event::Notice { message }
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

/// The CLI's tool that runs a shell command, which is the one call that has
/// an exit status.
const SHELL_TOOL: &str = "Bash";

/// How the CLI opens a failed shell command's result.
const EXIT_CODE_PREFIX: &str = "Exit code ";

/// How the CLI opens a result it saved to a file for being too large, and
/// carries only the first part of.
const PERSISTED_PREFIX: &str = "<persisted-output>";

/// How the CLI ends the line it puts in place of the lines it cut out of the
/// middle of a long result: `... (1033 lines truncated)`.
const TRUNCATED_SUFFIX: &str = " lines truncated)";

/// How the CLI ends the line it puts in place of the characters it cut out of
/// the middle of a long failure: `... [20014 characters truncated] ...`. It
/// cuts a failed command's output to about 10 KB this way, mid-line at both
/// ends, and saves none of it to a file.
const CHARACTERS_TRUNCATED_SUFFIX: &str = " characters truncated] ...";

/// How the CLI opens the result of a `Write` that made a file that was not
/// there, as against one that replaced a file that was.
const CREATED_PREFIX: &str = "File created successfully at:";

/// The status a shell command exited with, where the CLI said.
///
/// The CLI writes the status only into a failure, as the result's first line:
/// in 18,756 shell results across 336 of its own transcripts on the machine
/// this was written on, every failed command that ran began `Exit code <n>`
/// and no successful one did. A success it reports with no status at all, and
/// it reports three things as successes that did not exit zero or did not
/// exit yet: an interrupted command, one moved to the background, and one
/// whose non-zero status it read as a success of its own accord — the 283
/// results that carried a `returnCodeInterpretation`, all of them `grep`
/// finding nothing or `cmp` finding a difference. A success with none of
/// those is one the CLI let through because it exited zero. A success with
/// no report beside it at all is not read as anything.
fn exit_code(
    name: &str,
    outcome: ToolOutcome,
    output: &str,
    reported: Option<&serde_json::Value>,
) -> Option<i32> {
    if name != SHELL_TOOL {
        return None;
    }
    match outcome {
        ToolOutcome::Failed => reported_status(output),
        ToolOutcome::Ok => {
            let report: wire::ShellReport = serde_json::from_value(reported?.clone()).ok()?;
            let finished = !report.interrupted
                && report.background_task_id.is_none()
                && report.return_code_interpretation.is_none();
            finished.then_some(0)
        }
        ToolOutcome::Denied => None,
    }
}

/// What a shell call that ran `cargo test` reported, where it was one: the
/// counts, where its output held the whole run, the status it exited with,
/// whether it is known to have failed after its tests started and the tests
/// each failing binary named, where its list was left whole.
///
/// A refused call ran nothing and is not a test run. The counts are read
/// only from the whole of the output, which is what [`whole_output`] finds.
/// Nothing is read for a run whose status is not known, because its counts
/// could not be taken whatever they said. Whether it failed is read from
/// whatever the CLI handed over, whole or not: a failed command's output past
/// about 30,000 characters loses its end before it is cut from the middle,
/// and the start of a run is all that says its tests ran. A shorter one keeps
/// its end, and with it the lists of what failed that the cut left.
fn test_run(
    name: &str,
    arguments: &serde_json::Value,
    outcome: ToolOutcome,
    output: &str,
    reported: Option<&serde_json::Value>,
    exit_code: Option<i32>,
    read_spilled: Option<ReadSpilled>,
) -> Option<TestRunRecord> {
    let ran = match outcome {
        ToolOutcome::Ok | ToolOutcome::Failed => name == SHELL_TOOL,
        ToolOutcome::Denied => false,
    };
    let command = string_at(arguments, "command")?;
    if !ran || !test_run::is_test_run(command) {
        return None;
    }
    let whole = exit_code.and_then(|_| whole_output(output, reported, read_spilled));
    Some(TestRunRecord::read(
        command,
        output,
        whole.as_deref(),
        exit_code,
    ))
}

/// The whole of what a shell command printed, where it can be had.
///
/// That is the result itself where the CLI handed it over whole, and the file
/// it saved it to where the output was too large to hand over — read with
/// `read_spilled`, never from the preview beside it. `None` where the CLI cut
/// it in the middle — a `... (<n> lines truncated)` line where the lines were,
/// or `... [<n> characters truncated] ...` where the characters were — where
/// it interrupted the command, and where a saved output cannot be read whole.
/// A cut can fall between two test binaries and leave every remaining block
/// whole, so the shape of what is left cannot be trusted to show it.
fn whole_output<'a>(
    output: &'a str,
    reported: Option<&serde_json::Value>,
    read_spilled: Option<ReadSpilled>,
) -> Option<Cow<'a, str>> {
    let report = reported
        .and_then(|report| serde_json::from_value::<wire::ShellReport>(report.clone()).ok());
    if let Some(report) = report {
        if report.interrupted {
            return None;
        }
        if let Some(path) = report.persisted_output_path {
            let read = read_spilled?;
            return read(Path::new(&path), report.persisted_output_size?).map(Cow::Owned);
        }
    }
    let marked_cut = output.lines().any(|line| {
        let line = line.trim();
        line.starts_with(PERSISTED_PREFIX)
            || (line.starts_with("... (") && line.ends_with(TRUNCATED_SUFFIX))
            || (line.starts_with("... [") && line.ends_with(CHARACTERS_TRUNCATED_SUFFIX))
    });
    (!marked_cut).then_some(Cow::Borrowed(output))
}

/// The status in a failed command's `Exit code <n>` line, where it opens the
/// result.
fn reported_status(output: &str) -> Option<i32> {
    output
        .lines()
        .next()?
        .strip_prefix(EXIT_CODE_PREFIX)?
        .trim()
        .parse()
        .ok()
}

/// Why a call did not succeed, as the CLI told the model, without the
/// `<tool_use_error>` markup it wraps its own refusals in and without the
/// status line that [`exit_code`] has already taken.
fn failure_reason(output: &str, exit_code: Option<i32>) -> Option<String> {
    let text = output.trim();
    let text = text
        .strip_prefix("<tool_use_error>")
        .and_then(|inner| inner.strip_suffix("</tool_use_error>"))
        .unwrap_or(text);
    let text = match exit_code {
        Some(_) => text.split_once('\n').map_or("", |(_, rest)| rest),
        None => text,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The report beside a file tool's result, where it is about `path`.
///
/// The report rides on the line rather than on the result block, so one that
/// names another file is not this call's and is not read as it.
fn file_report(reported: Option<&serde_json::Value>, path: &str) -> Option<wire::FileReport> {
    let report: wire::FileReport = serde_json::from_value(reported?.clone()).ok()?;
    (report.file_path.as_deref() == Some(path)).then_some(report)
}

/// The hunks the CLI reported for a change to `path`, all of them or none.
///
/// One hunk that does not add up to its header drops the lot: a diff with
/// one of its hunks missing reads as a change that did not happen.
fn reported_hunks(reported: Option<&serde_json::Value>, path: &str) -> Vec<Hunk> {
    let Some(report) = file_report(reported, path) else {
        return Vec::new();
    };
    report
        .structured_patch
        .into_iter()
        .map(hunk_of)
        .collect::<Option<Vec<Hunk>>>()
        .unwrap_or_default()
}

/// A created file as one hunk, from what the CLI reported it created.
fn created_hunk(reported: Option<&serde_json::Value>, path: &str) -> Vec<Hunk> {
    file_report(reported, path)
        .filter(|report| report.kind.as_deref() == Some("create"))
        .and_then(|report| Hunk::created(report.content.as_deref()?))
        .into_iter()
        .collect()
}

/// One hunk of the CLI's patch, or `None` where a line has no side marker or
/// the lines do not add up to the header.
///
/// `\ No newline at end of file` is a note about the line above it, and the
/// header does not count it, so it is passed over rather than drawn.
fn hunk_of(patch: wire::PatchHunk) -> Option<Hunk> {
    let mut lines = Vec::with_capacity(patch.lines.len());
    for line in patch.lines {
        let mut chars = line.chars();
        let side = chars.next()?;
        let text = chars.as_str().to_owned();
        lines.push(match side {
            ' ' => Line::Context(text),
            '-' => Line::Removed(text),
            '+' => Line::Added(text),
            '\\' => continue,
            _ => return None,
        });
    }
    Hunk::checked(
        patch.old_start,
        patch.old_lines,
        patch.new_start,
        patch.new_lines,
        lines,
    )
}

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

/// The first line of a shell command, marked as cut where there is more.
fn first_line(command: &str) -> String {
    let mut lines = command.trim().lines();
    let first = lines.next().unwrap_or_default().trim_end();
    match lines.next() {
        Some(_) => format!("{first} …"),
        None => first.to_owned(),
    }
}

/// Arguments of a tool nothing here knows, as `key: value` pairs: strings,
/// numbers and booleans as they are, a list as its length, an object as `{…}`.
///
/// Text comes first, then numbers and flags, then the shapes: the line is cut
/// at the pane's edge, and a query or a name says more about a call than its
/// page size does. Within each, the keys keep the parser's alphabetical order.
fn scalars_of(input: &serde_json::Value) -> Option<String> {
    let serde_json::Value::Object(fields) = input else {
        return None;
    };
    let mut pairs: Vec<(u8, String)> = fields
        .iter()
        .map(|(key, value)| {
            let (rank, value) = match value {
                serde_json::Value::String(text) => (0, first_line(text)),
                serde_json::Value::Array(items) => (2, format!("[{}]", items.len())),
                serde_json::Value::Object(_) => (2, "{…}".to_owned()),
                other => (1, other.to_string()),
            };
            (rank, format!("{key}: {value}"))
        })
        .collect();
    pairs.sort_by_key(|(rank, _)| *rank);
    let pairs: Vec<String> = pairs.into_iter().map(|(_, pair)| pair).collect();
    (!pairs.is_empty()).then(|| pairs.join(", "))
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

/// The first line of `text` that says anything, without the marks that make
/// it a markdown heading, or `None` where no line does.
fn opening_line(text: &str) -> Option<String> {
    text.lines()
        .map(|line| line.trim().trim_start_matches('#').trim())
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

/// The count a background task's `usage` carries, where it carries one.
fn total_tokens(usage: Option<&wire::TaskUsage>) -> Option<u64> {
    usage.and_then(|usage| usage.total_tokens)
}

/// What a sub-agent call was spawned to do, as the Activity pane names it:
/// the kind of agent it asked for, where it named one, and what it was for.
fn label_of(input: &serde_json::Value) -> Option<String> {
    let field = |key| input.get(key).and_then(serde_json::Value::as_str);
    match (field("subagent_type"), field("description")) {
        (Some(kind), Some(description)) => Some(format!("{kind}: {description}")),
        (Some(kind), None) => Some(kind.to_owned()),
        (None, Some(description)) => Some(description.to_owned()),
        (None, None) => None,
    }
}

/// The CLI's slash commands, as the event that replaces the list before them.
fn listed_as_offered(commands: Vec<wire::Command>, unavailable: &[String]) -> Event {
    Event::Commands {
        commands: commands
            .into_iter()
            .filter(|command| !unavailable.contains(&command.name))
            .map(|command| SlashCommand {
                name: command.name,
                description: command.description,
                argument_hint: command.argument_hint.filter(|hint| !hint.trim().is_empty()),
            })
            .collect(),
    }
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
    use std::sync::Mutex;

    use niobe_core::test_run::TestCounts;

    use super::*;

    fn translator() -> Translator {
        let mut translator = Translator::new("max");
        translator
            .line(r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5"}"#);
        translator
    }

    fn billing(events: &[Event]) -> Vec<Billing> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Billing { billing } => Some(*billing),
                _ => None,
            })
            .collect()
    }

    /// A turn's closing report, with one model served by `provider`.
    fn served_by(provider: &str) -> String {
        format!(
            r#"{{"type":"result","subtype":"success","modelUsage":{{"opus-5":{{"inputTokens":1,"outputTokens":1,"costUSD":0.01,"provider":"{provider}"}}}}}}"#
        )
    }

    #[test]
    fn a_session_on_an_api_key_is_metered_from_its_first_line() {
        for source in ["ANTHROPIC_API_KEY", "apiKeyHelper", "/login managed key"] {
            let mut translator = Translator::new("work");
            let events = translator.line(&format!(
                r#"{{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5","apiKeySource":"{source}"}}"#
            ));
            assert_eq!(billing(&events), [Billing::Metered], "{source}");
        }
    }

    #[test]
    fn a_login_on_the_first_party_api_is_a_plan_once_a_turn_says_who_served_it() {
        let mut translator = Translator::new("max");
        let opened = translator.line(
            r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5","apiKeySource":"none"}"#,
        );
        // No key names a login, a bearer token and a cloud provider alike, so
        // the first line says nothing yet.
        assert_eq!(billing(&opened), []);

        assert_eq!(
            billing(&translator.line(&served_by("firstParty"))),
            [Billing::Plan]
        );
        // Said once: the next turn repeats what the session already shows.
        assert_eq!(billing(&translator.line(&served_by("firstParty"))), []);
    }

    #[test]
    fn a_model_served_by_a_cloud_provider_or_a_gateway_is_metered() {
        for provider in ["bedrock", "vertex", "foundry", "gateway"] {
            let mut translator = Translator::new("work");
            translator.line(
                r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5","apiKeySource":"none"}"#,
            );
            assert_eq!(
                billing(&translator.line(&served_by(provider))),
                [Billing::Metered],
                "{provider}"
            );
        }
    }

    /// A CLI that does not say where its credential came from could be on a
    /// plan or on a bearer token billed by the request, and the provider alone
    /// cannot tell the two apart.
    #[test]
    fn the_first_party_api_without_a_credential_source_is_left_unsaid() {
        let mut translator = translator();
        assert_eq!(billing(&translator.line(&served_by("firstParty"))), []);
    }

    /// The operator knows the contract behind a login; the stream only shows
    /// how the requests went. A seat billed by use looks like a plan in it.
    #[test]
    fn a_profile_that_names_its_billing_is_taken_at_its_word() {
        let mut translator = Translator::new("company").billed_as(Billing::Metered);
        let opened = translator.line(
            r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5","apiKeySource":"none"}"#,
        );
        assert_eq!(billing(&opened), [Billing::Metered]);
        assert_eq!(billing(&translator.line(&served_by("firstParty"))), []);
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
    fn a_sub_agents_streamed_messages_name_its_model_once_until_it_changes() {
        let mut translator = translator();
        translator.line(&agent_call("toolu_task", "Agent"));
        let start = |model: &str| {
            format!(
                r#"{{"type":"stream_event","parent_tool_use_id":"toolu_task","event":{{"type":"message_start","message":{{"model":"{model}"}}}}}}"#
            )
        };

        let models: Vec<Event> = ["haiku-4-5", "haiku-4-5", "sonnet-5"]
            .iter()
            .flat_map(|model| translator.line(&start(model)))
            .collect();

        let named: Vec<Option<&str>> = models
            .iter()
            .map(|event| match event {
                Event::AgentProgress { model, .. } => model.as_deref(),
                other => panic!("only the agent's model is reported: {other:?}"),
            })
            .collect();
        assert_eq!(named, [Some("haiku-4-5"), Some("sonnet-5")]);
    }

    #[test]
    fn progress_on_a_task_that_is_not_a_running_sub_agent_says_nothing() {
        let mut translator = translator();
        let progress = r#"{"type":"system","subtype":"task_progress","tool_use_id":"toolu_bash","description":"Running build","usage":{"total_tokens":10}}"#;

        assert!(translator.line(progress).is_empty());
    }

    #[test]
    fn a_failed_sub_agent_says_why_before_it_ends() {
        let mut translator = translator();
        translator.line(&agent_call("toolu_a", "Agent"));
        translator.line(&launched("toolu_a"));

        let events = translator.line(
            r##"{"type":"system","subtype":"task_notification","tool_use_id":"toolu_a","status":"failed","summary":"\n## Notion 404, gave up after 2 retries\n\nThe page was moved.","usage":{"total_tokens":4100}}"##,
        );

        assert_eq!(
            events,
            [
                Event::AgentProgress {
                    id: AgentId::new("toolu_a"),
                    model: None,
                    context_tokens: Some(4100),
                    latest: Some("Notion 404, gave up after 2 retries".to_owned()),
                },
                Event::AgentExit {
                    id: AgentId::new("toolu_a"),
                    outcome: AgentOutcome::Failed,
                },
            ]
        );
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
    fn a_family_billed_in_its_own_right_keeps_its_own_messages() {
        let mut translator = translator();
        translator.line(
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"opus-5"}}}"#,
        );
        translator.line(
            r#"{"type":"stream_event","event":{"type":"message_delta","usage":{"input_tokens":5,"output_tokens":7}}}"#,
        );

        // Both ids are on the bill, so the message belongs to the one it named,
        // and the windowed id's tokens are its own.
        let events = translator.line(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":5,"output_tokens":7},"modelUsage":{"opus-5":{"inputTokens":5,"outputTokens":7,"costUSD":0.1,"canonicalModel":"opus-5"},"opus-5[1m]":{"inputTokens":2,"outputTokens":3,"costUSD":0.05,"canonicalModel":"opus-5"}},"total_cost_usd":0.15}"#,
        );

        let billed: Vec<(&str, u64, u64)> = events
            .iter()
            .filter_map(|event| match event {
                Event::Usage(usage) => Some((usage.model.as_str(), usage.input, usage.output)),
                _ => None,
            })
            .collect();
        assert_eq!(billed, [("opus-5", 0, 0), ("opus-5[1m]", 2, 3)]);
    }

    /// The session is on the id with the window and its messages name the
    /// family; each message's tokens are filed under the id the turn's cost
    /// will be reported under, so that cost settles them.
    #[test]
    fn a_message_naming_the_family_of_the_sessions_window_is_filed_under_the_session() {
        let mut translator = Translator::new("max");
        translator
            .line(r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5[1m]"}"#);
        translator.line(
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"opus-5"}}}"#,
        );

        let events = translator.line(&delta(5, 7));

        let [Event::Usage(usage), Event::Context(_)] = events.as_slice() else {
            panic!("a usage record and the context: {events:?}");
        };
        assert_eq!(usage.model, "opus-5[1m]");
        assert_eq!((usage.input, usage.output), (5, 7));
    }

    /// A sub-agent's message is the agent's, on whatever the agent runs on;
    /// the session's window says nothing about which id bills it. Where the
    /// CLI bills it under the windowed id alone, its tokens are counted once.
    #[test]
    fn a_sub_agents_message_naming_the_family_keeps_its_name_and_is_counted_once() {
        let mut translator = Translator::new("max");
        translator
            .line(r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5[1m]"}"#);
        translator.line(
            r#"{"type":"stream_event","parent_tool_use_id":"toolu_task","event":{"type":"message_start","message":{"model":"opus-5"}}}"#,
        );
        let delta = translator.line(
            r#"{"type":"stream_event","parent_tool_use_id":"toolu_task","event":{"type":"message_delta","usage":{"input_tokens":5,"output_tokens":7}}}"#,
        );
        let [Event::Usage(usage)] = delta.as_slice() else {
            panic!("one usage record: {delta:?}");
        };
        assert_eq!(usage.model, "opus-5");

        let events = translator.line(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":5,"output_tokens":7},"modelUsage":{"opus-5[1m]":{"inputTokens":5,"outputTokens":7,"costUSD":0.1,"canonicalModel":"opus-5"}},"total_cost_usd":0.1}"#,
        );
        let billed: Vec<(&str, u64, u64)> = events
            .iter()
            .filter_map(|event| match event {
                Event::Usage(usage) => Some((usage.model.as_str(), usage.input, usage.output)),
                _ => None,
            })
            .collect();
        // The second record carries no tokens: it settles what went out under
        // the family, which the windowed id's cost covers.
        assert_eq!(billed, [("opus-5[1m]", 0, 0), ("opus-5", 0, 0)]);
    }

    /// Folds `lines` through a fresh translator into the session state, the
    /// way the shell does, so that a test reads what the operator would see.
    fn folded(lines: &[&str]) -> niobe_core::session::Totals {
        let mut translator = Translator::new("max");
        let mut state = niobe_core::session::SessionState::new();
        for line in lines {
            for event in translator.line(line) {
                state.apply(&event);
            }
        }
        state.totals().clone()
    }

    const INIT_FAMILY: &str =
        r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5"}"#;
    const INIT_WINDOW: &str =
        r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5[1m]"}"#;
    const MAIN_START: &str =
        r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"opus-5"}}}"#;
    const AGENT_START: &str = r#"{"type":"stream_event","parent_tool_use_id":"toolu_task","event":{"type":"message_start","message":{"model":"opus-5"}}}"#;
    const AGENT_DELTA: &str = r#"{"type":"stream_event","parent_tool_use_id":"toolu_task","event":{"type":"message_delta","usage":{"input_tokens":50,"output_tokens":5}}}"#;
    const FIRST_TURN_ON_THE_FAMILY: &str = r#"{"type":"result","subtype":"success","usage":{"input_tokens":100,"output_tokens":10},"modelUsage":{"opus-5":{"inputTokens":100,"outputTokens":10,"costUSD":0.5,"canonicalModel":"opus-5"}},"total_cost_usd":0.5}"#;
    const SECOND_TURN_ON_THE_WINDOW: &str = r#"{"type":"result","subtype":"success","usage":{"input_tokens":40,"output_tokens":4},"modelUsage":{"opus-5":{"inputTokens":100,"outputTokens":10,"costUSD":0.5,"canonicalModel":"opus-5"},"opus-5[1m]":{"inputTokens":40,"outputTokens":4,"costUSD":0.3,"canonicalModel":"opus-5"}},"total_cost_usd":0.8}"#;
    const MAIN_ON_THE_WINDOW_AGENT_ON_THE_FAMILY: &str = r#"{"type":"result","subtype":"success","usage":{"input_tokens":150,"output_tokens":15},"modelUsage":{"opus-5[1m]":{"inputTokens":100,"outputTokens":10,"costUSD":0.6,"canonicalModel":"opus-5"},"opus-5":{"inputTokens":50,"outputTokens":5,"costUSD":0.3,"canonicalModel":"opus-5"}},"total_cost_usd":0.9}"#;

    fn assert_counted_once(totals: &niobe_core::session::Totals, input: u64, output: u64) {
        assert_eq!((totals.input, totals.output), (input, output));
        assert_eq!(totals.records_unsettled, 0, "owed: {:?}", totals.unsettled);
    }

    /// `/model` moves the session onto the window between two turns, and the
    /// second turn's message still names the family: the bill says the family
    /// did not grow and the windowed id did, so the message is the window's.
    #[test]
    fn moving_onto_the_window_mid_session_counts_the_turn_once() {
        let totals = folded(&[
            INIT_FAMILY,
            MAIN_START,
            &delta(100, 10),
            FIRST_TURN_ON_THE_FAMILY,
            MAIN_START,
            &delta(40, 4),
            SECOND_TURN_ON_THE_WINDOW,
        ]);

        assert_counted_once(&totals, 140, 14);
        assert!((totals.reported_cost_usd - 0.8).abs() < 1e-9);
    }

    /// The same move, where the CLI's `init` for the second turn names the
    /// windowed id.
    #[test]
    fn moving_onto_the_window_announced_by_init_counts_the_turn_once() {
        let totals = folded(&[
            INIT_FAMILY,
            MAIN_START,
            &delta(100, 10),
            FIRST_TURN_ON_THE_FAMILY,
            INIT_WINDOW,
            MAIN_START,
            &delta(40, 4),
            SECOND_TURN_ON_THE_WINDOW,
        ]);

        assert_counted_once(&totals, 140, 14);
    }

    /// The main agent on the window and a sub-agent billed under the family,
    /// both of whose messages name the family: each id's bill takes the
    /// messages that made it grow, and neither is counted twice or lost.
    #[test]
    fn a_sub_agent_billed_under_the_family_beside_a_session_on_the_window_counts_once() {
        let totals = folded(&[
            INIT_WINDOW,
            MAIN_START,
            &delta(100, 10),
            AGENT_START,
            AGENT_DELTA,
            MAIN_ON_THE_WINDOW_AGENT_ON_THE_FAMILY,
        ]);

        assert_counted_once(&totals, 150, 15);
    }

    /// The same session read back with no `init` to say which id the main
    /// agent runs on: every message names the family, and the bill alone says
    /// how they split.
    #[test]
    fn messages_all_naming_the_family_split_by_what_each_billed_id_grew() {
        let totals = folded(&[
            MAIN_START,
            &delta(100, 10),
            AGENT_START,
            AGENT_DELTA,
            MAIN_ON_THE_WINDOW_AGENT_ON_THE_FAMILY,
        ]);

        assert_counted_once(&totals, 150, 15);
    }

    /// A sub-agent's messages name the family and the CLI bills them under the
    /// windowed id alone: the cost under that id covers them, so nothing is
    /// left owed under the name they went out with.
    #[test]
    fn a_family_the_bill_does_not_name_is_settled_by_the_id_that_billed_it() {
        let totals = folded(&[
            INIT_WINDOW,
            AGENT_START,
            AGENT_DELTA,
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":50,"output_tokens":5},"modelUsage":{"opus-5[1m]":{"inputTokens":50,"outputTokens":5,"costUSD":0.3,"canonicalModel":"opus-5"}},"total_cost_usd":0.3}"#,
        ]);

        assert_counted_once(&totals, 50, 5);
    }

    fn contexts(events: &[Event]) -> Vec<niobe_core::event::Context> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Context(context) => Some(context.clone()),
                _ => None,
            })
            .collect()
    }

    /// Uncached input, cache reads and cache writes are the three parts of
    /// one prompt, and all of it went into the window.
    #[test]
    fn the_context_is_every_prompt_token_of_the_main_agents_last_request() {
        let mut translator = translator();
        let events = translator.line(
            r#"{"type":"stream_event","event":{"type":"message_delta","usage":{"input_tokens":2,"cache_read_input_tokens":10118,"cache_creation_input_tokens":10948,"output_tokens":3}}}"#,
        );
        assert_eq!(
            contexts(&events),
            [niobe_core::event::Context {
                tokens: 21_068,
                model: "opus-5".to_owned(),
                window: None,
            }]
        );
    }

    /// A message that took several requests sums them at the top level, and
    /// that sum is no prompt that was ever sent. The last request's is.
    #[test]
    fn a_message_of_several_requests_is_sized_by_its_last() {
        let mut translator = translator();
        let events = translator.line(
            r#"{"type":"stream_event","event":{"type":"message_delta","usage":{"input_tokens":10,"cache_read_input_tokens":3000,"cache_creation_input_tokens":500,"output_tokens":90,"iterations":[{"input_tokens":4,"cache_read_input_tokens":1000,"cache_creation_input_tokens":200,"output_tokens":40},{"input_tokens":6,"cache_read_input_tokens":2000,"cache_creation_input_tokens":300,"output_tokens":50}]}}}"#,
        );
        assert_eq!(
            contexts(&events)
                .iter()
                .map(|c| c.tokens)
                .collect::<Vec<_>>(),
            [2_306]
        );
    }

    /// A sub-agent's prompt fills a context of its own, not the session's.
    #[test]
    fn a_sub_agents_request_says_nothing_about_the_sessions_context() {
        let mut translator = translator();
        translator.line(
            r#"{"type":"stream_event","parent_tool_use_id":"toolu_task","event":{"type":"message_start","message":{"model":"haiku-4-5"}}}"#,
        );
        let events = translator.line(
            r#"{"type":"stream_event","parent_tool_use_id":"toolu_task","event":{"type":"message_delta","usage":{"input_tokens":5000,"output_tokens":7}}}"#,
        );
        assert_eq!(contexts(&events), []);
    }

    /// The window is keyed by the id the session runs under, which carries
    /// the `[1m]` its messages leave off.
    #[test]
    fn the_window_the_cli_reports_is_carried_from_the_turn_it_arrives_in() {
        let mut translator = Translator::new("max");
        translator.line(
            r#"{"type":"system","subtype":"init","session_id":"s-1","model":"claude-opus-5[1m]"}"#,
        );
        translator.line(
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"claude-opus-5"}}}"#,
        );
        let first = translator.line(
            r#"{"type":"stream_event","event":{"type":"message_delta","usage":{"input_tokens":2,"cache_read_input_tokens":98,"output_tokens":3}}}"#,
        );
        assert_eq!(
            contexts(&first),
            [niobe_core::event::Context {
                tokens: 100,
                model: "claude-opus-5[1m]".to_owned(),
                window: None,
            }]
        );

        // The first report of the window restates the last request with it,
        // before the turn ends, so the meter does not wait a turn for it.
        let result = before_the_end(translator.line(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":2,"cache_read_input_tokens":98,"output_tokens":3},"modelUsage":{"claude-opus-5[1m]":{"inputTokens":2,"outputTokens":3,"cacheReadInputTokens":98,"costUSD":0.01,"contextWindow":1000000,"canonicalModel":"claude-opus-5"}},"total_cost_usd":0.01}"#,
        ));
        assert_eq!(
            contexts(&result),
            [niobe_core::event::Context {
                tokens: 100,
                model: "claude-opus-5[1m]".to_owned(),
                window: Some(1_000_000),
            }]
        );

        // A report that says the same again adds nothing.
        translator.line(
            r#"{"type":"stream_event","event":{"type":"message_delta","usage":{"input_tokens":1,"cache_read_input_tokens":199,"output_tokens":3}}}"#,
        );
        let again = before_the_end(translator.line(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":1,"cache_read_input_tokens":199,"output_tokens":3},"modelUsage":{"claude-opus-5[1m]":{"inputTokens":3,"outputTokens":6,"cacheReadInputTokens":297,"costUSD":0.02,"contextWindow":1000000,"canonicalModel":"claude-opus-5"}},"total_cost_usd":0.02}"#,
        ));
        assert_eq!(contexts(&again), []);
    }

    /// A window reported for a model the session is not on is not the
    /// session's window.
    #[test]
    fn a_window_reported_for_another_model_is_not_the_sessions() {
        let mut translator = translator();
        translator.line(&delta(2, 3));
        let events = translator.line(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":2,"output_tokens":3},"modelUsage":{"opus-5":{"inputTokens":2,"outputTokens":3,"costUSD":0.01},"haiku-4-5":{"inputTokens":9,"costUSD":0.001,"contextWindow":200000}},"total_cost_usd":0.011}"#,
        );
        assert_eq!(contexts(&events), []);
    }

    #[test]
    fn a_window_is_a_bracketed_suffix_on_the_same_id() {
        assert!(is_window_of("claude-opus-5[1m]", "claude-opus-5"));
        assert!(!is_window_of("claude-opus-5", "claude-opus-5"));
        assert!(!is_window_of("claude-opus-5-1", "claude-opus-5"));
        assert!(!is_window_of("claude-opus-5[1m]", "claude-opus"));
    }

    #[test]
    fn a_pricing_basis_the_bridge_does_not_know_is_reported_and_still_not_called_measured() {
        let mut translator = translator();

        let events = before_the_end(translator.line(
            r#"{"type":"result","subtype":"success","modelUsage":{"opus-5":{"costUSD":0.5,"costBasis":"billed"}},"total_cost_usd":0.5}"#,
        ));

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

        let first = before_the_end(
            translator.line(r#"{"type":"result","subtype":"success","total_cost_usd":0.25}"#),
        );
        let second = before_the_end(
            translator.line(r#"{"type":"result","subtype":"success","total_cost_usd":0.40}"#),
        );

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

        let events = before_the_end(translator.line(
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"the provider refused"}"#,
        ));

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
    fn a_session_on_a_cli_release_nobody_recorded_says_so_once() {
        let mut translator = Translator::new("max");

        let events = translator.line(
            r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5","claude_code_version":"9.9.9"}"#,
        );

        let said = warnings(&events);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("9.9.9"), "{said:?}");

        // The CLI writes `init` again whenever the session moves model, and a
        // release it already reported has not become news.
        let again = translator.line(
            r#"{"type":"system","subtype":"init","session_id":"s-1","model":"haiku-4-5","claude_code_version":"9.9.9"}"#,
        );
        assert!(warnings(&again).is_empty(), "{again:?}");
    }

    #[test]
    fn a_session_on_a_recorded_cli_release_says_nothing_about_it() {
        let mut translator = Translator::new("max");

        let version = crate::conformance::RECORDED
            .last()
            .expect("a recorded release");
        let events = translator.line(&format!(
            r#"{{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5","claude_code_version":"{version}"}}"#
        ));

        assert!(warnings(&events).is_empty(), "{events:?}");
    }

    #[test]
    fn a_cli_that_does_not_say_which_release_it_is_is_taken_at_its_word() {
        let mut translator = Translator::new("max");

        // Every release recorded so far names itself on `init`. One that does
        // not is a release this bridge can say nothing about, and a warning
        // that named no version would tell the operator nothing they could
        // act on.
        let events = translator
            .line(r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5"}"#);

        assert!(warnings(&events).is_empty(), "{events:?}");
    }

    fn notices(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Notice { message } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_system_subtype_the_bridge_does_not_know_is_a_notice_and_not_an_error() {
        let mut translator = translator();

        let events = translator.line(r#"{"type":"system","subtype":"weather","outlook":"fine"}"#);

        assert!(warnings(&events).is_empty(), "{events:?}");
        let said = notices(&events);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("`weather`"), "{said:?}");
    }

    #[test]
    fn a_message_type_the_bridge_does_not_know_is_a_notice_and_not_an_error() {
        let mut translator = translator();

        let events = translator.line(r#"{"type":"weather","outlook":"fine"}"#);

        assert!(warnings(&events).is_empty(), "{events:?}");
        let said = notices(&events);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("`weather`"), "{said:?}");
    }

    #[test]
    fn a_message_of_a_known_type_that_does_not_parse_is_still_an_error() {
        let mut translator = translator();

        let events = translator.line(r#"{"type":"system","subtype":7}"#);

        assert_eq!(warnings(&events).len(), 1, "{events:?}");
    }

    /// The CLI's bookkeeping for its background tasks, as Claude Code 2.1.278
    /// sent it for one sub-agent: each line is what the recording holds, cut
    /// to the keys that say what it is.
    const TASK_BOOKKEEPING: [&str; 3] = [
        r#"{"type":"system","subtype":"task_started","task_id":"a59795b7f983ac4a4","tool_use_id":"toolu_a","description":"Summarize catalog/cache.py","subagent_type":"quick-lookup","is_backgrounded":true,"task_type":"local_agent"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a59795b7f983ac4a4","task_type":"local_agent","description":"Summarize catalog/cache.py"}]}"#,
        r#"{"type":"system","subtype":"task_updated","task_id":"a59795b7f983ac4a4","patch":{"status":"completed","end_time":1789833985799}}"#,
    ];

    #[test]
    fn the_clis_bookkeeping_for_its_background_tasks_is_passed_over() {
        let mut translator = translator();
        translator.line(&agent_call("toolu_a", "Agent"));
        translator.line(&launched("toolu_a"));

        for line in TASK_BOOKKEEPING {
            let events = translator.line(line);

            assert!(events.is_empty(), "{line}: {events:?}");
        }
        assert_eq!(
            exits(&translator.line(&notified("toolu_a", "completed"))).len(),
            1,
            "the agent still ends at its notification, not at `task_updated`"
        );
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
        let result = before_the_end(translator.line(
            r#"{"type":"result","subtype":"success","permission_denials":[{"tool_name":"Bash","tool_use_id":"toolu_1"}]}"#,
        ));

        let [
            Event::PermissionRequest {
                id,
                tool,
                input,
                target,
                agent,
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
        assert_eq!(*agent, None, "the session's own call");
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
            before_the_end(translator.line(
                r#"{"type":"result","subtype":"success","permission_denials":[{"tool_name":"Bash","tool_use_id":"toolu_1"}]}"#
            ))
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
    fn a_command_the_cli_said_it_will_not_run_stays_out_of_a_later_list() {
        let mut translator = translator();
        translator.line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-1","response":{"commands":[{"name":"fast"}],"fast_mode_disabled_reason":"sdk_opt_in_required"}}}"#,
        );

        let events = translator.line(
            r#"{"type":"system","subtype":"commands_changed","commands":[{"name":"clear"},{"name":"fast"}]}"#,
        );
        let [Event::Commands { commands }] = events.as_slice() else {
            panic!("one list: {events:?}");
        };
        let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["clear"]);
    }

    #[test]
    fn a_refusal_the_cli_only_lists_at_the_end_is_still_recorded() {
        let mut translator = translator();

        let events = before_the_end(translator.line(
            r#"{"type":"result","subtype":"success","permission_denials":[{"tool_name":"Write","tool_use_id":"toolu_9","tool_input":{"file_path":"/etc/hosts"}}]}"#,
        ));

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

    /// The closing line lists a refusal after the call's result has come
    /// back, and the result is where the call is let go of; whose call it was
    /// outlives it until then.
    #[test]
    fn a_sub_agents_refusal_listed_only_at_the_end_is_still_that_agents() {
        let mut translator = translator();
        translator.line(
            r#"{"type":"assistant","parent_tool_use_id":"toolu_task","message":{"content":[{"type":"tool_use","id":"toolu_9","name":"Write","input":{"file_path":"/etc/hosts"}}]}}"#,
        );
        translator.line(
            r#"{"type":"user","parent_tool_use_id":"toolu_task","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_9","content":"refused","is_error":true}]}}"#,
        );

        let events = before_the_end(translator.line(
            r#"{"type":"result","subtype":"success","permission_denials":[{"tool_name":"Write","tool_use_id":"toolu_9","tool_input":{"file_path":"/etc/hosts"}}]}"#,
        ));

        let asked: Vec<Option<&str>> = events
            .iter()
            .filter_map(|event| match event {
                Event::PermissionRequest { agent, .. } => {
                    Some(agent.as_ref().map(|agent| agent.as_str()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(asked, [Some("toolu_task")], "{events:?}");
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
                    ..
                } => Some((path.clone(), *added, *removed)),
                _ => None,
            })
            .collect()
    }

    /// A tool result with what the CLI reported about the call beside it, as
    /// it sends one for every file tool.
    fn result_with(id: &str, content: &str, reported: &str) -> String {
        let content = serde_json::Value::String(content.to_owned());
        format!(
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"{id}","is_error":false,"content":{content}}}]}},"tool_use_result":{reported}}}"#
        )
    }

    /// The hunks every file change in the events carries, one list per change.
    fn hunks(events: &[Event]) -> Vec<Vec<Hunk>> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::FileChange { hunks, .. } => Some(hunks.clone()),
                _ => None,
            })
            .collect()
    }

    fn context(text: &str) -> Line {
        Line::Context(text.to_owned())
    }
    fn removed(text: &str) -> Line {
        Line::Removed(text.to_owned())
    }
    fn added(text: &str) -> Line {
        Line::Added(text.to_owned())
    }

    /// The shape recorded in `tests/fixtures/stdio-answers.jsonl` (Claude Code
    /// 2.1.278): the CLI's own diff of the file, under `structuredPatch`.
    #[test]
    fn an_edit_carries_the_hunks_the_cli_reported() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Edit",
            r#"{"file_path":"/repo/notes.txt","old_string":"beta","new_string":"gamma","replace_all":false}"#,
        ));

        let events = translator.line(&result_with(
            "t1",
            UPDATED,
            r#"{"filePath":"/repo/notes.txt","oldString":"beta","newString":"gamma","originalFile":"alpha\nbeta\n","structuredPatch":[{"oldStart":1,"oldLines":2,"newStart":1,"newLines":2,"lines":[" alpha","-beta","+gamma"]}],"userModified":false,"replaceAll":false}"#,
        ));

        assert_eq!(
            changes(&events),
            vec![("notes.txt".to_owned(), Some(1), Some(1))]
        );
        assert_eq!(
            hunks(&events),
            vec![vec![
                Hunk::checked(
                    1,
                    2,
                    1,
                    2,
                    vec![context("alpha"), removed("beta"), added("gamma")]
                )
                .expect("consistent")
            ]]
        );
    }

    #[test]
    fn an_edit_whose_result_reported_no_patch_carries_no_hunks() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Edit",
            r#"{"file_path":"/repo/notes.txt","old_string":"beta","new_string":"gamma"}"#,
        ));

        let events = translator.line(&result("t1", UPDATED, false));

        assert_eq!(hunks(&events), vec![Vec::<Hunk>::new()]);
    }

    /// A body that does not add up to its header was cut or misread, and one
    /// hunk dropped out of several would show a diff with a change missing.
    #[test]
    fn a_patch_any_hunk_of_which_disagrees_with_its_header_carries_no_hunks_at_all() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Edit",
            r#"{"file_path":"/repo/notes.txt","old_string":"b","new_string":"c","replace_all":true}"#,
        ));

        let events = translator.line(&result_with(
            "t1",
            UPDATED,
            r#"{"filePath":"/repo/notes.txt","structuredPatch":[{"oldStart":1,"oldLines":1,"newStart":1,"newLines":1,"lines":["-b","+c"]},{"oldStart":9,"oldLines":3,"newStart":9,"newLines":1,"lines":["-b","+c"]}],"replaceAll":true}"#,
        ));

        assert_eq!(hunks(&events), vec![Vec::<Hunk>::new()]);
        assert_eq!(changes(&events), vec![("notes.txt".to_owned(), None, None)]);
    }

    /// The report sits beside the line, not inside the result; one that names
    /// another file is about another call.
    #[test]
    fn a_patch_reported_for_another_file_is_not_taken_for_this_one() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Edit",
            r#"{"file_path":"/repo/notes.txt","old_string":"beta","new_string":"gamma"}"#,
        ));

        let events = translator.line(&result_with(
            "t1",
            UPDATED,
            r#"{"filePath":"/repo/other.txt","structuredPatch":[{"oldStart":1,"oldLines":1,"newStart":1,"newLines":1,"lines":["-beta","+gamma"]}]}"#,
        ));

        assert_eq!(hunks(&events), vec![Vec::<Hunk>::new()]);
    }

    /// The patch is of the whole file, so it holds every occurrence the call
    /// replaced — which is what makes a `replace_all` countable at all.
    #[test]
    fn a_replacement_made_everywhere_is_counted_from_the_hunks_the_cli_reported() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Edit",
            r#"{"file_path":"/repo/rep.txt","old_string":"foo","new_string":"bar","replace_all":true}"#,
        ));

        let events = translator.line(&result_with(
            "t1",
            "The file /repo/rep.txt has been updated. All occurrences were successfully replaced.",
            r#"{"filePath":"/repo/rep.txt","structuredPatch":[{"oldStart":1,"oldLines":2,"newStart":1,"newLines":2,"lines":["-foo","+bar"," x"]},{"oldStart":12,"oldLines":2,"newStart":12,"newLines":2,"lines":[" y","-foo","+bar"]}],"replaceAll":true}"#,
        ));

        assert_eq!(
            changes(&events),
            vec![("rep.txt".to_owned(), Some(2), Some(2))]
        );
        assert_eq!(hunks(&events)[0].len(), 2);
    }

    /// A created file's report carries no patch: the CLI diffs against a file
    /// that was not there, and says so with `type` rather than with hunks.
    #[test]
    fn a_created_file_is_one_hunk_of_the_lines_it_was_created_with() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Write",
            r#"{"file_path":"/repo/fresh.py","content":"def foo():\n    return None\n"}"#,
        ));

        let events = translator.line(&result_with(
            "t1",
            "File created successfully at: /repo/fresh.py",
            r#"{"type":"create","filePath":"/repo/fresh.py","content":"def foo():\n    return None\n","structuredPatch":[],"originalFile":null}"#,
        ));

        assert_eq!(
            changes(&events),
            vec![("fresh.py".to_owned(), Some(2), Some(0))]
        );
        assert_eq!(
            hunks(&events),
            vec![vec![
                Hunk::created("def foo():\n    return None\n").expect("two lines")
            ]]
        );
    }

    /// The call carries what the file became and never what it was; the
    /// report carries the diff between the two, so what the overwrite dropped
    /// is counted from that instead of left unstated.
    #[test]
    fn an_overwritten_file_is_counted_and_drawn_from_the_hunks_the_cli_reported() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Write",
            r#"{"file_path":"/repo/doomed.txt","content":"one\ntwo 2\nthree\n"}"#,
        ));

        let events = translator.line(&result_with(
            "t1",
            "The file /repo/doomed.txt has been updated successfully.",
            r#"{"type":"update","filePath":"/repo/doomed.txt","content":"one\ntwo 2\nthree\n","structuredPatch":[{"oldStart":1,"oldLines":4,"newStart":1,"newLines":3,"lines":[" one","-two","-gone","+two 2"," three"]}],"originalFile":"one\ntwo\ngone\nthree\n"}"#,
        ));

        assert_eq!(
            changes(&events),
            vec![("doomed.txt".to_owned(), Some(1), Some(2))]
        );
        assert_eq!(hunks(&events)[0].len(), 1);
    }

    /// `\ No newline at end of file` is a note about the line above it, not a
    /// line; the CLI's own header does not count it either.
    #[test]
    fn a_missing_newline_note_is_not_taken_for_a_line() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Edit",
            r#"{"file_path":"/repo/notes.txt","old_string":"beta","new_string":"gamma"}"#,
        ));

        let events = translator.line(&result_with(
            "t1",
            UPDATED,
            r#"{"filePath":"/repo/notes.txt","structuredPatch":[{"oldStart":1,"oldLines":1,"newStart":1,"newLines":1,"lines":["-beta","\\ No newline at end of file","+gamma","\\ No newline at end of file"]}]}"#,
        ));

        assert_eq!(
            hunks(&events),
            vec![vec![
                Hunk::checked(1, 1, 1, 1, vec![removed("beta"), added("gamma")])
                    .expect("consistent")
            ]]
        );
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

    /// A sub-agent call as Claude Code 2.1.278 writes it: the tool is `Agent`,
    /// and the call names the kind of agent it wants.
    fn agent_call(id: &str, tool: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"{id}","name":"{tool}","input":{{"subagent_type":"quick-lookup","description":"Summarize catalog/cache.py","prompt":"Read catalog/cache.py and summarize it."}}}}]}}}}"#
        )
    }

    /// The CLI's answer to a sub-agent call it ran in the background: the
    /// call returns at once, and `tool_use_result` says the agent was launched
    /// rather than that it finished.
    fn launched(id: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"{id}","content":[{{"type":"text","text":"Async agent launched successfully."}}]}}]}},"tool_use_result":{{"isAsync":true,"status":"async_launched","agentId":"a819f5cc82e486a11","description":"Summarize catalog/cache.py"}}}}"#
        )
    }

    /// The CLI's word that a background task stopped, in the shape its own
    /// schema gives `system`/`task_notification`.
    fn notified(id: &str, status: &str) -> String {
        format!(
            r#"{{"type":"system","subtype":"task_notification","task_id":"a819f5cc82e486a11","tool_use_id":"{id}","status":"{status}","output_file":"","summary":"Agent \"Summarize catalog/cache.py\" finished","usage":{{"total_tokens":6609,"tool_uses":1,"duration_ms":5506}}}}"#
        )
    }

    fn exits(events: &[Event]) -> Vec<(String, AgentOutcome)> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::AgentExit { id, outcome } => Some((id.as_str().to_owned(), *outcome)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_call_to_the_agent_tool_spawns_a_sub_agent_labelled_with_its_kind() {
        let mut translator = translator();

        let events = translator.line(&agent_call("toolu_a", "Agent"));

        let spawned: Vec<(&str, &str)> = events
            .iter()
            .filter_map(|event| match event {
                Event::AgentSpawn { id, label, .. } => Some((id.as_str(), label.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            spawned,
            [("toolu_a", "quick-lookup: Summarize catalog/cache.py")]
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::ToolCallStart { name, .. } if name == "Agent")),
            "the call itself is still in the transcript: {events:?}"
        );
    }

    #[test]
    fn the_tools_other_name_spawns_a_sub_agent_too() {
        let mut translator = translator();

        let events = translator.line(&agent_call("toolu_t", "Task"));

        assert!(
            events.iter().any(
                |event| matches!(event, Event::AgentSpawn { id, .. } if id.as_str() == "toolu_t")
            ),
            "{events:?}"
        );
    }

    #[test]
    fn a_tool_with_neither_name_is_an_ordinary_call() {
        let mut translator = translator();

        let events = translator.line(&agent_call("toolu_x", "TaskCreate"));

        assert!(
            events
                .iter()
                .all(|event| !matches!(event, Event::AgentSpawn { .. })),
            "{events:?}"
        );
    }

    #[test]
    fn a_sub_agent_launched_in_the_background_is_still_running_when_its_call_returns() {
        let mut translator = translator();
        translator.line(&agent_call("toolu_a", "Agent"));

        let events = translator.line(&launched("toolu_a"));

        assert!(exits(&events).is_empty(), "{events:?}");
        assert!(
            events.iter().any(|event| matches!(
                event,
                Event::ToolCallEnd {
                    outcome: ToolOutcome::Ok,
                    ..
                }
            )),
            "the launch itself is a call that ran: {events:?}"
        );
    }

    #[test]
    fn a_background_sub_agent_ends_when_the_cli_says_its_task_stopped() {
        for (status, outcome) in [
            ("completed", AgentOutcome::Completed),
            ("failed", AgentOutcome::Failed),
            ("stopped", AgentOutcome::Cancelled),
        ] {
            let mut translator = translator();
            translator.line(&agent_call("toolu_a", "Agent"));
            translator.line(&launched("toolu_a"));

            let events = translator.line(&notified("toolu_a", status));

            assert_eq!(
                exits(&events),
                [("toolu_a".to_owned(), outcome)],
                "{status}"
            );
            assert!(warnings(&events).is_empty(), "{status}: {events:?}");
        }
    }

    #[test]
    fn a_sub_agent_that_notifies_again_after_it_ended_is_not_ended_twice() {
        let mut translator = translator();
        translator.line(&agent_call("toolu_a", "Agent"));
        translator.line(&launched("toolu_a"));
        translator.line(&notified("toolu_a", "completed"));

        let events = translator.line(&notified("toolu_a", "completed"));

        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn a_sub_agent_whose_call_returned_its_answer_has_finished() {
        let mut translator = translator();
        translator.line(&agent_call("toolu_a", "Agent"));

        let events = translator.line(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_a","content":"The cache is a bounded dict."}]},"tool_use_result":{"status":"completed","prompt":"Read catalog/cache.py and summarize it."}}"#,
        );

        assert_eq!(
            exits(&events),
            [("toolu_a".to_owned(), AgentOutcome::Completed)]
        );
    }

    #[test]
    fn a_sub_agent_whose_launch_failed_has_failed() {
        let mut translator = translator();
        translator.line(&agent_call("toolu_a", "Agent"));

        let events = translator.line(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_a","content":"Agent type 'nope' not found","is_error":true}]}}"#,
        );

        assert_eq!(
            exits(&events),
            [("toolu_a".to_owned(), AgentOutcome::Failed)]
        );
    }

    #[test]
    fn a_task_notification_for_something_that_is_not_a_sub_agent_is_passed_over() {
        let mut translator = translator();

        let events = translator.line(&notified("toolu_bash", "completed"));

        assert!(events.is_empty(), "{events:?}");
    }

    /// What a result line reported before the turn ended, having checked that
    /// it did end, and last.
    fn before_the_end(mut events: Vec<Event>) -> Vec<Event> {
        assert_eq!(events.pop(), Some(Event::TurnEnded), "{events:?}");
        events
    }

    fn summary_of_call(name: &str, input: &str) -> Option<String> {
        let mut translator = translator().in_dir("/repo");
        translator
            .line(&call("t1", name, input))
            .into_iter()
            .find_map(|event| match event {
                Event::ToolCallStart { summary, .. } => Some(summary),
                _ => None,
            })
            .flatten()
    }

    #[test]
    fn a_call_is_summed_up_in_the_words_a_person_would_use_for_it() {
        let cases = [
            (
                "Bash",
                r#"{"command":"cargo test -p niobe-tui","description":"Run the tests"}"#,
                "cargo test -p niobe-tui",
            ),
            ("Bash", r#"{"command":"cd x\ncargo build"}"#, "cd x …"),
            (
                "Read",
                r#"{"file_path":"/repo/crates/ui.rs"}"#,
                "crates/ui.rs",
            ),
            (
                "Edit",
                r#"{"file_path":"/repo/AGENTS.md","old_string":"a","new_string":"b"}"#,
                "AGENTS.md",
            ),
            (
                "Write",
                r#"{"file_path":"/elsewhere/notes.txt","content":"x"}"#,
                "/elsewhere/notes.txt",
            ),
            (
                "Grep",
                r#"{"pattern":"fn draw","path":"/repo/crates"}"#,
                "fn draw in crates",
            ),
            ("Glob", r#"{"pattern":"**/*.rs"}"#, "**/*.rs"),
            (
                "WebFetch",
                r#"{"url":"https://example.com/a","prompt":"read it"}"#,
                "https://example.com/a",
            ),
            (
                "WebSearch",
                r#"{"query":"ratatui shadow"}"#,
                "ratatui shadow",
            ),
            (
                "Agent",
                r#"{"subagent_type":"Explore","description":"Find the loop","prompt":"…"}"#,
                "Find the loop",
            ),
            (
                "mcp__claude_ai_Notion__notion-search",
                r#"{"query":"Niobe milestones","page_size":10,"filters":{"a":1},"ids":["x","y"]}"#,
                "query: Niobe milestones, page_size: 10, filters: {…}, ids: [2]",
            ),
        ];
        for (name, input, expected) in cases {
            assert_eq!(
                summary_of_call(name, input).as_deref(),
                Some(expected),
                "{name} {input}"
            );
        }
    }

    #[test]
    fn a_call_with_no_arguments_worth_reading_has_no_summary() {
        assert_eq!(summary_of_call("mcp__x__list", "{}"), None);
    }

    #[test]
    fn a_finished_call_repeats_the_summary_it_started_with() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call("t1", "Read", r#"{"file_path":"/repo/notes.txt"}"#));

        let events = translator.line(&result("t1", "three lines", false));

        let summary = events.iter().find_map(|event| match event {
            Event::ToolCallEnd { summary, .. } => Some(summary.clone()),
            _ => None,
        });
        assert_eq!(summary, Some(Some("notes.txt".to_owned())), "{events:?}");
    }

    #[test]
    fn the_result_line_ends_the_turn_after_everything_it_reports() {
        let mut translator = translator();
        for line in [
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":2,"output_tokens":10}}"#,
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"the provider refused"}"#,
        ] {
            let events = translator.line(line);
            assert_eq!(events.last(), Some(&Event::TurnEnded), "{events:?}");
            assert_eq!(
                events.iter().filter(|e| **e == Event::TurnEnded).count(),
                1,
                "{events:?}"
            );
        }
    }

    /// What the one call's end in `events` says about how it ended: its exit
    /// status and its reason.
    fn ending(events: &[Event]) -> (Option<i32>, Option<String>) {
        let ends: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::ToolCallEnd {
                    exit_code, error, ..
                } => Some((*exit_code, error.clone())),
                _ => None,
            })
            .collect();
        let [ending] = ends.as_slice() else {
            panic!("one call ended: {events:?}");
        };
        ending.clone()
    }

    /// The report the CLI writes beside a shell command's result, with the
    /// keys it carried in every transcript recorded on this machine.
    fn shell_report(extra: &str) -> String {
        format!(
            r#"{{"stdout":"","stderr":"","interrupted":false,"isImage":false,"noOutputExpected":false{extra}}}"#
        )
    }

    fn shell_call(translator: &mut Translator) {
        translator.line(&call("t1", "Bash", r#"{"command":"cargo test"}"#));
    }

    /// A `cargo test` run of one test binary, whole, as the shell tool
    /// captures it with its standard error in it.
    const TEST_RUN: &str =
        "    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.01s
     Running unittests src/lib.rs (target/debug/deps/demo-760e00b68511d171)

running 3 tests
test tests::adds ... ok
test tests::slow ... ignored
test tests::wrong ... FAILED

test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `--lib`";

    /// Whether each test run the events report is known to have failed.
    fn failed_runs(events: &[Event]) -> Vec<bool> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::TestRun { failed, .. } => Some(*failed),
                _ => None,
            })
            .collect()
    }

    /// Every test run the events report, as its counts and exit status.
    fn test_runs(events: &[Event]) -> Vec<(Option<TestCounts>, Option<i32>)> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::TestRun {
                    counts, exit_code, ..
                } => Some((*counts, *exit_code)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_test_run_reports_its_own_counts_straight_after_the_call_ends() {
        let mut translator = translator();
        shell_call(&mut translator);

        let events = translator.line(&result("t1", &format!("Exit code 101\n{TEST_RUN}"), true));

        let counts = TestCounts {
            passed: 1,
            failed: 1,
            ignored: 1,
            suites: 1,
        };
        assert_eq!(test_runs(&events), [(Some(counts), Some(101))]);
        let at = events
            .iter()
            .position(|event| matches!(event, Event::TestRun { .. }))
            .expect("the run is reported");
        assert!(
            matches!(
                events.get(at.wrapping_sub(1)),
                Some(Event::ToolCallEnd { .. })
            ),
            "{events:?}"
        );
    }

    #[test]
    fn a_test_run_the_cli_handed_over_in_part_is_not_read() {
        let cut = TEST_RUN.replace(
            "test tests::slow ... ignored\n",
            "... (1 lines truncated)\n",
        );
        let preview = format!(
            "<persisted-output>\nOutput too large (55.6KB). Full output saved to: /x.txt\n\n\
             Preview (first 2KB):\n{TEST_RUN}\n</persisted-output>"
        );
        for (output, reported) in [
            (cut.as_str(), shell_report("")),
            (preview.as_str(), shell_report("")),
            (
                TEST_RUN,
                shell_report(r#","persistedOutputPath":"/x.txt","persistedOutputSize":56907"#),
            ),
            (
                TEST_RUN,
                shell_report("").replace(r#""interrupted":false"#, r#""interrupted":true"#),
            ),
        ] {
            let mut translator = translator();
            shell_call(&mut translator);

            let events = translator.line(&result_with("t1", output, &reported));

            let counts: Vec<_> = test_runs(&events).into_iter().map(|(c, _)| c).collect();
            assert_eq!(counts, [None], "{output}\n{reported}");
        }
    }

    /// A `cargo test` run of one test binary that passed, whole.
    const PASSING_RUN: &str =
        "    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.01s
     Running unittests src/lib.rs (target/debug/deps/demo-760e00b68511d171)

running 2 tests
test tests::adds ... ok
test tests::subtracts ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
";

    /// How many times [`read_spilled`] was asked for a file, by the tests
    /// that hand it to a translator. Each such test names its own file, so
    /// that tests running at once count apart.
    static READS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    /// Stands in for the file system: `/whole.txt` holds [`PASSING_RUN`],
    /// `/preview.txt` holds its first part, and nothing else exists.
    fn read_spilled(path: &Path, size: u64) -> Option<String> {
        READS
            .lock()
            .expect("no test panics holding it")
            .push(path.to_owned());
        let text = match path.to_str()? {
            "/whole.txt" | "/once.txt" => PASSING_RUN,
            "/preview.txt" => PASSING_RUN.get(..200)?,
            _ => return None,
        };
        (text.len() as u64 == size).then(|| text.to_owned())
    }

    fn reads_of(path: &str) -> usize {
        READS
            .lock()
            .expect("no test panics holding it")
            .iter()
            .filter(|read| read.as_os_str() == path)
            .count()
    }

    /// The result of a passing run the CLI saved to `path`, with the preview
    /// it hands the model in its place.
    fn spilled(path: &str, size: usize) -> String {
        let preview = format!(
            "<persisted-output>\nOutput too large (55.6KB). Full output saved to: {path}\n\n\
             Preview (first 2KB):\n{}\n</persisted-output>",
            PASSING_RUN.get(..200).expect("the run is longer than that")
        );
        result_with(
            "t1",
            &preview,
            &shell_report(&format!(
                r#","persistedOutputPath":"{path}","persistedOutputSize":{size}"#
            )),
        )
    }

    #[test]
    fn a_test_run_the_cli_saved_to_a_file_is_read_from_that_file() {
        let mut translator = translator().reading_spilled_with(read_spilled);
        shell_call(&mut translator);

        let events = translator.line(&spilled("/whole.txt", PASSING_RUN.len()));

        let counts = TestCounts {
            passed: 2,
            failed: 0,
            ignored: 0,
            suites: 1,
        };
        assert_eq!(test_runs(&events), [(Some(counts), Some(0))]);
    }

    #[test]
    fn a_saved_test_run_whose_file_is_not_the_whole_run_is_not_read() {
        for (path, size) in [
            ("/missing.txt", PASSING_RUN.len()),
            ("/whole.txt", PASSING_RUN.len() + 1),
            ("/preview.txt", 200),
        ] {
            let mut translator = translator().reading_spilled_with(read_spilled);
            shell_call(&mut translator);

            let events = translator.line(&spilled(path, size));

            assert_eq!(test_runs(&events), [(None, Some(0))], "{path} {size}");
        }
    }

    #[test]
    fn a_saved_test_run_is_not_read_by_a_translator_handed_no_reader() {
        let mut translator = translator();
        shell_call(&mut translator);

        let events = translator.line(&spilled("/whole.txt", PASSING_RUN.len()));

        assert_eq!(test_runs(&events), [(None, Some(0))]);
    }

    #[test]
    fn a_saved_file_is_read_once_and_only_for_a_test_run() {
        let mut translator = translator().reading_spilled_with(read_spilled);
        shell_call(&mut translator);
        translator.line(&spilled("/once.txt", PASSING_RUN.len()));
        // The same result again finds no call waiting on it.
        translator.line(&spilled("/once.txt", PASSING_RUN.len()));
        translator.line(&call("t2", "Bash", r#"{"command":"cat big.log"}"#));
        translator.line(&spilled("/other.txt", PASSING_RUN.len()).replace(r#""t1""#, r#""t2""#));

        assert_eq!(reads_of("/once.txt"), 1);
        assert_eq!(reads_of("/other.txt"), 0);
    }

    /// How the CLI cuts a failed command's output: by characters, from the
    /// middle of one line to the middle of another, around a line of its own.
    /// Recorded from 2.1.282 on a `cargo test --workspace`. What is left can
    /// read as a whole run of fewer binaries, as it does here.
    #[test]
    fn a_failed_test_run_the_cli_cut_by_characters_is_not_read() {
        let cut = format!(
            "Exit code 101\n{}",
            TEST_RUN.replace(
                "\nerror: test failed",
                "\n... [20014 characters truncated] ...\n\nrror: test failed"
            )
        );
        let mut translator = translator();
        shell_call(&mut translator);

        let events = translator.line(&result("t1", &cut, true));

        assert_eq!(test_runs(&events), [(None, Some(101))]);
        assert_eq!(
            failed_runs(&events),
            [true],
            "a run that exited 101 after its tests started failed, counted or not"
        );
    }

    #[test]
    fn a_test_run_whose_build_failed_is_not_a_failed_run() {
        let build_failed = "Exit code 101
   Compiling demo v0.1.0 (/work/demo)
error[E0308]: mismatched types
 --> src/lib.rs:7:52

error: could not compile `demo` (lib test) due to 1 previous error";
        let mut translator = translator();
        shell_call(&mut translator);

        let events = translator.line(&result("t1", build_failed, true));

        assert_eq!(test_runs(&events), [(None, Some(101))]);
        assert_eq!(failed_runs(&events), [false]);
    }

    #[test]
    fn a_counted_run_is_failed_exactly_where_its_counts_say_so() {
        let mut failing = translator();
        shell_call(&mut failing);
        let events = failing.line(&result("t1", &format!("Exit code 101\n{TEST_RUN}"), true));
        assert_eq!(failed_runs(&events), [true]);

        let mut passing = translator();
        shell_call(&mut passing);
        let events = passing.line(&result_with("t1", PASSING_RUN, &shell_report("")));
        assert_eq!(failed_runs(&events), [false]);
    }

    #[test]
    fn a_test_run_filtered_down_to_its_summaries_happened_and_is_not_read() {
        let mut translator = translator();
        translator.line(&call(
            "t1",
            "Bash",
            r#"{"command":"cargo test 2>&1 | grep 'test result'"}"#,
        ));

        let summary = "test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s";
        let events = translator.line(&result_with("t1", summary, &shell_report("")));

        assert_eq!(test_runs(&events), [(None, Some(0))]);
    }

    #[test]
    fn a_failing_test_run_filtered_to_its_tail_names_the_list_the_tail_kept() {
        let mut translator = translator();
        translator.line(&call(
            "t1",
            "Bash",
            r#"{"command":"cargo test 2>&1 | tail -5"}"#,
        ));

        let tail = "failures:\n    tests::wrong\n\n\
                    test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; \
                    0 filtered out; finished in 0.00s\n\n\
                    error: test failed, to rerun pass `--lib`";
        let events = translator.line(&result_with("t1", tail, &shell_report("")));

        let named: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::TestRun {
                    counts,
                    exit_code,
                    failed,
                    failures,
                    ..
                } => Some((*counts, *exit_code, *failed, failures.clone())),
                _ => None,
            })
            .collect();
        let lib = niobe_core::FailedTests {
            binary: "--lib".to_owned(),
            tests: vec!["tests::wrong".to_owned()],
        };
        assert_eq!(named, [(None, Some(0), true, vec![lib])]);
    }

    #[test]
    fn a_refused_test_run_or_another_command_is_no_test_run() {
        let mut other = translator();
        other.line(&call("t1", "Bash", r#"{"command":"cat log.txt"}"#));
        let events = other.line(&result_with("t1", TEST_RUN, &shell_report("")));
        assert_eq!(test_runs(&events), []);

        let mut refused = translator();
        shell_call(&mut refused);
        refused.refused(&ToolCallId::new("t1"));
        let events = refused.line(&result("t1", "The operator denied this call", true));
        assert_eq!(test_runs(&events), []);
    }

    #[test]
    fn a_shell_command_that_failed_carries_the_status_it_exited_with_and_its_reason() {
        let mut translator = translator();
        shell_call(&mut translator);

        let events = translator.line(&result(
            "t1",
            "Exit code 101\nerror: test failed, to rerun pass `--lib`",
            true,
        ));

        assert_eq!(
            ending(&events),
            (
                Some(101),
                Some("error: test failed, to rerun pass `--lib`".to_owned())
            )
        );
    }

    #[test]
    fn a_shell_command_the_cli_reported_a_plain_success_for_exited_zero() {
        let mut translator = translator();
        shell_call(&mut translator);

        let events = translator.line(&result_with("t1", "ok", &shell_report("")));

        assert_eq!(ending(&events), (Some(0), None));
    }

    #[test]
    fn a_success_the_cli_read_a_status_as_names_no_status_it_did_not_report() {
        // `grep` finding nothing exits 1, and the CLI reports it as a success
        // with its reading of the status beside it — not the status itself.
        for extra in [
            r#","returnCodeInterpretation":"No matches found""#,
            r#","backgroundTaskId":"b1""#,
        ] {
            let mut translator = translator();
            shell_call(&mut translator);

            let events = translator.line(&result_with("t1", "", &shell_report(extra)));

            assert_eq!(ending(&events), (None, None), "{extra}");
        }

        let mut translator = translator();
        shell_call(&mut translator);
        let interrupted =
            shell_report("").replace(r#""interrupted":false"#, r#""interrupted":true"#);
        let events = translator.line(&result_with("t1", "", &interrupted));
        assert_eq!(ending(&events), (None, None), "an interrupted command");
    }

    #[test]
    fn a_shell_command_with_no_report_beside_it_has_no_status() {
        let mut translator = translator();
        shell_call(&mut translator);

        let events = translator.line(&result("t1", "ok", false));

        assert_eq!(ending(&events), (None, None));
    }

    #[test]
    fn a_shell_command_the_cli_would_not_run_has_a_reason_and_no_status() {
        let mut translator = translator();
        shell_call(&mut translator);

        let events = translator.line(&result(
            "t1",
            "<tool_use_error>Blocked: sleep 60 followed by: ls</tool_use_error>",
            true,
        ));

        assert_eq!(
            ending(&events),
            (None, Some("Blocked: sleep 60 followed by: ls".to_owned()))
        );
    }

    #[test]
    fn a_failure_that_came_back_with_no_words_has_no_reason() {
        let mut translator = translator();
        translator.line(&call("t1", "Read", r#"{"file_path":"/repo/a"}"#));

        let events = translator.line(&result("t1", "  \n", true));

        assert_eq!(ending(&events), (None, None));
    }

    #[test]
    fn only_a_shell_command_has_an_exit_status() {
        let mut translator = translator();
        translator.line(&call("t1", "Read", r#"{"file_path":"/repo/a"}"#));

        let events = translator.line(&result_with("t1", "Exit code 3", &shell_report("")));

        assert_eq!(ending(&events), (None, None));
    }
}
