// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The session the shell tests draw, and the helper that draws it.
//!
//! Shared by the snapshot tests and the frame-budget test. They are separate
//! test binaries so that no frame is timed while the snapshot tests draw
//! hundreds of frames on the same cores.
//!
//! The events are written here rather than taken from a recording: these tests
//! are about where things land on screen, and a fixture that drifts would make
//! the pictures drift with it.

// Each of the two binaries that include this module compiles its own copy and
// uses the part of it that it needs: the frame-budget test times frames and
// never asks what colour they came out.
#![allow(
    dead_code,
    reason = "shared by two test binaries, neither of which uses all of it"
)]

use niobe_core::event::{
    AgentOutcome, Backend, Event, Mode, PermissionDecision, SessionMeta, ToolOutcome, Usage,
    UsageWindow, UsageWindows,
};
use niobe_tui::app::{App, Repo};
use niobe_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Style};

fn session_events() -> Vec<Event> {
    vec![
        Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "default".to_owned(),
            model: "opus-5".to_owned(),
            backend_session: None,
        }),
        Event::ModeSelected { mode: Mode::Ask },
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
            summary: None,
        },
        Event::ToolCallEnd {
            id: "t1".into(),
            name: "Read".to_owned(),
            input: "catalog/fetch.ts".to_owned(),
            output: "212 lines".to_owned(),
            bytes: 7_412,
            outcome: ToolOutcome::Ok,
            summary: None,
        },
        Event::Usage(Usage {
            input: 2_100,
            output: 180,
            cache_read: 18_400,
            cache_write: 900,
            cache_write_1h: 0,
            reasoning: 0,
            model: "opus-5".to_owned(),
            cost_usd: Some(0.04),
            cost_basis: None,
        }),
        // A plan profile: the windows are what this session is metered
        // against, so they are on the status line where a budget would be.
        // The reset times are fixed instants in the past — the session is a
        // recording, and a reset in the future would make what the shell draws
        // for it depend on the day the test ran.
        Event::UsageWindows(UsageWindows {
            five_hour: Some(UsageWindow {
                utilization: 0.62,
                resets_at: Some(1_767_225_600),
            }),
            seven_day: Some(UsageWindow {
                utilization: 0.18,
                resets_at: Some(1_767_830_400),
            }),
            using_overage: false,
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
        Event::AssistantMessage {
            text: "Caching the etag beside the body so a 304 can be answered from the LRU."
                .to_owned(),
        },
        Event::ToolCallStart {
            id: "t2".into(),
            name: "Edit".to_owned(),
            input: "catalog/fetch.ts".to_owned(),
            summary: None,
        },
        Event::PermissionRequest {
            id: "t2".into(),
            tool: "Edit".to_owned(),
            input: r#"{"file_path":"catalog/fetch.ts"}"#.to_owned(),
            target: Some("catalog/fetch.ts".to_owned()),
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
            summary: None,
        },
        Event::FileChange {
            path: "catalog/fetch.ts".to_owned(),
            added: Some(38),
            removed: Some(9),
        },
        // The two readings a count can have besides a figure: a rewrite whose
        // previous contents the backend never showed, and a change it stated
        // no size for at all.
        Event::AssistantMessage {
            text: "Rewriting the 304 test around the cached body.".to_owned(),
        },
        Event::FileChange {
            path: "catalog/etag.test.ts".to_owned(),
            added: Some(24),
            removed: None,
        },
        Event::AssistantMessage {
            text: "The notebook that demonstrates the fetcher needs the new call shape.".to_owned(),
        },
        Event::FileChange {
            path: "docs/notebooks/catalog.ipynb".to_owned(),
            added: None,
            removed: None,
        },
        Event::ToolCallEnd {
            id: "t3".into(),
            name: "Bash".to_owned(),
            input: "npm test -- fetch".to_owned(),
            output: "1 failing".to_owned(),
            bytes: 2_048,
            outcome: ToolOutcome::Failed,
            summary: None,
        },
        Event::Usage(Usage {
            input: 3_400,
            output: 620,
            cache_read: 22_000,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 1_200,
            model: "opus-5".to_owned(),
            cost_usd: None,
            cost_basis: None,
        }),
        Event::AssistantMessage {
            text: "Etags cached in the LRU; 304s short-circuit. One test still red — the \
                   304 path asserts a body that is no longer sent."
                .to_owned(),
        },
    ]
}

/// A session part-way through a task: tool calls, usage with and without a
/// cost, a decision and two sub-agents.
pub fn running_session() -> App {
    let mut app = App::new(Repo {
        name: "example-app".to_owned(),
        branch: Some("main".to_owned()),
    });
    app.extend(&session_events());
    app
}

/// Draws one frame and returns the screen as text, one line per row.
pub fn screen(app: &mut App, width: u16, height: u16) -> String {
    render(drawn(app, width, height).backend().buffer())
}

/// Draws one frame and returns the colour of every cell.
///
/// A theme changes no character on screen, so a picture of the text says
/// nothing about one. This is the picture that does: a legend of every style
/// the frame used, and one character per cell naming which. A palette that
/// moved shows up as a legend line that changed; a colour that reached the
/// wrong half of the screen shows up as a map that did.
pub fn paint(app: &mut App, width: u16, height: u16) -> String {
    let terminal = drawn(app, width, height);
    let buffer = terminal.backend().buffer();

    let mut legend: Vec<Style> = Vec::new();
    let mut rows = Vec::with_capacity(usize::from(buffer.area.height));
    for y in 0..buffer.area.height {
        let mut row = String::with_capacity(usize::from(buffer.area.width));
        for x in 0..buffer.area.width {
            let style = buffer.cell((x, y)).map_or_else(Style::new, Cell::style);
            let at = legend.iter().position(|seen| *seen == style);
            let at = at.unwrap_or_else(|| {
                legend.push(style);
                legend.len() - 1
            });
            row.push(mark(at));
        }
        rows.push(row);
    }

    let legend: Vec<String> = legend
        .iter()
        .enumerate()
        .map(|(at, style)| format!("  {}  {}", mark(at), described(style)))
        .collect();
    format!("{}\n\n{}", legend.join("\n"), rows.join("\n"))
}

/// The character a style is drawn as in a paint map.
///
/// Sixty-two of them, which is far more than a palette of sixteen colours can
/// make out of the handful of modifiers the shell uses. Running out would make
/// two styles read as one, so it panics rather than drawing a map that lies.
fn mark(at: usize) -> char {
    const MARKS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    char::from(
        *MARKS
            .get(at)
            .expect("a frame uses fewer styles than there are marks for them"),
    )
}

/// One style, as a legend line reads it.
fn described(style: &Style) -> String {
    let colour = |colour: Option<Color>| match colour {
        Some(colour) => format!("{colour:?}"),
        // The cell a double-width glyph spills into is left as the terminal's
        // own: nothing is drawn there, so there is no colour to name.
        None => "unset".to_owned(),
    };
    let mut described = format!("{} on {}", colour(style.fg), colour(style.bg));
    if !style.add_modifier.is_empty() {
        described.push_str(&format!(", {:?}", style.add_modifier).to_lowercase());
    }
    described
}

fn drawn(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal =
        Terminal::new(TestBackend::new(width, height)).expect("a test backend cannot fail");
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("a test backend cannot fail");
    terminal
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
