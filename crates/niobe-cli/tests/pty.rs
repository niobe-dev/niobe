// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The binary in a real pty: how it gives that terminal back, and what it does
//! when the terminal goes away.
//!
//! The shell only opens on a terminal, so a terminal is what these tests give
//! it: a pty whose other end the test holds like a terminal emulator would —
//! reading everything drawn on it, so the shell is never blocked writing into a
//! full one — and typing into it.
//!
//! Three of these drive the shell down each way out and read the bytes that
//! came back: a clean quit, restored by the guard; a SIGTERM, restored by the
//! guard after the flag the handler sets ends the loop; and a panic, restored
//! by the panic hook while the stack unwinds. Nothing short of a real terminal
//! proves the last two — the hook writes to the process's own standard output,
//! and the signal disposition belongs to the process.
//!
//! The fourth case is the terminal closing under the shell, which no signal
//! reports: a process outside the session that owns a terminal is not sent
//! SIGHUP when it goes, and the hangup on the descriptor is the only thing that
//! says so.
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
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use niobe_store::{SessionId, Store};
use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::process::{Pid, Signal};
use rustix::pty::OpenptFlags;
use rustix::termios::Winsize;

/// How long the shell may take to act on its terminal going away, or on a
/// SIGTERM. Both are noticed once per tick of the event loop.
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

/// Entering the mode the shell draws in: the alternate screen. Leaving it
/// proves nothing unless the shell entered it first.
const ENTER_ALTERNATE_SCREEN: &str = "\x1b[?1049h";

/// Leaving it. Counted rather than looked for, so that a terminal handed back
/// twice fails as well as one never handed back at all.
const LEAVE_ALTERNATE_SCREEN: &str = "\x1b[?1049l";

/// The terminal handed back: off the alternate screen and the cursor visible,
/// in that order. The shell writes the two from one place, so they arrive as
/// one run of bytes, and a restoration that stopped in the middle — a visible
/// prompt with no cursor on it — is not this.
const RESTORED: &str = "\x1b[?1049l\x1b[?25h";

/// Ctrl+Q, one of the two keys that quit the shell.
const CTRL_Q: &[u8] = b"\x11";

/// The variable the binary reads to panic inside the event loop, which is the
/// only way in to the one exit path nothing else can reach. The binary compiles
/// it in only with debug assertions on, which is how `cargo test` builds it.
const PANIC_ON_RECORD: &str = "NIOBE_TEST_PANIC_ON_RECORD";

