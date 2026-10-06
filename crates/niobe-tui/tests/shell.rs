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
    session_with_a_markdown_reply, session_with_a_tab_indented_reply, session_with_finished_turns,
    session_with_test_records, session_with_two_agents_at_work, style_at, styles,
    unmetered_session,
};
use niobe_core::event::{
    AgentId, Backend, Event, Mode, PermissionDecision, Usage, UsageWindow, UsageWindows,
};
use niobe_core::{FailedTests, TestCounts, TestRunRecord};
use niobe_tui::app::{App, Pane, Repo, Section, SelectedProfile};
use niobe_tui::theme::{CLASSIC, CYBER, Depth, MODERN, NEO, THEMES, Theme};
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
        agent: None,
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

    if niobe_tui::snapshots::updating().unwrap_or_else(|refused| panic!("{refused}")) {
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
    let without = screen(&mut metered_session(), 120, 40);
    assert!(
        without.contains("≥$0.04"),
        "with nothing to value the rest, the figure is a floor:\n{without}"
    );

    let mut priced = metered_session().with_prices(Box::new(ATenthOfACentPerThousand));
    let totals = priced.session().totals().clone();
    let owed: u64 = totals
        .unsettled
        .values()
        .map(|owed| owed.total().tokens())
        .sum();
    let expected = totals.reported_cost_usd + owed as f64 / 1_000.0 * 0.001;

    let frame = screen(&mut priced, 120, 40);
    // 30,420 tokens owed for, at a tenth of a cent per thousand, is $0.03042
    // on top of the $0.04 the recording reported: $0.07042, to the cent.
    assert_eq!(owed, 30_420);
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

/// Two sub-agents of one kind at work at once: each of their calls is named
/// by the agent's tag, numbered because the two are of one kind, two agents'
/// calls to one tool stay two rows, and what an agent says is drawn under its
/// own tag rather than as the session's.
#[test]
fn whose_each_row_is_shows_where_two_agents_calls_interleave() {
    let frame = screen(&mut session_with_two_agents_at_work(), 120, 30);
    for row in [
        "deep1 catalog/fetch.py",
        "deep2 catalog/cache.py",
        "deep1 catalog/etag.py",
        "↳ deep2",
    ] {
        assert!(frame.contains(row), "{row} is not on screen:\n{frame}");
    }
    assert_snapshot("agents-120x30", &frame);
}

/// Grouped by agent from the Activity pane, each agent's rows read top to
/// bottom under a heading with its tag and task, and the bar says the key
/// that ungroups them.
#[test]
fn grouped_by_agent_each_agents_rows_stand_under_its_heading() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = session_with_two_agents_at_work();
    screen(&mut app, 120, 30);
    let press = |app: &mut App, code| app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    // The bar has room for the pane's keys at full width.
    let focused = screen(&mut app, 200, 60);
    assert!(focused.contains("a groups by agent"), "{focused}");

    press(&mut app, KeyCode::Char('a'));
    assert!(screen(&mut app, 200, 60).contains("a ungroups"));
    let frame = screen(&mut app, 120, 30);
    for row in ["▾ deep1", "▾ deep2"] {
        assert!(frame.contains(row), "{row} is not on screen:\n{frame}");
    }
    let heading = frame
        .lines()
        .position(|line| line.contains("▾ deep1"))
        .expect("the heading is drawn");
    let rows: Vec<&str> = frame.lines().skip(heading + 1).take(2).collect();
    assert!(
        rows[0].contains("catalog/fetch.py") && rows[1].contains("catalog/etag.py"),
        "the first agent's rows are not together under it:\n{frame}"
    );
    assert_snapshot("agents-grouped-120x30", &frame);
}

