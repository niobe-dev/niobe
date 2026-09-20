// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What the repository looks like now.
//!
//! The shell touches no filesystem and cannot run `git`, so it says what it
//! needs — somewhere to ask what the repository has become — and the binary
//! hands it something that does it, the same way [`crate::journal`] is handed a
//! session store.
//!
//! Reading a repository is not free: counting the working tree's changed lines
//! costs tens of milliseconds on a small tree and grows with it, which is
//! several frames' worth of a 60 Hz budget. So [`Watch::look`] never reads
//! anything itself. It is asked once a tick and answers with a read that has
//! already finished elsewhere, or with nothing — and nothing leaves the last
//! read that worked on screen, which is also what a read that failed or is
//! still running looks like from here.

use crate::app::Repo;

/// Where the shell gets the state of the repository it is running in.
pub trait Watch {
    /// A read that has finished since the last call, or `None` when there is
    /// none: nothing has changed, nothing has come back yet, or the last
    /// attempt failed. Never blocks — the loop has a terminal to draw on every
    /// tick and can never be inside a subprocess waiting for it.
    fn look(&mut self) -> Option<Repo>;

    /// Tells the watch the session has just changed a file, so that the pane
    /// catches up with an edit sooner than the next scheduled read would.
    /// Advisory: a watch that reads on its own cadence may ignore it.
    fn changed(&mut self) {}
}

/// A watch that never reports anything: a recorded log being looked at rather
/// than continued, and a session running nowhere in particular.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unwatched;

impl Watch for Unwatched {
    fn look(&mut self) -> Option<Repo> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_that_is_watching_nothing_is_never_handed_a_read() {
        assert_eq!(Unwatched.look(), None);
    }
}
