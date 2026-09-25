// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A folded session, printed: what `replay` and `--resume` show when standard
//! output is not a terminal.
//!
//! Every figure is read off the same fold the shell's panes read, and the cost
//! carries the same label the Usage pane gives it.

use niobe_core::Billing;
use niobe_core::session::SessionState;
use niobe_tui::app::App;

/// The lines describing a folded session, without a header.
pub fn lines(app: &App) -> Vec<String> {
    let session = app.session();
    vec![
        format!("tokens      {}", tokens(session)),
        format!("cost        {}", cost(session, app.prices())),
        format!("billing     {}", billing(session)),
        format!("tool calls  {}", tool_calls(session)),
        format!(
            "messages    {} from you · {} from the agent",
            grouped(session.user_messages()),
            grouped(session.assistant_messages())
        ),
        format!("files       {}", files(session)),
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

/// The pane's label, and the reason a figure is an estimate, a floor or
/// missing.
///
/// On a plan the figure is named for what it is, as the pane names it.
fn cost(session: &SessionState, prices: Option<&dyn niobe_tui::Prices>) -> String {
    let t = session.totals();
    let label = match session.billing() {
        Some(Billing::Plan) if t.records > 0 => format!(
            "API-equivalent {}",
            niobe_tui::session_cost(session, prices)
        ),
        Some(Billing::Plan | Billing::Metered) | None => niobe_tui::session_cost(session, prices),
    };
    if t.records == 0 || t.cost_fully_reported() {
        return label;
    }
    format!(
        "{label} — {} of {} usage records no reported cost covers",
        grouped(t.records_unsettled),
        grouped(t.records)
    )
}

/// How the session is billed, and what that makes the cost line above it.
///
/// Printed where the pane has no room to: the pane shows no dollar figure for
/// a session nothing described, and this says why and what would.
fn billing(session: &SessionState) -> String {
    match session.billing() {
        Some(Billing::Plan) => {
            "plan — the cost is what the work would have cost on the API; no money moved \
             with it"
                .to_owned()
        }
        Some(Billing::Metered) => {
            "metered — every token is billed, so the cost is money spent".to_owned()
        }
        None => "not known — the backend did not say and the profile does not set `billing`, \
                 so the cost may be money spent or what a plan's work would have cost"
            .to_owned(),
    }
}

/// The files the session changed and by how much, with the same floor marks
/// the changes pane gives them: a figure no call stated is an em dash, and one
/// only some of them stated reads "at least".
fn files(session: &SessionState) -> String {
    let changed = session.files();
    if changed.is_empty() {
        return "—".to_owned();
    }

    let added: u64 = changed.iter().map(|file| file.added).sum();
    let removed: u64 = changed.iter().map(|file| file.removed).sum();
    let unstated = changed
        .iter()
        .filter(|file| !file.added_stated() || !file.removed_stated())
        .count();

    let mut line = format!(
        "{} changed — +{} −{}",
        grouped(changed.len() as u64),
        grouped(added),
        grouped(removed)
    );
    if unstated > 0 {
        line.push_str(&format!(
            " (a floor: {} of them changed by an amount the backend did not state)",
            grouped(unstated as u64)
        ));
    }
    line
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
            cost_basis: None,
            settles_model: false,
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
            "cost        ≥$0.25 — 1 of 2 usage records no reported cost covers"
        );
        assert_eq!(
            summary[0],
            "tokens      3,000 — 2,400 in · 600 out · 0 cache read · 0 cache write · 0 reasoning"
        );
    }

    /// Prices `opus-5` at a tenth of a cent per thousand tokens and nothing
    /// else, so the figure below is worked out by hand.
    #[derive(Debug)]
    struct OnlyOpus;

    impl niobe_tui::Prices for OnlyOpus {
        fn estimate(&self, usage: &Usage) -> Option<f64> {
            (usage.model == "opus-5").then(|| usage.tokens() as f64 / 1_000.0 * 0.001)
        }
    }

    #[test]
    fn a_floor_part_of_which_is_estimated_carries_the_panes_label() {
        let mut haiku = usage(None);
        if let Event::Usage(usage) = &mut haiku {
            usage.model = "haiku-4-5".to_owned();
        }
        // $0.25 reported, 1,500 opus tokens owed ($0.0015), haiku unpriced.
        let app = folded(&[usage(Some(0.25)), usage(None), haiku]).with_prices(Box::new(OnlyOpus));
        assert_eq!(
            lines(&app)[1],
            "cost        ≥~$0.25 — 2 of 3 usage records no reported cost covers"
        );
    }

    fn changed(path: &str, added: Option<u64>, removed: Option<u64>) -> Event {
        Event::FileChange {
            path: path.to_owned(),
            added,
            removed,
            hunks: Vec::new(),
        }
    }

    #[test]
    fn a_session_that_changed_nothing_says_so_with_a_dash() {
        assert_eq!(lines(&folded(&[]))[5], "files       —");
    }

    #[test]
    fn the_files_line_adds_up_what_the_backend_stated_and_marks_what_it_did_not() {
        let summary = lines(&folded(&[
            changed("src/fetch.rs", Some(38), Some(9)),
            changed("notes.md", Some(1), None),
        ]));
        assert_eq!(
            summary[5],
            "files       2 changed — +39 −9 (a floor: 1 of them changed by an amount the \
             backend did not state)"
        );
    }

    #[test]
    fn a_session_whose_every_change_was_stated_gives_the_bare_figures() {
        let summary = lines(&folded(&[changed("src/fetch.rs", Some(38), Some(9))]));
        assert_eq!(summary[5], "files       1 changed — +38 −9");
    }

    fn billed(billing: Billing) -> Event {
        Event::Billing { billing }
    }

    #[test]
    fn the_billing_mode_is_printed_with_the_cost_it_explains() {
        let plan = lines(&folded(&[billed(Billing::Plan), usage(Some(0.25))]));
        assert_eq!(plan[1], "cost        API-equivalent $0.25");
        assert!(plan[2].starts_with("billing     plan — "), "{}", plan[2]);

        let metered = lines(&folded(&[billed(Billing::Metered), usage(Some(0.25))]));
        assert_eq!(metered[1], "cost        $0.25");
        assert!(
            metered[2].starts_with("billing     metered — "),
            "{}",
            metered[2]
        );

        let unsaid = lines(&folded(&[usage(Some(0.25))]));
        assert!(
            unsaid[2].starts_with("billing     not known — "),
            "{}",
            unsaid[2]
        );
    }

    #[test]
    fn a_fully_reported_cost_is_the_bare_figure() {
        let summary = lines(&folded(&[usage(Some(0.25)), usage(Some(0.5))]));
        assert_eq!(summary[1], "cost        $0.75");
    }
}
