// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Attaching a session to the backend its profile runs.
//!
//! This is the only module that names a bridge. The shell is handed something
//! that implements [`Bridge`] and never learns which one, which is what keeps
//! a second backend a file here rather than a change everywhere.

use std::path::Path;

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
) -> Result<Attachment, String> {
    let Some(selected) = profile else {
        return Ok(detached());
    };

    match selected.profile.backend() {
        Backend::Claude => {
            let session =
                Session::spawn(&claude_options(root, selected, with)).map_err(describe)?;
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

/// What the `claude` bridge is spawned with under `profile`.
///
/// Written apart from the spawn so that what a profile turns into can be
/// checked without starting a real session on the operator's own
/// subscription.
fn claude_options(root: &Path, profile: &Selected<'_>, with: &Attach) -> Options {
    let mut options = Options::new(root, profile.name);
    options.env = profile.profile.env().clone();
    options.args = profile.profile.args().to_vec();
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
    options
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

    #[test]
    fn a_session_under_no_profile_is_attached_to_nothing() {
        let attachment =
            attach(Path::new("/repo"), None, &Attach::default()).expect("nothing to start");

        assert!(!attachment.attached());
    }

    #[test]
    fn a_backend_with_no_bridge_leaves_the_session_attached_to_nothing() {
        let config = config("[profiles.codex]\nbackend = \"codex\"\n");
        let selected = config
            .select(Some("codex"))
            .expect("the profile is defined")
            .expect("a profile was selected");

        let attachment = attach(Path::new("/repo"), Some(&selected), &Attach::default())
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
        );

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
        );

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

        let options = claude_options(Path::new("/repo"), &selected, &Attach::default());

        // The shell cycles from `ask` when nothing has reported a mode, so a
        // session that starts anywhere else would move on the first keypress.
        assert_eq!(options.mode, Mode::Ask);
        assert_eq!(options.budget_usd, None);
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
