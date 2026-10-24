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
///
/// Both methods are called on the loop that draws the shell, so neither may
/// wait on whatever the events are kept in: a store another process holds
/// locked would hold every key and every streamed word up with it. A journal
/// that writes somewhere that can wait hands the event on and says how the
/// write went later, from [`settled`](Journal::settled).
pub trait Journal {
    /// Hands one event on to be kept. Called in the order the events
    /// happened, before the frame that shows them is drawn.
    fn append(&mut self, event: &Event);

    /// How each event handed on since the last call was kept, in the order
    /// they were handed on, as far as their writes have finished. Every event
    /// is answered once, by this call or a later one, so one the journal could
    /// not keep is reported rather than lost.
    fn settled(&mut self) -> Vec<Result<(), JournalError>>;

    /// The number the session is recorded under, as `niobe sessions` prints
    /// it, once it is. Nothing for a journal that keeps no numbered record.
    fn recorded_as(&self) -> Option<String> {
        None
    }
}

/// A journal that keeps nothing: for a recorded log that is being looked at
/// rather than continued.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unrecorded;

impl Journal for Unrecorded {
    fn append(&mut self, _event: &Event) {}

    /// Nothing: an event nobody asked to keep was neither kept nor lost.
    fn settled(&mut self) -> Vec<Result<(), JournalError>> {
        Vec::new()
    }
}
