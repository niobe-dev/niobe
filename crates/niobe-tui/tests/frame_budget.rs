// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! How long the shell takes to redraw, to take a long line into the
//! composer, and to show a file written from scratch.
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
use niobe_tui::app::{App, Arrival, COMPOSER_CAP, WorkingFile};
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

/// How deep the quotes of [`a_reply_quoted_thousands_deep_redraws_inside_a_frame_budget`]
/// are nested, and how many lines are drawn inside them: a reply of 22 KB,
/// which a model can write.
const QUOTE_DEPTH: usize = 8000;
const QUOTED_LINES: usize = 2000;

/// A resize of a reply that opens thousands of quotes inside one another and
/// draws lines inside the deepest. Every level is a block the renderer opens
/// and closes and every line sits behind all of them, and the reply is
/// rendered again on every delta while it streams, so a cost per level that
/// grows with the depth, or a cost per line that does, stalls the screen.
#[test]
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
fn a_reply_quoted_thousands_deep_redraws_inside_a_frame_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = running_session();
    app.apply(&Event::AssistantMessage {
        text: format!(
            "{}{}deep",
            "> ".repeat(QUOTE_DEPTH),
            "line\\\n".repeat(QUOTED_LINES)
        ),
        agent: None,
    });
    let frame = screen(&mut app, 200, 60);
    assert!(frame.contains("deep"), "the reply is drawn: {frame}");

    let median = median_frame(&mut app, &[(200, 60), (180, 60)]);

    assert!(
        median <= FRAME_BUDGET,
        "the median resized frame of a reply quoted {QUOTE_DEPTH} deep took {median:?}, over \
         the {FRAME_BUDGET:?} budget"
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

/// Replies in the transcript a search is held open over, each
/// [`MARKDOWN_REPLY`].
const SEARCHED_REPLIES: usize = 600;

/// The redraw every tick makes with a search open over a long session, a
/// phrase of every reply found: the bar counts the matches before anything is
/// drawn, so the frame has to know them all, and an entry is searched when
/// it is drawn again rather than on every frame.
#[test]
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
fn a_search_open_over_a_long_session_redraws_inside_a_frame_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = running_session();
    for _ in 0..SEARCHED_REPLIES {
        app.apply(&Event::UserMessage {
            text: "What changed?".to_owned(),
        });
        app.apply(&Event::AssistantMessage {
            text: MARKDOWN_REPLY.to_owned(),
            agent: None,
        });
    }
    app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    for c in "answered from the lru".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let frame = screen(&mut app, 200, 60);
    // The session the replies are added to says the phrase once itself.
    let found = SEARCHED_REPLIES + 1;
    assert!(
        frame.contains(&format!(" of {found} ")),
        "every reply's matches are counted, or the frame times nothing: {frame}"
    );

    let median = median_frame(&mut app, &[(200, 60)]);

    assert!(
        median <= FRAME_BUDGET,
        "the median frame with a search open over {SEARCHED_REPLIES} markdown replies at \
         200x60 took {median:?}, over the {FRAME_BUDGET:?} budget"
    );
}

/// The size of the one reply an idle session holds: an agent that printed a
/// generated file, or a log, back into the conversation.
const HUGE_REPLY_BYTES: usize = 20 * 1024 * 1024;

/// What holding a reply of [`HUGE_REPLY_BYTES`] may add to the redraw an idle
/// shell makes every tick, ten a second: 1% of one core. A fresh idle shell
/// spends about as much again, and the two together stay under 2%, which is
/// what a shell left open beside the editor may cost.
const IDLE_ALLOWANCE: Duration = Duration::from_millis(1);

