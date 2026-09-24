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

use niobe_core::diff::{Hunk, Line as DiffLine};
use niobe_core::event::{
    AgentOutcome, Backend, Event, Mode, PermissionDecision, SessionMeta, ToolOutcome, Usage,
    UsageWindow, UsageWindows,
};
use std::time::{Duration, Instant, UNIX_EPOCH};

use niobe_tui::app::{App, Commit, Repo, WorkingFile};
use niobe_tui::clock::Clock;
use niobe_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Style};

/// One finished MCP call, which is all the tools section reads: an end with a
/// name, a size and an outcome.
fn mcp_call(name: &str, bytes: u64, outcome: ToolOutcome) -> Event {
    Event::ToolCallEnd {
        id: name.into(),
        name: name.to_owned(),
        input: "{}".to_owned(),
        output: String::new(),
        bytes,
        outcome,
        summary: None,
        exit_code: None,
        error: None,
    }
}

/// A `Read` of `path` that started, for a run of them to end together.
fn read_started(id: &str, path: &str) -> Event {
    Event::ToolCallStart {
        id: id.into(),
        name: "Read".to_owned(),
        input: path.to_owned(),
        summary: Some(path.to_owned()),
    }
}

/// The end of a `Read` of `path` that returned `bytes`.
fn read_ended(id: &str, path: &str, bytes: u64) -> Event {
    Event::ToolCallEnd {
        id: id.into(),
        name: "Read".to_owned(),
        input: path.to_owned(),
        output: String::new(),
        bytes,
        outcome: ToolOutcome::Ok,
        summary: Some(path.to_owned()),
        exit_code: None,
        error: None,
    }
}

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
        // Three reads started together, as a model asks for them in one
        // message: the run the transcript folds into one group.
        read_started("t1", "catalog/fetch.ts"),
        read_started("t1b", "catalog/cache.ts"),
        read_started("t1c", "tests/fetch.test.ts"),
        read_ended("t1", "catalog/fetch.ts", 7_412),
        read_ended("t1b", "catalog/cache.ts", 3_210),
        read_ended("t1c", "tests/fetch.test.ts", 5_020),
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
            settles_model: false,
        }),
        // A plan profile: the windows are what this session is metered
        // against, so they are in the Usage pane where a budget would be.
        // Both resets are a fixed distance ahead of [`READ_AT`], the moment
        // the shell reads this recording at, so the times the pane draws are
        // the same on every machine and in every month.
        Event::UsageWindows(UsageWindows {
            five_hour: Some(UsageWindow {
                utilization: 0.62,
                resets_at: Some(READ_AT + 2 * 3_600 + 59 * 60),
            }),
            seven_day: Some(UsageWindow {
                utilization: 0.18,
                resets_at: Some(READ_AT + 4 * 86_400 + 79 * 60),
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
        // What each agent reported about itself, each leaving something out,
        // so the pictures carry an agent with no step and one with no model
        // as well as one with everything.
        Event::AgentProgress {
            id: "a1".into(),
            model: Some("claude-sonnet-5".to_owned()),
            context_tokens: Some(18_300),
            latest: Some("Reading tests/stream.rs".to_owned()),
        },
        Event::AgentProgress {
            id: "a2".into(),
            model: Some("claude-haiku-4-5-20251001".to_owned()),
            context_tokens: Some(4_100),
            latest: None,
        },
        Event::AgentExit {
            id: "a2".into(),
            outcome: AgentOutcome::Completed,
        },
        // A third agent, so the pane's pictures carry all three states an
        // agent can be drawn in rather than only the two that went well.
        Event::AgentSpawn {
            id: "a3".into(),
            parent: None,
            label: "doc-writer → docs/etags.md".to_owned(),
        },
        Event::AgentProgress {
            id: "a3".into(),
            model: None,
            context_tokens: None,
            latest: Some("Notion 404, gave up after 2 retries".to_owned()),
        },
        Event::AgentExit {
            id: "a3".into(),
            outcome: AgentOutcome::Failed,
        },
        Event::Decision {
            summary: "Key the cache on the request URL, not the manifest id — two \
                      manifests can share an id across catalogs."
                .to_owned(),
            rationale: None,
            rejected: vec![],
        },
        // Three calls to one MCP server, one of which failed: what the tools
        // section has to collapse into a single family row carrying its own
        // failure count.
        mcp_call(
            "mcp__claude_ai_Notion__notion-search",
            1_100,
            ToolOutcome::Ok,
        ),
        mcp_call(
            "mcp__claude_ai_Notion__notion-fetch",
            2_600,
            ToolOutcome::Ok,
        ),
        Event::ToolCallEnd {
            id: "mcp__claude_ai_Notion__notion-update-page".into(),
            name: "mcp__claude_ai_Notion__notion-update-page".to_owned(),
            input: "{}".to_owned(),
            output: "404 object_not_found — the page is not shared with the integration".to_owned(),
            bytes: 96,
            outcome: ToolOutcome::Failed,
            summary: None,
            exit_code: None,
            error: Some(
                "404 object_not_found — the page is not shared with the integration".to_owned(),
            ),
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
            message: None,
        },
        Event::ToolCallEnd {
            id: "t2".into(),
            name: "Edit".to_owned(),
            input: "catalog/fetch.ts".to_owned(),
            output: "+6 −2".to_owned(),
            bytes: 640,
            outcome: ToolOutcome::Ok,
            summary: None,
            exit_code: None,
            error: None,
        },
        // Two hunks, as the backend reports a change: what the transcript
        // draws under the call, numbered on each side, with the lines between
        // the hunks elided.
        Event::FileChange {
            path: "catalog/fetch.ts".to_owned(),
            added: Some(6),
            removed: Some(2),
            hunks: vec![
                hunk(
                    40,
                    40,
                    &[
                        " export async function fetchCached(url: string, init: RequestInit) {",
                        "   const cached = cache.get(url);",
                        "   const headers = new Headers(init.headers);",
                        "-  if (cached) headers.set(\"If-None-Match\", cached.etag);",
                        "-  const res = await fetch(url, init);",
                        "+  if (cached?.etag) {",
                        "+    headers.set(\"If-None-Match\", cached.etag);",
                        "+  }",
                        "+  const res = await fetch(url, { ...init, headers });",
                        "+  if (res.status === 304 && cached) return cached.body;",
                        "   return res;",
                    ],
                ),
                hunk(
                    112,
                    115,
                    &[
                        "   cache.set(url, {",
                        "+    etag: res.headers.get(\"ETag\"),",
                        "     body,",
                    ],
                ),
            ],
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
            hunks: Vec::new(),
        },
        Event::AssistantMessage {
            text: "The notebook that demonstrates the fetcher needs the new call shape.".to_owned(),
        },
        Event::FileChange {
            path: "docs/notebooks/catalog.ipynb".to_owned(),
            added: None,
            removed: None,
            hunks: Vec::new(),
        },
        Event::ToolCallStart {
            id: "t3".into(),
            name: "Bash".to_owned(),
            input: "npm test -- fetch".to_owned(),
            summary: None,
        },
        Event::ToolCallEnd {
            id: "t3".into(),
            name: "Bash".to_owned(),
            input: "npm test -- fetch".to_owned(),
            output: "Exit code 1\n1 failing: fetch returns the cached body on a 304".to_owned(),
            bytes: 2_048,
            outcome: ToolOutcome::Failed,
            summary: None,
            exit_code: Some(1),
            error: Some("1 failing: fetch returns the cached body on a 304".to_owned()),
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
            settles_model: false,
        }),
        Event::AssistantMessage {
            text: "Etags cached in the LRU; 304s short-circuit. One test still red — the \
                   304 path asserts a body that is no longer sent."
                .to_owned(),
        },
    ]
}

/// When the shell reads the session below: 13:41 on a Friday twenty thousand
/// days after the epoch.
///
/// A fixed moment, in a clock that never moves for daylight saving, because
/// the pane draws when a window comes back and a picture of the screen cannot
/// depend on the day the test ran or the machine it ran on.
const READ_AT: u64 = 20_000 * 86_400 + 13 * 3_600 + 41 * 60;

/// How long before it is read the session's events happened: `1m 42s`, which
/// is what the sub-agent still running has been running for.
const RAN_FOR: u64 = 102;

/// The twenty-three files a read of the repository reported, written out so
/// that the pictures cover what the pane has to get right: several
/// directories, a file at the repository root, a file git counts no lines in,
/// a file that changed by no lines at all, a rename, and a path far longer
/// than the column it has to fit in.
fn working_tree() -> Vec<WorkingFile> {
    let counted = |path: &str, added: u64, removed: u64| WorkingFile {
        path: path.to_owned(),
        added: Some(added),
        removed: Some(removed),
    };
    vec![
        WorkingFile {
            path: "CHANGELOG.md".to_owned(),
            added: Some(0),
            removed: Some(0),
        },
        counted("catalog/fetch.ts", 149, 12),
        counted("catalog/etag.ts", 88, 41),
        counted("catalog/cache/lru.ts", 31, 9),
        counted("catalog/cache/index.ts", 12, 0),
        counted("docs/caching.md", 64, 22),
        counted("docs/adr/0004-etags.md", 34, 1),
        WorkingFile {
            path: "docs/diagrams/cache.png".to_owned(),
            added: None,
            removed: None,
        },
        counted("server/handlers/manifest.ts", 97, 4),
        counted("server/handlers/health.ts", 3, 19),
        counted("server/middleware/etag.ts", 41, 37),
        counted("server/index.ts", 8, 6),
        counted("tests/etag.test.ts", 122, 8),
        counted("tests/fetch.test.ts", 76, 14),
        counted("tests/helpers.ts", 5, 2),
        counted(
            "tests/fixtures/recorded_catalog_2026-09-18_manifest_not_modified.json",
            97,
            0,
        ),
        counted(
            "tests/fixtures/manifest.json => tests/fixtures/manifest.v2.json",
            6,
            3,
        ),
        counted("web/components/Catalog.tsx", 54, 31),
        counted("web/components/Manifest.tsx", 18, 7),
        counted("web/hooks/useCatalog.ts", 27, 18),
        counted("web/styles/catalog.css", 14, 9),
        counted("scripts/seed.ts", 22, 5),
        counted("scripts/verify.ts", 9, 11),
    ]
}

/// What a read of the repository hands the shell part-way through a task: a
/// branch that is ahead of its upstream, a working tree with more files in it
/// than the pane can show, and commits this session made, one of them still
/// unpushed.
fn read_repository() -> Repo {
    Repo {
        name: "example-app".to_owned(),
        branch: Some("main".to_owned()),
        read: true,
        ahead: Some(3),
        behind: Some(0),
        working: working_tree(),
        commits: vec![
            Commit {
                hash: "9f2c1ab".to_owned(),
                subject: "feat: keep the etag beside the body".to_owned(),
                // Dated against the same fixed moment the session is read at,
                // so the ages in the pictures do not move with the clock.
                at: Some(UNIX_EPOCH + Duration::from_secs(READ_AT - 12 * 60)),
                pushed: Some(false),
            },
            Commit {
                hash: "41de07c".to_owned(),
                subject: "test: a 304 is answered from the cache".to_owned(),
                at: Some(UNIX_EPOCH + Duration::from_secs(READ_AT - 2 * 3_600)),
                pushed: Some(true),
            },
        ],
    }
}

/// A session part-way through a task: tool calls, usage with and without a
/// cost, a decision and two sub-agents.
/// A hunk written the way a unified diff prints one, each line behind its
/// ` `, `-` or `+`.
pub fn hunk(old_start: u64, new_start: u64, lines: &[&str]) -> Hunk {
    let lines: Vec<DiffLine> = lines
        .iter()
        .map(|line| match line.split_at(1) {
            ("-", text) => DiffLine::Removed(text.to_owned()),
            ("+", text) => DiffLine::Added(text.to_owned()),
            (_, text) => DiffLine::Context(text.to_owned()),
        })
        .collect();
    let old = lines
        .iter()
        .filter(|l| !matches!(l, DiffLine::Added(_)))
        .count() as u64;
    let new = lines
        .iter()
        .filter(|l| !matches!(l, DiffLine::Removed(_)))
        .count() as u64;
    Hunk::checked(old_start, old, new_start, new, lines).expect("written to agree with itself")
}

pub fn running_session() -> App {
    session_read_at_a_fixed_moment(&session_events())
}

/// How far apart a tool call's start and end land for each event between
/// them, so that every row in the transcript has a duration to draw.
/// The question that gates a call steps with it, because an answer starts
/// the call's clock again.
const CALL_STEP_MS: u64 = 400;

/// The session folded at a fixed moment and read at a fixed moment.
///
/// The events land a hundred and two seconds before the shell reads them, so
/// the figures measured between the two — the time a decision was recorded at,
/// how long the agent still running has been running — are real durations in
/// the pictures rather than zeroes. A tool call's start and end, and the
/// question between them, are the exception: each lands [`CALL_STEP_MS`]
/// later for every event folded in before it, so how long a call ran is a
/// real duration too, while every figure the panes read stays at the one
/// moment.
fn session_read_at_a_fixed_moment(events: &[Event]) -> App {
    let clock = Clock::fixed(0).expect("UTC is an offset");
    let mut app = App::new(read_repository()).with_clock(clock.clone());
    let moment = |millis| clock.at(UNIX_EPOCH + Duration::from_millis(millis));
    let folded = (READ_AT - RAN_FOR) * 1_000;
    app.tick(Instant::now(), Some(moment(folded)));
    for (step, event) in (0..).zip(events) {
        let at = match event {
            Event::ToolCallStart { .. }
            | Event::ToolCallEnd { .. }
            | Event::PermissionRequest { .. }
            | Event::PermissionResponse { .. } => folded + step * CALL_STEP_MS,
            _ => folded,
        };
        app.apply_at(event, moment(at));
    }
    app.tick(Instant::now(), Some(moment(READ_AT * 1_000)));
    app
}

/// The same session on a profile no backend meters: every event above except
/// the one that reports the plan's windows.
///
/// A metered profile has no windows, and neither has a CLI release that does
/// not report them. What the pane must not do is stand a `0%` in for either.
pub fn unmetered_session() -> App {
    let events: Vec<Event> = session_events()
        .into_iter()
        .filter(|event| !matches!(event, Event::UsageWindows(_)))
        .collect();
    session_read_at_a_fixed_moment(&events)
}

/// A reply written the way the assistant writes one: a heading, emphasis,
/// code spans, a nested list, a table and a fenced code block.
pub const MARKDOWN_REPLY: &str = "## What changed

**How it works:** the fetcher keeps the `etag` beside the body, so a *304* is \
answered from the LRU without reading the body again.

- `catalog/fetch.ts` keeps the etag
  - and sends `If-None-Match`
- `etag.test.ts` asserts the cached body

| file | lines |
|---|---:|
| fetch.ts | +38 |
| etag.test.ts | +24 |

```ts
if (res.status === 304) return cached.body;
```
";

/// The running session after one more prompt, answered in [`MARKDOWN_REPLY`],
/// scrolled to the reply.
pub fn session_with_a_markdown_reply() -> App {
    let mut app = running_session();
    app.apply(&Event::UserMessage {
        text: "What changed?".to_owned(),
    });
    app.apply(&Event::AssistantMessage {
        text: MARKDOWN_REPLY.to_owned(),
    });
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

/// Draws one frame and returns the style of every cell, row by row.
pub fn styles(app: &mut App, width: u16, height: u16) -> Vec<Style> {
    let terminal = drawn(app, width, height);
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(Cell::style)
        .collect()
}

/// Draws one frame and returns the style of the first cell of the first place
/// `text` appears on it, reading row by row.
pub fn style_at(app: &mut App, width: u16, height: u16, text: &str) -> Option<Style> {
    let terminal = drawn(app, width, height);
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height).find_map(|y| {
        let row: Vec<&Cell> = (0..buffer.area.width)
            .filter_map(|x| buffer.cell((x, y)))
            .collect();
        let symbols: Vec<&str> = row.iter().map(|cell| cell.symbol()).collect();
        (0..row.len()).find_map(|x| {
            symbols[x..]
                .concat()
                .starts_with(text)
                .then(|| row[x].style())
        })
    })
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
