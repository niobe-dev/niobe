// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Where the events the operator produces in the shell are kept.
//!
//! The shell cannot name the session store: it depends on `niobe-core` alone
//! and touches no filesystem. It says what it needs instead, and the binary
//! hands it something that does it.

use niobe_core::event::Event;

/// Why an event could not be kept. Shown to the operator as it reads.
pub type JournalError = Box<dyn std::error::Error + Send + Sync>;

/// Keeps the events the operator produces, so that a restart can show them.
pub trait Journal {
    /// Keeps one event. Called in the order the events happened, before the
    /// frame that shows them is drawn.
    fn append(&mut self, event: &Event) -> Result<(), JournalError>;
}

/// A journal that keeps nothing: for a recorded log that is being looked at
/// rather than continued.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unrecorded;

impl Journal for Unrecorded {
    fn append(&mut self, _event: &Event) -> Result<(), JournalError> {
        Ok(())
    }
}
