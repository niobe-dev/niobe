// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The shapes the recordings carry, against the shapes this bridge was
//! written for.
//!
//! The recordings in `tests/fixtures/` are the protocol as the `claude` CLI
//! actually printed it, and every other test in this crate asserts what the
//! bridge *makes* of them. This one asserts what is *in* them: every message
//! type, every `system` subtype, every content block and every stream event,
//! against a list that is checked in.
//!
//! That list is the point. A CLI release that adds a message type, or moves
//! one, changes the recordings — and a shape the bridge silently passes over
//! (a content block it has no place for, a stream event it ignores) produces
//! no warning at runtime and no failure anywhere else. Here it is a red test
//! naming the shape, which is what stops a new protocol version from being
//! read wrong in silence.

#![allow(
    clippy::expect_used,
    clippy::print_stdout,
    reason = "test helpers: a failed expectation is the test failing, and a skipped check says so"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use niobe_bridge_claude::conformance;

/// Every shape the recordings carry, sorted.
///
/// One entry per distinct thing the CLI can put on a line: a message `type`, a
/// `system` subtype, the body of a `stream_event`, and the kind of a content
/// block or of a delta.
///
/// Adding a shape here is a decision that the bridge handles it — reads it,
/// passes over it on purpose, or reports it. Do not add one to quiet the test.
const SHAPES: &[&str] = &[
    // What a recording deliberately holds that cannot be read at all: a line
    // that is not JSON, and a type from a later release on each carrier. Each
    // becomes a warning entry rather than the end of the session, which is the
    // behaviour the other tests in this crate assert.
    "(not json)",
    "a_message_type_from_a_later_version",
    "teleport",
    // The live stream.
    "assistant",
    "control_request/can_use_tool",
    "rate_limit_event",
    "result/success",
    "stream_event/content_block_delta",
    "stream_event/message_delta",
    "stream_event/message_start",
    "system/compact_boundary",
    "system/init",
    "system/permission_denied",
    "system/status",
    "system/thinking_tokens",
    "user",
    // Content blocks and deltas, which ride on both the stream and the
    // transcript.
    "block/text",
    "block/thinking",
    "block/tool_result",
    "block/tool_use",
    "delta/text_delta",
    "delta/thinking_delta",
    // The transcript's own records: what the session cost, how it was gating
    // calls, and the CLI's furniture.
    "agent-name",
    "ai-title",
    "attachment",
    "bridge-session",
    "cost-state",
    "file-history-snapshot",
    "last-prompt",
    "mode",
    "permission-mode",
    "pr-link",
    "system/local_command",
];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Every recording in `tests/fixtures/`, live streams and transcripts alike.
fn recordings() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut directories = vec![fixtures()];
    while let Some(directory) = directories.pop() {
        let entries = std::fs::read_dir(&directory).expect("the fixture directory is readable");
        for entry in entries {
            let path = entry.expect("a fixture directory entry").path();
            if path.is_dir() {
                directories.push(path);
            } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                found.push(path);
            }
        }
    }
    found.sort();
    assert!(!found.is_empty(), "there are recordings to read");
    found
}

/// The shapes one recorded line carries.
fn shapes_of(line: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return vec!["(not json)".to_owned()];
    };
    let at = |path: &[&str]| -> Option<String> {
        let mut cursor = &value;
        for key in path {
            cursor = cursor.get(key)?;
        }
        cursor.as_str().map(str::to_owned)
    };
    let kind = at(&["type"]).unwrap_or_else(|| "(untyped)".to_owned());
    let named =
        |part: Option<String>| format!("{kind}/{}", part.unwrap_or_else(|| "(none)".to_owned()));

    let mut shapes = vec![match kind.as_str() {
        "system" | "result" => named(at(&["subtype"])),
        "control_request" => named(at(&["request", "subtype"])),
        "control_response" => named(at(&["response", "subtype"])),
        "stream_event" => named(at(&["event", "type"])),
        _ => kind.clone(),
    }];
    if let Some(delta) = at(&["event", "delta", "type"]) {
        shapes.push(format!("delta/{delta}"));
    }
    if let Some(blocks) = value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(serde_json::Value::as_array)
    {
        for block in blocks {
            let block = block
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("(untyped)");
            shapes.push(format!("block/{block}"));
        }
    }
    shapes
}

