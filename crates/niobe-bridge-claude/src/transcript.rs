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
//! * **Most records are the CLI's own furniture.** Titles, attachments, file
//!   snapshots and queue operations say nothing about what the session did,
//!   what it changed or what it cost. They are named here so that they are
//!   passed over in silence rather than reported as messages Niobe cannot
//!   read.
//!
//! [`Event::UserMessage`]: niobe_core::event::Event::UserMessage

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;

use niobe_core::event::Event;

use crate::conformance;
use crate::translate::{self, Translator};
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

/// What the CLI puts where a message's model would be when it wrote the
/// message itself rather than asking a model for it — the line it shows when a
/// request failed, and the like. Read off the transcripts above, where it
/// stands against exactly the messages no model produced.
const NO_MODEL: &str = "<synthetic>";

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
    let read: Vec<(&str, Result<Line, serde_json::Error>)> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| (line, serde_json::from_str::<Line>(line)))
        .collect();
    let priced = read
        .iter()
        .any(|(_, record)| matches!(record, Ok(Line::CostState(_))));

    let mut fold = Fold::new(profile, cwd, id, priced);
    // Before anything is folded: what was recorded by a release nobody has
    // read is read at the operator's own risk, and a session imported from one
    // is read without anyone watching it happen.
    if let Some(version) = release_of(&text)
        && !conformance::recorded(&version)
    {
        fold.out
            .push(translate::warn(conformance::unrecorded(&version)));
    }
    for (line, record) in read {
        fold.record(line, record);
    }
    Ok(fold.out)
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
/// itself, on one line.
fn first_prompt(path: &Path) -> Option<String> {
    let file = BufReader::new(File::open(path).ok()?);
    for line in file.lines() {
        let Ok(record) = serde_json::from_str::<Line>(&line.ok()?) else {
            continue;
        };
        let Line::User(user) = record else { continue };
        if user.is_meta {
            continue;
        }
        let Some(said) = user.message.content.as_ref().and_then(said) else {
            continue;
        };
        if CLI_WROTE_IT.iter().any(|mark| said.starts_with(mark)) {
            continue;
        }
        return Some(said);
    }
    None
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
    /// The mode the session was last recorded in, as the CLI spells it. The
    /// CLI writes the mode out again whenever it writes anything, so only a
    /// change is worth an event.
    mode: Option<String>,
}

impl Fold {
    fn new(profile: &str, cwd: &Path, id: String, priced: bool) -> Self {
        Self {
            translator: Translator::new(profile).in_dir(cwd).of_session(id),
            out: Vec::new(),
            priced,
            counted: BTreeMap::new(),
            mode: None,
        }
    }

    fn record(&mut self, line: &str, record: Result<Line, serde_json::Error>) {
        match record {
            Ok(Line::Assistant(record)) => self.assistant(record),
            Ok(Line::User(record)) => self.user(record),
            Ok(Line::CostState(record)) => self.cost(record),
            Ok(Line::PermissionMode(record)) => self.mode(record),
            Ok(Line::Aside) => {}
            Ok(Line::Unknown) => self.out.push(translate::warn(format!(
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

    fn assistant(&mut self, record: Assistant) {
        let Assistant { message } = record;

        // The model is named on the message itself, which is where the live
        // stream reads it from too — off the head of the message rather than
        // off the session, so that a session that moved model says so where it
        // moved. A message the CLI wrote itself, such as the one it shows when
        // a request failed, is marked as written by no model; it is still what
        // the operator was shown, so it is kept, but naming the session after
        // it would put a model that does not exist in the status line and
        // would overwrite the one the session is really on.
        if let Some(model) = message.model.filter(|model| model != NO_MODEL) {
            self.fold(wire::Message::StreamEvent(wire::StreamEvent {
                event: wire::StreamBody::MessageStart {
                    message: wire::StartMessage { model: Some(model) },
                },
                parent_tool_use_id: None,
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
            },
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
        if let Some(usage) = message.usage
            && first_time
            && !self.priced
        {
            self.fold(wire::Message::StreamEvent(wire::StreamEvent {
                event: wire::StreamBody::MessageDelta { usage: Some(usage) },
                parent_tool_use_id: None,
            }));
        }
    }

    fn user(&mut self, record: User) {
        // The CLI writes notes of its own into the transcript as user turns —
        // the caveat that precedes a local command's output, and the like —
        // and marks them. They are not what the operator said.
        if record.is_meta {
            return;
        }
        if let Some(text) = record.message.content.as_ref().and_then(said) {
            self.out.push(Event::UserMessage { text });
        }
        // The same record carries the results of the calls the turn before it
        // made, which the translator reads and this does not.
        self.fold(wire::Message::User(wire::Envelope {
            message: wire::ApiMessage {
                content: record.message.content,
            },
        }));
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
                 does not model. The status line shows no mode until the session is moved to \
                 one it does."
            )),
        });
        self.mode = Some(spelt);
    }

    fn fold(&mut self, message: wire::Message) {
        self.out.append(&mut self.translator.message(message));
    }
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
    /// Records that say nothing about what the session did, changed or cost:
    /// the CLI's own title for the session (`ai-title`) and the name it gives
    /// a sub-agent's session (`agent-name`), the prompt it offers to repeat
    /// (`last-prompt`), the context it attached to a turn (`attachment`), the
    /// file snapshots a rewind would restore (`file-history-snapshot`,
    /// `file-history-delta`), its queue (`queue-operation`), its editing mode
    /// (`mode`, which is not the permission mode), its own notes to the screen
    /// (`system`), its latched status line (`atis-latch`), the pull request it
    /// opened from the session (`pr-link`) and the handle its own service
    /// holds the session under (`bridge-session`).
    ///
    /// Read off every record type present across the transcripts on the
    /// machine this was written on — sixteen in all — rather than off the
    /// types one session happened to produce. Four of them are read
    /// (`assistant`, `user`, `cost-state`, `permission-mode`) and the other
    /// twelve are here. A type left out is a warning entry per record in front
    /// of the operator, for a record that says nothing: `bridge-session` alone
    /// stood in fifteen thousand of them.
    #[serde(
        rename = "system",
        alias = "agent-name",
        alias = "ai-title",
        alias = "atis-latch",
        alias = "attachment",
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
}

/// The turn itself.
#[derive(Debug, Deserialize)]
struct UserBody {
    content: Option<wire::Content>,
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

    use niobe_core::event::Mode;

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
        let priced = lines
            .iter()
            .any(|line| line.contains(r#""type":"cost-state""#));
        let mut fold = Fold::new("max", Path::new("/repo"), "s-1".to_owned(), priced);
        for line in lines {
            fold.record(line, serde_json::from_str::<Line>(line));
        }
        fold.out
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
            r#"{"type":"ai-title","aiTitle":"Etag support"}"#,
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
    fn a_record_this_version_cannot_read_is_reported_and_the_rest_is_kept() {
        let events = folded(&[
            r#"{"type":"teleport","to":"mars"}"#,
            "not json at all",
            &prompt("still folded"),
        ]);

        let [
            Event::Error {
                message: unknown, ..
            },
            Event::Error {
                message: unreadable,
                ..
            },
            said,
        ] = events.as_slice()
        else {
            panic!("two warnings and the turn: {events:?}");
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

        let [Event::Error { message, fatal }, Event::Usage(_)] = events.as_slice() else {
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
}
