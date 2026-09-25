// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A recorded `claude` transcript listed, folded and priced.
//!
//! The expected numbers are the ones written into
//! `tests/fixtures/README.md` and derivable from the fixture with `jq`, not
//! numbers this crate produced, so a bug in the fold cannot agree with itself.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::path::{Path, PathBuf};

use niobe_bridge_claude::transcript;
use niobe_core::event::{AgentOutcome, Backend, CostBasis, Event, Mode, SessionMeta, ToolOutcome};
use niobe_core::session::SessionState;
use niobe_core::test_run::{FailedTests, TestCounts};

/// The session in the fixture, as the CLI names it.
const SESSION: &str = "2f6c1e10-8f4b-4d2a-9c3e-7a5b0d1e6f42";

/// The session in the fixture that spawned three sub-agents, whose own
/// transcripts are kept beside it.
const WITH_AGENTS: &str = "7b3e9d20-4c1a-4f5e-9b8d-2e6a1c0f5d73";

/// A directory of transcripts in the shape the CLI writes one.
fn transcripts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcripts")
}

/// Every event the fixture folds to, in order, for a session in `/repo`.
fn folded() -> Vec<Event> {
    folded_session(SESSION)
}

/// Every event session `id` in the fixture folds to, in order.
fn folded_session(id: &str) -> Vec<Event> {
    transcript::events(
        &transcripts().join(format!("{id}.jsonl")),
        "max",
        Path::new("/repo"),
    )
    .expect("the transcript reads")
}

/// What was reported about sub-agent `id`, in order: each report's model,
/// context size and latest line.
type Reports = Vec<(Option<String>, Option<u64>, Option<String>)>;

fn reports_on(events: &[Event], id: &str) -> Reports {
    events
        .iter()
        .filter_map(|event| match event {
            Event::AgentProgress {
                id: agent,
                model,
                context_tokens,
                latest,
            } if agent.as_str() == id => Some((model.clone(), *context_tokens, latest.clone())),
            _ => None,
        })
        .collect()
}

/// Where in `events` the sub-agent `id` was spawned, reported on and ended.
fn positions(events: &[Event], id: &str) -> (Option<usize>, Vec<usize>, Option<usize>) {
    let spawn = events.iter().position(
        |event| matches!(event, Event::AgentSpawn { id: agent, .. } if agent.as_str() == id),
    );
    let reports = events
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            matches!(event, Event::AgentProgress { id: agent, .. } if agent.as_str() == id)
        })
        .map(|(at, _)| at)
        .collect();
    let exit = events.iter().position(
        |event| matches!(event, Event::AgentExit { id: agent, .. } if agent.as_str() == id),
    );
    (spawn, reports, exit)
}

fn usage_records(events: &[Event]) -> Vec<&niobe_core::Usage> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Usage(usage) => Some(usage),
            _ => None,
        })
        .collect()
}

fn warnings(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Error { message, fatal } => {
                assert!(!fatal, "a record the bridge could not read ended the fold");
                Some(message.as_str())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_transcript_is_listed_by_the_id_that_continues_it_and_by_what_was_asked() {
    let listed = transcript::list(&transcripts()).expect("the directory lists");

    // The directory the CLI keeps a session's sub-agents in is not a session
    // of its own.
    let mut ids: Vec<&str> = listed.iter().map(|found| found.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, [SESSION, WITH_AGENTS], "{listed:?}");
    let only = listed
        .iter()
        .find(|found| found.id == SESSION)
        .expect("the session is listed");
    assert_eq!(
        only.first_prompt.as_deref(),
        Some("add an etag to the catalog response"),
        "the `/clear` the CLI expanded was listed as though the operator had typed it"
    );
}

#[test]
fn a_transcript_folds_into_what_the_session_said_and_did() {
    let events = folded();
    let said: Vec<&Event> = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::UserMessage { .. }
                    | Event::AssistantMessage { .. }
                    | Event::ToolCallEnd { .. }
                    | Event::FileChange { .. }
            )
        })
        .collect();

    let [first, second, reply, call, change, closing] = said.as_slice() else {
        panic!("the turns, the call and the change: {said:?}");
    };
    // A turn the CLI wrote for itself is still a turn the model was given, so
    // the history carries it; only the session list passes over it.
    assert!(
        matches!(first, Event::UserMessage { text } if text.starts_with("<command-name>/clear")),
    );
    assert_eq!(
        *second,
        &Event::UserMessage {
            text: "add an etag to the catalog response".to_owned()
        }
    );
    assert_eq!(
        *reply,
        &Event::AssistantMessage {
            text: "I will add the header.".to_owned()
        }
    );
    let Event::ToolCallEnd { name, outcome, .. } = call else {
        panic!("the Edit call ended: {call:?}");
    };
    assert_eq!(name, "Edit");
    assert_eq!(*outcome, ToolOutcome::Ok);
    assert_eq!(
        *change,
        &Event::FileChange {
            // Named the way `git` names it, because the fold was told where
            // the session ran.
            path: "catalog/fetch.ts".to_owned(),
            added: Some(3),
            removed: Some(1),
            hunks: Vec::new(),
        }
    );
    assert!(matches!(closing, Event::AssistantMessage { .. }));
}

