// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Driving the `claude` binary as a subprocess.
//!
//! The CLI is spawned once per session and kept alive with its standard input
//! open for as long as the session lasts. That is not an optimisation: given
//! `--input-format stream-json` the CLI waits about three seconds for a first
//! line and then goes on without one, so a session that closed stdin between
//! turns would get one turn and a dead subprocess.
//!
//! Two threads read the child, because a pipe that nobody drains fills up and
//! stops the writer: one turns standard output into events, one keeps whatever
//! the CLI writes to standard error so that a failure can be reported with the
//! CLI's own words rather than with an exit status.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use niobe_core::event::{Event, PermissionDecision, ToolCallId};

use crate::translate::Translator;

/// The binary this bridge drives, as looked up on `PATH`.
pub const BINARY: &str = "claude";

/// How long a closing session waits for the CLI to leave on its own once its
/// standard input is closed, before it is killed.
///
/// The CLI has a session of its own to write out. Long enough for that on a
/// loaded machine, short enough that quitting the shell stays instant.
const GOODBYE: Duration = Duration::from_millis(500);

/// What the CLI is told when the operator refuses a call.
///
/// The CLI hands it to the model as the refused call's result, where it is
/// otherwise indistinguishable from a tool that broke, so it names who
/// refused.
const REFUSED: &str = "The operator denied this call in Niobe.";

/// What the CLI is waiting on an answer to: the tool call it asked about, and
/// the id the answer is addressed to, with the arguments to approve.
///
/// Written by the thread that reads the CLI and read by whoever answers, so
/// it is shared: the reader records a prompt before it hands out the event
/// that announces it, which is what stops an answer arriving for a request
/// this side cannot yet address.
type Waiting = Arc<Mutex<BTreeMap<String, (String, serde_json::Value)>>>;

/// Calls refused here that the thread reading the CLI has not been told about
/// yet.
///
/// The refusal comes back from the CLI as an ordinary tool result with an
/// error on it, so the translator has to be told before that result arrives or
/// it reads the call as a tool that broke. Written before the answer goes out
/// and drained before every line, which is what puts it there first.
type Refusals = Arc<Mutex<Vec<ToolCallId>>>;

/// How the CLI decides whether a tool call may run.
///
/// Spelled as the CLI spells it on the command line; the names are the vendor's
/// and are passed through unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    /// Ask about anything not already allowed.
    Default,
    /// Edits to files in the working directory go through without asking.
    AcceptEdits,
    /// Plan first, change nothing.
    Plan,
}

impl PermissionMode {
    /// The value for `--permission-mode`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
        }
    }
}

/// What a session is spawned with.
#[derive(Debug, Clone)]
pub struct Options {
    /// The binary to run. The plain name is looked up on `PATH`.
    pub binary: PathBuf,
    /// The directory the CLI runs in, which is the repository it works on.
    pub cwd: PathBuf,
    /// Variables set for the child, on top of the ones Niobe was started with.
    /// Passed through exactly as the profile wrote them.
    pub env: BTreeMap<String, String>,
    /// Arguments appended after the ones this bridge passes, so that a profile
    /// can reach a flag Niobe does not model.
    pub args: Vec<String>,
    /// The profile the session runs under, for [`Event::SessionMeta`].
    pub profile: String,
    /// The model to ask for. `None` leaves the choice to the CLI.
    pub model: Option<String>,
    /// How tool calls are gated.
    pub permission_mode: PermissionMode,
    /// A session of the CLI's own to continue, rather than starting a new one.
    pub resume: Option<String>,
    /// Settings to run under, as the JSON document `--settings` takes or the
    /// path of a file holding one.
    pub settings: Option<String>,
    /// Whether permission prompts are asked over the stdio control channel.
    ///
    /// Off unless something is answering them: with it on, the CLI stops and
    /// waits for an answer to every gated call, and a session with nothing to
    /// answer waits for ever.
    pub ask_over_stdio: bool,
}

