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
    let file = std::fs::File::open(path).ok()?;
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
}
