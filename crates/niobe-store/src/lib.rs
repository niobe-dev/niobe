// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The session store: every event of every session, append-only, in SQLite.
//!
//! The ledger and every total on screen are folds over the event stream, so the
//! stream is the record and nothing else is. A store whose rows could be edited
//! in place would be a store whose totals cannot be defended, which is why the
//! append-only rule is enforced by the database's own triggers and not only by
//! the absence of an update method here.
//!
//! Two on-disk forms of a stream live in this crate:
//!
//! * [`Store`] — the SQLite database a session is recorded into and resumed
//!   from. [`Recorder`] is the write side a running session holds.
//! * [`read_log`] — a JSON Lines log, one [`niobe_core::Event`] per line, as
//!   the development `niobe replay` command and the test fixtures use.
//!
//! Depends on `niobe-core` and on no other workspace crate.

mod jsonl;
mod recorder;
mod store;

pub use jsonl::{LogError, read_log};
pub use recorder::Recorder;
pub use store::{SCHEMA_VERSION, SessionId, SessionSummary, Store, StoreError, StoredEvent};