impl Options {
    /// A session in `cwd`, under `profile`, with everything else at its
    /// default.
    pub fn new(cwd: impl Into<PathBuf>, profile: impl Into<String>) -> Self {
        Self {
            binary: PathBuf::from(BINARY),
            cwd: cwd.into(),
            env: BTreeMap::new(),
            args: Vec::new(),
            profile: profile.into(),
            model: None,
            permission_mode: PermissionMode::Default,
            resume: None,
            settings: None,
            ask_over_stdio: false,
        }
    }

    /// The arguments the CLI is run with, in order.
    ///
    /// `--verbose` is not a preference: without it the CLI collapses a
    /// stream-json session down to its `result`, and everything this bridge
    /// reads — the messages, the tool calls, the per-message usage — is gone.
    /// `--include-partial-messages` is load-bearing for the same reason: the
    /// only token counts that reconcile with the turn are on its stream
    /// events.
    pub fn argv(&self) -> Vec<String> {
        let mut argv = vec![
            "-p".to_owned(),
            "--input-format".to_owned(),
            "stream-json".to_owned(),
            "--output-format".to_owned(),
            "stream-json".to_owned(),
            "--verbose".to_owned(),
            "--include-partial-messages".to_owned(),
            "--permission-mode".to_owned(),
            self.permission_mode.as_str().to_owned(),
        ];
        if self.ask_over_stdio {
            argv.push("--permission-prompt-tool".to_owned());
            argv.push("stdio".to_owned());
        }
        for (flag, value) in [
            ("--model", &self.model),
            ("--resume", &self.resume),
            ("--settings", &self.settings),
        ] {
            if let Some(value) = value {
                argv.push(flag.to_owned());
                argv.push(value.clone());
            }
        }
        argv.extend(self.args.iter().cloned());
        argv
    }
}

/// Why a session could not be started.
#[derive(Debug)]
pub enum SpawnError {
    /// There is no such binary on `PATH`.
    NotInstalled {
        /// The name that was looked up.
        binary: PathBuf,
    },
    /// The binary is there and could not be started.
    Failed {
        /// The name that was looked up.
        binary: PathBuf,
        /// What the operating system said.
        error: std::io::Error,
    },
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled { binary } => write!(
                f,
                "`{}` is not on PATH. Niobe drives the official CLI rather than \
                 replacing it, so install it and sign in with `{} /login` first; \
                 `niobe profiles` shows which backend this profile runs.",
                binary.display(),
                binary.display(),
            ),
            Self::Failed { binary, error } => {
                write!(f, "cannot start `{}`: {error}", binary.display())
            }
        }
    }
}

impl std::error::Error for SpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotInstalled { .. } => None,
            Self::Failed { error, .. } => Some(error),
        }
    }
}

/// A running `claude` session.
///
/// Dropping it closes the session: standard input is closed so the CLI can
/// write its own transcript out, and the process is killed if it has not left
/// by then.
#[derive(Debug)]
pub struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<Event>,
    waiting: Waiting,
    refusals: Refusals,
    stderr: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
    /// Whether the child's exit has already been turned into an event, so that
    /// a session that has ended says so once rather than on every drain.
    reported: bool,
}

