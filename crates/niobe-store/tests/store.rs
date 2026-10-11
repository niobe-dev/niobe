// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The session store, against a real SQLite file.
//!
//! "Restart" here means what it means for the binary: the [`Store`] is dropped,
//! the file is opened again by a fresh connection, and whatever comes back is
//! folded from scratch.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use niobe_core::event::{AgentId, Event, TokenCounts, ToolCallId, Usage};
use niobe_core::session::SessionState;
use niobe_store::{Recorder, SessionId, Store, StoreError, read_log};

/// The recorded `claude` bridge session `niobe-core` asserts its fold against.
const FIXTURE: &str = include_str!("../../niobe-core/tests/fixtures/claude-session.jsonl");

fn fixture() -> Vec<Event> {
    read_log(FIXTURE).expect("the committed fixture parses")
}

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

#[test]
fn a_new_store_lists_no_sessions() {
    let (_dir, path) = scratch();
    let store = Store::open(&path).expect("a store opens on a new file");
    assert!(store.sessions().expect("listing works").is_empty());
}

#[test]
fn events_come_back_in_the_order_they_were_appended_with_sequence_numbers() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let session = store.create_session().expect("a session is created");

    for text in ["one", "two", "three"] {
        store
            .append(session, &user(text))
            .expect("an append succeeds");
    }

    let stored = store.events(session).expect("the session loads");
    let seqs: Vec<u64> = stored.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, [1, 2, 3]);
    assert_eq!(
        stored.iter().map(|e| e.event.clone()).collect::<Vec<_>>(),
        [user("one"), user("two"), user("three")]
    );
    assert!(stored.windows(2).all(|w| w[0].at <= w[1].at));
}

#[test]
fn sequence_numbers_count_per_session() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let first = store.create_session().expect("a session is created");
    let second = store.create_session().expect("a session is created");

    store
        .append(first, &user("a"))
        .expect("a session in a temporary store takes an event");
    store
        .append(second, &user("b"))
        .expect("a session in a temporary store takes an event");
    let seq = store
        .append(first, &user("c"))
        .expect("a session in a temporary store takes an event");

    assert_eq!(seq, 2);
    assert_eq!(
        store
            .events(second)
            .expect("the session was just written to the store")[0]
            .seq,
        1
    );
}

#[test]
fn a_restart_shows_the_same_timeline_and_the_same_totals() {
    let (_dir, path) = scratch();
    let events = fixture();

    let session = {
        let store = Store::open(&path).expect("a store opens");
        let session = store.create_session().expect("a session is created");
        for event in &events {
            store.append(session, event).expect("an append succeeds");
        }
        session
    };

    let reopened = Store::open(&path).expect("the store opens again");
    let loaded: Vec<Event> = reopened
        .events(session)
        .expect("the session loads")
        .into_iter()
        .map(|stored| stored.event)
        .collect();

    assert_eq!(loaded, events);
    assert_eq!(SessionState::replay(&loaded), SessionState::replay(&events));
}

#[test]
fn the_database_itself_refuses_to_edit_or_delete_an_event() {
    let (_dir, path) = scratch();
    {
        let store = Store::open(&path).expect("a store opens");
        let session = store.create_session().expect("a session is created");
        store
            .append(session, &user("keep me"))
            .expect("a session in a temporary store takes an event");
    }

    let conn = raw(&path);
    let forged = r#"{"type":"user_message","text":"forged"}"#;
    let update = conn.execute("UPDATE events SET event = '{}'", []);
    let delete = conn.execute("DELETE FROM events", []);
    let drop_session = conn.execute("DELETE FROM sessions", []);
    let update_session = conn.execute("UPDATE sessions SET started_at = 42", []);
    // A replace deletes the row in the way and fires no delete trigger, so it
    // is refused on the way in instead.
    let replace = conn.execute(
        "REPLACE INTO events (session_id, seq, at, event) VALUES (1, 1, 0, ?1)",
        [forged],
    );
    let insert_or_replace = conn.execute(
        "INSERT OR REPLACE INTO events (session_id, seq, at, event) VALUES (1, 1, 0, ?1)",
        [forged],
    );
    let replace_session = conn.execute("REPLACE INTO sessions (id, started_at) VALUES (1, 42)", []);

    for (what, result) in [
        ("update", update),
        ("delete", delete),
        ("session delete", drop_session),
        ("session update", update_session),
        ("replace", replace),
        ("insert or replace", insert_or_replace),
        ("session replace", replace_session),
    ] {
        let error = result.expect_err(what).to_string();
        assert!(error.contains("append-only"), "{what}: {error}");
    }

    let store = Store::open(&path).expect("the store opens again");
    let listed = store.sessions().expect("listing works");
    assert_ne!(
        listed[0].started_at,
        std::time::UNIX_EPOCH + std::time::Duration::from_millis(42)
    );
    assert_eq!(
        store
            .events(listed[0].id)
            .expect("the session was just written to the store")[0]
            .event,
        user("keep me")
    );
}

