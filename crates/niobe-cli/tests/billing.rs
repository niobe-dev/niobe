// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What a recorded session's Usage pane shows, by how the session was billed.
//!
//! How a session is billed is worked out by the bridge from what the CLI said,
//! and drawn by the shell from the event the bridge produced. The binary is
//! the only crate that may name both, so a recording can be walked from the
//! CLI's own lines to the pane only here. The pane is compared with a picture
//! in `tests/snapshots/`, and only the pane: the rest of the shell has its own
//! pictures in the shell's crate. After an intentional change, regenerate with
//! `UPDATE_SNAPSHOTS=1 cargo test -p niobe-cli --test billing` and read the
//! diff before accepting it.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::path::PathBuf;

use niobe_bridge_claude::Translator;
use niobe_core::Billing;
use niobe_core::event::Usage;
use niobe_ledger::{Date, PriceTable};
use niobe_tui::app::{App, Repo};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// Two turns on a claude.ai login, recorded from the live CLI: `init` names no
/// API key and every `result` says Anthropic's own API served the model.
const MAX: &str = include_str!("../../niobe-bridge-claude/tests/fixtures/stream-json.jsonl");

/// The recording, translated as a live session under `profile` would be, and
/// folded into the shell.
fn shell_on(recording: &str, translator: Translator) -> App {
    let mut translator = translator;
    let events: Vec<_> = recording
        .lines()
        .filter(|line| !line.trim().is_empty())
        .flat_map(|line| translator.line(line))
        .collect();
    let mut app = App::new(Repo {
        name: "niobe".to_owned(),
        branch: Some("main".to_owned()),
        ..Repo::default()
    });
    app.extend(&events);
    app
}

/// The bundled price table, read on the day the recordings were made, as the
/// binary hands it to the shell: what the shell prices a token no reported
/// cost covers with.
#[derive(Debug)]
struct Bundled(PriceTable);

impl niobe_tui::Prices for Bundled {
    fn estimate(&self, usage: &Usage) -> Option<f64> {
        let day = Date::new(2026, 9, 19).expect("19 September 2026 is a date");
        self.0.cost(usage, day).usd()
    }
}

/// The Usage pane, as text: the rows of its box, cut out of a 120×30 frame.
///
/// The shell is drawn without a clock, so a window's reset time — which reads
/// against the moment it is drawn at — is left off rather than changing with
/// the day the test runs on.
fn usage_pane(app: &mut App) -> String {
    let mut terminal =
        Terminal::new(TestBackend::new(120, 30)).expect("a test backend cannot fail");
    terminal
        .draw(|frame| niobe_tui::ui::draw(frame, app))
        .expect("a test backend cannot fail");
    let buffer = terminal.backend().buffer();
    let rows: Vec<Vec<&str>> = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()))
                .collect()
        })
        .collect();

    // The pane's corners are the theme's, so its edges are found from its
    // title: the run of rule either side of it is the top border.
    let (top, at) = rows
        .iter()
        .enumerate()
        .find_map(|(y, row)| {
            (0..row.len().saturating_sub(5))
                .skip(2)
                .find(|&x| {
                    // A title sits in a rule; the menu's `Usage` does not.
                    row[x..x + 5].concat() == "Usage"
                        && row[x - 2] != " "
                        && row[x - 2] == row[x + 6]
                })
                .map(|x| (y, x))
        })
        .expect("the Usage pane is drawn");
    let title = &rows[top];
    let rule = title[at - 2];
    let left = (0..at - 1)
        .rev()
        .find(|&x| title[x] != rule)
        .expect("the border has a corner");
    let right = (at + 6..title.len())
        .find(|&x| title[x] != rule)
        .expect("the border has a corner");

    // The bottom border is the first row under the title with no padding
    // inside its left edge; a rule inside the pane is padded.
    let mut pane = String::new();
    for (y, row) in rows.iter().enumerate().skip(top) {
        pane.push_str(&row[left..=right].concat());
        pane.push('\n');
        if y > top && row[left + 1] != " " {
            break;
        }
    }
    pane
}

fn assert_snapshot(name: &str, pane: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().expect("the path has a directory"))
            .expect("the snapshot directory can be made");
        std::fs::write(&path, pane).expect("the snapshot can be written");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "no snapshot at {}: {error}\nrun `UPDATE_SNAPSHOTS=1 cargo test` to write one",
            path.display()
        )
    });
    assert_eq!(pane, expected, "the Usage pane moved from {name}");
}

#[test]
fn a_recorded_max_session_shows_windows_and_tokens_and_no_dollar_total() {
    let mut app = shell_on(MAX, Translator::new("max"));
    assert_eq!(app.session().billing(), Some(Billing::Plan));

    let pane = usage_pane(&mut app);
    assert!(pane.contains("5h "), "{pane}");
    assert!(pane.contains("7d "), "{pane}");
    // What the CLI priced the work at is on screen, and named for what it is.
    assert!(pane.contains("API-equivalent $"), "{pane}");
    assert!(!pane.contains("session "), "{pane}");
    assert_snapshot("usage-max-120x30", &pane);
}

/// Two turns on `claude-opus-5[1m]`, whose messages name the family and whose
/// `result`s bill the id with the window, $0.269486 in all.
const LONG_CONTEXT: &str =
    include_str!("../../niobe-bridge-claude/tests/fixtures/long-context.jsonl");

/// The CLI's cost covers every token of the session, so the pane shows that
/// figure, as a measurement of what the CLI reported rather than a floor with
/// an estimate for the same tokens added on top, against one model row.
#[test]
fn a_session_on_the_1m_window_costs_what_the_cli_reported() {
    let table = PriceTable::bundled().expect("the bundled price table reads");
    let mut app = shell_on(
        LONG_CONTEXT,
        Translator::new("company").billed_as(Billing::Metered),
    )
    .with_prices(Box::new(Bundled(table)));

    let pane = usage_pane(&mut app);
    assert!(pane.contains("session $0.27 "), "{pane}");
    assert!(
        !pane.contains('~'),
        "no part of the bill is estimated: {pane}"
    );
    assert_eq!(pane.matches("opus-5").count(), 1, "one model row: {pane}");
    assert_snapshot("usage-long-context-120x30", &pane);
}

/// The profile knows the contract; the stream only shows how the requests
/// went. The same recorded login, on a profile that says it is billed by use,
/// is drawn as money.
#[test]
fn the_same_login_on_a_profile_billed_by_use_leads_with_money() {
    let mut app = shell_on(MAX, Translator::new("company").billed_as(Billing::Metered));
    assert_eq!(app.session().billing(), Some(Billing::Metered));

    let pane = usage_pane(&mut app);
    let session = pane.find("session $").expect("the cost leads the pane");
    let model = pane.find("sonnet-5").expect("the model rows are drawn");
    assert!(session < model, "{pane}");
    assert!(!pane.contains("API-equivalent"), "{pane}");
}
