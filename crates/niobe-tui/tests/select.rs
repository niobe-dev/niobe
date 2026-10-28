// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Selecting the transcript with the mouse and clicking the links in it, on
//! the shell as it is drawn.
//!
//! The shell takes the mouse, so the terminal selects nothing itself: a drag
//! has to mark and copy what it covers, and a click has to open a link.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

mod common;

use common::{running_session, screen, style_at};
use niobe_core::event::Event;
use niobe_tui::Handoff;
use niobe_tui::app::App;
use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

const WIDTH: u16 = 120;
const HEIGHT: u16 = 30;

/// The running session after one more reply, saying `reply`.
fn replied(reply: &str) -> App {
    let mut app = running_session();
    app.apply(&Event::UserMessage {
        text: "Where is it?".to_owned(),
    });
    app.apply(&Event::AssistantMessage {
        text: reply.to_owned(),
        agent: None,
    });
    app
}

fn mouse(kind: MouseEventKind, (column, row): (u16, u16)) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

/// The cell `text` starts at on `frame`, the last place it is drawn.
fn cell_of(frame: &str, text: &str) -> (u16, u16) {
    let (row, line) = frame
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(text))
        .last()
        .unwrap_or_else(|| panic!("{text:?} is on screen:\n{frame}"));
    let byte = line.find(text).expect("the line was found by it");
    let column = line[..byte].chars().count();
    (
        u16::try_from(column).expect("the screen is 120 wide"),
        u16::try_from(row).expect("the screen is 30 high"),
    )
}

/// Drags from `from` to `to` and lets go there.
fn drag(app: &mut App, from: (u16, u16), to: (u16, u16)) {
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), from));
    app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), to));
    app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), to));
}

/// Clicks `at` without moving.
fn click(app: &mut App, at: (u16, u16)) {
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), at));
    app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), at));
}

#[test]
fn a_drag_over_the_transcript_copies_what_it_covers_when_let_go() {
    let mut app = replied("The config lives in niobe.toml beside the manifest.");
    let frame = screen(&mut app, WIDTH, HEIGHT);
    let (column, row) = cell_of(&frame, "niobe.toml");

    drag(&mut app, (column, row), (column + 9, row));

    assert_eq!(
        app.take_handoffs(),
        vec![Handoff::Copy("niobe.toml".to_owned())]
    );
}

#[test]
fn a_drag_over_inline_code_copies_its_spaces_as_the_agent_wrote_them() {
    let code = "sed 's/  / /;s/^   //' notes.txt";
    let mut app = replied(&format!("Squeeze them with `{code}` and check."));
    let frame = screen(&mut app, WIDTH, HEIGHT);
    let (column, row) = cell_of(&frame, "sed '");
    let last = u16::try_from(code.chars().count()).expect("the code is short") - 1;

    drag(&mut app, (column, row), (column + last, row));

    assert_eq!(app.take_handoffs(), vec![Handoff::Copy(code.to_owned())]);
}

#[test]
fn a_drag_across_lines_copies_them_without_the_gutter_they_are_drawn_in() {
    let mut app = replied("First line of the answer.\n\nSecond paragraph here.");
    let frame = screen(&mut app, WIDTH, HEIGHT);
    let from = cell_of(&frame, "First line");
    let (_, row) = cell_of(&frame, "Second paragraph");

    // From the gutter the reply is drawn in, as a drag that takes whole
    // lines starts.
    drag(&mut app, (from.0 - 2, from.1), (WIDTH - 40, row));

    assert_eq!(
        app.take_handoffs(),
        vec![Handoff::Copy(
            "First line of the answer.\n\nSecond paragraph here.".to_owned()
        )]
    );
}

