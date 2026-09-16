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

use std::time::{Duration, Instant};

use common::{running_session, screen};
use niobe_tui::app::App;

/// A frame is drawn inside a 60 Hz budget at the largest supported snapshot
/// size, so a resize redraws without a visible stutter. The test binary is a
/// debug build, which draws this frame several times slower than the release
/// binary does, so the budget holds for the release binary with room to spare.
const FRAME_BUDGET: Duration = Duration::from_millis(16);

/// How many frames are timed. Odd, so the median is one of them.
const FRAMES: usize = 31;

#[test]
fn a_resize_redraws_inside_a_frame_budget() {
    let mut app = running_session();
    // Warm: the first draw allocates what later draws reuse.
    let _ = screen(&mut app, 200, 60);

    let median = median_frame(&mut app, 200, 60);

    assert!(
        median <= FRAME_BUDGET,
        "the median frame at 200x60 took {median:?}, over the {FRAME_BUDGET:?} budget"
    );
}

/// The median time to draw one frame. A frame the scheduler took the core away
/// from says nothing about the drawing code, and a mean lets one such frame
/// push the whole figure over the budget. The median holds until half the
/// frames are slowed, which is what a slower drawing path does.
fn median_frame(app: &mut App, width: u16, height: u16) -> Duration {
    let mut frames: Vec<Duration> = (0..FRAMES)
        .map(|_| {
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
