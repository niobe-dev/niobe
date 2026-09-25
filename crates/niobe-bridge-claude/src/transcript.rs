// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The session transcripts the `claude` CLI keeps for itself, read.
//!
//! The CLI writes every session it runs to a JSON Lines file of its own, one
//! directory per working directory it has run in. Niobe reads those files and
//! writes nothing back: the CLI's store is the CLI's, and the only thing Niobe
//! does with what it finds there is show it and hand the session id to
//! `--resume`, which is the CLI's own way of continuing a conversation.
//!
//! Nothing here goes near a credential. The transcripts hold messages, tool
//! calls and token counts; the CLI's tokens live elsewhere in its
//! configuration directory and are never opened.
//!
//! # The transcript is not the stream
//!
//! The file carries the same messages as the live protocol in envelopes of its
//! own, so the folding is [`Translator`]'s and only the envelopes are read
//! here. Four differences are the reason this module exists:
//!
//! * **The operator's turns are in it.** A live session's prompts are ones
//!   Niobe sent, so the translator drops the CLI's echo of them; a transcript
//!   is the only record of what was said, so its user turns become
//!   [`Event::UserMessage`].
//! * **Usage is on the message, and repeated.** One API response is written
//!   out as several records — one per run of content blocks — with the same
//!   `usage` stamped on each. Unlike the live stream's per-record snapshots
//!   the repeated figures are identical and final, so counting one per
//!   `message.id` is both necessary and enough.
//! * **There is no `result` line, and the bill is under other names.** What the
//!   session cost is in a `cost-state` record the CLI writes when it leaves,
//!   whose `modelUsage` is the same running total per model that closes a live
//!   turn — but keyed by the id the session was *billed* under, which is not
//!   the id its messages named. A session that has one is therefore counted
//!   from it alone, and one the CLI has not closed yet is counted from its
//!   messages; see [`Fold::assistant`] for why the two cannot be added.
//! * **Most records are the CLI's own furniture.** Attachments, file
//!   snapshots and queue operations say nothing about what the session did,
//!   what it changed or what it cost. They are named here so that they are
//!   passed over in silence rather than reported as messages Niobe cannot
//!   read. The one piece of furniture that is read is the CLI's title for the
//!   session, which becomes [`Event::Titled`]; a transcript the CLI never
//!   titled is titled by the first prompt the operator typed.
//! * **A sub-agent's messages are not in it.** The CLI writes each agent's
//!   conversation to a transcript of its own, in a directory named after the
//!   session, and the session's file says only that the agent was launched
//!   and, later, that it stopped. Each agent's own messages — its calls, its
//!   edits, the model it ran on — are read from its file and folded in where
//!   the CLI's timestamps put them among the session's, which is where the
//!   live stream would have shown them; see [`side_files`] and [`Threads`].
//!
//! [`Event::Titled`]: niobe_core::event::Event::Titled
//!
//! [`Event::UserMessage`]: niobe_core::event::Event::UserMessage

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;

use niobe_core::event::Event;

use crate::conformance;
use crate::spilled;
use crate::translate::{self, RecordedAgent, Translator};
use crate::wire;

/// The directory under the CLI's own configuration directory that holds one
/// directory per working directory it has run in.
const PROJECTS: &str = "projects";

/// The extension the CLI gives a transcript.
const EXTENSION: &str = "jsonl";

/// What the CLI wraps a turn it wrote itself in, rather than one the operator
/// typed: a slash command as the CLI expanded it, and the output of one.
///
/// Read off transcripts recorded by Claude Code 2.1.277 on 18 September 2026.
/// Only the session list uses this — a turn the CLI wrote is still a turn the
/// model was given, so the imported history carries it as it stands, and only
/// the one line that says what a session was about skips over it.
const CLI_WROTE_IT: [&str; 2] = ["<command-name>", "<local-command-stdout>"];

/// The `origin.kind` of the turn the CLI writes when background work stops.
///
/// Read off the transcripts on the machine this was written on, 19 September
/// 2026: 1,111 such turns from twenty-two releases between 2.1.231 and
/// 2.1.277, each one a `<task-notification>`. 932 of them name the call that
/// started the work in `<tool-use-id>` and how it ended in `<status>`; the
/// rest name no call, so there is no agent to match them to and they are
/// passed over.
const TASK_NOTIFICATION: &str = "task-notification";

/// What the CLI puts where a message's model would be when it wrote the
/// message itself rather than asking a model for it — the line it shows when a
/// request failed, and the like. Read off the transcripts above, where it
/// stands against exactly the messages no model produced.
const NO_MODEL: &str = "<synthetic>";

/// The directory beside a session's transcript, named after the session, that
/// holds its sub-agents' own transcripts.
const SUB_AGENTS: &str = "subagents";

/// What the CLI ends the name of a sub-agent's description file with. The
/// transcript beside it has the same name with [`EXTENSION`] in its place.
const META: &str = ".meta.json";

/// The `commandMode` of the attachment the CLI writes when background work
/// stops while a turn is running, and the notification is folded into that
/// turn rather than starting one of its own.
///
/// Read off the transcripts on the machine this was written on, 25 September
/// 2026: 1,841 such attachments from 2.1.241 to 2.1.281, 1,609 of them naming a
/// call in `<tool-use-id>`. Without it, an agent that finished while its
/// session was working would read back as running for ever.
const QUEUED_NOTIFICATION: &str = "task-notification";

/// The variable the CLI takes its configuration directory from.
pub const CONFIG_DIR_VAR: &str = "CLAUDE_CONFIG_DIR";

/// The directory under the home directory the CLI keeps its state in when
/// [`CONFIG_DIR_VAR`] does not say.
const DEFAULT_DIR: &str = ".claude";

/// The CLI's own configuration directory: what [`CONFIG_DIR_VAR`] names, and
/// `~/.claude` where nothing does.
///
/// Only an absolute `configured` counts. A relative one names a different
/// directory from every working directory, and Niobe's is not always the one
/// the CLI is started in; resolving it here would be a guess at which.
///
/// `None` is a machine that says neither, where there is nothing to look in.
pub fn config_dir(configured: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let absolute = |dir: &OsStr| Some(PathBuf::from(dir)).filter(|dir| dir.is_absolute());
    configured
        .and_then(absolute)
        .or_else(|| absolute(home?).map(|home| home.join(DEFAULT_DIR)))
}

/// The directory the CLI writes `cwd`'s transcripts into, under `config` —
/// its own configuration directory.
///
/// The CLI names the directory after the working directory with every `/` and
/// every `.` replaced by `-`. Read off a machine running Claude Code 2.1.277
/// on 18 September 2026: all thirty directories present matched their own
/// transcripts' recorded `cwd` under that rule, including paths with dots in
/// them and paths that already contained a `-`.
pub fn directory(config: &Path, cwd: &Path) -> PathBuf {
    let flattened: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| match c {
            '/' | '.' => '-',
            other => other,
        })
        .collect();
    config.join(PROJECTS).join(flattened)
}

/// One session the CLI has recorded for a working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    /// The id the CLI calls the session by, which is the file's name without
    /// its extension and the id its `--resume` takes.
    pub id: String,
    /// The file it was read from.
    pub path: PathBuf,
    /// When the CLI last wrote to it. `None` where the filesystem did not say.
    pub last_at: Option<SystemTime>,
    /// The first turn the CLI did not write itself, where the transcript has
    /// one.
    pub first_prompt: Option<String>,
}

/// Why a transcript could not be read.
#[derive(Debug)]
pub enum TranscriptError {
    /// The directory of transcripts could not be listed.
    Directory {
        /// The directory that was being listed.
        path: PathBuf,
        /// What the operating system said.
        error: std::io::Error,
    },
    /// One transcript could not be read.
    File {
        /// The file that was being read.
        path: PathBuf,
        /// What the operating system said.
        error: std::io::Error,
    },
}

impl std::fmt::Display for TranscriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Directory { path, error } => write!(
                f,
                "cannot read the claude sessions in {}: {error}",
                path.display()
            ),
            Self::File { path, error } => {
                write!(f, "cannot read {}: {error}", path.display())
            }
        }
    }
}

impl std::error::Error for TranscriptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Directory { error, .. } | Self::File { error, .. } => Some(error),
        }
    }
}

