// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The question a session opens on where the repository's config sets what
//! only a trusted file may: whether to put that file in force.
//!
//! Asked before the backend is started rather than once a session runs,
//! because what the answer decides — the environment, arguments and settings
//! the backend starts with, the profile it runs under, the rules that answer
//! its prompts — is fixed the moment it starts. The shell knows no config:
//! what the file sets arrives as rows the caller wrote.

use ratatui::crossterm::event::KeyCode;

/// What a repository's config would put in force once it is trusted, as the
/// question shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    /// The file, as the operator is shown it.
    pub path: String,
    /// What trusting it puts in force, one row each: what it is, and what the
    /// file sets it to.
    pub grants: Vec<(String, String)>,
}

/// How the operator answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Put the file in force as it was read, and remember that until it
    /// changes.
    Trust,
    /// Open the session without what the file needs trust for, and ask again
    /// next time.
    NotNow,
}

impl Answer {
    /// The answers, in the order they are offered.
    pub const OFFERED: [Answer; 2] = [Answer::Trust, Answer::NotNow];

    /// What the answer is called where it is offered.
    pub fn label(self) -> &'static str {
        match self {
            Answer::Trust => "Yes, trust this config",
            Answer::NotNow => "No, open without it",
        }
    }
}

/// The question while it is up: what is asked, and which answer the cursor
/// is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asking {
    /// What is asked.
    pub question: Question,
    /// Which of [`Answer::OFFERED`] the cursor is on.
    pub at: usize,
    /// Whether the operator has typed at the question since they last chose
    /// an answer: an Enter then ends what they were typing, not the question.
    typing: bool,
}

/// What one key did to the question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reply {
    /// The key gave this answer.
    Answered(Answer),
    /// The key moved the cursor, or did nothing.
    Moved,
    /// The key was typed at the question, or was the Enter that ended what
    /// was typed, and answered nothing.
    Typed,
}

impl Asking {
    /// The question, with the cursor on "not now".
    ///
    /// Not on trusting: a session is often started and typed into straight
    /// away, and the answer an Enter gives unread must be the one that puts
    /// nothing in force.
    pub fn new(question: Question) -> Self {
        Self {
            question,
            at: not_now(),
            typing: false,
        }
    }

    /// The answer the cursor is on.
    pub fn selected(&self) -> Answer {
        Answer::OFFERED
            .get(self.at)
            .copied()
            .unwrap_or(Answer::NotNow)
    }

    /// One key: moves the cursor, gives an answer, or is taken as typing.
    ///
    /// Only the arrows and the digits the answers are numbered with choose:
    /// a letter is what an operator who has not read the question types as
    /// their first prompt, so `j` and `k` choose nothing here. A key that
    /// does not choose puts the cursor back on "not now" and holds back the
    /// next Enter, which ends what was typed rather than answering; an arrow
    /// or a digit chooses again. A digit moves the cursor and does not
    /// answer, as it does on a permission question: Enter is the one key that
    /// gives the trust. Esc is "not now", the answer that changes nothing.
    pub(crate) fn on_key(&mut self, code: KeyCode) -> Reply {
        let last = Answer::OFFERED.len().saturating_sub(1);
        match code {
            KeyCode::Up => self.choose(self.at.saturating_sub(1)),
            KeyCode::Down => self.choose(self.at.saturating_add(1).min(last)),
            KeyCode::Char(c) => match numbered(c).filter(|at| *at <= last) {
                Some(at) => self.choose(at),
                None => return self.typed(),
            },
            KeyCode::Enter if self.typing => {
                self.typing = false;
                return Reply::Typed;
            }
            KeyCode::Enter => return Reply::Answered(self.selected()),
            KeyCode::Esc => return Reply::Answered(Answer::NotNow),
            _ => return self.typed(),
        }
        Reply::Moved
    }

    fn choose(&mut self, at: usize) {
        self.at = at;
        self.typing = false;
    }

