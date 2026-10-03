// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The session store, as the journal the shell writes through.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::SystemTime;

use niobe_core::event::Event;
use niobe_store::{Recorder, SessionId};
use niobe_tui::journal::{Journal, JournalError};

use crate::repo;

/// Records the shell's events into the session store of a repository, on a
/// thread of its own.
///
/// The shell hands each event over and goes on drawing: SQLite waits out a
/// lock another process holds on the store — a `sqlite3` shell, a backup — for
/// up to its busy timeout on every write, and on the loop that wait was a
/// frozen screen for every event. The writer keeps the events in the order
/// they were handed over, dates each by when it was, and answers for every
/// one through [`Journal::settled`].
///
/// For a new session the store is opened on the first event, not when the
/// shell opens: looking at the shell and quitting leaves no `.niobe` directory
/// behind and no empty session in the list.
#[derive(Debug)]
pub struct StoreJournal {
    /// Where events go to be written; taken when the journal is finished.
    events: Option<mpsc::Sender<Handed>>,
    answers: mpsc::Receiver<Written>,
    writer: Option<JoinHandle<()>>,
    /// Set once nothing more will be handed over: see [`write`].
    closing: Arc<AtomicBool>,
    /// Events handed over that have not been answered for yet.
    waiting: u64,
    /// The refusals of events the writer could not be handed, behind
    /// whatever it answered for the events before them.
    unsent: Vec<JournalError>,
    session: Option<SessionId>,
}

/// One event on its way to the writer, with the time it happened.
#[derive(Debug)]
struct Handed {
    event: Event,
    at: SystemTime,
}

/// How the writer kept one event, with the session it is recorded in.
type Written = Result<Option<SessionId>, JournalError>;

/// What the writer records into.
#[derive(Debug)]
enum Target {
    /// Nothing recorded yet; the store of this repository root is opened on the
    /// first event.
    Pending(PathBuf),
    /// Recording.
    Open(Recorder),
}

/// How a journal ended: the session it recorded, and the events of its last
/// moments that were never saved, which the shell had closed too soon to say.
#[derive(Debug)]
pub struct Finished {
    /// The session recorded, once anything has been.
    pub session: Option<SessionId>,
    /// How many events went unsaved after the shell last asked.
    pub unsaved: u64,
    /// Why the last of them was refused.
    pub error: Option<String>,
}

impl StoreJournal {
    /// A journal that opens the store of the repository at `root` on its first
    /// event and records a new session there.
    pub fn pending(root: PathBuf) -> Self {
        Self::start(Target::Pending(root), None)
    }

    /// A journal that goes on recording into `recorder`'s session.
    pub fn open(recorder: Recorder) -> Self {
        let session = recorder.session();
        Self::start(Target::Open(recorder), session)
    }

    fn start(target: Target, session: Option<SessionId>) -> Self {
        let (events, handed) = mpsc::channel();
        let (written, answers) = mpsc::channel();
        let closing = Arc::new(AtomicBool::new(false));
        let writer = {
            let closing = Arc::clone(&closing);
            std::thread::spawn(move || write(target, &handed, &written, &closing))
        };
        Self {
            events: Some(events),
            answers,
            writer: Some(writer),
            closing,
            waiting: 0,
            unsent: Vec::new(),
            session,
        }
    }

    /// The session being recorded, once the writer has said it recorded
    /// anything.
    pub fn session(&self) -> Option<SessionId> {
        self.session
    }

    /// Waits for every event handed over to be written or refused, and says
    /// what became of those the shell was never told about.
    pub fn finish(mut self) -> Finished {
        self.close();
        let mut finished = Finished {
            session: None,
            unsaved: 0,
            error: None,
        };
        for refused in self.settled().into_iter().filter_map(Result::err) {
            finished.unsaved = finished.unsaved.saturating_add(1);
            finished.error = Some(refused.to_string());
        }
        // A writer that panicked answered for none of what it still held.
        finished.unsaved = finished.unsaved.saturating_add(self.waiting);
        finished.session = self.session;
        finished
    }

    fn close(&mut self) {
        self.closing.store(true, Ordering::Release);
        self.events = None;
        if let Some(writer) = self.writer.take() {
            // A panic on the writer is already reported by the hook; what it
            // left unwritten is counted from `waiting`.
            drop(writer.join());
        }
    }

    fn answered(&mut self, written: Written) -> Result<(), JournalError> {
        self.waiting = self.waiting.saturating_sub(1);
        let session = written?;
        if session.is_some() {
            self.session = session;
        }
        Ok(())
    }
}