/// The sessions the CLI has recorded in `dir`, newest first.
///
/// A directory that is not there is a working directory the CLI has never run
/// in, which is a list of no sessions and not a failure.
///
/// A transcript whose first turn cannot be read is still listed, with no
/// prompt against it: the id is what continues the session, and a file that
/// cannot be read reports itself when it is.
pub fn list(dir: &Path) -> Result<Vec<Transcript>, TranscriptError> {
    let failed = |error: std::io::Error| TranscriptError::Directory {
        path: dir.to_path_buf(),
        error,
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(failed(error)),
    };

    let mut transcripts = Vec::new();
    for entry in entries {
        let entry = entry.map_err(failed)?;
        let path = entry.path();
        if path.extension().and_then(OsStr::to_str) != Some(EXTENSION) {
            continue;
        }
        let Some(id) = path.file_stem().and_then(OsStr::to_str) else {
            continue;
        };
        transcripts.push(Transcript {
            id: id.to_owned(),
            last_at: entry.metadata().ok().and_then(|at| at.modified().ok()),
            first_prompt: first_prompt(&path),
            path,
        });
    }

    // Newest first, and by id where two carry the same time or none — a
    // listing whose order changed between runs would be one nobody could read
    // twice. A transcript the filesystem gave no time for sorts last, because
    // `None` is not a claim that it is old.
    transcripts.sort_by(|a, b| b.last_at.cmp(&a.last_at).then_with(|| a.id.cmp(&b.id)));
    Ok(transcripts)
}

/// Everything the transcript at `path` says happened, as events, for a session
/// running under `profile` in `cwd`.
///
/// A line that cannot be read becomes a warning entry and the rest of the file
/// is still folded, exactly as on the live stream: a transcript written by a
/// newer CLI than this build knows must not take the history with it.
pub fn events(path: &Path, profile: &str, cwd: &Path) -> Result<Vec<Event>, TranscriptError> {
    let text = std::fs::read_to_string(path).map_err(|error| TranscriptError::File {
        path: path.to_path_buf(),
        error,
    })?;
    let id = path
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_owned();

    // Read once, then folded: whether the session was priced decides where its
    // tokens are counted from, and the record that says so is at the end.
    let read = records(&text);
    let priced = read
        .iter()
        .any(|record| matches!(record.record, Ok(Line::CostState(_))));
    let sides = side_files(path);
    let mut threads = Threads::read(&sides);
    let last_usage = last_usage(read.iter().chain(threads.records()));
    let agents = sides
        .iter()
        .map(|side| (side.call.clone(), recorded_agent(&side.text)))
        .collect();

    let mut fold = Fold::new(profile, cwd, id, priced, agents).counting(last_usage);
    // A session the CLI never titled is captioned by what the operator first
    // asked. Said here rather than left to the first message, because the
    // first message in a transcript may be one the CLI wrote itself.
    let titled = read
        .iter()
        .any(|record| matches!(record.record, Ok(Line::AiTitle(_))));
    if !titled
        && let Some(title) = read
            .iter()
            .find_map(|record| record.record.as_ref().ok().and_then(prompt))
    {
        fold.out.push(Event::Titled { title });
    }
    // Before anything is folded: what was recorded by a release nobody has
    // read is read at the operator's own risk, and a session imported from one
    // is read without anyone watching it happen.
    if let Some(version) = release_of(&text)
        && !conformance::recorded(&version)
    {
        fold.out
            .push(translate::warn(conformance::unrecorded(&version)));
    }
    for record in read {
        // The session's own file is kept in the order the CLI wrote it, which
        // is not always the order of its stamps: an agent's records go in
        // before the first of the session's that was stamped after them.
        if let Some(at) = &record.at {
            threads.fold_before(Some(at), &mut fold);
        }
        threads.spawned_in(&record.record);
        fold.record(record.line, record.record, None);
    }
    threads.fold_before(None, &mut fold);
    Ok(fold.out)
}

/// One record of a transcript, as read, with the time the CLI stamped on it.
struct Record<'a> {
    line: &'a str,
    /// When the CLI wrote it: `2026-09-19T11:14:47.000Z`, always to the
    /// millisecond and always in UTC, so that two stamps compare as text.
    /// Measured on the machine this was written on, 26 September 2026: all
    /// 143,141 stamps in the sessions that ran sub-agents, and in their
    /// agents' files, were written that way.
    at: Option<String>,
    record: Result<Line, serde_json::Error>,
}

/// Every record of the transcript `text`, in the order it holds them.
fn records(text: &str) -> Vec<Record<'_>> {
    #[derive(Deserialize)]
    struct Stamp {
        timestamp: Option<String>,
    }

    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| Record {
            line,
            at: serde_json::from_str::<Stamp>(line)
                .ok()
                .and_then(|stamp| stamp.timestamp),
            record: serde_json::from_str::<Line>(line),
        })
        .collect()
}

/// The usage of the last record the CLI wrote of each API response, by its
/// `message.id`.
///
/// The CLI writes one response out as several records, and on each the
/// output count as it stood when that record was written: only the last is
/// the response's own. Measured on the machine this was written on, 26
/// September 2026: of 58,546 responses written over more than one record,
/// the counts moved within 8,200, always in `output_tokens` alone, and the
/// last record held the largest count every time. Almost all of those were in
/// sub-agents' files; four were in sessions' own.
fn last_usage<'a>(records: impl Iterator<Item = &'a Record<'a>>) -> BTreeMap<String, wire::Usage> {
    let mut last = BTreeMap::new();
    for record in records {
        if let Ok(Line::Assistant(Assistant { message })) = &record.record
            && let (Some(id), Some(usage)) = (&message.id, &message.usage)
        {
            last.insert(id.clone(), usage.clone());
        }
    }
    last
}

/// Every sub-agent's own records, each waiting for its place among the
/// session's.
///
/// An agent's records are folded in the order its file holds them, and each
/// goes in once the session has reached the time the CLI stamped on it. Two
/// agents' records stamped the same instant go in the order the agents were
/// spawned. No record goes in before the call that spawned its agent has been
/// folded, whatever its stamp says: the call is what the agent's work is
/// shown under. An agent whose spawn the session never records is left out.
struct Threads<'a> {
    /// Each agent's records not yet folded, by the id of the call that spawned
    /// it.
    waiting: BTreeMap<String, VecDeque<Record<'a>>>,
    /// The agents whose spawn has been folded, in the order it was.
    spawned: Vec<String>,
}

impl<'a> Threads<'a> {
    fn read(sides: &'a [SideFile]) -> Self {
        Self {
            waiting: sides
                .iter()
                .map(|side| (side.call.clone(), records(&side.text).into()))
                .collect(),
            spawned: Vec::new(),
        }
    }

    /// Every record still waiting, in no particular order.
    fn records(&self) -> impl Iterator<Item = &Record<'a>> {
        self.waiting.values().flatten()
    }

    /// Notes each agent whose spawning call `record` makes.
    fn spawned_in(&mut self, record: &Result<Line, serde_json::Error>) {
        let Ok(Line::Assistant(Assistant { message })) = record else {
            return;
        };
        let Some(wire::Content::Blocks(blocks)) = &message.content else {
            return;
        };
        for block in blocks {
            if let wire::Block::ToolUse { id, .. } = block
                && self.waiting.contains_key(id)
                && !self.spawned.contains(id)
            {
                self.spawned.push(id.clone());
            }
        }
    }

    /// Folds every agent record stamped before `bound`, or every one left
    /// where there is no bound, earliest first.
    fn fold_before(&mut self, bound: Option<&str>, fold: &mut Fold) {
        while let Some((call, record)) = self.next_before(bound) {
            self.spawned_in(&record.record);
            fold.record(record.line, record.record, Some(&call));
        }
    }

    /// The earliest waiting record of a spawned agent stamped before `bound`,
    /// and the call that spawned its agent. A record with no stamp goes in as
    /// soon as the one before it has.
    fn next_before(&mut self, bound: Option<&str>) -> Option<(String, Record<'a>)> {
        let call = self
            .spawned
            .iter()
            .filter_map(|call| {
                let at = self.waiting.get(call)?.front()?.at.as_deref();
                bound
                    .is_none_or(|bound| at.is_none_or(|at| at < bound))
                    .then_some((at, call))
            })
            // `min_by_key` keeps the first of equals, which is the agent
            // spawned first.
            .min_by_key(|(at, _)| *at)
            .map(|(_, call)| call.clone())?;
        let record = self.waiting.get_mut(&call)?.pop_front()?;
        Some((call, record))
    }
}

