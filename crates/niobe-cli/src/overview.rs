// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What `niobe overview` prints: every session recorded in the repository,
//! folded, as one Markdown page — what went well, what went wrong, what was
//! left open, the tools the agent reached for, the paths it changed most and
//! the decisions it stated.
//!
//! Every figure is a sum of what [`SessionState`] folds each session to and
//! every list is read off the same folds. Nothing is asked of a model: a
//! summary a model wrote would be a guess with the look of a record. The
//! decisions are the one list in the agent's words, and the page says so.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::SystemTime;

use niobe_core::event::Event;
use niobe_core::session::{DecisionRecord, SessionState};
use niobe_store::{SessionId, StoredEvent};

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
    /// When each of [`SessionState::decisions`] was stated, in the same
    /// order: the fold carries no times, and the store does.
    pub decided_at: Vec<SystemTime>,
    /// Whether its last turn went on in a session a clear opened: the prompt
    /// that typed the clear is the last of this session's record, and the
    /// turn's end is the first of the next one's, so the turn this fold
    /// leaves running did not stop.
    pub cleared: bool,
}

/// Every session the store at `root` holds, folded, oldest first; none where
/// nothing has been recorded there.
///
/// The store starts a session at each clear, but a store written before it
/// did holds a clear inside the session it cleared. The fold forgets what a
/// conversation spent at its clear, so such a record folded whole would count
/// only what was spent after its last clear: each of its conversations is
/// folded as a session of its own instead, under the record's id, as the
/// store now keeps them.
pub fn read(root: &Path) -> Result<Vec<Folded>, String> {
    let Some(store) = repo::read_existing_store(root)? else {
        return Ok(Vec::new());
    };
    let mut read = Vec::new();
    for summary in store.sessions().map_err(|e| e.to_string())? {
        let stored = store.events(summary.id).map_err(|e| e.to_string())?;
        read.extend(fold(summary.id, summary.started_at, &stored));
    }
    // The store lists the newest first; a record of what was built reads
    // from the start, and a record's conversations in the order they ran.
    read.sort_by_key(|(_, bounds)| bounds.key);
    let (mut folded, bounds): (Vec<Folded>, Vec<Bounds>) = read.into_iter().unzip();
    mark_cleared(&mut folded, &bounds);
    Ok(folded)
}

/// The conversations of the record `id`, each from the clear that opened it,
/// or from the record's start, to the event before the next clear, folded,
/// with their bounds. A record with no events is one conversation of none.
///
/// One fold runs through the whole record, as it ran live, and each
/// conversation is what it held just before the clear that ended it: what a
/// clear keeps — how the session is billed, its profile and model, the plan's
/// windows — is said once in a record, before the first clear, and the
/// conversations after it are still billed by it.
fn fold(id: SessionId, created: SystemTime, stored: &[StoredEvent]) -> Vec<(Folded, Bounds)> {
    let mut folded = Vec::new();
    let mut state = SessionState::new();
    let mut decided_at = Vec::new();
    let mut first: Option<&StoredEvent> = None;
    let mut last: Option<&StoredEvent> = None;
    for event in stored {
        if matches!(event.event, Event::Cleared) && first.is_some() {
            folded.push(conversation(
                id,
                created,
                (first, last),
                state.clone(),
                std::mem::take(&mut decided_at),
                folded.len(),
            ));
            first = None;
        }
        first = first.or(Some(event));
        last = Some(event);
        state.apply(&event.event);
        decided_at.resize(state.decisions().len(), event.at);
    }
    folded.push(conversation(
        id,
        created,
        (first, last),
        state,
        decided_at,
        folded.len(),
    ));
    folded
}

/// The `part`-th conversation of the record `id`, between the events `ends`,
/// folded to `state`.
fn conversation(
    id: SessionId,
    created: SystemTime,
    ends: (Option<&StoredEvent>, Option<&StoredEvent>),
    state: SessionState,
    decided_at: Vec<SystemTime>,
    part: usize,
) -> (Folded, Bounds) {
    let (first, last) = ends;
    let bounds = Bounds {
        key: (id, part),
        first: first.map(|event| (event.at, matches!(event.event, Event::Cleared))),
        last: last.map(|event| event.at),
    };
    let folded = Folded {
        id,
        // When its first event happened, which an imported session dates
        // by the CLI's own record rather than by its import.
        started_at: first.map_or(created, |event| event.at),
        state,
        decided_at,
        cleared: false,
    };
    (folded, bounds)
}

