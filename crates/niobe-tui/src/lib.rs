// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The terminal UI.
//!
//! Depends on [`niobe_core`] and nothing else from the workspace: no bridge
//! type may reach this crate, which is what keeps a second backend cheap.
//! `cargo xtask layering` fails the build if that stops being true.
//!
//! The shell is three regions — a menu bar, the panes and an F-key bar —
//! drawn from [`app::App`], which is a fold over
//! [`niobe_core::event::Event`] and nothing else. Two properties are worth
//! stating because the rest of the crate is arranged around them:
//!
//! * **Nothing on screen is invented.** Where the fold has no number the pane
//!   draws an em dash, and says what is not wired up where that is not obvious.
//! * **The terminal is always handed back.** A guard, a panic hook and a signal
//!   handler cover the three ways out; see [`terminal`]. A fourth — a terminal
//!   that goes away — ends the session rather than failing it: there is nothing
//!   left to hand the terminal back to, and losing a terminal is not a session
//!   that went wrong. The event loop can find that out three ways, and they all
//!   end the session the same: a hangup on the descriptor it waits on, SIGHUP
//!   from the kernel, and the draw at the top of a tick failing on a terminal
//!   that went in the moment before it.

pub mod app;
pub mod bridge;
mod calls;
pub mod clock;
mod find;
mod fx;
mod hunks;
mod input;
pub mod journal;
mod keys;
mod markdown;
mod mention;
mod meter;
pub mod prices;
pub mod rules;
pub mod run;
pub mod shell;
pub mod terminal;
mod text;
pub mod theme;
mod tree;
mod turns;
pub mod ui;
pub mod usage;
pub mod watch;

pub use app::{
    Answer, App, Ask, Call, Commit, Entry, EntryKind, Picker, Repo, Section, SelectedProfile,
    WorkingFile,
};
pub use bridge::{Bridge, BridgeError, Detached};
pub use journal::{Journal, JournalError, Unrecorded};
pub use prices::Prices;
pub use rules::{Forgotten, Rules, RulesError};
pub use run::{Ended, run};
pub use shell::{NoShell, OPERATOR_SHELL, Ran, Shell, ShellError};
pub use terminal::{Shutdown, Stop, TerminalGuard, install_panic_hook};
pub use theme::{CLASSIC, CYBER, MODERN, NEO, Theme};
pub use ui::{MIN_SIZE, WIDE_COLUMNS, draw, session_cost};
pub use watch::{Unwatched, Watch};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimum_size_is_the_classic_terminal() {
        assert_eq!(MIN_SIZE, (80, 24));
    }

    #[test]
    fn the_right_stack_needs_a_hundred_columns() {
        assert!(WIDE_COLUMNS > MIN_SIZE.0);
    }
}
