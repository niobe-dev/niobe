// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Bridge to the official `claude` CLI.
//!
//! Niobe drives the vendor binary as a subprocess and translates its output
//! into the shared event model. It never reads the CLI's credential files and
//! never sets its user agent: the official binary is the only thing that
//! touches subscription credentials, and everything this bridge knows it
//! learned from documented flags and the messages the CLI prints.
//!
//! Two halves, and they are separable on purpose:
//!
//! * [`Translator`] turns one line of stream-json into [`Event`]s. It owns no
//!   process and does no I/O, so a recorded log exercises exactly the code a
//!   live session runs.
//! * [`Session`] spawns the CLI, keeps its standard input open for the life of
//!   the session, and reads its output through a [`Translator`] on a thread.
//!
//! The CLI's own types stay in this crate — they are not public anywhere — so
//! nothing vendor-shaped can reach the TUI or the ledger.
//!
//! ```no_run
//! use niobe_bridge_claude::{Options, Session};
//!
//! let mut session = Session::spawn(&Options::new(".", "max"))?;
//! session.send("what changed in the last commit?")?;
//! for event in session.drain() {
//!     println!("{event:?}");
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! [`Event`]: niobe_core::event::Event

mod driver;
mod translate;
mod wire;

pub use driver::{BINARY, Options, PermissionMode, Session, SpawnError};
pub use translate::Translator;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drives_the_official_binary() {
        assert_eq!(BINARY, "claude");
    }
}
