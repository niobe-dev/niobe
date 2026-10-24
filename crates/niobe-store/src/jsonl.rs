// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Reading a JSON Lines event log: one serialized [`Event`] per line, which
//! may say when it happened in an `at` beside the event's own fields — whole
//! milliseconds since the Unix epoch, the clock the store dates an event by.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
///
/// A byte-order mark at the start is passed over: an editor that saves UTF-8
/// with one has not changed a single event in the file.
pub fn read_log(text: &str) -> Result<Vec<Event>, LogError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
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

/// Parses a whole log as [`read_log`] does, each event beside the time its
/// line gives it, or `None` where the line gives none.
///
/// An `at` that is not a whole number of milliseconds fails the read as an
/// event that does not parse would: a time misread is a session dated to a
/// day it did not run on.
pub fn read_dated_log(text: &str) -> Result<Vec<(Event, Option<SystemTime>)>, LogError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(i, line)| {
            dated(line).map_err(|source| LogError {
                line: i + 1,
                source,
            })
        })
        .collect()
}

fn dated(line: &str) -> Result<(Event, Option<SystemTime>), serde_json::Error> {
    let mut value: serde_json::Value = serde_json::from_str(line)?;
    let at = match value.as_object_mut().and_then(|fields| fields.remove("at")) {
        None => None,
        Some(at) => {
            let millis: u64 = serde_json::from_value(at)?;
            Some(UNIX_EPOCH + Duration::from_millis(millis))
        }
    };
    Ok((serde_json::from_value(value)?, at))
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

    #[test]
    fn a_log_saved_with_a_byte_order_mark_reads_as_the_same_events() {
        let line = r#"{"type":"user_message","text":"hi"}"#;

        assert_eq!(
            read_log(&format!("\u{feff}{line}\n")).expect("the mark is passed over"),
            read_log(line).expect("a log")
        );
    }

    #[test]
    fn a_line_may_say_when_it_happened_and_one_that_does_not_is_undated() {
        let log = "{\"type\":\"user_message\",\"text\":\"hi\",\"at\":1789000000123}\n\
                   {\"type\":\"user_message\",\"text\":\"there\"}\n";
        let hi = Event::UserMessage {
            text: "hi".to_owned(),
        };
        let there = Event::UserMessage {
            text: "there".to_owned(),
        };
        assert_eq!(
            read_dated_log(log).expect("a dated and an undated line parse"),
            [
                (
                    hi,
                    Some(UNIX_EPOCH + Duration::from_millis(1_789_000_000_123))
                ),
                (there, None)
            ]
        );
        assert_eq!(
            read_log(log)
                .expect("the time is no part of the event")
                .len(),
            2
        );
    }

    #[test]
    fn a_time_that_is_not_whole_milliseconds_is_reported_by_its_line() {
        for at in ["\"2026-03-01\"", "-1", "1.5"] {
            let log = format!("\n{{\"type\":\"user_message\",\"text\":\"hi\",\"at\":{at}}}\n");
            let error = read_dated_log(&log).expect_err("not a time");
            assert_eq!(error.line(), 2, "{at}");
        }
    }
}
