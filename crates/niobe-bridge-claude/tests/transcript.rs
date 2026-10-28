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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use niobe_bridge_claude::transcript;
use niobe_core::event::{
    AgentOutcome, Backend, CompactTrigger, Event, Mode, SessionMeta, ToolOutcome,
};
use niobe_core::session::SessionState;
use niobe_core::test_run::{FailedTests, TestCounts};

/// The session in the fixture, as the CLI names it.
const SESSION: &str = "2f6c1e10-8f4b-4d2a-9c3e-7a5b0d1e6f42";

/// The session in the fixture that spawned three sub-agents, whose own
/// transcripts are kept beside it.
const WITH_AGENTS: &str = "7b3e9d20-4c1a-4f5e-9b8d-2e6a1c0f5d73";

/// The session in the fixture that ran two shell commands in the background,
/// recorded live as `background-commands.jsonl` beside it.
const BACKGROUND: &str = "df5b30bf-d767-43d8-b7dd-7b101f69c268";

/// The session in the fixture that the CLI closed while a background agent
/// was still writing: the agent's last response is stamped after the
/// session's last stamped record, and before the `cost-state` that bills it.
const CLOSED_UNDER_AN_AGENT: &str = "4d9e2b71-6a3c-4f08-8e15-c2b7a0d94f36";

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

/// Every event the fixture folds to, in order, with the time each was
/// recorded at.
fn timed() -> Vec<transcript::Timed> {
    transcript::timed(
        &transcripts().join(format!("{SESSION}.jsonl")),
        "max",
        Path::new("/repo"),
    )
    .expect("the transcript reads")
}

/// The moment the fixture's records write as `2026-09-18T09:00:<secs>.000Z`:
/// `date -u -d 2026-09-18T09:00:00Z +%s` is 1789722000.
fn at_second(secs: u64) -> Option<SystemTime> {
    Some(UNIX_EPOCH + Duration::from_secs(1_789_722_000 + secs))
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

/// The summary a compacted session carries on from is the CLI's, and the
/// history says the context was compacted rather than putting the summary in
/// the operator's mouth.
#[test]
fn a_compaction_is_reported_as_one_and_not_as_something_the_operator_said() {
    let events = folded();

    let said: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            Event::UserMessage { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        said.iter().all(|text| !text.contains("being continued")),
        "{said:?}"
    );
    assert!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Compacted { .. }))
            .collect::<Vec<_>>()
            == [&Event::Compacted {
                trigger: CompactTrigger::Manual,
                before: Some(48_210),
                after: Some(3_120),
            }],
        "{events:?}"
    );
}