#[test]
fn a_transcript_is_captioned_by_the_title_the_cli_gave_it() {
    let events = folded();

    let titles: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::Titled { title } => Some(title.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(titles, ["Etag on the catalog response"]);
    assert_eq!(
        SessionState::replay(&events).caption().as_deref(),
        Some("Etag on the catalog response")
    );
}

#[test]
fn a_transcript_says_what_it_ran_as_and_what_it_ran_on() {
    let events = folded();

    assert_eq!(
        events.first(),
        Some(&Event::ModeSelected { mode: Mode::Plan }),
        "{events:?}"
    );
    let meta = events.iter().find_map(|event| match event {
        Event::SessionMeta(meta) => Some(meta),
        _ => None,
    });
    assert_eq!(
        meta,
        Some(&SessionMeta {
            backend: Backend::Claude,
            profile: "max".to_owned(),
            model: "claude-sonnet-5".to_owned(),
            // The id the file is named by, which is the only thing that can
            // hand the conversation back to the CLI.
            backend_session: Some(SESSION.to_owned()),
        })
    );
}

#[test]
fn a_priced_session_is_counted_from_the_accounting_the_cli_closed_it_with() {
    let events = folded();
    let records = usage_records(&events);

    // The session's messages name `claude-sonnet-5` and the CLI billed it as
    // `claude-sonnet-5[1m]`, which is a different rate for the same work. One
    // record per model it billed, and not one per message as well: adding both
    // would report every token of the session twice, and reading one id as the
    // other would be a guess about a price.
    let named: Vec<&str> = records.iter().map(|usage| usage.model.as_str()).collect();
    assert_eq!(named, ["claude-haiku-4-5", "claude-sonnet-5[1m]"]);
    assert!(
        records.iter().all(|usage| usage.cost_usd.is_some()),
        "the CLI closed the session with what it cost, so nothing here is a floor: {records:?}"
    );
}

#[test]
fn an_imported_session_folds_into_the_totals_the_cli_recorded() {
    let state = SessionState::replay(&folded());
    let totals = state.totals();

    // Exactly what the closing `cost-state` holds, across both models.
    assert_eq!(totals.input, 5 + 900);
    assert_eq!(totals.output, 65 + 10);
    assert_eq!(totals.cache_read, 2_100);
    assert_eq!(totals.cache_write, 150);
    assert_eq!(totals.reasoning, 0);

    assert_eq!(totals.records, 2);
    assert_eq!(
        totals.records_unsettled, 0,
        "the CLI recorded what the session cost, so the figure is not a floor"
    );
    assert!(
        (totals.reported_cost_usd - 0.051).abs() < 1e-9,
        "the session cost {} rather than the 0.051 the CLI recorded",
        totals.reported_cost_usd
    );
}

#[test]
fn an_imported_cost_is_api_equivalent_and_never_measured() {
    let events = folded();
    let priced: Vec<_> = usage_records(&events)
        .into_iter()
        .filter(|usage| usage.cost_usd.is_some())
        .collect();

    assert_eq!(priced.len(), 2, "one per model the session spent on");
    for usage in &priced {
        assert_eq!(
            usage.cost_basis,
            Some(CostBasis::ApiEquivalent),
            "{} was priced as {:?}",
            usage.model,
            usage.cost_basis
        );
    }
}

#[test]
fn a_record_this_version_cannot_read_is_shown_and_not_a_lost_history() {
    let events = folded();

    // A record type nobody has read is a notice: the CLI wrote it, and
    // nothing in it says anything failed.
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::Notice { message } if message.contains("teleport")
        )),
        "{events:?}"
    );
    // A line that is not JSON at all is something that could not be read.
    let complaints = warnings(&events);
    assert_eq!(complaints.len(), 1, "{complaints:?}");
    assert!(complaints[0].contains("could not read"), "{complaints:?}");
}