#[test]
fn sessions_are_listed_newest_first_with_their_first_prompt_and_size() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let older = store.create_session().expect("a session is created");
    store
        .append(older, &user("add etag support"))
        .expect("a session in a temporary store takes an event");
    store
        .append(older, &user("now the tests"))
        .expect("a session in a temporary store takes an event");
    let newer = store.create_session().expect("a session is created");
    store
        .append(
            newer,
            &Event::AssistantMessage {
                text: "hello".to_owned(),
                agent: None,
            },
        )
        .expect("a session in a temporary store takes an event");

    let sessions = store.sessions().expect("listing works");
    assert_eq!(
        sessions.iter().map(|s| s.id).collect::<Vec<_>>(),
        [newer, older]
    );
    assert_eq!(sessions[1].events, 2);
    assert_eq!(
        sessions[1].first_prompt.as_deref(),
        Some("add etag support")
    );
    assert_eq!(sessions[0].first_prompt, None);
    assert!(sessions[1].last_at.is_some());
}

#[test]
fn a_session_is_listed_with_the_last_title_its_backend_gave_it() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let titled = store.create_session().expect("a session is created");
    let titled_event = |title: &str| Event::Titled {
        title: title.to_owned(),
    };
    for event in [
        user("add etag support"),
        titled_event("Etag support"),
        titled_event("Static file handler etag support"),
    ] {
        store
            .append(titled, &event)
            .expect("a session in a temporary store takes an event");
    }
    let untitled = store.create_session().expect("a session is created");
    store
        .append(untitled, &user("fix it"))
        .expect("a session in a temporary store takes an event");

    let sessions = store.sessions().expect("listing works");
    assert_eq!(sessions[0].title, None);
    assert_eq!(
        sessions[1].title.as_deref(),
        Some("Static file handler etag support")
    );
}

#[test]
fn an_unknown_session_is_an_error_not_an_empty_timeline() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let missing: SessionId = "42".parse().expect("a number is a session id");

    assert!(matches!(
        store.events(missing),
        Err(StoreError::NoSuchSession(id)) if id == missing
    ));
    assert!(matches!(
        store.append(missing, &user("lost")),
        Err(StoreError::NoSuchSession(_))
    ));
}

#[test]
fn a_store_from_a_newer_niobe_is_refused_rather_than_misread() {
    let (_dir, path) = scratch();
    drop(Store::open(&path).expect("a store opens"));
    raw(&path)
        .pragma_update(None, "user_version", 99)
        .expect("the schema version can be set");

    assert!(matches!(
        Store::open(&path),
        Err(StoreError::UnsupportedSchema { found: 99, .. })
    ));
}

#[test]
fn an_unreadable_row_names_the_session_and_the_sequence_number() {
    let (_dir, path) = scratch();
    let session = {
        let store = Store::open(&path).expect("a store opens");
        let session = store.create_session().expect("a session is created");
        store
            .append(session, &user("fine"))
            .expect("a session in a temporary store takes an event");
        session
    };
    raw(&path)
        .execute(
            "INSERT INTO events (session_id, seq, at, event) VALUES (?1, 2, 0, ?2)",
            rusqlite::params![
                session.to_string().parse::<i64>().expect("ids are numbers"),
                r#"{"type":"from_the_future"}"#
            ],
        )
        .expect("a row can be appended directly");

    let events = Store::open(&path)
        .expect("the store opens")
        .events(session)
        .expect("one row this build cannot read does not keep the session shut");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event, user("fine"));
    match &events[1].event {
        Event::Error { message, fatal } => {
            assert!(!fatal);
            assert!(message.contains("event 2"), "{message}");
            assert!(message.contains(&format!("session {session}")), "{message}");
        }
        other => panic!("the unreadable row is not a warning: {other:?}"),
    }
}

