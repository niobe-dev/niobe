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
}

impl Asking {
    /// The question, with the cursor on the first answer.
    pub fn new(question: Question) -> Self {
        Self { question, at: 0 }
    }

    /// The answer the cursor is on.
    pub fn selected(&self) -> Answer {
        Answer::OFFERED
            .get(self.at)
            .copied()
            .unwrap_or(Answer::NotNow)
    }

    /// One key: moves the cursor, or gives an answer.
    ///
    /// A digit moves the cursor and does not answer, as it does on a
    /// permission question: Enter is the one key that gives the trust, so a
    /// stray digit cannot. Esc is "not now", the answer that changes nothing.
    pub(crate) fn on_key(&mut self, code: KeyCode) -> Option<Answer> {
        let last = Answer::OFFERED.len().saturating_sub(1);
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.at = self.at.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.at = self.at.saturating_add(1).min(last),
            KeyCode::Char(c) => {
                if let Some(number) = c.to_digit(10)
                    && let Some(at) = usize::try_from(number)
                        .ok()
                        .and_then(|number| number.checked_sub(1))
                    && at <= last
                {
                    self.at = at;
                }
            }
            KeyCode::Enter => return Some(self.selected()),
            KeyCode::Esc => return Some(Answer::NotNow),
            _ => {}
        }
        None
    }
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
    fn enter_gives_the_answer_the_cursor_is_on_and_it_starts_on_trusting() {
        assert_eq!(asking().on_key(KeyCode::Enter), Some(Answer::Trust));

        let mut moved = asking();
        assert_eq!(moved.on_key(KeyCode::Down), None);
        assert_eq!(moved.on_key(KeyCode::Enter), Some(Answer::NotNow));
    }

    #[test]
    fn a_digit_moves_the_cursor_without_answering() {
        let mut asking = asking();

        assert_eq!(asking.on_key(KeyCode::Char('2')), None);
        assert_eq!(asking.selected(), Answer::NotNow);
        assert_eq!(asking.on_key(KeyCode::Char('9')), None);
        assert_eq!(asking.on_key(KeyCode::Char('0')), None);
        assert_eq!(asking.selected(), Answer::NotNow);
        assert_eq!(asking.on_key(KeyCode::Char('1')), None);
        assert_eq!(asking.selected(), Answer::Trust);
    }

    #[test]
    fn esc_opens_the_session_without_the_file() {
        assert_eq!(asking().on_key(KeyCode::Esc), Some(Answer::NotNow));
    }

    #[test]
    fn the_cursor_stays_on_the_answers() {
        let mut asking = asking();
        asking.on_key(KeyCode::Up);
        assert_eq!(asking.at, 0);
        asking.on_key(KeyCode::Down);
        asking.on_key(KeyCode::Down);
        assert_eq!(asking.at, 1);
    }
}
