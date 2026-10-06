// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Attaching a session to the backend its profile runs.
//!
//! This is the only module that names a bridge. The shell is handed something
//! that implements [`Bridge`] and never learns which one, which is what keeps
//! a second backend a file here rather than a change everywhere.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::SystemTime;

use niobe_bridge_claude::transcript::{self, TranscriptError};
use niobe_bridge_claude::{Options, Session, SpawnError, Spent};
use niobe_config::Selected;
use niobe_core::event::{Backend, Event, Mode, PermissionDecision, ToolCallId};
use niobe_core::image::Image;
use niobe_tui::bridge::{Bridge, BridgeError, Detached};

/// What a session is attached with, beyond the profile that names the backend.
///
/// All three are decided by the invocation and by what a resumed session
/// already recorded, so they travel together rather than as a widening
/// argument list.
#[derive(Debug, Clone, Default)]
pub struct Attach {
    /// The id the backend calls an earlier session by, where one was recorded.
    pub resume: Option<String>,
    /// How tool calls are gated. `None` leaves it at what a new session starts
    /// in, which is the mode the shell shows before anything has said.
    pub mode: Option<Mode>,
    /// The most the session may spend, in USD.
    pub budget_usd: Option<f64>,
    /// Whether the backend is asked to title the session from its first
    /// prompt. Only a new session is: one carried on was titled, if it was,
    /// when it began.
    pub title: bool,
}

/// What a session is attached to.
#[derive(Debug)]
pub struct Attachment {
    bridge: Box<dyn Bridge>,
    attached: bool,
    process_groups: Option<Receiver<u32>>,
}

impl Attachment {
    /// Whether a backend is listening, which is what tells the shell that a
    /// prompt has somewhere to go.
    pub fn attached(&self) -> bool {
        self.attached
    }

    /// Where the process group each backend subprocess leads is named, where
    /// one was started: the first is there already, and one more comes each
    /// time the backend is started again within the session. Taken once, by
    /// whatever is to end those groups if the session cannot.
    pub fn take_process_groups(&mut self) -> Option<Receiver<u32>> {
        self.process_groups.take()
    }

    /// The backend, for the event loop.
    pub fn bridge(&mut self) -> &mut dyn Bridge {
        self.bridge.as_mut()
    }
}

/// Opens the backend `profile` runs, in the repository at `root`.
///
/// A session under no profile, and one under a profile whose backend has no
/// bridge, is attached to nothing: the shell says so when a prompt is sent,
/// rather than the binary refusing to start over a backend the operator may
/// not have wanted to use. A backend that has a bridge and cannot be started
/// *is* a failure, and is reported before the shell takes the terminal, so
/// that instructions land on a screen the operator can read.
///
/// `with.resume` is the id the backend calls an earlier session by, where one
/// was recorded: Niobe's session id names the recording, and only the CLI's own
/// id can hand its transcript back.
///
/// `config_dir` and `home` are the values of `CLAUDE_CONFIG_DIR` and `HOME`,
/// which say where a resumed session's transcript is.
pub fn attach(
    root: &Path,
    profile: Option<&Selected<'_>>,
    with: &Attach,
    config_dir: Option<OsString>,
    home: Option<OsString>,
) -> Result<Attachment, String> {
    let Some(selected) = profile else {
        return Ok(detached());
    };

    match selected.profile.backend() {
        Backend::Claude => {
            let transcripts = transcripts(Some(selected), root, config_dir.clone(), home.clone());
            let options = resumed_options(root, selected, with, config_dir, home)?;
            let session = Session::spawn(&options).map_err(describe)?;
            let (groups, process_groups) = channel();
            // The receiver is in hand, so the send cannot fail.
            let _ = groups.send(session.process_group());
            Ok(Attachment {
                bridge: Box::new(Claude {
                    session,
                    transcripts,
                    groups,
                }),
                attached: true,
                process_groups: Some(process_groups),
            })
        }
        // Not implemented yet: neither the `codex` bridge nor the native agent
        // loop spawns anything, so a session under one of them has a profile
        // and no backend. The shell says which it is when a prompt is sent.
        Backend::Codex | Backend::Native => Ok(detached()),
    }
}

/// One session a backend recorded for itself, as the session list shows it.
///
/// The bridge's own type does not leave this module. A session list is drawn
/// from what any backend can say about a session of its own — what it calls
/// it, when it last wrote to it, and what it was asked — and not from what the
/// `claude` CLI happens to write down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// The id the backend calls the session by, which is what `--resume`
    /// takes.
    pub id: String,
    /// When the backend last wrote to it, where it could say.
    pub last_at: Option<SystemTime>,
    /// The first thing the operator asked it, where there is one.
    pub first_prompt: Option<String>,
}

/// The file the `claude` CLI reads its own settings out of, in its
/// configuration directory.
const CLI_SETTINGS: &str = "settings.json";

