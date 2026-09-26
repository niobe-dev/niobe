// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The shell, rendered.
//!
//! The shell must render at 80x24 and at 200x60 and redraw smoothly on resize.
//! Both sizes are drawn into a [`TestBackend`] and compared against a committed
//! picture of the screen, so a layout change has to be looked at rather than
//! merely compiled. Regenerate with `UPDATE_SNAPSHOTS=1 cargo test`.
//!
//! A theme changes no character on screen but the line the focused pane is
//! drawn in, so its pictures are of the colours instead: `<theme>-*` is a
//! legend of every style the frame used and a map of which cell got which, and
//! `<theme>-truecolor-*` is the same on a terminal that draws 24-bit colour.
//! The text pictures stay in the default theme, which is what keeps one
//! committed picture of the layout rather than one per palette.
//!
//! How long a redraw takes is measured in `frame_budget.rs`, a binary of its
//! own, because the tests here run in parallel and would share its cores.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

mod common;

use std::path::PathBuf;

use common::{
    at_work, metered_session, paint, running_session, screen, session_with_a_long_write,
    session_with_a_markdown_reply, session_with_finished_turns, session_with_test_records,
    session_with_test_runs, style_at, styles, unmetered_session,
};
use niobe_core::event::{Backend, Event, Mode, Usage, UsageWindow, UsageWindows};
use niobe_core::{FailedTests, TestCounts, TestRunRecord};
use niobe_tui::app::{App, Pane, Repo, Section, SelectedProfile};
use niobe_tui::theme::{CLASSIC, CYBER, Depth, MODERN, NEO, THEMES, Theme};
use niobe_tui::ui;
use ratatui::style::{Color, Style};

/// The same session, stopped on a permission prompt it is waiting on.
///
/// The prompt is the `control_request` of
/// `niobe-bridge-claude/tests/fixtures/stream-json.jsonl`, translated: the
/// bridge's own tests assert that the recorded line becomes exactly this
/// event, so the question is driven by a recording without this crate being able
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
        ..Repo::default()
    })
}

/// The line the default theme draws the pane with the keyboard in, glyph by
/// glyph, so the tests that find a pane by its border read as the frame does.
/// [`the_focus_glyphs_are_the_default_themes`] keeps them honest.
const FOCUS_TOP_LEFT: char = '┏';
const FOCUS_TOP_RIGHT: char = '┓';
const FOCUS_BOTTOM_LEFT: char = '┗';
const FOCUS_SIDE: char = '┃';
const FOCUS_EDGE: char = '━';