/// Each sub-agent's model is read off its own messages, which the CLI keeps in
/// a transcript of their own beside the session's. The spawn names none.
#[test]
fn a_read_back_sub_agent_is_named_by_the_model_its_own_transcript_names() {
    let events = folded_session(WITH_AGENTS);

    for (id, model) in [
        ("toolu_sum", "claude-haiku-4-5-20251001"),
        ("toolu_fetch", "claude-opus-5"),
        ("toolu_cache", "claude-opus-5"),
    ] {
        let named: Vec<String> = reports_on(&events, id)
            .into_iter()
            .filter_map(|(model, _, _)| model)
            .collect();
        assert_eq!(named, [model], "{id}: {events:?}");
        let (spawn, reports, _) = positions(&events, id);
        let spawn = spawn.expect("the agent was spawned");
        assert!(
            reports.iter().all(|at| *at > spawn),
            "{id} was reported on before it was spawned"
        );
    }
    assert!(warnings(&events).is_empty(), "{:?}", warnings(&events));
}

/// A finished agent's line is the first line of its own last message — the
/// answer it gave — and never the CLI's word that it finished. One that never
/// finished has no answer to show, and the step it was on is not read back.
#[test]
fn a_read_back_sub_agent_that_finished_shows_its_own_answer() {
    let events = folded_session(WITH_AGENTS);

    for (id, answer) in [
        (
            "toolu_sum",
            Some("The cache is a bounded least-recently-used map over an OrderedDict."),
        ),
        (
            "toolu_fetch",
            Some("fetch() stores a 304 response's empty body over the cached page."),
        ),
        ("toolu_cache", None),
    ] {
        let lines: Vec<String> = reports_on(&events, id)
            .into_iter()
            .filter_map(|(_, _, latest)| latest)
            .collect();
        assert_eq!(lines, answer.into_iter().collect::<Vec<_>>(), "{id}");
        assert!(
            lines.iter().all(|line| !line.contains("finished")),
            "{id} shows the CLI's notice: {lines:?}"
        );
    }
}

/// Both ways the CLI writes an agent's end are read: as a turn of its own
/// when the agent finished between turns, and as an attachment to the running
/// turn when it finished during one. An agent the transcript never says ended
/// is still running; its answer is reported before its end.
#[test]
fn a_read_back_sub_agent_ends_where_the_transcript_says_it_did() {
    let events = folded_session(WITH_AGENTS);

    for id in ["toolu_sum", "toolu_fetch"] {
        let (_, reports, exit) = positions(&events, id);
        let exit = exit.unwrap_or_else(|| panic!("{id} ended: {events:?}"));
        assert!(
            matches!(
                &events[exit],
                Event::AgentExit {
                    outcome: AgentOutcome::Completed,
                    ..
                }
            ),
            "{id}"
        );
        assert!(
            reports.iter().all(|at| *at < exit),
            "{id} reported after its end"
        );
    }
    let (_, _, exit) = positions(&events, "toolu_cache");
    assert_eq!(exit, None, "the cache reviewer never finished");
}

/// The transcript holds no per-agent figure the live stream's context size
/// can be checked against — the CLI's own count in a notification is not what
/// the agent's messages add up to — so a read-back agent reports none.
#[test]
fn a_read_back_sub_agent_reports_no_token_figure() {
    let events = folded_session(WITH_AGENTS);

    for id in ["toolu_sum", "toolu_fetch", "toolu_cache"] {
        assert!(
            reports_on(&events, id)
                .iter()
                .all(|(_, tokens, _)| tokens.is_none()),
            "{id}"
        );
    }
}

