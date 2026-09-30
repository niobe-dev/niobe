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
//! A stop is not an exit, but it hands the terminal to whatever stopped the
//! process, which is usually the operator's own shell: SIGTSTP is caught by
//! [`Shutdown`] as well, and the event loop hands the terminal back with
//! [`TerminalGuard::suspend`], stops the process with [`stop_until_continued`]
//! and takes the terminal again with [`TerminalGuard::resume`] when it is
//! continued.
//!
//! Where the terminal can report keys the legacy encoding cannot tell apart —
//! Shift+Enter from Enter, above all — the guard asks it to: with the kitty
//! protocol where the terminal answers its query, and with xterm's
//! modifyOtherKeys where it answers only its attributes, which is how tmux
//! answers. Every one of those paths takes back whichever was asked for.
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
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
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

/// What the terminal was asked to report keys with and still owes being asked
/// to stop, as an [`Asked`].
///
/// Read by the panic hook for the same reason as [`TERMINAL_ENTERED`]; kept
/// apart from it because a terminal is sent only the sequence that takes back
/// what it was asked for, and nothing where it was asked for nothing.
static KEYBOARD_ASKED: AtomicU8 = AtomicU8::new(Asked::Nothing as u8);

/// What the terminal was asked to report the keys the legacy encoding cannot
/// tell apart with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Asked {
    /// Nothing: it answered neither way, or its keys are not read here.
    Nothing,
    /// The kitty protocol's disambiguation, pushed onto its stack.
    Enhancement,
    /// xterm's modifyOtherKeys.
    OtherKeys,
}

impl Asked {
    /// The value [`KEYBOARD_ASKED`] held, read back.
    fn from_stored(stored: u8) -> Self {
        match stored {
            stored if stored == Self::Enhancement as u8 => Self::Enhancement,
            stored if stored == Self::OtherKeys as u8 => Self::OtherKeys,
            _ => Self::Nothing,
        }
    }
}

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

/// Asks the terminal to mark the start and end of a paste, so that the lines
/// of one are read as text rather than as a key press each, Enter included.
const PASTE_ON: &[u8] = b"\x1b[?2004h";

/// Takes [`PASTE_ON`] back. Harmless on a terminal that was never asked, for
/// the same reason as [`MOUSE_OFF`]; one left asked would wrap every paste
/// into the shell the operator returns to in escape sequences.
const PASTE_OFF: &[u8] = b"\x1b[?2004l";

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

/// Asks a terminal that did not answer the kitty query for xterm's
/// modifyOtherKeys, which is what tmux reports Shift+Enter in under
/// `extended-keys on`: as `CSI 27 ; 2 ; 13 ~`, which [`crate::keys`] reads.
///
/// Level 1, not 2. Measured with tmux 3.7c, level 1 changes Shift+Enter and
/// leaves Ctrl+J the line feed and Alt+Enter an Esc and a carriage return;
/// level 2 sends those as sequences too. A terminal that knows neither
/// protocol reads this as nothing, and tmux under `extended-keys off` ignores
/// it, which leaves the keys as they were.
const OTHER_KEYS_ON: &[u8] = b"\x1b[>4;1m";

/// Takes [`OTHER_KEYS_ON`] back to whatever the terminal had before.
const OTHER_KEYS_OFF: &[u8] = b"\x1b[>4m";

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

/// What was typed around the replies in `heard`: every byte but those of an
/// `ESC [ ? … u` or `ESC [ ? … c` reply, in the order it came.
fn typed_around(heard: &[u8]) -> Vec<u8> {
    const INTRODUCER: &[u8] = b"\x1b[?";
    let mut typed = Vec::new();
    let mut rest = heard;
    while let Some(start) = rest
        .windows(INTRODUCER.len())
        .position(|window| window == INTRODUCER)
    {
        typed.extend_from_slice(&rest[..start]);
        let body = &rest[start + INTRODUCER.len()..];
        match body
            .iter()
            .position(|byte| !(byte.is_ascii_digit() || *byte == b';'))
        {
            Some(end) if matches!(body[end], b'u' | b'c') => rest = &body[end + 1..],
            // Not a reply: what looked like one was typed.
            _ => {
                typed.extend_from_slice(&rest[start..start + INTRODUCER.len()]);
                rest = body;
            }
        }
    }
    typed.extend_from_slice(rest);
    typed
}

