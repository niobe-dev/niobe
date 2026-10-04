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
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use niobe_core::event::ToolCallId;
use niobe_tui::shell::{Ran, Shell, ShellError};

use crate::reaper::Reaper;

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

/// How long the commands still running when the session ends are given to
/// end on SIGTERM before they are killed. Shorter than [`GRACE`]: the
/// terminal has been handed back by then, but niobe has not exited, and the
/// operator who quit is waiting on it. A command that listens ends within a
/// few milliseconds, so only one that does not is waited for this long.
const QUIT_GRACE: Duration = Duration::from_millis(500);

/// How long, once `sh` has ended, what it printed is given to reach its end.
/// Something `sh` put in the background may hold its output open for as long
/// as it runs; the command has ended all the same, and what reaches the output
/// after this is not recorded as the command's.
const LINGER: Duration = Duration::from_millis(100);

/// The operator's commands, run in the directory the session runs in.
#[derive(Debug)]
pub struct Commands {
    cwd: PathBuf,
    ended: Receiver<Ran>,
    ends: Sender<Ran>,
    /// The process group of each command, by the call it is recorded as,
    /// until the group is seen with nobody left in it. Each command's `sh`
    /// leads a group of its own, so that what it started can be stopped with
    /// it, and the group is kept after `sh` has ended: what `sh` put in the
    /// background is still in it, and is still the session's to stop.
    groups: Arc<Mutex<Vec<Group>>>,
    /// Told of every group, so that a session killed outright does not leave
    /// them running.
    reaper: Option<Reaper>,
    /// How long what is still running when the session ends is given to end
    /// on SIGTERM before it is killed: [`QUIT_GRACE`], but for a test.
    grace: Duration,
    /// The process groups the backend has led, while the reaper is told of
    /// them. They are the bridge's to stop; they are kept here only so that
    /// the reaper is told when each is seen empty, as a command's is.
    backend: Vec<u32>,
    /// Where word of each group the backend leads comes from: the one it
    /// started in, and one for each time it is started again.
    backend_groups: Option<Receiver<u32>>,
}

/// The process group one command's `sh` leads.
#[derive(Debug)]
struct Group {
    id: ToolCallId,
    leader: u32,
    /// Whether `sh` has ended and been reaped. Until then its group cannot be
    /// empty, and is the command's own.
    ended: bool,
}

impl Commands {
    /// Runs commands in `cwd`.
    pub fn at(cwd: &Path) -> Self {
        let (ends, ended) = channel();
        Self {
            cwd: cwd.to_path_buf(),
            ended,
            ends,
            groups: Arc::new(Mutex::new(Vec::new())),
            reaper: None,
            grace: QUIT_GRACE,
            backend: Vec::new(),
            backend_groups: None,
        }
    }

    /// Tells `reaper` of every group a command starts, and of every one seen
    /// empty.
    pub fn reaped_by(mut self, reaper: Reaper) -> Self {
        self.reaper = Some(reaper);
        self
    }

    /// Tells the reaper of each group the backend has started since the last
    /// look.
    fn watch_the_backends_new_groups(&mut self) {
        let (Some(reaper), Some(groups)) = (&self.reaper, &self.backend_groups) else {
            return;
        };
        for group in groups.try_iter() {
            reaper.watch(group);
            self.backend.push(group);
        }
    }

    /// Tells the reaper each of the backend's groups has ended once nobody is
    /// left in it. Until then its number cannot be given to anyone else, so
    /// whatever the backend started is still the session's to stop.
    fn let_go_of_the_backend_once_empty(&mut self) {
        let reaper = self.reaper.as_ref();
        self.backend.retain(|&group| {
            let kept = occupied(group);
            if !kept && let Some(reaper) = reaper {
                reaper.forget(group);
            }
            kept
        });
    }

    /// Tells the reaper of each process group the backend leads, as `groups`
    /// names them, and, once each is seen with nobody left in it, that it has
    /// ended.
    pub fn reaping_the_backend(mut self, groups: Receiver<u32>) -> Self {
        if self.reaper.is_some() {
            self.backend_groups = Some(groups);
            self.watch_the_backends_new_groups();
        }
        self
    }
}