/// A reviewer's call refused and the other reviewer's waiting: the refusal
/// and the question each say which agent asked, by the tag its calls' rows
/// give it, rather than reading as the session's own.
#[test]
fn a_sub_agents_question_and_its_refusal_say_which_agent_asked() {
    let mut app = session_with_two_agents_at_work();
    let asked = |id: &str, command: &str, agent: &str| Event::PermissionRequest {
        id: id.into(),
        tool: "Bash".to_owned(),
        input: format!(r#"{{"command":"{command}"}}"#),
        target: Some(command.to_owned()),
        agent: Some(AgentId::new(agent)),
    };
    app.apply(&asked("f3", "curl -sI localhost:8080", "toolu_fetch"));
    app.apply(&Event::PermissionResponse {
        id: "f3".into(),
        decision: PermissionDecision::Deny,
        message: None,
    });
    app.apply(&asked("c2", "python3 probe.py", "toolu_cache"));

    let frame = screen(&mut app, 120, 30);
    for row in [
        "! denied  deep1 › Bash · curl -sI localhost:8080",
        "? deep2 asks",
    ] {
        assert!(frame.contains(row), "{row} is not on screen:\n{frame}");
    }
    assert!(!frame.contains("? claude asks"), "{frame}");
    assert_snapshot("agents-asking-120x30", &frame);
}

/// A refused command of several lines, drawn on its one row, keeps its
/// lines apart: `set -e` and `cd /tmp` joined would read as another command.
#[test]
fn a_denied_command_of_several_lines_keeps_its_lines_apart_on_its_row() {
    let mut app = running_session();
    let command = "set -e\ncd /tmp\necho step 0";
    app.apply(&Event::PermissionRequest {
        id: "multi".into(),
        tool: "Bash".to_owned(),
        input: r#"{"command":"set -e\ncd /tmp\necho step 0"}"#.to_owned(),
        target: Some(command.to_owned()),
        agent: None,
    });
    app.apply(&Event::PermissionResponse {
        id: "multi".into(),
        decision: PermissionDecision::Deny,
        message: None,
    });

    let frame = screen(&mut app, 120, 30);
    assert!(
        frame.contains("! denied  Bash · set -e ↵ cd /tmp ↵ echo step 0"),
        "{frame}"
    );
    assert_snapshot("denied-multiline-120x30", &frame);
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
const TRANSCRIPT_TOP: usize = 2;

/// The long write, opened, with a reply under it long enough to scroll back
/// through without the diff coming into view.
fn long_write_under_a_long_reply() -> App {
    let mut app = session_with_a_long_write();
    let reply: String = (1..=60).map(|n| format!("- reply line {n:02}\n")).collect();
    app.apply(&Event::AssistantMessage {
        text: reply,
        agent: None,
    });
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

/// A terminal draws no tab, so code indented with tabs, as Go and Makefiles
/// are, would lose every level of its nesting: each tab is taken to the next
/// stop four columns on, the way the diff under a call draws one.
#[test]
fn tab_indented_code_in_a_reply_keeps_its_nesting() {
    let frame = screen(&mut session_with_a_tab_indented_reply(), 80, 24);
    for line in [
        "│ func handle(ok bool) {",
        "│     if ok {",
        "│         return",
        "│     }",
        "│     ID  string",
        "│     Body    []byte",
    ] {
        assert!(frame.contains(line), "{line:?} is not drawn:\n{frame}");
    }
    assert_snapshot("markdown-tabs-80x24", &frame);
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

/// A reset the backend timed past what the clock can hold is a window with
/// no reset to name, not a shell that panics drawing it.
#[test]
fn a_window_resetting_past_what_the_clock_holds_is_drawn_without_its_reset() {
    let mut app = running_session();
    app.apply(&Event::UsageWindows(UsageWindows {
        five_hour: Some(UsageWindow {
            utilization: 0.5,
            resets_at: Some(u64::MAX),
        }),
        seven_day: None,
        using_overage: false,
    }));

    let frame = screen(&mut app, 200, 60);

    let row = frame
        .lines()
        .find(|row| row.contains("5h "))
        .expect("the five-hour window is drawn");
    assert!(row.contains("50%"), "{row}");
    assert!(!row.contains("resets"), "{row}");
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

/// The question stands off the transcript the way a dialog stands off the
/// panes: the two columns right of its frame, from its second row down, and
/// the row under it, from its third column on, are its shadow, in every
/// theme, drawn as a dialog's is: in the shadow's colours, over the
/// transcript's own characters, with no shading glyph laid over a blank.
#[test]
fn the_question_casts_a_shadow_on_the_transcript() {
    for theme in THEMES {
        let mut app = session_waiting_on_a_prompt()
            .with_depth(Depth::Sixteen)
            .with_theme(theme);
        let (width, height) = (80, 24);
        let frame = screen(&mut app, width, height);
        let cells = styles(&mut app, width, height);
        let rows: Vec<Vec<char>> = frame.lines().map(|row| row.chars().collect()).collect();
        let find = |corner: char| {
            rows.iter()
                .enumerate()
                .find_map(|(y, row)| row.iter().position(|&c| c == corner).map(|x| (x, y)))
        };
        let (left, top) = find('┌').expect("the question's top-left corner is on screen");
        let (right, bottom) = find('┘').expect("the question's bottom-right corner is on screen");

        let mut shadow = Vec::new();
        for y in top + 1..=bottom + 1 {
            shadow.extend([(right + 1, y), (right + 2, y)]);
        }
        shadow.extend((left + 2..=right).map(|x| (x, bottom + 1)));
        for (x, y) in shadow {
            let style = cells[y * usize::from(width) + x];
            assert_eq!(
                (style.bg, style.fg),
                (Some(theme.shadow), Some(theme.shadow_fg)),
                "{}: no shadow at column {x}, row {y}:\n{frame}",
                theme.name
            );
            assert_ne!(
                rows[y].get(x),
                Some(&'░'),
                "{}: the shadow hides what is under it at column {x}, row {y}:\n{frame}",
                theme.name
            );
        }
    }
}

/// A session waiting on a `Bash` call whose command is sixty-two lines long,
/// more than any pane these tests draw has rows for.
fn session_waiting_on_a_long_command() -> App {
    let mut app = running_session();
    let mut command = String::from("set -e\ncd /tmp\n");
    for step in 1..=59 {
        command.push_str(&format!("echo step {step} && \\\n"));
    }
    command.push_str("rm -rf ~/important");
    app.apply(&Event::PermissionRequest {
        id: "toolu_long_command".into(),
        tool: "Bash".to_owned(),
        input: format!(
            r#"{{"command":"{}"}}"#,
            command.replace('\\', "\\\\").replace('\n', "\\n")
        ),
        target: Some(command),
        agent: None,
    });
    app
}

/// What is asked, where the call starts and every answer are on screen for a
/// prompt taller than the pane, and the rows cut from it say so.
fn assert_a_tall_question_fits(frame: &str) {
    let rows = question_rows(frame);
    assert!(frame.contains("? claude asks"), "{frame}");
    assert!(rows.iter().any(|row| row == "to run Bash"), "{frame}");
    assert!(rows.iter().any(|row| row == "set -e"), "{frame}");
    for option in [
        "▶ 1. Allow once",
        "2. Always allow Bash",
        "3. Always allow this target",
        "4. Deny",
    ] {
        assert!(
            rows.iter().any(|row| row.starts_with(option)),
            "`{option}` is not on screen:\n{frame}"
        );
    }
    assert!(
        rows.iter()
            .any(|row| row.contains("more lines") && row.contains("Ctrl+T")),
        "nothing says the call was cut, or how to read it whole:\n{frame}"
    );
}

#[test]
fn a_question_taller_than_the_pane_keeps_what_is_asked_and_its_answers_on_screen() {
    let small = screen(&mut session_waiting_on_a_long_command(), 80, 24);
    assert_a_tall_question_fits(&small);
    assert_snapshot("asking-long-80x24", &small);

    let large = screen(&mut session_waiting_on_a_long_command(), 120, 40);
    assert_a_tall_question_fits(&large);
    assert_snapshot("asking-long-120x40", &large);
}

#[test]
fn enter_answers_nothing_while_the_top_of_the_question_is_not_drawn() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = session_waiting_on_a_long_command();
    app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    let frame = screen(&mut app, 80, 24);
    assert!(
        !frame.contains("to run Bash"),
        "the whole call fits:\n{frame}"
    );

    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(
        !app.take_produced()
            .iter()
            .any(|event| matches!(event, Event::PermissionResponse { .. })),
        "a call was answered with the question it asks off screen"
    );
    assert!(app.asking().is_some());
}

/// A key read on its own, at `at`.
fn alone(at: std::time::Instant) -> niobe_tui::app::Arrival {
    niobe_tui::app::Arrival { at, alone: true }
}

fn answered(app: &mut App) -> bool {
    app.take_produced()
        .iter()
        .any(|event| matches!(event, Event::PermissionResponse { .. }))
}

#[test]
fn a_question_on_a_window_too_small_to_draw_it_takes_no_answer_until_the_window_grows() {
    use niobe_tui::app::ASK_QUIET;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::time::{Duration, Instant};

    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    let shown = Instant::now();
    let mut app = session_waiting_on_a_prompt();
    app.tick(shown, None);
    let frame = screen(&mut app, 60, 20);
    assert!(!frame.contains("claude asks"), "{frame}");

    app.on_key_read(key(KeyCode::Down), alone(shown + ASK_QUIET * 2));
    app.on_key_read(key(KeyCode::Enter), alone(shown + ASK_QUIET * 4));
    app.on_key_read(key(KeyCode::Char('2')), alone(shown + ASK_QUIET * 6));

    assert!(!answered(&mut app), "a call was answered off screen");
    assert!(app.take_rules().is_empty());
    assert!(app.asking().is_some());
    let frame = screen(&mut app, 60, 20);
    assert!(
        frame.contains("Not taken as an answer: the window is too small"),
        "{frame}"
    );

    let grown = shown + Duration::from_secs(10);
    app.tick(grown, None);
    let frame = screen(&mut app, 80, 24);
    assert!(frame.contains("claude asks"), "{frame}");
    app.on_key_read(key(KeyCode::Enter), alone(grown + ASK_QUIET / 5));
    assert!(
        !answered(&mut app),
        "a key pressed as the question came into view answered it"
    );

    app.on_key_read(key(KeyCode::Enter), alone(grown + ASK_QUIET * 3));
    assert!(answered(&mut app), "the question on screen took no answer");
    assert!(app.asking().is_none());
}

/// The rows of the question box, with its borders and the pane's taken off.
fn question_rows(frame: &str) -> Vec<String> {
    frame
        .lines()
        .filter_map(|row| row.split('│').nth(1))
        .map(|row| row.trim().to_owned())
        .collect()
}

/// The rows of the question box with its borders and their one-cell margins
/// taken off, and nothing else: each is the frame's inner width, and its
/// spaces, leading or trailing, are its own.
fn question_cells(frame: &str) -> Vec<String> {
    frame
        .lines()
        .filter_map(|row| row.split('│').nth(1))
        .map(|row| {
            let row = row.strip_prefix(' ').unwrap_or(row);
            row.strip_suffix(' ').unwrap_or(row).to_owned()
        })
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
        agent: None,
    });
    let frame = screen(&mut app, 120, 30);
    let rows = question_rows(&frame);

    assert!(
        rows.iter()
            .any(|row| row.starts_with("2. Always allow Notion·search")),
        "the tool's option does not name it the way the timeline does:\n{frame}"
    );
    let rule: String = question_cells(&frame)
        .into_iter()
        .skip_while(|row| !row.starts_with("3. niobe saves"))
        .take_while(|row| !row.trim().is_empty())
        .collect();
    assert_eq!(
        rule.trim_end(),
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
        agent: None,
    });
    let frame = screen(&mut app, 200, 60);

    let call: String = question_cells(&frame)
        .into_iter()
        .take_while(|row| !row.starts_with("▶ 1."))
        .collect();
    let words = call.matches("word").count();
    // Forty in the command and forty again in the arguments, none cut off.
    assert_eq!(words, 80, "{frame}");
    assert!(frame.lines().all(|row| text_width(row) <= 200));
}

/// A session waiting on a `Bash` call whose command means what its spacing
/// says: an indented Python body, a tab, and a quoted string and a comment
/// with runs of spaces in them.
fn session_waiting_on_a_spaced_command() -> App {
    let mut app = running_session();
    let command = "python3 -c '\nclass H:\n    def log(self, a): pass\n\tprint(\"a    b\")\n' # three   spaces";
    app.apply(&Event::PermissionRequest {
        id: "toolu_spaced".into(),
        tool: "Bash".to_owned(),
        input: format!(
            r#"{{"command":"{}"}}"#,
            command
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\t', "\\t")
        ),
        target: Some(command.to_owned()),
        agent: None,
    });
    app
}

#[test]
fn the_question_draws_a_command_with_every_space_and_tab_it_has() {
    let frame = screen(&mut session_waiting_on_a_spaced_command(), 120, 40);
    let rows = question_cells(&frame);
    let from = |first: &str, count: usize| -> Vec<String> {
        rows.iter()
            .map(|row| row.trim_end().to_owned())
            .skip_while(|row| row != first)
            .take(count)
            .collect()
    };
    let arguments: String = rows
        .iter()
        .skip_while(|row| !row.starts_with(r#"{"command""#))
        .take_while(|row| !row.trim().is_empty())
        .cloned()
        .collect();

    assert_eq!(
        from("python3 -c '", 5),
        [
            "python3 -c '",
            "class H:",
            "    def log(self, a): pass",
            "    print(\"a    b\")",
            "' # three   spaces",
        ],
        "{frame}"
    );
    assert_eq!(
        arguments.trim_end(),
        r#"{"command":"python3 -c '\nclass H:\n    def log(self, a): pass\n\tprint(\"a    b\")\n' # three   spaces"}"#,
        "the arguments are not drawn as they arrived:\n{frame}"
    );
    assert_eq!(
        from("3. niobe saves Bash(python3 -c '", 5),
        [
            "3. niobe saves Bash(python3 -c '",
            "class H:",
            "    def log(self, a): pass",
            "    print(\"a    b\")",
            "' # three   spaces)",
        ],
        "the rule the operator would save is not drawn as it is kept:\n{frame}"
    );
    assert_snapshot("asking-spaced-120x40", &frame);
}

#[test]
fn a_short_standing_answer_with_a_tab_in_it_is_repeated_as_it_is_kept() {
    let mut app = running_session();
    app.apply(&Event::PermissionRequest {
        id: "toolu_tab".into(),
        tool: "Bash".to_owned(),
        input: r#"{"command":"ls\t-l"}"#.to_owned(),
        target: Some("ls\t-l".to_owned()),
        agent: None,
    });
    let frame = screen(&mut app, 120, 30);
    let rows = question_cells(&frame);

    assert!(
        rows.iter()
            .any(|row| row.trim_end() == "3. niobe saves Bash(ls  -l)"),
        "the rule is drawn only on its row, its tab a single space:\n{frame}"
    );
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
        agent: None,
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
        agent: None,
    });
    app.tick(t0 + Duration::from_secs(75), None);
    app
}

/// A terminal cell cannot hold a tab, and a row drawn on one line has no
/// column for one to line up: a literal tab in what a call does is a space,
/// in the call's row and in the activity row under the transcript.
#[test]
fn a_tab_in_what_a_call_does_is_drawn_as_a_space() {
    let mut app = at_work(running_session(), std::time::Duration::from_secs(5));
    app.apply(&Event::ToolCallStart {
        id: "t1".into(),
        name: "Bash".to_owned(),
        input: r#"{"command":"printf 'a\tb'"}"#.to_owned(),
        summary: Some("printf 'a\tb'".to_owned()),
        agent: None,
    });
    let frame = screen(&mut app, 120, 40);
    assert!(!frame.contains("printf 'ab'"), "{frame}");
    assert!(frame.contains("running Bash  printf 'a b'"), "{frame}");
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
        ratatui::crossterm::event::KeyCode::F(4),
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
    // Its keys whole, though the box is sized for the model names.
    for (width, height) in [(80, 24), (120, 30)] {
        let frame = screen(&mut app, width, height);
        assert!(
            frame.contains("Esc keep") && frame.contains("this one"),
            "{frame}"
        );
        assert_snapshot(&format!("model-list-{width}x{height}"), &frame);
    }
}

/// A shell asking whether to trust a repository's config that sets
/// `grants` rows of what only a trusted file may.
fn asking_trust(grants: Vec<(String, String)>) -> App {
    empty_session().asking_trust(niobe_tui::trust::Question {
        path: "/home/me/src/niobe/.niobe/config.toml".to_owned(),
        grants,
    })
}

fn row(what: &str, value: &str) -> (String, String) {
    (what.to_owned(), value.to_owned())
}

#[test]
fn the_trust_question_names_the_file_what_trusting_it_puts_in_force_and_the_answers() {
    let mut app = asking_trust(vec![
        row(
            "permissions",
            "allow Edit, Bash, mcp__claude_ai_Notion__notion-search, Bash(cargo test)",
        ),
        row(
            "work",
            "env AWS_PROFILE=work-sso, CLAUDE_CODE_USE_BEDROCK=1",
        ),
        row("work", "auth_refresh aws sso login --profile work-sso"),
        row("default_profile", "work"),
    ]);

    for (width, height) in [(80, 24), (120, 30)] {
        let frame = screen(&mut app, width, height);
        assert!(frame.contains("Trust this repository's config?"), "{frame}");
        assert!(
            frame.contains("/home/me/src/niobe/.niobe/config.toml"),
            "{frame}"
        );
        assert!(frame.contains("AWS_PROFILE=work-sso"), "{frame}");
        assert!(frame.contains("1. Yes, trust this config"), "{frame}");
        assert!(frame.contains("2. No, open without it"), "{frame}");
        assert_snapshot(&format!("trust-{width}x{height}"), &frame);
    }
}

#[test]
fn a_config_that_sets_more_than_fits_keeps_the_answers_on_screen() {
    let grants = (1..=40)
        .map(|n| row("permissions", &format!("allow Bash(script-{n}.sh)")))
        .collect();
    let mut app = asking_trust(grants);

    let frame = screen(&mut app, 80, 24);

    assert!(frame.contains("1. Yes, trust this config"), "{frame}");
    assert!(frame.contains("2. No, open without it"), "{frame}");
    assert!(
        frame.contains("more"),
        "the rows left out were not counted:\n{frame}"
    );
    assert_snapshot("trust-long-80x24", &frame);
}

#[test]
fn the_trust_question_on_a_window_too_small_to_draw_it_takes_no_answer_until_the_window_grows() {
    use niobe_tui::app::ASK_QUIET;
    use niobe_tui::trust::Answer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::time::{Duration, Instant};

    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let shown = Instant::now();
    let mut app = asking_trust(vec![row("work", "env AWS_PROFILE=work-sso")]);
    app.tick(shown, None);
    let frame = screen(&mut app, 60, 20);
    assert!(
        !frame.contains("Trust this repository's config?"),
        "{frame}"
    );

    app.on_key_read(enter, alone(shown + ASK_QUIET * 2));
    app.on_key_read(
        KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
        alone(shown + ASK_QUIET * 4),
    );

    assert_eq!(app.trust_answer(), None);
    assert!(app.trusting().is_some());
    let frame = screen(&mut app, 60, 20);
    assert!(
        frame.contains("Not taken as an answer: the window is too small"),
        "{frame}"
    );

    let grown = shown + Duration::from_secs(10);
    app.tick(grown, None);
    let frame = screen(&mut app, 80, 24);
    assert!(frame.contains("Trust this repository's config?"), "{frame}");
    app.on_key_read(enter, alone(grown + ASK_QUIET / 5));
    assert_eq!(app.trust_answer(), None);

    app.on_key_read(enter, alone(grown + ASK_QUIET * 3));
    assert_eq!(app.trust_answer(), Some(Answer::Trust));
}

#[test]
fn a_budget_is_in_the_usage_pane_with_what_has_been_spent_against_it() {
    let mut app = metered_session().with_budget(0.50);

    let frame = screen(&mut app, 120, 30);

    // The fixture reports $0.04 of cost and one record without any, so the
    // figure beside the budget is the floor of what was spent.
    assert!(frame.contains("budget ≥$0.04/$0.50"), "{frame}");
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
fn the_plans_usage_windows_are_the_headline_and_cost_and_usage_says_when_they_come_back() {
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

    app.perform(niobe_tui::menu::Action::Usage);
    let pressed = screen(&mut app, 200, 60);
    // The item reads the windows out against the same moment the pane draws
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
    let owed = totals.unsettled["opus-5"].total().tokens() as f64 / 1_000.0 * 0.001;
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
    billed_turn(niobe_core::Billing::Metered, worked_seconds, cost_usd)
}

/// One turn of `worked_seconds` that cost `cost_usd`, on a session billed as
/// `billing` from its start.
fn billed_turn(billing: niobe_core::Billing, worked_seconds: u64, cost_usd: Option<f64>) -> App {
    let clock = niobe_tui::clock::Clock::fixed(0).expect("UTC is an offset");
    let at = |seconds| clock.at(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds));
    let mut app = App::new(Repo::default()).with_clock(clock.clone());
    app.tick(std::time::Instant::now(), Some(at(0)));
    let billing = Event::Billing { billing };
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
        settles_model: false,
        fast: false,
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
    let mut app = billed_turn(niobe_core::Billing::Plan, 1_800, Some(0.50));
    let frame = screen(&mut app, 120, 30);
    assert!(!frame.contains("/h"), "{frame}");
}

/// A budget on a metered account is money against money, so it stands with
/// the session's cost at the head of the pane.
#[test]
fn a_metered_profiles_budget_stands_under_its_cost() {
    let frame = screen(&mut metered_session().with_budget(0.50), 120, 30);
    let session = frame.find("session ").expect("the cost is drawn");
    let budget = frame.find("budget ").expect("the budget is drawn");
    let model = frame.find("opus-5    ").expect("the model rows are drawn");
    assert!(session < budget && budget < model, "{frame}");
}

/// On a plan no money moves with the work, and what limits it is the
/// plan's windows: the figure the CLI prices it at is what the same work
/// would have cost on the API, which nobody pays, so no dollar figure is
/// drawn at all — in the pane, the transcript or a turn's rule.
#[test]
fn a_plan_shows_no_dollar_figure() {
    let mut app = running_session();
    let frame = screen(&mut app, 120, 30);
    assert!(!frame.contains('$'), "{frame}");
    assert!(!frame.contains("API-eq"), "{frame}");
    assert!(!frame.contains("session "), "{frame}");
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

    // And Cost & usage says what it does not do yet rather than reading out
    // a window nobody reported.
    app.perform(niobe_tui::menu::Action::Usage);
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
        frame.contains("extra     — · on"),
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
            settles_model: false,
            fast: false,
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

/// Every row of every size is drawn, and nothing panics. The buffer clips
/// every write to its own width, so text running past a pane cannot show
/// here: that ratatui draws every laid-out line inside its pane is what
/// `ui`'s own `every_line_laid_out_fits_the_width_ratatui_draws_it_in`
/// checks, on the lines themselves.
#[test]
fn every_size_between_the_two_renders_without_a_panic() {
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

/// The rows inside the Usage pane's border, read off a drawn frame: the
/// figures a test about the bill is about, and none of the transcript's text,
/// which can hold a dash or a zero of its own.
fn usage_pane(frame: &str) -> Vec<String> {
    let lines: Vec<Vec<char>> = frame.lines().map(|line| line.chars().collect()).collect();
    let (top, left) = lines
        .iter()
        .enumerate()
        .find_map(|(row, line)| {
            let text: String = line.iter().collect();
            let title = text.find(" Usage ")?;
            let before = text[..title].chars().count();
            let corner = line[..before]
                .iter()
                .rposition(|c| *c == '┌' || *c == FOCUS_TOP_LEFT)?;
            Some((row, corner))
        })
        .expect("the frame has a Usage pane");
    lines[top + 1..]
        .iter()
        .take_while(|line| {
            line.get(left)
                .is_some_and(|c| *c != '└' && *c != FOCUS_BOTTOM_LEFT)
        })
        .map(|line| line[left + 1..].iter().collect())
        .collect()
}

#[test]
fn nothing_the_backends_did_not_report_appears_as_a_number() {
    // Two usage records, one of them without a cost: the pane must show the sum
    // as a floor rather than as the session's bill.
    let floor = usage_pane(&screen(&mut metered_session(), 120, 30)).join("\n");
    assert!(floor.contains("≥$0.04"), "{floor}");

    // A metered session with a budget, whose only record carries no cost:
    // nothing in the pane may read as money that was measured.
    let mut unreported = empty_session().with_budget(5.0);
    unreported.apply(&Event::Billing {
        billing: niobe_core::Billing::Metered,
    });
    unreported.apply(&Event::Usage(niobe_core::Usage {
        input: 1_000,
        output: 100,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: 0,
        reasoning: 0,
        model: "opus-5".to_owned(),
        cost_usd: None,
        settles_model: false,
        fast: false,
    }));
    // Its tokens were measured, so a cache hit of none is a figure; money
    // nobody reported is not.
    let pane = usage_pane(&screen(&mut unreported, 120, 30)).join("\n");
    assert!(
        !pane.contains("$0.00"),
        "a zero bill in the Usage pane:\n{pane}"
    );
    assert!(pane.contains("session unpriced"), "{pane}");
    assert!(pane.contains("budget —/$5.00"), "{pane}");

    // An empty session has no cost at all, and the pane says so itself.
    let empty = usage_pane(&screen(&mut empty_session(), 120, 30)).join("\n");
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

/// The gutter between the panes stays empty while a turn runs, in every
/// theme: the spinner under the transcript is what says a turn is going.
#[test]
fn the_desktop_stays_empty_while_a_turn_runs() {
    use std::time::Duration;

    for theme in THEMES {
        let mut app = at_work(running_session().with_theme(theme), Duration::from_secs(5));
        let frame = screen(&mut app, 120, 30);
        let desk = gutter(&frame);
        assert!(
            desk.chars().all(char::is_whitespace),
            "{}: {desk:?}",
            theme.name
        );
        assert!(frame.contains("working"), "{frame}");
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
/// content is what the panes are drawn to, and text touching a double border
/// is what they are drawn that way to avoid. What a pane holds starts on the
/// row under its title: the title stands apart on the border already.
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
    }
}

/// A folded section is its header and nothing else.
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
}

/// The pane scrolls rather than truncating: the last of twenty-three files
/// is below the pane's foot, and it is reachable.
#[test]
fn the_changes_pane_scrolls_to_the_last_of_the_files() {
    let mut app = running_session();
    // Draw once so the pane knows how tall it is and how much it holds; the
    // event loop has always drawn before a wheel notch can arrive.
    let _ = screen(&mut app, 200, 60);
    app.scroll_pane(Pane::Changes, 24);
    let frame = screen(&mut app, 200, 60);

    assert_snapshot("changes-scrolled-200x60", &frame);
    assert!(
        frame.contains("verify.ts"),
        "the last file is what the pane was scrolled to"
    );
}

/// A test run is the transcript's to show, under the call that ran it: its
/// counts beside a bar of them, and no section of the Changes pane repeating
/// the last of them.
#[test]
fn a_test_run_is_drawn_under_its_call_with_a_bar_of_its_counts() {
    let counts = TestCounts {
        passed: 612,
        failed: 3,
        ignored: 0,
        suites: 30,
    };
    let run = TestRunRecord::new(
        Some(counts),
        Some(101),
        true,
        vec![FailedTests {
            binary: "--test statement".to_owned(),
            tests: vec!["a_statement_line_037_rounds_like_the_ledger".to_owned()],
        }],
    );
    let mut app = session_with_test_records(&[run]);
    let frame = screen(&mut app, 200, 60);

    assert_snapshot("tests-200x60", &frame);
    assert!(
        frame.contains("━━  612 passed · 3 failed · a_statement_line_037"),
        "{frame}"
    );
    assert!(!frame.contains("▾ Tests"), "{frame}");
    assert_eq!(
        style_at(&mut app, 200, 60, "━━━  612 passed").and_then(|style| style.fg),
        Some(CLASSIC.add),
        "the passes are the bar's colour of an addition"
    );
}

/// A directory that is not a repository has no branch and no working tree,
/// and the pane says nothing about either rather than drawing them empty: a
/// `Working tree` section reading `no files` would be a claim about a
/// repository that is not there.
#[test]
fn a_session_outside_a_repository_draws_no_branch_and_no_working_tree() {
    let mut app = App::new(Repo {
        name: "scratch".to_owned(),
        branch: None,
        ..Repo::default()
    });
    let frame = screen(&mut app, 120, 30);

    assert!(!frame.contains('⎇'), "there is no branch to name");
    assert!(!frame.contains("Working tree"), "{frame}");
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

/// The two sections the pane holds, each with the summary line the operator
/// reads it by.
#[test]
fn the_activity_pane_holds_the_sub_agents_and_the_tools() {
    let frame = screen(&mut running_session(), 200, 60);

    assert!(frame.contains("─ Activity "), "{frame}");
    for section in ["▾ Sub-agents", "▾ Tools"] {
        assert!(frame.contains(section), "{section} is missing:\n{frame}");
    }
    // The recorded session spawned three: one still running, one done, one
    // failed. The summary counts what it counted.
    assert!(
        frame.contains("1 running · 3 spawned · 1 failed"),
        "{frame}"
    );
}

/// What the running session's three agents were spawned to do, which is
/// what their rows in the Activity pane are found by.
const TESTS_TASK: &str = "Cover tests/fetch.test.ts";
const REVIEW_TASK: &str = "Review catalog/cache.ts";
const DOCS_TASK: &str = "Write docs/etags.md";

/// A session whose backend died runs nothing: the agent it was running leaves
/// the list, counted as cut short rather than failed, and the call it was
/// making is drawn as cut short, not as still running.
#[test]
fn a_session_that_ended_draws_nothing_as_still_running() {
    let mut app = running_session();
    app.apply(&Event::ToolCallStart {
        id: "toolu_cut".into(),
        name: "Bash".to_owned(),
        input: r#"{"command":"cargo build"}"#.to_owned(),
        summary: Some("cargo build".to_owned()),
        agent: None,
    });
    assert!(screen(&mut app, 200, 60).contains("◆ test "));

    app.apply(&Event::Error {
        message: "the `claude` session ended: exit status 1".to_owned(),
        fatal: true,
    });
    let frame = screen(&mut app, 200, 60);

    assert!(
        !frame.lines().any(|line| line.contains(TESTS_TASK)),
        "{frame}"
    );
    let call = frame
        .lines()
        .find(|line| line.contains("cargo build"))
        .unwrap_or_default();
    assert!(call.contains("cut short"), "{call:?}");
    assert!(frame.contains("0 running"), "{frame}");
    assert!(frame.contains("1 cut short"), "{frame}");
    assert!(frame.contains("1 failed"), "{frame}");
    assert!(
        !frame
            .lines()
            .any(|line| line.contains("running") && !line.contains("0 running")),
        "something is still drawn as running:\n{frame}"
    );
}

/// The pane lists what is running now. An agent that finished, failed or was
/// cancelled leaves the list, and the header's counts are where it went: a
/// list of every agent a long session spawned buries the one at work under
/// the ones that stopped.
#[test]
fn only_a_running_agent_is_listed_and_a_finished_one_is_counted() {
    let frame = screen(&mut running_session(), 200, 60);

    assert!(frame.contains("◆ test "), "running:\n{frame}");
    for finished in [REVIEW_TASK, DOCS_TASK] {
        assert!(
            !frame.lines().any(|line| line.contains(finished)),
            "{finished} has stopped and is still listed:\n{frame}"
        );
    }
    assert!(
        frame.contains("1 running · 3 spawned · 1 failed"),
        "{frame}"
    );
}

/// A running agent's status column is an elapsed time, measured from the
/// moment it was spawned against the moment the shell is drawing at.
#[test]
fn a_running_agent_is_timed() {
    let frame = screen(&mut running_session(), 200, 60);

    let running = frame
        .lines()
        .find(|line| line.contains(TESTS_TASK))
        .unwrap_or_default();
    assert!(
        running.trim_end_matches(['│', ' ']).ends_with(" 1m 42s"),
        "the session ran for 102 seconds before it was read:\n{running:?}"
    );
    assert!(!running.contains("ctx"), "{running:?}");
}

/// The running session with a second agent running, one its backend has
/// reported nothing about yet: no model, no step.
fn session_with_a_silent_agent() -> App {
    let mut app = running_session();
    app.apply(&Event::AgentSpawn {
        id: "a4".into(),
        parent: None,
        kind: Some("planner".to_owned()),
        label: format!("planner: {PLAN_TASK}"),
    });
    app
}

/// What the agent the backend has said nothing about was spawned to do.
const PLAN_TASK: &str = "Plan the etag rollout";

/// The model a sub-agent's own messages named is drawn beside what it was
/// spawned to do, shortened the way the Usage pane shortens it; an agent whose
/// backend named none is drawn with none.
#[test]
fn a_sub_agents_model_is_drawn_beside_what_it_was_spawned_to_do() {
    let frame = screen(&mut session_with_a_silent_agent(), 200, 60);
    let row = |label: &str| {
        frame
            .lines()
            .find(|line| line.contains(label))
            .unwrap_or_default()
            .to_owned()
    };

    assert!(row(TESTS_TASK).contains(" sonnet "), "{frame}");
    assert!(row(PLAN_TASK).contains("◆ plan"), "{frame}");
    for model in ["sonnet", "haiku", "opus"] {
        assert!(!row(PLAN_TASK).contains(model), "{frame}");
    }
}

/// Under an agent is what it is doing now, and under an agent that reported
/// nothing there is nothing — not an empty `└`.
#[test]
fn under_each_agent_is_the_last_thing_it_was_seen_doing() {
    let frame = screen(&mut session_with_a_silent_agent(), 200, 60);
    let lines: Vec<&str> = frame.lines().collect();
    let under = |label: &str| {
        let at = lines
            .iter()
            .position(|line| line.contains(label))
            .expect("the agent has a row");
        lines[at + 1].to_owned()
    };

    assert!(
        under(TESTS_TASK).contains("└ Reading tests/stream.rs"),
        "{frame}"
    );
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
    assert_eq!(under_agents, 1, "{frame}");
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

/// The pane scrolls rather than truncating: the tools are below the
/// sub-agents, and they are reachable.
#[test]
fn the_activity_pane_scrolls_to_what_is_below_the_agents() {
    let mut app = running_session();
    // Draw once so the pane knows how tall it is and how much it holds. At
    // 200x60 it holds everything it has; a terminal half that tall is where the
    // pane has to scroll.
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
        !frame.contains("◆ test "),
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
        !frame.contains(TESTS_TASK),
        "the agents are folded away, not merely scrolled past:\n{frame}"
    );
    assert!(frame.contains("▾ Tools"), "{frame}");
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

fn ctrl_f(app: &mut App) {
    app.on_key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::Char('f'),
        ratatui::crossterm::event::KeyModifiers::CONTROL,
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
    assert!(row.contains("@ file"), "{row}");
    assert!(row.contains("Ctrl+F find"), "{row}");
    assert!(
        !row.contains("/ command"),
        "the bar promises commands the backend has not listed:\n{row}"
    );
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

    app.perform(niobe_tui::menu::Action::Stop);
    let row = bar_row(&screen(&mut app, 80, 24));
    assert!(row.contains("Nothing is running"), "{row}");
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
    ctrl_f(&mut app);
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
            row.contains("Ask for a change · @ file · Ctrl+F find"),
            "with the mode in the badge, the placeholder has its room back:\n{row}"
        );
    }
}

#[test]
fn a_terminal_that_reports_shift_enter_holds_no_newline_key_at_eighty_columns() {
    let mut app = running_session().reports_shift_enter();
    let row = bar_row(&screen(&mut app, 80, 24));
    assert!(
        row.contains("Ask for a change · @ file · Ctrl+F find"),
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

    app.perform(niobe_tui::menu::Action::Stop);
    let after = screen(&mut app, 120, 30);
    let row = bar_row(&after);
    assert!(row.contains("Nothing is running"), "{row}");
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

/// The composer's rows on a frame, from the bar's first down to the pane's
/// bottom border.
fn composer_box(frame: &str) -> Vec<String> {
    let rows: Vec<&str> = frame.lines().collect();
    let bar = rows
        .iter()
        .position(|row| row.contains("  > "))
        .unwrap_or_else(|| panic!("no ask bar on the frame:\n{frame}"));
    rows[bar..]
        .iter()
        .take_while(|row| !row.starts_with(FOCUS_BOTTOM_LEFT))
        .map(|row| (*row).to_owned())
        .collect()
}

/// Numbered words, `w0 w1 w2 …`, to exactly `length` characters, so a test
/// can tell from any row which part of the prompt it holds.
fn numbered_words(length: usize) -> String {
    let mut text = String::new();
    for n in 0.. {
        let word = format!("w{n} ");
        if text.len() + word.len() > length {
            break;
        }
        text.push_str(&word);
    }
    while text.len() < length {
        text.push('z');
    }
    text
}

#[test]
fn a_long_line_without_a_break_is_drawn_whole_from_its_first_row() {
    let mut app = running_session();
    let prompt = numbered_words(300);
    type_keys(&mut app, &prompt);
    let frame = screen(&mut app, 80, 40);
    let rows = composer_box(&frame);

    assert!(
        rows[0].contains("> w0 w1 w2"),
        "the prompt's first row scrolled out of the composer:\n{frame}"
    );
    let shown: String = rows
        .iter()
        .map(|row| {
            row.split_once("> ")
                .map_or(row.as_str(), |(_, typed)| typed)
        })
        .map(|row| row.trim_matches(|c| c == FOCUS_SIDE || c == ' '))
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        shown.split_whitespace().collect::<Vec<_>>(),
        prompt.split_whitespace().collect::<Vec<_>>(),
        "every wrapped row of the prompt is on screen:\n{frame}"
    );
}

#[test]
fn paragraphs_that_each_wrap_to_two_rows_get_a_row_for_every_row_they_wrap_to() {
    let mut app = running_session();
    for paragraph in ["one", "two", "three"] {
        if paragraph != "one" {
            app.on_key(ratatui::crossterm::event::KeyEvent::new(
                ratatui::crossterm::event::KeyCode::Char('j'),
                ratatui::crossterm::event::KeyModifiers::CONTROL,
            ));
        }
        type_keys(&mut app, &format!("{paragraph} {}", "word ".repeat(18)));
    }
    let frame = screen(&mut app, 80, 40);
    let rows = composer_box(&frame);

    assert_eq!(rows.len(), 6, "{frame}");
    assert!(rows[0].contains("> one word"), "{frame}");
    assert!(rows[2].contains("two word"), "{frame}");
    assert!(rows[4].contains("three word"), "{frame}");
}

#[test]
fn a_prompt_past_a_third_of_the_pane_stops_the_composer_and_keeps_the_cursor_row() {
    let mut app = running_session();
    let prompt = numbered_words(2000);
    type_keys(&mut app, &prompt);
    let frame = screen(&mut app, 80, 24);
    let rows = composer_box(&frame);

    let pane_bottom = frame
        .lines()
        .collect::<Vec<_>>()
        .iter()
        .rposition(|row| row.starts_with(FOCUS_BOTTOM_LEFT))
        .expect("the pane is closed");
    assert_eq!(rows.len(), (pane_bottom - 2) / 3, "{frame}");
    let last_word = prompt
        .split_whitespace()
        .last()
        .expect("the prompt has words");
    assert!(
        rows.last().is_some_and(|row| row.contains(last_word)),
        "the row the cursor is on is out of sight:\n{frame}"
    );
}

#[test]
fn a_line_of_wide_characters_grows_the_composer_by_the_rows_it_is_drawn_in() {
    let mut app = running_session();
    let prompt = "漢字かな交じり文、".repeat(10);
    type_keys(&mut app, &prompt);
    let frame = screen(&mut app, 80, 40);
    let rows = composer_box(&frame);

    // The frame holds a wide character's second cell as a space.
    let shown: String = rows
        .iter()
        .map(|row| {
            row.split_once("> ")
                .map_or(row.as_str(), |(_, typed)| typed)
        })
        .flat_map(|row| row.chars().filter(|c| *c != FOCUS_SIDE && *c != ' '))
        .collect();
    assert!(shown.starts_with("漢字かな"), "{frame}");
    assert_eq!(shown, prompt, "{frame}");
}

#[test]
fn the_composer_grows_as_the_character_that_wraps_is_typed_and_shrinks_back() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = running_session();
    let mut typed = 0;
    let rows = loop {
        press(&mut app, KeyCode::Char('x'));
        typed += 1;
        let rows = composer_box(&screen(&mut app, 80, 40));
        if rows.len() > 1 || typed > 200 {
            break rows;
        }
    };
    assert_eq!(rows.len(), 2, "{rows:#?}");
    assert!(
        rows[0].contains(&format!("> {}", "x".repeat(typed - 1))),
        "the first row is the line up to the wrap:\n{rows:#?}"
    );
    assert_eq!(rows[1].matches('x').count(), 1, "{rows:#?}");

    press(&mut app, KeyCode::Backspace);
    assert_eq!(composer_box(&screen(&mut app, 80, 40)).len(), 1);
}

#[test]
fn opening_a_line_after_a_wrapped_one_keeps_both_of_its_rows_in_sight() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let line = numbered_words(110);
    for (key, terminal) in [
        (
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            running_session().reports_shift_enter(),
        ),
        (
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
            running_session(),
        ),
        (
            KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
            running_session(),
        ),
    ] {
        let mut app = terminal;
        type_keys(&mut app, &line);
        assert_eq!(composer_box(&screen(&mut app, 80, 40)).len(), 2);
        app.on_key(key);
        let frame = screen(&mut app, 80, 40);
        let rows = composer_box(&frame);
        assert_eq!(rows.len(), 3, "{key:?}:\n{frame}");
        assert!(rows[0].contains("> w0 w1"), "{key:?}:\n{frame}");
        assert!(
            rows[1].contains(line.split_whitespace().last().expect("words")),
            "{key:?}:\n{frame}"
        );
    }
}

#[test]
fn a_prompt_cut_back_under_the_cap_is_drawn_from_its_first_row() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = running_session();
    type_keys(&mut app, &numbered_words(1500));
    let capped = composer_box(&screen(&mut app, 80, 24));
    for _ in 0..1300 {
        press(&mut app, KeyCode::Backspace);
    }
    let frame = screen(&mut app, 80, 24);
    let rows = composer_box(&frame);
    assert!(rows.len() < capped.len(), "{frame}");
    assert!(
        rows[0].contains("> w0 w1"),
        "the box has room for the whole prompt but its first rows stayed scrolled away:\n{frame}"
    );
}

#[test]
fn a_reply_too_long_for_the_bar_wraps_in_it_rather_than_being_cut() {
    let mut app = running_session();
    app.perform(niobe_tui::menu::Action::SignIn);
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
        said.ends_with("use /login there"),
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
    press(&mut app, KeyCode::Tab);
    let _ = screen(&mut app, 200, 60);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    let frame = screen(&mut app, 200, 60);

    assert!(frame.contains("▸ Tools"), "{frame}");
    assert!(
        !app.folded(Section::SubAgents),
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
        app.apply(&Event::AssistantMessage { text, agent: None });
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
fn ctrl_f_searches_the_transcript_steps_between_matches_and_esc_puts_the_view_back() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = session_with_a_needle();
    let tail = screen(&mut app, 120, 30);
    assert!(
        !tail.contains("reply three"),
        "the test needs it off screen"
    );

    ctrl_f(&mut app);
    let row = find_row(&screen(&mut app, 120, 30));
    assert!(row.contains("Find in the transcript"), "{row}");
    assert!(app.composed().is_empty(), "the key went into the prompt");

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
    let mut app = session_with_a_needle();
    ctrl_f(&mut app);
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
    ctrl_f(&mut app);
    type_keys(&mut app, "haystack");
    let frame = screen(&mut app, 120, 30);
    assert!(find_row(&frame).contains("no match"), "{frame}");
    press(&mut app, KeyCode::Esc);
    assert_eq!(screen(&mut app, 120, 30), tail);
}

/// Folding the runs of calls takes the matches inside them off the
/// transcript, above the one stepped to; the one stepped to is the same text
/// after it, not whichever match now has its number.
#[test]
fn folding_the_calls_above_the_current_match_keeps_the_same_match_current() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = running_session();
    for text in ["look in fetch.ts first", "look in fetch.ts second"] {
        app.apply(&Event::AssistantMessage {
            text: text.to_owned(),
            agent: None,
        });
    }
    let open = screen(&mut app, 200, 60);
    assert!(open.contains("├ catalog/fetch.ts"), "{open}");
    ctrl_f(&mut app);
    type_keys(&mut app, "fetch.ts");
    let _ = screen(&mut app, 200, 60);
    press(&mut app, KeyCode::Up);
    let frame = screen(&mut app, 200, 60);
    let theme = app.theme().to_owned();
    let chip = Some((Some(theme.pane_bg), Some(theme.hot)));
    let current =
        |app: &mut App, text: &str| style_at(app, 200, 60, text).map(|style| (style.fg, style.bg));
    assert_eq!(current(&mut app, "fetch.ts first"), chip, "{frame}");
    let before = find_row(&frame);

    app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    let folded = screen(&mut app, 200, 60);

    assert!(!folded.contains("├ catalog/fetch.ts"), "{folded}");
    assert_ne!(
        find_row(&folded),
        before,
        "the fold took no match away:\n{folded}"
    );
    assert_eq!(current(&mut app, "fetch.ts first"), chip, "{folded}");
    assert_ne!(current(&mut app, "fetch.ts second"), chip, "{folded}");
}

/// A reply that arrives while the search is open is searched too, and the
/// match stepped to stays where it was.
#[test]
fn a_reply_arriving_while_the_search_is_open_is_counted() {
    let mut app = session_with_a_needle();
    let _ = screen(&mut app, 120, 30);
    ctrl_f(&mut app);
    type_keys(&mut app, "needle");
    assert!(find_row(&screen(&mut app, 120, 30)).contains("2 of 2"));

    app.apply(&Event::AssistantMessage {
        text: "a needle at the end".to_owned(),
        agent: None,
    });
    let frame = screen(&mut app, 120, 30);

    assert!(find_row(&frame).contains("2 of 3"), "{frame}");
}

#[test]
fn a_slash_anywhere_but_the_start_of_a_prompt_is_a_slash() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = session_with_a_needle();
    type_keys(&mut app, "a/b");
    assert!(app.finding().is_none());
    assert_eq!(app.composed(), "a/b");

    let mut app = session_with_a_needle();
    type_keys(&mut app, "/etc");
    assert!(
        app.finding().is_none(),
        "a slash opening the prompt searched"
    );
    assert_eq!(app.composed(), "/etc", "and the prompt starts with it");

    let mut app = session_with_a_needle();
    ctrl_f(&mut app);
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

/// The backend's commands as it lists them, each with what it does and, where
/// it takes something, what.
fn listed_commands(names: &[(&str, &str, Option<&str>)]) -> Event {
    Event::Commands {
        commands: names
            .iter()
            .map(
                |(name, description, hint)| niobe_core::event::SlashCommand {
                    name: (*name).to_owned(),
                    description: (*description).to_owned(),
                    argument_hint: hint.map(str::to_owned),
                },
            )
            .collect(),
    }
}

/// A session whose backend has listed its commands.
fn session_with_commands() -> App {
    let mut app = empty_session();
    app.apply(&listed_commands(&[
        (
            "compact",
            "Free up context by summarizing the conversation so far",
            None,
        ),
        ("context", "Show current context usage", None),
        ("fast", "Toggle fast mode", Some("[on|off]")),
        (
            "autocompact",
            "Configure the auto-compact window size",
            None,
        ),
    ]));
    app
}

/// The rows of the list of commands standing over the transcript, each
/// trimmed to what is inside its border.
fn command_list(frame: &str) -> Vec<String> {
    let rows: Vec<&str> = frame.lines().collect();
    let Some(top) = rows.iter().position(|row| row.contains(" / command ")) else {
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
fn a_slash_offers_the_backends_commands_and_enter_picks_one_to_send() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = session_with_commands();
    type_keys(&mut app, "/");
    assert!(app.finding().is_none());
    let frame = screen(&mut app, 120, 30);
    let offered = command_list(&frame);
    assert_eq!(offered.len(), 4, "{frame}");
    assert!(
        offered[2].starts_with("/fast [on|off]") && offered[2].contains("Toggle fast mode"),
        "a command is offered with what it takes and what it does: {offered:?}"
    );

    type_keys(&mut app, "comp");
    let frame = screen(&mut app, 120, 30);
    let offered = command_list(&frame);
    assert_eq!(offered.len(), 2, "{frame}");
    assert!(offered[0].starts_with("/compact "), "{offered:?}");
    assert!(offered[1].starts_with("/autocompact "), "{offered:?}");
    let theme = app.theme().to_owned();
    let chosen = style_at(&mut app, 120, 30, " /compact").expect("the list is drawn");
    assert_eq!(
        (chosen.fg, chosen.bg),
        (Some(theme.pane_bg), Some(theme.hot)),
        "the command Enter would take is marked as a chip"
    );

    press(&mut app, KeyCode::Enter);
    assert_eq!(app.composed(), "/compact ");
    assert!(
        app.take_produced().is_empty(),
        "the Enter that picked a command sent the prompt"
    );
    assert!(command_list(&screen(&mut app, 120, 30)).is_empty());

    type_keys(&mut app, "keep the plan");
    press(&mut app, KeyCode::Enter);
    let sent = app.take_produced();
    assert!(
        sent.iter().any(|event| matches!(
            event,
            Event::UserMessage { text } if text == "/compact keep the plan"
        )),
        "the command goes to the backend as the prompt it reads it from: {sent:?}"
    );
}

#[test]
fn enter_on_a_command_typed_in_full_sends_it_rather_than_a_longer_one() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = empty_session();
    app.apply(&listed_commands(&[
        ("review-pr", "Review a pull request", None),
        ("review", "Review the changes", None),
    ]));
    type_keys(&mut app, "/review");
    let offered = command_list(&screen(&mut app, 120, 30));
    assert!(offered[0].starts_with("/review "), "{offered:?}");

    press(&mut app, KeyCode::Enter);
    let sent = app.take_produced();
    assert!(
        sent.iter().any(|event| matches!(
            event,
            Event::UserMessage { text } if text == "/review"
        )),
        "the command typed in full is sent on the first Enter: {sent:?}"
    );
    assert_eq!(app.composed(), "");
}

#[test]
fn enter_on_another_command_than_the_one_typed_in_full_puts_that_one_in_the_prompt() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = empty_session();
    app.apply(&listed_commands(&[
        ("review-pr", "Review a pull request", None),
        ("review", "Review the changes", None),
    ]));
    type_keys(&mut app, "/review");
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.composed(), "/review-pr ");
    assert!(app.take_produced().is_empty());

    let mut app = empty_session();
    app.apply(&listed_commands(&[("review", "Review the changes", None)]));
    type_keys(&mut app, "/REVIEW");
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.composed(),
        "/review ",
        "a name typed in another case is written as the backend lists it"
    );
    assert!(app.take_produced().is_empty());
}

