// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Every kind of event, with every field it can carry set, through a real
//! store file and back: a field the encoding drops is a figure a resumed
//! session no longer has.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use niobe_core::diff::{Hunk, Line};
use niobe_core::event::{
    AgentId, AgentOutcome, Backend, Billing, ChangeScope, CompactTrigger, Context, Event,
    McpServer, McpStatus, Mode, ModelOption, Overage, OverageRefusal, PermissionDecision,
    SessionMeta, SlashCommand, TokenCounts, ToolCallId, ToolOutcome, Usage, UsageWindow,
    UsageWindows,
};
use niobe_core::proposal::{Action, Proposal};
use niobe_core::test_run::{FailedTests, TestCounts};
use niobe_store::Store;

fn text(s: &str) -> String {
    s.to_owned()
}

fn agent() -> Option<AgentId> {
    Some(AgentId::new("agent-7"))
}

/// One value of every variant, each optional field filled where it can be.
///
/// The match in [`every_variant_is_listed`] has no wildcard, so a variant
/// added to [`Event`] fails to compile until it is listed here too.
fn one_of_each() -> Vec<Event> {
    let hunk = Hunk::checked(
        3,
        2,
        3,
        2,
        vec![
            Line::Context(text("fn a() {")),
            Line::Removed(text("    1")),
            Line::Added(text("    2")),
        ],
    )
    .expect("the header agrees with the lines");
    vec![
        Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: text("max"),
            model: text("claude-opus-4-8[1m]"),
            backend_session: Some(text("b6c1")),
        }),
        Event::UserMessage { text: text("hi") },
        Event::PromptNotSent {
            text: text("lost"),
            why: Some(text("the subprocess has gone")),
        },
        Event::Titled {
            title: text("A title"),
        },
        Event::AssistantDelta { text: text("he") },
        Event::AssistantMessage {
            text: text("hello"),
            agent: agent(),
        },
        Event::ToolCallStart {
            id: ToolCallId::new("t1"),
            name: text("Bash"),
            input: text(r#"{"command":"cargo test"}"#),
            summary: Some(text("cargo test")),
            agent: agent(),
        },
        Event::ToolCallEnd {
            id: ToolCallId::new("t1"),
            name: text("Bash"),
            input: text(r#"{"command":"cargo test"}"#),
            output: text("short"),
            bytes: 12_345,
            outcome: ToolOutcome::Failed,
            summary: Some(text("cargo test")),
            exit_code: Some(-101),
            error: Some(text("exit code 101")),
            command: Some(text("cargo test")),
            untrimmed: Some(text("the whole output")),
        },
        Event::TurnEnded,
        Event::Usage(Usage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            cache_write_1h: Some(5),
            reasoning: 6,
            model: text("claude-opus-4-8"),
            cost_usd: Some(0.1 + 0.2),
            settles_model: Some(false),
            fast: true,
        }),
        Event::UsageWindows(UsageWindows {
            five_hour: Some(UsageWindow {
                utilization: 0.33,
                resets_at: Some(1_800_000_000),
            }),
            seven_day: Some(UsageWindow {
                utilization: 1.25,
                resets_at: None,
            }),
            using_overage: true,
            overage: Overage::Refused(OverageRefusal::ZeroLimit),
        }),
        Event::Billing {
            billing: Billing::Metered,
        },
        Event::Context(Context {
            tokens: 120_000,
            model: text("claude-opus-4-8[1m]"),
            window: Some(1_000_000),
        }),
        Event::Cleared,
        Event::CompactionStarted,
        Event::CompactionEnded,
        Event::Compacted {
            trigger: CompactTrigger::Auto,
            before: Some(150_000),
            after: Some(9_000),
        },
        Event::SessionLeft,
        Event::PermissionRequest {
            id: ToolCallId::new("t2"),
            tool: text("Write"),
            input: text(r#"{"file_path":"a.rs","content":"x"}"#),
            target: Some(text("a.rs")),
            proposal: Some(Proposal::new(
                Action::Write {
                    path: text("a.rs"),
                    content: text("x"),
                    existed: Some(true),
                },
                ["file_path", "content"],
            )),
            reason: Some(text("outside the project")),
            agent: agent(),
        },
        Event::PermissionResponse {
            id: ToolCallId::new("t2"),
            decision: PermissionDecision::AllowByRule,
            message: Some(text("use b.rs")),
        },
        Event::PermissionWithdrawn {
            id: ToolCallId::new("t2"),
        },
        Event::ModeSelected { mode: Mode::Plan },
        Event::ModeUnknown,
        Event::ModelSelected {
            model: text("opus"),
        },
        Event::ModelRefused {
            back_to: Some(text("sonnet")),
        },
        Event::FileChange {
            path: text("src/a.rs"),
            added: Some(1),
            removed: Some(1),
            hunks: vec![hunk],
            scope: ChangeScope::AgentMemory,
        },
        Event::TestRun {
            id: ToolCallId::new("t1"),
            counts: Some(TestCounts {
                passed: 10,
                failed: 2,
                ignored: 1,
                suites: 3,
            }),
            exit_code: Some(101),
            failed: true,
            failures: vec![
                FailedTests {
                    binary: text("--lib"),
                    tests: vec![text("a"), text("b")],
                },
                FailedTests {
                    binary: text("--test cli"),
                    tests: vec![text("c")],
                },
            ],
        },
        Event::AgentSpawn {
            id: AgentId::new("agent-8"),
            parent: agent(),
            kind: Some(text("Explore")),
            label: text("look around"),
        },
        Event::AgentProgress {
            id: AgentId::new("agent-8"),
            model: Some(text("claude-haiku-4-5")),
            context_tokens: Some(4_000),
            latest: Some(text("Reading a.rs")),
        },
        Event::AgentExit {
            id: AgentId::new("agent-8"),
            outcome: AgentOutcome::Cancelled,
        },
        Event::Commands {
            commands: vec![SlashCommand {
                name: text("review"),
                description: text("reviews"),
                argument_hint: Some(text("[pr]")),
                mid_prompt: true,
            }],
        },
        Event::McpServers {
            servers: vec![McpServer {
                name: text("docs"),
                status: McpStatus::Other(text("sleeping")),
                source: Some(text("user")),
                tools: vec![text("search")],
                scope: Some(text("project")),
                error: Some(text("refused")),
                endpoint: Some(text("stdio · python3")),
                reports_as: Some(text("docs 1.0")),
            }],
        },
        Event::Models {
            models: vec![ModelOption {
                id: text("opus"),
                name: Some(text("Opus 4.8")),
                description: Some(text("the big one")),
                resolves_to: Some(text("claude-opus-4-8")),
                auto_mode: Some(false),
            }],
        },
        Event::TurnTotalDiffers {
            per_message: TokenCounts {
                input: 1,
                output: 2,
                cache_read: 3,
                cache_write: 4,
                cache_write_1h: Some(1),
            },
            reported: TokenCounts {
                input: 2,
                output: 3,
                cache_read: 4,
                cache_write: 5,
                cache_write_1h: Some(2),
            },
            backend_session: Some(text("b6c1")),
            unfinished: vec![text("msg_1")],
        },
        Event::Notice {
            message: text("retrying"),
        },
        Event::LimitReached {
            message: text("You've hit your session limit · resets 11:10pm"),
        },
        Event::Looked,
        Event::Error {
            message: text("gone"),
            fatal: true,
        },
    ]
}

