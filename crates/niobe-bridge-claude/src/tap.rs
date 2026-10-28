// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A copy of what crosses the pipes of one `claude` process, kept on request.
//!
//! The events Niobe stores are the translation of what the CLI said, and the
//! translation is what a recording is meant to check. Keeping the stream
//! itself lets a recording be taken from a session as it ran, with no program
//! standing in front of the CLI on `PATH`.
//!
//! One file per process, in a directory the caller chose. The first line names
//! the arguments the CLI was started with, as a JSON array; each line after it
//! is `out ` or `in ` and then, byte for byte, one line the CLI wrote to
//! standard output or was sent on standard input, in the order they crossed.
//! The file holds the whole conversation — every prompt, and every tool
//! output, whatever a tool read — so it, and a directory made for it, are
//! the operator's alone, as the session store is.
//!
//! Written on a thread of its own, so that a slow disk never holds the reader
//! of the CLI's output or the writer of its input. A write that fails ends the
//! copy and nothing else: the session does not depend on it.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::mpsc::{Sender, channel};
use std::time::{SystemTime, UNIX_EPOCH};

/// Where a line went.
#[derive(Debug)]
enum Direction {
    /// The CLI wrote it to its standard output.
    Out,
    /// It was written to the CLI's standard input.
    In,
}

/// The copy of one process's pipes.
#[derive(Debug, Clone)]
pub(crate) struct Tap {
    lines: Sender<(Direction, Vec<u8>)>,
}

impl Tap {
    /// Starts a copy in a new file in `directory`, which is created if it is
    /// not there, with `argv` as its first line.
    pub(crate) fn open(directory: &Path, argv: &[String]) -> std::io::Result<Self> {
        private_dir(directory)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        let name = format!("claude-{stamp}-{}.stream", std::process::id());
        let mut file = BufWriter::new(private_file(&directory.join(name))?);
        let header = serde_json::to_string(argv).map_err(std::io::Error::other)?;
        writeln!(file, "{header}")?;
        file.flush()?;

        let (lines, queued) = channel::<(Direction, Vec<u8>)>();
        std::thread::spawn(move || {
            for (direction, line) in queued {
                let prefix: &[u8] = match direction {
                    Direction::Out => b"out ",
                    Direction::In => b"in ",
                };
                let written = file
                    .write_all(prefix)
                    .and_then(|()| file.write_all(&line))
                    .and_then(|()| file.write_all(b"\n"))
                    .and_then(|()| file.flush());
                if written.is_err() {
                    return;
                }
            }
        });
        Ok(Self { lines })
    }

    /// A line the CLI wrote, without its line break.
    pub(crate) fn out(&self, line: &[u8]) {
        // The copy ended: nothing is owed to it.
        let _ = self.lines.send((Direction::Out, line.to_vec()));
    }

    /// A line sent to the CLI, without its line break.
    pub(crate) fn sent(&self, line: &[u8]) {
        let _ = self.lines.send((Direction::In, line.to_vec()));
    }
}

/// Makes `directory` and any parent it lacks readable by the operator alone.
/// One that is there keeps its mode: the operator chose it.
#[cfg(unix)]
fn private_dir(directory: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
}

#[cfg(not(unix))]
fn private_dir(directory: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)
}

/// A new file at `path`, readable by the operator alone from the moment it
/// exists, so no other user can open it before its mode is set.
#[cfg(unix)]
fn private_file(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> std::io::Result<File> {
    File::create_new(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .expect("the path was made")
            .permissions()
            .mode()
            & 0o777
    }

    /// The copy holds every prompt and every tool output, whatever a tool
    /// read, so it is the operator's alone, as the session store is.
    #[cfg(unix)]
    #[test]
    fn a_copy_and_the_directory_it_makes_are_readable_by_the_operator_alone() {
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let directory = scratch.path().join("taps");

        let _tap = Tap::open(&directory, &["--print".to_owned()]).expect("the copy opens");

        assert_eq!(mode(&directory), 0o700);
        let files: Vec<_> = std::fs::read_dir(&directory)
            .expect("the directory reads")
            .map(|entry| entry.expect("an entry").path())
            .collect();
        assert_eq!(files.len(), 1, "{files:?}");
        assert_eq!(mode(&files[0]), 0o600);
    }
}
