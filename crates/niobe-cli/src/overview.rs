// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What `niobe overview` prints: every session recorded in the repository,
//! folded, as one Markdown page — what went well, what went wrong, what was
//! left open, the tools the agent reached for and the paths it changed most.
//!
//! Every figure is a sum of what [`SessionState`] folds each session to and
//! every list is read off the same folds. Nothing is asked of a model: a
//! summary a model wrote would be a guess with the look of a record.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::SystemTime;

use niobe_core::session::SessionState;
use niobe_store::SessionId;

use crate::args::Narrowing;
use crate::repo;
use crate::sessions::utc_minute;
use crate::summary::{cache_write, grouped};

/// How many tools and how many paths are listed before the rest are counted.
const ROWS: usize = 10;

/// One recorded session, folded.
#[derive(Debug)]
pub struct Folded {
    /// The session, as `niobe --resume` takes it.
    pub id: SessionId,
    /// When its first event happened.
    pub started_at: SystemTime,
    /// Its events, folded.
    pub state: SessionState,
}

/// Every session the store at `root` holds, folded, oldest first; none where
/// nothing has been recorded there.
pub fn read(root: &Path) -> Result<Vec<Folded>, String> {
    let Some(store) = repo::read_existing_store(root)? else {
        return Ok(Vec::new());
    };
    let mut folded = Vec::new();
    for summary in store.sessions().map_err(|e| e.to_string())? {
        let stored = store.events(summary.id).map_err(|e| e.to_string())?;
        let mut state = SessionState::new();
        for event in &stored {
            state.apply(&event.event);
        }
        folded.push(Folded {
            id: summary.id,
            // When its first event happened, which an imported session dates
            // by the CLI's own record rather than by its import.
            started_at: stored.first().map_or(summary.started_at, |event| event.at),
            state,
        });
    }
    // The store lists the newest first; a record of what was built reads
    // from the start.
    folded.sort_by_key(|session| session.id);
    Ok(folded)
}

/// The page, for the shell's overview: written on a thread of its own each
/// time the overview opens, since folding every session a repository has
/// recorded takes as long as there are sessions, and the shell has a frame to
/// draw every tick.
#[derive(Debug)]
pub struct Page {
    root: PathBuf,
    writing: Option<Receiver<String>>,
}

impl Page {
    /// The page for the repository at `root`.
    pub fn at(root: PathBuf) -> Self {
        Self {
            root,
            writing: None,
        }
    }
}

/// The page for `root`, or why it could not be read, as the page.
fn written(root: &Path) -> String {
    match read(root) {
        Ok(folded) => {
            markdown(&repo::describe(root).name, &folded, Narrowing::default()).join("\n")
        }
        Err(error) => format!("The sessions recorded here could not be read: {error}"),
    }
}

impl niobe_tui::Overview for Page {
    fn load(&mut self) {
        if self.writing.is_some() {
            return;
        }
        let (sent, received) = mpsc::channel();
        let root = self.root.clone();
        let started = std::thread::Builder::new()
            .name("overview".to_owned())
            .spawn(move || {
                // Nobody left to hand it to is a shell that has closed.
                let _ = sent.send(written(&root));
            });
        if started.is_err() {
            // A process that can start no thread still shows the page:
            // written here, at the cost of the frames it takes.
            let (sent, received) = mpsc::channel();
            let _ = sent.send(written(&self.root));
            self.writing = Some(received);
            return;
        }
        self.writing = Some(received);
    }

    fn drain(&mut self) -> Option<String> {
        let page = match self.writing.as_ref()?.try_recv() {
            Ok(page) => Some(page),
            Err(TryRecvError::Empty) => return None,
            // The writer ended without a page: the dialog goes on saying it
            // is reading, and opening it again starts another.
            Err(TryRecvError::Disconnected) => None,
        };
        self.writing = None;
        page
    }
}

/// The sessions of `folded`, oldest first, that `narrowing` keeps: those
/// started on its day or after, and of them the newest it names.
pub fn narrowed(folded: Vec<Folded>, narrowing: Narrowing) -> Vec<Folded> {
    let mut kept: Vec<Folded> = folded
        .into_iter()
        .filter(|session| {
            narrowing
                .since
                .is_none_or(|since| niobe_ledger::Date::of(session.started_at) >= since)
        })
        .collect();
    if let Some(last) = narrowing.last {
        let cut = kept.len().saturating_sub(last);
        kept.drain(..cut);
    }
    kept
}