#[test]
fn what_a_drag_selected_stays_marked_until_a_key_is_pressed() {
    let mut app = replied("The config lives in niobe.toml beside the manifest.");
    let frame = screen(&mut app, WIDTH, HEIGHT);
    let unmarked = style_at(&mut app, WIDTH, HEIGHT, "niobe.toml");
    let (column, row) = cell_of(&frame, "niobe.toml");

    drag(&mut app, (column, row), (column + 9, row));
    let marked = style_at(&mut app, WIDTH, HEIGHT, "niobe.toml");
    assert_ne!(marked, unmarked, "the selection is drawn");
    assert_eq!(
        style_at(&mut app, WIDTH, HEIGHT, " beside"),
        style_at(&mut app, WIDTH, HEIGHT, " the manifest"),
        "and only where the drag went"
    );

    app.on_key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::Char('x'),
        KeyModifiers::NONE,
    ));
    assert_eq!(style_at(&mut app, WIDTH, HEIGHT, "niobe.toml"), unmarked);
}

#[test]
fn a_click_on_a_link_opens_it_and_a_click_beside_it_opens_nothing() {
    let mut app = replied("The docs are at https://example.com/docs/select. Read them.");
    let frame = screen(&mut app, WIDTH, HEIGHT);
    let (column, row) = cell_of(&frame, "example.com");

    click(&mut app, (column + 3, row));
    assert_eq!(
        app.take_handoffs(),
        vec![Handoff::Open("https://example.com/docs/select".to_owned())]
    );

    let beside = cell_of(&frame, "Read them");
    click(&mut app, beside);
    assert_eq!(app.take_handoffs(), Vec::new());
}

#[test]
fn a_markdown_link_opens_where_it_leads() {
    let mut app = replied("See [the guide](https://example.com/guide) first.");
    let frame = screen(&mut app, WIDTH, HEIGHT);
    let at = cell_of(&frame, "example.com/guide");

    click(&mut app, at);

    assert_eq!(
        app.take_handoffs(),
        vec![Handoff::Open("https://example.com/guide".to_owned())]
    );
}

#[test]
fn how_a_copy_went_is_said_on_the_bar() {
    let mut app = replied("Short.");
    let _ = screen(&mut app, WIDTH, HEIGHT);

    app.handed_off(&Handoff::Copy("two\nlines".to_owned()), Ok(()));
    assert_eq!(app.hint(), Some("Copied 2 lines"));
    app.handed_off(
        &Handoff::Copy("x".to_owned()),
        Err("this needs pbcopy".to_owned()),
    );
    assert_eq!(app.hint(), Some("Nothing copied: this needs pbcopy"));
}

#[test]
fn a_drag_past_the_bottom_of_the_transcript_scrolls_it_on() {
    let mut app = running_session();
    let _ = screen(&mut app, WIDTH, HEIGHT);
    app.scroll_to_head();
    let _ = screen(&mut app, WIDTH, HEIGHT);
    let before = app.scroll();

    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), (4, 3)));
    app.on_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        (4, HEIGHT - 1),
    ));

    assert_eq!(app.scroll(), before + 1);
}

#[test]
fn a_link_too_long_for_its_line_opens_whole_from_the_part_on_the_next() {
    let link = format!("https://example.com/{}/end", "segment".repeat(20));
    let mut app = replied(&format!("It is at {link} now."));
    let frame = screen(&mut app, WIDTH, HEIGHT);
    let at = cell_of(&frame, "/end");

    click(&mut app, at);

    assert_eq!(app.take_handoffs(), vec![Handoff::Open(link)], "{frame}");
}

/// A link as long as its row is wide, or anywhere near, fits on it whole, and
/// the word after it starts the next row: that word is not part of the link,
/// however exactly the link filled the row it ended.
#[test]
fn a_link_that_fills_its_row_opens_without_the_word_on_the_next() {
    for length in 30..=120 {
        let link = format!("https://example.com/{}", "a".repeat(length - 20));
        let mut app = replied(&format!("{link} nextword here"));
        let frame = screen(&mut app, WIDTH, HEIGHT);
        let at = cell_of(&frame, "https://example.com/");

        click(&mut app, at);

        assert_eq!(
            app.take_handoffs(),
            vec![Handoff::Open(link.clone())],
            "a link {length} long:\n{frame}"
        );
    }
}

