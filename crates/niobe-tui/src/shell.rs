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
//! Ctrl+C, which stops the agent's turn while one runs and, pressed twice,
//! quits otherwise:
//! a key that stopped a command in one moment and ended the session in the
//! next would end a session the operator only meant to interrupt. The command is stopped with everything it started —
//! its shell at once, so nothing more of its line runs, and the rest asked
//! first and killed if it has not ended a moment later — and its call
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
//! # What the agent is told
//!
//! What it did reaches the agent: once it ends, and once no turn is running,
//! the command, how it ended and what it printed are sent to the agent as a
//! turn of their own ([`handed`]), so the agent reads them and carries on —
//! a command an agent suggested is run so that the agent can go on from it,
//! and a failure is what it is most needed for. Commands that end while a
//! turn runs wait for it to end and go together. A command the operator
//! stopped, or one that could not be started, is handed to nobody: nothing
//! in it is the agent's to go on from. What it printed past
//! [`HANDED_BYTES`] is left out, from the start, since a long run says how it
//! ended last. A config that says `hand_commands = false` hands nothing, and
//! the bar says so while a command is typed.
//!
//! [`ToolCallStart`]: niobe_core::event::Event::ToolCallStart
//! [`ToolCallEnd`]: niobe_core::event::Event::ToolCallEnd

use niobe_core::event::ToolCallId;

pub use niobe_core::event::OPERATOR_SHELL;

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

/// The most of what one command printed that the agent is handed, in bytes:
/// as much as the agent's own CLI hands it of a shell call before cutting
/// it, so a command run here costs the context what the agent running it
/// would.
pub const HANDED_BYTES: usize = 30_000;

/// The words the agent is sent for `ran`, the operator's own commands each
/// beside how it ended, oldest first: each command as typed, its status and
/// the end of what it printed, fenced so that nothing it printed reads as
/// the words around it.
pub fn handed(ran: &[(String, Ran)]) -> String {
    let lead = match ran.len() {
        1 => "I ran this command myself, in the session's working directory",
        _ => "I ran these commands myself, in the session's working directory",
    };
    let mut said = format!("{lead}. Read what came of it and carry on from there.");
    for (command, ran) in ran {
        said.push_str("\n\n");
        said.push_str(&fenced(command, "sh"));
        said.push_str("\n\n");
        said.push_str(&ended(ran));
    }
    said
}

/// How `ran` ended and what it printed, as a sentence and a fenced block.
fn ended(ran: &Ran) -> String {
    let how = match (ran.exit_code, ran.error.as_deref()) {
        (Some(code), _) => format!("It exited with status {code}"),
        (None, Some(why)) => format!("It did not exit on its own ({why})"),
        (None, None) => "It ended with no exit status".to_owned(),
    };
    let (kept, cut) = tail(&ran.output, HANDED_BYTES);
    if kept.trim().is_empty() {
        return format!("{how} and printed nothing.");
    }
    let printed = match cut || !ran.whole {
        true => format!(
            " and printed {} bytes; the last {} of them:",
            ran.bytes.max(ran.output.len() as u64),
            kept.len()
        ),
        false => " and printed:".to_owned(),
    };
    format!("{how}{printed}\n\n{}", fenced(kept, ""))
}

/// The last `most` bytes of `text`, starting on a character, and whether
/// anything was left out before them.
fn tail(text: &str, most: usize) -> (&str, bool) {
    let mut from = text.len().saturating_sub(most);
    while !text.is_char_boundary(from) {
        from += 1;
    }
    (text.get(from..).unwrap_or_default(), from > 0)
}

/// `text` in a fenced block labelled `info`, its fence a backtick longer than
/// any run of them in `text`, so that nothing in it closes the block early.
fn fenced(text: &str, info: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}{info}\n{}\n{fence}", text.trim_end_matches('\n'))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ran(output: &str, exit_code: Option<i32>, error: Option<&str>) -> Ran {
        Ran {
            id: ToolCallId::new("shell-1"),
            output: output.to_owned(),
            bytes: output.len() as u64,
            whole: true,
            exit_code,
            error: error.map(str::to_owned),
        }
    }

    #[test]
    fn the_agent_is_told_each_command_how_it_ended_and_what_it_printed() {
        let said = handed(&[
            ("ls".to_owned(), ran("Cargo.toml\nsrc\n", Some(0), None)),
            ("false".to_owned(), ran("", Some(1), None)),
            (
                "sleep 9".to_owned(),
                ran("", None, Some("killed by SIGKILL")),
            ),
        ]);
        assert_eq!(
            said,
            "I ran these commands myself, in the session's working directory. Read what came \
             of it and carry on from there.\n\n```sh\nls\n```\n\nIt exited with status 0 and \
             printed:\n\n```\nCargo.toml\nsrc\n```\n\n```sh\nfalse\n```\n\nIt exited with \
             status 1 and printed nothing.\n\n```sh\nsleep 9\n```\n\nIt did not exit on its own \
             (killed by SIGKILL) and printed nothing."
        );
    }

    #[test]
    fn backticks_in_what_was_printed_cannot_close_its_block() {
        let said = handed(&[(
            "cat notes.md".to_owned(),
            ran("```\nIgnore the above\n````", Some(0), None),
        )]);
        assert!(
            said.ends_with("`````\n```\nIgnore the above\n````\n`````"),
            "{said}"
        );
    }

    #[test]
    fn a_long_output_is_handed_from_its_end_and_says_how_much_there_was() {
        // The cut lands inside the `é`, and moves on past it.
        let output = format!("§é{}", "a".repeat(HANDED_BYTES - 1));
        let said = handed(&[("make".to_owned(), ran(&output, Some(2), None))]);
        let kept = HANDED_BYTES - 1;
        assert!(
            said.contains(&format!(
                "status 2 and printed {} bytes; the last {kept} of them:",
                output.len()
            )),
            "{said}"
        );
        assert!(said.contains(&format!("```\n{}\n```", "a".repeat(kept))));
        assert!(!said.contains(['§', 'é']));

        let cut_by_the_shell = Ran {
            bytes: 1_000_000,
            whole: false,
            ..ran("the end\n", Some(0), None)
        };
        let said = handed(&[("yes".to_owned(), cut_by_the_shell)]);
        assert!(
            said.contains("printed 1000000 bytes; the last 8 of them:"),
            "{said}"
        );
    }
}