#[test]
fn a_list_the_backend_sends_again_replaces_what_is_offered() {
    let mut app = session_with_commands();
    app.apply(&listed_commands(&[(
        "code-review-graph:review_changes (MCP)",
        "Pre-commit review workflow",
        None,
    )]));
    type_keys(&mut app, "/");
    let offered = command_list(&screen(&mut app, 120, 30));
    assert_eq!(offered.len(), 1, "{offered:?}");
    assert!(
        offered[0].starts_with("/code-review-graph:review_changes (MCP)"),
        "{offered:?}"
    );
}

#[test]
fn a_command_is_offered_only_where_it_opens_the_prompt_and_esc_leaves_it_as_typed() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = session_with_commands();
    type_keys(&mut app, "see /comp");
    assert!(command_list(&screen(&mut app, 120, 30)).is_empty());

    let mut app = session_with_commands();
    type_keys(&mut app, "/co");
    assert!(!command_list(&screen(&mut app, 120, 30)).is_empty());
    press(&mut app, KeyCode::Esc);
    type_keys(&mut app, "n");
    assert!(
        command_list(&screen(&mut app, 120, 30)).is_empty(),
        "the list came back for the word it was closed on"
    );
    press(&mut app, KeyCode::Enter);
    assert!(
        !app.take_produced().is_empty(),
        "with the list closed, Enter sends"
    );

    type_keys(&mut app, "/co");
    assert!(
        !command_list(&screen(&mut app, 120, 30)).is_empty(),
        "a list closed on one prompt stayed closed for the next"
    );
}