impl Session {
    /// Spawns the CLI and starts reading it.
    pub fn spawn(options: &Options) -> Result<Self, SpawnError> {
        let mut command = Command::new(&options.binary);
        command
            .args(options.argv().iter().map(OsString::from))
            .current_dir(&options.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in &options.env {
            command.env(name, value);
        }

        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                SpawnError::NotInstalled {
                    binary: options.binary.clone(),
                }
            } else {
                SpawnError::Failed {
                    binary: options.binary.clone(),
                    error,
                }
            }
        })?;

        // The pipes were all asked for above, so a missing one is this
        // function's own bug rather than a state a caller can reach; taking
        // them with `take` and matching keeps the promise that nothing in a
        // library unwraps.
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            return Err(SpawnError::Failed {
                binary: options.binary.clone(),
                error: std::io::Error::other("the child was started without its pipes"),
            });
        };

        let (sender, events) = mpsc::channel();
        let mut translator = Translator::new(options.profile.clone());
        let waiting: Waiting = Arc::new(Mutex::new(BTreeMap::new()));
        let asked = Arc::clone(&waiting);
        let refusals: Refusals = Arc::new(Mutex::new(Vec::new()));
        let refused = Arc::clone(&refusals);
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(mut refused) = refused.lock() {
                    for id in refused.drain(..) {
                        translator.refused(&id);
                    }
                }
                let events = translator.line(&line);
                // Recorded before the events go out, never after: the shell
                // learns of a prompt by draining the events, and an answer to
                // one this side had not yet written down would be refused for
                // a request that is in fact waiting.
                if let Ok(mut waiting) = asked.lock() {
                    for ask in translator.take_asked() {
                        waiting.insert(ask.id.as_str().to_owned(), (ask.request_id, ask.input));
                    }
                }
                for event in events {
                    // The receiver is gone: the session was dropped, and there
                    // is no one left to tell.
                    if sender.send(event).is_err() {
                        return;
                    }
                }
            }
        });

        let kept = Arc::new(Mutex::new(String::new()));
        let writing = Arc::clone(&kept);
        let errors = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Ok(mut kept) = writing.lock() {
                    kept.push_str(&line);
                    kept.push('\n');
                }
            }
        });

        Ok(Self {
            child,
            stdin: Some(stdin),
            events,
            waiting,
            refusals,
            stderr: kept,
            readers: vec![reader, errors],
            reported: false,
        })
    }

    /// Sends one turn.
    ///
    /// The CLI reads turns as JSON lines on its standard input for as long as
    /// the session lasts, so this is a write and a flush and nothing else: the
    /// reply arrives through [`Session::drain`].
    pub fn send(&mut self, prompt: &str) -> std::io::Result<()> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(std::io::Error::other(
                "the session has ended; its standard input is closed",
            ));
        };
        let line = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": prompt },
        });
        writeln!(stdin, "{line}")?;
        stdin.flush()
    }

    /// Answers a permission prompt the CLI is waiting on.
    ///
    /// The CLI stops the turn on a gated call and goes on the moment an answer
    /// for its `request_id` arrives, so this is a write and a flush and
    /// nothing else. An answer for a call the CLI is not waiting on is refused
    /// rather than written: the protocol would ignore it, and a session that
    /// silently dropped a decision would look as though the call had been
    /// allowed.
    ///
    /// An approval carries the arguments back as they came. The protocol sends
    /// the approved arguments rather than a bare yes, and Niobe approves a
    /// call without ever rewriting one, so what goes back is what was shown.
    pub fn answer(&mut self, id: &ToolCallId, decision: PermissionDecision) -> std::io::Result<()> {
        let asked = self
            .waiting
            .lock()
            .ok()
            .and_then(|mut waiting| waiting.remove(id.as_str()));
        let Some((request_id, input)) = asked else {
            return Err(std::io::Error::other(format!(
                "the `claude` session is not waiting on a decision about tool call `{id}`"
            )));
        };

        // Before the answer goes out, never after: the refusal comes back as
        // an ordinary tool result with an error on it, and the thread reading
        // the CLI has to know which it is before it reads that result.
        if !decision.allowed()
            && let Ok(mut refusals) = self.refusals.lock()
        {
            refusals.push(id.clone());
        }

        let Some(stdin) = self.stdin.as_mut() else {
            return Err(std::io::Error::other(
                "the session has ended; its standard input is closed",
            ));
        };
        writeln!(stdin, "{}", control_response(&request_id, &input, decision))?;
        stdin.flush()
    }

    /// Everything the CLI has produced since the last call.
    ///
    /// Never blocks. A session whose subprocess has gone reports that once,
    /// with whatever the CLI wrote to standard error, and then nothing.
    pub fn drain(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty) => return events,
                Err(TryRecvError::Disconnected) => {
                    if let Some(ended) = self.ended() {
                        events.push(ended);
                    }
                    return events;
                }
            }
        }
    }

    /// The event a session's subprocess leaving produces, the once.
    fn ended(&mut self) -> Option<Event> {
        if self.reported {
            return None;
        }
        self.reported = true;
        // Standard input is closed here rather than on drop: the CLI has
        // already gone, and holding the pipe would keep a descriptor for a
        // process that will never read it.
        self.stdin = None;

        let status = match self.child.wait() {
            Ok(status) => status,
            Err(error) => {
                return Some(Event::Error {
                    message: format!("the `claude` session ended and could not be reaped: {error}"),
                    fatal: true,
                });
            }
        };
        let said = self
            .stderr
            .lock()
            .map(|kept| kept.trim().to_owned())
            .unwrap_or_default();

        if status.success() && said.is_empty() {
            return Some(Event::Notice {
                message: "the `claude` session ended.".to_owned(),
            });
        }
        Some(Event::Error {
            message: ended_because(status, &said),
            fatal: true,
        })
    }
}

