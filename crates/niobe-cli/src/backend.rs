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
use niobe_core::event::{Backend, Event};
use niobe_tui::bridge::{Bridge, BridgeError, Detached};

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
/// `resume` is the id the backend calls an earlier session by, where one was
/// recorded: Niobe's session id names the recording, and only the CLI's own id
/// can hand its transcript back.
pub fn attach(
    root: &Path,
    profile: Option<&Selected<'_>>,
    resume: Option<String>,
) -> Result<Attachment, String> {
    let Some(selected) = profile else {
        return Ok(detached());
    };

    match selected.profile.backend() {
        Backend::Claude => {
            let mut options = Options::new(root, selected.name);
            options.env = selected.profile.env().clone();
            options.args = selected.profile.args().to_vec();
            options.resume = resume;
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
        let attachment = attach(Path::new("/repo"), None, None).expect("nothing to start");

        assert!(!attachment.attached());
    }

    #[test]
    fn a_backend_with_no_bridge_leaves_the_session_attached_to_nothing() {
        let config = config("[profiles.codex]\nbackend = \"codex\"\n");
        let selected = config
            .select(Some("codex"))
            .expect("the profile is defined")
            .expect("a profile was selected");

        let attachment =
            attach(Path::new("/repo"), Some(&selected), None).expect("nothing to start");

        assert!(!attachment.attached());
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