#[test]
fn a_list_of_files_closed_on_a_word_opens_again_for_the_next_word_in_its_place() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = session_with_files();
    type_keys(&mut app, "@cat");
    press(&mut app, KeyCode::Esc);
    for _ in 0..4 {
        press(&mut app, KeyCode::Backspace);
    }
    type_keys(&mut app, "@cat");
    assert!(
        !file_list(&screen(&mut app, 120, 30)).is_empty(),
        "the list stayed closed for a word typed again where the closed one was"
    );
}

#[test]
fn a_session_whose_backend_listed_no_commands_does_not_offer_one() {
    let mut app = empty_session();
    assert!(!bar_row(&screen(&mut app, 200, 60)).contains("/ command"));
    type_keys(&mut app, "/");
    assert_eq!(app.composed(), "/");
    assert!(command_list(&screen(&mut app, 120, 30)).is_empty());

    let mut app = session_with_commands();
    let row = bar_row(&screen(&mut app, 200, 60));
    assert!(
        row.contains("/ command or skill"),
        "a session with commands to run says how to reach them: {row}"
    );
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
fn a_command_after_a_blank_one_still_runs_here_rather_than_reaching_the_agent() {
    use ratatui::crossterm::event::KeyCode;

    let mut app = session_that_runs_commands();
    press(&mut app, KeyCode::Char('!'));
    type_keys(&mut app, "   ");
    press(&mut app, KeyCode::Enter);
    assert!(app.take_commands().is_empty(), "a blank command ran");
    assert!(
        app.composed().is_empty(),
        "the blank command was left behind"
    );
    press(&mut app, KeyCode::Esc);

    run_command(&mut app, "echo after-blank");
    let produced = app.take_produced();
    assert!(
        !produced
            .iter()
            .any(|event| matches!(event, Event::UserMessage { .. })),
        "the command was sent to the agent as a prompt: {produced:?}"
    );
}

#[test]
fn bang_on_a_composer_of_spaces_opens_what_it_opens_on_nothing() {
    let mut app = session_that_runs_commands();
    type_keys(&mut app, "  ");
    run_command(&mut app, "echo spaced");
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
fn the_stop_key_is_named_until_the_last_running_command_ends() {
    let mut app = session_that_runs_commands();
    let first = run_command(&mut app, "sleep 1");
    let second = run_command(&mut app, "sleep 2");
    let ended = |id| niobe_tui::Ran {
        id,
        output: String::new(),
        bytes: 0,
        whole: true,
        exit_code: Some(0),
        error: None,
    };

    app.ran(ended(first));
    assert_eq!(
        app.hint(),
        Some("Ctrl+G stops it"),
        "one is still running, so the key still stops something"
    );

    app.ran(ended(second));
    assert_eq!(app.hint(), None, "nothing is left for the key to stop");
    let row = bar_row(&screen(&mut app, 200, 60));
    assert!(!row.contains("Ctrl+G"), "{row}");
}

#[test]
fn a_command_ending_leaves_a_hint_that_is_not_about_stopping_it() {
    let mut app = session_that_runs_commands();
    let id = run_command(&mut app, "sleep 1");
    stop_key(&mut app);
    stop_key(&mut app);
    assert_eq!(app.hint(), Some("No ! command is running"));

    app.ran(niobe_tui::Ran {
        id,
        output: String::new(),
        bytes: 0,
        whole: true,
        exit_code: None,
        error: Some("ended by signal 15".to_owned()),
    });
    assert_eq!(app.hint(), Some("No ! command is running"));
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
fn a_stopped_command_whose_sh_exited_0_reads_as_stopped_and_not_as_exit_0() {
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
            [Event::ToolCallEnd { outcome: ToolOutcome::Failed, exit_code: None, error: Some(error), .. }]
                if error == "stopped by the operator"
        ),
        "a stopped command read as one that succeeded: {produced:?}"
    );
    let frame = screen(&mut app, 120, 30);
    assert!(!frame.contains("exit 0"), "{frame}");
    assert!(frame.contains("stopped by the operator"), "{frame}");
}

#[test]
fn ctrl_g_with_no_command_running_says_so() {
    let mut app = session_that_runs_commands();

    stop_key(&mut app);

    assert!(app.take_stops().is_empty());
    assert_eq!(app.hint(), Some("No ! command is running"));
    assert!(!app.should_quit());
}

/// A `❤️` is two cells as ratatui draws it and one apiece by character, so a
/// reply of them wrapped by characters overran its line and lost its end.
#[test]
fn a_reply_of_emoji_is_wrapped_as_wide_as_it_is_drawn() {
    let mut app = App::new(Repo::default());
    app.apply(&Event::AssistantMessage {
        text: format!("{} END", "\u{2764}\u{fe0f}".repeat(40)),
        agent: None,
    });

    let frame = screen(&mut app, 120, 30);

    assert!(
        frame.contains("END"),
        "the end of the reply was cut off:\n{frame}"
    );
}

/// A file the repository does not track yet is in the working tree, marked
/// new: counted where its lines were read, an em dash where they were not,
/// and in the header either way.
#[test]
fn new_files_are_in_the_working_tree_marked_new_and_counted_where_read() {
    let working = |path: &str, added: Option<u64>, new: bool| niobe_tui::app::WorkingFile {
        path: path.to_owned(),
        added,
        removed: Some(0),
        new,
    };
    let mut app = App::new(Repo {
        name: "niobe".to_owned(),
        branch: Some("main".to_owned()),
        read: true,
        working: vec![
            working("kept.txt", Some(1), false),
            working("large.txt", None, true),
            working("sub/dir/inner.txt", Some(3), true),
        ],
        ..Repo::default()
    });

    let frame = screen(&mut app, 120, 30);

    assert_snapshot("working-new-120x30", &frame);
    let header = frame
        .lines()
        .find(|line| line.contains("Working tree"))
        .expect("the working tree is drawn");
    assert!(header.contains("3 files  +≥4 −0"), "{header}");
}

/// Git counts no lines in a binary file, so a working tree whose only change
/// is one has no figure to add up: its header says so rather than `+0 −0`.
#[test]
fn a_working_tree_of_nothing_but_a_binary_file_has_no_line_counts() {
    let mut app = App::new(Repo {
        name: "niobe".to_owned(),
        branch: Some("main".to_owned()),
        read: true,
        working: vec![niobe_tui::app::WorkingFile {
            path: "docs/diagram.png".to_owned(),
            added: None,
            removed: None,
            new: false,
        }],
        ..Repo::default()
    });

    let frame = screen(&mut app, 120, 30);
    let header = frame
        .lines()
        .find(|line| line.contains("Working tree"))
        .expect("the working tree is drawn");

    // The same dash a file's own row draws where git gave it no count.
    assert!(header.contains("1 file  — —"), "{header}");
    assert!(!header.contains("+0"), "{header}");
}

fn alt(letter: char) -> ratatui::crossterm::event::KeyEvent {
    ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::Char(letter),
        ratatui::crossterm::event::KeyModifiers::ALT,
    )
}

