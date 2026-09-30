// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The session store when something other than this session is in the way: a
//! second `niobe` writing the same file, a file that cannot be written, a row
//! that no longer decodes, and a log whose last line was cut short.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;

use niobe_core::event::Event;
use niobe_store::{Recorder, SessionId, Store, StoreError, read_log};

fn scratch() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    let path = dir.path().join("sessions.db");
    (dir, path)
}

fn user(text: &str) -> Event {
    Event::UserMessage {
        text: text.to_owned(),
    }
}

fn raw(path: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(path).expect("the store file opens directly")
}

fn raw_id(session: SessionId) -> i64 {
    session.to_string().parse().expect("ids are numbers")
}

/// Two shells in one repository, each recording its own session into the same
/// file at the same moment: every event of both lands, in order, because a
/// writer waits out the other's commit rather than failing on it.
#[test]
fn two_sessions_recording_into_one_file_at_once_both_keep_every_event() {
    const EVENTS: usize = 300;
    let (_dir, path) = scratch();
    drop(Store::open(&path).expect("the store is created"));
    let start = Arc::new(Barrier::new(2));

    let writers: Vec<_> = ["left", "right"]
        .into_iter()
        .map(|name| {
            let path = path.clone();
            let start = Arc::clone(&start);
            thread::spawn(move || {
                let mut recorder = Recorder::new(Store::open(&path).expect("the store opens"));
                start.wait();
                for i in 0..EVENTS {
                    recorder
                        .record(&user(&format!("{name} {i}")))
                        .expect("a busy file is waited on, not refused");
                }
                recorder.session().expect("a session was opened")
            })
        })
        .collect();
    let sessions: Vec<SessionId> = writers
        .into_iter()
        .map(|writer| writer.join().expect("the writer did not panic"))
        .collect();

    let store = Store::open(&path).expect("the store opens");
    assert_ne!(sessions[0], sessions[1]);
    for (session, name) in sessions.iter().zip(["left", "right"]) {
        let stored = store.events(*session).expect("the session loads");
        let texts: Vec<Event> = stored.into_iter().map(|s| s.event).collect();
        let expected: Vec<Event> = (0..EVENTS).map(|i| user(&format!("{name} {i}"))).collect();
        assert_eq!(
            texts, expected,
            "session {session} lost or reordered events"
        );
    }
}

/// Two shells started at once in a repository that has no store yet: both
/// open it, and it is created once.
#[test]
fn two_shells_creating_the_store_at_once_both_open_it() {
    for _ in 0..20 {
        let (_dir, path) = scratch();
        let start = Arc::new(Barrier::new(2));
        let openers: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    Store::open(&path).map(|store| {
                        store
                            .append(store.create_session()?, &user("hello"))
                            .map(|_| ())
                    })
                })
            })
            .collect();
        for opener in openers {
            opener
                .join()
                .expect("the opener did not panic")
                .expect("the store opens")
                .expect("and takes an event");
        }
        assert_eq!(
            Store::open(&path)
                .expect("the store opens")
                .sessions()
                .expect("listing")
                .len(),
            2
        );
    }
}

/// A second writer holding the file past the busy timeout makes this one's
/// append fail with an error the shell puts on screen, and the recorder goes on
/// recording once the file is free: nothing stops recording without saying so,
/// and the next event is numbered after the last one kept.
#[test]
fn a_file_locked_past_the_timeout_fails_the_append_and_recording_resumes_after() {
    let (_dir, path) = scratch();
    let mut recorder = Recorder::new(Store::open(&path).expect("the store opens"));
    recorder.record(&user("before")).expect("record");
    let session = recorder.session().expect("a session was opened");

    let mut other = raw(&path);
    let lock = other
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .expect("the other writer takes the lock");
    let error = recorder
        .record(&user("while locked"))
        .expect_err("a locked file is not written");
    assert!(
        matches!(&error, StoreError::Sqlite(e) if e.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)),
        "{error}"
    );
    assert!(error.to_string().contains("locked"), "{error}");
    drop(lock);

    assert_eq!(recorder.record(&user("after")).expect("record"), 2);
    let stored = recorder.store().events(session).expect("load");
    assert_eq!(
        stored.into_iter().map(|s| s.event).collect::<Vec<_>>(),
        [user("before"), user("after")]
    );
}

/// Makes `path` and the directory it is in read-only until the value is
/// dropped. `None` where the permissions do not bind, as for root.
struct ReadOnly {
    dir: PathBuf,
    file: PathBuf,
}