#[test]
fn the_focus_glyphs_are_the_default_themes() {
    let line = Theme::default().border_focus.to_border_set();
    for (glyph, drawn) in [
        (FOCUS_TOP_LEFT, line.top_left),
        (FOCUS_TOP_RIGHT, line.top_right),
        (FOCUS_BOTTOM_LEFT, line.bottom_left),
        (FOCUS_SIDE, line.vertical_left),
        (FOCUS_EDGE, line.horizontal_top),
    ] {
        assert_eq!(glyph.to_string(), drawn);
    }
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
fn every_theme_paints_the_shell_at_both_sizes() {
    for theme in THEMES {
        let name = theme.name.to_lowercase();
        for (width, height) in [(80, 24), (200, 60)] {
            assert_snapshot(
                &format!("{name}-{width}x{height}"),
                &paint(&mut running_session().with_theme(theme), width, height),
            );
        }
    }
}

/// On a terminal that says it draws 24-bit colour, a designed theme is drawn
/// in its design's own values: the committed picture is the colours the
/// design names, cell by cell.
#[test]
fn the_designed_themes_paint_their_own_colours_on_a_deep_terminal() {
    for theme in [CYBER, NEO, MODERN] {
        let name = theme.name.to_lowercase();
        assert_snapshot(
            &format!("{name}-truecolor-120x30"),
            &paint(
                &mut running_session()
                    .with_depth(Depth::TrueColour)
                    .with_theme(theme),
                120,
                30,
            ),
        );
    }
}

/// Every colour a frame is drawn in, foreground and background, cell by cell.
fn colours_drawn(app: &mut App) -> Vec<Color> {
    styles(app, 120, 30)
        .into_iter()
        .flat_map(|style| [style.fg, style.bg])
        .flatten()
        .collect()
}

/// A terminal that cannot draw 24-bit colour is never sent one: every theme,
/// the designed ones included, falls back to its sixteen-colour table, and a
/// frame of it is drawn in nothing else.
#[test]
fn a_terminal_without_truecolor_is_drawn_in_the_sixteen_names_only() {
    for theme in THEMES {
        let mut app = session_waiting_on_a_prompt()
            .with_depth(Depth::Sixteen)
            .with_theme(theme);
        for colour in colours_drawn(&mut app) {
            assert!(
                !matches!(colour, Color::Rgb(..) | Color::Indexed(_)),
                "{}: {colour:?} reached a sixteen-colour terminal",
                theme.name
            );
        }
    }
}

/// And a designed theme on a deep terminal draws nothing in a named colour,
/// which a user's own scheme would repaint under the design.
#[test]
fn a_designed_theme_on_a_deep_terminal_draws_every_cell_in_its_design() {
    for theme in [CYBER, NEO, MODERN] {
        let mut app = session_waiting_on_a_prompt()
            .with_depth(Depth::TrueColour)
            .with_theme(theme);
        for colour in colours_drawn(&mut app) {
            assert!(
                matches!(colour, Color::Rgb(..)),
                "{}: {colour:?} is drawn in a named colour",
                theme.name
            );
        }
    }
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

/// The Usage pane's cost figure is what the shell is for, so it has to reach
/// the screen and not merely the label function. The
/// recorded session reports tokens and no money, which without a price sheet
/// reads `unpriced`; with one it reads the estimate, marked as one.
#[test]
fn a_price_sheet_puts_a_running_figure_in_the_cost_pane() {
    let without = screen(&mut running_session(), 120, 40);
    assert!(
        without.contains("≥$0.04"),
        "with nothing to value the rest, the figure is a floor:\n{without}"
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
/// Each finished turn is ruled off with what it spent. The first has no
/// window share, because nothing was reported before it, and says nothing in
/// its place; at 80 columns the pane is too narrow for every figure, and the
/// ones that give way go whole.
#[test]
fn a_finished_turn_is_ruled_off_with_what_it_spent() {
    let frame = screen(&mut session_with_finished_turns(), 120, 30);
    assert!(
        frame.contains("── turn 1 13:36 · 6400 tok · 22s ──"),
        "{frame}"
    );
    assert!(
        frame.contains("── turn 2 13:37 · 22k tok · 1% of 5h · 38s ──"),
        "{frame}"
    );
    assert_snapshot("turns-120x30", &frame);

    let frame = screen(&mut session_with_finished_turns(), 80, 24);
    assert!(frame.contains("── turn 2"), "{frame}");
    assert!(!frame.contains("0% of 5h"), "{frame}");
    assert_snapshot("turns-80x24", &frame);
}

#[test]
fn a_diff_cut_at_twenty_rows_opens_in_place_and_cuts_again() {
    let mut app = session_with_a_long_write();

    let cut = screen(&mut app, 120, 40);
    assert!(cut.contains("… 10 more rows · Ctrl+T shows them"), "{cut}");
    assert!(!cut.contains("etag30"), "{cut}");
    assert_snapshot("diff-cut-120x40", &cut);

    app.open_diffs();
    let open = screen(&mut app, 120, 40);
    assert!(open.contains("etag30"), "{open}");
    assert!(
        open.contains("▴ the last 10 rows are shown · Ctrl+T hides them"),
        "{open}"
    );
    assert_snapshot("diff-open-120x40", &open);

    app.open_diffs();
    assert_eq!(screen(&mut app, 120, 40), cut);
}

/// The first transcript line that reads `marker`, and which row of the screen
/// it is on. Only the session pane's text is kept, so the scrollbar's thumb
/// moving down its track does not count as the line moving.
fn first_row_with(frame: &str, marker: &str) -> Option<(usize, String)> {
    frame
        .lines()
        .enumerate()
        .filter_map(|(at, row)| row.split('┃').nth(1).map(|text| (at, text)))
        .find(|(_, text)| text.contains(marker))
        .map(|(at, text)| (at, text.trim_end_matches('█').to_owned()))
}

/// The screen row the transcript's first line is drawn on at 120×40.
const TRANSCRIPT_TOP: usize = 3;

/// The long write, opened, with a reply under it long enough to scroll back
/// through without the diff coming into view.
fn long_write_under_a_long_reply() -> App {
    let mut app = session_with_a_long_write();
    let reply: String = (1..=60).map(|n| format!("- reply line {n:02}\n")).collect();
    app.apply(&Event::AssistantMessage { text: reply });
    app
}

#[test]
fn opening_or_cutting_the_diffs_while_scrolled_back_keeps_the_lines_being_read() {
    let mut app = long_write_under_a_long_reply();
    app.open_diffs();
    screen(&mut app, 120, 40);
    app.scroll_up(20);
    let before = screen(&mut app, 120, 40);
    assert!(
        !before.contains("export const"),
        "the diff is above the view:\n{before}"
    );
    let top = first_row_with(&before, "reply line").expect("the reply is in view");

    app.open_diffs();
    let cut = screen(&mut app, 120, 40);
    assert_eq!(
        first_row_with(&cut, "reply line"),
        Some(top.clone()),
        "{cut}"
    );

    app.open_diffs();
    let open = screen(&mut app, 120, 40);
    assert_eq!(first_row_with(&open, "reply line"), Some(top), "{open}");
    assert!(!app.follows_tail());
}

#[test]
fn folding_the_calls_while_scrolled_back_keeps_the_lines_being_read() {
    let mut app = long_write_under_a_long_reply();
    screen(&mut app, 120, 40);
    app.scroll_up(20);
    let before = screen(&mut app, 120, 40);
    let top = first_row_with(&before, "reply line").expect("the reply is in view");

    app.fold_calls();
    let folded = screen(&mut app, 120, 40);
    assert_eq!(first_row_with(&folded, "reply line"), Some(top), "{folded}");
}

#[test]
fn a_line_cut_away_leaves_the_view_on_the_nearest_line_above_it_still_drawn() {
    let mut app = long_write_under_a_long_reply();
    app.open_diffs();
    screen(&mut app, 120, 40);
    app.scroll_to_head();
    // Put a line only the opened diff draws at the top of the view.
    let mut before = screen(&mut app, 120, 40);
    for _ in 0..100 {
        if first_row_with(&before, "etag28").is_some_and(|(at, _)| at == TRANSCRIPT_TOP) {
            break;
        }
        app.scroll_down(1);
        before = screen(&mut app, 120, 40);
    }
    assert_eq!(
        first_row_with(&before, "etag28").map(|(at, _)| at),
        Some(TRANSCRIPT_TOP),
        "{before}"
    );

    app.open_diffs();
    let cut = screen(&mut app, 120, 40);
    // The line is gone with the rows the cut hides; the nearest one above it
    // still drawn is the last row kept, with the way to the rest under it.
    assert!(!cut.contains("etag28"), "{cut}");
    assert_eq!(
        first_row_with(&cut, "etag20").map(|(at, _)| at),
        Some(TRANSCRIPT_TOP),
        "{cut}"
    );
    assert_eq!(
        first_row_with(&cut, "more rows").map(|(at, _)| at),
        Some(TRANSCRIPT_TOP + 1),
        "{cut}"
    );
}

#[test]
fn toggling_at_the_tail_still_follows_the_tail() {
    let mut app = long_write_under_a_long_reply();
    screen(&mut app, 120, 40);
    app.open_diffs();
    app.fold_calls();
    let frame = screen(&mut app, 120, 40);
    assert!(app.follows_tail());
    assert!(frame.contains("reply line 60"), "{frame}");
}

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

/// A theme is a palette and the line its focused pane is drawn in, and
/// nothing else: every character stays where it was, so the one committed
/// picture of the layout covers every theme. Nothing on screen names the
/// palette in force — `9 Theme` in the F-key row is where one is changed.
#[test]
fn a_theme_moves_no_character_on_screen_but_the_focus_line() {
    let in_the_default_line = |theme: Theme, frame: String| {
        let (from, to) = (
            theme.border_focus.to_border_set(),
            Theme::default().border_focus.to_border_set(),
        );
        let mut frame = frame;
        for (from, to) in [
            (from.top_left, to.top_left),
            (from.top_right, to.top_right),
            (from.bottom_left, to.bottom_left),
            (from.bottom_right, to.bottom_right),
            (from.vertical_left, to.vertical_left),
            (from.horizontal_top, to.horizontal_top),
        ] {
            frame = frame.replace(from, to);
        }
        frame
    };
    for (width, height) in [(80, 24), (200, 60)] {
        let default = screen(&mut running_session(), width, height);
        for theme in THEMES {
            let frame = screen(&mut running_session().with_theme(theme), width, height);
            assert_eq!(
                in_the_default_line(theme, frame),
                default,
                "{} moved something at {width}x{height}",
                theme.name
            );
        }
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
fn a_recorded_prompt_puts_the_question_in_the_transcript_at_both_sizes() {
    assert_snapshot(
        "asking-80x24",
        &screen(&mut session_waiting_on_a_prompt(), 80, 24),
    );
    assert_snapshot(
        "asking-200x60",
        &screen(&mut session_waiting_on_a_prompt(), 200, 60),
    );
}

/// The rows of the question box, with its borders and the pane's taken off.
fn question_rows(frame: &str) -> Vec<String> {
    frame
        .lines()
        .filter_map(|row| row.split('│').nth(1))
        .map(|row| row.trim().to_owned())
        .collect()
}

#[test]
fn the_question_shows_what_would_run_and_every_way_to_answer_it() {
    use niobe_tui::app::Answer;

    let mut app = session_waiting_on_a_prompt();
    let frame = screen(&mut app, 120, 30);

    assert!(frame.contains("? claude asks"), "{frame}");
    assert!(frame.contains("blocks turn 1"), "{frame}");
    let rows = question_rows(&frame);
    assert!(rows.iter().any(|row| row == "to run Read"), "{frame}");
    assert!(rows.iter().any(|row| row == "/repo/notes.txt"), "{frame}");
    // The whole of the arguments, not a summary of them.
    assert!(
        rows.iter()
            .any(|row| row == r#"{"file_path":"/repo/notes.txt"}"#),
        "{frame}"
    );
    // Numbered, the first selected, each with what choosing it does.
    for (number, label, hint) in [
        ("▶ 1.", "Allow once", "this call only"),
        ("2.", "Always allow Read", "niobe saves Read"),
        (
            "3.",
            "Always allow this target",
            "niobe saves Read(/repo/notes.txt)",
        ),
        ("4.", "Deny", "the call does not run"),
    ] {
        assert!(
            rows.iter()
                .any(|row| row.starts_with(&format!("{number} {label}")) && row.ends_with(hint)),
            "option {number} does not read `{label} … {hint}`:\n{frame}"
        );
    }
    assert!(frame.contains("Tab type your own"), "{frame}");
    assert!(frame.contains("Esc decide later"), "{frame}");
    // In the transcript, not over it: the panes and the ask bar are still
    // drawn around it.
    assert!(frame.contains(" Usage "), "{frame}");
    assert!(frame.contains("Ask for a change"), "{frame}");

    // Answered, the question goes and the session is drawn as it was.
    app.answer(Answer::Once);
    assert_eq!(
        screen(&mut app, 120, 30),
        screen(&mut running_session(), 120, 30),
        "the question left something behind on the frame"
    );
}

#[test]
fn the_selected_answer_is_a_solid_bar_as_well_as_a_mark() {
    let mut app = session_waiting_on_a_prompt().with_theme(CLASSIC);
    let bar = (
        Some(CLASSIC.pane_bg),
        Some(CLASSIC.title),
        ratatui::style::Modifier::BOLD,
    );
    let face = |style: Option<Style>| style.map(|s| (s.fg, s.bg, s.add_modifier));
    for text in ["▶ 1.", "Allow once", "this call only"] {
        assert_eq!(
            face(style_at(&mut app, 120, 30, text)),
            Some(bar),
            "`{text}` is not on the inverted bar"
        );
    }
    assert_ne!(
        face(style_at(&mut app, 120, 30, "Always allow Read")),
        Some(bar)
    );
}

#[test]
fn a_standing_answer_too_long_for_its_column_is_repeated_whole() {
    let mut app = running_session();
    let command = "grep -rn description --include=Cargo.toml . | grep -v target";
    app.apply(&Event::PermissionRequest {
        id: "toolu_mcp".into(),
        tool: "mcp__claude_ai_Notion__notion-search".to_owned(),
        input: format!(r#"{{"command":"{command}"}}"#),
        target: Some(command.to_owned()),
    });
    let frame = screen(&mut app, 120, 30);
    let rows = question_rows(&frame);

    assert!(
        rows.iter()
            .any(|row| row.starts_with("2. Always allow Notion·search")),
        "the tool's option does not name it the way the timeline does:\n{frame}"
    );
    let rule: String = rows
        .iter()
        .skip_while(|row| !row.starts_with("3. niobe saves"))
        .take_while(|row| !row.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        rule,
        format!("3. niobe saves mcp__claude_ai_Notion__notion-search({command})"),
        "the rule the operator would save is not shown whole:\n{frame}"
    );
}

#[test]
fn a_long_request_wraps_inside_the_question() {
    let mut app = running_session();
    let command = format!("echo {}", "word ".repeat(40));
    app.apply(&Event::PermissionRequest {
        id: "toolu_long".into(),
        tool: "Bash".to_owned(),
        input: format!(r#"{{"command":"{}"}}"#, command.trim()),
        target: Some(command.trim().to_owned()),
    });
    let frame = screen(&mut app, 200, 60);

    let rows = question_rows(&frame);
    let words = rows
        .iter()
        .take_while(|row| !row.starts_with("▶ 1."))
        .map(|row| row.matches("word").count())
        .sum::<usize>();
    // Forty in the command and forty again in the arguments, none cut off.
    assert_eq!(words, 80, "{frame}");
    assert!(frame.lines().all(|row| text_width(row) <= 200));
}

fn text_width(row: &str) -> usize {
    row.chars().count()
}

#[test]
fn a_question_put_off_stays_in_the_transcript_and_says_it_is_waiting() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = session_waiting_on_a_prompt();
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let frame = screen(&mut app, 120, 30);

    assert!(frame.contains("? claude asks"), "{frame}");
    assert!(frame.contains("the turn is still waiting on it"), "{frame}");
    assert!(frame.contains("Esc answer it"), "{frame}");
    assert!(
        !frame.contains("▶"),
        "a put-off question still shows a live selection"
    );
}

#[test]
fn an_answer_being_written_is_shown_in_the_question_with_where_it_goes() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = session_waiting_on_a_prompt();
    for code in [KeyCode::Tab, KeyCode::Char('n'), KeyCode::Char('o')] {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }
    let frame = screen(&mut app, 120, 30);

    assert!(frame.contains("› no▏"), "{frame}");
    assert!(
        frame.contains("the call does not run, and the agent is given"),
        "{frame}"
    );
    assert!(frame.contains("Enter send"), "{frame}");
    assert_eq!(app.composed(), "", "the words went to the composer");
}

#[test]
fn a_question_arriving_while_scrolled_back_does_not_move_the_view() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = running_session();
    screen(&mut app, 120, 30);
    app.on_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    let before = screen(&mut app, 120, 30);
    // The transcript grows by the question, so the scrollbar's thumb may move
    // down its track; the lines in view are what must not.
    let top = |frame: &str| {
        frame
            .lines()
            .take(10)
            .collect::<Vec<_>>()
            .join("\n")
            .replace('█', &FOCUS_SIDE.to_string())
    };

    app.apply(&Event::PermissionRequest {
        id: "toolu_read".into(),
        tool: "Read".to_owned(),
        input: r#"{"file_path":"/repo/notes.txt"}"#.to_owned(),
        target: Some("/repo/notes.txt".to_owned()),
    });
    let after = screen(&mut app, 120, 30);

    assert_eq!(top(&before), top(&after), "the question moved the view");
    assert!(after.contains("A question is waiting"), "{after}");
}

/// A prompt sent from this shell, with a call of its turn still running, a
/// minute and a quarter in by the clock the event loop hands the shell.
fn session_at_work() -> App {
    use std::time::{Duration, Instant};

    let mut app = App::new(Repo {
        name: "example-app".to_owned(),
        branch: Some("main".to_owned()),
        ..Repo::default()
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
    assert!(
        frame.contains(&format!("{FOCUS_EDGE} Model {FOCUS_EDGE}")),
        "{frame}"
    );
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

    // The fixture reports 0.62 and 0.18, which is what the pane says and all
    // it says: no window is rounded into another's place.
    assert!(frame.contains("62%"), "{frame}");
    assert!(frame.contains("18%"), "{frame}");
    assert!(!frame.contains("extra"), "{frame}");
    // Both resets are the same day the session is read on, so both read as a
    // time on the clock rather than as a weekday.
    assert!(frame.contains("resets 16:40"), "{frame}");

    app.on_key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::F(5),
        ratatui::crossterm::event::KeyModifiers::NONE,
    ));
    let pressed = screen(&mut app, 200, 60);
    // The key reads the windows out against the same moment the pane draws
    // them at: one clock, or the shell says two things about one window.
    assert!(
        pressed.contains("5h window 62%, resets in 2h 59m"),
        "{pressed}"
    );
    assert!(
        pressed.contains("7d window 18%, resets in 4d 1h"),
        "{pressed}"
    );
}

/// The picture of a pane with no windows in it and no word on how the session
/// is billed, so that what such a session shows is something a change has to
/// be read against rather than something nobody has seen.
#[test]
fn a_session_with_no_windows_and_no_billing_draws_neither() {
    let frame = screen(&mut unmetered_session(), 120, 30);
    assert!(!frame.contains("5h"), "{frame}");
    assert!(!frame.contains("resets"), "{frame}");
    assert_snapshot("unmetered-120x30", &frame);
}

/// Prices one model and no other, so that a pane priced by it has a model it
/// can value and one it cannot.
#[derive(Debug)]
struct OnlyOpus;

impl niobe_tui::Prices for OnlyOpus {
    fn estimate(&self, usage: &niobe_core::event::Usage) -> Option<f64> {
        (usage.model == "opus-5").then(|| usage.tokens() as f64 / 1_000.0 * 0.001)
    }
}

/// On a metered account the money is the budget, so the pane opens with it,
/// and each model's row says what that model cost — or an em dash where
/// nothing reported its cost and no price covers it.
#[test]
fn a_metered_profile_leads_the_usage_pane_with_money_and_prices_each_model() {
    let mut app = metered_session().with_prices(Box::new(OnlyOpus));
    let frame = screen(&mut app, 120, 30);

    assert!(!frame.contains("5h"), "{frame}");
    assert!(!frame.contains("extra"), "{frame}");
    // The money comes before the models, not under them.
    let session = frame.find("session").expect("the session's cost is drawn");
    let model = frame.find("opus-5    ").expect("the model rows are drawn");
    assert!(session < model, "{frame}");
    // Haiku is priced by nothing, so the session's figure is a floor under
    // what it cost; opus is, so the floor counts the table's estimate for it
    // and never reads less than opus's own row.
    let totals = app.session().totals().clone();
    let owed = totals.unsettled["opus-5"].tokens() as f64 / 1_000.0 * 0.001;
    assert!(
        frame.contains(&format!(
            "session ≥~${:.2}",
            totals.reported_cost_usd + owed
        )),
        "{frame}"
    );
    let haiku = frame
        .lines()
        .find(|line| line.contains("haiku-4-5"))
        .expect("haiku has a row");
    assert!(
        haiku.trim_end_matches([' ', '│', '█']).ends_with('—'),
        "{haiku}"
    );
    // Opus's own reported figure plus what the table makes of what is owed.
    let opus = format!("~${:.2}", totals.reported_cost_by_model["opus-5"] + owed);
    assert!(
        frame
            .lines()
            .any(|line| line.contains("opus-5") && line.contains(&opus)),
        "{opus}\n{frame}"
    );
    assert_snapshot("metered-120x30", &frame);
}

/// A metered session that has worked for a measured time says how fast it
/// spends, beside what it has spent and on the same terms: a floor under the
/// cost gives a floor under the rate.
#[test]
fn a_metered_session_that_worked_a_measured_time_shows_its_spend_rate() {
    let mut app = metered_session();
    let frame = screen(&mut app, 120, 30);
    let worked = app
        .worked()
        .expect("the fixture's turn is stamped at both ends");
    let spent = app.session().totals().reported_cost_usd;
    let rate = spent / worked.as_secs_f64() * 3_600.0;
    assert!(
        frame.contains(&format!("session ≥${spent:.2} · ≥${rate:.2}/h worked")),
        "{frame}"
    );
}

/// A metered session and the moments it ran at.
fn metered_turn(worked_seconds: u64, cost_usd: Option<f64>) -> App {
    let clock = niobe_tui::clock::Clock::fixed(0).expect("UTC is an offset");
    let at = |seconds| clock.at(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds));
    let mut app = App::new(Repo::default()).with_clock(clock.clone());
    app.tick(std::time::Instant::now(), Some(at(0)));
    let billing = Event::Billing {
        billing: niobe_core::Billing::Metered,
    };
    let prompt = Event::UserMessage {
        text: "go".to_owned(),
    };
    // A million tokens, which `OnlyOpus` values at a dollar.
    let usage = Event::Usage(Usage {
        input: 500_000,
        output: 500_000,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: 0,
        reasoning: 0,
        model: "opus-5".to_owned(),
        cost_usd,
        cost_basis: cost_usd.map(|_| niobe_core::event::CostBasis::Measured),
        settles_model: false,
    });
    for event in [&billing, &prompt, &usage] {
        app.apply_at(event, at(0));
    }
    app.apply_at(&Event::TurnEnded, at(worked_seconds));
    app.tick(std::time::Instant::now(), Some(at(worked_seconds)));
    app
}

#[test]
fn a_rate_is_the_cost_over_the_hours_worked() {
    let frame = screen(&mut metered_turn(1_800, Some(0.50)), 120, 30);
    assert!(frame.contains("session $0.50 · $1.00/h worked"), "{frame}");
}

/// An estimated cost gives an estimated rate.
#[test]
fn a_rate_over_an_estimated_cost_is_an_estimate() {
    let mut app = metered_turn(1_800, None).with_prices(Box::new(OnlyOpus));
    let frame = screen(&mut app, 120, 30);
    assert!(
        frame.contains("session ~$1.00 · ~$2.00/h worked"),
        "{frame}"
    );
}

/// Over the first seconds of a session one request's price is the whole
/// figure, and multiplied up to an hour it reads as a rate nobody is paying.
#[test]
fn a_session_that_has_worked_under_a_minute_shows_no_rate() {
    let frame = screen(&mut metered_turn(59, Some(0.05)), 120, 30);
    assert!(frame.contains("session $0.05"), "{frame}");
    assert!(!frame.contains("/h"), "{frame}");
}

/// A session folded with no clock behind it — an imported transcript, a log —
/// has spent a measured amount over no measured time, and a rate over a
/// guessed time is a fabricated figure. Nothing is drawn, not a zero.
#[test]
fn a_session_with_no_measured_time_shows_no_rate() {
    let mut app = App::new(Repo::default());
    app.extend(&[
        Event::Billing {
            billing: niobe_core::Billing::Metered,
        },
        Event::UserMessage {
            text: "go".to_owned(),
        },
        Event::TurnEnded,
    ]);
    let frame = screen(&mut app, 120, 30);
    assert!(!frame.contains("/h"), "{frame}");
}

/// On a plan no money moves with the work, so there is no rate of spending it.
#[test]
fn a_plan_shows_no_spend_rate() {
    let mut app = metered_turn(1_800, Some(0.50));
    app.apply(&Event::Billing {
        billing: niobe_core::Billing::Plan,
    });
    let frame = screen(&mut app, 120, 30);
    assert!(frame.contains("API-equivalent $0.50"), "{frame}");
    assert!(!frame.contains("/h"), "{frame}");
}

/// A budget on a metered account is money against money, so it stands with
/// the session's cost at the head of the pane.
#[test]
fn a_metered_profiles_budget_stands_under_its_cost() {
    let frame = screen(&mut metered_session().with_budget(0.50), 120, 30);
    let session = frame.find("session ").expect("the cost is drawn");
    let budget = frame.find("budget $").expect("the budget is drawn");
    let model = frame.find("opus-5    ").expect("the model rows are drawn");
    assert!(session < budget && budget < model, "{frame}");
}

/// On a plan no money moves with the work, so what the CLI prices it at is
/// what the same work would have cost on the API: shown, labelled so, dimmed,
/// and never as the session's cost.
#[test]
fn a_plan_shows_what_the_work_would_have_cost_as_api_equivalent_and_dim() {
    let mut app = running_session();
    let frame = screen(&mut app, 120, 30);
    assert!(frame.contains("API-equivalent ≥$0.04"), "{frame}");
    assert!(!frame.contains("session "), "{frame}");
    let dim = app.theme().dim;
    assert_eq!(
        style_at(&mut app, 120, 30, "≥$0.04").and_then(|style| style.fg),
        Some(dim)
    );
}

/// A session whose billing nothing has said and the profile does not set
/// shows no dollar figure: whether it is money spent or money not spent is
/// the one thing the figure cannot say for itself.
#[test]
fn a_session_nobody_said_the_billing_of_shows_no_dollar_figure() {
    let frame = screen(&mut unmetered_session(), 120, 30);
    assert!(frame.contains("session — · billing not known"), "{frame}");
    assert!(!frame.contains("$0."), "{frame}");
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
fn a_plan_spending_beyond_its_flat_fee_is_marked_in_the_usage_pane() {
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
    assert!(frame.contains("100%"), "{frame}");
    assert!(frame.contains(" 91%"), "{frame}");
    assert!(
        frame.contains("extra — · on"),
        "the plan is spending real money and the pane did not say so:\n{frame}"
    );
    assert!(
        !frame.contains("$0.00"),
        "what the extra costs is not a figure any backend reports:\n{frame}"
    );
}

/// A session whose sub-agents ran on cheaper models than its main turn, which
/// is what the per-model block exists to show: the session's tokens split by
/// who spent them.
fn session_on_three_models() -> App {
    let mut app = running_session();
    for (model, input, output, cache_read) in [
        ("claude-sonnet-5-20250929", 900_u64, 240_u64, 9_000_u64),
        ("claude-haiku-4-5-20251001", 400, 90, 2_000),
    ] {
        app.apply(&Event::Usage(Usage {
            input,
            output,
            cache_read,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 0,
            model: model.to_owned(),
            cost_usd: Some(0.01),
            cost_basis: None,
            settles_model: false,
        }));
    }
    app
}

/// The block the Usage pane's token tiles became: a row per model that spent
/// something, the share it spent, and how much.
#[test]
fn the_usage_pane_splits_the_sessions_tokens_by_the_model_that_spent_them() {
    let mut app = session_on_three_models();
    let frame = screen(&mut app, 120, 30);

    // Worked out from the fixture by hand: opus has 2,100+180+18,400+900 and
    // 3,400+620+22,000+1,200, which is 48,800; sonnet 10,140; haiku 2,490.
    // Of 61,430 that is 79.4%, 16.5% and 4.1% — floors 79, 16, 4, and the
    // percent left over goes to sonnet, cut by most.
    let totals = app.session().totals();
    assert_eq!(totals.tokens_by_model.get("opus-5"), Some(&48_800));
    assert_eq!(totals.tokens(), 61_430);

    for row in ["opus-5", "sonnet-5", "haiku-4-5"] {
        assert!(frame.contains(row), "no row for {row}:\n{frame}");
    }
    // The ids are shown shortened, so the release date the backend reported is
    // not what the operator reads a row for.
    assert!(!frame.contains("20250929"), "{frame}");
    assert!(frame.contains(" 79%"), "{frame}");
    assert!(frame.contains(" 17%"), "{frame}");
    assert!(frame.contains("  4%"), "{frame}");
    // Compacted, through the same function the tiles it replaced used.
    assert!(frame.contains("49k"), "{frame}");

    assert_snapshot("models-120x30", &frame);
}

/// The shares on screen add up to the session, at every width the pane has.
/// Three figures rounded one at a time read 99% or 101%, and a column of
/// percentages that does not add up is the pane arguing with itself.
#[test]
fn the_per_model_shares_on_screen_add_up_to_a_hundred() {
    let mut app = session_on_three_models();
    for width in [120, 200] {
        let frame = screen(&mut app, width, 40);
        let shares: Vec<u64> = frame.lines().filter_map(model_row_share).collect();
        assert_eq!(shares.len(), 3, "at {width} columns:\n{frame}");
        assert_eq!(
            shares.iter().sum::<u64>(),
            100,
            "at {width} columns:\n{frame}"
        );
    }
}

/// The percent on a row naming one of the three models, if this line is one.
///
/// A row of the frame is the whole width of the screen, panes and borders and
/// all, so the label is looked for inside it rather than at its start. What
/// makes a line a model row is a share right after the label — which the menu
/// row, where the session's model is also named, does not have.
fn model_row_share(row: &str) -> Option<u64> {
    ["opus-5", "sonnet-5", "haiku-4-5"]
        .into_iter()
        .find_map(|model| {
            let after = &row[row.find(model)? + model.len()..];
            let figure = after.split('%').next()?.trim();
            match (1..=3).contains(&figure.len()) {
                true => figure.parse::<u64>().ok(),
                false => None,
            }
        })
}

/// One model is one row. Not a bar at 100% next to three blank tiles, which is
/// what the four tiles this block replaced did with a single-model session.
#[test]
fn a_session_on_one_model_gets_one_row_and_nothing_beside_it() {
    let mut app = running_session();
    let frame = screen(&mut app, 120, 30);

    let rows: Vec<&str> = frame
        .lines()
        .filter(|row| model_row_share(row).is_some())
        .collect();
    assert_eq!(rows.len(), 1, "{frame}");
    assert!(rows[0].contains("100%"), "{rows:?}");
    assert!(!frame.contains("tokens in"), "the tiles are gone:\n{frame}");
}

/// The cache row is a different kind of figure from the model rows above it —
/// a hit rate, not a share of the session — so it has to be unmistakable or it
/// is a lie told in a bar chart.
#[test]
fn the_cache_row_is_a_hit_rate_and_says_so() {
    let mut app = running_session();
    let frame = screen(&mut app, 120, 30);

    // The fixture sent 2,100 + 3,400 of uncached input, 900 of cache writes
    // and 40,400 of reads: 40,400 of 46,800 is 86.3%.
    assert!(frame.contains("cache hit"), "{frame}");
    assert!(frame.contains(" 86%"), "{frame}");
    // And the reads themselves, so the rate is readable as what it came from.
    assert!(frame.contains("40k"), "{frame}");
    // The row is not one of the model rows: it does not answer the question
    // they do, and it must not be counted with them.
    assert!(model_row_share("cache hit  86%").is_none());
}

/// A session nothing has been billed for yet has no rate to report — which is
/// not a cache that missed. Nor does it get a row per model it never ran.
#[test]
fn a_session_with_nothing_reported_yet_draws_no_share_and_no_rate() {
    let mut app = empty_session();
    let frame = screen(&mut app, 120, 30);

    assert!(!frame.contains("cache hit   0%"), "{frame}");
    assert!(!frame.contains("100%"), "{frame}");
    assert!(
        frame.contains("no tokens reported yet"),
        "the block says it is empty rather than drawing zeroes:\n{frame}"
    );
}

#[test]
fn the_right_stack_collapses_below_a_hundred_columns() {
    let narrow = screen(&mut running_session(), 99, 30);
    let wide = screen(&mut running_session(), 100, 30);

    // Matched on the border the title sits in, so the menu bar's own `Usage`
    // and `Files` entries cannot stand in for a pane. None of them has the
    // keyboard, so each is in a single line.
    for pane in ["─ Usage ", "─ Activity ", "─ Changes "] {
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
    assert!(narrow.contains(" add etag support "));
    assert!(wide.contains(" add etag support "));
}

#[test]
fn the_session_pane_is_titled_by_what_the_session_is_about_at_every_width() {
    let mut app = empty_session();
    app.apply(&Event::UserMessage {
        text: "Price the replayed sessions against the dated table and show the cost floor"
            .to_owned(),
    });

    for width in 80..=200 {
        let frame = screen(&mut app, width, 24);
        let top: Vec<char> = frame.lines().nth(1).unwrap_or_default().chars().collect();
        let corner = top
            .iter()
            .position(|&c| c == FOCUS_TOP_RIGHT)
            .unwrap_or_else(|| panic!("no top-right corner at {width}:\n{frame}"));
        let edge: String = top[..=corner].iter().collect();
        let edge = edge.trim_start();

        assert!(
            edge.starts_with(&format!("{FOCUS_TOP_LEFT}{FOCUS_EDGE}"))
                && edge.ends_with(&format!("{FOCUS_EDGE}{FOCUS_TOP_RIGHT}")),
            "the edge is broken at {width}: {edge}"
        );
        assert!(
            edge.contains(" Price the replayed "),
            "no caption at {width}: {edge}"
        );
        assert!(
            edge.contains("cost floor ") || edge.contains("… "),
            "the caption is cut without saying so at {width}: {edge}"
        );
        assert!(
            !edge.contains("Session ─"),
            "the repository stands in for a caption at {width}: {edge}"
        );
    }
}

/// The rows a pane's top border is on, in the order they appear, whether the
/// pane has the keyboard or not.
fn pane_tops(frame: &str, from: usize) -> Vec<usize> {
    frame
        .lines()
        .enumerate()
        .filter(|(_, row)| {
            row.chars()
                .skip(from)
                .any(|c| c == FOCUS_TOP_LEFT || c == '┌')
        })
        .map(|(at, _)| at)
        .collect()
}

#[test]
fn the_body_is_cut_in_the_proportions_the_layout_is_drawn_to() {
    let frame = screen(&mut running_session(), 200, 60);
    let first = frame.lines().nth(1).unwrap_or_default();

    // The session pane, a column of desktop, then the right-hand stack.
    let gap = first
        .chars()
        .position(|c| c == FOCUS_TOP_RIGHT)
        .unwrap_or(0)
        + 1;
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
    assert!(!tail.contains("Jump to bottom"));

    app.scroll_to_head();
    let head = screen(&mut app, 80, 24);
    assert_ne!(head, tail, "paging to the top drew the same frame");
    assert!(
        head.contains("Jump to bottom ↓"),
        "the pane did not offer the way back down:\n{head}"
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
        .and_then(|border| {
            // The session pane's top-right corner, one column of desktop,
            // then the right-hand stack's top-left corner.
            let row: Vec<char> = border.chars().collect();
            row.windows(3)
                .position(|w| "┐┓╗".contains(w[0]) && "┌┏╔".contains(w[2]))
        })
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

/// The cells of the body that are desktop in a `width`-column `frame`: in a
/// column no pane draws anything in, from the first row of the body to the
/// last. A pane always draws its border at its top, so no column a pane is in
/// qualifies. A frame's rows are trimmed of trailing blanks, so a cell past
/// the end of its row is blank.
fn desktop_columns(frame: &str, width: u16) -> Vec<usize> {
    let rows: Vec<Vec<char>> = frame.lines().map(|row| row.chars().collect()).collect();
    let body = &rows[1..rows.len() - 1];
    (0..usize::from(width))
        .filter(|&x| body.iter().all(|row| row.get(x).is_none_or(|&c| c == ' ')))
        .collect()
}

/// The motion is drawn behind the panes, so a pane's cell is never touched
/// by it: in every theme, at both depths, at every size and on many frames,
/// the only cells that differ from the same moment with effects off are in
/// columns of desktop no pane is in.
#[test]
fn the_motion_never_touches_a_cell_a_pane_owns() {
    use std::time::Duration;

    let mut moved = std::collections::BTreeSet::new();
    for theme in THEMES {
        for depth in [Depth::Sixteen, Depth::TrueColour] {
            for (width, height) in [(80, 24), (120, 30), (200, 60)] {
                for tenths in [0, 7, 33, 120, 751] {
                    let elapsed = Duration::from_millis(tenths * 100);
                    let dressed = |on: bool| {
                        at_work(
                            running_session()
                                .with_depth(depth)
                                .with_theme(theme)
                                .with_effects(on),
                            elapsed,
                        )
                    };
                    let (mut on, mut off) = (dressed(true), dressed(false));
                    let still = screen(&mut off, width, height);
                    let desktop = desktop_columns(&still, width);
                    // Wide, a column at each edge of the screen and one
                    // between the session pane and the right stack; narrow,
                    // the session pane takes the whole body.
                    let wide = width >= ui::WIDE_COLUMNS;
                    assert_eq!(desktop.len(), 3 * usize::from(wide), "{width}x{height}");
                    if wide {
                        let edges = (desktop.first(), desktop.last());
                        let last = usize::from(width) - 1;
                        assert_eq!(edges, (Some(&0), Some(&last)), "{width}x{height}");
                    }
                    let (moving, moving_styles) = (
                        screen(&mut on, width, height),
                        styles(&mut on, width, height),
                    );
                    let still_styles = styles(&mut off, width, height);

                    let columns = usize::from(width);
                    let mut touched = 0;
                    let texts = moving.lines().zip(still.lines()).enumerate();
                    for (y, (a, b)) in texts {
                        for (x, (a, b)) in a.chars().zip(b.chars()).enumerate() {
                            let cell = y * columns + x;
                            let differs = a != b || moving_styles[cell] != still_styles[cell];
                            touched += usize::from(differs);
                            assert!(
                                !differs || desktop.contains(&x),
                                "{} {width}x{height} at {elapsed:?}: the motion drew {a:?} \
                                 over a pane's {b:?} at column {x}, row {y}",
                                theme.name
                            );
                        }
                    }
                    // A test that only ever compared two blank desktops would
                    // pass whatever the motion drew.
                    if touched > 0 {
                        moved.insert(theme.name);
                    }
                }
            }
        }
    }
    // Each of the three themes that move drew on the desktop at some moment,
    // and the one that stays still never did.
    assert_eq!(moved.len(), 3, "{moved:?}");
    assert!(!moved.contains(CLASSIC.name), "{moved:?}");
}

/// Turned off, the desktop stays empty while a turn runs, in every theme,
/// and the turn still says it is running under the transcript.
#[test]
fn with_effects_off_the_desktop_is_empty_while_a_turn_runs() {
    use std::time::Duration;

    for theme in THEMES {
        let mut app = at_work(
            running_session().with_theme(theme).with_effects(false),
            Duration::from_secs(3),
        );
        let frame = screen(&mut app, 120, 30);
        let desk = gutter(&frame);
        assert_eq!(
            desktop_columns(&frame, 120).len(),
            3,
            "{}: {frame}",
            theme.name
        );
        assert!(
            desk.chars().all(char::is_whitespace),
            "{}: {desk:?}",
            theme.name
        );
        assert!(frame.contains("working"), "{frame}");
    }
}

/// A frame of each motion, where the operator sees it: the desktop between
/// the panes, five seconds into a turn.
#[test]
fn a_frame_of_each_motion_is_drawn_on_the_desktop_between_the_panes() {
    use std::time::Duration;

    for theme in [NEO, CYBER, MODERN] {
        let mut app = at_work(running_session().with_theme(theme), Duration::from_secs(5));
        let frame = screen(&mut app, 120, 30);
        assert!(
            !gutter(&frame).chars().all(char::is_whitespace),
            "{}: the motion drew nothing between the panes",
            theme.name
        );
        assert_snapshot(
            &format!("motion-{}-120x30", theme.name.to_lowercase()),
            &frame,
        );
    }
}

/// The columns a row is made of: a pane's border is [`FOCUS_SIDE`] on the
/// pane with the keyboard and `│` on the others, or the scrollbar's thumb where one is drawn
/// over it.
fn borders(row: &str) -> Vec<usize> {
    row.chars()
        .enumerate()
        .filter(|&(_, c)| c == FOCUS_SIDE || matches!(c, '│' | '█'))
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
            under
                .chars()
                .all(|c| c == FOCUS_SIDE || matches!(c, '│' | ' ')),
            "the row under the pane titles at {width}x{height} is not blank: {under:?}"
        );
    }
}

/// A folded section is its header and nothing else, and what was under it
/// gives its rows back to the sections below.
///
/// The marker is what says so: `▸` closed against `▾` open, which reads
/// without colour and on a terminal that draws neither in bold.
#[test]
fn the_changes_pane_folds_a_section_away() {
    let mut app = running_session();
    app.fold(Section::WorkingTree);
    let frame = screen(&mut app, 200, 60);

    assert_snapshot("changes-folded-200x60", &frame);
    assert!(
        frame.contains("▸ Working tree"),
        "a folded section still has to say that it is folded"
    );
    assert!(
        !frame.contains("cache/lru.ts"),
        "the files are folded away, not merely scrolled past"
    );
    assert!(
        frame.contains("▾ Commits"),
        "the sections below take the rows the folded one gave back"
    );
}

/// The pane scrolls rather than truncating: the commits and what the session
/// itself reported are below twenty-three files, and they are reachable.
#[test]
fn the_changes_pane_scrolls_to_what_is_below_the_files() {
    let mut app = running_session();
    // Draw once so the pane knows how tall it is and how much it holds; the
    // event loop has always drawn before a wheel notch can arrive.
    let _ = screen(&mut app, 200, 60);
    app.scroll_pane(Pane::Changes, 24);
    let frame = screen(&mut app, 200, 60);

    assert_snapshot("changes-scrolled-200x60", &frame);
    assert!(
        frame.contains("▾ Commits"),
        "the commits section is what the pane was scrolled to"
    );
    assert!(
        frame.contains("9f2c1ab"),
        "a commit the session made is drawn with its short hash"
    );
}

const GREEN: TestCounts = TestCounts {
    passed: 637,
    failed: 0,
    ignored: 0,
    suites: 30,
};

/// The Changes pane's rows, scrolled to the bottom, where the Tests section
/// is drawn.
fn changes_scrolled_down(app: &mut App, width: u16, height: u16) -> String {
    let _ = screen(app, width, height);
    app.scroll_pane(Pane::Changes, isize::MAX / 2);
    screen(app, width, height)
}

/// The Tests section's header row, from its name to the pane's edge: the
/// transcript beside it draws each run's own figures under its call, and
/// those are not what the section says.
fn tests_header(frame: &str) -> &str {
    frame
        .lines()
        .find_map(|line| line.split_once("▾ Tests").map(|(_, header)| header))
        .and_then(|header| header.split('│').next())
        .expect("the frame draws the Tests section")
}

/// The test run the agent made, as its own summary reported it, at the foot
/// of the Changes pane and dated by when its call finished.
#[test]
fn the_agents_own_test_run_is_drawn_with_its_counts_and_its_age() {
    let mut app = session_with_test_runs(&[(Some(GREEN), Some(0), false)]);
    let frame = changes_scrolled_down(&mut app, 200, 60);

    assert_snapshot("tests-200x60", &frame);
    assert!(
        frame.contains("▾ Tests  637 passed · 0 failed · 30 suites · 1m ago"),
        "{frame}"
    );
    assert_eq!(
        style_at(&mut app, 200, 60, "637 passed").and_then(|style| style.fg),
        Some(CLASSIC.add),
        "a run that passed says so in the colour of an addition"
    );
}

#[test]
fn a_session_that_ran_no_tests_draws_no_tests_section() {
    // The session does run `npm test`, which is not a format this shell
    // reads, so it is not a test run as far as the pane is concerned.
    let mut app = running_session();
    let frame = changes_scrolled_down(&mut app, 200, 60);

    assert!(!frame.contains("Tests"), "{frame}");
    assert!(!frame.contains("0 passed"), "{frame}");
}

#[test]
fn a_failing_run_is_drawn_as_failing_with_its_own_count() {
    let failing = TestCounts {
        passed: 612,
        failed: 3,
        ignored: 2,
        suites: 30,
    };
    let mut app = session_with_test_runs(&[(Some(failing), Some(101), true)]);
    let frame = changes_scrolled_down(&mut app, 200, 60);

    assert!(
        frame.contains("612 passed · 3 failed · 2 ignored · 30 suites · 1m ago"),
        "{frame}"
    );
    assert_eq!(
        style_at(&mut app, 200, 60, "3 failed").and_then(|style| style.fg),
        Some(CLASSIC.del)
    );
    assert_ne!(
        style_at(&mut app, 200, 60, "612 passed").and_then(|style| style.fg),
        Some(CLASSIC.add),
        "a failing run does not colour its passes as if all were well"
    );
}

#[test]
fn a_run_whose_result_was_not_read_says_so_and_gives_no_count() {
    let mut app =
        session_with_test_runs(&[(Some(GREEN), Some(0), false), (None, Some(101), false)]);
    let frame = changes_scrolled_down(&mut app, 200, 60);

    assert!(
        frame.contains("▾ Tests  exit 101 · result not read · 1m ago"),
        "{frame}"
    );
    assert!(
        !tests_header(&frame).contains("passed"),
        "the earlier run's counts are not the latest run's"
    );
}

/// A failing run whose output the backend could not read whole — the Claude
/// CLI cuts a long failure and keeps none of it — says it failed, apart from
/// a run whose result was simply not read, and still gives no count.
#[test]
fn a_run_known_to_have_failed_without_counts_says_it_failed_and_gives_no_count() {
    let mut app = session_with_test_runs(&[(Some(GREEN), Some(0), false), (None, Some(101), true)]);
    let frame = changes_scrolled_down(&mut app, 200, 60);

    assert!(
        frame.contains("▾ Tests  failed · exit 101 · counts not read · 1m ago"),
        "{frame}"
    );
    let header = tests_header(&frame);
    assert!(!header.contains("passed"), "{header}");
    assert!(!header.contains("result not read"), "{header}");
    assert_eq!(
        style_at(&mut app, 200, 60, "failed · exit").and_then(|style| style.fg),
        Some(CLASSIC.del),
        "a failure is drawn in the failure colour"
    );
}

/// A failed run whose output still held a failing binary's list of what
/// failed names those tests under the section's header, as that binary's: an
/// earlier binary's failures may be in the part that was cut.
#[test]
fn the_tests_a_failed_run_named_are_listed_as_the_failures_of_their_binary() {
    let run = TestRunRecord::new(
        None,
        Some(101),
        true,
        vec![FailedTests {
            binary: "--test statement".to_owned(),
            tests: vec![
                "a_statement_line_037_rounds_like_the_ledger".to_owned(),
                "a_statement_line_088_rounds_like_the_ledger".to_owned(),
            ],
        }],
    );
    let mut app = session_with_test_records(&[run]);
    let frame = changes_scrolled_down(&mut app, 200, 60);

    let row = |text: &str| {
        frame
            .lines()
            .position(|line| line.contains(text))
            .unwrap_or_else(|| panic!("{text:?} is drawn:\n{frame}"))
    };
    let header = row("▾ Tests  failed · exit 101 · counts not read · 1m ago");
    assert_eq!(
        [
            row("│   failing in --test statement "),
            row("│   ✗ a_statement_line_037_rounds_like_the_ledger "),
            row("│   ✗ a_statement_line_088_rounds_like_the_ledger "),
        ],
        [header + 1, header + 2, header + 3],
        "{frame}"
    );
    assert_eq!(
        style_at(&mut app, 200, 60, "✗ a_statement_line_037").and_then(|style| style.fg),
        Some(CLASSIC.del)
    );

    app.fold(Section::Tests);
    let folded = changes_scrolled_down(&mut app, 200, 60);
    assert!(folded.contains("▸ Tests  failed"), "{folded}");
    assert!(!folded.contains("failing in --test statement"), "{folded}");
    assert!(!folded.contains("✗ a_statement_line_037"), "{folded}");
}

/// A run that went on past a failing binary lists each binary's failures
/// under its own label, in the order the binaries ran.
#[test]
fn a_run_that_failed_in_two_binaries_lists_each_binarys_failures_under_its_own_label() {
    let counts = TestCounts {
        passed: 5,
        failed: 2,
        ignored: 1,
        suites: 3,
    };
    let run = TestRunRecord::new(
        Some(counts),
        Some(101),
        true,
        vec![
            FailedTests {
                binary: "--lib".to_owned(),
                tests: vec!["tests::wrong".to_owned()],
            },
            FailedTests {
                binary: "--test api".to_owned(),
                tests: vec!["from_outside".to_owned()],
            },
        ],
    );
    let mut app = session_with_test_records(&[run]);
    let frame = changes_scrolled_down(&mut app, 200, 60);

    let row = |text: &str| {
        frame
            .lines()
            .position(|line| line.contains(text))
            .unwrap_or_else(|| panic!("{text:?} is drawn:\n{frame}"))
    };
    let header = row("▾ Tests  5 passed · 2 failed");
    assert_eq!(
        [
            row("│   failing in --lib "),
            row("│   ✗ tests::wrong "),
            row("│   failing in --test api "),
            row("│   ✗ from_outside "),
        ],
        [header + 1, header + 2, header + 3, header + 4],
        "{frame}"
    );
}

/// `cargo test 2>&1 | tail -30` of a failing run exits with tail's status,
/// and the list its tail kept proves the failure by itself: the section says
/// the run failed, with no count and no `exit 0` beside it.
#[test]
fn a_tailed_run_that_listed_a_failure_reads_as_failed_with_no_count() {
    let run = TestRunRecord::new(
        None,
        Some(0),
        false,
        vec![FailedTests {
            binary: "--lib".to_owned(),
            tests: vec!["tests::wrong".to_owned()],
        }],
    );
    let mut app = session_with_test_records(&[run]);
    let frame = changes_scrolled_down(&mut app, 200, 60);

    assert!(
        frame.contains("▾ Tests  failed · counts not read · 1m ago"),
        "{frame}"
    );
    assert!(frame.contains("│   failing in --lib "), "{frame}");
    assert!(frame.contains("│   ✗ tests::wrong "), "{frame}");
    let header = tests_header(&frame);
    assert!(!header.contains("exit"), "{header}");
}

#[test]
fn a_narrow_pane_keeps_the_counts_and_sheds_the_rest() {
    let mut app = session_with_test_runs(&[(Some(GREEN), Some(0), false)]);
    let frame = changes_scrolled_down(&mut app, 120, 30);

    assert!(frame.contains("▾ Tests  637 passed · 0 failed"), "{frame}");
    assert!(!frame.contains("30 suites"), "{frame}");
}

/// A directory that is not a repository has no branch and no commits, and the
/// pane says nothing about either rather than drawing them empty: a `Commits`
/// section reading `none this session` would be a claim about a repository
/// that is not there.
#[test]
fn a_session_outside_a_repository_draws_no_branch_and_no_commits() {
    let mut app = App::new(Repo {
        name: "scratch".to_owned(),
        branch: None,
        ..Repo::default()
    });
    let frame = screen(&mut app, 120, 30);

    assert!(!frame.contains('⎇'), "there is no branch to name");
    assert!(!frame.contains("Commits"), "{frame}");
    assert!(!frame.contains("Working tree"), "{frame}");
    assert!(
        frame.contains("This session"),
        "what the session itself changed is not the repository's to report"
    );
}

/// The wheel goes to whatever the pointer is over. Two panes scroll, and a
/// notch over one of them must not move the other.
#[test]
fn a_wheel_notch_over_the_changes_pane_leaves_the_transcript_where_it_was() {
    let mut app = session_with_a_markdown_reply();
    let _ = screen(&mut app, 200, 60);
    app.scroll_to_head();
    let transcript = app.scroll();

    // Inside the Changes pane: the right-hand column, a third of the way down.
    app.on_mouse(wheel_at(170, 25));

    assert_eq!(
        app.scroll(),
        transcript,
        "a notch over the Changes pane moved the transcript"
    );
    assert!(
        app.pane_scroll(Pane::Changes) > 0,
        "and it did not move the pane it was over"
    );
}

/// One notch of the wheel, at a point on the screen.
fn wheel_at(column: u16, row: u16) -> ratatui::crossterm::event::MouseEvent {
    ratatui::crossterm::event::MouseEvent {
        kind: ratatui::crossterm::event::MouseEventKind::ScrollDown,
        column,
        row,
        modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
    }
}

/// The three sections the pane holds, each with the summary line the operator
/// reads it by.
#[test]
fn the_activity_pane_holds_the_sub_agents_the_decisions_and_the_tools() {
    let frame = screen(&mut running_session(), 200, 60);

    assert!(frame.contains("─ Activity "), "{frame}");
    for section in ["▾ Sub-agents", "▾ Decisions", "▾ Tools"] {
        assert!(frame.contains(section), "{section} is missing:\n{frame}");
    }
    // The recorded session spawned three: one still running, one done, one
    // failed. The summary counts what it counted.
    assert!(
        frame.contains("1 running · 3 spawned · 1 failed"),
        "{frame}"
    );
}

/// A sixteen-colour terminal in a palette the operator chose is not somewhere
/// a colour can be the only difference between an agent that finished and one
/// that failed, so the glyph carries the state on its own.
#[test]
fn a_sub_agents_state_is_legible_without_its_colour() {
    let frame = screen(&mut running_session(), 200, 60);

    assert!(frame.contains("◆ test-writer"), "running:\n{frame}");
    assert!(frame.contains("◇ reviewer"), "done:\n{frame}");
    assert!(frame.contains("✗ doc-writer"), "failed:\n{frame}");
}

/// A list of only what is running now would erase the failure at the moment
/// it matters most.
#[test]
fn an_agent_that_finished_stays_in_the_list_with_its_outcome() {
    let frame = screen(&mut running_session(), 200, 60);

    let row = frame
        .lines()
        .find(|line| line.contains("doc-writer"))
        .unwrap_or_default();
    assert!(row.contains("failed"), "{row:?}");

    let done = frame
        .lines()
        .find(|line| line.contains("reviewer"))
        .unwrap_or_default();
    assert!(done.contains("done"), "{done:?}");
}

/// A running agent's status column is an elapsed time, measured from the
/// moment it was spawned against the moment the shell is drawing at. A
/// finished one carries the size its conversation reached where its backend
/// reported one, and a failed one carries its state alone.
#[test]
fn a_running_agent_is_timed_and_a_finished_one_shows_its_context() {
    let frame = screen(&mut running_session(), 200, 60);

    let running = frame
        .lines()
        .find(|line| line.contains("test-writer"))
        .unwrap_or_default();
    assert!(
        running.contains("running 1m 42s"),
        "the session ran for 102 seconds before it was read:\n{running:?}"
    );

    assert!(!running.contains("ctx"), "{running:?}");

    for (finished, word) in [("reviewer", "done 4100 ctx"), ("doc-writer", "failed")] {
        let row = frame
            .lines()
            .find(|line| line.contains(finished))
            .unwrap_or_default();
        // Nothing follows the status column but the pane's own border.
        assert!(
            row.trim_end_matches(['│', ' ']).ends_with(word),
            "{finished} reads other than {word:?}: {row:?}"
        );
    }
}

/// The model a sub-agent's own messages named is drawn beside what it was
/// spawned to do, shortened the way the Usage pane shortens it; an agent whose
/// backend named none is drawn with none.
#[test]
fn a_sub_agents_model_is_drawn_beside_what_it_was_spawned_to_do() {
    let frame = screen(&mut running_session(), 200, 60);
    let row = |label: &str| {
        frame
            .lines()
            .find(|line| line.contains(label))
            .unwrap_or_default()
            .to_owned()
    };

    assert!(
        row("test-writer").contains("tests/fetch.test.ts sonnet-5"),
        "{frame}"
    );
    assert!(
        row("reviewer").contains("catalog/cache.ts haiku-4-5"),
        "{frame}"
    );
    for model in ["sonnet", "haiku", "opus"] {
        assert!(!row("doc-writer").contains(model), "{frame}");
    }
}

/// Under an agent is the last thing it was observed doing, and under an agent
/// that reported nothing there is nothing — not an empty `└`.
#[test]
fn under_each_agent_is_the_last_thing_it_was_seen_doing() {
    let frame = screen(&mut running_session(), 200, 60);
    let lines: Vec<&str> = frame.lines().collect();
    let under = |label: &str| {
        let at = lines
            .iter()
            .position(|line| line.contains(label))
            .expect("the agent has a row");
        lines[at + 1].to_owned()
    };

    assert!(
        under("test-writer").contains("└ Reading tests/stream.rs"),
        "{frame}"
    );
    assert!(
        under("doc-writer").contains("└ Notion 404, gave up after 2 retries"),
        "{frame}"
    );
    assert!(under("reviewer").contains("✗ doc-writer"), "{frame}");
    // The transcript hangs rows of its own from a `└`; in the column to its
    // right, every one is under an agent.
    let under_agents: usize = frame
        .lines()
        .filter_map(|line| {
            line.split_once(&format!("{FOCUS_SIDE} │"))
                .map(|(_, right)| right)
        })
        .map(|right| right.matches('└').count())
        .sum();
    assert_eq!(under_agents, 2, "{frame}");
}

/// A decision carries the time it was recorded, in a column of its own, and
/// what wraps out of the summary hangs under the summary rather than under
/// the time.
#[test]
fn a_decision_is_drawn_under_the_time_it_was_recorded_at() {
    let frame = screen(&mut running_session(), 200, 60);
    let rows: Vec<&str> = frame.lines().collect();
    let at = rows
        .iter()
        .position(|line| line.contains("Key the cache on the request URL"))
        .expect("the newest decision is the first one under the header");

    // The recorded session is folded at 13:39 and read at 13:41: the column
    // carries the moment the decision was recorded, not the moment it is
    // drawn at.
    assert!(rows[at].contains("13:39"), "{:?}", rows[at]);
    assert!(
        rows[at + 1].contains("manifests can share an id"),
        "the summary wraps under itself, not under the time: {:?}",
        rows[at + 1]
    );
    // Columns, not bytes: the transcript beside the pane draws characters
    // wider than a byte, and the two rows carry different ones.
    let column = |row: &str, text: &str| row.find(text).map(|at| row[..at].chars().count());
    let hang = column(rows[at + 1], "two manifests").zip(column(rows[at], "Key the cache"));
    assert!(
        hang.map(|(a, b)| a == b).unwrap_or(false),
        "the hanging indent is the summary's own column: {hang:?}"
    );
}

/// Newest first: the pane does not follow its own tail, so a decision
/// appended below the fold would never be seen. Under the header it is always
/// one row from a heading the eye already has.
#[test]
fn the_newest_decision_is_the_one_under_the_header() {
    let frame = screen(&mut running_session(), 200, 60);
    let rows: Vec<&str> = frame.lines().collect();
    let header = rows
        .iter()
        .position(|line| line.contains("▾ Decisions"))
        .expect("the section is drawn");

    assert!(
        rows[header + 1].contains("Key the cache on the request URL"),
        "the second decision recorded is the first one drawn: {:?}",
        rows[header + 1]
    );
}

/// A session reaches for one MCP server a dozen ways, and a row each says
/// less about where its calls went than one row saying how many went to that
/// server. A backend's own tools are already the family they belong to.
#[test]
fn the_tools_of_one_mcp_server_are_counted_as_one_family() {
    let frame = screen(&mut running_session(), 200, 60);

    let row = frame
        .lines()
        .find(|line| line.contains("Notion·*"))
        .unwrap_or_default();
    assert!(row.contains('3'), "three calls to the one server: {row:?}");
    assert!(
        row.contains("✗ 1"),
        "the family's own failure is on the family's row: {row:?}"
    );
    assert!(
        !frame
            .lines()
            .any(|line| line.contains("Notion·search") && line.contains('━')),
        "an individual MCP tool is not a row of its own:\n{frame}"
    );

    // A tool with no family shows under its own name, and carries no failure
    // column where it has no failures.
    let read = frame
        .lines()
        .find(|line| line.contains("Read"))
        .unwrap_or_default();
    assert!(!read.contains('✗'), "{read:?}");
}

/// Nothing having happened reads as nothing having happened — not as a figure
/// of zero, and not as a bar drawn for a tool that never ran.
#[test]
fn an_untouched_activity_pane_draws_no_zeroed_bar() {
    let frame = screen(&mut empty_session(), 120, 30);

    assert!(frame.contains("none spawned"), "{frame}");
    assert!(frame.contains("none recorded"), "{frame}");
    assert!(frame.contains("none called"), "{frame}");
    // A bar is drawn in the same heavy line the focused pane's edges are, so
    // the rows those edges are on are not where a bar is looked for.
    let bar = frame
        .lines()
        .filter(|row| !row.contains(FOCUS_TOP_LEFT) && !row.contains(FOCUS_BOTTOM_LEFT))
        .any(|row| row.contains('━'));
    assert!(!bar, "a bar was drawn for a tool that never ran:\n{frame}");
    assert!(
        !frame.contains('✗'),
        "no failure count where nothing failed:\n{frame}"
    );
}

/// The pane scrolls rather than truncating: with three sections in it, the
/// tools are below the sub-agents and the decisions, and they are reachable.
#[test]
fn the_activity_pane_scrolls_to_what_is_below_the_agents() {
    let mut app = running_session();
    // Draw once so the pane knows how tall it is and how much it holds. At
    // 200x60 it holds everything it has; a terminal half that tall is where a
    // pane with three sections in it has to scroll.
    let _ = screen(&mut app, 120, 30);
    app.scroll_pane(Pane::Activity, 40);
    let frame = screen(&mut app, 120, 30);

    assert_snapshot("activity-scrolled-120x30", &frame);
    // The section's rows rather than its header: scrolled to its end, the
    // pane shows the last rows it holds, and how many of them fit above the
    // tools depends on how tall the panes over it are.
    assert!(
        frame.contains("Notion·*"),
        "the tools section is what the pane was scrolled to:\n{frame}"
    );
    assert!(
        !frame.contains("◆ test-writer"),
        "and the agents above it are what it scrolled past:\n{frame}"
    );
}

/// Each section folds on its own, and folding one in the Activity pane leaves
/// the Changes pane where it was.
#[test]
fn the_activity_pane_folds_a_section_away() {
    let mut app = running_session();
    app.fold(Section::SubAgents);
    let frame = screen(&mut app, 200, 60);

    assert!(
        frame.contains("▸ Sub-agents"),
        "a folded section still has to say that it is folded:\n{frame}"
    );
    assert!(
        !frame.contains("test-writer"),
        "the agents are folded away, not merely scrolled past:\n{frame}"
    );
    assert!(frame.contains("▾ Decisions"), "{frame}");
    assert!(
        frame.contains("▾ Working tree"),
        "folding a section of one pane left the other alone:\n{frame}"
    );
}

/// The wheel goes to whatever the pointer is over. Three panes scroll now, and
/// a notch over the Activity pane must move neither of the other two.
#[test]
fn a_wheel_notch_over_the_activity_pane_moves_only_that_pane() {
    let mut app = running_session();
    let _ = screen(&mut app, 120, 30);
    app.scroll_to_head();
    let transcript = app.scroll();
    let changes = app.pane_scroll(Pane::Changes);

    // Inside the Activity pane: the right-hand column, near the bottom.
    app.on_mouse(wheel_at(100, 26));

    assert_eq!(app.scroll(), transcript, "the transcript moved");
    assert_eq!(app.pane_scroll(Pane::Changes), changes, "the Changes moved");
    assert!(
        app.pane_scroll(Pane::Activity) > 0,
        "and the pane the pointer was over did not"
    );
}

/// A log that kept no times gives a decision no time. The column stays, so the
/// summaries keep their edge, but it is left blank: a time of zero would be a
/// moment nobody recorded, and the pane would be inventing one.
#[test]
fn a_decision_from_a_log_with_no_times_is_drawn_without_one() {
    let mut app = empty_session();
    app.apply(&Event::Decision {
        summary: "Read the etag off the response, not the cache entry.".to_owned(),
        rationale: None,
        rejected: vec![],
    });
    let frame = screen(&mut app, 120, 30);
    let row = frame
        .lines()
        .find(|line| line.contains("Read the etag off the response"))
        .unwrap_or_default();

    assert!(
        !row.contains(':'),
        "a shell with no clock drew a time anyway: {row:?}"
    );
    // The summary still starts where a decision with a time would: the column
    // is what keeps the section's left edge straight.
    let dated = screen(&mut running_session(), 120, 30);
    let with_time = dated
        .lines()
        .find(|line| line.contains("13:39"))
        .unwrap_or_default();
    assert_eq!(
        row.find("Read the etag"),
        with_time.find("Reuse the existing LRU"),
        "the summaries do not share a column:\n{row:?}\n{with_time:?}"
    );
}

/// The row the ask bar is drawn on: the one where a badge meets the prompt's
/// marker, whichever mode the badge names.
fn bar_row(frame: &str) -> String {
    frame
        .lines()
        .find(|row| row.contains("  > "))
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("no ask bar on the frame:\n{frame}"))
}

fn press(app: &mut App, code: ratatui::crossterm::event::KeyCode) {
    app.on_key(ratatui::crossterm::event::KeyEvent::new(
        code,
        ratatui::crossterm::event::KeyModifiers::NONE,
    ));
}

#[test]
fn the_ask_bar_wears_its_badge_in_every_theme() {
    for theme in niobe_tui::theme::THEMES {
        let mut app = running_session().with_theme(theme);
        let badge = style_at(&mut app, 120, 30, " ask ").expect("the bar is drawn");
        assert_eq!(
            (badge.fg, badge.bg),
            (Some(theme.pane_bg), Some(theme.hot)),
            "the badge is the pane's colour on the hot one, as a chip"
        );
        assert!(
            badge.add_modifier.contains(ratatui::style::Modifier::BOLD),
            "the badge is bold"
        );
    }
}

#[test]
fn the_placeholder_names_only_what_the_composer_does_today() {
    // Wide enough for the whole placeholder beside the hints the bar holds.
    let mut app = running_session();
    let row = bar_row(&screen(&mut app, 200, 60));
    assert!(row.contains("Ask for a change"), "{row}");
    assert!(row.contains("/ search transcript"), "{row}");
    assert!(row.contains("@ file"), "{row}");
    assert!(
        !row.contains("! shell"),
        "the bar promises `! shell`, which the composer does not do:\n{row}"
    );

    app.type_into_composer(ratatui_textarea::Input {
        key: ratatui_textarea::Key::Char('x'),
        ..Default::default()
    });
    let row = bar_row(&screen(&mut app, 120, 30));
    assert!(
        !row.contains("Ask for a change"),
        "the placeholder stays behind what was typed:\n{row}"
    );
}

#[test]
fn the_bar_names_the_mode_once_a_backend_has_said_it() {
    let mut app = empty_session();
    let row = bar_row(&screen(&mut app, 200, 60));
    assert!(
        row.contains(" —  > "),
        "nothing has said how this session gates tool calls, so the badge claims no mode:\n{row}"
    );
    for mode in [Mode::Plan, Mode::Ask, Mode::Auto] {
        assert!(!row.contains(&format!(" {mode}  > ")), "{row}");
    }
    assert!(row.contains("Shift+Tab mode"), "{row}");
    assert!(row.contains("Ctrl+J newline"), "{row}");

    for mode in [Mode::Plan, Mode::Ask, Mode::Auto] {
        app.apply(&Event::ModeSelected { mode });
        let row = bar_row(&screen(&mut app, 200, 60));
        assert!(
            row.contains(&format!(" {mode}  > ")),
            "the badge names the mode:\n{row}"
        );
        assert!(
            !row.contains(&format!("{mode} mode")),
            "the mode is said twice, in the badge and at the right end:\n{row}"
        );
        assert!(row.contains("Shift+Tab cycles"), "{row}");
        assert!(row.contains("Ctrl+J newline"), "{row}");
    }
}

#[test]
fn the_badge_keeps_the_mode_while_the_right_end_is_taken() {
    let mut app = running_session();
    app.apply(&Event::ModeSelected { mode: Mode::Auto });

    press(&mut app, ratatui::crossterm::event::KeyCode::F(3));
    let row = bar_row(&screen(&mut app, 80, 24));
    assert!(row.contains("F3 Diff"), "{row}");
    assert!(
        row.contains(" auto  > "),
        "the shell's reply hid the mode:\n{row}"
    );

    press(&mut app, ratatui::crossterm::event::KeyCode::Char('x'));
    for c in "a prompt long enough to reach where the hints are drawn on the bar".chars() {
        app.type_into_composer(ratatui_textarea::Input {
            key: ratatui_textarea::Key::Char(c),
            ..Default::default()
        });
    }
    let row = bar_row(&screen(&mut app, 80, 24));
    assert!(
        row.contains(" auto  > "),
        "a long prompt hid the mode:\n{row}"
    );
}

#[test]
fn a_command_and_a_search_keep_their_own_badges() {
    let mut app = running_session().runs_commands();
    app.apply(&Event::ModeSelected { mode: Mode::Auto });

    press(&mut app, ratatui::crossterm::event::KeyCode::Char('!'));
    let frame = screen(&mut app, 120, 30);
    assert!(frame.contains(" shell  $ "), "{frame}");
    assert!(!frame.contains(" auto  > "), "{frame}");

    press(&mut app, ratatui::crossterm::event::KeyCode::Esc);
    press(&mut app, ratatui::crossterm::event::KeyCode::Char('/'));
    let frame = screen(&mut app, 120, 30);
    assert!(frame.contains(" find  / "), "{frame}");
    assert!(!frame.contains(" auto  > "), "{frame}");
}

#[test]
fn the_bar_names_shift_enter_only_on_a_terminal_that_can_report_it() {
    let row = bar_row(&screen(&mut empty_session(), 200, 60));
    assert!(
        !row.contains("Shift+Enter"),
        "on a terminal that sends Enter for Shift+Enter, the bar names the key that sends:\n{row}"
    );

    let mut app = empty_session().reports_shift_enter();
    let row = bar_row(&screen(&mut app, 200, 60));
    assert!(row.contains("Shift+Enter newline"), "{row}");
    assert!(!row.contains("Ctrl+J"), "{row}");
}

#[test]
fn a_narrowing_bar_drops_whole_hints_and_never_cuts_one() {
    let mut app = running_session();
    app.apply(&Event::ModeSelected { mode: Mode::Auto });
    let whole = ["Shift+Tab cycles", "Ctrl+J newline"];
    let mut seen = std::collections::BTreeSet::new();
    for width in 80..=200 {
        let row = bar_row(&screen(&mut app, width, 30));
        let shown: Vec<&str> = whole
            .iter()
            .copied()
            .filter(|hint| row.contains(hint))
            .collect();
        // The key that opens a line is held: on a terminal that sends Enter
        // for Shift+Enter, it is the hint whose absence sends a prompt the
        // operator meant to go on writing. The reminder of the key that
        // cycles the mode gives way before it.
        let held = ["Ctrl+J newline"];
        assert!(
            shown == whole || shown == held,
            "at {width} columns the hints are not the held ones or all of them:\n{row}"
        );
        for piece in ["Ctrl+", "Shift+T"] {
            assert!(
                shown.iter().any(|hint| hint.starts_with(piece)) || !row.contains(piece),
                "at {width} columns a hint is cut short:\n{row}"
            );
        }
        seen.insert(shown.len());
    }
    assert!(
        seen.len() > 1,
        "no width in the range dropped a hint, so the test proves nothing: {seen:?}"
    );
}

/// Where the bar at 80 columns has not room for the placeholder whole and the
/// key hints, the placeholder's trailing affordances give way first: they
/// remind the operator of what the composer can do, while the key that opens
/// a line — on a terminal that sends Enter for Shift+Enter — is what stops
/// Enter sending a prompt half-written. After them the hints that are only
/// reminders give way (`Shift+Tab cycles`, `Tab panes`); the newline key, and
/// `Shift+Tab mode` before any mode is reported, keep their wording whole. The
/// mode itself is the badge's, which gives way to nothing.
#[test]
fn the_bar_keeps_the_key_that_opens_a_line_at_eighty_columns() {
    let mut unannounced = empty_session();
    let row = bar_row(&screen(&mut unannounced, 80, 24));
    assert!(row.contains("Shift+Tab mode"), "{row}");
    assert!(row.contains("Ctrl+J newline"), "{row}");
    assert!(row.contains("Ask for a change"), "{row}");

    for mode in [Mode::Ask, Mode::Auto, Mode::Plan] {
        let mut app = running_session();
        app.apply(&Event::ModeSelected { mode });
        let row = bar_row(&screen(&mut app, 80, 24));
        assert!(row.contains(&format!(" {mode}  > ")), "{row}");
        assert!(row.contains("Ctrl+J newline"), "{row}");
        assert!(
            row.contains("Ask for a change · / search transcript · @ file"),
            "with the mode in the badge, the placeholder has its room back:\n{row}"
        );
    }
}

#[test]
fn a_terminal_that_reports_shift_enter_holds_no_newline_key_at_eighty_columns() {
    let mut app = running_session().reports_shift_enter();
    let row = bar_row(&screen(&mut app, 80, 24));
    assert!(
        row.contains("Ask for a change · / search transcript · @ file"),
        "Shift+Enter cannot be mistaken for Enter here, so the placeholder keeps its room:\n{row}"
    );
    assert!(row.contains(" ask  > "), "{row}");
    assert!(row.contains("Shift+Tab cycles"), "{row}");
    assert!(!row.contains("newline"), "{row}");
}

#[test]
fn typing_takes_room_from_the_hints_rather_than_writing_over_them() {
    let mut app = running_session();
    for c in "a prompt long enough to reach where the hints are drawn on the bar".chars() {
        app.type_into_composer(ratatui_textarea::Input {
            key: ratatui_textarea::Key::Char(c),
            ..Default::default()
        });
    }
    let row = bar_row(&screen(&mut app, 80, 24));
    assert!(row.contains("a prompt long enough"), "{row}");
    assert!(
        !row.contains("newline"),
        "the hints were drawn over what was typed:\n{row}"
    );
}

#[test]
fn the_shells_reply_is_said_in_the_bar_without_taking_a_row() {
    let mut app = running_session();
    let before = screen(&mut app, 120, 30);

    press(&mut app, ratatui::crossterm::event::KeyCode::F(3));
    let after = screen(&mut app, 120, 30);
    let row = bar_row(&after);
    assert!(row.contains("F3 Diff"), "{row}");
    assert_eq!(
        before.lines().position(|row| row.contains(" ask ")),
        after.lines().position(|row| row.contains(" ask ")),
        "the reply pushed the bar down a row:\n{after}"
    );
    assert_eq!(
        before.lines().filter(|row| row.contains("────")).count(),
        after.lines().filter(|row| row.contains("────")).count(),
    );

    press(&mut app, ratatui::crossterm::event::KeyCode::Char('x'));
    assert!(!bar_row(&screen(&mut app, 120, 30)).contains("F3 Diff"));
}

#[test]
fn the_composer_still_grows_to_a_third_of_the_pane() {
    let mut app = running_session();
    for _ in 0..20 {
        app.type_into_composer(ratatui_textarea::Input {
            key: ratatui_textarea::Key::Char('x'),
            ..Default::default()
        });
        app.on_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Enter,
            ratatui::crossterm::event::KeyModifiers::ALT,
        ));
    }
    let frame = screen(&mut app, 80, 24);
    let rows: Vec<&str> = frame.lines().collect();
    let bar = rows
        .iter()
        .position(|row| row.contains(" ask "))
        .expect("the bar is drawn");
    let bottom = rows
        .iter()
        .rposition(|row| row.starts_with(FOCUS_BOTTOM_LEFT))
        .expect("the pane is closed");
    // The pane's inner rows, less the one border and padding row above.
    let inner = bottom - 2;
    assert_eq!(bottom - bar, inner / 3, "{frame}");
}

#[test]
fn a_line_opened_after_the_last_one_grows_the_composer_rather_than_hiding_it() {
    let mut app = running_session();
    app.type_into_composer(ratatui_textarea::Input {
        key: ratatui_textarea::Key::Char('a'),
        ..Default::default()
    });
    let before = screen(&mut app, 120, 30);
    app.on_key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::Char('j'),
        ratatui::crossterm::event::KeyModifiers::CONTROL,
    ));
    let after = screen(&mut app, 120, 30);

    let bar = |frame: &str| {
        frame
            .lines()
            .position(|row| row.contains(" ask "))
            .expect("the bar is drawn")
    };
    assert!(
        after
            .lines()
            .nth(bar(&after))
            .is_some_and(|row| row.contains("> a")),
        "the line typed before the new one scrolled out of the composer:\n{after}"
    );
    assert_eq!(bar(&after) + 1, bar(&before), "{after}");
}

#[test]
fn a_reply_too_long_for_the_bar_wraps_in_it_rather_than_being_cut() {
    let mut app = running_session();
    press(&mut app, ratatui::crossterm::event::KeyCode::F(1));
    let frame = screen(&mut app, 80, 24);
    let rows: Vec<&str> = frame.lines().collect();
    let bar = rows
        .iter()
        .position(|row| row.contains(" ask "))
        .expect("the bar is drawn");
    let said: String = rows[bar..]
        .iter()
        .take_while(|row| !row.starts_with(FOCUS_BOTTOM_LEFT))
        .map(|row| row.trim_matches(|c| c == FOCUS_SIDE || c == ' ').to_owned())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        said.ends_with("Shift- or Option-drag selects text"),
        "the reply was cut:\n{frame}"
    );
    assert!(!said.contains("Ask for a change"), "{frame}");
}

/// A run of calls to one tool is one group: its row carries the run's summed
/// figures, and Ctrl+O folds every group to that row and opens them again.
#[test]
fn a_run_of_calls_folds_to_its_group_row_and_opens_again() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = running_session();
    let open = screen(&mut app, 200, 60);
    assert!(open.contains("▾ Read ×3"), "{open}");
    assert!(open.contains("15.3 kB · 3.6s"), "the run's sums: {open}");
    assert!(open.contains("├ catalog/fetch.ts"), "{open}");

    app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    let folded = screen(&mut app, 200, 60);
    assert!(folded.contains("▸ Read ×3"), "{folded}");
    assert!(folded.contains("15.3 kB · 3.6s"), "{folded}");
    assert!(!folded.contains("├ catalog/fetch.ts"), "{folded}");

    app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    assert_eq!(screen(&mut app, 200, 60), open);
}