/// The redraw every tick makes while nothing happens, with one reply of
/// [`HUGE_REPLY_BYTES`] in the transcript, against the same session's redraw
/// without it. The reply was laid out once when it arrived; a frame that asks
/// again whether it changed must not cost what reading all of it costs.
///
/// The two sessions' frames are timed in turn, so that what else the machine
/// is doing slows both alike.
#[test]
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
fn an_idle_session_holding_a_twenty_megabyte_reply_redraws_as_fast_as_one_without() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let idle = |reply: String| {
        let mut app = running_session();
        app.apply(&Event::UserMessage {
            text: "Print the generated catalog.".to_owned(),
        });
        app.apply(&Event::AssistantMessage {
            text: reply,
            agent: None,
        });
        app.apply(&Event::TurnEnded);
        let _ = screen(&mut app, 200, 60);
        app
    };
    let mut holding = idle(MARKDOWN_REPLY.repeat(HUGE_REPLY_BYTES / MARKDOWN_REPLY.len()));
    let mut without = idle(MARKDOWN_REPLY.to_owned());

    let mut times: Vec<(Duration, Duration)> = (0..FRAMES)
        .map(|_| (frame_time(&mut holding), frame_time(&mut without)))
        .collect();
    times.sort_unstable_by_key(|(holding, without)| holding.saturating_sub(*without));
    let (holding, without) = times[FRAMES / 2];

    assert!(
        holding.saturating_sub(without) <= IDLE_ALLOWANCE,
        "the median idle frame holding a {HUGE_REPLY_BYTES}-byte reply at 200x60 took \
         {holding:?} against {without:?} without it, over the {IDLE_ALLOWANCE:?} allowance"
    );
}

/// How long a key typed into a search over a long transcript may take to
/// land and be drawn: what the composer's longest paste is held to.
const SEARCH_KEY_BUDGET: Duration = Duration::from_millis(50);

/// A key typed into the search over a session holding a reply of
/// [`HUGE_REPLY_BYTES`]: every key changes what is looked for, so every key
/// looks through the whole transcript again. What the transcript's lines
/// read as to a search is read once, by the first key, and kept with them;
/// the keys after it, a letter typed and taken back in turn, are timed.
#[test]
#[cfg_attr(debug_assertions, ignore = "a key is timed in an optimised build")]
fn a_key_typed_into_a_search_over_a_twenty_megabyte_reply_lands_inside_a_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = running_session();
    app.apply(&Event::UserMessage {
        text: "Print the generated catalog.".to_owned(),
    });
    app.apply(&Event::AssistantMessage {
        text: MARKDOWN_REPLY.repeat(HUGE_REPLY_BYTES / MARKDOWN_REPLY.len()),
        agent: None,
    });
    app.apply(&Event::TurnEnded);
    app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    for c in "lru".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let frame = screen(&mut app, 200, 60);
    assert!(
        frame.contains(" of "),
        "the reply's matches are counted, or the keys time nothing:\n{frame}"
    );

    let mut keys: Vec<Duration> = (0..FRAMES)
        .map(|n| {
            let key = match n % 2 {
                0 => KeyCode::Backspace,
                _ => KeyCode::Char('u'),
            };
            let started = Instant::now();
            app.on_key(KeyEvent::new(key, KeyModifiers::NONE));
            let _ = screen(&mut app, 200, 60);
            started.elapsed()
        })
        .collect();
    keys.sort_unstable();
    let median = keys[FRAMES / 2];

    assert!(
        median <= SEARCH_KEY_BUDGET,
        "the median key typed into a search over a {HUGE_REPLY_BYTES}-byte reply took \
         {median:?} to land and draw, over the {SEARCH_KEY_BUDGET:?} budget"
    );
}