/// The version a recorded line names, under the key its carrier writes it
/// under: `claude_code_version` on a live `system`/`init`, whose schema in the
/// shipped CLI names it that, and `version` on a transcript record.
///
/// A line is read under one key and never the other, so a recording that
/// spells the key the way the *other* carrier does fails rather than passing
/// on a field the bridge would never look at.
fn version_in(line: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    let at = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let init = value.get("type").and_then(serde_json::Value::as_str) == Some("system")
        && value.get("subtype").and_then(serde_json::Value::as_str) == Some("init");
    match init {
        true => {
            assert!(
                at("version").is_none(),
                "a live `system`/`init` names the CLI release under `claude_code_version`, which \
                 is the key the shipped CLI's own init schema uses and the one this bridge reads. \
                 A recording that writes `version` there describes a message no CLI sends: {line}"
            );
            at("claude_code_version")
        }
        false => at("version"),
    }
}

fn lines_of(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .expect("a recording is readable")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_shape_in_the_recordings_is_one_this_bridge_was_written_for() {
    let mut found = BTreeSet::new();
    for path in recordings() {
        for line in lines_of(&path) {
            found.extend(shapes_of(&line));
        }
    }
    let expected: BTreeSet<&str> = SHAPES.iter().copied().collect();
    let found_names: BTreeSet<&str> = found.iter().map(String::as_str).collect();

    let new: Vec<&str> = found_names.difference(&expected).copied().collect();
    assert!(
        new.is_empty(),
        "the recordings carry shapes this bridge was not written for: {new:?}. Read what the CLI \
         now sends, decide what the bridge does with it — folds it, passes over it, or reports it \
         — and only then add it to SHAPES."
    );

    let gone: Vec<&str> = expected.difference(&found_names).copied().collect();
    assert!(
        gone.is_empty(),
        "SHAPES names shapes no recording carries any more: {gone:?}. A shape with no recording \
         behind it is a claim about the protocol that nothing checks."
    );
}

#[test]
fn every_recording_says_which_cli_version_it_was_recorded_from() {
    for path in recordings() {
        let versions: BTreeSet<String> = lines_of(&path)
            .iter()
            .filter_map(|line| version_in(line))
            .collect();
        assert!(
            !versions.is_empty(),
            "{} names no CLI version. A recording whose version is not in it cannot be re-recorded \
             against the right release.",
            path.display()
        );
        for version in versions {
            assert!(
                conformance::RECORDED.contains(&version.as_str()),
                "{} was recorded from Claude Code {version}, which conformance::RECORDED does not \
                 name.",
                path.display()
            );
        }
    }
}

#[test]
fn the_installed_cli_is_a_release_these_recordings_cover() {
    let Ok(output) = Command::new(niobe_bridge_claude::BINARY)
        .arg("--version")
        .output()
    else {
        // The CLI is not on this machine, which is every CI runner: there is
        // no installed release to check the recordings against, and saying so
        // beats a green tick that means nothing.
        println!("skipped: the `claude` CLI is not installed here");
        return;
    };
    let said = String::from_utf8_lossy(&output.stdout);
    let Some(installed) = output
        .status
        .success()
        .then(|| said.split_whitespace().next())
        .flatten()
    else {
        // Something on this machine answers to `claude` and does not answer
        // `--version` with one. There is nothing to compare, and guessing at
        // what it is would be worse than saying so.
        println!(
            "skipped: `{} --version` said no version",
            niobe_bridge_claude::BINARY
        );
        return;
    };

    assert!(
        conformance::recorded(installed),
        "the installed CLI is Claude Code {installed}, and this bridge's recordings are of {:?}. \
         Record a session from {installed} into tests/fixtures/, read what changed, and name it in \
         conformance::RECORDED.",
        conformance::RECORDED
    );
}
