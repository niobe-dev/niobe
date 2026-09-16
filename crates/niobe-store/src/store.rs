// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The SQLite database sessions are recorded into and resumed from.
//!
//! One row per event, keyed by the session and a per-session sequence number,
//! carrying the wall clock at which it was stored and the event as the JSON it
//! serializes to. [`Event`] deliberately has no timestamp and no sequence
//! number of its own: this row is where both live.
//!
//! The event is stored as JSON text rather than spread over columns so that a
//! new event variant needs no migration, and so that a row read with the
//! `sqlite3` shell is legible without this crate.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use niobe_core::event::Event;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

/// The schema this build creates and reads. Stored in SQLite's `user_version`
/// so that a store written by a newer build is refused instead of misread.
pub const SCHEMA_VERSION: i64 = 1;

/// How long a write waits for another `niobe` in the same repository to finish
/// its own before failing.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The whole schema at [`SCHEMA_VERSION`].
///
/// The triggers are the append-only rule. They make an `UPDATE` or `DELETE`
/// fail in the database itself, so the rule holds for the `sqlite3` shell and
/// for any future code path, not only for the methods this crate happens to
/// expose.
const SCHEMA: &str = "
CREATE TABLE sessions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at INTEGER NOT NULL
);

CREATE TABLE events (
    session_id INTEGER NOT NULL REFERENCES sessions (id),
    seq        INTEGER NOT NULL,
    at         INTEGER NOT NULL,
    event      TEXT    NOT NULL,
    PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;

CREATE TRIGGER events_are_append_only_on_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;

CREATE TRIGGER events_are_append_only_on_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;

CREATE TRIGGER sessions_are_append_only_on_update BEFORE UPDATE ON sessions
BEGIN SELECT RAISE(ABORT, 'sessions are append-only'); END;

CREATE TRIGGER sessions_are_append_only_on_delete BEFORE DELETE ON sessions
BEGIN SELECT RAISE(ABORT, 'sessions are append-only'); END;
";

/// Identifies a session within one store.
///
/// Allocated by the database and never reused, so the number an operator reads
/// off `niobe sessions` names the same session for the life of the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(i64);

impl std::fmt::Display for SessionId {
    /// Formats as the bare number, honouring width and alignment so the id
    /// lines up in a table.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::str::FromStr for SessionId {
    type Err = std::num::ParseIntError;

    /// Parses the number `niobe sessions` prints. A number that names no
    /// session parses fine and is reported when it is looked up.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.trim().parse().map(Self)
    }
}

/// An event as it was stored.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    /// Position in the session, from 1, without gaps.
    pub seq: u64,
    /// The wall clock when the event was stored.
    pub at: SystemTime,
    /// What happened.
    pub event: Event,
}

/// One line of the session list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// The session.
    pub id: SessionId,
    /// When the session was created.
    pub started_at: SystemTime,
    /// When its newest event was stored; `None` for a session with no events.
    pub last_at: Option<SystemTime>,
    /// How many events it holds.
    pub events: u64,
    /// The text of its first user message, if it has one.
    pub first_prompt: Option<String>,
}