/// A link wider than its row is cut where the row ends and carried on at the
/// start of the next, and opens whole from either part, without the word
/// after it.
#[test]
fn a_link_wider_than_its_row_opens_whole_from_either_part() {
    for length in 71..=150 {
        let link = format!("https://example.com/{}", "a".repeat(length - 20));
        let mut app = replied(&format!("{link} nextword here"));
        let frame = screen(&mut app, WIDTH, HEIGHT);

        for part in ["https://example.com/", "a nextword"] {
            let Some((column, row)) = frame
                .lines()
                .enumerate()
                .filter_map(|(row, line)| Some((line[..line.find(part)?].chars().count(), row)))
                .last()
            else {
                continue;
            };
            let column = u16::try_from(column).expect("the screen is 120 wide");
            let row = u16::try_from(row).expect("the screen is 30 high");
            click(&mut app, (column, row));

            assert_eq!(
                app.take_handoffs(),
                vec![Handoff::Open(link.clone())],
                "a link {length} long, clicked on {part:?}:\n{frame}"
            );
        }
    }
}

/// The click was the dialog's, not the transcript's under it: a link there
/// would hand the machine's opener an address nobody aimed at, and a drag
/// would copy text the dialog hides.
#[test]
fn a_click_or_a_drag_over_an_open_dialog_reaches_nothing_under_it() {
    use niobe_tui::menu::Action;

    for (name, open) in [
        ("history", Action::History),
        ("theme", Action::Theme),
        ("search", Action::Find),
    ] {
        let mut app = replied("The docs are at https://example.com/docs/select. Read them.");
        let frame = screen(&mut app, WIDTH, HEIGHT);
        let (column, row) = cell_of(&frame, "example.com");
        app.perform(open);
        assert!(
            app.picking().is_some() || app.finding().is_some() || app.browser().is_some(),
            "{name} did not open"
        );

        click(&mut app, (column + 3, row));
        assert_eq!(app.take_handoffs(), vec![], "{name}: a click opened it");

        drag(&mut app, (column, row), (column + 9, row));
        assert_eq!(app.take_handoffs(), vec![], "{name}: a drag copied it");
    }
}

/// The running session after one more call of its own, a `Grep` that
/// returned `output`.
fn searched(output: &str) -> App {
    let mut app = running_session();
    app.apply(&Event::UserMessage {
        text: "Where is it used?".to_owned(),
    });
    app.apply(&Event::ToolCallStart {
        id: "g1".into(),
        name: "Grep".to_owned(),
        input: "{\"pattern\":\"etag\"}".to_owned(),
        summary: Some("etag".to_owned()),
        agent: None,
    });
    app.apply(&Event::ToolCallEnd {
        id: "g1".into(),
        name: "Grep".to_owned(),
        input: "{\"pattern\":\"etag\"}".to_owned(),
        output: output.to_owned(),
        bytes: output.len() as u64,
        outcome: niobe_core::event::ToolOutcome::Ok,
        summary: Some("etag".to_owned()),
        exit_code: None,
        error: None,
        command: None,
        untrimmed: None,
    });
    app
}

#[test]
fn a_click_on_a_calls_row_opens_the_call_whole() {
    let mut app = searched("catalog/fetch.ts:41: etag");
    let frame = screen(&mut app, WIDTH, HEIGHT);

    click(&mut app, cell_of(&frame, "⚙ Grep"));

    let detail = app.detail().expect("the click opened the call");
    assert_eq!(
        detail.text(),
        "{\"pattern\":\"etag\"}catalog/fetch.ts:41: etag"
    );
    assert!(
        app.take_handoffs().is_empty(),
        "nothing went to the desktop"
    );
}

/// A call's start, and its end returning `output`.
fn call_events(id: &str, name: &str, input: &str, output: &str) -> (Event, Event) {
    (
        Event::ToolCallStart {
            id: id.into(),
            name: name.to_owned(),
            input: input.to_owned(),
            summary: None,
            agent: None,
        },
        Event::ToolCallEnd {
            id: id.into(),
            name: name.to_owned(),
            input: input.to_owned(),
            output: output.to_owned(),
            bytes: output.len() as u64,
            outcome: niobe_core::event::ToolOutcome::Ok,
            summary: None,
            exit_code: None,
            error: None,
            command: None,
            untrimmed: None,
        },
    )
}

