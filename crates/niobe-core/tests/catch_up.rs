// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The catch-up of a recorded session against the fold of the same events:
//! what [`CatchUp`] says happened after a mark is what folding only the
//! events after it says.

use niobe_core::catch_up::{CatchUp, Mark};
use niobe_core::event::Event;
use niobe_core::session::SessionState;

const SESSION: &str = include_str!("fixtures/claude-session.jsonl");

fn recorded_events() -> Vec<Event> {
    SESSION
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line {} is not an Event: {e}", i + 1))
        })
        .collect()
}

#[test]
fn every_figure_of_the_catch_up_is_the_fold_of_the_events_after_the_mark() {
    let events = recorded_events();
    // Each prompt opens a turn, so a mark taken just before one leaves no
    // turn half on either side of it.
    let prompts: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| matches!(event, Event::UserMessage { .. }))
        .map(|(at, _)| at)
        .collect();
    for &split in &prompts[1..] {
        let mut session = SessionState::replay(&events[..split]);
        let mark = Mark::of(&session);
        for event in &events[split..] {
            session.apply(event);
        }
        let caught = CatchUp::since(&mark, &session);
        let after = SessionState::replay(&events[split..]);

        let lines = |files: &[niobe_core::FileChanges]| -> Vec<(String, u64, u64, u64)> {
            files
                .iter()
                .map(|file| (file.path.clone(), file.added, file.removed, file.changes))
                .collect()
        };
        assert_eq!(
            lines(&caught.files),
            lines(after.files()),
            "split at {split}"
        );
        assert_eq!(
            caught.agents_started,
            after.agents_spawned(),
            "split at {split}"
        );
        assert_eq!(
            caught.agents_finished,
            after.agents_completed() + after.agents_failed() + after.agents_cancelled(),
            "split at {split}"
        );
        assert_eq!(caught.errors, after.errors(), "split at {split}");
        assert_eq!(
            caught.asked,
            after.permission_requests(),
            "split at {split}"
        );
        assert_eq!(caught.turns.len(), after.turns().len(), "split at {split}");
        assert_eq!(caught.waiting, after.pending_permissions().len());
    }
}

#[test]
fn the_fixture_s_later_stretches_hold_something_to_catch_up_on() {
    let events = recorded_events();
    let session = SessionState::replay(&events);
    let caught = CatchUp::since(&Mark::default(), &session);
    assert_eq!(caught.agents_started, 3);
    assert_eq!(caught.files.len(), 1, "{:?}", caught.files);
    assert_eq!(
        (caught.files[0].added, caught.files[0].removed),
        (21, 4),
        "20+1 added and 4 removed, by the fixture's two file changes"
    );
}