/// How long one frame of `app` takes to draw at 200x60.
fn frame_time(app: &mut App) -> Duration {
    let started = Instant::now();
    let _ = screen(app, 200, 60);
    started.elapsed()
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
            command: None,
            untrimmed: None,
        });
        app.apply(&Event::FileChange {
            path,
            added: Some(8),
            removed: Some(4),
            hunks: vec![edit_hunk(40), edit_hunk(210)],
            scope: niobe_core::event::ChangeScope::Project,
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

/// Lines in a file written from scratch, as an agent writes a generated file
/// or a lock file: a diff of every line in it, which the transcript cuts at
/// twenty rows.
const WRITTEN_LINES: usize = 100_000;

/// How long a file of [`WRITTEN_LINES`] may take to reach the transcript and
/// be drawn there. Longer than a frame, because the change is hashed and
/// laid out once when it arrives; short enough that the shell does not stop.
const WRITE_BUDGET: Duration = Duration::from_millis(50);

/// How many times the write is applied and drawn. Odd, so the median is one
/// of them; few, because each needs a session and a file of its own.
const WRITES: usize = 7;

/// A file written from scratch reaches the transcript and is drawn there
/// without a pause the operator would notice, however many lines it has:
/// the transcript draws twenty of them, and what that costs grows with the
/// file no faster than its length.
#[test]
#[cfg_attr(debug_assertions, ignore = "a write is timed in an optimised build")]
fn a_hundred_thousand_line_write_lands_and_draws_inside_the_write_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let written = written_file();

    let mut times: Vec<Duration> = (0..WRITES)
        .map(|_| {
            let mut app = running_session();
            let _ = screen(&mut app, 200, 60);
            let write = write_of(&written);
            let started = Instant::now();
            for event in &write {
                app.apply(event);
            }
            let frame = screen(&mut app, 200, 60);
            let took = started.elapsed();
            assert!(
                frame.contains("… 99980 more rows"),
                "the write is drawn cut, or its time says nothing: {frame}"
            );
            took
        })
        .collect();
    times.sort_unstable();
    let median = times[WRITES / 2];

    assert!(
        median <= WRITE_BUDGET,
        "a {WRITTEN_LINES}-line write took {median:?} to apply and draw, over the \
         {WRITE_BUDGET:?} budget"
    );
}

/// A permission question for a write of [`WRITTEN_LINES`] lines is drawn with
/// every tick while it waits, and the file is in the call it draws from: the
/// frame has to cost what the twenty rows it shows do, not the file. Timed
/// cut, as it comes up, and under Ctrl+T, which lays the whole change out.
#[test]
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
fn a_question_for_a_hundred_thousand_line_write_redraws_inside_the_write_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let content = written_file();
    let path = "catalog/generated.ts";
    let mut app = running_session();
    app.apply(&Event::PermissionRequest {
        id: "huge".into(),
        tool: "Write".to_owned(),
        input: serde_json::json!({ "file_path": path, "content": content }).to_string(),
        target: Some(path.to_owned()),
        proposal: Some(niobe_core::Proposal::new(
            niobe_core::Action::Write {
                path: path.to_owned(),
                content: content.clone(),
                existed: Some(false),
            },
            ["file_path", "content"],
        )),
        reason: None,
        agent: None,
    });
    let first = screen(&mut app, 200, 60);
    assert!(
        first.contains("more rows"),
        "the change is drawn cut, or its time says nothing: {first}"
    );

    let cut = median_frame(&mut app, &[(200, 60)]);
    assert!(
        cut <= WRITE_BUDGET,
        "the median frame of a {WRITTEN_LINES}-line write's question took {cut:?}, over \
         {WRITE_BUDGET:?}"
    );

    app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    let _ = screen(&mut app, 200, 60);
    let open = median_frame(&mut app, &[(200, 60)]);
    assert!(
        open <= WRITE_BUDGET,
        "the median frame of the same question under Ctrl+T took {open:?}, over \
         {WRITE_BUDGET:?}"
    );
}

/// A resize with a file of [`WRITTEN_LINES`] in the transcript lays its diff
/// out again at every width, and still redraws inside a frame.
#[test]
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
fn a_resize_redraws_a_hundred_thousand_line_write_inside_a_frame_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = running_session();
    for event in &write_of(&written_file()) {
        app.apply(event);
    }
    let _ = screen(&mut app, 200, 60);

    let median = median_frame(&mut app, &[(200, 60), (180, 60)]);

    assert!(
        median <= FRAME_BUDGET,
        "the median resized frame with a {WRITTEN_LINES}-line write at 200x60 took \
         {median:?}, over the {FRAME_BUDGET:?} budget"
    );
}

/// A file of [`WRITTEN_LINES`] lines, each different.
fn written_file() -> String {
    (1..=WRITTEN_LINES)
        .map(|n| format!("export const entry{n:06} = \"W/{n:06}\";\n"))
        .collect()
}

