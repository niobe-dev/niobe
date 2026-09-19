// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The shell, rendered.
//!
//! The shell must render at 80x24 and at 200x60 and redraw smoothly on resize.
//! Both sizes are drawn into a [`TestBackend`] and compared against a committed
//! picture of the screen, so a layout change has to be looked at rather than
//! merely compiled. Regenerate with `UPDATE_SNAPSHOTS=1 cargo test`.
//!
//! A theme changes no character on screen, so its pictures are of the colours
//! instead: `neo-*` is a legend of every style the frame used and a map of
//! which cell got which. The text pictures stay in the default theme, which is
//! what keeps one committed picture of the layout rather than one per palette.
//!
//! How long a redraw takes is measured in `frame_budget.rs`, a binary of its
//! own, because the tests here run in parallel and would share its cores.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

mod common;

use std::path::PathBuf;

use common::{paint, running_session, screen};
use niobe_core::event::{Backend, Event, Mode, UsageWindow, UsageWindows};
use niobe_tui::app::{App, Repo, SelectedProfile};
use niobe_tui::theme::{CLASSIC, NEO};

/// The same session, stopped on a permission prompt it is waiting on.
///
/// The prompt is the `control_request` of
/// `niobe-bridge-claude/tests/fixtures/stream-json.jsonl`, translated: the
/// bridge's own tests assert that the recorded line becomes exactly this
/// event, so the modal is driven by a recording without this crate being able
/// to name a bridge.
pub fn session_waiting_on_a_prompt() -> App {
    let mut app = running_session();
    app.apply(&Event::PermissionRequest {
        id: "toolu_read".into(),
        tool: "Read".to_owned(),
        input: r#"{"file_path":"/repo/notes.txt"}"#.to_owned(),
        target: Some("/repo/notes.txt".to_owned()),
    });
    app
}

fn empty_session() -> App {
    App::new(Repo {
        name: "niobe".to_owned(),
        branch: Some("main".to_owned()),
    })
}

fn snapshot_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.txt"))
}

/// Compares a frame against its committed picture, or writes one when asked.
fn assert_snapshot(name: &str, screen: &str) {
    let path = snapshot_path(name);
    let screen = format!("{screen}\n");

    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, &screen).expect("the snapshot directory is committed");
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "no snapshot at {}: {e}\nrun `UPDATE_SNAPSHOTS=1 cargo test` to write one",
            path.display()
        )
    });

    assert_eq!(
        screen,
        expected,
        "the shell no longer draws what {} records; look at the frame and, if it is \
         right, run `UPDATE_SNAPSHOTS=1 cargo test`",
        path.display()
    );
}

#[test]
fn the_neo_theme_paints_the_shell_at_both_sizes() {
    assert_snapshot(
        "neo-80x24",
        &paint(&mut running_session().with_theme(NEO), 80, 24),
    );
    assert_snapshot(
        "neo-200x60",
        &paint(&mut running_session().with_theme(NEO), 200, 60),
    );
}

/// A theme is a palette and nothing else: every character stays where it was,
/// so the one committed picture of the layout covers every theme. The menu bar
/// is the exception, because it names the theme in force and the names are not
/// the same width.
#[test]
fn a_theme_moves_no_character_on_screen_but_the_name_it_shows() {
    for (width, height) in [(80, 24), (200, 60)] {
        let classic = screen(&mut running_session().with_theme(CLASSIC), width, height);
        let neo = screen(&mut running_session().with_theme(NEO), width, height);

        let mut classic = classic.lines();
        let mut neo = neo.lines();
        let (classic_menu, neo_menu) = (classic.next(), neo.next());
        assert!(
            classic_menu.is_some_and(|line| line.contains("theme:CLASSIC")),
            "{classic_menu:?}"
        );
        assert!(
            neo_menu.is_some_and(|line| line.contains("theme:NEO")),
            "{neo_menu:?}"
        );
        assert_eq!(
            classic.collect::<Vec<_>>(),
            neo.collect::<Vec<_>>(),
            "the theme moved something at {width}x{height}"
        );
    }
}

