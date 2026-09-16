// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The shell, rendered.
//!
//! The shell must render at 80x24 and at 200x60 and redraw smoothly on resize.
//! Both sizes are drawn into a [`TestBackend`] and compared against a committed
//! picture of the screen, so a layout change has to be looked at rather than
//! merely compiled. Regenerate with `UPDATE_SNAPSHOTS=1 cargo test`.
//!
//! The events below are written here rather than taken from a recording: this
//! file is about where things land on screen, and a fixture that drifts would
//! make the pictures drift with it.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::path::PathBuf;
use std::time::Instant;

use niobe_core::event::{
    AgentOutcome, Backend, Event, PermissionDecision, SessionMeta, ToolOutcome, Usage,
};
use niobe_tui::app::{App, Repo, SelectedProfile};
use niobe_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

/// A frame is drawn well inside a 60 Hz budget at the largest supported
/// snapshot size, so a resize redraws without a visible stutter.
const FRAME_BUDGET_MS: u128 = 16;

fn session_events() -> Vec<Event> {
    vec![
        Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "default".to_owned(),
            model: "opus-5".to_owned(),
        }),
        Event::UserMessage {
            text: "add etag support to the catalog fetcher so we stop re-downloading \
                   unchanged manifests"
                .to_owned(),
        },
        Event::AssistantMessage {
            text: "Reading catalog/fetch.ts and its callers first. Two call sites, one \
                   test file."
                .to_owned(),
        },
        Event::ToolCallStart {
            id: "t1".into(),
            name: "Read".to_owned(),
            input: "catalog/fetch.ts".to_owned(),
        },
        Event::ToolCallEnd {
            id: "t1".into(),
            name: "Read".to_owned(),
            input: "catalog/fetch.ts".to_owned(),
            output: "212 lines".to_owned(),
            bytes: 7_412,
            outcome: ToolOutcome::Ok,
        },
        Event::Usage(Usage {
            input: 2_100,
            output: 180,
            cache_read: 18_400,
            cache_write: 900,
            reasoning: 0,
            model: "opus-5".to_owned(),
            cost_usd: Some(0.04),
        }),
        Event::Decision {
            summary: "Reuse the existing LRU instead of a new Map — avoids a second \
                      eviction policy."
                .to_owned(),
            rationale: None,
            rejected: vec!["A second Map keyed by URL".to_owned()],
        },
        Event::AgentSpawn {
            id: "a1".into(),
            parent: None,
            label: "test-writer → tests/fetch.test.ts".to_owned(),
        },
        Event::AgentSpawn {
            id: "a2".into(),
            parent: None,
            label: "reviewer → catalog/cache.ts".to_owned(),
        },
        Event::AgentExit {
            id: "a2".into(),
            outcome: AgentOutcome::Completed,
        },
        Event::ToolCallStart {
            id: "t2".into(),
            name: "Edit".to_owned(),
            input: "catalog/fetch.ts".to_owned(),
        },
        Event::PermissionRequest {
            id: "t2".into(),
            tool: "Edit".to_owned(),
            input: "catalog/fetch.ts".to_owned(),
        },
        Event::PermissionResponse {
            id: "t2".into(),
            decision: PermissionDecision::Allow,
        },
        Event::ToolCallEnd {
            id: "t2".into(),
            name: "Edit".to_owned(),
            input: "catalog/fetch.ts".to_owned(),
            output: "+38 −9".to_owned(),
            bytes: 640,
            outcome: ToolOutcome::Ok,
        },
        Event::ToolCallEnd {
            id: "t3".into(),
            name: "Bash".to_owned(),
            input: "npm test -- fetch".to_owned(),
            output: "1 failing".to_owned(),
            bytes: 2_048,
            outcome: ToolOutcome::Failed,
        },
        Event::Usage(Usage {
            input: 3_400,
            output: 620,
            cache_read: 22_000,
            cache_write: 0,
            reasoning: 1_200,
            model: "opus-5".to_owned(),
            cost_usd: None,
        }),
        Event::AssistantMessage {
            text: "Etags cached in the LRU; 304s short-circuit. One test still red — the \
                   304 path asserts a body that is no longer sent."
                .to_owned(),
        },
    ]
}

fn running_session() -> App {
    let mut app = App::new(Repo {
        name: "example-app".to_owned(),
        branch: Some("main".to_owned()),
    });
    app.extend(&session_events());
    app
}

fn empty_session() -> App {
    App::new(Repo {
        name: "niobe".to_owned(),
        branch: Some("main".to_owned()),
    })
}

/// Draws one frame and returns the screen as text, one line per row.
fn screen(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal =
        Terminal::new(TestBackend::new(width, height)).expect("a test backend cannot fail");
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("a test backend cannot fail");
    render(terminal.backend().buffer())
}

fn render(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()))
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
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
fn a_selected_profile_is_named_in_the_status_line_and_the_menu_bar() {
    let mut app = empty_session().with_profile(SelectedProfile {
        name: "work".to_owned(),
        backend: Backend::Claude,
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
fn a_resize_redraws_inside_a_frame_budget() {
    let mut app = running_session();
    // Warm: the first draw allocates the buffers a resize would reuse.
    let _ = screen(&mut app, 200, 60);

    let started = Instant::now();
    for _ in 0..10 {
        let _ = screen(&mut app, 200, 60);
    }
    let per_frame = started.elapsed().as_millis() / 10;

    assert!(
        per_frame <= FRAME_BUDGET_MS,
        "a frame at 200x60 took {per_frame} ms, over the {FRAME_BUDGET_MS} ms budget"
    );
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