/// The `claude` CLI's own settings file, where that file puts every session on
/// this machine on Bedrock.
///
/// Read because a profile that names no settings file of its own runs under
/// this one whatever its `env` says: the CLI lays its settings' `env` over the
/// environment its process was started with, so a variable a profile cleared
/// is set again by the file. Which is the whole reason a profile can name a
/// settings file at all, and why a listing says which profiles have not.
///
/// One key is read — `env.CLAUDE_CODE_USE_BEDROCK` — and nothing is written.
/// No credential of the CLI's is opened here or anywhere else in Niobe.
///
/// `None` where there is no such file, where it cannot be read, where it is
/// not JSON or where it does not turn Bedrock on: a note nothing substantiates
/// is not a note worth printing.
pub fn bedrock_settings(config_dir: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let dir = transcript::config_dir(config_dir.as_deref(), home.as_deref())?;
    let path = dir.join(CLI_SETTINGS);
    let text = std::fs::read_to_string(&path).ok()?;
    let settings: serde_json::Value = serde_json::from_str(&text).ok()?;
    let set = settings.get("env")?.get("CLAUDE_CODE_USE_BEDROCK")?;
    on(set).then_some(path)
}

/// Whether a value the CLI's settings set a variable to turns it on.
///
/// The CLI reads its settings' `env` as strings, and the way one is turned off
/// there is `"0"` or the empty string — which is also how a settings file
/// clears a variable the settings beneath it set. A value of another JSON type
/// is not one the CLI would take, so nothing is read into it.
fn on(value: &serde_json::Value) -> bool {
    match value.as_str() {
        Some(text) => !text.is_empty() && text != "0",
        None => false,
    }
}

/// Every file the backend `profile` runs gives its agent as memory, for a
/// session at `cwd`, in the order the agent is given them, as the shell lists
/// them.
///
/// Only the `claude` CLI loads memory files Niobe knows the rules of; another
/// backend, or a shell with no profile and so no backend, has none to list. The CLI's configuration directory is
/// found as [`transcripts`] finds it, since its auto-memory is kept beside
/// the transcripts.
pub fn memory(
    profile: Option<&niobe_config::Profile>,
    cwd: &Path,
    config_dir: Option<OsString>,
    home: Option<OsString>,
) -> Vec<niobe_tui::MemoryFile> {
    let Some(profile) = profile.filter(|profile| profile.backend() == Backend::Claude) else {
        return Vec::new();
    };
    let configured = profile
        .env()
        .get(transcript::CONFIG_DIR_VAR)
        .map(OsString::from)
        .or(config_dir);
    let config = transcript::config_dir(configured.as_deref(), home.as_deref());
    let home = home.map(PathBuf::from).filter(|home| home.is_absolute());
    niobe_bridge_claude::memory::sources(&niobe_bridge_claude::memory::Lookup {
        cwd,
        config: config.as_deref(),
        home: home.as_deref(),
    })
    .into_iter()
    .map(|source| memory_file(source, cwd, home.as_deref()))
    .collect()
}

/// One of the CLI's memory files, as the shell lists it.
fn memory_file(
    source: niobe_bridge_claude::memory::Source,
    cwd: &Path,
    home: Option<&Path>,
) -> niobe_tui::MemoryFile {
    use niobe_bridge_claude::memory::Scope as Found;
    use niobe_tui::memory::Scope;

    let scope = match source.scope {
        Found::Managed => Scope::Managed,
        Found::User => Scope::User,
        Found::Parent => Scope::Parent,
        Found::Project => Scope::Project,
        Found::Local => Scope::Local,
        Found::Imported => Scope::Imported,
        Found::AutoMemory => Scope::AutoMemory,
        Found::AutoEntry => Scope::AutoEntry,
    };
    let inside = source.path.strip_prefix(cwd).ok();
    let under_home = home.and_then(|home| source.path.strip_prefix(home).ok());
    let path = match (scope, inside, under_home) {
        // Listed under the index that names it, beside which it is kept.
        (Scope::AutoEntry, _, _) => source.path.file_name().map_or_else(
            || source.path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        ),
        (_, Some(inside), _) => inside.display().to_string(),
        (_, None, Some(under_home)) => format!("~/{}", under_home.display()),
        (_, None, None) => source.path.display().to_string(),
    };
    // A change is named relative to where the session runs where the file
    // is inside it, as the bridge names it, and absolute otherwise.
    let named = inside.map_or_else(
        || source.path.display().to_string(),
        |inside| inside.display().to_string(),
    );
    let editable = std::fs::symlink_metadata(&source.path)
        .is_ok_and(|meta| meta.file_type().is_file())
        .then(|| source.path.display().to_string());
    niobe_tui::MemoryFile {
        scope,
        path,
        named,
        depth: source.depth,
        bytes: source.bytes,
        text: source.text,
        note: source.note,
        editable,
    }
}

/// Where the `claude` CLI keeps the transcripts of the sessions it has run in
/// `cwd`, from the values of `CLAUDE_CONFIG_DIR` and `HOME`.
///
/// The profile's own environment is read first, because it is the environment
/// the CLI would be started with: a profile that points the binary at another
/// configuration directory points its transcripts there too.
///
/// `None` where nothing says which directory that is, which is a machine with
/// neither variable set.
pub fn transcripts(
    profile: Option<&Selected<'_>>,
    cwd: &Path,
    config_dir: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    let configured = profile
        .and_then(|selected| selected.profile.env().get(transcript::CONFIG_DIR_VAR))
        .map(OsString::from)
        .or(config_dir);
    let config = transcript::config_dir(configured.as_deref(), home.as_deref())?;
    Some(transcript::directory(&config, cwd))
}