#[test]
fn a_transcript_is_listed_by_the_id_that_continues_it_and_by_what_was_asked() {
    let listed = transcript::list(&transcripts(), Path::new("/repo")).expect("the directory lists");

    // The directory the CLI keeps a session's sub-agents in is not a session
    // of its own.
    let mut ids: Vec<&str> = listed.iter().map(|found| found.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        [SESSION, CLOSED_UNDER_AN_AGENT, WITH_AGENTS, BACKGROUND],
        "{listed:?}"
    );
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
    // Read as the command line that was typed, as a live session sees it.
    assert_eq!(
        *first,
        &Event::UserMessage {
            text: "/clear".to_owned()
        }
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
            text: "I will add the header.".to_owned(),
            agent: None,
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
            scope: niobe_core::event::ChangeScope::Project,
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
        assert!(!reports.is_empty(), "{id} reported nothing");
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
            matches!(event, Event::AssistantMessage { text, agent: None } if text.starts_with("Three agents"))
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

/// How many calls and messages each owner made in `events`, the session's
/// own under `None`, in the order each owner first appeared.
fn owners(events: &[Event]) -> Vec<(Option<&str>, usize, usize)> {
    let mut owners: Vec<(Option<&str>, usize, usize)> = Vec::new();
    for event in events {
        let (agent, call) = match event {
            Event::ToolCallStart { agent, .. } => (agent, true),
            Event::AssistantMessage { agent, .. } => (agent, false),
            _ => continue,
        };
        let agent = agent.as_ref().map(|agent| agent.as_str());
        let at = match owners.iter().position(|(owner, ..)| *owner == agent) {
            Some(at) => at,
            None => {
                owners.push((agent, 0, 0));
                owners.len() - 1
            }
        };
        let (_, calls, messages) = &mut owners[at];
        match call {
            true => *calls += 1,
            false => *messages += 1,
        }
    }
    owners
}

/// A call or message read back from an agent's side file is that agent's:
/// the one whose spawning call the side file's description names. The counts
/// are each file's `tool_use` and `text` blocks, as `jq` counts them.
#[test]
fn a_read_back_sub_agents_calls_and_words_carry_the_agent_that_made_them() {
    let events = folded_session(WITH_AGENTS);
    assert_eq!(
        owners(&events),
        [
            (None, 3, 3),
            (Some("toolu_fetch"), 3, 2),
            (Some("toolu_cache"), 2, 1),
            (Some("toolu_sum"), 1, 1),
        ],
        "{events:?}"
    );
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

/// An agent that wrote after the session's last stamped record is still
/// billed by the `cost-state` the CLI wrote as it left, so its response is
/// folded before that record rather than counted again after it. The totals
/// are the README's for this session.
#[test]
fn an_agent_that_wrote_after_the_sessions_last_stamp_is_billed_once_by_the_closing_cost_state() {
    let events = folded_session(CLOSED_UNDER_AN_AGENT);

    let totals = SessionState::replay(&events).totals().clone();
    assert_eq!(
        (
            totals.input,
            totals.output,
            totals.cache_read,
            totals.cache_write
        ),
        (9, 170, 27_000, 2_800)
    );
    assert!((totals.reported_cost_usd - 0.25).abs() < 1e-9);
    assert_eq!(totals.records_unsettled, 0);
    let (spawn, reports, _) = positions(&events, "toolu_late");
    assert!(
        spawn.is_some() && !reports.is_empty(),
        "the agent's response is still in the timeline: {events:#?}"
    );
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
/// naming `saved` and `size`. The file is copied to where the CLI keeps it,
/// its session's `tool-results/` under its `projects/` directory, since the
/// bridge reads a saved output from nowhere else; where `saved` is missing,
/// the report names a file that is missing there too.
fn saved_test_runs(saved: &Path, size: u64) -> Vec<(Option<TestCounts>, Option<i32>)> {
    let dir = tempfile::tempdir().expect("a temporary directory can be made");
    let session = "f9e80817-ed99-446a-ad84-62e8d1cc9018";
    let path = dir.path().join(format!("{session}.jsonl"));
    let results = dir
        .path()
        .join("projects/-repo")
        .join(session)
        .join("tool-results");
    std::fs::create_dir_all(&results).expect("the temporary directory is writable");
    let laid = results.join(saved.file_name().expect("the fixture names a file"));
    if saved.exists() {
        std::fs::copy(saved, &laid).expect("the fixture is copied");
    }
    let saved = laid.to_str().expect("the temporary path is UTF-8");
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
type Reported = (Option<TestCounts>, Option<i32>, bool, Vec<FailedTests>);

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
        [(None, Some(101), true, Vec::new())],
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
    assert_eq!(cut_test_runs(&cut), [(None, Some(101), true, vec![named])]);
}

#[test]
fn a_cut_run_that_shows_no_test_binary_starting_is_not_failed() {
    // The same result stopped where the first test binary would have
    // started: exit 101 with nothing to say a test ran.
    let cut = recorded_cut();
    let (built, _) = cut
        .split_once("     Running unittests")
        .expect("the recording starts a test binary");

    assert_eq!(cut_test_runs(built), [(None, Some(101), false, Vec::new())]);
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

/// A session closed, resumed and closed again, then resumed once more and
/// still open, as the README records it: two `cost-state` records and a turn
/// after the second.
fn resumed_and_open() -> Vec<Event> {
    transcript::events(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/resumed-open-transcript.jsonl"),
        "max",
        Path::new("/repo"),
    )
    .expect("the transcript reads")
}

/// Each `cost-state` is the session's running total when a process left,
/// and the CLI carries that total on into the process that resumes it. What
/// each covers is counted from it, as the difference from the one before;
/// the turn after the last is covered by none, and is counted from its own
/// message.
#[test]
fn a_turn_after_the_last_accounting_is_counted_from_its_message_and_the_rest_from_the_accounting() {
    let events = resumed_and_open();

    let counted: Vec<(&str, [u64; 4], Option<f64>)> = usage_records(&events)
        .iter()
        .map(|usage| {
            (
                usage.model.as_str(),
                [
                    usage.input,
                    usage.output,
                    usage.cache_read,
                    usage.cache_write,
                ],
                usage.cost_usd,
            )
        })
        .collect();
    assert_eq!(counted.len(), 3, "{counted:?}");
    assert_eq!(
        (counted[0].0, counted[0].1),
        ("claude-sonnet-5-5", [2, 3, 10_341, 9_370])
    );
    assert_eq!(
        (counted[1].0, counted[1].1),
        ("claude-sonnet-5-5", [2, 3, 12_071, 11_518])
    );
    assert_eq!(
        counted[2],
        ("claude-sonnet-5-5", [2, 3, 12_071, 12_221], None),
        "the open turn is priced by nothing the CLI wrote"
    );
    let cost = |at: usize| counted[at].2.expect("an accounting carries what it cost");
    assert!((cost(0) - 0.039_582_2).abs() < 1e-9, "{}", cost(0));
    assert!((cost(1) - 0.048_520_2).abs() < 1e-9, "{}", cost(1));
}

/// A process that ran two turns and left wrote one `cost-state` for both, as
/// the README records it. Neither turn's own tokens are known: the record is
/// the process's whole spend, and drawn under its last turn it would read as
/// that turn's — 58,556 tokens, by
/// `jq -s '[.[] | select(.type=="cost-state") | .modelUsage[] | .inputTokens+.outputTokens+.cacheReadInputTokens+.cacheCreationInputTokens] | add'`,
/// where the second turn's message spent 30,296. The session's total still
/// holds it.
#[test]
fn an_accounting_of_several_turns_is_counted_in_the_session_and_in_none_of_its_turns() {
    let events = transcript::events(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/resumed-transcript.jsonl"),
        "max",
        Path::new("/repo"),
    )
    .expect("the transcript reads");
    let state = SessionState::replay(&events);

    let turns: Vec<Option<u64>> = state.turns().iter().map(|turn| turn.tokens).collect();
    assert_eq!(turns, [None, None], "{:?}", state.turns());
    assert_eq!(state.totals().tokens(), 58_556);
}

/// Where each process ran one turn, each `cost-state` is that turn's bill,
/// and the turn is counted with it: 19,716 tokens, then 43,310 less 19,716,
/// by the same `jq` over `resumed-open-transcript.jsonl`.
#[test]
fn an_accounting_of_one_turn_is_that_turns_spend() {
    let state = SessionState::replay(&resumed_and_open());

    let turns: Vec<Option<u64>> = state.turns().iter().map(|turn| turn.tokens).collect();
    assert_eq!(turns, [Some(19_716), Some(43_310 - 19_716)]);
}

/// The process that resumes a session can be told something before it is
/// asked anything — a local command typed, its mode moved — and the turn the
/// accounting ended is not ended again by the prompt after that: the same
/// two turns, with the same spend, as where nothing came between.
#[test]
fn what_a_resumed_process_records_before_its_first_prompt_ends_no_turn_of_its_own() {
    let recorded = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/resumed-open-transcript.jsonl"),
    )
    .expect("the fixture reads");
    let first_accounting = recorded
        .find(r#"{"type":"cost-state""#)
        .expect("the fixture holds an accounting");
    let resumed_at = first_accounting
        + recorded[first_accounting..]
            .find('\n')
            .expect("the accounting is a whole line")
        + 1;
    for between in [
        concat!(
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/model</command-name>\n<command-args>sonnet</command-args>"}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"<local-command-stdout>Set model to Sonnet 5.5</local-command-stdout>"}}"#,
            "\n",
        ),
        "{\"type\":\"permission-mode\",\"permissionMode\":\"plan\"}\n",
    ] {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let path = dir.path().join("s-1.jsonl");
        let text = [&recorded[..resumed_at], between, &recorded[resumed_at..]].concat();
        std::fs::write(&path, text).expect("the transcript is written");

        let events =
            transcript::events(&path, "max", Path::new("/repo")).expect("the transcript reads");
        let state = SessionState::replay(&events);

        let turns: Vec<Option<u64>> = state.turns().iter().map(|turn| turn.tokens).collect();
        assert_eq!(
            turns,
            [Some(19_716), Some(43_310 - 19_716)],
            "{between}{:?}",
            state.turns()
        );
    }
}

/// The open turn's tokens are measured, so they are in the totals; what it
/// cost was never written down, so the bill is a floor. The totals come to
/// what the CLI recorded once that process left, as the README shows.
#[test]
fn a_resumed_session_still_open_is_counted_whole_and_its_bill_reads_as_a_floor() {
    let totals = SessionState::replay(&resumed_and_open()).totals().clone();

    assert_eq!(
        (
            totals.input,
            totals.output,
            totals.cache_read,
            totals.cache_write
        ),
        (6, 9, 34_483, 33_109)
    );
    assert!(
        (totals.reported_cost_usd - 0.088_102_4).abs() < 1e-9,
        "{}",
        totals.reported_cost_usd
    );
    assert_eq!(totals.records_unsettled, 1);
    assert!(!totals.cost_fully_reported());
}

/// The CLI's own transcript of the session `api-errors.jsonl` recorded; see
/// `tests/fixtures/README.md`.
fn api_errors() -> Vec<Event> {
    transcript::events(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/api-errors-transcript.jsonl"),
        "max",
        Path::new("/repo"),
    )
    .expect("the transcript reads")
}

fn notices(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Notice { message } => Some(message.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn every_retry_in_a_read_back_session_is_an_error_saying_what_the_cli_said_and_which_attempt() {
    let events = api_errors();

    let retries: Vec<&str> = warnings(&events)
        .into_iter()
        .filter(|warning| warning.contains("retrying"))
        .collect();
    assert_eq!(
        retries,
        [
            "a request failed (529 Overloaded), and the CLI is retrying it: attempt 1 of 2, in 0.5 s",
            "a request failed (529 Overloaded), and the CLI is retrying it: attempt 2 of 2, in 1.2 s",
            "a request failed (529 Overloaded), and the CLI is retrying it: attempt 1 of 2, in 0.6 s",
            "a request failed (529 Overloaded), and the CLI is retrying it: attempt 2 of 2, in 1.1 s",
        ]
    );
}

#[test]
fn a_read_back_refusal_and_banner_are_shown_at_the_level_the_cli_gave_them() {
    let events = api_errors();

    assert!(
        warnings(&events).contains(
            &"`claude-sonnet-5` declined to answer, and the CLI has no other model to give the \
              request to"
        ),
        "{events:#?}"
    );
    assert!(
        notices(&events).contains(
            &"Sonnet 5's safeguards stopped the response above · continuing once with that noted"
        ),
        "{events:#?}"
    );
}

#[test]
fn a_read_back_local_commands_output_is_its_answer_and_a_stop_hook_says_nothing() {
    let events = api_errors();

    let said = notices(&events);
    assert!(
        said.iter()
            .any(|notice| notice.starts_with("## Context Usage")),
        "{said:?}"
    );
    // Its two stop hooks ran and say nothing; the only notices are the
    // banner and the output of `/context`.
    assert_eq!(said.len(), 2, "{said:?}");
}

#[test]
fn an_imported_prompt_is_at_the_time_its_record_was_written() {
    // The record holding the prompt is stamped `2026-09-18T09:00:02.000Z`.
    let prompt = timed()
        .into_iter()
        .find(|timed| {
            matches!(&timed.event, Event::UserMessage { text } if text == "add an etag to the catalog response")
        })
        .expect("the fixture's prompt is read");
    assert_eq!(prompt.at, at_second(2));
}

#[test]
fn every_event_one_record_folds_to_has_that_records_time() {
    // The assistant record stamped `09:00:06.000Z` says a line and starts the
    // call `toolu_1`, both in one message.
    let timed = timed();
    let said = timed
        .iter()
        .find(|timed| matches!(&timed.event, Event::AssistantMessage { text, .. } if text == "I will add the header."))
        .expect("the line before the call is read");
    let call = timed
        .iter()
        .find(|timed| matches!(&timed.event, Event::ToolCallStart { id, .. } if id.as_str() == "toolu_1"))
        .expect("the call is read");
    assert_eq!(said.at, at_second(6));
    assert_eq!(call.at, at_second(6));
}

#[test]
fn no_imported_event_is_given_a_time_its_records_do_not_hold() {
    // `jq -r 'select(.timestamp) | .timestamp'` over the fixture, less the
    // line that is not JSON: every stamp it holds is a whole second but two.
    let recorded: Vec<Option<SystemTime>> = [0, 1, 2, 5, 6, 7, 12, 13]
        .into_iter()
        .map(at_second)
        .chain([
            at_second(1).map(|at| at + Duration::from_millis(200)),
            at_second(1).map(|at| at + Duration::from_millis(400)),
        ])
        .collect();
    for timed in timed() {
        assert!(
            timed.at.is_none() || recorded.contains(&timed.at),
            "{:?} at {:?}",
            timed.event,
            timed.at
        );
    }
}

#[test]
fn a_record_with_no_stamp_leaves_its_events_with_no_time() {
    // The `permission-mode` record that puts the session in plan mode carries
    // no `timestamp`, and the line that is not JSON has none to read.
    let timed = timed();
    let mode = timed
        .iter()
        .find(|timed| matches!(timed.event, Event::ModeSelected { mode: Mode::Plan }))
        .expect("the mode is read");
    assert_eq!(mode.at, None);
    let unreadable = timed
        .iter()
        .find(|timed| matches!(&timed.event, Event::Notice { .. }))
        .expect("the line that is not JSON is reported");
    assert_eq!(unreadable.at, None);
}

#[test]
fn a_record_whose_stamp_does_not_parse_still_reads_with_no_time() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let path = dir
        .path()
        .join("0d1f6e2a-1b3c-4d5e-8f70-112233445566.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"type":"user","message":{"role":"user","content":"first"},"timestamp":"the eighteenth of September","cwd":"/repo"}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"second"},"timestamp":"2026-09-18T09:00:02.000Z","cwd":"/repo"}"#,
            "\n",
        ),
    )
    .expect("the transcript is written");
    let prompts: Vec<(String, Option<SystemTime>)> =
        transcript::timed(&path, "max", Path::new("/repo"))
            .expect("a stamp that does not parse does not fail the read")
            .into_iter()
            .filter_map(|timed| match timed.event {
                Event::UserMessage { text } => Some((text, timed.at)),
                _ => None,
            })
            .collect();
    assert_eq!(
        prompts,
        vec![
            ("first".to_owned(), None),
            ("second".to_owned(), at_second(2))
        ]
    );
}

/// A transcript written by the test, under the name the CLI gives one, with
/// each of `sides` as the file of the sub-agent spawned by the call named.
fn constructed(session: &[String], sides: &[(&str, Vec<String>)]) -> (tempfile::TempDir, PathBuf) {
    let id = "5c1e7a42-9d3b-4f60-8a17-3b2d9e0c4f58";
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join(format!("{id}.jsonl"));
    std::fs::write(&path, session.join("\n") + "\n").expect("the session is written");
    if !sides.is_empty() {
        let agents = dir.path().join(id).join("subagents");
        std::fs::create_dir_all(&agents).expect("the side files' directory is made");
        for (at, (call, lines)) in sides.iter().enumerate() {
            std::fs::write(
                agents.join(format!("agent-a{at}.meta.json")),
                format!(r#"{{"toolUseId":"{call}"}}"#),
            )
            .expect("the meta file is written");
            std::fs::write(
                agents.join(format!("agent-a{at}.jsonl")),
                lines.join("\n") + "\n",
            )
            .expect("the side file is written");
        }
    }
    (dir, path)
}

/// An assistant record saying `text`, stamped `at`, in a response of its own:
/// the API never gives two responses one id.
fn said(text: &str, at: &str) -> String {
    format!(
        r#"{{"type":"assistant","message":{{"id":"m-{at}-{text}","model":"claude-opus-5","role":"assistant","content":[{{"type":"text","text":"{text}"}}],"usage":{{"input_tokens":1,"output_tokens":1}}}},"uuid":"u-{at}","timestamp":"{at}","version":"2.1.277"}}"#
    )
}

/// An assistant record spawning the sub-agent `call`, stamped `at`.
fn spawning(call: &str, at: &str) -> String {
    format!(
        r#"{{"type":"assistant","message":{{"id":"s-{call}","model":"claude-opus-5","role":"assistant","content":[{{"type":"tool_use","id":"{call}","name":"Agent","input":{{"subagent_type":"quick-lookup","description":"Look","prompt":"Look."}}}}],"usage":{{"input_tokens":1,"output_tokens":1}}}},"uuid":"u-{call}","timestamp":"{at}","version":"2.1.277"}}"#
    )
}

fn said_in_order(events: &[Event]) -> Vec<(String, bool)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::AssistantMessage { text, agent } => Some((text.clone(), agent.is_some())),
            _ => None,
        })
        .collect()
}

/// An agent's record stamped at the same moment as a record of the session's
/// is not before it: the session's goes first, where the file puts it, and the
/// agent's after.
#[test]
fn an_agents_record_stamped_the_same_moment_as_the_sessions_follows_it() {
    let at = "2026-09-18T09:00:03.000Z";
    let (_dir, path) = constructed(
        &[
            spawning("toolu_a", "2026-09-18T09:00:01.000Z"),
            said("main says", at),
        ],
        &[("toolu_a", vec![said("agent says", at)])],
    );

    let events = transcript::events(&path, "max", Path::new("/repo")).expect("it reads");

    assert_eq!(
        said_in_order(&events),
        [
            ("main says".to_owned(), false),
            ("agent says".to_owned(), true)
        ],
        "{events:#?}"
    );
}

/// The caption and the release warning are the reader's own words, said
/// before any record was read, and no record's time is theirs.
#[test]
fn what_the_reader_says_itself_has_no_time_and_a_record_keeps_its_own() {
    let (_dir, path) = constructed(
        &[
            r#"{"type":"user","message":{"role":"user","content":"add etag support"},"uuid":"u1","timestamp":"2026-09-18T09:00:01.000Z","version":"9.9.999","sessionId":"s"}"#.to_owned(),
        ],
        &[],
    );

    let timed = transcript::timed(&path, "max", Path::new("/repo")).expect("it reads");

    let at = |wanted: fn(&Event) -> bool| {
        timed
            .iter()
            .find(|entry| wanted(&entry.event))
            .map(|entry| entry.at)
    };
    assert_eq!(at(|e| matches!(e, Event::Titled { .. })), Some(None));
    assert_eq!(
        at(|e| matches!(e, Event::Error { .. })),
        Some(None),
        "the release warning: {timed:#?}"
    );
    let stamp = at(|e| matches!(e, Event::UserMessage { .. })).flatten();
    assert_eq!(
        stamp,
        Some(UNIX_EPOCH + Duration::from_secs(1_789_722_001)),
        "{timed:#?}"
    );
}

/// What the CLI's `informational` record says at the level of an error is
/// shown as one, and a `scheduled_task_fire` that says nothing says nothing.
#[test]
fn an_informational_record_keeps_its_level_and_an_empty_fire_is_no_notice() {
    let record = |extra: &str| {
        format!(
            r#"{{"type":"system",{extra},"uuid":"x","timestamp":"2026-09-18T09:00:02.000Z","version":"2.1.277"}}"#
        )
    };
    let (_dir, path) = constructed(
        &[
            record(
                r#""subtype":"informational","content":"the disk is nearly full","level":"error""#,
            ),
            record(r#""subtype":"scheduled_task_fire","content":"   ""#),
            record(r#""subtype":"informational","content":"fine","level":"info""#),
        ],
        &[],
    );

    let events = transcript::events(&path, "max", Path::new("/repo")).expect("it reads");

    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::Error { message, fatal: false } if message == "the disk is nearly full"
        )),
        "an error level is a warning: {events:#?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::Notice { message } if message == "fine"
        )),
        "{events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Notice { message } if message.trim().is_empty())),
        "an empty fire says nothing: {events:#?}"
    );
}

