// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A recorded `claude` stream translated and folded.
//!
//! The expected numbers are the ones written into
//! `tests/fixtures/README.md` and derivable from the fixture with `jq`, not
//! numbers this crate produced, so a bug in the translation cannot agree with
//! itself.

use niobe_bridge_claude::Translator;
use niobe_core::event::{
    AgentOutcome, Backend, CostBasis, Event, PermissionDecision, ToolOutcome, UsageWindow,
};
use niobe_core::session::SessionState;

/// A two-turn session as the CLI prints it.
const FIXTURE: &str = include_str!("fixtures/stream-json.jsonl");
const SUB_AGENTS: &str = include_str!("fixtures/sub-agents.jsonl");

/// Every event the fixture translates to, in order.
fn translated() -> Vec<Event> {
    let mut translator = Translator::new("max");
    FIXTURE
        .lines()
        .filter(|line| !line.trim().is_empty())
        .flat_map(|line| translator.line(line))
        .collect()
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
                assert!(
                    !fatal,
                    "a message the bridge could not read ended the session"
                );
                Some(message.as_str())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn the_recorded_usage_windows_are_what_the_session_is_metered_against() {
    let state = SessionState::replay(&translated());
    let windows = state
        .usage_windows()
        .expect("the recording carries a rate_limit_event");

    assert_eq!(
        windows.five_hour,
        Some(UsageWindow {
            utilization: 0.68,
            resets_at: Some(1_789_689_600),
        })
    );
    assert_eq!(
        windows.seven_day,
        Some(UsageWindow {
            utilization: 0.27,
            resets_at: Some(1_790_118_000),
        })
    );
    assert!(
        !windows.using_overage,
        "the recording is of a plan inside its windows"
    );
}

#[test]
fn a_recorded_stream_folds_into_the_totals_the_cli_reported() {
    let state = SessionState::replay(&translated());
    let totals = state.totals();

    // The six message_delta records, plus the tokens `modelUsage` reported for
    // a model that never produced a message of its own.
    assert_eq!(totals.input, 9 + 900);
    assert_eq!(totals.output, 115 + 10);
    assert_eq!(totals.cache_read, 5_700);
    assert_eq!(totals.cache_write, 185);
    assert_eq!(
        totals.reasoning, 0,
        "thinking tokens are a share of the output"
    );

    // Six per-message records with no money on them, and one cost record per
    // model per turn that had something new to report.
    assert_eq!(totals.records, 9);
    assert_eq!(
        totals.records_unsettled, 0,
        "every message record's model was settled by a cost record, so the \
         CLI's figure is the session's cost and not a floor under it"
    );
    assert!(totals.unsettled.is_empty());
    assert!(
        (totals.reported_cost_usd - 0.091).abs() < 1e-9,
        "the session cost {} rather than the 0.091 the CLI reported",
        totals.reported_cost_usd
    );
}

/// While a turn is in flight the CLI has reported its tokens and none of its
/// money, and what the operator is owed is a running figure rather than
/// nothing. The fold prices none of it; it says which tokens no cost covers,
/// in the shape a price table takes.
#[test]
fn a_turn_in_flight_says_which_tokens_no_cost_covers() {
    let events = translated();
    let first_cost = events
        .iter()
        .position(|event| matches!(event, Event::Usage(usage) if usage.cost_usd.is_some()))
        .expect("the recording closes a turn with a cost");

    let mid_turn = SessionState::replay(&events[..first_cost]);
    let totals = mid_turn.totals();

    assert!(totals.records_unsettled > 0, "no cost has landed yet");
    let owed: u64 = totals.unsettled.values().map(|usage| usage.tokens()).sum();
    assert_eq!(
        owed,
        totals.tokens(),
        "every token reported so far is still owed for"
    );
    assert!(
        totals
            .unsettled
            .values()
            .all(|usage| usage.cost_usd.is_none()),
        "what is owed for carries no cost of its own"
    );
}

/// Anthropic bills a cache write bought for an hour at twice the input rate
/// and one bought for five minutes at 1.25×, so which lifetime a write was
/// bought for is the difference between a bill and a guess. The CLI states it
/// per message, in `cache_creation`, and the recording carries both.
#[test]
fn each_message_says_which_lifetime_its_cache_writes_were_bought_for() {
    let events = translated();
    let split: Vec<(u64, u64)> = usage_records(&events)
        .into_iter()
        .filter(|usage| usage.cost_usd.is_none())
        .map(|usage| (usage.cache_write, usage.cache_write_1h))
        .collect();

    // `msg_2`'s 50 writes were bought for five minutes; every other message's
    // were bought for the hour. Both readings are in `stream-json.jsonl`, and
    // the table in `tests/fixtures/README.md` is where they are written down.
    assert_eq!(
        split,
        [(100, 100), (50, 0), (10, 10), (10, 10), (10, 10), (5, 5)]
    );

    let state = SessionState::replay(&events);
    let totals = state.totals();
    assert_eq!(totals.cache_write, 185);
    assert_eq!(
        totals.cache_write_1h, 135,
        "185 written, of which 50 for five minutes"
    );
}

#[test]
fn the_per_message_tokens_add_up_to_what_the_cli_reported_for_the_turn() {
    // The bridge checks this itself on every `result` and says so when it
    // fails, so the absence of that warning is the assertion.
    let events = translated();
    let complaints: Vec<&str> = warnings(&events)
        .into_iter()
        .filter(|w| w.contains("do not add up"))
        .collect();

    assert!(complaints.is_empty(), "{complaints:?}");
}

#[test]
fn usage_is_folded_from_the_stream_events_not_from_the_assistant_snapshot() {
    // `msg_1` carries `usage.output_tokens: 2` on its `assistant` line — the
    // count at the moment that line was written — and finished at 40. Folding
    // the snapshot is the bug this asserts against.
    let events = translated();
    let first = usage_records(&events)
        .first()
        .copied()
        .cloned()
        .expect("the first message produced a usage record");

    assert_eq!(first.output, 40);
    assert_eq!(first.input, 3);
    assert_eq!(first.cache_read, 1_000);
    assert_eq!(first.cache_write, 100);
    assert_eq!(
        first.cache_write_1h, 100,
        "the CLI bought the hour, not the five minutes"
    );
    assert_eq!(first.model, "claude-sonnet-5");
}

#[test]
fn a_plan_backends_cost_is_api_equivalent_and_never_measured() {
    let events = translated();
    let priced: Vec<_> = usage_records(&events)
        .into_iter()
        .filter(|usage| usage.cost_usd.is_some())
        .collect();

    assert_eq!(
        priced.len(),
        3,
        "one per model per turn that cost something new"
    );
    for usage in &priced {
        assert_eq!(
            usage.cost_basis,
            Some(CostBasis::ApiEquivalent),
            "{} was priced as {:?}",
            usage.model,
            usage.cost_basis
        );
    }
    assert!(
        usage_records(&events)
            .iter()
            .all(|usage| usage.cost_basis != Some(CostBasis::Measured)),
        "a figure the CLI computed from list prices was stored as money that moved"
    );
}

#[test]
fn a_session_cost_that_runs_on_is_reported_as_what_the_turn_added() {
    let events = translated();
    let sonnet: Vec<f64> = usage_records(&events)
        .into_iter()
        .filter(|usage| usage.model == "claude-sonnet-5")
        .filter_map(|usage| usage.cost_usd)
        .collect();

    // `modelUsage` ran 0.05 then 0.09 for the session, so the turns cost 0.05
    // and 0.04. Adding the two totals would have reported 0.14.
    assert_eq!(sonnet.len(), 2);
    assert!((sonnet[0] - 0.05).abs() < 1e-9, "{sonnet:?}");
    assert!((sonnet[1] - 0.04).abs() < 1e-9, "{sonnet:?}");
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
fn a_message_type_the_bridge_does_not_know_is_a_notice_and_not_a_crash() {
    let events = translated();

    // A shape nobody read stays in front of the operator, as a notice: the
    // CLI did not say anything went wrong.
    assert!(
        notices(&events)
            .iter()
            .any(|n| n.contains("a_message_type_from_a_later_version")),
        "{events:?}"
    );
    // A line that is not JSON at all is something that could not be read.
    let complaints = warnings(&events);
    assert_eq!(complaints.len(), 1, "{complaints:?}");
    assert!(complaints[0].contains("could not read"), "{complaints:?}");
}

#[test]
fn what_the_session_ran_comes_from_the_cli_not_from_the_profile() {
    let events = translated();
    let Some(Event::SessionMeta(meta)) = events.first() else {
        panic!("the first event says what is running: {:?}", events.first());
    };

    assert_eq!(meta.backend, Backend::Claude);
    assert_eq!(meta.profile, "max");
    assert_eq!(meta.model, "claude-sonnet-5");
    assert_eq!(meta.backend_session.as_deref(), Some("s-1"));
}

#[test]
fn tool_calls_and_their_results_reach_the_timeline_in_full() {
    let events = translated();
    let state = SessionState::replay(&events);
    let tools = state.tools();

    assert_eq!(tools.started, 3);
    assert_eq!(tools.finished, 3);
    assert_eq!(
        tools.denied, 1,
        "a call the CLI refused read as a tool that broke rather than one that was not allowed"
    );
    assert_eq!(tools.failed, 0);
    assert_eq!(tools.unmatched_ends, 0);
    assert_eq!(tools.by_name.get("Read"), Some(&1));
    assert_eq!(tools.by_name.get("Bash"), Some(&1));
    assert_eq!(tools.by_name.get("Task"), Some(&1));

    let read = events
        .iter()
        .find_map(|event| match event {
            Event::ToolCallEnd { name, .. } if name == "Read" => Some(event.clone()),
            _ => None,
        })
        .expect("the Read call finished");
    let Event::ToolCallEnd {
        input,
        output,
        bytes,
        outcome,
        ..
    } = read
    else {
        panic!("a tool call end");
    };
    assert_eq!(input, r#"{"file_path":"/repo/notes.txt"}"#);
    assert_eq!(output, "line one\nline two");
    assert_eq!(bytes, 17);
    assert_eq!(outcome, ToolOutcome::Ok);
}

#[test]
fn a_task_call_is_a_sub_agent_as_well_as_a_tool_call() {
    let events = translated();
    let state = SessionState::replay(&events);

    assert_eq!(state.agents_spawned(), 1);
    assert_eq!(state.agents_completed(), 1);
    assert!(state.running_agents().is_empty());
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::AgentSpawn { label, .. } if label == "check the tests"
        )),
        "the sub-agent was named by what it was asked to do"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::AgentExit { outcome, .. } if *outcome == AgentOutcome::Completed))
    );
}