/// A call, a message and a permission prompt written before any of them said
/// which agent made it load as the session's own, and one that does say comes
/// back naming it.
#[test]
fn calls_messages_and_prompts_stored_before_they_named_an_agent_load_as_the_sessions() {
    let (_dir, path) = scratch();
    let session = {
        let store = Store::open(&path).expect("a store opens");
        let session = store.create_session().expect("a session is created");
        store
            .append(session, &user("review it"))
            .expect("a session in a temporary store takes an event");
        session
    };
    let id = session.to_string().parse::<i64>().expect("ids are numbers");
    let conn = raw(&path);
    for (seq, row) in [
        (
            2,
            r#"{"type":"tool_call_start","id":"t","name":"Read","input":"{}"}"#,
        ),
        (3, r#"{"type":"assistant_message","text":"Done."}"#),
        (
            4,
            r#"{"type":"permission_request","id":"p","tool":"Bash","input":"{}","target":"ls"}"#,
        ),
    ] {
        conn.execute(
            "INSERT INTO events (session_id, seq, at, event) VALUES (?1, ?2, 0, ?3)",
            rusqlite::params![id, seq, row],
        )
        .expect("a row in the older shape can be appended directly");
    }
    drop(conn);

    let store = Store::open(&path).expect("the store opens");
    let owned = Event::ToolCallStart {
        id: ToolCallId::new("a1"),
        name: "Grep".to_owned(),
        input: "{}".to_owned(),
        summary: None,
        agent: Some(AgentId::new("toolu_a")),
    };
    store
        .append(session, &owned)
        .expect("a session in a temporary store takes an event");

    let events: Vec<Event> = store
        .events(session)
        .expect("the older rows load")
        .into_iter()
        .map(|stored| stored.event)
        .collect();
    let owners: Vec<Option<&str>> = events
        .iter()
        .filter_map(|event| match event {
            Event::ToolCallStart { agent, .. }
            | Event::AssistantMessage { agent, .. }
            | Event::PermissionRequest { agent, .. } => Some(agent.as_ref().map(AgentId::as_str)),
            _ => None,
        })
        .collect();
    assert_eq!(owners, [None, None, None, Some("toolu_a")]);
}

#[test]
fn a_turn_total_that_differed_comes_back_with_what_a_report_of_it_needs() {
    let (_dir, path) = scratch();
    let counts = |input, cache_read| TokenCounts {
        input,
        output: 8,
        cache_read,
        cache_write: 0,
    };
    let differs = Event::TurnTotalDiffers {
        per_message: counts(6, 234_375),
        reported: counts(8, 352_445),
        backend_session: Some("506fa4fd-8719-4564-8c62-5c6ae4b1a471".to_owned()),
        unfinished: vec!["msg_cut (request req_cut)".to_owned()],
    };
    let events = [user("write the six pages"), differs, Event::TurnEnded];

    let session = {
        let store = Store::open(&path).expect("a store opens");
        let session = store.create_session().expect("a session is created");
        for event in &events {
            store.append(session, event).expect("an append succeeds");
        }
        session
    };

    let loaded: Vec<Event> = Store::open(&path)
        .expect("the store opens again")
        .events(session)
        .expect("the session loads")
        .into_iter()
        .map(|stored| stored.event)
        .collect();
    assert_eq!(loaded, events);
    assert_eq!(
        SessionState::replay(&loaded).beyond_messages().cache_read,
        118_070
    );
}

#[test]
fn reported_costs_come_back_bit_for_bit() {
    // A spread of doubles with long decimal expansions, generated rather than
    // hand-picked so the check does not depend on knowing which ones a lossy
    // parser gets wrong.
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let costs: Vec<f64> = (0..2_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % 10_000_000) as f64 / 1_000_000.0 * 1.000_000_7
        })
        .collect();

    let store = Store::open_in_memory().expect("an in-memory store opens");
    let session = store.create_session().expect("a session is created");
    for cost in &costs {
        store
            .append(
                session,
                &Event::Usage(Usage {
                    input: 1,
                    output: 1,
                    cache_read: 0,
                    cache_write: 0,
                    cache_write_1h: 0,
                    reasoning: 0,
                    model: "opus-5".to_owned(),
                    cost_usd: Some(*cost),
                    settles_model: false,
                    fast: false,
                }),
            )
            .expect("a session in a temporary store takes an event");
    }

    let back: Vec<f64> = store
        .events(session)
        .expect("the session was just written to the store")
        .into_iter()
        .map(|stored| match stored.event {
            Event::Usage(usage) => usage.cost_usd.expect("every record was priced"),
            other => panic!("unexpected event {other:?}"),
        })
        .collect();

    let drifted = costs
        .iter()
        .zip(&back)
        .filter(|(a, b)| a.to_bits() != b.to_bits())
        .count();
    assert_eq!(drifted, 0, "{drifted} of {} costs changed", costs.len());
}

