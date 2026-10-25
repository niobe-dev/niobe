// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The signals that ask `niobe -p` to stop: SIGTERM, which a CI job's timeout
//! sends before it kills, and SIGINT, which Ctrl+C sends.
//!
//! Left to their default, either ends the run where it stands: the turn's
//! last events never reach the store, no `--json` is printed and the session
//! reads as one cut off. Caught, the first one stops the turn and lets the run
//! finish as it would have, and the run then ends by that same signal, so the
//! shell or job runner that sent it sees what it expects — a `for` loop in
//! `sh` stops at a child Ctrl+C ended, and a runner reads 143 or 130. A second
//! signal of either kind does what it would have without any of this, so a
//! run whose teardown stalls can still be ended at once.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A signal that asked the run to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopping {
    /// SIGTERM.
    Terminated,
    /// SIGINT.
    Interrupted,
}

impl Stopping {
    /// The signal's name, as `kill -l` and a job runner's log write it.
    pub fn name(self) -> &'static str {
        match self {
            Stopping::Terminated => "SIGTERM",
            Stopping::Interrupted => "SIGINT",
        }
    }

    /// The signal's number.
    fn number(self) -> i32 {
        match self {
            Stopping::Terminated => signal_hook::consts::SIGTERM,
            Stopping::Interrupted => signal_hook::consts::SIGINT,
        }
    }

    /// What a shell reports a process this signal ended as: 128 and the
    /// signal's number.
    pub fn status(self) -> u8 {
        u8::try_from(self.number()).map_or(u8::MAX, |number| number.saturating_add(128))
    }
}

/// The two flags the stopping signals set, once registered.
#[derive(Debug)]
pub struct Signals {
    terminated: Arc<AtomicBool>,
    interrupted: Arc<AtomicBool>,
}

impl Signals {
    /// Registers the handlers. The default action is registered first and
    /// runs only once a flag is set, so the first signal sets its flag and a
    /// later one ends the process as it would have.
    pub fn watch() -> std::io::Result<Self> {
        let terminated = Arc::new(AtomicBool::new(false));
        let interrupted = Arc::new(AtomicBool::new(false));
        for (signal, flag) in [
            (signal_hook::consts::SIGTERM, &terminated),
            (signal_hook::consts::SIGINT, &interrupted),
        ] {
            for either in [&terminated, &interrupted] {
                signal_hook::flag::register_conditional_default(signal, Arc::clone(either))?;
            }
            signal_hook::flag::register(signal, Arc::clone(flag))?;
        }
        Ok(Self {
            terminated,
            interrupted,
        })
    }

    /// The signal that asked the run to stop, if one has; SIGTERM where both
    /// have, since it is the one a runner sends before it kills.
    pub fn received(&self) -> Option<Stopping> {
        if self.terminated.load(Ordering::SeqCst) {
            Some(Stopping::Terminated)
        } else if self.interrupted.load(Ordering::SeqCst) {
            Some(Stopping::Interrupted)
        } else {
            None
        }
    }
}

/// Ends the process by `signal`, as it would have ended had nothing caught
/// it. Returns only where that could not be done, and the caller then exits
/// with [`Stopping::status`], which is what a shell reads either way.
pub fn end_by(signal: Stopping) {
    let _ = signal_hook::low_level::emulate_default_handler(signal.number());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signalled_run_reports_the_status_a_shell_reads_for_it() {
        assert_eq!(Stopping::Terminated.status(), 143);
        assert_eq!(Stopping::Interrupted.status(), 130);
    }
}
