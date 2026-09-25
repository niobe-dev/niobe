// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Entering and — on every path out — leaving the mode the shell draws in.
//!
//! The terminal is restored on every exit path, including panics and SIGTERM,
//! so restoration is not a line at the end of `main`. It is three mechanisms that each cover one way out:
//!
//! * a clean quit and any error — [`TerminalGuard`]'s `Drop`,
//! * a panic — the hook [`install_panic_hook`] installs, which runs before the
//!   guard unwinds (this is why the release profile keeps `panic = "unwind"`),
//! * SIGTERM and SIGHUP — [`Shutdown`], which flips a flag the event loop reads
//!   so the guard drops normally. It keeps the two apart: SIGTERM asks the
//!   process to stop, SIGHUP says the terminal it ran on has gone, and the
//!   session ended for different reasons.
//!
//! Where the terminal can report keys the legacy encoding cannot tell apart —
//! Shift+Enter from Enter, above all — the guard asks it to, and every one of
//! those paths takes that back with the rest.
//!
//! All three are idempotent, so overlapping paths — a panic while a SIGTERM is
//! pending — restore once and do not fight each other.
//!
//! A terminal can also go away without any of that: no signal is sent when the
//! process is not in the session that owns it. The event loop's own wait for
//! input sees the hangup and quits, which is the first path above — except that
//! the sequences written here cannot reach a terminal that has gone, so the
//! failure to write them is not what [`crate::run`] reports. It is read instead:
//! a loop that failed on a terminal these sequences then cannot reach failed
//! because that terminal went, which is the hangup the wait would have reported
//! had the close landed while the tick was being spent there rather than in the
//! draw at the top of the next one.
//!
//! Each path is proven against the binary on a real terminal in the CLI's
//! `tests/pty.rs`. Nothing less can: the hook writes to the process's own
//! standard output, and the signal disposition belongs to the process, so the
//! unit tests below reach the guard and the flag but not the way out.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

/// Whether a guard has put the process's terminal into the drawing mode.
///
/// Read only by the panic hook, which has no guard to ask: a panic in a unit
/// test must not spray escape sequences at a terminal the shell never touched.
static TERMINAL_ENTERED: AtomicBool = AtomicBool::new(false);

/// Whether the terminal was asked to report keys the legacy encoding cannot
/// tell apart, and still owes being asked to stop.
///
/// Read by the panic hook for the same reason as [`TERMINAL_ENTERED`]; kept
/// apart from it because a terminal that cannot report those keys is never
/// asked, and must not be sent the sequence that takes the request back.
static KEYBOARD_PUSHED: AtomicBool = AtomicBool::new(false);

/// Asks the terminal to report mouse buttons and the wheel, in SGR encoding.
///
/// Written by hand rather than with crossterm's `EnableMouseCapture`, which
/// also asks for every movement of the pointer: the shell reads only the
/// wheel and a click, and a report per movement would wake the event loop for
/// nothing.
const MOUSE_ON: &[u8] = b"\x1b[?1000h\x1b[?1006h";

/// Takes [`MOUSE_ON`] back. Harmless on a terminal that was never asked, which
/// is what lets the panic hook write it without knowing how far entry got.
const MOUSE_OFF: &[u8] = b"\x1b[?1006l\x1b[?1000l";

/// Asks which keyboard enhancements the terminal has on, then for its primary
/// device attributes.
///
/// A terminal that knows the kitty keyboard protocol answers the first; every
/// terminal answers the second. So an answer to the second with none to the
/// first before it is a terminal that cannot report Shift+Enter, and nothing
/// has to be waited out to learn so.
const KEYBOARD_QUERY: &[u8] = b"\x1b[?u\x1b[c";

/// Pushes the one enhancement the shell needs onto the terminal's stack:
/// disambiguating escape codes, which is what makes Shift+Enter arrive as
/// itself rather than as the carriage return Enter sends. Nothing else is
/// asked for — reporting key releases, say, would send every key twice.
const KEYBOARD_ON: &[u8] = b"\x1b[>1u";

/// Pops what [`KEYBOARD_ON`] pushed. Written only to a terminal that was sent
/// it: a terminal that does not know the protocol has no reason to read this
/// as nothing.
const KEYBOARD_OFF: &[u8] = b"\x1b[<1u";

/// How long the terminal has to answer [`KEYBOARD_QUERY`] before it is taken
/// not to report the keys. Every terminal answers the attributes, so this is
/// only ever waited out on one that answers nothing at all, and the shell
/// opens on it a second late rather than not at all.
const KEYBOARD_PATIENCE: Duration = Duration::from_secs(1);