impl Shell for Commands {
    fn run(&mut self, id: &ToolCallId, command: &str) -> Result<(), ShellError> {
        if let Ok(mut groups) = self.groups.lock() {
            let_go_of_the_empty(&mut groups, self.reaper.as_ref());
        }
        let mut child = spawn(&self.cwd, command)?;
        let leader = child.id();
        let printed = child.stdout.take();
        if let Ok(mut groups) = self.groups.lock() {
            groups.push(Group {
                id: id.clone(),
                leader,
                ended: false,
            });
        }
        if let Some(reaper) = &self.reaper {
            reaper.watch(leader);
        }

        let id = id.clone();
        let ends = self.ends.clone();
        let groups = Arc::clone(&self.groups);
        let reaper = self.reaper.clone();
        std::thread::spawn(move || {
            let ran = wait(id, &mut child, printed);
            if let Ok(mut groups) = groups.lock() {
                for group in groups.iter_mut().filter(|group| group.leader == leader) {
                    group.ended = true;
                }
                let_go_of_the_empty(&mut groups, reaper.as_ref());
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
        let Some(leader) = still_running(&self.groups, id) else {
            return;
        };
        // The line's own shell is ended outright, and first: one that has a
        // trap on the signal, or defers it while it waits on a step, would
        // otherwise go on to the next step of the line once the one it
        // stopped has ended. What it started is asked and given the grace.
        end_the_shell(leader);
        stop(leader);
        let groups = Arc::clone(&self.groups);
        let reaper = self.reaper.clone();
        let id = id.clone();
        std::thread::spawn(move || {
            std::thread::sleep(GRACE);
            // Only while it is still listed, looked at again first: a group
            // is taken off the list once it is seen empty, so a group killed
            // here cannot be one a new process has taken the number of.
            if let Ok(mut groups) = groups.lock() {
                let_go_of_the_empty(&mut groups, reaper.as_ref());
                if groups
                    .iter()
                    .any(|group| group.id == id && group.leader == leader)
                {
                    kill(leader);
                }
            }
        });
    }

    /// Also lets go of every group that has emptied since the last look, the
    /// backend's among them, so
    /// that neither the quit nor the reaper is left holding the number of a
    /// group that is gone: once empty, it can be given to anyone's.
    fn drain(&mut self) -> Vec<Ran> {
        if let Ok(mut groups) = self.groups.lock() {
            let_go_of_the_empty(&mut groups, self.reaper.as_ref());
        }
        self.watch_the_backends_new_groups();
        self.let_go_of_the_backend_once_empty();
        self.ended.try_iter().collect()
    }
}

impl Drop for Commands {
    /// Stops every command still running, and whatever any command started
    /// that is still running: the session is over, and nothing it ran on the
    /// operator's behalf is left running with no one to see it end. Each
    /// group is asked first and killed if anything in it is still running
    /// after [`QUIT_GRACE`]; a quit with nothing still running, or only what
    /// ends when asked, is not held up.
    ///
    /// What is stopped is what is still in a command's process group. A
    /// process that calls `setsid` leaves it for a session and a group of its
    /// own, whose number nothing here is told, and is left running: a group
    /// is the most that can be signalled without walking every process on
    /// the machine, and a command that daemonises itself has asked to
    /// outlive what started it.
    fn drop(&mut self) {
        if let Ok(mut groups) = self.groups.lock() {
            let_go_of_the_empty(&mut groups, self.reaper.as_ref());
            for group in groups.iter() {
                stop(group.leader);
            }
        }
        let until = Instant::now() + self.grace;
        while Instant::now() < until && !none_running(&self.groups) {
            std::thread::sleep(Duration::from_millis(5));
        }
        // Under the lock, as in `stop`, and looked at again first: a group
        // that emptied while this waited is let go of rather than killed, so a
        // group killed here is still the command's own.
        if let Ok(mut groups) = self.groups.lock() {
            let_go_of_the_empty(&mut groups, self.reaper.as_ref());
            for group in groups.iter() {
                kill(group.leader);
            }
        }
    }
}

/// Takes off `groups` each group whose `sh` has ended with nothing it
/// started still running, and tells `reaper` so.
///
/// A group's number is not given to a new process while anyone is in it, so
/// until it is seen empty it can only be signalled as the command's own.
fn let_go_of_the_empty(groups: &mut Vec<Group>, reaper: Option<&Reaper>) {
    groups.retain(|group| {
        let kept = !group.ended || occupied(group.leader);
        if !kept && let Some(reaper) = reaper {
            reaper.forget(group.leader);
        }
        kept
    });
}

/// Whether every command has ended and been reaped, and nothing any of them
/// started is still running.
fn none_running(groups: &Mutex<Vec<Group>>) -> bool {
    groups.lock().map_or(true, |groups| {
        groups
            .iter()
            .all(|group| group.ended && !occupied(group.leader))
    })
}

/// The group the command recorded as `id` leads, while its `sh` runs.
fn still_running(groups: &Mutex<Vec<Group>>, id: &ToolCallId) -> Option<u32> {
    let groups = groups.lock().ok()?;
    groups
        .iter()
        .find(|group| group.id == *id && !group.ended)
        .map(|group| group.leader)
}

/// Starts `command` under `sh`, in `cwd`.
///
/// Standard error is sent where standard output goes by the shell itself, so
/// the two arrive in one stream in the order they were written. There is no
/// terminal to give it — the shell has it — so standard input is closed, a
/// pager is told to print straight through rather than wait for a key, and
/// the command runs in a session of its own ([`detached`]), where `/dev/tty`
/// names no terminal: a program that opens it, as `sudo` or `ssh` asks for a
/// password, would otherwise write over the frame the shell is drawing.
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
    detached(&mut sh);
    sh.spawn()
        .map_err(|error| format!("cannot start sh: {error}").into())
}

/// Starts `sh` in a session of its own, which takes it away from the
/// terminal the shell draws on and makes it the leader of a process group of
/// its own, numbered as its pid: the group every stop and the reaper end.
///
/// `unsafe` is denied workspace-wide and allowed here: `setsid` has to be
/// called in the child between fork and exec, `pre_exec` is the only hook
/// there, and the safe `CommandExt::setsid` is not stable yet.
#[cfg(unix)]
#[allow(unsafe_code)]
fn detached(sh: &mut Command) {
    // SAFETY: between fork and exec the child may make only async-signal-safe
    // calls. `setsid` is a single system call that allocates nothing and takes
    // no lock, and the closure touches nothing of the parent's.
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(sh, || {
            rustix::process::setsid()?;
            Ok(())
        });
    }
}

/// Reads what the command prints while it runs, and ends once `sh` has:
/// with everything it printed where its output closes within [`LINGER`], and
/// with what it printed up to then where something it put in the background
/// holds the output open.
fn wait(id: ToolCallId, child: &mut Child, printed: Option<ChildStdout>) -> Ran {
    let tail = Arc::new(Mutex::new(Tail::default()));
    let (read, all_read) = channel::<()>();
    match printed {
        Some(printed) => {
            let tail = Arc::clone(&tail);
            std::thread::spawn(move || {
                read_end(printed, &tail);
                let _ = read.send(());
            });
        }
        None => drop(read),
    }
    let (exit_code, error) = match child.wait() {
        Ok(status) => (status.code(), signalled(status)),
        Err(error) => (None, Some(format!("cannot wait for it: {error}"))),
    };
    let finished = !matches!(
        all_read.recv_timeout(LINGER),
        Err(RecvTimeoutError::Timeout)
    );
    let (kept, bytes) = tail.lock().map(|tail| tail.end()).unwrap_or_default();
    let whole = finished && u64::try_from(kept.len()).is_ok_and(|kept| kept == bytes);
    let output = String::from_utf8_lossy(&kept).into_owned();
    Ran {
        id,
        output,
        bytes,
        whole,
        exit_code,
        error,
    }
}

/// The end of what a command has printed so far, and how much it printed in
/// all.
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
        if self.kept.len() > 2 * KEPT {
            self.kept.drain(..self.kept.len() - KEPT);
        }
    }

    /// The last [`KEPT`] bytes, and how many were printed in all.
    fn end(&self) -> (Vec<u8>, u64) {
        let from = self.kept.len().saturating_sub(KEPT);
        let kept = self.kept.get(from..).unwrap_or_default().to_vec();
        (kept, self.bytes)
    }
}

