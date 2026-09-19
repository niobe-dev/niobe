// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Attaching a session to the backend its profile runs.
//!
//! This is the only module that names a bridge. The shell is handed something
//! that implements [`Bridge`] and never learns which one, which is what keeps
//! a second backend a file here rather than a change everywhere.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use niobe_bridge_claude::transcript;
use niobe_bridge_claude::{Options, Session, SpawnError};
use niobe_config::Selected;
use niobe_core::event::{Backend, Event, Mode, PermissionDecision, ToolCallId};
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
}

/// What a session is attached to.
#[derive(Debug)]
pub struct Attachment {
    bridge: Box<dyn Bridge>,
    attached: bool,
}

impl Attachment {
    /// Whether a backend is listening, which is what tells the shell that a
    /// prompt has somewhere to go.
    pub fn attached(&self) -> bool {
        self.attached
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
pub fn attach(
    root: &Path,
    profile: Option<&Selected<'_>>,
    with: &Attach,
    home: Option<OsString>,
) -> Result<Attachment, String> {
    let Some(selected) = profile else {
        return Ok(detached());
    };

    match selected.profile.backend() {
        Backend::Claude => {
            let options = claude_options(root, selected, with, home)?;
            let session = Session::spawn(&options).map_err(describe)?;
            Ok(Attachment {
                bridge: Box::new(Claude(session)),
                attached: true,
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
    Ok(options)
}

fn detached() -> Attachment {
    Attachment {
        bridge: Box::new(Detached),
        attached: false,
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
struct Claude(Session);

impl Bridge for Claude {
    fn send(&mut self, prompt: &str) -> Result<(), BridgeError> {
        self.0.send(prompt).map_err(BridgeError::from)
    }

    fn answer(&mut self, id: &ToolCallId, decision: PermissionDecision) -> Result<(), BridgeError> {
        self.0.answer(id, decision).map_err(BridgeError::from)
    }

    fn set_mode(&mut self, mode: Mode) -> Result<(), BridgeError> {
        self.0.set_mode(mode).map_err(BridgeError::from)
    }

    fn set_model(&mut self, model: &str) -> Result<(), BridgeError> {
        self.0.set_model(model).map_err(BridgeError::from)
    }

    fn drain(&mut self) -> Vec<Event> {
        self.0.drain()
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

    /// The `claude` CLI's own settings on a machine that has none, which is
    /// every machine these tests run on.
    const NO_HOME: Option<OsString> = None;

    #[test]
    fn a_session_under_no_profile_is_attached_to_nothing() {
        let attachment = attach(Path::new("/repo"), None, &Attach::default(), NO_HOME)
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
}