/// On a pane too narrow for everything, what the call does gives way and
/// what it cost does not.
#[test]
fn a_narrow_pane_cuts_what_a_call_does_before_what_it_cost() {
    let frame = screen(&mut running_session(), 80, 24);
    let row = frame
        .lines()
        .find(|line| line.contains("✗ Bash"))
        .expect("the failed command is in view at the tail");
    assert!(row.contains("exit 1 · 0.4s"), "{row}");
}

/// The row a pane's title is on, and whether its border there is the double
/// line the pane with the keyboard is drawn in.
fn title_row<'a>(frame: &'a str, title: &str) -> &'a str {
    frame
        .lines()
        .find(|row| row.contains(&format!(" {title} ")))
        .unwrap_or_default()
}

/// Exactly one pane has the keyboard, and it says so three ways: the theme's
/// focus line where the others are single, its own colour on that border, and
/// an inverted title. The border is the one a monochrome terminal keeps.
#[test]
fn the_pane_with_the_keyboard_is_the_one_drawn_in_the_focus_line() {
    use ratatui::crossterm::event::KeyCode;
    use ratatui::style::Modifier;

    let mut app = running_session();
    let _ = screen(&mut app, 120, 30);
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    let frame = screen(&mut app, 120, 30);

    assert_snapshot("activity-focused-120x30", &frame);
    let focused: Vec<&str> = ["add etag support", "Usage", "Changes", "Activity"]
        .into_iter()
        .filter(|title| title_row(&frame, title).contains(&format!("{FOCUS_EDGE} ")))
        .collect();
    assert_eq!(focused, ["Activity"], "{frame}");

    let title = style_at(&mut app, 120, 30, " Activity ").expect("the title is drawn");
    assert!(
        title.add_modifier.contains(Modifier::REVERSED),
        "the focused title is not inverted: {title:?}"
    );
    let other = style_at(&mut app, 120, 30, " Changes ").expect("the title is drawn");
    assert!(
        !other.add_modifier.contains(Modifier::REVERSED),
        "{other:?}"
    );

    // The section cursor is on the pane's first header, drawn the same way,
    // and nowhere in the pane that does not have the keyboard.
    let cursor = style_at(&mut app, 120, 30, "▾ Sub-agents").expect("the header is drawn");
    assert!(
        cursor.add_modifier.contains(Modifier::REVERSED),
        "{cursor:?}"
    );
    let header = style_at(&mut app, 120, 30, "▾ Working tree").expect("the header is drawn");
    assert!(
        !header.add_modifier.contains(Modifier::REVERSED),
        "{header:?}"
    );
}

