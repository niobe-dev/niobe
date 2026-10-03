// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! How long a stored session takes to open, load and fold again, as a
//! resumed session does.
//!
//! This is a test binary of its own, for the reason
//! `niobe-tui/tests/frame_budget.rs` is: cargo runs test binaries one after
//! another, so nothing else in this crate's suite shares the cores while the
//! store is timed. The fastest of [`RUNS`] is asserted rather than the median
//! the frames are held to: this is I/O against a SQLite file, which waits on
//! the disk as well as the cores. With a `yes` on every core of a 12-core Mac
//! and other builds writing, one median of 31 came to 76 ms, and the fastest
//! of each of ten such sets stayed within the budget. Code that got slower
//! makes every run slower, the fastest too.
//!
//! It is timed only in an optimised build, which is what ships: in the debug
//! suite, with a load average of thirty-five to fifty from other builds, the
//! same measurement ranged from 20 ms to over a second, and the budget
//! measured the machine.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::time::{Duration, Instant};

use niobe_core::session::SessionState;
use niobe_store::{Store, read_log};

/// The recorded `claude` bridge session `niobe-core` asserts its fold against.
const FIXTURE: &str = include_str!("../../niobe-core/tests/fixtures/claude-session.jsonl");

/// A stored session of this size must load and replay in under this long.
///
/// The budget was set against the 200-event log this fixture replaced: timed
/// on four two-vCPU CI runners, opening, loading and folding it took a median
/// of 2.0 to 2.8 ms with the rest of the suite running in parallel and 1.3 to
/// 1.7 ms alone, and the slowest single run of the 80 timed was 4.8 ms. The
/// recording that replaced it is 1.6 times the events and 2.7 times the
/// bytes, and its best run on a developer machine was 2.2 ms — so the budget
/// keeps most of the order of magnitude it was given.
const REPLAY_BUDGET: Duration = Duration::from_millis(50);

/// How many times the stored session is opened, loaded and folded.
const RUNS: usize = 31;

#[test]
#[cfg_attr(debug_assertions, ignore = "the store is timed in an optimised build")]
fn a_stored_session_loads_and_replays_inside_the_budget() {
    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    let path = dir.path().join("sessions.db");
    let session = {
        let store = Store::open(&path).expect("a store opens");
        let session = store.create_session().expect("a session is created");
        for event in read_log(FIXTURE).expect("the committed fixture parses") {
            store.append(session, &event).expect("an append succeeds");
        }
        session
    };

    let fastest = (0..RUNS)
        .map(|_| {
            let started = Instant::now();
            let store = Store::open(&path).expect("the store opens again");
            let stored = store.events(session).expect("the session loads");
            let state = SessionState::replay(stored.iter().map(|s| &s.event));
            let elapsed = started.elapsed();
            assert_eq!(stored.len(), 603);
            assert_eq!(state.totals().records, 25);
            elapsed
        })
        .min()
        .expect("the store was timed at least once");

    assert!(
        fastest < REPLAY_BUDGET,
        "open, load and replay took {fastest:?} at the fastest of {RUNS}, over the \
         {REPLAY_BUDGET:?} budget"
    );
}