/// What a Rust process that panicked exits with.
const PANICKED: i32 = 101;

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

    /// Waits for the shell's end of the pty to close, and gives back
    /// everything that was ever drawn on it.
    ///
    /// The reader stops of its own accord at the hangup, so the sequences the
    /// shell wrote on its way out are all in: the stop flag is only how the
    /// wait ends if the hangup never comes.
    fn drained(mut self) -> String {
        let reader = self.reader.take().expect("the reader thread is still held");
        let deadline = Instant::now() + PATIENCE;
        while !reader.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.stop.store(true, Ordering::SeqCst);
        reader.join().expect("the reader thread did not panic");
        self.drawn
            .lock()
            .expect("the reader thread did not panic")
            .clone()
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

/// The shell, ready to run on `slave` and record into `cwd`, with no user
/// config: the operator's own would otherwise change what these tests see.
///
/// Standard error goes to the terminal too, the way it does for an operator:
/// where a panic's message lands relative to the restoration is the whole
/// point of the hook that restores.
fn shell_command(slave: &File, cwd: &Path) -> Command {
    let stdin = slave.try_clone().expect("the slave can be duplicated");
    let stdout = slave.try_clone().expect("the slave can be duplicated");
    let stderr = slave.try_clone().expect("the slave can be duplicated");
    let mut command = Command::new(env!("CARGO_BIN_EXE_niobe"));
    command
        .current_dir(cwd)
        .env("XDG_CONFIG_HOME", cwd.join("no-user-config-here"))
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    command
}

/// Opens the shell on `slave`.
fn shell_on(slave: &File, cwd: &Path) -> Child {
    shell_command(slave, cwd)
        .spawn()
        .expect("the niobe binary runs")
}

/// Sends `signal` to the shell.
fn signal(shell: &Child, signal: Signal) {
    let raw = i32::try_from(shell.id()).expect("a pid fits in an i32");
    let pid = Pid::from_raw(raw).expect("the shell has a pid");
    rustix::process::kill_process(pid, signal).expect("the shell can be signalled");
}

/// Asserts that the shell handed the terminal back exactly once on `path`, the
/// way out of the shell it was driven down.
fn assert_handed_back(drawn: &str, path: &str) {
    assert!(
        drawn.contains(ENTER_ALTERNATE_SCREEN),
        "on {path} the shell never entered the alternate screen, so leaving it would prove nothing"
    );
    let left = drawn.matches(LEAVE_ALTERNATE_SCREEN).count();
    assert_eq!(
        left, 1,
        "on {path} the shell left the alternate screen {left} time(s), not once"
    );
    assert!(
        drawn.contains(RESTORED),
        "on {path} the shell left the alternate screen without showing the cursor: {drawn:?}"
    );
}

/// Waits for the shell to end, and says how long it took from the call and how
/// it ended.
fn ended(shell: &mut Child) -> (Duration, ExitStatus) {
    let started = Instant::now();
    while started.elapsed() < PATIENCE {
        if let Some(status) = shell
            .try_wait()
            .expect("the shell can be asked whether it has ended")
        {
            return (started.elapsed(), status);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = shell.kill();
    panic!("the shell was still running {PATIENCE:?} after it was asked to end");
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
fn a_clean_quit_hands_the_terminal_back() {
    let repo = repo();
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows(OPENING_FRAME);

    terminal.typed(CTRL_Q);

    let (_, status) = ended(&mut shell);
    drop(slave);
    let drawn = terminal.drained();

    assert!(status.success(), "a clean quit ended with {status}");
    assert_handed_back(&drawn, "a clean quit");
}

#[test]
fn a_sigterm_hands_the_terminal_back() {
    let repo = repo();
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows(OPENING_FRAME);

    signal(&shell, Signal::TERM);

    let (took, status) = ended(&mut shell);
    drop(slave);
    let drawn = terminal.drained();

    assert!(
        took < DEADLINE,
        "the shell took {took:?} to act on a SIGTERM, over the {DEADLINE:?} it has"
    );
    // The default disposition kills the process outright, on the alternate
    // screen in raw mode; catching the signal is what turns it into a quit.
    assert!(status.success(), "a SIGTERM ended the shell with {status}");
    assert_handed_back(&drawn, "a SIGTERM");
}

/// Only with debug assertions on: the panic the binary is asked for is
/// compiled into no other build, and `cargo test --release` builds both this
/// and the binary without it.
#[cfg(debug_assertions)]
#[test]
fn a_panic_hands_the_terminal_back() {
    let repo = repo();
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_command(&slave, repo.path())
        .env(PANIC_ON_RECORD, "1")
        .spawn()
        .expect("the niobe binary runs");
    terminal.shows(OPENING_FRAME);

    // Sending a prompt is what reaches the journal, and the journal is what
    // panics: inside the event loop, with the terminal still in the drawing
    // mode and the guard still held.
    terminal.typed(b"anything\r");

    let (_, status) = ended(&mut shell);
    drop(slave);
    let drawn = terminal.drained();

    assert_eq!(
        status.code(),
        Some(PANICKED),
        "the shell was asked to panic and ended with {status}"
    );
    assert_handed_back(&drawn, "a panic");

    // The guard would restore on its own as the stack unwinds, so restoring is
    // not what the hook is for: it restores *first*, so that the message is
    // printed to a terminal that is still there. Printed the other way round it
    // goes onto the alternate screen, which is then torn down, and the operator
    // is left with a working prompt and no reason why their session ended.
    let restored = drawn
        .find(RESTORED)
        .expect("the shell handed the terminal back");
    let message = drawn
        .find(PANIC_ON_RECORD)
        .expect("the panic message reached the terminal");
    assert!(
        restored < message,
        "on a panic the message was printed before the terminal was handed back, so it went onto the alternate screen and was torn down with it"
    );
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

    let (took, _) = ended(&mut shell);
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
