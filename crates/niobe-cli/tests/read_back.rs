// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A session as it ran and as it reads back.
//!
//! Every recording of the `claude` CLI's stream is translated as a live
//! session's would be and folded into a shell as its lines arrive, with the
//! operator answering every question the shell puts, and everything handed to
//! a real session store dated by the clock it arrived on. The store is opened
//! again, read back and folded into a second shell as `--resume` folds one,
//! under the standing answers the config holds by then, and again under
//! none. The fold, every
//! transcript entry — how much each asks of the operator included — the
//! sub-agents, the time worked and the 120×30 frame must be the same, and so
//! must what `niobe --resume` and `niobe replay` print for it: a figure that
//! reads one way while the session runs and another once it is opened again
//! teaches the operator to trust neither.
//!
//! What a read-back is documented to show apart from the live shell is left
//! out of the comparison, each for its reason: [`App::resumed`] dates the
//! plan's windows as from before the session opened again, which is what it
//! is for, and the loop's own notices — a spike, the budget, files changed
//! outside the edit tools — are said once, by the loop, never by the fold.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use niobe_bridge_claude::Translator;
use niobe_core::event::Event;
use niobe_core::permission::Allowlist;
use niobe_store::{Recorder, SessionId, Store};
use niobe_tui::app::{Answer, App, Repo};
use niobe_tui::clock::Clock;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// When the recordings are said to start, on a UTC clock.
const STARTED: u64 = 1_789_689_600 - 2 * 3_600;

/// The recordings of the CLI's own stream; its transcripts are read by a
/// reader of their own and are not a stream.
fn recordings() -> Vec<PathBuf> {
    let dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../niobe-bridge-claude/tests/fixtures");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("the bridge's fixtures are there")
        .map(|entry| entry.expect("a directory entry reads").path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "jsonl")
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().contains("transcript"))
        })
        .collect();
    found.sort();
    assert!(found.len() > 20, "the recordings are found: {found:?}");
    found
}

fn utc() -> Clock {
    Clock::fixed(0).expect("UTC is a timezone")
}

/// A shell attached to a backend, as a live session's is and as `--resume`
/// opens one: only an attached shell takes an answer to a question.
fn shell() -> App {
    App::new(Repo {
        name: "niobe".to_owned(),
        branch: Some("main".to_owned()),
        ..Repo::default()
    })
    .attached()
}

/// The moment the `n`th line of a recording arrived at: a second apart, so
/// that every turn and call takes a time the screen shows.
fn arrived(n: usize) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(STARTED) + Duration::from_secs(n as u64)
}

/// A recording run as a live session: each line drained on a pass of the
/// loop that ticked at the moment it arrived, its events folded in and handed
/// to the journal, then what the operator did about it, as the loop hands
/// over what the shell produced.
fn live(recording: &Path, store: Store) -> (App, SessionId) {
    let text = std::fs::read_to_string(recording).expect("the recording reads");
    let mut translator = Translator::new("max");
    let mut app = shell();
    let mut recorder = Recorder::new(store);
    let keep = |recorder: &mut Recorder, event: &Event, at: SystemTime| {
        // The journal starts a session of its own at a clear, as the binary's does.
        if matches!(event, Event::Cleared) {
            recorder.start_over();
        }
        recorder
            .record_at(event, at)
            .expect("the store keeps the event");
    };
    let started = Instant::now();
    let mut between = true;
    for (n, line) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let at = arrived(n);
        app.tick(started + Duration::from_secs(n as u64), Some(utc().at(at)));
        for event in prompted(translator.line(line), &mut between) {
            app.apply(&event);
            keep(&mut recorder, &event, at);
        }
        answer_as_the_operator(&mut app);
        for event in app.take_produced() {
            keep(&mut recorder, &event, at);
        }
    }
    let session = recorder
        .session()
        .expect("the recording recorded something");
    (app, session)
}

/// The events of one line, with the prompt that opened a turn before its
/// first event, as the shell folds the operator's prompt in before anything
/// the backend answers it with: the recordings hold the CLI's side alone.
fn prompted(events: Vec<Event>, between: &mut bool) -> Vec<Event> {
    let mut out = Vec::with_capacity(events.len() + 1);
    for event in events {
        let opens = matches!(
            event,
            Event::AssistantDelta { .. }
                | Event::AssistantMessage { .. }
                | Event::ToolCallStart { .. }
                | Event::Usage(_)
        );
        if opens && std::mem::take(between) {
            out.push(Event::UserMessage {
                text: "go on".to_owned(),
            });
        }
        if matches!(event, Event::TurnEnded) {
            *between = true;
        }
        out.push(event);
    }
    out
}

/// What the operator does about the questions a line put: a standing rule
/// answers what it covers, as the loop has it answer before anything is
/// drawn, and the operator allows the rest for good where the question offers
/// that — so a later question the answer covers is one a rule answers.
fn answer_as_the_operator(app: &mut App) {
    app.settle_rules();
    while let Some(ask) = app.asking() {
        let options = ask.options();
        let answer = [Answer::AlwaysTarget, Answer::AlwaysTool]
            .into_iter()
            .find(|always| options.contains(always))
            .unwrap_or(Answer::Once);
        app.answer(answer);
        app.settle_rules();
    }
}

/// The stored session folded into a shell as `--resume` folds it, under
/// `rules`: the standing answers the config holds when it is opened.
fn read_back(store: &Store, session: SessionId, rules: Allowlist) -> App {
    let stored = store.events(session).expect("the session reads back");
    let mut app = shell().with_rules(rules);
    app.extend_at(stored.iter().map(|s| (&s.event, utc().at(s.at))));
    app
}