/// The events of a `Write` that created a file holding `written`: the call,
/// and the change it made, as the backend reports a file it created.
fn write_of(written: &str) -> Vec<Event> {
    let path = "catalog/generated.ts".to_owned();
    vec![
        Event::ToolCallStart {
            id: "write".into(),
            name: "Write".to_owned(),
            input: path.clone(),
            summary: Some(path.clone()),
            agent: None,
        },
        Event::ToolCallEnd {
            id: "write".into(),
            name: "Write".to_owned(),
            input: path.clone(),
            output: String::new(),
            bytes: written.len() as u64,
            outcome: niobe_core::event::ToolOutcome::Ok,
            summary: Some(path.clone()),
            exit_code: None,
            error: None,
            command: None,
            untrimmed: None,
        },
        Event::FileChange {
            path,
            added: Some(WRITTEN_LINES as u64),
            removed: Some(0),
            hunks: vec![niobe_core::diff::Hunk::created(written).expect("lines written")],
            scope: niobe_core::event::ChangeScope::Project,
        },
    ]
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
            cache_write_1h: Some(0),
            reasoning: 0,
            model: TIERED_MODELS[(n % 2) as usize].to_owned(),
            cost_usd: None,
            settles_model: Some(false),
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

/// A working tree far larger than the pane can show: the Changes pane lists
/// every row it holds on every frame, because that is what tells it how far it
/// can be scrolled, and lays out the ones on screen. A big refactor is the
/// case that makes that expensive, so it is the case that is timed.
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

/// A working tree of a directory nobody ignored yet — a package manager's
/// install, a build's output — where every file is new and the pane holds far
/// more rows than a screen: what a frame costs must not grow with them.
const HUGE_WORKING_TREE: usize = 50_000;

#[test]
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
fn a_huge_working_tree_redraws_inside_a_frame_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = running_session();
    let mut repo = app.repo().clone();
    repo.working = (0..HUGE_WORKING_TREE)
        .map(|n| WorkingFile {
            path: format!("node_modules/pkg-{:03}/lib/file_{n:05}.js", n / 64),
            added: Some(n as u64 % 400),
            removed: Some(0),
            new: true,
        })
        .collect();
    app.set_repo(repo);
    let _ = screen(&mut app, 200, 60);

    let median = median_frame(&mut app, &[(200, 60)]);

    assert!(
        median <= FRAME_BUDGET,
        "the median frame with {HUGE_WORKING_TREE} changed files at 200x60 took \
         {median:?}, over the {FRAME_BUDGET:?} budget"
    );
}

/// A session busy enough that the Activity pane has to build every row it
/// holds: four sub-agents and a dozen tools.
///
/// The pane builds them all on every frame, because that is what tells it
/// how far it can be scrolled, so what it costs is timed rather than assumed.
const BUSY_AGENTS: usize = 4;
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
            kind: None,
            label: format!("explorer-{n} → crates/niobe-core/src/session.rs"),
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
            command: None,
            untrimmed: None,
        });
    }
    let _ = screen(&mut app, 200, 60);

    let median = median_frame(&mut app, &[(200, 60)]);

    assert!(
        median <= FRAME_BUDGET,
        "the median frame with {BUSY_AGENTS} agents and {BUSY_TOOLS} tools at 200x60 took {median:?}, over the {FRAME_BUDGET:?} budget"
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
/// would notice, as long as the composer holds.
#[test]
#[cfg_attr(debug_assertions, ignore = "a paste is timed in an optimised build")]
fn a_line_as_long_as_the_composer_holds_pasted_lands_inside_the_paste_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let text = typed_line(COMPOSER_CAP);

    let mut times: Vec<Duration> = (0..FRAMES)
        .map(|_| {
            let mut app = running_session();
            let _ = screen(&mut app, 200, 60);
            let started = Instant::now();
            app.on_paste(&text);
            let took = started.elapsed();
            assert_eq!(
                app.composer().lines()[0].len(),
                COMPOSER_CAP,
                "the paste did not land whole, so its time says nothing"
            );
            took
        })
        .collect();
    times.sort_unstable();
    let median = times[FRAMES / 2];

    assert!(
        median <= PASTE_BUDGET,
        "a {COMPOSER_CAP}-byte one-line paste took {median:?} to land, over the \
         {PASTE_BUDGET:?} budget"
    );
}