/// What a terminal has said so far in answer to [`KEYBOARD_QUERY`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// The attributes have not arrived yet.
    Waiting,
    /// It reported its keyboard flags before its attributes.
    Reports,
    /// Its attributes came with no keyboard flags before them.
    DoesNot,
}

/// Reads the replies in `heard`: `ESC [ ? <flags> u` for the keyboard, then
/// `ESC [ ? <attributes> c` for the device. Anything else between them is
/// passed over.
fn answer(heard: &[u8]) -> Answer {
    const INTRODUCER: &[u8] = b"\x1b[?";
    let mut reports = false;
    let mut rest = heard;
    while let Some(start) = rest
        .windows(INTRODUCER.len())
        .position(|window| window == INTRODUCER)
    {
        let body = &rest[start + INTRODUCER.len()..];
        let Some(end) = body
            .iter()
            .position(|byte| !(byte.is_ascii_digit() || *byte == b';'))
        else {
            return Answer::Waiting;
        };
        match body[end] {
            b'u' => reports = true,
            b'c' if reports => return Answer::Reports,
            b'c' => return Answer::DoesNot,
            _ => {}
        }
        rest = &body[end..];
    }
    Answer::Waiting
}

/// Asks the terminal whether it can report Shift+Enter, and waits for the
/// answer on standard input.
///
/// Asked here rather than with crossterm's own query, which writes to
/// `/dev/tty` whenever it can open it: that is the controlling terminal, and a
/// process can be drawing on a terminal that is not its controlling one. The
/// question goes where the screen is drawn and the answer is read where the
/// keys are, which is the terminal the shell is actually on.
///
/// The answer is read before crossterm reads anything, so whatever the
/// operator types in the moment it takes is not a key the shell sees. Only
/// standard input is asked: when it is not a terminal the keys come from
/// `/dev/tty`, which `poll(2)` cannot wait on everywhere, and the legacy keys
/// are what the shell falls back on.
#[cfg(unix)]
fn reports_keys(out: &mut impl Write) -> bool {
    use std::io::IsTerminal;
    use std::os::fd::AsFd;

    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return false;
    }
    if out
        .write_all(KEYBOARD_QUERY)
        .and_then(|()| out.flush())
        .is_err()
    {
        return false;
    }

    let deadline = std::time::Instant::now() + KEYBOARD_PATIENCE;
    let mut heard = Vec::new();
    let mut buffer = [0u8; 256];
    loop {
        match answer(&heard) {
            Answer::Waiting => {}
            Answer::Reports => return true,
            Answer::DoesNot => return false,
        }
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return false;
        }
        // Idle is a wait a signal cut short as well as one that ran out, so
        // the deadline rather than the wait decides when to stop.
        match crate::input::poll_input(stdin.as_fd(), left) {
            Ok(crate::input::Input::Ready) => {}
            Ok(crate::input::Input::Idle) => continue,
            Ok(crate::input::Input::HungUp) | Err(_) => return false,
        }
        match rustix::io::read(stdin.as_fd(), &mut buffer) {
            Ok(0) | Err(_) => return false,
            Ok(read) => heard.extend_from_slice(&buffer[..read]),
        }
    }
}

/// Asks the terminal whether it can report Shift+Enter. Only the POSIX side
/// asks; elsewhere the legacy keys are what the shell uses.
#[cfg(not(unix))]
fn reports_keys(_out: &mut impl Write) -> bool {
    false
}

/// Writes the sequences that put a terminal into the drawing mode.
fn enter_screen(out: &mut impl Write) -> io::Result<()> {
    execute!(out, EnterAlternateScreen, Hide)?;
    out.write_all(MOUSE_ON)?;
    out.flush()
}

/// Writes the sequences that take a terminal out of the drawing mode, and
/// pops the keyboard enhancement when `keyboard` says it was pushed.
///
/// Separated from [`TerminalGuard`] so that the panic hook, which cannot reach
/// the guard, emits exactly the same bytes. The mouse goes back first: a
/// terminal left reporting it would print escape sequences into the shell the
/// operator returns to at every click. The keyboard is popped before the
/// alternate screen is left, because the protocol keeps one stack per screen
/// and the push was made on this one.
fn leave(out: &mut impl Write, keyboard: bool) -> io::Result<()> {
    out.write_all(MOUSE_OFF)?;
    if keyboard {
        out.write_all(KEYBOARD_OFF)?;
    }
    execute!(out, LeaveAlternateScreen, Show)
}