/// Where in `events` each tool call started, by its id, in order.
fn call_starts(events: &[Event]) -> Vec<(usize, &str)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(at, event)| match event {
            Event::ToolCallStart { id, .. } => Some((at, id.as_str())),
            _ => None,
        })
        .collect()
}

/// An agent's own calls are in its side file, and read back they fall where
/// the CLI's timestamps put them: after the spawn, interleaved with the other
/// agents' calls and the session's own messages, and before the agent's end.
/// The order is the one the README's `jq` program derives from the fixture.
#[test]
fn a_read_back_sub_agents_own_calls_are_in_the_timeline_in_the_order_the_cli_stamped_them() {
    let events = folded_session(WITH_AGENTS);
    let starts = call_starts(&events);

    let order: Vec<&str> = starts.iter().map(|(_, id)| *id).collect();
    assert_eq!(
        order,
        [
            "toolu_sum",
            "toolu_fetch",
            "toolu_cache",
            "toolu_f1",
            "toolu_c1",
            "toolu_s1",
            "toolu_f2",
            "toolu_c2",
            "toolu_f3",
        ],
        "{events:?}"
    );

    for (agent, own) in [
        ("toolu_sum", &["toolu_s1"][..]),
        ("toolu_fetch", &["toolu_f1", "toolu_f2", "toolu_f3"][..]),
        ("toolu_cache", &["toolu_c1", "toolu_c2"][..]),
    ] {
        let (spawn, _, exit) = positions(&events, agent);
        let spawn = spawn.expect("the agent was spawned");
        for call in own {
            let (at, _) = starts
                .iter()
                .find(|(_, id)| id == call)
                .expect("the agent's call is in the timeline");
            assert!(*at > spawn, "{call} started before {agent} was spawned");
            assert!(
                exit.is_none_or(|exit| *at < exit),
                "{call} started after {agent} ended"
            );
        }
    }

    // The session's own message, stamped between the agents' calls, stays
    // between them.
    let said = events
        .iter()
        .position(|event| {
            matches!(event, Event::AssistantMessage { text } if text.starts_with("Three agents"))
        })
        .expect("the session's message is in the timeline");
    let at = |call: &str| {
        starts
            .iter()
            .find(|(_, id)| *id == call)
            .map(|(at, _)| *at)
            .expect("the call started")
    };
    assert!(at("toolu_c2") < said && said < at("toolu_f3"));
    assert!(warnings(&events).is_empty(), "{:?}", warnings(&events));
}

/// The agent's prompt is the session speaking to the agent, not the operator
/// speaking to the session.
#[test]
fn a_read_back_sub_agents_prompt_is_not_the_operators_turn() {
    let events = folded_session(WITH_AGENTS);

    let turns: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            Event::UserMessage { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        turns,
        [
            "summarize catalog/cache.py, and review cache.py and fetch.py for bugs, in the background"
        ]
    );
}

/// A session the CLI never closed is counted from its messages, and an
/// agent's messages were billed as much as the session's: each is counted
/// once, from the last record the CLI wrote of it, under the model it ran on.
/// The figures are the README's, derived from the fixture with `jq`.
#[test]
fn an_unpriced_read_back_session_counts_its_agents_messages_under_their_own_models() {
    let events = folded_session(WITH_AGENTS);

    let mut by_model: std::collections::BTreeMap<&str, [u64; 4]> =
        std::collections::BTreeMap::new();
    for usage in usage_records(&events) {
        assert_eq!(usage.cost_usd, None, "{usage:?}");
        let counts = by_model.entry(usage.model.as_str()).or_default();
        counts[0] += usage.input;
        counts[1] += usage.output;
        counts[2] += usage.cache_read;
        counts[3] += usage.cache_write;
    }
    assert_eq!(
        by_model,
        std::collections::BTreeMap::from([
            ("claude-haiku-4-5-20251001", [18, 553, 5_701, 6_221]),
            ("claude-opus-5", [18, 2_480, 178_278, 16_268]),
        ])
    );

    let totals = SessionState::replay(&events).totals().clone();
    assert_eq!(
        (
            totals.input,
            totals.output,
            totals.cache_read,
            totals.cache_write
        ),
        (36, 3_033, 183_979, 22_489)
    );
    assert_eq!(totals.cache_write_1h, 22_489);
}

