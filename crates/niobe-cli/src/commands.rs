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
use std::time::Duration;

use niobe_core::event::ToolCallId;
use niobe_tui::shell::{Ran, Shell, ShellError};

/// How much of what a command printed is kept: the end of it, since that is
/// where a command says how it went. A command that prints more is still
/// read to the end, so that it is never stopped by a pipe nobody empties, and
/// its end says how much it printed in all.
const KEPT: usize = 1024 * 1024;

/// How long a command the operator stopped is given to end on SIGTERM before
/// it is killed. Long enough for a server to close what it holds; short
/// enough that a command which ignores the signal, or a `sh` that outlives
/// it and goes on with the rest of the line, is not left running.
const GRACE: Duration = Duration::from_secs(2);

/// The operator's commands, run in the directory the session runs in.
#[derive(Debug)]
pub struct Commands {
    cwd: PathBuf,
    ended: Receiver<Ran>,
    ends: Sender<Ran>,
    /// The process of each command still running, by the call it is recorded
    /// as. Each leads a process group of its own, so that what it started can
    /// be stopped with it.
    running: Arc<Mutex<Vec<(ToolCallId, u32)>>>,
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
            running.push((id.clone(), pid));
        }

        let id = id.clone();
        let ends = self.ends.clone();
        let running = Arc::clone(&self.running);
        std::thread::spawn(move || {
            let ran = wait(id, &mut child, printed);
            if let Ok(mut running) = running.lock() {
                running.retain(|(_, running)| *running != pid);
            }
            // The shell has gone: the session ended while this ran, and there
            // is nobody left to tell.
            let _ = ends.send(ran);
        });
        Ok(())
    }

    /// Asks the group the command leads to end, and kills it if it has not
    /// within [`GRACE`]. Its end, with what it printed up to then, is read by
    /// the thread already waiting on it.
    fn stop(&mut self, id: &ToolCallId) {
        let Some(pid) = still_running(&self.running, id) else {
            return;
        };
        stop(pid);
        let running = Arc::clone(&self.running);
        let id = id.clone();
        std::thread::spawn(move || {
            std::thread::sleep(GRACE);
            // Only while it is still listed: the thread waiting on it takes it
            // off the list after reaping it, so a group killed here cannot be
            // one a new process has taken the number of.
            if let Ok(running) = running.lock()
                && running.contains(&(id, pid))
            {
                kill(pid);
            }
        });
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
        for (_, pid) in running.iter() {
            stop(*pid);
        }
    }
}

/// The process the command recorded as `id` runs as, while it runs.
fn still_running(running: &Mutex<Vec<(ToolCallId, u32)>>, id: &ToolCallId) -> Option<u32> {
    let running = running.lock().ok()?;
    running
        .iter()
        .find(|(running, _)| running == id)
        .map(|(_, pid)| *pid)
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

/// Asks the process group `pid` leads to end.
#[cfg(unix)]
fn stop(pid: u32) {
    signal_group(pid, rustix::process::Signal::TERM);
}

/// Ends the process group `pid` leads, whether it listens or not.
#[cfg(unix)]
fn kill(pid: u32) {
    signal_group(pid, rustix::process::Signal::KILL);
}

#[cfg(unix)]
fn signal_group(pid: u32, signal: rustix::process::Signal) {
    let group = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw);
    if let Some(group) = group {
        // A group that has already gone is what was wanted.
        let _ = rustix::process::kill_process_group(group, signal);
    }
}

#[cfg(not(unix))]
fn stop(_pid: u32) {}