/// The sessions the `claude` CLI has recorded for `cwd`, newest first.
///
/// A machine that says nothing about where the CLI keeps its state has no such
/// sessions to offer, which is an empty list and not a failure: Niobe's own
/// sessions are still there to list.
pub fn recorded(
    profile: Option<&Selected<'_>>,
    cwd: &Path,
    config_dir: Option<OsString>,
    home: Option<OsString>,
) -> Result<Vec<Recorded>, String> {
    let Some(dir) = transcripts(profile, cwd, config_dir, home) else {
        return Ok(Vec::new());
    };
    let listed = transcript::list(&dir).map_err(|e| e.to_string())?;
    Ok(listed
        .into_iter()
        .map(|transcript| Recorded {
            id: transcript.id,
            last_at: transcript.last_at,
            first_prompt: transcript.first_prompt,
        })
        .collect())
}

/// Everything one of those sessions says happened, as events.
///
/// The transcript is read and nothing is written back: the CLI's store stays
/// the CLI's, and the id is handed to its own `--resume` to carry the
/// conversation on.
pub fn history(
    profile: Option<&Selected<'_>>,
    cwd: &Path,
    session: &str,
    config_dir: Option<OsString>,
    home: Option<OsString>,
) -> Result<Vec<Event>, String> {
    let dir = transcripts(profile, cwd, config_dir, home).ok_or_else(|| {
        format!(
            "cannot look for claude session {session}: neither {} nor HOME says where the \
             claude CLI keeps its sessions",
            transcript::CONFIG_DIR_VAR
        )
    })?;
    let path = dir.join(format!("{session}.jsonl"));
    if !path.exists() {
        return Err(format!(
            "no claude session {session} in {}; `niobe sessions` lists the ones there are",
            dir.display()
        ));
    }
    // The transcript says nothing about the profile it was recorded under, so
    // what goes on the session is the profile it is being continued under —
    // and nothing, where none was selected.
    let name = profile.map_or("", |selected| selected.name);
    transcript::events(&path, name, cwd).map_err(|e| e.to_string())
}

/// What the `claude` CLI had recorded session `id` as spending when it last
/// left it, which it restores on `--resume` and which the resumed process's
/// first turn therefore reports on top of.
///
/// Nothing where the transcript cannot be found: the CLI then has nothing to
/// restore from either, as far as anything here can tell, and the resume
/// itself is what reports a session that is not there. A transcript that is
/// there and cannot be read is a spend that is unknown, not nothing: the CLI
/// may still restore it, and reading it as nothing would bill it again.
fn spent(
    profile: &Selected<'_>,
    root: &Path,
    id: &str,
    config_dir: Option<OsString>,
    home: Option<OsString>,
) -> Spent {
    spent_in(
        transcripts(Some(profile), root, config_dir, home).as_deref(),
        id,
    )
}

/// [`spent`], from the directory `dir` the CLI keeps its transcripts in.
fn spent_in(dir: Option<&Path>, id: &str) -> Spent {
    let Some(dir) = dir else {
        return Spent::default();
    };
    match transcript::spent(&dir.join(format!("{id}.jsonl"))) {
        Ok(spent) => spent,
        Err(TranscriptError::File { error, .. })
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Spent::default()
        }
        Err(error) => Spent::unknown(error.to_string()),
    }
}

/// [`claude_options`], starting from what the CLI's own transcript last
/// recorded the session spending where it resumes one: the CLI restores its
/// running totals on `--resume`, and a translator that started from nothing
/// would bill the first turn the whole earlier session again.
fn resumed_options(
    root: &Path,
    profile: &Selected<'_>,
    with: &Attach,
    config_dir: Option<OsString>,
    home: Option<OsString>,
) -> Result<Options, String> {
    let mut options = claude_options(root, profile, with, home.clone())?;
    if let Some(id) = &with.resume {
        options.spent = spent(profile, root, id, config_dir, home);
    }
    Ok(options)
}

/// What the `claude` bridge is spawned with under `profile`.
///
/// Written apart from the spawn so that what a profile turns into can be
/// checked without starting a real session on the operator's own
/// subscription — and so that a profile naming a settings file that is not
/// there fails here, which is before there is a process to fail after.
fn claude_options(
    root: &Path,
    profile: &Selected<'_>,
    with: &Attach,
    home: Option<OsString>,
) -> Result<Options, String> {
    let mut options = Options::new(root, profile.name);
    options.env = profile.profile.env().clone();
    options.args = profile.profile.args().to_vec();
    options.billing = profile.profile.billing();
    options.settings = crate::config::settings_path(profile, home)?;
    options.resume = with.resume.clone();
    options.budget_usd = with.budget_usd;
    // A resumed session starts the CLI in the mode it was left in; a new one
    // leaves it at the default, which is the mode the shell shows until
    // something says otherwise.
    if let Some(mode) = with.mode {
        options.mode = mode;
    }
    // The shell answers permission prompts, so the CLI is told to ask. The
    // flag is not set for a backend with nothing answering: the CLI stops the
    // turn on every gated call and waits for an answer that would never come.
    options.ask_over_stdio = true;
    // The shell's bridge starts the CLI again on its conversation when
    // something stops it between turns: see `Claude::carry_on`.
    options.carry_on = true;
    options.ask_title = with.title && with.resume.is_none();
    Ok(options)
}