/// The focused pane's border is drawn in the theme's focused colour, and the
/// panes without the keyboard in the plain frame colour.
#[test]
fn the_focused_border_is_in_the_colour_the_theme_keeps_for_it() {
    let mut app = running_session().with_theme(CLASSIC);
    let session = style_at(&mut app, 120, 30, "╔").expect("the session pane is focused");
    let usage = style_at(&mut app, 120, 30, "┌").expect("the other panes are single");

    assert_eq!(session.fg, Some(CLASSIC.frame_focus));
    assert_eq!(usage.fg, Some(CLASSIC.frame));
}

/// The arrows walk the focused pane's section headers and Enter folds the one
/// under the cursor, leaving the transcript and the composer alone.
#[test]
fn a_focused_pane_folds_the_section_under_its_cursor() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = running_session();
    let _ = screen(&mut app, 200, 60);
    press(&mut app, KeyCode::Tab);
    let _ = screen(&mut app, 200, 60);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    let frame = screen(&mut app, 200, 60);

    // The cursor scrolled the pane down to the commits, below the files, and
    // folding them keeps their header in view.
    assert!(frame.contains("▸ Commits"), "{frame}");
    assert!(
        !app.folded(Section::WorkingTree),
        "one section, the one under the cursor"
    );
    assert_eq!(app.composed(), "", "Enter folded rather than typed");
}

