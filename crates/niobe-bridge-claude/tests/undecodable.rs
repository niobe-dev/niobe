// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A running [`Session`] against a CLI whose output is not what the stream
//! promises: bytes that are not UTF-8, and a standard output that closes while
//! the process goes on.
//!
//! The bytes are real: a tool's output echoed back, or a file read in Latin-1,
//! reaches the CLI's own standard output or standard error unchanged. Each
//! stand-in here stays alive once it has printed, as the CLI does between
//! turns, because a session that waited on it would wait for ever — and it is
//! the shell's draw loop that calls [`Session::drain`].

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use niobe_bridge_claude::{Options, Session};
use niobe_core::event::Event;

/// The longest one [`Session::drain`] may take. It runs on the thread that
/// draws the screen, so anything it waits on is a frozen shell.
const DRAIN: Duration = Duration::from_millis(100);

/// How long a stand-in may take to print what it prints, from its first line.
///
/// Generous for the reason `tests/answers.rs` gives: a freshly written script
/// can be held at its first instruction by an endpoint-security agent.
const PATIENCE: Duration = Duration::from_secs(60);

/// A reply that says `text`, as the CLI sends a finished message.
fn assistant(text: &str) -> String {
    format!(
        r#"{{"type":"assistant","message":{{"model":"claude-opus-5","id":"msg_{text}","type":"message","role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}}}"#
    )
}

/// The line that ends a turn.
const RESULT: &str = r#"{"type":"result","subtype":"success","is_error":false,"result":"done"}"#;

/// Writes a `claude` into `dir` that runs `body` after reading the request a
/// session starts with and the turn it is sent.
fn stand_in(dir: &Path, body: &str) -> PathBuf {
    let script = dir.join("claude");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nread -r first\nread -r turn\n{body}"),
    )
    .expect("the stand-in is written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("the stand-in is made executable");
    }
    script
}

/// Runs one turn against a stand-in running `body`, draining until `done`
/// says the events so far are enough, and returns them with the longest any
/// one drain took.
fn turn(body: &str, done: impl Fn(&[Event]) -> bool) -> (Vec<Event>, Duration) {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let mut options = Options::new(dir.path(), "max");
    options.binary = stand_in(dir.path(), body);
    let mut session = Session::spawn(&options).expect("the stand-in starts");
    session.send("say something").expect("the turn is sent");

    let started = Instant::now();
    let mut events = Vec::new();
    let mut longest = Duration::ZERO;
    while !done(&events) {
        assert!(
            started.elapsed() < PATIENCE,
            "the turn never finished: {events:#?}"
        );
        let draining = Instant::now();
        events.extend(session.drain());
        longest = longest.max(draining.elapsed());
        std::thread::sleep(Duration::from_millis(5));
    }
    (events, longest)
}

/// Whether a turn has ended.
fn turn_ended(events: &[Event]) -> bool {
    events.iter().any(|event| matches!(event, Event::TurnEnded))
}

/// The replies among `events`, in order.
fn replies(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::AssistantMessage { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// A stand-in body that prints a reply, `between`, another reply and the end
/// of the turn, then stays alive without reading, as a CLI between turns does
/// not quite: it does not even leave when its standard input closes.
fn around(between: &str) -> String {
    format!(
        "printf '%s\\n' '{before}'\n{between}\nprintf '%s\\n' '{after}' '{RESULT}'\nexec sleep 30\n",
        before = assistant("before"),
        after = assistant("after"),
    )
}

#[test]
fn a_line_that_is_not_utf8_is_passed_over_and_the_turn_goes_on() {
    let (events, longest) = turn(&around(r"printf 'BAD\377\376LINE\n'"), turn_ended);

    assert_eq!(replies(&events), ["before", "after"], "{events:#?}");
    assert!(longest < DRAIN, "one drain took {longest:?}");
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::Error { message, fatal: false } if message.contains("UTF-8")
        )),
        "the line that could not be read was not reported: {events:#?}"
    );
}

#[test]
fn a_reply_carrying_bytes_that_are_not_utf8_is_shown_with_them_replaced() {
    let body = format!(
        "printf '%s\\n' '{before}'\nprintf '{bad}\\n'\nprintf '%s\\n' '{RESULT}'\nexec sleep 30\n",
        before = assistant("before"),
        bad = assistant(r"caf\351"),
    );

    let (events, _) = turn(&body, turn_ended);

    assert_eq!(replies(&events), ["before", "caf\u{fffd}"], "{events:#?}");
}

#[test]
fn standard_error_that_is_not_utf8_is_still_read_so_the_cli_is_never_stopped_by_it() {
    // More than a pipe holds, after the bad line: a reader that stopped at the
    // bad line would leave the CLI blocked on the write, and the reply after
    // it would never come.
    let flood = r"printf 'BAD\377\376LINE\n' >&2; i=0; while [ $i -lt 2000 ]; do printf '%080d\n' $i >&2; i=$((i+1)); done";

    let (events, longest) = turn(&around(flood), turn_ended);

    assert_eq!(replies(&events), ["before", "after"], "{events:#?}");
    assert!(longest < DRAIN, "one drain took {longest:?}");
}

#[test]
fn a_cli_that_closes_its_output_and_keeps_running_is_stopped_without_freezing_the_shell() {
    let body = format!(
        "printf '%s\\n' '{before}'\nexec 1>&-\nexec sleep 30\n",
        before = assistant("before"),
    );
    let fatal = |events: &[Event]| {
        events
            .iter()
            .any(|event| matches!(event, Event::Error { fatal: true, .. }))
    };

    let (events, longest) = turn(&body, fatal);

    assert!(longest < DRAIN, "one drain took {longest:?}");
    let said = events
        .iter()
        .find_map(|event| match event {
            Event::Error {
                message,
                fatal: true,
            } => Some(message.as_str()),
            _ => None,
        })
        .expect("the session ended with an error");
    assert!(said.contains("still running"), "{said}");
    assert!(
        !events.iter().any(
            |event| matches!(event, Event::Notice { message } if message.contains("session ended."))
        ),
        "a CLI that had to be stopped was reported as a clean end: {events:#?}"
    );
}
