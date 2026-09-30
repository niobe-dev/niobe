// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! How long the shell takes to redraw, and to take a long line into the
//! composer.
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
use niobe_tui::app::{App, Arrival, WorkingFile};
use niobe_tui::theme::{Depth, THEMES};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A frame is drawn inside a 60 Hz budget at the largest supported snapshot
/// size, so a resize redraws without a visible stutter.
///
/// The frames are timed only in an optimised build (`cargo test --release`),
/// because that is what ships. A debug build draws them five to seven times
/// slower, between about 10 and 18 ms on an M2 Pro, so there a frame test
/// passes or fails on the machine's load rather than on the drawing code;
/// the optimised build draws the same frames in 1.5 to 3 ms.
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
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
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
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
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
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
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
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
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
/// long session of markdown replies, with the spinner under it moving.
#[test]
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
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

/// Records owed for in a session that never settles — a transcript read in
/// with no money in it, a backend that never reports any — which the shell
/// values one record at a time, as the request each was, on every frame.
const OWED_RECORDS: usize = 10_000;

/// A metered session owing for [`OWED_RECORDS`] requests over two models,
/// with the Usage pane drawing each model's valued cost and the session's.
///
/// The price sheet here stands in for the ledger's, which this crate cannot
/// name: each record is looked up by its model and priced by its own prompt
/// against a long-context threshold, which is the work the ledger does per
/// record. Timed against the bundled table itself on the machine this was
/// written on (M2 Pro, release): one valuation of all 10,000 took 365 µs and
/// the frame 1.5 ms.
#[test]
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
fn a_session_owing_for_many_requests_redraws_inside_a_frame_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = running_session().with_prices(Box::new(Tiered));
    app.apply(&Event::Billing {
        billing: niobe_core::event::Billing::Metered,
    });
    for n in 0..OWED_RECORDS as u64 {
        app.apply(&Event::Usage(niobe_core::event::Usage {
            input: 100 + n,
            output: 50,
            cache_read: 1_000 * (n % 300),
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 0,
            model: TIERED_MODELS[(n % 2) as usize].to_owned(),
            cost_usd: None,
            cost_basis: None,
            settles_model: false,
            fast: false,
        }));
    }
    let frame = screen(&mut app, 200, 60);
    assert!(frame.contains("~$"), "the owed tokens are valued: {frame}");

    let median = median_frame(&mut app, &[(200, 60)]);

    assert!(
        median <= FRAME_BUDGET,
        "the median frame owing for {OWED_RECORDS} requests at 200x60 took {median:?}, over \
         the {FRAME_BUDGET:?} budget"
    );
}

/// The models [`Tiered`] prices.
const TIERED_MODELS: [&str; 2] = ["claude-sonnet-4-5", "claude-opus-5"];

/// Prices a request as the ledger does: its model's rates, dearer past a
/// 200K prompt.
#[derive(Debug)]
struct Tiered;