/// The line that answers one permission prompt.
///
/// Written as its own function because it is the half of answering that can be
/// checked without a subprocess: what reaches the CLI's standard input is the
/// whole of the protocol on this side.
fn control_response(
    request_id: &str,
    input: &serde_json::Value,
    decision: PermissionDecision,
) -> serde_json::Value {
    let response = match decision.allowed() {
        true => serde_json::json!({ "behavior": "allow", "updatedInput": input }),
        false => serde_json::json!({ "behavior": "deny", "message": REFUSED }),
    };
    serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response,
        },
    })
}

/// What to say about a session whose CLI left.
///
/// The CLI's own words come first and whole: a guess about why it stopped is
/// worth less than what it said, and it is the only thing that stays true when
/// the vendor changes its messages. The sign-in hint is added on top of them,
/// never instead, and only when they read like an authentication failure —
/// which is a guess about a string, so the operator sees both.
fn ended_because(status: std::process::ExitStatus, said: &str) -> String {
    let mut message = match said.is_empty() {
        true => format!("the `claude` session ended with {status} and said nothing."),
        false => format!("the `claude` session ended with {status}: {said}"),
    };
    if looks_logged_out(said) {
        message.push_str(
            "\nThat reads like the CLI is not signed in. Niobe never touches its credentials: \
             run `claude /login` yourself, then start the session again.",
        );
    }
    message
}

/// Whether what the CLI said reads like it is not signed in.
fn looks_logged_out(said: &str) -> bool {
    let said = said.to_lowercase();
    [
        "log in",
        "login",
        "sign in",
        "authenticat",
        "credential",
        "unauthorized",
        "oauth",
    ]
    .iter()
    .any(|needle| said.contains(needle))
}

