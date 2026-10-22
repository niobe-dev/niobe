// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A recorded session with sub-agents, folded through the bridge into the
//! shell and narrowed to one agent at a time: what the transcript then draws
//! is that agent's own messages and calls — the ones the CLI sent under its
//! `parent_tool_use_id` — and nothing of the session's or another agent's.
//!
//! This is the one place that can say so end to end: the bridge reads which
//! agent a message is from, and the shell draws by it.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use niobe_bridge_claude::Translator;
use niobe_core::event::{AgentId, Event};
use niobe_tui::app::{App, Repo};
use niobe_tui::menu::Action;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const SUB_AGENTS: &str = include_str!("../../niobe-bridge-claude/tests/fixtures/sub-agents.jsonl");

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn narrowed_to_a_sub_agent_the_transcript_draws_its_messages_and_calls_alone() {
    let mut translator = Translator::new("max");
    let events: Vec<Event> = SUB_AGENTS
        .lines()
        .filter(|line| !line.trim().is_empty())
        .flat_map(|line| translator.line(line))
        .collect();
    let agents: Vec<AgentId> = events
        .iter()
        .filter_map(|event| match event {
            Event::AgentSpawn { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    let mut app = App::new(Repo::default());
    app.extend(&events);
    let everything = app.entries_shown().len();

    assert!(agents.len() >= 2, "the recording's agents: {agents:?}");
    for (at, agent) in agents.iter().enumerate() {
        let said: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                Event::AssistantMessage {
                    text,
                    agent: Some(by),
                } if by == agent => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let calls = events
            .iter()
            .filter(|event| {
                matches!(event, Event::ToolCallStart { agent: Some(by), .. } if by == agent)
            })
            .count();

        app.perform(Action::OneAgent);
        for _ in at..agents.len() - 1 {
            app.on_key(key(KeyCode::Up));
        }
        app.on_key(key(KeyCode::Enter));
        let tag = app.only_agent_tag().expect("Enter narrows the transcript");
        let shown = app.entries_shown();

        assert!(
            shown
                .iter()
                .all(|entry| entry.agent.as_deref() == Some(&tag)),
            "{tag}: an entry of someone else's is drawn"
        );
        let drawn_said: Vec<&str> = shown
            .iter()
            .filter(|entry| entry.kind == niobe_tui::app::EntryKind::SubAgent)
            .map(|entry| entry.body.as_str())
            .collect();
        assert_eq!(drawn_said, said, "{tag}: what it said");
        let drawn_calls: usize = shown.iter().map(|entry| entry.calls.len()).sum();
        assert_eq!(drawn_calls, calls, "{tag}: its calls");
        assert!(calls + said.len() > 0, "{tag} did nothing in the recording");

        app.perform(Action::OneAgent);
        assert!(
            app.only_agent_tag().is_none(),
            "the key draws everything again"
        );
        assert_eq!(app.entries_shown().len(), everything);
    }
}
