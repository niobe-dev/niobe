// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Permission prompts answered through a running [`Session`], against the
//! exchange a live CLI was recorded making.
//!
//! The stand-in `claude` prints `tests/fixtures/stdio-answers.jsonl` line by
//! line and, after each `control_request`, waits for the answer on its standard
//! input the way the CLI does and keeps it. So this covers what no translation
//! test can: that what reaches the CLI is an answer it took, and that a
//! refusal made here reaches the translator before the tool result it comes
//! back as — which the CLI does not announce with a `permission_denied` of its
//! own.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use niobe_bridge_claude::{Options, Session, SpawnError};
use niobe_core::event::{Event, PermissionDecision, ToolCallId, ToolOutcome};

/// How long the whole exchange may take. The stand-in answers at once, so
/// anything near this is a session waiting on an answer that never came.
const PATIENCE: Duration = Duration::from_secs(10);

/// The recording the stand-in replays.
fn recording() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stdio-answers.jsonl")
}

/// A `claude` that reads a turn, replays the recording, stops at every prompt
/// until it is answered, and writes the answers to `answers.jsonl` beside it.
fn stand_in(dir: &Path) -> PathBuf {
    let script = dir.join("claude");
    let body = format!(
        "#!/bin/sh\n\
         read -r turn\n\
         while IFS= read -r line; do\n\
         \tprintf '%s\\n' \"$line\"\n\
         \tcase \"$line\" in\n\
         \t'{{\"type\":\"control_request\"'*) read -r answer <&3; printf '%s\\n' \"$answer\" >> '{answers}' ;;\n\
         \tesac\n\
         done 3<&0 < '{recording}'\n",
        answers = dir.join("answers.jsonl").display(),
        recording = recording().display(),
    );
    std::fs::write(&script, body).expect("the stand-in is written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("the stand-in is made executable");
    }
    script
}

/// Starts the stand-in, waiting out Linux's `ETXTBSY`.
///
/// The test binary runs its tests on threads, and a thread that forks while
/// another has the stand-in open for writing hands that open file to its
/// child until the child execs. Executing a file something holds open for
/// writing is refused as "text file busy", however briefly, so the start is
/// tried again for as long as that is the reason, and for no other failure.
fn spawn(options: &Options) -> Session {
    let started = Instant::now();
    loop {
        match Session::spawn(options) {
            Err(SpawnError::Failed { error, .. })
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && started.elapsed() < PATIENCE =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            started => return started.expect("the stand-in starts"),
        }
    }
}

/// Runs the recorded turn, answering each prompt with what `decide` says
/// about the tool it names, and returns every event and every answer the
/// stand-in received.
fn exchange(decide: impl Fn(&str) -> PermissionDecision) -> (Vec<Event>, Vec<serde_json::Value>) {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let mut options = Options::new(dir.path(), "max");
    options.binary = stand_in(dir.path());
    options.ask_over_stdio = true;
    let mut session = spawn(&options);
    session.send("edit the notes").expect("the turn is sent");

    let started = Instant::now();
    let mut events = Vec::new();
    loop {
        assert!(
            started.elapsed() < PATIENCE,
            "the session stalled: {events:#?}"
        );
        let drained = session.drain();
        for (id, tool) in questions(&drained) {
            session
                .answer(&id, decide(&tool))
                .expect("the prompt is answered");
        }
        let ended = drained.iter().any(|event| {
            matches!(
                event,
                Event::Notice { .. } | Event::Error { fatal: true, .. }
            )
        });
        events.extend(drained);
        if ended {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let answers = std::fs::read_to_string(dir.path().join("answers.jsonl"))
        .expect("the stand-in kept the answers")
        .lines()
        .map(|line| serde_json::from_str(line).expect("each answer is one JSON line"))
        .collect();
    (events, answers)
}

/// The prompts in `events` that are waiting on an answer.
///
/// A refusal the translator reports on the CLI's behalf arrives as a request
/// and its answer together; that is a record of what happened, not a
/// question.
fn questions(events: &[Event]) -> Vec<(ToolCallId, String)> {
    let answered: Vec<&ToolCallId> = events
        .iter()
        .filter_map(|event| match event {
            Event::PermissionResponse { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    events
        .iter()
        .filter_map(|event| match event {
            Event::PermissionRequest { id, tool, .. } if !answered.contains(&id) => {
                Some((id.clone(), tool.clone()))
            }
            _ => None,
        })
        .collect()
}

fn outcome_of(events: &[Event], call: &str) -> ToolOutcome {
    events
        .iter()
        .find_map(|event| match event {
            Event::ToolCallEnd { id, outcome, .. } if *id == ToolCallId::new(call) => {
                Some(*outcome)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("call {call} never ended: {events:#?}"))
}

/// The recorded `Edit`, which was allowed.
const EDIT: &str = "toolu_014hfbDL7qowjc9UghUGfJBu";
/// The recorded `Write`, which was refused.
const WRITE: &str = "toolu_01KndEjUbvi3Z2bQfdCboKp1";

fn by_tool(tool: &str) -> PermissionDecision {
    match tool {
        "Write" => PermissionDecision::Deny,
        _ => PermissionDecision::Allow,
    }
}

#[test]
fn each_prompt_is_answered_by_the_request_id_the_cli_asked_under() {
    let (_, answers) = exchange(by_tool);

    let addressed: Vec<&str> = answers
        .iter()
        .map(|answer| {
            answer["response"]["request_id"]
                .as_str()
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(
        addressed,
        [
            "9001e185-ee2f-48e3-9622-d97172e9b715",
            "d3a56133-cb82-4f24-a415-1e47957b1f7d"
        ]
    );
}

#[test]
fn an_approval_hands_back_the_arguments_the_cli_asked_about() {
    let (events, answers) = exchange(by_tool);

    let approval = &answers[0]["response"]["response"];
    assert_eq!(approval["behavior"], "allow");
    assert_eq!(
        approval["updatedInput"],
        serde_json::json!({
            "file_path": "/repo/notes.txt",
            "old_string": "beta",
            "new_string": "gamma",
            "replace_all": false,
        })
    );
    assert_eq!(outcome_of(&events, EDIT), ToolOutcome::Ok);
}

#[test]
fn a_refusal_made_here_reads_as_denied_though_the_cli_never_announces_it() {
    let (events, answers) = exchange(by_tool);

    // What the model is told is what the CLI put in the tool result, word for
    // word, so it has to say the call was refused rather than that it broke.
    let refusal = &answers[1]["response"]["response"];
    assert_eq!(refusal["behavior"], "deny");
    let told = std::fs::read_to_string(recording())
        .expect("the recording is readable")
        .contains(&format!(
            r#""content":{},"is_error":true"#,
            refusal["message"]
        ));
    assert!(told, "the recording does not carry {refusal} as the result");

    assert_eq!(outcome_of(&events, WRITE), ToolOutcome::Denied);
    let refusals = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::PermissionResponse {
                    decision: PermissionDecision::Deny,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        refusals, 0,
        "the closing result's permission_denials was reported as a refusal of its own"
    );
}