#[test]
fn a_click_on_a_call_that_ended_after_it_was_cut_short_opens_that_call() {
    // The turn ends with the build still running, and the fold cuts it; a
    // later call starts and ends; then the build's end arrives late, as a
    // sub-agent's records can, and ends the build on its own row.
    let mut app = running_session();
    let (build, built) = call_events("b1", "Bash", "{\"command\":\"make\"}", "built ok");
    let (grep, grepped) = call_events("g2", "Grep", "{\"pattern\":\"etag\"}", "fetch.ts:41");
    app.apply(&Event::UserMessage {
        text: "Build it.".to_owned(),
    });
    app.apply(&build);
    app.apply(&Event::TurnEnded);
    app.apply(&Event::UserMessage {
        text: "Where is it used?".to_owned(),
    });
    app.apply(&grep);
    app.apply(&grepped);
    app.apply(&built);
    let frame = screen(&mut app, WIDTH, 60);

    click(&mut app, cell_of(&frame, "⚙ Bash"));

    let detail = app.detail().expect("the click opened a call");
    assert_eq!(detail.text(), "{\"command\":\"make\"}built ok", "{frame}");
}

#[test]
fn a_drag_across_a_calls_row_selects_it_and_opens_nothing() {
    let mut app = searched("catalog/fetch.ts:41: etag");
    let frame = screen(&mut app, WIDTH, HEIGHT);
    let (column, row) = cell_of(&frame, "⚙ Grep");

    drag(&mut app, (column, row), (column + 8, row));

    assert!(app.detail().is_none());
    assert!(matches!(
        app.take_handoffs().as_slice(),
        [Handoff::Copy(copied)] if copied.contains("Grep")
    ));
}

#[test]
fn a_click_on_a_run_of_calls_opens_the_list_on_the_runs_first() {
    let mut app = running_session();
    let frame = screen(&mut app, WIDTH, 60);
    // The session's reads are one run; its own row names no one call.
    click(&mut app, cell_of(&frame, "Read ×3"));

    let picker = app.picking().expect("the click opened the list of calls");
    assert_eq!(
        picker.options[picker.at].note.as_deref(),
        Some("catalog/fetch.ts · 7.2 kB")
    );
    assert!(app.detail().is_none());
}

#[test]
fn a_click_on_a_calls_row_in_an_open_run_opens_that_call() {
    for (row, path) in [
        ("├ catalog/fetch.ts", "catalog/fetch.ts"),
        ("├ catalog/cache.ts", "catalog/cache.ts"),
        ("└ tests/fetch.test.ts", "tests/fetch.test.ts"),
    ] {
        let mut app = running_session();
        let frame = screen(&mut app, WIDTH, 60);

        click(&mut app, cell_of(&frame, row));

        let detail = app.detail().expect("the click opened the call on its row");
        assert_eq!(detail.text(), path, "{row}");
        assert!(app.picking().is_none(), "{row}");
    }
}

#[test]
fn a_click_on_what_is_drawn_under_a_call_in_an_open_run_opens_that_call() {
    let mut app = running_session();
    let (first, first_end) = call_events("x1", "Bash", "{\"command\":\"make a\"}", "");
    let (second, second_end) = call_events("x2", "Bash", "{\"command\":\"make b\"}", "");
    let failed = match first_end {
        Event::ToolCallEnd {
            id,
            name,
            input,
            summary,
            command,
            untrimmed,
            ..
        } => Event::ToolCallEnd {
            id,
            name,
            input,
            output: "make: *** no rule to make target 'a'".to_owned(),
            bytes: 37,
            outcome: niobe_core::event::ToolOutcome::Failed,
            summary,
            exit_code: Some(2),
            error: Some("no rule to make target 'a'".to_owned()),
            command,
            untrimmed,
        },
        other => other,
    };
    app.apply(&Event::UserMessage {
        text: "Build both.".to_owned(),
    });
    app.apply(&first);
    app.apply(&second);
    app.apply(&failed);
    app.apply(&second_end);
    let frame = screen(&mut app, WIDTH, 60);

    // The reason the first failed is drawn under its row.
    click(&mut app, cell_of(&frame, "no rule to make target"));

    let detail = app.detail().expect("the click opened the call it is under");
    assert!(
        detail.text().starts_with("{\"command\":\"make a\"}"),
        "{frame}"
    );
}