#[test]
fn a_recorder_creates_its_session_on_the_first_event_and_not_before() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let mut recorder = Recorder::new(store);
    assert_eq!(recorder.session(), None);
    assert!(
        recorder
            .store()
            .sessions()
            .expect("the store was just opened and can list its sessions")
            .is_empty()
    );

    recorder
        .record(&user("first"))
        .expect("a session in a temporary store takes an event");
    recorder
        .record(&user("second"))
        .expect("a session in a temporary store takes an event");

    let session = recorder
        .session()
        .expect("the first event opened a session");
    let sessions = recorder
        .store()
        .sessions()
        .expect("the store was just opened and can list its sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, session);
    assert_eq!(sessions[0].events, 2);
}

#[test]
fn an_event_recorded_late_is_dated_when_it_happened() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let mut recorder = Recorder::new(store);
    let happened = SystemTime::UNIX_EPOCH + Duration::from_millis(1_759_449_600_250);

    recorder
        .record_at(&user("typed while the store was locked"), happened)
        .expect("a session in a temporary store takes an event");

    let session = recorder.session().expect("the event opened a session");
    let stored = recorder
        .store()
        .events(session)
        .expect("the session was just written to the store");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].at, happened);
}

#[test]
fn a_resumed_recorder_appends_to_the_session_it_resumed() {
    let (_dir, path) = scratch();
    let session = {
        let mut recorder = Recorder::new(Store::open(&path).expect("a store opens"));
        recorder
            .record(&user("before the restart"))
            .expect("a session in a temporary store takes an event");
        recorder.session().expect("a session was opened")
    };

    let mut resumed = Recorder::resume(Store::open(&path).expect("the store opens again"), session)
        .expect("the session exists");
    resumed
        .record(&user("after the restart"))
        .expect("a session in a temporary store takes an event");

    let stored = resumed
        .store()
        .events(session)
        .expect("the session was just written to the store");
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[1].seq, 2);
    assert_eq!(
        resumed
            .store()
            .sessions()
            .expect("the store was just opened and can list its sessions")
            .len(),
        1
    );

    let missing: SessionId = "7".parse().expect("a number is a session id");
    assert!(matches!(
        Recorder::resume(
            Store::open(&path).expect("the store file is in a temporary directory"),
            missing
        ),
        Err(StoreError::NoSuchSession(_))
    ));
}

#[cfg(unix)]
#[test]
fn a_session_one_recorder_holds_cannot_be_taken_up_by_another_until_it_lets_go() {
    let (_dir, path) = scratch();
    let mut first = Recorder::new(Store::open(&path).expect("a store opens"));
    first
        .record(&user("from the first window"))
        .expect("a session in a temporary store takes an event");
    let session = first.session().expect("a session was opened");

    let second = Recorder::resume(Store::open(&path).expect("the store opens again"), session);
    let said = second
        .expect_err("the session is open in the first recorder")
        .to_string();
    assert!(said.contains(&format!("session {session}")), "{said}");
    assert!(said.contains("open"), "{said}");

    drop(first);
    Recorder::resume(
        Store::open(&path).expect("the store file is in a temporary directory"),
        session,
    )
    .expect("a session nobody holds is taken up");
}