/// Reads `from` into `into` until it ends.
fn read_end(mut from: impl Read, into: &Mutex<Tail>) {
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        match from.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let read = chunk.get(..n).unwrap_or_default();
                if let Ok(mut tail) = into.lock() {
                    tail.add(read);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
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

/// Ends the process `pid` itself, whether it listens or not, and nothing
/// else in its group.
#[cfg(unix)]
fn end_the_shell(pid: u32) {
    let pid = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw);
    if let Some(pid) = pid {
        // A shell that has already gone is what was wanted.
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
    }
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

/// Whether anyone is left in the process group `leader` leads.
#[cfg(unix)]
fn occupied(leader: u32) -> bool {
    i32::try_from(leader)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|group| rustix::process::test_kill_process_group(group).is_ok())
}

#[cfg(not(unix))]
fn occupied(_leader: u32) -> bool {
    false
}

#[cfg(not(unix))]
fn stop(_pid: u32) {}

#[cfg(not(unix))]
fn end_the_shell(_pid: u32) {}

#[cfg(not(unix))]
fn kill(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// How long a test waits for a command, a child of one, or its end before
    /// calling it hung.
    ///
    /// A command that hangs never comes, so a long wait costs a passing test
    /// nothing. With three copies of this binary running at once, starting
    /// `sh` took 0.87 s at the median and the slowest wait measured was 7.25 s,
    /// on a 12-core Mac; ten seconds had been failing under more load than
    /// that.
    const PATIENCE: Duration = Duration::from_secs(60);

    impl Commands {
        /// The same commands, given `grace` rather than [`QUIT_GRACE`] to end
        /// when the session does.
        fn with_grace(mut self, grace: Duration) -> Self {
            self.grace = grace;
            self
        }
    }

    /// Runs `command` in `cwd` and waits for it to end.
    fn ran(cwd: &Path, command: &str) -> Ran {
        let mut commands = Commands::at(cwd);
        commands
            .run(&ToolCallId::new("t"), command)
            .expect("sh starts");
        ended(&mut commands, PATIENCE).expect("the command ends")
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

    /// A pager would wait for a key there is no terminal to give it.
    #[test]
    fn a_command_is_told_to_page_through_cat() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");

        let ran = ran(dir.path(), "echo \"$PAGER $GIT_PAGER\"");

        assert_eq!(ran.output, "cat cat\n");
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
        assert_eq!(ran.output.len(), 1024 * 1024, "a mebibyte is kept");
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
                &format!("sleep 300 & echo $! > {}; wait", marker.display()),
            )
            .expect("sh starts");
        let child = pid_in(&marker);

        drop(commands);

        gone(child, "what the command started outlived the session");
    }

    #[test]
    fn a_command_that_will_not_end_when_the_session_does_is_killed_without_holding_up_the_quit() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let marker = dir.path().join("ignoring");
        let mut commands = Commands::at(dir.path());
        // An ignored signal stays ignored across exec, so `sleep` ignores it
        // too, and so does `sh`.
        commands
            .run(
                &ToolCallId::new("t"),
                &format!(
                    "trap '' TERM; sleep 300 & echo $! > {}; wait",
                    marker.display()
                ),
            )
            .expect("sh starts");
        let child = pid_in(&marker);
        let quit = Instant::now();

        drop(commands);

        // The grace and then the kill, not the five minutes the command would
        // run for.
        let held = quit.elapsed();
        assert!(held < PATIENCE, "the quit was held up for {held:?}");
        gone(child, "a command that ignores SIGTERM outlived the session");
    }

    #[test]
    fn a_command_that_ends_when_asked_does_not_hold_up_the_quit_for_the_grace() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let marker = dir.path().join("running");
        // A grace no loaded machine takes that long over: only a quit that
        // waits it out can reach half of it.
        let grace = 2 * PATIENCE;
        let mut commands = Commands::at(dir.path()).with_grace(grace);
        commands
            .run(
                &ToolCallId::new("t"),
                &format!("echo $$ > {}; exec sleep 300", marker.display()),
            )
            .expect("sh starts");
        pid_in(&marker);
        let quit = Instant::now();

        drop(commands);

        let held = quit.elapsed();
        assert!(
            held < grace / 2,
            "a command that ended on SIGTERM held up the quit for {held:?}"
        );
    }

    #[test]
    fn a_command_ends_when_its_shell_does_though_what_it_left_running_holds_its_output() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let marker = dir.path().join("left-running");
        let mut commands = Commands::at(dir.path());
        commands
            .run(
                &ToolCallId::new("t"),
                &format!("echo before; (sleep 300 & echo $! > {})", marker.display()),
            )
            .expect("sh starts");
        let child = pid_in(&marker);

        // Well short of the five minutes the child holds the output for.
        let ran = ended(&mut commands, PATIENCE)
            .expect("the command was still running after its shell ended");

        assert_eq!(ran.output, "before\n");
        assert_eq!(ran.exit_code, Some(0));
        assert!(!ran.whole, "what reaches the output later is not in it");
        drop(commands);
        gone(child, "what the command left running outlived the session");
    }

    #[test]
    fn what_an_ended_command_left_running_is_stopped_when_the_session_ends() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let marker = dir.path().join("left-running");
        let mut commands = Commands::at(dir.path());
        let ran = ran_in(
            &mut commands,
            &format!(
                "(sleep 300 >/dev/null 2>&1 & echo $! > {})",
                marker.display()
            ),
        );
        assert!(ran.whole, "nothing held the output open");
        let child = pid_in(&marker);

        drop(commands);

        gone(
            child,
            "what an ended command left running outlived the session",
        );
    }

    /// The backend's group is the bridge's to stop. Once it has emptied, its
    /// number can be given to anyone's, so the reaper is told to let it go,
    /// as it is of a command's.
    #[test]
    fn the_backends_group_is_let_go_of_once_it_has_emptied() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let mut backend = Command::new("sleep");
        std::os::unix::process::CommandExt::process_group(backend.arg("30"), 0);
        let mut backend = backend.spawn().expect("sleep starts");
        let reaper = Reaper::start().expect("sh starts");
        let (started, groups) = channel();
        started.send(backend.id()).expect("the receiver is held");
        let mut commands = Commands::at(dir.path())
            .reaped_by(reaper)
            .reaping_the_backend(groups);

        commands.drain();
        assert_eq!(commands.backend, [backend.id()]);

        backend.kill().expect("sleep is killed");
        backend.wait().expect("sleep is reaped");
        commands.drain();
        assert!(commands.backend.is_empty());
    }

    /// A backend started again in the same session leads a group of its own,
    /// which the reaper is told of as the first was, and let go of as it was.
    #[test]
    fn a_backend_started_again_has_its_group_reaped_and_let_go_of_too() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let backend = || {
            let mut sleep = Command::new("sleep");
            std::os::unix::process::CommandExt::process_group(sleep.arg("30"), 0);
            sleep.spawn().expect("sleep starts")
        };
        let mut first = backend();
        let (started, groups) = channel();
        started.send(first.id()).expect("the receiver is held");
        let mut commands = Commands::at(dir.path())
            .reaped_by(Reaper::start().expect("sh starts"))
            .reaping_the_backend(groups);
        first.kill().expect("sleep is killed");
        first.wait().expect("sleep is reaped");

        let mut again = backend();
        started.send(again.id()).expect("the receiver is held");
        commands.drain();
        assert_eq!(commands.backend, [again.id()]);

        again.kill().expect("sleep is killed");
        again.wait().expect("sleep is reaped");
        commands.drain();
        assert!(commands.backend.is_empty());
    }

    #[test]
    fn a_command_whose_group_has_emptied_is_let_go_of() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let mut commands = Commands::at(dir.path());

        ran_in(&mut commands, "true");

        let groups = commands.groups.lock().expect("nothing else holds the list");
        assert!(
            groups.is_empty(),
            "an empty group is still kept: {groups:?}"
        );
    }

    /// A group can empty after its `sh` has ended, when what the command left
    /// running exits on its own. It is let go of at the next look rather than
    /// kept for the quit to signal, by which time its number could be
    /// someone else's.
    ///
    /// What the command left running is ended by the test, once the shell
    /// has, and waited out, so the one drain made after is the first to see
    /// the group empty however long the machine took to get there.
    #[test]
    fn a_group_that_empties_after_its_shell_ended_is_let_go_of_at_the_next_drain() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let marker = dir.path().join("left-running");
        let mut commands = Commands::at(dir.path());
        ran_in(
            &mut commands,
            &format!(
                "(sleep 300 >/dev/null 2>&1 & echo $! > {})",
                marker.display()
            ),
        );
        let child = pid_in(&marker);
        assert_eq!(
            commands
                .groups
                .lock()
                .expect("nothing else holds the list")
                .len(),
            1,
            "a group with something still running in it was let go of"
        );

        let pid = rustix::process::Pid::from_raw(child).expect("a pid is positive");
        rustix::process::kill_process(pid, rustix::process::Signal::KILL)
            .expect("what the command left running can be killed");
        gone(child, "what the command left running outlived its kill");
        commands.drain();

        let groups = commands.groups.lock().expect("nothing else holds the list");
        assert!(
            groups.is_empty(),
            "a group emptied after its shell was kept for the quit: {groups:?}"
        );
    }

    /// Waits for `marker` to hold the pid a command wrote into it.
    fn pid_in(marker: &Path) -> i32 {
        let until = Instant::now() + PATIENCE;
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
        let until = Instant::now() + PATIENCE;
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
                    "echo before; sleep 300 & echo $! > {}; wait",
                    marker.display()
                ),
            )
            .expect("sh starts");
        let child = pid_in(&marker);

        commands.stop(&id);

        let ran = ended(&mut commands, PATIENCE).expect("the command ends");
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

    /// Stopping a step stops the line: the rest of it does not run once the
    /// step it was waiting on has been ended.
    #[test]
    fn a_stopped_command_runs_none_of_the_rest_of_its_line() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let started = dir.path().join("started");
        let after = dir.path().join("marker");
        let mut commands = Commands::at(dir.path());
        let id = ToolCallId::new("t");
        commands
            .run(
                &id,
                // A shell with a trap on the signal acts on it only once the
                // step it waits on has ended, and then goes on to the next.
                &format!(
                    "trap 'echo stopping' TERM; echo $$ > {}; sleep 30; echo done > {}",
                    started.display(),
                    after.display()
                ),
            )
            .expect("sh starts");
        pid_in(&started);

        commands.stop(&id);

        ended(&mut commands, PATIENCE).expect("the command ends");
        assert!(!after.exists(), "the line ran on after its stop");
    }

    #[test]
    fn a_command_that_will_not_end_when_asked_is_killed() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let mut commands = Commands::at(dir.path());
        let id = ToolCallId::new("t");
        let marker = dir.path().join("ignoring");
        // An ignored signal stays ignored across exec, so `sleep` ignores it.
        commands
            .run(
                &id,
                &format!(
                    "trap '' TERM; sleep 300 & echo $! > {}; wait",
                    marker.display()
                ),
            )
            .expect("sh starts");
        let child = pid_in(&marker);
        let asked = Instant::now();

        commands.stop(&id);

        // The line's own shell is ended at once, whatever it does with the
        // signal, so nothing more of the line runs.
        let ran = ended(&mut commands, PATIENCE).expect("the command ends");
        assert!(
            matches!(
                (ran.exit_code, ran.error.as_deref()),
                (None, Some("ended by signal 9"))
            ),
            "the shell was not ended outright: {ran:?}"
        );
        // What it started, which will not end when asked, is killed once the
        // grace has run out.
        gone(child, "what ignored the stop was never killed");
        assert!(
            asked.elapsed() >= GRACE,
            "it was killed before it was asked"
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
                &format!("sleep 300 & echo $! > {}; wait", marker.display()),
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
        ended(commands, PATIENCE).expect("the command ends")
    }

    #[test]
    fn a_command_ended_by_a_signal_says_so_and_has_no_status() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");

        let ran = ran(dir.path(), "kill -KILL $$");

        assert_eq!(ran.exit_code, None);
        assert_eq!(ran.error.as_deref(), Some("ended by signal 9"));
    }
}