/// Holds the terminal in the mode the shell draws in, and takes it back out
/// when it is dropped.
///
/// Restoration is idempotent: calling [`TerminalGuard::restore`] and then
/// dropping the guard restores once.
#[derive(Debug)]
pub struct TerminalGuard<W: Write> {
    out: W,
    /// Whether this guard turned raw mode on, and so owes turning it off.
    raw_mode: bool,
    /// Whether this guard pushed the keyboard enhancement, and so owes
    /// popping it.
    keyboard: bool,
    restored: bool,
}

impl<W: Write> TerminalGuard<W> {
    /// Enters raw mode and the alternate screen, hides the cursor, asks for
    /// the mouse wheel and clicks and, where the terminal says it can, for
    /// Shift+Enter to be told from Enter.
    pub fn enter(out: W) -> io::Result<Self> {
        enable_raw_mode()?;

        // The guard exists before the alternate screen is entered, so that a
        // failure on the way in is still a guard that undoes raw mode as it
        // unwinds rather than an error returned from a raw terminal.
        let mut guard = Self {
            out,
            raw_mode: true,
            keyboard: false,
            restored: false,
        };
        // Set before the screen is entered, not after: it is what
        // `restore` checks, and a failure on the next line has to undo raw
        // mode rather than decide there was nothing to undo.
        TERMINAL_ENTERED.store(true, Ordering::SeqCst);
        enter_screen(&mut guard.out)?;
        // On the alternate screen, where the push is made: the protocol keeps
        // a stack per screen.
        if reports_keys(&mut guard.out) {
            guard.push_keyboard()?;
        }

        Ok(guard)
    }

    /// Asks the terminal to tell Shift+Enter from Enter. Marked as owed
    /// before it is written, for the same reason as the screen: a write that
    /// fails halfway still leaves a terminal that may have taken the push.
    fn push_keyboard(&mut self) -> io::Result<()> {
        self.keyboard = true;
        if self.raw_mode {
            KEYBOARD_PUSHED.store(true, Ordering::SeqCst);
        }
        self.out.write_all(KEYBOARD_ON)?;
        self.out.flush()
    }

    /// Whether the terminal said it can tell Shift+Enter from Enter, and was
    /// asked to. Only then is Shift+Enter a key the shell can name: on any
    /// other terminal it sends the same byte as Enter.
    pub fn reports_shift_enter(&self) -> bool {
        self.keyboard
    }

    /// Enters the alternate screen without touching raw mode.
    ///
    /// Raw mode is a property of the process's controlling terminal, not of the
    /// sink, so a test that enabled it would put the test harness itself into
    /// raw mode. Tests use this; the shell does not.
    #[cfg(test)]
    fn enter_screen_only(mut out: W) -> io::Result<Self> {
        enter_screen(&mut out)?;
        Ok(Self {
            out,
            raw_mode: false,
            keyboard: false,
            restored: false,
        })
    }

    /// Puts the terminal back. Safe to call more than once.
    pub fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;

        if !self.raw_mode {
            return leave(&mut self.out, self.keyboard);
        }

        // A panic unwinds through this guard after the hook has already
        // restored, and the hook clears the same flag: claiming it here is what
        // keeps the two paths from each writing a sequence.
        if !TERMINAL_ENTERED.swap(false, Ordering::SeqCst) {
            return Ok(());
        }

        // Both run even if the first fails: half a restoration is a terminal
        // the operator has to fix by hand.
        KEYBOARD_PUSHED.store(false, Ordering::SeqCst);
        let raw = disable_raw_mode();
        let screen = leave(&mut self.out, self.keyboard);
        raw.and(screen)
    }
}

impl<W: Write> Drop for TerminalGuard<W> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Installs a panic hook that restores the terminal before the panic message is
/// printed, then chains to whatever hook was already there.
///
/// Without it a panic prints its message into the alternate screen, which is
/// then torn down — so the operator sees a working prompt and no reason why
/// their session ended.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if TERMINAL_ENTERED.swap(false, Ordering::SeqCst) {
            let keyboard = KEYBOARD_PUSHED.swap(false, Ordering::SeqCst);
            let _ = disable_raw_mode();
            let _ = leave(&mut io::stdout(), keyboard);
        }
        previous(info);
    }));
}

/// Why a signal is ending the session.
///
/// The two are not the same ending. A process asked to stop ran on a terminal
/// that is still there to be handed back and to be printed on; a process whose
/// terminal hung up has neither, and its session did not fail — it lost the
/// screen it was drawn on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The process was asked to stop.
    Requested,
    /// The terminal the session ran on went away.
    TerminalGone,
}