/// A sub-agent's own transcript, and the call that spawned the agent.
struct SideFile {
    call: String,
    text: String,
}

/// Every sub-agent's own transcript, for the session whose transcript is at
/// `path`, each with the id of the call that spawned it.
///
/// The CLI keeps each agent in `<session>/subagents/agent-<id>.jsonl`, beside
/// a `.meta.json` that names the call that spawned it in `toolUseId`. Read off
/// the machine this was written on, 25 September 2026: 455 agents' files from
/// 2.1.246 to 2.1.280, every one of them with a description naming its call.
///
/// An agent whose files are missing or cannot be read is left out, and shows
/// what the session's own file says about it and no more: the session's
/// history does not depend on them.
fn side_files(path: &Path) -> Vec<SideFile> {
    #[derive(Deserialize)]
    struct Meta {
        #[serde(rename = "toolUseId")]
        tool_use_id: Option<String>,
    }

    let dir = path.with_extension("").join(SUB_AGENTS);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut sides = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(META)) else {
            continue;
        };
        let Some(call) = std::fs::read_to_string(entry.path())
            .ok()
            .and_then(|text| serde_json::from_str::<Meta>(&text).ok())
            .and_then(|meta| meta.tool_use_id)
        else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(dir.join(format!("{stem}.{EXTENSION}"))) else {
            continue;
        };
        sides.push(SideFile { call, text });
    }
    // The directory's own order is the file system's; the call's id is at
    // least the same on every machine.
    sides.sort_by(|a, b| a.call.cmp(&b.call));
    sides
}

/// What one sub-agent's transcript says about it beyond its messages: the
/// text of its last message, where no call followed it.
///
/// An agent's last message is its answer only once it has stopped; while it
/// works, its text is what it says before its next call, and the call is
/// what follows. So a call clears the answer, and a transcript cut off
/// mid-step has none.
fn recorded_agent(text: &str) -> RecordedAgent {
    let mut agent = RecordedAgent::default();
    for line in text.lines() {
        let Ok(Line::Assistant(Assistant { message })) = serde_json::from_str::<Line>(line) else {
            continue;
        };
        let Some(wire::Content::Blocks(blocks)) = message.content else {
            continue;
        };
        if blocks
            .iter()
            .any(|block| matches!(block, wire::Block::ToolUse { .. }))
        {
            agent.answer = None;
        } else if let Some(said) = said(&wire::Content::Blocks(blocks)) {
            agent.answer = Some(said);
        }
    }
    agent
}

/// The CLI release a transcript was written by, off the first record that says.
///
/// The CLI stamps `version` on every record it writes during a turn, and on
/// none of the furniture it writes between them; a transcript that names no
/// release at all is one this bridge can say nothing about.
fn release_of(text: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Stamp {
        version: Option<String>,
    }

    text.lines()
        .filter_map(|line| serde_json::from_str::<Stamp>(line).ok())
        .find_map(|stamp| stamp.version)
}

/// The first turn of the transcript at `path` that the CLI did not write
/// itself.
fn first_prompt(path: &Path) -> Option<String> {
    let file = BufReader::new(File::open(path).ok()?);
    for line in file.lines() {
        let Ok(record) = serde_json::from_str::<Line>(&line.ok()?) else {
            continue;
        };
        if let Some(said) = prompt(&record) {
            return Some(said);
        }
    }
    None
}

/// What the operator said in `record`, where it is a turn they typed rather
/// than one the CLI wrote itself.
fn prompt(record: &Line) -> Option<String> {
    let Line::User(user) = record else {
        return None;
    };
    if user.is_meta {
        return None;
    }
    let said = user.message.content.as_ref().and_then(said)?;
    (!CLI_WROTE_IT.iter().any(|mark| said.starts_with(mark))).then_some(said)
}

/// Folds one transcript into events.
struct Fold {
    translator: Translator,
    out: Vec<Event>,
    /// Whether the transcript carries the CLI's own accounting for the session,
    /// which is where a session that has one is counted from. See
    /// [`Fold::assistant`].
    priced: bool,
    /// The `message.id` of every API response whose tokens have been counted,
    /// so that the records that repeat one are not billed again.
    counted: BTreeMap<String, ()>,
    /// The usage the last record of each API response carries, which is the
    /// response's own; see [`last_usage`].
    last_usage: BTreeMap<String, wire::Usage>,
    /// The mode the session was last recorded in, as the CLI spells it. The
    /// CLI writes the mode out again whenever it writes anything, so only a
    /// change is worth an event.
    mode: Option<String>,
    /// The title the CLI last gave the session, for the same reason.
    title: Option<String>,
}

impl Fold {
    fn new(
        profile: &str,
        cwd: &Path,
        id: String,
        priced: bool,
        agents: BTreeMap<String, RecordedAgent>,
    ) -> Self {
        Self {
            translator: Translator::new(profile)
                .in_dir(cwd)
                .of_session(id)
                .reading_spilled_with(spilled::read)
                .knowing_agents(agents),
            out: Vec::new(),
            priced,
            counted: BTreeMap::new(),
            last_usage: BTreeMap::new(),
            mode: None,
            title: None,
        }
    }

    /// The same fold, counting each API response by the usage its last
    /// record carries rather than by the first it meets.
    fn counting(mut self, last_usage: BTreeMap<String, wire::Usage>) -> Self {
        self.last_usage = last_usage;
        self
    }

    /// Folds one record of the session's own file, where `agent` is `None`,
    /// or of the file of the sub-agent spawned by call `agent`.
    fn record(&mut self, line: &str, record: Result<Line, serde_json::Error>, agent: Option<&str>) {
        match record {
            Ok(Line::Assistant(record)) => self.assistant(record, agent),
            Ok(Line::User(record)) => self.user(record, agent),
            Ok(Line::CostState(record)) => self.cost(record),
            Ok(Line::PermissionMode(record)) => self.mode(record),
            Ok(Line::AiTitle(record)) => self.title(record),
            Ok(Line::Attachment(record)) => self.attachment(record),
            Ok(Line::Aside) => {}
            Ok(Line::Unknown) => self.out.push(translate::unread(format!(
                "the transcript holds a record of type `{}`, which this version of Niobe does \
                 not know how to read. It was not counted.",
                translate::kind_of(line)
            ))),
            Err(error) => self.out.push(translate::warn(format!(
                "the transcript holds a `{}` record this version of Niobe could not read, so it \
                 was not counted: {error}",
                translate::kind_of(line)
            ))),
        }
    }

