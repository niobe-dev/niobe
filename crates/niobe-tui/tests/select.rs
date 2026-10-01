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
