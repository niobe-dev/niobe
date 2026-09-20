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

use common::{paint, running_session, screen, session_with_a_markdown_reply};
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

/// A tenth of a cent per thousand tokens, so the figure the pane draws is one
/// the test worked out rather than one the price table did.
#[derive(Debug)]
struct ATenthOfACentPerThousand;

impl niobe_tui::Prices for ATenthOfACentPerThousand {
    fn estimate(&self, usage: &niobe_core::event::Usage) -> Option<f64> {
        Some(usage.tokens() as f64 / 1_000.0 * 0.001)
    }
}

/// The session pane's cost tile is what the shell is for, so the running
/// figure has to reach the screen and not merely the label function. The
/// recorded session reports tokens and no money, which without a price sheet
/// reads `unpriced`; with one it reads the estimate, marked as one.
#[test]
fn a_price_sheet_puts_a_running_figure_in_the_cost_pane() {
    let without = screen(&mut running_session(), 120, 40);
    assert!(
        without.contains("≥$0.04"),
        "with nothing to value the rest, the tile is a floor:\n{without}"
    );

    let mut priced = running_session().with_prices(Box::new(ATenthOfACentPerThousand));
    let totals = priced.session().totals().clone();
    let owed: u64 = totals.unsettled.values().map(|usage| usage.tokens()).sum();
    let expected = totals.reported_cost_usd + owed as f64 / 1_000.0 * 0.001;

    let frame = screen(&mut priced, 120, 40);
    // 27,220 tokens owed for, at a tenth of a cent per thousand, is $0.02722
    // on top of the $0.04 the recording reported: $0.06722, to the cent.
    assert_eq!(owed, 27_220);
    assert_eq!(
        format!("~${expected:.2}"),
        "~$0.07",
        "${:.5} reported plus {owed} tokens owed for",
        totals.reported_cost_usd
    );
    assert!(frame.contains("~$0.07"), "{frame}");
    assert!(
        !frame.contains("≥$"),
        "a valued turn is an estimate, not a floor:\n{frame}"
    );
}

/// A reply's markdown is drawn, not shown: no asterisks, backticks or pipes
/// reach the screen, and the picture in each theme is the committed one.
#[test]
fn a_reply_in_markdown_is_drawn_styled_in_both_themes() {
    let frame = screen(&mut session_with_a_markdown_reply(), 120, 40);
    for mark in ["**", "`", "| file", "## ", "```"] {
        assert!(
            !frame.contains(mark),
            "{mark:?} reached the screen:\n{frame}"
        );
    }
    assert!(
        frame.contains("• catalog/fetch.ts keeps the etag"),
        "{frame}"
    );
    assert_snapshot("markdown-120x40", &frame);
    assert_snapshot(
        "markdown-neo-120x40",
        &paint(
            &mut session_with_a_markdown_reply().with_theme(NEO),
            120,
            40,
        ),
    );
}