/// Asks the terminal whether it can report Shift+Enter, and waits for the
/// answer on standard input. [`Answer::Waiting`] is none having come.
///
/// Asked here rather than with crossterm's own query, which writes to
/// `/dev/tty` whenever it can open it: that is the controlling terminal, and a
/// process can be drawing on a terminal that is not its controlling one. The
/// question goes where the screen is drawn and the answer is read where the
/// keys are, which is the terminal the shell is actually on.
///
/// The answer is read before the shell reads any key, so whatever the
/// operator types in the moment it takes is read with it; it is given back
/// beside the answer, for the shell to take as the first keys it reads. Only
/// standard input is asked: when it is not a terminal the keys come from
/// `/dev/tty`, which `poll(2)` cannot wait on everywhere, and the legacy keys
/// are what the shell falls back on.
#[cfg(unix)]
fn reports_keys(out: &mut impl Write) -> (Answer, Vec<u8>) {
    use std::io::IsTerminal;
    use std::os::fd::AsFd;

    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return (Answer::Waiting, Vec::new());
    }
    if out
        .write_all(KEYBOARD_QUERY)
        .and_then(|()| out.flush())
        .is_err()
    {
        return (Answer::Waiting, Vec::new());
    }

    let deadline = std::time::Instant::now() + KEYBOARD_PATIENCE;
    let mut heard = Vec::new();
    let mut buffer = [0u8; 256];
    let answered = loop {
        match answer(&heard) {
            Answer::Waiting => {}
            known => break known,
        }
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break Answer::Waiting;
        }
        // Idle is a wait a signal cut short as well as one that ran out, so
        // the deadline rather than the wait decides when to stop.
        match crate::input::poll_input(stdin.as_fd(), left) {
            Ok(crate::input::Input::Ready) => {}
            Ok(crate::input::Input::Idle) => continue,
            Ok(crate::input::Input::HungUp) | Err(_) => break Answer::Waiting,
        }
        match rustix::io::read(stdin.as_fd(), &mut buffer) {
            Ok(0) | Err(_) => break Answer::Waiting,
            Ok(read) => heard.extend_from_slice(&buffer[..read]),
        }
    };
    (answered, typed_around(&heard))
}

/// Asks the terminal whether it can report Shift+Enter. Only the POSIX side
/// asks; elsewhere the legacy keys are what the shell uses.
#[cfg(not(unix))]
fn reports_keys(_out: &mut impl Write) -> (Answer, Vec<u8>) {
    (Answer::Waiting, Vec::new())
}

/// Writes the sequences that put a terminal into the drawing mode.
fn enter_screen(out: &mut impl Write) -> io::Result<()> {
    execute!(out, EnterAlternateScreen, Hide)?;
    out.write_all(MOUSE_ON)?;
    out.write_all(PASTE_ON)?;
    out.flush()
}

