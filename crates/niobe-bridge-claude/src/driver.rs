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
//! What goes to the CLI is written by a thread of its own, from a queue:
//! a CLI busy elsewhere does not read its standard input, and a line longer
//! than the pipe holds — a pasted log — would otherwise stop the thread that
//! draws the screen until the CLI came back to it. A write that is still
//! waiting after [`STALLED`] is said to be, and one that fails is reported,
//! both as events like anything else the session has to say.
//!
//! Two threads read the child, because a pipe that nobody drains fills up and
//! stops the writer: one turns standard output into events, one keeps whatever
//! the CLI writes to standard error so that a failure can be reported with the
//! CLI's own words rather than with an exit status. Both read bytes, not text:
//! a line that is not UTF-8 is read with the bytes replaced, because a reader
//! that stopped on one would lose the rest of the session and could leave the
//! CLI blocked on a pipe nobody drains.
//!
//! The CLI leads a process group of its own, and everything it starts — the
//! shell a tool call runs in, an MCP server — is in it unless it leaves. That
//! is what a closing session ends, rather than the CLI alone: something it
//! started holds the pipes it inherited, so the readers would never see them
//! close, and nobody would be left to see it end.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use niobe_core::event::{Billing, Event, Mode, PermissionDecision, ToolCallId};
use niobe_core::image::Image;

use crate::base64;
use crate::spilled;
use crate::translate::{Spent, Translator};

/// The binary this bridge drives, as looked up on `PATH`.
pub const BINARY: &str = "claude";

/// How long a closing session waits for the CLI to leave on its own once its
/// standard input is closed, before it is killed.
///
/// The CLI has a session of its own to write out. Long enough for that on a
/// loaded machine, short enough that quitting the shell stays instant.
const GOODBYE: Duration = Duration::from_millis(500);

/// How much of what the CLI writes to standard error is kept: the end of it,
/// since a CLI that fails says why last.
///
/// Everything it wrote reaches the screen and the store as one error, so the
/// bound is what keeps a CLI that floods the pipe from burying its exit
/// status under megabytes nobody reads, and from leaving the screen a message
/// too large to lay out on every frame.
const SAID: usize = 8 * 1024;

/// How long what the CLI started is given to end on SIGTERM once the CLI has
/// gone, before its group is killed.
///
/// A process that listens ends within a few milliseconds, so only one that
/// does not is waited for this long; the operator who quit is waiting too.
const LEFTOVERS: Duration = Duration::from_millis(250);

/// How long a closing session waits for its reader threads once the CLI's
/// group has gone, before it leaves them.
///
/// Only something that left the group — a daemon that started a session of
/// its own — can still hold a pipe by then. A thread left reading it ends
/// with the process, and a quit is not held up for as long as that runs.
const LET_GO: Duration = Duration::from_millis(100);

/// How long a line can wait on the CLI to read it before the session says so.
///
/// The CLI reads its standard input while a turn runs, so a line that has not
/// gone in by then is waiting on a CLI that has stopped reading, not on one
/// that is busy; saying so sooner would fire on a slow machine.
const STALLED: Duration = Duration::from_secs(5);

/// What the CLI is told when the operator refuses a call.
///
/// The CLI hands it to the model as the refused call's result, where it is
/// otherwise indistinguishable from a tool that broke, so it names who
/// refused.
const REFUSED: &str = "The operator denied this call in Niobe.";

/// What goes in front of the operator's own words when they answered a prompt
/// by writing rather than choosing. The words follow whole, so the model reads
/// what was written and who wrote it, and nothing Niobe made up between them.
const REFUSED_SAYING: &str = "The operator denied this call in Niobe and answered instead: ";

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

/// The line being written to the CLI's standard input, what it is, and when
/// the writing started; nothing while the writer is waiting for a line.
///
/// Written by the thread that writes and read by the drain, which is how a
/// write the CLI is not reading becomes something the operator is told.
type Writing = Arc<Mutex<Option<(Instant, &'static str)>>>;

/// One line for the CLI's standard input, and what it is, for a report that
/// it could not be sent.
#[derive(Debug)]
struct Line {
    text: String,
    what: &'static str,
}

/// How the CLI spells a mode, on the command line and over the control
/// channel.
///
/// The CLI takes `acceptEdits`, `auto`, `bypassPermissions`, `default`,
/// `dontAsk` and `plan`, and refuses anything else with an error naming the
/// list. Niobe models three of them and sends nothing outside this function,
/// so a mode the shell offers is always one the CLI takes.
fn spelt(mode: Mode) -> &'static str {
    match mode {
        Mode::Plan => "plan",
        Mode::Ask => "default",
        Mode::Auto => "auto",
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
    /// How the profile says the session is billed. `None` leaves it to what
    /// the CLI's stream shows.
    pub billing: Option<Billing>,
    /// The model to ask for. `None` leaves the choice to the CLI.
    pub model: Option<String>,
    /// How tool calls are gated.
    pub mode: Mode,
    /// The most the CLI may spend on this session, in USD. `None` leaves it
    /// unlimited. The CLI checks it between turns, so a session can finish
    /// above the figure by the cost of the turn that crossed it.
    pub budget_usd: Option<f64>,
    /// A session of the CLI's own to continue, rather than starting a new one.
    pub resume: Option<String>,
    /// What the CLI had recorded the resumed session as spending, which its
    /// first turn reports on top of; see [`Spent`]. Empty for a new session.
    pub spent: Spent,
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
            billing: None,
            model: None,
            mode: Mode::Ask,
            budget_usd: None,
            resume: None,
            spent: Spent::default(),
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
            spelt(self.mode).to_owned(),
        ];
        if let Some(budget) = self.budget_usd {
            argv.push("--max-budget-usd".to_owned());
            argv.push(budget.to_string());
        }
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
    /// The directory the session is to run in is not there — deleted, as a
    /// worktree removed under a terminal still in it is. Checked apart from
    /// the binary because the operating system reports both as the same
    /// "not found".
    NoDirectory {
        /// The directory the CLI was to run in.
        directory: PathBuf,
    },
    /// The binary is there and could not be started.
    Failed {
        /// The file that was found for the name looked up, where `PATH` found
        /// one — which of several on `PATH` is what the operator has to know
        /// to fix it — and otherwise the name itself.
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
            Self::NoDirectory { directory } => write!(
                f,
                "the directory the session was to run in, {}, is not there; start niobe \
                 again from one that is",
                directory.display()
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
            Self::NotInstalled { .. } | Self::NoDirectory { .. } => None,
            Self::Failed { error, .. } => Some(error),
        }
    }
}

/// The file `binary` names as a command would find it: itself where it has a
/// directory in it, and otherwise the first file of that name in a directory
/// of `path` — the child's own `PATH` where the profile sets one, as the
/// spawn looks it up — or of this process's.
fn found_on_path(binary: &Path, path: Option<&String>) -> Option<PathBuf> {
    if binary.components().count() > 1 {
        return Some(binary.to_path_buf());
    }
    let path = match path {
        Some(path) => OsString::from(path),
        None => std::env::var_os("PATH")?,
    };
    std::env::split_paths(&path)
        .map(|dir| dir.join(binary))
        .find(|candidate| candidate.is_file())
}