/// Two files of agent records that name one call are two files for one agent,
/// which a map would let replace each other by the directory's order: the first
/// by name is read, deterministically, and the other is said to be passed over.
#[test]
fn two_side_files_naming_one_call_say_which_was_not_read() {
    let at = "2026-09-18T09:00:03.000Z";
    let (_dir, path) = constructed(
        &[spawning("toolu_a", "2026-09-18T09:00:01.000Z")],
        &[
            ("toolu_a", vec![said("first file", at)]),
            ("toolu_a", vec![said("second file", at)]),
        ],
    );

    let events = transcript::events(&path, "max", Path::new("/repo")).expect("it reads");

    assert_eq!(
        said_in_order(&events),
        [("first file".to_owned(), true)],
        "{events:#?}"
    );
    let warned: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            Event::Error { message, .. } => Some(message.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(warned.len(), 1, "{warned:?}");
    assert!(
        warned[0].contains("toolu_a") && warned[0].contains("agent-a1"),
        "{warned:?}"
    );
}

#[test]
fn a_read_back_background_commands_end_is_the_notice_the_live_stream_gave() {
    let read_back = folded_session(BACKGROUND);
    let mut translator = niobe_bridge_claude::Translator::new("max").in_dir("/repo");
    let live: Vec<Event> = include_str!("fixtures/background-commands.jsonl")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .flat_map(|line| translator.line(line))
        .collect();

    assert_eq!(
        notices(&read_back),
        [
            "the background command `sleep 2; echo finished-in-background` finished with exit \
             code 0",
            "the background command `sleep 3; echo about-to-fail; exit 3` failed with exit code 3",
        ]
    );
    assert_eq!(notices(&read_back), notices(&live));
}
