// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What `niobe cost` prints: each session the store holds, with the tokens it
//! spent and the money its backend reported, as a table, as JSON or as CSV,
//! or the tokens of each day's sessions summed by profile, with `--by-day`. Every
//! figure is folded from the stored events, so a session recorded before
//! the day table existed is counted in it.
//!
//! The JSON is the shape `schema/cost.schema.json` publishes, so that
//! something else can depend on it. Nothing here prices anything: the money
//! is what the backend reported, and the records it does not cover are
//! counted rather than valued, so an export never carries an estimate a
//! reader could take for a bill. A session billed on a plan has no money at
//! all, as the shell shows none: a plan is limited by its windows.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use niobe_core::session::{SessionState, Totals};
use niobe_tui::{Billed, OLDER_RECORDING, cost_of};
use serde_json::{Map, Value, json};

use crate::overview::Folded;
use crate::sessions::civil_from_days;
use crate::summary::grouped;

/// The version of the JSON's shape, which `schema/cost.schema.json` names. It
/// moves only when a field changes meaning or goes away; one added does not.
pub const SCHEMA_VERSION: u64 = 1;

/// How `niobe cost` prints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Format {
    /// A table to read.
    #[default]
    Table,
    /// One JSON document, in the published shape.
    Json,
    /// One row per session under a header row.
    Csv,
    /// A table of the sessions started on each day under each profile, with
    /// their tokens.
    Days,
}

/// The columns of the CSV, in order.
const CSV_HEADER: &str = "id,started_at,billing,input,output,cache_read,cache_write,\
                          cache_write_1h,cache_write_unsplit,reasoning,tokens,reported_usd,cost_complete,\
                          records,records_uncovered,older_recording";

/// What `sessions` print as, one line per element.
pub fn lines(repository: &str, sessions: &[Folded], format: Format) -> Vec<String> {
    match format {
        Format::Table => table(sessions),
        Format::Days => days(sessions),
        Format::Json => vec![document(repository, sessions).to_string()],
        Format::Csv => std::iter::once(CSV_HEADER.to_owned())
            .chain(sessions.iter().map(csv_row))
            .collect(),
    }
}

/// The word the export calls a session's billing by.
fn billing(billed: Billed) -> &'static str {
    match billed {
        Billed::Plan => "plan",
        Billed::Metered => "metered",
        Billed::Both => "both",
        Billed::Unknown => "unknown",
    }
}

/// The totals whose money is the session's cost, as the shell's is: all of
/// it billed by use, the metered part of a session billed both ways, and none
/// on a plan or where nothing said how it was billed, since the figure there
/// may be what the work would have cost on the API, which nobody paid.
fn money(session: &SessionState) -> Option<&Totals> {
    Billed::of(session).spent(session)
}

/// A moment as RFC 3339 in UTC, to the second.
pub(crate) fn rfc3339(at: SystemTime) -> String {
    let secs = at
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let second = secs.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second / 3_600,
        second / 60 % 60,
        second % 60
    )
}

/// The money of `totals` where a backend reported any, `None` where none did.
fn reported(totals: &Totals) -> Option<f64> {
    (!totals.reported_cost_by_model.is_empty()).then_some(totals.reported_cost_usd)
}

fn document(repository: &str, sessions: &[Folded]) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "repository": repository,
        "sessions": sessions.iter().map(session_json).collect::<Vec<_>>(),
    })
}

fn session_json(folded: &Folded) -> Value {
    let mut session = figures(&folded.state);
    session.insert("id".to_owned(), json!(folded.id.get()));
    session.insert("started_at".to_owned(), json!(rfc3339(folded.started_at)));
    Value::Object(session)
}

