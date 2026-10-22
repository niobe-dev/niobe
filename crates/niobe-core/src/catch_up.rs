// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What happened since the operator last looked: the turns that ended, the
//! files changed and by how much, the sub-agents started and finished, how far
//! the five-hour window moved, and what is waiting on them now.
//!
//! Worked out from the session fold and nothing else — no model is asked to
//! sum anything up — as the difference between the fold now and a [`Mark`] of
//! it taken when the operator last looked. A difference of two folds is the
//! fold of the events between them, so every figure here is one the session's
//! own panes would show for that stretch of it.

use std::collections::BTreeMap;

use crate::event::UsageWindow;
use crate::session::{FileChanges, SessionState, TurnRecord};

/// The figures of a session at the moment the operator last looked, which a
/// later [`CatchUp`] is counted from.
///
/// A conversation started over ([`crate::Event::Cleared`]) begins its figures
/// from nothing, so a mark of the one before it means nothing in it: whoever
/// keeps a mark starts it over there, at [`Mark::default`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mark {
    turns: usize,
    files: BTreeMap<String, FileChanges>,
    agents_spawned: u64,
    agents_finished: u64,
    errors: u64,
    asked: u64,
    five_hour: Option<UsageWindow>,
}

impl Mark {
    /// The figures of `session` as it stands.
    pub fn of(session: &SessionState) -> Self {
        Self {
            turns: session.turns().len(),
            files: session
                .files()
                .iter()
                .map(|file| (file.path.clone(), file.clone()))
                .collect(),
            agents_spawned: session.agents_spawned(),
            agents_finished: finished(session),
            errors: session.errors(),
            asked: session.permission_requests(),
            five_hour: session
                .usage_windows()
                .and_then(|windows| windows.five_hour),
        }
    }
}

/// What happened in a session since a [`Mark`] of it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CatchUp {
    /// The turns that ended, in the order they did.
    pub turns: Vec<TurnRecord>,
    /// Each file changed, with the lines and calls since the mark alone.
    /// [`FileChanges::why`] is the latest, as the session holds it.
    pub files: Vec<FileChanges>,
    /// Sub-agents started.
    pub agents_started: u64,
    /// Sub-agents that stopped, however they did.
    pub agents_finished: u64,
    /// Errors the backend reported.
    pub errors: u64,
    /// How far the five-hour window moved, as a share of it. `None` wherever
    /// that is not a measurement — no level at one end or the other, a level
    /// with no reset, a window that started over in between, a level that
    /// went down — read as [`TurnRecord::five_hour_share`] is.
    pub five_hour: Option<f64>,
    /// Permission questions the backend put.
    pub asked: u64,
    /// Permission questions waiting on the operator now, whenever they were
    /// put: what the catch-up says is waiting, and not news in itself.
    pub waiting: usize,
}

impl CatchUp {
    /// What happened in `session` since `mark`.
    pub fn since(mark: &Mark, session: &SessionState) -> Self {
        Self {
            turns: session
                .turns()
                .get(mark.turns..)
                .unwrap_or_default()
                .to_vec(),
            files: session
                .files()
                .iter()
                .filter_map(|file| changed_since(file, mark.files.get(&file.path)))
                .collect(),
            agents_started: session.agents_spawned().saturating_sub(mark.agents_spawned),
            agents_finished: finished(session).saturating_sub(mark.agents_finished),
            errors: session.errors().saturating_sub(mark.errors),
            asked: session.permission_requests().saturating_sub(mark.asked),
            five_hour: moved(
                mark.five_hour,
                session
                    .usage_windows()
                    .and_then(|windows| windows.five_hour),
            ),
            waiting: session.pending_permissions().len(),
        }
    }

    /// Whether nothing happened that the operator would want told: no turn
    /// ended, nothing changed, no sub-agent came or went, nothing failed and
    /// nothing was asked. The window moving on its own is not news, and nor
    /// is a question still waiting that was told of before.
    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
            && self.files.is_empty()
            && self.agents_started == 0
            && self.agents_finished == 0
            && self.errors == 0
            && self.asked == 0
    }
}

/// Every sub-agent of `session` that has stopped, however it stopped.
fn finished(session: &SessionState) -> u64 {
    session
        .agents_completed()
        .saturating_add(session.agents_failed())
        .saturating_add(session.agents_cancelled())
        .saturating_add(session.agents_interrupted())
}