#[cfg(not(unix))]
fn kill(_pid: u32) {}

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
        let child = pid_in(&marker);

        drop(commands);

        gone(child, "what the command started outlived the session");
    }

    /// Waits for `marker` to hold the pid a command wrote into it.
    fn pid_in(marker: &Path) -> i32 {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let pid = std::fs::read_to_string(marker).unwrap_or_default();
            if let Ok(pid) = pid.trim().parse::<i32>() {
                return pid;
            }
            assert!(
                Instant::now() < until,
                "the command never started its child"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits for the process `pid` to be gone.
    fn gone(pid: i32, what: &str) {
        let pid = rustix::process::Pid::from_raw(pid).expect("a pid is positive");
        let until = Instant::now() + Duration::from_secs(10);
        while rustix::process::test_kill_process(pid).is_ok() {
            assert!(Instant::now() < until, "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_command_the_operator_stops_ends_with_what_it_started_and_keeps_what_it_printed() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let marker = dir.path().join("still-running");
        let mut commands = Commands::at(dir.path());
        let id = ToolCallId::new("t");
        commands
            .run(
                &id,
                &format!(
                    "echo before; sleep 30 & echo $! > {}; wait",
                    marker.display()
                ),
            )
            .expect("sh starts");
        let child = pid_in(&marker);

        commands.stop(&id);

        let ran = ended(&mut commands, Duration::from_secs(10)).expect("the command ends");
        assert_eq!(ran.id, id);
        // `sh` may add a line of its own saying what the signal ended.
        assert!(
            ran.output.starts_with("before\n"),
            "what it printed is not kept: {:?}",
            ran.output
        );
        gone(child, "what the command started outlived its stop");

        let again = ran_in(&mut commands, "echo still here");
        assert_eq!(again.output, "still here\n", "the shell runs on");
    }

    #[test]
    fn a_command_that_will_not_end_when_asked_is_killed() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let mut commands = Commands::at(dir.path());
        let id = ToolCallId::new("t");
        let marker = dir.path().join("ignoring");
        // An ignored signal stays ignored across exec, so `sleep` ignores it
        // too, and so does `sh`.
        commands
            .run(
                &id,
                &format!("trap '' TERM; echo $$ > {}; sleep 30", marker.display()),
            )
            .expect("sh starts");
        pid_in(&marker);
        let asked = Instant::now();

        commands.stop(&id);

        let ran = ended(&mut commands, Duration::from_secs(10)).expect("the command ends");
        assert!(
            asked.elapsed() >= GRACE,
            "it was killed before it was asked"
        );
        // The group is signalled one process at a time, so `sh` can see
        // `sleep` killed and exit with 128 + 9 before its own turn comes.
        assert!(
            matches!(
                (ran.exit_code, ran.error.as_deref()),
                (None, Some("ended by signal 9")) | (Some(137), None)
            ),
            "it was not killed: {ran:?}"
        );
    }

    #[test]
    fn stopping_a_command_that_has_ended_stops_nothing_else() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let mut commands = Commands::at(dir.path());
        let done = ran_in(&mut commands, "true");
        let marker = dir.path().join("running");
        commands
            .run(
                &ToolCallId::new("other"),
                &format!("sleep 30 & echo $! > {}; wait", marker.display()),
            )
            .expect("sh starts");
        let child = pid_in(&marker);

        commands.stop(&done.id);

        std::thread::sleep(Duration::from_millis(200));
        assert!(
            commands.drain().is_empty(),
            "a stop for one command ended another"
        );
        let child = rustix::process::Pid::from_raw(child).expect("a pid is positive");
        assert!(rustix::process::test_kill_process(child).is_ok());
    }

    /// Runs `command` on `commands` and waits for it to end.
    fn ran_in(commands: &mut Commands, command: &str) -> Ran {
        let id = ToolCallId::new(command);
        commands.run(&id, command).expect("sh starts");
        ended(commands, Duration::from_secs(10)).expect("the command ends")
    }

    #[test]
    fn a_command_ended_by_a_signal_says_so_and_has_no_status() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");

        let ran = ran(dir.path(), "kill -KILL $$");

        assert_eq!(ran.exit_code, None);
        assert_eq!(ran.error.as_deref(), Some("ended by signal 9"));
    }
}
