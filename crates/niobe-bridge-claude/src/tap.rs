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
//! The file holds the whole conversation.
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
        std::fs::create_dir_all(directory)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        let name = format!("claude-{stamp}-{}.stream", std::process::id());
        let mut file = BufWriter::new(File::create_new(directory.join(name))?);
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
