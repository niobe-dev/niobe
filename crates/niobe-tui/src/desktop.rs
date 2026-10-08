// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What the shell hands to the desktop around it: text the operator selected,
//! to go on the clipboard, a link they clicked, to open, and a memory file
//! they asked to edit, to go to a text editor.
//!
//! A file to edit is never handed to whatever the system opens files with:
//! that dispatches on the file's kind, and runs a `.command` or a `.desktop`
//! file rather than showing it, while the file is one a repository named. It
//! goes to a text editor, on the terminal the shell hands back while it runs.
//!
//! The shell runs no program, so it says what it wants and the binary hands
//! it something that does it, the way [`crate::images`] is handed something
//! that reads the clipboard.

/// Something that copies text, opens links and edits files for the shell.
///
/// A copy and a link are not waited on past starting: the loop has a
/// terminal to draw on, and a browser that takes a second to come forward
/// must not freeze it. An edit is, because the editor has the terminal.
pub trait Desktop: std::fmt::Debug {
    /// Puts `text` on the clipboard. An error says why it could not, in
    /// words the operator reads.
    fn copy(&mut self, text: &str) -> Result<(), String>;

    /// Opens `link` where the operator's system opens links. An error says
    /// why it could not, in words the operator reads.
    fn open(&mut self, link: &str) -> Result<(), String>;

    /// Opens the file at `path` in a text editor, and returns once the editor
    /// has ended. The loop hands the terminal back before it asks and takes
    /// it again after, so an editor that runs on the terminal has it. An
    /// error says why it could not, in words the operator reads.
    fn edit(&mut self, path: &str) -> Result<(), String>;
}

/// No desktop: a recorded log being looked at with nothing around it. Every
/// request is refused, saying so.
#[derive(Debug, Clone, Default)]
pub struct NoDesktop;

impl Desktop for NoDesktop {
    fn copy(&mut self, _text: &str) -> Result<(), String> {
        Err("this session has no clipboard".to_owned())
    }

    fn open(&mut self, _link: &str) -> Result<(), String> {
        Err("this session cannot open links".to_owned())
    }

    fn edit(&mut self, _path: &str) -> Result<(), String> {
        Err("this session has no editor".to_owned())
    }
}

/// One thing the shell has asked of the desktop and not yet handed over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handoff {
    /// Text to put on the clipboard.
    Copy(String),
    /// A link to open.
    Open(String),
    /// A file to open in a text editor, on the terminal.
    Edit(String),
}

impl Handoff {
    /// Whether the terminal is handed back while this is handed over: only
    /// an editor runs on it.
    pub fn lends_the_terminal(&self) -> bool {
        match self {
            Self::Copy(_) | Self::Open(_) => false,
            Self::Edit(_) => true,
        }
    }
}
