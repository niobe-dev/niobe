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

use niobe_core::event::{AgentId, Event, ToolCallId, Usage};
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

    store.append(first, &user("a")).expect("append");
    store.append(second, &user("b")).expect("append");
    let seq = store.append(first, &user("c")).expect("append");

    assert_eq!(seq, 2);
    assert_eq!(store.events(second).expect("load")[0].seq, 1);
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
        store.append(session, &user("keep me")).expect("append");
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
        store.events(listed[0].id).expect("load")[0].event,
        user("keep me")
    );
}

#[test]
fn sessions_are_listed_newest_first_with_their_first_prompt_and_size() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let older = store.create_session().expect("a session is created");
    store
        .append(older, &user("add etag support"))
        .expect("append");
    store.append(older, &user("now the tests")).expect("append");
    let newer = store.create_session().expect("a session is created");
    store
        .append(
            newer,
            &Event::AssistantMessage {
                text: "hello".to_owned(),
                agent: None,
            },
        )
        .expect("append");

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
        store.append(session, &user("fine")).expect("append");
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
        store.append(session, &user("review it")).expect("append");
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
    store.append(session, &owned).expect("append");

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
            .expect("append");
    }

    let back: Vec<f64> = store
        .events(session)
        .expect("load")
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
    assert!(recorder.store().sessions().expect("listing").is_empty());

    recorder.record(&user("first")).expect("record");
    recorder.record(&user("second")).expect("record");

    let session = recorder
        .session()
        .expect("the first event opened a session");
    let sessions = recorder.store().sessions().expect("listing");
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
        .expect("record");

    let session = recorder.session().expect("the event opened a session");
    let stored = recorder.store().events(session).expect("load");
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
            .expect("record");
        recorder.session().expect("a session was opened")
    };

    let mut resumed = Recorder::resume(Store::open(&path).expect("the store opens again"), session)
        .expect("the session exists");
    resumed.record(&user("after the restart")).expect("record");

    let stored = resumed.store().events(session).expect("load");
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[1].seq, 2);
    assert_eq!(resumed.store().sessions().expect("listing").len(), 1);

    let missing: SessionId = "7".parse().expect("a number is a session id");
    assert!(matches!(
        Recorder::resume(Store::open(&path).expect("opens"), missing),
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
        .expect("record");
    let session = first.session().expect("a session was opened");

    let second = Recorder::resume(Store::open(&path).expect("the store opens again"), session);
    let said = second
        .expect_err("the session is open in the first recorder")
        .to_string();
    assert!(said.contains(&format!("session {session}")), "{said}");
    assert!(said.contains("open"), "{said}");

    drop(first);
    Recorder::resume(Store::open(&path).expect("opens"), session)
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
    recorder.record(&user("four")).expect("record");

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

    assert!(matches!(
        recorder.record(&user("one")),
        Err(StoreError::Hold(_))
    ));
    assert!(!target.exists(), "the link was not followed");
}

#[cfg(unix)]
#[test]
fn a_recorder_removes_its_hold_file_when_it_lets_go() {
    let (dir, path) = scratch();
    let mut first = Recorder::new(Store::open(&path).expect("a store opens"));
    first.record(&user("held")).expect("record");
    let session = first.session().expect("a session was opened");
    let hold = dir.path().join(format!("sessions.db-open-{session}"));
    assert!(hold.exists());

    drop(first);
    assert!(!hold.exists());

    let resumed = Recorder::resume(Store::open(&path).expect("opens"), session)
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
    first.record(&user("held")).expect("record");
    let session = first.session().expect("a session was opened");

    let reader = Store::open_to_read(&path).expect("the store opens to read");
    assert_eq!(reader.events(session).expect("load").len(), 1);
}

#[cfg(unix)]
#[test]
fn a_session_reads_as_held_while_a_recorder_holds_it_and_the_look_takes_nothing() {
    let (dir, path) = scratch();
    let mut first = Recorder::new(Store::open(&path).expect("a store opens"));
    first.record(&user("held")).expect("record");
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
    Recorder::resume(Store::open(&path).expect("opens"), session)
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

#[test]
fn a_session_that_opens_with_a_slash_command_is_listed_by_the_prompt_after_it() {
    let store = Store::open_in_memory().expect("an in-memory store opens");
    let session = store.create_session().expect("a session is created");
    store.append(session, &user("/clear")).expect("append");
    store
        .append(session, &user("add etag support"))
        .expect("append");

    let listed = store.sessions().expect("listing works");

    assert_eq!(listed[0].first_prompt.as_deref(), Some("add etag support"));
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
    store.append(first, &meta("conv-1")).expect("append");
    store
        .append(first, &user("add etag support"))
        .expect("append");
    store
        .append(
            first,
            &Event::AssistantDelta {
                text: "done".to_owned(),
            },
        )
        .expect("append");
    store.append(first, &user("/compact")).expect("append");
    store
        .append(second, &user("why does it hang?"))
        .expect("append");

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
        store.append(session, &event).expect("append");
    }
    store.append(other, &user("nothing said")).expect("append");

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
