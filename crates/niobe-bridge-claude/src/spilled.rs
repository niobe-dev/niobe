// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The file the CLI saves a shell command's output to when it is too large to
//! hand the model whole.
//!
//! Past about 30 KB the CLI gives the model the first 2 KB of the output and
//! writes all of it to a file of its own, under its config directory in the
//! session's `tool-results/`. The result's report names the file and how many
//! bytes went into it. That file is the CLI's output, not a credential: this
//! reads nothing the operator could not `cat`, and only a file the CLI named.
//!
//! The byte count is what says the file is still the one written for that
//! result. Across 186 saved outputs on the machine this was written on, every
//! file still there held exactly the bytes its report named.

use std::io::Read;
use std::path::Path;

/// The largest saved output read. The largest of the 186 above was 6.6 MB; a
/// test run many times that is not one worth holding in memory to count.
const LIMIT: u64 = 64 * 1024 * 1024;

/// The whole of the output saved at `path`, where the file holds exactly
/// `size` bytes of UTF-8. `None` where it is missing, cannot be read, has
/// changed size since the CLI wrote it, or is larger than [`LIMIT`].
pub(crate) fn read(path: &Path, size: u64) -> Option<String> {
    if size > LIMIT {
        return None;
    }
    let file = open(path).ok()?;
    // A path the report names is only the CLI's word: a device or a FIFO
    // there would be read for ever or waited on, and hold the session up.
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    // One byte past the size, so that a file that has grown is seen to have.
    file.take(size.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 != size {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Opens `path` for reading without waiting for a writer, which opening a
/// FIFO otherwise does. On a regular file the flag changes nothing.
#[cfg(unix)]
pub(crate) fn open(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    // The flag's bits fit an `i32`; were they not to, the regular-file check
    // after the open would still refuse a FIFO, only after waiting on it.
    let nonblock = i32::try_from(rustix::fs::OFlags::NONBLOCK.bits()).unwrap_or(0);
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nonblock)
        .open(path)
}

#[cfg(not(unix))]
pub(crate) fn open(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        reason = "test helpers: a failed expectation is the test failing"
    )]

    use super::*;

    fn saved(text: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = dir.path().join("b1x24ppwb.txt");
        std::fs::write(&path, text).expect("the temporary directory is writable");
        (dir, path)
    }

    #[test]
    fn a_saved_output_of_the_size_the_cli_reported_is_read_whole() {
        let (_dir, path) = saved("running 1 test ✓\n".as_bytes());

        assert_eq!(
            read(&path, "running 1 test ✓\n".len() as u64).as_deref(),
            Some("running 1 test ✓\n")
        );
    }

    #[test]
    fn a_saved_output_that_changed_size_is_not_read() {
        let (_dir, path) = saved(b"running 1 test\n");

        assert_eq!(read(&path, 14), None, "it has grown");
        assert_eq!(read(&path, 16), None, "it has shrunk");
    }

    #[test]
    fn a_missing_unreadable_or_oversized_output_is_not_read() {
        let (dir, path) = saved(&[0xff, 0xfe, b'\n']);

        assert_eq!(read(&path, 3), None, "it is not UTF-8");
        assert_eq!(read(&dir.path().join("gone.txt"), 3), None);
        assert_eq!(read(dir.path(), 3), None, "a directory is no output");
        assert_eq!(read(&path, LIMIT + 1), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_named_as_the_saved_output_is_refused_at_once() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let fifo = dir.path().join("b1x24ppwb.txt");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "the FIFO is made");

        let started = std::time::Instant::now();
        assert_eq!(read(&fifo, 100), None);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "waited {:?} on the FIFO",
            started.elapsed()
        );
    }

    #[test]
    fn a_directory_named_as_the_saved_output_is_refused() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        assert_eq!(read(dir.path(), 0), None);
    }
}