/// A key typed into a prompt as long as the composer holds lands, and the
/// frame after it is drawn, inside a frame. The composer's editor lays out
/// the whole of what it holds again after every edit, which is what
/// [`COMPOSER_CAP`] is set by: a megabyte on one line took 90 ms a key.
#[test]
#[cfg_attr(debug_assertions, ignore = "a key is timed in an optimised build")]
fn a_key_typed_into_a_prompt_at_the_composer_cap_lands_and_draws_inside_a_frame_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = running_session();
    app.on_paste(&typed_line(COMPOSER_CAP - FRAMES));
    let _ = screen(&mut app, 200, 60);

    let mut times: Vec<Duration> = (0..FRAMES)
        .map(|_| {
            let started = Instant::now();
            app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
            let _ = screen(&mut app, 200, 60);
            started.elapsed()
        })
        .collect();
    assert_eq!(
        app.composer().lines()[0].len(),
        COMPOSER_CAP,
        "every key landed, or their times say nothing"
    );
    times.sort_unstable();
    let median = times[FRAMES / 2];

    assert!(
        median <= FRAME_BUDGET,
        "a key typed into a {COMPOSER_CAP}-byte line took {median:?} to land and draw, over \
         the {FRAME_BUDGET:?} budget"
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

/// Prompts in a repository's history that has been used for a long while.
const REMEMBERED_PROMPTS: usize = 20_000;

/// Sessions those prompts were sent in.
const REMEMBERED_SESSIONS: usize = 400;

/// A repository's history as long as [`REMEMBERED_PROMPTS`], each prompt about
/// two kilobytes and no two alike, with the history dialog open on `view`.
fn session_remembering_many_prompts(sessions_view: bool) -> App {
    use niobe_tui::history::{Past, PastPrompt, PastSession, Target};

    let mut app = running_session().remembers();
    let target = |n: usize| Target::Recorded(format!("{}", n % REMEMBERED_SESSIONS + 1));
    let body = "Refactor the catalog fetcher so a 304 answers from the cache. ".repeat(32);
    app.set_past(Past {
        prompts: (0..REMEMBERED_PROMPTS)
            .map(|n| PastPrompt {
                text: format!("Prompt {n}: {body}"),
                at: None,
                session: target(n),
            })
            .collect(),
        sessions: (0..REMEMBERED_SESSIONS)
            .map(|n| PastSession {
                target: target(n),
                last: None,
                first_prompt: None,
                title: None,
                open_elsewhere: false,
            })
            .collect(),
        unread: None,
    });
    app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    if sessions_view {
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    }
    let _ = screen(&mut app, 120, 30);
    app
}

/// The median time a key in the history dialog takes to land and the frame
/// after it to draw, over keys that filter it, unfilter it and move its
/// cursor. The filter is a letter no prompt holds, so every prompt is read
/// to the end to find that.
fn median_history_key(app: &mut App) -> Duration {
    let keys = [
        KeyCode::Char('x'),
        KeyCode::Backspace,
        KeyCode::Down,
        KeyCode::Down,
    ];
    let mut times: Vec<Duration> = (0..FRAMES)
        .map(|n| {
            let started = Instant::now();
            app.on_key(KeyEvent::new(keys[n % keys.len()], KeyModifiers::NONE));
            let _ = screen(app, 120, 30);
            started.elapsed()
        })
        .collect();
    times.sort_unstable();
    times[FRAMES / 2]
}

/// The history dialog over a history of [`REMEMBERED_PROMPTS`]: each key
/// filters or moves through every prompt there is, and has to land and be
/// drawn inside a frame.
#[test]
#[cfg_attr(debug_assertions, ignore = "a key is timed in an optimised build")]
fn a_key_in_the_history_dialog_over_twenty_thousand_prompts_lands_and_draws_inside_a_frame_budget()
{
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = session_remembering_many_prompts(false);
    let frame = screen(&mut app, 120, 30);
    assert!(
        frame.contains("Prompt 0: Refactor"),
        "the history is listed, or the keys time nothing:\n{frame}"
    );

    let median = median_history_key(&mut app);

    assert!(
        median <= FRAME_BUDGET,
        "a key in the history dialog over {REMEMBERED_PROMPTS} prompts took {median:?} to \
         land and draw, over the {FRAME_BUDGET:?} budget"
    );
}

/// The same history listed by session: each session's prompts are searched
/// by the filter, so a key has the same prompts to go through.
#[test]
#[cfg_attr(debug_assertions, ignore = "a key is timed in an optimised build")]
fn a_key_in_the_history_dialogs_sessions_over_twenty_thousand_prompts_lands_and_draws_inside_a_frame_budget()
 {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = session_remembering_many_prompts(true);
    let frame = screen(&mut app, 120, 30);
    assert!(
        frame.contains("#1 "),
        "the sessions are listed, or the keys time nothing:\n{frame}"
    );

    let median = median_history_key(&mut app);

    assert!(
        median <= FRAME_BUDGET,
        "a key in the history dialog's sessions over {REMEMBERED_PROMPTS} prompts took \
         {median:?} to land and draw, over the {FRAME_BUDGET:?} budget"
    );
}

/// How large an output the view of one call is timed reading: a megabyte.
const LARGE_OUTPUT: usize = 1 << 20;

/// A call whose output is a megabyte is opened from the list of calls and
/// paged through: what is wrapped is what is on screen, so the frame costs a
/// screenful whether the megabyte is short lines or one line of minified
/// JSON. Each frame is a key and the draw after it: opening the call, or a
/// page down through it.
#[test]
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
fn a_call_with_a_megabyte_of_output_opens_and_pages_inside_a_frame_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lines = "x".repeat(99) + "\n";
    for (shape, output) in [
        ("short lines", lines.repeat(LARGE_OUTPUT / lines.len())),
        ("one line", "{\"k\":1},".repeat(LARGE_OUTPUT / 8)),
    ] {
        let mut app = running_session();
        app.apply(&Event::ToolCallEnd {
            id: "large".into(),
            name: "Bash".to_owned(),
            input: "{\"command\":\"cat big.json\"}".to_owned(),
            bytes: output.len() as u64,
            output,
            outcome: niobe_core::event::ToolOutcome::Ok,
            summary: Some("cat big.json".to_owned()),
            exit_code: Some(0),
            error: None,
            command: None,
            untrimmed: None,
        });
        let _ = screen(&mut app, 200, 60);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        let mut frames: Vec<Duration> = (0..FRAMES)
            .map(|frame| {
                let started = Instant::now();
                if frame % 8 == 0 {
                    app.on_key(key(KeyCode::Esc));
                    app.perform(niobe_tui::menu::Action::Calls);
                    app.on_key(key(KeyCode::Enter));
                } else {
                    app.on_key(key(KeyCode::PageDown));
                }
                let _ = screen(&mut app, 200, 60);
                started.elapsed()
            })
            .collect();
        assert!(app.detail().is_some(), "the call is open");
        frames.sort_unstable();
        let median = frames[FRAMES / 2];
        let slowest = frames[FRAMES - 1];

        assert!(
            median <= FRAME_BUDGET,
            "{shape}: the median frame reading a {LARGE_OUTPUT}-byte output took {median:?}, \
             over the {FRAME_BUDGET:?} budget"
        );
        assert!(
            slowest <= FRAME_BUDGET * 2,
            "{shape}: the slowest frame, opening the call, took {slowest:?}"
        );
    }
}

