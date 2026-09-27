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
use niobe_tui::app::{App, Commit, Repo, WorkingFile};
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
    ("c1csi", "A\u{9b}31mB"),
    ("vt", "A\x0bB"),
    ("ff", "A\x0cB"),
    ("tab", "A\tB"),
    ("nul", "A\0B"),
];

fn is_dangerous(c: char) -> bool {
    c.is_control() && c != '\n'
        || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}')
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
            },
        ])
    }));
    all.extend(check("filechange", |p| {
        with(vec![Event::FileChange {
            path: format!("src/{p}.rs"),
            added: Some(1),
            removed: Some(1),
            hunks: vec![hunk(1, 1, &[&format!("-old {p}"), &format!("+new {p}")])],
        }])
    }));
    all.extend(check("permission", |p| {
        with(vec![Event::PermissionRequest {
            id: "perm".into(),
            tool: format!("Bash{p}"),
            input: format!("{{\"command\":\"{p}\"}}"),
            target: Some(format!("rm {p}")),
            agent: None,
        }])
    }));
    all.extend(check("decision", |p| {
        with(vec![Event::Decision {
            summary: format!("dec {p}"),
            rationale: Some(format!("why {p}")),
            rejected: vec![format!("rej {p}")],
        }])
    }));
    all.extend(check("agent", |p| {
        with(vec![
            Event::AgentSpawn {
                id: AgentId::from("qa"),
                parent: None,
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
            }],
            commits: vec![Commit {
                hash: "abc1234".into(),
                subject: format!("subj {p}"),
                at: None,
                pushed: Some(false),
            }],
            files: vec![],
        });
        app.apply(&Event::UserMessage { text: "hi".into() });
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
        app.on_key(key(KeyCode::Char('/')));
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