/// The context meter is the session's own conversation; an agent's is a
/// different conversation, and moving the meter to it would show a size the
/// session never had.
#[test]
fn a_read_back_sub_agents_messages_do_not_move_the_sessions_context() {
    let events = folded_session(WITH_AGENTS);
    let without_agents = {
        let dir = copied_without_agents();
        transcript::events(
            &dir.path().join(format!("{WITH_AGENTS}.jsonl")),
            "max",
            Path::new("/repo"),
        )
        .expect("the transcript reads")
    };

    let contexts = |events: &[Event]| -> Vec<Event> {
        events
            .iter()
            .filter(|event| matches!(event, Event::Context(_)))
            .cloned()
            .collect()
    };
    assert_eq!(contexts(&events), contexts(&without_agents));
}

/// The session's file alone, in a directory of its own, with no side files
/// beside it.
fn copied_without_agents() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    std::fs::copy(
        transcripts().join(format!("{WITH_AGENTS}.jsonl")),
        dir.path().join(format!("{WITH_AGENTS}.jsonl")),
    )
    .expect("the session's file is copied");
    dir
}

/// The fixture's session and its side files, copied into a directory of its
/// own with `extra` appended to the session's file.
fn copied_with(extra: &str) -> tempfile::TempDir {
    let dir = copied_without_agents();
    let session = dir.path().join(format!("{WITH_AGENTS}.jsonl"));
    let mut text = std::fs::read_to_string(&session).expect("the copy reads");
    text.push_str(extra);
    text.push('\n');
    std::fs::write(&session, text).expect("the copy is written");

    let from = transcripts().join(WITH_AGENTS).join("subagents");
    let to = dir.path().join(WITH_AGENTS).join("subagents");
    std::fs::create_dir_all(&to).expect("the side files' directory is made");
    for entry in std::fs::read_dir(&from).expect("the side files are listed") {
        let entry = entry.expect("a side file is listed");
        std::fs::copy(entry.path(), to.join(entry.file_name())).expect("a side file is copied");
    }
    dir
}

/// A session the CLI closed is counted from its `cost-state` alone, which
/// already holds what its agents spent: reading their side files adds
/// nothing to it.
#[test]
fn a_priced_read_back_sessions_totals_are_unchanged_by_its_agents_side_files() {
    let cost_state = r#"{"type":"cost-state","sessionId":"7b3e9d20-4c1a-4f5e-9b8d-2e6a1c0f5d73","totalCostUSD":0.5,"modelUsage":{"claude-opus-5[1m]":{"inputTokens":18,"outputTokens":2480,"cacheReadInputTokens":178278,"cacheCreationInputTokens":16268,"webSearchRequests":0,"costUSD":0.49,"contextWindow":1000000},"claude-haiku-4-5-20251001":{"inputTokens":18,"outputTokens":553,"cacheReadInputTokens":5701,"cacheCreationInputTokens":6221,"webSearchRequests":0,"costUSD":0.01,"contextWindow":200000}},"hasUnknownModelCost":false}"#;
    let dir = copied_with(cost_state);
    let events = transcript::events(
        &dir.path().join(format!("{WITH_AGENTS}.jsonl")),
        "max",
        Path::new("/repo"),
    )
    .expect("the transcript reads");

    let named: Vec<&str> = usage_records(&events)
        .iter()
        .map(|usage| usage.model.as_str())
        .collect();
    assert_eq!(
        named,
        ["claude-haiku-4-5-20251001", "claude-opus-5[1m]"],
        "one record per model the CLI billed, and none per message"
    );
    let totals = SessionState::replay(&events).totals().clone();
    assert_eq!(
        (
            totals.input,
            totals.output,
            totals.cache_read,
            totals.cache_write
        ),
        (36, 3_033, 183_979, 22_489)
    );
    assert!((totals.reported_cost_usd - 0.5).abs() < 1e-9);
    assert_eq!(totals.records_unsettled, 0);
}

