// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What the shell is attached to.
//!
//! The shell cannot name a bridge: it depends on `niobe-core` alone, and a
//! backend's types reaching this crate is the one thing the layering exists to
//! prevent. So it says what it needs — somewhere to send a turn, somewhere to
//! take events from — and the binary hands it something that does it, the same
//! way [`crate::journal`] is handed a session store.
//!
//! Both halves are deliberately non-blocking. The event loop has a terminal to
//! draw and a signal flag to read on every tick, so it can never be inside a
//! backend waiting for a reply.

use niobe_core::event::Event;

/// Why a turn could not be sent. Shown to the operator as it reads.
pub type BridgeError = Box<dyn std::error::Error + Send + Sync>;

/// A backend a session is attached to.
///
/// `Debug` is required so that whatever holds one can derive it: the workspace
/// asks every public type to be printable, and a backend handle is one of the
/// few things a bug report wants named.
pub trait Bridge: std::fmt::Debug {
    /// Sends one turn. Returns once the backend has it, not once it has
    /// answered: the answer arrives through [`Bridge::drain`].
    fn send(&mut self, prompt: &str) -> Result<(), BridgeError>;

    /// Everything the backend has produced since the last call, oldest first.
    /// Never blocks; an empty answer means nothing has arrived yet, never that
    /// the session is over.
    fn drain(&mut self) -> Vec<Event>;
}

/// No backend: a recorded log being looked at rather than continued, and a
/// session started under no profile at all.
#[derive(Debug, Clone, Copy, Default)]
pub struct Detached;

impl Bridge for Detached {
    fn send(&mut self, _prompt: &str) -> Result<(), BridgeError> {
        Err("this session is not attached to a backend".into())
    }

    fn drain(&mut self) -> Vec<Event> {
        Vec::new()
    }
}
