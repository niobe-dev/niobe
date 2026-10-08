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

use std::time::Instant;

use ratatui::crossterm::event::KeyCode;

use crate::app::ASK_QUIET;

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
    /// When the cursor was last put on trusting, which an Enter gives only
    /// once [`ASK_QUIET`] has passed since.
    trust_chosen: Option<Instant>,
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
    /// The key was an Enter that came too soon after trusting was chosen to
    /// have been meant as the answer, and answered nothing.
    Hasty,
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
            trust_chosen: None,
        }
    }

    /// The answer the cursor is on.
    pub fn selected(&self) -> Answer {
        Answer::OFFERED
            .get(self.at)
            .copied()
            .unwrap_or(Answer::NotNow)
    }

    /// One key, pressed `at`: moves the cursor, gives an answer, or is taken
    /// as typing.
    ///
    /// Only the arrows and the digits the answers are numbered with choose:
    /// a letter is what an operator who has not read the question types as
    /// their first prompt, so `j` and `k` choose nothing here. A key that
    /// does not choose puts the cursor back on "not now" and holds back the
    /// next Enter, which ends what was typed rather than answering; an arrow
    /// or a digit chooses again. A digit moves the cursor and does not
    /// answer, as it does on a permission question: Enter is the one key that
    /// gives the trust. Esc is "not now", the answer that changes nothing.
    ///
    /// Enter gives the trust only once the cursor has rested on it for
    /// [`ASK_QUIET`]. A first prompt that is the digit `1` chooses trusting
    /// as a deliberate `1` does, and the two differ only in what follows: the
    /// Enter that sends a prompt comes as fast as the operator types, while
    /// one who chose an answer looks at the cursor before confirming it. An
    /// Enter that comes sooner is taken as the end of a typed prompt: it
    /// answers nothing and puts the cursor back on "not now", so trusting is
    /// chosen again, and then confirmed after a pause, or not at all.
    pub(crate) fn on_key(&mut self, code: KeyCode, at: Instant) -> Reply {
        let last = Answer::OFFERED.len().saturating_sub(1);
        match code {
            KeyCode::Up => self.choose(self.at.saturating_sub(1), at),
            KeyCode::Down => self.choose(self.at.saturating_add(1).min(last), at),
            KeyCode::Char(c) => match numbered(c).filter(|chosen| *chosen <= last) {
                Some(chosen) => self.choose(chosen, at),
                None => return self.typed(),
            },
            KeyCode::Enter if self.typing => {
                self.typing = false;
                return Reply::Typed;
            }
            KeyCode::Enter if self.hasty(at) => {
                self.at = not_now();
                self.trust_chosen = None;
                return Reply::Hasty;
            }
            KeyCode::Enter => return Reply::Answered(self.selected()),
            KeyCode::Esc => return Reply::Answered(Answer::NotNow),
            _ => return self.typed(),
        }
        Reply::Moved
    }

    fn choose(&mut self, chosen: usize, at: Instant) {
        self.at = chosen;
        self.typing = false;
        self.trust_chosen = (self.selected() == Answer::Trust).then_some(at);
    }

    fn typed(&mut self) -> Reply {
        self.at = not_now();
        self.typing = true;
        self.trust_chosen = None;
        Reply::Typed
    }

    /// Whether an Enter `at` would give the trust before the cursor has
    /// rested on it for [`ASK_QUIET`].
    fn hasty(&self, at: Instant) -> bool {
        self.selected() == Answer::Trust
            && self
                .trust_chosen
                .is_none_or(|chosen| at < chosen + ASK_QUIET)
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
    use std::time::Duration;

    fn asking() -> Asking {
        Asking::new(Question {
            path: "/r/.niobe/config.toml".to_owned(),
            grants: vec![("permissions".to_owned(), "allow Bash".to_owned())],
        })
    }

    /// Keys pressed one after another at the pace of someone typing.
    struct Keys {
        at: Instant,
    }

    impl Keys {
        fn new() -> Self {
            Self { at: Instant::now() }
        }

        /// `code`, a typist's beat after the last key.
        fn typed(&mut self, asking: &mut Asking, code: KeyCode) -> Reply {
            self.at += Duration::from_millis(150);
            asking.on_key(code, self.at)
        }

        /// `code`, after a look at the question longer than its quiet.
        fn after_a_pause(&mut self, asking: &mut Asking, code: KeyCode) -> Reply {
            self.at += ASK_QUIET;
            asking.on_key(code, self.at)
        }
    }

    #[test]
    fn enter_gives_the_answer_the_cursor_is_on() {
        let (mut moved, mut keys) = (asking(), Keys::new());
        assert_eq!(keys.typed(&mut moved, KeyCode::Up), Reply::Moved);
        assert_eq!(
            keys.after_a_pause(&mut moved, KeyCode::Enter),
            Reply::Answered(Answer::Trust)
        );
    }

    #[test]
    fn a_digit_moves_the_cursor_without_answering() {
        let (mut asking, mut keys) = (asking(), Keys::new());

        assert_eq!(keys.typed(&mut asking, KeyCode::Char('1')), Reply::Moved);
        assert_eq!(asking.selected(), Answer::Trust);
        assert_eq!(keys.typed(&mut asking, KeyCode::Char('2')), Reply::Moved);
        assert_eq!(asking.selected(), Answer::NotNow);
        assert_eq!(keys.typed(&mut asking, KeyCode::Char('1')), Reply::Moved);
        assert_eq!(
            keys.after_a_pause(&mut asking, KeyCode::Enter),
            Reply::Answered(Answer::Trust)
        );
    }

    #[test]
    fn enter_alone_on_a_fresh_question_does_not_trust() {
        assert_eq!(asking().selected(), Answer::NotNow);
        assert_eq!(
            asking().on_key(KeyCode::Enter, Instant::now()),
            Reply::Answered(Answer::NotNow)
        );
    }

    #[test]
    fn a_prompt_typed_at_the_question_and_sent_trusts_nothing() {
        for prompt in [
            "hello",
            "make it work",
            "k",
            "1 more thing",
            "1",
            "21",
            "2 1",
        ] {
            let (mut asking, mut keys) = (asking(), Keys::new());
            for c in prompt.chars() {
                assert_ne!(
                    keys.typed(&mut asking, KeyCode::Char(c)),
                    Reply::Answered(Answer::Trust),
                    "{prompt}"
                );
            }
            assert!(
                matches!(
                    keys.typed(&mut asking, KeyCode::Enter),
                    Reply::Typed | Reply::Hasty
                ),
                "{prompt}"
            );
            assert_eq!(asking.selected(), Answer::NotNow, "{prompt}");
        }
    }

    #[test]
    fn an_enter_straight_after_choosing_trust_answers_nothing_and_puts_the_cursor_back() {
        for choosing in [KeyCode::Char('1'), KeyCode::Up] {
            let (mut asking, mut keys) = (asking(), Keys::new());
            keys.typed(&mut asking, choosing);

            assert_eq!(keys.typed(&mut asking, KeyCode::Enter), Reply::Hasty);
            assert_eq!(asking.selected(), Answer::NotNow);
            // The next Enter, however long after, gives the answer the cursor
            // is now on: trusting has to be chosen again.
            assert_eq!(
                keys.after_a_pause(&mut asking, KeyCode::Enter),
                Reply::Answered(Answer::NotNow)
            );
        }
    }

    #[test]
    fn moving_back_onto_trust_starts_its_wait_again() {
        let (mut asking, mut keys) = (asking(), Keys::new());
        keys.typed(&mut asking, KeyCode::Up);
        keys.after_a_pause(&mut asking, KeyCode::Down);
        keys.after_a_pause(&mut asking, KeyCode::Up);

        assert_eq!(keys.typed(&mut asking, KeyCode::Enter), Reply::Hasty);
    }

    #[test]
    fn an_enter_straight_after_choosing_not_now_answers_it() {
        let (mut asking, mut keys) = (asking(), Keys::new());
        keys.typed(&mut asking, KeyCode::Char('2'));
        assert_eq!(
            keys.typed(&mut asking, KeyCode::Enter),
            Reply::Answered(Answer::NotNow)
        );
    }

    #[test]
    fn after_typing_enter_answers_again_once_it_has_been_held_back() {
        let (mut asking, mut keys) = (asking(), Keys::new());
        assert_eq!(keys.typed(&mut asking, KeyCode::Char('x')), Reply::Typed);
        assert_eq!(keys.typed(&mut asking, KeyCode::Enter), Reply::Typed);
        assert_eq!(
            keys.typed(&mut asking, KeyCode::Enter),
            Reply::Answered(Answer::NotNow)
        );
    }

    #[test]
    fn an_arrow_after_typing_chooses_and_enter_then_answers() {
        let (mut asking, mut keys) = (asking(), Keys::new());
        keys.typed(&mut asking, KeyCode::Char('x'));
        assert_eq!(keys.typed(&mut asking, KeyCode::Up), Reply::Moved);
        assert_eq!(
            keys.after_a_pause(&mut asking, KeyCode::Enter),
            Reply::Answered(Answer::Trust)
        );
    }

    #[test]
    fn esc_opens_the_session_without_the_file() {
        assert_eq!(
            asking().on_key(KeyCode::Esc, Instant::now()),
            Reply::Answered(Answer::NotNow)
        );
    }

    #[test]
    fn the_cursor_stays_on_the_answers() {
        let (mut asking, mut keys) = (asking(), Keys::new());
        keys.typed(&mut asking, KeyCode::Down);
        assert_eq!(asking.at, 1);
        keys.typed(&mut asking, KeyCode::Up);
        keys.typed(&mut asking, KeyCode::Up);
        assert_eq!(asking.at, 0);
    }
}
