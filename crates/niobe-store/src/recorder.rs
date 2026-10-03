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
/// session in the list. It opens it once: where the session's hold cannot be
/// taken, the event is refused and the next one tries the hold again on the
/// same session, rather than opening another — rows are never deleted, so
/// each would stay in the list for good.
#[derive(Debug)]
pub struct Recorder {
    store: Store,
    session: Option<SessionId>,
    /// The session's hold, kept for as long as the recorder is: see
    /// [`Held`]. `None` until it has been taken.
    held: Option<Held>,
}

impl Recorder {
    /// A recorder for a session that does not exist yet.
    pub fn new(store: Store) -> Self {
        Self {
            store,
            session: None,
            held: None,
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
            held: Some(held),
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
        let session = self.held_session()?;
        self.store.append_at(session, event, at)
    }

    /// The session, opened if it is not yet and held if it is not yet.
    fn held_session(&mut self) -> Result<SessionId, StoreError> {
        let session = match self.session {
            Some(session) => session,
            None => {
                let session = self.store.create_session()?;
                self.session = Some(session);
                session
            }
        };
        if self.held.is_none() {
            self.held = Some(Held::take(&self.store, session)?);
        }
        Ok(session)
    }
}

/// A session held open by a recorder: an exclusive `flock` on a file beside
/// the store named for the session, taken without waiting.
///
/// Advisory, and released by the kernel when the file is closed — with the
/// recorder, or with the process however it ends — so a session a killed
/// niobe was recording is free again at once. The recorder removes the file
/// as it lets go, while it still holds the lock, so the store's directory does
/// not gather one file per session; a killed niobe leaves its file behind,
/// and the next hold on that session takes it over. A store with no file
/// behind it, in memory, holds nothing. Where there is no `flock`, nothing is
/// held.
#[derive(Debug)]
struct Held {
    /// The file and the name it was locked at; `None` for a store in memory.
    #[cfg(unix)]
    file: Option<(std::path::PathBuf, std::os::fd::OwnedFd)>,
}

/// How many times a hold is tried on a file that was removed between being
/// opened and being locked, before the session is taken to be held: each
/// retry means another recorder took the session and let it go meanwhile.
#[cfg(unix)]
const HOLD_ATTEMPTS: usize = 3;

impl Held {
    #[cfg(unix)]
    fn take(store: &Store, session: SessionId) -> Result<Self, StoreError> {
        let Some(path) = store.path() else {
            return Ok(Self { file: None });
        };
        let path = hold_path(&path, session);
        for _ in 0..HOLD_ATTEMPTS {
            let file = Self::lock(&path, session)?;
            // A recorder letting go removes the file before closing it, so the
            // lock just taken may be on a file no longer at this name, which
            // the next recorder would make afresh and lock as well.
            if is_at(&file, &path)? {
                return Ok(Self {
                    file: Some((path, file)),
                });
            }
        }
        Err(StoreError::SessionOpen(session))
    }

    #[cfg(not(unix))]
    fn take(_store: &Store, _session: SessionId) -> Result<Self, StoreError> {
        Ok(Self {})
    }

    #[cfg(unix)]
    fn lock(
        path: &std::path::Path,
        session: SessionId,
    ) -> Result<std::os::fd::OwnedFd, StoreError> {
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
            Ok(()) => Ok(file),
            Err(rustix::io::Errno::WOULDBLOCK) => Err(StoreError::SessionOpen(session)),
            Err(e) => Err(StoreError::Hold(e.into())),
        }
    }
}

#[cfg(unix)]
impl Drop for Held {
    fn drop(&mut self) {
        let Some((path, file)) = &self.file else {
            return;
        };
        // Only the file this hold locked, and before the lock goes with it:
        // removed after, it could be another recorder's by then. A file left
        // behind is taken over by the next hold, so a failure here costs a
        // file and nothing else, and a drop has nowhere to report it.
        if matches!(is_at(file, path), Ok(true)) {
            let _ = rustix::fs::unlink(path);
        }
    }
}

/// Whether `session` of `store` is held: see [`Store::is_held`].
///
/// A holder always has its file at the name, so a session with none is not
/// held; one with a file is held where a shared lock on it is refused. The
/// shared lock is let go at once, and a hold being taken in that instant is
/// refused as a hold on a held session is: only a session the list already
/// shows is looked at, and only a resume of that same session can be taking
/// its hold then.
#[cfg(unix)]
pub(crate) fn is_held(store: &Store, session: SessionId) -> Result<bool, StoreError> {
    use rustix::fs::{FlockOperation, Mode, OFlags};
    let Some(path) = store.path() else {
        return Ok(false);
    };
    let file = match rustix::fs::open(
        hold_path(&path, session),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(file) => file,
        Err(rustix::io::Errno::NOENT) => return Ok(false),
        Err(e) => return Err(StoreError::Hold(e.into())),
    };
    match rustix::fs::flock(&file, FlockOperation::NonBlockingLockShared) {
        Ok(()) => Ok(false),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(true),
        Err(e) => Err(StoreError::Hold(e.into())),
    }
}

#[cfg(not(unix))]
pub(crate) fn is_held(_store: &Store, _session: SessionId) -> Result<bool, StoreError> {
    Ok(false)
}

/// Whether `file` is the file at `path` now, not one that was there.
#[cfg(unix)]
fn is_at(file: &std::os::fd::OwnedFd, path: &std::path::Path) -> Result<bool, StoreError> {
    let held = rustix::fs::fstat(file).map_err(|e| StoreError::Hold(e.into()))?;
    match rustix::fs::lstat(path) {
        Ok(named) => Ok(held.st_dev == named.st_dev && held.st_ino == named.st_ino),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(e) => Err(StoreError::Hold(e.into())),
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