/// Why the store could not do what was asked.
#[derive(Debug)]
pub enum StoreError {
    /// SQLite failed: the file is unreadable, locked past the busy timeout, or
    /// not a database.
    Sqlite(rusqlite::Error),
    /// An event could not be serialized.
    Encode(serde_json::Error),
    /// A stored row is not an event this build understands.
    Decode {
        /// The session the row belongs to.
        session: SessionId,
        /// The row's sequence number.
        seq: u64,
        /// What the parser said.
        source: serde_json::Error,
    },
    /// The file carries a schema version this build does not read.
    UnsupportedSchema {
        /// The version in the file.
        found: i64,
        /// The version this build reads.
        supported: i64,
    },
    /// No session has this id.
    NoSuchSession(SessionId),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "session store: {e}"),
            Self::Encode(e) => write!(f, "an event could not be serialized: {e}"),
            Self::Decode {
                session,
                seq,
                source,
            } => write!(
                f,
                "session {session}, event {seq} cannot be read by this niobe: {source}"
            ),
            Self::UnsupportedSchema { found, supported } => write!(
                f,
                "the session store is at schema version {found} and this niobe reads \
                 version {supported}; it was written by a different niobe"
            ),
            Self::NoSuchSession(id) => write!(f, "no session {id} in this store"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(e) => Some(e),
            Self::Encode(e) | Self::Decode { source: e, .. } => Some(e),
            Self::UnsupportedSchema { .. } | Self::NoSuchSession(_) => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

/// An open session store.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens the store at `path`, creating the file and the schema if neither
    /// exists. The parent directory must exist.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::prepare(Connection::open(path)?)
    }

    /// A store that lives only as long as the value, for tests and for a
    /// session that is not to be kept.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(mut conn: Connection) -> Result<Self, StoreError> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        // WAL lets `niobe sessions` read while a shell in the same repository
        // writes. `synchronous = NORMAL` syncs at checkpoints rather than on
        // every commit: a crash of the process loses nothing, and a power cut
        // can lose the last few events but never corrupts the file. Every
        // streamed delta is its own commit, so `FULL` would pay a sync for each
        // fragment of every reply.
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&mut conn)?;
        Ok(Self { conn })
    }

    /// Starts a new, empty session.
    pub fn create_session(&self) -> Result<SessionId, StoreError> {
        self.conn.execute(
            "INSERT INTO sessions (started_at) VALUES (?1)",
            params![unix_millis(SystemTime::now())],
        )?;
        Ok(SessionId(self.conn.last_insert_rowid()))
    }

    /// Whether a session with this id exists.
    pub fn has_session(&self, session: SessionId) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![session.0],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Appends one event to a session and returns its sequence number.
    ///
    /// The sequence number is computed and inserted in one statement, and the
    /// `(session, seq)` key is unique, so two writers to the same session get an
    /// error rather than two events at the same position.
    pub fn append(&self, session: SessionId, event: &Event) -> Result<u64, StoreError> {
        let json = serde_json::to_string(event).map_err(StoreError::Encode)?;
        let inserted = self.conn.query_row(
            "INSERT INTO events (session_id, seq, at, event)
             SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3 FROM events WHERE session_id = ?1
             RETURNING seq",
            params![session.0, unix_millis(SystemTime::now()), json],
            |row| row.get::<_, i64>(0),
        );

        match inserted {
            Ok(seq) => Ok(seq.unsigned_abs()),
            // A foreign-key failure is the common way in here, but whatever the
            // failure, an absent session is the more useful thing to report.
            Err(e) => match self.has_session(session) {
                Ok(false) => Err(StoreError::NoSuchSession(session)),
                _ => Err(e.into()),
            },
        }
    }

    /// Every event of a session, in the order it was appended.
    pub fn events(&self, session: SessionId) -> Result<Vec<StoredEvent>, StoreError> {
        if !self.has_session(session)? {
            return Err(StoreError::NoSuchSession(session));
        }

        let mut statement = self
            .conn
            .prepare("SELECT seq, at, event FROM events WHERE session_id = ?1 ORDER BY seq")?;
        let rows = statement.query_map(params![session.0], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        rows.map(|row| {
            let (seq, at, json) = row?;
            let seq = seq.unsigned_abs();
            let event = serde_json::from_str(&json).map_err(|source| StoreError::Decode {
                session,
                seq,
                source,
            })?;
            Ok(StoredEvent {
                seq,
                at: from_unix_millis(at),
                event,
            })
        })
        .collect()
    }

    /// Every session in the store, newest first.
    pub fn sessions(&self) -> Result<Vec<SessionSummary>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT s.id, s.started_at, COUNT(e.seq), MAX(e.at),
                    (SELECT json_extract(f.event, '$.text')
                       FROM events f
                      WHERE f.session_id = s.id
                        AND json_extract(f.event, '$.type') = 'user_message'
                      ORDER BY f.seq
                      LIMIT 1)
               FROM sessions s
               LEFT JOIN events e ON e.session_id = s.id
              GROUP BY s.id
              ORDER BY s.id DESC",
        )?;

        let rows = statement.query_map([], |row| {
            Ok(SessionSummary {
                id: SessionId(row.get(0)?),
                started_at: from_unix_millis(row.get(1)?),
                events: row.get::<_, i64>(2)?.unsigned_abs(),
                last_at: row.get::<_, Option<i64>>(3)?.map(from_unix_millis),
                first_prompt: row.get(4)?,
            })
        })?;

        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// Creates the schema in a new file and checks the version of an existing one.
///
/// The version is read inside an immediate transaction, so two processes
/// opening the same new file cannot both decide to create the tables.
fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let found: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;

    match found {
        0 => {
            tx.execute_batch(SCHEMA)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        SCHEMA_VERSION => {}
        _ => {
            return Err(StoreError::UnsupportedSchema {
                found,
                supported: SCHEMA_VERSION,
            });
        }
    }

    tx.commit()?;
    Ok(())
}

/// Milliseconds since the Unix epoch. A clock set before 1970 reads as the
/// epoch rather than wrapping.
fn unix_millis(at: SystemTime) -> i64 {
    at.duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn from_unix_millis(millis: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(millis.unsigned_abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_id_is_the_number_the_list_prints() {
        let id: SessionId = " 12 ".parse().expect("a number parses");
        assert_eq!(id.to_string(), "12");
        assert_eq!(format!("{id:>4}"), "  12");
        assert!("twelve".parse::<SessionId>().is_err());
    }

    #[test]
    fn wall_clock_millis_round_trip() {
        let at = UNIX_EPOCH + Duration::from_millis(1_789_000_000_123);
        assert_eq!(from_unix_millis(unix_millis(at)), at);
    }

    #[test]
    fn a_clock_before_the_epoch_reads_as_the_epoch() {
        let before = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(unix_millis(before), 0);
    }
}
