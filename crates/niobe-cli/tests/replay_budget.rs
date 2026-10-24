// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! How long `niobe replay` says it took to read and fold the recorded
//! session.
//!
//! This is a test binary of its own, for the reason
//! `niobe-tui/tests/frame_budget.rs` is: cargo runs test binaries one after
//! another, so none of this crate's other tests — each of which starts the
//! binary, or a terminal, or a stand-in CLI — shares the cores while the fold
//! is timed, and the median of [`RUNS`] is asserted so that a run the
//! scheduler interrupted cannot fail it. It is timed only in an optimised
//! build, which is what ships: in the debug suite, with a load average of
//! thirty-five to fifty from other builds, a replay reported up to 398 ms,
//! and the budget measured the machine.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::path::Path;
use std::process::Command;

const FIXTURE: &str = "../niobe-core/tests/fixtures/claude-session.jsonl";

/// The fixture must be read and folded in under this many milliseconds.
///
/// Timed on four two-vCPU CI runners, the binary reported a median of 1.6 to
/// 2.3 ms with the rest of the suite running in parallel and 1.1 to 1.4 ms
/// alone, and the slowest single run of the 80 timed was 4.2 ms.
const REPLAY_BUDGET_MS: f64 = 50.0;

/// How many times the fixture is replayed. Odd, so the median is one of them.
const RUNS: usize = 31;

/// The milliseconds one `niobe replay` of the fixture says it took, from the
/// header it prints first.
fn replayed_in() -> f64 {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = here.join(FIXTURE);
    let output = Command::new(env!("CARGO_BIN_EXE_niobe"))
        .args(["replay", fixture.to_str().expect("a UTF-8 path")])
        .current_dir(here)
        .env("XDG_CONFIG_HOME", here.join("no-user-config-here"))
        .env("CLAUDE_CONFIG_DIR", here.join("no-claude-sessions-here"))
        .env_remove("HOME")
        .output()
        .expect("the niobe binary runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let out = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    out.lines()
        .next()
        .and_then(|header| header.split(" in ").nth(1))
        .and_then(|rest| rest.strip_suffix(" ms"))
        .and_then(|ms| ms.parse().ok())
        .unwrap_or_else(|| panic!("no timing in the header: {out}"))
}

#[test]
#[cfg_attr(debug_assertions, ignore = "a replay is timed in an optimised build")]
fn replaying_the_fixture_takes_less_than_the_budget() {
    let mut runs: Vec<f64> = (0..RUNS).map(|_| replayed_in()).collect();
    runs.sort_unstable_by(f64::total_cmp);
    let median = runs[RUNS / 2];

    assert!(
        median < REPLAY_BUDGET_MS,
        "replay took {median} ms at the median of {RUNS}, over the {REPLAY_BUDGET_MS} ms budget"
    );
}
