// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Where a standing answer to a permission prompt is kept.
//!
//! The shell decides and matches; it cannot write a config file, because it
//! touches no filesystem. So it says what it needs — somewhere to put a rule
//! the operator has just made — and the binary hands it something that does
//! it, the same way [`crate::journal`] is handed a session store.
//!
//! A rule that is not kept is not a failure of the session: the answer still
//! stands for as long as the shell is open, and the transcript says that it
//! will not outlive it.

use niobe_core::permission::Rule;

/// Why a rule could not be kept. Shown to the operator as it reads.
pub type RulesError = Box<dyn std::error::Error + Send + Sync>;

/// How far a rule that was kept reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// It answers the same prompt in the next session too.
    NextSession,
    /// It is kept where the next session reads it, but that session will not
    /// use it until the operator has trusted the file it is kept in.
    UntilTrusted,
}

/// Keeps the standing answers the operator gives, so that the same prompt does
/// not come back in the next session.
pub trait Rules {
    /// Keeps one rule, and says how far it reaches. Called once, when the
    /// operator makes it.
    fn remember(&mut self, rule: &Rule) -> Result<Reach, RulesError>;
}

/// Rules that are kept nowhere: a recorded log being looked at rather than
/// continued, and a session with no config file to write to.
#[derive(Debug, Clone, Copy, Default)]
pub struct Forgotten;

impl Rules for Forgotten {
    /// Says nothing about the rule: a session with nowhere to keep one has
    /// no next session to tell the operator about.
    fn remember(&mut self, _rule: &Rule) -> Result<Reach, RulesError> {
        Ok(Reach::NextSession)
    }
}