/// A running `claude` session.
///
/// Dropping it closes the session: standard input is closed so the CLI can
/// write its own transcript out, the process is killed if it has not left
/// by then, and then so is everything it started that has not left either.
/// A CLI that leaves on its own has what it started ended as it is found
/// gone, since that would otherwise hold its output open and its end unsaid.
#[derive(Debug)]
pub struct Session {
    child: Child,
    /// The queue the thread writing the CLI's standard input takes lines
    /// from. Dropping it closes standard input once what is queued has gone
    /// in, which is how the CLI is asked to leave.
    stdin: Option<Sender<Line>>,
    /// What the thread writing standard input could not send, as events.
    unsent: Receiver<Event>,
    writing: Writing,
    /// When the write that was last said to be stalled started, so a stall is
    /// said once rather than on every drain.
    said_stalled: Option<Instant>,
    events: Receiver<Event>,
    waiting: Waiting,
    refusals: Refusals,
    stderr: Arc<Mutex<Tail>>,
    /// The threads reading standard output and standard error, in that
    /// order, and the one writing standard input.
    threads: Vec<JoinHandle<()>>,
    /// How many requests this side has made, which is what the next one is
    /// addressed by. The CLI numbers its own requests, so Niobe's carry a
    /// prefix of their own: two requests answered by the same id would have
    /// each other's answers.
    control_requests: u64,
    /// Whether the child's exit has already been turned into an event, so that
    /// a session that has ended says so once rather than on every drain.
    reported: bool,
    /// Whether the process group the CLI led has been ended, which is done
    /// once, as soon as the CLI has been reaped. Once the group is empty its
    /// id is free for any process to lead a group under, and a signal sent to
    /// it later — at the quit, hours on — could reach a stranger's group.
    group_ended: bool,
    /// When the CLI's standard output was first found closed. The process
    /// leaving closes it a moment before it can be reaped, so the end is not
    /// reported until one or the other — the process gone, or [`GOODBYE`]
    /// passed with it still there — and never by waiting on it.
    output_closed: Option<Instant>,
    /// When the CLI was first found to have left while its standard output
    /// was still open, which is what something it started that holds the
    /// pipe keeps it: EOF does not come while that runs, so the end is not
    /// waited for there.
    exited: Option<Instant>,
    /// How long a write waits before it is said to be stalled: [`STALLED`],
    /// but for tests that cannot wait that long.
    stalled_after: Duration,
}