/// What a CLI started again is started with: `next`, as the session that
/// stopped hands it on, from what the CLI's transcript in `transcripts` last
/// recorded the conversation spending, for the reason [`resumed_options`]
/// reads it.
fn carried_on(mut next: Options, transcripts: Option<&Path>) -> Options {
    if let Some(id) = &next.resume {
        next.spent = spent_in(transcripts, id);
    }
    next
}

fn detached() -> Attachment {
    Attachment {
        bridge: Box::new(Detached),
        attached: false,
        process_groups: None,
    }
}

/// What to print when a backend could not be started.
///
/// The error already says what to do; this only names the profile, because the
/// operator picked a profile and not a binary.
fn describe(error: SpawnError) -> String {
    format!("cannot start this profile's backend: {error}")
}

/// The `claude` bridge behind the trait the shell asked for.
#[derive(Debug)]
struct Claude {
    session: Session,
    /// Where the CLI keeps its transcripts, which is where what a CLI started
    /// again will restore as spent is read from.
    transcripts: Option<PathBuf>,
    /// Told the process group of each CLI started again.
    groups: Sender<u32>,
}

impl Claude {
    /// Starts the CLI again on its conversation where the one before was
    /// stopped between turns, and does nothing otherwise.
    ///
    /// Done when the operator next asks the backend for something rather than
    /// as soon as the stop is seen: whatever stopped the CLI — a shutdown, a
    /// logout, the operator — may still be under way, and a CLI started into
    /// it would only be stopped again. A start that fails leaves the session
    /// as it was, to be tried again by the next request.
    fn carry_on(&mut self) -> Result<(), BridgeError> {
        let Some(next) = self.session.next() else {
            return Ok(());
        };
        let next = carried_on(next, self.transcripts.as_deref());
        let session = Session::spawn(&next).map_err(|error| {
            BridgeError::from(format!(
                "the `claude` CLI could not be started again: {error}"
            ))
        })?;
        // Nothing may be listening: a session with no reaper.
        let _ = self.groups.send(session.process_group());
        self.session = session;
        Ok(())
    }
}

impl Bridge for Claude {
    fn send(&mut self, prompt: &str, images: &[Image]) -> Result<(), BridgeError> {
        self.carry_on()?;
        self.session.send(prompt, images).map_err(BridgeError::from)
    }

    fn answer(
        &mut self,
        id: &ToolCallId,
        decision: PermissionDecision,
        message: Option<&str>,
    ) -> Result<(), BridgeError> {
        self.session
            .answer(id, decision, message)
            .map_err(BridgeError::from)
    }

    fn set_mode(&mut self, mode: Mode) -> Result<(), BridgeError> {
        self.carry_on()?;
        self.session.set_mode(mode).map_err(BridgeError::from)
    }

    fn set_model(&mut self, model: &str) -> Result<(), BridgeError> {
        self.carry_on()?;
        self.session.set_model(model).map_err(BridgeError::from)
    }

    fn interrupt(&mut self) -> Result<(), BridgeError> {
        self.session.interrupt().map_err(BridgeError::from)
    }

