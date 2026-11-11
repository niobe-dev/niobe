// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Where the choices the operator makes in the shell are kept — the theme,
//! the model, the effort — so that the next session opens with them.
//!
//! The shell touches no filesystem, so it says what it needs, as it does for
//! a standing answer in [`crate::rules`], and the binary hands it something
//! that keeps them. A choice that is not kept still holds for this session.

/// Why a choice could not be kept. Shown to the operator as it reads.
pub type PreferencesError = Box<dyn std::error::Error + Send + Sync>;

/// One choice the operator made, to be kept for the next session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preference {
    /// The palette the shell draws in, by its name.
    Theme(String),
    /// The model the session answers with, as the operator named it; `None`
    /// where the backend refused the one chosen and the choice is not to be
    /// kept.
    Model(Option<String>),
    /// How hard the model works on a turn, as the backend's `/effort` takes
    /// it.
    Effort(String),
}

impl Preference {
    /// What was chosen, in the words the transcript says it in.
    pub fn described(&self) -> String {
        match self {
            Preference::Theme(theme) => format!("theme {theme}"),
            Preference::Model(Some(model)) => format!("model {model}"),
            Preference::Model(None) => "no model".to_owned(),
            Preference::Effort(effort) => format!("effort {effort}"),
        }
    }
}

/// Keeps the operator's choices for the next session.
pub trait Preferences {
    /// Keeps one choice. Called once, when the operator makes it.
    fn keep(&mut self, preference: &Preference) -> Result<(), PreferencesError>;
}

/// Choices kept nowhere: a recorded log being looked at rather than
/// continued, and a session with no config directory to keep them in.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unkept;

impl Preferences for Unkept {
    fn keep(&mut self, _preference: &Preference) -> Result<(), PreferencesError> {
        Ok(())
    }
}