/// How large a prompt the history dialog is timed showing: a megabyte, as a
/// log pasted into a `claude` session read in here is.
const LARGE_PROMPT: usize = 1 << 20;

/// An earlier session asked a megabyte, and the history dialog's cursor
/// rests on it, in its prompts and in its sessions: what is wrapped of it is
/// what fits under the list, so the frame costs a screenful whether the
/// megabyte is short lines, one line of ASCII or one line of another script.
#[test]
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
fn a_megabyte_prompt_under_the_history_dialogs_cursor_draws_inside_a_frame_budget() {
    use niobe_tui::history::{Past, PastPrompt, PastSession, Target};

    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lines = "error: the fetch failed at line 42 of the log\n";
    let shapes = [
        ("short lines", lines.repeat(LARGE_PROMPT / lines.len())),
        ("one line", "word ".repeat(LARGE_PROMPT / 5)),
        ("one line of Cyrillic", "журнал ".repeat(LARGE_PROMPT / 13)),
    ];
    for (shape, body) in shapes {
        for sessions_view in [false, true] {
            let mut app = running_session().remembers();
            let earlier = Target::Recorded("1".to_owned());
            app.set_past(Past {
                prompts: vec![PastPrompt {
                    text: format!("megabyte {body}"),
                    at: None,
                    session: earlier.clone(),
                }],
                sessions: vec![PastSession {
                    target: earlier,
                    last: None,
                    first_prompt: None,
                    title: None,
                    open_elsewhere: false,
                }],
                unread: None,
            });
            let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
            app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
            if sessions_view {
                app.on_key(key(KeyCode::Tab));
            }
            for c in "megabyte".chars() {
                app.on_key(key(KeyCode::Char(c)));
            }
            let frame = screen(&mut app, 120, 30);
            assert!(
                frame.contains("megabyte"),
                "{shape}: the prompt is under the cursor, or nothing is timed:\n{frame}"
            );

            let mut frames: Vec<Duration> = (0..FRAMES)
                .map(|_| {
                    let started = Instant::now();
                    let _ = screen(&mut app, 120, 30);
                    started.elapsed()
                })
                .collect();
            frames.sort_unstable();
            let median = frames[FRAMES / 2];

            assert!(
                median <= FRAME_BUDGET,
                "{shape}, sessions {sessions_view}: the median frame with a {LARGE_PROMPT}-byte \
                 prompt under the history dialog's cursor took {median:?}, over the \
                 {FRAME_BUDGET:?} budget"
            );
        }
    }
}