    fn drain(&mut self) -> Vec<Event> {
        self.session.drain()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use niobe_config::Config;

    fn config(text: &str) -> Config {
        Config::parse(text, &PathBuf::from("config.toml")).expect("the config parses")
    }

    /// The memory a `claude` profile's agent is given is listed as the shell
    /// shows it: inside the repository relative to it, under the home
    /// directory from `~`, and an auto-memory note by its name under its
    /// index. A codex profile's backend loads none of it.
    #[test]
    fn the_memory_listed_is_named_as_the_shell_shows_it_and_only_for_claude() {
        use niobe_tui::memory::Scope;
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let home = dir.path().join("home");
        let repo = home.join("src").join("repo");
        let notes = transcript::directory(&home.join(".claude"), &repo).join("memory");
        for (path, text) in [
            (repo.join("CLAUDE.md"), "@AGENTS.md\n"),
            (repo.join("AGENTS.md"), "# Agents\n"),
            (home.join(".claude").join("CLAUDE.md"), "be brief\n"),
            (
                notes.join("MEMORY.md"),
                "- [Facts](facts.md) — verified facts\n",
            ),
            (notes.join("facts.md"), "init repeats\n"),
        ] {
            std::fs::create_dir_all(path.parent().expect("a file has a directory"))
                .expect("the temporary directory is writable");
            std::fs::write(&path, text).expect("the temporary directory is writable");
        }
        let claude = config("[profiles.max]\nbackend = \"claude\"\n");
        let listed = memory(
            claude.profiles().get("max"),
            &repo,
            None,
            Some(home.clone().into_os_string()),
        );
        let shown: Vec<(Scope, &str, &str, usize)> = listed
            .iter()
            .filter(|file| file.scope != Scope::Managed)
            .map(|file| {
                (
                    file.scope,
                    file.path.as_str(),
                    file.named.as_str(),
                    file.depth,
                )
            })
            .collect();
        let index = format!(
            "~/{}",
            notes
                .join("MEMORY.md")
                .strip_prefix(&home)
                .expect("under the home directory")
                .display()
        );
        let absolute = |path: &Path| path.display().to_string();
        assert_eq!(
            shown,
            [
                (
                    Scope::User,
                    "~/.claude/CLAUDE.md",
                    absolute(&home.join(".claude/CLAUDE.md")).as_str(),
                    0
                ),
                (Scope::Project, "CLAUDE.md", "CLAUDE.md", 0),
                (Scope::Imported, "AGENTS.md", "AGENTS.md", 1),
                (
                    Scope::AutoMemory,
                    index.as_str(),
                    absolute(&notes.join("MEMORY.md")).as_str(),
                    0
                ),
                (
                    Scope::AutoEntry,
                    "facts.md",
                    absolute(&notes.join("facts.md")).as_str(),
                    1
                ),
            ]
        );
        assert!(listed.iter().all(|file| file.editable.is_some()));

        let codex = config("[profiles.codex]\nbackend = \"codex\"\n");
        assert!(
            memory(
                codex.profiles().get("codex"),
                &repo,
                None,
                Some(home.into_os_string())
            )
            .is_empty()
        );
    }

    /// The `claude` CLI's own settings on a machine that has none, which is
    /// every machine these tests run on.
    const NO_HOME: Option<OsString> = None;

    #[test]
    fn a_resumed_session_starts_from_what_its_transcript_last_recorded_it_spending() {
        let config = config("[profiles.max]\nbackend = \"claude\"\n");
        let selected = config
            .select(Some("max"))
            .expect("the profile is defined")
            .expect("a profile was selected");
        let claude = tempfile::tempdir().expect("a temporary directory");
        let root = Path::new("/repo");
        let dir = transcript::directory(claude.path(), root);
        std::fs::create_dir_all(&dir).expect("the project directory is made");
        std::fs::write(
            dir.join("s-1.jsonl"),
            r#"{"type":"cost-state","totalCostUSD":0.25,"modelUsage":{"opus-5":{"inputTokens":1,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4,"costUSD":0.25}}}"#,
        )
        .expect("the transcript is written");

        let found = spent(
            &selected,
            root,
            "s-1",
            Some(claude.path().as_os_str().to_owned()),
            NO_HOME,
        );
        assert_eq!(found.cost_usd("opus-5"), Some(0.25));
        let options = resumed_options(
            root,
            &selected,
            &Attach {
                resume: Some("s-1".to_owned()),
                mode: Some(niobe_core::event::Mode::Plan),
                budget_usd: None,
                title: false,
            },
            Some(claude.path().as_os_str().to_owned()),
            NO_HOME,
        )
        .expect("the options are made");
        assert_eq!(options.spent.cost_usd("opus-5"), Some(0.25));
        assert_eq!(options.resume.as_deref(), Some("s-1"));
        assert_eq!(options.mode, niobe_core::event::Mode::Plan);

        let missing = spent(
            &selected,
            root,
            "s-2",
            Some(claude.path().as_os_str().to_owned()),
            NO_HOME,
        );
        assert!(missing.is_empty());
    }

    #[test]
    fn a_transcript_that_cannot_be_read_leaves_the_earlier_spend_unknown_and_says_why() {
        let config = config("[profiles.max]\nbackend = \"claude\"\n");
        let selected = config
            .select(Some("max"))
            .expect("the profile is defined")
            .expect("a profile was selected");
        let claude = tempfile::tempdir().expect("a temporary directory");
        let root = Path::new("/repo");
        let dir = transcript::directory(claude.path(), root);
        std::fs::create_dir_all(dir.join("s-1.jsonl")).expect("a directory where the file is");

        let found = spent(
            &selected,
            root,
            "s-1",
            Some(claude.path().as_os_str().to_owned()),
            NO_HOME,
        );
        assert!(!found.is_known());
        let first = niobe_bridge_claude::Translator::new("max")
            .resuming(&found)
            .line(r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus-5"}"#);
        let Some(Event::Notice { message }) = first.first() else {
            panic!("the first event says the spend is unknown: {first:?}");
        };
        assert!(message.contains("s-1.jsonl"), "{message}");
    }

    #[test]
    fn a_session_under_no_profile_is_attached_to_nothing() {
        let attachment = attach(Path::new("/repo"), None, &Attach::default(), None, NO_HOME)
            .expect("nothing to start");

        assert!(!attachment.attached());
    }

    #[test]
    fn a_backend_with_no_bridge_leaves_the_session_attached_to_nothing() {
        let config = config("[profiles.codex]\nbackend = \"codex\"\n");
        let selected = config
            .select(Some("codex"))
            .expect("the profile is defined")
            .expect("a profile was selected");

        let attachment = attach(
            Path::new("/repo"),
            Some(&selected),
            &Attach::default(),
            None,
            NO_HOME,
        )
        .expect("nothing to start");

        assert!(!attachment.attached());
    }

    #[test]
    fn a_claude_session_is_told_to_ask_because_the_shell_answers() {
        let config = config(
            "[profiles.max]\nbackend = \"claude\"\nenv = { A = \"1\" }\nargs = [\"--add-dir\", \"/other\"]\n",
        );
        let selected = config
            .select(Some("max"))
            .expect("the profile is defined")
            .expect("a profile was selected");

        let options = claude_options(
            Path::new("/repo"),
            &selected,
            &Attach {
                resume: Some("s-1".to_owned()),
                ..Attach::default()
            },
            NO_HOME,
        )
        .expect("the profile names no settings file to be missing");

        assert!(
            options.ask_over_stdio,
            "the CLI would decide for itself what the operator is meant to be asked"
        );
        let argv = options.argv();
        let flag = argv
            .iter()
            .position(|a| a == "--permission-prompt-tool")
            .expect("the prompt tool");
        assert_eq!(argv[flag + 1], "stdio");
        assert_eq!(options.profile, "max");
        assert_eq!(options.env["A"], "1");
        assert_eq!(options.args, ["--add-dir", "/other"]);
        assert_eq!(options.resume.as_deref(), Some("s-1"));
        assert_eq!(options.billing, None, "the profile left billing to the CLI");
    }

    #[test]
    fn a_profile_that_says_how_it_is_billed_tells_the_bridge() {
        let config = config("[profiles.company]\nbackend = \"claude\"\nbilling = \"metered\"\n");
        let selected = config
            .select(Some("company"))
            .expect("the profile is defined")
            .expect("a profile was selected");

        let options = claude_options(Path::new("/repo"), &selected, &Attach::default(), NO_HOME)
            .expect("the profile names no settings file to be missing");

        assert_eq!(options.billing, Some(niobe_core::Billing::Metered));
    }

    #[test]
    fn a_new_session_asks_the_cli_for_its_title_only_where_titles_are_on() {
        let config = config("[profiles.max]\nbackend = \"claude\"\n");
        let selected = config
            .select(Some("max"))
            .expect("the profile is defined")
            .expect("a profile was selected");
        let asks = |with: &Attach| {
            claude_options(Path::new("/repo"), &selected, with, NO_HOME)
                .expect("the profile names no settings file to be missing")
                .ask_title
        };

        assert!(asks(&Attach {
            title: true,
            ..Attach::default()
        }));
        assert!(!asks(&Attach::default()), "with titles off no call is made");
        assert!(
            !asks(&Attach {
                title: true,
                resume: Some("s-1".to_owned()),
                ..Attach::default()
            }),
            "a session carried on was titled, if at all, when it began"
        );
    }

    #[test]
    fn a_profile_that_names_a_settings_file_starts_the_cli_under_it() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let file = dir.path().join("claude-personal.json");
        std::fs::write(&file, "{}").expect("the settings file is written");
        let config =
            config("[profiles.max]\nbackend = \"claude\"\nsettings = \"~/claude-personal.json\"\n");
        let selected = config
            .select(Some("max"))
            .expect("the profile is defined")
            .expect("a profile was selected");

        let options = claude_options(
            Path::new("/repo"),
            &selected,
            &Attach::default(),
            Some(OsString::from(dir.path())),
        )
        .expect("the settings file is there");

        let argv = options.argv();
        let flag = argv
            .iter()
            .position(|a| a == "--settings")
            .unwrap_or_else(|| panic!("no --settings: {argv:?}"));
        assert_eq!(argv[flag + 1], file.display().to_string());
    }

    #[test]
    fn a_profile_that_names_no_settings_file_passes_no_settings_argument() {
        let config = config("[profiles.max]\nbackend = \"claude\"\n");
        let selected = config
            .select(Some("max"))
            .expect("the profile is defined")
            .expect("a profile was selected");

        let options = claude_options(Path::new("/repo"), &selected, &Attach::default(), NO_HOME)
            .expect("the profile names no settings file to be missing");

        assert_eq!(options.settings, None);
        // Not a flag with an empty value, and not the CLI's own settings
        // rewritten as one: a profile that says nothing starts the CLI the way
        // the operator's own settings would.
        assert!(!options.argv().contains(&"--settings".to_owned()));
    }

    #[test]
    fn a_settings_file_that_is_not_there_stops_the_session_before_anything_is_spawned() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let missing = dir.path().join("gone.json");
        let config = config(&format!(
            "[profiles.max]\nbackend = \"claude\"\nsettings = \"{}\"\n",
            missing.display()
        ));
        let selected = config
            .select(Some("max"))
            .expect("the profile is defined")
            .expect("a profile was selected");

        // Nothing here can spawn: the options a session is started from are
        // what fails, so there is no order in which the CLI runs first.
        let error = claude_options(Path::new("/repo"), &selected, &Attach::default(), NO_HOME)
            .expect_err("the settings file is not there");

        assert_eq!(
            error,
            format!(
                "config.toml:3: profiles.max.settings: no such file: {}",
                missing.display()
            )
        );
    }

