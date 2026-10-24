// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What a tool call waiting on the operator would do, read out of its
//! arguments by the backend that knows the tool.
//!
//! A permission question has the call's arguments as the backend sends them:
//! one JSON string, where an edit's lines arrive as `\n` escapes and a shell
//! command is quoted inside the object it is already the target of. A
//! backend that knows what its tool's arguments mean says so here — the text
//! an edit replaces and what replaces it, a file's whole new content, a
//! command — so the question can show what would happen rather than what is
//! sent. Which of the arguments it read is said too, so that the rest are
//! still shown: what the operator approves is all of them.

use serde::{Deserialize, Serialize};

/// A call's arguments as what the call would do, and which arguments that
/// reading covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    action: Action,
    covers: Vec<String>,
}

impl Proposal {
    /// `action`, read from the arguments named in `covers`. Every argument
    /// not named there is one the reading leaves to be shown as it is.
    pub fn new(action: Action, covers: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            action,
            covers: covers.into_iter().map(Into::into).collect(),
        }
    }

    /// What the call would do.
    pub fn action(&self) -> &Action {
        &self.action
    }

    /// Whether the argument named `key` is part of what [`Self::action`]
    /// says, so need not be shown again beside it.
    pub fn covers(&self, key: &str) -> bool {
        self.covers.iter().any(|covered| covered == key)
    }
}

/// What a call would do, in the terms a question shows it in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Replace the text `old` with `new` in the file at `path`: where `every`,
    /// each place the file holds it, and otherwise the one place.
    Replace {
        /// The file, as the call names it.
        path: String,
        /// The text replaced.
        old: String,
        /// What replaces it.
        new: String,
        /// Whether every occurrence is replaced rather than one.
        every: bool,
    },
    /// Write `content` as the whole of the file at `path`.
    Write {
        /// The file, as the call names it.
        path: String,
        /// Everything the file holds afterwards.
        content: String,
        /// Whether a file was at `path` when the call was asked about: one
        /// that was loses what it held, which the call does not carry.
        /// `None` where nobody looked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        existed: Option<bool>,
    },
    /// Run `command` in a shell.
    Command {
        /// The command, exactly as it would run.
        command: String,
    },
    /// Stop planning and carry out `plan`: the agent was asked to plan before
    /// it acts, and asks whether the plan it wrote may go ahead.
    Plan {
        /// The plan, as the agent wrote it, in markdown.
        plan: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proposal_covers_the_arguments_it_was_read_from_and_no_others() {
        let proposal = Proposal::new(
            Action::Command {
                command: "ls".to_owned(),
            },
            ["command"],
        );
        assert!(proposal.covers("command"));
        assert!(!proposal.covers("description"));
    }

    #[test]
    fn a_write_nobody_looked_for_is_stored_without_saying_whether_it_existed() {
        let proposal = Proposal::new(
            Action::Write {
                path: "a.txt".to_owned(),
                content: "x".to_owned(),
                existed: None,
            },
            ["file_path", "content"],
        );
        let stored = serde_json::to_string(&proposal).expect("a proposal serializes");
        assert!(!stored.contains("existed"), "{stored}");
        let read: Proposal = serde_json::from_str(&stored).expect("and reads back");
        assert_eq!(read, proposal);
    }

    #[test]
    fn a_plan_is_stored_and_read_back_as_a_plan() {
        let proposal = Proposal::new(
            Action::Plan {
                plan: "1. Guard it.".to_owned(),
            },
            ["plan"],
        );
        let stored = serde_json::to_string(&proposal).expect("a proposal serializes");
        assert!(stored.contains(r#""kind":"plan""#), "{stored}");
        let read: Proposal = serde_json::from_str(&stored).expect("and reads back");
        assert_eq!(read, proposal);
    }
}
