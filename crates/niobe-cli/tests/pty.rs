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
//! came back: a clean quit, restored by the guard; a SIGTERM, SIGINT or
//! SIGQUIT, restored by the guard after the flag the handler sets ends the loop,
//! which also ends the `!` commands still running; and a panic, restored
//! by the panic hook while the stack unwinds. Nothing short of a real terminal
//! proves the last two — the hook writes to the process's own standard output,
//! and the signal disposition belongs to the process.
//!
//! A clean quit is also driven on terminals with no room to draw on — one
//! cell, and no size at all — and on one whose `TERM` says `dumb`: each still
//! ends with status 0 and gives the terminal back.
//!
//! A stop is not a way out, but it hands the terminal over all the same: a
//! SIGTSTP, or Ctrl+Z read as a key, hands it back before the process stops,
//! and a SIGCONT takes it again and draws the whole frame.
//!
//! The fourth case is the terminal closing under the shell, and the shell has
//! three ways of finding out. A process outside the session that owns a
//! terminal is not sent SIGHUP when it goes, so for these tests the hangup on
//! the descriptor is what says so; the draw at the top of the next tick failing
//! is the other side of that race, arranged by drawing on a terminal that is
//! not the one being waited on; and SIGHUP is the way an operator's own shell
//! hears it, sent here on a terminal that is still there to be read. What the
//! shell draws cannot be read once the terminal has gone, so the first two read
//! the session out of the store instead, and the exit status — a session that
//! lost its terminal ended; it did not fail, and a wrapper reads that here.
//!
//! The last case needs no shell, and no terminal either: a failure reported
//! onto a standard error that cannot be written. That is what a terminal
//! closing under a session leaves behind, and what reporting does with it
//! decides whether the operator is left with a reason, a status, or a crash.
//! The stream it is arranged on is not a pty, for a reason that test carries.
//!
//! What a session started is not left behind on any of these ways out,
//! SIGKILL included: the CLI's process group, and each `!` command's group
//! even after its `sh` has ended with something still running in the
//! background.
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
use std::net::Shutdown;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
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

/// How long a test waits, once the shell has drawn what a line typed into it
/// did, before pressing Enter, so that the shell reads the two apart as it
/// would a person's keys. Far longer than a debug build takes to read a key.
const KEY_GAP: Duration = Duration::from_millis(100);

/// How long a test waits for the shell to draw, to record or to end before
/// calling it hung.
///
/// Every wait returns as soon as what it waits for happens, so a passing test
/// waits no longer for this being long; a hung one never gets there either
/// way. With two whole-workspace test runs sharing a 12-core Mac (load
/// average 17), the opening frame came after 0.12 s at the median and 11.06 s
/// at worst — the binary starting, not the harness — and a shell ended within
/// 9.27 s of being asked; ten seconds failed there.
const PATIENCE: Duration = Duration::from_secs(60);

/// How long a wait on the pty blocks before looking at the stop flag again.
const POLL: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 20_000_000,
};

/// Text from the opening frame with no styling inside it to be broken up by
/// escape sequences: once it has been read off the pty, the shell has drawn.
const OPENING_FRAME: &str = "A terminal coding agent that keeps you aware of what is";

/// Entering the mode the shell draws in: the alternate screen. Leaving it
/// proves nothing unless the shell entered it first.
const ENTER_ALTERNATE_SCREEN: &str = "\x1b[?1049h";

/// Leaving it. Counted rather than looked for, so that a terminal handed back
/// twice fails as well as one never handed back at all.
const LEAVE_ALTERNATE_SCREEN: &str = "\x1b[?1049l";

/// The terminal handed back: the mouse no longer reported, a paste no longer
/// bracketed, the keyboard enhancement popped, off the alternate screen and
/// the cursor visible, in that order. The shell writes them from one place, so
/// they arrive as one run of bytes, and a restoration that stopped in the
/// middle — a prompt that prints escape sequences at every click or around
/// every paste, reads Enter as `CSI 13 u`, or has no cursor on it — is not
/// this.
///
/// The terminals these tests open answer as one that reports keys does, so
/// this is what every way out has to leave behind.
const RESTORED: &str = "\x1b[?1006l\x1b[?1000l\x1b[?2004l\x1b[<1u\x1b[?1049l\x1b[?25h";

/// The same, on a terminal that cannot report keys: it was never pushed the
/// enhancement, so it is never asked to pop it, and the modifyOtherKeys it was
/// asked for instead goes back to what the terminal had.
const RESTORED_LEGACY: &str = "\x1b[?1006l\x1b[?1000l\x1b[?2004l\x1b[>4m\x1b[?1049l\x1b[?25h";

/// What the shell asks a terminal on the way in: which keyboard enhancements
/// it has on, then its primary device attributes.
const KEYBOARD_QUERY: &str = "\x1b[?u\x1b[c";

/// The shell asking the terminal to tell Shift+Enter from Enter, and taking
/// that back.
const KEYBOARD_ON: &str = "\x1b[>1u";
const KEYBOARD_OFF: &str = "\x1b[<1u";

/// The shell asking a terminal that did not answer the flags for xterm's
/// modifyOtherKeys, which is what tmux reports Shift+Enter under, and taking
/// that back.
const OTHER_KEYS_ON: &str = "\x1b[>4;1m";
const OTHER_KEYS_OFF: &str = "\x1b[>4m";

/// Shift+Enter as xterm's modifyOtherKeys sends it, and tmux with
/// `extended-keys` on once asked, or set to `always`.
const SHIFT_ENTER_OTHER_KEYS: &[u8] = b"\x1b[27;2;13~";

/// How the terminal a test opens answers [`KEYBOARD_QUERY`].
#[derive(Clone, Copy)]
enum Keys {
    /// As one that knows the kitty keyboard protocol: its flags, none on yet,
    /// then its attributes.
    Reported,
    /// As one that does not: its attributes alone.
    Legacy,
}

impl Keys {
    fn answer(self) -> &'static [u8] {
        match self {
            Self::Reported => b"\x1b[?0u\x1b[?62;22c",
            Self::Legacy => b"\x1b[?62;22c",
        }
    }

    /// What every way out of the shell has to leave behind on this terminal.
    fn restored(self) -> &'static str {
        match self {
            Self::Reported => RESTORED,
            Self::Legacy => RESTORED_LEGACY,
        }
    }

    /// The sequence that takes back what the shell asked this terminal for.
    fn taken_back(self) -> &'static str {
        match self {
            Self::Reported => KEYBOARD_OFF,
            Self::Legacy => OTHER_KEYS_OFF,
        }
    }
}

/// Ctrl+Q, one of the two keys that quit the shell.
const CTRL_Q: &[u8] = b"\x11";

/// F9, the key that moves the shell to the next theme. A terminal sends it as
/// this sequence, which is what proves the key reaches the shell as F9 rather
/// than as the characters it is spelt with.
const F9: &[u8] = b"\x1b[20~";

/// The variable the binary reads to panic inside the event loop, which is the
/// only way in to the one exit path nothing else can reach. The binary compiles
/// it in only with debug assertions on, which is how `cargo test` builds it.
const PANIC_ON_RECORD: &str = "NIOBE_TEST_PANIC_ON_RECORD";

/// What a Rust process that panicked exits with.
const PANICKED: i32 = 101;

/// What the binary exits with when a failure could not be written to standard
/// error: the reason is lost, and the status is what is left to say so.
const FAILED_UNREPORTED: i32 = 2;

/// The size the pty reports. A pty starts at no size at all, and there is
/// nothing to draw into nothing.
const SIZE: Winsize = Winsize {
    ws_row: 24,
    ws_col: 80,
    ws_xpixel: 0,
    ws_ypixel: 0,
};