/// What `narrowing` kept, said as the page says it; `None` for every
/// session.
fn kept(narrowing: Narrowing) -> Option<String> {
    match (narrowing.last, narrowing.since) {
        (None, None) => None,
        (Some(last), None) => Some(format!(
            "the newest {}",
            counted(last as u64, "session", "sessions")
        )),
        (None, Some(since)) => Some(format!("the sessions started on {since} or later")),
        (Some(last), Some(since)) => Some(format!(
            "the newest {} started on {since} or later",
            counted(last as u64, "session", "sessions")
        )),
    }
}

/// The page for the sessions of the repository `name`, oldest first, which
/// `narrowing` kept of those recorded.
///
/// An empty `sessions` is a page of one line saying so.
pub fn markdown(name: &str, sessions: &[Folded], narrowing: Narrowing) -> Vec<String> {
    let mut lines = vec![format!("# What has been built in {}", inline(name))];
    let (Some(first), Some(last)) = (sessions.first(), sessions.last()) else {
        lines.push(String::new());
        lines.push(match kept(narrowing) {
            None => "No sessions recorded in this repository.".to_owned(),
            Some(kept) => format!("No session recorded in this repository is among {kept}."),
        });
        return lines;
    };
    lines.push(String::new());
    lines.push(format!(
        "{} recorded, the first started {} UTC and the last {} UTC.",
        counted(sessions.len() as u64, "session", "sessions"),
        utc_minute(first.started_at),
        utc_minute(last.started_at)
    ));
    if let Some(kept) = kept(narrowing) {
        lines.push(String::new());
        lines.push(format!(
            "Read: {kept}. `niobe overview` alone reads every one."
        ));
    }
    section(&mut lines, "Highlights", highlights(sessions));
    section(&mut lines, "Lowlights", lowlights(sessions));
    section(&mut lines, "Left open", left_open(sessions));
    section(&mut lines, "Most used tools", tools(sessions));
    section(&mut lines, "Most changed paths", paths(sessions));
    section(&mut lines, "Usage", usage(sessions));
    lines
}

fn section(lines: &mut Vec<String>, heading: &str, body: Vec<String>) {
    lines.push(String::new());
    lines.push(format!("## {heading}"));
    lines.push(String::new());
    lines.extend(body);
}

fn highlights(sessions: &[Folded]) -> Vec<String> {
    let turns = sum(sessions, |s| s.turns().len() as u64);
    let prompts = sum(sessions, SessionState::user_messages);
    let replies = sum(sessions, SessionState::assistant_messages);
    let finished = sum(sessions, |s| s.tools().finished);
    let failed = sum(sessions, |s| s.tools().failed);
    let agents = sum(sessions, SessionState::agents_completed);
    vec![
        format!("- {} ended.", counted(turns, "turn", "turns")),
        format!(
            "- {} sent, {} from the agent.",
            counted(prompts, "prompt", "prompts"),
            counted(replies, "reply", "replies")
        ),
        format!(
            "- {} of {} tool calls finished without failing.",
            grouped(finished.saturating_sub(failed)),
            grouped(finished)
        ),
        format!("- {} finished.", counted(agents, "sub-agent", "sub-agents")),
        format!(
            "- {} counted no test failing.",
            counted(
                sum(sessions, |s| s.test_runs().passed),
                "test run",
                "test runs"
            )
        ),
    ]
}

/// What went wrong, in all and then session by session, each with the
/// command that opens it.
fn lowlights(sessions: &[Folded]) -> Vec<String> {
    let mut lines = vec![
        format!(
            "- {} failed.",
            counted(
                sum(sessions, |s| s.test_runs().failed),
                "test run",
                "test runs"
            )
        ),
        format!(
            "- {} failed, {} denied.",
            counted(
                sum(sessions, |s| s.tools().failed),
                "tool call",
                "tool calls"
            ),
            grouped(sum(sessions, |s| s.tools().denied))
        ),
        format!(
            "- {} reported.",
            counted(sum(sessions, SessionState::errors), "error", "errors")
        ),
        format!(
            "- {} failed.",
            counted(
                sum(sessions, SessionState::agents_failed),
                "sub-agent",
                "sub-agents"
            )
        ),
    ];
    for session in sessions {
        let s = &session.state;
        let mut what = Vec::new();
        if s.tools().failed > 0 {
            what.push(counted(s.tools().failed, "failed call", "failed calls"));
        }
        if s.tools().denied > 0 {
            what.push(counted(s.tools().denied, "denied call", "denied calls"));
        }
        if s.test_runs().failed > 0 {
            what.push(counted(
                s.test_runs().failed,
                "failed test run",
                "failed test runs",
            ));
        }
        if s.errors() > 0 {
            what.push(counted(s.errors(), "error", "errors"));
        }
        if let Some(error) = s.fatal_error() {
            what.push(format!("ended on an error: {}", inline(error)));
        }
        if !what.is_empty() {
            lines.push(format!(
                "- Session {}: {} — `niobe --resume {}`",
                session.id,
                what.join(", "),
                session.id
            ));
        }
    }
    lines
}

