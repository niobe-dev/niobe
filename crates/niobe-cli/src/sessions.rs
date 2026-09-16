// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The session list `niobe sessions` prints.

use std::time::{SystemTime, UNIX_EPOCH};

use niobe_store::SessionSummary;

/// How much of a first prompt fits on a list line.
const PROMPT_CHARS: usize = 60;

/// The list, one session per line under a header, newest first.
pub fn table(sessions: &[SessionSummary]) -> Vec<String> {
    let id_width = sessions
        .iter()
        .map(|s| s.id.to_string().len())
        .max()
        .unwrap_or(0)
        .max("ID".len());

    let mut lines = vec![format!(
        "{:>id_width$}  {:<16}  {:>6}  FIRST PROMPT",
        "ID", "STARTED (UTC)", "EVENTS"
    )];
    lines.extend(sessions.iter().map(|s| {
        format!(
            "{:>id_width$}  {:<16}  {:>6}  {}",
            s.id,
            utc_minute(s.started_at),
            s.events,
            s.first_prompt
                .as_deref()
                .map_or_else(|| "—".to_owned(), prompt_line)
        )
    }));
    lines
}

/// A prompt on one line, cut to fit.
fn prompt_line(prompt: &str) -> String {
    let flat = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= PROMPT_CHARS {
        return flat;
    }
    let mut cut: String = flat.chars().take(PROMPT_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// `YYYY-MM-DD HH:MM` in UTC.
///
/// UTC because the local zone needs the system time-zone database, which is a
/// dependency for one column; the header says which zone it is.
fn utc_minute(at: SystemTime) -> String {
    let secs = at
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let minutes = secs.rem_euclid(86_400) / 60;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        minutes / 60,
        minutes % 60
    )
}

/// The proleptic Gregorian date of a day count from 1970-01-01, after Howard
/// Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn times_print_in_utc_to_the_minute() {
        // Checked against Python's `datetime.fromtimestamp(s, UTC)`.
        assert_eq!(utc_minute(at(0)), "1970-01-01 00:00");
        assert_eq!(utc_minute(at(951_782_400)), "2000-02-29 00:00");
        assert_eq!(utc_minute(at(1_789_000_000)), "2026-09-10 00:26");
        assert_eq!(utc_minute(at(4_102_444_799)), "2099-12-31 23:59");
    }

    #[test]
    fn a_long_or_multiline_prompt_is_cut_to_one_line() {
        assert_eq!(prompt_line("fix\n  the   build"), "fix the build");
        let long = "x".repeat(200);
        let cut = prompt_line(&long);
        assert_eq!(cut.chars().count(), PROMPT_CHARS);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn the_table_aligns_under_its_header() {
        let sessions = [
            SessionSummary {
                id: "12".parse().expect("a number is a session id"),
                started_at: at(1_789_000_000),
                last_at: Some(at(1_789_000_060)),
                events: 200,
                first_prompt: Some("add etag support".to_owned()),
            },
            SessionSummary {
                id: "3".parse().expect("a number is a session id"),
                started_at: at(0),
                last_at: None,
                events: 0,
                first_prompt: None,
            },
        ];

        assert_eq!(
            table(&sessions),
            [
                "ID  STARTED (UTC)     EVENTS  FIRST PROMPT",
                "12  2026-09-10 00:26     200  add etag support",
                " 3  1970-01-01 00:00       0  —",
            ]
        );
    }
}