#[test]
fn an_open_menu_hangs_from_its_name_with_the_keys_that_do_each_item() {
    let mut app = running_session();
    app.on_key(alt('v'));
    for (width, height) in [(80, 24), (120, 30)] {
        let frame = screen(&mut app, width, height);
        assert!(frame.contains("Group by agent"), "{frame}");
        assert!(frame.contains("F5  Ctrl+T"), "{frame}");
        assert!(
            frame.contains("shown"),
            "a pane says whether it is: {frame}"
        );
        assert_snapshot(&format!("menu-view-{width}x{height}"), &frame);
    }
}

#[test]
fn the_settings_name_the_files_the_session_was_read_from() {
    let mut app = running_session().with_places(niobe_tui::Places {
        config_files: vec![
            niobe_tui::ConfigFile {
                path: "/home/me/.config/niobe/config.toml".to_owned(),
                exists: true,
                openable: true,
            },
            niobe_tui::ConfigFile {
                path: "/home/me/src/niobe/.niobe/config.toml".to_owned(),
                exists: false,
                openable: false,
            },
        ],
        memory: None,
    });
    app.on_key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::F(9),
        ratatui::crossterm::event::KeyModifiers::NONE,
    ));
    let frame = screen(&mut app, 120, 30);
    assert!(frame.contains(" Settings "), "{frame}");
    assert!(
        frame.contains("/home/me/.config/niobe/config.toml"),
        "{frame}"
    );
    assert!(frame.contains("o open"), "{frame}");
    assert_snapshot("settings-120x30", &frame);
}

