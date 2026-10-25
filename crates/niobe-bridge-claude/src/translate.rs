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
//! Seven properties of the stream are not obvious and are the reason this file
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
//!   usage is folded from there wherever one arrives.
//! * **A reply can be cut off before its `message_delta`.** Seen on
//!   Claude Code 2.1.288: a reply's stream stopped after its text, no
//!   `message_delta` came, and twelve minutes later the CLI carried the turn
//!   on with a prompt of its own saying the reply was cut off. Its closing
//!   `result` counted the cut reply from the snapshot on its last `assistant`
//!   line. So a message still waiting for its count when the next one starts,
//!   or when the turn ends, is counted from that snapshot, and the operator
//!   is told it was cut — except where the operator's interrupt stopped it: a
//!   reply stopped that way is one the CLI counts nothing for. Claude Code
//!   2.1.285 marks its `assistant` line `aborted` and sends no
//!   `message_delta`; 2.1.292, stopped before any `assistant` line, still
//!   sends one after its `[Request interrupted by user]` note, and its
//!   `result` and running totals leave that count out too. So the note, the
//!   mark, or a turn that ends `aborted` with the reply in flight each say the
//!   reply is not counted, and is not called cut off.
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
//!   A `system`/`status` that names a mode other than the one last reported
//!   is a [`Event::ModeSelected`] too: it is how the CLI says it left plan
//!   mode when a plan was approved.
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
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use niobe_core::diff::{self, Hunk, Line};
use niobe_core::event::{
    AgentId, AgentOutcome, Backend, Billing, ChangeScope, CompactTrigger, Context, Event,
    McpServer, McpStatus, Mode, ModelOption, Overage, OverageRefusal, PermissionDecision,
    SessionMeta, SlashCommand, TokenCounts, ToolCallId, ToolOutcome, Usage, UsageWindow,
    UsageWindows,
};
use niobe_core::proposal::{Action, Proposal};
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

/// Why a call an interrupt cut off did not run to its end.
const STOPPED: &str = "stopped by the operator";

/// What the transcript says of a turn an interrupt ended.
const STOPPED_TURN: &str = "The turn was stopped by the operator.";

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

impl From<Counts> for TokenCounts {
    fn from(counts: Counts) -> Self {
        Self {
            input: counts.input,
            output: counts.output,
            cache_read: counts.cache_read,
            cache_write: counts.cache_write,
            cache_write_1h: None,
        }
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

/// A message whose `message_start` has arrived and whose usage has not.
#[derive(Debug)]
struct InFlight {
    /// The id its tokens are filed under.
    model: String,
    /// Whether it is served in fast mode.
    fast: bool,
    /// The counts its `message_start` carried: the prompt, where the
    /// `message_delta` that ends it counts only the output.
    opened: Option<wire::Usage>,
    /// The API's id for the message, where its `message_start` named one.
    id: Option<String>,
    /// The id of the request the message answers, off its `assistant` line.
    request: Option<String>,
    /// The counts its last `assistant` line carried: what the message is
    /// counted from if its stream is cut off before its `message_delta`.
    snapshot: Option<wire::Usage>,
    /// Whether the operator's interrupt stopped it as it was written: its
    /// `assistant` line said so, the CLI's note of the interrupt arrived while
    /// it was in flight, or the turn ended on the interrupt with it in flight.
    aborted: bool,
}

impl InFlight {
    /// The message as a report names it: its id, and the request it answers
    /// where that is known. `None` where the stream named neither.
    fn name(&self) -> Option<String> {
        match (&self.id, &self.request) {
            (Some(id), Some(request)) => Some(format!("{id} (request {request})")),
            (Some(id), None) => Some(id.clone()),
            (None, Some(request)) => Some(format!("request {request}")),
            (None, None) => None,
        }
    }

    /// Whether an `assistant` line for the message `id` is a line of this
    /// one. A side that names no id cannot say otherwise, and the stream it
    /// arrived on is the same.
    fn is(&self, id: Option<&str>) -> bool {
        match (self.id.as_deref(), id) {
            (Some(mine), Some(theirs)) => mine == theirs,
            _ => true,
        }
    }
}

/// Whether the CLI went on with the turn after a reply was cut off, which
/// is what the notice of the cut says it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CarriedOn {
    Yes,
    No,
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

/// The CLI's answer to a model change Niobe asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAnswer {
    /// The control request the answer is to.
    pub request_id: String,
    /// Whether the CLI moved to the model; false where it refused.
    pub taken: bool,
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
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Reported {
    tokens: Counts,
    cost_usd: f64,
}

/// What the CLI had recorded a session as spending, per model, when it last
/// left it.
///
/// The CLI restores these running totals when the session is resumed, so the
/// first `result` of the resumed process carries the whole earlier session
/// plus its own turn — recorded live on Claude Code 2.1.282: a session left at
/// $0.0560548 came back from one more turn reporting $0.064619. A translator
/// that starts from nothing reports all of it as that turn's cost, on top of
/// the earlier turns already on screen, so a resumed session's translator
/// starts from this instead.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Spent {
    by_model: BTreeMap<String, Reported>,
    /// Why the earlier spend could not be read, where it could not.
    unknown: Option<String>,
}

impl Spent {
    /// The totals a `modelUsage` block states, which is what the CLI's
    /// `cost-state` record carries.
    ///
    /// A cost recorded there that no bill can be leaves the spend unknown:
    /// measuring the resumed session's bills from it would report the
    /// difference as spend.
    pub(crate) fn from_model_usage(model_usage: &BTreeMap<String, wire::ModelUsage>) -> Self {
        let unmeant = model_usage
            .iter()
            .find_map(|(model, usage)| match &usage.cost_usd {
                Some(wire::Cost::Unmeant(text)) => Some((model, text)),
                Some(wire::Cost::Said(_)) | None => None,
            });
        if let Some((model, text)) = unmeant {
            return Self::unknown(format!(
                "the CLI recorded {model} as having cost `{text}`, which no bill can be"
            ));
        }
        let by_model = model_usage
            .iter()
            .map(|(model, usage)| {
                let reported = Reported {
                    tokens: Counts::from(usage),
                    cost_usd: usage
                        .cost_usd
                        .as_ref()
                        .and_then(wire::Cost::usd)
                        .unwrap_or_default(),
                };
                (model.clone(), reported)
            })
            .collect();
        Self {
            by_model,
            unknown: None,
        }
    }

    /// A session whose earlier spend could not be read, for the reason `why`.
    ///
    /// Not the same as nothing spent: the CLI restores its running totals
    /// whatever this side could read, so the first `result` still carries
    /// the earlier session, and only how much of it is unknown.
    pub fn unknown(why: impl Into<String>) -> Self {
        Self {
            by_model: BTreeMap::new(),
            unknown: Some(why.into()),
        }
    }

    /// Whether nothing was recorded as spent: a session the CLI never left
    /// cleanly, or one that has not run a turn. Not a session whose spend is
    /// [`Spent::unknown`].
    pub fn is_empty(&self) -> bool {
        self.by_model.is_empty() && self.unknown.is_none()
    }

    /// Whether what was spent is known, which it is unless it was
    /// [`Spent::unknown`].
    pub fn is_known(&self) -> bool {
        self.unknown.is_none()
    }