/// Flags set when a signal ends the session.
///
/// The default disposition for SIGTERM kills the process outright, which leaves
/// the terminal in raw mode on the alternate screen. Catching it costs two
/// atomic reads per tick and turns the signal into an ordinary quit. SIGHUP is
/// caught for the same reason and kept apart from it, because it carries more:
/// the terminal is gone.
#[derive(Debug, Clone)]
pub struct Shutdown {
    #[cfg(unix)]
    requested: std::sync::Arc<AtomicBool>,
    #[cfg(unix)]
    hung_up: std::sync::Arc<AtomicBool>,
}

impl Shutdown {
    /// Registers the handlers. On a platform without POSIX signals these are
    /// flags that are never set, and the guard still covers every other path.
    #[cfg(unix)]
    pub fn install() -> io::Result<Self> {
        let requested = std::sync::Arc::new(AtomicBool::new(false));
        let hung_up = std::sync::Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(
            signal_hook::consts::SIGTERM,
            std::sync::Arc::clone(&requested),
        )?;
        signal_hook::flag::register(signal_hook::consts::SIGHUP, std::sync::Arc::clone(&hung_up))?;
        Ok(Self { requested, hung_up })
    }

    /// Registers the handlers.
    #[cfg(not(unix))]
    pub fn install() -> io::Result<Self> {
        Ok(Self {})
    }

    /// What a signal has asked of the session, if one has.
    ///
    /// A hangup is answered first when both have arrived: SIGTERM asks for an
    /// ending the terminal will be there to see, and a terminal that has gone
    /// is the truer of the two answers.
    #[cfg(unix)]
    pub fn requested(&self) -> Option<Stop> {
        if self.hung_up.load(Ordering::SeqCst) {
            Some(Stop::TerminalGone)
        } else if self.requested.load(Ordering::SeqCst) {
            Some(Stop::Requested)
        } else {
            None
        }
    }

    /// What a signal has asked of the session, if one has.
    #[cfg(not(unix))]
    pub fn requested(&self) -> Option<Stop> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEAVE_ALTERNATE_SCREEN: &str = "\x1b[?1049l";
    const ENTER_ALTERNATE_SCREEN: &str = "\x1b[?1049h";
    const SHOW_CURSOR: &str = "\x1b[?25h";
    const MOUSE_ON: &str = "\x1b[?1000h";
    const MOUSE_OFF: &str = "\x1b[?1000l";