#[test]
fn a_call_the_cli_refused_is_recorded_once_though_it_is_reported_twice() {
    // The CLI announces the refusal as it happens and lists it again in the
    // turn's closing `result`. Counting both would double every denial.
    let events = translated();
    let state = SessionState::replay(&events);

    assert_eq!(state.permissions_denied(), 1);
    assert!(events.iter().any(|event| matches!(
        event,
        Event::PermissionResponse { decision, .. } if *decision == PermissionDecision::Deny
    )));
}

#[test]
fn a_prompt_the_cli_is_waiting_on_is_asked_and_kept_to_be_answered() {
    let mut translator = Translator::new("max");
    let events: Vec<Event> = FIXTURE
        .lines()
        .filter(|line| !line.trim().is_empty())
        .flat_map(|line| translator.line(line))
        .collect();

    let asked: Vec<&Event> = events
        .iter()
        .filter(|event| matches!(event, Event::PermissionRequest { .. }))
        .collect();
    let Some(Event::PermissionRequest {
        id,
        tool,
        input,
        target,
    }) = asked.first().copied()
    else {
        panic!("the CLI asked before it read the file: {asked:?}");
    };

    assert_eq!(id.as_str(), "toolu_read");
    assert_eq!(tool, "Read");
    assert_eq!(input, r#"{"file_path":"/repo/notes.txt"}"#);
    assert_eq!(
        target.as_deref(),
        Some("/repo/notes.txt"),
        "a standing answer could not be written about this call"
    );

    // The event says what to show; this says what to address the answer to.
    let waiting = translator.take_asked();
    assert_eq!(waiting.len(), 1, "{waiting:?}");
    assert_eq!(waiting[0].id.as_str(), "toolu_read");
    assert_eq!(waiting[0].request_id, "req_1");
    assert_eq!(
        waiting[0].input,
        serde_json::json!({ "file_path": "/repo/notes.txt" }),
        "an approval would have sent back arguments the operator never saw"
    );
    assert!(
        translator.take_asked().is_empty(),
        "the same prompt would be answered twice"
    );
}

#[test]
fn a_prompt_with_no_answer_in_the_recording_stays_pending() {
    // The fixture is what the CLI printed; Niobe's answer goes the other way,
    // on standard input, so folding the recording alone leaves the prompt up.
    let state = SessionState::replay(&translated());

    assert_eq!(state.permission_requests(), 2);
    assert_eq!(state.permissions_denied(), 1);
    assert_eq!(
        state
            .pending_permissions()
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        ["toolu_read"]
    );
}

#[test]
fn a_compaction_is_a_notice_rather_than_a_failure() {
    let events = translated();
    let compacted: Vec<&str> = notices(&events)
        .into_iter()
        .filter(|notice| notice.contains("compacted"))
        .collect();

    assert_eq!(compacted.len(), 1, "{compacted:?}");
    assert!(compacted[0].contains("41000"), "{compacted:?}");
}

#[test]
fn the_transcript_reads_as_the_session_happened() {
    let events = translated();
    let state = SessionState::replay(&events);

    assert_eq!(state.assistant_messages(), 3);
    assert_eq!(state.last_assistant(), Some("Done."));
    assert_eq!(
        state.errors(),
        1,
        "the line that is not JSON, and nothing else"
    );
    assert_eq!(state.fatal_error(), None);
}

/// A session that edits files, translated and folded.
///
/// The counts the fold ends with are checked against the ones
/// `git diff --numstat` printed for the same edits applied to the same files.
/// Both are in `tests/fixtures/README.md`; they were measured, not derived
/// from this crate, so a bug in the counting cannot agree with itself.
mod edits {
    use super::*;

    const EDITS: &str = include_str!("fixtures/edits.jsonl");

    fn folded() -> SessionState {
        let mut translator = Translator::new("max").in_dir("/repo");
        let events: Vec<Event> = EDITS
            .lines()
            .filter(|line| !line.trim().is_empty())
            .flat_map(|line| translator.line(line))
            .collect();
        assert!(
            warnings(&events).is_empty(),
            "the fixture is not a stream this bridge reads: {:?}",
            warnings(&events)
        );
        SessionState::replay(&events)
    }

    /// `git diff --numstat` for the same edits: `catalog/fetch.ts` two edits
    /// deep, `catalog/conditional.ts` written new.
    #[test]
    fn the_files_a_session_edited_are_counted_the_way_git_counts_them() {
        let state = folded();
        let files = state.files();

        assert_eq!(
            files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec![
                "catalog/fetch.ts",
                "catalog/conditional.ts",
                "notes.md",
                "catalog/cache.ts",
            ],
            "the pane would list the files in an order the session did not touch them in"
        );

        // `4  2  catalog/fetch.ts` — one line replaced by three, then one by
        // one, and the path is the one `git` prints rather than the absolute
        // one the CLI sent.
        let fetch = &files[0];
        assert_eq!((fetch.added, fetch.removed), (4, 2));
        assert_eq!(fetch.changes, 2);
        assert!(fetch.added_stated() && fetch.removed_stated());

        // `3  0  catalog/conditional.ts` — a file that was not there before.
        let conditional = &files[1];
        assert_eq!((conditional.added, conditional.removed), (3, 0));
        assert!(conditional.added_stated() && conditional.removed_stated());
    }

    /// The two counts the calls' arguments cannot support, each marked rather
    /// than guessed where the CLI's report of the change is not beside the
    /// result — this fixture leaves those reports out. Both would be wrong if
    /// they were filled in, and both are under the truth rather than over it,
    /// which is what a floor means.
    #[test]
    fn a_count_the_stream_does_not_carry_is_marked_rather_than_filled_in() {
        let state = folded();

        // `1  4  notes.md`: the overwrite says what the file becomes and never
        // what it was, so the four lines it dropped are nowhere in the stream.
        let notes = &state.files()[2];
        assert_eq!(notes.added, 1);
        assert!(notes.added_stated());
        assert_eq!(notes.removed, 0);
        assert!(
            !notes.removed_stated(),
            "a removal the stream never carried was shown as the whole figure"
        );

        // `2  2  catalog/cache.ts`: the CLI replaced both occurrences and the
        // call describes one, so neither side is a count this can defend.
        let cache = &state.files()[3];
        assert_eq!((cache.added, cache.removed), (0, 0));
        assert_eq!(cache.changes, 1);
        assert!(!cache.added_stated() && !cache.removed_stated());
    }

    /// The CLI refuses a `Write` to a file nothing has read, and that refusal
    /// arrives as an ordinary tool result. A file the session failed to write
    /// is not a file the session changed.
    #[test]
    fn a_write_the_cli_refused_put_nothing_in_the_change_set() {
        let state = folded();

        assert_eq!(state.files().len(), 4, "{:?}", state.files());
        assert_eq!(
            state.files()[2].changes,
            1,
            "the write that failed was counted alongside the one that ran"
        );
        assert_eq!(state.tools().failed, 1);
    }

    #[test]
    fn each_file_carries_the_models_own_words_from_just_before_it_changed() {
        let state = folded();

        assert_eq!(
            state.files()[1].why.as_deref(),
            Some("A helper module for the conditional-request checks.")
        );
        assert_eq!(
            state.files()[0].why.as_deref(),
            Some("Returning the cached body on a 304."),
            "the second edit did not bring its own explanation with it"
        );
    }
}

/// A turn recorded with the CLI's own report beside each tool result, where
/// the hunks a file change carries come from.
mod reported_hunks {
    use super::*;
    use niobe_core::diff::{Hunk, Line};

    const ANSWERS: &str = include_str!("fixtures/stdio-answers.jsonl");

    /// The `Edit` the fixture's README describes: `beta` became `gamma` in a
    /// two-line file, and the CLI reported that as one hunk with `alpha` above
    /// it. The refused `Write` changed nothing and carries nothing.
    #[test]
    fn a_recorded_edit_carries_the_hunk_the_cli_reported_beside_its_result() {
        let mut translator = Translator::new("max").in_dir("/repo");
        let changes: Vec<Event> = ANSWERS
            .lines()
            .filter(|line| !line.trim().is_empty())
            .flat_map(|line| translator.line(line))
            .filter(|event| matches!(event, Event::FileChange { .. }))
            .collect();

        assert_eq!(
            changes,
            vec![Event::FileChange {
                path: "notes.txt".to_owned(),
                added: Some(1),
                removed: Some(1),
                hunks: vec![
                    Hunk::checked(
                        1,
                        2,
                        1,
                        2,
                        vec![
                            Line::Context("alpha".to_owned()),
                            Line::Removed("beta".to_owned()),
                            Line::Added("gamma".to_owned()),
                        ],
                    )
                    .expect("the recorded header counts two lines on each side"),
                ],
            }]
        );
    }
}

/// A live session on a model with its 1M-token window selected, where the
/// messages and the bill name the model differently. The numbers are the
/// recording's own `result` lines, as `tests/fixtures/README.md` sets out.
mod long_context {
    use super::*;

    const LONG_CONTEXT: &str = include_str!("fixtures/long-context.jsonl");

    fn translated() -> Vec<Event> {
        let mut translator = Translator::new("max").in_dir("/repo");
        LONG_CONTEXT
            .lines()
            .filter(|line| !line.trim().is_empty())
            .flat_map(|line| translator.line(line))
            .collect()
    }

    /// Every message names `claude-opus-5` and `modelUsage` names
    /// `claude-opus-5[1m]`. Reconciling the one against the other by id finds
    /// nothing in common and reports every token a second time: 8 in, 12 out,
    /// 43,944 cache read and 51,666 cache write.
    #[test]
    fn a_session_billed_under_another_id_than_its_messages_counts_each_token_once() {
        let events = translated();
        assert!(
            warnings(&events).is_empty(),
            "the recording folds without complaint: {:?}",
            warnings(&events)
        );
        let totals = SessionState::replay(&events).totals().clone();

        assert_eq!(totals.input, 4);
        assert_eq!(totals.output, 6);
        assert_eq!(totals.cache_read, 21_972);
        assert_eq!(totals.cache_write, 25_833);
        assert!(
            (totals.reported_cost_usd - 0.269_486).abs() < 1e-9,
            "the session cost {} rather than the 0.269486 the CLI reported",
            totals.reported_cost_usd
        );
    }

    /// The bill is filed under the id the CLI billed, with the money on it
    /// and no tokens: the messages already carried every one of them.
    #[test]
    fn the_cost_is_reported_under_the_id_the_cli_billed() {
        let events = translated();
        let billed: Vec<_> = usage_records(&events)
            .into_iter()
            .filter(|usage| usage.cost_usd.is_some())
            .collect();

        assert_eq!(billed.len(), 2, "one cost record a turn: {billed:?}");
        for usage in billed {
            assert_eq!(usage.model, "claude-opus-5[1m]");
            assert_eq!(usage.cost_basis, Some(CostBasis::ApiEquivalent));
            assert_eq!(
                (
                    usage.input,
                    usage.output,
                    usage.cache_read,
                    usage.cache_write
                ),
                (0, 0, 0, 0),
                "{usage:?}"
            );
        }
    }

    /// The shell names the model the CLI said the session is on, which
    /// carries the window; a message naming the family does not take it away.
    #[test]
    fn the_session_stays_on_the_model_with_its_window() {
        let events = translated();
        let models: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                Event::SessionMeta(meta) => Some(meta.model.as_str()),
                _ => None,
            })
            .collect();

        assert_eq!(models, ["claude-opus-5[1m]"]);
    }
}