/// What `now` holds beyond `then`, the same file at the mark, where a call
/// changed it since.
fn changed_since(now: &FileChanges, then: Option<&FileChanges>) -> Option<FileChanges> {
    let Some(then) = then else {
        return Some(now.clone());
    };
    let changes = now.changes.saturating_sub(then.changes);
    (changes > 0).then(|| FileChanges {
        path: now.path.clone(),
        added: now.added.saturating_sub(then.added),
        removed: now.removed.saturating_sub(then.removed),
        added_unstated: now.added_unstated.saturating_sub(then.added_unstated),
        removed_unstated: now.removed_unstated.saturating_sub(then.removed_unstated),
        changes,
        why: now.why.clone(),
    })
}

/// How far the window moved from `then` to `now`, where both are levels of
/// the same window and it did not go down.
fn moved(then: Option<UsageWindow>, now: Option<UsageWindow>) -> Option<f64> {
    let (then, now) = (then?, now?);
    let same = then.resets_at.is_some() && then.resets_at == now.resets_at;
    let share = now.utilization - then.utilization;
    (same && share >= 0.0).then_some(share)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{AgentId, AgentOutcome, ChangeScope, Event, UsageWindows};

    fn change(path: &str, added: u64, removed: u64) -> Event {
        Event::FileChange {
            path: path.to_owned(),
            added: Some(added),
            removed: Some(removed),
            hunks: Vec::new(),
            scope: ChangeScope::Project,
        }
    }

    fn window(utilization: f64, resets_at: u64) -> Event {
        Event::UsageWindows(UsageWindows {
            five_hour: Some(UsageWindow {
                utilization,
                resets_at: Some(resets_at),
            }),
            seven_day: None,
            using_overage: false,
        })
    }

    #[test]
    fn nothing_since_the_mark_is_an_empty_catch_up() {
        let mut session = SessionState::new();
        session.apply(&change("a.rs", 3, 1));
        let mark = Mark::of(&session);
        let caught = CatchUp::since(&mark, &session);
        assert!(caught.is_empty(), "{caught:?}");
        assert_eq!(caught.five_hour, None, "no window was ever reported");
    }

    #[test]
    fn a_file_changed_before_and_after_the_mark_counts_only_what_came_after() {
        let mut session = SessionState::new();
        session.apply(&change("a.rs", 3, 1));
        let mark = Mark::of(&session);
        session.apply(&change("a.rs", 5, 2));
        session.apply(&change("b.rs", 1, 0));

        let caught = CatchUp::since(&mark, &session);
        let lines: Vec<(&str, u64, u64, u64)> = caught
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.added, file.removed, file.changes))
            .collect();
        assert_eq!(lines, [("a.rs", 5, 2, 1), ("b.rs", 1, 0, 1)]);
    }

    #[test]
    fn the_window_moved_is_measured_only_within_one_window() {
        let mut session = SessionState::new();
        session.apply(&window(0.20, 1_000));
        let mark = Mark::of(&session);
        session.apply(&window(0.35, 1_000));
        let moved = CatchUp::since(&mark, &session)
            .five_hour
            .expect("the same window, read twice");
        assert!((moved - 0.15).abs() < 1e-9, "{moved}");

        session.apply(&window(0.02, 19_000));
        assert_eq!(
            CatchUp::since(&mark, &session).five_hour,
            None,
            "the window started over in between"
        );
    }

    #[test]
    fn sub_agents_started_and_finished_and_a_question_waiting_are_news() {
        let mut session = SessionState::new();
        let mark = Mark::of(&session);
        session.apply(&Event::AgentSpawn {
            id: AgentId::from("a1"),
            parent: None,
            kind: None,
            label: "look".to_owned(),
        });
        session.apply(&Event::AgentExit {
            id: AgentId::from("a1"),
            outcome: AgentOutcome::Failed,
        });
        session.apply(&Event::PermissionRequest {
            id: "t1".into(),
            tool: "Bash".to_owned(),
            input: "{}".to_owned(),
            target: None,
            proposal: None,
            reason: None,
            agent: None,
        });
        let caught = CatchUp::since(&mark, &session);
        assert_eq!(
            (
                caught.agents_started,
                caught.agents_finished,
                caught.asked,
                caught.waiting
            ),
            (1, 1, 1, 1)
        );
        assert!(!caught.is_empty());
    }
}