#[test]
fn the_shell_renders_at_eighty_by_twentyfour() {
    assert_snapshot("running-80x24", &screen(&mut running_session(), 80, 24));
}

#[test]
fn the_shell_renders_at_two_hundred_by_sixty() {
    assert_snapshot("running-200x60", &screen(&mut running_session(), 200, 60));
}

#[test]
fn an_empty_session_renders_at_both_sizes() {
    assert_snapshot("empty-80x24", &screen(&mut empty_session(), 80, 24));
    assert_snapshot("empty-120x30", &screen(&mut empty_session(), 120, 30));
}

#[test]
fn a_recorded_prompt_puts_the_modal_on_screen_at_both_sizes() {
    assert_snapshot(
        "asking-80x24",
        &screen(&mut session_waiting_on_a_prompt(), 80, 24),
    );
    assert_snapshot(
        "asking-200x60",
        &screen(&mut session_waiting_on_a_prompt(), 200, 60),
    );
}

#[test]
fn the_modal_shows_what_would_run_and_every_way_to_answer_it() {
    use niobe_tui::app::Answer;

    let mut app = session_waiting_on_a_prompt();
    let frame = screen(&mut app, 120, 30);

    assert!(frame.contains("Permission"), "{frame}");
    assert!(frame.contains("Read wants to run"), "{frame}");
    assert!(frame.contains("/repo/notes.txt"), "{frame}");
    // The whole of the arguments, not a summary of them.
    assert!(
        frame.contains(r#"{"file_path":"/repo/notes.txt"}"#),
        "{frame}"
    );
    assert!(frame.contains("y allow once"), "{frame}");
    assert!(frame.contains("n deny"), "{frame}");
    assert!(frame.contains("a always Read"), "{frame}");
    assert!(
        frame.contains("p always Read(/repo/notes.txt)"),
        "the standing answer does not say what it would allow:\n{frame}"
    );
    assert!(frame.contains("waiting on you"), "{frame}");

    // Answered, the modal goes and the session is drawn as it was.
    app.answer(Answer::Once);
    assert_eq!(
        screen(&mut app, 120, 30),
        screen(&mut running_session(), 120, 30),
        "the modal left something behind on the frame"
    );
}

#[test]
fn a_standing_answer_too_long_for_its_column_is_given_rows_of_its_own_and_read_whole() {
    let mut app = running_session();
    let command = "grep -rn description --include=Cargo.toml . | grep -v target";
    app.apply(&Event::PermissionRequest {
        id: "toolu_mcp".into(),
        tool: "mcp__claude_ai_Notion__notion-search".to_owned(),
        input: format!(r#"{{"command":"{command}"}}"#),
        target: Some(command.to_owned()),
    });
    let frame = screen(&mut app, 120, 30);
    // The frame's rows, with the modal's borders and padding taken off.
    let rows: Vec<&str> = frame
        .lines()
        .filter_map(|row| row.split('║').nth(2))
        .map(str::trim)
        .collect();

    assert!(
        rows.contains(&"a always mcp__claude_ai_Notion__notion-search"),
        "the tool's standing answer runs into the next one:\n{frame}"
    );
    let rule: String = rows
        .iter()
        .skip_while(|row| !row.starts_with("p always"))
        .take_while(|row| !row.is_empty() && !row.contains("waiting on you"))
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        rule,
        format!("p always mcp__claude_ai_Notion__notion-search({command})"),
        "the rule the operator would save is not shown whole:\n{frame}"
    );
}

#[test]
fn a_selected_profile_is_named_in_the_status_line_and_the_menu_bar() {
    let mut app = empty_session().with_profile(SelectedProfile {
        name: "work".to_owned(),
        backend: Backend::Claude,
        models: Vec::new(),
    });
    let label = "work · claude, not attached";

    let narrow = screen(&mut app, 80, 24);
    let status = narrow.lines().nth(21).unwrap_or_default();
    assert!(status.contains(label), "{narrow}");

    let wide = screen(&mut app, 120, 30);
    let menu = wide.lines().next().unwrap_or_default();
    assert!(menu.contains(label), "{wide}");
}

#[test]
fn the_status_line_says_how_tool_calls_are_gated_and_which_key_changes_it() {
    let mut app = running_session();
    let frame = screen(&mut app, 120, 30);
    assert!(frame.contains("▸▸ ask mode"), "{frame}");
    assert!(frame.contains("Shift+Tab cycles"), "{frame}");

    // A session nothing has reported a mode for claims none.
    let empty = screen(&mut empty_session(), 120, 30);
    assert!(!empty.contains("mode"), "{empty}");
}

#[test]
fn a_model_the_operator_moved_to_is_what_the_status_line_names() {
    let mut app = running_session();
    assert!(screen(&mut app, 120, 30).contains("claude · opus-5"));

    app.apply(&Event::ModelSelected {
        model: "haiku".to_owned(),
    });

    let frame = screen(&mut app, 120, 30);
    assert!(
        frame.contains("claude · haiku"),
        "the status line named the model the session moved off:\n{frame}"
    );
}

#[test]
fn the_model_list_shows_what_the_profile_offers_and_when_a_choice_lands() {
    let mut app = running_session().with_profile(SelectedProfile {
        name: "max".to_owned(),
        backend: Backend::Claude,
        models: vec!["opus-5".to_owned(), "sonnet-5".to_owned()],
    });
    app.on_key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::F(8),
        ratatui::crossterm::event::KeyModifiers::NONE,
    ));

    let frame = screen(&mut app, 120, 30);
    assert!(frame.contains("═ Model ═"), "{frame}");
    assert!(frame.contains("opus-5"), "{frame}");
    assert!(frame.contains("sonnet-5"), "{frame}");
    assert!(
        frame.contains("applied from the next turn"),
        "the list did not say when a choice takes effect:\n{frame}"
    );
}

