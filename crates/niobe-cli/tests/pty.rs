// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The binary in a real pty, and what it does when that pty goes away.
//!
//! The shell only opens on a terminal, so a terminal is what these tests give
//! it: a pty whose other end the test holds like a terminal emulator would —
//! reading everything drawn on it, so the shell is never blocked writing into a
//! full one — and then closes. That is the case no signal reports: a process
//! outside the session that owns a terminal is not sent SIGHUP when it goes,
//! and the hangup on the descriptor is the only thing that says so.
//!
//! A test binary of its own, because it measures how long the shell takes to
//! notice; tests that draw or replay in the same binary would be measured
//! with it.

#![cfg(unix)]
#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::ffi::OsStr;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use niobe_store::{SessionId, Store};
use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::pty::OpenptFlags;
use rustix::termios::Winsize;

/// How long the shell may take to notice that its terminal has gone.
const DEADLINE: Duration = Duration::from_secs(1);

/// How long a test waits for the shell to draw, to record or to end before
/// calling it hung. Long enough that a loaded machine does not fail it, short
/// enough that a hung test is not mistaken for a slow one.
const PATIENCE: Duration = Duration::from_secs(10);

/// How long a wait on the pty blocks before looking at the stop flag again.
const POLL: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 20_000_000,
};

/// Text from the opening frame with no styling inside it to be broken up by
/// escape sequences: once it has been read off the pty, the shell has drawn.
const OPENING_FRAME: &str = "A terminal coding agent that shows you the bill.";

/// The size the pty reports. A pty starts at no size at all, and there is
/// nothing to draw into nothing.
const SIZE: Winsize = Winsize {
    ws_row: 24,
    ws_col: 80,
    ws_xpixel: 0,
    ws_ypixel: 0,
};

/// The end of the pty a terminal emulator holds: everything the shell draws is
/// read off it as it arrives, and typing into it is typing into the shell.
struct Terminal {
    master: Arc<OwnedFd>,
    drawn: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}

impl Terminal {
    /// Opens a pty, and gives back the end the shell is to be run on.
    fn open() -> (Self, File) {
        let master = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)
            .expect("a pty can be opened");
        rustix::pty::grantpt(&master).expect("the slave can be granted");
        rustix::pty::unlockpt(&master).expect("the slave can be unlocked");
        // macOS `posix_openpt` takes no O_CLOEXEC, so the flag is set after the
        // fact: a shell that inherited this end would be holding its own
        // terminal open and would never see it close.
        rustix::io::fcntl_setfd(&master, rustix::io::FdFlags::CLOEXEC)
            .expect("the master can be closed on exec");

        let name = rustix::pty::ptsname(&master, Vec::new()).expect("the slave has a name");
        let slave = File::options()
            .read(true)
            .write(true)
            .open(OsStr::from_bytes(name.as_bytes()))
            .expect("the slave can be opened");
        // Through the slave: the master of a pty no slave has been opened on is
        // not a terminal to ask about, and both ends share the one size.
        rustix::termios::tcsetwinsize(&slave, SIZE).expect("the pty can be given a size");

        let master = Arc::new(master);
        let drawn = Arc::new(Mutex::new(String::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let reader = std::thread::spawn({
            let master = Arc::clone(&master);
            let drawn = Arc::clone(&drawn);
            let stop = Arc::clone(&stop);
            move || read_until_stopped(&master, &drawn, &stop)
        });

        (
            Self {
                master,
                drawn,
                stop,
                reader: Some(reader),
            },
            slave,
        )
    }

    /// Waits until the shell has drawn `wanted`.
    fn shows(&self, wanted: &str) {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            let drawn = self.drawn.lock().expect("the reader thread did not panic");
            if drawn.contains(wanted) {
                return;
            }
            drop(drawn);
            std::thread::sleep(Duration::from_millis(5));
        }
        let drawn = self.drawn.lock().expect("the reader thread did not panic");
        panic!("the shell never drew {wanted:?}; it drew: {drawn:?}");
    }

    /// Types into the shell.
    fn typed(&self, keys: &[u8]) {
        rustix::io::write(&*self.master, keys).expect("the pty takes what is typed");
    }