    #[test]
    fn a_profile_from_a_file_nobody_trusted_starts_the_cli_with_nothing_of_its_own() {
        let config = config(
            "[profiles.repo]\nbackend = \"claude\"\n\
             env = { ANTHROPIC_BASE_URL = \"https://somewhere-else.example\" }\n\
             args = [\"--settings\", \"{}\"]\nauth_refresh = \"curl https://somewhere-else.example\"\n",
        )
        .untrusted();
        let selected = config
            .select(Some("repo"))
            .expect("the profile is defined")
            .expect("a profile was selected");

        let options = claude_options(Path::new("/repo"), &selected, &Attach::default(), NO_HOME)
            .expect("nothing of the profile's is in force");

        assert!(options.env.is_empty(), "{:?}", options.env);
        assert!(options.args.is_empty(), "{:?}", options.args);
        let argv = options.argv().join(" ");
        assert!(!argv.contains("somewhere-else"), "{argv}");
        assert!(!argv.contains("--settings"), "{argv}");
        // The refresh command is not a thing this module runs; it is not in
        // the profile at all once the file it came from is untrusted.
        assert_eq!(selected.profile.auth_refresh(), None);
    }

    #[test]
    fn an_untrusted_profile_cannot_move_where_the_clis_own_sessions_are_looked_for() {
        let config = config(
            "[profiles.repo]\nbackend = \"claude\"\nenv = { CLAUDE_CONFIG_DIR = \"/elsewhere\" }\n",
        )
        .untrusted();
        let selected = config
            .select(Some("repo"))
            .expect("the profile is defined")
            .expect("a profile was selected");

        assert_eq!(
            transcripts(
                Some(&selected),
                Path::new("/w/repo"),
                None,
                Some(OsString::from("/home/me"))
            ),
            Some(PathBuf::from("/home/me/.claude/projects/-w-repo")),
            "a clone pointed niobe at a directory of its own"
        );
    }

