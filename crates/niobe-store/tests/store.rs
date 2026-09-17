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
use std::time::Instant;

use niobe_core::event::{Event, Usage};
use niobe_core::session::SessionState;
use niobe_store::{Recorder, SessionId, Store, StoreError, read_log};

/// The recorded 200-event log `niobe-core` asserts its fold against.
const FIXTURE: &str = include_str!("../../niobe-core/tests/fixtures/session-200.jsonl");

/// A stored 200-event session must load and replay in under this many
/// milliseconds.
///
/// This test stays in the binary it shares with the ones around it, where the
/// shell redraw needed one of its own. Timed on four two-vCPU CI runners,
/// opening, loading and folding took a median of 2.0 to 2.8 ms with those
/// siblings running in parallel and 1.3 to 1.7 ms alone, and the slowest
/// single run of the 80 timed was 4.8 ms. The siblings cost it about 1.7
/// times, which is the factor that broke the shell redraw — it survives here
/// because it starts with thirty times the headroom, not two.
const REPLAY_BUDGET_MS: u128 = 50;

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
fn a_stored_two_hundred_event_session_loads_and_replays_inside_the_budget() {
    let (_dir, path) = scratch();
    let session = {
        let store = Store::open(&path).expect("a store opens");
        let session = store.create_session().expect("a session is created");
        for event in &fixture() {
            store.append(session, event).expect("an append succeeds");
        }
        session
    };

    let started = Instant::now();
    let store = Store::open(&path).expect("the store opens again");
    let stored = store.events(session).expect("the session loads");
    let state = SessionState::replay(stored.iter().map(|s| &s.event));
    let elapsed = started.elapsed();

    assert_eq!(stored.len(), 200);
    assert_eq!(state.totals().records, 14);
    assert!(
        elapsed.as_millis() < REPLAY_BUDGET_MS,
        "open, load and replay took {elapsed:?}, over the {REPLAY_BUDGET_MS} ms budget"
    );
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
    let update = conn.execute("UPDATE events SET event = '{}'", []);
    let delete = conn.execute("DELETE FROM events", []);
    let drop_session = conn.execute("DELETE FROM sessions", []);

    for (what, result) in [
        ("update", update),
        ("delete", delete),
        ("session delete", drop_session),
    ] {
        let error = result.expect_err(what).to_string();
        assert!(error.contains("append-only"), "{what}: {error}");
    }

    let store = Store::open(&path).expect("the store opens again");
    let session = store.sessions().expect("listing works")[0].id;
    assert_eq!(
        store.events(session).expect("load")[0].event,
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

    let error = Store::open(&path)
        .expect("the store opens")
        .events(session)
        .expect_err("an unknown event type does not load");
    assert!(
        matches!(error, StoreError::Decode { seq: 2, .. }),
        "{error}"
    );
    assert!(error.to_string().contains("event 2"), "{error}");
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
