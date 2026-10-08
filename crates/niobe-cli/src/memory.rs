// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What reads the agent's memory for the shell's Memory view.
//!
//! The files are read each time the view opens, on a thread of its own: the
//! lookup is bounded, but it follows the imports a repository names through
//! as many directories as it likes, and a slow or network disk can take
//! seconds over it, while the shell has a frame to draw every tick. Each file
//! is read only where it is a regular file and no larger than the bridge's
//! limit — a larger one is listed with its size — without waiting on one that
//! is not. A read each time the view opens is what lets it show a file the
//! session has just changed as it is now.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use niobe_tui::{Memory, MemoryFile};

use crate::backend;

/// The lookup itself, run on the reader's thread.
type Lookup = Arc<dyn Fn() -> Vec<MemoryFile> + Send + Sync>;

/// Reads the memory the selected profile's backend gives its agent, for a
/// session at a repository's root.
pub struct Files {
    lookup: Lookup,
    reading: Option<Receiver<Vec<MemoryFile>>>,
}

// By hand, because the lookup is a closure.
impl std::fmt::Debug for Files {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Files")
            .field("reading", &self.reading.is_some())
            .finish_non_exhaustive()
    }
}

impl Files {
    /// What the backend of `profile`, started at `root`, would be given, with
    /// the CLI's configuration directory and the home directory as this
    /// process's environment names them.
    pub fn at(root: PathBuf, profile: Option<niobe_config::Profile>) -> Self {
        let config_dir = std::env::var_os(niobe_bridge_claude::transcript::CONFIG_DIR_VAR);
        let home: Option<OsString> = std::env::var_os("HOME");
        Self::looking_up(move || {
            backend::memory(profile.as_ref(), &root, config_dir.clone(), home.clone())
        })
    }

    /// Reads with `lookup`, on a thread of its own each time a load is asked
    /// for.
    fn looking_up(lookup: impl Fn() -> Vec<MemoryFile> + Send + Sync + 'static) -> Self {
        Self {
            lookup: Arc::new(lookup),
            reading: None,
        }
    }
}

impl Memory for Files {
    fn load(&mut self) {
        if self.reading.is_some() {
            return;
        }
        let (sent, received) = mpsc::channel();
        let lookup = Arc::clone(&self.lookup);
        let started = std::thread::Builder::new()
            .name("memory".to_owned())
            .spawn(move || {
                // Nobody left to hand it to is a shell that has closed.
                let _ = sent.send(lookup());
            });
        if started.is_err() {
            // A process that can start no thread still shows what its agent
            // is given: read here, at the cost of the frames it takes.
            let (sent, received) = mpsc::channel();
            let _ = sent.send((self.lookup)());
            self.reading = Some(received);
            return;
        }
        self.reading = Some(received);
    }

    fn drain(&mut self) -> Option<Vec<MemoryFile>> {
        let found = match self.reading.as_ref()?.try_recv() {
            Ok(found) => Some(found),
            Err(TryRecvError::Empty) => return None,
            // The reader ended without an answer. Handing in an empty list
            // would say the agent is given no memory, which nobody found out;
            // the view goes on saying it is reading, and opening it again
            // starts another read.
            Err(TryRecvError::Disconnected) => None,
        };
        self.reading = None;
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn file(path: &str) -> MemoryFile {
        MemoryFile {
            scope: niobe_tui::memory::Scope::Project,
            path: path.to_owned(),
            named: path.to_owned(),
            depth: 0,
            text: Some(String::new()),
            bytes: Some(0),
            note: None,
            editable: None,
        }
    }

    /// Asks `files` for what it found until it says, or a loaded machine's
    /// patience runs out.
    fn landed(files: &mut Files) -> Option<Vec<MemoryFile>> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(found) = files.drain() {
                return Some(found);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn a_load_returns_while_a_slow_lookup_is_still_running_and_the_files_arrive_after() {
        let (release, released) = mpsc::channel::<()>();
        let released = Mutex::new(released);
        let mut files = Files::looking_up(move || {
            if let Ok(released) = released.lock() {
                let _ = released.recv();
            }
            vec![file("CLAUDE.md")]
        });

        let started = Instant::now();
        files.load();
        assert!(
            started.elapsed() < Duration::from_millis(16),
            "the load waited on the lookup"
        );
        assert_eq!(
            files.drain(),
            None,
            "something arrived before the lookup ended"
        );

        release
            .send(())
            .expect("the lookup is waiting to be released");
        let found = landed(&mut files).expect("the lookup's files never arrived");
        assert_eq!(found, vec![file("CLAUDE.md")]);
        assert_eq!(files.drain(), None, "what was found was handed in twice");
    }

    #[test]
    fn a_load_asked_for_while_one_is_under_way_is_that_one() {
        let (release, released) = mpsc::channel::<()>();
        let released = Mutex::new(released);
        let lookups = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&lookups);
        let mut files = Files::looking_up(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            if let Ok(released) = released.lock() {
                let _ = released.recv();
            }
            Vec::new()
        });

        files.load();
        files.load();
        release
            .send(())
            .expect("the lookup is waiting to be released");

        assert_eq!(landed(&mut files), Some(Vec::new()));
        assert_eq!(lookups.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_lookup_that_ends_without_an_answer_hands_in_nothing_and_the_next_load_reads_again() {
        let lookups = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&lookups);
        let mut files = Files::looking_up(move || {
            if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("the first lookup dies, as a reader thread that panicked would");
            }
            vec![file("CLAUDE.md")]
        });

        files.load();
        // Until the reader is seen gone: the drain lets go of a reader that
        // ended without an answer, and keeps one still running.
        let deadline = Instant::now() + Duration::from_secs(10);
        while files.reading.is_some() && Instant::now() < deadline {
            assert_eq!(
                files.drain(),
                None,
                "a lookup that died said the agent has no memory"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            files.reading.is_none(),
            "the dead reader was never let go of"
        );

        files.load();
        assert_eq!(landed(&mut files), Some(vec![file("CLAUDE.md")]));
    }
}