/// Writes the sequences that take a terminal out of the drawing mode, and
/// takes back what `keyboard` says the terminal was asked to report keys with.
///
/// Separated from [`TerminalGuard`] so that the panic hook, which cannot reach
/// the guard, emits exactly the same bytes. The mouse and the paste markers go
/// back first: a terminal left reporting them would print escape sequences
/// into the shell the operator returns to at every click and every paste. The keyboard is popped before the
/// alternate screen is left, because the protocol keeps one stack per screen
/// and the push was made on this one.
fn leave(out: &mut impl Write, keyboard: Asked) -> io::Result<()> {
    out.write_all(MOUSE_OFF)?;
    out.write_all(PASTE_OFF)?;
    match keyboard {
        Asked::Nothing => {}
        Asked::Enhancement => out.write_all(KEYBOARD_OFF)?,
        Asked::OtherKeys => out.write_all(OTHER_KEYS_OFF)?,
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
    /// What this guard asked the terminal to report keys with, and so owes
    /// taking back.
    keyboard: Asked,
    /// Whether the terminal is handed back for a stop, and so owes being
    /// taken again rather than handed back a second time.
    suspended: bool,
    restored: bool,
    /// What the operator typed while the shell waited for the terminal's
    /// answer about its keyboard, not yet read as keys.
    typed: Vec<u8>,
}

impl<W: Write> TerminalGuard<W> {
    /// Enters raw mode and the alternate screen, hides the cursor, asks for
    /// the mouse wheel and clicks, for pastes to be bracketed, and for
    /// Shift+Enter to be told from Enter: with the kitty protocol where the
    /// terminal says it knows it, and with modifyOtherKeys where it answered
    /// without saying so.
    pub fn enter(out: W) -> io::Result<Self> {
        enable_raw_mode()?;

        // The guard exists before the alternate screen is entered, so that a
        // failure on the way in is still a guard that undoes raw mode as it
        // unwinds rather than an error returned from a raw terminal.
        let mut guard = Self {
            out,
            raw_mode: true,
            keyboard: Asked::Nothing,
            suspended: false,
            restored: false,
            typed: Vec::new(),
        };
        // Set before the screen is entered, not after: it is what
        // `restore` checks, and a failure on the next line has to undo raw
        // mode rather than decide there was nothing to undo.
        TERMINAL_ENTERED.store(true, Ordering::SeqCst);
        enter_screen(&mut guard.out)?;
        // On the alternate screen, where the push is made: the protocol keeps
        // a stack per screen. modifyOtherKeys is asked for only where an
        // answer came, which is where standard input is a terminal `poll(2)`
        // sees — the one whose keys the shell reads itself, and so the one
        // where the form it arrives in is read rather than dropped.
        let (answered, typed) = reports_keys(&mut guard.out);
        guard.typed = typed;
        match answered {
            Answer::Reports => guard.ask_keyboard(Asked::Enhancement)?,
            Answer::DoesNot => guard.ask_keyboard(Asked::OtherKeys)?,
            Answer::Waiting => {}
        }

        Ok(guard)
    }

    /// Asks the terminal to tell Shift+Enter from Enter as `asked` says.
    /// Marked as owed before it is written, for the same reason as the screen:
    /// a write that fails halfway still leaves a terminal that may have taken
    /// the request.
    fn ask_keyboard(&mut self, asked: Asked) -> io::Result<()> {
        self.keyboard = asked;
        if self.raw_mode {
            KEYBOARD_ASKED.store(asked as u8, Ordering::SeqCst);
        }
        let request = match asked {
            Asked::Nothing => return Ok(()),
            Asked::Enhancement => KEYBOARD_ON,
            Asked::OtherKeys => OTHER_KEYS_ON,
        };
        self.out.write_all(request)?;
        self.out.flush()
    }

    /// Whether the terminal said it can tell Shift+Enter from Enter, and was
    /// asked to. Only then is Shift+Enter a key the shell can name before it
    /// has arrived: modifyOtherKeys is asked for on a terminal that did not
    /// say, and tmux asked for it still sends the carriage return Enter sends
    /// when the terminal it runs in cannot tell the two apart.
    pub fn reports_shift_enter(&self) -> bool {
        self.keyboard == Asked::Enhancement
    }

    /// What the operator typed while the terminal was being asked about its
    /// keyboard, for the shell to read before anything it reads after.
    pub fn take_typed(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.typed)
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
            keyboard: Asked::Nothing,
            suspended: false,
            restored: false,
            typed: Vec::new(),
        })
    }

    /// Puts the terminal back. Safe to call more than once.
    pub fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        // A terminal handed back for a stop that was never taken again is
        // already the operator's.
        if self.suspended {
            return Ok(());
        }
        self.hand_back()
    }

    /// Hands the terminal back for a stop, as [`Self::restore`] would, but
    /// keeps what was asked of it so that [`Self::resume`] can ask again.
    /// Does nothing to a terminal already handed back.
    pub fn suspend(&mut self) -> io::Result<()> {
        if self.restored || self.suspended {
            return Ok(());
        }
        self.suspended = true;
        self.hand_back()
    }

    /// Takes the terminal again after [`Self::suspend`]: raw mode, the
    /// alternate screen, the mouse, bracketed pastes, and the keys as they
    /// were first asked for. The terminal is not asked again what it can
    /// report — it is the same terminal, and waiting on the answer would hold
    /// the frame up.
    ///
    /// What the last frame drew is not assumed to be on the screen any more,
    /// so the caller owes a whole frame rather than what changed since it.
    pub fn resume(&mut self) -> io::Result<()> {
        if self.restored || !self.suspended {
            return Ok(());
        }
        if self.raw_mode {
            enable_raw_mode()?;
            TERMINAL_ENTERED.store(true, Ordering::SeqCst);
        }
        // Cleared only once raw mode is back: until then, a failure has
        // nothing to undo, and a restore that followed would write a
        // hand-back the terminal already had.
        self.suspended = false;
        enter_screen(&mut self.out)?;
        self.ask_keyboard(self.keyboard)
    }

    /// Takes the terminal again after a stop the process could not catch.
    ///
    /// A SIGSTOP stops the process with the terminal as it held it, and the
    /// operator's shell may have taken it back meanwhile and left it cooked.
    /// crossterm believes raw mode is still on and would do nothing when asked
    /// for it, so it is let go and taken again, which reads the terminal's
    /// modes as they are now. The screen, the mouse and pastes are asked for
    /// again, which changes nothing where they are already on; the enhanced
    /// keys are let go before they are asked for, because a terminal keeps
    /// them as a stack and a second push would outlive the quit's one pop.
    /// Does nothing to a terminal handed back or suspended.
    pub fn reassert(&mut self) -> io::Result<()> {
        if self.restored || self.suspended {
            return Ok(());
        }
        if self.raw_mode {
            disable_raw_mode()?;
            enable_raw_mode()?;
            TERMINAL_ENTERED.store(true, Ordering::SeqCst);
        }
        enter_screen(&mut self.out)?;
        if self.keyboard == Asked::Enhancement {
            self.out.write_all(KEYBOARD_OFF)?;
        }
        self.ask_keyboard(self.keyboard)
    }

    /// Writes what takes the terminal out of the drawing mode, once, however
    /// many of the ways out reach it.
    fn hand_back(&mut self) -> io::Result<()> {
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
        KEYBOARD_ASKED.store(Asked::Nothing as u8, Ordering::SeqCst);
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
            let keyboard =
                Asked::from_stored(KEYBOARD_ASKED.swap(Asked::Nothing as u8, Ordering::SeqCst));
            let _ = disable_raw_mode();
            let _ = leave(&mut io::stdout(), keyboard);
        }
        previous(info);
    }));
}