    /// What was recorded as spent on `model`, in USD.
    pub fn cost_usd(&self, model: &str) -> Option<f64> {
        self.by_model.get(model).map(|reported| reported.cost_usd)
    }
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
    /// carries the turn's authoritative usage and neither the model nor the
    /// speed, so both are remembered from the `message_start` that opened the
    /// same stream.
    in_flight: BTreeMap<Option<String>, InFlight>,
    /// What each outstanding `tool_use` id was called and called with, so that
    /// the `tool_result` can repeat both without the consumer holding state.
    tool_calls: BTreeMap<String, Call>,
    /// The sub-agent behind each call that has ended since the last `result`,
    /// by call id: the closing line can list a refusal of one of them after
    /// its `tool_result` took the call out of `tool_calls`, and the refusal is
    /// that agent's.
    ended_agents: BTreeMap<String, AgentId>,
    /// The words of the usage limit the session itself ran into this turn,
    /// kept so that the turn's closing `result`, which repeats them as an
    /// error, folds into the one notice already shown.
    usage_limit: Option<String>,
    /// The sub-agent calls whose agent ran into the usage limit: they ended
    /// because the plan stopped them, not because they failed.
    limited_agents: BTreeMap<String, ()>,
    /// The permission prompts read since the last drain, for whatever owns the
    /// CLI's standard input to answer.
    asked: Vec<Asked>,
    /// The calls whose prompts the CLI withdrew since the last drain, which
    /// whatever answers prompts must stop holding as asked.
    withdrawn: Vec<ToolCallId>,
    /// The sub-agents this session has spawned, by the id of the call that
    /// spawned each, and whether each is still running.
    agents: BTreeMap<String, Running>,
    /// The shell commands the CLI moved to the background and has not said
    /// the end of, by the id of the call that started each: its command.
    background: BTreeMap<String, String>,
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
    denied: BTreeMap<String, Option<String>>,
    /// The `request_id`s of the model changes Niobe asked for that the CLI has
    /// not answered yet: a refusal of one of those is what puts the session
    /// back on the model it last reported.
    model_requests: BTreeMap<String, String>,
    /// The `request_id`s of the mode changes Niobe asked for that the CLI has
    /// not answered yet, each with the mode it asks for.
    mode_requests: BTreeMap<String, Mode>,
    /// The mode the last mode change the CLI took asked for, until the CLI
    /// reports the mode it is in.
    taken_mode: Option<Mode>,
    /// The mode the CLI was started in, for a refusal that comes before the
    /// CLI has reported one.
    started_in: Option<Mode>,
    /// The model the last model change the CLI took asked for, until an
    /// `init` says what the CLI resolved it to.
    taken_model: Option<String>,
    /// The model changes the CLI has answered since the last
    /// [`Translator::take_model_answers`], oldest first.
    model_answers: Vec<ModelAnswer>,
    /// The `request_id`s of the titles Niobe asked for that the CLI has not
    /// answered yet: an answer to one of those carries a title rather than a
    /// change taking effect.
    title_requests: BTreeSet<String>,
    /// The `request_id`s of the `mcp_status` requests Niobe made that the CLI
    /// has not answered yet: a refusal of one changed nothing the session runs
    /// with, and says only that this CLI does not answer it.
    mcp_status_requests: BTreeSet<String>,
    /// The reconnects Niobe asked for that the CLI has not answered yet, by
    /// `request_id`, each with the server it names: the answer names neither.
    reconnects: BTreeMap<String, String>,
    /// The rewinds Niobe asked for that the CLI has not answered yet, by the
    /// `request_id` of each of their two requests.
    rewinds: BTreeMap<String, Rewind>,
    /// Control requests an answer called for, not yet taken to be written.
    requests: Vec<String>,
    /// What the CLI's last answer to `mcp_status` said of each server, by
    /// name, which every later `init` lists again without.
    mcp_said: BTreeMap<String, McpServer>,
    /// The servers the last `init` listed, and how each stood: what an answer
    /// that omits one is read against.
    mcp_listed: BTreeMap<String, McpStatus>,
    /// The servers the last answer left out, and how each stood when it did:
    /// one that still stands so is not asked about again for the omission.
    mcp_omitted: BTreeMap<String, McpStatus>,
    /// Whether the servers should be asked about again: one stands otherwise
    /// than the CLI last said, or a reconnect was answered. Taken by
    /// [`Translator::take_mcp_status_wanted`].
    mcp_status_wanted: bool,
    /// Whether the CLI refused `mcp_status`, which no later request will
    /// change: it is not asked again.
    mcp_status_refused: bool,
    /// Per-message usage since the last `result`, for the turn reconciliation.
    turn: Counts,
    /// Of the per-message cache writes since the last `result`, the ones
    /// bought for an hour: `None` once a message gave no split, since the
    /// share of the others would read as the turn's.
    turn_1h: Option<u64>,
    /// The messages since the last `result` whose stream ended before their
    /// `message_delta`, as a report names them, for the turn reconciliation.
    unfinished: Vec<String>,
    /// Per model, everything reported for it so far this session.
    reported: BTreeMap<String, Reported>,
    /// Why a resumed session's earlier spend is unknown, until the notice
    /// saying so has gone out with the first events.
    unknown_spend: Option<String>,
    /// Whether the next bill's running totals still carry an earlier spend
    /// nothing here knows: that of a resumed session whose transcript could
    /// not be read, until its first `result` or a `/clear`.
    restored_unknown: bool,
    /// The message types and system subtypes already reported as unread, so a
    /// shape the CLI repeats is said once rather than after every turn.
    unread_said: BTreeSet<String>,
    /// The models that first bill named, whose cost from then on is short by
    /// an amount nothing here knows. Each settlement of one is followed by a
    /// record no cost covers, so the session's figure stays a floor until a
    /// `/clear` starts the conversation over.
    short: BTreeSet<String>,
    /// The call each permission prompt the CLI has asked is about, by the
    /// id the CLI asked it under, so that a prompt it withdraws is known.
    open_requests: BTreeMap<String, ToolCallId>,
    /// Messages whose usage arrived before anything named their model — a
    /// `message_delta` with no `message_start` ahead of it, before `init` —
    /// held until the turn's `result` says which id billed them. Filed under
    /// no model, they would be priced as unknown and then counted again when
    /// the bill named the id they were spent on.
    unattributed: Vec<Usage>,
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
    /// What says whether a file is at a path, where the caller handed one
    /// in. `None` leaves it unsaid of every file a write would replace.
    file_exists: Option<FileExists>,
    /// Whether the CLI's last request state was `compacting`, so that the
    /// state after it is reported as the compaction's end.
    compacting: bool,
    /// The mode the CLI last said it gates calls in, as it spells it, so a
    /// `status` that repeats it says nothing new.
    mode: Option<String>,
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

/// Whether a file is at a path, as a permission question to write it is
/// asked: what the file held is lost to the write and is not in the call, so
/// the question says whether there was one. `None` where that cannot be told.
///
/// Handed in as [`ReadSpilled`] is, so the translator does no I/O of its own.
pub(crate) type FileExists = fn(&Path) -> Option<bool>;

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
            usage_limit: None,
            limited_agents: BTreeMap::new(),
            asked: Vec::new(),
            withdrawn: Vec::new(),
            agents: BTreeMap::new(),
            background: BTreeMap::new(),
            agent_models: BTreeMap::new(),
            recorded_agents: BTreeMap::new(),
            denied: BTreeMap::new(),
            model_requests: BTreeMap::new(),
            mode_requests: BTreeMap::new(),
            taken_mode: None,
            started_in: None,
            taken_model: None,
            model_answers: Vec::new(),
            title_requests: BTreeSet::new(),
            mcp_status_requests: BTreeSet::new(),
            reconnects: BTreeMap::new(),
            rewinds: BTreeMap::new(),
            requests: Vec::new(),
            mcp_said: BTreeMap::new(),
            mcp_listed: BTreeMap::new(),
            mcp_omitted: BTreeMap::new(),
            mcp_status_wanted: false,
            mcp_status_refused: false,
            turn: Counts::default(),
            turn_1h: Some(0),
            unfinished: Vec::new(),
            reported: BTreeMap::new(),
            unknown_spend: None,
            restored_unknown: false,
            unread_said: BTreeSet::new(),
            short: BTreeSet::new(),
            open_requests: BTreeMap::new(),
            unattributed: Vec::new(),
            windows: BTreeMap::new(),
            context: None,
            checked_release: false,
            billed_as: None,
            api_key_source: None,
            billing: None,
            read_spilled: None,
            file_exists: None,
            compacting: false,
            mode: None,
        }
    }

    /// The same translator, for a resumed session the CLI had recorded as
    /// having spent `spent`: what the CLI restores is counted as already
    /// reported, so the first turn reports only its own cost.
    ///
    /// Where `spent` is [`Spent::unknown`], the first bill cannot be split
    /// into what was restored and what the turn added, so none of it is
    /// counted: that turn's tokens are its messages' own, and its cost goes
    /// unreported. Leaving it out understates the session by one turn, so
    /// its figure is kept a floor from then on; counting it would add the
    /// whole earlier session again, which is the larger error and the one
    /// nothing on screen could tell apart from a real cost. The first events
    /// say so.
    #[must_use]
    pub fn resuming(mut self, spent: &Spent) -> Self {
        self.reported = spent.by_model.clone();
        self.unknown_spend = spent.unknown.clone();
        self.restored_unknown = spent.unknown.is_some();
        self
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

    /// The same translator, told the mode the CLI was started in, which is
    /// what a mode change refused before the CLI reports a mode of its own
    /// goes back to.
    #[must_use]
    pub fn started_in(mut self, mode: Mode) -> Self {
        self.started_in = Some(mode);
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
    /// 30 KB, and a whole workspace's test run is past that. The same reader
    /// reads back the whole of an output Niobe's hook trimmed, from the file
    /// the hook wrote it to.
    #[must_use]
    pub(crate) fn reading_spilled_with(mut self, read: ReadSpilled) -> Self {
        self.read_spilled = Some(read);
        self
    }

    /// The same translator, looking with `exists` for the file a write it
    /// is asked about would replace.
    ///
    /// Only a live session's translator looks: a transcript read back later
    /// would be told what is on disk now, not what was when it asked.
    #[must_use]
    pub(crate) fn checking_files_with(mut self, exists: FileExists) -> Self {
        self.file_exists = Some(exists);
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
    /// Recorded against Claude Code 2.1.278, this is the only thing that marks
    /// such a call denied: the CLI sends no `system`/`permission_denied` for a
    /// call refused over the control channel, only for one it refuses by its
    /// own rules. From 2.1.309 the CLI marks every refusal beside its result
    /// too, which the translator reads without being told.
    pub fn refused(&mut self, id: &ToolCallId) {
        self.denied.insert(id.as_str().to_owned(), None);
    }

    /// Records that the control request `request_id` asks the CLI for a
    /// different model.
    ///
    /// The CLI's answer names the request it answers and nothing else, so
    /// only this says that a refusal is of a model: told it, the translator
    /// follows the refusal's warning with [`Event::ModelRefused`], and the
    /// session goes back to showing the model that is still running.
    pub fn asked_for_model(&mut self, request_id: &str, model: &str) {
        self.model_requests
            .insert(request_id.to_owned(), model.to_owned());
    }

    /// Records that the control request `request_id` asks the CLI to gate
    /// tool calls in `mode`.
    ///
    /// The CLI refuses some changes — `auto` where the plan, the model or
    /// fast mode rules it out — and its answer names only the request, so
    /// only this says a refusal is of a mode: told it, the translator follows
    /// the refusal's warning with the mode the CLI is still in, and the
    /// session stops showing the one it never moved to.
    pub fn asked_for_mode(&mut self, request_id: &str, mode: Mode) {
        self.mode_requests.insert(request_id.to_owned(), mode);
    }

    /// Records that the control request `request_id` asks the CLI to title
    /// the session (`generate_session_title`).
    ///
    /// The answer names the request and nothing else, so only this says that
    /// a `title` in it is the session's, and that a refusal of it changed
    /// nothing the session was running with.
    pub fn asked_for_title(&mut self, request_id: &str) {
        self.title_requests.insert(request_id.to_owned());
    }

    /// Records that the control request `request_id` asks the CLI how its MCP
    /// servers stand (`mcp_status`).
    ///
    /// The answer is known by what it carries, so this is only what tells a
    /// refusal of it — from a CLI that does not answer the request — from the
    /// refusal of a change: nothing the session runs with is different.
    pub fn asked_for_mcp_status(&mut self, request_id: &str) {
        self.mcp_status_requests.insert(request_id.to_owned());
    }

    /// Records that the control request `request_id` asks the CLI to
    /// reconnect the MCP server `server` (`mcp_reconnect`).
    ///
    /// The answer names the request and nothing else, so only this says which
    /// server a refusal is of.
    pub fn asked_to_reconnect(&mut self, request_id: &str, server: &str) {
        self.reconnects
            .insert(request_id.to_owned(), server.to_owned());
    }

    /// Tells the translator that `dry` asks the CLI what putting the files
    /// back as they were before the prompt it sent as `prompt_id` would
    /// change. Where the answer says they can go back, the request that puts
    /// them back is asked as `real`, and taken with [`Self::take_requests`]:
    /// asked before the dry run's answer, the CLI could put the files back
    /// first and the dry run say nothing would change. `prompt` is how the
    /// prompt is named to the operator.
    pub fn asked_to_rewind(&mut self, dry: &str, real: &str, prompt_id: &str, prompt: &str) {
        self.rewinds.insert(
            dry.to_owned(),
            Rewind::Dry {
                real: real.to_owned(),
                prompt_id: prompt_id.to_owned(),
                prompt: prompt.to_owned(),
            },
        );
    }

    /// The control requests the CLI's answers called for since the last
    /// call, each a line for its standard input, oldest first.
    pub fn take_requests(&mut self) -> Vec<String> {
        std::mem::take(&mut self.requests)
    }

    /// Whether the MCP servers should be asked about again since the last
    /// call: an `init` listed one standing otherwise than the CLI last said
    /// — so why it failed, if it did, is not known — or a reconnect was
    /// answered. Never once the CLI has refused to say.
    pub fn take_mcp_status_wanted(&mut self) -> bool {
        std::mem::take(&mut self.mcp_status_wanted) && !self.mcp_status_refused
    }

    /// The model changes the CLI answered since the last call, oldest first.
    ///
    /// Separate from the events for the reason [`Translator::take_asked`]
    /// is: [`Event::ModelRefused`] does not say which request it refuses, and
    /// whatever starts the CLI again needs to know which change never took
    /// effect when two were asked for before either was answered.
    pub fn take_model_answers(&mut self) -> Vec<ModelAnswer> {
        std::mem::take(&mut self.model_answers)
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

    /// The calls whose prompts the CLI withdrew since the last call, oldest
    /// first.
    ///
    /// The CLI takes no answer to a request it has cancelled, so whatever
    /// took a prompt from [`Translator::take_asked`] has to let it go here:
    /// held on to, the call would still be answerable, and a session that
    /// ended after it would read as one that ended while asking.
    pub fn take_withdrawn(&mut self) -> Vec<ToolCallId> {
        std::mem::take(&mut self.withdrawn)
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
        let mut out: Vec<Event> = self
            .unknown_spend
            .take()
            .map(unknown_spend)
            .into_iter()
            .collect();
        out.extend(self.read(line, true));
        out
    }

    /// The events of one line, where `mend` says a line that does not parse
    /// may be read again with each half of a surrogate pair it holds alone
    /// read as U+FFFD: see [`wire::without_lone_surrogates`].
    fn read(&mut self, line: &str, mend: bool) -> Vec<Event> {
        match serde_json::from_str::<wire::Message>(line) {
            Ok(wire::Message::Unknown) => {
                let kind = kind_of(line);
                self.unread_once(
                    format!("type:{kind}"),
                    format!(
                        "the CLI sent a message of type `{kind}`, which this version of Niobe \
                         does not know how to read. It was not counted. Later ones of this \
                         type are not reported again."
                    ),
                )
                .into_iter()
                .collect()
            }
            Ok(message) => self.message(message),
            Err(error) => match mend.then(|| wire::without_lone_surrogates(line)).flatten() {
                Some(mended) => {
                    let mut out = vec![warn(HALF_A_CHARACTER.to_owned())];
                    out.extend(self.read(&mended, false));
                    out
                }
                None => self.without_figures(line, &error),
            },
        }
    }

    /// A line that did not read as a message, read again without its token
    /// counts and costs, where those are what kept it from reading.
    ///
    /// A count the protocol cannot mean — negative, fractional, a string, past
    /// what a counter holds — is one figure; refusing the whole line over it
    /// lost a reply the operator never saw, or a `result` and with it the end
    /// of the turn, which left the shell working on a turn that was over.
    /// The figures are left out rather than read as something, so nothing
    /// drawn is a number the CLI did not say; the CLI's per-model bill is a
    /// running total, so the next `result` still carries what this one's held.
    fn without_figures(&mut self, line: &str, error: &serde_json::Error) -> Vec<Event> {
        let kind = kind_of(line);
        let unreadable = || {
            vec![warn(format!(
                "the CLI sent a `{kind}` message this version of Niobe could not read, so it \
                 was not counted: {error}"
            ))]
        };
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(line) else {
            return unreadable();
        };
        if !strip_figures(&mut value) {
            return unreadable();
        }
        let Ok(message) = serde_json::from_value::<wire::Message>(value) else {
            return unreadable();
        };
        let mut out = vec![warn(format!(
            "the CLI sent a `{kind}` message whose token counts or cost this version of Niobe \
             could not read ({error}), so none of its figures were counted; the rest of it was."
        ))];
        out.extend(self.message(message));
        out
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
            wire::Message::System(system) => self.system(*system, &mut out),
            wire::Message::Assistant(envelope) => self.assistant(envelope, &mut out),
            wire::Message::User(envelope) => self.user(envelope, &mut out),
            wire::Message::StreamEvent(event) => self.stream(event, &mut out),
            wire::Message::ControlRequest(request) => self.control(request, &mut out),
            wire::Message::ControlCancelRequest(cancel) => self.withdrawn(cancel, &mut out),
            wire::Message::Result(outcome) => self.result(outcome, &mut out),
            wire::Message::ControlResponse(response) => self.answered(response, &mut out),
            wire::Message::RateLimitEvent(event) => rate_limit(event, &mut out),
            wire::Message::ConversationReset(_) => self.reset(&mut out),
            // Passed over: see `wire::Progress` for what it carries and why
            // none of it is counted or shown.
            wire::Message::ToolProgress(_) => {}
            // Passed over: see `wire::Lifecycle`.
            wire::Message::CommandLifecycle(_) => {}
            // Only the line a message arrived on says what type it was, so
            // whoever read that line is the one that can report it.
            wire::Message::Unknown => {}
        }
        out
    }

    /// Reads the CLI's answer to a request of Niobe's own: the slash commands
    /// the answer to `initialize` lists, the session's title, or the refusal
    /// of a change.
    ///
    /// Only this side asks the CLI anything, so every `control_response` is an
    /// answer to a request made here — what the CLI offers, a title for the
    /// session, or a mode or a model the session was asked to move to. A refusal left unreported would
    /// leave the shell showing a change that never happened, which for a model
    /// takes [`Event::ModelRefused`] as well as the warning: the shell shows
    /// the model the operator chose until the CLI answers otherwise.
    fn answered(&mut self, response: wire::ControlResponse, out: &mut Vec<Event>) {
        let Some(outcome) = response.response else {
            return;
        };
        if self.answered_of_mcp(&outcome, out) {
            return;
        }
        if let Some(rewind) = outcome
            .request_id
            .as_deref()
            .and_then(|id| self.rewinds.remove(id))
        {
            self.answered_rewind(rewind, outcome, out);
            return;
        }
        if let Some(mode) = outcome
            .request_id
            .as_deref()
            .and_then(|id| self.mode_requests.remove(id))
        {
            self.answered_mode(mode, &outcome, out);
            return;
        }
        let asked_model = outcome
            .request_id
            .as_deref()
            .and_then(|id| self.model_requests.remove(id));
        let of_model = asked_model.is_some();
        // A model change the CLI took is what it runs from its next turn, and
        // what a refusal of a later one goes back to: its next `init` has not
        // said so yet.
        if let Some(model) = &asked_model
            && outcome.subtype.as_deref() == Some("success")
        {
            self.taken_model = Some(model.clone());
            out.push(Event::ModelSelected {
                model: model.clone(),
            });
        }
        if of_model && let Some(request_id) = &outcome.request_id {
            self.model_answers.push(ModelAnswer {
                request_id: request_id.clone(),
                taken: outcome.subtype.as_deref() == Some("success"),
            });
        }
        let of_title = outcome
            .request_id
            .as_deref()
            .is_some_and(|id| self.title_requests.remove(id));
        if of_title {
            out.extend(titled(outcome));
            return;
        }
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
            if let Some(models) = answer.models {
                out.push(offered_models(models));
            }
            if let Some(servers) = answer.mcp_servers {
                out.push(self.mcp_status(servers));
            }
            return;
        }
        out.push(refused_change(&outcome));
        if of_model {
            out.push(Event::ModelRefused {
                back_to: self.taken_model.clone().or_else(|| self.model.clone()),
            });
        }
    }

    /// Reads the answer to a request to gate calls in `mode`. One the CLI took
    /// is what a later refusal goes back to; one it refused puts the session
    /// back in the mode the CLI is still in — unless a later change is still
    /// unanswered, which is then what the session shows until its own answer.
    fn answered_mode(&mut self, mode: Mode, outcome: &wire::ControlOutcome, out: &mut Vec<Event>) {
        if outcome.subtype.as_deref() == Some("success") {
            self.taken_mode = Some(mode);
            return;
        }
        out.push(refused_change(outcome));
        if !self.mode_requests.is_empty() {
            return;
        }
        let still = match (self.taken_mode, self.mode.as_deref()) {
            (Some(taken), _) => Some(Event::ModeSelected { mode: taken }),
            (None, Some(spelt)) => Some(match read_mode(spelt) {
                Some(mode) => Event::ModeSelected { mode },
                None => Event::ModeUnknown,
            }),
            (None, None) => self.started_in.map(|mode| Event::ModeSelected { mode }),
        };
        out.extend(still);
    }

    /// Reads an answer to one of a rewind's two requests. A dry run that
    /// says the files can go back asks for them to be put back, and is kept
    /// for that answer to say what went back; one that says they cannot is
    /// said at once, with its reason.
    fn answered_rewind(
        &mut self,
        rewind: Rewind,
        outcome: wire::ControlOutcome,
        out: &mut Vec<Event>,
    ) {
        let taken = outcome.subtype.as_deref() == Some("success");
        match rewind {
            Rewind::Dry {
                real,
                prompt_id,
                prompt,
            } => match (taken, outcome.response) {
                (true, Some(answer)) if answer.can_rewind == Some(true) => {
                    let would = Would {
                        files: answer
                            .files_changed
                            .unwrap_or_default()
                            .iter()
                            .map(|path| self.relative(path))
                            .collect(),
                        insertions: answer.insertions,
                        deletions: answer.deletions,
                    };
                    self.requests
                        .push(rewind_request(&real, &prompt_id, false).to_string());
                    self.rewinds.insert(real, Rewind::Real { prompt, would });
                }
                (true, Some(answer)) => out.push(not_rewound(&prompt, answer.error)),
                (_, _) => out.push(not_rewound(&prompt, outcome.error)),
            },
            Rewind::Real { prompt, would } => out.push(match taken {
                true => rewound(&prompt, &would),
                false => not_rewound(&prompt, outcome.error),
            }),
        }
    }

    /// Reads the answer to a request about the MCP servers, and says whether
    /// it was one.
    ///
    /// A reconnect the CLI took says nothing: how the server now stands is
    /// what the `mcp_status` asked after it shows. One it refused is said with
    /// the server's name and the CLI's reason, which is why the server would
    /// not come back. A refused `mcp_status` is a CLI that does not answer it,
    /// and is not asked again.
    fn answered_of_mcp(&mut self, outcome: &wire::ControlOutcome, out: &mut Vec<Event>) -> bool {
        let Some(id) = outcome.request_id.as_deref() else {
            return false;
        };
        let taken = outcome.subtype.as_deref() == Some("success");
        if let Some(server) = self.reconnects.remove(id) {
            self.mcp_status_wanted = true;
            if !taken {
                out.push(warn(format!(
                    "the MCP server `{server}` could not be reconnected: {}",
                    outcome.error.as_deref().unwrap_or("the CLI gave no reason")
                )));
            }
            return true;
        }
        if self.mcp_status_requests.remove(id) && !taken {
            self.mcp_status_refused = true;
            return true;
        }
        false
    }

    /// The servers as the CLI's answer to `mcp_status` has them, kept for
    /// every later `init` to be read against.
    fn mcp_status(&mut self, servers: Vec<wire::McpServerStatus>) -> Event {
        let servers: Vec<McpServer> = servers.into_iter().map(mcp_server_status).collect();
        self.mcp_omitted = self
            .mcp_listed
            .iter()
            .filter(|(name, _)| !servers.iter().any(|server| &server.name == *name))
            .map(|(name, status)| (name.clone(), status.clone()))
            .collect();
        self.mcp_said = servers
            .iter()
            .map(|server| (server.name.clone(), server.clone()))
            .collect();
        Event::McpServers { servers }
    }

    /// The servers an `init` lists, with what the CLI last said of each in
    /// answer to `mcp_status`: where it is configured and how it is reached
    /// for every one it named, and why it failed for one that still stands
    /// as it did then. One that stands otherwise is asked about again.
    fn mcp_servers(&mut self, servers: Vec<wire::McpServer>, tools: &[String]) -> Event {
        let mut listed = Vec::with_capacity(servers.len());
        self.mcp_listed.clear();
        for server in mcp_servers(servers, tools) {
            let said = self.mcp_said.get(&server.name);
            self.mcp_listed
                .insert(server.name.clone(), server.status.clone());
            // A server the answer left out has nothing said of it to be read
            // against, and asking again would be told the same: it is asked
            // about once for each way it stands.
            let omitted =
                said.is_none() && self.mcp_omitted.get(&server.name) == Some(&server.status);
            if !omitted && said.is_none_or(|said| said.status != server.status) {
                self.mcp_status_wanted = true;
            }
            listed.push(match said {
                Some(said) => McpServer {
                    scope: said.scope.clone(),
                    endpoint: said.endpoint.clone(),
                    reports_as: said.reports_as.clone(),
                    error: said.error.clone().filter(|_| said.status == server.status),
                    ..server
                },
                None => server,
            });
        }
        Event::McpServers { servers: listed }
    }

    /// Reports `spelt`, the mode the CLI says it now gates calls in.
    fn reported_mode(&mut self, spelt: String, out: &mut Vec<Event>) {
        match read_mode(&spelt) {
            Some(mode) => out.push(Event::ModeSelected { mode }),
            // The CLI gates calls in ways Niobe has no word for —
            // `acceptEdits`, `bypassPermissions`, `dontAsk` — and a profile
            // can ask for one through its own arguments. Naming it beats
            // showing a mode the session is not in, and so does putting down
            // the one the session was in before. Said once while it lasts:
            // every turn's `init` repeats it.
            None => {
                if self.mode.as_ref() != Some(&spelt) {
                    out.push(warn(format!(
                        "the CLI is gating tool calls as `{spelt}`, which this version of Niobe \
                         does not model. No mode is reported until the session is moved to one \
                         it does."
                    )));
                }
                out.push(Event::ModeUnknown);
            }
        }
        self.mode = Some(spelt);
        self.taken_mode = None;
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
                if let Some(servers) = system.mcp_servers {
                    let event =
                        self.mcp_servers(servers, system.tools.as_deref().unwrap_or_default());
                    out.push(event);
                }
                if let Some(mode) = system.permission_mode {
                    self.reported_mode(mode, out);
                }
            }
            // The last request's size is of a conversation the model no
            // longer has, so it is not restated with a window learnt after
            // this: the next request's is the first figure of what is left,
            // and the shell reads it as what the compaction shrank to.
            Some("compact_boundary") => {
                self.compacting = false;
                self.context = None;
                out.push(compacted(system.compact_metadata.unwrap_or_default()));
            }
            Some("permission_denied") => self.denied(system, out),
            Some("task_notification") => self.task_notification(system, out),
            Some("task_progress") => self.task_progress(system, out),
            // The CLI's own request state. `compacting` is the one that says
            // what the session is doing: summarising the conversation rather
            // than answering, and whatever state follows it — no state at
            // all, as Claude Code 2.1.282 sends it — is that ending. The rest,
            // `requesting` and whatever the CLI adds next, say what the
            // process is doing, and the shell already shows that a turn is in
            // flight.
            Some("status") => {
                let compacting = system.status.as_deref() == Some("compacting");
                match (self.compacting, compacting) {
                    (false, true) => out.push(Event::CompactionStarted),
                    (true, false) => out.push(Event::CompactionEnded),
                    (false, false) | (true, true) => {}
                }
                self.compacting = compacting;
                // The CLI moves itself out of plan mode when the operator
                // approves a plan, back to the mode it was in before, and a
                // `status` mid-turn is the only place it says so.
                if let Some(mode) = system.permission_mode
                    && self.mode.as_ref() != Some(&mode)
                {
                    self.reported_mode(mode, out);
                }
            }
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
            // The CLI saying its permission classifier is still deciding on a
            // call (`status` `checking`, sent when the check has run past four
            // seconds) and that it has (`done`). The call is already on screen
            // and its prompt, if it comes, arrives as a `can_use_tool`
            // request, so there is nothing to add to either.
            Some("permission_check_status") => {}
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
            // Something the CLI would have shown its own operator. Claude Code
            // 2.1.282 sends one when it turns fast mode off because the
            // account cannot pay for it, while every `init` and `result`
            // still says fast mode is on: this is where the operator learns
            // the session runs at the standard speed.
            Some("notification") => {
                if let Some(message) = system.text {
                    out.push(Event::Notice { message });
                }
            }
            // A request that failed and that the CLI is about to make again,
            // as Claude Code 2.1.296 puts on the live stream what its own
            // transcript calls `api_error`. The wire carries no sentence of
            // the CLI's for it, so the failure is said by its status and the
            // CLI's name for its kind.
            Some("api_retry") => {
                let kind = system.error.as_ref().and_then(|error| error.as_str());
                let failure = match (system.error_status, kind) {
                    (Some(status), Some(kind)) => format!("{status} {kind}"),
                    (Some(status), None) => status.to_string(),
                    (None, Some(kind)) => kind.to_owned(),
                    (None, None) => "no reason given".to_owned(),
                };
                out.push(retrying(
                    &failure,
                    Retry {
                        attempt: system.attempt,
                        of: system.max_retries,
                        in_ms: system.retry_delay_ms,
                    },
                ));
            }
            // A line the CLI shows its own operator, at the level it names:
            // a reply its safeguards stopped, a usage limit it is waiting
            // out, a command it did not know.
            Some("informational") => {
                let content = system.content.as_ref().and_then(|content| content.as_str());
                out.extend(content.and_then(|said| informed(said, system.level.as_deref())));
            }
            Some("model_refusal_no_fallback") => out.push(declined(
                system.original_model.as_deref(),
                system.api_refusal_category.as_deref(),
                system.api_refusal_explanation.as_deref(),
            )),
            // Not observed on the live stream from a session that provoked
            // each (README, `api-errors.jsonl`), though the CLI writes them
            // into its own transcript, where `transcript.rs` decides on each:
            // `api_error` (sent here as `api_retry`), `turn_duration`,
            // `stop_hook_summary`, `local_command`, `away_summary`,
            // `scheduled_task_fire` and `agents_killed`. One that arrives is
            // reported below, once, as a shape nobody has read here.
            other => {
                let subtype = other.unwrap_or("(none)");
                out.extend(self.unread_once(
                    format!("system:{subtype}"),
                    format!(
                        "the CLI sent a system message of subtype `{subtype}`, which this \
                         version of Niobe does not know how to read. Later ones of this \
                         subtype are not reported again."
                    ),
                ));
            }
        }
    }

    /// The notice for a shape nobody has read, the first time `key` is seen.
    fn unread_once(&mut self, key: String, message: String) -> Option<Event> {
        self.unread_said.insert(key).then(|| unread(message))
    }

    /// Whether `id` is the call that spawned one of this session's
    /// sub-agents, running or not.
    fn is_agent(&self, id: Option<&str>) -> bool {
        id.is_some_and(|id| self.agents.contains_key(id))
    }

    /// The CLI's word, on the live stream, that something it ran in the
    /// background has stopped.
    ///
    /// A sub-agent's end ends the agent. A background command's call already
    /// ended when the command was put in the background, so its end is a
    /// notice: without it the operator sees the model answer output they
    /// were never shown arriving.
    ///
    /// What the agent answered and how large its conversation ended up are
    /// reported before its end, because nothing is reported about an agent
    /// after it.
    fn task_notification(&mut self, system: wire::System, out: &mut Vec<Event>) {
        let Some(id) = system.tool_use_id else { return };
        let answer = system.summary.as_deref().and_then(opening_line);
        self.agent_progress(&id, None, total_tokens(system.usage.as_ref()), answer, out);
        out.append(&mut self.task_stopped(
            &id,
            system.status.as_deref(),
            system.summary.as_deref(),
        ));
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
    /// started has stopped, with `status` in the CLI's spelling and its
    /// `summary` of it.
    ///
    /// A notice, where `id` put a shell command in the background
    /// ([`background_ended`]). Nothing, where it did neither that nor spawn a
    /// sub-agent. Nothing either for a sub-agent that has already ended — one
    /// that is sent another message runs again and notifies again, and only
    /// the first end is one this session saw begin.
    ///
    /// The live stream spells the statuses `completed`, `failed` and
    /// `stopped`, which are the CLI's own schema for the message; its
    /// transcripts also say `killed` for a task stopped from outside. Both
    /// read as cancelled.
    pub(crate) fn task_stopped(
        &mut self,
        id: &str,
        status: Option<&str>,
        summary: Option<&str>,
    ) -> Vec<Event> {
        let mut out = Vec::new();
        if let Some(command) = self.background.remove(id) {
            out.push(background_ended(&command, status, summary));
            return out;
        }
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
    /// The CLI writes `init` again at the start of every turn, so this reports
    /// the first one and nothing after it.
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

        let reason = system
            .decision_reason
            .filter(|said| !said.trim().is_empty());
        self.denied.insert(id.clone(), reason.clone());
        let id = ToolCallId::new(id);
        out.push(Event::PermissionRequest {
            id: id.clone(),
            tool,
            input,
            target,
            // Refused without asking: there is no question to show it in.
            proposal: None,
            reason,
            agent,
        });
        out.push(Event::PermissionResponse {
            id,
            decision: PermissionDecision::Deny,
            message: None,
        });
    }

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
        self.restored_unknown = false;
        // The turn a resumed bill left uncounted belongs to the conversation
        // before the clear; the new one's settlements cover all it spent.
        self.short.clear();
        self.unattributed.clear();
        self.turn = Counts::default();
        self.turn_1h = Some(0);
        self.unfinished.clear();
        self.context = None;
        // The conversation after a clear is a stored session of its own,
        // read back without the one before it: how it is billed, which
        // release ran it and what the CLI said that this build cannot read
        // are said for it again, or it reads as unbilled and unwarned.
        self.billing = None;
        self.checked_release = false;
        self.unread_said.clear();
        out.push(Event::Cleared);
    }

    /// Records the model the session is on, producing a fresh [`SessionMeta`]
    /// where the model or the conversation has changed since it was last said
    /// — which is how a routing decision reaches the menu row.
    fn set_model(&mut self, model: String, out: &mut Vec<Event>) {
        if self.model.as_deref() == Some(model.as_str())
            && self.announced_session == self.backend_session
        {
            return;
        }
        self.model = Some(model.clone());
        self.taken_model = None;
        self.announced_session = self.backend_session.clone();
        out.push(Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: self.profile.clone(),
            model,
            backend_session: self.backend_session.clone(),
        }));
    }

    fn assistant(&mut self, envelope: wire::Envelope, out: &mut Vec<Event>) {
        self.note_snapshot(&envelope);
        if let (Some(agent), Some(model)) = (
            envelope.parent_tool_use_id.as_deref(),
            envelope.message.model.clone(),
        ) {
            self.agent_model(agent, model, out);
        }
        // The CLI answers a request the plan's usage limit refused with a
        // message of its own, and repeats the words in the turn's `result`
        // and in every sub-agent's last message. They are one event: said
        // once, by the session, and not at all by an agent, which is shown as
        // stopped by it.
        if let Some(words) = envelope.usage_limit() {
            match envelope.parent_tool_use_id {
                Some(call) => {
                    self.limited_agents.insert(call, ());
                }
                None => self.usage_limit_hit(words, out),
            }
            return;
        }
        // A message that names the sub-agent call it belongs to is that
        // agent's: its words and its calls are the agent's, not the session's.
        let agent = envelope.parent_tool_use_id.map(AgentId::new);
        let written_by_cli = envelope.message.model.as_deref() == Some(CLI_WRITTEN);
        let blocks = match envelope.message.content {
            Some(wire::Content::Blocks(blocks)) => blocks,
            Some(wire::Content::Text(text)) => {
                let text = lines_kept_where(written_by_cli, text);
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
                            target: rule_target(&name, &input),
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
                            kind: agent_kind(&input),
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
            let text = lines_kept_where(written_by_cli, text);
            out.push(Event::AssistantMessage { text, agent });
        }
        out.append(&mut calls);
    }

    /// Says once a turn that the plan's usage limit stopped it, in the CLI's
    /// words, which carry when the window starts over in the operator's zone.
    fn usage_limit_hit(&mut self, words: String, out: &mut Vec<Event>) {
        if self.usage_limit.is_some() {
            return;
        }
        out.push(Event::LimitReached {
            message: words.clone(),
        });
        self.usage_limit = Some(words);
    }

    fn user(&mut self, envelope: wire::Envelope, out: &mut Vec<Event>) {
        if envelope.says_interrupted() {
            self.stopped(&envelope.parent_tool_use_id);
        }
        let background = envelope.launched_in_background();
        let interrupted: Vec<String> = envelope
            .tool_result_meta
            .iter()
            .filter_map(|meta| meta.id.clone())
            .filter(|id| envelope.interrupted(id))
            .collect();
        let refused: Vec<String> = envelope
            .tool_result_meta
            .iter()
            .filter_map(|meta| meta.id.clone())
            .filter(|id| envelope.refused(id))
            .collect();
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
            // The CLI's own note beside the result says so too, from 2.1.309
            // on every kind of refusal, which is what marks one the driver did
            // not answer.
            // An interrupt reaches the model as a rejection, which says
            // nothing the operator does not know; the CLI's note beside the
            // result is what says it was their stop.
            let outcome = match (
                is_error.unwrap_or(false),
                self.denied.contains_key(&tool_use_id) || refused.contains(&tool_use_id),
                interrupted.contains(&tool_use_id),
            ) {
                (_, true, _) => ToolOutcome::Denied,
                (_, false, true) => ToolOutcome::Interrupted,
                (true, false, false) => ToolOutcome::Failed,
                (false, false, false) => ToolOutcome::Ok,
            };

            // A sub-agent the CLI ran in the background is still working when
            // its call returns — the result says only that it was launched —
            // and its end arrives later as a `task_notification`. One that ran
            // in the foreground, or never started, ends with its call.
            let background = outcome == ToolOutcome::Ok && background;
            if !background {
                let stopped_by_limit = self.limited_agents.remove(&tool_use_id).is_some();
                self.agent_ended(
                    &tool_use_id,
                    match outcome {
                        _ if stopped_by_limit => AgentOutcome::Cancelled,
                        ToolOutcome::Ok => AgentOutcome::Completed,
                        ToolOutcome::Failed | ToolOutcome::Denied => AgentOutcome::Failed,
                        ToolOutcome::Interrupted => AgentOutcome::Cancelled,
                    },
                    out,
                );
            }

            // Only a call that ran changed anything: a refused or broken edit
            // would otherwise put a file in the change set that is not in the
            // diff.
            let change = match outcome {
                ToolOutcome::Ok => self.file_change(&name, &arguments, &output, reported.as_ref()),
                ToolOutcome::Failed | ToolOutcome::Denied | ToolOutcome::Interrupted => None,
            };
            let exit_code = exit_code(&name, outcome, &output, reported.as_ref());
            let error = match outcome {
                ToolOutcome::Ok => None,
                ToolOutcome::Failed => failure_reason(&output, exit_code),
                // The CLI's own reason for the refusal says more than the
                // result that carries it back to the model.
                ToolOutcome::Denied => self
                    .denied
                    .get(&tool_use_id)
                    .cloned()
                    .flatten()
                    .or_else(|| failure_reason(&output, exit_code)),
                ToolOutcome::Interrupted => Some(STOPPED.to_owned()),
            };

            let untrimmed = match (outcome, name == SHELL_TOOL) {
                (ToolOutcome::Ok, true) => untrimmed(reported.as_ref(), self.read_spilled),
                _ => None,
            };
            let bytes = untrimmed.as_ref().map_or(bytes, |whole| whole.len() as u64);
            let tested = test_run(
                &name,
                &arguments,
                outcome,
                untrimmed.as_deref().unwrap_or(&output),
                reported.as_ref(),
                exit_code,
                self.read_spilled,
            );
            let command = match name == SHELL_TOOL {
                true => string_at(&arguments, "command").map(str::to_owned),
                false => None,
            };
            if outcome == ToolOutcome::Ok
                && let Some(command) = &command
                && moved_to_background(reported.as_ref())
            {
                self.background.insert(tool_use_id.clone(), command.clone());
            }
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
                command,
                untrimmed,
            });
            out.extend(change);
            out.extend(tested);
        }
    }

    /// What a finished call did to a file, where it was a call that edits one.
    ///
    /// The counts come from the diff the CLI reported beside the result, and
    /// from the call's own arguments only where no usable diff came with it.
    /// The arguments are the text an `Edit` matched, which can start or stop
    /// mid-line: `foo();` → `foo();\nbar();` on the line `    foo(); // c`
    /// moves ` // c` onto the new line, which `git diff --numstat` counts as
    /// `2 1` and a diff of the two strings as `1 0`. Two cases the arguments
    /// do not settle at all:
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
    /// The report's patch is of the whole file, so it settles both; an update
    /// reported with no hunks at all wrote what was already there. Where no
    /// usable patch was reported — as for every sub-agent's call, whose
    /// results carry no report on the live stream — the counts stay unstated
    /// rather than filled in: the lines a `Write` wrote are not the lines it
    /// changed.
    ///
    /// What the patch does not settle is a change of line endings. The CLI
    /// takes them off before it diffs — neither the patch's lines nor
    /// `originalFile` carry a `\r` — and a `Write` writes `\n`: recorded with
    /// 2.1.282, a `Write` of one/TWO/three over `one\r\ntwo\r\nthree\r\n`
    /// left `one\nTWO\nthree\n` on disk and reported one line changed, where
    /// `git diff --numstat` counts all three. Nothing in the report says the
    /// file had `\r\n` endings, and by the time it arrives the file on disk
    /// has none, so such a change is counted as its words' change alone. An
    /// `Edit` keeps the endings the file had.
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
                let replace_all = arguments
                    .get("replace_all")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                // The CLI's hunks are of the whole lines the edit touched; the
                // arguments are the text it matched, which can stop mid-line
                // and leave the rest of that line out of any diff of them.
                let counts = match (hunks.is_empty(), replace_all) {
                    (false, _) => diff::hunks_changed(&hunks),
                    (true, true) => None,
                    (true, false) => diff::replacement_changed(
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
                let counts = match created {
                    true => Some((written.lines().count() as u64, 0)),
                    false => overwrite_changed(reported, path, &hunks),
                };
                match counts {
                    Some((added, removed)) => (path, Some(added), Some(removed), hunks),
                    None => (path, None, None, hunks),
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
            scope: self.scope_of(path),
        })
    }

    /// Whose file `path` is: one of the notes the CLI keeps for the agent,
    /// or the project's.
    ///
    /// The CLI keeps an agent's notes on a project in `memory/` inside the
    /// directory it keeps that project's transcripts in:
    /// `<config>/projects/<working directory flattened>/memory/`, where the
    /// flattened name is the path with every character but an ASCII letter or
    /// digit made a `-`, so it starts with one ([`crate::transcript::directory`]
    /// sets the rule out). Read off the 57 such directories on the machine
    /// this was written on, 4 October 2026, under Claude Code 2.1.288.
    ///
    /// The flattened name is matched by its shape rather than against this
    /// session's own: none of those 57 was named after one of the many
    /// worktrees sessions had run in there, so a session in a worktree keeps
    /// its notes under another directory's name, and the CLI's configuration
    /// directory need not be `~/.claude`. A file inside the working directory is always the
    /// project's, whatever its path says, so a repository's own `memory/`
    /// directory is never taken for the agent's.
    fn scope_of(&self, path: &str) -> ChangeScope {
        let path = Path::new(path);
        let inside = self.cwd.as_deref().is_some_and(|cwd| path.starts_with(cwd));
        let parts: Vec<&str> = path
            .components()
            .filter_map(|part| part.as_os_str().to_str())
            .collect();
        let kept = parts.windows(4).any(|run| match run {
            [projects, flattened, memory, _] => {
                *projects == PROJECTS_DIR && *memory == MEMORY_DIR && is_flattened(flattened)
            }
            _ => false,
        });
        match path.is_absolute() && !inside && kept {
            true => ChangeScope::AgentMemory,
            false => ChangeScope::Project,
        }
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
                let fast = message.fast();
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
                // The message before it on the same stream never said what it
                // finished at: its stream was cut off.
                if let Some(left) = self.in_flight.remove(&stream) {
                    self.cut_off(&stream, left, CarriedOn::Yes, out);
                }
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
                self.in_flight.insert(
                    stream,
                    InFlight {
                        model: billed,
                        fast,
                        opened: message.usage,
                        id: message.id,
                        request: None,
                        snapshot: None,
                        aborted: false,
                    },
                );
            }

            wire::StreamBody::MessageDelta { usage } => {
                let Some(usage) = usage else { return };
                let started = self.in_flight.remove(&stream);
                // Its count arrives after the interrupt that stopped it, and
                // the CLI's `result` and running totals leave it out.
                if started.as_ref().is_some_and(|message| message.aborted) {
                    return;
                }
                let usage = match started.as_ref().and_then(|message| message.opened.as_ref()) {
                    Some(opened) => opened.completed_by(&usage),
                    None => usage,
                };
                self.count_message(&stream, started, &usage, out);
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

    /// Counts one message's `usage` on `stream`, filed under the model its
    /// `message_start` named.
    fn count_message(
        &mut self,
        stream: &Option<String>,
        started: Option<InFlight>,
        usage: &wire::Usage,
        out: &mut Vec<Event>,
    ) {
        let fast = usage.fast() || started.as_ref().is_some_and(|message| message.fast);
        let model = started
            .map(|message| message.model)
            .or_else(|| self.model.clone());
        let counts = Counts::from(usage);
        self.turn.add(counts);
        self.turn_1h = self
            .turn_1h
            .zip(usage.cache_write_1h())
            .map(|(sum, hour)| sum.saturating_add(hour));
        let record = Usage {
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
            model: model.clone().unwrap_or_default(),
            // The CLI reports no money per message; the turn's `result` does,
            // for the session so far, and settles this record along with it.
            cost_usd: None,
            settles_model: Some(false),
            fast,
        };
        match model {
            Some(model) => self.file(model, record, out),
            None => self.unattributed.push(record),
        }
        if stream.is_none() {
            self.report_context(usage.last_prompt(), out);
        }
    }

    /// Keeps what an `assistant` line says about the message in flight on its
    /// stream: the snapshot of its counts, the request it answers, and
    /// whether an interrupt stopped it.
    fn note_snapshot(&mut self, envelope: &wire::Envelope) {
        let Some(message) = self.in_flight.get_mut(&envelope.parent_tool_use_id) else {
            return;
        };
        if !message.is(envelope.message.id.as_deref()) {
            return;
        }
        if let Some(usage) = &envelope.message.usage {
            message.snapshot = Some(usage.clone());
        }
        if let Some(request) = &envelope.request_id {
            message.request = Some(request.clone());
        }
        message.aborted |= envelope.aborted;
    }

    /// Marks the message in flight on `stream`, if one is, as one the
    /// operator's interrupt stopped: the CLI counts nothing for it, whether
    /// or not its `message_delta` still arrives.
    fn stopped(&mut self, stream: &Option<String>) {
        if let Some(message) = self.in_flight.get_mut(stream) {
            message.aborted = true;
        }
    }

    /// A message whose stream ended before its `message_delta`: counted from
    /// the snapshot its last `assistant` line carried, which is what the CLI
    /// counts it at, and named for the turn's reconciliation.
    ///
    /// One the operator's interrupt stopped is left uncounted, as the CLI
    /// leaves it. One the stream named nothing of — no id, no snapshot — is
    /// passed over in silence: the CLI's own transcripts are folded by
    /// starting a message for every record they hold and counting each
    /// response once, so a start with nothing after it is how they read.
    fn cut_off(
        &mut self,
        stream: &Option<String>,
        mut message: InFlight,
        carried_on: CarriedOn,
        out: &mut Vec<Event>,
    ) {
        if message.aborted {
            return;
        }
        let name = message.name();
        let snapshot = message.snapshot.take();
        if name.is_none() && snapshot.is_none() {
            return;
        }
        if let Some(name) = &name {
            self.unfinished.push(name.clone());
        }
        if stream.is_none() {
            out.push(Event::Notice {
                message: cut_off_notice(name.as_deref(), snapshot.is_some(), carried_on),
            });
        }
        if let Some(snapshot) = snapshot {
            self.count_message(stream, Some(message), &snapshot, out);
        }
    }

    /// Takes back a prompt the CLI stopped waiting on, so the question does
    /// not stay on screen for an answer that would reach nothing.
    fn withdrawn(&mut self, cancel: wire::CancelRequest, out: &mut Vec<Event>) {
        let Some(id) = cancel
            .request_id
            .and_then(|request_id| self.open_requests.remove(&request_id))
        else {
            return;
        };
        self.withdrawn.push(id.clone());
        out.push(Event::PermissionWithdrawn { id });
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
        // A prompt is waited on, and answered, by the id of the call it gates.
        // One that names no call is keyed by its request instead, which is
        // unique to it: keyed by an empty id, a second such prompt would take
        // the first one's place, and the first would never be answered.
        let call_id = body
            .tool_use_id
            .or_else(|| request.request_id.clone())
            .unwrap_or_default();
        // The request names the agent too, but by the CLI's own id for it,
        // which nothing else on the stream uses; the call it gates names the
        // agent the way its calls and messages do.
        let agent = self
            .tool_calls
            .get(&call_id)
            .and_then(|call| call.agent.clone());
        let id = ToolCallId::new(call_id);
        if let Some(request_id) = request.request_id {
            self.open_requests.insert(request_id.clone(), id.clone());
            self.asked.push(Asked {
                id: id.clone(),
                request_id,
                input: body.input.clone().unwrap_or(serde_json::Value::Null),
            });
        }
        let tool = body.tool_name.unwrap_or_default();
        let proposal = body
            .input
            .as_ref()
            .and_then(|input| self.proposal_of(&tool, input));
        let reason = asking_reason(body.decision_reason, body.blocked_path);
        let target = body
            .input
            .as_ref()
            .and_then(|input| rule_target(&tool, input));
        out.push(Event::PermissionRequest {
            id,
            tool,
            input: body.input.as_ref().map(render).unwrap_or_default(),
            target,
            proposal,
            reason,
            agent,
        });
    }

    /// What a call to the CLI's tool `tool` would do, read from its `input`,
    /// for the tools whose arguments are known here: `Edit`, `Write`, `Bash`
    /// and `ExitPlanMode`. `None` for any other tool, and for arguments not of the shape
    /// the tool takes, which the question then shows as they are — but for
    /// `ExitPlanMode`, which is a plan however its arguments are shaped.
    fn proposal_of(&self, tool: &str, input: &serde_json::Value) -> Option<Proposal> {
        let text = |key: &str| input.get(key)?.as_str().map(str::to_owned);
        match tool {
            "Edit" => {
                let every = match input.get("replace_all") {
                    None => false,
                    Some(every) => every.as_bool()?,
                };
                let action = Action::Replace {
                    path: text("file_path")?,
                    old: text("old_string")?,
                    new: text("new_string")?,
                    every,
                };
                Some(Proposal::new(
                    action,
                    ["file_path", "old_string", "new_string", "replace_all"],
                ))
            }
            "Write" => {
                let path = text("file_path")?;
                let action = Action::Write {
                    existed: self.existed(&path),
                    path,
                    content: text("content")?,
                };
                Some(Proposal::new(action, ["file_path", "content"]))
            }
            "Bash" => Some(Proposal::new(
                Action::Command {
                    command: text("command")?,
                },
                ["command"],
            )),
            // The CLI fills `plan` in from the plan file and asks without it
            // where the agent wrote none. That is still a plan going ahead,
            // with nothing to read: proposed as one, so that no standing
            // answer is offered that would let every later plan through.
            "ExitPlanMode" => Some(match text("plan") {
                Some(plan) => Proposal::new(Action::Plan { plan }, ["plan"]),
                None => Proposal::new(
                    Action::Plan {
                        plan: String::new(),
                    },
                    [] as [&str; 0],
                ),
            }),
            _ => None,
        }
    }

    /// Whether a file is at `path` — taken relative to where the session
    /// runs where it is not absolute — or `None` where nothing looks, the
    /// session's directory is not known, or the path starts with `~`.
    fn existed(&self, path: &str) -> Option<bool> {
        let exists = self.file_exists?;
        // `~` is the home directory to the CLI and a directory of this name
        // here: the file it would write is not one this can look for.
        if path == "~" || path.starts_with("~/") {
            return None;
        }
        let path = Path::new(path);
        match path.is_absolute() {
            true => exists(path),
            false => exists(&self.cwd.as_ref()?.join(path)),
        }
    }

    fn result(&mut self, outcome: wire::Outcome, out: &mut Vec<Event>) {
        let interrupted = outcome.interrupted();
        let shown_limit = self.usage_limit.take();
        let limit_words = limit_in(&outcome, shown_limit.as_deref());
        // The session's own reply still waiting for its count when the turn
        // ends was cut off, unless the turn ends on the operator's interrupt,
        // which is what stopped it. A sub-agent's is left: one launched in the
        // background goes on writing after the turn that launched it ends.
        if interrupted {
            self.stopped(&None);
        }
        if let Some(left) = self.in_flight.remove(&None) {
            self.cut_off(&None, left, CarriedOn::No, out);
        }
        self.reconcile_turn(outcome.usage.as_ref(), out);
        self.report_billing(Some(&outcome.model_usage), out);
        self.attribute_held(&outcome.model_usage, out);
        self.report_cost(&outcome, out);
        self.learn_windows(&outcome.model_usage, out);

        // The closing line lists every call the turn refused, including the
        // ones already reported as they happened. Anything left is a refusal
        // the CLI did not announce — recorded here, late, rather than lost.
        for denial in outcome.permission_denials {
            let id = denial.tool_use_id.unwrap_or_default();
            if self.denied.insert(id.clone(), None).is_some() {
                continue;
            }
            let agent = self
                .tool_calls
                .get(&id)
                .and_then(|call| call.agent.clone())
                .or_else(|| self.ended_agents.get(&id).cloned());
            let id = ToolCallId::new(id);
            let tool = denial.tool_name.unwrap_or_default();
            let target = denial
                .tool_input
                .as_ref()
                .and_then(|input| rule_target(&tool, input));
            out.push(Event::PermissionRequest {
                id: id.clone(),
                tool,
                input: denial.tool_input.as_ref().map(render).unwrap_or_default(),
                target,
                proposal: None,
                reason: None,
                agent,
            });
            out.push(Event::PermissionResponse {
                id,
                decision: PermissionDecision::Deny,
                message: None,
            });
        }
        self.ended_agents.clear();

        if interrupted {
            // The CLI reports an interrupt as an error with a diagnostic for
            // its own developers; to the operator it is the stop they asked
            // for, and the session goes on.
            out.push(Event::Notice {
                message: STOPPED_TURN.to_owned(),
            });
        } else if let Some(words) = limit_words {
            // The turn ended on the plan's usage limit. The session has said
            // so already when the CLI sent the message; a result that repeats
            // it as a failure would show the same event as an error.
            if shown_limit.is_none() {
                out.push(Event::LimitReached { message: words });
            }
        } else if outcome.is_error || outcome.subtype.as_deref() != Some("success") {
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
            // A window of nothing is no window: it would override the table's.
            if let Some(window) = usage.context_window.filter(|window| *window > 0) {
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
        let summed_1h = self.turn_1h.replace(0);
        let unfinished = std::mem::take(&mut self.unfinished);
        let Some(usage) = usage else {
            return;
        };
        let reported = Counts::from(usage);
        if reported.is_empty() {
            return;
        }
        let reported_1h = usage.cache_write_1h();
        // The split is compared only where both sides gave one: a CLI that
        // reports no split has said nothing that could disagree.
        let split_differs = summed_1h
            .zip(reported_1h)
            .is_some_and(|(summed, reported)| summed != reported);
        if reported != summed || split_differs {
            out.push(Event::TurnTotalDiffers {
                per_message: TokenCounts {
                    cache_write_1h: summed_1h,
                    ..summed.into()
                },
                reported: TokenCounts {
                    cache_write_1h: reported_1h,
                    ..reported.into()
                },
                backend_session: self.backend_session.clone(),
                unfinished,
            });
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
        if std::mem::take(&mut self.restored_unknown) {
            self.take_as_reported(outcome, out);
            return;
        }
        let from = out.len();
        self.report_bill(outcome, out);
        let settled: Vec<String> = out[from..]
            .iter()
            .filter_map(|event| {
                let Event::Usage(usage) = event else {
                    return None;
                };
                (usage.settles_model == Some(true) && self.short.contains(&usage.model))
                    .then(|| usage.model.clone())
            })
            .collect();
        out.extend(settled.into_iter().map(short_by_unknown));
    }

    /// [`Translator::report_cost`] for a bill whose running totals hold
    /// nothing this side does not know.
    fn report_bill(&mut self, outcome: &wire::Outcome, out: &mut Vec<Event>) {
        if outcome.model_usage.is_empty() {
            self.report_session_cost(outcome, out);
            return;
        }
        let moved = self.file_messages_under_their_bill(&outcome.model_usage);

        let mut costs = 0.0;
        let mut unmeant = false;
        let mut settled = Vec::new();
        for (model, usage) in &outcome.model_usage {
            if let Some(basis) = &usage.cost_basis
                && basis != "list"
            {
                out.push(warn(format!(
                    "the CLI priced {model} on a basis this version of Niobe does not know \
                     (`{basis}`). Its figure is kept as the CLI reported it."
                )));
            }
            // A cost no bill can be is left out, and the model's last
            // readable cost stays what the next one is measured from: the
            // figure is a running total, so the next one still holds this
            // turn's spend.
            let cost_now = match &usage.cost_usd {
                Some(wire::Cost::Said(usd)) => Some(*usd),
                Some(wire::Cost::Unmeant(text)) => {
                    out.push(unmeant_cost(model, text));
                    unmeant = true;
                    None
                }
                None => Some(0.0),
            };
            costs += cost_now.unwrap_or_default();

            let seen = self.reported.entry(model.clone()).or_default();
            let tokens = seen.tokens.beyond(Counts::from(usage));
            let spent = cost_now.map_or(0.0, |now| now - seen.cost_usd);
            if tokens.is_empty() && spent <= 0.0 {
                continue;
            }
            seen.tokens.add(tokens);
            let Some(now) = cost_now else {
                out.push(self.uncosted_record(model.clone(), tokens));
                continue;
            };
            seen.cost_usd = now;
            if spent > 0.0 {
                settled.push(model.clone());
            }
            out.push(self.cost_record(model.clone(), tokens, spent));
        }
        for model in moved.into_iter().filter(|model| !settled.contains(model)) {
            out.push(covered_elsewhere(model));
        }

        let total = match &outcome.total_cost_usd {
            Some(wire::Cost::Said(total)) => Some(*total),
            Some(wire::Cost::Unmeant(text)) => {
                out.push(unmeant_cost("the session", text));
                None
            }
            None => None,
        };
        if let Some(total) = total
            && !unmeant
            && (total - costs).abs() > COST_TOLERANCE_USD
        {
            out.push(warn(format!(
                "the CLI's per-model costs add up to ${costs:.6}, and it reported \
                 ${total:.6} for the session. The per-model figures are what was counted."
            )));
        }
    }

    /// Counts the running totals `outcome` states as already reported,
    /// reporting none of them: the first bill of a resumed session whose
    /// earlier spend is unknown. See [`Translator::resuming`].
    ///
    /// A bill holding a cost no bill can be is no base to measure from, so it
    /// is passed over and the next bill is taken as the base instead: the
    /// turn between goes uncounted, as the first one does, and the session's
    /// figure stays the floor it already is.
    fn take_as_reported(&mut self, outcome: &wire::Outcome, out: &mut Vec<Event>) {
        let costs = outcome
            .model_usage
            .iter()
            .map(|(model, usage)| (model.as_str(), usage.cost_usd.as_ref()));
        let session = outcome
            .model_usage
            .is_empty()
            .then_some(("the session", outcome.total_cost_usd.as_ref()));
        let unmeant: Vec<Event> = costs
            .chain(session)
            .filter_map(|(what, cost)| match cost {
                Some(wire::Cost::Unmeant(text)) => Some(unmeant_cost(what, text)),
                Some(wire::Cost::Said(_)) | None => None,
            })
            .collect();
        if !unmeant.is_empty() {
            out.extend(unmeant);
            self.restored_unknown = true;
            return;
        }
        for (model, usage) in &outcome.model_usage {
            let seen = self.reported.entry(model.clone()).or_default();
            seen.tokens = Counts::from(usage);
            seen.cost_usd = usage
                .cost_usd
                .as_ref()
                .and_then(wire::Cost::usd)
                .unwrap_or_default();
            self.short.insert(model.clone());
        }
        if outcome.model_usage.is_empty()
            && let Some(total) = outcome.total_cost_usd.as_ref().and_then(wire::Cost::usd)
        {
            let model = self.model.clone().unwrap_or_default();
            self.reported.entry(model.clone()).or_default().cost_usd = total;
            self.short.insert(model);
        }
    }

    /// Reports a message's usage under `model`, counted as filed there.
    fn file(&mut self, model: String, mut record: Usage, out: &mut Vec<Event>) {
        let counts = Counts {
            input: record.input,
            output: record.output,
            cache_read: record.cache_read,
            cache_write: record.cache_write,
        };
        self.reported
            .entry(model.clone())
            .or_default()
            .tokens
            .add(counts);
        record.model = model;
        out.push(Event::Usage(record));
    }

    /// Files the messages no model was named for under the id the turn's
    /// bill names, where it names exactly one — or, where the `result`
    /// carries no per-model bill, under the model the session has since
    /// been named on. Where neither names one, they stay held: the CLI's
    /// bill is a running total, so a later one still carries them, and filing
    /// them before it is reconciled keeps them from being counted twice.
    ///
    /// Where the bill names several ids, nothing says which one a message
    /// was spent on. Its tokens are then counted from the bill alone, which
    /// reports every token no message was filed for, so they are counted
    /// once and at the right price; the operator is told why the per-message
    /// records do not carry them.
    fn attribute_held(
        &mut self,
        model_usage: &BTreeMap<String, wire::ModelUsage>,
        out: &mut Vec<Event>,
    ) {
        if self.unattributed.is_empty() {
            return;
        }
        let billed = match model_usage.len() {
            0 => match self.model.clone() {
                Some(model) => Some(model),
                None => return,
            },
            1 => model_usage.keys().next().cloned(),
            _ => None,
        };
        let held = std::mem::take(&mut self.unattributed);
        let Some(model) = billed else {
            let tokens = held.iter().fold(0u64, |sum, record| {
                sum.saturating_add(record.input)
                    .saturating_add(record.output)
                    .saturating_add(record.cache_read)
                    .saturating_add(record.cache_write)
            });
            out.push(warn(format!(
                "{tokens} tokens arrived in messages that named no model, and the turn's \
                 bill names {} models. They are counted from the bill's per-model totals \
                 instead of from the messages.",
                model_usage.len()
            )));
            return;
        };
        for record in held {
            self.file(model.clone(), record, out);
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
    ///
    /// The total is the session's, so what it adds is measured against all
    /// that has been reported under every model, not under the current one:
    /// after a move between models, or a bill split across several, the
    /// current model's share alone would count the others' again. A
    /// difference within [`COST_TOLERANCE_USD`] is the sum's rounding rather
    /// than spend, and is left for the next total, which still holds it.
    fn report_session_cost(&mut self, outcome: &wire::Outcome, out: &mut Vec<Event>) {
        let total = match &outcome.total_cost_usd {
            Some(wire::Cost::Said(total)) => *total,
            // Left out, and not the base the next total is measured from.
            Some(wire::Cost::Unmeant(text)) => {
                out.push(unmeant_cost("the session", text));
                return;
            }
            None => return,
        };
        let reported: f64 = self.reported.values().map(|seen| seen.cost_usd).sum();
        let spent = total - reported;
        if spent <= COST_TOLERANCE_USD {
            return;
        }
        let model = self.model.clone().unwrap_or_default();
        self.reported.entry(model.clone()).or_default().cost_usd += spent;
        out.push(self.cost_record(model, Counts::default(), spent));
    }

    /// A usage record carrying what a turn cost, and whatever tokens the
    /// per-message records had not already accounted for.
    ///
    /// The CLI computes every figure it prints from published list prices,
    /// whatever login is behind it. On a metered account that is taken as the
    /// bill, the provider charging those prices; on a plan no money moves with
    /// the work, and the shell shows no figure at all, so nothing here needs
    /// to say which the record is: the session's billing does.
    /// A usage record for tokens the bill reported beside a cost no bill can
    /// be: no money, and settling nothing, so they stay owed rather than
    /// read as covered.
    fn uncosted_record(&self, model: String, tokens: Counts) -> Event {
        Event::Usage(Usage {
            input: tokens.input,
            output: tokens.output,
            cache_read: tokens.cache_read,
            cache_write: tokens.cache_write,
            // As in `cost_record`: `modelUsage` splits no cache write by
            // lifetime.
            cache_write_1h: (tokens.cache_write == 0).then_some(0),
            reasoning: 0,
            model,
            cost_usd: None,
            settles_model: Some(false),
            fast: false,
        })
    }

    fn cost_record(&self, model: String, tokens: Counts, spent: f64) -> Event {
        Event::Usage(Usage {
            input: tokens.input,
            output: tokens.output,
            cache_read: tokens.cache_read,
            cache_write: tokens.cache_write,
            // `modelUsage` reports no lifetime split for its cache writes, so
            // writes counted only there were bought for a lifetime nobody
            // said: none for the hour would read them all as five-minute
            // ones. A record with no writes splits nothing it could misstate.
            cache_write_1h: (tokens.cache_write == 0).then_some(0),
            reasoning: 0,
            model,
            // A difference of zero is not a report that the turn was free.
            cost_usd: (spent > 0.0).then_some(spent),
            // `spent` is what the CLI has billed under this model less what it
            // had already billed, and the tokens above are those no message
            // reported. Between them the two cover every record emitted under
            // the model so far, so this figure settles them.
            settles_model: Some(true),
            fast: false,
        })
    }
}

/// What the operator is told where the CLI stated a cost no bill can be:
/// `what` is the model or the session it was stated for, `text` the value as
/// written.
fn unmeant_cost(what: &str, text: &str) -> Event {
    warn(format!(
        "the CLI reported {what} as costing `{text}`, which no bill can be, so no cost was \
         counted from it. The next cost it reports is measured from the last one that could be."
    ))
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
        cache_write_1h: Some(0),
        reasoning: 0,
        model,
        cost_usd: Some(0.0),
        settles_model: Some(true),
        fast: false,
    })
}

/// A record of nothing under `model` that no cost covers, put after each
/// settlement of a model whose first resumed bill went uncounted.
///
/// A settlement covers every record of its model folded before it, the
/// uncounted turn's among them, and would leave the session's figure reading
/// as whole while it is short by that turn. Owing for one empty record keeps
/// it a floor until the next settlement, which this follows again.
fn short_by_unknown(model: String) -> Event {
    Event::Usage(Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: Some(0),
        reasoning: 0,
        model,
        cost_usd: None,
        settles_model: Some(false),
        fast: false,
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
/// One of the two requests a rewind is made of.
#[derive(Debug)]
enum Rewind {
    /// The dry run: the request that puts the files back once it says they
    /// can go, the id of the prompt they go back to before, and how that
    /// prompt is named.
    Dry {
        real: String,
        prompt_id: String,
        prompt: String,
    },
    /// The request that puts the files back, with how the prompt is named
    /// and what the dry run said would go back.
    Real { prompt: String, would: Would },
}

/// What a dry run of a rewind said would go back.
#[derive(Debug)]
struct Would {
    /// The files, as the operator reads their paths.
    files: Vec<String>,
    insertions: Option<u64>,
    deletions: Option<u64>,
}

/// `rewind_files`, asking the CLI to put the files back as they were before
/// the prompt it was sent as `prompt_id`, or with `dry_run` only to say what
/// that would change.
pub(crate) fn rewind_request(
    request_id: &str,
    prompt_id: &str,
    dry_run: bool,
) -> serde_json::Value {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {
            "subtype": "rewind_files",
            "user_message_id": prompt_id,
            "dry_run": dry_run,
        },
    })
}

/// What the session is told once the CLI has put the files back as they
/// were before `prompt`: which went back, or that none had changed.
fn rewound(prompt: &str, would: &Would) -> Event {
    let message = match would.files.is_empty() {
        true => format!("No file had changed since \"{prompt}\", so none was put back."),
        false => {
            let lines = match (would.insertions, would.deletions) {
                (Some(added), Some(removed)) => format!(" (+{added} −{removed})"),
                _ => String::new(),
            };
            format!(
                "Files put back as they were before \"{prompt}\": {}{lines}. The \
                 conversation keeps everything said since.",
                would.files.join(", ")
            )
        }
    };
    Event::Notice { message }
}

/// What the session is told where the CLI would not put the files back as
/// they were before `prompt`, with its reason where it gave one.
fn not_rewound(prompt: &str, reason: Option<String>) -> Event {
    warn(format!(
        "no files were put back as they were before \"{prompt}\": {}",
        reason.as_deref().unwrap_or("the CLI gave no reason")
    ))
}

/// The model the CLI names on a message it wrote itself rather than asked a
/// model for: the answer to a local command such as `/cost`, or a refusal.
const CLI_WRITTEN: &str = "<synthetic>";

/// `text` with each of its lines kept where `plain` says the CLI wrote it.
///
/// The shell draws a reply as markdown, where a single line break joins two
/// lines into one paragraph. The CLI's own text is plain, laid out a line a
/// figure (`/cost` puts each usage window on a line of its own), so each such
/// break is made a markdown hard break. A fenced block already keeps its
/// lines, and a break beside a blank line is a paragraph's end, so neither is
/// touched. A model's reply is left as the model wrote it.
fn lines_kept_where(plain: bool, text: String) -> String {
    if !plain {
        return text;
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut kept = String::with_capacity(text.len() + lines.len() * 2);
    let mut fenced = false;
    for (at, line) in lines.iter().enumerate() {
        kept.push_str(line);
        let Some(next) = lines.get(at + 1) else {
            break;
        };
        let fence = ["```", "~~~"]
            .iter()
            .any(|mark| line.trim_start().starts_with(mark));
        if fence {
            fenced = !fenced;
        } else if !fenced && !line.trim().is_empty() && !next.trim().is_empty() {
            kept.push_str("  ");
        }
        kept.push('\n');
    }
    kept
}

/// The warning that a change Niobe asked for was refused, in the CLI's words.
fn refused_change(outcome: &wire::ControlOutcome) -> Event {
    warn(format!(
        "the CLI refused a change Niobe asked for, so the session is running as it was: {}",
        outcome.error.as_deref().unwrap_or("the CLI gave no reason")
    ))
}

pub(crate) fn read_mode(mode: &str) -> Option<Mode> {
    match mode {
        "plan" => Some(Mode::Plan),
        "default" | "manual" => Some(Mode::Ask),
        "auto" => Some(Mode::Auto),
        _ => None,
    }
}

/// What the CLI's answer to a request for the session's title says.
///
/// A title is the session's caption from then on. None — the CLI makes none
/// for a prompt too short to name or one that is a command — leaves the caption
/// on the first prompt, which is nothing to say. A refusal is said as a notice
/// rather than a warning: nothing the session runs with was changed or kept
/// from changing, and the caption is still the first prompt.
fn titled(outcome: wire::ControlOutcome) -> Option<Event> {
    if outcome.subtype.as_deref() == Some("success") {
        let title = outcome.response?.title?;
        let title = title.trim();
        return (!title.is_empty()).then(|| Event::Titled {
            title: title.to_owned(),
        });
    }
    Some(Event::Notice {
        message: format!(
            "the CLI did not title the session, so it is captioned by its first prompt: {}",
            outcome.error.as_deref().unwrap_or("the CLI gave no reason")
        ),
    })
}

/// The compaction the CLI reports with `metadata`, on the live stream or in
/// its own transcript.
pub(crate) fn compacted(metadata: wire::CompactMetadata) -> Event {
    Event::Compacted {
        trigger: match metadata.trigger.as_deref() {
            Some("manual") => CompactTrigger::Manual,
            Some("auto") => CompactTrigger::Auto,
            _ => CompactTrigger::Unstated,
        },
        before: metadata.pre_tokens,
        after: metadata.post_tokens,
    }
}

/// The warning for a line of the CLI's that held half of a character, which
/// is shown as U+FFFD, as a byte of a line that is not UTF-8 is.
pub(crate) const HALF_A_CHARACTER: &str = "the CLI sent a line holding half of a character, cut where \
    the CLI cut a string; the half is shown as \u{fffd}.";

/// A non-fatal entry: something the session should show and go on from.
pub(crate) fn warn(message: String) -> Event {
    Event::Error {
        message,
        fatal: false,
    }
}

/// What the transcript says where a reply's stream was cut off before the CLI
/// said what the reply finished at: `name` is the message, where the stream
/// named it, and `counted` whether the CLI had recorded a count to take.
fn cut_off_notice(name: Option<&str>, counted: bool, carried_on: CarriedOn) -> String {
    let what = match carried_on {
        CarriedOn::Yes => {
            "The reply above was cut off before the CLI said it was finished, and \
                           the CLI is carrying the turn on from where it stopped."
        }
        CarriedOn::No => "The reply above was cut off before the CLI said it was finished.",
    };
    let count = match counted {
        true => {
            "Its tokens are counted from what the CLI recorded for it as it was written, \
                 which is what the CLI counts it at in the turn's total."
        }
        false => "The CLI recorded no count for it, so its tokens are not counted.",
    };
    match name {
        Some(name) => format!("{what} {count} Message {name}."),
        None => format!("{what} {count}"),
    }
}

/// What a resumed session says first where what it had spent before could
/// not be read, for the reason `why`.
fn unknown_spend(why: String) -> Event {
    Event::Notice {
        message: format!(
            "what this session had spent before it was resumed could not be read ({why}). \
             The CLI's first bill carries that earlier spend with the first turn's, so it is \
             not counted: the first turn's cost is left out, and its tokens are its \
             messages' own."
        ),
    }
}

/// Which retry of a failed request the CLI is making, and when, each as far as
/// the CLI said.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Retry {
    pub(crate) attempt: Option<u64>,
    pub(crate) of: Option<u64>,
    pub(crate) in_ms: Option<u64>,
}

/// The entry for a request that failed and that the CLI is making again:
/// what failed, in `failure`, and which retry this is.
///
/// An error entry, because the CLI files it as one (`level: "error"` in its
/// own transcript), and not the end of the session: the turn waits on the
/// retry, and if the retries run out it ends on the failure.
pub(crate) fn retrying(failure: &str, retry: Retry) -> Event {
    let mut message = format!("a request failed ({failure}), and the CLI is retrying it");
    let attempt = match (retry.attempt, retry.of) {
        (Some(attempt), Some(of)) => Some(format!("attempt {attempt} of {of}")),
        (Some(attempt), None) => Some(format!("attempt {attempt}")),
        (None, Some(_) | None) => None,
    };
    let delay = retry.in_ms.map(|ms| {
        let tenths = ms.saturating_add(50) / 100;
        format!("in {}.{} s", tenths / 10, tenths % 10)
    });
    let when: Vec<String> = attempt.into_iter().chain(delay).collect();
    if !when.is_empty() {
        message.push_str(": ");
        message.push_str(&when.join(", "));
    }
    warn(message)
}

/// The entry for a line the CLI shows its operator at `level`: a warning
/// where the CLI says `warning` or `error`, and a notice otherwise — `notice`
/// is the level recorded, and a level a later release adds is not an alarm
/// until it says so. A line with nothing in it is nothing to show.
pub(crate) fn informed(said: &str, level: Option<&str>) -> Option<Event> {
    if said.trim().is_empty() {
        return None;
    }
    let message = said.to_owned();
    Some(match level {
        Some("warning" | "error") => warn(message),
        _ => Event::Notice { message },
    })
}

/// The entry for a model that declined to answer where the CLI had no other
/// model to give the request to: a warning, as the CLI files it, since the
/// turn ends without the answer it asked for. The CLI's own sentence about it
/// follows as a reply.
pub(crate) fn declined(
    model: Option<&str>,
    category: Option<&str>,
    explanation: Option<&str>,
) -> Event {
    let who = model.map_or_else(|| "the model".to_owned(), |model| format!("`{model}`"));
    let mut message =
        format!("{who} declined to answer, and the CLI has no other model to give the request to");
    let why: Vec<&str> = [category, explanation]
        .into_iter()
        .flatten()
        .filter(|part| !part.trim().is_empty())
        .collect();
    if !why.is_empty() {
        message.push_str(": ");
        message.push_str(&why.join(" — "));
    }
    warn(message)
}

/// The entry for a shape this bridge has not read: visible, so that a shape a
/// new release added cannot pass unnoticed, and a notice, because the CLI did
/// not say that anything failed.
pub(crate) fn unread(message: String) -> Event {
    Event::Notice { message }
}

/// Takes the token counts and costs out of a message, at every place the
/// CLI puts them: a `result`'s bill, an `assistant` line's `message.usage`, a
/// `message_start`'s and a `message_delta`'s. Whether there were any.
///
/// Only those places: a tool call's arguments may hold a key of the same
/// name, and they are shown as the model wrote them.
fn strip_figures(message: &mut serde_json::Value) -> bool {
    const BILL: [&str; 3] = ["usage", "modelUsage", "total_cost_usd"];
    let mut removed = false;
    let mut take = |object: Option<&mut serde_json::Value>, keys: &[&str]| {
        if let Some(object) = object.and_then(serde_json::Value::as_object_mut) {
            for key in keys {
                removed |= object.remove(*key).is_some();
            }
        }
    };
    take(Some(&mut *message), &BILL);
    take(message.get_mut("message"), &["usage"]);
    take(message.get_mut("event"), &["usage"]);
    take(
        message
            .get_mut("event")
            .and_then(|event| event.get_mut("message")),
        &["usage"],
    );
    removed
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

/// The directory under the CLI's configuration directory it keeps one
/// directory per project in, and the one in each of those that holds the
/// agent's notes on that project.
const PROJECTS_DIR: &str = "projects";
const MEMORY_DIR: &str = "memory";

/// Whether `name` is a working directory as the CLI flattens one into a
/// directory name: an absolute path, so it starts with the `-` its leading
/// `/` became, and nothing but ASCII letters, digits and dashes after.
fn is_flattened(name: &str) -> bool {
    name.starts_with('-') && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
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
        ToolOutcome::Denied | ToolOutcome::Interrupted => None,
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
        ToolOutcome::Denied | ToolOutcome::Interrupted => false,
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

/// The whole of what a shell command printed, where Niobe's hook trimmed what
/// the model read of it: the file the hook names in the result, read with
/// `read` as long as it holds the bytes the hook said it wrote. Only a file
/// named `<id>.txt` in a `.niobe/trim` directory is read, which is where the
/// hook writes, so a result naming any other path reads nothing.
fn untrimmed(reported: Option<&serde_json::Value>, read: Option<ReadSpilled>) -> Option<String> {
    let report = serde_json::from_value::<wire::ShellReport>(reported?.clone()).ok()?;
    let path = PathBuf::from(report.niobe_raw_output_path?);
    let dir = path.parent()?;
    let kept = path.extension()? == "txt"
        && dir.file_name()? == "trim"
        && dir.parent()?.file_name()? == ".niobe";
    if !kept {
        return None;
    }
    read?(&path, report.niobe_raw_output_size?)
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

/// What a `Write` over a file that was already there changed: the reported
/// patch's counts, nothing where the CLI reported an update with no hunks —
/// the file was written as it stood — and `None` where no usable patch says.
fn overwrite_changed(
    reported: Option<&serde_json::Value>,
    path: &str,
    hunks: &[Hunk],
) -> Option<(u64, u64)> {
    if !hunks.is_empty() {
        return diff::hunks_changed(hunks);
    }
    file_report(reported, path)
        .filter(|report| report.kind.as_deref() == Some("update"))
        .filter(|report| report.structured_patch.is_empty())
        .map(|_| (0, 0))
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
/// The CLI's own tools whose arguments beside the target say how the call
/// runs rather than what it reaches, read from their schemas: a shell
/// command's description and timeout, an edit's strings, a read's line
/// range, a fetch's prompt.
const CLI_TARGETED_TOOLS: [&str; 11] = [
    "Bash",
    "PowerShell",
    "Read",
    "Edit",
    "MultiEdit",
    "Write",
    "NotebookEdit",
    "Glob",
    "Grep",
    "LS",
    "WebFetch",
];

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

/// The target a standing answer to a prompt is written about and matched
/// against, which is the call's target unless the call also asks for
/// something a rule about that target does not say.
///
/// A rule names a tool and one target; the call's other arguments are not in
/// it. Most of them change how a command runs rather than what it may reach,
/// but a shell command that asks to leave the CLI's sandbox reaches what the
/// same command inside it cannot, and `Bash(cargo test)` was not written
/// about that. Such a call has no target, so only a rule for the whole tool
/// answers it and the operator is asked otherwise.
///
/// The same holds for a tool whose schema is not the CLI's own — an MCP
/// server's, or one added since — where nothing here knows what the other
/// arguments do: `{"command":"git","args":["push","--force"]}` is not `git`,
/// and a `path` beside an `owner` and a `repo` is a file in that repository.
/// Its target is named only where it is the call's one argument.
fn rule_target(tool: &str, input: &serde_json::Value) -> Option<String> {
    let unsandboxed =
        input.get("dangerouslyDisableSandbox") == Some(&serde_json::Value::Bool(true));
    let unread = !CLI_TARGETED_TOOLS.contains(&tool)
        && input.as_object().is_some_and(|fields| fields.len() != 1);
    match unsandboxed || unread {
        true => None,
        false => target_of(input),
    }
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
                wire::Block::ToolUse { .. }
                | wire::Block::ToolResult { .. }
                | wire::Block::Other => None,
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

/// Whether a shell call's report says the CLI moved its command to the
/// background, so that its result is of its launch and its end comes later.
fn moved_to_background(reported: Option<&serde_json::Value>) -> bool {
    reported
        .and_then(|report| serde_json::from_value::<wire::ShellReport>(report.clone()).ok())
        .is_some_and(|report| report.background_task_id.is_some())
}

/// The notice that a shell command the CLI ran in the background ended, with
/// `status` and `summary` as its notification spells them.
///
/// A notice, and not an error, whatever the status: the command failing is
/// the command's outcome, which the model reads and answers as it does a
/// failed call, and the session goes on. The exit code is the CLI's, read out
/// of its summary (`… failed with exit code 3`), since the notification
/// carries none of its own.
fn background_ended(command: &str, status: Option<&str>, summary: Option<&str>) -> Event {
    let how = match status {
        Some("completed") => "finished".to_owned(),
        Some("failed") => "failed".to_owned(),
        Some("stopped" | "killed") => "was stopped".to_owned(),
        Some(other) => format!("ended as `{other}`"),
        None => "ended".to_owned(),
    };
    let code = summary
        .and_then(exit_code_in)
        .map(|code| format!(" with exit code {code}"))
        .unwrap_or_default();
    let mut lines = command.lines();
    let first = lines.next().unwrap_or_default().trim();
    let more = match lines.next() {
        Some(_) => " …",
        None => "",
    };
    Event::Notice {
        message: format!("the background command `{first}{more}` {how}{code}"),
    }
}

/// The status the CLI's summary of a background command names, after its
/// `exit code`.
fn exit_code_in(summary: &str) -> Option<i32> {
    let (_, after) = summary.rsplit_once("exit code ")?;
    let digits: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
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

/// The kind of agent a sub-agent call asked for, where it named one.
fn agent_kind(input: &serde_json::Value) -> Option<String> {
    input
        .get("subagent_type")
        .and_then(serde_json::Value::as_str)
        .filter(|kind| !kind.trim().is_empty())
        .map(str::to_owned)
}

/// The CLI's slash commands, as the event that replaces the list before them.
fn listed_as_offered(commands: Vec<wire::Command>, unavailable: &[String]) -> Event {
    Event::Commands {
        commands: commands
            .into_iter()
            .filter(|command| !unavailable.contains(&command.name))
            .map(|command| SlashCommand {
                mid_prompt: honoured_mid_prompt(&command),
                name: command.name,
                description: command.description,
                argument_hint: command.argument_hint.filter(|hint| !hint.trim().is_empty()),
            })
            .collect(),
    }
}

/// The MCP servers `init` lists, each with the tools of the session's that
/// are its own.
fn mcp_servers(servers: Vec<wire::McpServer>, tools: &[String]) -> Vec<McpServer> {
    servers
        .into_iter()
        .map(|server| McpServer {
            tools: server_tools(&server.name, tools),
            status: mcp_status(server.status),
            source: said(server.source),
            name: server.name,
            scope: None,
            error: None,
            endpoint: None,
            reports_as: None,
        })
        .collect()
}

/// One server as the answer to `mcp_status` has it.
fn mcp_server_status(server: wire::McpServerStatus) -> McpServer {
    McpServer {
        status: mcp_status(server.status),
        source: said(server.source),
        tools: server
            .tools
            .unwrap_or_default()
            .into_iter()
            .map(|tool| tool.name)
            .filter(|name| !name.is_empty())
            .collect(),
        scope: said(server.scope),
        error: said(server.error),
        endpoint: server.config.and_then(endpoint),
        reports_as: server.server_info.and_then(|info| {
            let named = format!(
                "{} {}",
                info.name.unwrap_or_default(),
                info.version.unwrap_or_default()
            );
            said(Some(named.trim().to_owned()))
        }),
        name: server.name,
    }
}

/// Why the CLI asked about a call, from the reason it gave and the path it
/// stopped at: its words, with the path after them where it named one.
fn asking_reason(reason: Option<String>, blocked_path: Option<String>) -> Option<String> {
    match (said(reason), said(blocked_path)) {
        (Some(reason), Some(path)) => Some(format!("{reason} ({path})")),
        (Some(reason), None) => Some(reason),
        (None, Some(path)) => Some(format!("blocked path: {path}")),
        (None, None) => None,
    }
}

/// What a field the CLI wrote empty, or left out, says: nothing.
fn said(text: Option<String>) -> Option<String> {
    text.filter(|text| !text.trim().is_empty())
}

/// How a server is reached, from its configuration: its transport and the
/// program run or the address called. A program's arguments and environment
/// and an address's credentials and query are left out, being where a
/// configuration keeps an API key, so the record of the session holds none.
fn endpoint(config: wire::McpConfig) -> Option<String> {
    let transport = said(config.transport);
    let target = match (said(config.command), said(config.url)) {
        (Some(command), _) => command,
        (None, Some(url)) => without_secrets(&url),
        (None, None) => return transport,
    };
    // A configuration with a command and no `type` is a program run over
    // standard input and output, which is what the CLI takes it for.
    let transport = transport.unwrap_or_else(|| "stdio".to_owned());
    Some(format!("{transport} · {target}"))
}

/// `url` as scheme, host and port alone, with a `/…` where there was a path.
///
/// Hosted servers keep the API key in the path (`/mcp/<key>/sse`), in a
/// `;param` after it or in a user and password before the host, so the path
/// is not kept at all: what the operator needs to tell servers apart is the
/// host. A user and password end at the last `@` of the authority, since a
/// password may hold one, and are dropped with or without a scheme before
/// them — an address that names none reads as a host all the same.
fn without_secrets(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, url),
    };
    let end = rest.find(['/', '?', '#', ';']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let path = match tail.trim_start_matches('/').is_empty() || tail.starts_with(['?', '#', ';']) {
        true => "",
        false => "/…",
    };
    match scheme {
        Some(scheme) => format!("{scheme}://{host}{path}"),
        None => format!("{host}{path}"),
    }
}

/// The tools of `server`'s among the session's: those named
/// `mcp__<server>__<tool>`, where the server's name has every character but
/// an ASCII letter, a digit, `_` and `-` written as `_`, as the CLI names
/// them — `claude.ai Notion` gives `mcp__claude_ai_Notion__…`.
fn server_tools(server: &str, tools: &[String]) -> Vec<String> {
    let named: String = server
        .chars()
        .map(
            |c| match c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                true => c,
                false => '_',
            },
        )
        .collect();
    let prefix = format!("mcp__{named}__");
    tools
        .iter()
        .filter_map(|tool| tool.strip_prefix(&prefix))
        .filter(|tool| !tool.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A server's status, in the CLI's spelling.
fn mcp_status(status: String) -> McpStatus {
    match status.as_str() {
        // An entry that named no status, or not as text.
        "" => McpStatus::Other("not reported".to_owned()),
        "connected" => McpStatus::Connected,
        "pending" => McpStatus::Pending,
        "needs-auth" => McpStatus::NeedsAuth,
        "failed" => McpStatus::Failed,
        "disabled" => McpStatus::Disabled,
        _ => McpStatus::Other(status),
    }
}

/// The models the CLI offers, as the event that replaces the list before
/// them. A name or a description the CLI wrote empty is one it did not give.
fn offered_models(models: Vec<wire::Model>) -> Event {
    let given = |text: Option<String>| text.filter(|text| !text.trim().is_empty());
    Event::Models {
        models: models
            .into_iter()
            .filter(|model| !model.value.trim().is_empty())
            .map(|model| ModelOption {
                id: model.value,
                name: given(model.display_name),
                description: given(model.description),
                resolves_to: given(model.resolved_model),
                auto_mode: model.supports_auto_mode,
            })
            .collect(),
    }
}

/// Whether the CLI takes `command` named anywhere in a prompt, rather than
/// only where the prompt opens with it.
///
/// Recorded in `tests/fixtures/skill-mid-prompt.jsonl`: a skill and a command
/// of the repository's own, each named mid-prompt, were run by the model
/// through its `Skill` tool. What the CLI ships itself is left out, though
/// the skills it bundles would be taken too: it marks them `builtin` just as
/// it marks `/compact`, which the model cannot run, and the one list that
/// tells a skill from a command (`skills` on `init`) arrives only once the
/// first turn starts. An MCP server's prompt, named `<server>:<prompt> (MCP)`,
/// is the CLI's to expand at the start of a prompt and is no skill.
fn honoured_mid_prompt(command: &wire::Command) -> bool {
    !command.builtin && !command.name.ends_with(" (MCP)")
}

/// The usage limit a closing `result` ended its turn on, if it did: the words
/// the session already showed, or, where none arrived as a message, the
/// result's own where they open the way the CLI words the limit.
fn limit_in(outcome: &wire::Outcome, shown: Option<&str>) -> Option<String> {
    if !outcome.is_error {
        return None;
    }
    let result = outcome.result.as_deref().unwrap_or_default();
    match shown {
        Some(words) => (outcome.errors.is_empty() && (result.is_empty() || result == words))
            .then(|| words.to_owned()),
        None => (outcome.errors.is_empty() && wire::is_usage_limit_text(result))
            .then(|| result.to_owned()),
    }
}

/// The windows one `rate_limit_event` reported.
///
/// A free function rather than a method: the message says what is left of the
/// plan, which is a level the CLI measured and not something the translator
/// accumulates, so there is no state for it to touch. A message with no window
/// in it produces nothing — a window reported as zero would claim an untouched
/// plan, which is a different thing from a CLI that said nothing — unless it
/// says the plan is spending beyond its fee, which is reported on its own.
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
        overage: overage(&info.overage_status, &info.overage_disabled_reason),
    };
    if windows.is_empty() && !windows.using_overage && windows.overage == Overage::Unreported {
        return;
    }
    out.push(Event::UsageWindows(windows));
}

/// Whether the plan may spend beyond its fee, read from the CLI's
/// `overageStatus` and `overageDisabledReason`, whose values are the ones
/// Claude Code 2.1.309's schema lists.
///
/// A status this does not know is the CLI not having said anything this
/// bridge can stand behind, never a guess at which way it went; a reason it
/// does not know still leaves the refusal, with no reason.
fn overage(status: &str, reason: &str) -> Overage {
    match status {
        "allowed" | "allowed_warning" => Overage::Allowed,
        "rejected" => Overage::Refused(match reason {
            "out_of_credits" => OverageRefusal::OutOfCredits,
            "overage_not_provisioned" | "no_limits_configured" => OverageRefusal::NotSetUp,
            "org_level_disabled"
            | "org_level_disabled_until"
            | "org_service_level_disabled"
            | "seat_tier_level_disabled"
            | "member_level_disabled" => OverageRefusal::TurnedOff,
            "seat_tier_zero_credit_limit"
            | "group_zero_credit_limit"
            | "member_zero_credit_limit" => OverageRefusal::ZeroLimit,
            _ => OverageRefusal::Unstated,
        }),
        _ => Overage::Unreported,
    }
}

/// One window, kept only where the CLI measured it: a level that is not a
/// finite number is not a measurement, and could not be stored as one.
fn read_window(window: Option<wire::Window>) -> Option<UsageWindow> {
    let window = window?;
    Some(UsageWindow {
        utilization: window.utilization.filter(|level| level.is_finite())?,
        resets_at: window.resets_at,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use niobe_core::test_run::TestCounts;

    use super::*;

    #[test]
    fn whether_a_model_runs_auto_is_what_the_cli_said_and_unknown_where_it_did_not() {
        let model = |value: &str, supports: Option<bool>| wire::Model {
            value: value.to_owned(),
            display_name: None,
            description: None,
            resolved_model: None,
            supports_auto_mode: supports,
        };

        let offered = offered_models(vec![
            model("opus", Some(true)),
            model("haiku", Some(false)),
            model("sonnet", None),
        ]);

        let Event::Models { models } = offered else {
            panic!("models were offered");
        };
        let said: Vec<Option<bool>> = models.iter().map(|model| model.auto_mode).collect();
        assert_eq!(said, [Some(true), Some(false), None]);
    }

    /// A name, a description or a model the CLI wrote empty or blank is one
    /// it did not give, and a model with no value to ask for it by is none.
    #[test]
    fn a_model_offered_with_blank_words_has_them_left_out_and_one_with_no_value_is_not_offered() {
        let model = |value: &str, words: &str| wire::Model {
            value: value.to_owned(),
            display_name: Some(words.to_owned()),
            description: Some(words.to_owned()),
            resolved_model: Some(words.to_owned()),
            supports_auto_mode: None,
        };
        let offered = offered_models(vec![
            model(" ", "Opus"),
            model("opus", " "),
            model("haiku", ""),
        ]);
        assert_eq!(
            offered,
            Event::Models {
                models: ["opus", "haiku"]
                    .map(|id| ModelOption {
                        id: id.to_owned(),
                        name: None,
                        description: None,
                        resolves_to: None,
                        auto_mode: None,
                    })
                    .to_vec()
            }
        );
    }

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

        assert!(warnings(&events).is_empty(), "{events:?}");
        let differs = differences(&events);
        assert_eq!(differs.len(), 1, "{events:?}");
        let (per_message, reported, _, _) = differs[0];
        assert_eq!((per_message.input, per_message.output), (2, 10));
        assert_eq!((reported.input, reported.output), (2, 99));
        assert!(
            events.iter().all(|event| !matches!(event, Event::Usage(_))),
            "a mismatch invented a record to close the gap"
        );
    }

    fn written_for_an_hour(write: u64, hour: u64) -> String {
        format!(
            r#"{{"type":"stream_event","event":{{"type":"message_delta","usage":{{"input_tokens":2,"output_tokens":10,"cache_creation_input_tokens":{write},"cache_creation":{{"ephemeral_5m_input_tokens":{},"ephemeral_1h_input_tokens":{hour}}}}}}}}}"#,
            write - hour
        )
    }

    fn closed_with_an_hour(write: u64, hour: u64) -> String {
        format!(
            r#"{{"type":"result","subtype":"success","usage":{{"input_tokens":4,"output_tokens":20,"cache_creation_input_tokens":{write},"cache_creation":{{"ephemeral_5m_input_tokens":{},"ephemeral_1h_input_tokens":{hour}}}}}}}"#,
            write - hour
        )
    }

    #[test]
    fn a_turn_whose_one_hour_cache_writes_do_not_add_up_is_reported() {
        let mut translator = translator();
        translator.line(&written_for_an_hour(300, 300));
        translator.line(&written_for_an_hour(200, 0));

        let events = translator.line(&closed_with_an_hour(500, 500));

        let differs = differences(&events);
        assert_eq!(differs.len(), 1, "{events:?}");
        let (per_message, reported, _, _) = differs[0];
        assert_eq!(per_message.cache_write, reported.cache_write);
        assert_eq!(
            (per_message.cache_write_1h, reported.cache_write_1h),
            (Some(300), Some(500))
        );
    }

    #[test]
    fn a_turn_whose_one_hour_cache_writes_add_up_is_not_reported() {
        let mut translator = translator();
        translator.line(&written_for_an_hour(300, 300));
        translator.line(&written_for_an_hour(200, 100));

        let events = translator.line(&closed_with_an_hour(500, 400));

        assert!(differences(&events).is_empty(), "{events:?}");
    }

    #[test]
    fn a_turn_with_a_message_that_gave_no_split_does_not_compare_the_one_hour_share() {
        let mut translator = translator();
        translator.line(&written_for_an_hour(300, 300));
        translator.line(
            r#"{"type":"stream_event","event":{"type":"message_delta","usage":{"input_tokens":2,"output_tokens":10,"cache_creation_input_tokens":200}}}"#,
        );

        let events = translator.line(&closed_with_an_hour(500, 500));

        assert!(differences(&events).is_empty(), "{events:?}");
    }

    type Difference<'a> = (
        &'a TokenCounts,
        &'a TokenCounts,
        Option<&'a str>,
        &'a [String],
    );

    fn differences(events: &[Event]) -> Vec<Difference<'_>> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::TurnTotalDiffers {
                    per_message,
                    reported,
                    backend_session,
                    unfinished,
                } => Some((
                    per_message,
                    reported,
                    backend_session.as_deref(),
                    unfinished.as_slice(),
                )),
                _ => None,
            })
            .collect()
    }

    fn started(id: &str) -> String {
        format!(
            r#"{{"type":"stream_event","event":{{"type":"message_start","message":{{"model":"opus-5","id":"{id}"}}}}}}"#
        )
    }

    fn written(id: &str, extra: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"model":"opus-5","id":"{id}","content":[{{"type":"text","text":"Writing"}}]{extra}}}}}"#
        )
    }

    fn cut_notices(events: &[Event]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Notice { message } if message.contains("cut off") => Some(message.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_mismatch_left_by_a_reply_with_no_count_names_the_conversation_and_the_reply() {
        let mut translator = translator();
        translator.line(&started("msg_cut"));
        // The line names the request and carries no counts to take.
        translator.line(
            r#"{"type":"assistant","message":{"model":"opus-5","id":"msg_cut","content":[{"type":"text","text":"Writing"}]},"request_id":"req_cut"}"#,
        );
        let mut events = translator.line(&started("msg_next"));
        events.extend(translator.line(&delta(2, 10)));
        events.extend(translator.line(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":4,"output_tokens":18}}"#,
        ));

        let differs = differences(&events);
        assert_eq!(differs.len(), 1, "{events:?}");
        let (_, _, backend_session, unfinished) = differs[0];
        assert_eq!(backend_session, Some("s-1"));
        assert_eq!(unfinished, ["msg_cut (request req_cut)"]);
        let cut = cut_notices(&events);
        assert_eq!(cut.len(), 1, "{events:?}");
        assert!(cut[0].contains("not counted"), "{cut:?}");
        assert!(warnings(&events).is_empty(), "{events:?}");
    }

    #[test]
    fn a_reply_cut_off_as_the_turn_ends_is_counted_from_its_last_snapshot() {
        let mut translator = translator();
        translator.line(&started("msg_1"));
        translator.line(&written(
            "msg_1",
            r#","usage":{"input_tokens":1,"output_tokens":3}"#,
        ));
        translator.line(&written(
            "msg_1",
            r#","usage":{"input_tokens":1,"output_tokens":5}"#,
        ));

        let events = translator.line(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":1,"output_tokens":5}}"#,
        );

        let counted: Vec<(u64, u64)> = events
            .iter()
            .filter_map(|event| match event {
                Event::Usage(usage) => Some((usage.input, usage.output)),
                _ => None,
            })
            .collect();
        assert_eq!(counted, [(1, 5)]);
        assert!(differences(&events).is_empty(), "{events:?}");
        let cut = cut_notices(&events);
        assert_eq!(cut.len(), 1, "{events:?}");
        assert!(!cut[0].contains("carrying the turn on"), "{cut:?}");
        assert!(cut[0].contains("msg_1"), "{cut:?}");
    }

    /// A sub-agent's reply cut off by the next one starting on its stream is
    /// counted, but it is the sub-agent's: the main transcript is not told a
    /// reply was cut off, as it is for one of the main agent's.
    #[test]
    fn a_sub_agents_reply_cut_off_puts_no_notice_in_the_main_transcript() {
        let start = |id: &str, parent: &str| {
            format!(
                r#"{{"type":"stream_event","event":{{"type":"message_start","message":{{"model":"opus-5","id":"{id}"}}}}{parent}}}"#
            )
        };
        let write = |id: &str, parent: &str| {
            format!(
                r#"{{"type":"assistant","message":{{"model":"opus-5","id":"{id}","content":[{{"type":"text","text":"Writing"}}],"usage":{{"input_tokens":1,"output_tokens":3}}}}{parent}}}"#
            )
        };
        for (parent, notices) in [(r#","parent_tool_use_id":"toolu_agent""#, 0), ("", 1)] {
            let mut translator = translator();
            translator.line(&start("msg_1", parent));
            translator.line(&write("msg_1", parent));

            let events = translator.line(&start("msg_2", parent));

            assert_eq!(cut_notices(&events).len(), notices, "{parent}: {events:?}");
        }
    }

    #[test]
    fn a_reply_the_operator_stopped_is_not_counted_from_its_snapshot() {
        let mut translator = translator();
        translator.line(&started("msg_1"));
        translator.line(
            r#"{"type":"assistant","message":{"model":"opus-5","id":"msg_1","content":[{"type":"text","text":"Writing"}],"usage":{"input_tokens":2,"output_tokens":679}},"aborted":true}"#,
        );

        let events = translator.line(
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"terminal_reason":"aborted_streaming","usage":{"input_tokens":0,"output_tokens":0}}"#,
        );

        assert!(
            events.iter().all(|event| !matches!(event, Event::Usage(_))),
            "{events:?}"
        );
        assert!(cut_notices(&events).is_empty(), "{events:?}");
    }

    #[test]
    fn a_stopped_reply_stays_stopped_when_a_later_line_of_it_omits_the_flag() {
        let mut translator = translator();
        translator.line(&started("msg_1"));
        translator.line(
            r#"{"type":"assistant","message":{"model":"opus-5","id":"msg_1","content":[{"type":"text","text":"Writing"}],"usage":{"input_tokens":2,"output_tokens":679}},"aborted":true}"#,
        );
        translator.line(&written(
            "msg_1",
            r#","usage":{"input_tokens":2,"output_tokens":700}"#,
        ));

        // The count that still arrives for it is one the CLI leaves out.
        let mut events = translator.line(&delta(2, 700));
        events.extend(translator.line(STOPPED_STREAMING));

        assert!(
            events.iter().all(|event| !matches!(event, Event::Usage(_))),
            "{events:?}"
        );
    }

    #[test]
    fn a_line_of_another_message_does_not_stand_in_for_the_snapshot_of_the_one_in_flight() {
        let mut translator = translator();
        translator.line(&started("msg_1"));
        translator.line(&written(
            "msg_1",
            r#","usage":{"input_tokens":1,"output_tokens":5}"#,
        ));
        translator.line(&written(
            "msg_other",
            r#","usage":{"input_tokens":9,"output_tokens":900}"#,
        ));

        let events = translator.line(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":1,"output_tokens":5}}"#,
        );

        let counted: Vec<(u64, u64)> = events
            .iter()
            .filter_map(|event| match event {
                Event::Usage(usage) => Some((usage.input, usage.output)),
                _ => None,
            })
            .collect();
        assert_eq!(counted, [(1, 5)]);
    }

    const STOPPED_STREAMING: &str = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"terminal_reason":"aborted_streaming","usage":{"input_tokens":0,"output_tokens":0}}"#;

    #[test]
    fn a_reply_stopped_before_any_line_of_it_was_written_is_not_called_cut_off() {
        let mut translator = translator();
        translator.line(&started("msg_1"));

        let events = translator.line(STOPPED_STREAMING);

        assert!(cut_notices(&events).is_empty(), "{events:?}");
        assert!(
            events.iter().all(|event| !matches!(event, Event::Usage(_))),
            "{events:?}"
        );
        assert!(differences(&events).is_empty(), "{events:?}");
    }

    #[test]
    fn a_message_delta_after_the_interrupt_of_its_reply_is_not_counted() {
        let mut translator = translator();
        translator.line(&started("msg_1"));
        let mut events = translator.line(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]},"parent_tool_use_id":null}"#,
        );
        events.extend(translator.line(&delta(2, 4)));
        events.extend(translator.line(STOPPED_STREAMING));

        assert!(
            events.iter().all(|event| !matches!(event, Event::Usage(_))),
            "{events:?}"
        );
        assert!(cut_notices(&events).is_empty(), "{events:?}");
        assert!(differences(&events).is_empty(), "{events:?}");
    }

    #[test]
    fn a_reply_that_finished_before_the_interrupt_is_still_counted() {
        let mut translator = translator();
        translator.line(&started("msg_1"));
        let mut events = translator.line(&delta(2, 40));
        events.extend(translator.line(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]},"parent_tool_use_id":null}"#,
        ));
        events.extend(translator.line(
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"terminal_reason":"aborted_tools","usage":{"input_tokens":2,"output_tokens":40}}"#,
        ));

        let counted: Vec<(u64, u64)> = events
            .iter()
            .filter_map(|event| match event {
                Event::Usage(usage) => Some((usage.input, usage.output)),
                _ => None,
            })
            .collect();
        assert_eq!(counted, [(2, 40)]);
        assert!(differences(&events).is_empty(), "{events:?}");
    }

    #[test]
    fn a_reply_finished_with_its_message_delta_is_not_counted_again_from_its_snapshot() {
        let mut translator = translator();
        translator.line(&started("msg_1"));
        translator.line(&written(
            "msg_1",
            r#","usage":{"input_tokens":2,"output_tokens":1}"#,
        ));
        let mut events = translator.line(&delta(2, 40));
        events.extend(translator.line(&started("msg_2")));
        events.extend(translator.line(&delta(1, 1)));
        events.extend(translator.line(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":3,"output_tokens":41}}"#,
        ));

        assert!(cut_notices(&events).is_empty(), "{events:?}");
        assert!(differences(&events).is_empty(), "{events:?}");
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
            differences(&events).is_empty(),
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

    #[test]
    fn a_retry_after_a_failure_with_no_response_is_said_by_the_kind_of_failure_alone() {
        let events = Translator::new("max").line(
            r#"{"type":"system","subtype":"api_retry","attempt":3,"max_retries":10,"retry_delay_ms":4049,"error_status":null,"error":"unknown"}"#,
        );

        assert_eq!(
            events,
            [warn(
                "a request failed (unknown), and the CLI is retrying it: attempt 3 of 10, in 4.0 s"
                    .to_owned()
            )]
        );
    }

    #[test]
    fn a_banner_the_cli_shows_as_a_warning_is_a_warning_and_an_empty_one_is_nothing() {
        let mut translator = Translator::new("max");

        let warned = translator.line(
            r#"{"type":"system","subtype":"informational","content":"Unknown command: /clar. Did you mean /clear?","level":"warning"}"#,
        );
        let empty = translator
            .line(r#"{"type":"system","subtype":"informational","content":" ","level":"notice"}"#);

        assert_eq!(
            warned,
            [warn(
                "Unknown command: /clar. Did you mean /clear?".to_owned()
            )]
        );
        assert!(empty.is_empty(), "{empty:?}");
    }

    #[test]
    fn a_refusal_that_says_why_carries_the_apis_category_and_words() {
        let events = Translator::new("max").line(
            r#"{"type":"system","subtype":"model_refusal_no_fallback","original_model":"claude-opus-5-5","api_refusal_category":"cyber","api_refusal_explanation":"The request was declined.","content":""}"#,
        );

        assert_eq!(
            events,
            [warn(
                "`claude-opus-5-5` declined to answer, and the CLI has no other model to give \
                 the request to: cyber — The request was declined."
                    .to_owned()
            )]
        );
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

    fn live_record(start_usage: &str) -> Usage {
        let mut translator = Translator::new("max");
        translator
            .line(r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5"}"#);
        translator.line(&format!(
            r#"{{"type":"stream_event","event":{{"type":"message_start","message":{{"model":"opus-5","usage":{start_usage}}}}}}}"#
        ));
        let events = translator.line(&delta(5, 7));
        let [Event::Usage(usage), Event::Context(_)] = events.as_slice() else {
            panic!("a usage record and the context: {events:?}");
        };
        usage.clone()
    }

    /// The live stream names a message's speed in its `message_start` and
    /// not in the `message_delta` its record is built from, so a message
    /// served in fast mode is valued at the fast price while its turn runs.
    #[test]
    fn a_live_message_started_in_fast_mode_is_counted_as_fast() {
        assert!(live_record(r#"{"input_tokens":2,"output_tokens":1,"speed":"fast"}"#).fast);
    }

    /// Claude Code 2.1.282 reports `fast_mode_state: "on"` for a session whose
    /// fast mode the account could not pay for, and serves it at the standard
    /// speed: only the message's own speed says how it is billed.
    #[test]
    fn a_live_message_served_at_the_standard_speed_is_not_fast() {
        assert!(!live_record(r#"{"input_tokens":2,"output_tokens":1,"speed":"standard"}"#).fast);
    }

    /// The CLI's word that it did not do what the session asked for, as
    /// Claude Code 2.1.282 sent it when the account could not pay for fast
    /// mode, reaches the operator in its own words.
    #[test]
    fn a_notification_from_the_cli_is_passed_on_in_its_words() {
        let events = Translator::new("max").line(
            r#"{"type":"system","subtype":"notification","key":"fast-mode-overage-rejected","text":"Fast mode disabled · usage credits exhausted","priority":"immediate","color":"error"}"#,
        );

        let [Event::Notice { message }] = events.as_slice() else {
            panic!("one notice: {events:?}");
        };
        assert_eq!(message, "Fast mode disabled · usage credits exhausted");
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

    /// A shape the CLI repeats after nearly every turn would otherwise fill
    /// the transcript with the same sentence; once says that Niobe is not
    /// reading it, and a second kind is still news.
    #[test]
    fn an_unknown_message_is_reported_once_per_kind_in_a_session() {
        let mut translator = Translator::new("max");
        let first = translator.line(r#"{"type":"teleport","uuid":"a"}"#);
        assert_eq!(notices(&first).len(), 1, "the first one is reported");
        assert!(
            translator
                .line(r#"{"type":"teleport","uuid":"b"}"#)
                .is_empty()
        );
        assert_eq!(
            notices(&translator.line(r#"{"type":"another_kind"}"#)).len(),
            1,
            "a different type is a different shape"
        );
    }

    #[test]
    fn an_unknown_system_subtype_is_reported_once_per_subtype_in_a_session() {
        let mut translator = Translator::new("max");
        let line = r#"{"type":"system","subtype":"warp_status"}"#;
        let first = translator.line(line);
        assert_eq!(notices(&first).len(), 1);
        assert!(
            warnings(&first).is_empty(),
            "a shape nobody read is not an error"
        );
        assert!(translator.line(line).is_empty());
        assert_eq!(
            notices(&translator.line(r#"{"type":"system","subtype":"other_status"}"#)).len(),
            1
        );
    }

    /// The CLI restores a resumed session's running totals whether or not
    /// its transcript could be read here, so its first bill carries the
    /// earlier session in a share nothing here knows. That bill is not
    /// counted, the operator is told why, and each bill after it is counted
    /// by what it added.
    #[test]
    fn a_resumed_session_whose_earlier_spend_is_unknown_does_not_bill_it_again() {
        let mut translator =
            Translator::new("max").resuming(&Spent::unknown("cannot read s-1.jsonl"));
        let events: Vec<Event> = [
            INIT_FAMILY,
            MAIN_START,
            &delta(40, 4),
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":40,"output_tokens":4},"modelUsage":{"opus-5":{"inputTokens":140,"outputTokens":14,"costUSD":0.7}},"total_cost_usd":0.7}"#,
            MAIN_START,
            &delta(10, 1),
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":10,"output_tokens":1},"modelUsage":{"opus-5":{"inputTokens":150,"outputTokens":15,"costUSD":0.75}},"total_cost_usd":0.75}"#,
        ]
        .iter()
        .flat_map(|line| translator.line(line))
        .collect();
        let totals = niobe_core::session::SessionState::replay(&events)
            .totals()
            .clone();

        assert_eq!((totals.input, totals.output), (50, 5));
        assert!(
            (totals.reported_cost_usd - 0.05).abs() < 1e-9,
            "{}",
            totals.reported_cost_usd
        );
        assert!(!totals.cost_fully_reported(), "{totals:?}");
        assert_eq!(totals.records_unsettled, 1);
        let said = notices(&events);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("cannot read s-1.jsonl"), "{said:?}");
        assert!(
            matches!(events.first(), Some(Event::Notice { .. })),
            "{events:?}"
        );
        assert_eq!(warnings(&events), Vec::<String>::new());
    }

    /// A cleared conversation is a stored session of its own, which is read
    /// back alone: what only the first init and the first result said has to
    /// be said again for it, or it reads as unbilled and unwarned.
    #[test]
    fn a_clear_says_how_the_session_is_billed_and_what_it_runs_on_again() {
        let init = r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5","apiKeySource":"ANTHROPIC_API_KEY","claude_code_version":"9.9.999"}"#;
        let reset =
            r#"{"type":"conversation_reset","new_conversation_id":"s-2","trigger":"clear"}"#;
        let mut translator = Translator::new("work");

        let first = translator.line(init);
        let cleared = translator.line(reset);
        let again = translator.line(init);

        assert_eq!(billing(&first), [Billing::Metered]);
        assert_eq!(warnings(&first).len(), 1, "{first:?}");
        assert!(cleared.contains(&Event::Cleared), "{cleared:?}");
        assert_eq!(billing(&again), [Billing::Metered], "{again:?}");
        assert_eq!(warnings(&again).len(), 1, "the release warning: {again:?}");
    }

    #[test]
    fn a_message_type_seen_before_a_clear_is_reported_again_after_it() {
        let odd = r#"{"type":"system","subtype":"from_the_future"}"#;
        let reset =
            r#"{"type":"conversation_reset","new_conversation_id":"s-2","trigger":"clear"}"#;
        let mut translator = translator();

        let first = translator.line(odd);
        let repeated = translator.line(odd);
        translator.line(reset);
        let after = translator.line(odd);

        let notices = |events: &[Event]| {
            events
                .iter()
                .filter(|event| matches!(event, Event::Notice { .. }))
                .count()
        };
        assert_eq!(notices(&first), 1, "{first:?}");
        assert_eq!(notices(&repeated), 0, "{repeated:?}");
        assert_eq!(notices(&after), 1, "{after:?}");
    }

    /// After `/clear` the CLI's totals restart from nothing, so the first
    /// bill after it is the new conversation's alone, whatever was unknown
    /// before it.
    #[test]
    fn a_clear_before_the_first_bill_leaves_nothing_unknown_in_it() {
        let mut translator = Translator::new("max").resuming(&Spent::unknown("unreadable"));
        let events: Vec<Event> = [
            INIT_FAMILY,
            r#"{"type":"conversation_reset","new_conversation_id":"s-2","trigger":"clear"}"#,
            MAIN_START,
            &delta(40, 4),
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":40,"output_tokens":4},"modelUsage":{"opus-5":{"inputTokens":40,"outputTokens":4,"costUSD":0.2}},"total_cost_usd":0.2}"#,
        ]
        .iter()
        .flat_map(|line| translator.line(line))
        .collect();
        let totals = niobe_core::session::SessionState::replay(&events)
            .totals()
            .clone();

        assert!(
            (totals.reported_cost_usd - 0.2).abs() < 1e-9,
            "{}",
            totals.reported_cost_usd
        );
    }

    /// The turn a resumed session could not split from what it restored is
    /// the cleared conversation's shortfall, and the clear leaves it behind:
    /// the bills after it are the new conversation's whole.
    #[test]
    fn a_clear_after_an_uncounted_resumed_bill_leaves_the_new_conversation_whole() {
        let mut translator = Translator::new("max").resuming(&Spent::unknown("unreadable"));
        let events: Vec<Event> = [
            INIT_FAMILY,
            MAIN_START,
            &delta(40, 4),
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":40,"output_tokens":4},"modelUsage":{"opus-5":{"inputTokens":140,"outputTokens":14,"costUSD":0.7}},"total_cost_usd":0.7}"#,
            r#"{"type":"conversation_reset","new_conversation_id":"s-2","trigger":"clear"}"#,
            MAIN_START,
            &delta(10, 1),
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":10,"output_tokens":1},"modelUsage":{"opus-5":{"inputTokens":10,"outputTokens":1,"costUSD":0.05}},"total_cost_usd":0.05}"#,
        ]
        .iter()
        .flat_map(|line| translator.line(line))
        .collect();
        let totals = niobe_core::session::SessionState::replay(&events)
            .totals()
            .clone();

        assert_eq!((totals.input, totals.output), (10, 1));
        assert!(
            (totals.reported_cost_usd - 0.05).abs() < 1e-9,
            "{}",
            totals.reported_cost_usd
        );
        assert!(totals.cost_fully_reported(), "{totals:?}");
    }

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
        // The two messages and one cost record per billed id: the family
        // gave tokens up and grew in cost, so its own record settles it.
        assert_eq!(totals.records, 4);
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

    /// The same over two turns, the second billing 30 input and 3 output
    /// tokens no message carried: what the first turn moved to the windowed
    /// id is no longer the family's, so the second moves only its own
    /// message, and the tokens no message carried are counted from the bill.
    #[test]
    fn a_family_billed_under_its_window_turn_after_turn_moves_each_message_once() {
        let totals = folded(&[
            INIT_WINDOW,
            AGENT_START,
            AGENT_DELTA,
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":50,"output_tokens":5},"modelUsage":{"opus-5[1m]":{"inputTokens":50,"outputTokens":5,"costUSD":0.3,"canonicalModel":"opus-5"}},"total_cost_usd":0.3}"#,
            AGENT_START,
            AGENT_DELTA,
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":80,"output_tokens":8},"modelUsage":{"opus-5[1m]":{"inputTokens":130,"outputTokens":13,"costUSD":0.8,"canonicalModel":"opus-5"}},"total_cost_usd":0.8}"#,
        ]);

        assert_counted_once(&totals, 130, 13);
        assert!((totals.reported_cost_usd - 0.8).abs() < 1e-9);
    }

    /// Messages naming the family, billed under two ids of it, one of which
    /// grew by tokens no message carried: the first id takes what it is
    /// short, the second only what the family still has to spare, and the
    /// rest of its growth is counted from the bill.
    #[test]
    fn a_family_split_between_two_billed_ids_gives_each_token_up_once() {
        let totals = folded(&[
            MAIN_START,
            &delta(120, 0),
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":150,"output_tokens":0},"modelUsage":{"opus-5[1m]":{"inputTokens":100,"costUSD":0.5,"canonicalModel":"opus-5"},"opus-5[x]":{"inputTokens":50,"costUSD":0.25,"canonicalModel":"opus-5"}},"total_cost_usd":0.75}"#,
        ]);

        assert_counted_once(&totals, 150, 0);
        assert!((totals.reported_cost_usd - 0.75).abs() < 1e-9);
    }

    fn billed_models(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Usage(usage) => Some(usage.model.clone()),
                _ => None,
            })
            .collect()
    }

    /// A `message_delta` whose `message_start` never arrived, before anything
    /// named the session's model, is filed under the one id the turn's bill
    /// names, rather than under no model and then again under that id. It is
    /// filed as the message reported it, with the cache writes' lifetime and
    /// the speed that `modelUsage` does not carry.
    #[test]
    fn a_message_no_model_was_named_for_is_billed_to_the_one_id_the_result_names() {
        let lines = [
            r#"{"type":"stream_event","event":{"type":"message_delta","usage":{"input_tokens":100,"output_tokens":10,"cache_creation_input_tokens":90,"cache_creation":{"ephemeral_1h_input_tokens":60,"ephemeral_5m_input_tokens":30},"speed":"fast"}}}"#,
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":100,"output_tokens":10,"cache_creation_input_tokens":90},"modelUsage":{"claude-opus-5":{"inputTokens":100,"outputTokens":10,"cacheCreationInputTokens":90,"costUSD":0.5}},"total_cost_usd":0.5}"#,
        ];
        let totals = folded(&lines);
        assert_counted_once(&totals, 100, 10);
        assert_eq!((totals.cache_write, totals.cache_write_1h), (90, 60));
        assert!((totals.reported_cost_usd - 0.5).abs() < 1e-9);

        let mut translator = Translator::new("max");
        let events: Vec<Event> = lines
            .iter()
            .flat_map(|line| translator.line(line))
            .collect();
        assert!(
            billed_models(&events).iter().all(|model| !model.is_empty()),
            "billed to no model: {events:?}"
        );
        assert_eq!(warnings(&events), Vec::<String>::new());
        let filed = events.iter().find_map(|event| match event {
            Event::Usage(usage) => Some(usage.clone()),
            _ => None,
        });
        assert_eq!(
            filed,
            Some(Usage {
                input: 100,
                output: 10,
                cache_read: 0,
                cache_write: 90,
                cache_write_1h: Some(60),
                reasoning: 0,
                model: "claude-opus-5".to_owned(),
                cost_usd: None,
                settles_model: Some(false),
                fast: true,
            })
        );
    }

    /// A `result` that names no model holds the message over to the next
    /// one, whose running total still carries it.
    #[test]
    fn a_message_no_model_was_named_for_waits_for_a_bill_that_names_one() {
        let totals = folded(&[
            &delta(100, 10),
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":100,"output_tokens":10}}"#,
            &delta(40, 4),
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":40,"output_tokens":4},"modelUsage":{"claude-opus-5":{"inputTokens":140,"outputTokens":14,"costUSD":0.7}},"total_cost_usd":0.7}"#,
        ]);
        assert_counted_once(&totals, 140, 14);
    }

    /// Where the bill names more than one id, nothing says which the message
    /// was: its tokens are counted from the bill, once, and the operator is
    /// told why.
    #[test]
    fn a_message_no_model_was_named_for_is_counted_from_a_bill_naming_several() {
        let lines = [
            &delta(150, 15) as &str,
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":150,"output_tokens":15},"modelUsage":{"claude-opus-5":{"inputTokens":100,"outputTokens":10,"costUSD":0.5},"claude-haiku-4-5":{"inputTokens":50,"outputTokens":5,"costUSD":0.01}},"total_cost_usd":0.51}"#,
        ];
        let totals = folded(&lines);
        assert_counted_once(&totals, 150, 15);

        let mut translator = Translator::new("max");
        let events: Vec<Event> = lines
            .iter()
            .flat_map(|line| translator.line(line))
            .collect();
        assert!(
            billed_models(&events).iter().all(|model| !model.is_empty()),
            "billed to no model: {events:?}"
        );
        let said = warnings(&events);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("named no model"), "{said:?}");
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
    fn a_level_that_is_not_a_number_is_no_window() {
        for level in [f64::NAN, f64::INFINITY] {
            let window = wire::Window {
                utilization: Some(level),
                resets_at: Some(1_789_779_600),
            };
            assert_eq!(read_window(Some(window)), None, "{level}");
        }
    }

    #[test]
    fn a_window_is_a_bracketed_suffix_on_the_same_id() {
        assert!(is_window_of("claude-opus-5[1m]", "claude-opus-5"));
        assert!(!is_window_of("claude-opus-5", "claude-opus-5"));
        assert!(!is_window_of("claude-opus-5-1", "claude-opus-5"));
        assert!(!is_window_of("claude-opus-5[1m]", "claude-opus"));
        assert!(!is_window_of("claude-opus-5[1m", "claude-opus-5"));
    }

    #[test]
    fn a_pricing_basis_the_bridge_does_not_know_is_reported_and_its_figure_kept() {
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
        assert_eq!(usage.cost_usd, Some(0.5));
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

    /// A session total a rounding's width above what was reported is the
    /// sum's rounding, not spend: nothing is reported for it, and the next
    /// total, which still holds it, reports it with the rest.
    #[test]
    fn a_session_total_a_rounding_above_the_last_reports_nothing_until_it_grows() {
        let mut translator = translator();
        translator.line(r#"{"type":"result","subtype":"success","total_cost_usd":0.25}"#);

        let rounding = before_the_end(
            translator.line(r#"{"type":"result","subtype":"success","total_cost_usd":0.25003}"#),
        );
        let grown = before_the_end(
            translator.line(r#"{"type":"result","subtype":"success","total_cost_usd":0.40}"#),
        );

        assert!(rounding.is_empty(), "{rounding:?}");
        assert!((cost_reported(&grown) - 0.15).abs() < 1e-9, "{grown:?}");
    }

    fn cost_reported(events: &[Event]) -> f64 {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Usage(usage) => usage.cost_usd,
                _ => None,
            })
            .sum()
    }

    #[test]
    fn a_session_total_after_a_model_change_reports_only_what_the_session_added() {
        let mut translator = Translator::new("max");
        let mut events = translator
            .line(r#"{"type":"system","subtype":"init","session_id":"s-1","model":"sonnet-5"}"#);
        events.extend(
            translator.line(r#"{"type":"result","subtype":"success","total_cost_usd":0.05}"#),
        );
        events.extend(
            translator
                .line(r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5"}"#),
        );
        let last =
            translator.line(r#"{"type":"result","subtype":"success","total_cost_usd":0.08}"#);
        events.extend(last.clone());

        assert!((cost_reported(&events) - 0.08).abs() < 1e-9, "{events:?}");
        let [Event::Usage(usage), Event::TurnEnded] = last.as_slice() else {
            panic!("one cost record: {last:?}");
        };
        assert_eq!(usage.model, "opus-5");
        assert!((usage.cost_usd.unwrap_or_default() - 0.03).abs() < 1e-9);
    }

    #[test]
    fn a_session_total_after_a_split_bill_counts_every_models_share_as_reported() {
        let mut translator = translator();
        let mut events = translator.line(
            r#"{"type":"result","subtype":"success","modelUsage":{"sonnet-5":{"costUSD":0.04},"haiku-4-5":{"costUSD":0.01}},"total_cost_usd":0.05}"#,
        );
        let last =
            translator.line(r#"{"type":"result","subtype":"success","total_cost_usd":0.05}"#);
        events.extend(last.clone());

        assert!((cost_reported(&events) - 0.05).abs() < 1e-9, "{events:?}");
        assert_eq!(
            last,
            vec![Event::TurnEnded],
            "a total that has not moved was billed again"
        );
    }

    /// A bill whose tokens grew while its cost did not says nothing about
    /// what those tokens cost: the record for them carries no figure, and
    /// the turn's tokens are left owed rather than read as free.
    #[test]
    fn a_bill_that_grew_in_tokens_and_not_in_cost_reports_no_cost_for_them() {
        let mut translator = Translator::new("max");
        let mut events: Vec<Event> = [
            INIT_FAMILY,
            MAIN_START,
            &delta(100, 10),
            FIRST_TURN_ON_THE_FAMILY,
            MAIN_START,
            &delta(40, 4),
        ]
        .iter()
        .flat_map(|line| translator.line(line))
        .collect();
        let last = translator.line(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":40,"output_tokens":4},"modelUsage":{"opus-5":{"inputTokens":150,"outputTokens":15,"costUSD":0.5,"canonicalModel":"opus-5"}},"total_cost_usd":0.5}"#,
        );
        events.extend(last.clone());

        let [Event::Usage(usage), Event::TurnEnded] = last.as_slice() else {
            panic!("one usage record: {last:?}");
        };
        assert_eq!((usage.input, usage.output), (10, 1));
        assert_eq!(usage.cost_usd, None);
        let totals = niobe_core::session::SessionState::replay(&events)
            .totals()
            .clone();
        assert_eq!((totals.input, totals.output), (150, 15));
        assert_eq!(totals.records_unsettled, 2, "{:?}", totals.unsettled);
        assert!((totals.reported_cost_usd - 0.5).abs() < 1e-9);
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

        // The CLI writes `init` again at the start of every turn, on a model
        // moved or not, and a release it already reported has not become news.
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

    fn limits(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::LimitReached { message } => Some(message.clone()),
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

    /// What Claude Code 2.1.287 says while a tool call runs: a heartbeat every
    /// thirty seconds, as recorded during one `Bash` call, and the frame it
    /// sends when a sub-agent's request is retried, in the shape the CLI's own
    /// schema gives it.
    const TOOL_PROGRESS: [&str; 2] = [
        r#"{"type":"tool_progress","tool_use_id":"toolu_a-heartbeat-0","tool_name":"Bash","parent_tool_use_id":"toolu_a","elapsed_time_seconds":30,"heartbeat":true,"session_id":"s-1","uuid":"41ca27e9-61e6-4d21-a5e4-add6590835d0"}"#,
        r#"{"type":"tool_progress","tool_use_id":"toolu_b","tool_name":"Agent","parent_tool_use_id":null,"elapsed_time_seconds":0,"session_id":"s-1","uuid":"dbba2b1b-91ff-49a5-84d7-4e48d4746725","subagent_type":"quick-lookup","subagent_retry":{"agent_id":"a1","attempt":1,"max_retries":10,"retry_delay_ms":500,"error_status":529,"error_category":"overloaded"}}"#,
    ];

    #[test]
    fn the_clis_word_that_a_tool_call_is_still_running_is_passed_over() {
        let mut translator = translator();

        for line in TOOL_PROGRESS {
            let events = translator.line(line);

            assert!(events.is_empty(), "{line}: {events:?}");
        }
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
        // A search is about where it looks, so "always this target" on a
        // `Grep` is a rule about its directory, not about what it looked
        // for; a pattern alone, as a `Glob` may carry, is its target.
        assert_eq!(
            target(r#"{"pattern":"TODO","path":"/repo/src"}"#).as_deref(),
            Some("/repo/src")
        );
        assert_eq!(
            target(r#"{"pattern":"**/*.rs"}"#).as_deref(),
            Some("**/*.rs")
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

    /// A prompt with no call id is still a prompt of its own: two of them
    /// are two questions, each answered by its own request.
    #[test]
    fn prompts_with_no_call_id_are_told_apart_by_their_requests() {
        let mut translator = translator();

        let mut ids = Vec::new();
        for request in ["c1", "c2"] {
            let events = translator.line(&format!(
                r#"{{"type":"control_request","request_id":"{request}","request":{{"subtype":"can_use_tool","tool_name":"Bash","input":{{"command":"ls"}}}}}}"#
            ));
            let [Event::PermissionRequest { id, .. }] = events.as_slice() else {
                panic!("one prompt: {events:?}");
            };
            ids.push(id.clone());
        }

        assert_ne!(ids[0], ids[1]);
        let asked: Vec<_> = translator
            .take_asked()
            .into_iter()
            .map(|ask| (ask.id, ask.request_id))
            .collect();
        assert_eq!(
            asked,
            [
                (ids[0].clone(), "c1".to_owned()),
                (ids[1].clone(), "c2".to_owned())
            ]
        );
    }

    /// A standing rule is written about a command, and a call that also asks
    /// to run outside the CLI's sandbox is a different thing to allow. It has
    /// no target, so only a rule for the whole tool answers it.
    #[test]
    fn a_command_asking_to_leave_the_sandbox_is_not_answered_by_a_rule_for_the_command() {
        let mut translator = translator();

        let events = translator.line(
            r#"{"type":"control_request","request_id":"c1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"cargo test","dangerouslyDisableSandbox":true},"tool_use_id":"toolu_1"}}"#,
        );

        let [Event::PermissionRequest { target, .. }] = events.as_slice() else {
            panic!("one prompt: {events:?}");
        };
        assert_eq!(target, &None);
        let mut rules = niobe_core::permission::Allowlist::new();
        rules.insert(niobe_core::permission::Rule::targeted("Bash", "cargo test"));
        assert!(!rules.allows("Bash", target.as_deref()));
        rules.insert(niobe_core::permission::Rule::tool("Bash"));
        assert!(rules.allows("Bash", target.as_deref()));
    }

    fn target_asked(tool: &str, input: &str) -> Option<String> {
        let events = translator().line(&format!(
            r#"{{"type":"control_request","request_id":"c1","request":{{"subtype":"can_use_tool","tool_name":"{tool}","input":{input},"tool_use_id":"toolu_1"}}}}"#
        ));
        let [Event::PermissionRequest { target, .. }] = events.as_slice() else {
            panic!("one prompt: {events:?}");
        };
        target.clone()
    }

    /// Nothing here knows what an MCP server's tool does with the arguments
    /// beside the one named like a target: `git` run with `["status"]` and
    /// with `["push", "--force"]` would both be `git`, and a file path is a
    /// different file in another owner's repository. A rule about that one
    /// argument would answer calls the operator never saw, so such a call has
    /// no target and only a rule for the whole tool answers it.
    #[test]
    fn a_tool_nothing_here_knows_has_a_target_only_where_it_is_its_one_argument() {
        let shell = r#"{"command":"git","args":["push","--force"]}"#;
        assert_eq!(target_asked("mcp__shell__run", shell), None);
        let elsewhere = r#"{"owner":"someone","repo":"private","path":"README.md","content":"x"}"#;
        assert_eq!(
            target_asked("mcp__github__create_or_update_file", elsewhere),
            None
        );
        let mut rules = niobe_core::permission::Allowlist::new();
        rules.insert(niobe_core::permission::Rule::targeted(
            "mcp__shell__run",
            "git",
        ));
        assert!(!rules.allows(
            "mcp__shell__run",
            target_asked("mcp__shell__run", shell).as_deref()
        ));

        assert_eq!(
            target_asked("mcp__fs__read_file", r#"{"path":"/repo/a.rs"}"#).as_deref(),
            Some("/repo/a.rs")
        );
        assert_eq!(
            target_asked(
                "mcp__browser__navigate",
                r#"{"url":"https://example.test/"}"#
            )
            .as_deref(),
            Some("https://example.test/")
        );
    }

    /// The CLI's own tools are read by their schemas, where the arguments
    /// beside the target say how the call runs, not what it reaches.
    #[test]
    fn the_clis_own_tools_keep_their_target_beside_their_other_arguments() {
        assert_eq!(
            target_asked(
                "Bash",
                r#"{"command":"cargo test","description":"Run the tests","timeout":600000}"#
            )
            .as_deref(),
            Some("cargo test")
        );
        assert_eq!(
            target_asked(
                "Edit",
                r#"{"file_path":"/repo/a.rs","old_string":"a","new_string":"b"}"#
            )
            .as_deref(),
            Some("/repo/a.rs")
        );
        assert_eq!(
            target_asked("Write", r#"{"file_path":"/repo/a.rs","content":"x"}"#).as_deref(),
            Some("/repo/a.rs")
        );
        assert_eq!(
            target_asked(
                "NotebookEdit",
                r#"{"notebook_path":"/repo/a.ipynb","new_source":"x"}"#
            )
            .as_deref(),
            Some("/repo/a.ipynb")
        );
        assert_eq!(
            target_asked(
                "WebFetch",
                r#"{"url":"https://example.test/","prompt":"Summarise it"}"#
            )
            .as_deref(),
            Some("https://example.test/")
        );
        assert_eq!(
            target_asked(
                "Read",
                r#"{"file_path":"/repo/a.rs","offset":10,"limit":5}"#
            )
            .as_deref(),
            Some("/repo/a.rs")
        );
    }

    fn proposal_asked(line: &str, translator: &mut Translator) -> Option<Proposal> {
        let events = translator.line(line);
        let [Event::PermissionRequest { proposal, .. }] = events.as_slice() else {
            panic!("one prompt: {events:?}");
        };
        proposal.clone()
    }

    /// The recorded shape of an `Edit` prompt, with lines in its strings.
    #[test]
    fn an_edit_prompt_proposes_its_replacement() {
        let proposal = proposal_asked(
            r#"{"type":"control_request","request_id":"c1","request":{"subtype":"can_use_tool","tool_name":"Edit","display_name":"Edit","input":{"file_path":"/repo/notes.txt","old_string":"a\nbeta","new_string":"a\ngamma","replace_all":true},"tool_use_id":"toolu_1"}}"#,
            &mut translator(),
        )
        .expect("an edit is read");
        assert_eq!(
            proposal.action(),
            &Action::Replace {
                path: "/repo/notes.txt".to_owned(),
                old: "a\nbeta".to_owned(),
                new: "a\ngamma".to_owned(),
                every: true,
            }
        );
        for key in ["file_path", "old_string", "new_string", "replace_all"] {
            assert!(proposal.covers(key), "{key}");
        }
    }

    #[test]
    fn an_edit_without_replace_all_replaces_one_occurrence() {
        let proposal = proposal_asked(
            r#"{"type":"control_request","request_id":"c1","request":{"subtype":"can_use_tool","tool_name":"Edit","input":{"file_path":"/f","old_string":"a","new_string":"b"},"tool_use_id":"toolu_1"}}"#,
            &mut translator(),
        );
        assert!(matches!(
            proposal.as_ref().map(Proposal::action),
            Some(Action::Replace { every: false, .. })
        ));
    }

    #[test]
    fn a_write_prompt_proposes_the_content_and_says_whether_the_file_was_there() {
        fn exists(path: &Path) -> Option<bool> {
            match path.to_str() {
                Some("/repo/old.txt") => Some(true),
                Some("/repo/new.txt") => Some(false),
                _ => None,
            }
        }
        let asked = |path: &str, translator: &mut Translator| {
            proposal_asked(
                &format!(
                    r#"{{"type":"control_request","request_id":"c1","request":{{"subtype":"can_use_tool","tool_name":"Write","input":{{"file_path":"{path}","content":"one\ntwo\n"}},"tool_use_id":"toolu_1"}}}}"#
                ),
                translator,
            )
            .map(|proposal| proposal.action().clone())
        };
        let write = |path: &str, existed| Action::Write {
            path: path.to_owned(),
            content: "one\ntwo\n".to_owned(),
            existed,
        };
        let mut checking = translator().checking_files_with(exists);
        assert_eq!(
            asked("/repo/old.txt", &mut checking),
            Some(write("/repo/old.txt", Some(true)))
        );
        assert_eq!(
            asked("/repo/new.txt", &mut checking),
            Some(write("/repo/new.txt", Some(false)))
        );
        // Nothing handed in to look with: nobody looked.
        assert_eq!(
            asked("/repo/old.txt", &mut translator()),
            Some(write("/repo/old.txt", None))
        );
    }

    /// A relative path is relative to where the CLI runs, which a translator
    /// told no directory cannot know, so it does not look.
    #[test]
    fn a_write_to_a_relative_path_is_looked_for_under_the_session_directory() {
        fn exists(path: &Path) -> Option<bool> {
            Some(path == Path::new("/repo/a.txt"))
        }
        let line = r#"{"type":"control_request","request_id":"c1","request":{"subtype":"can_use_tool","tool_name":"Write","input":{"file_path":"a.txt","content":"x"},"tool_use_id":"toolu_1"}}"#;
        let existed = |translator: &mut Translator| match proposal_asked(line, translator)
            .map(|proposal| proposal.action().clone())
        {
            Some(Action::Write { existed, .. }) => existed,
            other => panic!("a write: {other:?}"),
        };
        assert_eq!(
            existed(&mut translator().checking_files_with(exists).in_dir("/repo")),
            Some(true)
        );
        assert_eq!(existed(&mut translator().checking_files_with(exists)), None);
    }

    /// The CLI's Write takes `~` for the home directory; a path of that name
    /// looked for under the session's directory would find nothing, and the
    /// question would call an existing dotfile a new file.
    #[test]
    fn a_write_to_the_home_directory_is_not_looked_for_as_a_directory_named_tilde() {
        fn exists(_: &Path) -> Option<bool> {
            Some(false)
        }
        let line = r#"{"type":"control_request","request_id":"c1","request":{"subtype":"can_use_tool","tool_name":"Write","input":{"file_path":"~/.zshrc","content":"x"},"tool_use_id":"toolu_1"}}"#;
        let mut translator = translator().checking_files_with(exists).in_dir("/repo");

        let action =
            proposal_asked(line, &mut translator).map(|proposal| proposal.action().clone());

        assert!(
            matches!(action, Some(Action::Write { existed: None, .. })),
            "{action:?}"
        );
    }

    /// An edit whose `replace_all` is not a boolean is not read as one that
    /// replaces a single occurrence: what it asks for is not known, so the
    /// question shows the arguments as they are.
    #[test]
    fn an_edit_whose_replace_all_is_not_a_boolean_proposes_nothing() {
        let line = |every: &str| {
            format!(
                r#"{{"type":"control_request","request_id":"c1","request":{{"subtype":"can_use_tool","tool_name":"Edit","input":{{"file_path":"/repo/a.txt","old_string":"a","new_string":"b","replace_all":{every}}},"tool_use_id":"toolu_1"}}}}"#
            )
        };

        assert!(proposal_asked(&line("true"), &mut translator()).is_some());
        assert!(proposal_asked(&line("false"), &mut translator()).is_some());
        assert!(proposal_asked(&line(r#""yes""#), &mut translator()).is_none());
        assert!(proposal_asked(&line("1"), &mut translator()).is_none());
    }

    #[test]
    fn a_shell_prompt_proposes_its_command_and_leaves_the_rest_to_be_shown() {
        let proposal = proposal_asked(
            r#"{"type":"control_request","request_id":"c1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"cargo test","description":"Run the tests","dangerouslyDisableSandbox":true},"tool_use_id":"toolu_1"}}"#,
            &mut translator(),
        )
        .expect("a command is read");
        assert_eq!(
            proposal.action(),
            &Action::Command {
                command: "cargo test".to_owned()
            }
        );
        assert!(proposal.covers("command"));
        assert!(!proposal.covers("description"));
        assert!(!proposal.covers("dangerouslyDisableSandbox"));
    }

    #[test]
    fn leaving_plan_mode_proposes_the_plan_and_leaves_its_file_to_be_shown() {
        // As Claude Code 2.1.309 asks it, plan and the file it was saved to.
        let proposal = proposal_asked(
            r#"{"type":"control_request","request_id":"c1","request":{"subtype":"can_use_tool","tool_name":"ExitPlanMode","input":{"plan":"1. Guard mean().\n2. Run it.","planFilePath":"/home/u/.claude/plans/p.md"},"tool_use_id":"toolu_1"}}"#,
            &mut translator(),
        )
        .expect("a plan is read");
        assert_eq!(
            proposal.action(),
            &Action::Plan {
                plan: "1. Guard mean().\n2. Run it.".to_owned()
            }
        );
        assert!(proposal.covers("plan"));
        assert!(!proposal.covers("planFilePath"));
    }

    /// Claude Code 2.1.310 fills `plan` in from the plan file before it asks,
    /// and leaves the call's own arguments as they were where there is no
    /// such file: the prompt is still a plan going ahead, with nothing to
    /// read, and is never one a standing answer may approve.
    #[test]
    fn leaving_plan_mode_with_no_plan_written_is_still_a_plan() {
        for input in [
            "{}",
            r#"{"planFilePath":"/p.md"}"#,
            r#"{"plan":7,"allowedPrompts":[]}"#,
        ] {
            let line = format!(
                r#"{{"type":"control_request","request_id":"c1","request":{{"subtype":"can_use_tool","tool_name":"ExitPlanMode","input":{input},"tool_use_id":"toolu_1"}}}}"#
            );
            let proposal = proposal_asked(&line, &mut translator()).expect("a plan is proposed");
            assert_eq!(
                proposal.action(),
                &Action::Plan {
                    plan: String::new()
                },
                "{input}"
            );
            assert!(!proposal.covers("plan"), "{input}");
            assert!(!proposal.covers("planFilePath"), "{input}");
        }
    }

    /// Arguments of another shape than the tool's are not read as the tool's:
    /// the question shows them as they are.
    #[test]
    fn a_prompt_whose_arguments_are_not_the_tools_proposes_nothing() {
        for (tool, input) in [
            ("Edit", r#"{"file_path":"/f","old_string":"a"}"#),
            (
                "Edit",
                r#"{"file_path":"/f","old_string":"a","new_string":7}"#,
            ),
            ("Write", r#"{"file_path":"/f"}"#),
            ("Bash", r#"{"cmd":"ls"}"#),
            ("Read", r#"{"file_path":"/f"}"#),
            ("mcp__notion__search", r#"{"command":"ls"}"#),
        ] {
            let line = format!(
                r#"{{"type":"control_request","request_id":"c1","request":{{"subtype":"can_use_tool","tool_name":"{tool}","input":{input},"tool_use_id":"toolu_1"}}}}"#
            );
            assert_eq!(
                proposal_asked(&line, &mut translator()),
                None,
                "{tool} {input}"
            );
        }
    }

    #[test]
    fn a_prompt_the_cli_withdraws_is_withdrawn_from_the_session() {
        let mut translator = translator();
        translator.line(
            r#"{"type":"control_request","request_id":"c1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"},"tool_use_id":"toolu_1"}}"#,
        );

        let events = translator.line(r#"{"type":"control_cancel_request","request_id":"c1"}"#);
        assert_eq!(
            events,
            [Event::PermissionWithdrawn {
                id: "toolu_1".into()
            }]
        );

        assert_eq!(translator.take_withdrawn(), [ToolCallId::from("toolu_1")]);

        let again = translator.line(r#"{"type":"control_cancel_request","request_id":"c1"}"#);
        assert!(again.is_empty(), "{again:?}");
        assert!(translator.take_withdrawn().is_empty());
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

    /// A refusal the CLI announces names the target a prompt about the same
    /// call would: a rule for `git` is not about `git push --force`.
    #[test]
    fn a_refused_call_has_the_target_its_prompt_would_have() {
        let mut translator = translator();
        translator.line(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"mcp__shell__run","input":{"command":"git","args":["push","--force"]}},{"type":"tool_use","id":"toolu_2","name":"Bash","input":{"command":"cargo test","dangerouslyDisableSandbox":true}}]}}"#,
        );

        for id in ["toolu_1", "toolu_2"] {
            let announced = translator.line(&format!(
                r#"{{"type":"system","subtype":"permission_denied","tool_use_id":"{id}"}}"#
            ));
            let [Event::PermissionRequest { target, .. }, ..] = announced.as_slice() else {
                panic!("the refusal: {announced:?}");
            };
            assert_eq!(target, &None, "{id}");
        }
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
                ..
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

    /// The answers Claude Code 2.1.309 gave a dry run and then the rewind
    /// itself, started with `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING`.
    #[test]
    fn files_put_back_before_a_prompt_are_one_notice_naming_them() {
        let mut translator = Translator::new("max").in_dir("/repo");
        translator.asked_to_rewind("niobe-1", "niobe-2", "p-1", "make mean() return 0");

        let dry = translator.line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-1","response":{"canRewind":true,"filesChanged":["/repo/stats.py","/repo/src/io.py"],"insertions":3,"deletions":2}}}"#,
        );
        let asked = translator.take_requests();
        let done = translator.line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-2","response":{"canRewind":true,"skippedLinks":0}}}"#,
        );

        assert!(dry.is_empty(), "{dry:?}");
        assert_eq!(asked, [rewind_request("niobe-2", "p-1", false).to_string()]);
        assert_eq!(
            done,
            [Event::Notice {
                message: "Files put back as they were before \"make mean() return 0\": \
                          stats.py, src/io.py (+3 −2). The conversation keeps everything \
                          said since."
                    .to_owned()
            }]
        );
    }

    #[test]
    fn a_rewind_the_cli_cannot_do_is_said_with_its_reason_and_not_asked_for() {
        let mut translator = Translator::new("max");
        translator.asked_to_rewind("niobe-1", "niobe-2", "p-1", "/cost");

        let dry = translator.line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-1","response":{"canRewind":false,"error":"No file checkpoint found for this message."}}}"#,
        );

        assert!(translator.take_requests().is_empty());
        assert_eq!(
            warnings(&dry),
            [
                "no files were put back as they were before \"/cost\": No file checkpoint \
              found for this message."
            ]
        );
    }

    #[test]
    fn a_rewind_the_cli_refuses_outright_is_said_with_its_reason() {
        let mut translator = Translator::new("max");
        translator.asked_to_rewind("niobe-1", "niobe-2", "p-1", "fix it");

        let dry = translator.line(
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"niobe-1","error":"Failed to rewind: File checkpointing is not enabled"}}"#,
        );

        assert!(translator.take_requests().is_empty());
        assert_eq!(
            warnings(&dry),
            [
                "no files were put back as they were before \"fix it\": Failed to rewind: \
              File checkpointing is not enabled"
            ]
        );
    }

    #[test]
    fn a_rewind_that_changed_no_file_says_so() {
        let mut translator = Translator::new("max");
        translator.asked_to_rewind("niobe-1", "niobe-2", "p-1", "explain it");

        translator.line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-1","response":{"canRewind":true,"filesChanged":[],"insertions":0,"deletions":0}}}"#,
        );
        let done = translator.line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-2","response":{"canRewind":true}}}"#,
        );

        assert_eq!(
            done,
            [Event::Notice {
                message: "No file had changed since \"explain it\", so none was put back."
                    .to_owned()
            }]
        );
    }

    #[test]
    fn a_reply_the_cli_writes_itself_keeps_each_of_its_lines() {
        let mut translator = Translator::new("max");

        // `/cost` as Claude Code 2.1.309 answers it, without the model.
        let events = translator.line(
            r#"{"type":"assistant","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"Using your subscription\n\nCurrent session: 17% used\nCurrent week: 35% used\n\n```\nkept as\nwritten\n```"}]}}"#,
        );

        assert_eq!(
            events,
            [Event::AssistantMessage {
                text: "Using your subscription\n\nCurrent session: 17% used  \nCurrent week: 35% used\n\n```\nkept as\nwritten\n```".to_owned(),
                agent: None,
            }]
        );
    }

    #[test]
    fn a_reply_the_model_wrote_is_left_as_markdown() {
        let mut translator = Translator::new("max");

        let events = translator.line(
            r#"{"type":"assistant","message":{"model":"claude-opus-5-5","role":"assistant","content":[{"type":"text","text":"one\ntwo"}]}}"#,
        );

        assert_eq!(
            events,
            [Event::AssistantMessage {
                text: "one\ntwo".to_owned(),
                agent: None,
            }]
        );
    }

    #[test]
    fn a_mode_the_cli_moves_to_mid_turn_reaches_the_session() {
        let mut translator = Translator::new("max");
        translator.line(r#"{"type":"system","subtype":"init","permissionMode":"plan"}"#);

        // As Claude Code 2.1.309 reports a plan the operator approved: the
        // CLI goes back to the mode it was in before plan, mid-turn.
        let events = translator
            .line(r#"{"type":"system","subtype":"status","status":null,"permissionMode":"auto"}"#);

        assert_eq!(events, [Event::ModeSelected { mode: Mode::Auto }]);
    }

    #[test]
    fn a_session_moved_to_a_mode_niobe_does_not_model_no_longer_shows_the_one_it_left() {
        let mut translator = Translator::new("max");
        let mut events =
            translator.line(r#"{"type":"system","subtype":"init","permissionMode":"plan"}"#);

        events.extend(translator.line(
            r#"{"type":"system","subtype":"status","status":null,"permissionMode":"bypassPermissions"}"#,
        ));

        let state = niobe_core::session::SessionState::replay(&events);
        assert_eq!(
            state.mode(),
            None,
            "the shell still shows plan while the CLI asks about nothing: {events:?}"
        );
    }

    #[test]
    fn an_init_that_repeats_a_mode_niobe_does_not_model_warns_once() {
        let mut translator = Translator::new("max");
        let init = r#"{"type":"system","subtype":"init","permissionMode":"acceptEdits"}"#;

        let first = translator.line(init);
        let again = translator.line(init);

        assert_eq!(warnings(&first).len(), 1, "{first:?}");
        assert!(warnings(&again).is_empty(), "{again:?}");
    }

    #[test]
    fn a_status_that_repeats_the_mode_the_cli_is_in_selects_nothing() {
        let mut translator = Translator::new("max");
        translator.line(r#"{"type":"system","subtype":"init","permissionMode":"plan"}"#);

        let events = translator.line(
            r#"{"type":"system","subtype":"status","status":"requesting","permissionMode":"plan"}"#,
        );

        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn a_mode_niobe_does_not_model_is_reported_once_however_often_a_status_repeats_it() {
        let mut translator = Translator::new("max");
        let status = r#"{"type":"system","subtype":"status","permissionMode":"bypassPermissions"}"#;

        let first = translator.line(status);
        let again = translator.line(status);

        assert_eq!(warnings(&first).len(), 1, "{first:?}");
        assert!(again.is_empty(), "{again:?}");
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
    fn a_refused_model_change_is_reported_as_one_and_any_other_refusal_is_not() {
        let mut translator = translator();
        translator.asked_for_model("niobe-2", "refuse");

        let mode = translator.line(
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"niobe-1","error":"Cannot set permission mode"}}"#,
        );
        let model = translator.line(
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"niobe-2","error":"model refuse is not available"}}"#,
        );

        assert_eq!(warnings(&mode).len(), 1, "{mode:?}");
        assert_eq!(mode.len(), 1, "{mode:?}");
        assert_eq!(warnings(&model).len(), 1, "{model:?}");
        assert!(
            warnings(&model)[0].contains("model refuse is not available"),
            "{model:?}"
        );
        assert_eq!(model.len(), 2, "{model:?}");
        assert_eq!(
            model[1],
            Event::ModelRefused {
                back_to: Some("opus-5".to_owned())
            }
        );
    }

    #[test]
    fn a_refusal_goes_back_to_the_model_a_taken_change_put_the_cli_on() {
        let mut translator = translator();
        translator.line(
            r#"{"type":"system","subtype":"init","model":"claude-haiku-4-5-20251001","session_id":"s"}"#,
        );
        translator.asked_for_model("niobe-2", "claude-opus-4-8");
        translator.asked_for_model("niobe-3", "refuse");

        let taken = translator.line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-2"}}"#,
        );
        let refused = translator.line(
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"niobe-3","error":"model refuse is not available"}}"#,
        );

        assert_eq!(
            taken,
            [Event::ModelSelected {
                model: "claude-opus-4-8".to_owned()
            }]
        );
        assert_eq!(
            refused[1],
            Event::ModelRefused {
                back_to: Some("claude-opus-4-8".to_owned())
            }
        );
    }

    const REFUSED_AUTO: &str = r#"{"type":"control_response","response":{"subtype":"error","request_id":"niobe-2","error":"Cannot set permission mode to auto: auto mode is unavailable for your plan"}}"#;

    #[test]
    fn a_refused_mode_change_puts_the_session_back_in_the_mode_the_cli_reported() {
        let mut translator = Translator::new("max");
        translator.line(
            r#"{"type":"system","subtype":"init","session_id":"s","model":"opus-5","permissionMode":"default"}"#,
        );
        translator.asked_for_mode("niobe-2", Mode::Auto);

        let refused = translator.line(REFUSED_AUTO);

        assert_eq!(warnings(&refused).len(), 1, "{refused:?}");
        assert!(
            warnings(&refused)[0].contains("auto mode is unavailable for your plan"),
            "{refused:?}"
        );
        assert_eq!(refused[1], Event::ModeSelected { mode: Mode::Ask });
    }

    #[test]
    fn a_mode_change_refused_before_the_cli_reported_a_mode_goes_back_to_the_one_it_started_in() {
        let mut translator = Translator::new("max").started_in(Mode::Plan);
        translator.asked_for_mode("niobe-2", Mode::Auto);

        let refused = translator.line(REFUSED_AUTO);

        assert_eq!(refused[1], Event::ModeSelected { mode: Mode::Plan });
    }

    #[test]
    fn a_refused_mode_change_goes_back_to_the_mode_a_taken_change_put_the_cli_in() {
        let mut translator = Translator::new("max").started_in(Mode::Ask);
        translator.asked_for_mode("niobe-1", Mode::Plan);
        translator.asked_for_mode("niobe-2", Mode::Auto);

        let taken = translator.line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-1","response":{"mode":"plan"}}}"#,
        );
        let refused = translator.line(REFUSED_AUTO);

        assert!(taken.is_empty(), "{taken:?}");
        assert_eq!(refused[1], Event::ModeSelected { mode: Mode::Plan });
    }

    #[test]
    fn a_refused_mode_change_with_a_later_one_still_unanswered_leaves_the_session_on_the_later() {
        let mut translator = Translator::new("max").started_in(Mode::Ask);
        translator.asked_for_mode("niobe-2", Mode::Auto);
        translator.asked_for_mode("niobe-3", Mode::Plan);

        let refused = translator.line(REFUSED_AUTO);

        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(warnings(&refused).len(), 1, "{refused:?}");
    }

    #[test]
    fn a_refusal_with_no_change_taken_before_it_goes_back_to_the_models_the_cli_started_on() {
        let mut translator = translator();
        translator.line(
            r#"{"type":"system","subtype":"init","model":"claude-haiku-4-5-20251001","session_id":"s"}"#,
        );
        translator.asked_for_model("niobe-2", "refuse");

        let refused = translator.line(
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"niobe-2","error":"nope"}}"#,
        );

        assert_eq!(
            refused[1],
            Event::ModelRefused {
                back_to: Some("claude-haiku-4-5-20251001".to_owned())
            }
        );
    }

    #[test]
    fn the_title_the_cli_answers_with_titles_the_session() {
        let mut translator = translator();
        translator.asked_for_title("niobe-2");

        let events = translator.line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-2","response":{"title":"Static file handler etag support"}}}"#,
        );

        assert_eq!(
            events,
            [Event::Titled {
                title: "Static file handler etag support".to_owned()
            }]
        );
    }

    #[test]
    fn a_title_is_trimmed_and_a_blank_one_is_none() {
        let mut translator = translator();
        translator.asked_for_title("niobe-2");
        translator.asked_for_title("niobe-3");

        let padded = translator.line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-2","response":{"title":"  Etag support\n"}}}"#,
        );
        let blank = translator.line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-3","response":{"title":" \n "}}}"#,
        );

        assert_eq!(
            padded,
            [Event::Titled {
                title: "Etag support".to_owned()
            }]
        );
        assert!(blank.is_empty(), "{blank:?}");
    }

    #[test]
    fn a_title_the_cli_did_not_make_leaves_the_caption_to_the_first_prompt() {
        let mut translator = translator();
        translator.asked_for_title("niobe-2");
        translator.asked_for_title("niobe-3");

        let none = translator.line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-2","response":{"title":null}}}"#,
        );
        let refused = translator.line(
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"niobe-3","error":"Unsupported control request subtype: generate_session_title"}}"#,
        );

        assert!(none.is_empty(), "{none:?}");
        assert!(
            warnings(&refused).is_empty(),
            "no change was refused, so none is said to be: {refused:?}"
        );
        assert!(
            matches!(
                refused.as_slice(),
                [Event::Notice { message }]
                    if message.contains("first prompt")
                        && message.contains("Unsupported control request subtype")
            ),
            "{refused:?}"
        );
    }

    #[test]
    fn a_model_change_the_cli_took_is_not_refused_by_a_later_answer_to_the_same_id() {
        let mut translator = translator();
        translator.asked_for_model("niobe-2", "refuse");

        let taken = translator.line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-2"}}"#,
        );
        let again = translator.line(
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"niobe-2","error":"said twice"}}"#,
        );

        assert_eq!(
            taken,
            [Event::ModelSelected {
                model: "refuse".to_owned()
            }]
        );
        assert_eq!(again.len(), 1, "{again:?}");
        assert_eq!(warnings(&again).len(), 1, "{again:?}");
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
            differences(&events).is_empty(),
            "a turn the CLI totalled nothing for was reported as a mismatch: {events:?}"
        );
        assert_eq!(said.len(), 1, "{said:?}");
    }

    const LIMIT_WORDS: &str = "You've hit your session limit · resets 11:10pm (Europe/Berlin)";

    /// The shape of the CLI's own note when a plan's window is spent, from the
    /// transcripts of 2.1.292 on 7 October 2026: a message no model wrote,
    /// with the failure named beside it.
    fn limit_message(parent: Option<&str>) -> String {
        let parent = parent.map_or("null".to_owned(), |id| format!("\"{id}\""));
        format!(
            r#"{{"type":"assistant","parent_tool_use_id":{parent},"error":"rate_limit","apiError":"usage_limit_reached","message":{{"model":"<synthetic>","id":"m-limit","role":"assistant","content":[{{"type":"text","text":"{LIMIT_WORDS}"}}],"usage":{{"input_tokens":0,"output_tokens":0}}}}}}"#
        )
    }

    fn limit_result() -> String {
        format!(
            r#"{{"type":"result","subtype":"success","is_error":true,"result":"{LIMIT_WORDS}","usage":{{"input_tokens":0,"output_tokens":0}}}}"#
        )
    }

    #[test]
    fn a_usage_limit_hit_is_said_once_and_is_no_error() {
        let mut translator = translator();

        let mut events = translator.line(&limit_message(None));
        events.extend(translator.line(&limit_result()));

        assert_eq!(limits(&events), [LIMIT_WORDS.to_owned()], "{events:?}");
        assert!(
            !notices(&events)
                .iter()
                .any(|message| message.contains("session limit")),
            "the limit was said again as a notice: {events:?}"
        );
        assert!(warnings(&events).is_empty(), "{events:?}");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::AssistantMessage { .. })),
            "the limit was also shown as a reply: {events:?}"
        );
        assert!(
            matches!(events.last(), Some(Event::TurnEnded)),
            "{events:?}"
        );
    }

    #[test]
    fn a_limit_hit_the_cli_names_only_in_the_result_is_still_said_once() {
        let mut translator = translator();

        let events = translator.line(&limit_result());

        assert_eq!(limits(&events), [LIMIT_WORDS.to_owned()], "{events:?}");
        assert!(notices(&events).is_empty(), "{events:?}");
        assert!(warnings(&events).is_empty(), "{events:?}");
    }

    #[test]
    fn a_sub_agent_the_limit_stopped_is_stopped_and_says_nothing_of_its_own() {
        let mut translator = translator();
        translator.line(&agent_call("toolu_a", "Agent"));

        let mut events = translator.line(&limit_message(Some("toolu_a")));
        events.extend(translator.line(&result("toolu_a", &format!("\"{LIMIT_WORDS}\""), true)));

        assert!(notices(&events).is_empty(), "{events:?}");
        assert!(limits(&events).is_empty(), "{events:?}");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::AssistantMessage { .. })),
            "{events:?}"
        );
        assert_eq!(
            exits(&events),
            [("toolu_a".to_owned(), AgentOutcome::Cancelled)]
        );
    }

    #[test]
    fn a_failure_that_is_not_the_limit_still_reports_as_one() {
        let mut translator = translator();
        translator.line(&limit_message(None));

        let events = translator.line(
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"the provider refused"}"#,
        );

        assert_eq!(warnings(&events).len(), 1, "{events:?}");
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
                overage: Overage::Refused(OverageRefusal::OutOfCredits),
            })]
        );
    }

    /// Each status and reason Claude Code 2.1.309's schema lists, and what
    /// the bridge makes of it; an unknown status is nothing said.
    #[test]
    fn whether_extra_usage_is_allowed_is_read_as_the_cli_said_it() {
        for (status, reason, read) in [
            ("allowed", "", Overage::Allowed),
            ("allowed_warning", "", Overage::Allowed),
            (
                "rejected",
                "out_of_credits",
                Overage::Refused(OverageRefusal::OutOfCredits),
            ),
            (
                "rejected",
                "overage_not_provisioned",
                Overage::Refused(OverageRefusal::NotSetUp),
            ),
            (
                "rejected",
                "no_limits_configured",
                Overage::Refused(OverageRefusal::NotSetUp),
            ),
            (
                "rejected",
                "org_level_disabled",
                Overage::Refused(OverageRefusal::TurnedOff),
            ),
            (
                "rejected",
                "org_level_disabled_until",
                Overage::Refused(OverageRefusal::TurnedOff),
            ),
            (
                "rejected",
                "org_service_level_disabled",
                Overage::Refused(OverageRefusal::TurnedOff),
            ),
            (
                "rejected",
                "seat_tier_level_disabled",
                Overage::Refused(OverageRefusal::TurnedOff),
            ),
            (
                "rejected",
                "member_level_disabled",
                Overage::Refused(OverageRefusal::TurnedOff),
            ),
            (
                "rejected",
                "seat_tier_zero_credit_limit",
                Overage::Refused(OverageRefusal::ZeroLimit),
            ),
            (
                "rejected",
                "group_zero_credit_limit",
                Overage::Refused(OverageRefusal::ZeroLimit),
            ),
            (
                "rejected",
                "member_zero_credit_limit",
                Overage::Refused(OverageRefusal::ZeroLimit),
            ),
            (
                "rejected",
                "fetch_error",
                Overage::Refused(OverageRefusal::Unstated),
            ),
            (
                "rejected",
                "unknown",
                Overage::Refused(OverageRefusal::Unstated),
            ),
            ("rejected", "", Overage::Refused(OverageRefusal::Unstated)),
            ("something_new", "out_of_credits", Overage::Unreported),
            ("", "", Overage::Unreported),
        ] {
            assert_eq!(overage(status, reason), read, "{status} / {reason}");
        }
    }

    /// A message that says only that overage is refused still reaches the
    /// session: it is the backend saying no, which is worth showing.
    #[test]
    fn a_refusal_of_extra_usage_with_no_window_is_still_reported() {
        let mut translator = translator();

        let events = translator.line(
            r#"{"type":"rate_limit_event","rate_limit_info":{"overageStatus":"rejected","overageDisabledReason":"out_of_credits"}}"#,
        );

        assert_eq!(
            events,
            [Event::UsageWindows(UsageWindows {
                five_hour: None,
                seven_day: None,
                using_overage: false,
                overage: Overage::Refused(OverageRefusal::OutOfCredits),
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
    /// The overage flag is read on its own: a message that names no window
    /// still says the plan is spending beyond its fee.
    #[test]
    fn overage_with_no_window_is_still_reported() {
        let mut translator = translator();

        let events = translator.line(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","isUsingOverage":true}}"#,
        );

        assert_eq!(
            events,
            [Event::UsageWindows(UsageWindows {
                five_hour: None,
                seven_day: None,
                using_overage: true,
                overage: niobe_core::event::Overage::Unreported,
            })]
        );
    }

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
                overage: niobe_core::event::Overage::Unreported,
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

    /// Whose file each change in the events changed.
    fn scopes(events: &[Event]) -> Vec<niobe_core::event::ChangeScope> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::FileChange { scope, .. } => Some(*scope),
                _ => None,
            })
            .collect()
    }

    fn edited(translator: &mut Translator, id: &str, path: &str) -> Vec<Event> {
        translator.line(&call(
            id,
            "Edit",
            &format!(r#"{{"file_path":"{path}","old_string":"a","new_string":"b"}}"#),
        ));
        translator.line(&result(
            id,
            &format!("The file {path} has been updated successfully."),
            false,
        ))
    }

    /// The CLI keeps the agent's notes on a project in `memory/` beside the
    /// project's transcripts, under its own configuration directory.
    #[test]
    fn an_edit_to_the_agents_notes_is_marked_as_its_memory_and_one_to_the_repository_is_not() {
        use niobe_core::event::ChangeScope;

        let mut translator = translator().in_dir("/w/app");
        let memory = edited(
            &mut translator,
            "t1",
            "/home/me/.claude/projects/-w-app/memory/notes.md",
        );
        let index = edited(
            &mut translator,
            "t2",
            "/home/me/.claude/projects/-w-app/memory/MEMORY.md",
        );
        let project = edited(&mut translator, "t3", "/w/app/src/lib.rs");

        assert_eq!(scopes(&memory), [ChangeScope::AgentMemory]);
        assert_eq!(scopes(&index), [ChangeScope::AgentMemory]);
        assert_eq!(scopes(&project), [ChangeScope::Project]);
    }

    /// Only the CLI's own layout is memory: a `memory/` directory inside the
    /// repository is the project's, and so is a path that only looks like
    /// the layout from somewhere the CLI keeps nothing.
    #[test]
    fn a_memory_directory_anywhere_else_is_the_projects() {
        use niobe_core::event::ChangeScope;

        let mut translator = translator().in_dir("/w/app");
        for (id, path) in [
            ("t1", "/w/app/projects/-w-app/memory/notes.md"),
            ("t2", "/w/app/memory/notes.md"),
            ("t3", "/home/me/projects/my app/memory/notes.md"),
            ("t4", "/home/me/.claude/projects/-w-app/notes.md"),
        ] {
            assert_eq!(
                scopes(&edited(&mut translator, id, path)),
                [ChangeScope::Project],
                "{path}"
            );
        }
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
                .expect("the hunk's counts match its lines")
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
                    .expect("the hunk's counts match its lines")
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

    /// A `Write` carries what the file becomes and never what it was, so with
    /// no patch beside it neither count is known: overwriting a five-line
    /// file with one line changed is `1 1` to `git diff --numstat`, and the
    /// call wrote five.
    #[test]
    fn an_overwrite_with_no_patch_states_neither_count() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Write",
            r#"{"file_path":"/repo/f.txt","content":"a\nb\nC\nd\ne\n"}"#,
        ));

        let events = translator.line(&result(
            "t1",
            "The file /repo/f.txt has been updated successfully.",
            false,
        ));

        assert_eq!(
            changes(&events),
            vec![("f.txt".to_owned(), None, None)],
            "{events:?}"
        );
    }

    /// The arguments stop mid-line; the CLI's hunks are of whole lines, and
    /// `git diff --numstat` on the same edit says `2 1`.
    #[test]
    fn an_edit_that_stops_mid_line_is_counted_from_the_clis_hunks() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Edit",
            r#"{"file_path":"/repo/f.c","old_string":"foo();","new_string":"foo();\nbar();"}"#,
        ));

        let events = translator.line(&result_with(
            "t1",
            UPDATED,
            r#"{"filePath":"/repo/f.c","structuredPatch":[{"oldStart":1,"oldLines":1,"newStart":1,"newLines":2,"lines":["-    foo(); // c","+    foo();","+bar(); // c"]}]}"#,
        ));

        assert_eq!(
            changes(&events),
            vec![("f.c".to_owned(), Some(2), Some(1))],
            "{events:?}"
        );
    }

    /// A sub-agent's results carry no report on the live stream, so every
    /// file one overwrites is a file whose counts nothing states.
    #[test]
    fn a_sub_agents_overwrite_states_neither_count() {
        let mut translator = translator().in_dir("/repo");
        translator.line(
            r#"{"type":"assistant","parent_tool_use_id":"toolu_task","message":{"content":[{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"/repo/f.txt","content":"a\nb\nC\nd\ne\n"}}]}}"#,
        );

        let events = translator.line(
            r#"{"type":"user","parent_tool_use_id":"toolu_task","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":false,"content":"The file /repo/f.txt has been updated successfully."}]}}"#,
        );

        assert_eq!(
            changes(&events),
            vec![("f.txt".to_owned(), None, None)],
            "{events:?}"
        );
    }

    /// Written as it stood: the CLI reports an update whose patch has no
    /// hunks, and `git diff --numstat` shows no change.
    #[test]
    fn an_overwrite_reported_with_an_empty_patch_changed_nothing() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Write",
            r#"{"file_path":"/repo/f.txt","content":"a\nb\nc\n"}"#,
        ));

        let events = translator.line(&result_with(
            "t1",
            "The file /repo/f.txt has been updated successfully.",
            r#"{"type":"update","filePath":"/repo/f.txt","content":"a\nb\nc\n","structuredPatch":[],"originalFile":"a\nb\nc\n"}"#,
        ));

        assert_eq!(
            changes(&events),
            vec![("f.txt".to_owned(), Some(0), Some(0))],
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

        let spawned: Vec<(&str, Option<&str>, &str)> = events
            .iter()
            .filter_map(|event| match event {
                Event::AgentSpawn {
                    id, kind, label, ..
                } => Some((id.as_str(), kind.as_deref(), label.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            spawned,
            [(
                "toolu_a",
                Some("quick-lookup"),
                "quick-lookup: Summarize catalog/cache.py"
            )]
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
    fn a_background_sub_agent_whose_call_failed_ended_with_it() {
        let mut translator = translator();
        translator.line(&agent_call("toolu_a", "Agent"));
        let failed = launched("toolu_a").replace(
            r#""tool_use_id":"toolu_a","#,
            r#""tool_use_id":"toolu_a","is_error":true,"#,
        );

        let events = translator.line(&failed);

        assert_eq!(
            exits(&events),
            [("toolu_a".to_owned(), AgentOutcome::Failed)]
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

    /// A shell call put in the background, and the notification of its end.
    fn backgrounded(translator: &mut Translator, command: &str) {
        translator.line(&call(
            "toolu_bash",
            "Bash",
            &format!(r#"{{"command":{}}}"#, serde_json::json!(command)),
        ));
        translator.line(&result_with(
            "toolu_bash",
            "Command running in background with ID: b1.",
            &shell_report(r#","backgroundTaskId":"b1""#),
        ));
    }

    fn command_notified(status: &str, summary: &str) -> String {
        format!(
            r#"{{"type":"system","subtype":"task_notification","task_id":"b1","tool_use_id":"toolu_bash","status":"{status}","output_file":"/tmp/b1.output","summary":"{summary}"}}"#
        )
    }

    #[test]
    fn a_background_commands_end_is_told_once_with_the_exit_code_its_summary_names() {
        let mut translator = translator();
        backgrounded(&mut translator, "cargo test\n--release");

        let ended = translator.line(&command_notified(
            "failed",
            r#"Background command \"Run the tests\" failed with exit code 101"#,
        ));
        let again = translator.line(&command_notified("failed", "failed"));

        assert_eq!(
            ended,
            [Event::Notice {
                message: "the background command `cargo test …` failed with exit code 101"
                    .to_owned()
            }]
        );
        assert!(again.is_empty(), "{again:?}");
    }

    #[test]
    fn a_background_command_stopped_without_a_summary_says_only_that() {
        let mut translator = translator();
        backgrounded(&mut translator, "sleep 60");

        let events = translator.line(
            r#"{"type":"system","subtype":"task_notification","tool_use_id":"toolu_bash","status":"stopped"}"#,
        );

        assert_eq!(
            events,
            [Event::Notice {
                message: "the background command `sleep 60` was stopped".to_owned()
            }]
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
                r#"{"query":"release notes","page_size":10,"filters":{"a":1},"ids":["x","y"]}"#,
                "query: release notes, page_size: 10, filters: {…}, ids: [2]",
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
    fn a_finished_shell_call_carries_its_whole_command_and_no_other_call_does() {
        let mut translator = translator().in_dir("/repo");
        translator.line(&call(
            "t1",
            "Bash",
            r#"{"command":"cd crates\ncargo test","description":"Run the tests"}"#,
        ));
        translator.line(&call("t2", "Read", r#"{"file_path":"/repo/notes.txt"}"#));

        let mut events = translator.line(&result("t1", "ok", false));
        events.extend(translator.line(&result("t2", "three lines", false)));

        let commands: Vec<Option<String>> = events
            .iter()
            .filter_map(|event| match event {
                Event::ToolCallEnd { command, .. } => Some(command.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            commands,
            [Some("cd crates\ncargo test".to_owned()), None],
            "{events:?}"
        );
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
            "/whole.txt" | "/once.txt" | "/repo/.niobe/trim/t1.txt" | "/repo/trim/t1.txt" => {
                PASSING_RUN
            }
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

    /// The result of a passing run Niobe's hook trimmed, naming `path` as
    /// where it wrote the whole.
    fn trimmed_by_niobe(path: &str, size: usize) -> String {
        result_with(
            "t1",
            "test result: ok. 2 passed\n[niobe left out 2 lines]",
            &shell_report(&format!(
                r#","niobeRawOutputPath":"{path}","niobeRawOutputSize":{size}"#
            )),
        )
    }

    fn ended_call(events: &[Event]) -> (&str, u64, Option<&str>) {
        events
            .iter()
            .find_map(|event| match event {
                Event::ToolCallEnd {
                    output,
                    bytes,
                    untrimmed,
                    ..
                } => Some((output.as_str(), *bytes, untrimmed.as_deref())),
                _ => None,
            })
            .expect("the result ends the call")
    }

    #[test]
    fn an_output_niobe_trimmed_carries_its_whole_and_is_sized_and_counted_by_it() {
        let mut translator = translator().reading_spilled_with(read_spilled);
        shell_call(&mut translator);

        let events = translator.line(&trimmed_by_niobe(
            "/repo/.niobe/trim/t1.txt",
            PASSING_RUN.len(),
        ));

        let (output, bytes, untrimmed) = ended_call(&events);
        assert!(output.starts_with("test result: ok. 2 passed"));
        assert_eq!(untrimmed, Some(PASSING_RUN));
        assert_eq!(bytes, PASSING_RUN.len() as u64);
        let counts: Vec<_> = test_runs(&events).into_iter().map(|(c, _)| c).collect();
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0].map(|c| (c.passed, c.suites)), Some((2, 1)));
    }

    #[test]
    fn a_trimmed_output_is_read_back_only_from_niobes_own_directory_and_at_its_size() {
        for (path, size) in [
            ("/repo/trim/t1.txt", PASSING_RUN.len()),
            ("/repo/.niobe/trim/t1.txt", PASSING_RUN.len() + 1),
        ] {
            let mut translator = translator().reading_spilled_with(read_spilled);
            shell_call(&mut translator);

            let events = translator.line(&trimmed_by_niobe(path, size));

            let (output, bytes, untrimmed) = ended_call(&events);
            assert_eq!(untrimmed, None, "{path}");
            assert_eq!(bytes, output.len() as u64, "{path}");
        }
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
    fn a_commit_whose_message_mentions_cargo_test_leaves_the_failing_run_shown() {
        let mut translator = translator();
        let mut state = niobe_core::session::SessionState::new();
        shell_call(&mut translator);
        let commit = serde_json::json!({
            "command": "git commit -m \"fix: parser; cargo test now covers it\"",
        });
        for line in [
            result("t1", &format!("Exit code 101\n{TEST_RUN}"), true),
            call("t2", "Bash", &commit.to_string()),
            result(
                "t2",
                "[main 1a2b3c4] fix: parser; cargo test now covers it",
                false,
            ),
        ] {
            for event in translator.line(&line) {
                state.apply(&event);
            }
        }

        let run = state.test_run().expect("the failing run is reported");
        assert!(run.failed, "{run:?}");
        assert_eq!(run.exit_code, Some(101));
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

    /// Writing a script that runs the tests is not running them, so the
    /// session's last run stays the one that ran.
    #[test]
    fn a_here_document_that_names_cargo_test_leaves_the_last_real_run_standing() {
        let mut translator = translator();
        shell_call(&mut translator);
        let mut events =
            translator.line(&result("t1", &format!("Exit code 101\n{TEST_RUN}"), true));
        translator.line(&call(
            "t2",
            "Bash",
            r#"{"command":"cat > run.sh <<'EOF'\n#!/bin/sh\ncargo test\nEOF"}"#,
        ));
        events.extend(translator.line(&result_with("t2", "", &shell_report(""))));

        let state = niobe_core::SessionState::replay(&events);
        let counts = state
            .test_run()
            .and_then(|run| run.counts)
            .expect("the real run's counts stand");
        assert_eq!((counts.passed, counts.failed), (1, 1));
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

    #[test]
    fn a_servers_tools_are_found_by_the_name_the_cli_writes_it_under() {
        let tools: Vec<String> = [
            "Read",
            "mcp__claude_ai_Notion__notion-search",
            "mcp__code-review-graph__query_graph",
            "mcp__claude_ai_Notion_extra__fetch",
        ]
        .map(str::to_owned)
        .to_vec();

        assert_eq!(server_tools("claude.ai Notion", &tools), ["notion-search"]);
        assert_eq!(server_tools("code-review-graph", &tools), ["query_graph"]);
        assert!(server_tools("Read", &tools).is_empty());
    }

    #[test]
    fn a_server_status_niobe_has_no_word_for_is_kept_in_the_clis() {
        assert_eq!(mcp_status("needs-auth".to_owned()), McpStatus::NeedsAuth);
        assert_eq!(
            mcp_status("reconnecting".to_owned()),
            McpStatus::Other("reconnecting".to_owned())
        );
    }

    #[test]
    fn an_mcp_servers_address_is_kept_without_its_credentials_path_or_query() {
        assert_eq!(
            without_secrets("https://user:key@mcp.example.com:8443/v1/mcp?token=abc#x"),
            "https://mcp.example.com:8443/…"
        );
        assert_eq!(
            without_secrets("http://127.0.0.1:9/mcp"),
            "http://127.0.0.1:9/…"
        );
        assert_eq!(without_secrets("http://127.0.0.1:9"), "http://127.0.0.1:9");
        assert_eq!(without_secrets("http://127.0.0.1:9/"), "http://127.0.0.1:9");
        // The three shapes the key was found in.
        assert_eq!(
            without_secrets("https://actions.zapier.com/mcp/sk-ak-SECRET123/sse"),
            "https://actions.zapier.com/…"
        );
        assert_eq!(
            without_secrets("user:pw@host.example/mcp"),
            "host.example/…"
        );
        assert_eq!(
            without_secrets("https://host.example;token=abc/mcp"),
            "https://host.example"
        );
        assert_eq!(
            without_secrets("https://host/mcp;token=abc"),
            "https://host/…"
        );
        // A password holding an `@` ends at the last one: none of it is kept.
        assert_eq!(
            without_secrets("https://user:p@ss@host.example/mcp"),
            "https://host.example/…"
        );
        let config =
            |transport: Option<&str>, command: Option<&str>, url: Option<&str>| wire::McpConfig {
                transport: transport.map(str::to_owned),
                command: command.map(str::to_owned),
                url: url.map(str::to_owned),
            };
        assert_eq!(
            endpoint(config(None, Some("npx"), None)).as_deref(),
            Some("stdio · npx")
        );
        assert_eq!(
            endpoint(config(Some("sse"), None, Some("https://x.dev/sse?key=1"))).as_deref(),
            Some("sse · https://x.dev/…")
        );
        assert_eq!(endpoint(config(None, None, None)), None);
    }

    const STATUS_FAILED: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-2","response":{"mcpServers":[{"name":"docs","status":"failed","error":"ECONNREFUSED","scope":"project"}]}}}"#;

    fn init_with(status: &str) -> String {
        format!(
            r#"{{"type":"system","subtype":"init","session_id":"s","mcp_servers":[{{"name":"docs","status":"{status}","source":"project"}}]}}"#
        )
    }

    /// An answer that leaves a server out says nothing of it: asking again
    /// after every turn would be answered the same, and store a list each time.
    #[test]
    fn a_server_the_answer_left_out_is_not_asked_about_again_while_it_stands_so() {
        let mut translator = Translator::new("max");
        let init = |docs: &str| {
            format!(
                r#"{{"type":"system","subtype":"init","session_id":"s","mcp_servers":[{{"name":"docs","status":"{docs}","source":"project"}},{{"name":"ghost","status":"pending"}}]}}"#
            )
        };
        translator.line(&init("connected"));
        assert!(
            translator.take_mcp_status_wanted(),
            "a first listing is asked about"
        );
        translator.asked_for_mcp_status("niobe-2");
        translator.line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-2","response":{"mcpServers":[{"name":"docs","status":"connected","scope":"project"}]}}}"#,
        );

        for _ in 0..3 {
            translator.line(&init("connected"));
            assert!(
                !translator.take_mcp_status_wanted(),
                "the omitted server stands as it did"
            );
        }

        translator.line(&init("failed"));
        assert!(
            translator.take_mcp_status_wanted(),
            "a server that changed is asked about"
        );
    }

    /// One server's entry is not worth the whole `init`: its session id and
    /// model still arrive, and the entry reads as not reported.
    #[test]
    fn an_init_with_a_malformed_server_entry_still_yields_the_session() {
        for status in ["", r#","status":null"#, r#","status":5"#] {
            let mut translator = Translator::new("max");
            let events = translator.line(&format!(
                r#"{{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5","mcp_servers":[{{"name":"docs"{status}}}]}}"#
            ));

            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, Event::SessionMeta(_))),
                "{status}: {events:?}"
            );
            let servers = events.iter().find_map(|event| match event {
                Event::McpServers { servers } => Some(servers),
                _ => None,
            });
            assert!(
                servers.is_some_and(|servers| servers.len() == 1
                    && servers[0].status == McpStatus::Other("not reported".to_owned())),
                "{status}: {events:?}"
            );
        }
    }

    #[test]
    fn a_mistyped_overage_flag_or_a_fractional_reset_still_reports_the_windows() {
        let mut translator = Translator::new("max");
        let events = translator.line(
            r#"{"type":"rate_limit_event","rate_limit_info":{"isUsingOverage":null,"unifiedWindows":{"five_hour":{"utilization":0.5,"resetsAt":1790000000.5}}}}"#,
        );

        assert!(
            events.iter().any(
                |event| matches!(event, Event::UsageWindows(windows) if windows.five_hour.is_some())
            ),
            "{events:?}"
        );
    }

    #[test]
    fn a_context_window_of_nothing_is_no_window() {
        let mut translator = Translator::new("max");
        translator.line(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":1,"output_tokens":1},"modelUsage":{"opus-5":{"inputTokens":1,"outputTokens":1,"costUSD":0.01,"contextWindow":0}},"total_cost_usd":0.01}"#,
        );
        assert_eq!(translator.windows.get("opus-5"), None);
    }

    #[test]
    fn a_server_that_stands_otherwise_than_the_cli_last_said_is_asked_about_again() {
        let mut translator = Translator::new("max");
        translator.asked_for_mcp_status("niobe-2");
        translator.line(STATUS_FAILED);
        assert!(!translator.take_mcp_status_wanted());

        translator.line(&init_with("failed"));
        assert!(
            !translator.take_mcp_status_wanted(),
            "a server that failed as it did is asked about again"
        );
        let events = translator.line(&init_with("connected"));
        assert!(translator.take_mcp_status_wanted());
        let Some(Event::McpServers { servers }) = events
            .iter()
            .find(|event| matches!(event, Event::McpServers { .. }))
        else {
            panic!("no server list: {events:?}");
        };
        assert_eq!(
            servers[0].error, None,
            "a connected server kept its failure"
        );
        assert_eq!(servers[0].scope.as_deref(), Some("project"));
    }

    #[test]
    fn a_cli_that_refuses_mcp_status_is_not_asked_again_and_nothing_is_said() {
        let mut translator = Translator::new("max");
        translator.asked_for_mcp_status("niobe-2");
        let events = translator.line(
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"niobe-2","error":"Unsupported control request subtype: mcp_status"}}"#,
        );
        assert_eq!(events, []);
        translator.line(&init_with("failed"));
        assert!(!translator.take_mcp_status_wanted());
    }

    #[test]
    fn a_reconnect_answered_either_way_has_the_servers_asked_about_again() {
        let mut translator = Translator::new("max");
        translator.asked_to_reconnect("niobe-3", "docs");
        let events = translator.line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"niobe-3"}}"#,
        );
        assert_eq!(events, []);
        assert!(translator.take_mcp_status_wanted());
        assert!(!translator.take_mcp_status_wanted(), "asked about twice");
    }

    /// A token count the API's own schema lets be `null`, as a gateway in
    /// front of it writes one, is a count that was not reported.
    #[test]
    fn a_token_count_written_as_null_still_ends_the_turn_and_counts_the_rest() {
        let mut translator = translator();
        let events = translator.line(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":5,"output_tokens":7,"cache_read_input_tokens":null,"cache_creation_input_tokens":null},"modelUsage":{"opus-5":{"inputTokens":5,"outputTokens":7,"cacheReadInputTokens":null,"costUSD":0.1}},"total_cost_usd":0.1}"#,
        );
        assert_eq!(events.last(), Some(&Event::TurnEnded), "{events:?}");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::Error { .. })),
            "{events:?}"
        );
        assert!((cost_reported(&events) - 0.1).abs() < 1e-9, "{events:?}");
    }

    #[test]
    fn a_count_the_protocol_cannot_mean_still_ends_the_turn_and_counts_none_of_its_figures() {
        for odd in ["-3", "1.5", "\"12\"", "18446744073709551616"] {
            let mut translator = translator();
            let events = translator.line(&format!(
                r#"{{"type":"result","subtype":"success","usage":{{"input_tokens":{odd},"output_tokens":7}},"modelUsage":{{"opus-5":{{"inputTokens":5,"outputTokens":7,"costUSD":0.1}}}},"total_cost_usd":0.1}}"#
            ));
            assert_eq!(events.last(), Some(&Event::TurnEnded), "{odd}: {events:?}");
            assert!(
                !events.iter().any(|event| matches!(event, Event::Usage(_))),
                "{odd}: a figure was counted from an unreadable bill: {events:?}"
            );
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, Event::Error { fatal: false, .. })),
                "{odd}: nothing said the figures were not read: {events:?}"
            );
        }
    }

    #[test]
    fn a_reply_whose_usage_cannot_be_read_is_still_shown() {
        let mut translator = translator();
        let events = translator.line(
            r#"{"type":"assistant","message":{"model":"opus-5","id":"msg_1","content":[{"type":"text","text":"Done."}],"usage":{"input_tokens":1,"output_tokens":-1}}}"#,
        );
        assert!(
            events.iter().any(
                |event| matches!(event, Event::AssistantMessage { text, .. } if text == "Done.")
            ),
            "{events:?}"
        );
    }

    fn bill(model_cost: &str, total: &str) -> String {
        format!(
            r#"{{"type":"result","subtype":"success","modelUsage":{{"opus-5":{{"inputTokens":10,"outputTokens":1,"costUSD":{model_cost}}}}},"total_cost_usd":{total}}}"#
        )
    }

    fn session_bill(total: &str) -> String {
        format!(r#"{{"type":"result","subtype":"success","total_cost_usd":{total}}}"#)
    }

    fn reported_after(translator: &mut Translator, lines: &[String]) -> (f64, Vec<Event>) {
        let events: Vec<Event> = lines
            .iter()
            .flat_map(|line| translator.line(line))
            .collect();
        let totals = niobe_core::session::SessionState::replay(&events)
            .totals()
            .clone();
        (totals.reported_cost_usd, events)
    }

    fn warned(events: &[Event]) -> bool {
        events
            .iter()
            .any(|event| matches!(event, Event::Error { fatal: false, .. }))
    }

    /// The costs no bill can be: below nothing, a string, and more than any
    /// session could run up.
    const UNMEANT_COSTS: [&str; 4] = ["-5", "\"0.5\"", "1e308", "2000000"];

    #[test]
    fn a_cost_no_bill_can_be_is_no_figure_and_not_what_the_next_bill_is_measured_from() {
        for odd in UNMEANT_COSTS {
            let mut translator = translator();
            let (cost, events) = reported_after(
                &mut translator,
                &[bill("0.3", "0.3"), bill(odd, odd), bill("0.8", "0.8")],
            );
            assert!((cost - 0.8).abs() < 1e-9, "{odd}: {cost} from {events:?}");
            assert!(warned(&events), "{odd}: nothing said: {events:?}");
            assert_eq!(
                events.iter().filter(|e| **e == Event::TurnEnded).count(),
                3,
                "{odd}: {events:?}"
            );
        }
    }

    #[test]
    fn a_cost_no_bill_can_be_on_the_first_bill_leaves_the_next_measured_from_nothing() {
        for odd in UNMEANT_COSTS {
            let mut translator = translator();
            let (cost, events) =
                reported_after(&mut translator, &[bill(odd, odd), bill("0.5", "0.5")]);
            assert!((cost - 0.5).abs() < 1e-9, "{odd}: {cost} from {events:?}");
            assert!(warned(&events), "{odd}: nothing said: {events:?}");
        }
    }

    #[test]
    fn a_session_total_no_bill_can_be_is_no_figure_and_not_a_base() {
        for odd in UNMEANT_COSTS {
            let mut translator = translator();
            let (cost, events) = reported_after(
                &mut translator,
                &[session_bill("0.3"), session_bill(odd), session_bill("0.8")],
            );
            assert!((cost - 0.8).abs() < 1e-9, "{odd}: {cost} from {events:?}");
            assert!(warned(&events), "{odd}: nothing said: {events:?}");
        }
    }

    /// A resumed session whose earlier spend is unknown takes its first bill
    /// as the base; a first bill that is no bill cannot be one, so the next
    /// readable one is taken instead and the turn between stays uncounted.
    #[test]
    fn a_resumed_first_bill_no_bill_can_be_leaves_the_next_one_as_the_base() {
        for odd in UNMEANT_COSTS {
            let mut translator = Translator::new("max").resuming(&Spent::unknown("unreadable"));
            translator
                .line(r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5"}"#);
            let (cost, events) = reported_after(
                &mut translator,
                &[bill(odd, odd), bill("0.7", "0.7"), bill("0.9", "0.9")],
            );
            assert!((cost - 0.2).abs() < 1e-9, "{odd}: {cost} from {events:?}");
            assert!(warned(&events), "{odd}: nothing said: {events:?}");
        }
    }

    #[test]
    fn a_resumed_session_total_no_bill_can_be_leaves_the_next_one_as_the_base() {
        for odd in UNMEANT_COSTS {
            let mut translator = Translator::new("max").resuming(&Spent::unknown("unreadable"));
            translator
                .line(r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5"}"#);
            let (cost, events) = reported_after(
                &mut translator,
                &[session_bill(odd), session_bill("0.7"), session_bill("0.9")],
            );
            assert!((cost - 0.2).abs() < 1e-9, "{odd}: {cost} from {events:?}");
            assert!(warned(&events), "{odd}: nothing said: {events:?}");
        }
    }

    #[test]
    fn a_recorded_spend_holding_a_cost_no_bill_can_be_is_unknown() {
        for odd in UNMEANT_COSTS {
            let model_usage: BTreeMap<String, wire::ModelUsage> = serde_json::from_str(&format!(
                r#"{{"opus-5":{{"inputTokens":10,"costUSD":{odd}}}}}"#
            ))
            .expect("a cost the protocol cannot mean still reads");
            let spent = Spent::from_model_usage(&model_usage);
            assert!(!spent.is_known(), "{odd}: {spent:?}");
        }
    }

    #[test]
    fn a_line_holding_half_of_a_surrogate_pair_is_read_with_that_half_as_a_replacement() {
        // The CLI is JavaScript: a string it cuts by UTF-16 units can end on
        // the first half of an emoji, and `JSON.stringify` writes that half
        // as an escape of its own, which serde_json refuses.
        let mut translator = Translator::new("max");
        translator.line(
            r#"{"type":"assistant","message":{"model":"opus-5","id":"m1","type":"message","role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]}}"#,
        );

        let ended = translator.line(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok \ud83d"}]}}"#,
        );
        let finished = translator.line(
            r#"{"type":"result","subtype":"success","is_error":false,"result":"\ude00 cut"}"#,
        );

        assert!(
            ended.iter().any(|event| matches!(
                event,
                Event::ToolCallEnd { id, output, .. } if id.as_str() == "t1" && output == "ok \u{fffd}"
            )),
            "{ended:?}"
        );
        assert!(
            finished
                .iter()
                .any(|event| matches!(event, Event::TurnEnded)),
            "{finished:?}"
        );
    }
}