    /// A message from the model: the session's own, or where `agent` names a
    /// call, the sub-agent's that call spawned, folded as the live stream
    /// folds one that names its `parent_tool_use_id` — its model reported as
    /// the agent's rather than the session's, its tokens under that model,
    /// and the session's context meter left where it was.
    fn assistant(&mut self, record: Assistant, agent: Option<&str>) {
        let Assistant { message } = record;
        let parent = agent.map(str::to_owned);

        // The model is named on the message itself, which is where the live
        // stream reads it from too — off the head of the message rather than
        // off the session, so that a session that moved model says so where it
        // moved. A message the CLI wrote itself, such as the one it shows when
        // a request failed, is marked as written by no model; it is still what
        // the operator was shown, so it is kept, but naming the session after
        // it would put a model that does not exist in front of the operator
        // would overwrite the one the session is really on.
        if let Some(model) = message.model.filter(|model| model != NO_MODEL) {
            self.fold(wire::Message::StreamEvent(wire::StreamEvent {
                event: wire::StreamBody::MessageStart {
                    message: wire::StartMessage { model: Some(model) },
                },
                parent_tool_use_id: parent.clone(),
            }));
        }

        // Once per API response, however many records the CLI split it over.
        // A record the CLI gave no id cannot be recognised as a repeat of one
        // already counted; it is counted, because a record with no id is one
        // the CLI wrote once.
        let first_time = match &message.id {
            Some(id) => self.counted.insert(id.clone(), ()).is_none(),
            None => true,
        };

        self.fold(wire::Message::Assistant(wire::Envelope {
            message: wire::ApiMessage {
                content: message.content,
                // Reported above, off the message's head, as the stream does.
                model: None,
            },
            parent_tool_use_id: parent.clone(),
            tool_use_result: None,
        }));

        // Counted here only where the session was never priced. A message
        // names the model it ran on as the family — `claude-opus-5` — and the
        // CLI bills the session against the id that carries the context window
        // with it — `claude-opus-5[1m]`, which is a different rate. Measured on
        // this machine on 18 September 2026 across twenty-five transcripts: not
        // one message named the id its session was billed under. Counting both
        // would report every token twice, and deciding that one id is the
        // other would be a guess about a price: the live `result` names the
        // family behind a billed id in `canonicalModel`, and `cost-state`
        // does not. So a priced session is counted
        // from the CLI's own accounting and this from the messages, and a
        // session the CLI has not closed yet — which has no accounting — is
        // counted from the messages, which is measured and reads as a floor.
        //
        // A sub-agent's messages are counted the same way. They are billed
        // like the session's own, and the CLI's accounting for a session
        // includes them: measured on the machine this was written on, 26
        // September 2026, across the 56 closed sessions that ran agents, the
        // sessions' own messages came to 33.6% of the cache writes their
        // `cost-state` recorded and 92.4% with their agents' messages added,
        // and the agents' took no session over its own record in 55 of them.
        // A session with that record is counted from it, so an agent's
        // messages add nothing to one; an unclosed session's floor without
        // them would leave out most of what it spent.
        let usage = message
            .id
            .as_ref()
            .and_then(|id| self.last_usage.get(id).cloned())
            .or(message.usage);
        if let Some(usage) = usage
            && first_time
            && !self.priced
        {
            self.fold(wire::Message::StreamEvent(wire::StreamEvent {
                event: wire::StreamBody::MessageDelta { usage: Some(usage) },
                parent_tool_use_id: parent,
            }));
        }
    }

    /// A turn of the session's, or where `agent` names a call, of the
    /// conversation the sub-agent that call spawned had with the session.
    fn user(&mut self, record: User, agent: Option<&str>) {
        // The CLI writes notes of its own into the transcript as user turns —
        // the caveat that precedes a local command's output, and the like —
        // and marks them. They are not what the operator said.
        if record.is_meta {
            return;
        }
        // Background work stopping is written as a user turn too, and it is
        // the CLI speaking. What it says is how a sub-agent launched in the
        // background ended, which the call's own result never does.
        let kind = record
            .origin
            .as_ref()
            .and_then(|origin| origin.get("kind"))
            .and_then(serde_json::Value::as_str);
        if kind == Some(TASK_NOTIFICATION) {
            if let Some(text) = record.message.content.as_ref().and_then(said) {
                self.notified(&text);
            }
            return;
        }
        // What an agent is told is the session speaking to it, and the
        // operator said none of it.
        if agent.is_none()
            && let Some(text) = record.message.content.as_ref().and_then(said)
        {
            self.out.push(Event::UserMessage { text });
        }
        // The same record carries the results of the calls the turn before it
        // made, which the translator reads and this does not.
        self.fold(wire::Message::User(wire::Envelope {
            message: wire::ApiMessage {
                content: record.message.content,
                model: None,
            },
            parent_tool_use_id: agent.map(str::to_owned),
            tool_use_result: record.tool_use_result,
        }));
    }

    /// What the CLI attached to a running turn. The one attachment read is a
    /// notification that background work stopped while the turn ran; the
    /// rest is context the CLI gave the model, and says nothing about what
    /// the session did.
    fn attachment(&mut self, record: Attachment) {
        let Some(attached) = record.attachment else {
            return;
        };
        let field = |key: &str| attached.get(key).and_then(serde_json::Value::as_str);
        if field("type") == Some("queued_command")
            && field("commandMode") == Some(QUEUED_NOTIFICATION)
            && let Some(text) = field("prompt")
        {
            let text = text.to_owned();
            self.notified(&text);
        }
    }

    /// The CLI's word, in its own markup, that background work stopped. What
    /// it says about a sub-agent is how it ended; its `<summary>` is the
    /// CLI's wording and not the agent's, and is not read.
    fn notified(&mut self, text: &str) {
        if let Some(id) = tag(text, "tool-use-id") {
            let mut ended = self.translator.task_stopped(id, tag(text, "status"));
            self.out.append(&mut ended);
        }
    }

    fn cost(&mut self, record: CostState) {
        // The CLI says when it could not price something it ran, and a total
        // that is missing a model is a floor rather than the bill. Said
        // plainly, because nothing else in the record marks which model it was.
        if record.has_unknown_model_cost {
            self.out.push(translate::warn(
                "the CLI could not price part of this session, so what it recorded for it is \
                 less than the session cost."
                    .to_owned(),
            ));
        }
        // `modelUsage` is the session's running total per model, which is what
        // closes a turn on the live stream as well, so it is folded by the
        // same path: what is reported is the difference from what the
        // per-message records already accounted for.
        self.fold(wire::Message::Result(wire::Outcome {
            subtype: Some("success".to_owned()),
            is_error: false,
            // The transcript states no per-turn total, so there is nothing for
            // the per-message records to be reconciled against.
            usage: None,
            model_usage: record.model_usage,
            total_cost_usd: record.total_cost_usd,
            // A refusal is in the transcript as the tool result it became; the
            // CLI keeps no list of them here.
            permission_denials: Vec::new(),
            result: None,
            errors: Vec::new(),
        }));
    }

    /// The CLI's title, whenever it differs from the last one: the CLI writes
    /// the same title out again and again, and only a change is news.
    fn title(&mut self, record: AiTitle) {
        let Some(title) = record.title.filter(|title| !title.trim().is_empty()) else {
            return;
        };
        if self.title.as_deref() == Some(title.as_str()) {
            return;
        }
        self.title = Some(title.clone());
        self.out.push(Event::Titled { title });
    }

    fn mode(&mut self, record: PermissionMode) {
        let Some(spelt) = record.permission_mode else {
            return;
        };
        if self.mode.as_deref() == Some(spelt.as_str()) {
            return;
        }
        self.out.push(match translate::read_mode(&spelt) {
            Some(mode) => Event::ModeSelected { mode },
            // The CLI gates calls in ways Niobe has no word for. Naming the
            // one this session ran under beats showing a mode it was not in.
            None => translate::warn(format!(
                "the session was gating tool calls as `{spelt}`, which this version of Niobe \
                 does not model. No mode is reported until the session is moved to one it \
                 does."
            )),
        });
        self.mode = Some(spelt);
    }

    fn fold(&mut self, message: wire::Message) {
        self.out.append(&mut self.translator.message(message));
    }
}

/// The text of the first `<name>` element in a turn the CLI wrote in its own
/// markup, such as a task notification.
fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let length = text[start..].find(&close)?;
    Some(text[start..start + length].trim())
}