impl ReadOnly {
    fn new(file: &Path) -> Option<Self> {
        use std::os::unix::fs::PermissionsExt;
        let dir = file
            .parent()
            .expect("the store is in a directory")
            .to_path_buf();
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o444))
            .expect("the file's mode can be set");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555))
            .expect("the directory's mode can be set");
        let guard = Self {
            dir,
            file: file.to_path_buf(),
        };
        let binds = std::fs::OpenOptions::new().write(true).open(file).is_err();
        // A check this machine cannot make, which a CI runner must be able
        // to: skipped quietly there, it would be a pass that proved nothing.
        assert!(
            binds || std::env::var_os("CI").is_none(),
            "file permissions do not bind this user, so a read-only store cannot be tested"
        );
        binds.then_some(guard)
    }
}

impl Drop for ReadOnly {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::set_permissions(&self.file, std::fs::Permissions::from_mode(0o644));
    }
}

/// A store on a disk that cannot be written is refused when it is opened, with
/// an error that says so, rather than opened and quietly keeping nothing.
/// SQLite cannot read a write-ahead-logged file from a directory it cannot
/// create the file's shared-memory index in, so the refusal covers reading too.
#[cfg(unix)]
#[test]
fn a_read_only_store_is_refused_with_an_error_that_says_so() {
    let (_dir, path) = scratch();
    {
        let mut recorder = Recorder::new(Store::open(&path).expect("the store opens"));
        recorder.record(&user("kept")).expect("record");
    }
    let Some(_read_only) = ReadOnly::new(&path) else {
        return;
    };

    let error = Store::open(&path).expect_err("a read-only store is not opened");
    assert!(error.to_string().contains("readonly"), "{error}");
}

/// Reading needs no write: a store whose file and directory cannot be
/// written is read as it is, every session and every event of it, though a
/// store opened to record into refuses it.
#[cfg(unix)]
#[test]
fn a_read_only_store_is_read_as_it_is() {
    let (_dir, path) = scratch();
    let session = {
        let mut recorder = Recorder::new(Store::open(&path).expect("the store opens"));
        recorder.record(&user("kept")).expect("record");
        recorder.session().expect("the record opened a session")
    };
    let Some(_read_only) = ReadOnly::new(&path) else {
        return;
    };

    let store = Store::open_to_read(&path).expect("a read-only store is read");
    let sessions = store.sessions().expect("the list reads");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].first_prompt.as_deref(), Some("kept"));
    assert_eq!(store.events(session).expect("the session reads").len(), 1);
}

#[test]
fn a_store_opened_to_read_that_can_be_written_is_read_too() {
    let (_dir, path) = scratch();
    Recorder::new(Store::open(&path).expect("the store opens"))
        .record(&user("kept"))
        .expect("record");
    let store = Store::open_to_read(&path).expect("the store is read");
    assert_eq!(store.sessions().expect("the list reads").len(), 1);
}

/// A row that is not even JSON — a torn write, or a file edited by hand —
/// fails the load of its own session, and only that: the list still shows
/// every session, and every other session still loads.
#[test]
fn a_row_that_is_not_json_fails_only_its_own_session() {
    let (_dir, path) = scratch();
    let (broken, fine) = {
        let store = Store::open(&path).expect("a store opens");
        let broken = store.create_session().expect("a session is created");
        let fine = store.create_session().expect("a session is created");
        store.append(fine, &user("fine")).expect("append");
        (broken, fine)
    };
    raw(&path)
        .execute(
            "INSERT INTO events (session_id, seq, at, event) VALUES (?1, 1, 0, ?2)",
            rusqlite::params![raw_id(broken), r#"{"type":"user_mess"#],
        )
        .expect("a row can be appended directly");

    let store = Store::open(&path).expect("the store opens");
    let sessions = store
        .sessions()
        .expect("one bad row does not fail the list");
    assert_eq!(sessions.len(), 2);
    let listed = sessions
        .iter()
        .find(|s| s.id == broken)
        .expect("the broken session is listed");
    assert_eq!(listed.events, 1);
    assert_eq!(listed.first_prompt, None);

    assert!(matches!(
        store.events(broken).expect("the session opens").as_slice(),
        [niobe_store::StoredEvent {
            event: niobe_core::event::Event::Error { fatal: false, .. },
            ..
        }]
    ));
    assert_eq!(
        store.events(fine).expect("the other session loads").len(),
        1
    );
}

/// A log whose writer died mid-line ends in half a record. The read fails and
/// names that last line, as for a bad line anywhere else: a log missing its
/// tail folds into totals that look complete and are not.
#[test]
fn a_log_cut_off_mid_line_is_refused_at_its_last_line() {
    let log = "{\"type\":\"user_message\",\"text\":\"one\"}\n\
               {\"type\":\"user_message\",\"text\":\"two\"}\n\
               {\"type\":\"user_mess";
    let error = read_log(log).expect_err("half a record is not an event");
    assert_eq!(error.line(), 3);
    assert!(error.to_string().contains("EOF"), "{error}");
}