    fn written(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_owned()).expect("crossterm writes UTF-8")
    }

    #[test]
    fn dropping_the_guard_restores_the_screen() {
        let mut sink = Vec::new();
        {
            let _guard =
                TerminalGuard::enter_screen_only(&mut sink).expect("a Vec sink cannot fail");
        }

        let out = written(&sink);
        assert!(
            out.contains(ENTER_ALTERNATE_SCREEN),
            "did not enter: {out:?}"
        );
        assert!(
            out.contains(LEAVE_ALTERNATE_SCREEN),
            "did not leave: {out:?}"
        );
        assert!(out.contains(SHOW_CURSOR), "cursor left hidden: {out:?}");
        assert!(
            out.find(ENTER_ALTERNATE_SCREEN) < out.find(LEAVE_ALTERNATE_SCREEN),
            "left before it entered: {out:?}"
        );
        assert!(
            out.find(MOUSE_ON) < out.find(MOUSE_OFF),
            "the mouse was not handed back after it was taken: {out:?}"
        );
    }

    #[test]
    fn restoring_twice_restores_once() {
        let mut sink = Vec::new();
        {
            let mut guard =
                TerminalGuard::enter_screen_only(&mut sink).expect("a Vec sink cannot fail");
            guard.restore().expect("a Vec sink cannot fail");
            guard.restore().expect("a Vec sink cannot fail");
        }

        let out = written(&sink);
        assert_eq!(
            out.matches(LEAVE_ALTERNATE_SCREEN).count(),
            1,
            "restored more than once: {out:?}"
        );
    }

    #[test]
    fn an_unwinding_panic_still_restores() {
        let mut sink = Vec::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard =
                TerminalGuard::enter_screen_only(&mut sink).expect("a Vec sink cannot fail");
            panic!("the backend died mid-draw");
        }));

        assert!(result.is_err(), "the panic was swallowed");
        let out = written(&sink);
        assert!(
            out.contains(LEAVE_ALTERNATE_SCREEN),
            "a panic left the terminal on the alternate screen: {out:?}"
        );
        assert!(out.contains(SHOW_CURSOR), "a panic left the cursor hidden");
        assert!(out.contains(MOUSE_OFF), "a panic left the mouse captured");
    }

    #[test]
    fn a_guard_that_pushed_the_keyboard_pops_it_before_leaving_the_screen() {
        let mut sink = Vec::new();
        {
            let mut guard =
                TerminalGuard::enter_screen_only(&mut sink).expect("a Vec sink cannot fail");
            guard.push_keyboard().expect("a Vec sink cannot fail");
            assert!(guard.reports_shift_enter());
        }

        let out = written(&sink);
        let pushed = out.find("\x1b[>1u").expect("the keyboard was pushed");
        let popped = out.find("\x1b[<1u").expect("the keyboard was never popped");
        assert!(
            out.find(ENTER_ALTERNATE_SCREEN) < Some(pushed),
            "pushed onto the main screen's stack: {out:?}"
        );
        assert!(
            Some(popped) < out.find(LEAVE_ALTERNATE_SCREEN),
            "popped after leaving the screen it was pushed on: {out:?}"
        );
    }

    #[test]
    fn a_guard_that_never_pushed_the_keyboard_never_pops_it() {
        let mut sink = Vec::new();
        {
            let guard =
                TerminalGuard::enter_screen_only(&mut sink).expect("a Vec sink cannot fail");
            assert!(!guard.reports_shift_enter());
        }

        let out = written(&sink);
        assert!(
            !out.contains("\x1b[<1u"),
            "popped what it never pushed: {out:?}"
        );
    }

    #[test]
    fn keyboard_flags_before_the_attributes_are_a_terminal_that_reports_keys() {
        assert_eq!(answer(b"\x1b[?0u\x1b[?62;22c"), Answer::Reports);
    }

    #[test]
    fn the_attributes_alone_are_a_terminal_that_does_not() {
        assert_eq!(answer(b"\x1b[?62;22c"), Answer::DoesNot);
    }

    #[test]
    fn an_answer_cut_off_mid_reply_is_still_awaited() {
        assert_eq!(answer(b""), Answer::Waiting);
        assert_eq!(answer(b"\x1b[?0u"), Answer::Waiting);
        assert_eq!(answer(b"\x1b[?0u\x1b[?6"), Answer::Waiting);
    }

    #[test]
    fn keys_typed_around_the_replies_do_not_change_the_answer() {
        assert_eq!(answer(b"ab\x1b[?1u\x1bx\x1b[?1;2c"), Answer::Reports);
        assert_eq!(answer(b"ab\x1b[A\x1b[?1;2c"), Answer::DoesNot);
    }

    /// A signal is delivered to the process, not to the `Shutdown` that asked
    /// for it: a raise sets the flag of every `Shutdown` any test installed. So
    /// the tests that install and the tests that raise take this in turn, and
    /// each one sees only the signals it sent itself.
    static SIGNALLING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_shutdown_starts_unrequested() {
        let _turn = SIGNALLING.lock().expect("no test panics holding this");

        let shutdown = Shutdown::install().expect("registering the signals cannot fail here");

        assert_eq!(shutdown.requested(), None);
    }

    #[cfg(unix)]
    #[test]
    fn sigterm_asks_the_event_loop_to_stop() {
        let _turn = SIGNALLING.lock().expect("no test panics holding this");
        let shutdown = Shutdown::install().expect("registering the signals cannot fail here");

        // Sent to this process: the handler registered above catches it instead
        // of the default disposition killing the test.
        signal_hook::low_level::raise(signal_hook::consts::SIGTERM)
            .expect("raising a signal we handle cannot fail");

        assert_eq!(
            shutdown.requested(),
            Some(Stop::Requested),
            "SIGTERM did not reach the flag"
        );
    }

    #[cfg(unix)]
    #[test]
    fn sighup_says_the_terminal_went_away_rather_than_that_a_stop_was_asked_for() {
        let _turn = SIGNALLING.lock().expect("no test panics holding this");
        let shutdown = Shutdown::install().expect("registering the signals cannot fail here");

        signal_hook::low_level::raise(signal_hook::consts::SIGHUP)
            .expect("raising a signal we handle cannot fail");

        // Read as a stop, a session whose terminal hung up ends as a quit: the
        // shell then tries to hand that terminal back and to print on it, and
        // reports failing at both as a session that failed.
        assert_eq!(shutdown.requested(), Some(Stop::TerminalGone));
    }
}