    /// Closes the terminal, which is what the shell has to notice.
    fn close(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(reader) = self.reader.take() {
            reader.join().expect("the reader thread did not panic");
        }
        // The last reference: the descriptor closes with it, and that is the
        // hangup.
        assert_eq!(Arc::strong_count(&self.master), 1);
    }
}

/// Reads everything the shell draws until the test stops, or until the shell
/// closes its end.
fn read_until_stopped(master: &OwnedFd, drawn: &Mutex<String>, stop: &AtomicBool) {
    let mut buffer = [0u8; 4096];
    while !stop.load(Ordering::SeqCst) {
        let mut fds = [PollFd::new(master, PollFlags::IN)];
        let ready = rustix::event::poll(&mut fds, Some(&POLL)).unwrap_or(0);
        if ready == 0 {
            continue;
        }
        match rustix::io::read(master, &mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => drawn
                .lock()
                .expect("the test thread did not panic holding the lock")
                .push_str(&String::from_utf8_lossy(&buffer[..read])),
        }
    }
}

/// A directory that is the root of a repository, with nothing recorded in it.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    std::fs::create_dir(dir.path().join(".git")).expect("a .git directory can be made");
    dir
}

/// Opens the shell on `slave`, recording into `cwd`, with no user config: the
/// operator's own would otherwise change what these tests see.
fn shell_on(slave: &File, cwd: &Path) -> Child {
    let stdin = slave.try_clone().expect("the slave can be duplicated");
    let stdout = slave.try_clone().expect("the slave can be duplicated");
    Command::new(env!("CARGO_BIN_EXE_niobe"))
        .current_dir(cwd)
        .env("XDG_CONFIG_HOME", cwd.join("no-user-config-here"))
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::null())
        .spawn()
        .expect("the niobe binary runs")
}

/// Waits for the shell to end, and says how long it took from the call.
fn ended(shell: &mut Child) -> Duration {
    let started = Instant::now();
    while started.elapsed() < PATIENCE {
        if shell
            .try_wait()
            .expect("the shell can be asked whether it has ended")
            .is_some()
        {
            return started.elapsed();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = shell.kill();
    panic!("the shell was still running {PATIENCE:?} after its terminal went away");
}

/// Waits until the session being recorded in `root` holds `count` events.
fn recorded(root: &Path, count: usize) {
    let db = root.join(".niobe").join("sessions.db");
    let session: SessionId = "1".parse().expect("1 is a session id");
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if let Ok(store) = Store::open(&db)
            && let Ok(events) = store.events(session)
            && events.len() >= count
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "the shell never recorded {count} event(s) in {}",
        root.display()
    );
}

/// Runs the binary in `cwd` with standard output on a pipe, which is what makes
/// `--resume` print the fold instead of opening the shell.
fn niobe(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_niobe"))
        .args(args)
        .current_dir(cwd)
        .env("XDG_CONFIG_HOME", cwd.join("no-user-config-here"))
        .output()
        .expect("the niobe binary runs")
}

#[test]
fn the_shell_ends_when_the_terminal_it_draws_on_goes_away() {
    let repo = repo();
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows(OPENING_FRAME);

    // The shell holds descriptors of its own, so the test's copy of the slave
    // goes too: the hangup is what closing the last master does.
    drop(slave);
    terminal.close();

    let took = ended(&mut shell);
    assert!(
        took < DEADLINE,
        "the shell took {took:?} to notice that its terminal had gone, over the {DEADLINE:?} it has"
    );
}

#[test]
fn a_session_whose_terminal_went_away_is_still_there_to_resume() {
    let repo = repo();
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows(OPENING_FRAME);

    terminal.typed(b"etags please\r");
    recorded(repo.path(), 1);

    drop(slave);
    terminal.close();
    ended(&mut shell);

    let resumed = niobe(repo.path(), &["--resume", "1"]);
    let out = String::from_utf8(resumed.stdout).expect("stdout is UTF-8");
    assert!(resumed.status.success(), "{out}");
    assert!(out.starts_with("session 1 · 1 events"), "{out}");
    assert!(out.contains("1 from you"), "{out}");

    // The prompt itself, which the list shows a session by.
    let listed = niobe(repo.path(), &["sessions"]);
    let listed = String::from_utf8(listed.stdout).expect("stdout is UTF-8");
    assert!(listed.contains("etags please"), "{listed}");
}