fn translated_sub_agents() -> Vec<Event> {
    let mut translator = Translator::new("max").in_dir("/repo");
    SUB_AGENTS
        .lines()
        .filter(|line| !line.trim().is_empty())
        .flat_map(|line| translator.line(line))
        .collect()
}

#[test]
fn a_recorded_agent_call_is_a_sub_agent_named_for_its_kind_and_its_task() {
    let events = translated_sub_agents();

    let spawned: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            Event::AgentSpawn { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        spawned,
        [
            "quick-lookup: Summarize catalog/cache.py",
            "deep-reasoner: Review catalog/fetch.py for bugs",
            "deep-reasoner: Review catalog/cache.py for bugs",
        ]
    );
}

#[test]
fn recorded_background_sub_agents_end_when_the_cli_says_they_stopped() {
    let events = translated_sub_agents();
    let state = SessionState::replay(&events);

    assert_eq!(state.agents_spawned(), 3);
    assert_eq!(state.agents_completed(), 3);
    assert_eq!(
        state.peak_running_agents(),
        2,
        "the parallel pair ran together"
    );
    assert!(state.running_agents().is_empty());

    // Each launch returned at once; each end came later, from its own
    // notification. The review of cache.py finished first.
    let ended: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            Event::AgentExit { id, outcome } if *outcome == AgentOutcome::Completed => {
                Some(id.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        ended,
        [
            "toolu_01Bjmsju8Kjg8jieHzs66KmX",
            "toolu_018oEYQ3e89jvpp8SycSyQ75",
            "toolu_018CBLWZbbx5bZCa7U5VkDVi",
        ]
    );
    assert!(
        warnings(&events)
            .iter()
            .all(|warning| !warning.contains("task_notification")),
        "a sub-agent's notification was reported as unreadable"
    );
}

#[test]
fn a_recorded_session_that_runs_sub_agents_carries_no_error_entry() {
    let events = translated_sub_agents();

    // The recording holds 28 lines of the CLI's background-task bookkeeping —
    // `task_started`, `task_progress`, `task_updated`,
    // `background_tasks_changed` — and nothing in it failed.
    assert_eq!(warnings(&events), Vec::<&str>::new());
}

#[test]
fn the_cli_lists_the_sub_agent_tool_under_one_name_and_the_model_calls_it_by_the_other() {
    let init = SUB_AGENTS
        .lines()
        .find(|line| line.contains(r#""subtype":"init""#))
        .expect("the recording opens its turns with init");
    let init: serde_json::Value = serde_json::from_str(init).expect("init is JSON");
    let tools: Vec<&str> = init["tools"]
        .as_array()
        .expect("init lists the tools")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(
        tools.contains(&"Task") && !tools.contains(&"Agent"),
        "{tools:?}"
    );

    let state = SessionState::replay(&translated_sub_agents());
    assert_eq!(state.tools().by_name.get("Agent"), Some(&3));
    assert_eq!(state.tools().by_name.get("Task"), None);
}

#[test]
fn a_message_delta_says_its_cache_writes_were_bought_for_an_hour_inside_its_iterations() {
    let events = translated_sub_agents();

    let per_message: Vec<&niobe_core::Usage> = usage_records(&events)
        .into_iter()
        .filter(|usage| usage.cost_usd.is_none() && usage.cache_write > 0)
        .collect();
    assert!(!per_message.is_empty());
    for usage in per_message {
        assert_eq!(
            usage.cache_write_1h, usage.cache_write,
            "every write this session made was an hour's: {usage:?}"
        );
    }
}

/// What the recording's sub-agent reports said about each agent, per field,
/// in the order they said it.
fn progress_of<T>(events: &[Event], agent: &str, field: impl Fn(&Event) -> Option<T>) -> Vec<T> {
    events
        .iter()
        .filter(|event| matches!(event, Event::AgentProgress { id, .. } if id.as_str() == agent))
        .filter_map(field)
        .collect()
}

const SUMMARISER: &str = "toolu_01Bjmsju8Kjg8jieHzs66KmX";
const FETCH_REVIEWER: &str = "toolu_018CBLWZbbx5bZCa7U5VkDVi";
const CACHE_REVIEWER: &str = "toolu_018oEYQ3e89jvpp8SycSyQ75";

#[test]
fn a_recorded_sub_agents_model_is_the_one_its_own_messages_name_once() {
    let events = translated_sub_agents();
    let model = |event: &Event| match event {
        Event::AgentProgress { model, .. } => model.clone(),
        _ => None,
    };

    // `message.model` on the `assistant` lines whose `parent_tool_use_id` is
    // the agent's call — see the fixtures' README. The spawn names none.
    assert_eq!(
        progress_of(&events, SUMMARISER, model),
        ["claude-haiku-4-5-20251001"]
    );
    assert_eq!(
        progress_of(&events, FETCH_REVIEWER, model),
        ["claude-opus-5"]
    );
    assert_eq!(
        progress_of(&events, CACHE_REVIEWER, model),
        ["claude-opus-5"]
    );
}

#[test]
fn a_recorded_sub_agents_context_is_what_the_cli_counted_for_that_agent_alone() {
    let events = translated_sub_agents();
    let tokens = |event: &Event| match event {
        Event::AgentProgress { context_tokens, .. } => *context_tokens,
        _ => None,
    };

    // `usage.total_tokens` of each `task_progress` and then the
    // `task_notification` naming the agent's call, read off the recording
    // with the `jq` program in the fixtures' README.
    assert_eq!(progress_of(&events, SUMMARISER, tokens), [5967, 6566]);
    assert_eq!(
        progress_of(&events, CACHE_REVIEWER, tokens),
        [12938, 13009, 15069, 15114, 17103, 18181, 21508]
    );
    assert_eq!(
        progress_of(&events, FETCH_REVIEWER, tokens),
        [
            12980, 13023, 15088, 15136, 17455, 17567, 18457, 20235, 22634, 25861
        ]
    );
}

#[test]
fn a_recorded_sub_agents_latest_line_is_its_step_and_then_its_own_answer() {
    let events = translated_sub_agents();
    let latest = |event: &Event| match event {
        Event::AgentProgress { latest, .. } => latest.clone(),
        _ => None,
    };

    assert_eq!(
        progress_of(&events, SUMMARISER, latest),
        [
            "Reading catalog/cache.py",
            "This module implements an LRU (least-recently-used) cache with a fixed capacity \
             using OrderedDict. The `get` method retrieves values and marks them as recently \
             used, while `put` adds or updates entries and removes the least-recently-used item \
             when capacity is exceeded.",
        ]
    );
    // A long answer is drawn from its first line, not its whole text.
    assert_eq!(
        progress_of(&events, CACHE_REVIEWER, latest)
            .last()
            .map(String::as_str),
        Some("Three real bugs, ordered by severity.")
    );
}

#[test]
fn nothing_is_reported_about_a_sub_agent_after_it_has_ended() {
    let events = translated_sub_agents();

    for agent in [SUMMARISER, FETCH_REVIEWER, CACHE_REVIEWER] {
        let ended = events
            .iter()
            .position(|event| matches!(event, Event::AgentExit { id, .. } if id.as_str() == agent))
            .expect("every recorded agent ended");
        assert!(
            events[ended..].iter().all(
                |event| !matches!(event, Event::AgentProgress { id, .. } if id.as_str() == agent)
            ),
            "{agent} was reported on after its exit"
        );
    }
}
