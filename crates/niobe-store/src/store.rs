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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use niobe_core::event::Event;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

/// The schema this build creates and reads. Stored in SQLite's `user_version`
/// so that a store written by a newer build is refused instead of misread.
pub const SCHEMA_VERSION: i64 = 1;

/// How long a write waits for another `niobe` in the same repository to finish
/// its own before failing.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The whole schema at [`SCHEMA_VERSION`], less [`REPLACE_GUARDS`].
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

/// The rest of the append-only rule: a row inserted over one already there.
///
/// `REPLACE` and `INSERT OR REPLACE` delete the row in the way and insert the
/// new one, and SQLite fires no delete trigger for that unless recursive
/// triggers are on, so without these a stored event could be rewritten in
/// place from the `sqlite3` shell. Created where missing on every open that
/// can write, rather than by a new schema version, so that a store an
/// earlier build made gains them and an earlier build can still read it.
const REPLACE_GUARDS: &str = "
CREATE TRIGGER IF NOT EXISTS events_are_append_only_on_replace BEFORE INSERT ON events
WHEN EXISTS (SELECT 1 FROM events WHERE session_id = NEW.session_id AND seq = NEW.seq)
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;

CREATE TRIGGER IF NOT EXISTS sessions_are_append_only_on_replace BEFORE INSERT ON sessions
WHEN NEW.id IS NOT NULL AND EXISTS (SELECT 1 FROM sessions WHERE id = NEW.id)
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
    /// Another niobe is recording this session: two recording it at once
    /// would interleave two conversations in one record.
    SessionOpen(SessionId),
    /// The file that marks a session as being recorded could not be opened.
    Hold(std::io::Error),
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
            Self::SessionOpen(id) => write!(
                f,
                "session {id} is open in another niobe; `niobe --resume {id} | cat` prints \
                 it without taking it over"
            ),
            Self::Hold(e) => write!(f, "cannot mark the session as open: {e}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(e) => Some(e),
            Self::Encode(e) | Self::Decode { source: e, .. } => Some(e),
            Self::Hold(e) => Some(e),
            Self::UnsupportedSchema { .. } | Self::NoSuchSession(_) | Self::SessionOpen(_) => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

/// What stands in a session's events for a row that could not be read.
fn unreadable(error: &StoreError) -> Event {
    Event::Error {
        message: format!("{error}; it is left out of what this session shows"),
        fatal: false,
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

    /// The file the store is kept in, or `None` for one kept in memory.
    pub(crate) fn path(&self) -> Option<std::path::PathBuf> {
        self.conn
            .path()
            .filter(|path| !path.is_empty())
            .map(std::path::PathBuf::from)
    }

    /// Opens the store at `path` to read it, as [`Store::open`] does where the
    /// file can be written, and as it is where it cannot.
    ///
    /// A store on a read-only mount, or whose file and directory were made
    /// read-only, is still one the operator can list. [`Store::open`] refuses
    /// it: putting the file in WAL mode is a write. Where that is the refusal,
    /// the file is opened read-only, and where SQLite cannot read a WAL file
    /// that way either — a clean close leaves no `-shm` beside it, and one
    /// cannot be made in a directory that is not writable — it is read as
    /// immutable: as it is on disk, with no locking, which is right because
    /// nothing can be writing to it. A store opened this way refuses writes.
    pub fn open_to_read(path: &Path) -> Result<Self, StoreError> {
        match Self::open(path) {
            Err(StoreError::Sqlite(error)) if refused_a_write(&error) => {}
            opened => return opened,
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;
        match Self::readable(Connection::open_with_flags(path, flags)?) {
            Err(StoreError::Sqlite(error)) if refused_a_write(&error) => {}
            opened => return opened,
        }
        Self::readable(Connection::open_with_flags(immutable_uri(path), flags)?)
    }

    /// A read-only connection, once it has shown it can read the store and
    /// that the store is one this build reads.
    fn readable(conn: Connection) -> Result<Self, StoreError> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        let found: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if found != SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema {
                found,
                supported: SCHEMA_VERSION,
            });
        }
        Ok(Self { conn })
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
        use_wal(&conn)?;
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

    /// Every event of a session, in the order it was appended. A row this
    /// build cannot read is a non-fatal [`Event::Error`] in its place.
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
            // One row this build cannot read — written by a newer niobe, or
            // torn — stands as a warning in its place rather than keeping the
            // rest of the session from being opened.
            let event = serde_json::from_str(&json).unwrap_or_else(|source| {
                unreadable(&StoreError::Decode {
                    session,
                    seq,
                    source,
                })
            });
            Ok(StoredEvent {
                seq,
                at: from_unix_millis(at),
                event,
            })
        })
        .collect()
    }

    /// Every session in the store, newest first.
    ///
    /// A slash command is passed over when looking for the first prompt: it
    /// is an instruction to the CLI, such as `/clear`, and not what the
    /// session was asked to do.
    ///
    /// A row that is not JSON is passed over when looking for the first
    /// prompt, because `json_extract` fails the whole statement on one: a torn
    /// row in one session would otherwise hide every session from the list.
    /// The `CASE` is what guarantees `json_extract` never sees it; SQLite may
    /// evaluate the terms of an `AND` in either order.
    pub fn sessions(&self) -> Result<Vec<SessionSummary>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT s.id, s.started_at, COUNT(e.seq), MAX(e.at),
                    (SELECT json_extract(f.event, '$.text')
                       FROM events f
                      WHERE f.session_id = s.id
                        AND CASE WHEN json_valid(f.event)
                                 THEN json_extract(f.event, '$.type') = 'user_message'
                                      AND substr(json_extract(f.event, '$.text'), 1, 1) <> '/'
                            END
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

/// Whether SQLite refused because something had to be written — a read-only
/// file, or a directory it could not make its `-shm` file in.
fn refused_a_write(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(failure.code, rusqlite::ErrorCode::ReadOnly | rusqlite::ErrorCode::CannotOpen)
    )
}