/// Replies in the transcript [`an_export_of_a_long_session_lands_inside_a_frame_budget`]
/// copies, each [`MARKDOWN_REPLY`] under a prompt: a working day's session.
const EXPORTED_REPLIES: usize = 2000;

/// The transcript copied to the clipboard from a long session. It is laid
/// out already, so the copy costs a walk over its lines; it is made on the
/// draw path, between two frames, so it has to fit in one.
#[test]
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
fn an_export_of_a_long_session_lands_inside_a_frame_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = running_session();
    for _ in 0..EXPORTED_REPLIES {
        app.apply(&Event::UserMessage {
            text: "What changed?".to_owned(),
        });
        app.apply(&Event::AssistantMessage {
            text: MARKDOWN_REPLY.to_owned(),
            agent: None,
        });
    }
    let _ = screen(&mut app, 200, 60);

    let mut exports: Vec<Duration> = (0..FRAMES)
        .map(|_| {
            let started = Instant::now();
            app.perform(niobe_tui::menu::Action::Export);
            let handoffs = app.take_handoffs();
            let elapsed = started.elapsed();
            assert!(
                matches!(handoffs.as_slice(), [niobe_tui::desktop::Handoff::Copy(text)] if text.contains("What changed?")),
                "the transcript is copied: {handoffs:?}"
            );
            elapsed
        })
        .collect();
    exports.sort_unstable();
    let median = exports[FRAMES / 2];

    assert!(
        median <= FRAME_BUDGET,
        "the median export of {EXPORTED_REPLIES} markdown replies took {median:?}, over the \
         {FRAME_BUDGET:?} budget"
    );
}

/// Drag events past an edge of the transcript in
/// [`a_selection_across_a_long_session_is_copied_inside_a_frame_budget`]:
/// each scrolls the pane a line, and these are more than the transcript of
/// [`EXPORTED_REPLIES`] replies has, so the selection runs from one end of it
/// to the other.
const DRAGS_PAST_THE_EDGE: usize = 200_000;