#[test]
fn a_pane_hidden_from_the_view_menu_gives_its_rows_to_the_others() {
    let mut app = running_session();
    app.perform(niobe_tui::menu::Action::Pane(
        niobe_tui::menu::SidePane::Changes,
    ));
    let frame = screen(&mut app, 120, 30);
    assert!(!frame.contains(" Changes "), "{frame}");
    assert!(frame.contains(" Activity "), "{frame}");
    assert_snapshot("changes-hidden-120x30", &frame);

    for pane in [
        niobe_tui::menu::SidePane::Usage,
        niobe_tui::menu::SidePane::Activity,
    ] {
        app.perform(niobe_tui::menu::Action::Pane(pane));
    }
    let frame = screen(&mut app, 120, 30);
    assert!(
        !frame.contains(" Usage ") && !frame.contains(" Activity "),
        "with every pane hidden the session takes the width: {frame}"
    );
}

/// The session above with a store behind it holding two earlier sessions of
/// Niobe's and one the `claude` CLI recorded, each dated back from the moment
/// the session is read at.
fn with_history() -> App {
    use niobe_tui::history::{Past, PastPrompt, PastSession, Target};

    let mut app = running_session().remembers();
    let now = app
        .stamp()
        .expect("the session is read at a fixed moment")
        .at();
    let clock = niobe_tui::clock::Clock::fixed(0).expect("UTC is an offset");
    let ago = |secs: u64| Some(clock.at(now - std::time::Duration::from_secs(secs)));
    let prompt = |text: &str, session: &str, secs| PastPrompt {
        text: text.to_owned(),
        at: ago(secs),
        session: Target::Recorded(session.to_owned()),
    };
    app.set_past(Past {
        prompts: vec![
            prompt("and a test for the 304 path", "12", 3_600),
            prompt(
                "add etag support to the static handler\nkeep the weak form for gzip",
                "12",
                4_000,
            ),
            prompt("why does the resume test hang?", "11", 3 * 86_400),
        ],
        sessions: vec![
            PastSession {
                target: Target::Recorded("12".to_owned()),
                last: ago(3_500),
                first_prompt: None,
                open_elsewhere: false,
            },
            PastSession {
                target: Target::Claude("2f6c1e10-8f4b-4d2a-9c3e-7a5b0d1e6f42".to_owned()),
                last: ago(2 * 86_400),
                first_prompt: Some("rename the crate".to_owned()),
                open_elsewhere: false,
            },
            PastSession {
                target: Target::Recorded("11".to_owned()),
                last: ago(3 * 86_400),
                first_prompt: None,
                open_elsewhere: true,
            },
        ],
        unread: None,
    });
    app
}