/// What a turn holds as prose, where it holds any.
///
/// A turn is written as a bare string when it is nothing but text and as
/// blocks when it is anything else; the blocks of a turn are its text and the
/// results of the calls before it, and only the text is what was said.
fn said(content: &wire::Content) -> Option<String> {
    let text = match content {
        wire::Content::Text(text) => text.clone(),
        wire::Content::Blocks(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                wire::Block::Text { text } => Some(text.as_str()),
                wire::Block::ToolUse { .. } | wire::Block::ToolResult { .. } => None,
                wire::Block::Other => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    (!text.trim().is_empty()).then_some(text)
}

/// One line of a transcript.
///
/// Every record the CLI writes is named, including the ones that are read for
/// nothing: a record type that fell into [`Line::Unknown`] is reported, and a
/// transcript full of furniture would otherwise report a warning per line.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum Line {
    #[serde(rename = "assistant")]
    Assistant(Assistant),
    #[serde(rename = "user")]
    User(User),
    /// What the session cost, written when the CLI leaves.
    #[serde(rename = "cost-state")]
    CostState(CostState),
    /// How the session was gating tool calls.
    #[serde(rename = "permission-mode")]
    PermissionMode(PermissionMode),
    /// The CLI's own title for the session.
    #[serde(rename = "ai-title")]
    AiTitle(AiTitle),
    /// Something the CLI attached to a turn: mostly context it gave the
    /// model, and once in a while the notice that a sub-agent stopped.
    #[serde(rename = "attachment")]
    Attachment(Attachment),
    /// Records that say nothing about what the session did, changed or cost:
    /// the name the CLI gives a sub-agent's session (`agent-name`), the prompt it offers to repeat
    /// (`last-prompt`), the file snapshots a rewind would restore (`file-history-snapshot`,
    /// `file-history-delta`), its queue (`queue-operation`), its editing mode
    /// (`mode`, which is not the permission mode), its own notes to the screen
    /// (`system`), its latched status line (`atis-latch`), the pull request it
    /// opened from the session (`pr-link`) and the handle its own service
    /// holds the session under (`bridge-session`).
    ///
    /// Read off every record type present across the transcripts on the
    /// machine this was written on — sixteen in all — rather than off the
    /// types one session happened to produce. Six of them are read
    /// (`assistant`, `user`, `cost-state`, `permission-mode`, `ai-title`,
    /// `attachment`) and the other ten are here. A type left out is a warning entry per record in front
    /// of the operator, for a record that says nothing: `bridge-session` alone
    /// stood in fifteen thousand of them.
    #[serde(
        rename = "system",
        alias = "agent-name",
        alias = "atis-latch",
        alias = "bridge-session",
        alias = "file-history-delta",
        alias = "file-history-snapshot",
        alias = "last-prompt",
        alias = "mode",
        alias = "pr-link",
        alias = "queue-operation"
    )]
    Aside,
    #[serde(other)]
    Unknown,
}

/// A message from the model, as the transcript wraps it.
#[derive(Debug, Deserialize)]
struct Assistant {
    message: AssistantBody,
}

/// The message itself: the same API message the live stream carries, with the
/// model and the token counts on it rather than on lines of their own.
#[derive(Debug, Deserialize)]
struct AssistantBody {
    /// The API's id for the response, which several records may repeat.
    id: Option<String>,
    model: Option<String>,
    usage: Option<wire::Usage>,
    content: Option<wire::Content>,
}

/// A turn from the operator, or the results of the calls the last turn made.
#[derive(Debug, Deserialize)]
struct User {
    message: UserBody,
    /// Set on a turn the CLI wrote for its own purposes rather than one the
    /// operator sent.
    #[serde(rename = "isMeta", default)]
    is_meta: bool,
    /// Where the turn came from. The CLI writes it both as a bare word
    /// (`"cli"`) and as an object with a `kind`, so it is read as a value: a
    /// shape this cannot type would otherwise lose the turn it is on.
    #[serde(default)]
    origin: Option<serde_json::Value>,
    /// What the tool whose result this record carries reported about itself;
    /// the live stream's `tool_use_result`, under the transcript's spelling.
    #[serde(rename = "toolUseResult", default)]
    tool_use_result: Option<serde_json::Value>,
}

/// The turn itself.
#[derive(Debug, Deserialize)]
struct UserBody {
    content: Option<wire::Content>,
}

/// Something the CLI attached to a turn.
#[derive(Debug, Deserialize)]
struct Attachment {
    /// What was attached, which takes a different shape for every kind of
    /// attachment; read as a value, so that a kind this cannot type is passed
    /// over rather than reported as a record that could not be read.
    #[serde(default)]
    attachment: Option<serde_json::Value>,
}

/// What the session has cost, per model and in total.
#[derive(Debug, Deserialize)]
struct CostState {
    #[serde(rename = "modelUsage", default)]
    model_usage: BTreeMap<String, wire::ModelUsage>,
    #[serde(rename = "totalCostUSD")]
    total_cost_usd: Option<f64>,
    /// Set where the CLI ran a model it has no price for, which makes the
    /// total it recorded a floor.
    #[serde(rename = "hasUnknownModelCost", default)]
    has_unknown_model_cost: bool,
}

/// The CLI's title for the session, in its own words.
///
/// The interactive CLI writes one once the session has a subject, and again
/// whenever it re-titles it — the same record over and over, mostly. Read off
/// the transcripts on the machine this was written on, 25 September 2026: 261
/// of 534 carried one, all but nine of them interactive sessions; no headless
/// session from 2.1.277 on carried one, so a session driven over the stream
/// is captioned from its first prompt instead. One title in the whole set
/// changed after it was first written.
#[derive(Debug, Deserialize)]
struct AiTitle {
    #[serde(rename = "aiTitle")]
    title: Option<String>,
}

/// How the session was gating tool calls, in the CLI's own spelling.
#[derive(Debug, Deserialize)]
struct PermissionMode {
    #[serde(rename = "permissionMode")]
    permission_mode: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use niobe_core::event::{AgentId, AgentOutcome, Mode};

    #[test]
    fn the_cli_keeps_its_transcripts_where_its_own_variable_says() {
        let var = |dir: &str| Some(OsStr::new(dir).to_owned());

        assert_eq!(
            config_dir(var("/elsewhere").as_deref(), var("/home/me").as_deref()),
            Some(PathBuf::from("/elsewhere"))
        );
        assert_eq!(
            config_dir(None, var("/home/me").as_deref()),
            Some(PathBuf::from("/home/me/.claude"))
        );
        // A relative one names a different directory from every working
        // directory, so it says nothing on its own.
        assert_eq!(
            config_dir(var("state").as_deref(), var("/home/me").as_deref()),
            Some(PathBuf::from("/home/me/.claude"))
        );
        assert_eq!(config_dir(None, None), None);
    }

    #[test]
    fn a_working_directory_names_the_directory_the_cli_writes_it_to() {
        assert_eq!(
            directory(Path::new("/home/me/.claude"), Path::new("/w/my.app")),
            Path::new("/home/me/.claude/projects/-w-my-app")
        );
        // A path that already holds a `-` keeps it, so two directories cannot
        // collide by one of them being flattened onto the other.
        assert_eq!(
            directory(Path::new("/c"), Path::new("/w/a-b")),
            Path::new("/c/projects/-w-a-b")
        );
    }

    /// Writes `text` as the transcript of session `id` in `dir`, last written
    /// to `age` before now.
    fn transcript(dir: &Path, id: &str, age: Duration, text: &str) -> PathBuf {
        let path = dir.join(format!("{id}.{EXTENSION}"));
        std::fs::write(&path, text).expect("the transcript is written");
        let at = SystemTime::now() - age;
        File::options()
            .write(true)
            .open(&path)
            .expect("the transcript opens")
            .set_times(std::fs::FileTimes::new().set_modified(at))
            .expect("the modification time is set");
        path
    }

    fn prompt(text: &str) -> String {
        format!(r#"{{"type":"user","message":{{"role":"user","content":{text:?}}}}}"#)
    }

    #[test]
    fn sessions_are_listed_newest_first_with_what_they_were_asked() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        transcript(
            dir.path(),
            "older",
            Duration::from_secs(600),
            &prompt("the older one"),
        );
        transcript(
            dir.path(),
            "newer",
            Duration::from_secs(60),
            &prompt("the newer one"),
        );
        std::fs::write(dir.path().join("notes.txt"), "not a transcript")
            .expect("the file is written");

        let listed = list(dir.path()).expect("the directory lists");

        let named: Vec<(&str, Option<&str>)> = listed
            .iter()
            .map(|t| (t.id.as_str(), t.first_prompt.as_deref()))
            .collect();
        assert_eq!(
            named,
            [
                ("newer", Some("the newer one")),
                ("older", Some("the older one"))
            ]
        );
    }

    #[test]
    fn a_directory_the_cli_has_never_written_to_holds_no_sessions() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");