#[cfg(unix)]
#[test]
fn a_session_whose_hold_cannot_be_taken_is_opened_once_and_taken_up_when_it_can_be() {
    let (dir, path) = scratch();
    let blocker = dir.path().join("sessions.db-open-1");
    std::fs::create_dir(&blocker).expect("a directory can be made at the hold's name");
    let mut recorder = Recorder::new(Store::open(&path).expect("a store opens"));

    for text in ["one", "two", "three"] {
        assert!(matches!(
            recorder.record(&user(text)),
            Err(StoreError::Hold(_))
        ));
    }
    std::fs::remove_dir(&blocker).expect("the directory is removed");
    recorder
        .record(&user("four"))
        .expect("a session in a temporary store takes an event");

    let listed = recorder.store().sessions().expect("listing works");
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].id,
        recorder.session().expect("a session was opened")
    );
    assert_eq!(listed[0].events, 1);
}

#[cfg(unix)]
#[test]
fn a_link_at_the_hold_name_is_refused_and_nothing_is_made_where_it_points() {
    let (dir, path) = scratch();
    let target = dir.path().join("planted");
    std::os::unix::fs::symlink(&target, dir.path().join("sessions.db-open-1"))
        .expect("a link can be made at the hold's name");
    let mut recorder = Recorder::new(Store::open(&path).expect("a store opens"));

    let said = recorder
        .record(&user("one"))
        .expect_err("the link is refused");
    assert!(matches!(said, StoreError::Hold(_)));
    let said = said.to_string();
    assert!(said.contains("sessions.db-open-1 is a link"), "{said}");
    assert!(!target.exists(), "the link was not followed");
}

#[cfg(unix)]
#[test]
fn a_recorder_removes_its_hold_file_when_it_lets_go() {
    let (dir, path) = scratch();
    let mut first = Recorder::new(Store::open(&path).expect("a store opens"));
    first
        .record(&user("held"))
        .expect("a session in a temporary store takes an event");
    let session = first.session().expect("a session was opened");
    let hold = dir.path().join(format!("sessions.db-open-{session}"));
    assert!(hold.exists());

    drop(first);
    assert!(!hold.exists());

    let resumed = Recorder::resume(
        Store::open(&path).expect("the store file is in a temporary directory"),
        session,
    )
    .expect("a session nobody holds is taken up");
    assert!(hold.exists());
    drop(resumed);
    assert!(!hold.exists());
}

#[cfg(unix)]
#[test]
fn a_session_can_still_be_read_while_a_recorder_holds_it() {
    let (_dir, path) = scratch();
    let mut first = Recorder::new(Store::open(&path).expect("a store opens"));
    first
        .record(&user("held"))
        .expect("a session in a temporary store takes an event");
    let session = first.session().expect("a session was opened");

    let reader = Store::open_to_read(&path).expect("the store opens to read");
    assert_eq!(
        reader
            .events(session)
            .expect("the session was just written to the store")
            .len(),
        1
    );
}

#[cfg(unix)]
#[test]
fn a_session_reads_as_held_while_a_recorder_holds_it_and_the_look_takes_nothing() {
    let (dir, path) = scratch();
    let mut first = Recorder::new(Store::open(&path).expect("a store opens"));
    first
        .record(&user("held"))
        .expect("a session in a temporary store takes an event");
    let session = first.session().expect("a session was opened");
    let reader = Store::open_to_read(&path).expect("the store opens to read");

    assert!(reader.is_held(session).expect("the hold is looked at"));

    drop(first);
    assert!(!reader.is_held(session).expect("the hold is looked at"));
    assert!(
        !dir.path()
            .join(format!("sessions.db-open-{session}"))
            .exists(),
        "looking made no hold file"
    );
    Recorder::resume(
        Store::open(&path).expect("the store file is in a temporary directory"),
        session,
    )
    .expect("a session that was looked at is still taken up");
}

#[cfg(unix)]
#[test]
fn a_hold_file_a_killed_niobe_left_behind_does_not_read_as_held() {
    let (dir, path) = scratch();
    let store = Store::open(&path).expect("a store opens");
    let session = store.create_session().expect("a session is created");
    std::fs::write(dir.path().join(format!("sessions.db-open-{session}")), "")
        .expect("the hold file is written");

    assert!(!store.is_held(session).expect("the hold is looked at"));
}