/// When a stored session's first and last events happened, and whether the
/// first was a clear.
struct Bounds {
    /// The record it was folded from, and which of the record's
    /// conversations it is.
    key: (SessionId, usize),
    first: Option<(SystemTime, bool)>,
    last: Option<SystemTime>,
}

/// Marks each session whose running turn a later session's clear went on
/// with, `bounds[i]` being `folded[i]`'s.
///
/// The store keeps no link between the two: the journal starts a session at
/// the clear and nothing else. So each session that opens on a clear is
/// matched to the session with a turn still running whose record ended last
/// before the clear, of those created before it and not matched already:
/// another shell's session can come between the two in the store's
/// numbering, but not between the prompt that typed a clear and the clear.
fn mark_cleared(folded: &mut [Folded], bounds: &[Bounds]) {
    let mut clears: Vec<(SystemTime, (SessionId, usize))> = bounds
        .iter()
        .filter_map(|bounds| match bounds.first {
            Some((at, true)) => Some((at, bounds.key)),
            Some((_, false)) | None => None,
        })
        .collect();
    clears.sort_unstable();
    for (cleared_at, clearing) in clears {
        let before = folded
            .iter()
            .zip(bounds)
            .enumerate()
            .filter(|(_, (session, bounds))| {
                bounds.key < clearing
                    && !session.cleared
                    && session.state.turn_running()
                    && bounds.last.is_some_and(|last| last <= cleared_at)
            })
            .max_by_key(|(_, (_, bounds))| (bounds.last, bounds.key))
            .map(|(index, _)| index);
        if let Some(session) = before.and_then(|index| folded.get_mut(index)) {
            session.cleared = true;
        }
    }
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
        Ok(folded) => markdown(
            &repo::describe(root).name,
            &folded,
            Narrowing::default(),
            repo::uncommitted(root).as_deref(),
        )
        .join("\n"),
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
/// `uncommitted` is the working tree's changed files as git lists them now,
/// or `None` where git could not be read. An empty `sessions` is a page of
/// one line saying so.
pub fn markdown(
    name: &str,
    sessions: &[Folded],
    narrowing: Narrowing,
    uncommitted: Option<&[String]>,
) -> Vec<String> {
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
    section(&mut lines, "Left open", left_open(sessions, uncommitted));
    section(&mut lines, "Most used tools", tools(sessions));
    section(&mut lines, "Most changed paths", paths(sessions));
    section(
        &mut lines,
        "Decisions, as the agents said",
        decisions(sessions),
    );
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
    let succeeded = sum(sessions, |s| s.tools().succeeded());
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
            grouped(succeeded),
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
/// with sub-agents still running — and the files the sessions changed that
/// are among `uncommitted`, the working tree's changes as git lists them
/// now, `None` where git could not be read.
fn left_open(sessions: &[Folded], uncommitted: Option<&[String]>) -> Vec<String> {
    let mut lines: Vec<String> = sessions
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
            } else if s.turn_running() && !session.cleared {
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
    lines.extend(not_committed(sessions, uncommitted));
    match lines.is_empty() {
        true => vec![
            "Nothing: every session ended between turns, and what they changed is committed."
                .to_owned(),
        ],
        false => lines,
    }
}

/// The line naming the files the sessions changed that `uncommitted` holds,
/// in the order git sorts them; none where every one is committed.
///
/// A path is matched as the backend named it, which is relative to where the
/// session ran: a session started below the repository's root names its
/// files from there, and git from the root, so those are not matched.
fn not_committed(sessions: &[Folded], uncommitted: Option<&[String]>) -> Option<String> {
    let Some(uncommitted) = uncommitted else {
        return Some(
            "- Whether what the sessions changed is committed could not be read from git."
                .to_owned(),
        );
    };
    let changed: std::collections::BTreeSet<&str> = sessions
        .iter()
        .flat_map(|session| session.state.files())
        .map(|file| file.path.as_str())
        .collect();
    let mut open: Vec<&str> = uncommitted
        .iter()
        .map(String::as_str)
        .filter(|path| changed.contains(path))
        .collect();
    open.sort_unstable();
    open.dedup();
    let (shown, rest) = open.split_at(open.len().min(ROWS));
    let mut named = shown
        .iter()
        .map(|path| format!("`{}`", inline(path)))
        .collect::<Vec<_>>()
        .join(", ");
    if !rest.is_empty() {
        named.push_str(&format!(" and {} more", grouped(rest.len() as u64)));
    }
    (!open.is_empty()).then(|| {
        format!(
            "- {} the sessions changed {} not committed: {named}",
            counted(open.len() as u64, "file", "files"),
            if open.len() == 1 { "is" } else { "are" }
        )
    })
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

/// Every decision the sessions' replies stated, newest first, each with when
/// and in which session: the agent's account, said to be one.
fn decisions(sessions: &[Folded]) -> Vec<String> {
    let mut stated: Vec<(SystemTime, SessionId, &DecisionRecord)> = sessions
        .iter()
        .flat_map(|session| {
            session
                .state
                .decisions()
                .iter()
                .zip(&session.decided_at)
                .map(|(decision, at)| (*at, session.id, decision))
        })
        .collect();
    if stated.is_empty() {
        return vec!["None stated.".to_owned()];
    }
    // Stable, so two stated in one reply keep their order under the reverse.
    stated.sort_by_key(|(at, id, _)| (*at, *id));
    let mut lines = vec![
        "As each session's agent stated them in its replies: its own account of what it \
         chose, which nothing here checked. Newest first."
            .to_owned(),
        String::new(),
    ];
    // A sub-agent's decision is that agent's account and not the session's,
    // and says so.
    lines.extend(stated.into_iter().rev().map(|(at, id, decision)| {
        let by = match decision.agent {
            Some(_) => ", a sub-agent",
            None => "",
        };
        format!(
            "- {} UTC, session {id}{by}: {}",
            utc_minute(at),
            inline(&decision.summary)
        )
    }));
    lines
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
            decided_at: vec![
                UNIX_EPOCH + Duration::from_secs(1_789_732_800);
                state.decisions().len()
            ],
            state,
            cleared: false,
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
            markdown("catalog", &[], Narrowing::default(), None).join("\n")
        );
        assert_eq!(page.drain(), None, "handed over once");
    }

    /// Two clears in a row: each session's running turn is the one the next
    /// clear went on with, and a session whose turn ended is matched to none.
    #[test]
    fn each_clear_is_matched_to_the_running_turn_that_ended_last_before_it() {
        let prompt = || Event::UserMessage {
            text: "/clear".to_owned(),
        };
        let at = |minute: u64| UNIX_EPOCH + Duration::from_secs(1_789_732_800 + minute * 60);
        let bounds = |id: u32, first: u64, clear: bool, last: u64| Bounds {
            key: (id.to_string().parse().expect("a number is a session id"), 0),
            first: Some((at(first), clear)),
            last: Some(at(last)),
        };
        let mut sessions = [
            folded(1, &[prompt(), Event::TurnEnded]),
            folded(2, &[prompt()]),
            folded(3, &[Event::Cleared, Event::TurnEnded, prompt()]),
            folded(4, &[Event::Cleared, Event::TurnEnded]),
            folded(5, &[Event::Cleared, Event::TurnEnded]),
        ];
        let stored = [
            bounds(1, 0, false, 1),
            bounds(2, 2, false, 3),
            bounds(3, 3, true, 6),
            bounds(4, 6, true, 7),
            bounds(5, 9, true, 9),
        ];

        mark_cleared(&mut sessions, &stored);

        assert_eq!(
            sessions.iter().map(|s| s.cleared).collect::<Vec<_>>(),
            [false, true, true, false, false]
        );
    }

    #[test]
    fn the_decisions_are_listed_newest_first_as_the_agents_account_with_session_and_time() {
        let said = |text: &str| Event::AssistantMessage {
            text: text.to_owned(),
            agent: None,
        };
        let mut first = folded(1, &[said("DECISION: kept the cache\nDECISION: no | pipes")]);
        first.decided_at = vec![
            UNIX_EPOCH + Duration::from_secs(1_789_732_800),
            UNIX_EPOCH + Duration::from_secs(1_789_732_860),
        ];
        let mut second = folded(2, &[said("DECISION: left the exporter alone")]);
        second.decided_at = vec![UNIX_EPOCH + Duration::from_secs(1_789_819_200)];

        let lines = decisions(&[first, second]);

        assert_eq!(
            lines,
            [
                "As each session's agent stated them in its replies: its own account of what it \
                 chose, which nothing here checked. Newest first.",
                "",
                "- 2026-09-19 12:00 UTC, session 2: left the exporter alone",
                "- 2026-09-18 12:01 UTC, session 1: no | pipes",
                "- 2026-09-18 12:00 UTC, session 1: kept the cache",
            ]
        );
        assert_eq!(
            decisions(&[folded(3, &[said("No decision.")])]),
            ["None stated."]
        );
    }

    #[test]
    fn a_decision_a_sub_agent_stated_says_it_was_a_sub_agent_s() {
        let mut session = folded(
            1,
            &[Event::AssistantMessage {
                text: "DECISION: read only the parser".to_owned(),
                agent: Some("a1".into()),
            }],
        );
        session.decided_at = vec![UNIX_EPOCH + Duration::from_secs(1_789_732_800)];

        assert_eq!(
            decisions(&[session])[2],
            "- 2026-09-18 12:00 UTC, session 1, a sub-agent: read only the parser"
        );
    }

    #[test]
    fn a_repository_with_no_sessions_says_so_in_one_line() {
        assert_eq!(
            markdown("niobe", &[], Narrowing::default(), None),
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
            markdown("niobe", &[], from, None)[2],
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
            left_open(&sessions, Some(&[])),
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
    fn the_files_a_session_changed_that_are_still_uncommitted_are_left_open() {
        let changed = |path: &str| Event::FileChange {
            path: path.to_owned(),
            added: Some(1),
            removed: Some(0),
            hunks: Vec::new(),
            scope: niobe_core::event::ChangeScope::Project,
        };
        let sessions = [
            folded(1, &[changed("src/a.rs"), changed("src/b.rs")]),
            folded(2, &[changed("src/a.rs"), changed("README.md")]),
        ];
        let tree = ["README.md", "src/a.rs", "notes.txt"].map(str::to_owned);

        assert_eq!(
            left_open(&sessions, Some(&tree)),
            ["- 2 files the sessions changed are not committed: `README.md`, `src/a.rs`"]
        );
        assert_eq!(
            left_open(&sessions, Some(&[])),
            ["Nothing: every session ended between turns, and what they changed is committed."]
        );
        assert_eq!(
            left_open(&sessions, None),
            ["- Whether what the sessions changed is committed could not be read from git.",]
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
    fn a_denied_or_stopped_call_is_not_counted_as_finishing_without_failing() {
        let session = folded(
            1,
            &[
                call("a", "Read", ToolOutcome::Ok),
                call("b", "Bash", ToolOutcome::Failed),
                call("c", "Bash", ToolOutcome::Denied),
                call("d", "Bash", ToolOutcome::Interrupted),
            ]
            .concat(),
        );
        assert_eq!(
            highlights(&[session])[2],
            "- 1 of 4 tool calls finished without failing."
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
        let lines = left_open(&[folded(4, &events)], Some(&[]));
        assert_eq!(
            lines,
            ["- Session 4: stopped waiting on 1 question — `niobe --resume 4`"]
        );
        assert_eq!(
            left_open(&[folded(5, &[])], Some(&[])),
            ["Nothing: every session ended between turns, and what they changed is committed."]
        );
    }
}