#[test]
fn a_budget_is_on_the_status_line_with_what_has_been_spent_against_it() {
    let mut app = running_session().with_budget(0.50);

    let frame = screen(&mut app, 120, 30);

    // The fixture reports $0.04 of cost and one record without any, so the
    // figure beside the budget is what was actually reported and no more.
    assert!(frame.contains("budget $0.04/$0.50"), "{frame}");
    assert!(
        !screen(&mut running_session(), 120, 30).contains("budget"),
        "a session with no budget was given one"
    );
}

#[test]
fn a_cycled_mode_is_on_the_status_line_and_a_denied_key_is_not() {
    let mut app = running_session();
    app.on_key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::BackTab,
        ratatui::crossterm::event::KeyModifiers::SHIFT,
    ));

    let frame = screen(&mut app, 120, 30);
    assert!(frame.contains("▸▸ auto mode"), "{frame}");
    assert_eq!(
        app.take_produced(),
        [Event::ModeSelected { mode: Mode::Auto }]
    );
}

#[test]
fn the_plans_usage_windows_are_the_headline_and_f5_says_when_they_come_back() {
    let mut app = running_session();
    let frame = screen(&mut app, 120, 30);

    // The fixture reports 0.62 and 0.18, which is what the line says and all
    // it says: no window is rounded into another's place.
    assert!(frame.contains("62%/5h · 18%/7d"), "{frame}");
    assert!(!frame.contains("overage"), "{frame}");

    app.on_key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::F(5),
        ratatui::crossterm::event::KeyModifiers::NONE,
    ));
    let pressed = screen(&mut app, 120, 30);
    // The fixture's windows came back long ago, so the line says so rather
    // than counting down from nothing. What it says while one is still running
    // is asserted against a fixed clock in `app`'s own tests.
    assert!(
        pressed.contains("5h window 62%, already reset"),
        "{pressed}"
    );
    assert!(
        pressed.contains("7d window 18%, already reset"),
        "{pressed}"
    );
}