/// What the record shows was not finished: a session that stopped in the
/// middle of a turn, on a question nobody answered, on a failing test run, or
/// with sub-agents still running.
fn left_open(sessions: &[Folded]) -> Vec<String> {
    let lines: Vec<String> = sessions
        .iter()
        .filter_map(|session| {
            let s = &session.state;
            let mut what = Vec::new();
            if !s.pending_permissions().is_empty() {
                what.push(format!(
                    "stopped waiting on {}",
                    counted(
                        s.pending_permissions().len() as u64,
                        "question",
                        "questions"
                    )
                ));
            } else if s.turn_running() {
                what.push("stopped in the middle of a turn".to_owned());
            }
            if s.test_run().is_some_and(|run| run.failed) {
                what.push("its last test run failed".to_owned());
            }
            if !s.running_agents().is_empty() {
                what.push(format!(
                    "{} never reported an end",
                    counted(s.running_agents().len() as u64, "sub-agent", "sub-agents")
                ));
            }
            (!what.is_empty()).then(|| {
                format!(
                    "- Session {}: {} — `niobe --resume {}`",
                    session.id,
                    what.join(", "),
                    session.id
                )
            })
        })
        .collect();
    match lines.is_empty() {
        true => vec!["Nothing: every session ended between turns.".to_owned()],
        false => lines,
    }
}

fn tools(sessions: &[Folded]) -> Vec<String> {
    let mut calls: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for session in sessions {
        let tools = session.state.tools();
        for (name, n) in &tools.by_name {
            let entry = calls.entry(name).or_default();
            entry.0 = entry.0.saturating_add(*n);
        }
        for (name, n) in &tools.failed_by_name {
            let entry = calls.entry(name).or_default();
            entry.1 = entry.1.saturating_add(*n);
        }
    }
    if calls.is_empty() {
        return vec!["No tool calls finished.".to_owned()];
    }
    let mut rows: Vec<_> = calls.into_iter().collect();
    rows.sort_by(|(a, (a_calls, _)), (b, (b_calls, _))| b_calls.cmp(a_calls).then(a.cmp(b)));
    let mut lines = vec![
        "| Tool | Calls | Failed |".to_owned(),
        "| --- | ---: | ---: |".to_owned(),
    ];
    lines.extend(rows.iter().take(ROWS).map(|(name, (calls, failed))| {
        format!(
            "| {} | {} | {} |",
            cell(name),
            grouped(*calls),
            grouped(*failed)
        )
    }));
    more(&mut lines, rows.len(), "tool", "tools");
    lines
}

/// A path's changes in every session, and how many sessions changed it.
#[derive(Default)]
struct Changed {
    added: u64,
    removed: u64,
    added_unstated: u64,
    removed_unstated: u64,
    changes: u64,
    sessions: u64,
}

fn paths(sessions: &[Folded]) -> Vec<String> {
    let mut changed: BTreeMap<&str, Changed> = BTreeMap::new();
    for session in sessions {
        for file in session.state.files() {
            let entry = changed.entry(&file.path).or_default();
            entry.added = entry.added.saturating_add(file.added);
            entry.removed = entry.removed.saturating_add(file.removed);
            entry.added_unstated = entry.added_unstated.saturating_add(file.added_unstated);
            entry.removed_unstated = entry.removed_unstated.saturating_add(file.removed_unstated);
            entry.changes = entry.changes.saturating_add(file.changes);
            entry.sessions = entry.sessions.saturating_add(1);
        }
    }
    if changed.is_empty() {
        return vec!["No files changed.".to_owned()];
    }
    let mut rows: Vec<_> = changed.into_iter().collect();
    rows.sort_by(|(a, x), (b, y)| {
        let size = |c: &Changed| c.added.saturating_add(c.removed);
        size(y)
            .cmp(&size(x))
            .then(y.sessions.cmp(&x.sessions))
            .then(a.cmp(b))
    });
    let mut lines = vec![
        "| Path | Added | Removed | Sessions |".to_owned(),
        "| --- | ---: | ---: | ---: |".to_owned(),
    ];
    lines.extend(rows.iter().take(ROWS).map(|(path, c)| {
        format!(
            "| {} | {} | {} | {} |",
            cell(path),
            lines_of(c.added, c.added_unstated, c.changes),
            lines_of(c.removed, c.removed_unstated, c.changes),
            grouped(c.sessions)
        )
    }));
    more(&mut lines, rows.len(), "path", "paths");
    lines
}