/// Nothing a recorder makes is a FIFO, and opening one to read waits for a
/// writer that may never come: the look at a session's hold is taken for
/// every session the history lists, so it must not wait on one.
#[cfg(unix)]
#[test]
fn a_fifo_at_the_hold_name_is_looked_at_without_waiting_and_is_not_a_hold() {
    let (dir, path) = scratch();
    let store = Store::open(&path).expect("a store opens");
    let session = store.create_session().expect("a session is created");
    let made = std::process::Command::new("mkfifo")
        .arg(dir.path().join(format!("sessions.db-open-{session}")))
        .status()
        .expect("mkfifo runs");
    assert!(made.success(), "a FIFO is made at the hold's name");

    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sent.send(store.is_held(session).map_err(|e| e.to_string()));
    });
    let held = received
        .recv_timeout(Duration::from_secs(5))
        .expect("the look returns rather than waiting for a writer");

    assert_eq!(held, Ok(false));
}

#[test]
fn a_session_that_opens_with_a_slash_command_is_listed_by_the_prompt_after_it() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let session = store.create_session().expect("a session is created");
    store
        .append(session, &user("/clear"))
        .expect("a session in a temporary store takes an event");
    store
        .append(session, &user("add etag support"))
        .expect("a session in a temporary store takes an event");

    let listed = store.sessions().expect("listing works");

    assert_eq!(listed[0].first_prompt.as_deref(), Some("add etag support"));
}

#[test]
fn a_prompt_that_reached_no_backend_is_neither_the_first_prompt_nor_one_of_the_prompts() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let session = store.create_session().expect("a session is created");
    store
        .append(
            session,
            &Event::PromptNotSent {
                text: "hello".to_owned(),
            },
        )
        .expect("a session in a temporary store takes an event");

    let listed = store.sessions().expect("listing works");
    assert_eq!(listed.len(), 1, "the session itself is still listed");
    assert_eq!(listed[0].first_prompt, None);
    assert!(store.prompts().expect("the prompts are read").is_empty());
}

fn meta(backend_session: &str) -> Event {
    Event::SessionMeta(niobe_core::event::SessionMeta {
        backend: niobe_core::event::Backend::Claude,
        profile: "max".to_owned(),
        model: "opus-5".to_owned(),
        backend_session: Some(backend_session.to_owned()),
    })
}

#[test]
fn every_prompt_of_every_session_comes_back_newest_first_with_its_session() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let first = store.create_session().expect("a session is created");
    let second = store.create_session().expect("a session is created");
    store
        .append(first, &meta("conv-1"))
        .expect("a session in a temporary store takes an event");
    store
        .append(first, &user("add etag support"))
        .expect("a session in a temporary store takes an event");
    store
        .append(
            first,
            &Event::AssistantDelta {
                text: "done".to_owned(),
            },
        )
        .expect("a session in a temporary store takes an event");
    store
        .append(first, &user("/compact"))
        .expect("a session in a temporary store takes an event");
    store
        .append(second, &user("why does it hang?"))
        .expect("a session in a temporary store takes an event");

    let prompts = store.prompts().expect("the prompts are read");
    let listed: Vec<(SessionId, &str)> = prompts
        .iter()
        .map(|prompt| (prompt.session, prompt.text.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            (second, "why does it hang?"),
            (first, "/compact"),
            (first, "add etag support"),
        ]
    );
    assert!(prompts.windows(2).all(|w| w[0].at >= w[1].at));
}

#[test]
fn every_conversation_a_session_carried_on_is_named_with_it() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let session = store.create_session().expect("a session is created");
    let other = store.create_session().expect("a session is created");
    for event in [meta("conv-1"), user("go"), meta("conv-1"), meta("conv-2")] {
        store
            .append(session, &event)
            .expect("a session in a temporary store takes an event");
    }
    store
        .append(other, &user("nothing said"))
        .expect("a session in a temporary store takes an event");

    let mut carried = store.conversations().expect("they are read");
    carried.sort();
    assert_eq!(
        carried,
        [
            (session, "conv-1".to_owned()),
            (session, "conv-2".to_owned())
        ]
    );
}