/// A metered profile reports no window, and a CLI version that does not emit
/// them reports none either. Both must leave the segment off the line: a
/// `0%/5h` would read as a plan nobody had touched.
#[test]
fn a_session_no_backend_reported_a_window_for_shows_no_window_at_all() {
    let mut app = empty_session();
    let frame = screen(&mut app, 120, 30);
    assert!(!frame.contains("/5h"), "{frame}");
    assert!(!frame.contains("/7d"), "{frame}");

    // And F5 says what the key does not do yet rather than reading out a
    // window nobody reported.
    app.on_key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::F(5),
        ratatui::crossterm::event::KeyModifiers::NONE,
    ));
    let pressed = screen(&mut app, 120, 30);
    assert!(pressed.contains("not implemented yet"), "{pressed}");
    assert!(!pressed.contains("window 0%"), "{pressed}");
}

#[test]
fn a_plan_spending_beyond_its_flat_fee_is_marked_on_the_status_line() {
    let mut app = running_session();
    app.apply(&Event::UsageWindows(UsageWindows {
        five_hour: Some(UsageWindow {
            utilization: 1.0,
            resets_at: Some(1_789_779_600),
        }),
        seven_day: Some(UsageWindow {
            utilization: 0.91,
            resets_at: Some(1_790_118_000),
        }),
        using_overage: true,
    }));

    let frame = screen(&mut app, 120, 30);
    assert!(frame.contains("100%/5h · 91%/7d"), "{frame}");
    assert!(
        frame.contains("overage"),
        "the plan is spending real money and the line did not say so:\n{frame}"
    );
}

#[test]
fn the_right_stack_collapses_below_a_hundred_columns() {
    let narrow = screen(&mut running_session(), 99, 30);
    let wide = screen(&mut running_session(), 100, 30);

    // Matched on the border the title sits in, so the menu bar's own `Cost`
    // and `Files` entries cannot stand in for a pane.
    for pane in ["═ Cost ", "═ Parallel ", "═ Changes "] {
        assert!(
            !narrow.contains(pane),
            "the {pane:?} pane is still drawn at 99 columns:\n{narrow}"
        );
        assert!(
            wide.contains(pane),
            "the {pane:?} pane is missing at 100 columns:\n{wide}"
        );
    }

    // The session pane is what the room goes to.
    assert!(narrow.contains("Session ─ example-app"));
    assert!(wide.contains("Session ─ example-app"));
}

#[test]
fn a_window_under_the_minimum_says_so_instead_of_drawing_a_broken_shell() {
    let small = screen(&mut running_session(), 79, 23);
    assert!(small.contains("needs 80×24"), "{small}");
    assert!(small.contains("this window is 79×23"), "{small}");
    assert!(!small.contains("Session ─"), "{small}");
}

#[test]
fn every_size_between_the_two_renders_without_a_panic_or_an_overrun() {
    let mut app = running_session();

    for width in (80..=200).step_by(3) {
        for height in (24..=60).step_by(3) {
            let frame = screen(&mut app, width, height);
            assert_eq!(
                frame.lines().count(),
                usize::from(height),
                "{width}x{height} drew the wrong number of rows"
            );
            for line in frame.lines() {
                assert!(
                    line.chars().count() <= usize::from(width),
                    "{width}x{height} overran the screen: {line:?}"
                );
            }
        }
    }
}

#[test]
fn scrolling_back_moves_the_transcript_and_says_that_it_did() {
    let mut app = running_session();
    let tail = screen(&mut app, 80, 24);
    assert!(!tail.contains("scrolled back"));

    app.scroll_to_head();
    let head = screen(&mut app, 80, 24);
    assert_ne!(head, tail, "paging to the top drew the same frame");
    assert!(
        head.contains("scrolled back"),
        "the pane did not say it was scrolled back:\n{head}"
    );

    app.scroll_to_tail();
    assert_eq!(screen(&mut app, 80, 24), tail, "paging back did not return");
}

#[test]
fn nothing_the_backends_did_not_report_appears_as_a_number() {
    // Two usage records, one of them without a cost: the pane must show the sum
    // as a floor rather than as the session's bill.
    let wide = screen(&mut running_session(), 120, 30);
    assert!(wide.contains("≥$0.04"), "{wide}");

    // An empty session has no cost at all, and says so.
    let empty = screen(&mut empty_session(), 120, 30);
    assert!(empty.contains("no backend"), "{empty}");
    assert!(empty.contains('—'), "{empty}");
}
