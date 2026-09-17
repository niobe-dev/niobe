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

use niobe_core::event::{
    AgentOutcome, Backend, Event, PermissionDecision, SessionMeta, ToolOutcome, Usage,
};
use niobe_tui::app::{App, Repo};
use niobe_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

fn session_events() -> Vec<Event> {
    vec![
        Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "default".to_owned(),
            model: "opus-5".to_owned(),
            backend_session: None,
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
            cache_write_1h: 0,
            reasoning: 0,
            model: "opus-5".to_owned(),
            cost_usd: Some(0.04),
            cost_basis: None,
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
