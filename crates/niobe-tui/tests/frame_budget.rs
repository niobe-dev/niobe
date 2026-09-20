// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! How long the shell takes to redraw.
//!
//! This is a test binary of its own. Cargo runs test binaries one after
//! another, so no other test draws while these frames are timed; inside one
//! binary the tests run in parallel, and on a two-core CI runner the snapshot
//! tests alone made a frame take about 1.7 times as long.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

mod common;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use common::{MARKDOWN_REPLY, running_session, screen};
use niobe_core::event::Event;
use niobe_tui::app::{App, WorkingFile};

/// A frame is drawn inside a 60 Hz budget at the largest supported snapshot
/// size, so a resize redraws without a visible stutter. The test binary is a
/// debug build, which draws this frame several times slower than the release
/// binary does, so the budget holds for the release binary with room to spare.
const FRAME_BUDGET: Duration = Duration::from_millis(16);

/// How many frames are timed. Odd, so the median is one of them.
const FRAMES: usize = 31;

/// Held by each test while it times, so the tests of this binary, which the
/// harness would run in parallel, time their frames one after the other and
/// never on shared cores.
static ALONE: Mutex<()> = Mutex::new(());

/// Replies in a long session's transcript, each [`MARKDOWN_REPLY`].
const LONG_SESSION_REPLIES: usize = 60;

/// A resize: every frame at a different width, so every entry is wrapped
/// again, and the frame has to come in inside the budget anyway.
#[test]
fn a_resize_redraws_inside_a_frame_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = running_session();
    // Warm: the first draw allocates what later draws reuse.
    let _ = screen(&mut app, 200, 60);

    let median = median_frame(&mut app, &[(200, 60), (180, 60)]);

    assert!(
        median <= FRAME_BUDGET,
        "the median resized frame at 200x60 took {median:?}, over the {FRAME_BUDGET:?} budget"
    );
}

/// The redraw every tick makes, ten a second, in a long session of replies in
/// markdown. Each reply is parsed and wrapped once and drawn from what that
/// made after, so a transcript that only grows costs a frame what its visible
/// lines cost.
///
/// A resize of this session renders every reply again, once. That is timed
/// outside this test, against the release binary: in a debug build it lands
/// within a few milliseconds of the budget and would fail on a slower runner
/// for reasons that are not the drawing code's.
#[test]
fn a_long_session_of_markdown_redraws_inside_a_frame_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = running_session();
    for _ in 0..LONG_SESSION_REPLIES {
        app.apply(&Event::UserMessage {
            text: "What changed?".to_owned(),
        });
        app.apply(&Event::AssistantMessage {
            text: MARKDOWN_REPLY.to_owned(),
        });
    }
    let _ = screen(&mut app, 200, 60);

    let median = median_frame(&mut app, &[(200, 60)]);

    assert!(
        median <= FRAME_BUDGET,
        "the median frame of {LONG_SESSION_REPLIES} markdown replies at 200x60 took \
         {median:?}, over the {FRAME_BUDGET:?} budget"
    );
}

/// The median time to draw one frame, cycling through `sizes`. A frame the scheduler took the core away
/// from says nothing about the drawing code, and a mean lets one such frame
/// push the whole figure over the budget. The median holds until half the
/// frames are slowed, which is what a slower drawing path does.
fn median_frame(app: &mut App, sizes: &[(u16, u16)]) -> Duration {
    let mut frames: Vec<Duration> = (0..FRAMES)
        .map(|frame| {
            let (width, height) = sizes[frame % sizes.len()];
            let started = Instant::now();
            let _ = screen(app, width, height);
            started.elapsed()
        })
        .collect();
    frames.sort_unstable();
    frames
        .get(FRAMES / 2)
        .copied()
        .expect("FRAMES frames were timed")
}

/// A working tree far larger than the pane can show: the Changes pane builds
/// every row it holds on every frame, because that is what tells it how far it
/// can be scrolled. A big refactor is the case that makes that expensive, so
/// it is the case that is timed.
const BIG_WORKING_TREE: usize = 500;

#[test]
fn a_large_working_tree_redraws_inside_a_frame_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = running_session();
    let mut repo = app.repo().clone();
    repo.working = (0..BIG_WORKING_TREE)
        .map(|n| WorkingFile {
            path: format!("crates/niobe-{}/src/module_{n:03}.rs", n % 7),
            added: Some(n as u64),
            removed: Some(n as u64 / 3),
        })
        .collect();
    app.set_repo(repo);
    let _ = screen(&mut app, 200, 60);

    let median = median_frame(&mut app, &[(200, 60)]);

    assert!(
        median <= FRAME_BUDGET,
        "the median frame with {BIG_WORKING_TREE} changed files at 200x60 took \
         {median:?}, over the {FRAME_BUDGET:?} budget"
    );
}