/// An agent that edits a file changes the repository as much as the session
/// does. The counts are `git diff --numstat` on the same edit, as the README
/// records it.
#[test]
fn a_read_back_sub_agents_edit_is_in_the_change_set() {
    let events = folded_session(WITH_AGENTS);
    let state = SessionState::replay(&events);

    let files: Vec<(&str, u64, u64, u64)> = state
        .files()
        .iter()
        .map(|file| {
            (
                file.path.as_str(),
                file.added,
                file.removed,
                file.added_unstated + file.removed_unstated,
            )
        })
        .collect();
    assert_eq!(files, [("catalog/fetch.py", 2, 1, 0)]);

    let hunks: Vec<usize> = events
        .iter()
        .filter_map(|event| match event {
            Event::FileChange { hunks, .. } => Some(hunks.len()),
            _ => None,
        })
        .collect();
    assert_eq!(hunks, [1], "the CLI's own hunk rides with the change");
}

/// A `cargo test --workspace` whose output the CLI saved to a file, as its own
/// transcript records the call and the result: Claude Code 2.1.282, with the
/// records cut down to what the bridge reads, the preview shortened to its
/// first lines and the report's copy of the output left out. `{saved}` and
/// `{size}` stand for where the file is and how many bytes the CLI said it
/// wrote.
const SAVED_RUN: [&str; 2] = [
    r#"{"type":"assistant","message":{"id":"msg_011CfQUVCNkDYs9dh6ZLTxNa","model":"claude-opus-5-5","role":"assistant","content":[{"type":"tool_use","id":"toolu_01JeJeYUUENSCaXAuZhZP2qH","name":"Bash","input":{"command":"cargo test --workspace","description":"Run workspace tests","timeout":600000}}]}}"#,
    r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_01JeJeYUUENSCaXAuZhZP2qH","type":"tool_result","content":"<persisted-output>\nOutput too large (86KB). Full output saved to: {saved}\n\nPreview (first 2KB):\n    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.60s\n     Running unittests src/lib.rs (target/debug/deps/niobe_bridge_claude-6eb70ea3aac9fd8d)\n\nrunning 151 tests\n</persisted-output>","is_error":false}]},"toolUseResult":{"stdout":"","stderr":"","interrupted":false,"isImage":false,"noOutputExpected":false,"persistedOutputPath":"{saved}","persistedOutputSize":{size}}}"#,
];

/// The bytes the CLI reported writing for that run, which is the size of the
/// file it saved.
const SAVED_BYTES: u64 = 88_059;

/// The file the CLI saved that run to, recorded whole.
fn saved_run() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tool-results/cargo-test-workspace.txt")
}