/// Below a hundred columns there is no pane beside the session, so Tab has
/// nowhere to go and the bar does not offer it.
#[test]
fn a_session_alone_on_screen_keeps_the_keyboard() {
    use niobe_tui::app::Focus;
    use ratatui::crossterm::event::KeyCode;

    let mut app = running_session();
    let frame = screen(&mut app, 80, 24);
    press(&mut app, KeyCode::Tab);

    assert_eq!(app.focus(), Focus::Session);
    assert!(!frame.contains("Tab panes"), "{frame}");
    let wide = screen(&mut app, 200, 60);
    assert!(wide.contains("Tab panes"), "{wide}");

    // A question takes Tab for writing its answer, so while it holds the
    // keyboard the bar does not offer Tab for anything else.
    let asking = screen(&mut session_waiting_on_a_prompt(), 200, 60);
    assert!(!asking.contains("Tab panes"), "{asking}");
}

/// Scrolled back, the transcript offers the way down as a button at its
/// bottom right; a click on it returns to the newest line, and the button
/// goes with the need for it.
#[test]
fn the_way_back_down_is_a_button_that_returns_to_the_newest_line() {
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    let mut app = running_session();
    let tail = screen(&mut app, 120, 30);
    app.scroll_to_head();
    let head = screen(&mut app, 120, 30);

    let (row, line) = head
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("Jump to bottom ↓"))
        .expect("scrolled back, the button is drawn");
    assert!(
        !head.contains("scrolled back"),
        "the old hint is gone, not kept beside the button:\n{head}"
    );
    let column = line
        .chars()
        .position(|c| c == '↓')
        .expect("the button has its arrow");

    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: u16::try_from(column).expect("the screen is 120 wide"),
        row: u16::try_from(row).expect("the screen is 30 high"),
        modifiers: KeyModifiers::NONE,
    });

    assert!(app.follows_tail());
    assert_eq!(screen(&mut app, 120, 30), tail);
}