fn under(profile: &str) -> Event {
    Event::SessionMeta(niobe_core::event::SessionMeta {
        backend: niobe_core::event::Backend::Claude,
        profile: profile.to_owned(),
        model: "opus-5".to_owned(),
        backend_session: None,
    })
}

fn five_hour(utilization: f64) -> Event {
    Event::UsageWindows(niobe_core::event::UsageWindows {
        five_hour: Some(niobe_core::event::UsageWindow {
            utilization,
            resets_at: Some(1_789_000_000),
        }),
        seven_day: None,
        using_overage: false,
    })
}

#[test]
fn the_newest_windows_a_profile_recorded_come_back_with_when_they_were_recorded() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let epoch = SystemTime::UNIX_EPOCH;
    let older = store.create_session().expect("a session is created");
    let newer = store.create_session().expect("a session is created");
    let other = store.create_session().expect("a session is created");
    let at = |seconds| epoch + Duration::from_secs(seconds);

    store
        .append_at(older, &under("max"), at(100))
        .expect("a session in a temporary store takes an event");
    store
        .append_at(older, &five_hour(0.2), at(110))
        .expect("a session in a temporary store takes an event");
    store
        .append_at(newer, &under("max"), at(200))
        .expect("a session in a temporary store takes an event");
    store
        .append_at(newer, &five_hour(0.37), at(210))
        .expect("a session in a temporary store takes an event");
    // A report that names no window is not a level, and is passed over.
    let silent = Event::UsageWindows(niobe_core::event::UsageWindows {
        using_overage: true,
        ..niobe_core::event::UsageWindows::default()
    });
    store
        .append_at(newer, &silent, at(220))
        .expect("a session in a temporary store takes an event");
    // Another profile's account is not this one's.
    store
        .append_at(other, &under("work"), at(300))
        .expect("a session in a temporary store takes an event");
    store
        .append_at(other, &five_hour(0.9), at(310))
        .expect("a session in a temporary store takes an event");

    let (when, windows) = store
        .last_usage_windows("max")
        .expect("the store is read")
        .expect("the profile recorded windows");
    assert_eq!(when, at(210));
    assert_eq!(windows.five_hour.map(|w| w.utilization), Some(0.37));
    assert_eq!(
        store
            .last_usage_windows("nobody")
            .expect("the store is read"),
        None
    );
}

/// A session resumed under another profile runs on another account from then
/// on: what it recorded before the switch is the first profile's, and what it
/// recorded after is the second's.
#[test]
fn windows_belong_to_the_profile_in_force_when_they_were_recorded() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let at = |seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
    let session = store.create_session().expect("a session is created");

    store
        .append_at(session, &under("personal"), at(100))
        .expect("a session in a temporary store takes an event");
    store
        .append_at(session, &five_hour(0.8), at(110))
        .expect("a session in a temporary store takes an event");
    store
        .append_at(session, &under("work"), at(200))
        .expect("a session in a temporary store takes an event");

    assert_eq!(
        store.last_usage_windows("work").expect("the store is read"),
        None,
        "the windows recorded under personal were shown for work"
    );

    store
        .append_at(session, &five_hour(0.3), at(210))
        .expect("a session in a temporary store takes an event");

    let (when, windows) = store
        .last_usage_windows("work")
        .expect("the store is read")
        .expect("work recorded windows after the switch");
    assert_eq!(when, at(210));
    assert_eq!(windows.five_hour.map(|w| w.utilization), Some(0.3));
    let (when, windows) = store
        .last_usage_windows("personal")
        .expect("the store is read")
        .expect("personal recorded windows before the switch");
    assert_eq!(when, at(110));
    assert_eq!(windows.five_hour.map(|w| w.utilization), Some(0.8));
}

#[test]
fn the_newest_windows_are_the_latest_recorded_whichever_session_was_opened_first() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let at = |seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
    let opened_first = store.create_session().expect("a session is created");
    let opened_later = store.create_session().expect("a session is created");
    store
        .append_at(opened_later, &under("max"), at(100))
        .expect("a session in a temporary store takes an event");
    store
        .append_at(opened_later, &five_hour(0.2), at(110))
        .expect("a session in a temporary store takes an event");
    store
        .append_at(opened_first, &under("max"), at(200))
        .expect("a session in a temporary store takes an event");
    store
        .append_at(opened_first, &five_hour(0.37), at(210))
        .expect("a session in a temporary store takes an event");

    let (when, windows) = store
        .last_usage_windows("max")
        .expect("the store is read")
        .expect("the profile recorded windows");
    assert_eq!(when, at(210));
    assert_eq!(windows.five_hour.map(|w| w.utilization), Some(0.37));
}