    fn typed(&mut self) -> Reply {
        self.at = not_now();
        self.typing = true;
        Reply::Typed
    }
}

/// Where "not now" is among [`Answer::OFFERED`].
fn not_now() -> usize {
    Answer::OFFERED
        .iter()
        .position(|answer| *answer == Answer::NotNow)
        .unwrap_or(0)
}

/// Which answer a digit names, counting from one.
fn numbered(c: char) -> Option<usize> {
    c.to_digit(10)
        .and_then(|number| usize::try_from(number).ok())
        .and_then(|number| number.checked_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asking() -> Asking {
        Asking::new(Question {
            path: "/r/.niobe/config.toml".to_owned(),
            grants: vec![("permissions".to_owned(), "allow Bash".to_owned())],
        })
    }

    #[test]
    fn enter_gives_the_answer_the_cursor_is_on() {
        let mut moved = asking();
        assert_eq!(moved.on_key(KeyCode::Up), Reply::Moved);
        assert_eq!(moved.on_key(KeyCode::Enter), Reply::Answered(Answer::Trust));
    }

    #[test]
    fn a_digit_moves_the_cursor_without_answering() {
        let mut asking = asking();

        assert_eq!(asking.on_key(KeyCode::Char('1')), Reply::Moved);
        assert_eq!(asking.selected(), Answer::Trust);
        assert_eq!(asking.on_key(KeyCode::Char('2')), Reply::Moved);
        assert_eq!(asking.selected(), Answer::NotNow);
        assert_eq!(asking.on_key(KeyCode::Char('1')), Reply::Moved);
        assert_eq!(
            asking.on_key(KeyCode::Enter),
            Reply::Answered(Answer::Trust)
        );
    }

    #[test]
    fn enter_alone_on_a_fresh_question_does_not_trust() {
        assert_eq!(asking().selected(), Answer::NotNow);
        assert_eq!(
            asking().on_key(KeyCode::Enter),
            Reply::Answered(Answer::NotNow)
        );
    }

    #[test]
    fn a_prompt_typed_at_the_question_and_sent_trusts_nothing() {
        for prompt in ["hello", "make it work", "k", "1 more thing"] {
            let mut asking = asking();
            for c in prompt.chars() {
                assert_ne!(
                    asking.on_key(KeyCode::Char(c)),
                    Reply::Answered(Answer::Trust),
                    "{prompt}"
                );
            }
            assert_eq!(asking.on_key(KeyCode::Enter), Reply::Typed, "{prompt}");
            assert_eq!(asking.selected(), Answer::NotNow, "{prompt}");
        }
    }

    #[test]
    fn after_typing_enter_answers_again_once_it_has_been_held_back() {
        let mut asking = asking();
        assert_eq!(asking.on_key(KeyCode::Char('x')), Reply::Typed);
        assert_eq!(asking.on_key(KeyCode::Enter), Reply::Typed);
        assert_eq!(
            asking.on_key(KeyCode::Enter),
            Reply::Answered(Answer::NotNow)
        );
    }

    #[test]
    fn an_arrow_after_typing_chooses_and_enter_then_answers() {
        let mut asking = asking();
        asking.on_key(KeyCode::Char('x'));
        assert_eq!(asking.on_key(KeyCode::Up), Reply::Moved);
        assert_eq!(
            asking.on_key(KeyCode::Enter),
            Reply::Answered(Answer::Trust)
        );
    }

    #[test]
    fn esc_opens_the_session_without_the_file() {
        assert_eq!(
            asking().on_key(KeyCode::Esc),
            Reply::Answered(Answer::NotNow)
        );
    }

    #[test]
    fn the_cursor_stays_on_the_answers() {
        let mut asking = asking();
        asking.on_key(KeyCode::Down);
        assert_eq!(asking.at, 1);
        asking.on_key(KeyCode::Up);
        asking.on_key(KeyCode::Up);
        assert_eq!(asking.at, 0);
    }
}