/// A line count with the marks the changes pane gives it: an em dash where
/// no call said, and a floor where only some did.
fn lines_of(stated: u64, unstated: u64, changes: u64) -> String {
    match (unstated, changes > unstated) {
        (0, _) => grouped(stated),
        (_, true) => format!("≥{}", grouped(stated)),
        (_, false) => "—".to_owned(),
    }
}

/// Tokens only. Whether a session's tokens were money depends on how each
/// was billed, which `niobe --resume <id>` prints session by session; a
/// dollar sum across sessions billed differently would add unlike things.
fn usage(sessions: &[Folded]) -> Vec<String> {
    let records = sum(sessions, |s| s.totals().records);
    if records == 0 {
        return vec!["— no usage was reported.".to_owned()];
    }
    vec![format!(
        "- {} tokens: {} in · {} out · {} cache read · {}.",
        grouped(sum(sessions, |s| s.totals().tokens())),
        grouped(sum(sessions, |s| s.totals().input)),
        grouped(sum(sessions, |s| s.totals().output)),
        grouped(sum(sessions, |s| s.totals().cache_read)),
        cache_write(
            sum(sessions, |s| s.totals().cache_write),
            sum(sessions, |s| s.totals().cache_write_1h),
            sum(sessions, |s| s.totals().cache_write_unsplit),
        )
    )]
}

fn more(lines: &mut Vec<String>, rows: usize, one: &str, many: &str) {
    if rows > ROWS {
        lines.push(String::new());
        lines.push(format!(
            "And {} more.",
            counted((rows - ROWS) as u64, one, many)
        ));
    }
}

fn sum(sessions: &[Folded], of: impl Fn(&SessionState) -> u64) -> u64 {
    sessions
        .iter()
        .map(|session| of(&session.state))
        .fold(0, u64::saturating_add)
}

fn counted(n: u64, one: &str, many: &str) -> String {
    match n {
        1 => format!("1 {one}"),
        _ => format!("{} {many}", grouped(n)),
    }
}

/// Text from a session as one table cell: a `|` would end the cell and a
/// line break the row, so each is written as Markdown reads it back.
fn cell(text: &str) -> String {
    inline(text).replace('|', "\\|")
}