#[test]
fn ctrl_r_lists_every_prompt_and_shows_the_chosen_one_whole() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = with_history();
    app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);

    for (width, height) in [(80, 24), (120, 30)] {
        let frame = screen(&mut app, width, height);
        assert!(frame.contains("this session"), "{frame}");
        assert!(frame.contains("#12"), "{frame}");
        assert!(
            frame.contains("keep the weak form for gzip"),
            "the prompt under the cursor is shown whole:\n{frame}"
        );
        assert!(frame.contains("Enter put in the prompt"), "{frame}");
        assert_snapshot(&format!("history-prompts-{width}x{height}"), &frame);
    }
}

#[test]
fn the_history_turned_to_its_sessions_lists_each_with_its_prompts() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = with_history();
    app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Down);

    for (width, height) in [(80, 24), (120, 30)] {
        let frame = screen(&mut app, width, height);
        assert!(frame.contains("claude 2f6c1e10"), "{frame}");
        assert!(frame.contains("2 prompts"), "{frame}");
        assert!(frame.contains("and a test for the 304 path"), "{frame}");
        assert!(frame.contains("Enter open"), "{frame}");
        assert_snapshot(&format!("history-sessions-{width}x{height}"), &frame);
    }
}

#[test]
fn a_session_of_ten_prompts_lists_its_first_prompt_in_the_same_column_as_the_rest() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = with_history();
    for n in 2..=10 {
        app.apply(&Event::UserMessage {
            text: format!("step {n}"),
        });
    }
    app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    press(&mut app, KeyCode::Tab);

    let frame = screen(&mut app, 120, 30);
    // The column the first prompt starts at in the row of the session `row`
    // names.
    let column = |row: &str, said: &str| {
        frame
            .lines()
            .filter(|line| line.contains(row))
            .find_map(|line| line.find(said).map(|at| line[..at].chars().count()))
            .unwrap_or_else(|| panic!("{said:?} is listed for {row:?}:\n{frame}"))
    };
    let rename = column("claude 2f6c1e10", "rename the crate");
    assert!(frame.contains("10 prompts"), "{frame}");
    assert_eq!(
        column("this session", "add etag support"),
        rename,
        "{frame}"
    );
    assert_eq!(column("#12", "add etag support"), rename, "{frame}");
}

#[test]
fn a_session_another_niobe_has_open_is_marked_and_enter_on_it_says_why_it_stays_shut() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = with_history();
    app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    press(&mut app, KeyCode::Tab);
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);

    assert!(!app.should_quit());
    let frame = screen(&mut app, 80, 24);
    assert!(frame.contains("#11 in use"), "{frame}");
    assert!(
        frame.contains("#11 is open in another niobe: quit it there to open it here"),
        "{frame}"
    );
    assert_snapshot("history-session-in-use-80x24", &frame);
}

#[test]
fn the_history_opened_before_its_load_lands_says_it_is_reading() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = running_session().remembers();
    app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    press(&mut app, KeyCode::Tab);

    let frame = screen(&mut app, 120, 30);
    assert!(frame.contains("reading the earlier sessions"), "{frame}");
    assert!(!frame.contains("no other session"), "{frame}");
}
