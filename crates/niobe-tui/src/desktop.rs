// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What the shell hands to the desktop around it: text the operator selected,
//! to go on the clipboard, and a link they clicked, to open.
//!
//! The shell runs no program, so it says what it wants and the binary hands
//! it something that does it, the way [`crate::images`] is handed something
//! that reads the clipboard.

/// Something that copies text and opens links for the shell.
///
/// Neither is waited on past starting: the loop has a terminal to draw on,
/// and a browser that takes a second to come forward must not freeze it.
pub trait Desktop: std::fmt::Debug {
    /// Puts `text` on the clipboard. An error says why it could not, in
    /// words the operator reads.
    fn copy(&mut self, text: &str) -> Result<(), String>;

    /// Opens `link` where the operator's system opens links. An error says
    /// why it could not, in words the operator reads.
    fn open(&mut self, link: &str) -> Result<(), String>;
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
}

/// One thing the shell has asked of the desktop and not yet handed over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handoff {
    /// Text to put on the clipboard.
    Copy(String),
    /// A link to open.
    Open(String),
}