/// A drag that selects the whole transcript of a long session, scrolling it
/// as it goes, let go: the text is copied on the draw path, between two
/// frames, so it has to fit in one.
#[test]
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
fn a_selection_across_a_long_session_is_copied_inside_a_frame_budget() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = running_session();
    for _ in 0..EXPORTED_REPLIES {
        app.apply(&Event::UserMessage {
            text: "What changed?".to_owned(),
        });
        app.apply(&Event::AssistantMessage {
            text: MARKDOWN_REPLY.to_owned(),
            agent: None,
        });
    }
    let mouse = |kind, column, row| MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    let mut copies: Vec<Duration> = (0..FRAMES)
        .map(|copy| {
            let frame = screen(&mut app, 200, 60);
            // From the tail up past the top, then from the top down past the
            // bottom, so each copy runs the length of the transcript.
            let upwards = copy % 2 == 0;
            let rows: Vec<(usize, &str)> = frame
                .lines()
                .enumerate()
                .filter(|(_, line)| line.contains("What changed?"))
                .collect();
            let (row, line) = match upwards {
                true => rows.last(),
                false => rows.first(),
            }
            .copied()
            .expect("a prompt is on screen");
            let byte = line
                .find("What changed?")
                .expect("the line was found by it");
            let column = u16::try_from(line[..byte].chars().count()).expect("200 wide");
            let row = u16::try_from(row).expect("60 high");
            let past = match upwards {
                true => 0,
                false => 59,
            };
            app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), column, row));
            for _ in 0..DRAGS_PAST_THE_EDGE {
                app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), column, past));
            }
            let started = Instant::now();
            app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), column, past));
            let handoffs = app.take_handoffs();
            let elapsed = started.elapsed();
            assert!(
                matches!(
                    handoffs.as_slice(),
                    [niobe_tui::desktop::Handoff::Copy(text)]
                        if text.matches("What changed?").count() + 1 >= EXPORTED_REPLIES
                ),
                "the whole transcript is copied"
            );
            elapsed
        })
        .collect();
    copies.sort_unstable();
    let median = copies[FRAMES / 2];

    assert!(
        median <= FRAME_BUDGET,
        "the median copy of a selection across {EXPORTED_REPLIES} markdown replies took \
         {median:?}, over the {FRAME_BUDGET:?} budget"
    );
}

/// How large a memory file the Memory view is timed reading: the most the
/// binary reads of one, a mebibyte.
const LARGE_MEMORY: usize = 1 << 20;

/// A memory file as large as the binary reads is opened with Enter in the
/// Memory view and paged through: what is wrapped is what is on screen, so
/// the frame costs a screenful whether the mebibyte is short lines or one
/// line. Each frame is a key and the draw after it: opening the file, or a
/// page down through it.
#[test]
#[cfg_attr(debug_assertions, ignore = "a frame is timed in an optimised build")]
fn a_mebibyte_memory_file_opens_and_pages_inside_a_frame_budget() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lines = "- run the gate before every commit, and say what it found\n";
    let shapes = [
        ("short lines", lines.repeat(LARGE_MEMORY / lines.len())),
        ("one line", "word ".repeat(LARGE_MEMORY / 5)),
    ];
    for (shape, text) in shapes {
        let mut app = running_session();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.on_key(key(KeyCode::F(8)));
        assert!(app.take_memory_request(), "F8 asked for no read");
        app.set_memory(vec![niobe_tui::MemoryFile {
            scope: niobe_tui::memory::Scope::Project,
            path: "CLAUDE.md".to_owned(),
            named: "CLAUDE.md".to_owned(),
            depth: 0,
            bytes: Some(text.len() as u64),
            text: Some(text),
            note: None,
            editable: None,
        }]);
        app.on_key(key(KeyCode::Enter));
        let _ = screen(&mut app, 200, 60);

        let mut frames: Vec<Duration> = (0..FRAMES)
            .map(|frame| {
                let started = Instant::now();
                if frame % 8 == 0 {
                    app.on_key(key(KeyCode::Esc));
                    app.on_key(key(KeyCode::Enter));
                } else {
                    app.on_key(key(KeyCode::PageDown));
                }
                let _ = screen(&mut app, 200, 60);
                started.elapsed()
            })
            .collect();
        assert!(
            app.memory().is_some_and(|view| view.reading().is_some()),
            "{shape}: the file is open, or nothing is timed"
        );
        frames.sort_unstable();
        let median = frames[FRAMES / 2];

        assert!(
            median <= FRAME_BUDGET,
            "{shape}: the median frame reading a {LARGE_MEMORY}-byte memory file took \
             {median:?}, over the {FRAME_BUDGET:?} budget"
        );
    }
}