/// What `session` spent, in the fields every published shape names a
/// session's spend by: its billing, its tokens per kind and per model, and the
/// money its backend reported.
pub(crate) fn figures(session: &SessionState) -> Map<String, Value> {
    let t = session.totals();
    let cost = money(session).map(|m| {
        json!({
            "reported_usd": reported(m),
            "complete": m.cost_fully_reported(),
            "records": m.records,
            "records_uncovered": m.records_unsettled,
            "older_recording": m.settlement_unrecorded(),
        })
    });
    Map::from_iter([
        ("billing".to_owned(), json!(billing(Billed::of(session)))),
        (
            "tokens".to_owned(),
            json!({
                "input": t.input,
                "output": t.output,
                "cache_read": t.cache_read,
                "cache_write": t.cache_write,
                "cache_write_1h": t.cache_write_1h,
                "cache_write_unsplit": t.cache_write_unsplit,
                "reasoning": t.reasoning,
                "total": t.tokens(),
            }),
        ),
        ("models".to_owned(), json!(t.tokens_by_model)),
        ("cost".to_owned(), json!(cost)),
    ])
}

/// A field of the CSV: empty where there is no figure, never a zero.
fn field<T: ToString>(value: Option<T>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

fn csv_row(folded: &Folded) -> String {
    let session = &folded.state;
    let t = session.totals();
    let m = money(session);
    [
        folded.id.get().to_string(),
        rfc3339(folded.started_at),
        billing(Billed::of(session)).to_owned(),
        t.input.to_string(),
        t.output.to_string(),
        t.cache_read.to_string(),
        t.cache_write.to_string(),
        t.cache_write_1h.to_string(),
        t.cache_write_unsplit.to_string(),
        t.reasoning.to_string(),
        t.tokens().to_string(),
        field(m.and_then(reported)),
        field(m.map(Totals::cost_fully_reported)),
        field(m.map(|m| m.records)),
        field(m.map(|m| m.records_unsettled)),
        field(m.map(Totals::settlement_unrecorded)),
    ]
    .join(",")
}

fn table(sessions: &[Folded]) -> Vec<String> {
    if sessions.is_empty() {
        return vec!["No sessions recorded in this repository.".to_owned()];
    }
    let id_width = sessions
        .iter()
        .map(|s| s.id.to_string().len())
        .max()
        .unwrap_or(0)
        .max("ID".len());
    let mut lines = vec![format!(
        "{:>id_width$}  {:<20}  {:<7}  {:>12}  COST",
        "ID", "STARTED (UTC)", "BILLING", "TOKENS"
    )];
    lines.extend(sessions.iter().map(|s| {
        let session = &s.state;
        let cost = money(session).map_or_else(|| "—".to_owned(), labelled);
        format!(
            "{:>id_width$}  {:<20}  {:<7}  {:>12}  {cost}",
            s.id,
            rfc3339(s.started_at),
            billing(Billed::of(session)),
            grouped(session.totals().tokens()),
        )
    }));
    lines.push(String::new());
    lines.push(
        "The cost is the money each backend reported: `≥` where some of a session's records \
         carry none, `older recording` where they do not say what a reported cost covers, \
         and `—` on a plan, or where nothing said how a session was billed."
            .to_owned(),
    );
    lines
}

/// The sessions summed by the UTC day each started on and the profile it
/// ran under, oldest day first. Tokens only: a day's money would add a
/// plan's nothing to a metered bill, which is a figure nobody was charged.
/// A session that never said its profile is counted under `—`.
fn days(sessions: &[Folded]) -> Vec<String> {
    if sessions.is_empty() {
        return vec!["No sessions recorded in this repository.".to_owned()];
    }
    let mut by_day: BTreeMap<(String, String), (u64, u64)> = BTreeMap::new();
    for folded in sessions {
        let day = rfc3339(folded.started_at)
            .get(..10)
            .unwrap_or_default()
            .to_owned();
        let profile = folded
            .state
            .meta()
            .map_or_else(|| "—".to_owned(), |meta| meta.profile.clone());
        let (count, tokens) = by_day.entry((day, profile)).or_default();
        *count = count.saturating_add(1);
        *tokens = tokens.saturating_add(folded.state.totals().tokens());
    }
    let profile_width = by_day
        .keys()
        .map(|(_, profile)| profile.chars().count())
        .max()
        .unwrap_or(0)
        .max("PROFILE".len());
    let mut lines = vec![format!(
        "{:<10}  {:<profile_width$}  {:>8}  {:>12}",
        "DAY", "PROFILE", "SESSIONS", "TOKENS"
    )];
    lines.extend(by_day.iter().map(|((day, profile), (count, tokens))| {
        format!(
            "{day:<10}  {profile:<profile_width$}  {count:>8}  {:>12}",
            grouped(*tokens)
        )
    }));
    lines.push(String::new());
    lines.push(
        "Each day is the UTC day a session started on, and its tokens are every token \
         that day's sessions reported under that profile, wherever they were billed."
            .to_owned(),
    );
    lines
}

/// What `totals` cost as the shell labels it, with no estimate beside it,
/// and the mark the Usage pane gives a floor an older recording left.
fn labelled(totals: &Totals) -> String {
    let cost = cost_of(totals, None);
    if totals.settlement_unrecorded() {
        format!("{cost} · {OLDER_RECORDING}")
    } else {
        cost
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use niobe_core::event::{Backend, Event, SessionMeta, Usage};
    use std::time::Duration;

    fn session(id: u32, started: u64, input: u64) -> Folded {
        let mut state = SessionState::new();
        state.apply(&Event::Usage(Usage {
            input,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: Some(0),
            reasoning: 0,
            model: "opus-5".to_owned(),
            cost_usd: None,
            settles_model: Some(false),
            fast: false,
        }));
        Folded {
            id: id.to_string().parse().expect("a number is a session id"),
            started_at: UNIX_EPOCH + Duration::from_secs(started),
            state,
            decided_at: Vec::new(),
        }
    }

    #[test]
    fn the_days_table_sums_each_utc_days_sessions_by_profile_oldest_first() {
        // 1_789_732_800 is 2026-09-18T12:00:00Z; 43_199 s later is the day's
        // last second, and 43_200 s later the next day's first.
        let mut on_max = session(4, 1_789_732_800 + 60, 7);
        on_max.state.apply(&Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "max".to_owned(),
            model: "opus-5".to_owned(),
            backend_session: None,
        }));
        let sessions = [
            session(1, 1_789_732_800, 1_000),
            session(2, 1_789_732_800 + 43_199, 2_500),
            session(3, 1_789_732_800 + 43_200, 40),
            on_max,
        ];

        assert_eq!(
            lines("repo", &sessions, Format::Days),
            [
                "DAY         PROFILE  SESSIONS        TOKENS",
                "2026-09-18  max             1             7",
                "2026-09-18  —               2         3,500",
                "2026-09-19  —               1            40",
                "",
                "Each day is the UTC day a session started on, and its tokens are every token \
                 that day's sessions reported under that profile, wherever they were billed.",
            ]
        );
    }

    #[test]
    fn the_days_table_of_no_sessions_says_so() {
        assert_eq!(
            lines("repo", &[], Format::Days),
            ["No sessions recorded in this repository."]
        );
    }

    #[test]
    fn a_moment_is_written_in_rfc_3339_utc_to_the_second() {
        // Checked against Python's `datetime.fromtimestamp(s, UTC).isoformat()`.
        let at = |secs| UNIX_EPOCH + Duration::from_secs(secs);
        assert_eq!(rfc3339(at(0)), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(at(1_789_732_800)), "2026-09-18T12:00:00Z");
        assert_eq!(rfc3339(at(1_789_779_659)), "2026-09-19T01:00:59Z");
    }

    #[test]
    fn a_missing_figure_is_an_empty_field_not_a_zero() {
        assert_eq!(field::<u64>(None), "");
        assert_eq!(field(Some(0)), "0");
    }
}
