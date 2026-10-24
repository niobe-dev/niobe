// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The project overview, asked of the binary and read in the shell.
//!
//! Every session recorded in the repository, folded, is the binary's to read:
//! the shell touches no store. So the loop asks for the page the way it asks
//! [`crate::memory::Memory`] for the agent's files — once each time the view
//! opens, never waited on — and the page is read whole, scrolled, in the
//! dialog a tool call is read in.

/// Something that writes the project overview for the shell.
pub trait Overview: std::fmt::Debug {
    /// Starts writing the page from what is recorded now. Returns at once;
    /// the page arrives through [`Overview::drain`]. A load asked for while
    /// one is under way is that one.
    fn load(&mut self);

    /// The page the newest load that has finished wrote, once: Markdown, a
    /// line to a row. Never blocks.
    fn drain(&mut self) -> Option<String>;
}

/// No sessions to read: a recorded log being looked at. Every load writes an
/// empty page, at once.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOverview {
    asked: bool,
}

impl Overview for NoOverview {
    fn load(&mut self) {
        self.asked = true;
    }

    fn drain(&mut self) -> Option<String> {
        std::mem::take(&mut self.asked).then(String::new)
    }
}

/// The title the overview's dialog is drawn under, which is also how a page
/// that lands is told from a tool call opened since.
pub(crate) const TITLE: &str = "Project overview";

/// What the dialog says while the binary is still writing the page.
pub(crate) const READING: &str = "reading every session recorded here…";
