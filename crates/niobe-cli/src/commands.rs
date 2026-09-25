// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Runs the commands the operator types after `!`.
//!
//! Who allows such a command, what it is recorded as and what it does not
//! reach is written down in [`niobe_tui::shell`]; this is the part that
//! starts processes, which the shell may not.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use niobe_core::event::ToolCallId;
use niobe_tui::shell::{Ran, Shell, ShellError};

/// How much of what a command printed is kept: the end of it, since that is
/// where a command says how it went. A command that prints more is still
/// read to the end, so that it is never stopped by a pipe nobody empties, and
/// its end says how much it printed in all.
const KEPT: usize = 1024 * 1024;

/// The operator's commands, run in the directory the session runs in.
#[derive(Debug)]
pub struct Commands {
    cwd: PathBuf,
    ended: Receiver<Ran>,
    ends: Sender<Ran>,
    /// The process of each command still running, which leads a process
    /// group of its own, so that what it started can be stopped with it.
    running: Arc<Mutex<Vec<u32>>>,
}

impl Commands {
    /// Runs commands in `cwd`.
    pub fn at(cwd: &Path) -> Self {
        let (ends, ended) = channel();
        Self {
            cwd: cwd.to_path_buf(),
            ended,
            ends,
            running: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl Shell for Commands {
    fn run(&mut self, id: &ToolCallId, command: &str) -> Result<(), ShellError> {
        let mut child = spawn(&self.cwd, command)?;
        let pid = child.id();
        let printed = child.stdout.take();
        if let Ok(mut running) = self.running.lock() {
            running.push(pid);
        }

        let id = id.clone();
        let ends = self.ends.clone();
        let running = Arc::clone(&self.running);
        std::thread::spawn(move || {
            let ran = wait(id, &mut child, printed);
            if let Ok(mut running) = running.lock() {
                running.retain(|running| *running != pid);
            }
            // The shell has gone: the session ended while this ran, and there
            // is nobody left to tell.
            let _ = ends.send(ran);
        });
        Ok(())
    }

    fn drain(&mut self) -> Vec<Ran> {
        self.ended.try_iter().collect()
    }
}

impl Drop for Commands {
    /// Stops every command still running, and whatever it started: the
    /// session is over, and nothing it ran on the operator's behalf is left
    /// running with no one to see it end.
    fn drop(&mut self) {
        let Ok(running) = self.running.lock() else {
            return;
        };
        for pid in running.iter() {
            stop(*pid);
        }
    }
}

/// Starts `command` under `sh`, in `cwd`.
///
/// Standard error is sent where standard output goes by the shell itself, so
/// the two arrive in one stream in the order they were written. There is no
/// terminal to give it — the shell has it — so standard input is closed and
/// a pager is told to print straight through rather than wait for a key.
fn spawn(cwd: &Path, command: &str) -> Result<Child, ShellError> {
    let mut sh = Command::new("sh");
    sh.arg("-c")
        .arg(format!("exec 2>&1\n{command}"))
        .current_dir(cwd)
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut sh, 0);
    sh.spawn()
        .map_err(|error| format!("cannot start sh: {error}").into())
}

/// Reads what the command prints until it is done, then waits for it.
fn wait(id: ToolCallId, child: &mut Child, printed: Option<ChildStdout>) -> Ran {
    let (kept, bytes) = printed.map(read_end).unwrap_or_default();
    let whole = u64::try_from(kept.len()).is_ok_and(|kept| kept == bytes);
    let output = String::from_utf8_lossy(&kept).into_owned();
    let (exit_code, error) = match child.wait() {
        Ok(status) => (status.code(), signalled(status)),
        Err(error) => (None, Some(format!("cannot wait for it: {error}"))),
    };
    Ran {
        id,
        output,
        bytes,
        whole,
        exit_code,
        error,
    }
}

/// The last [`KEPT`] bytes `from` gives before it ends, and how many it gave
/// in all.
fn read_end(mut from: impl Read) -> (Vec<u8>, u64) {
    let mut kept = Vec::new();
    let mut bytes: u64 = 0;
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        match from.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let read = chunk.get(..n).unwrap_or_default();
                kept.extend_from_slice(read);
                bytes = bytes.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
                // Dropped a slice at a time rather than on every read, which
                // would move the whole buffer for each chunk.
                if kept.len() > 2 * KEPT {
                    kept.drain(..kept.len() - KEPT);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    if kept.len() > KEPT {
        kept.drain(..kept.len() - KEPT);
    }
    (kept, bytes)
}

/// Why a command ended without an exit status of its own, where a signal
/// ended it.
#[cfg(unix)]
fn signalled(status: std::process::ExitStatus) -> Option<String> {
    std::os::unix::process::ExitStatusExt::signal(&status)
        .map(|signal| format!("ended by signal {signal}"))
}

#[cfg(not(unix))]
fn signalled(_status: std::process::ExitStatus) -> Option<String> {
    None
}

/// Stops the process group `pid` leads.
#[cfg(unix)]
fn stop(pid: u32) {
    let group = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw);
    if let Some(group) = group {
        // A group that has already gone is what was wanted.
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::TERM);
    }
}

#[cfg(not(unix))]
fn stop(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::{Duration, Instant};

    /// Runs `command` in `cwd` and waits for it to end.
    fn ran(cwd: &Path, command: &str) -> Ran {
        let mut commands = Commands::at(cwd);
        commands
            .run(&ToolCallId::new("t"), command)
            .expect("sh starts");
        ended(&mut commands, Duration::from_secs(10)).expect("the command ends")
    }

    fn ended(commands: &mut Commands, patience: Duration) -> Option<Ran> {
        let until = Instant::now() + patience;
        while Instant::now() < until {
            if let Some(ran) = commands.drain().pop() {
                return Some(ran);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    #[test]
    fn a_command_prints_both_streams_in_order_and_ends_with_its_status() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");

        let ran = ran(dir.path(), "echo one; echo two >&2; echo three; exit 3");

        assert_eq!(ran.output, "one\ntwo\nthree\n");
        assert_eq!(ran.bytes, 14);
        assert!(ran.whole);
        assert_eq!(ran.exit_code, Some(3));
        assert_eq!(ran.error, None, "an exit of its own needs no reason");
    }

    #[test]
    fn a_command_runs_in_the_sessions_directory_with_nothing_to_read() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        std::fs::write(dir.path().join("here.txt"), "").expect("the file is written");

        let ran = ran(dir.path(), "ls; cat; echo read nothing");

        assert_eq!(ran.output, "here.txt\nread nothing\n");
        assert_eq!(ran.exit_code, Some(0));
    }

    #[test]
    fn a_command_that_prints_more_than_is_kept_keeps_its_end_and_counts_all_of_it() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");

        let ran = ran(
            dir.path(),
            "head -c 3000000 /dev/zero | tr '\\0' 'x'; echo; echo the end",
        );

        assert!(!ran.whole);
        assert_eq!(ran.bytes, 3_000_000 + 1 + 8);
        assert_eq!(ran.output.len(), KEPT);
        assert!(
            ran.output.ends_with("x\nthe end\n"),
            "the end is what is kept"
        );
    }

    #[test]
    fn a_command_still_running_when_the_session_ends_is_stopped_with_what_it_started() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let marker = dir.path().join("still-running");
        let mut commands = Commands::at(dir.path());
        commands
            .run(
                &ToolCallId::new("t"),
                &format!("sleep 30 & echo $! > {}; wait", marker.display()),
            )
            .expect("sh starts");
        let until = Instant::now() + Duration::from_secs(10);
        let child = loop {
            let pid = std::fs::read_to_string(&marker).unwrap_or_default();
            if let Ok(pid) = pid.trim().parse::<i32>() {
                break pid;
            }
            assert!(
                Instant::now() < until,
                "the command never started its child"
            );
            std::thread::sleep(Duration::from_millis(10));
        };

        drop(commands);

        let child = rustix::process::Pid::from_raw(child).expect("a pid is positive");
        let until = Instant::now() + Duration::from_secs(10);
        while rustix::process::test_kill_process(child).is_ok() {
            assert!(
                Instant::now() < until,
                "what the command started outlived the session"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_command_ended_by_a_signal_says_so_and_has_no_status() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");

        let ran = ran(dir.path(), "kill -KILL $$");

        assert_eq!(ran.exit_code, None);
        assert_eq!(ran.error.as_deref(), Some("ended by signal 9"));
    }
}
