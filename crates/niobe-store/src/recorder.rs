// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The write side a running session holds.

use std::time::SystemTime;

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
    /// The session's hold, kept for as long as the recorder is: see
    /// [`Held`].
    _held: Option<Held>,
}

impl Recorder {
    /// A recorder for a session that does not exist yet.
    pub fn new(store: Store) -> Self {
        Self {
            store,
            session: None,
            _held: None,
        }
    }

    /// A recorder that appends to an existing session, after the events it
    /// already holds.
    ///
    /// Refused with [`StoreError::SessionOpen`] while another recorder — in
    /// this process or another — holds the session. Reading it is not.
    pub fn resume(store: Store, session: SessionId) -> Result<Self, StoreError> {
        if !store.has_session(session)? {
            return Err(StoreError::NoSuchSession(session));
        }
        let held = Held::take(&store, session)?;
        Ok(Self {
            store,
            session: Some(session),
            _held: held,
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

    /// Appends one event, dated now, opening the session first if this is its
    /// first. Returns the event's sequence number.
    pub fn record(&mut self, event: &Event) -> Result<u64, StoreError> {
        self.record_at(event, SystemTime::now())
    }

    /// Appends one event dated `at`, as [`record`](Self::record) does: see
    /// [`Store::append_at`].
    pub fn record_at(&mut self, event: &Event, at: SystemTime) -> Result<u64, StoreError> {
        let session = match self.session {
            Some(session) => session,
            None => {
                let session = self.store.create_session()?;
                self._held = Held::take(&self.store, session)?;
                self.session = Some(session);
                session
            }
        };
        self.store.append_at(session, event, at)
    }
}

/// A session held open by a recorder: an exclusive `flock` on a file beside
/// the store named for the session, taken without waiting.
///
/// Advisory, and released by the kernel when the file is closed — with the
/// recorder, or with the process however it ends — so a session a killed
/// niobe was recording is free again at once. A store with no file behind it,
/// in memory, holds nothing. Where there is no `flock`, nothing is held.
#[derive(Debug)]
struct Held {
    #[cfg(unix)]
    _file: std::os::fd::OwnedFd,
}

impl Held {
    fn take(store: &Store, session: SessionId) -> Result<Option<Self>, StoreError> {
        let Some(path) = store.path() else {
            return Ok(None);
        };
        Self::at(&hold_path(&path, session), session).map(Some)
    }

    #[cfg(unix)]
    fn at(path: &std::path::Path, session: SessionId) -> Result<Self, StoreError> {
        use rustix::fs::{FlockOperation, Mode, OFlags};
        // Not through a link: a repository chooses its links, and one at this
        // name would choose where an empty file is made.
        let file = rustix::fs::open(
            path,
            OFlags::CREATE | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|e| StoreError::Hold(e.into()))?;
        match rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Self { _file: file }),
            Err(rustix::io::Errno::WOULDBLOCK) => Err(StoreError::SessionOpen(session)),
            Err(e) => Err(StoreError::Hold(e.into())),
        }
    }

    #[cfg(not(unix))]
    fn at(_path: &std::path::Path, _session: SessionId) -> Result<Self, StoreError> {
        Ok(Self {})
    }
}

/// The file beside the store at `store` that marks `session` as held. Named
/// with the store's own name before it, so an ignore rule for the store's
/// side files covers it.
fn hold_path(store: &std::path::Path, session: SessionId) -> std::path::PathBuf {
    let mut name = store.as_os_str().to_owned();
    name.push(format!("-open-{session}"));
    std::path::PathBuf::from(name)
}