        assert_eq!(
            list(&dir.path().join("never-run-here")).expect("absence is not a failure"),
            []
        );
    }

    #[test]
    fn the_prompt_a_session_is_listed_by_is_the_first_the_cli_did_not_write() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let lines = [
            r#"{"type":"user","message":{"role":"user","content":"a caveat"},"isMeta":true}"#
                .to_owned(),
            prompt("<command-name>/clear</command-name>\n<command-args></command-args>"),
            prompt("<local-command-stdout>Set model to Opus</local-command-stdout>"),
            prompt("what changed in the last commit?"),
        ];
        let path = transcript(dir.path(), "s", Duration::ZERO, &lines.join("\n"));

        assert_eq!(
            first_prompt(&path).as_deref(),
            Some("what changed in the last commit?")
        );
    }

    #[test]
    fn a_session_that_said_nothing_is_listed_without_a_prompt() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let path = transcript(
            dir.path(),
            "s",
            Duration::ZERO,
            r#"{"type":"mode","mode":"normal"}"#,
        );

        assert_eq!(first_prompt(&path), None);
    }

    /// Folds `lines` as the transcript of a session in `/repo`.
    fn folded(lines: &[&str]) -> Vec<Event> {
        folded_knowing(lines, BTreeMap::new())
    }

    /// Folds `lines` as the transcript of a session in `/repo` whose
    /// sub-agents' own transcripts say `agents`.
    fn folded_knowing(lines: &[&str], agents: BTreeMap<String, RecordedAgent>) -> Vec<Event> {
        let priced = lines
            .iter()
            .any(|line| line.contains(r#""type":"cost-state""#));
        let mut fold = Fold::new("max", Path::new("/repo"), "s-1".to_owned(), priced, agents);
        for line in lines {
            fold.record(line, serde_json::from_str::<Line>(line), None);
        }
        fold.out
    }

    /// A call to the sub-agent tool, as the transcript writes it.
    const AGENT_CALL: &str = r#"{"type":"assistant","message":{"id":"msg_1","model":"claude-opus-5","content":[{"type":"tool_use","id":"toolu_a","name":"Agent","input":{"subagent_type":"quick-lookup","description":"Summarize catalog/cache.py","prompt":"Summarize it."}}]}}"#;

    /// What a sub-agent's own transcript said: it answered.
    fn answered() -> BTreeMap<String, RecordedAgent> {
        BTreeMap::from([(
            "toolu_a".to_owned(),
            RecordedAgent {
                answer: Some("## The cache is an LRU map.\nIt evicts the oldest entry.".to_owned()),
            },
        )])
    }

    /// The model, context size and latest line of each report on `toolu_a`,
    /// and whether it ended, in order.
    fn agent_story(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::AgentSpawn { .. } => Some("spawn".to_owned()),
                Event::AgentProgress {
                    model,
                    context_tokens,
                    latest,
                    ..
                } => Some(format!("{model:?} {context_tokens:?} {latest:?}")),
                Event::AgentExit { outcome, .. } => Some(format!("exit {outcome:?}")),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_sub_agent_that_ran_in_the_foreground_reports_its_recorded_answer_before_its_end() {
        let events = folded_knowing(
            &[
                AGENT_CALL,
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_a","content":"The cache is an LRU map."}]}}"#,
            ],
            answered(),
        );

        assert_eq!(
            agent_story(&events),
            [
                "spawn",
                r#"None None Some("The cache is an LRU map.")"#,
                "exit Completed",
            ]
        );
    }

    #[test]
    fn a_sub_agent_that_failed_shows_no_answer() {
        let events = folded_knowing(
            &[
                AGENT_CALL,
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_a","content":"Agent crashed","is_error":true}]}}"#,
            ],
            answered(),
        );

        assert_eq!(agent_story(&events), ["spawn", "exit Failed"]);
    }

    #[test]
    fn a_sub_agent_with_no_transcript_of_its_own_reports_nothing_more() {
        let events = folded(&[
            AGENT_CALL,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_a","content":"done"}]}}"#,
        ]);

        assert_eq!(agent_story(&events), ["spawn", "exit Completed"]);
    }

    /// A notification the CLI folded into a running turn as an attachment,
    /// and the context it attaches to every turn, which is passed over.
    #[test]
    fn a_notification_attached_to_a_running_turn_ends_its_agent() {
        let events = folded(&[
            AGENT_CALL,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_a","content":"Async agent launched successfully."}]},"toolUseResult":{"isAsync":true,"status":"async_launched","agentId":"a1"}}"#,
            r#"{"type":"attachment","attachment":{"type":"date","date":"2026-09-19"}}"#,
            r#"{"type":"attachment","attachment":{"type":"queued_command","commandMode":"prompt","prompt":"<tool-use-id>toolu_a</tool-use-id><status>completed</status>"}}"#,
        ]);
        assert_eq!(
            agent_story(&events),
            ["spawn"],
            "only a notification ends it"
        );

        let events = folded(&[
            AGENT_CALL,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_a","content":"Async agent launched successfully."}]},"toolUseResult":{"isAsync":true,"status":"async_launched","agentId":"a1"}}"#,
            r#"{"type":"attachment","attachment":{"type":"queued_command","commandMode":"task-notification","prompt":"<task-notification>\n<tool-use-id>toolu_a</tool-use-id>\n<status>completed</status>\n<summary>Agent \"Summarize catalog/cache.py\" finished</summary>\n</task-notification>"}}"#,
        ]);
        assert_eq!(agent_story(&events), ["spawn", "exit Completed"]);
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, Event::Error { .. } | Event::Notice { .. })),
            "{events:?}"
        );
    }

    #[test]
    fn an_agents_answer_is_its_last_message_where_no_call_followed_it() {
        let line = |content: &str, model: &str| {
            format!(
                r#"{{"type":"assistant","isSidechain":true,"message":{{"model":"{model}","content":{content}}}}}"#
            )
        };
        let thinking = r#"[{"type":"thinking","thinking":"…","signature":"s"}]"#;
        let call = r#"[{"type":"tool_use","id":"t","name":"Read","input":{}}]"#;
        let text = |said: &str| format!(r#"[{{"type":"text","text":"{said}"}}]"#);

        let finished = [
            line(&text("Let me read it."), "claude-opus-5"),
            line(call, "claude-opus-5"),
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":"x"}]}}"#.to_owned(),
            line(thinking, "claude-opus-5"),
            line(&text("It is an LRU map."), "claude-opus-5"),
            line(&text("An error the CLI wrote."), NO_MODEL),
        ]
        .join("\n");
        assert_eq!(
            recorded_agent(&finished),
            RecordedAgent {
                answer: Some("An error the CLI wrote.".to_owned()),
            },
            "a message no model wrote is still the last thing said"
        );

        let cut_off = [
            line(&text("Let me read it."), "claude-opus-5"),
            line(call, "claude-opus-5"),
        ]
        .join("\n");
        assert_eq!(recorded_agent(&cut_off).answer, None);
    }

    #[test]
    fn each_agents_transcript_is_found_by_the_call_its_description_names() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let session = dir.path().join("s.jsonl");
        let agents = dir.path().join("s").join(SUB_AGENTS);
        std::fs::create_dir_all(&agents).expect("the sub-agents' directory is made");
        let answer = r#"{"type":"assistant","message":{"model":"claude-haiku-4-5","content":[{"type":"text","text":"Done."}]}}"#;
        for (name, meta) in [
            (
                "agent-a1",
                r#"{"agentType":"quick-lookup","toolUseId":"toolu_a"}"#,
            ),
            // A description that names no call has no agent to go with.
            ("agent-a2", r#"{"agentType":"quick-lookup"}"#),
        ] {
            std::fs::write(agents.join(format!("{name}{META}")), meta).expect("written");
            std::fs::write(agents.join(format!("{name}.{EXTENSION}")), answer).expect("written");
        }
        // A description whose transcript is gone.
        std::fs::write(
            agents.join(format!("agent-a3{META}")),
            r#"{"toolUseId":"toolu_c"}"#,
        )
        .expect("written");

        let found: Vec<(String, String)> = side_files(&session)
            .into_iter()
            .map(|side| (side.call, side.text))
            .collect();
        assert_eq!(found, [("toolu_a".to_owned(), answer.to_owned())]);
        assert!(
            side_files(&dir.path().join("none.jsonl")).is_empty(),
            "a session with no sub-agents"
        );
    }

    #[test]
    fn the_mode_is_reported_when_it_changes_and_not_on_every_record() {
        let events = folded(&[
            r#"{"type":"permission-mode","permissionMode":"plan"}"#,
            r#"{"type":"permission-mode","permissionMode":"plan"}"#,
            r#"{"type":"permission-mode","permissionMode":"default"}"#,
        ]);

        assert_eq!(
            events,
            [
                Event::ModeSelected { mode: Mode::Plan },
                Event::ModeSelected { mode: Mode::Ask },
            ]
        );
    }

    #[test]
    fn a_mode_niobe_has_no_word_for_is_named_rather_than_shown_as_one_it_has() {
        let events =
            folded(&[r#"{"type":"permission-mode","permissionMode":"bypassPermissions"}"#]);

        let [Event::Error { message, fatal }] = events.as_slice() else {
            panic!("one warning: {events:?}");
        };
        assert!(!fatal);
        assert!(message.contains("bypassPermissions"), "{message}");
    }

    #[test]
    fn the_records_the_cli_keeps_for_its_own_screen_are_passed_over_in_silence() {
        let events = folded(&[
            r#"{"type":"attachment","attachment":{"type":"model"}}"#,
            r#"{"type":"file-history-snapshot","messageId":"m"}"#,
            r#"{"type":"queue-operation","operation":"add"}"#,
            r#"{"type":"mode","mode":"normal"}"#,
            r#"{"type":"atis-latch","atis":""}"#,
            r#"{"type":"last-prompt","lastPrompt":"again"}"#,
            r#"{"type":"system","subtype":"turn_duration","durationMs":12}"#,
        ]);

        assert_eq!(events, []);
    }

    #[test]
    fn the_clis_title_is_reported_when_it_changes_and_not_on_every_record() {
        let events = folded(&[
            r#"{"type":"ai-title","aiTitle":"Interstellar objects in catalog","sessionId":"s-1"}"#,
            r#"{"type":"ai-title","aiTitle":"Interstellar objects in catalog","sessionId":"s-1"}"#,
            r#"{"type":"ai-title","aiTitle":"  ","sessionId":"s-1"}"#,
            r#"{"type":"ai-title","aiTitle":"interstellar-objects-search-issue","sessionId":"s-1"}"#,
        ]);

        assert_eq!(
            events,
            [
                Event::Titled {
                    title: "Interstellar objects in catalog".to_owned()
                },
                Event::Titled {
                    title: "interstellar-objects-search-issue".to_owned()
                },
            ]
        );
    }

    #[test]
    fn a_transcript_the_cli_never_titled_is_titled_by_the_first_prompt_the_operator_typed() {
        let events = imported(&[
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"Caveat: the messages below were generated by the user"}}"#,
            r#"{"type":"user","message":{"role":"user","content":"Add etag support to the fetcher"}}"#,
        ]);

        assert_eq!(
            events.first(),
            Some(&Event::Titled {
                title: "Add etag support to the fetcher".to_owned()
            })
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Titled { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn a_transcript_the_cli_titled_is_titled_by_the_cli_alone() {
        let events = imported(&[
            r#"{"type":"user","message":{"role":"user","content":"Add etag support to the fetcher"}}"#,
            r#"{"type":"ai-title","aiTitle":"Etag support","sessionId":"s-1"}"#,
        ]);

        let titles: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::Titled { title } => Some(title.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(titles, ["Etag support"]);
    }

    /// Folds the transcript `lines` through the whole of [`events`], which is
    /// where a file is read as a file rather than as records already parsed.
    fn imported(lines: &[&str]) -> Vec<Event> {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let path = transcript(dir.path(), "s-1", Duration::ZERO, &lines.join("\n"));
        events(&path, "max", Path::new("/repo")).expect("the transcript reads")
    }

    #[test]
    fn a_session_recorded_by_a_cli_release_nobody_recorded_says_so_once() {
        let events = imported(&[
            r#"{"type":"mode","mode":"normal","version":"9.9.9"}"#,
            r#"{"type":"mode","mode":"normal","version":"9.9.9"}"#,
            &prompt("what did this cost?"),
        ]);

        let said: Vec<&String> = events
            .iter()
            .filter_map(|event| match event {
                Event::Error { message, .. } => Some(message),
                _ => None,
            })
            .collect();
        assert_eq!(said.len(), 1, "{events:?}");
        assert!(said[0].contains("9.9.9"), "{said:?}");
    }

    #[test]
    fn a_session_recorded_by_a_recorded_cli_release_says_nothing_about_it() {
        let version = crate::conformance::RECORDED
            .last()
            .expect("a recorded release");
        let events = imported(&[
            &format!(r#"{{"type":"mode","mode":"normal","version":"{version}"}}"#),
            &prompt("what did this cost?"),
        ]);

        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::Error { .. })),
            "{events:?}"
        );
    }

    #[test]
    fn a_record_this_version_cannot_read_is_shown_and_the_rest_is_kept() {
        let events = folded(&[
            r#"{"type":"teleport","to":"mars"}"#,
            "not json at all",
            &prompt("still folded"),
        ]);

        // A type nobody has read is a notice; a line that is not a record at
        // all is something that could not be read.
        let [
            Event::Notice { message: unknown },
            Event::Error {
                message: unreadable,
                ..
            },
            said,
        ] = events.as_slice()
        else {
            panic!("a notice, a warning and the turn: {events:?}");
        };
        assert!(unknown.contains("teleport"), "{unknown}");
        assert!(unreadable.contains("(untyped)"), "{unreadable}");
        assert_eq!(
            said,
            &Event::UserMessage {
                text: "still folded".to_owned()
            }
        );
    }

    /// One API response, written out as the CLI writes one.
    fn response(id: &str, model: &str, input: u64, output: u64) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"id":"{id}","role":"assistant","model":"{model}",
               "content":[{{"type":"text","text":"said"}}],
               "usage":{{"input_tokens":{input},"output_tokens":{output}}}}}}}"#
        )
        .replace('\n', "")
    }

    #[test]
    fn a_session_the_cli_has_not_closed_is_counted_from_its_own_messages() {
        let one = response("msg_1", "claude-opus-5", 3, 40);

        let events = folded(&[&one, &one]);

        let counted: Vec<&Event> = events
            .iter()
            .filter(|event| matches!(event, Event::Usage(_)))
            .collect();
        // Once for the response, however many records it was written over, and
        // with nothing else to count it from.
        assert_eq!(
            counted,
            [&Event::Usage(niobe_core::Usage {
                input: 3,
                output: 40,
                cache_read: 0,
                cache_write: 0,
                cache_write_1h: 0,
                reasoning: 0,
                model: "claude-opus-5".to_owned(),
                cost_usd: None,
                cost_basis: None,
                settles_model: false,
            })]
        );
    }

    /// A session the CLI has not closed is counted from its messages, and the
    /// lifetime a message's cache writes were bought for is on the message.
    /// Dropping it here would price those writes at the five-minute rate, on
    /// exactly the sessions that have no accounting to be checked against.
    #[test]
    fn an_unclosed_sessions_cache_writes_keep_the_lifetime_they_were_bought_for() {
        let written = r#"{"type":"assistant","message":{"id":"msg_1","role":"assistant","model":"claude-opus-5","content":[{"type":"text","text":"said"}],"usage":{"input_tokens":3,"output_tokens":40,"cache_read_input_tokens":900,"cache_creation_input_tokens":80,"cache_creation":{"ephemeral_1h_input_tokens":50,"ephemeral_5m_input_tokens":30}}}}"#;

        let events = folded(&[written]);

        let counted: Vec<&Event> = events
            .iter()
            .filter(|event| matches!(event, Event::Usage(_)))
            .collect();
        assert_eq!(
            counted,
            [&Event::Usage(niobe_core::Usage {
                input: 3,
                output: 40,
                cache_read: 900,
                cache_write: 80,
                cache_write_1h: 50,
                reasoning: 0,
                model: "claude-opus-5".to_owned(),
                cost_usd: None,
                cost_basis: None,
                settles_model: false,
            })]
        );
    }

    #[test]
    fn a_message_the_cli_wrote_itself_does_not_rename_the_model_the_session_runs_on() {
        let events = folded(&[
            &response("msg_1", "claude-opus-5", 3, 40),
            &response("msg_2", NO_MODEL, 0, 0),
        ]);

        let models: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                Event::SessionMeta(meta) => Some(meta.model.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(models, ["claude-opus-5"]);
        // The message itself is still what the operator was shown.
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::AssistantMessage { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn a_cost_the_cli_could_not_finish_is_said_to_be_short() {
        let events = folded(&[
            r#"{"type":"cost-state","totalCostUSD":0.5,"hasUnknownModelCost":true,
                "modelUsage":{"m":{"inputTokens":1,"outputTokens":1,"costUSD":0.5}}}"#,
        ]);

        // The cost-state line is read as the CLI's closing line, so the turn
        // it closes ends after it.
        let [
            Event::Error { message, fatal },
            Event::Usage(_),
            Event::TurnEnded,
        ] = events.as_slice()
        else {
            panic!("the warning and the cost: {events:?}");
        };
        assert!(!fatal);
        assert!(message.contains("less than the session cost"), "{message}");
    }

    #[test]
    fn the_turns_the_cli_wrote_for_itself_are_not_the_operators() {
        let events = folded(&[
            r#"{"type":"user","message":{"role":"user","content":"a caveat"},"isMeta":true}"#,
            &prompt("and this is mine"),
        ]);

        assert_eq!(
            events,
            [Event::UserMessage {
                text: "and this is mine".to_owned()
            }]
        );
    }

    /// A sub-agent call and the CLI's answer that it launched the agent in the
    /// background, as Claude Code 2.1.278 writes both to its transcript.
    const LAUNCHED: [&str; 2] = [
        r#"{"type":"assistant","message":{"id":"msg_1","model":"claude-haiku-4-5-20251001","content":[{"type":"tool_use","id":"toolu_a","name":"Agent","input":{"subagent_type":"quick-lookup","description":"Summarize catalog/cache.py","prompt":"Read it."}}]}}"#,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_a","content":[{"type":"text","text":"Async agent launched successfully."}]}]},"toolUseResult":{"isAsync":true,"status":"async_launched","agentId":"a819f5cc82e486a11"}}"#,
    ];

    /// The turn the CLI writes when background work stops, naming the call
    /// that started it.
    fn notification(id: &str, status: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":"<task-notification>\n<task-id>a819f5cc82e486a11</task-id>\n<tool-use-id>{id}</tool-use-id>\n<output-file>/tmp/a.output</output-file>\n<status>{status}</status>\n<summary>Agent \"Summarize catalog/cache.py\" finished</summary>\n</task-notification>"}},"origin":{{"kind":"task-notification"}}}}"#
        )
    }

    #[test]
    fn a_background_sub_agent_in_a_transcript_ends_where_the_cli_wrote_that_it_stopped() {
        let launched = folded(&LAUNCHED);
        assert!(
            launched
                .iter()
                .any(|event| matches!(event, Event::AgentSpawn { .. })),
            "{launched:?}"
        );
        assert!(
            launched
                .iter()
                .all(|event| !matches!(event, Event::AgentExit { .. })),
            "ended at its launch: {launched:?}"
        );

        for (status, outcome) in [
            ("completed", AgentOutcome::Completed),
            ("failed", AgentOutcome::Failed),
            ("killed", AgentOutcome::Cancelled),
        ] {
            let stopped = notification("toolu_a", status);
            let events = folded(&[LAUNCHED[0], LAUNCHED[1], &stopped]);

            let exits: Vec<&Event> = events
                .iter()
                .filter(|event| matches!(event, Event::AgentExit { .. }))
                .collect();
            assert_eq!(
                exits,
                [&Event::AgentExit {
                    id: AgentId::new("toolu_a"),
                    outcome
                }],
                "{status}"
            );
        }
    }

    #[test]
    fn the_cli_saying_background_work_stopped_is_not_the_operator_speaking() {
        let about_a_command = notification("toolu_bash", "completed");
        let events = folded(&[&about_a_command]);

        assert_eq!(events, []);
    }

    /// An edit as the CLI writes it to its own transcript: the report of the
    /// change is under `toolUseResult`, the transcript's spelling of the live
    /// stream's `tool_use_result`.
    #[test]
    fn an_imported_edit_carries_the_hunks_the_cli_wrote_beside_it() {
        let events = folded(&[
            r#"{"type":"assistant","message":{"id":"msg_1","model":"claude-opus-5","content":[{"type":"tool_use","id":"toolu_e","name":"Edit","input":{"file_path":"/repo/notes.txt","old_string":"beta","new_string":"gamma","replace_all":false}}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_e","content":"The file /repo/notes.txt has been updated successfully."}]},"toolUseResult":{"filePath":"/repo/notes.txt","oldString":"beta","newString":"gamma","structuredPatch":[{"oldStart":1,"oldLines":2,"newStart":1,"newLines":2,"lines":[" alpha","-beta","+gamma"]}],"userModified":false,"replaceAll":false}}"#,
        ]);

        let hunks: Vec<usize> = events
            .iter()
            .filter_map(|event| match event {
                Event::FileChange { hunks, .. } => Some(hunks.len()),
                _ => None,
            })
            .collect();
        assert_eq!(hunks, [1], "{events:?}");
    }

    /// Writes a session's file and one sub-agent's file beside it, spawned by
    /// call `toolu_a`, and folds the session.
    fn with_agent(session: &[&str], agent: &[&str]) -> Vec<Event> {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let path = dir.path().join(format!("s.{EXTENSION}"));
        std::fs::write(&path, session.join("\n")).expect("the session is written");
        let sides = dir.path().join("s").join(SUB_AGENTS);
        std::fs::create_dir_all(&sides).expect("the sub-agents' directory is made");
        std::fs::write(
            sides.join(format!("agent-a1{META}")),
            r#"{"toolUseId":"toolu_a"}"#,
        )
        .expect("written");
        std::fs::write(
            sides.join(format!("agent-a1.{EXTENSION}")),
            agent.join("\n"),
        )
        .expect("written");
        events(&path, "max", Path::new("/repo")).expect("the transcript reads")
    }

    fn output_counts(events: &[Event]) -> Vec<(String, u64)> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Usage(usage) => Some((usage.model.clone(), usage.output)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_response_written_over_several_records_is_counted_once_by_its_last() {
        let record = |at: &str, output: u64, block: &str| {
            format!(
                r#"{{"type":"assistant","timestamp":"{at}","message":{{"id":"msg_1","model":"claude-opus-5","content":[{block}],"usage":{{"input_tokens":2,"output_tokens":{output}}}}}}}"#
            )
        };
        let thinking = r#"{"type":"thinking","thinking":"…","signature":"s"}"#;
        let text = r#"{"type":"text","text":"Done."}"#;

        let events = with_agent(
            &[
                &record("2026-09-19T11:14:40.000Z", 3, thinking),
                &record("2026-09-19T11:14:41.000Z", 40, text),
            ],
            &[],
        );
        assert_eq!(output_counts(&events), [("claude-opus-5".to_owned(), 40)]);
    }

    #[test]
    fn an_agents_record_stamped_before_its_spawn_waits_for_the_spawn() {
        let spawn = r#"{"type":"assistant","timestamp":"2026-09-19T11:14:44.000Z","message":{"id":"msg_1","model":"claude-opus-5","content":[{"type":"tool_use","id":"toolu_a","name":"Agent","input":{"subagent_type":"quick-lookup","description":"Look","prompt":"Look."}}]}}"#;
        let early = r#"{"type":"assistant","timestamp":"2026-09-19T11:14:43.000Z","message":{"id":"msg_a","model":"claude-haiku-4-5","content":[{"type":"tool_use","id":"toolu_r","name":"Read","input":{"file_path":"/repo/a.py"}}],"usage":{"input_tokens":1,"output_tokens":5}}}"#;

        let events = with_agent(&[spawn], &[early]);
        let order: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                Event::AgentSpawn { .. } => Some("spawn"),
                Event::AgentProgress { .. } => Some("model"),
                Event::ToolCallStart { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(order, ["spawn", "toolu_a", "model", "toolu_r"]);
        assert_eq!(
            output_counts(&events),
            [("claude-haiku-4-5".to_owned(), 5)],
            "the agent's tokens are under its own model, and the spawn's record had none"
        );
    }

    #[test]
    fn an_agent_whose_spawn_the_session_never_records_is_left_out() {
        let early = r#"{"type":"assistant","timestamp":"2026-09-19T11:14:43.000Z","message":{"id":"msg_a","model":"claude-haiku-4-5","content":[{"type":"tool_use","id":"toolu_r","name":"Read","input":{}}],"usage":{"input_tokens":1,"output_tokens":5}}}"#;
        let said = r#"{"type":"user","timestamp":"2026-09-19T11:14:40.000Z","message":{"content":"hello"}}"#;

        let events = with_agent(&[said], &[early]);
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, Event::ToolCallStart { .. } | Event::Usage(_))),
            "{events:?}"
        );
    }
}