/// A transcript that fits its pane has nothing to scroll back to, and a key
/// that would scroll it does not leave it claiming it has.
#[test]
fn a_transcript_too_short_to_scroll_never_offers_the_way_back_down() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = empty_session();
    let _ = screen(&mut app, 120, 30);
    press(&mut app, KeyCode::PageUp);
    let frame = screen(&mut app, 120, 30);

    assert!(!frame.contains("Jump to bottom"), "{frame}");
    assert!(
        !frame.contains('█'),
        "no scrollbar where nothing scrolls:\n{frame}"
    );
}

/// A session long enough to scroll, with a word in two of its replies: one
/// far above the newest line and one a screen or so above it.
fn session_with_a_needle() -> App {
    let mut app = empty_session();
    for n in 0..40 {
        let text = match n {
            3 => "the needle in reply three".to_owned(),
            30 => "the needle in reply thirty".to_owned(),
            n => format!("reply {n}, with nothing in it worth finding"),
        };
        app.apply(&Event::UserMessage {
            text: format!("prompt {n}"),
        });
        app.apply(&Event::AssistantMessage { text });
        app.apply(&Event::TurnEnded);
    }
    app
}

/// The row the bar is drawn on while it searches.
fn find_row(frame: &str) -> String {
    frame
        .lines()
        .find(|row| row.contains(" find ") && row.contains(" / "))
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("no search in the bar:\n{frame}"))
}