/// The test runs a transcript of [`SAVED_RUN`] reports, with its report
/// naming `saved` and `size`.
fn saved_test_runs(saved: &Path, size: u64) -> Vec<(Option<TestCounts>, Option<i32>)> {
    let dir = tempfile::tempdir().expect("a temporary directory can be made");
    let path = dir
        .path()
        .join("f9e80817-ed99-446a-ad84-62e8d1cc9018.jsonl");
    let saved = saved.to_str().expect("the fixture path is UTF-8");
    let text: String = SAVED_RUN
        .iter()
        .map(|record| {
            record
                .replace("{saved}", saved)
                .replace("{size}", &size.to_string())
                + "\n"
        })
        .collect();
    std::fs::write(&path, text).expect("the temporary directory is writable");

    transcript::events(&path, "max", Path::new("/repo"))
        .expect("the transcript reads")
        .into_iter()
        .filter_map(|event| match event {
            Event::TestRun {
                counts, exit_code, ..
            } => Some((counts, exit_code)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_test_run_the_cli_saved_to_a_file_is_counted_from_the_file() {
    let runs = saved_test_runs(&saved_run(), SAVED_BYTES);

    let counts = TestCounts {
        passed: 1040,
        failed: 0,
        ignored: 0,
        suites: 32,
    };
    assert_eq!(runs, [(Some(counts), Some(0))]);
}

/// A failing `cargo test --workspace` whose output the CLI cut, as its own
/// transcript records the call and the result: Claude Code 2.1.282, with the
/// records cut down to what the bridge reads. `{cut}` stands for the result
/// the CLI handed the model, which is the fixture
/// `cargo-test-workspace-cut.txt`, byte for byte.
const CUT_RUN: [&str; 2] = [
    r#"{"type":"assistant","message":{"id":"msg_011CfQUDWDMVUkNFbtZKYet1","model":"claude-opus-5-5","role":"assistant","content":[{"type":"tool_use","id":"toolu_01Gbjs71jH54b1JBB9hdcxBr","name":"Bash","input":{"command":"cargo test --workspace","description":"Run workspace tests","timeout":600000}}]}}"#,
    r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":{cut},"is_error":true,"tool_use_id":"toolu_01Gbjs71jH54b1JBB9hdcxBr"}]}}"#,
];

/// What a test run reported: its counts, its exit status, whether it failed
/// and the tests it named.
type Reported = (Option<TestCounts>, Option<i32>, bool, Option<FailedTests>);

/// The test run a transcript of [`CUT_RUN`] reports, with `cut` as the
/// result.
fn cut_test_runs(cut: &str) -> Vec<Reported> {
    let dir = tempfile::tempdir().expect("a temporary directory can be made");
    let path = dir
        .path()
        .join("762e36f1-b86c-4ff7-a551-056eeb6ece36.jsonl");
    let cut = serde_json::Value::String(cut.to_owned()).to_string();
    let text: String = CUT_RUN
        .iter()
        .map(|record| record.replace("{cut}", &cut) + "\n")
        .collect();
    std::fs::write(&path, text).expect("the temporary directory is writable");

    transcript::events(&path, "max", Path::new("/repo"))
        .expect("the transcript reads")
        .into_iter()
        .filter_map(|event| match event {
            Event::TestRun {
                counts,
                exit_code,
                failed,
                failures,
                ..
            } => Some((counts, exit_code, failed, failures)),
            _ => None,
        })
        .collect()
}

fn recorded_cut() -> String {
    fixture("cargo-test-workspace-cut.txt")
}

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(path).expect("the fixture is there")
}

#[test]
fn a_failing_run_the_cli_cut_is_failed_and_counts_nothing() {
    let cut = recorded_cut();
    assert_eq!(
        cut.chars().count(),
        10_040,
        "the result the CLI handed over"
    );

    assert_eq!(
        cut_test_runs(&cut),
        [(None, Some(101), true, None)],
        "the list of what failed was cut away with the end of the run"
    );
}

#[test]
fn a_failing_run_the_cli_cut_names_the_tests_its_kept_end_lists() {
    let cut = fixture("cargo-test-failing-cut.txt");
    assert_eq!(
        cut.chars().count(),
        10_040,
        "the result the CLI handed over"
    );
    assert!(cut.contains("... [12901 characters truncated] ..."));

    let named = FailedTests {
        binary: "--test statement".to_owned(),
        tests: vec![
            "a_statement_line_037_rounds_like_the_ledger".to_owned(),
            "a_statement_line_088_rounds_like_the_ledger".to_owned(),
        ],
    };
    assert_eq!(cut_test_runs(&cut), [(None, Some(101), true, Some(named))]);
}

#[test]
fn a_cut_run_that_shows_no_test_binary_starting_is_not_failed() {
    // The same result stopped where the first test binary would have
    // started: exit 101 with nothing to say a test ran.
    let cut = recorded_cut();
    let (built, _) = cut
        .split_once("     Running unittests")
        .expect("the recording starts a test binary");

    assert_eq!(cut_test_runs(built), [(None, Some(101), false, None)]);
}

#[test]
fn a_saved_test_run_whose_file_is_gone_or_changed_is_not_read() {
    let gone = saved_run().with_file_name("b8u0w4de3.txt");

    assert_eq!(saved_test_runs(&gone, SAVED_BYTES), [(None, Some(0))]);
    assert_eq!(
        saved_test_runs(&saved_run(), SAVED_BYTES - 1),
        [(None, Some(0))],
        "the file holds more than the CLI wrote"
    );
}