    #[test]
    fn a_sessions_budget_and_mode_reach_the_binary_that_enforces_them() {
        let config = config("[profiles.max]\nbackend = \"claude\"\n");
        let selected = config
            .select(Some("max"))
            .expect("the profile is defined")
            .expect("a profile was selected");

        let options = claude_options(
            Path::new("/repo"),
            &selected,
            &Attach {
                resume: None,
                mode: Some(Mode::Plan),
                budget_usd: Some(0.5),
                title: false,
            },
            NO_HOME,
        )
        .expect("the profile names no settings file to be missing");

        assert_eq!(options.mode, Mode::Plan);
        assert_eq!(options.budget_usd, Some(0.5));
    }

    #[test]
    fn a_session_that_says_nothing_about_its_mode_starts_where_the_shell_shows_it() {
        let config = config("[profiles.max]\nbackend = \"claude\"\n");
        let selected = config
            .select(Some("max"))
            .expect("the profile is defined")
            .expect("a profile was selected");

        let options = claude_options(Path::new("/repo"), &selected, &Attach::default(), NO_HOME)
            .expect("the profile names no settings file to be missing");

        // The shell cycles from `ask` when nothing has reported a mode, so a
        // session that starts anywhere else would move on the first keypress.
        assert_eq!(options.mode, Mode::Ask);
        assert_eq!(options.budget_usd, None);
    }

    #[test]
    fn a_profile_that_moves_the_clis_configuration_directory_moves_its_sessions_with_it() {
        let config = config(
            "[profiles.max]\nbackend = \"claude\"\nenv = { CLAUDE_CONFIG_DIR = \"/elsewhere\" }\n",
        );
        let selected = config
            .select(Some("max"))
            .expect("the profile is defined")
            .expect("a profile was selected");
        let home = Some(OsString::from("/home/me"));

        assert_eq!(
            transcripts(
                Some(&selected),
                Path::new("/w/repo"),
                Some(OsString::from("/ignored")),
                home.clone()
            ),
            Some(PathBuf::from("/elsewhere/projects/-w-repo"))
        );
        // With no profile saying otherwise, where this process was told.
        assert_eq!(
            transcripts(
                None,
                Path::new("/w/repo"),
                Some(OsString::from("/elsewhere")),
                home.clone()
            ),
            Some(PathBuf::from("/elsewhere/projects/-w-repo"))
        );
        assert_eq!(
            transcripts(None, Path::new("/w/repo"), None, home),
            Some(PathBuf::from("/home/me/.claude/projects/-w-repo"))
        );
    }

    /// A `claude` configuration directory holding `settings`, or holding
    /// nothing where that is `None`.
    fn cli_config(settings: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        if let Some(settings) = settings {
            std::fs::write(dir.path().join("settings.json"), settings)
                .expect("the settings file is written");
        }
        dir
    }

    #[test]
    fn settings_that_put_every_session_on_this_machine_on_bedrock_are_found() {
        let dir = cli_config(Some(
            r#"{"env": {"CLAUDE_CODE_USE_BEDROCK": "1", "AWS_PROFILE": "an-account"}}"#,
        ));

        assert_eq!(
            bedrock_settings(Some(OsString::from(dir.path())), None),
            Some(dir.path().join("settings.json"))
        );
    }

    #[test]
    fn settings_that_do_not_turn_bedrock_on_are_no_note_to_print() {
        // Cleared and off, which is how a settings file takes back what the
        // settings beneath it set, and a file that says nothing about it.
        for settings in [
            r#"{"env": {"CLAUDE_CODE_USE_BEDROCK": ""}}"#,
            r#"{"env": {"CLAUDE_CODE_USE_BEDROCK": "0"}}"#,
            r#"{"env": {"AWS_PROFILE": "an-account"}}"#,
            r#"{"model": "opus"}"#,
            "not json at all",
        ] {
            let dir = cli_config(Some(settings));
            assert_eq!(
                bedrock_settings(Some(OsString::from(dir.path())), None),
                None,
                "{settings}"
            );
        }

        let empty = cli_config(None);
        assert_eq!(
            bedrock_settings(Some(OsString::from(empty.path())), None),
            None
        );
        assert_eq!(bedrock_settings(None, None), None);
    }

