// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Reading a JSON Lines event log: one serialized [`Event`] per line.

use niobe_core::event::Event;

/// A line of a log that is not an [`Event`].
#[derive(Debug)]
pub struct LogError {
    line: usize,
    source: serde_json::Error,
}

impl LogError {
    /// The 1-based line the bad record is on, counting blank lines, so it
    /// matches what an editor shows.
    pub fn line(&self) -> usize {
        self.line
    }
}

impl std::fmt::Display for LogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {} is not an event: {}", self.line, self.source)
    }
}

impl std::error::Error for LogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Parses a whole log. Blank lines are skipped; the first line that is not an
/// event fails the read, because a log with a hole in it folds into totals that
/// look right and are not.
pub fn read_log(text: &str) -> Result<Vec<Event>, LogError> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(i, line)| {
            serde_json::from_str(line).map_err(|source| LogError {
                line: i + 1,
                source,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_lines_are_skipped() {
        let log = "\n{\"type\":\"user_message\",\"text\":\"hi\"}\n\n";
        assert_eq!(
            read_log(log).expect("one event and some blank lines parse"),
            [Event::UserMessage {
                text: "hi".to_owned()
            }]
        );
    }

    #[test]
    fn a_bad_line_is_reported_by_the_line_an_editor_shows() {
        let log = "{\"type\":\"user_message\",\"text\":\"hi\"}\n\n{\"type\":\"nope\"}\n";
        let error = read_log(log).expect_err("an unknown type does not parse");
        assert_eq!(error.line(), 3);
        assert!(error.to_string().starts_with("line 3 is not an event"));
    }
}
