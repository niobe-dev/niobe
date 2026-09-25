// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Where a command the operator types after `!` is run.
//!
//! The shell runs no process and touches no filesystem, so it says what it
//! needs — somewhere to hand a command, somewhere to hear back how it ended —
//! and the binary hands it something that does it, the way
//! [`crate::bridge`] is handed a backend.
//!
//! # Who allows it
//!
//! Nobody is asked. A command typed after `!` is the operator's own act, run
//! with the operator's own authority, the same as typing it into another
//! terminal in the same directory: the only person a prompt could ask is the
//! person who typed it. So:
//!
//! * the `[permissions]` rules do not apply to it. They are standing answers
//!   the operator gives the *agent*, and nothing here is the agent asking;
//! * nor does the mode. Plan mode means the agent changes nothing, not that
//!   the operator may not;
//! * it runs in the directory the session runs in, with the environment niobe
//!   was started with, standard input closed and no terminal — the shell has
//!   the terminal — so a command that waits for input reads the end of it
//!   and a pager prints straight through. It runs until it ends, the
//!   operator stops it with [`STOP_KEY`], or niobe quits, which stops it.
//!
//! # Stopping one
//!
//! [`STOP_KEY`] stops the newest command still running that has not already
//! been asked to stop, and a second press the one before it, so several
//! commands running at once can each be stopped without quitting. It is not
//! Ctrl+C, which quits, as it has always done: a key that quit in one moment
//! and stopped a command in the next would end a session the operator only
//! meant to interrupt. The command is stopped with everything it started —
//! asked first, and killed if it has not ended a moment later — and its call
//! ends as stopped by the operator, with what it printed up to then.
//!
//! # What it leaves behind
//!
//! It is not a side channel. It is recorded as a tool call like any other —
//! a [`ToolCallStart`] and a [`ToolCallEnd`] named [`OPERATOR_SHELL`], kept in
//! the session store and shown in the transcript with what it printed — so a
//! session read back shows it where it happened. A `cargo test` run this way
//! is read for its counts the way the agent's is.
//!
//! It costs nothing: no model is asked anything, so it reports no usage.
//!
//! What it printed does **not** reach the agent. The backend is never told the
//! command ran, and the next turn's model has not seen its output. Handing
//! that output to the agent — what to send, how much of it, as what — is a
//! separate decision about what enters the context, and nothing makes it yet.
//!
//! [`ToolCallStart`]: niobe_core::event::Event::ToolCallStart
//! [`ToolCallEnd`]: niobe_core::event::Event::ToolCallEnd

use niobe_core::event::ToolCallId;

/// The tool name a command run with `!` is recorded under.
///
/// No backend names a tool this way, so the operator's commands are never
/// counted as one of the agent's tools, nor grouped with its calls.
pub const OPERATOR_SHELL: &str = "! shell";

/// The key that stops a command the operator ran, as the help and the hints
/// name it.
pub const STOP_KEY: &str = "Ctrl+G";

/// Why a command the operator stopped did not run to its end.
pub const STOPPED: &str = "stopped by the operator";

/// Why a command could not be started. Shown to the operator as it reads.
pub type ShellError = Box<dyn std::error::Error + Send + Sync>;

/// How a command the operator ran ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    /// The call it was recorded as, which [`Shell::run`] was handed.
    pub id: ToolCallId,
    /// What it printed, standard error with standard output in the order it
    /// was written, as far as it was kept.
    pub output: String,
    /// How many bytes it printed, all of them: more than `output` holds where
    /// it printed more than is kept.
    pub bytes: u64,
    /// Whether `output` is all of what it printed.
    pub whole: bool,
    /// The status it exited with. `None` where it did not exit — a signal
    /// ended it — or could not be started.
    pub exit_code: Option<i32>,
    /// Why it did not run to an exit of its own, where it did not: the signal
    /// that ended it, or why it could not be waited on.
    pub error: Option<String>,
}

/// Something that runs the operator's commands.
///
/// Both halves are non-blocking for the reason [`crate::bridge::Bridge`]'s
/// are: the event loop has a terminal to draw on every tick.
pub trait Shell: std::fmt::Debug {
    /// Starts `command`, recorded as the call `id`. Returns once it has
    /// started, not once it has ended: the end arrives through
    /// [`Shell::drain`].
    fn run(&mut self, id: &ToolCallId, command: &str) -> Result<(), ShellError>;

    /// Stops the command recorded as the call `id`, with whatever it started.
    /// Returns once it has been told, not once it has ended: its end arrives
    /// through [`Shell::drain`] like any other. A command that has already
    /// ended, or was never started, is left alone.
    fn stop(&mut self, id: &ToolCallId);

    /// Every command that has ended since the last call, in the order they
    /// ended. Never blocks.
    fn drain(&mut self) -> Vec<Ran>;
}

/// No shell: a recorded log being looked at rather than continued, where
/// nothing is run on the operator's behalf.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoShell;

impl Shell for NoShell {
    fn run(&mut self, _id: &ToolCallId, _command: &str) -> Result<(), ShellError> {
        Err("this session runs no commands".into())
    }

    fn stop(&mut self, _id: &ToolCallId) {}

    fn drain(&mut self) -> Vec<Ran> {
        Vec::new()
    }
}