fn type_keys(app: &mut App, typed: &str) {
    for c in typed.chars() {
        press(app, ratatui::crossterm::event::KeyCode::Char(c));
    }
}

#[test]
fn slash_searches_the_transcript_steps_between_matches_and_esc_puts_the_view_back() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = session_with_a_needle();
    let tail = screen(&mut app, 120, 30);
    assert!(
        !tail.contains("reply three"),
        "the test needs it off screen"
    );

    press(&mut app, KeyCode::Char('/'));
    let row = find_row(&screen(&mut app, 120, 30));
    assert!(row.contains("Find in the transcript"), "{row}");
    assert!(app.composed().is_empty(), "the slash went into the prompt");

    type_keys(&mut app, "needle");
    let frame = screen(&mut app, 120, 30);
    let row = find_row(&frame);
    assert!(
        row.contains("2 of 2"),
        "the search starts on the match nearest where the view was:\n{frame}"
    );
    assert!(frame.contains("needle in reply thirty"), "{frame}");
    let theme = app.theme().to_owned();
    let mark = style_at(&mut app, 120, 30, "needle").expect("the match is on screen");
    assert_eq!(
        (mark.fg, mark.bg),
        (Some(theme.pane_bg), Some(theme.hot)),
        "the match stepped to is marked as a chip"
    );

    press(&mut app, KeyCode::Up);
    let frame = screen(&mut app, 120, 30);
    assert!(find_row(&frame).contains("1 of 2"), "{frame}");
    assert!(
        frame.contains("needle in reply three"),
        "stepping up did not bring the older match into view:\n{frame}"
    );

    press(&mut app, KeyCode::Up);
    assert!(
        find_row(&screen(&mut app, 120, 30)).contains("2 of 2"),
        "stepping past the first match goes round to the last"
    );

    press(&mut app, KeyCode::Esc);
    assert!(app.finding().is_none());
    assert!(app.follows_tail(), "Esc left the view where the match was");
    assert_eq!(screen(&mut app, 120, 30), tail);
}

#[test]
fn every_match_on_screen_is_marked_and_only_the_current_one_as_a_chip() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = session_with_a_needle();
    press(&mut app, KeyCode::Char('/'));
    type_keys(&mut app, "worth");
    let frame = screen(&mut app, 120, 30);
    let theme = app.theme().to_owned();
    let marked: Vec<Style> = styles(&mut app, 120, 30)
        .into_iter()
        .filter(|style| style.bg == Some(theme.hot) && style.fg == Some(theme.pane_bg))
        .collect();
    assert!(!marked.is_empty(), "{frame}");
    let underlined = styles(&mut app, 120, 30).into_iter().any(|style| {
        style
            .add_modifier
            .contains(ratatui::style::Modifier::UNDERLINED)
    });
    assert!(
        underlined,
        "the matches other than the current one are not marked:\n{frame}"
    );
}

#[test]
fn a_query_found_nowhere_says_so() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = session_with_a_needle();
    let tail = screen(&mut app, 120, 30);
    press(&mut app, KeyCode::Char('/'));
    type_keys(&mut app, "haystack");
    let frame = screen(&mut app, 120, 30);
    assert!(find_row(&frame).contains("no match"), "{frame}");
    press(&mut app, KeyCode::Esc);
    assert_eq!(screen(&mut app, 120, 30), tail);
}

#[test]
fn a_slash_anywhere_but_the_start_of_a_prompt_is_a_slash() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = session_with_a_needle();
    type_keys(&mut app, "a/b");
    assert!(app.finding().is_none());
    assert_eq!(app.composed(), "a/b");

    let mut app = session_with_a_needle();
    type_keys(&mut app, "//etc");
    assert!(app.finding().is_none(), "a second slash leaves the search");
    assert_eq!(app.composed(), "/etc", "and starts the prompt with one");

    let mut app = session_with_a_needle();
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Backspace);
    assert!(
        app.finding().is_none(),
        "backspace on an empty search leaves it"
    );
    assert!(app.composed().is_empty());
}

/// A session in a repository the binary has listed the files of.
fn session_with_files() -> App {
    let mut app = empty_session();
    app.set_repo(Repo {
        name: "niobe".to_owned(),
        branch: Some("main".to_owned()),
        read: true,
        files: [
            "README.md",
            "catalog/fetch.ts",
            "catalog/etag.ts",
            "catalog/cache/lru.ts",
            "tests/fetch.test.ts",
        ]
        .map(str::to_owned)
        .to_vec(),
        ..Repo::default()
    });
    app
}

/// The rows of the list of files standing over the transcript, each trimmed
/// to what is inside its border.
fn file_list(frame: &str) -> Vec<String> {
    let rows: Vec<&str> = frame.lines().collect();
    let Some(top) = rows.iter().position(|row| row.contains(" @ file ")) else {
        return Vec::new();
    };
    rows[top + 1..]
        .iter()
        .take_while(|row| !row.contains('└'))
        .map(|row| {
            let inside = row.split('│').nth(1).unwrap_or_default();
            inside.trim().to_owned()
        })
        .collect()
}