#[test]
fn every_variant_is_listed() {
    let mut seen = Vec::new();
    for event in one_of_each() {
        let name = match event {
            Event::SessionMeta(_) => "session_meta",
            Event::UserMessage { .. } => "user_message",
            Event::PromptNotSent { .. } => "prompt_not_sent",
            Event::Titled { .. } => "titled",
            Event::AssistantDelta { .. } => "assistant_delta",
            Event::AssistantMessage { .. } => "assistant_message",
            Event::ToolCallStart { .. } => "tool_call_start",
            Event::ToolCallEnd { .. } => "tool_call_end",
            Event::TurnEnded => "turn_ended",
            Event::Usage(_) => "usage",
            Event::UsageWindows(_) => "usage_windows",
            Event::Billing { .. } => "billing",
            Event::Context(_) => "context",
            Event::Cleared => "cleared",
            Event::CompactionStarted => "compaction_started",
            Event::CompactionEnded => "compaction_ended",
            Event::Compacted { .. } => "compacted",
            Event::SessionLeft => "session_left",
            Event::PermissionRequest { .. } => "permission_request",
            Event::PermissionResponse { .. } => "permission_response",
            Event::PermissionWithdrawn { .. } => "permission_withdrawn",
            Event::ModeSelected { .. } => "mode_selected",
            Event::ModeUnknown => "mode_unknown",
            Event::ModelSelected { .. } => "model_selected",
            Event::ModelRefused { .. } => "model_refused",
            Event::FileChange { .. } => "file_change",
            Event::TestRun { .. } => "test_run",
            Event::AgentSpawn { .. } => "agent_spawn",
            Event::AgentProgress { .. } => "agent_progress",
            Event::AgentExit { .. } => "agent_exit",
            Event::Commands { .. } => "commands",
            Event::McpServers { .. } => "mcp_servers",
            Event::Models { .. } => "models",
            Event::TurnTotalDiffers { .. } => "turn_total_differs",
            Event::Notice { .. } => "notice",
            Event::LimitReached { .. } => "limit_reached",
            Event::Looked => "looked",
            Event::Error { .. } => "error",
        };
        assert!(!seen.contains(&name), "{name} is listed twice");
        seen.push(name);
    }
    assert_eq!(seen.len(), 38, "every variant has one value: {seen:?}");
}