impl Journal for StoreJournal {
    fn recorded_as(&self) -> Option<String> {
        self.session().map(|session| session.to_string())
    }

    fn append(&mut self, event: &Event) {
        panic_if_the_environment_asks();
        let handed = Handed {
            event: event.clone(),
            at: SystemTime::now(),
        };
        match &self.events {
            Some(events) if events.send(handed).is_ok() => {
                self.waiting = self.waiting.saturating_add(1);
            }
            _ => self
                .unsent
                .push("the session store's writer has stopped".into()),
        }
    }

    fn settled(&mut self) -> Vec<Result<(), JournalError>> {
        let mut settled = Vec::new();
        while let Ok(written) = self.answers.try_recv() {
            settled.push(self.answered(written));
        }
        settled.extend(self.unsent.drain(..).map(Err));
        settled
    }
}

impl Drop for StoreJournal {
    /// Waits for the writer, so that a session the binary opens next — or a
    /// test reading the store — finds everything this one was handed.
    fn drop(&mut self) {
        self.close();
    }
}

/// The writer: records every event handed over, in order, and answers for
/// each.
///
/// Once the journal is closing, the first refusal ends the writing: an event
/// that waited out the busy timeout says the store is held, and each of the
/// rest would wait it out again, holding up the quit by that much for every
/// event still queued. They are refused instead, with that refusal's reason.
fn write(
    mut target: Target,
    handed: &mpsc::Receiver<Handed>,
    written: &mpsc::Sender<Written>,
    closing: &AtomicBool,
) {
    let mut given_up: Option<String> = None;
    for Handed { event, at } in handed {
        let outcome = match &given_up {
            Some(reason) => Err(reason.clone().into()),
            None => target.record(&event, at),
        };
        if let Err(error) = &outcome
            && closing.load(Ordering::Acquire)
        {
            given_up.get_or_insert_with(|| error.to_string());
        }
        // A journal dropped without being finished has stopped listening, and
        // the events are written all the same.
        drop(written.send(outcome));
    }
}

impl Target {
    fn record(&mut self, event: &Event, at: SystemTime) -> Written {
        // A store that failed to open stays pending, so the next event tries
        // again rather than the whole session going unrecorded.
        if let Self::Pending(root) = self {
            *self = Self::Open(Recorder::new(repo::open_or_create_store(root)?));
        }
        let Self::Open(recorder) = self else {
            return Err("the session store was not opened".into());
        };
        recorder.record_at(event, at)?;
        Ok(recorder.session())
    }
}

/// The variable that asks for the panic below.
#[cfg(debug_assertions)]
const PANIC_ON_RECORD: &str = "NIOBE_TEST_PANIC_ON_RECORD";

/// Panics while the shell holds the terminal, when the environment asks for it.
///
/// A panic is the one way out of the shell that no test reaches on its own: the
/// terminal is put back by the panic hook rather than by the guard, the hook
/// writes to the process's own standard output, and nothing in the shell
/// panics on purpose. Proving that path needs the binary running on a real
/// terminal and something inside the event loop that panics, and the journal is
/// the only code of this crate the loop calls. `tests/pty.rs` sets the variable
/// and reads the sequences that came back.
///
/// Compiled out without debug assertions, so the released binary carries no
/// such switch.
#[cfg(debug_assertions)]
fn panic_if_the_environment_asks() {
    if std::env::var_os(PANIC_ON_RECORD).is_some() {
        panic!("{PANIC_ON_RECORD} asked for a panic while the shell held the terminal");
    }
}

