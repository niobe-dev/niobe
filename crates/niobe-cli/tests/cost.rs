// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! `niobe cost` over a store holding a recorded session billed by use, the
//! same session billed on a plan, and one that never said how it was billed:
//! the JSON against the schema it publishes, `schema/cost.schema.json`, and
//! its figures against what `jq` reads off the recordings — with the program
//! beside each figure — rather than against the fold the binary ran.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use niobe_core::event::{Billing, Event};
use niobe_store::{Store, read_log};
use serde_json::{Value, json};

mod schema;

const FIXTURES: &str = "../niobe-core/tests/fixtures";

/// Noon UTC on 18 September 2026, the day the fixture was recorded.
const RECORDED_ON_MS: u64 = 1_789_732_800_000;

/// A repository named `catalog` holding the recorded session billed by use,
/// the same billed on a plan, and the one the producer dropped events from,
/// which says nothing of its billing, in that order.
fn repository() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    let root = dir.path().join("catalog");
    std::fs::create_dir_all(root.join(".git")).expect("a .git directory can be made");
    std::fs::create_dir(root.join(".niobe")).expect("a .niobe directory can be made");
    let store = Store::open(&root.join(".niobe").join("sessions.db"))
        .expect("a store opens in a temporary directory");
    for (log, billing) in [
        ("claude-session.jsonl", Some(Billing::Metered)),
        ("claude-session.jsonl", Some(Billing::Plan)),
        ("producer-gaps.jsonl", None),
    ] {
        let session = store.create_session().expect("a session is created");
        let text = std::fs::read_to_string(fixtures().join(log)).expect("the fixture reads");
        let events = billing
            .map(|billing| Event::Billing { billing })
            .into_iter()
            .chain(read_log(&text).expect("the fixture parses"));
        for event in events {
            store
                .append_at(
                    session,
                    &event,
                    UNIX_EPOCH + Duration::from_millis(RECORDED_ON_MS),
                )
                .expect("an append succeeds");
        }
    }
    (dir, root)
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES)
}

fn cost(root: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_niobe"))
        .arg("cost")
        .args(args)
        .current_dir(root)
        .env("XDG_CONFIG_HOME", root.join("no-user-config-here"))
        .env("CLAUDE_CONFIG_DIR", root.join("no-claude-sessions-here"))
        .env_remove("HOME")
        .output()
        .expect("the niobe binary runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("the output is UTF-8")
}

fn exported(root: &Path) -> Value {
    serde_json::from_str(&cost(root, &["--json"])).expect("`--json` prints one JSON document")
}

#[test]
fn the_json_is_the_shape_the_published_schema_names() {
    let (_dir, root) = repository();
    let refused = schema::refusals("cost.schema.json", &exported(&root));
    assert!(refused.is_empty(), "{refused:#?}");
}

#[test]
fn the_validator_refuses_what_the_schema_does_not_allow() {
    let (_dir, root) = repository();
    let mut document = exported(&root);
    document["sessions"][0]["billing"] = json!("free");
    document["sessions"][0]["cost"]["records"] = json!(-1);
    document["sessions"][1]["tokens"]["guessed"] = json!(3);
    document["sessions"][2]["started_at"] = json!("18 September 2026");
    let refused = schema::refusals("cost.schema.json", &document);
    assert_eq!(refused.len(), 4, "{refused:#?}");
}

/// Each figure is what `jq` reads off `claude-session.jsonl`:
///
/// ```sh
/// jq -s '[.[] | select(.type == "usage")] | {
///   records: length, input: (map(.input) | add), output: (map(.output) | add),
///   cache_read: (map(.cache_read) | add), cache_write: (map(.cache_write) | add),
///   cache_write_1h: (map(.cache_write_1h) | add), reasoning: (map(.reasoning) | add),
///   reported: (map(.cost_usd // 0) | add),
///   models: (group_by(.model) | map({(.[0].model):
///     (map(.input + .output + .cache_read + .cache_write + .reasoning) | add)}) | add)}'
/// ```
#[test]
fn a_session_billed_by_use_carries_the_tokens_and_money_its_records_hold() {
    let (_dir, root) = repository();
    let document = exported(&root);
    let metered = &document["sessions"][0];

    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["repository"], "catalog");
    assert_eq!(metered["id"], 1);
    assert_eq!(metered["started_at"], "2026-09-18T12:00:00Z");
    assert_eq!(metered["billing"], "metered");
    assert_eq!(
        metered["tokens"],
        json!({
            "input": 120, "output": 19_606, "cache_read": 460_375, "cache_write": 82_887,
            "cache_write_1h": 40_054, "cache_write_unsplit": 0, "reasoning": 0,
            "total": 120 + 19_606 + 460_375 + 82_887,
        })
    );
    assert_eq!(
        metered["models"],
        json!({
            "claude-haiku-4-5-20251001": 148_744,
            "claude-opus-5[1m]": 220_195,
            "claude-sonnet-5": 194_049,
        })
    );
    let usd = metered["cost"]["reported_usd"]
        .as_f64()
        .expect("the recording reports a cost");
    assert!((usd - 0.865_523_95).abs() < 1e-9, "{usd}");
    assert_eq!(metered["cost"]["records"], 25);
}