#[test]
fn every_variant_with_every_field_comes_back_from_the_store_as_it_went_in() {
    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    let path = dir.path().join("sessions.db");
    let events = one_of_each();
    let session = {
        let store = Store::open(&path).expect("a store opens");
        let session = store.create_session().expect("a session is created");
        for event in &events {
            store
                .append(session, event)
                .expect("a session in a temporary store takes an event");
        }
        session
    };

    let read: Vec<Event> = Store::open(&path)
        .expect("the store opens again")
        .events(session)
        .expect("the session reads back")
        .into_iter()
        .map(|stored| stored.event)
        .collect();

    assert_eq!(read.len(), events.len());
    for (wrote, came_back) in events.iter().zip(&read) {
        assert_eq!(came_back, wrote);
    }
}

/// What older releases wrote, in the shape they wrote it: each still reads,
/// as the event it was.
#[test]
fn events_in_the_shapes_earlier_releases_wrote_still_read() {
    let older = [
        r#"{"type":"model_refused"}"#,
        r#"{"type":"usage","input":1,"output":2,"cache_read":3,"cache_write":4,"reasoning":0,"model":"m","cost_usd":null,"settles_model":true,"cost_basis":"api_equivalent"}"#,
        r#"{"type":"usage","input":1,"output":2,"cache_read":3,"cache_write":4,"reasoning":0,"model":"m","cost_usd":0.5}"#,
        r#"{"type":"test_run","id":"t","counts":null,"failures":{"binary":"--lib","tests":["a"]}}"#,
        r#"{"type":"usage_windows","five_hour":{"utilization":null,"resets_at":1},"seven_day":null,"using_overage":false}"#,
        r#"{"type":"compacted"}"#,
        r#"{"type":"agent_spawn","id":"a","parent":null,"label":"Explore: look"}"#,
        r#"{"type":"tool_call_end","id":"t","name":"Bash","input":"{}","output":"","bytes":0,"outcome":"ok"}"#,
        r#"{"type":"permission_request","id":"t","tool":"Bash","input":"{}"}"#,
        r#"{"type":"session_meta","backend":"claude","profile":"p","model":"m"}"#,
        r#"{"type":"context","tokens":1,"model":"m"}"#,
    ];
    for line in older {
        let event: Result<Event, _> = serde_json::from_str(line);
        assert!(event.is_ok(), "{line} no longer reads: {event:?}");
    }
}