#[test]
fn of_two_reports_in_the_same_millisecond_the_later_one_stands() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let session = store.create_session().expect("a session is created");
    store
        .append_at(session, &under("max"), at)
        .expect("a session in a temporary store takes an event");
    store
        .append_at(session, &five_hour(0.2), at)
        .expect("a session in a temporary store takes an event");
    store
        .append_at(session, &five_hour(0.37), at)
        .expect("a session in a temporary store takes an event");

    let (_, windows) = store
        .last_usage_windows("max")
        .expect("the store is read")
        .expect("the profile recorded windows");
    assert_eq!(windows.five_hour.map(|w| w.utilization), Some(0.37));
}

/// Appends `event` to `session` straight into the file, as a row of a build
/// that wrote something this one does not, or was cut off writing it.
fn append_raw(path: &Path, session: SessionId, seq: i64, event: &str) {
    raw(path)
        .execute(
            "INSERT INTO events (session_id, seq, at, event) VALUES (?1, ?2, 0, ?3)",
            rusqlite::params![
                session.to_string().parse::<i64>().expect("ids are numbers"),
                seq,
                event
            ],
        )
        .expect("a row can be appended directly");
}

#[test]
fn a_row_that_is_not_json_does_not_hide_the_windows_beside_it() {
    let (_dir, path) = scratch();
    let session = {
        let store = Store::open(&path).expect("a store opens");
        let session = store.create_session().expect("a session is created");
        store
            .append(session, &under("max"))
            .expect("a session in a temporary store takes an event");
        store
            .append(session, &five_hour(0.37))
            .expect("a session in a temporary store takes an event");
        session
    };
    append_raw(&path, session, 3, r#"{"type":"usage_wind"#);

    let (_, windows) = Store::open(&path)
        .expect("the store opens")
        .last_usage_windows("max")
        .expect("one torn row does not fail the read")
        .expect("the profile recorded windows");
    assert_eq!(windows.five_hour.map(|w| w.utilization), Some(0.37));
}

#[test]
fn only_a_session_meta_says_which_profile_a_session_ran_under() {
    let (_dir, path) = scratch();
    let session = {
        let store = Store::open(&path).expect("a store opens");
        let session = store.create_session().expect("a session is created");
        store
            .append(session, &under("work"))
            .expect("a session in a temporary store takes an event");
        store
            .append(session, &five_hour(0.9))
            .expect("a session in a temporary store takes an event");
        session
    };
    append_raw(
        &path,
        session,
        3,
        r#"{"type":"from_the_future","profile":"max"}"#,
    );

    assert_eq!(
        Store::open(&path)
            .expect("the store opens")
            .last_usage_windows("max")
            .expect("the store is read"),
        None
    );
}

/// A file at the store's path that is some other program's, with tables of
/// its own and no `user_version`: listing sessions looks at it and leaves it
/// as it was, instead of giving it niobe's tables, triggers and WAL mode.
#[test]
fn a_foreign_database_is_refused_to_read_and_left_byte_identical() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("sessions.db");
    {
        let conn = rusqlite::Connection::open(&path).expect("a database is made");
        conn.execute("create table t(x)", [])
            .expect("a table is made");
        conn.execute("insert into t values (1)", [])
            .expect("a row is added");
    }
    let before = std::fs::read(&path).expect("the file reads");

    let refused = Store::open_to_read(&path).expect_err("it is not a niobe store");

    assert!(
        matches!(refused, StoreError::UnsupportedSchema { found: 0, .. }),
        "{refused:?}"
    );
    assert_eq!(std::fs::read(&path).expect("the file reads"), before);
    let beside: Vec<String> = std::fs::read_dir(dir.path())
        .expect("the directory lists")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        beside,
        ["sessions.db"],
        "no WAL or index was made beside it"
    );
}
