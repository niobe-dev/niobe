// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A permission prompt, from the recorded line to the rule in the config.
//!
//! The binary is the only crate that may name the bridge and the shell at
//! once, so it is the only place the whole path can be walked in one test:
//! a `control_request` as the CLI prints it, translated into an event, folded
//! into the shell, answered at the keyboard, and the standing answer written
//! into a config file that is read back.
//!
//! Nothing here spawns `claude`. A test that did would start a real session on
//! the operator's own subscription, and what is being checked is Niobe's half
//! of the exchange, which is fully determined by the recorded line.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use niobe_bridge_claude::Translator;
use niobe_core::event::{Event, PermissionDecision};
use niobe_core::permission::Rule;
use niobe_tui::app::{App, Repo};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// One line of a recorded `claude` session: the CLI stopping a turn on a call
/// it will not make until something answers.
const RECORDED: &str = r#"{"type":"control_request","request_id":"req_1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"cargo test"},"description":"cargo test","tool_use_id":"toolu_1"}}"#;

/// The shell, with the recorded prompt folded into it.
fn shell_on_the_prompt() -> App {
    let mut translator = Translator::new("max");
    let events = translator.line(RECORDED);
    assert!(
        matches!(events.as_slice(), [Event::PermissionRequest { .. }]),
        "the recorded line is a prompt: {events:?}"
    );

    let mut app = App::new(Repo {
        name: "niobe".to_owned(),
        branch: Some("main".to_owned()),
        ..Repo::default()
    })
    .attached();
    app.extend(&events);
    app
}

/// Chooses the answer numbered `number` and confirms it, as the operator does.
fn choose(app: &mut App, number: char) {
    for code in [KeyCode::Char(number), KeyCode::Enter] {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }
}

/// The screen as text, so that what the operator is shown is asserted rather
/// than assumed.
fn screen(app: &mut App) -> String {
    let mut terminal =
        Terminal::new(TestBackend::new(100, 30)).expect("a test backend cannot fail");
    terminal
        .draw(|frame| niobe_tui::ui::draw(frame, app))
        .expect("a test backend cannot fail");
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_recorded_prompt_stops_the_shell_and_shows_what_would_run() {
    let mut app = shell_on_the_prompt();
    let frame = screen(&mut app);

    // The recorded line alone carries no `init`, so nothing has said which
    // backend is asking yet.
    assert!(frame.contains("? agent asks"), "{frame}");
    // Attached to a turn this shell did not start, so there is no number to
    // give it.
    assert!(frame.contains("blocks this turn"), "{frame}");
    assert!(frame.contains("to run Bash"), "{frame}");
    assert!(frame.contains("cargo test"), "{frame}");
    assert_eq!(app.session().pending_permissions().len(), 1);
}

#[test]
fn allowing_the_prompt_answers_the_call_it_gates_and_leaves_no_rule() {
    let mut app = shell_on_the_prompt();

    choose(&mut app, '1');

    assert_eq!(
        app.take_produced(),
        [Event::PermissionResponse {
            id: "toolu_1".into(),
            decision: PermissionDecision::Allow,
            message: None,
        }]
    );
    assert!(app.take_rules().is_empty());
    assert!(app.session().pending_permissions().is_empty());
    assert!(!screen(&mut app).contains("agent asks"));
}

#[test]
fn denying_the_prompt_is_visible_in_the_timeline() {
    let mut app = shell_on_the_prompt();

    choose(&mut app, '4');

    assert_eq!(app.session().permissions_denied(), 1);
    let frame = screen(&mut app);
    assert!(
        frame.contains("denied") && frame.contains("Bash · cargo test"),
        "a refusal is indistinguishable from the agent not acting:\n{frame}"
    );
}

#[test]
fn an_always_answer_survives_a_restart() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = dir.path().join(".niobe").join("config.toml");

    // The session the operator answers "always this target" in.
    let mut app = shell_on_the_prompt();
    choose(&mut app, '3');
    let made = app.take_rules();
    assert_eq!(made, [Rule::targeted("Bash", "cargo test")]);
    for rule in &made {
        niobe_config::remember(&config, rule).expect("the config is written");
    }

    // The next session, which reads that file and never asks again.
    let allowed = niobe_config::Config::read(&config)
        .expect("what was written reads back")
        .expect("the file is there")
        .allowed()
        .clone();
    let mut next = App::new(Repo::default()).attached().with_rules(allowed);
    next.extend(&Translator::new("max").line(RECORDED));
    next.settle_rules();

    assert!(
        next.asking().is_none(),
        "the operator was asked again about a call they had already answered for good"
    );
    assert_eq!(
        next.take_produced(),
        [Event::PermissionResponse {
            id: "toolu_1".into(),
            decision: PermissionDecision::AllowByRule,
            message: None,
        }]
    );
}

#[test]
fn a_rule_covers_the_target_it_names_and_nothing_else() {
    let mut app = shell_on_the_prompt();
    choose(&mut app, '3');

    // A different command under the same tool is a different question.
    app.apply(&Event::PermissionRequest {
        id: "toolu_2".into(),
        tool: "Bash".to_owned(),
        input: r#"{"command":"cargo publish"}"#.to_owned(),
        target: Some("cargo publish".to_owned()),
        agent: None,
    });
    app.settle_rules();

    assert_eq!(
        app.asking().map(|ask| ask.target.clone()),
        Some(Some("cargo publish".to_owned())),
        "a standing answer about one command allowed another"
    );
}

#[test]
fn always_this_tool_answers_every_call_to_it() {
    let mut app = shell_on_the_prompt();
    choose(&mut app, '2');
    assert_eq!(app.take_rules(), [Rule::tool("Bash")]);

    app.apply(&Event::PermissionRequest {
        id: "toolu_2".into(),
        tool: "Bash".to_owned(),
        input: r#"{"command":"cargo publish"}"#.to_owned(),
        target: Some("cargo publish".to_owned()),
        agent: None,
    });
    app.settle_rules();

    assert!(app.asking().is_none());
}

#[test]
fn an_answer_written_at_the_prompt_is_the_answer_the_call_gets() {
    let mut app = shell_on_the_prompt();

    app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    for c in "run only the unit tests".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(
        app.take_produced(),
        [Event::PermissionResponse {
            id: "toolu_1".into(),
            decision: PermissionDecision::Deny,
            message: Some("run only the unit tests".to_owned()),
        }],
        "the words did not go back as the answer to the call"
    );
    assert!(app.session().pending_permissions().is_empty());
    assert_eq!(
        app.session().user_messages(),
        0,
        "the answer became a new turn"
    );
}