/// The `file:` URI that opens `path` as immutable, with every byte a URI
/// gives a meaning to written as `%XX`.
fn immutable_uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for byte in path.as_os_str().as_encoded_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                uri.push(char::from(*byte));
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri.push_str("?immutable=1");
    uri
}

/// Creates the schema in a new file and checks the version of an existing one.
///
/// The version is read inside an immediate transaction, so two processes
/// opening the same new file cannot both decide to create the tables.
/// Puts the file in WAL mode, which it keeps from then on.
///
/// Two shells creating the store at once both convert it, and SQLite answers
/// the loser of that race busy at once rather than through the busy timeout,
/// because waiting there could deadlock; so the conversion is tried again,
/// under the same timeout, until the other shell's has finished.
fn use_wal(conn: &Connection) -> Result<(), StoreError> {
    let started = Instant::now();
    loop {
        match conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(())) {
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::DatabaseBusy
                    && started.elapsed() < BUSY_TIMEOUT =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            done => return Ok(done?),
        }
    }
}

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
    tx.execute_batch(REPLACE_GUARDS)?;

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

    /// A full disk fails the append with an error rather than dropping the
    /// event, and once there is room again the next event is numbered after
    /// the last one kept, so the session has no hole where the disk was full.
    /// `max_page_count` stands in for the disk: it is SQLite's own way to make
    /// a write report `SQLITE_FULL`.
    #[test]
    fn a_full_disk_fails_the_append_and_the_session_goes_on_without_a_gap() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let store = Store::open(&dir.path().join("sessions.db")).expect("a store opens");
        let session = store.create_session().expect("a session is created");
        let text = |i: usize| Event::UserMessage {
            text: format!("{i} {}", "x".repeat(2_000)),
        };
        store.append(session, &text(0)).expect("append");

        let pages: i64 = store
            .conn
            .pragma_query_value(None, "page_count", |row| row.get(0))
            .expect("the page count reads");
        store
            .conn
            .pragma_update(None, "max_page_count", pages)
            .expect("the page limit is set");
        let error = (1..100)
            .find_map(|i| store.append(session, &text(i)).err())
            .expect("the capped file fills up");
        assert!(
            matches!(&error, StoreError::Sqlite(e) if e.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull)),
            "{error}"
        );

        store
            .conn
            .pragma_update(None, "max_page_count", 1_000_000)
            .expect("the page limit is lifted");
        let kept = store.events(session).expect("the session loads");
        let next = store.append(session, &text(999)).expect("append");
        assert_eq!(next, kept.len() as u64 + 1);
        let seqs: Vec<u64> = store
            .events(session)
            .expect("the session loads")
            .iter()
            .map(|stored| stored.seq)
            .collect();
        assert_eq!(seqs, (1..=next).collect::<Vec<_>>());
    }

    #[test]
    fn a_clock_before_the_epoch_reads_as_the_epoch() {
        let before = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(unix_millis(before), 0);
    }
}
