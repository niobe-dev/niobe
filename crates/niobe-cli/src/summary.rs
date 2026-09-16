// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A folded session, printed: what `replay` and `--resume` show when standard
//! output is not a terminal.
//!
//! Every figure is read off the same fold the shell's panes read, and the cost
//! carries the same label the cost pane gives it.

use niobe_core::session::SessionState;
use niobe_tui::app::App;

/// The lines describing a folded session, without a header.
pub fn lines(app: &App) -> Vec<String> {
    let session = app.session();
    vec![
        format!("tokens      {}", tokens(session)),
        format!("cost        {}", cost(session)),
        format!("tool calls  {}", tool_calls(session)),
        format!(
            "messages    {} from you · {} from the agent",
            grouped(session.user_messages()),
            grouped(session.assistant_messages())
        ),
        format!(
            "record      {} decisions · {} checkpoints · {} errors",
            session.decisions().len(),
            session.checkpoints().len(),
            grouped(session.errors())
        ),
        format!("transcript  {} entries", app.entries().len()),
    ]
}

fn tokens(session: &SessionState) -> String {
    let t = session.totals();
    if t.records == 0 {
        return "—".to_owned();
    }
    format!(
        "{} — {} in · {} out · {} cache read · {} cache write · {} reasoning",
        grouped(t.tokens()),
        grouped(t.input),
        grouped(t.output),
        grouped(t.cache_read),
        grouped(t.cache_write),
        grouped(t.reasoning)
    )
}

/// The pane's label, and the reason a figure is a floor or missing.
fn cost(session: &SessionState) -> String {
    let t = session.totals();
    let label = niobe_tui::session_cost(session);
    if t.records == 0 || t.cost_fully_reported() {
        return label;
    }
    format!(
        "{label} — {} of {} usage records reported no cost",
        grouped(t.records_without_cost),
        grouped(t.records)
    )
}

fn tool_calls(session: &SessionState) -> String {
    let tools = session.tools();
    let mut parts = vec![
        format!("{} finished", grouped(tools.finished)),
        format!("{} failed", grouped(tools.failed)),
        format!("{} denied", grouped(tools.denied)),
    ];
    if tools.unmatched_ends > 0 {
        parts.push(format!("{} without a start", grouped(tools.unmatched_ends)));
    }
    parts.join(" · ")
}

/// A count with thousands separated, so a token total reads at a glance.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use niobe_core::event::{Event, Usage};
    use niobe_tui::app::Repo;

    fn usage(cost_usd: Option<f64>) -> Event {
        Event::Usage(Usage {
            input: 1_200,
            output: 300,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 0,
            model: "opus-5".to_owned(),
            cost_usd,
        })
    }

    fn folded(events: &[Event]) -> App {
        let mut app = App::new(Repo::default());
        app.extend(events);
        app
    }

    #[test]
    fn counts_are_grouped_in_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(grouped(122_554), "122,554");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }

    #[test]
    fn a_session_without_usage_shows_dashes_not_zeros() {
        let summary = lines(&folded(&[]));
        assert_eq!(summary[0], "tokens      —");
        assert_eq!(summary[1], "cost        —");
    }

    #[test]
    fn a_partly_reported_cost_says_why_it_is_a_floor() {
        let summary = lines(&folded(&[usage(Some(0.25)), usage(None)]));
        assert_eq!(
            summary[1],
            "cost        ≥$0.25 — 1 of 2 usage records reported no cost"
        );
        assert_eq!(
            summary[0],
            "tokens      3,000 — 2,400 in · 600 out · 0 cache read · 0 cache write · 0 reasoning"
        );
    }

    #[test]
    fn a_fully_reported_cost_is_the_bare_figure() {
        let summary = lines(&folded(&[usage(Some(0.25)), usage(Some(0.5))]));
        assert_eq!(summary[1], "cost        $0.75");
    }
}