fn frame(app: &mut App, at: SystemTime) -> String {
    app.tick(Instant::now(), Some(utc().at(at)));
    let mut terminal =
        Terminal::new(TestBackend::new(120, 30)).expect("a test backend cannot fail");
    terminal
        .draw(|frame| niobe_tui::ui::draw(frame, app))
        .expect("a test backend cannot fail");
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            let row: String = (0..buffer.area.width)
                .map(|x| buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()))
                .collect();
            format!("{row}\n")
        })
        .collect()
}

/// What `niobe --resume` and `niobe replay` print for the stored session,
/// each without the line that says where the events came from.
fn printed(dir: &Path, store: &Store, session: SessionId) -> (String, String) {
    let log: String = store
        .events(session)
        .expect("the session reads back")
        .iter()
        .map(|stored| {
            let mut value = serde_json::to_value(&stored.event).expect("an event is JSON");
            let at = stored.at.duration_since(UNIX_EPOCH).expect("after 1970");
            value["at"] = serde_json::Value::from(
                u64::try_from(at.as_millis()).expect("a recording's time fits"),
            );
            format!("{value}\n")
        })
        .collect();
    let path = dir.join("log.jsonl");
    std::fs::write(&path, log).expect("the log is written");
    let run = |args: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_niobe"))
            .args(args)
            .current_dir(dir)
            .env("XDG_CONFIG_HOME", dir.join("no-user-config"))
            .env("CLAUDE_CONFIG_DIR", dir.join("no-claude-sessions"))
            .env_remove("HOME")
            .output()
            .expect("the binary runs");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .skip(1)
            .map(|line| format!("{line}\n"))
            .collect::<String>()
    };
    let id = session.get().to_string();
    (
        run(&["--resume", &id]),
        run(&["replay", path.to_str().expect("a UTF-8 path")]),
    )
}

/// Every way a recording reads differently once read back, by name.
fn differences(recording: &Path) -> Vec<String> {
    let name = recording
        .file_name()
        .expect("a recording has a name")
        .to_string_lossy()
        .into_owned();
    let dir = tempfile::tempdir().expect("a scratch directory");
    std::fs::create_dir(dir.path().join(".git")).expect("a .git directory is made");
    std::fs::create_dir(dir.path().join(".niobe")).expect("a .niobe directory is made");
    let path = dir.path().join(".niobe").join("sessions.db");
    let (mut ran, session) = live(recording, Store::open(&path).expect("the store opens"));
    let store = Store::open(&path).expect("the store opens again");
    let mut back = read_back(&store, session, ran.allowed().clone());

    let mut differ = Vec::new();
    // A clear starts a session of its own in the store, which begins at the
    // clear: what the CLI said only before it — the commands it offers, the
    // plan's windows — the live shell carries over and the new session's
    // record does not hold. The fold and the frame are compared only where
    // nothing was cleared.
    let cleared = session.get() > 1;
    if !cleared && ran.session() != back.session() {
        differ.push(format!(
            "{name}: the fold differs\nlive: {:#?}\nback: {:#?}",
            ran.session(),
            back.session()
        ));
    }
    let (live_entries, back_entries) = (ran.entries(), back.entries());
    if live_entries != back_entries {
        let first = live_entries
            .iter()
            .zip(back_entries)
            .position(|(l, b)| l != b);
        differ.push(format!(
            "{name}: the transcript differs ({} entries live, {} back), first at {first:?}\n\
             live: {:#?}\nback: {:#?}",
            live_entries.len(),
            back_entries.len(),
            first.map(|i| &live_entries[i]),
            first.map(|i| &back_entries[i]),
        ));
    }
    // Opened where none of those answers is in force — the session's own
    // taken out of the config since, or given to `-p --allow` for that run
    // alone — the record still says who answered each question.
    let unruled = read_back(&store, session, Allowlist::default());
    if unruled.entries() != live_entries {
        differ.push(format!(
            "{name}: the transcript differs read back with no standing answers"
        ));
    }
    if ran.agents() != back.agents() {
        differ.push(format!("{name}: the sub-agents differ"));
    }
    if ran.worked() != back.worked() {
        differ.push(format!(
            "{name}: worked {:?} live, {:?} back",
            ran.worked(),
            back.worked()
        ));
    }
    let decided = |app: &App| -> Vec<_> {
        app.decisions()
            .map(|(decision, at)| (decision.clone(), at))
            .collect()
    };
    if decided(&ran) != decided(&back) {
        differ.push(format!("{name}: the decisions differ"));
    }
    let end = arrived(100_000);
    let (live_frame, back_frame) = (frame(&mut ran, end), frame(&mut back, end));
    if !cleared && live_frame != back_frame {
        differ.push(format!(
            "{name}: the frame differs\nlive:\n{live_frame}\nback:\n{back_frame}"
        ));
    }
    let (resumed, replayed) = printed(dir.path(), &store, session);
    if resumed != replayed {
        differ.push(format!(
            "{name}: printed differently\n--resume:\n{resumed}\nreplay:\n{replayed}"
        ));
    }
    differ
}

#[test]
fn every_recording_reads_back_as_it_ran() {
    let differ: Vec<String> = recordings()
        .iter()
        .flat_map(|recording| differences(recording))
        .collect();
    assert!(differ.is_empty(), "{}", differ.join("\n\n"));
}