/// Stops the process as SIGTSTP's default action would, and returns once it
/// has been continued.
///
/// With SIGSTOP rather than by raising SIGTSTP again: the kernel discards a
/// SIGTSTP whose default action would stop a process in an orphaned process
/// group, and the process has already handed its terminal back by the time it
/// gets here, so it would go on drawing on a terminal it no longer holds.
#[cfg(unix)]
pub fn stop_until_continued() -> io::Result<()> {
    signal_hook::low_level::emulate_default_handler(signal_hook::consts::SIGTSTP)
}

/// Stops the process until it is continued. There is no job control to stop
/// for off the POSIX side, so this returns at once.
#[cfg(not(unix))]
pub fn stop_until_continued() -> io::Result<()> {
    Ok(())
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
/// atomic reads per tick and turns the signal into an ordinary quit. SIGINT and
/// SIGQUIT are caught as the same request: raw mode reads Ctrl+C and Ctrl+\ as
/// keys, so neither arrives from the keyboard, but `kill`, `timeout` and the
/// task runners of editors send them, and their default disposition kills the
/// process just as outright. SIGHUP is caught for the same reason and kept
/// apart from them, because it carries more: the terminal is gone.
///
/// SIGTSTP and SIGCONT are caught too, though neither ends anything. SIGTSTP's
/// default action stops the process where it stands, raw on the alternate
/// screen, and hands that terminal to the operator's shell; caught, it asks
/// the loop to hand the terminal back first. SIGCONT says the process ran
/// again after a stop, including one it could not catch, and that whatever
/// drew on the terminal meanwhile is on the screen now.
#[derive(Debug, Clone)]
pub struct Shutdown {
    #[cfg(unix)]
    requested: std::sync::Arc<AtomicBool>,
    #[cfg(unix)]
    hung_up: std::sync::Arc<AtomicBool>,
    #[cfg(unix)]
    suspend: std::sync::Arc<AtomicBool>,
    #[cfg(unix)]
    continued: std::sync::Arc<AtomicBool>,
    /// Set once the shell is done with the terminal, when this is dropped:
    /// from then on SIGTERM, SIGINT and SIGQUIT do what they would have
    /// without the shell, so a teardown that stalls after the terminal was
    /// handed back can still be ended by sending one again. Never while the
    /// shell holds the terminal, where ending at once would leave it raw.
    ///
    /// `None` until [`Shutdown::hands_back_signals_when_dropped`]: signals are
    /// the process's, and a `Shutdown` a test installs and drops must not
    /// leave the next test's SIGTERM ending the test binary.
    #[cfg(unix)]
    released: Option<std::sync::Arc<AtomicBool>>,
}

/// The signals that ask the session to stop.
#[cfg(unix)]
const STOPPING: [i32; 3] = [
    signal_hook::consts::SIGTERM,
    signal_hook::consts::SIGINT,
    signal_hook::consts::SIGQUIT,
];

impl Drop for Shutdown {
    fn drop(&mut self) {
        #[cfg(unix)]
        self.release();
    }
}

impl Shutdown {
    /// Registers the handlers. On a platform without POSIX signals these are
    /// flags that are never set, and the guard still covers every other path.
    #[cfg(unix)]
    pub fn install() -> io::Result<Self> {
        let requested = std::sync::Arc::new(AtomicBool::new(false));
        let hung_up = std::sync::Arc::new(AtomicBool::new(false));
        for asked in STOPPING {
            signal_hook::flag::register(asked, std::sync::Arc::clone(&requested))?;
        }
        signal_hook::flag::register(signal_hook::consts::SIGHUP, std::sync::Arc::clone(&hung_up))?;
        let suspend = std::sync::Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(
            signal_hook::consts::SIGTSTP,
            std::sync::Arc::clone(&suspend),
        )?;
        let continued = std::sync::Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(
            signal_hook::consts::SIGCONT,
            std::sync::Arc::clone(&continued),
        )?;
        Ok(Self {
            requested,
            hung_up,
            suspend,
            continued,
            released: None,
        })
    }

    /// The same handlers, and once this is dropped — after the shell has
    /// handed the terminal back — SIGTERM, SIGINT and SIGQUIT do what they
    /// would have without them: see [`Shutdown::released`]. For the one
    /// `Shutdown` a process runs its shell under.
    #[cfg(unix)]
    pub fn hands_back_signals_when_dropped(mut self) -> io::Result<Self> {
        let released = std::sync::Arc::new(AtomicBool::new(false));
        for asked in STOPPING {
            signal_hook::flag::register_conditional_default(
                asked,
                std::sync::Arc::clone(&released),
            )?;
        }
        self.released = Some(released);
        Ok(self)
    }

    /// [`Shutdown::hands_back_signals_when_dropped`], where there are no
    /// POSIX signals to hand back.
    #[cfg(not(unix))]
    pub fn hands_back_signals_when_dropped(self) -> io::Result<Self> {
        Ok(self)
    }

    /// Hands the three stopping signals back their own behaviour: see
    /// [`Shutdown::released`]. Done when the shell drops this, which is after
    /// it has handed the terminal back.
    #[cfg(unix)]
    fn release(&self) {
        if let Some(released) = &self.released {
            released.store(true, Ordering::SeqCst);
        }
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

    /// Whether a SIGTSTP has asked the process to stop since this was last
    /// asked. Each one is answered once.
    #[cfg(unix)]
    pub fn take_suspend(&self) -> bool {
        self.suspend.swap(false, Ordering::SeqCst)
    }

    /// Whether a SIGTSTP has asked the process to stop.
    #[cfg(not(unix))]
    pub fn take_suspend(&self) -> bool {
        false
    }

    /// Whether the process has been continued after a stop since this was
    /// last asked, and so owes the terminal a whole frame.
    #[cfg(unix)]
    pub fn take_continued(&self) -> bool {
        self.continued.swap(false, Ordering::SeqCst)
    }

    /// Whether the process has been continued after a stop.
    #[cfg(not(unix))]
    pub fn take_continued(&self) -> bool {
        false
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
    const PASTE_ON: &str = "\x1b[?2004h";
    const PASTE_OFF: &str = "\x1b[?2004l";

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
        assert!(
            out.contains(PASTE_ON) && out.find(PASTE_ON) < out.find(PASTE_OFF),
            "a paste was not asked to be marked, or was left marked: {out:?}"
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
            guard
                .ask_keyboard(Asked::Enhancement)
                .expect("a Vec sink cannot fail");
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
    fn a_guard_that_asked_for_modify_other_keys_takes_it_back_and_pops_nothing() {
        let mut sink = Vec::new();
        {
            let mut guard =
                TerminalGuard::enter_screen_only(&mut sink).expect("a Vec sink cannot fail");
            guard
                .ask_keyboard(Asked::OtherKeys)
                .expect("a Vec sink cannot fail");
            // Not until a Shift+Enter has come: the terminal did not say it
            // can send one.
            assert!(!guard.reports_shift_enter());
        }

        let out = written(&sink);
        let asked = out
            .find("\x1b[>4;1m")
            .expect("modifyOtherKeys was asked for");
        let taken_back = out
            .find("\x1b[>4m")
            .expect("modifyOtherKeys was never taken back");
        assert!(asked < taken_back, "{out:?}");
        assert!(
            Some(taken_back) < out.find(LEAVE_ALTERNATE_SCREEN),
            "{out:?}"
        );
        assert!(!out.contains("\x1b[>1u"), "{out:?}");
        assert!(!out.contains("\x1b[<1u"), "{out:?}");
    }

    #[test]
    fn a_suspended_guard_hands_back_then_asks_for_it_all_again_and_restores_once() {
        let mut sink = Vec::new();
        let resumed_at;
        {
            let mut guard =
                TerminalGuard::enter_screen_only(&mut sink).expect("a Vec sink cannot fail");
            guard
                .ask_keyboard(Asked::Enhancement)
                .expect("a Vec sink cannot fail");
            guard.suspend().expect("a Vec sink cannot fail");
            guard.suspend().expect("a Vec sink cannot fail");
            resumed_at = guard.out.len();
            guard.resume().expect("a Vec sink cannot fail");
        }

        let (before, after) = sink.split_at(resumed_at);
        let (before, after) = (written(before), written(after));
        assert_eq!(
            before.matches(LEAVE_ALTERNATE_SCREEN).count(),
            1,
            "{before:?}"
        );
        assert_eq!(before.matches("\x1b[<1u").count(), 1, "{before:?}");
        let entered = after
            .find(ENTER_ALTERNATE_SCREEN)
            .expect("resuming did not enter the alternate screen again");
        let pushed = after
            .find("\x1b[>1u")
            .expect("resuming did not ask for the keyboard it had before");
        assert!(entered < pushed, "pushed onto the main screen: {after:?}");
        assert!(
            after.contains(MOUSE_ON) && after.contains(PASTE_ON),
            "{after:?}"
        );
        assert_eq!(
            after.matches(LEAVE_ALTERNATE_SCREEN).count(),
            1,
            "{after:?}"
        );
        assert_eq!(after.matches("\x1b[<1u").count(), 1, "{after:?}");
    }

    #[test]
    fn a_guard_dropped_while_suspended_does_not_hand_back_twice() {
        let mut sink = Vec::new();
        {
            let mut guard =
                TerminalGuard::enter_screen_only(&mut sink).expect("a Vec sink cannot fail");
            guard.suspend().expect("a Vec sink cannot fail");
        }

        let out = written(&sink);
        assert_eq!(out.matches(LEAVE_ALTERNATE_SCREEN).count(), 1, "{out:?}");
    }

    #[test]
    fn what_was_asked_of_the_keyboard_is_read_back_as_it_was_stored() {
        for asked in [Asked::Nothing, Asked::Enhancement, Asked::OtherKeys] {
            assert_eq!(Asked::from_stored(asked as u8), asked);
        }
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
        assert!(
            !out.contains("\x1b[>4m"),
            "took back modifyOtherKeys it never asked for: {out:?}"
        );
    }

    #[test]
    fn keys_typed_before_between_and_after_the_replies_are_kept_in_order() {
        assert_eq!(typed_around(b"a\x1b[?1uB\x1b[?62;22cC"), b"aBC".to_vec());
        assert_eq!(typed_around(b"\x1b[?62;22c"), Vec::<u8>::new());
        assert_eq!(typed_around(b"\x11"), b"\x11".to_vec(), "a Ctrl+Q alone");
        assert_eq!(typed_around(b"\x1b[?x"), b"\x1b[?x".to_vec(), "not a reply");
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

    #[cfg(unix)]
    #[test]
    fn sigtstp_asks_for_one_suspend_and_ends_nothing() {
        let _turn = SIGNALLING.lock().expect("no test panics holding this");
        let shutdown = Shutdown::install().expect("registering the signals cannot fail here");

        // Caught, as SIGTERM is above: the default action would stop the test.
        signal_hook::low_level::raise(signal_hook::consts::SIGTSTP)
            .expect("raising a signal we handle cannot fail");

        assert!(shutdown.take_suspend());
        assert!(!shutdown.take_suspend(), "one SIGTSTP is one stop");
        assert_eq!(shutdown.requested(), None);
    }

    #[cfg(unix)]
    #[test]
    fn sigcont_says_the_process_was_continued_once() {
        let _turn = SIGNALLING.lock().expect("no test panics holding this");
        let shutdown = Shutdown::install().expect("registering the signals cannot fail here");

        signal_hook::low_level::raise(signal_hook::consts::SIGCONT)
            .expect("raising a signal we handle cannot fail");

        assert!(shutdown.take_continued());
        assert!(!shutdown.take_continued());
        assert_eq!(shutdown.requested(), None);
    }
}
