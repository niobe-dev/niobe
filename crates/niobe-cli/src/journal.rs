// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The session store, as the journal the shell writes through.

use std::path::PathBuf;

use niobe_core::event::Event;
use niobe_store::{Recorder, SessionId};
use niobe_tui::journal::{Journal, JournalError};

use crate::repo;

/// Records the shell's events into the session store of a repository.
///
/// For a new session the store is opened on the first event, not when the
/// shell opens: looking at the shell and quitting leaves no `.niobe` directory
/// behind and no empty session in the list.
#[derive(Debug)]
pub enum StoreJournal {
    /// Nothing recorded yet; the store of this repository root is opened on the
    /// first event.
    Pending(PathBuf),
    /// Recording.
    Open(Recorder),
}

impl StoreJournal {
    /// The session being recorded, once anything has been.
    pub fn session(&self) -> Option<SessionId> {
        match self {
            Self::Pending(_) => None,
            Self::Open(recorder) => recorder.session(),
        }
    }
}

impl Journal for StoreJournal {
    fn append(&mut self, event: &Event) -> Result<(), JournalError> {
        panic_if_the_environment_asks();
        // A store that failed to open stays pending, so the next event tries
        // again rather than the whole session going unrecorded.
        if let Self::Pending(root) = self {
            *self = Self::Open(Recorder::new(repo::open_or_create_store(root)?));
        }
        let Self::Open(recorder) = self else {
            return Err("the session store was not opened".into());
        };
        recorder.record(event)?;
        Ok(())
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
    use std::time::{Instant, SystemTime};

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
            journal.append(&event).expect("the store accepts the event");
        }
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
        let journal = StoreJournal::Pending(dir.path().to_path_buf());

        assert_eq!(journal.session(), None);
        assert!(!dir.path().join(".niobe").exists());
    }

    #[test]
    fn prompts_sent_in_the_shell_come_back_after_a_restart() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let root = dir.path().to_path_buf();

        let mut before = App::new(Repo::default());
        let mut journal = StoreJournal::Pending(root.clone());
        send(&mut before, &mut journal, "add etag support");
        send(&mut before, &mut journal, "and a test for the 304 path");
        let session = journal
            .session()
            .expect("the first prompt opened a session");
        drop(journal);

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
        let mut journal = StoreJournal::Pending(root.clone());
        send(&mut before, &mut journal, "add etag support");
        send(&mut before, &mut journal, "and a test for the 304 path");
        let session = journal
            .session()
            .expect("the first prompt opened a session");
        drop(journal);

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

    #[test]
    fn a_resumed_journal_appends_after_what_was_there() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let root = dir.path().to_path_buf();

        let mut app = App::new(Repo::default());
        let mut journal = StoreJournal::Pending(root.clone());
        send(&mut app, &mut journal, "one");
        let session = journal.session().expect("a session was opened");
        drop(journal);

        let store = repo::open_existing_store(&root)
            .expect("the store opens")
            .expect("the store exists");
        let recorder = Recorder::resume(store, session).expect("the session exists");
        let mut journal = StoreJournal::Open(recorder);
        send(&mut app, &mut journal, "two");

        assert_eq!(journal.session(), Some(session));
        let store = repo::open_existing_store(&root)
            .expect("the store opens")
            .expect("the store exists");
        assert_eq!(store.events(session).expect("load").len(), 2);
        assert_eq!(store.sessions().expect("listing").len(), 1);
    }
}
