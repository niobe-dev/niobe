// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Which of the repository's new commits this session made.
//!
//! A repository is shared: another agent, an editor or the operator in a
//! second terminal can commit to it while this session is open, and what git
//! records says nothing about which process made a commit. What this session
//! does know is when each of its own calls ran and what it ran. A commit is
//! claimed as the session's where its commit time falls inside a call of this
//! session — the agent's or a `!` command the operator ran here — that named
//! `git`, and nowhere else. A commit that cannot be placed inside one is left
//! out: a commit wrongly claimed is a claim about work nobody in this session
//! did, while one left out is only a row missing.
//!
//! The calls are the ones whose input names `git` rather than every call,
//! because a long one — a test run, a sub-agent working for minutes — would
//! otherwise claim whatever anybody else committed while it ran.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use niobe_core::event::ToolCallId;

/// How far either side of a call a commit is still placed inside it.
///
/// A commit's time is whole seconds, cut rather than rounded, and the call's
/// ends are stamped as the shell takes the event off the channel, a tick
/// after the backend sent it at most. A second covers both.
const SLACK: Duration = Duration::from_secs(1);

/// The stretches of time this session's calls to git ran for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GitCalls {
    /// Each call that named git and started with a time, in the order they
    /// started: when it started, and when it ended where it has.
    ran: Vec<(SystemTime, Option<SystemTime>)>,
    /// The calls of [`GitCalls::ran`] that have not ended, by where they are
    /// in it.
    running: BTreeMap<ToolCallId, usize>,
}

impl GitCalls {
    /// Notes a call that started at `at`, where its `input` names git.
    ///
    /// A call with no time — folded from a log that kept none — cannot place
    /// anything inside it, so it is not kept.
    pub fn started(&mut self, id: &ToolCallId, input: &str, at: Option<SystemTime>) {
        if let Some(at) = at.filter(|_| names_git(input)) {
            self.running.insert(id.clone(), self.ran.len());
            self.ran.push((at, None));
        }
    }

    /// Notes that a call ended at `at`. One that ended with no time is taken
    /// to have ended as it started, rather than to be running still.
    pub fn ended(&mut self, id: &ToolCallId, at: Option<SystemTime>) {
        if let Some((from, to)) = self.running.remove(id).and_then(|i| self.ran.get_mut(i)) {
            *to = Some(at.unwrap_or(*from).max(*from));
        }
    }

    /// Ends, at `at`, every call that has not ended and is not in `running`:
    /// the backend that ran them is gone, and a call that never ends would
    /// claim every commit made after it.
    pub fn stopped<V>(&mut self, running: &BTreeMap<ToolCallId, V>, at: Option<SystemTime>) {
        let gone: Vec<ToolCallId> = self
            .running
            .keys()
            .filter(|id| !running.contains_key(*id))
            .cloned()
            .collect();
        for id in gone {
            self.ended(&id, at);
        }
    }

    /// Whether a commit made at `at` was made inside one of the calls, where a
    /// call still running runs to `now`.
    pub fn made(&self, at: SystemTime, now: Option<SystemTime>) -> bool {
        self.ran.iter().any(|&(from, to)| {
            let Some(to) = to.or(now) else {
                return false;
            };
            whole_second(from).checked_sub(SLACK).unwrap_or(UNIX_EPOCH) <= at
                && at <= to.checked_add(SLACK).unwrap_or(to)
        })
    }
}

/// `at` with its fraction of a second cut off, which is what git does to the
/// time it writes into a commit.
fn whole_second(at: SystemTime) -> SystemTime {
    let seconds = at
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    UNIX_EPOCH + Duration::from_secs(seconds)
}

/// Whether `input` runs git: the word `git` on its own, as a command line or
/// the JSON a tool's arguments arrive as holds it, and not inside a path such
/// as `.gitignore` or a name such as `digit`.
fn names_git(input: &str) -> bool {
    input
        .split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_' || c == '.'))
        .any(|word| word == "git")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn at_millis(millis: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(millis)
    }

    fn id(name: &str) -> ToolCallId {
        ToolCallId::new(name)
    }

    fn ran(input: &str, from: u64, to: u64) -> GitCalls {
        let mut calls = GitCalls::default();
        calls.started(&id("t"), input, Some(at(from)));
        calls.ended(&id("t"), Some(at(to)));
        calls
    }

    #[test]
    fn a_commit_made_while_a_call_to_git_ran_is_the_sessions() {
        let calls = ran(r#"{"command":"git commit -m \"fix\""}"#, 100, 103);
        assert!(calls.made(at(102), Some(at(500))));
    }

    #[test]
    fn a_commit_made_while_no_call_of_the_session_ran_is_not_the_sessions() {
        let calls = ran("git commit -m fix", 100, 103);
        assert!(!calls.made(at(50), Some(at(500))));
        assert!(!calls.made(at(200), Some(at(500))));
    }

    #[test]
    fn a_commit_made_while_a_call_that_does_not_run_git_ran_is_not_the_sessions() {
        // A long test run is where another process's commit would otherwise
        // be claimed.
        let calls = ran(r#"{"command":"cargo test --workspace"}"#, 100, 400);
        assert!(!calls.made(at(200), Some(at(500))));
    }

    #[test]
    fn git_inside_a_path_or_a_word_is_not_a_call_to_git() {
        assert!(!names_git(r#"{"file_path":".gitignore"}"#));
        assert!(!names_git("cat .git/HEAD"));
        assert!(!names_git("count the digits"));
        assert!(names_git("cd sub && git add a.rs && git commit -m x"));
        assert!(names_git(r#"{"command":"git push"}"#));
    }

    #[test]
    fn a_commit_is_placed_inside_a_call_by_the_whole_second_git_wrote() {
        // Started at .900 of second 100 and committed in that second: git
        // writes 100, which is before the start to the millisecond.
        let mut calls = GitCalls::default();
        calls.started(&id("t"), "git commit", Some(at_millis(100_900)));
        calls.ended(&id("t"), Some(at_millis(101_200)));
        assert!(calls.made(at(100), None));
    }

    #[test]
    fn a_call_still_running_holds_what_is_committed_up_to_now() {
        let mut calls = GitCalls::default();
        calls.started(&id("t"), "git rebase main", Some(at(100)));
        assert!(calls.made(at(150), Some(at(160))));
        assert!(!calls.made(at(170), Some(at(160))));
        assert!(
            !calls.made(at(150), None),
            "with no clock there is no now for a running call to reach"
        );
    }

    #[test]
    fn a_call_the_session_stopped_under_holds_nothing_made_after() {
        let mut calls = GitCalls::default();
        calls.started(&id("t"), "git commit", Some(at(100)));
        calls.stopped(&BTreeMap::<ToolCallId, ()>::new(), Some(at(110)));
        assert!(calls.made(at(105), Some(at(900))));
        assert!(!calls.made(at(800), Some(at(900))));
    }

    #[test]
    fn a_call_with_no_time_places_no_commit() {
        let mut calls = GitCalls::default();
        calls.started(&id("t"), "git commit", None);
        calls.ended(&id("t"), None);
        assert!(!calls.made(at(0), Some(at(10))));
        assert_eq!(calls, GitCalls::default());
    }
}
