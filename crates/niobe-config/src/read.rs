// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Reading a config file that arrived with a clone, or any other file a
//! repository holds.
//!
//! A repository's `.niobe/config.toml` is whatever the repository committed,
//! and git commits a symlink as readily as a file: one to `/dev/zero` reads
//! forever and fills memory, and one to a FIFO blocks the open until something
//! writes to it. So the file is opened without waiting, is read only if the
//! handle is a regular file, and is read no further than a config could run.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// The most a config file may hold, in bytes.
///
/// A hand-written config is a few kilobytes; a mebibyte is room for any the
/// operator could have written, and small enough that reading one is instant.
pub const LIMIT: u64 = 1024 * 1024;

/// The text of the file at `path`, as [`std::fs::read_to_string`] would give
/// it, but only where `path` is a regular file no larger than [`LIMIT`].
///
/// A file that is not there is an error of kind
/// [`io::ErrorKind::NotFound`], as it is for the standard library, so a caller
/// can still tell no file from a file it cannot read. Anything else — a
/// device, a FIFO, a directory, a file past the limit — is an error that says
/// which, returned without waiting on the file or reading past the limit.
pub fn text(path: &Path) -> io::Result<String> {
    let bytes = bounded(path, ", which no config is")?;
    String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// The bytes of the file at `path`, on the terms [`text`] reads it: only
/// where it is a regular file no larger than [`LIMIT`], and without waiting
/// on it. For a file the repository holds that need not be text.
pub fn bytes(path: &Path) -> io::Result<Vec<u8>> {
    bounded(path, "")
}

/// The file's bytes, or an error that says why not; `too_large` ends the
/// message for a file past [`LIMIT`].
fn bounded(path: &Path, too_large: &str) -> io::Result<Vec<u8>> {
    let file = open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(LIMIT.saturating_add(1)).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("larger than {} MiB{too_large}", LIMIT / (1024 * 1024)),
        ));
    }
    Ok(bytes)
}

/// Opens `path` for reading without waiting for a writer, which opening a FIFO
/// otherwise does. On a regular file the flag changes nothing.
#[cfg(unix)]
fn open(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    // The flag's bits are a small positive number, so they fit an `i32`; were
    // they not to, opening without it would still refuse a FIFO afterwards,
    // only after waiting for a writer.
    let nonblock = i32::try_from(rustix::fs::OFlags::NONBLOCK.bits()).unwrap_or(0);
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nonblock)
        .open(path)
}

#[cfg(not(unix))]
fn open(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// How long a refusal may take: long enough for a loaded machine, far
    /// shorter than reading `/dev/zero` into memory or waiting on a FIFO.
    const FAST: Duration = Duration::from_secs(2);

    /// The refusal of `path`, read on a thread of its own so that a read
    /// that waits fails the test at the deadline rather than holding it up.
    fn refused(path: &Path) -> io::Error {
        let (sender, received) = std::sync::mpsc::channel();
        let path = path.to_path_buf();
        std::thread::spawn(move || sender.send(text(&path)));
        received
            .recv_timeout(FAST)
            .expect("the read returns without waiting on the file")
            .expect_err("the file is refused")
    }

    #[test]
    fn a_regular_file_reads_as_its_text() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "default_profile = \"a\"\n").expect("the file is written");

        assert_eq!(text(&path).expect("it reads"), "default_profile = \"a\"\n");
    }

    #[test]
    fn a_missing_file_is_not_found_as_it_is_for_the_standard_library() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");

        let error = text(&dir.path().join("config.toml")).expect_err("there is no file");

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_dev_zero_is_refused_without_reading_it() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let path = dir.path().join("config.toml");
        std::os::unix::fs::symlink("/dev/zero", &path).expect("the link is made");

        assert_eq!(refused(&path).to_string(), "not a regular file");
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_waiting_for_a_writer() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let path = dir.path().join("config.toml");
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "the FIFO is made");

        assert_eq!(refused(&path).to_string(), "not a regular file");
    }

    #[test]
    fn a_file_past_the_limit_is_refused_without_reading_it_all() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, vec![b'#'; 10 * 1024 * 1024]).expect("the file is written");

        assert_eq!(
            refused(&path).to_string(),
            "larger than 1 MiB, which no config is"
        );
    }

    #[test]
    fn a_file_exactly_at_the_limit_still_reads() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let path = dir.path().join("config.toml");
        let limit = usize::try_from(LIMIT).expect("a mebibyte fits a usize");
        std::fs::write(&path, vec![b'#'; limit]).expect("the file is written");

        assert_eq!(text(&path).expect("it reads").len(), limit);
    }

    #[test]
    fn a_directory_is_refused() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");

        assert_eq!(refused(dir.path()).to_string(), "not a regular file");
    }
}