impl niobe_tui::Prices for Tiered {
    fn estimate(&self, usage: &niobe_core::event::Usage) -> Option<f64> {
        let (input, output, read) = match TIERED_MODELS.iter().position(|m| *m == usage.model)? {
            0 => (3.0, 15.0, 0.3),
            _ => (5.0, 25.0, 0.5),
        };
        let prompt = usage.input + usage.cache_read + usage.cache_write;
        let dearer = if prompt > 200_000 { 2.0 } else { 1.0 };
        Some(
            dearer
                * (usage.input as f64 * input
                    + usage.output as f64 * output
                    + usage.cache_read as f64 * read)
                / 1e6,
        )
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
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
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
            new: false,
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
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
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

/// The length of the shorter line [`keys_read_together_type_a_long_line_in_time_linear_in_its_length`]
/// types; the longer is four times it.
const TYPED_LINE: usize = 5_000;

/// How many times each line is typed. The fastest of them is the one compared.
const TYPINGS: usize = 11;

/// Keys the terminal hands over in one read — a paste it did not bracket —
/// go into the composer as one insert. The composer's editor lays its whole
/// text out again after every edit, so the same line put in a key at a time
/// costs its length once per key: 10 000 characters took 1.4 s that way.
/// Four times the line has to take about four times as long, not sixteen.
///
/// The fastest typing of each line is compared rather than the median: load
/// on the machine only ever adds time, so the fastest is the one it touched
/// least. The two lines are typed in turn, so a burst of load falls on both.
/// With every core of an M2 Pro kept busy, the median of five put the ratio
/// anywhere up to 18; the fastest of eleven kept it between 3.9 and 6.
#[test]
fn keys_read_together_type_a_long_line_in_time_linear_in_its_length() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let (short, long) = (0..TYPINGS).fold((Duration::MAX, Duration::MAX), |(short, long), _| {
        (
            short.min(typing(TYPED_LINE)),
            long.min(typing(4 * TYPED_LINE)),
        )
    });

    let ratio = long.as_secs_f64() / short.as_secs_f64().max(f64::EPSILON);
    assert!(
        ratio < 8.0,
        "{TYPED_LINE} characters read together took {short:?} and four times as many \
         {long:?}: {ratio:.1} times as long, where linear is 4 and quadratic 16"
    );
}

/// How long a paste may take to reach the composer. Longer than a frame: the
/// composer's editor lays out the whole of what it holds after an edit, and
/// in this debug build that is about 25 ms for 100 KB of one line, where the
/// release binary takes about 4.
const PASTE_BUDGET: Duration = Duration::from_millis(50);

/// A line pasted whole reaches the composer without a pause the operator
/// would notice, however long.
#[test]
#[cfg_attr(debug_assertions, ignore = "a paste is timed in an optimised build")]
fn a_hundred_kilobyte_line_pasted_lands_inside_the_paste_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let text = typed_line(100_000);

    let mut times: Vec<Duration> = (0..FRAMES)
        .map(|_| {
            let mut app = running_session();
            let _ = screen(&mut app, 200, 60);
            let started = Instant::now();
            app.on_paste(&text);
            let took = started.elapsed();
            assert_eq!(
                app.composer().lines()[0].chars().count(),
                100_000,
                "the paste did not land whole, so its time says nothing"
            );
            took
        })
        .collect();
    times.sort_unstable();
    let median = times[FRAMES / 2];

    assert!(
        median <= PASTE_BUDGET,
        "a 100 KB one-line paste took {median:?} to land, over the {PASTE_BUDGET:?} budget"
    );
}

/// How long a line of `length` characters takes to type into the composer,
/// read from the terminal all at once.
fn typing(length: usize) -> Duration {
    let keys: Vec<KeyEvent> = typed_line(length)
        .chars()
        .map(|c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
        .collect();
    let mut app = running_session();
    // Drawn once, so the composer knows its width and wraps what is typed
    // into it, as it does in a session.
    let _ = screen(&mut app, 200, 60);
    let started = Instant::now();
    app.on_keys_read(
        &keys,
        Arrival {
            at: started,
            alone: false,
        },
    );
    let took = started.elapsed();
    assert_eq!(
        app.composer()
            .lines()
            .first()
            .map(|line| line.chars().count()),
        Some(length),
        "the line did not reach the composer whole"
    );
    took
}

/// Prose of `length` characters on one line: words of six letters.
fn typed_line(length: usize) -> String {
    (0..length)
        .map(|n| if n % 7 == 6 { ' ' } else { 'a' })
        .collect()
}

/// A repository as large as any the list under an `@` word has to rank.
const LISTED_FILES: usize = 200_000;

/// The list under an `@` word, open over a very large repository: every
/// frame draws it, so what ranking the listed files costs is what a frame
/// costs, and it is ranked once for the text typed rather than on every one.
#[test]
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
fn the_file_list_under_an_at_redraws_inside_a_frame_budget_over_a_large_repository() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = running_session();
    let mut repo = app.repo().clone();
    repo.files = (0..LISTED_FILES)
        .map(|n| format!("crates/part_{}/src/file{n}.rs", n % 97))
        .collect();
    app.set_repo(repo);
    app.on_paste("look @file12");
    assert!(
        !app.mention_files().0.is_empty(),
        "the list is open, or its frame would time nothing"
    );
    let _ = screen(&mut app, 200, 60);

    let median = median_frame(&mut app, &[(200, 60)]);

    assert!(
        median <= FRAME_BUDGET,
        "the median frame with the @ list open over {LISTED_FILES} files at 200x60 took \
         {median:?}, over the {FRAME_BUDGET:?} budget"
    );
}