/// Text from a session on one line of the page.
fn inline(text: &str) -> String {
    text.split(['\n', '\r']).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use niobe_core::event::{Event, ToolCallId, ToolOutcome};

    use super::*;

    fn folded(id: u32, events: &[Event]) -> Folded {
        let mut state = SessionState::new();
        for event in events {
            state.apply(event);
        }
        Folded {
            id: id.to_string().parse().expect("a number is a session id"),
            started_at: UNIX_EPOCH + Duration::from_secs(1_789_732_800),
            state,
        }
    }

    fn call(id: &str, name: &str, outcome: ToolOutcome) -> [Event; 2] {
        [
            Event::ToolCallStart {
                id: ToolCallId::new(id),
                name: name.to_owned(),
                input: "{}".to_owned(),
                summary: None,
                agent: None,
            },
            Event::ToolCallEnd {
                id: ToolCallId::new(id),
                name: name.to_owned(),
                input: "{}".to_owned(),
                output: String::new(),
                bytes: 0,
                outcome,
                summary: None,
                exit_code: None,
                error: None,
                command: None,
                untrimmed: None,
            },
        ]
    }

    /// The shell's page is the one `niobe overview` prints, written off the
    /// caller's thread and handed over once.
    #[test]
    fn the_shells_page_is_written_on_a_thread_and_handed_over_once() {
        use niobe_tui::Overview as _;

        let dir = tempfile::tempdir().expect("a temporary directory");
        let root = dir.path().join("catalog");
        std::fs::create_dir_all(root.join(".git")).expect("a .git directory");
        let mut page = Page::at(root.clone());
        assert_eq!(page.drain(), None, "nothing was asked for");

        page.load();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let written = loop {
            if let Some(written) = page.drain() {
                break written;
            }
            assert!(std::time::Instant::now() < deadline, "no page arrived");
            std::thread::yield_now();
        };
        assert_eq!(
            written,
            markdown("catalog", &[], Narrowing::default()).join("\n")
        );
        assert_eq!(page.drain(), None, "handed over once");
    }

    #[test]
    fn a_repository_with_no_sessions_says_so_in_one_line() {
        assert_eq!(
            markdown("niobe", &[], Narrowing::default()),
            [
                "# What has been built in niobe",
                "",
                "No sessions recorded in this repository."
            ]
        );
    }

    #[test]
    fn the_overview_keeps_the_newest_sessions_started_on_or_after_a_day() {
        let at = |id: u32, day: u64| Folded {
            started_at: UNIX_EPOCH + Duration::from_secs(day * 86_400),
            ..folded(id, &[])
        };
        let ids = |kept: Vec<Folded>| -> Vec<String> {
            kept.iter().map(|session| session.id.to_string()).collect()
        };
        let all = || vec![at(1, 20_000), at(2, 20_010), at(3, 20_020)];
        let since = niobe_ledger::Date::of(UNIX_EPOCH + Duration::from_secs(20_010 * 86_400));

        assert_eq!(ids(narrowed(all(), Narrowing::default())), ["1", "2", "3"]);
        let last = |n| Narrowing {
            last: Some(n),
            since: None,
        };
        assert_eq!(ids(narrowed(all(), last(2))), ["2", "3"]);
        assert_eq!(ids(narrowed(all(), last(9))), ["1", "2", "3"]);
        let from = Narrowing {
            last: None,
            since: Some(since),
        };
        assert_eq!(ids(narrowed(all(), from)), ["2", "3"]);
        let both = Narrowing {
            last: Some(1),
            since: Some(since),
        };
        assert_eq!(ids(narrowed(all(), both)), ["3"]);
        assert_eq!(
            markdown("niobe", &[], from)[2],
            format!(
                "No session recorded in this repository is among the sessions started on {since} or later."
            )
        );
    }

    #[test]
    fn a_session_whose_last_test_run_failed_is_left_open_and_counted() {
        let run = |failed| Event::TestRun {
            id: ToolCallId::new("t"),
            counts: None,
            exit_code: Some(if failed { 101 } else { 0 }),
            failed,
            failures: Vec::new(),
        };
        let red = folded(3, &[run(false), run(true)]);
        let green = folded(4, &[run(true), run(false)]);
        let sessions = [red, green];

        assert_eq!(
            left_open(&sessions),
            ["- Session 3: its last test run failed — `niobe --resume 3`"]
        );
        let low = lowlights(&sessions);
        assert_eq!(low[0], "- 2 test runs failed.");
        assert!(
            low.contains(&"- Session 3: 1 failed test run — `niobe --resume 3`".to_owned()),
            "{low:?}"
        );
    }

    #[test]
    fn a_tool_name_or_path_cannot_break_out_of_its_cell() {
        assert_eq!(cell("a|b\nc"), "a\\|b c");
    }

    #[test]
    fn tools_are_counted_across_sessions_most_used_first() {
        let one = folded(
            1,
            &[
                call("a", "Read", ToolOutcome::Ok),
                call("b", "Bash", ToolOutcome::Failed),
            ]
            .concat(),
        );
        let two = folded(2, &call("c", "Bash", ToolOutcome::Ok));
        assert_eq!(
            tools(&[one, two]),
            [
                "| Tool | Calls | Failed |",
                "| --- | ---: | ---: |",
                "| Bash | 2 | 1 |",
                "| Read | 1 | 0 |",
            ]
        );
    }

    #[test]
    fn a_line_count_no_call_stated_is_a_dash_and_one_some_stated_a_floor() {
        assert_eq!(lines_of(21, 0, 2), "21");
        assert_eq!(lines_of(21, 1, 2), "≥21");
        assert_eq!(lines_of(0, 2, 2), "—");
    }

    #[test]
    fn a_session_stopped_on_an_unanswered_question_is_left_open() {
        let mut events = vec![Event::UserMessage {
            text: "go".to_owned(),
        }];
        events.push(Event::PermissionRequest {
            id: ToolCallId::new("a"),
            tool: "Bash".to_owned(),
            input: "{}".to_owned(),
            target: None,
            proposal: None,
            reason: None,
            agent: None,
        });
        let lines = left_open(&[folded(4, &events)]);
        assert_eq!(
            lines,
            ["- Session 4: stopped waiting on 1 question — `niobe --resume 4`"]
        );
        assert_eq!(
            left_open(&[folded(5, &[])]),
            ["Nothing: every session ended between turns."]
        );
    }
}