impl Drop for Session {
    fn drop(&mut self) {
        // Closing standard input is how the CLI is asked to stop: it finishes
        // the turn it is on and writes its session out. Killing it first would
        // lose that, and the CLI's own transcript is what `--resume` reads.
        self.stdin = None;

        let deadline = Instant::now() + GOODBYE;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
                Err(_) => break,
            }
        }

        // The threads end when their pipes close, which the child leaving
        // does. Joined so that no thread outlives the session it reads.
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Options {
        Options::new("/repo", "max")
    }

    #[test]
    fn the_spawn_line_asks_for_every_message_the_bridge_reads() {
        let argv = options().argv();

        assert_eq!(
            argv,
            [
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--permission-mode",
                "default",
            ]
        );
    }

    #[test]
    fn a_profiles_own_arguments_come_last_so_they_can_reach_a_flag_niobe_does_not_model() {
        let mut options = options();
        options.model = Some("claude-sonnet-5".to_owned());
        options.args = vec!["--add-dir".to_owned(), "/other".to_owned()];

        let argv = options.argv();

        let model = argv.iter().position(|a| a == "--model").expect("the model");
        assert_eq!(argv[model + 1], "claude-sonnet-5");
        assert_eq!(&argv[argv.len() - 2..], ["--add-dir", "/other"]);
    }

    #[test]
    fn a_resumed_session_and_its_settings_are_passed_through() {
        let mut options = options();
        options.resume = Some("1a3e0a35".to_owned());
        options.settings = Some(r#"{"env":{"A":"1"}}"#.to_owned());

        let argv = options.argv();

        let resume = argv.iter().position(|a| a == "--resume").expect("resume");
        assert_eq!(argv[resume + 1], "1a3e0a35");
        let settings = argv
            .iter()
            .position(|a| a == "--settings")
            .expect("settings");
        assert_eq!(argv[settings + 1], r#"{"env":{"A":"1"}}"#);
    }

    #[test]
    fn nothing_asks_over_stdio_unless_something_is_answering() {
        assert!(
            !options()
                .argv()
                .iter()
                .any(|a| a == "--permission-prompt-tool")
        );

        let mut answering = options();
        answering.ask_over_stdio = true;
        let argv = answering.argv();

        let flag = argv
            .iter()
            .position(|a| a == "--permission-prompt-tool")
            .expect("the prompt tool");
        assert_eq!(argv[flag + 1], "stdio");
    }

    #[test]
    fn a_binary_that_is_not_installed_says_so_and_says_what_to_do() {
        let mut options = options();
        options.binary = PathBuf::from("claude-that-is-not-installed");
        options.cwd = std::env::temp_dir();

        let error = Session::spawn(&options).expect_err("there is no such binary");

        assert!(matches!(error, SpawnError::NotInstalled { .. }));
        let said = error.to_string();
        assert!(said.contains("is not on PATH"), "{said}");
        assert!(said.contains("/login"), "{said}");
    }

    #[test]
    fn an_approval_sends_back_the_arguments_that_were_shown() {
        let input = serde_json::json!({ "file_path": "/repo/notes.txt" });

        let allowed = control_response("c1", &input, PermissionDecision::Allow);
        let always = control_response("c1", &input, PermissionDecision::AllowAlways);

        assert_eq!(
            allowed,
            serde_json::json!({
                "type": "control_response",
                "response": {
                    "subtype": "success",
                    "request_id": "c1",
                    "response": { "behavior": "allow", "updatedInput": input },
                },
            })
        );
        // A rule stored beside the answer changes what Niobe asks next time,
        // never what this call is allowed to do.
        assert_eq!(always, allowed);
    }

    #[test]
    fn a_refusal_says_who_refused_because_the_model_is_told_it_as_an_error() {
        let refused = control_response(
            "c2",
            &serde_json::json!({ "command": "rm -rf build" }),
            PermissionDecision::Deny,
        );

        let response = &refused["response"]["response"];
        assert_eq!(response["behavior"], "deny");
        assert_eq!(response["message"], REFUSED);
        assert!(
            response.get("updatedInput").is_none(),
            "a refusal sent arguments to run: {refused}"
        );
    }

    #[test]
    fn an_answer_to_a_call_the_cli_never_asked_about_is_refused_rather_than_written() {
        // `/bin/echo` stands in for the CLI: it takes the arguments, prints
        // them and leaves. Nothing about this path talks to the process — the
        // point is that an answer with no request to address is refused before
        // anything is written, so a decision cannot be dropped in silence.
        let mut options = options();
        options.binary = PathBuf::from("/bin/echo");
        options.cwd = std::env::temp_dir();
        let mut session = Session::spawn(&options).expect("`/bin/echo` is on every unix");

        let error = session
            .answer(&ToolCallId::new("toolu_1"), PermissionDecision::Allow)
            .expect_err("the CLI was never asked about this call");

        let said = error.to_string();
        assert!(said.contains("not waiting on a decision"), "{said}");
        assert!(said.contains("toolu_1"), "{said}");
    }

    #[test]
    fn a_session_that_says_nothing_about_signing_in_is_reported_as_it_stands() {
        assert!(!looks_logged_out(
            "Error: EACCES: permission denied, open '/etc/hosts'"
        ));
        assert!(looks_logged_out("Invalid API key · Please run /login"));
        assert!(looks_logged_out("OAuth token has expired"));
    }
}