#[cfg(not(debug_assertions))]
fn panic_if_the_environment_asks() {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    use niobe_core::session::SessionState;
    use niobe_tui::app::{App, EntryKind, Repo};
    use niobe_tui::clock::{Clock, LocalMoment, Stamp};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn send(app: &mut App, journal: &mut StoreJournal, prompt: &str) {
        for c in prompt.chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for event in app.take_produced() {
            journal.append(&event);
        }
    }

    /// The next `count` answers of the writer, waited for.
    fn answers(journal: &mut StoreJournal, count: usize) -> Vec<Result<(), JournalError>> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut answers = Vec::new();
        while answers.len() < count {
            assert!(Instant::now() < deadline, "the writer answered {answers:?}");
            answers.extend(journal.settled());
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            answers.len(),
            count,
            "the writer answered for more than was asked"
        );
        answers
    }

    /// The transcript as the session left it: the shell's own notices are not
    /// session content and are not recorded.
    fn timeline(app: &App) -> Vec<niobe_tui::app::Entry> {
        app.entries()
            .iter()
            .filter(|entry| entry.kind != EntryKind::Notice)
            .cloned()
            .collect()
    }

    #[test]
    fn nothing_is_created_until_the_first_event() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let journal = StoreJournal::pending(dir.path().to_path_buf());

        assert_eq!(journal.session(), None);
        assert!(!dir.path().join(".niobe").exists());
    }

    #[test]
    fn prompts_sent_in_the_shell_come_back_after_a_restart() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let root = dir.path().to_path_buf();

        let mut before = App::new(Repo::default());
        let mut journal = StoreJournal::pending(root.clone());
        send(&mut before, &mut journal, "add etag support");
        send(&mut before, &mut journal, "and a test for the 304 path");
        let session = journal
            .finish()
            .session
            .expect("the first prompt opened a session");

        let store = repo::open_existing_store(&root)
            .expect("the store opens")
            .expect("the store exists");
        let stored = store.events(session).expect("the session loads");
        let mut after = App::new(Repo::default());
        after.extend(stored.iter().map(|s| &s.event));

        assert_eq!(timeline(&after), timeline(&before));
        assert_eq!(timeline(&after).len(), 2);
        assert_eq!(after.session(), before.session());
        assert_eq!(
            *after.session(),
            SessionState::replay(stored.iter().map(|s| &s.event))
        );
    }

    #[test]
    fn a_session_read_back_shows_the_times_the_store_recorded_it_at() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let root = dir.path().to_path_buf();

        let mut before = App::new(Repo::default());
        let mut journal = StoreJournal::pending(root.clone());
        send(&mut before, &mut journal, "add etag support");
        send(&mut before, &mut journal, "and a test for the 304 path");
        let session = journal
            .finish()
            .session
            .expect("the first prompt opened a session");

        let store = repo::open_existing_store(&root)
            .expect("the store opens")
            .expect("the store exists");
        let stored = store.events(session).expect("the session loads");

        // The shell reading it back is running on a clock of its own, and a
        // resumed turn must not be dated by it.
        let read_at = Stamp::new(SystemTime::UNIX_EPOCH, LocalMoment::at(0, 3, 0));
        let clock = Clock::system();
        let mut after = App::new(Repo::default());
        after.tick(Instant::now(), Some(read_at));
        after.extend_at(stored.iter().map(|s| (&s.event, clock.at(s.at))));

        let recorded: Vec<SystemTime> = stored.iter().map(|s| s.at).collect();
        let shown: Vec<SystemTime> = timeline(&after)
            .iter()
            .map(|entry| {
                entry
                    .at
                    .expect("a recorded entry carries the time it was recorded at")
                    .at()
            })
            .collect();

        assert_eq!(shown.len(), 2);
        assert_eq!(
            shown, recorded,
            "the shell dated a resumed session by its own clock"
        );
        assert!(
            shown.iter().all(|at| *at != SystemTime::UNIX_EPOCH),
            "the entries were stamped with the moment they were read"
        );
        assert_eq!(
            after.stamp(),
            Some(read_at),
            "the live clock did not come back after the recorded fold"
        );
    }

    /// A store that cannot be written is reported on every event, so the shell
    /// says so each time rather than going quiet after the first, and the
    /// journal opens it once it can be written again.
    #[test]
    fn a_read_only_store_is_reported_and_recording_starts_once_it_can_be_written() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let root = dir.path().to_path_buf();
        drop(repo::open_or_create_store(&root).expect("the store is created"));
        let path = repo::store_path(&root);
        let mode = |p: &std::path::Path, m| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(m))
                .expect("the mode can be set");
        };
        mode(&path, 0o444);
        mode(path.parent().expect("the store has a directory"), 0o555);
        if std::fs::OpenOptions::new().write(true).open(&path).is_ok() {
            // Permissions do not bind this user, as for root. A check this
            // machine cannot make, which a CI runner must be able to.
            mode(path.parent().expect("the store has a directory"), 0o755);
            assert!(
                std::env::var_os("CI").is_none(),
                "file permissions do not bind this user, so a read-only store cannot be tested"
            );
            return;
        }

        let mut journal = StoreJournal::pending(root.clone());
        let event = Event::UserMessage {
            text: "hello".to_owned(),
        };
        for _ in 0..2 {
            journal.append(&event);
            let answered = answers(&mut journal, 1);
            let error = answered[0]
                .as_ref()
                .expect_err("a read-only store keeps nothing");
            assert!(error.to_string().contains("readonly"), "{error}");
        }
        assert_eq!(journal.session(), None);

        mode(path.parent().expect("the store has a directory"), 0o755);
        mode(&path, 0o644);
        journal.append(&event);
        let answered = answers(&mut journal, 1);
        assert!(answered[0].is_ok(), "{answered:?}");
        let session = journal.session().expect("a session was opened");
        let store = repo::open_existing_store(&root)
            .expect("the store opens")
            .expect("the store exists");
        assert_eq!(store.events(session).expect("load").len(), 1);
    }

    /// Another process holding the store's write lock, as a `sqlite3` shell
    /// inside `BEGIN EXCLUSIVE` or a backup does, until it is dropped.
    fn lock(root: &std::path::Path) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(repo::store_path(root))
            .expect("a second connection to the store opens");
        conn.execute_batch("BEGIN EXCLUSIVE")
            .expect("the lock is free to take");
        conn
    }

    fn said(text: &str) -> Event {
        Event::UserMessage {
            text: text.to_owned(),
        }
    }

    /// A journal that has written its first event, so the store and the
    /// session exist for another process to lock.
    fn recording(root: &std::path::Path) -> StoreJournal {
        let mut journal = StoreJournal::pending(root.to_path_buf());
        journal.append(&said("first"));
        let answered = answers(&mut journal, 1);
        assert!(answered[0].is_ok(), "{answered:?}");
        journal
    }

    fn texts(root: &std::path::Path, session: SessionId) -> Vec<Event> {
        repo::open_existing_store(root)
            .expect("the store opens")
            .expect("the store exists")
            .events(session)
            .expect("the session loads")
            .into_iter()
            .map(|stored| stored.event)
            .collect()
    }

    #[test]
    fn an_event_is_taken_at_once_while_another_process_holds_the_store() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let mut journal = recording(dir.path());
        let held = lock(dir.path());

        let started = Instant::now();
        journal.append(&said("second"));
        let taken = started.elapsed();
        drop(held);

        assert!(
            taken < Duration::from_millis(100),
            "the shell waited {taken:?} on a store another process held"
        );
    }

    #[test]
    fn events_handed_over_while_the_store_is_held_are_written_in_order_once_it_is_let_go() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let mut journal = recording(dir.path());
        let held = lock(dir.path());

        journal.append(&said("second"));
        journal.append(&said("third"));
        let handed_by = SystemTime::now();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(journal.settled().len(), 0, "the writer got past the lock");
        drop(held);

        assert!(answers(&mut journal, 2).iter().all(Result::is_ok));
        let finished = journal.finish();
        assert_eq!(finished.unsaved, 0);
        let session = finished.session.expect("the first event opened a session");
        assert_eq!(
            texts(dir.path(), session),
            [said("first"), said("second"), said("third")]
        );
        let store = repo::open_existing_store(dir.path())
            .expect("the store opens")
            .expect("the store exists");
        let stored = store.events(session).expect("the session loads");
        assert!(
            stored[1].at <= handed_by && stored[2].at <= handed_by,
            "the events were dated by the write that waited out the lock"
        );
    }

    #[test]
    fn events_the_store_still_refuses_when_the_shell_closes_are_counted_as_unsaved() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let mut journal = recording(dir.path());
        let held = lock(dir.path());

        for text in ["second", "third", "fourth"] {
            journal.append(&said(text));
        }
        let started = Instant::now();
        let finished = journal.finish();
        let took = started.elapsed();
        drop(held);

        assert_eq!(finished.unsaved, 3);
        let error = finished.error.expect("the refusal says why");
        assert!(error.contains("locked"), "{error}");
        assert!(
            took < Duration::from_secs(10),
            "the quit waited {took:?}, out the lock once per event"
        );
        let session = finished.session.expect("the first event opened a session");
        assert_eq!(texts(dir.path(), session), [said("first")]);
    }

    #[test]
    fn a_resumed_journal_appends_after_what_was_there() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let root = dir.path().to_path_buf();

        let mut app = App::new(Repo::default());
        let mut journal = StoreJournal::pending(root.clone());
        send(&mut app, &mut journal, "one");
        let session = journal.finish().session.expect("a session was opened");

        let store = repo::open_existing_store(&root)
            .expect("the store opens")
            .expect("the store exists");
        let recorder = Recorder::resume(store, session).expect("the session exists");
        let mut journal = StoreJournal::open(recorder);
        send(&mut app, &mut journal, "two");

        let finished = journal.finish();
        assert_eq!(finished.session, Some(session));
        assert_eq!(finished.unsaved, 0);
        let store = repo::open_existing_store(&root)
            .expect("the store opens")
            .expect("the store exists");
        assert_eq!(store.events(session).expect("load").len(), 2);
        assert_eq!(store.sessions().expect("listing").len(), 1);
    }
}
