// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The write side a running session holds.

use niobe_core::event::Event;

use crate::store::{SessionId, Store, StoreError};

/// Records one session's events into a [`Store`].
///
/// A new recorder opens its session on the first event rather than up front,
/// so opening the shell and quitting without doing anything leaves no empty
/// session in the list.
#[derive(Debug)]
pub struct Recorder {
    store: Store,
    session: Option<SessionId>,
}

impl Recorder {
    /// A recorder for a session that does not exist yet.
    pub fn new(store: Store) -> Self {
        Self {
            store,
            session: None,
        }
    }

    /// A recorder that appends to an existing session, after the events it
    /// already holds.
    pub fn resume(store: Store, session: SessionId) -> Result<Self, StoreError> {
        if !store.has_session(session)? {
            return Err(StoreError::NoSuchSession(session));
        }
        Ok(Self {
            store,
            session: Some(session),
        })
    }

    /// The session being recorded, once there is one.
    pub fn session(&self) -> Option<SessionId> {
        self.session
    }

    /// The store underneath, for reading.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Appends one event, opening the session first if this is its first.
    /// Returns the event's sequence number.
    pub fn record(&mut self, event: &Event) -> Result<u64, StoreError> {
        let session = match self.session {
            Some(session) => session,
            None => {
                let session = self.store.create_session()?;
                self.session = Some(session);
                session
            }
        };
        self.store.append(session, event)
    }
}