#[test]
fn a_click_on_a_sub_agents_call_in_an_open_run_opens_it_with_the_rows_grouped_by_agent() {
    let mut app = running_session();
    app.apply(&Event::UserMessage {
        text: "Look for both.".to_owned(),
    });
    for (id, pattern) in [("s1", "etag_first"), ("s2", "etag_second")] {
        let input = format!("{{\"pattern\":\"{pattern}\"}}");
        app.apply(&Event::ToolCallStart {
            id: id.into(),
            name: "Grep".to_owned(),
            input: input.clone(),
            summary: Some(pattern.to_owned()),
            agent: Some("a1".into()),
        });
        let (_, end) = call_events(id, "Grep", &input, "");
        app.apply(&end);
    }
    app.group_by_agent();
    let frame = screen(&mut app, WIDTH, 60);

    click(&mut app, cell_of(&frame, "└ etag_second"));

    let detail = app.detail().expect("the click opened the call on its row");
    assert_eq!(detail.text(), "{\"pattern\":\"etag_second\"}", "{frame}");
}

#[test]
fn a_click_on_a_folded_run_opens_the_list_on_the_runs_first() {
    let mut app = running_session();
    app.fold_calls();
    let frame = screen(&mut app, WIDTH, 60);
    click(&mut app, cell_of(&frame, "Read ×3"));

    let picker = app.picking().expect("the click opened the list of calls");
    assert_eq!(
        picker.options[picker.at].note.as_deref(),
        Some("catalog/fetch.ts · 7.2 kB")
    );
    assert!(app.detail().is_none());
}

#[test]
fn a_click_on_a_link_in_a_reply_still_opens_the_link() {
    let mut app = replied("See https://example.com/etags for the spec.");
    let frame = screen(&mut app, WIDTH, HEIGHT);

    click(&mut app, cell_of(&frame, "example.com"));

    assert!(app.detail().is_none());
    assert!(matches!(
        app.take_handoffs().as_slice(),
        [Handoff::Open(link)] if link == "https://example.com/etags"
    ));
}

#[test]
fn a_drag_copies_only_what_was_drawn_and_none_of_the_controls_the_reply_hid() {
    // An escape that ends a bracketed paste and a carriage return draw as
    // nothing, so the operator cannot see them; on the clipboard they would
    // run the line in the shell it is pasted into.
    let mut app = replied("Then run ls -la\u{1b}[201~\r\u{202e} in the repository.");
    let frame = screen(&mut app, WIDTH, HEIGHT);
    let (column, row) = cell_of(&frame, "ls -la");
    let (end, _) = cell_of(&frame, " repository.");

    drag(&mut app, (column, row), (end, row));

    let seen = "ls -la[201~  in the";
    assert!(frame.contains(seen), "{frame}");
    assert_eq!(app.take_handoffs(), vec![Handoff::Copy(seen.to_owned())]);
}

#[test]
fn a_click_on_a_window_too_small_to_draw_the_shell_presses_nothing_the_last_frame_drew() {
    let mut app = running_session();
    let frame = screen(&mut app, 80, 24);
    let quit = cell_of(&frame, "Quit");
    let menu = cell_of(&frame, "Niobe");

    // Narrowed by a column, the window shows only that it is too small: the
    // F-key bar and the menus the last full frame drew are not on it.
    let frame = screen(&mut app, 79, 24);
    assert!(!frame.contains("Quit"), "{frame}");
    click(&mut app, quit);
    click(&mut app, menu);

    assert!(
        !app.should_quit(),
        "a click on a blank window quit the session"
    );
    assert!(
        app.menu().is_none(),
        "a click on a blank window opened a menu"
    );
}