/// The recording predates settlement marking, so its uncovered records may
/// have been paid for, and the export says so rather than leave a floor
/// unexplained.
#[test]
fn a_floor_an_older_recording_left_is_marked_as_one() {
    let (_dir, root) = repository();
    let cost = exported(&root)["sessions"][0]["cost"].clone();
    assert_eq!(cost["complete"], false, "{cost}");
    assert_eq!(cost["older_recording"], true, "{cost}");
    // jq -s '[.[] | select(.type == "usage" and .cost_usd == null)] | length'
    assert_eq!(cost["records_uncovered"], 14, "{cost}");
}

#[test]
fn a_plan_and_a_session_that_never_said_carry_tokens_and_no_money() {
    let (_dir, root) = repository();
    let document = exported(&root);
    let (plan, unsaid) = (&document["sessions"][1], &document["sessions"][2]);

    assert_eq!(plan["billing"], "plan");
    assert_eq!(plan["cost"], Value::Null);
    assert_eq!(plan["tokens"], document["sessions"][0]["tokens"]);

    assert_eq!(unsaid["billing"], "unknown");
    assert_eq!(unsaid["cost"], Value::Null);
    assert_eq!(unsaid["tokens"]["total"], 0);
}

#[test]
fn the_csv_is_a_header_and_a_row_a_session_with_no_money_left_empty() {
    let (_dir, root) = repository();
    let printed = cost(&root, &["--csv"]);
    let rows: Vec<Vec<&str>> = printed.lines().map(|l| l.split(',').collect()).collect();

    assert_eq!(rows.len(), 4, "{printed}");
    let header = &rows[0];
    assert!(
        rows.iter().all(|row| row.len() == header.len()),
        "{printed}"
    );
    let at = |name: &str| {
        header
            .iter()
            .position(|column| *column == name)
            .unwrap_or_else(|| panic!("no {name} column in {printed}"))
    };
    assert_eq!(rows[1][at("billing")], "metered");
    assert_eq!(
        rows[1][at("tokens")],
        (120 + 19_606 + 460_375 + 82_887).to_string()
    );
    assert_eq!(rows[1][at("reported_usd")], "0.86552395");
    for empty in [
        "reported_usd",
        "cost_complete",
        "records",
        "older_recording",
    ] {
        assert_eq!(
            rows[2][at(empty)],
            "",
            "a plan's {empty} is empty: {printed}"
        );
    }
}

#[test]
fn the_table_reads_a_plan_as_a_dash_and_a_floor_with_its_mark() {
    let (_dir, root) = repository();
    let printed = cost(&root, &["--last", "2"]);
    let rows: Vec<&str> = printed.lines().collect();
    let every = cost(&root, &[]);
    assert!(
        every.contains(" metered ") && every.contains("≥$0.87 · older recording"),
        "{every}"
    );

    assert!(rows[0].starts_with("ID"), "{printed}");
    assert!(
        rows[1].contains(" plan ") && rows[1].ends_with('—'),
        "{printed}"
    );
    assert!(
        rows[2].contains(" unknown ") && rows[2].ends_with('—'),
        "{printed}"
    );
    assert!(
        !printed.contains(" metered "),
        "--last 2 left it out: {printed}"
    );
}