/// A size with room on the bar for the key that opens a line: at [`SIZE`] the
/// bar keeps only its first hint.
const WIDE: Winsize = Winsize {
    ws_row: 60,
    ws_col: 200,
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
    /// Opens a pty on a terminal that reports keys, and gives back the end
    /// the shell is to be run on.
    fn open() -> (Self, File) {
        Self::answering(Keys::Reported)
    }

    /// Opens a pty on a terminal that answers the keyboard query as `keys`
    /// says, and gives back the end the shell is to be run on.
    fn answering(keys: Keys) -> (Self, File) {
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
            move || read_until_stopped(&master, &drawn, &stop, keys)
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
        self.shows_since(0, wanted);
    }

    /// How much the shell has drawn so far, as a place to read on from.
    fn mark(&self) -> usize {
        self.drawn
            .lock()
            .expect("the reader thread did not panic")
            .len()
    }

    /// Waits until the shell has drawn `wanted` after `from`, a [`Self::mark`].
    fn shows_since(&self, from: usize, wanted: &str) {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            let drawn = self.drawn.lock().expect("the reader thread did not panic");
            if drawn[from..].contains(wanted) {
                return;
            }
            drop(drawn);
            std::thread::sleep(Duration::from_millis(5));
        }
        // Copied out before panicking: a panic with the lock held poisons it,
        // and the reader's panic on that would bury this message in the log.
        let after = self.drawn.lock().expect("the reader thread did not panic")[from..].to_owned();
        panic!("the shell never drew {wanted:?}; after the mark it drew: {after:?}");
    }

    /// Everything the shell has written so far, once the reader has taken all
    /// of it off the pty.
    ///
    /// For a shell that can write no more — stopped, say — this is all it
    /// wrote. What the reader has kept so far may not be: a stopped process
    /// has put its bytes on the pty, but on a loaded machine the reader is
    /// often not yet scheduled to take them off.
    fn caught_up(&self) -> String {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            let drawn = self.drawn.lock().expect("the reader thread did not panic");
            let mut fds = [PollFd::new(&*self.master, PollFlags::IN)];
            let unread = rustix::event::poll(&mut fds, Some(&Timespec::default()))
                .expect("the pty can be polled");
            if unread == 0 {
                return drawn.clone();
            }
            drop(drawn);
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the pty still held unread output {PATIENCE:?} later");
    }

    /// Types into the shell.
    ///
    /// A line ending in Enter is typed, and then the Enter pressed on its own
    /// once the shell has had time to read the line: sent in one write, the
    /// two arrive in one read, which is what a paste from a terminal that
    /// does not bracket pastes looks like, and a pasted Enter is a line break.
    fn typed(&self, keys: &[u8]) {
        match keys.split_last() {
            Some((b'\r', line)) if !line.is_empty() => {
                let before = self.mark();
                self.write(line);
                self.drew_since(before);
                std::thread::sleep(KEY_GAP);
                self.write(b"\r");
            }
            _ => self.write(keys),
        }
    }

    /// Waits until the shell has drawn anything after `from`, a
    /// [`Self::mark`], which it does once it has read what was typed.
    fn drew_since(&self, from: usize) {
        let deadline = Instant::now() + PATIENCE;
        while self.mark() <= from {
            assert!(
                Instant::now() < deadline,
                "the shell drew nothing {PATIENCE:?} after keys were typed"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn write(&self, keys: &[u8]) {
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
/// closes its end, answering the keyboard query the first time it is asked, as
/// a terminal emulator would.
fn read_until_stopped(master: &OwnedFd, drawn: &Mutex<String>, stop: &AtomicBool, keys: Keys) {
    let mut buffer = [0u8; 4096];
    let mut answered = false;
    while !stop.load(Ordering::SeqCst) {
        let mut fds = [PollFd::new(master, PollFlags::IN)];
        let ready = rustix::event::poll(&mut fds, Some(&POLL)).unwrap_or(0);
        if ready == 0 {
            continue;
        }
        // Read under the lock the bytes are kept under, so that a test holding
        // it and finding the pty empty knows everything written is in `drawn`
        // (see `Terminal::caught_up`). Poll said there is something to read,
        // so the read does not block with the lock held.
        let mut drawn = drawn
            .lock()
            .expect("the test thread did not panic holding the lock");
        match rustix::io::read(master, &mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                drawn.push_str(&String::from_utf8_lossy(&buffer[..read]));
                if !answered && drawn.contains(KEYBOARD_QUERY) {
                    answered = true;
                    // A terminal that has gone takes no answer, and a test
                    // that closes it is not failed by that.
                    let _ = rustix::io::write(master, keys.answer());
                }
            }
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
/// config and no `COLORTERM`: the operator's own config, and the depth their
/// terminal announces, would otherwise change what these tests see.
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
        .env_remove("COLORTERM")
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

/// Opens the shell reading from one terminal and drawing on another.
///
/// The shell treats them as one: it waits on standard input and draws on
/// standard output, and for an operator those are the same device. Two ptys
/// separate the two ways the shell can find out that device has gone, so that
/// a test can close one of them and leave the other with nothing to report.
fn shell_reading_from(input: &File, screen: &File, cwd: &Path) -> Child {
    let stdin = input.try_clone().expect("the slave can be duplicated");
    let stdout = screen.try_clone().expect("the slave can be duplicated");
    let stderr = screen.try_clone().expect("the slave can be duplicated");
    Command::new(env!("CARGO_BIN_EXE_niobe"))
        .current_dir(cwd)
        .env("XDG_CONFIG_HOME", cwd.join("no-user-config-here"))
        .env_remove("COLORTERM")
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
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
fn assert_handed_back(drawn: &str, cooked: bool, path: &str) {
    assert_handed_back_as(drawn, cooked, path, Keys::Reported);
}

/// Everything the shell drew, once the test lets go of its end of the pty,
/// and whether the pty's line discipline was cooked again when it did: the
/// escape sequences say the screen was handed back, and only the modes say
/// raw mode was turned off.
fn released(terminal: Terminal, slave: File) -> (String, bool) {
    let cooked = cooked(&slave);
    drop(slave);
    (terminal.drained(), cooked)
}

/// Asserts that the shell handed back exactly once, on `path`, a terminal that
/// answered the keyboard query as `keys` says.
fn assert_handed_back_as(drawn: &str, cooked: bool, path: &str, keys: Keys) {
    assert!(
        cooked,
        "on {path} the shell left the terminal in raw mode: no line editing, no echo, no Ctrl+C"
    );
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
        drawn.contains(keys.restored()),
        "on {path} the shell left the alternate screen without taking the keyboard back or \
         showing the cursor: {drawn:?}"
    );
    assert_eq!(
        drawn.matches(keys.taken_back()).count(),
        1,
        "on {path} what the keyboard was asked for was not taken back exactly once: {drawn:?}"
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
    let mut command = Command::new(env!("CARGO_BIN_EXE_niobe"));
    command
        .args(args)
        .current_dir(cwd)
        .env("XDG_CONFIG_HOME", cwd.join("no-user-config-here"));
    command.output().expect("the niobe binary runs")
}

/// The transcript fixture the bridge folds, and the id the CLI calls it by.
const TRANSCRIPT: &str = "../niobe-bridge-claude/tests/fixtures/transcripts/\
                          2f6c1e10-8f4b-4d2a-9c3e-7a5b0d1e6f42.jsonl";
const TRANSCRIPT_SESSION: &str = "2f6c1e10-8f4b-4d2a-9c3e-7a5b0d1e6f42";

/// A configuration directory for the `claude` CLI holding that transcript as a
/// session it recorded in `cwd`, in the layout the CLI writes.
///
/// The name is made from the working directory as a process sees it, which is
/// the resolved one: a temporary directory on macOS is reached through a
/// symlink, and both binaries run in the directory behind it.
fn claude_config_with_the_transcript(cwd: &Path) -> tempfile::TempDir {
    let config = tempfile::tempdir().expect("a temporary directory can be created");
    let cwd = std::fs::canonicalize(cwd).expect("the working directory resolves");
    // The CLI keeps ASCII letters and digits and makes a `-` of the rest; a
    // temporary directory's name is ASCII, so one character is one dash.
    let flattened: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = config.path().join("projects").join(flattened);
    std::fs::create_dir_all(&dir).expect("the project directory can be made");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(TRANSCRIPT),
        dir.join(format!("{TRANSCRIPT_SESSION}.jsonl")),
    )
    .expect("the transcript is copied");
    config
}

#[test]
fn a_claude_session_read_in_is_a_niobe_session_from_then_on() {
    let repo = repo();
    let claude = claude_config_with_the_transcript(repo.path());
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_command(&slave, repo.path())
        .env("CLAUDE_CONFIG_DIR", claude.path())
        .args(["--resume", TRANSCRIPT_SESSION])
        .spawn()
        .expect("the niobe binary runs");

    // The last thing the CLI said, drawn in the shell's own transcript: the
    // history came in, and the shell opened at the end of it.
    terminal.shows("Done: the catalog response carries an etag.");
    // The thirteen events the transcript folds to, in the store.
    recorded(repo.path(), 13);
    terminal.typed(CTRL_Q);

    let (_, status) = ended(&mut shell);
    drop(slave);
    let drawn = terminal.drained();

    assert!(status.success(), "the imported session ended with {status}");
    assert!(
        drawn.contains("niobe session 1"),
        "the shell did not say which session it became:\n{drawn}"
    );
    // From here on it is Niobe's, resumed by its number, and folds to what the
    // transcript did.
    let resumed = niobe(repo.path(), &["--resume", "1"]);
    let out = String::from_utf8(resumed.stdout).expect("stdout is UTF-8");
    assert!(resumed.status.success());
    assert!(
        out.contains("905 in · 75 out · 2,100 cache read · 150 cache write"),
        "{out}"
    );
    assert!(
        out.contains("2 from you · 2 from the agent"),
        "the operator's own turns came in with the rest:\n{out}"
    );
    assert!(out.contains("1 changed — +3 −1"), "{out}");
}

/// A resume whose backend will not start is a session nobody carried on, and
/// is not left in the list: every retry would add another copy.
#[test]
fn a_claude_session_whose_backend_will_not_start_is_not_recorded() {
    let repo = repo();
    let claude = claude_config_with_the_transcript(repo.path());
    let home = user_config("default_profile = \"max\"\n\n[profiles.max]\nbackend = \"claude\"\n");
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_command(&slave, repo.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("CLAUDE_CONFIG_DIR", claude.path())
        .env("PATH", "/usr/bin:/bin")
        .args(["--resume", TRANSCRIPT_SESSION])
        .spawn()
        .expect("the niobe binary runs");

    let (_, status) = ended(&mut shell);
    drop(slave);
    let drawn = terminal.drained();

    assert_eq!(status.code(), Some(1), "{drawn}");
    assert!(drawn.contains("not on PATH"), "{drawn}");
    let listed = niobe(repo.path(), &["sessions"]);
    let out = String::from_utf8(listed.stdout).expect("stdout is UTF-8");
    assert!(
        !out.contains("EVENTS"),
        "the failed resume left a session behind:\n{out}"
    );
}

/// A user config directory holding `config`, to run the shell under.
fn user_config(config: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("a temporary directory can be created");
    std::fs::create_dir(home.path().join("niobe")).expect("a niobe config directory");
    std::fs::write(home.path().join("niobe").join("config.toml"), config)
        .expect("the user config is written");
    home
}

#[test]
fn a_settings_file_a_profile_names_and_this_machine_has_not_stops_the_session() {
    let repo = repo();
    let missing = repo.path().join("gone.json");
    let home = user_config(&format!(
        "default_profile = \"max\"\n\n[profiles.max]\nbackend = \"claude\"\nsettings = \"{}\"\n",
        missing.display()
    ));
    let (terminal, slave) = Terminal::open();

    let mut shell = shell_command(&slave, repo.path())
        .env("XDG_CONFIG_HOME", home.path())
        // Nothing of the operator's is started by a test: with no `claude` to
        // find, a run that got as far as spawning one would say that instead,
        // rather than open a session on whatever this machine is signed in as.
        .env("PATH", repo.path().join("nothing-here"))
        .spawn()
        .expect("the niobe binary runs");

    let (_, status) = ended(&mut shell);
    drop(slave);
    let drawn = terminal.drained();

    assert_eq!(status.code(), Some(1), "{drawn}");
    assert!(
        drawn.contains(&format!(
            "profiles.max.settings: no such file: {}",
            missing.display()
        )),
        "the failure names the key, the line and the path: {drawn}"
    );
    assert!(
        !drawn.contains("is not on PATH"),
        "the CLI was reached for before its settings were looked at: {drawn}"
    );
    // The shell never took the terminal, so there is nothing to hand back and
    // nothing was recorded here.
    assert!(!repo.path().join(".niobe").exists(), "{drawn}");
}

/// What a palette writes when it opens the menu row: its own text colour on
/// its own background, as ratatui writes an ANSI-256 pair. Nothing on screen
/// names the palette in force, so this is what a choice of one is read off.
const NEO_MENU: &str = "\u{1b}[38;5;2;48;5;0m";
const MODERN_MENU: &str = "\u{1b}[38;5;15;48;5;8m";
const CLASSIC_MENU: &str = "\u{1b}[38;5;0;48;5;7m";

/// The three ways a palette is chosen, on the one screen that can show it: the
/// flag, a config key, and F9 while the session runs.
#[test]
fn a_theme_is_selected_by_the_flag_by_the_config_and_by_f9() {
    let repo = repo();
    let home = user_config("theme = \"neo\"\n");
    let (terminal, slave) = Terminal::open();

    // The config alone: the shell opens on the theme it names.
    let mut shell = shell_command(&slave, repo.path())
        .env("XDG_CONFIG_HOME", home.path())
        .spawn()
        .expect("the niobe binary runs");
    terminal.shows(NEO_MENU);
    terminal.typed(F9);
    // F9 cycles the table in order, and `neo` is not the last of it.
    terminal.shows(MODERN_MENU);
    terminal.typed(CTRL_Q);
    let (_, status) = ended(&mut shell);
    assert!(status.success(), "the shell ended with {status}");

    // The flag beats the config, which is the point of having both.
    let (flagged, flagged_slave) = Terminal::open();
    let mut shell = shell_command(&flagged_slave, repo.path())
        .env("XDG_CONFIG_HOME", home.path())
        .arg("--theme")
        .arg("classic")
        .spawn()
        .expect("the niobe binary runs");
    flagged.shows(CLASSIC_MENU);
    flagged.typed(CTRL_Q);
    let (_, status) = ended(&mut shell);
    assert!(status.success(), "the shell ended with {status}");

    drop(slave);
    drop(flagged_slave);
    terminal.drained();
    flagged.drained();
}

/// `neo`'s menu row as its design draws it, in 24-bit colour.
const NEO_MENU_TRUECOLOR: &str = "\u{1b}[38;2;0;255;65;48;2;0;26;8m";

/// A terminal that says it draws 24-bit colour gets a designed theme in its
/// own colours, and `classic` still in the sixteen the terminal names.
#[test]
fn a_terminal_announcing_truecolor_gets_the_designed_colours_and_classic_stays_named() {
    let repo = repo();
    let (terminal, slave) = Terminal::open();

    let mut shell = shell_command(&slave, repo.path())
        .env("COLORTERM", "truecolor")
        .arg("--theme")
        .arg("neo")
        .spawn()
        .expect("the niobe binary runs");
    terminal.shows(NEO_MENU_TRUECOLOR);
    // neo → modern → cyber → classic.
    terminal.typed(F9);
    terminal.typed(F9);
    terminal.typed(F9);
    terminal.shows(CLASSIC_MENU);
    terminal.typed(CTRL_Q);
    let (_, status) = ended(&mut shell);
    assert!(status.success(), "the shell ended with {status}");

    drop(slave);
    terminal.drained();
}

#[test]
fn a_config_naming_a_theme_that_does_not_exist_stops_the_session_at_its_line() {
    let repo = repo();
    let home = user_config("\n\ntheme = \"matrix\"\n");
    let (terminal, slave) = Terminal::open();

    let mut shell = shell_command(&slave, repo.path())
        .env("XDG_CONFIG_HOME", home.path())
        .spawn()
        .expect("the niobe binary runs");

    let (_, status) = ended(&mut shell);
    drop(slave);
    let drawn = terminal.drained();

    assert_eq!(status.code(), Some(1), "{drawn}");
    assert!(
        drawn.contains("config.toml:3: theme: `matrix` is not a theme"),
        "the failure names the key, the line and the name: {drawn}"
    );
    assert!(drawn.contains("`classic`"), "{drawn}");
    // The shell never took the terminal, so a palette nobody has is not a
    // session opened in the default one.
    assert!(!repo.path().join(".niobe").exists(), "{drawn}");
}

/// Starts the shell in `repo` with `planted` a link at `.niobe/<name>`, and
/// returns what it drew and how it ended.
#[cfg(unix)]
fn started_with_a_link(repo: &Path, name: &str, planted: &Path) -> (ExitStatus, String) {
    std::fs::create_dir_all(repo.join(".niobe")).expect("the directory can be made");
    std::os::unix::fs::symlink(planted, repo.join(".niobe").join(name))
        .expect("the link is planted");
    let (terminal, slave) = Terminal::open();

    let mut shell = shell_command(&slave, repo)
        .spawn()
        .expect("the niobe binary runs");

    let (_, status) = ended(&mut shell);
    drop(slave);
    (status, terminal.drained())
}

#[cfg(unix)]
#[test]
fn a_session_store_linked_out_of_the_repository_stops_the_session_and_writes_nothing_there() {
    for (name, exists) in [
        ("sessions.db", false),
        ("sessions.db", true),
        (".gitignore", false),
        ("sessions.db-wal", false),
        ("sessions.db-shm", false),
        ("sessions.db-journal", false),
    ] {
        let repo = repo();
        let outside = tempfile::tempdir().expect("a temporary directory can be created");
        let planted = outside.path().join("leak");
        if exists {
            std::fs::write(&planted, "").expect("the file outside is written");
        }

        let (status, drawn) = started_with_a_link(repo.path(), name, &planted);

        assert_eq!(status.code(), Some(1), "{name}: {drawn}");
        assert!(drawn.contains(name), "the failure names the link: {drawn}");
        let written = std::fs::read(&planted).unwrap_or_default();
        assert!(written.is_empty(), "{name}: niobe wrote through the link");
        assert_eq!(
            std::fs::read_dir(outside.path()).expect("lists").count(),
            usize::from(exists),
            "{name}: niobe created a file outside the repository"
        );
    }
}

#[test]
fn a_clean_quit_hands_the_terminal_back() {
    let repo = repo();
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows(OPENING_FRAME);

    terminal.typed(CTRL_Q);

    let (_, status) = ended(&mut shell);
    let (drawn, cooked) = released(terminal, slave);

    assert!(status.success(), "a clean quit ended with {status}");
    assert_handed_back(&drawn, cooked, "a clean quit");
}

/// Quits a shell started on a terminal of `size` rows and columns, and
/// asserts that it ended cleanly and handed the terminal back.
///
/// Such a terminal has no room for the opening frame, so what the test waits
/// for is the alternate screen: raw mode is on before it is entered, so the
/// quit that follows is read as a key rather than eaten by the line
/// discipline.
fn quits_cleanly_at(rows: u16, cols: u16) {
    let repo = repo();
    let (terminal, slave) = Terminal::open();
    let size = Winsize {
        ws_row: rows,
        ws_col: cols,
        ..SIZE
    };
    rustix::termios::tcsetwinsize(&slave, size).expect("the pty can be resized");
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows(ENTER_ALTERNATE_SCREEN);

    terminal.typed(CTRL_Q);

    let (_, status) = ended(&mut shell);
    let (drawn, cooked) = released(terminal, slave);

    let path = format!("a quit on a {rows}×{cols} terminal");
    assert!(status.success(), "{path} ended with {status}");
    assert_handed_back(&drawn, cooked, &path);
}

#[test]
fn a_one_by_one_terminal_quits_cleanly_and_is_handed_back() {
    quits_cleanly_at(1, 1);
}

#[test]
fn a_terminal_with_no_size_quits_cleanly_and_is_handed_back() {
    quits_cleanly_at(0, 0);
}

#[test]
fn a_dumb_terminal_quits_cleanly_and_is_handed_back() {
    let repo = repo();
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_command(&slave, repo.path())
        .env("TERM", "dumb")
        .spawn()
        .expect("the niobe binary runs");
    terminal.shows(OPENING_FRAME);

    terminal.typed(CTRL_Q);

    let (_, status) = ended(&mut shell);
    let (drawn, cooked) = released(terminal, slave);

    assert!(
        status.success(),
        "a quit with TERM=dumb ended with {status}"
    );
    assert_handed_back(&drawn, cooked, "a quit with TERM=dumb");
}

#[test]
fn a_sigterm_hands_the_terminal_back() {
    let repo = repo();
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows(OPENING_FRAME);

    signal(&shell, Signal::TERM);

    let (took, status) = ended(&mut shell);
    let (drawn, cooked) = released(terminal, slave);

    assert!(
        took < DEADLINE,
        "the shell took {took:?} to act on a SIGTERM, over the {DEADLINE:?} it has"
    );
    // The default disposition kills the process outright, on the alternate
    // screen in raw mode; catching the signal is what turns it into a quit.
    assert!(status.success(), "a SIGTERM ended the shell with {status}");
    assert_handed_back(&drawn, cooked, "a SIGTERM");
}

#[test]
fn a_sigterm_takes_back_the_modify_other_keys_a_legacy_terminal_was_asked_for() {
    let repo = repo();
    let (terminal, slave) = Terminal::answering(Keys::Legacy);
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows(OPENING_FRAME);

    signal(&shell, Signal::TERM);

    let (_, status) = ended(&mut shell);
    let (drawn, cooked) = released(terminal, slave);

    assert!(status.success(), "a SIGTERM ended the shell with {status}");
    assert_handed_back_as(&drawn, cooked, "a SIGTERM", Keys::Legacy);
}

#[test]
fn a_terminal_that_reports_keys_is_asked_to_tell_shift_enter_and_the_bar_names_it() {
    let repo = repo();
    let (terminal, slave) = Terminal::open();
    rustix::termios::tcsetwinsize(&slave, WIDE).expect("the pty can be resized");
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows("Shift+Enter newline");

    terminal.typed(CTRL_Q);

    let (_, status) = ended(&mut shell);
    drop(slave);
    let drawn = terminal.drained();

    assert!(status.success(), "a clean quit ended with {status}");
    let entered = drawn
        .find(ENTER_ALTERNATE_SCREEN)
        .expect("the shell entered the alternate screen");
    let pushed = drawn
        .find(KEYBOARD_ON)
        .expect("a terminal that reports keys was never asked to");
    assert!(
        entered < pushed,
        "the keyboard was pushed onto the main screen's stack, which leaving the alternate \
         screen does not pop: {drawn:?}"
    );
    assert!(!drawn.contains("Ctrl+J newline"), "{drawn:?}");
}

#[test]
fn a_terminal_that_cannot_report_keys_is_asked_for_modify_other_keys_and_the_bar_names_ctrl_j() {
    let repo = repo();
    let (terminal, slave) = Terminal::answering(Keys::Legacy);
    rustix::termios::tcsetwinsize(&slave, WIDE).expect("the pty can be resized");
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows("Ctrl+J newline");

    terminal.typed(CTRL_Q);

    let (_, status) = ended(&mut shell);
    drop(slave);
    let drawn = terminal.drained();

    assert!(status.success(), "a clean quit ended with {status}");
    assert!(
        drawn.contains(RESTORED_LEGACY),
        "the terminal was not handed back: {drawn:?}"
    );
    let answered = drawn
        .find(KEYBOARD_QUERY)
        .expect("the terminal was asked what it reports");
    let asked = drawn
        .find(OTHER_KEYS_ON)
        .expect("a terminal that did not answer the flags was never asked for modifyOtherKeys");
    assert!(answered < asked, "{drawn:?}");
    assert!(!drawn.contains(KEYBOARD_ON), "{drawn:?}");
    assert!(
        !drawn.contains(KEYBOARD_OFF),
        "a terminal that was never asked to report keys was sent the sequence that takes it \
         back: {drawn:?}"
    );
    // Nothing has shown Shift+Enter arriving as itself here: tmux asked for
    // modifyOtherKeys still sends the carriage return Enter sends when the
    // terminal it runs in cannot tell the two apart, so naming it would be
    // naming the key that sends the prompt.
    assert!(!drawn.contains("Shift+Enter"), "{drawn:?}");
}

#[test]
fn shift_enter_in_modify_other_keys_opens_a_line_and_the_bar_names_it_from_then_on() {
    let repo = repo();
    let (terminal, slave) = Terminal::answering(Keys::Legacy);
    rustix::termios::tcsetwinsize(&slave, WIDE).expect("the pty can be resized");
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows("Ctrl+J newline");

    // As in the Ctrl+J test: were Shift+Enter dropped or read as Enter, the
    // command would not be the two lines that print this.
    terminal.typed(b"!");
    terminal.shows("the agent does not see");
    terminal.typed(b"printf %s \"joined-$((6*7))");
    terminal.typed(SHIFT_ENTER_OTHER_KEYS);
    terminal.typed(b"y\"\r");
    terminal.shows("joined-42");
    // Out of the `!` mode, whose bar does not name the newline key, and then
    // the key alone: the hint is right-aligned, so the word after it keeps its
    // cells and is never sent again.
    terminal.typed(b"\x7f");
    terminal.shows("Shift+Enter");
    terminal.typed(CTRL_Q);

    let (_, status) = ended(&mut shell);
    let (drawn, cooked) = released(terminal, slave);
    assert!(status.success(), "the shell ended with {status}");
    assert_handed_back_as(&drawn, cooked, "a clean quit", Keys::Legacy);
}

/// Only with debug assertions on: the panic the binary is asked for is
/// compiled into no other build, and `cargo test --release` builds both this
/// and the binary without it.
#[cfg(debug_assertions)]
#[test]
fn a_panic_hands_the_terminal_back() {
    panics_and_hands_back(Keys::Reported);
}

/// The panic hook has no guard to ask what the keyboard was asked for, so what
/// it takes back is proven on both answers.
#[cfg(debug_assertions)]
#[test]
fn a_panic_takes_back_the_modify_other_keys_a_legacy_terminal_was_asked_for() {
    panics_and_hands_back(Keys::Legacy);
}

/// Panics the shell on a terminal that answers the keyboard query as `keys`
/// says, and asserts that the terminal was handed back before the message.
#[cfg(debug_assertions)]
fn panics_and_hands_back(keys: Keys) {
    let repo = repo();
    let (terminal, slave) = Terminal::answering(keys);
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
    let (drawn, cooked) = released(terminal, slave);

    assert_eq!(
        status.code(),
        Some(PANICKED),
        "the shell was asked to panic and ended with {status}"
    );
    assert_handed_back_as(&drawn, cooked, "a panic", keys);

    // The guard would restore on its own as the stack unwinds, so restoring is
    // not what the hook is for: it restores *first*, so that the message is
    // printed to a terminal that is still there. Printed the other way round it
    // goes onto the alternate screen, which is then torn down, and the operator
    // is left with a working prompt and no reason why their session ended.
    let restored = drawn
        .find(keys.restored())
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

#[test]
fn a_shell_whose_terminal_went_away_ends_the_session_rather_than_failing() {
    let repo = repo();
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows(OPENING_FRAME);

    // A session with something in it, so that the shell has the line it prints
    // on the way out to print — to a terminal that is no longer there.
    terminal.typed(b"etags please\r");
    recorded(repo.path(), 1);

    drop(slave);
    terminal.close();

    let (_, status) = ended(&mut shell);
    assert!(
        status.success(),
        "a session that ended because its terminal went away exited with {status}, \
         which tells whatever started niobe that the session failed"
    );
}

/// A failure reported onto a standard error that cannot be written.
///
/// The shell draws at the top of every tick, so a terminal closing under a
/// session can raise an error and take away the stream that error is reported
/// on in the same instant. That instant cannot be arranged from outside the
/// process, and what stands in for it has to be a stream that refuses writes
/// every time it is asked.
///
/// A pty whose master has been closed is not one, however it looks. A process
/// that spawns children cannot keep any descriptor of its own to itself: a
/// child takes a copy of the whole table when it forks and drops the
/// close-on-exec ones only when it execs, so for that moment the master this
/// test closed is still open somewhere and the pty is still connected. A write
/// on the slave then succeeds, the reason is reported after all, and this reads
/// exit 1 where it asked for 2 — a few runs in every hundred with several of
/// these binaries running at once. It is the pty that is unreliable there and
/// not the reporting, which is the worst way for a test on this path to fail.
/// The device is not what is unreliable about it: a pty whose slave is still
/// open is never handed out again, so nothing recycles underneath this.
///
/// A socket shut down for writing refuses every write instead, and refuses it
/// on the strength of its own state rather than of a peer that something else
/// might hold open. Copies inherit the shutdown with the descriptor, so no
/// child can undo it. Which error the write fails with does not matter; what
/// reporting a failure onto a stream that refuses it does is what this reads.
#[test]
fn a_failure_that_cannot_be_reported_ends_as_a_failure_rather_than_a_crash() {
    let repo = repo();
    let (ours, theirs) = UnixStream::pair().expect("a socket pair can be made");
    theirs
        .shutdown(Shutdown::Write)
        .expect("the socket can be shut down for writing");
    // Not what makes the writes fail — the shutdown is, and that travels with
    // the descriptor wherever it is copied. This end is dropped only because
    // the test has no use for it.
    drop(ours);

    // Resuming a session that was never recorded fails before the shell would
    // open, so nothing here needs a terminal to draw on — only a standard
    // error to be reported on, which is the one that refuses.
    let mut shell = Command::new(env!("CARGO_BIN_EXE_niobe"))
        .args(["--resume", "9"])
        .current_dir(repo.path())
        .env("XDG_CONFIG_HOME", repo.path().join("no-user-config-here"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(OwnedFd::from(theirs)))
        .spawn()
        .expect("the niobe binary runs");

    let (_, status) = ended(&mut shell);
    assert_ne!(
        status.code(),
        Some(PANICKED),
        "reporting the failure panicked, which leaves a terminal that closed under a session \
         indistinguishable from a bug in niobe"
    );
    assert_eq!(
        status.code(),
        Some(FAILED_UNREPORTED),
        "a failure whose reason could not be written ended as {status}, which does not say that \
         the reason is missing"
    );
}

/// The terminal closing while the shell is drawing on it, rather than while it
/// waits to be typed into.
///
/// Both are the same closed terminal, and the loop has two ways of finding out:
/// the wait it spends the tick in reports the hangup, and the draw at the top
/// of the next tick fails. The wait wins almost every time, because that is
/// where the tick is spent; the draw finding out first is the other side of a
/// race, a run in a hundred under load, and nothing outside the process can
/// choose which side a close lands on when both ends are one device.
///
/// So the device is split: the shell reads from one pty and draws on another,
/// and only the one it draws on is closed. The wait has nothing to report, the
/// draw is the only thing that can notice, and the side of the race that is
/// otherwise reached by chance is the only side there is. What the session
/// exits with is what this reads — losing the screen it drew on ended the
/// session; it did not fail, and it is not a bug in `niobe` either, whichever
/// way the shell found out.
#[test]
fn a_terminal_that_goes_away_while_the_shell_draws_ends_the_session_rather_than_failing() {
    let repo = repo();
    let (keyboard, typed_into) = Terminal::open();
    let (screen, drawn_on) = Terminal::open();
    let mut shell = shell_reading_from(&typed_into, &drawn_on, repo.path());
    screen.shows(OPENING_FRAME);

    // Only the drawing surface goes. The shell's wait is on the other pty,
    // which is still open and has nothing to say.
    drop(drawn_on);
    screen.close();

    let (took, status) = ended(&mut shell);
    drop(typed_into);
    keyboard.close();

    assert!(
        took < DEADLINE,
        "the shell took {took:?} to notice that the terminal it drew on had gone, \
         over the {DEADLINE:?} it has"
    );
    assert_ne!(
        status.code(),
        Some(PANICKED),
        "the terminal going away mid-draw ended the shell in a panic, which reads as a bug in \
         niobe rather than as a window that closed"
    );
    assert!(
        status.success(),
        "a session that ended because the terminal it drew on went away exited with {status}, \
         which tells whatever started niobe that the session failed"
    );
}

/// A hangup, which is the kernel saying the terminal went away.
///
/// A session that runs in the session owning its terminal is told twice when
/// that terminal closes — SIGHUP from the kernel, and the hangup on the
/// descriptor the wait is in — and which telling arrives first is not something
/// either end chooses. Sent on its own, with the terminal still there to read
/// what happens next, it is that ending arranged where it can be watched.
///
/// The session ends without failing, and the terminal comes back. The line a
/// quit prints afterwards does not, because a hangup names a terminal there is
/// nothing left to print on: read as an ordinary stop, that line is written to
/// a window that has closed, and it is written by a macro that panics when the
/// write fails.
#[test]
fn a_hangup_ends_the_session_as_a_terminal_that_went_away_rather_than_as_a_quit() {
    let repo = repo();
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows(OPENING_FRAME);

    // A session with something in it, so that a quit would have the line to
    // print that a hangup must not.
    terminal.typed(b"etags please\r");
    recorded(repo.path(), 1);

    signal(&shell, Signal::HUP);

    let (took, status) = ended(&mut shell);
    let (drawn, cooked) = released(terminal, slave);

    assert!(
        took < DEADLINE,
        "the shell took {took:?} to act on a hangup, over the {DEADLINE:?} it has"
    );
    assert!(status.success(), "a hangup ended the shell with {status}");
    assert_handed_back(&drawn, cooked, "a hangup");
    assert!(
        !drawn.contains("niobe --resume"),
        "a hangup printed the line a quit prints, onto the terminal the hangup says has gone: \
         {drawn:?}"
    );
}

#[test]
fn ctrl_j_opens_a_line_on_a_terminal_that_cannot_report_keys() {
    let repo = repo();
    let (terminal, slave) = Terminal::answering(Keys::Legacy);
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows(OPENING_FRAME);

    // The line feed Ctrl+J sends is a byte of its own. Were it read as Enter,
    // the first line would run alone, an unterminated quote, and print nothing.
    // The shell's placeholder is waited for by words the frame before it did
    // not have in the same cells: the terminal is sent only the cells that
    // changed, so a word that coincides with the hint under it arrives cut.
    terminal.typed(b"!");
    terminal.shows("the agent does not see");
    terminal.typed(b"printf %s \"joined-$((6*7))\ny\"\r");
    terminal.shows("joined-42");
    terminal.typed(CTRL_Q);

    let (_, status) = ended(&mut shell);
    drop(slave);
    terminal.drained();
    assert!(status.success(), "the shell ended with {status}");
}

/// A command typed after `!` runs in the directory the session was opened
/// in, what it printed is drawn in the transcript, and it is kept in the
/// session store as a call like any other.
#[test]
fn a_command_typed_after_a_bang_runs_here_and_is_kept_as_a_call() {
    let repo = repo();
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows(OPENING_FRAME);

    terminal.typed(b"!");
    terminal.shows("what it prints");
    terminal.typed(b"echo kept > proof.txt; echo printed-$((6*7))\r");
    terminal.shows("printed-42");
    terminal.typed(CTRL_Q);

    let (_, status) = ended(&mut shell);
    drop(slave);
    terminal.drained();
    assert!(status.success(), "the shell ended with {status}");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("proof.txt")).expect("the command wrote here"),
        "kept\n"
    );

    recorded(repo.path(), 2);
    let store =
        Store::open(&repo.path().join(".niobe").join("sessions.db")).expect("the session was kept");
    let session: SessionId = "1".parse().expect("1 is a session id");
    let events = store.events(session).expect("the session's events read");
    let ended = events.iter().find_map(|recorded| match &recorded.event {
        niobe_core::event::Event::ToolCallEnd {
            name,
            output,
            exit_code,
            ..
        } => Some((name.clone(), output.clone(), *exit_code)),
        _ => None,
    });
    assert_eq!(
        ended,
        Some(("! shell".to_owned(), "printed-42\n".to_owned(), Some(0))),
        "{events:?}"
    );
}

/// A `claude` that writes down the arguments it was started with, names its
/// conversation `conv-1`, says it is gating calls in plan mode, and answers
/// every turn.
const WRITES_DOWN_ITS_ARGUMENTS_CLAUDE: &str = "#!/bin/sh\n\
    printf '%s\\n' \"$*\" >> args.log\n\
    read -r first\n\
    printf '%s\\n' '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"conv-1\",\"model\":\"claude-opus-5\",\"permissionMode\":\"plan\"}'\n\
    while read -r turn; do\n\
    printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5\",\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"answered-the-turn\"}]}}'\n\
    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n\
    done\n";

/// Continuing a session hands the CLI the conversation it recorded and the
/// mode it was left in: without either, the operator reads the old session
/// and talks to a new one, asked about everything they had stopped it asking.
#[test]
fn a_resumed_session_starts_its_cli_on_the_same_conversation_and_in_the_same_mode() {
    let repo = repo();
    let home = stand_in(repo.path(), WRITES_DOWN_ITS_ARGUMENTS_CLAUDE);
    for args in [&[][..], &["--resume", "1"][..]] {
        let (terminal, slave) = Terminal::open();
        let mut shell = shell_driving_the_stand_in(&slave, repo.path(), home.path())
            .args(args)
            .spawn()
            .expect("the niobe binary runs");
        if args.is_empty() {
            terminal.shows(OPENING_FRAME);
            terminal.typed(b"go\r");
        }
        terminal.shows("answered-the-turn");
        terminal.typed(CTRL_Q);
        let (_, status) = ended(&mut shell);
        let (drawn, _) = released(terminal, slave);
        assert!(status.success(), "{args:?} ended with {status}: {drawn}");
    }

    let started = std::fs::read_to_string(repo.path().join("args.log"))
        .expect("the stand-in wrote down how it was started");
    let started: Vec<&str> = started.lines().collect();
    assert_eq!(started.len(), 2, "{started:?}");
    assert!(!started[0].contains("--resume"), "{started:?}");
    assert!(started[1].contains("--resume conv-1"), "{started:?}");
    assert!(started[1].contains("--permission-mode plan"), "{started:?}");
}

/// A session read in from the CLI's own transcript carries on in the CLI
/// under the transcript's own id, and in the mode the transcript left it in.
#[test]
fn an_imported_session_starts_its_cli_on_its_transcript_and_in_its_mode() {
    let repo = repo();
    let home = stand_in(repo.path(), WRITES_DOWN_ITS_ARGUMENTS_CLAUDE);
    let claude = claude_config_with_the_transcript(repo.path());
    let transcript = std::fs::read_dir(claude.path().join("projects"))
        .expect("the projects directory lists")
        .next()
        .expect("one project")
        .expect("it reads")
        .path()
        .join(format!("{TRANSCRIPT_SESSION}.jsonl"));
    let mut text = std::fs::read_to_string(&transcript).expect("the transcript reads");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("{\"type\":\"permission-mode\",\"permissionMode\":\"plan\"}\n");
    std::fs::write(&transcript, text).expect("the transcript is written");
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_driving_the_stand_in(&slave, repo.path(), home.path())
        .env("CLAUDE_CONFIG_DIR", claude.path())
        .args(["--resume", TRANSCRIPT_SESSION])
        .spawn()
        .expect("the niobe binary runs");

    terminal.shows("Done: the catalog response carries an etag.");
    // Waited for before the quit, which ends the CLI's group at once.
    let started = written(&repo.path().join("args.log"));
    terminal.typed(CTRL_Q);
    let (_, status) = ended(&mut shell);
    let (drawn, _) = released(terminal, slave);
    assert!(status.success(), "{drawn}");

    assert!(
        started.contains(&format!("--resume {TRANSCRIPT_SESSION}")),
        "{started}"
    );
    assert!(started.contains("--permission-mode plan"), "{started}");
}

/// A `claude` that writes down the directory it was started in, then every
/// turn it is sent, and answers each with the same reply.
const WRITES_DOWN_WHERE_IT_RUNS_CLAUDE: &str = "#!/bin/sh\n\
    pwd -P > claude.cwd\n\
    read -r first\n\
    while read -r turn; do\n\
    printf '%s\\n' \"$turn\" >> turns.jsonl\n\
    printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5\",\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"answered-the-turn\"}]}}'\n\
    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n\
    done\n";

/// Waits until `path` holds something, and gives back what.
fn written(path: &Path) -> String {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.is_empty()
        {
            return text;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("nothing was written to {}", path.display());
}

/// Opened in a subdirectory, the session has one directory: the agent runs
/// at the repository's root, a `!` command runs there too, and a path an `@`
/// completion puts in the prompt is one the agent can open from there.
#[test]
fn a_session_opened_in_a_subdirectory_runs_everything_at_the_repositorys_root() {
    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    let root = std::fs::canonicalize(dir.path()).expect("the directory resolves");
    let git = |args: &[&str]| {
        let ran = Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .expect("git runs");
        assert!(ran.status.success(), "git {args:?}: {ran:?}");
    };
    git(&["init", "-q"]);
    let below = root.join("sub").join("dir");
    std::fs::create_dir_all(&below).expect("the subdirectory can be made");
    std::fs::write(below.join("inner.txt"), "in here\n").expect("the file is written");
    let home = stand_in(&root, WRITES_DOWN_WHERE_IT_RUNS_CLAUDE);
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_command(&slave, &below)
        .env("XDG_CONFIG_HOME", home.path())
        .env(
            "PATH",
            format!("{}:/bin:/usr/bin", root.join("bin").display()),
        )
        .spawn()
        .expect("the niobe binary runs");
    terminal.shows(OPENING_FRAME);

    terminal.typed(b"!");
    terminal.shows("what it prints");
    terminal.typed(b"pwd -P > bang.cwd\r");
    let bang = written(&root.join("bang.cwd"));
    terminal.typed(b"look at @inn");
    terminal.shows("inner.txt");
    terminal.typed(b"\t\r");
    terminal.shows("answered-the-turn");
    terminal.typed(CTRL_Q);

    let (_, status) = ended(&mut shell);
    let (drawn, cooked) = released(terminal, slave);
    assert!(status.success(), "the shell ended with {status}: {drawn}");
    let at_root = format!("{}\n", root.display());
    assert_eq!(written(&root.join("claude.cwd")), at_root, "the agent");
    assert_eq!(bang, at_root, "the operator's command");
    let turns = written(&root.join("turns.jsonl"));
    assert!(
        turns.contains("look at @sub/dir/inner.txt"),
        "the completed path is not the one the agent names the file by: {turns}"
    );
    assert_handed_back(&drawn, cooked, "a quit from a subdirectory");
}

/// A `claude` that reads the request a session opens with and one turn,
/// replies around a line that is not UTF-8, ends the turn, and then reads
/// whatever it is sent until its standard input closes — so a session that
/// closed it early would find it gone.
const UNDECODABLE_CLAUDE: &str = "#!/bin/sh\n\
    read -r first\n\
    read -r turn\n\
    printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5\",\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"written-before-the-bytes\"}]}}'\n\
    printf 'BAD\\377\\376LINE\\n'\n\
    printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5\",\"id\":\"msg_2\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"written-after-the-bytes\"}]}}'\n\
    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n\
    while read -r line; do :; done\n";

/// One line from the CLI that is not UTF-8 is passed over: the reply after it
/// is drawn, the session stays open for the next prompt, and the shell still
/// answers the keyboard and quits.
#[test]
fn a_line_from_the_cli_that_is_not_utf8_costs_neither_the_reply_nor_the_shell() {
    let repo = repo();
    let home = stand_in(repo.path(), UNDECODABLE_CLAUDE);
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_driving_the_stand_in(&slave, repo.path(), home.path())
        .spawn()
        .expect("the niobe binary runs");
    terminal.shows(OPENING_FRAME);

    terminal.typed(b"say something\r");
    terminal.shows("written-after-the-bytes");
    terminal.typed(b"and again\r");
    terminal.typed(CTRL_Q);

    let (_, status) = ended(&mut shell);
    let (drawn, cooked) = released(terminal, slave);
    assert!(status.success(), "the shell ended with {status}: {drawn}");
    assert!(
        !drawn.contains("standard input is closed"),
        "the session closed the CLI's input under it: {drawn}"
    );
    assert_handed_back(&drawn, cooked, "a quit after a line that was not UTF-8");
}

/// A `claude` that writes down every turn it is sent, one line each, and
/// answers each with the same reply.
const WRITES_DOWN_TURNS_CLAUDE: &str = "#!/bin/sh\n\
    read -r first\n\
    while read -r turn; do\n\
    printf '%s\\n' \"$turn\" >> turns.jsonl\n\
    printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5\",\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"answered-the-turn\"}]}}'\n\
    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n\
    done\n";

/// A paste the terminal bracketed is one prompt with its lines in it, sent
/// by the Enter after it: the carriage returns a terminal separates the
/// lines of a paste with are not Enter.
#[test]
fn a_bracketed_paste_of_three_lines_is_sent_as_one_turn() {
    let repo = repo();
    let home = stand_in(repo.path(), WRITES_DOWN_TURNS_CLAUDE);
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_driving_the_stand_in(&slave, repo.path(), home.path())
        .spawn()
        .expect("the niobe binary runs");
    terminal.shows(OPENING_FRAME);

    terminal.typed(b"\x1b[200~here is the log:\rerror: boom\rat foo.rs:3\x1b[201~");
    terminal.typed(b"\r");
    terminal.shows("answered-the-turn");
    terminal.typed(CTRL_Q);

    let (_, status) = ended(&mut shell);
    let (drawn, cooked) = released(terminal, slave);
    assert!(status.success(), "the shell ended with {status}: {drawn}");
    let turns = std::fs::read_to_string(repo.path().join("turns.jsonl"))
        .expect("the stand-in wrote down the turn it was sent");
    let turns: Vec<&str> = turns.lines().collect();
    assert_eq!(turns.len(), 1, "the paste was sent as {turns:?}");
    assert!(
        turns[0].contains(r"here is the log:\nerror: boom\nat foo.rs:3"),
        "the turn sent was not the paste with its lines: {turns:?}"
    );
    assert_handed_back(&drawn, cooked, "a quit after a paste");
}

/// A `claude` that starts writing a reply to its first turn and does not
/// finish it until it is asked to stop, writing down every request to stop it
/// gets; then closes that turn as the CLI closes one an interrupt cut off,
/// and answers the next turn whole.
const STOPPABLE_CLAUDE: &str = "#!/bin/sh\n\
    read -r first\n\
    read -r turn\n\
    printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5\",\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"started-a-long-turn\"}]}}'\n\
    while read -r line; do\n\
    case \"$line\" in *'\"interrupt\"'*) printf '%s\\n' \"$line\" >> interrupts.jsonl; break;; esac\n\
    done\n\
    printf '%s\\n' '{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"[Request interrupted by user]\"}]}}'\n\
    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"terminal_reason\":\"aborted_streaming\",\"errors\":[\"[ede_diagnostic] result_type=user\"]}'\n\
    read -r turn\n\
    printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5\",\"id\":\"msg_2\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"answered-after-the-stop\"}]}}'\n\
    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n\
    while read -r line; do :; done\n";

/// Stops a running turn with `key` and asserts it reached the CLI as one
/// `interrupt` request, and that the session was still there for the next
/// prompt.
fn stops_a_running_turn_with(key: &[u8], path: &str) {
    let repo = repo();
    let home = stand_in(repo.path(), STOPPABLE_CLAUDE);
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_driving_the_stand_in(&slave, repo.path(), home.path())
        .spawn()
        .expect("the niobe binary runs");
    terminal.shows(OPENING_FRAME);

    terminal.typed(b"go\r");
    terminal.shows("started-a-long-turn");
    terminal.typed(key);
    // The notice's words are drawn one cursor move apart, as a redraw sends
    // only the cells that changed, so its last word is what is waited on.
    terminal.shows("operator.");
    terminal.typed(b"again\r");
    terminal.shows("answered-after-the-stop");
    terminal.typed(CTRL_Q);

    let (_, status) = ended(&mut shell);
    let (drawn, cooked) = released(terminal, slave);
    assert!(status.success(), "the shell ended with {status}: {drawn}");
    let asked = std::fs::read_to_string(repo.path().join("interrupts.jsonl"))
        .expect("the stand-in was asked to stop the turn");
    let asked: Vec<&str> = asked.lines().collect();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(
        asked[0].contains(r#""type":"control_request""#),
        "{asked:?}"
    );
    assert_handed_back(&drawn, cooked, path);
}

#[test]
fn esc_during_a_running_turn_stops_it_and_the_session_takes_the_next_prompt() {
    stops_a_running_turn_with(b"\x1b", "a quit after Esc stopped a turn");
}

#[test]
fn ctrl_c_during_a_running_turn_stops_it_and_leaves_the_session_open() {
    stops_a_running_turn_with(b"\x03", "a quit after Ctrl+C stopped a turn");
}

/// With nothing to read keys from — standard input from `/dev/null` and no
/// controlling terminal to fall back on — the shell says so before it starts
/// the backend or takes the screen.
#[test]
fn a_shell_with_no_keys_to_read_stops_before_it_starts_anything() {
    let repo = repo();
    let home = stand_in(repo.path(), "#!/bin/sh\ntouch started\nsleep 5\n");
    let (terminal, slave) = Terminal::open();
    let null = File::open("/dev/null").expect("/dev/null opens");
    // A session of its own, so there is no controlling terminal behind it
    // whatever terminal the tests themselves were started from.
    let mut shell = Command::new("perl")
        .args(["-MPOSIX", "-e", "POSIX::setsid(); exec @ARGV or die $!"])
        .arg(env!("CARGO_BIN_EXE_niobe"))
        .current_dir(repo.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env(
            "PATH",
            format!("{}:/bin:/usr/bin", repo.path().join("bin").display()),
        )
        .stdin(Stdio::from(null))
        .stdout(Stdio::from(
            slave.try_clone().expect("the slave can be duplicated"),
        ))
        .stderr(Stdio::from(
            slave.try_clone().expect("the slave can be duplicated"),
        ))
        .spawn()
        .expect("perl runs niobe");

    let (_, status) = ended(&mut shell);
    drop(slave);
    let drawn = terminal.drained();

    assert_eq!(status.code(), Some(1), "{drawn}");
    assert!(
        drawn.contains("standard input is not a terminal"),
        "{drawn}"
    );
    assert!(
        !drawn.contains(ENTER_ALTERNATE_SCREEN),
        "the screen was taken: {drawn:?}"
    );
    assert!(
        !repo.path().join("started").exists(),
        "the backend was started for a shell that could not read a key"
    );
}

/// Puts `script` in `cwd` as the `claude` a session runs, and gives back a
/// user config whose default profile runs it.
fn stand_in(cwd: &Path, script: &str) -> tempfile::TempDir {
    let bin = cwd.join("bin");
    std::fs::create_dir(&bin).expect("a directory for the stand-in");
    let claude = bin.join("claude");
    std::fs::write(&claude, script).expect("the stand-in is written");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755))
            .expect("the stand-in is made executable");
    }
    user_config("default_profile = \"max\"\n\n[profiles.max]\nbackend = \"claude\"\n")
}

/// The shell on `slave`, recording into `cwd` under the config in `home`,
/// with the stand-in [`stand_in`] put in `cwd` first on its `PATH`.
fn shell_driving_the_stand_in(slave: &File, cwd: &Path, home: &Path) -> Command {
    let mut command = shell_command(slave, cwd);
    command.env("XDG_CONFIG_HOME", home).env(
        "PATH",
        format!("{}:/bin:/usr/bin", cwd.join("bin").display()),
    );
    command
}

/// A `claude` that starts something of its own on its standard output and
/// error — as a command a tool call left in the background, or an MCP server,
/// is — writes down its own process and that one, answers one turn, and
/// leaves as soon as its standard input closes. What it started stays, and
/// holds the pipes the session reads the CLI from.
const LEAVES_SOMETHING_RUNNING_CLAUDE: &str = "#!/bin/sh\n\
    echo $$ > claude.pid\n\
    sleep 77101 &\n\
    echo $! > started.pid\n\
    read -r first\n\
    read -r turn\n\
    printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5\",\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"replied-with-something-running\"}]}}'\n\
    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n\
    while read -r line; do :; done\n";

/// Ends the shell down `path` while the CLI has something running that holds
/// its pipes, and asserts the shell is gone within [`DEADLINE`] and took
/// with it everything the CLI started.
///
/// The CLI leaving does not close the pipes the session reads it from while
/// anything it started still holds them, so a session that waits for them to
/// close waits for as long as that runs: the operator is handed back a
/// terminal with a finished-looking session on it and no prompt.
fn quits_without_waiting_on_what_the_cli_started(path: &str, end: impl FnOnce(&Terminal, &Child)) {
    let repo = repo();
    let home = stand_in(repo.path(), LEAVES_SOMETHING_RUNNING_CLAUDE);
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_driving_the_stand_in(&slave, repo.path(), home.path())
        .spawn()
        .expect("the niobe binary runs");
    terminal.shows(OPENING_FRAME);
    terminal.typed(b"say something\r");
    terminal.shows("replied-with-something-running");
    let pid = |file: &str| {
        let written = std::fs::read_to_string(repo.path().join(file))
            .expect("the stand-in wrote down its processes before it answered");
        let raw: i32 = written.trim().parse().expect("a pid is a number");
        Pid::from_raw(raw).expect("a pid is positive")
    };
    let (cli, started) = (pid("claude.pid"), pid("started.pid"));

    end(&terminal, &shell);

    let (took, status) = ended(&mut shell);
    let (drawn, cooked) = released(terminal, slave);
    assert!(
        took < DEADLINE,
        "on {path} the shell took {took:?} to end, over the {DEADLINE:?} it has"
    );
    assert!(
        status.success(),
        "on {path} the shell ended with {status}: {drawn}"
    );
    assert_handed_back(&drawn, cooked, path);

    // What was killed is reaped by whoever inherited it, a moment later.
    let deadline = Instant::now() + DEADLINE;
    let left = || {
        rustix::process::test_kill_process(started).is_ok()
            || rustix::process::test_kill_process_group(cli).is_ok()
    };
    while left() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !left(),
        "on {path} what the CLI started was still running after the shell ended"
    );
}

/// A `claude` that starts something of its own on its standard output, as
/// [`LEAVES_SOMETHING_RUNNING_CLAUDE`] does, answers one turn and then falls
/// over, saying why on standard error, while what it started still holds the
/// pipes the session reads it from.
const DIES_WITH_SOMETHING_RUNNING_CLAUDE: &str = "#!/bin/sh\n\
    echo $$ > claude.pid\n\
    sleep 77103 &\n\
    echo $! > started.pid\n\
    read -r first\n\
    read -r turn\n\
    printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5\",\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"replied-before-falling-over\"}]}}'\n\
    echo 'fell-over-mid-session' >&2\n\
    exit 1\n";

/// A CLI that dies mid-session is reported as ended, in its own words and
/// after the reply it wrote first, even though something it started still
/// holds its standard output open — and that something is ended with it.
#[test]
fn a_cli_that_dies_while_something_it_started_holds_its_output_is_reported_ended() {
    let repo = repo();
    let home = stand_in(repo.path(), DIES_WITH_SOMETHING_RUNNING_CLAUDE);
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_driving_the_stand_in(&slave, repo.path(), home.path())
        .spawn()
        .expect("the niobe binary runs");
    terminal.shows(OPENING_FRAME);
    terminal.typed(b"say something\r");
    terminal.shows("replied-before-falling-over");
    let replied = Instant::now();
    terminal.shows("fell-over-mid-session");
    let took = replied.elapsed();
    let pid = |file: &str| {
        let written = std::fs::read_to_string(repo.path().join(file))
            .expect("the stand-in wrote down its processes before it answered");
        let raw: i32 = written.trim().parse().expect("a pid is a number");
        Pid::from_raw(raw).expect("a pid is positive")
    };
    let (cli, started) = (pid("claude.pid"), pid("started.pid"));
    let left = || {
        rustix::process::test_kill_process(started).is_ok()
            || rustix::process::test_kill_process_group(cli).is_ok()
    };
    let deadline = Instant::now() + DEADLINE;
    while left() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let still_running = left();

    terminal.typed(CTRL_Q);
    let (_, status) = ended(&mut shell);
    let (drawn, cooked) = released(terminal, slave);
    assert!(
        took < DEADLINE,
        "the end was drawn {took:?} after the reply, over the {DEADLINE:?} it has"
    );
    assert!(
        !still_running,
        "what the CLI started was still running after its end was reported"
    );
    let reply = drawn.find("replied-before-falling-over");
    let end = drawn.find("fell-over-mid-session");
    assert!(reply < end, "the end was drawn before the reply: {drawn}");
    assert!(status.success(), "the shell ended with {status}: {drawn}");
    assert_handed_back(&drawn, cooked, "a quit after the CLI fell over");
}

#[test]
fn a_quit_does_not_wait_on_what_the_cli_started_and_takes_it_along() {
    quits_without_waiting_on_what_the_cli_started("a clean quit", |terminal, _| {
        terminal.typed(CTRL_Q);
    });
}

#[test]
fn a_sigterm_does_not_wait_on_what_the_cli_started_and_takes_it_along() {
    quits_without_waiting_on_what_the_cli_started("a SIGTERM", |_, shell| {
        signal(shell, Signal::TERM);
    });
}

#[test]
fn a_hangup_does_not_wait_on_what_the_cli_started_and_takes_it_along() {
    quits_without_waiting_on_what_the_cli_started("a hangup", |_, shell| {
        signal(shell, Signal::HUP);
    });
}

/// Ends the shell with `signal` while a `!` command is running, and asserts
/// the shell took the same way out as a quit: the terminal handed back with
/// its line discipline cooked again, and the command's group gone with it.
///
/// The default disposition of SIGINT and SIGQUIT kills the process outright,
/// which leaves the terminal raw on the alternate screen and the command
/// running with no one left to end it. Neither comes from the keyboard while
/// the shell is up — raw mode reads Ctrl+C as a key — but `kill`, `timeout`
/// and the task runners of editors send them.
fn a_signal_hands_back_the_terminal_and_ends_the_bang_command(path: &str, sent: Signal) {
    let repo = repo();
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows(OPENING_FRAME);
    terminal.typed(b"!");
    terminal.shows("what it prints");
    terminal.typed(b"echo $$ > bang.tmp && mv bang.tmp bang.pid; sleep 77104\r");
    let marker = repo.path().join("bang.pid");
    let deadline = Instant::now() + PATIENCE;
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let written = std::fs::read_to_string(&marker).expect("the command wrote down its process");
    let raw: i32 = written.trim().parse().expect("a pid is a number");
    let group = Pid::from_raw(raw).expect("a pid is positive");

    signal(&shell, sent);

    let (took, status) = ended(&mut shell);
    let modes = rustix::termios::tcgetattr(&slave).expect("the pty's modes can be read");
    let (drawn, cooked) = released(terminal, slave);
    assert!(
        took < DEADLINE,
        "the shell took {took:?} to act on {path}, over the {DEADLINE:?} it has"
    );
    assert!(status.success(), "{path} ended the shell with {status}");
    assert_handed_back(&drawn, cooked, path);
    let cooked = rustix::termios::LocalModes::ICANON
        | rustix::termios::LocalModes::ECHO
        | rustix::termios::LocalModes::ISIG;
    assert!(
        modes.local_modes.contains(cooked),
        "on {path} the terminal was left without its line discipline: {:?}",
        modes.local_modes
    );

    // What was killed is reaped by whoever inherited it, a moment later.
    let deadline = Instant::now() + DEADLINE;
    let left = || rustix::process::test_kill_process_group(group).is_ok();
    while left() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !left(),
        "on {path} the `!` command was still running after the shell ended"
    );
}

#[test]
fn a_sigint_hands_back_the_terminal_and_ends_a_running_bang_command() {
    a_signal_hands_back_the_terminal_and_ends_the_bang_command("a SIGINT", Signal::INT);
}

#[test]
fn a_sigquit_hands_back_the_terminal_and_ends_a_running_bang_command() {
    a_signal_hands_back_the_terminal_and_ends_the_bang_command("a SIGQUIT", Signal::QUIT);
}

#[test]
fn a_sigterm_hands_back_the_terminal_and_ends_a_running_bang_command() {
    a_signal_hands_back_the_terminal_and_ends_the_bang_command("a SIGTERM", Signal::TERM);
}

/// A `claude` that starts something of its own with every descriptor it was
/// given let go — as a server a tool call left running is — writes down its
/// own process and that one, answers one turn, and leaves as soon as its
/// standard input closes. What it started holds nothing of the session's, so
/// only the process group it shares with the CLI can take it along.
const LEAVES_SOMETHING_DETACHED_CLAUDE: &str = "#!/bin/sh\n\
    echo $$ > claude.pid\n\
    sleep 77102 </dev/null >/dev/null 2>&1 &\n\
    echo $! > started.pid\n\
    read -r first\n\
    read -r turn\n\
    printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5\",\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"replied-with-something-detached\"}]}}'\n\
    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n\
    while read -r line; do :; done\n";

/// Ends the shell down `path` once the CLI has left something detached
/// running and a `!` command has ended with something of its own still
/// running in the background, and asserts that within `within` of the shell
/// ending nothing of either process group is left.
///
/// A `!` command's `sh` ending does not end what it put in the background,
/// and a shell that forgets the command's group once its `sh` has gone has
/// nothing left to stop at the end of the session.
fn ends_with_nothing_left_running(
    path: &str,
    within: Duration,
    end: impl FnOnce(&Terminal, &Child),
) {
    let repo = repo();
    let home = stand_in(repo.path(), LEAVES_SOMETHING_DETACHED_CLAUDE);
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_driving_the_stand_in(&slave, repo.path(), home.path())
        .spawn()
        .expect("the niobe binary runs");
    terminal.shows(OPENING_FRAME);
    terminal.typed(b"say something\r");
    terminal.shows("replied-with-something-detached");
    terminal.typed(b"!");
    terminal.shows("what it prints");
    terminal
        .typed(b"(sleep 77106 >/dev/null 2>&1 & echo $$ $! > bang.tmp && mv bang.tmp bang.pid)\r");
    terminal.shows("exit 0");
    let pids = |file: &str| -> Vec<Pid> {
        std::fs::read_to_string(repo.path().join(file))
            .expect("the processes were written down before the call ended")
            .split_whitespace()
            .map(|raw| {
                let raw: i32 = raw.parse().expect("a pid is a number");
                Pid::from_raw(raw).expect("a pid is positive")
            })
            .collect()
    };
    let (cli, started) = (pids("claude.pid")[0], pids("started.pid")[0]);
    let bang = pids("bang.pid");
    let (bang_group, backgrounded) = (bang[0], bang[1]);
    assert!(
        rustix::process::test_kill_process(backgrounded).is_ok(),
        "what the `!` command put in the background ended with it"
    );

    end(&terminal, &shell);

    ended(&mut shell);
    drop(slave);
    terminal.drained();
    // What was killed is reaped by whoever inherited it, a moment later.
    let deadline = Instant::now() + within;
    let left = || {
        [started, backgrounded]
            .into_iter()
            .any(|pid| rustix::process::test_kill_process(pid).is_ok())
            || [cli, bang_group]
                .into_iter()
                .any(|group| rustix::process::test_kill_process_group(group).is_ok())
    };
    while left() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let state = |pid: Pid| match rustix::process::test_kill_process(pid) {
        Ok(()) => "still there",
        Err(_) => "gone",
    };
    assert!(
        !left(),
        "{within:?} after {path} the CLI's detached process was {} and the `!` command's \
         background {}",
        state(started),
        state(backgrounded),
    );
}

#[test]
fn a_quit_takes_along_what_the_cli_and_an_ended_bang_command_left_running() {
    ends_with_nothing_left_running("a clean quit", DEADLINE, |terminal, _| {
        terminal.typed(CTRL_Q);
    });
}

/// Nothing runs in the shell after SIGKILL, so what the session started is
/// ended by something that outlives it and hears it go.
#[test]
fn a_sigkill_leaves_nothing_the_session_started_running() {
    ends_with_nothing_left_running("a SIGKILL", 2 * DEADLINE, |_, shell| {
        signal(shell, Signal::KILL);
    });
}

/// Ctrl+Z, which a terminal in raw mode hands the shell as a key rather than
/// as SIGTSTP.
const CTRL_Z: &[u8] = b"\x1a";

/// Waits until the shell has stopped, as the operator's job-control shell
/// would see it.
fn stopped(shell: &Child) {
    let raw = i32::try_from(shell.id()).expect("a pid fits in an i32");
    let pid = Pid::from_raw(raw).expect("the shell has a pid");
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        let waited = rustix::process::waitpid(
            Some(pid),
            rustix::process::WaitOptions::UNTRACED | rustix::process::WaitOptions::NOHANG,
        )
        .expect("the shell can be waited on");
        match waited {
            Some((_, status)) if status.stopped() => return,
            Some((_, status)) => panic!("the shell ended instead of stopping: {status:?}"),
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    panic!("the shell was still running {PATIENCE:?} after it was asked to stop");
}

/// Whether the pty's line discipline is cooked: the operator's shell reads
/// lines, echoes them and turns Ctrl+C into a signal.
fn cooked(slave: &File) -> bool {
    let modes = rustix::termios::tcgetattr(slave).expect("the pty's modes can be read");
    modes.local_modes.contains(
        rustix::termios::LocalModes::ICANON
            | rustix::termios::LocalModes::ECHO
            | rustix::termios::LocalModes::ISIG,
    )
}

/// Stops the shell with `stop`, and asserts that it handed the terminal back
/// before it stopped, took it again and drew the whole frame when continued,
/// and that a quit afterwards hands it back once more, exactly once.
///
/// A stop is not an exit, but it hands the terminal to the operator's shell,
/// which needs it cooked, on the main screen and reporting no mouse; and the
/// shell that is continued finds a screen the operator's shell drew over.
fn a_stop_hands_the_terminal_back_and_a_continue_takes_it_again(
    path: &str,
    stop: impl FnOnce(&Terminal, &Child),
) {
    let repo = repo();
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows(OPENING_FRAME);
    assert!(
        !cooked(&slave),
        "the shell never put the terminal in raw mode"
    );

    stop(&terminal, &shell);
    stopped(&shell);

    let before_stop = terminal.caught_up();
    assert_eq!(
        before_stop.matches(RESTORED).count(),
        1,
        "on {path} the shell did not hand the terminal back once before stopping: \
         {before_stop:?}"
    );
    assert!(
        cooked(&slave),
        "on {path} the shell stopped with the terminal raw"
    );

    let continued = terminal.mark();
    signal(&shell, Signal::CONT);
    terminal.shows_since(continued, ENTER_ALTERNATE_SCREEN);
    terminal.shows_since(continued, KEYBOARD_ON);
    // The whole frame, not only what changed: the operator's shell drew over
    // the screen while the shell was stopped, and a diff against the last
    // frame would leave that on it.
    terminal.shows_since(continued, OPENING_FRAME);
    assert!(
        !cooked(&slave),
        "on {path} the shell was continued without taking raw mode back"
    );

    terminal.typed(CTRL_Q);
    let (_, status) = ended(&mut shell);
    drop(slave);
    let drawn = terminal.drained();

    assert!(status.success(), "{path} then a quit ended with {status}");
    let after = &drawn[continued..];
    assert_eq!(
        after.matches(LEAVE_ALTERNATE_SCREEN).count(),
        1,
        "on {path} the quit after continuing did not leave the screen exactly once: {after:?}"
    );
    assert!(after.contains(RESTORED), "{after:?}");
    assert_eq!(after.matches(KEYBOARD_OFF).count(), 1, "{after:?}");
}

/// A SIGSTOP cannot be caught: the shell stops holding the terminal as it
/// had it, and the operator's shell may take it back and leave it cooked, as
/// bash does. Continued, the shell takes the terminal again whatever it
/// finds — raw mode, the screen, the keys — and draws the whole frame; a quit
/// afterwards still hands it back once.
#[test]
fn a_sigstop_nothing_could_catch_is_followed_by_taking_the_terminal_again() {
    let repo = repo();
    let (terminal, slave) = Terminal::open();
    let mut shell = shell_on(&slave, repo.path());
    terminal.shows(OPENING_FRAME);
    assert!(
        !cooked(&slave),
        "the shell never put the terminal in raw mode"
    );

    signal(&shell, Signal::STOP);
    stopped(&shell);
    let mut modes = rustix::termios::tcgetattr(&slave).expect("the pty's modes can be read");
    modes.local_modes |= rustix::termios::LocalModes::ICANON
        | rustix::termios::LocalModes::ECHO
        | rustix::termios::LocalModes::ISIG;
    rustix::termios::tcsetattr(&slave, rustix::termios::OptionalActions::Now, &modes)
        .expect("the pty's modes can be set");
    assert!(cooked(&slave), "the test could not cook the terminal");

    let continued = terminal.mark();
    signal(&shell, Signal::CONT);
    terminal.shows_since(continued, ENTER_ALTERNATE_SCREEN);
    terminal.shows_since(continued, KEYBOARD_ON);
    terminal.shows_since(continued, OPENING_FRAME);
    assert!(
        !cooked(&slave),
        "the shell was continued without taking raw mode back"
    );

    terminal.typed(CTRL_Q);
    let (_, status) = ended(&mut shell);
    drop(slave);
    let drawn = terminal.drained();

    assert!(
        status.success(),
        "a quit after the continue ended with {status}"
    );
    let after = &drawn[continued..];
    assert_eq!(
        after.matches(LEAVE_ALTERNATE_SCREEN).count(),
        1,
        "the quit did not leave the screen exactly once: {after:?}"
    );
    assert_eq!(after.matches(RESTORED).count(), 1, "{after:?}");
    assert!(
        after.rfind(KEYBOARD_OFF) > after.rfind(KEYBOARD_ON),
        "the keys were left enhanced: {after:?}"
    );
}

#[test]
fn a_sigtstp_hands_the_terminal_back_and_a_sigcont_takes_it_again() {
    a_stop_hands_the_terminal_back_and_a_continue_takes_it_again("a SIGTSTP", |_, shell| {
        signal(shell, Signal::TSTP);
    });
}

#[test]
fn ctrl_z_suspends_the_shell_as_a_sigtstp_would() {
    a_stop_hands_the_terminal_back_and_a_continue_takes_it_again("Ctrl+Z", |terminal, _| {
        terminal.typed(CTRL_Z);
    });
}