    #[test]
    fn a_machine_that_says_nothing_about_the_cli_has_no_sessions_of_its_own_to_offer() {
        assert_eq!(transcripts(None, Path::new("/w/repo"), None, None), None);
        assert_eq!(
            recorded(None, Path::new("/w/repo"), None, None),
            Ok(Vec::new())
        );
    }

    #[test]
    fn a_backend_that_cannot_be_started_names_the_profile_it_came_from() {
        // Deliberately not a spawn: a test that ran `claude` would start a
        // real session on the operator's own subscription. What spawning
        // reports is asserted in the bridge; what is this module's is that a
        // failure to start is a failure of the profile, and reads as one.
        let said = describe(SpawnError::NotInstalled {
            binary: PathBuf::from("claude"),
        });

        assert!(
            said.starts_with("cannot start this profile's backend"),
            "{said}"
        );
        assert!(said.contains("is not on PATH"), "{said}");
    }
    /// The opening and the end of one turn of conversation `s-9`, gated in
    /// plan mode, as the CLI writes them.
    const ONE_TURN: &str = r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"s-9","model":"claude-sonnet-5","permissionMode":"plan"}' '{"type":"result","subtype":"success","is_error":false,"session_id":"s-9"}'"#;

    /// Drains `bridge` until `wanted` arrives, and hands back what it drained.
    fn drained_until(bridge: &mut dyn Bridge, wanted: impl Fn(&Event) -> bool) -> Vec<Event> {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut events = Vec::new();
        while !events.iter().any(&wanted) {
            assert!(
                std::time::Instant::now() < until,
                "it never came: {events:#?}"
            );
            events.extend(bridge.drain());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        events
    }

    #[cfg(unix)]
    #[test]
    fn a_cli_stopped_between_turns_is_started_again_on_its_conversation_by_the_next_prompt() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a temporary directory");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).expect("the stand-in's directory is made");
        let seen = dir.path().join("seen");
        std::fs::create_dir(&seen).expect("the record's directory is made");
        // Started once, it answers a turn and then leaves the way Node does on
        // a SIGTERM; started again, it answers and stays. Each start records
        // the arguments it was given. Only this directory and the system's
        // own are on its PATH, so no real `claude` can be reached.
        let script = bin.join("claude");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nn=$(ls {seen} | wc -l | tr -d ' ')\necho \"$@\" > {seen}/$n\nread -r initialize\nread -r turn\n{ONE_TURN}\n[ \"$n\" = 0 ] && exit 143\nexec sleep 30\n",
                seen = seen.display()
            ),
        )
        .expect("the stand-in is written");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("the stand-in is made executable");
        let config = config(&format!(
            "[profiles.max]\nbackend = \"claude\"\nenv = {{ PATH = \"{}:/usr/bin:/bin\" }}\n",
            bin.display()
        ));
        let selected = config
            .select(Some("max"))
            .expect("the profile is defined")
            .expect("a profile was selected");

        let mut attachment = attach(
            dir.path(),
            Some(&selected),
            &Attach::default(),
            Some(dir.path().join("cli").into_os_string()),
            NO_HOME,
        )
        .expect("the stand-in starts");
        let groups = attachment.take_process_groups().expect("a CLI was started");
        let first = groups.try_recv().expect("the first group is named");
        let bridge = attachment.bridge();
        bridge.send("one", &[]).expect("the turn is sent");
        let events = drained_until(bridge, |event| matches!(event, Event::Notice { .. }));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::Error { fatal: true, .. })),
            "{events:#?}"
        );

        bridge
            .send("two", &[])
            .expect("the CLI is started again and sent the turn");
        drained_until(bridge, |event| matches!(event, Event::TurnEnded));

        let again = groups.try_recv().expect("the new group is named");
        assert_ne!(again, first);
        let argv = std::fs::read_to_string(seen.join("1")).expect("the second start recorded");
        assert!(argv.contains("--resume s-9"), "{argv}");
        assert!(argv.contains("--permission-mode plan"), "{argv}");
    }

    #[test]
    fn a_cli_started_again_starts_from_what_its_transcript_last_recorded_it_spending() {
        let claude = tempfile::tempdir().expect("a temporary directory");
        let root = Path::new("/repo");
        let dir = transcript::directory(claude.path(), root);
        std::fs::create_dir_all(&dir).expect("the project directory is made");
        std::fs::write(
            dir.join("s-9.jsonl"),
            r#"{"type":"cost-state","totalCostUSD":0.25,"modelUsage":{"opus-5":{"inputTokens":1,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4,"costUSD":0.25}}}"#,
        )
        .expect("the transcript is written");
        let mut next = Options::new(root, "max");
        next.resume = Some("s-9".to_owned());

        let carried = carried_on(next, Some(&dir));

        assert_eq!(carried.spent.cost_usd("opus-5"), Some(0.25));
        assert_eq!(carried.resume.as_deref(), Some("s-9"));
    }
}
