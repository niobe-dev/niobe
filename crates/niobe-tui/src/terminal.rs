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
//!   so the guard drops normally.
//!
//! All three are idempotent, so overlapping paths — a panic while a SIGTERM is
//! pending — restore once and do not fight each other.
//!
//! A terminal can also go away without any of that: no signal is sent when the
//! process is not in the session that owns it. The event loop's own wait for
//! input sees the hangup and quits, which is the first path above.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

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

/// Writes the sequences that take a terminal out of the drawing mode.
///
/// Separated from [`TerminalGuard`] so that the panic hook, which cannot reach
/// the guard, emits exactly the same bytes.
fn leave(out: &mut impl Write) -> io::Result<()> {
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
    restored: bool,
}

impl<W: Write> TerminalGuard<W> {
    /// Enters raw mode and the alternate screen, and hides the cursor.
    pub fn enter(out: W) -> io::Result<Self> {
        enable_raw_mode()?;

        // The guard exists before the alternate screen is entered, so that a
        // failure on the way in is still a guard that undoes raw mode as it
        // unwinds rather than an error returned from a raw terminal.
        let mut guard = Self {
            out,
            raw_mode: true,
            restored: false,
        };
        // Set before the screen is entered, not after: it is what
        // `restore` checks, and a failure on the next line has to undo raw
        // mode rather than decide there was nothing to undo.
        TERMINAL_ENTERED.store(true, Ordering::SeqCst);
        execute!(guard.out, EnterAlternateScreen, Hide)?;

        Ok(guard)
    }

    /// Enters the alternate screen without touching raw mode.
    ///
    /// Raw mode is a property of the process's controlling terminal, not of the
    /// sink, so a test that enabled it would put the test harness itself into
    /// raw mode. Tests use this; the shell does not.
    #[cfg(test)]
    fn enter_screen_only(mut out: W) -> io::Result<Self> {
        execute!(out, EnterAlternateScreen, Hide)?;
        Ok(Self {
            out,
            raw_mode: false,
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
            return leave(&mut self.out);
        }

        // A panic unwinds through this guard after the hook has already
        // restored, and the hook clears the same flag: claiming it here is what
        // keeps the two paths from each writing a sequence.
        if !TERMINAL_ENTERED.swap(false, Ordering::SeqCst) {
            return Ok(());
        }

        // Both run even if the first fails: half a restoration is a terminal
        // the operator has to fix by hand.
        let raw = disable_raw_mode();
        let screen = leave(&mut self.out);
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
            let _ = disable_raw_mode();
            let _ = leave(&mut io::stdout());
        }
        previous(info);
    }));
}

/// A flag set when the process is asked to stop.
///
/// The default disposition for SIGTERM kills the process outright, which leaves
/// the terminal in raw mode on the alternate screen. Catching it costs one
/// atomic read per tick and turns the signal into an ordinary quit.
#[derive(Debug, Clone)]
pub struct Shutdown {
    #[cfg(unix)]
    requested: std::sync::Arc<AtomicBool>,
}

impl Shutdown {
    /// Registers the handlers. On a platform without POSIX signals this is a
    /// flag that is never set, and the guard still covers every other path.
    #[cfg(unix)]
    pub fn install() -> io::Result<Self> {
        let requested = std::sync::Arc::new(AtomicBool::new(false));
        for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGHUP] {
            signal_hook::flag::register(signal, std::sync::Arc::clone(&requested))?;
        }
        Ok(Self { requested })
    }

    /// Registers the handlers.
    #[cfg(not(unix))]
    pub fn install() -> io::Result<Self> {
        Ok(Self {})
    }

    /// Whether a stop has been asked for.
    #[cfg(unix)]
    pub fn requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    /// Whether a stop has been asked for.
    #[cfg(not(unix))]
    pub fn requested(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEAVE_ALTERNATE_SCREEN: &str = "\x1b[?1049l";
    const ENTER_ALTERNATE_SCREEN: &str = "\x1b[?1049h";
    const SHOW_CURSOR: &str = "\x1b[?25h";

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
    }

    #[test]
    fn a_shutdown_starts_unrequested() {
        let shutdown = Shutdown::install().expect("registering SIGTERM cannot fail here");
        assert!(!shutdown.requested());
    }

    #[cfg(unix)]
    #[test]
    fn sigterm_sets_the_flag_the_event_loop_reads() {
        let shutdown = Shutdown::install().expect("registering SIGTERM cannot fail here");
        assert!(!shutdown.requested());

        // Sent to this process: the handler registered above catches it instead
        // of the default disposition killing the test.
        signal_hook::low_level::raise(signal_hook::consts::SIGTERM)
            .expect("raising a signal we handle cannot fail");

        assert!(shutdown.requested(), "SIGTERM did not reach the flag");
    }
}
