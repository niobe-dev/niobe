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

use common::{MARKDOWN_REPLY, at_work, hunk, running_session, screen};
use niobe_core::event::Event;
use niobe_tui::app::{App, WorkingFile};
use niobe_tui::theme::{Depth, THEMES};

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
            agent: None,
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

/// File changes in a long session's transcript, each drawn as its diff.
const LONG_SESSION_DIFFS: usize = 50;

/// The redraw every tick makes in a long session of edits, each drawn under
/// its call as the lines it changed: two hunks of a realistic size, with
/// lines long enough to be cut at the pane's edge. Like a reply, a diff is
/// laid out once and drawn from that after, so the frame costs what its
/// visible rows cost.
#[test]
fn a_long_session_of_diffs_redraws_inside_a_frame_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = session_of_diffs();
    let _ = screen(&mut app, 200, 60);

    let median = median_frame(&mut app, &[(200, 60)]);

    assert!(
        median <= FRAME_BUDGET,
        "the median frame of {LONG_SESSION_DIFFS} diffs at 200x60 took {median:?}, over the \
         {FRAME_BUDGET:?} budget"
    );
}

/// The same redraw with every diff opened to all its rows: each of these is
/// two hunks one row past the cut, so every entry holds more rows, and the
/// frame still costs only what its visible rows cost.
#[test]
fn a_long_session_of_opened_diffs_redraws_inside_a_frame_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = session_of_diffs();
    app.open_diffs();
    let _ = screen(&mut app, 200, 60);

    let median = median_frame(&mut app, &[(200, 60)]);

    assert!(
        median <= FRAME_BUDGET,
        "the median frame of {LONG_SESSION_DIFFS} opened diffs at 200x60 took {median:?}, over \
         the {FRAME_BUDGET:?} budget"
    );
}

/// A running session with [`LONG_SESSION_DIFFS`] edits in it, each drawn
/// under its call as the lines it changed.
fn session_of_diffs() -> App {
    let mut app = running_session();
    for n in 0..LONG_SESSION_DIFFS {
        let id = format!("edit-{n}");
        let path = format!("crates/niobe-{}/src/module_{n:02}.rs", n % 7);
        app.apply(&Event::ToolCallStart {
            id: id.as_str().into(),
            name: "Edit".to_owned(),
            input: path.clone(),
            summary: Some(path.clone()),
            agent: None,
        });
        app.apply(&Event::ToolCallEnd {
            id: id.as_str().into(),
            name: "Edit".to_owned(),
            input: path.clone(),
            output: "updated".to_owned(),
            bytes: 64,
            outcome: niobe_core::event::ToolOutcome::Ok,
            summary: Some(path.clone()),
            exit_code: None,
            error: None,
        });
        app.apply(&Event::FileChange {
            path,
            added: Some(8),
            removed: Some(4),
            hunks: vec![edit_hunk(40), edit_hunk(210)],
        });
    }
    app
}

/// Three lines of context each side of two removed lines and four added ones,
/// which is what the backend reports for an ordinary edit.
fn edit_hunk(start: u64) -> niobe_core::diff::Hunk {
    hunk(
        start,
        start,
        &[
            "     pub fn label_for(fold: &Fold, total: Money) -> Label {",
            "         let unsettled = fold.unsettled_count();",
            "         // Every figure the pane shows is measured or labelled, never a plausible guess.",
            "-        if unsettled > 0 { return Label::Estimate(total) }",
            "-        Label::Measured(total)",
            "+        match (unsettled, fold.unpriced_models().is_empty()) {",
            "+            (0, _) => Label::Measured(total),",
            "+            (_, true) => Label::Estimate(total),",
            "+            (_, false) => Label::Floor(total), // any unpriced model makes the figure a floor, which the pane marks",
            "         }",
            "     }",
            " ",
        ],
    )
}

/// The redraw every tick makes while a turn is running, in every theme: the
/// long session of markdown replies with the desktop moving behind it. The
/// motion is worked out for the cells the panes leave uncovered and no more,
/// so it costs the frame next to nothing; if it ever costs more, it is the
/// motion that gives way, not this budget.
#[test]
fn a_running_turn_redraws_inside_a_frame_budget_in_every_theme() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for theme in THEMES {
        for depth in [Depth::Sixteen, Depth::TrueColour] {
            let mut app = running_session().with_depth(depth).with_theme(theme);
            for _ in 0..LONG_SESSION_REPLIES {
                app.apply(&Event::UserMessage {
                    text: "What changed?".to_owned(),
                });
                app.apply(&Event::AssistantMessage {
                    text: MARKDOWN_REPLY.to_owned(),
                    agent: None,
                });
            }
            let mut app = at_work(app, Duration::from_secs(3));
            let _ = screen(&mut app, 200, 60);

            let median = median_frame(&mut app, &[(200, 60)]);

            assert!(
                median <= FRAME_BUDGET,
                "{} at {depth:?}: the median frame of a running turn at 200x60 took \
                 {median:?}, over the {FRAME_BUDGET:?} budget",
                theme.name
            );
        }
    }
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

/// A session busy enough that the Activity pane has to build every row it
/// holds: four sub-agents, ten decisions and a dozen tools.
///
/// The pane builds them all on every frame for the same reason the Changes
/// pane does — that is what tells it how far it can be scrolled — so what it
/// costs is timed rather than assumed.
const BUSY_AGENTS: usize = 4;
const BUSY_DECISIONS: usize = 10;
const BUSY_TOOLS: usize = 12;

#[test]
fn a_busy_activity_pane_redraws_inside_a_frame_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = running_session();

    for n in 0..BUSY_AGENTS {
        app.apply(&Event::AgentSpawn {
            id: format!("busy-{n}").into(),
            parent: None,
            label: format!("explorer-{n} → crates/niobe-core/src/session.rs"),
        });
    }
    for n in 0..BUSY_DECISIONS {
        app.apply(&Event::Decision {
            summary: format!(
                "Decision {n}: keep the fold the one place a number comes from, so two \
                 consumers cannot disagree about what the session spent."
            ),
            rationale: None,
            rejected: vec![],
        });
    }
    for n in 0..BUSY_TOOLS {
        app.apply(&Event::ToolCallEnd {
            id: format!("busy-t{n}").into(),
            name: format!("mcp__claude_ai_Server{n}__do-the-thing"),
            input: "{}".to_owned(),
            output: String::new(),
            bytes: 1_024,
            outcome: niobe_core::event::ToolOutcome::Ok,
            summary: None,
            exit_code: None,
            error: None,
        });
    }
    let _ = screen(&mut app, 200, 60);

    let median = median_frame(&mut app, &[(200, 60)]);

    assert!(
        median <= FRAME_BUDGET,
        "the median frame with {BUSY_AGENTS} agents, {BUSY_DECISIONS} decisions and \
         {BUSY_TOOLS} tools at 200x60 took {median:?}, over the {FRAME_BUDGET:?} budget"
    );
}
