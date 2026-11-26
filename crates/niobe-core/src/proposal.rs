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

    /// Whether only the operator can answer the call: it asks them
    /// something rather than asking leave to act.
    ///
    /// No standing rule answers such a call, and none is offered for it. A
    /// rule's answer is leave to go ahead, and a question let through with
    /// leave alone reaches the agent as one nobody answered.
    pub fn asks_the_operator(&self) -> bool {
        match self.action {
            Action::Questions { .. } => true,
            Action::Replace { .. }
            | Action::Write { .. }
            | Action::Command { .. }
            | Action::Plan { .. }
            | Action::Skill { .. } => false,
        }
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
    /// Put `questions` to the operator, one after another, and hand the
    /// agent what they answered. Never empty.
    Questions {
        /// The questions, in the order they are asked.
        questions: Vec<Question>,
    },
    /// Use the skill called `name`: load the instructions, and whatever else
    /// the skill bundles, that the backend keeps under that name, and hand it
    /// `asked` to work on.
    Skill {
        /// The skill, as the backend names it.
        name: String,
        /// What the skill is handed to work on, as the agent wrote it; empty
        /// where it handed nothing.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        asked: String,
        /// What the skill is, in the backend's words, where it said.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        about: Option<String>,
    },
}

/// One question the agent puts to the operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// A word or two the question is filed under, where the agent gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// What is asked, which the answer is handed back under.
    pub text: String,
    /// The answers on offer, in the agent's order. Empty where the agent
    /// asks for words rather than a choice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<Choice>,
    /// Whether more than one of [`Self::options`] may be chosen.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multiple: bool,
}

/// One answer a question offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    /// What the answer is called, which is what is handed back when it is
    /// chosen.
    pub label: String,
    /// What choosing it means, where the agent said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// What choosing it would look like — a snippet of code, a layout — in
    /// markdown, where the agent drew one to compare the answers by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// The operator's answer to one question: the labels of the answers chosen,
/// or the words written instead of choosing one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answered {
    /// The question, as [`Question::text`] has it.
    pub question: String,
    /// The answer: one label, several joined by `, `, or the operator's own
    /// words.
    pub answer: String,
    /// What the operator added about the answer they chose, where they
    /// added anything: handed to the agent beside the answer, never in
    /// place of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// What the agent is told when the operator would rather talk the questions
/// over than answer them: the call is refused with these words, which ask
/// the agent to find out what the operator wants to clarify before asking
/// again, and name every question with whatever answer it was already given.
pub fn wants_to_talk(questions: &[Question], answered: &[Answered]) -> String {
    let mut said = String::from(
        "The operator wants to talk these questions over before answering them. \
         They may have more context, or questions of their own. Ask them what they \
         would like to clarify, take their reply into account, and ask the questions \
         again, reworded if that helps.\nThe questions asked:",
    );
    for question in questions {
        said.push_str(&format!("\n- \"{}\"", question.text));
        let given = answered
            .iter()
            .find(|answered| answered.question == question.text);
        match given {
            Some(given) => said.push_str(&format!("\n  Answer: {}", given.answer)),
            None => said.push_str("\n  (No answer given)"),
        }
        if let Some(notes) = given.and_then(|given| given.notes.as_deref()) {
            said.push_str(&format!("\n  Notes: {notes}"));
        }
    }
    said
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

    fn questions() -> Proposal {
        Proposal::new(
            Action::Questions {
                questions: vec![Question {
                    header: Some("Deploy".to_owned()),
                    text: "Cut a release?".to_owned(),
                    options: vec![
                        Choice {
                            label: "Yes".to_owned(),
                            description: None,
                            preview: None,
                        },
                        Choice {
                            label: "No".to_owned(),
                            description: Some("Push only.".to_owned()),
                            preview: Some("```sh\ngit push\n```".to_owned()),
                        },
                    ],
                    multiple: false,
                }],
            },
            ["questions"],
        )
    }

    #[test]
    fn only_questions_ask_the_operator_rather_than_for_leave() {
        assert!(questions().asks_the_operator());
        let command = Proposal::new(
            Action::Command {
                command: "ls".to_owned(),
            },
            ["command"],
        );
        let plan = Proposal::new(
            Action::Plan {
                plan: "1.".to_owned(),
            },
            ["plan"],
        );
        assert!(!command.asks_the_operator());
        assert!(!plan.asks_the_operator());
    }

    #[test]
    fn questions_are_stored_and_read_back_as_asked() {
        let proposal = questions();
        let stored = serde_json::to_string(&proposal).expect("questions serialize");
        assert!(stored.contains(r#""kind":"questions""#), "{stored}");
        assert!(!stored.contains("multiple"), "{stored}");
        let read: Proposal = serde_json::from_str(&stored).expect("and read back");
        assert_eq!(read, proposal);
    }

    #[test]
    fn an_answer_without_notes_is_stored_as_it_was_before_notes_were_kept() {
        let answered = Answered {
            question: "Cut a release?".to_owned(),
            answer: "No".to_owned(),
            notes: None,
        };
        let stored = serde_json::to_string(&answered).expect("an answer serializes");
        assert_eq!(stored, r#"{"question":"Cut a release?","answer":"No"}"#);
        let read: Answered = serde_json::from_str(&stored).expect("and reads back");
        assert_eq!(read, answered);
    }

    #[test]
    fn talking_it_over_names_every_question_and_what_was_already_answered() {
        let Action::Questions { questions } = questions().action().clone() else {
            unreachable!("questions() builds questions");
        };
        let mut asked = questions.clone();
        asked.push(Question {
            header: None,
            text: "Which platforms?".to_owned(),
            options: Vec::new(),
            multiple: true,
        });
        let answered = [Answered {
            question: "Cut a release?".to_owned(),
            answer: "No".to_owned(),
            notes: Some("after the tag".to_owned()),
        }];
        let said = wants_to_talk(&asked, &answered);
        assert!(said.starts_with("The operator wants to talk"), "{said}");
        assert!(
            said.ends_with(
                "- \"Cut a release?\"\n  Answer: No\n  Notes: after the tag\n\
                 - \"Which platforms?\"\n  (No answer given)"
            ),
            "{said}"
        );
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