#[test]
fn at_the_start_of_a_word_offers_the_files_the_repository_listed_and_enter_names_one() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = session_with_files();
    assert!(
        bar_row(&screen(&mut app, 200, 60)).contains("@ file"),
        "a session with files to name says it can name them"
    );

    type_keys(&mut app, "keep the cache in @fetch");
    let frame = screen(&mut app, 120, 30);
    assert_eq!(
        file_list(&frame),
        ["catalog/fetch.ts", "tests/fetch.test.ts"],
        "{frame}"
    );
    let theme = app.theme().to_owned();
    let chosen = style_at(&mut app, 120, 30, " catalog/fetch.ts").expect("the list is drawn");
    assert_eq!(
        (chosen.fg, chosen.bg),
        (Some(theme.pane_bg), Some(theme.hot)),
        "the file Enter would take is marked as a chip"
    );

    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.composed(), "keep the cache in @tests/fetch.test.ts ");
    assert!(
        app.take_produced().is_empty(),
        "the Enter that named a file sent the prompt"
    );
    assert!(file_list(&screen(&mut app, 120, 30)).is_empty());

    type_keys(&mut app, "and @lru");
    press(&mut app, KeyCode::Tab);
    assert_eq!(
        app.composed(),
        "keep the cache in @tests/fetch.test.ts and @catalog/cache/lru.ts "
    );
}

#[test]
fn an_at_inside_a_word_is_an_at_and_esc_leaves_the_word_as_typed() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = session_with_files();
    type_keys(&mut app, "mail ops@etag");
    assert!(
        file_list(&screen(&mut app, 120, 30)).is_empty(),
        "an address opened the list of files"
    );

    let mut app = session_with_files();
    type_keys(&mut app, "@cat");
    assert!(!file_list(&screen(&mut app, 120, 30)).is_empty());
    press(&mut app, KeyCode::Esc);
    type_keys(&mut app, "s");
    assert!(
        file_list(&screen(&mut app, 120, 30)).is_empty(),
        "the list came back for the word it was closed on"
    );
    assert_eq!(app.composed(), "@cats");

    press(&mut app, KeyCode::Enter);
    assert!(
        !app.take_produced().is_empty(),
        "with the list closed, Enter sends"
    );
}

#[test]
fn a_session_with_no_files_to_name_does_not_offer_to_name_one() {
    let mut app = empty_session();
    let row = bar_row(&screen(&mut app, 120, 30));
    assert!(!row.contains("@ file"), "{row}");

    type_keys(&mut app, "@");
    assert!(file_list(&screen(&mut app, 120, 30)).is_empty());
}

/// A session that runs the operator's commands, in a repository with files.
fn session_that_runs_commands() -> App {
    session_with_files().runs_commands()
}

/// The row the bar is drawn on while it holds a command.
fn shell_row(frame: &str) -> String {
    frame
        .lines()
        .find(|row| row.contains(" shell ") && row.contains(" $ "))
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("no command in the bar:\n{frame}"))
}

/// Types `command` after `!` and runs it; returns the call it was recorded
/// as, which is what the loop hands the shell.
fn run_command(app: &mut App, command: &str) -> niobe_core::event::ToolCallId {
    use ratatui::crossterm::event::KeyCode;

    press(app, KeyCode::Char('!'));
    type_keys(app, command);
    press(app, KeyCode::Enter);
    let mut commands = app.take_commands();
    assert_eq!(commands.len(), 1, "one command was run");
    let (id, ran) = commands.remove(0);
    assert_eq!(ran, command);
    id
}

#[test]
fn bang_runs_a_command_that_is_recorded_as_a_call_and_shows_what_it_printed() {
    use niobe_core::event::ToolOutcome;
    use ratatui::crossterm::event::KeyCode;

    let mut app = session_that_runs_commands();
    assert!(bar_row(&screen(&mut app, 200, 60)).contains("! shell"));

    press(&mut app, KeyCode::Char('!'));
    let row = shell_row(&screen(&mut app, 120, 30));
    assert!(
        row.contains("the agent does not see what it prints"),
        "the bar does not say where the output goes:\n{row}"
    );
    let wide = shell_row(&screen(&mut app, 200, 60));
    assert!(wide.contains("Enter runs · Esc back"), "{wide}");

    type_keys(&mut app, "git status --short");
    press(&mut app, KeyCode::Enter);
    let commands = app.take_commands();
    let [(id, command)] = commands.as_slice() else {
        panic!("one command is handed out to be run: {commands:?}");
    };
    assert_eq!(command, "git status --short");
    assert!(app.composed().is_empty());
    assert!(!app.shell_mode(), "one `!` runs one command");

    let produced = app.take_produced();
    assert!(
        !produced
            .iter()
            .any(|event| matches!(event, Event::UserMessage { .. })),
        "the command was sent to the agent as a prompt: {produced:?}"
    );
    assert!(
        matches!(
            produced.as_slice(),
            [Event::ToolCallStart { id: started, name, input, .. }]
                if started == id && name == niobe_tui::OPERATOR_SHELL && input == "git status --short"
        ),
        "the command is not recorded as the start of a call: {produced:?}"
    );

    app.ran(niobe_tui::Ran {
        id: id.clone(),
        output: " M catalog/fetch.ts\n?? notes.md\n".to_owned(),
        bytes: 32,
        whole: true,
        exit_code: Some(0),
        error: None,
    });
    let produced = app.take_produced();
    assert!(
        matches!(
            produced.as_slice(),
            [Event::ToolCallEnd { id: ended, name, outcome: ToolOutcome::Ok, exit_code: Some(0), .. }]
                if ended == id && name == niobe_tui::OPERATOR_SHELL
        ),
        "the end is not recorded as the end of the call: {produced:?}"
    );

    let frame = screen(&mut app, 120, 30);
    let at = frame
        .lines()
        .position(|row| row.contains("! shell") && row.contains("git status --short"))
        .unwrap_or_else(|| panic!("the call is not in the transcript:\n{frame}"));
    let under: Vec<&str> = frame.lines().skip(at + 1).take(2).collect();
    assert!(under[0].contains("M catalog/fetch.ts"), "{frame}");
    assert!(under[1].contains("?? notes.md"), "{frame}");
}

#[test]
fn a_command_that_failed_shows_its_status_and_what_it_printed_rather_than_a_made_up_reason() {
    let mut app = session_that_runs_commands();
    let id = run_command(&mut app, "ls nowhere");
    app.ran(niobe_tui::Ran {
        id,
        output: "ls: nowhere: No such file or directory\n".to_owned(),
        bytes: 39,
        whole: true,
        exit_code: Some(1),
        error: None,
    });

    let frame = screen(&mut app, 120, 30);
    let row = frame
        .lines()
        .find(|row| row.contains("ls nowhere"))
        .unwrap_or_else(|| panic!("the call is not in the transcript:\n{frame}"));
    assert!(row.contains("exit 1"), "{row}");
    assert!(frame.contains("No such file or directory"), "{frame}");
    assert!(!frame.contains("backend said nothing"), "{frame}");
}

#[test]
fn a_command_that_could_not_start_is_a_failed_call_that_says_why() {
    let mut app = session_that_runs_commands();
    let id = run_command(&mut app, "make");
    app.not_run(&id, "cannot start sh: not found");

    let frame = screen(&mut app, 120, 30);
    assert!(
        frame.contains("not run: cannot start sh: not found"),
        "{frame}"
    );
}

#[test]
fn a_cargo_test_run_with_bang_is_read_for_its_counts() {
    let mut app = session_that_runs_commands();
    let id = run_command(&mut app, "cargo test");
    app.take_produced();
    let output = "    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.01s
     Running unittests src/lib.rs (target/debug/deps/demo-760e00b68511d171)

running 2 tests
test tests::adds ... ok
test tests::wrong ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

";
    app.ran(niobe_tui::Ran {
        id: id.clone(),
        output: output.to_owned(),
        bytes: u64::try_from(output.len()).expect("the output is short"),
        whole: true,
        exit_code: Some(0),
        error: None,
    });

    let produced = app.take_produced();
    assert!(
        produced.iter().any(|event| matches!(
            event,
            Event::TestRun { id: run, counts: Some(counts), failed: false, .. }
                if *run == id && counts.passed == 2
        )),
        "the run is not read: {produced:?}"
    );
}

#[test]
fn a_failing_cargo_test_run_with_bang_names_what_its_binary_listed_failing() {
    let mut app = session_that_runs_commands();
    let id = run_command(&mut app, "cargo test");
    app.take_produced();
    let output = "    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.01s
     Running unittests src/lib.rs (target/debug/deps/demo-760e00b68511d171)

running 2 tests
test tests::adds ... ok
test tests::wrong ... FAILED

failures:

failures:
    tests::wrong

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `--lib`
";
    app.ran(niobe_tui::Ran {
        id: id.clone(),
        output: output.to_owned(),
        bytes: u64::try_from(output.len()).expect("the output is short"),
        whole: true,
        exit_code: Some(101),
        error: None,
    });

    let named = FailedTests {
        binary: "--lib".to_owned(),
        tests: vec!["tests::wrong".to_owned()],
    };
    let produced = app.take_produced();
    assert!(
        produced.iter().any(|event| matches!(
            event,
            Event::TestRun { id: run, failed: true, failures, .. }
                if *run == id && *failures == [named.clone()]
        )),
        "the failures are not named: {produced:?}"
    );
}

#[test]
fn a_bang_anywhere_but_the_start_of_a_prompt_is_a_bang() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = session_that_runs_commands();
    type_keys(&mut app, "ship it!");
    assert!(!app.shell_mode());
    assert_eq!(app.composed(), "ship it!");

    let mut app = session_that_runs_commands();
    type_keys(&mut app, "!!important");
    assert!(!app.shell_mode(), "a second `!` leaves the command");
    assert_eq!(
        app.composed(),
        "!important",
        "and starts the prompt with one"
    );

    let mut app = session_that_runs_commands();
    press(&mut app, KeyCode::Char('!'));
    press(&mut app, KeyCode::Backspace);
    assert!(!app.shell_mode(), "backspace on an empty command leaves it");

    let mut app = session_with_files();
    let row = bar_row(&screen(&mut app, 200, 60));
    assert!(
        !row.contains("! shell"),
        "a session with nothing to run commands offers to run them:\n{row}"
    );
    type_keys(&mut app, "!x");
    assert!(!app.shell_mode());
    assert_eq!(app.composed(), "!x");
}

fn stop_key(app: &mut App) {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    app.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
}

#[test]
fn ctrl_g_stops_the_newest_command_still_running_and_leaves_the_session_open() {
    let mut app = session_that_runs_commands();
    let older = run_command(&mut app, "npm run dev");
    let newer = run_command(&mut app, "yes");
    assert_eq!(
        app.hint(),
        Some("Ctrl+G stops it"),
        "the key is named as it runs"
    );

    stop_key(&mut app);
    assert_eq!(app.take_stops(), [newer], "the newest one is stopped");
    assert!(!app.should_quit(), "stopping a command is not quitting");

    // Asked again before the first has ended, it moves on to the next one
    // rather than asking the same command twice.
    stop_key(&mut app);
    assert_eq!(app.take_stops(), [older]);

    stop_key(&mut app);
    assert!(
        app.take_stops().is_empty(),
        "both are already being stopped"
    );
    assert!(!app.should_quit());
}

#[test]
fn a_command_the_operator_stopped_ends_as_stopped_with_what_it_printed_up_to_then() {
    use niobe_core::event::ToolOutcome;

    let mut app = session_that_runs_commands();
    let id = run_command(&mut app, "yes");
    app.take_produced();
    stop_key(&mut app);
    app.take_stops();

    app.ran(niobe_tui::Ran {
        id: id.clone(),
        output: "y\ny\ny\n".to_owned(),
        bytes: 6,
        whole: true,
        exit_code: None,
        error: Some("ended by signal 15".to_owned()),
    });

    let produced = app.take_produced();
    assert!(
        matches!(
            produced.as_slice(),
            [Event::ToolCallEnd { id: ended, outcome: ToolOutcome::Failed, exit_code: None, error: Some(error), output, .. }]
                if *ended == id && error == "stopped by the operator" && output == "y\ny\ny\n"
        ),
        "the end does not say the operator stopped it: {produced:?}"
    );
    let frame = screen(&mut app, 120, 30);
    assert!(frame.contains("stopped by the operator"), "{frame}");
}

#[test]
fn a_stopped_command_whose_sh_exited_0_is_still_stopped_and_keeps_the_status() {
    use niobe_core::event::ToolOutcome;

    let mut app = session_that_runs_commands();
    let id = run_command(&mut app, "sleep 30 & wait");
    app.take_produced();
    stop_key(&mut app);
    app.take_stops();

    app.ran(niobe_tui::Ran {
        id,
        output: "sh: line 1: 43400 Terminated: 15 sleep 30\n".to_owned(),
        bytes: 42,
        whole: true,
        exit_code: Some(0),
        error: None,
    });

    let produced = app.take_produced();
    assert!(
        matches!(
            produced.as_slice(),
            [Event::ToolCallEnd { outcome: ToolOutcome::Failed, exit_code: Some(0), error: Some(error), .. }]
                if error == "stopped by the operator"
        ),
        "a stopped command read as one that succeeded: {produced:?}"
    );
}

#[test]
fn ctrl_g_with_no_command_running_says_so() {
    let mut app = session_that_runs_commands();

    stop_key(&mut app);

    assert!(app.take_stops().is_empty());
    assert_eq!(app.hint(), Some("No ! command is running"));
    assert!(!app.should_quit());
}