/// A theme is a palette and nothing else: every character stays where it was,
/// so the one committed picture of the layout covers every theme. Nothing on
/// screen names the palette in force — `9 Theme` in the F-key row is where one
/// is changed.
#[test]
fn a_theme_moves_no_character_on_screen() {
    for (width, height) in [(80, 24), (200, 60)] {
        assert_eq!(
            screen(&mut running_session().with_theme(CLASSIC), width, height),
            screen(&mut running_session().with_theme(NEO), width, height),
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
    // The first button has the focus, so Enter gives it.
    assert!(frame.contains("►Yes, once◄"), "{frame}");
    assert!(frame.contains(" Always Read "), "{frame}");
    assert!(frame.contains(" Pin this "), "{frame}");
    assert!(frame.contains(" No "), "{frame}");
    assert!(
        frame.contains("Pin this saves Read(/repo/notes.txt)"),
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
fn a_standing_answer_too_long_for_one_row_is_wrapped_and_read_whole() {
    let mut app = running_session();
    let command = "grep -rn description --include=Cargo.toml . | grep -v target";
    app.apply(&Event::PermissionRequest {
        id: "toolu_mcp".into(),
        tool: "mcp__claude_ai_Notion__notion-search".to_owned(),
        input: format!(r#"{{"command":"{command}"}}"#),
        target: Some(command.to_owned()),
    });
    let frame = screen(&mut app, 120, 30);
    // The frame's rows, with the dialog's borders and padding taken off.
    let rows: Vec<&str> = frame
        .lines()
        .filter_map(|row| row.split('║').nth(2))
        .map(str::trim)
        .collect();

    assert!(
        frame.contains(" Always Notion·search "),
        "the tool's button does not name it the way the timeline does:\n{frame}"
    );
    let rule: String = rows
        .iter()
        .skip_while(|row| !row.starts_with("Pin this saves"))
        .take_while(|row| !row.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        rule,
        format!("Pin this saves mcp__claude_ai_Notion__notion-search({command})"),
        "the rule the operator would save is not shown whole:\n{frame}"
    );
}

/// A prompt sent from this shell, with a call of its turn still running, a
/// minute and a quarter in by the clock the event loop hands the shell.
fn session_at_work() -> App {
    use std::time::{Duration, Instant};

    let mut app = App::new(Repo {
        name: "example-app".to_owned(),
        branch: Some("main".to_owned()),
    })
    .attached();
    for c in "fix the etag test".chars() {
        app.type_into_composer(ratatui_textarea::Input {
            key: ratatui_textarea::Key::Char(c),
            ..Default::default()
        });
    }
    app.submit();
    let t0 = Instant::now();
    app.tick(t0, None);
    app.apply(&Event::ToolCallStart {
        id: "t1".into(),
        name: "Bash".to_owned(),
        input: r#"{"command":"npm test -- fetch"}"#.to_owned(),
        summary: Some("npm test -- fetch".to_owned()),
    });
    app.tick(t0 + Duration::from_secs(75), None);
    app
}

#[test]
fn a_turn_at_work_says_so_under_the_transcript_until_it_ends() {
    let mut app = session_at_work();
    let frame = screen(&mut app, 80, 24);
    assert!(
        frame.contains("running Bash  npm test -- fetch · 1m 15s"),
        "{frame}"
    );
    assert_snapshot("working-80x24", &frame);

    app.apply(&Event::TurnEnded);
    let frame = screen(&mut app, 80, 24);
    assert!(!frame.contains("running Bash"), "{frame}");
}

#[test]
fn a_selected_profile_is_named_in_the_menu_row_with_nothing_yet_listening() {
    let mut app = empty_session().with_profile(SelectedProfile {
        name: "work".to_owned(),
        backend: Backend::Claude,
        models: Vec::new(),
    });

    // At the narrowest the shell draws in there is room for what it runs
    // under and not for the rest, so that is what is kept.
    let narrow = screen(&mut app, 80, 24);
    let menu = narrow.lines().next().unwrap_or_default();
    assert!(menu.contains("claude · work"), "{narrow}");

    let wide = screen(&mut app, 120, 30);
    let menu = wide.lines().next().unwrap_or_default();
    assert!(menu.contains("claude · work"), "{wide}");
    assert!(
        menu.contains("○ not attached"),
        "a prompt typed here goes nowhere and the row did not say so:\n{wide}"
    );
}

#[test]
fn a_model_the_operator_moved_to_is_what_the_menu_row_names() {
    let mut app = running_session();
    let menu = |app: &mut App| {
        screen(app, 120, 30)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()
    };
    assert!(menu(&mut app).contains("opus-5"), "{}", menu(&mut app));

    app.apply(&Event::ModelSelected {
        model: "haiku".to_owned(),
    });

    let row = menu(&mut app);
    assert!(
        row.contains("haiku") && !row.contains("opus-5"),
        "the menu row named the model the session moved off:\n{row}"
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
fn a_budget_is_in_the_usage_pane_with_what_has_been_spent_against_it() {
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
fn a_cycled_mode_is_produced_for_the_backend_and_a_denied_key_is_not() {
    let mut app = running_session();
    app.on_key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::BackTab,
        ratatui::crossterm::event::KeyModifiers::SHIFT,
    ));

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

    // Matched on the border the title sits in, so the menu bar's own `Usage`
    // and `Files` entries cannot stand in for a pane.
    for pane in ["═ Usage ", "═ Parallel ", "═ Changes "] {
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

/// The rows a pane's top border is on, in the order they appear.
fn pane_tops(frame: &str, from: usize) -> Vec<usize> {
    frame
        .lines()
        .enumerate()
        .filter(|(_, row)| row.chars().skip(from).any(|c| c == '╔'))
        .map(|(at, _)| at)
        .collect()
}

#[test]
fn the_body_is_cut_in_the_proportions_the_layout_is_drawn_to() {
    let frame = screen(&mut running_session(), 200, 60);
    let first = frame.lines().nth(1).unwrap_or_default();

    // The session pane, a column of desktop, then the right-hand stack.
    let gap = first.chars().position(|c| c == '╗').unwrap_or(0) + 1;
    let session = gap;
    let right = first.chars().count() - gap - 1;
    let split = session as f64 / right as f64;
    assert!(
        (1.85..=1.95).contains(&split),
        "the body is cut {split:.2} : 1, not 1.9 : 1 ({session} and {right} columns)"
    );

    // Usage takes what its figures need; Changes and Activity share the rest,
    // 1.3 : 1 in favour of the files.
    let tops = pane_tops(&frame, gap);
    let [usage, changes, activity] = tops[..] else {
        panic!("the right-hand stack is not three panes: {tops:?}");
    };
    let bottom = frame.lines().count() - 1;
    let (changes_rows, activity_rows) = (activity - changes, bottom - activity);
    let split = changes_rows as f64 / activity_rows as f64;
    assert!(
        (1.25..=1.35).contains(&split),
        "Changes to Activity is {split:.2} : 1, not 1.3 : 1 \
         ({changes_rows} and {activity_rows} rows)"
    );
    assert!(
        changes - usage < changes_rows,
        "Usage took more rows than its figures need: {} of them",
        changes - usage
    );
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
    assert!(empty.contains("No backend attached"), "{empty}");
    assert!(empty.contains('—'), "{empty}");
}

/// The column of desktop between the session pane and the right-hand stack,
/// over the rows the panes occupy. Everything else on screen is a pane, and
/// the panes are opaque.
fn gutter(frame: &str) -> String {
    let at = frame
        .lines()
        .nth(1)
        .and_then(|border| border.chars().position(|c| c == '╗'))
        .map(|at| at + 1)
        .expect("the session pane's top-right corner is on the body's first row");
    // The menu bar above the body, and the F-key bar below it.
    let body = frame.lines().count().saturating_sub(2);
    frame
        .lines()
        .skip(1)
        .take(body)
        .filter_map(|row| row.chars().nth(at))
        .collect()
}

/// Nothing fills the gutter until a turn is running, and what fills it then
/// moves. A shell that looked the same whether or not it was working is the
/// complaint this answers.
#[test]
fn the_desktop_moves_between_the_panes_while_a_turn_runs() {
    use std::time::Duration;

    let idle = gutter(&screen(&mut running_session(), 120, 30));
    assert!(
        idle.chars().all(char::is_whitespace),
        "an idle session animated the desktop: {idle:?}"
    );

    let mut app = session_at_work();
    let first = gutter(&screen(&mut app, 120, 30));
    assert!(
        !first.chars().all(char::is_whitespace),
        "a running turn left the desktop blank"
    );

    // Two tenths of a second later the strip has moved on.
    app.tick(std::time::Instant::now() + Duration::from_secs(200), None);
    let later = gutter(&screen(&mut app, 120, 30));
    assert_ne!(first, later, "the desktop drew the same frame twice");

    // And it stops with the turn, rather than running on over a finished one.
    app.apply(&Event::TurnEnded);
    let stopped = gutter(&screen(&mut app, 120, 30));
    assert!(
        stopped.chars().all(char::is_whitespace),
        "the desktop kept moving after the turn ended: {stopped:?}"
    );
}

/// The columns a row is made of: a pane's border is `║`, or the scrollbar's
/// thumb where one is drawn over it.
fn borders(row: &str) -> Vec<usize> {
    row.chars()
        .enumerate()
        .filter(|(_, c)| *c == '║' || *c == '█')
        .map(|(at, _)| at)
        .collect()
}

/// No pane puts its text against its border. A column either side of the
/// content and a blank row under the title is what the panes are drawn to,
/// and text touching a double border is what they are drawn that way to avoid.
#[test]
fn no_pane_draws_its_text_against_its_border() {
    for (width, height) in [(80, 24), (120, 30), (200, 60)] {
        let frame = screen(&mut running_session(), width, height);
        let rows: Vec<Vec<char>> = frame.lines().map(|row| row.chars().collect()).collect();

        for (at, row) in frame.lines().enumerate() {
            let cells = &rows[at];
            for pair in borders(row).chunks_exact(2) {
                let (left, right) = (pair[0], pair[1]);
                assert_eq!(
                    cells.get(left + 1).copied(),
                    Some(' '),
                    "row {at} at {width}x{height} touches the border on its left:\n{frame}"
                );
                assert_eq!(
                    cells.get(right - 1).copied(),
                    Some(' '),
                    "row {at} at {width}x{height} touches the border on its right:\n{frame}"
                );
            }
        }

        // The row under the body's top border belongs to the Session pane and
        // to the Usage pane at once, and both keep it blank.
        let under = frame.lines().nth(2).unwrap_or_default();
        assert!(
            under.chars().all(|c| c == '║' || c == ' '),
            "the row under the pane titles at {width}x{height} is not blank: {under:?}"
        );
    }
}