impl Session {
    /// Spawns the CLI and starts reading it.
    pub fn spawn(options: &Options) -> Result<Self, SpawnError> {
        if !options.cwd.is_dir() {
            return Err(SpawnError::NoDirectory {
                directory: options.cwd.clone(),
            });
        }
        let mut command = Command::new(&options.binary);
        command
            .args(options.argv().iter().map(OsString::from))
            .current_dir(&options.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
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
                    binary: found_on_path(&options.binary, options.env.get("PATH"))
                        .unwrap_or_else(|| options.binary.clone()),
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
        let mut translator = Translator::new(options.profile.clone())
            .resuming(&options.spent)
            .in_dir(options.cwd.clone())
            .reading_spilled_with(spilled::read);
        if let Some(billing) = options.billing {
            translator = translator.billed_as(billing);
        }
        let waiting: Waiting = Arc::new(Mutex::new(BTreeMap::new()));
        let asked = Arc::clone(&waiting);
        let refusals: Refusals = Arc::new(Mutex::new(Vec::new()));
        let refused = Arc::clone(&refusals);
        let reader = std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            while let Some((line, decoded)) = next_line(&mut stdout) {
                if line.trim().is_empty() {
                    continue;
                }
                if !decoded && sender.send(undecodable()).is_err() {
                    return;
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
                // a request that is in fact waiting. A prompt the CLI withdrew
                // is let go of here too, so that no answer is written to a
                // request it cancelled and its end is not read as one that
                // left a question open.
                if let Ok(mut waiting) = asked.lock() {
                    for ask in translator.take_asked() {
                        waiting.insert(ask.id.as_str().to_owned(), (ask.request_id, ask.input));
                    }
                    for id in translator.take_withdrawn() {
                        waiting.remove(id.as_str());
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

        let kept = Arc::new(Mutex::new(Tail::default()));
        let tail = Arc::clone(&kept);
        let errors = std::thread::spawn(move || read_tail(stderr, &tail));

        let (lines, queued) = mpsc::channel();
        let (failures, unsent) = mpsc::channel();
        let writing: Writing = Arc::new(Mutex::new(None));
        let progress = Arc::clone(&writing);
        let writer = std::thread::spawn(move || write_lines(stdin, &queued, &progress, &failures));

        let mut session = Self {
            child,
            stdin: Some(lines),
            unsent,
            writing,
            said_stalled: None,
            events,
            waiting,
            refusals,
            stderr: kept,
            threads: vec![reader, errors, writer],
            control_requests: 0,
            reported: false,
            group_ended: false,
            output_closed: None,
            exited: None,
            stalled_after: STALLED,
        };
        // The writer has only just been started, so the queue cannot be
        // closed yet; a CLI that has already left is reported by the next
        // drain, in its own words.
        let _ = session.initialize();
        Ok(session)
    }

    /// Asks the CLI what it offers, which it answers with its slash commands
    /// among other things.
    ///
    /// The answer is the only place the CLI lists its commands as the session
    /// starts: `commands_changed` is sent only when the list changes after
    /// that, and a session whose list never changes is never sent one. The
    /// CLI takes the request once, before anything else, so it goes out here.
    fn initialize(&mut self) -> std::io::Result<()> {
        let id = self.next_request_id();
        self.ask(&control_request(
            &id,
            serde_json::json!({ "subtype": "initialize" }),
        ))
    }

    /// Sends one turn, with the images the operator attached to it.
    ///
    /// The CLI reads turns as JSON lines on its standard input for as long as
    /// the session lasts, so this queues one line and nothing else: it never
    /// waits on the CLI, and the reply — or word that the turn could not be
    /// sent — arrives through [`Session::drain`].
    pub fn send(&mut self, prompt: &str, images: &[Image]) -> std::io::Result<()> {
        self.queue(turn_line(prompt, images).to_string(), "turn")
    }

    /// Answers a permission prompt the CLI is waiting on.
    ///
    /// The CLI stops the turn on a gated call and goes on the moment an answer
    /// for its `request_id` arrives, so this queues one line and nothing
    /// else. An answer for a call the CLI is not waiting on is refused
    /// rather than written: the protocol would ignore it, and a session that
    /// silently dropped a decision would look as though the call had been
    /// allowed.
    ///
    /// An approval carries the arguments back as they came, in `updatedInput`.
    /// Claude Code 2.1.278 was recorded taking an approval without that field
    /// as well, so it is not what makes the call go ahead; it is sent because
    /// the field is where the protocol puts the arguments that were approved,
    /// and Niobe approves a call without ever rewriting one, so what goes back
    /// is what was shown.
    ///
    /// `message` is what the operator wrote instead of choosing an answer. It
    /// goes out with a refusal, which is the one place the protocol carries
    /// words back to the model about a call it asked to make; an approval has
    /// nowhere to put it.
    pub fn answer(
        &mut self,
        id: &ToolCallId,
        decision: PermissionDecision,
        message: Option<&str>,
    ) -> std::io::Result<()> {
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

        self.queue(
            control_response(&request_id, &input, decision, message).to_string(),
            "answer",
        )
    }

    /// Asks the CLI to gate tool calls a different way, from now on.
    ///
    /// The CLI applies it to the turn after the request, and answers on the
    /// control channel; a refusal comes back as a `control_response` the
    /// reading thread turns into a visible entry, so nothing here waits for
    /// one. A mode the CLI would refuse cannot be reached: [`spelt`] maps the
    /// three modes Niobe models onto spellings the CLI takes.
    pub fn set_mode(&mut self, mode: Mode) -> std::io::Result<()> {
        let id = self.next_request_id();
        self.ask(&control_request(
            &id,
            serde_json::json!({ "subtype": "set_permission_mode", "mode": spelt(mode) }),
        ))
    }

    /// Asks the CLI to answer with a different model from its next turn.
    ///
    /// The conversation is kept: this is the running session being told to
    /// change, not a new one, so nothing of what has been said is lost. The
    /// CLI resolves the name — an alias such as `haiku`, or a full model id —
    /// and announces what it ended up on with the next message it starts.
    pub fn set_model(&mut self, model: &str) -> std::io::Result<()> {
        let id = self.next_request_id();
        self.ask(&control_request(
            &id,
            serde_json::json!({ "subtype": "set_model", "model": model }),
        ))
    }

    /// Asks the CLI to stop the turn it is running.
    ///
    /// The session and its conversation are kept, as they are when Esc stops
    /// a turn in the CLI's own interface. Recorded from 2.1.285 in
    /// `tests/fixtures/interrupted.jsonl`: the CLI answers the request, cuts
    /// off the call running or the reply being written, and closes the turn
    /// with a `result` whose `terminal_reason` says it was aborted.
    pub fn interrupt(&mut self) -> std::io::Result<()> {
        let id = self.next_request_id();
        self.ask(&control_request(
            &id,
            serde_json::json!({ "subtype": "interrupt" }),
        ))
    }

    /// Queues one control request for the CLI's standard input.
    fn ask(&mut self, request: &serde_json::Value) -> std::io::Result<()> {
        self.queue(request.to_string(), "request")
    }

    /// Hands one line to the thread writing the CLI's standard input.
    ///
    /// Refused only where nothing can be written any more: the session has
    /// ended, or a write has already failed and the writer has gone, which
    /// the drain reports.
    fn queue(&mut self, text: String, what: &'static str) -> std::io::Result<()> {
        let closed =
            || std::io::Error::other("the session has ended; its standard input is closed");
        let Some(stdin) = self.stdin.as_ref() else {
            return Err(closed());
        };
        stdin.send(Line { text, what }).map_err(|_| closed())
    }

    /// The id the next request this side makes is addressed by.
    fn next_request_id(&mut self) -> String {
        self.control_requests = self.control_requests.saturating_add(1);
        format!("niobe-{}", self.control_requests)
    }

    /// The process group the CLI leads, which is also where everything it
    /// starts runs unless that moves itself out. Ended with the session; the
    /// number is for what must end it when the session is not there to.
    pub fn process_group(&self) -> u32 {
        self.child.id()
    }

    /// Everything the CLI has produced since the last call.
    ///
    /// Never blocks: it runs on the thread that draws the screen. A session
    /// whose subprocess has gone reports that once, with whatever the CLI
    /// wrote to standard error, and then nothing. One whose subprocess closed
    /// its standard output and stayed is stopped, and reported as a failure.
    /// One whose subprocess left while something it started holds its
    /// standard output open has that ended, and is reported like any other.
    pub fn drain(&mut self) -> Vec<Event> {
        let mut events: Vec<Event> = self.unsent.try_iter().collect();
        events.extend(self.stalled());
        loop {
            match self.events.try_recv() {
                // Only a pipe held by something that left the CLI's group
                // can still deliver a line after the end was reported, and
                // it is not the CLI's.
                Ok(_) if self.reported => {}
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty) => {
                    if let Some(ended) = self.left_holding_output() {
                        events.push(ended);
                    }
                    return events;
                }
                Err(TryRecvError::Disconnected) => {
                    if let Some(ended) = self.ended() {
                        events.push(ended);
                    }
                    return events;
                }
            }
        }
    }

    /// Word that the line being written has waited on the CLI for longer
    /// than [`Session::stalled_after`], once for each line that has.
    ///
    /// Not an end: the line stays queued and goes in when the CLI reads it,
    /// or is reported unsent when the CLI leaves without reading it.
    fn stalled(&mut self) -> Option<Event> {
        let (since, what) = (*self.writing.lock().ok()?)?;
        if self.reported || self.said_stalled == Some(since) || since.elapsed() < self.stalled_after
        {
            return None;
        }
        self.said_stalled = Some(since);
        Some(Event::Error {
            message: format!(
                "the `claude` CLI has not read the {what} sent {}s ago; it goes in when the CLI reads it",
                since.elapsed().as_secs()
            ),
            fatal: false,
        })
    }

    /// The event a session's subprocess leaving produces, the once, or
    /// nothing yet while it may still be on its way out.
    fn ended(&mut self) -> Option<Event> {
        if self.reported {
            return None;
        }
        // Standard input is closed here rather than on drop: nothing the CLI
        // says can be read any more, and closing it is how the CLI is asked to
        // leave.
        self.stdin = None;
        let closed = *self.output_closed.get_or_insert_with(Instant::now);

        let status = match self.child.try_wait() {
            // The CLI can leave with the end of what it said still in the
            // pipe, and that end is where it says why. Reported without it,
            // a CLI that wrote a lot would read as one that said nothing.
            Ok(Some(_)) if !self.heard_out() && closed.elapsed() < GOODBYE => return None,
            Ok(Some(status)) => status,
            Ok(None) if closed.elapsed() < GOODBYE => return None,
            Ok(None) => {
                self.reported = true;
                self.unanswered();
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.end_the_group();
                return Some(Event::Error {
                    message: kept_running(&self.said()),
                    fatal: true,
                });
            }
            Err(error) => {
                self.reported = true;
                self.unanswered();
                return Some(Event::Error {
                    message: format!("the `claude` session ended and could not be reaped: {error}"),
                    fatal: true,
                });
            }
        };
        self.reported = true;
        self.end_the_group();
        let said = self.said();
        let unanswered = self.unanswered();

        if !status.success() || !said.is_empty() {
            return Some(Event::Error {
                message: ended_because(status, &said),
                fatal: true,
            });
        }
        // A clean exit is a clean end only when nothing was left asking: the
        // turn a prompt stopped did not finish, and reading the end as a clean
        // one would leave the prompt on screen asking for an answer nothing
        // can take.
        match unanswered.is_empty() {
            true => Some(Event::Notice {
                message: "the `claude` session ended.".to_owned(),
            }),
            false => Some(Event::Error {
                message: ended_asking(&unanswered),
                fatal: true,
            }),
        }
    }

    /// The end of a CLI that has left while its standard output is still
    /// open, or nothing while it is still running or its end may still come
    /// the ordinary way.
    ///
    /// Something the CLI started — a command a tool call left in the
    /// background, an MCP server — holds its pipes, and they do not close
    /// while that runs. It is asked to end, which closes them, and the end is
    /// then reported as any other once what was in the pipe has been read.
    /// Only what left the CLI's group can hold them past [`GOODBYE`]; the end
    /// is reported then without waiting on it, since everything the CLI wrote
    /// before it left has been read by that time.
    fn left_holding_output(&mut self) -> Option<Event> {
        if self.reported {
            return None;
        }
        let exited = match self.exited {
            Some(exited) => exited,
            None => {
                if !matches!(self.child.try_wait(), Ok(Some(_))) {
                    return None;
                }
                ask_group_to_end(self.child.id());
                *self.exited.insert(Instant::now())
            }
        };
        if exited.elapsed() >= LEFTOVERS {
            kill_group(self.child.id());
        }
        if exited.elapsed() < GOODBYE {
            return None;
        }
        self.output_closed.get_or_insert(exited);
        self.ended()
    }

    /// Ends what is left of the CLI's process group, the once: see
    /// [`Session::group_ended`]. Called only once the CLI has been reaped.
    fn end_the_group(&mut self) {
        if !self.group_ended {
            self.group_ended = true;
            end_group(self.child.id());
        }
    }

    /// The tool calls the CLI was still waiting on an answer about, which are
    /// forgotten here: a session that has ended takes no answer, and one kept
    /// would be refused only when written to a closed pipe.
    fn unanswered(&mut self) -> Vec<String> {
        self.waiting
            .lock()
            .map(|mut waiting| std::mem::take(&mut *waiting).into_keys().collect())
            .unwrap_or_default()
    }

    /// Whether standard error has been read to its end, which is when
    /// everything the CLI said is in [`Session::said`].
    fn heard_out(&self) -> bool {
        self.threads.get(1).is_none_or(JoinHandle::is_finished)
    }

    /// The end of what the CLI has written to standard error so far, led by
    /// how much of it was left out where that is not all of it.
    fn said(&self) -> String {
        self.stderr
            .lock()
            .map(|tail| tail.said())
            .unwrap_or_default()
    }
}

/// Writes each queued line to the CLI's standard input, in order, until the
/// queue is closed or a write fails.
///
/// A failed write is reported, with how many lines were queued behind it,
/// and ends the thread: the pipe does not reopen, and the session learns
/// from the queue refusing its next line that nothing more can be sent.
fn write_lines(
    mut stdin: ChildStdin,
    queued: &Receiver<Line>,
    writing: &Mutex<Option<(Instant, &'static str)>>,
    failures: &Sender<Event>,
) {
    for Line { mut text, what } in queued {
        text.push('\n');
        if let Ok(mut writing) = writing.lock() {
            *writing = Some((Instant::now(), what));
        }
        let written = stdin
            .write_all(text.as_bytes())
            .and_then(|()| stdin.flush());
        if let Ok(mut writing) = writing.lock() {
            *writing = None;
        }
        if let Err(error) = written {
            let behind = queued.try_iter().count();
            let _ = failures.send(Event::Error {
                message: not_sent(what, &error, behind),
                fatal: false,
            });
            return;
        }
    }
}

/// What the operator is told about a line the CLI's standard input would not
/// take, and the lines queued behind it that go with it.
fn not_sent(what: &str, error: &std::io::Error, behind: usize) -> String {
    let also = match behind {
        0 => String::new(),
        1 => ", nor could the one line queued after it".to_owned(),
        n => format!(", nor could the {n} lines queued after it"),
    };
    format!("the {what} could not be sent to the `claude` session: {error}{also}")
}

/// The end of what the CLI has written to standard error, and how much it
/// wrote in all.
#[derive(Debug, Default)]
struct Tail {
    kept: Vec<u8>,
    bytes: u64,
}

impl Tail {
    fn add(&mut self, read: &[u8]) {
        self.kept.extend_from_slice(read);
        self.bytes = self
            .bytes
            .saturating_add(u64::try_from(read.len()).unwrap_or(u64::MAX));
        // Dropped a slice at a time rather than on every read, which would
        // move the whole buffer for each chunk.
        if self.kept.len() > 2 * SAID {
            self.kept.drain(..self.kept.len() - SAID);
        }
    }

    /// The last [`SAID`] bytes as text, trimmed, with how many came before
    /// them in front where any did.
    ///
    /// Bytes that are not UTF-8 read as U+FFFD, as a line of output does; a
    /// character the cut went through is left out whole rather than read as
    /// one.
    fn said(&self) -> String {
        let mut from = self.kept.len().saturating_sub(SAID);
        let cut = self
            .bytes
            .saturating_sub(u64::try_from(self.kept.len() - from).unwrap_or(u64::MAX));
        if cut > 0 {
            while self
                .kept
                .get(from)
                .is_some_and(|byte| byte & 0b1100_0000 == 0b1000_0000)
            {
                from += 1;
            }
        }
        let text = String::from_utf8_lossy(self.kept.get(from..).unwrap_or_default());
        let text = text.trim();
        match cut {
            0 => text.to_owned(),
            _ => format!(
                "[the first {cut} of {} bytes it wrote are left out] {text}",
                self.bytes
            ),
        }
    }
}

/// Reads the CLI's standard error into `into` until it closes.
///
/// Read a chunk at a time rather than a line at a time, so that a CLI that
/// writes a great deal without a line break is held to [`SAID`] as well.
fn read_tail(mut from: impl Read, into: &Mutex<Tail>) {
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        match from.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => {
                if let Ok(mut tail) = into.lock() {
                    tail.add(chunk.get(..n).unwrap_or_default());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// The next line of one of the CLI's pipes, without its line ending, and
/// whether it was UTF-8 as sent. `None` once the pipe is closed or cannot be
/// read.
///
/// A line that is not UTF-8 is read with each byte that is not replaced by
/// U+FFFD rather than ending the pipe: the bytes come from whatever a tool
/// printed or a file held, and one of them must not cost the rest of the
/// session — nor leave the CLI blocked writing to a pipe nobody reads.
fn next_line(reader: &mut impl BufRead) -> Option<(String, bool)> {
    let mut bytes = Vec::new();
    match reader.read_until(b'\n', &mut bytes) {
        Ok(0) | Err(_) => return None,
        Ok(_) => {}
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    Some(match String::from_utf8(bytes) {
        Ok(line) => (line, true),
        Err(error) => (
            String::from_utf8_lossy(error.as_bytes()).into_owned(),
            false,
        ),
    })
}

/// The warning for a line of the CLI's output that was not UTF-8.
fn undecodable() -> Event {
    Event::Error {
        message: "the CLI sent a line that was not UTF-8; the bytes that were not are shown \
                  as \u{fffd}."
            .to_owned(),
        fatal: false,
    }
}

/// What to say about a CLI that closed its standard output and did not leave
/// when asked, so it was stopped: nothing it did after that could be shown.
fn kept_running(said: &str) -> String {
    let mut message = "the `claude` CLI closed its output while still running, so nothing more \
                       it did could be shown; it was stopped."
        .to_owned();
    if !said.is_empty() {
        message.push_str(" It said: ");
        message.push_str(said);
    }
    message
}

/// The line that sends one turn.
///
/// A turn with no image is the prompt as a plain string, which is what every
/// recorded session was sent. One with images is the prompt's text block and
/// then an image block per image, in the order they were attached: the order
/// Claude Code's own transcripts hold a pasted screenshot in, beside the
/// `[Image #N]` its text keeps where the image went.
fn turn_line(prompt: &str, images: &[Image]) -> serde_json::Value {
    let content = if images.is_empty() {
        serde_json::Value::from(prompt)
    } else {
        std::iter::once(serde_json::json!({ "type": "text", "text": prompt }))
            .chain(images.iter().map(|image| {
                serde_json::json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": image.media_type().as_str(),
                        "data": base64::encode(image.data()),
                    },
                })
            }))
            .collect()
    };
    serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": content },
    })
}

/// The line that asks the CLI to change something about the running session.
///
/// Written as its own function for the same reason as [`control_response`]:
/// what reaches the CLI's standard input is the whole of the protocol on this
/// side, and it can be checked without a subprocess.
fn control_request(request_id: &str, body: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": body,
    })
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
    message: Option<&str>,
) -> serde_json::Value {
    let refused = match message {
        Some(said) => format!("{REFUSED_SAYING}{said}"),
        None => REFUSED.to_owned(),
    };
    let response = match decision.allowed() {
        true => serde_json::json!({ "behavior": "allow", "updatedInput": input }),
        false => serde_json::json!({ "behavior": "deny", "message": refused }),
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

/// Why a session that left with success is reported as a failure: it left
/// with a permission prompt about each of `calls` still open.
fn ended_asking(calls: &[String]) -> String {
    let calls = calls
        .iter()
        .map(|id| format!("`{id}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "the `claude` session ended while it was still waiting on a decision about tool call {calls}."
    )
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
        // Dropping the queue closes it once what is queued has gone in; a
        // CLI that is not reading keeps it open, and is killed below.
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

        self.end_the_group();

        // The threads end when their pipes close, which the group leaving
        // does: the writer too, whose queue is closed and whose last write
        // fails once nothing reads it. Joined so that no thread outlives the
        // session it serves, but
        // never waited on without a bound: a pipe still held by something
        // outside the group would hold the quit up for as long as that runs.
        let until = Instant::now() + LET_GO;
        for thread in self.threads.drain(..) {
            while !thread.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

/// Ends what is left of the process group the CLI led: asked with SIGTERM,
/// then killed if anything is still in it after [`LEFTOVERS`].
///
/// Called once the CLI has been reaped, and straight after: a process group's
/// id is not given to a new process while any member of the group is alive,
/// so the group signalled is still the CLI's, or is gone and the signal finds
/// nothing. Called later, the group could have emptied and its id been given
/// to someone else's.
#[cfg(unix)]
fn end_group(leader: u32) {
    // A group with nobody left in it is what was wanted.
    if !ask_group_to_end(leader) {
        return;
    }
    let until = Instant::now() + LEFTOVERS;
    while group_of(leader)
        .is_some_and(|group| rustix::process::test_kill_process_group(group).is_ok())
    {
        if Instant::now() >= until {
            kill_group(leader);
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(not(unix))]
fn end_group(_leader: u32) {}

/// Sends SIGTERM to the process group the CLI led, without waiting on it,
/// and says whether anything was there to take it.
///
/// Called only once the CLI has been reaped, for the reason [`end_group`]
/// gives.
#[cfg(unix)]
fn ask_group_to_end(leader: u32) -> bool {
    group_of(leader).is_some_and(|group| {
        rustix::process::kill_process_group(group, rustix::process::Signal::TERM).is_ok()
    })
}

#[cfg(not(unix))]
fn ask_group_to_end(_leader: u32) -> bool {
    false
}

/// Kills whatever is left of the process group the CLI led.
#[cfg(unix)]
fn kill_group(leader: u32) {
    if let Some(group) = group_of(leader) {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
}

#[cfg(not(unix))]
fn kill_group(_leader: u32) {}

/// The process group a CLI with process id `leader` leads.
#[cfg(unix)]
fn group_of(leader: u32) -> Option<rustix::process::Pid> {
    i32::try_from(leader)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
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

    /// A `claude` that is there and cannot be run is named by where it was
    /// found: the operator has to find the file to fix it, and PATH can hold
    /// several.
    #[cfg(unix)]
    #[test]
    fn a_binary_found_that_cannot_be_run_is_named_by_its_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a scratch directory");
        let found = dir.path().join("claude");
        std::fs::write(&found, "#!/bin/sh\n").expect("the file is written");
        std::fs::set_permissions(&found, std::fs::Permissions::from_mode(0o644))
            .expect("the mode can be set");
        let mut options = options();
        options.cwd = dir.path().to_path_buf();
        options
            .env
            .insert("PATH".to_owned(), dir.path().display().to_string());

        let error = Session::spawn(&options).expect_err("the file is not executable");

        let said = error.to_string();
        assert!(matches!(error, SpawnError::Failed { .. }), "{said}");
        assert!(said.contains(&found.display().to_string()), "{said}");
    }

    #[test]
    fn a_turn_without_images_is_sent_as_the_prompt_string() {
        assert_eq!(
            turn_line("what changed?", &[]),
            serde_json::json!({
                "type": "user",
                "message": { "role": "user", "content": "what changed?" },
            })
        );
    }

    #[test]
    fn a_turn_with_images_is_its_text_then_each_image_in_the_order_attached() {
        let png = Image::from_bytes(b"\x89PNG\r\n\x1a\nfoo".to_vec()).expect("a PNG signature");
        let jpeg = Image::from_bytes(b"\xff\xd8\xffbar".to_vec()).expect("a JPEG signature");

        let line = turn_line("look: [Image #1] [Image #2]", &[png, jpeg]);

        assert_eq!(
            line,
            serde_json::json!({
                "type": "user",
                "message": { "role": "user", "content": [
                    { "type": "text", "text": "look: [Image #1] [Image #2]" },
                    { "type": "image", "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": "iVBORw0KGgpmb28=",
                    } },
                    { "type": "image", "source": {
                        "type": "base64",
                        "media_type": "image/jpeg",
                        "data": "/9j/YmFy",
                    } },
                ] },
            })
        );
    }

    #[test]
    fn an_approval_sends_back_the_arguments_that_were_shown() {
        let input = serde_json::json!({ "file_path": "/repo/notes.txt" });

        let allowed = control_response("c1", &input, PermissionDecision::Allow, None);
        let always = control_response("c1", &input, PermissionDecision::AllowAlways, None);

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
            None,
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
    fn a_refusal_in_the_operators_own_words_hands_the_agent_those_words() {
        let refused = control_response(
            "c3",
            &serde_json::json!({ "command": "git push" }),
            PermissionDecision::Deny,
            Some("open a PR instead"),
        );

        let response = &refused["response"]["response"];
        assert_eq!(response["behavior"], "deny");
        let said = response["message"].as_str().expect("the message is text");
        assert!(
            said.starts_with("The operator denied this call in Niobe"),
            "{said}"
        );
        assert!(said.ends_with("open a PR instead"), "{said}");
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
            .answer(&ToolCallId::new("toolu_1"), PermissionDecision::Allow, None)
            .expect_err("the CLI was never asked about this call");

        let said = error.to_string();
        assert!(said.contains("not waiting on a decision"), "{said}");
        assert!(said.contains("toolu_1"), "{said}");
    }

    /// Spawns a `claude` that reads the request a session starts with and a
    /// turn, asks about a call, and then leaves with `status`, and drains it
    /// until it has said it ended. The session is returned with what it said.
    #[cfg(unix)]
    fn asks_then_leaves(status: u8) -> (Session, Vec<Event>) {
        let request = r#"{"type":"control_request","request_id":"c1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"},"tool_use_id":"toolu_1"}}"#;
        let crash = match status {
            0 => String::new(),
            _ => "echo crashed >&2\n".to_owned(),
        };
        stand_in(&format!(
            "read -r first\nread -r turn\nprintf '%s\\n' '{request}'\n{crash}exit {status}\n"
        ))
    }

    /// Starts a session on a stand-in written moments ago.
    ///
    /// Linux will not run a file that any process holds open for writing, and a
    /// test beside this one that forks while the stand-in is being written takes
    /// a copy of that descriptor into its child, where it stays until the child
    /// runs its own program. Putting the file in place by a rename does not help:
    /// the copy refers to the same file. It is gone within moments, so "text
    /// file busy" is tried again rather than failed on.
    #[cfg(unix)]
    fn spawn_written(options: &Options) -> Session {
        let started = Instant::now();
        loop {
            match Session::spawn(options) {
                Err(SpawnError::Failed { error, .. })
                    if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                        && started.elapsed() < Duration::from_secs(5) =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                spawned => return spawned.expect("the stand-in starts"),
            }
        }
    }

    /// Starts a session on a `claude` that is the shell script `body`, and
    /// hands it back untouched, with the directory the script is in.
    #[cfg(unix)]
    fn started(body: &str) -> (Session, tempfile::TempDir) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a scratch directory");
        let script = dir.path().join("claude");
        std::fs::write(&script, format!("#!/bin/sh\n{body}")).expect("the stand-in is written");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("the stand-in is made executable");

        let mut options = options();
        options.binary = script;
        options.cwd = dir.path().to_path_buf();
        (spawn_written(&options), dir)
    }

    #[test]
    fn a_directory_that_is_not_there_is_named_rather_than_the_binary() {
        let mut options = options();
        options.binary = PathBuf::from("/bin/sh");
        options.cwd = PathBuf::from("/nonexistent/niobe-audit-dir");

        let said = Session::spawn(&options)
            .expect_err("there is nowhere to run it")
            .to_string();

        assert!(said.contains("/nonexistent/niobe-audit-dir"), "{said}");
        assert!(!said.contains("PATH"), "{said}");
    }

    /// Spawns a `claude` that is the shell script `body`, sends it a turn,
    /// and drains it until it has said it ended. The session is returned with
    /// what it said.
    #[cfg(unix)]
    fn stand_in(body: &str) -> (Session, Vec<Event>) {
        let (mut session, _dir) = started(body);
        // A stand-in that has already left cannot take the turn, and that is
        // reported by the drain rather than here.
        let _ = session.send("list the files", &[]);

        // Generous for the reason `tests/answers.rs` gives: a freshly written
        // script can be held at its first instruction for seconds.
        let started = Instant::now();
        let mut events = Vec::new();
        while !session.reported {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "the stand-in never ended: {events:#?}"
            );
            events.extend(session.drain());
            std::thread::sleep(Duration::from_millis(5));
        }
        (session, events)
    }

    #[cfg(unix)]
    #[test]
    fn a_cli_that_floods_standard_error_ends_with_its_status_and_the_end_of_what_it_said() {
        let (_, events) = stand_in(
            "read -r first\nhead -c 20000000 /dev/zero | tr '\\0' E >&2\necho 'the real reason' >&2\nexit 1\n",
        );

        let said = events
            .iter()
            .find_map(|event| match event {
                Event::Error {
                    message,
                    fatal: true,
                } => Some(message.as_str()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the end was not a failure: {events:#?}"));
        assert!(said.len() < 16 * 1024, "{} bytes", said.len());
        assert!(
            said.starts_with("the `claude` session ended with exit status: 1"),
            "{}",
            &said[..said.len().min(200)]
        );
        assert!(
            said.contains("20000016 bytes"),
            "{}",
            &said[..said.len().min(200)]
        );
        assert!(
            said.ends_with("the real reason"),
            "{}",
            &said[said.len().saturating_sub(200)..]
        );
    }

    /// What the CLI left running in its group is ended as soon as the CLI is
    /// reaped, not at the quit: by then the group could have emptied and its
    /// id been handed to a stranger's group, which the quit would signal.
    #[cfg(unix)]
    #[test]
    fn the_clis_group_is_ended_when_the_cli_is_reaped_and_not_again_at_the_quit() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let pid = dir.path().join("left.pid");
        let (session, _) = stand_in(&format!(
            "sleep 77105 >/dev/null 2>&1 &\necho $! > '{pid}'\nread -r first\nread -r turn\nexit 0\n",
            pid = pid.display()
        ));

        assert!(
            session.group_ended,
            "the group was left for the quit to end"
        );
        let left = std::fs::read_to_string(&pid)
            .ok()
            .and_then(|written| written.trim().parse().ok())
            .and_then(rustix::process::Pid::from_raw)
            .expect("the stand-in wrote the pid of what it left running");
        let deadline = Instant::now() + Duration::from_secs(5);
        while rustix::process::test_kill_process(left).is_ok() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            rustix::process::test_kill_process(left).is_err(),
            "what the CLI left running outlived the CLI's end"
        );
        drop(session);
    }

    /// Something that left the CLI's group cannot be ended with it, so its
    /// hold on the CLI's output is not waited out: the end is reported in the
    /// CLI's words, after what the CLI wrote before it, and nothing else.
    ///
    /// The wait is timed from the stand-in's last instruction rather than
    /// from the spawn: starting a freshly written script takes seconds on a
    /// loaded machine, and that is not the driver's to answer for. On an idle
    /// machine the end is reported 510–520 ms after it, which is [`GOODBYE`]
    /// and the drain's polling; the bound leaves room for a loaded one while
    /// still failing a driver that waits on the detached process.
    #[cfg(unix)]
    #[test]
    fn a_cli_that_dies_while_something_outside_its_group_holds_its_output_is_reported_ended() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let pid = dir.path().join("detached.pid");
        let gone = dir.path().join("gone");
        let reply = r#"{"type":"assistant","message":{"model":"claude-opus-5","id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"replied first"}]}}"#;
        let (session, events) = stand_in(&format!(
            "perl -e 'setpgrp(0, 0); open(my $f, \">\", $ARGV[0]); print $f $$; close($f); exec \"sleep\", \"77104\"' '{pid}' &\nwhile [ ! -s '{pid}' ]; do sleep 0.01; done\nread -r first\nread -r turn\nprintf '%s\\n' '{reply}'\necho 'fell over' >&2\ntouch '{gone}'\nexit 1\n",
            pid = pid.display(),
            gone = gone.display()
        ));
        let took = std::fs::metadata(&gone)
            .and_then(|written| written.modified())
            .ok()
            .and_then(|left| std::time::SystemTime::now().duration_since(left).ok())
            .expect("the stand-in marked its last instruction before it left");
        if let Some(detached) = std::fs::read_to_string(&pid)
            .ok()
            .and_then(|written| written.trim().parse().ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process(detached, rustix::process::Signal::KILL);
        }
        drop(session);

        assert!(
            took < Duration::from_secs(2),
            "the end was reported {took:?} after the CLI left"
        );
        assert!(
            events.iter().any(
                |event| matches!(event, Event::AssistantMessage { text, .. } if text == "replied first")
            ),
            "{events:#?}"
        );
        assert!(
            matches!(events.last(), Some(Event::Error { fatal: true, message })
                if message == "the `claude` session ended with exit status: 1: fell over"),
            "{events:#?}"
        );
    }

    /// Two prompts the CLI sent without a call id are two questions, and an
    /// answer to each reaches the request it was asked by.
    #[cfg(unix)]
    #[test]
    fn two_prompts_with_no_call_id_are_both_answered() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let answers = dir.path().join("answers");
        let ask = |request: &str| {
            format!(
                r#"{{"type":"control_request","request_id":"{request}","request":{{"subtype":"can_use_tool","tool_name":"Bash","input":{{"command":"ls"}}}}}}"#
            )
        };
        let (mut session, _script) = started(&format!(
            "read -r first\nread -r turn\nprintf '%s\\n' '{c1}' '{c2}'\nread -r one\nread -r two\nprintf '%s\\n%s\\n' \"$one\" \"$two\" > '{answers}'\n",
            c1 = ask("c1"),
            c2 = ask("c2"),
            answers = answers.display(),
        ));
        let _ = session.send("list the files", &[]);

        let started = Instant::now();
        let mut asked = Vec::new();
        while asked.len() < 2 {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "the stand-in never asked twice: {asked:?}"
            );
            asked.extend(session.drain().into_iter().filter_map(|event| match event {
                Event::PermissionRequest { id, .. } => Some(id),
                _ => None,
            }));
            std::thread::sleep(Duration::from_millis(5));
        }
        for id in &asked {
            session
                .answer(id, PermissionDecision::Allow, None)
                .expect("each prompt is waiting on its own answer");
        }
        while !session.reported {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "the stand-in never took both answers"
            );
            session.drain();
            std::thread::sleep(Duration::from_millis(5));
        }

        let written =
            std::fs::read_to_string(&answers).expect("the stand-in kept what it was sent");
        assert!(written.contains(r#""request_id":"c1""#), "{written}");
        assert!(written.contains(r#""request_id":"c2""#), "{written}");
    }

    /// What reaches the CLI's output after its end was reported was written
    /// by something that left its group, and is not the CLI's to say.
    #[cfg(unix)]
    #[test]
    fn a_line_written_after_the_end_was_reported_is_not_passed_on() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let at = |name: &str| dir.path().join(name).display().to_string();
        let late = r#"{"type":"assistant","message":{"model":"claude-opus-5","id":"msg_2","type":"message","role":"assistant","content":[{"type":"text","text":"said late"}]}}"#;
        let script = at("late.pl");
        std::fs::write(
            &script,
            format!(
                "setpgrp(0, 0);\n$| = 1;\nopen(my $f, '>', '{pid}'); print $f $$; close($f);\nselect(undef, undef, undef, 0.01) until -e '{go}';\nprint '{late}' . \"\\n\";\nopen(my $g, '>', '{done}'); close($g);\n",
                pid = at("detached.pid"),
                go = at("go"),
                done = at("done"),
            ),
        )
        .expect("the detached writer is written");
        let (mut session, _) = stand_in(&format!(
            "perl '{script}' &\nwhile [ ! -s '{pid}' ]; do sleep 0.01; done\nread -r first\nread -r turn\nexit 1\n",
            pid = at("detached.pid"),
        ));

        std::fs::write(at("go"), "").expect("the writer is told to write");
        let started = Instant::now();
        while !dir.path().join("done").exists() {
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the detached writer never wrote"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut after = Vec::new();
        for _ in 0..40 {
            after.extend(session.drain());
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(session);

        assert!(after.is_empty(), "{after:#?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_cli_that_dies_with_a_prompt_open_leaves_nothing_waiting_on_an_answer() {
        let (mut session, events) = asks_then_leaves(3);

        assert!(
            matches!(events.first(), Some(Event::PermissionRequest { .. })),
            "{events:#?}"
        );
        assert!(
            matches!(events.last(), Some(Event::Error { fatal: true, message }) if message.contains("crashed")),
            "{events:#?}"
        );
        let waiting = session.waiting.lock().expect("nothing panicked holding it");
        assert!(waiting.is_empty(), "{waiting:?}");
        drop(waiting);
        let said = session
            .answer(&ToolCallId::new("toolu_1"), PermissionDecision::Allow, None)
            .expect_err("there is no one left to answer")
            .to_string();
        assert!(said.contains("not waiting on a decision"), "{said}");
    }

    #[cfg(unix)]
    #[test]
    fn a_cli_that_leaves_cleanly_with_a_prompt_open_is_not_reported_as_a_clean_end() {
        let (session, events) = asks_then_leaves(0);

        let said = events
            .iter()
            .find_map(|event| match event {
                Event::Error {
                    message,
                    fatal: true,
                } => Some(message.as_str()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the end was not a failure: {events:#?}"));
        assert!(said.contains("toolu_1"), "{said}");
        assert!(
            session
                .waiting
                .lock()
                .expect("nothing panicked holding it")
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_cli_that_withdraws_its_prompt_and_leaves_cleanly_ends_cleanly() {
        let request = r#"{"type":"control_request","request_id":"c1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"},"tool_use_id":"toolu_1"}}"#;
        let cancel = r#"{"type":"control_cancel_request","request_id":"c1"}"#;
        let (_, events) = stand_in(&format!(
            "read -r first\nread -r turn\nprintf '%s\\n' '{request}' '{cancel}'\nexit 0\n"
        ));

        assert!(
            matches!(
                events.as_slice(),
                [
                    Event::PermissionRequest { .. },
                    Event::PermissionWithdrawn { .. },
                    Event::Notice { message },
                ] if message == "the `claude` session ended."
            ),
            "{events:#?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_answer_to_a_prompt_the_cli_withdrew_is_refused_rather_than_written() {
        let request = r#"{"type":"control_request","request_id":"c1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"},"tool_use_id":"toolu_1"}}"#;
        let cancel = r#"{"type":"control_cancel_request","request_id":"c1"}"#;
        let (mut session, _dir) = started(&format!(
            "read -r first\nread -r turn\nprintf '%s\\n' '{request}' '{cancel}'\nexec sleep 300\n"
        ));
        session
            .send("list the files", &[])
            .expect("the turn is queued");
        drained_until(&mut session, |event| {
            matches!(event, Event::PermissionWithdrawn { .. })
        });

        let said = session
            .answer(&ToolCallId::new("toolu_1"), PermissionDecision::Allow, None)
            .expect_err("the CLI no longer waits on this call")
            .to_string();

        assert!(said.contains("not waiting on a decision"), "{said}");
        assert!(said.contains("toolu_1"), "{said}");
    }

    /// More than a pipe holds, so a CLI that is not reading cannot take it
    /// all: a pasted log, say.
    #[cfg(unix)]
    fn a_long_paste() -> String {
        "a pasted log line\n".repeat(64 * 1024)
    }

    /// Drains `session` until one of its events is `wanted`, and returns them
    /// all.
    #[cfg(unix)]
    fn drained_until(session: &mut Session, wanted: impl Fn(&Event) -> bool) -> Vec<Event> {
        let started = Instant::now();
        let mut events = Vec::new();
        while !events.iter().any(&wanted) {
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "it never came: {events:#?}"
            );
            events.extend(session.drain());
            std::thread::sleep(Duration::from_millis(5));
        }
        events
    }

    #[cfg(unix)]
    #[test]
    fn a_cli_that_is_not_reading_does_not_hold_up_whoever_writes_to_it() {
        let (mut session, _dir) = started("exec sleep 300\n");

        let started = Instant::now();
        session
            .send(&a_long_paste(), &[])
            .expect("the turn is queued");
        session.set_mode(Mode::Plan).expect("the request is queued");
        session.set_model("haiku").expect("the request is queued");
        let took = started.elapsed();

        // A write that waited on the CLI would wait until it left, five
        // minutes on; queued, the lines took 105 ms at worst with two
        // whole-workspace test runs sharing the machine.
        assert!(took < Duration::from_secs(10), "writing took {took:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_turn_the_cli_does_not_read_in_time_is_said_to_be_waiting() {
        let (mut session, _dir) = started("exec sleep 30\n");
        session.stalled_after = Duration::from_millis(200);

        session
            .send(&a_long_paste(), &[])
            .expect("the turn is queued");
        let events = drained_until(&mut session, |event| {
            matches!(event, Event::Error { fatal: false, .. })
        });

        let said = events
            .iter()
            .filter(|event| matches!(event, Event::Error { .. }))
            .collect::<Vec<_>>();
        assert!(
            matches!(said.as_slice(), [Event::Error { message, fatal: false }]
                if message.contains("has not read the turn")),
            "{events:#?}"
        );
        let later = session.drain();
        assert!(
            !later
                .iter()
                .any(|event| matches!(event, Event::Error { .. })),
            "a stall is said once: {later:#?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_turn_the_cli_leaves_without_reading_is_reported_as_not_sent() {
        let (mut session, _dir) = started("exec sleep 1\n");

        session
            .send(&a_long_paste(), &[])
            .expect("the turn is queued");
        let events = drained_until(
            &mut session,
            |event| matches!(event, Event::Error { message, .. } if message.contains("could not be sent")),
        );

        assert!(
            events.iter().any(|event| matches!(event,
                Event::Error { message, fatal: false }
                    if message.starts_with("the turn could not be sent to the `claude` session"))),
            "{events:#?}"
        );
    }

    #[test]
    fn the_spawn_line_carries_the_mode_and_the_budget_the_session_runs_under() {
        let mut options = options();
        options.mode = Mode::Plan;
        options.budget_usd = Some(0.5);

        let argv = options.argv();

        let mode = argv
            .iter()
            .position(|a| a == "--permission-mode")
            .expect("the mode");
        assert_eq!(argv[mode + 1], "plan");
        let budget = argv
            .iter()
            .position(|a| a == "--max-budget-usd")
            .expect("the budget");
        assert_eq!(argv[budget + 1], "0.5");
    }

    #[test]
    fn a_session_with_no_budget_passes_no_budget_flag() {
        assert!(!options().argv().iter().any(|a| a == "--max-budget-usd"));
    }

    #[test]
    fn every_mode_the_shell_offers_is_spelt_the_way_the_cli_takes_it() {
        // The CLI accepts `acceptEdits`, `auto`, `bypassPermissions`,
        // `default`, `dontAsk` and `plan`; these three are the ones Niobe
        // models, and it never sends a spelling the CLI would refuse.
        assert_eq!(spelt(Mode::Plan), "plan");
        assert_eq!(spelt(Mode::Ask), "default");
        assert_eq!(spelt(Mode::Auto), "auto");
    }

    #[test]
    fn changing_the_mode_and_the_model_are_requests_the_cli_answers_by_id() {
        let mode = control_request(
            "niobe-1",
            serde_json::json!({
                "subtype": "set_permission_mode",
                "mode": spelt(Mode::Plan),
            }),
        );
        let model = control_request(
            "niobe-2",
            serde_json::json!({
                "subtype": "set_model",
                "model": "haiku",
            }),
        );

        assert_eq!(
            mode,
            serde_json::json!({
                "type": "control_request",
                "request_id": "niobe-1",
                "request": { "subtype": "set_permission_mode", "mode": "plan" },
            })
        );
        assert_eq!(model["request"]["model"], "haiku");
        assert_eq!(model["request_id"], "niobe-2");
    }

    #[test]
    fn each_request_is_addressed_by_an_id_of_its_own() {
        let mut options = options();
        options.binary = PathBuf::from("/bin/echo");
        options.cwd = std::env::temp_dir();
        let mut session = Session::spawn(&options).expect("`/bin/echo` is on every unix");

        // `/bin/echo` reads nothing, so what matters here is only that two
        // requests are never addressed the same way: an answer to one would
        // otherwise be read as the answer to the other.
        let first = session.next_request_id();
        let second = session.next_request_id();

        assert_ne!(first, second);
        assert!(first.starts_with("niobe-"), "{first}");
    }

    #[test]
    fn a_cut_through_a_character_leaves_the_character_out_rather_than_misreading_it() {
        let mut tail = Tail::default();
        // A run of two-byte characters and then one byte, so the cut at
        // [`SAID`] from the end falls between the two bytes of a character.
        tail.add("é".repeat(SAID).as_bytes());
        tail.add(b"x");

        let said = tail.said();

        let text = said
            .split_once("] ")
            .map(|(_, text)| text)
            .expect("the cut is said");
        assert!(!text.contains('\u{fffd}'), "{said}");
        assert!(text.ends_with("éx"), "{said}");
        assert_eq!(text.len(), SAID - 1, "{said}");
        let written = 2 * SAID + 1;
        assert!(
            said.starts_with(&format!(
                "[the first {} of {written} bytes it wrote are left out]",
                written - SAID
            )),
            "{}",
            &said[..80]
        );
    }

    #[test]
    fn what_fits_is_kept_whole_with_nothing_said_about_a_cut() {
        let mut tail = Tail::default();
        tail.add(b"Error: not signed in\n");

        assert_eq!(tail.said(), "Error: not signed in");
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
