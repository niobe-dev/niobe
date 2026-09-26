// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Writing a file Niobe did not create, without ever leaving half of it.
//!
//! A file truncated and written in place is empty for as long as the write
//! takes, and stays that way if the process dies inside it: the operator's
//! whole config, gone for one rule. So the new text is written to a file of
//! its own beside the old one, flushed to the disk, and renamed over it. A
//! rename within a directory is atomic, so a reader — or the next session
//! after a crash — finds either the old file or the new one, never a part.
//!
//! Two sessions in one repository that each read the file, add a rule and
//! write it back would still lose one of the rules to the other, so a writer
//! holds [`Lock`] across the whole read, change and replace. The lock is taken
//! on the directory rather than on the file, because the file it would be
//! taken on is exactly what the rename replaces: a second writer waiting on
//! the old file would wake up holding a lock on nothing anybody reads.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// Exclusive hold on the files of one directory, for as long as it lives.
///
/// Advisory: it keeps out another Niobe writing the same directory, not an
/// editor the operator has the file open in. It is released when it is
/// dropped, and by the kernel if the process dies holding it, so a crash
/// cannot leave a directory locked.
#[derive(Debug)]
pub(crate) struct Lock {
    _held: File,
}

impl Lock {
    /// Waits until no other writer holds `directory`, then holds it.
    pub(crate) fn directory(directory: &Path) -> io::Result<Self> {
        let directory = File::open(directory)?;
        exclusive(&directory)?;
        Ok(Self { _held: directory })
    }
}

#[cfg(unix)]
fn exclusive(directory: &File) -> io::Result<()> {
    rustix::fs::flock(directory, rustix::fs::FlockOperation::LockExclusive).map_err(io::Error::from)
}

/// Niobe runs on macOS and Linux; where there is no `flock` a writer goes
/// unlocked and a second session's rule can be lost, but the file itself is
/// still never left half written.
#[cfg(not(unix))]
fn exclusive(_directory: &File) -> io::Result<()> {
    Ok(())
}

/// The file `path` names once any symbolic link to it is followed, so that a
/// config kept in a dotfiles repository and linked into place is written
/// where it lives, and the link survives. A path to nothing yet is itself.
pub(crate) fn resolved(path: &Path) -> io::Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(real) => Ok(real),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(error) => Err(error),
    }
}

/// The directory `path` is in; `.` for a bare file name.
pub(crate) fn parent(path: &Path) -> &Path {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

/// Replaces the file at `path` with `bytes`, whole or not at all.
pub(crate) fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    replace_with(path, bytes, io::Write::write_all)
}

/// [`replace`], with the write itself handed in so that a test can make it
/// fail partway through, which is the case the whole module exists for.
pub(crate) fn replace_with(
    path: &Path,
    bytes: &[u8],
    write: impl FnOnce(&mut File, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let staged = staging(path)?;
    let written = stage(path, &staged, bytes, write).and_then(|()| std::fs::rename(&staged, path));
    if written.is_err() {
        // The staged file is ours and half of something; the error the
        // operator needs is the write's, not whether this cleanup worked.
        let _ = std::fs::remove_file(&staged);
    }
    written?;
    // The rename is only durable once the directory entry is. The file has
    // been replaced either way, so a directory that cannot be flushed does
    // not turn a write that happened into one reported as failed.
    let _ = File::open(parent(path)).and_then(|dir| dir.sync_all());
    Ok(())
}

/// Where the new text is written before it replaces `path`: beside it, so the
/// rename never crosses a file system, and hidden, so a crash between the two
/// leaves nothing in the operator's listing. The process id keeps two
/// processes apart; two writers in one process are kept apart by [`Lock`].
fn staging(path: &Path) -> io::Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "the path does not name a file")
    })?;
    let mut staged = std::ffi::OsString::from(".");
    staged.push(name);
    staged.push(format!(".{}.tmp", std::process::id()));
    Ok(parent(path).join(staged))
}

/// Writes `bytes` to `staged` and flushes it, with the permissions of the
/// file it will replace, so that a config the operator keeps private stays
/// private after Niobe has written it.
fn stage(
    path: &Path,
    staged: &Path,
    bytes: &[u8],
    write: impl FnOnce(&mut File, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let mut file = File::create(staged)?;
    match std::fs::metadata(path) {
        Ok(existing) => file.set_permissions(existing.permissions())?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    write(&mut file, bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("the directory lists")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_file_is_replaced_whole_and_nothing_is_left_beside_it() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "old").expect("written");

        replace(&file, b"new").expect("the file is replaced");

        assert_eq!(std::fs::read_to_string(&file).expect("read"), "new");
        assert_eq!(listing(dir.path()), ["config.toml"]);
    }

    #[test]
    fn a_file_that_is_not_there_yet_is_created() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let file = dir.path().join("config.toml");

        replace(&file, b"first").expect("the file is created");

        assert_eq!(std::fs::read_to_string(&file).expect("read"), "first");
    }

    #[cfg(unix)]
    #[test]
    fn a_private_file_stays_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("a temporary directory");
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "old").expect("written");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))
            .expect("the mode is set");

        replace(&file, b"new").expect("the file is replaced");

        let mode = std::fs::metadata(&file).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_file_is_written_where_it_lives_and_the_link_survives() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let real = dir.path().join("dotfiles-config.toml");
        let link = dir.path().join("config.toml");
        std::fs::write(&real, "old").expect("written");
        std::os::unix::fs::symlink(&real, &link).expect("the link is made");

        let target = resolved(&link).expect("the link resolves");
        replace(&target, b"new").expect("the file is replaced");

        assert!(
            std::fs::symlink_metadata(&link)
                .expect("stat")
                .file_type()
                .is_symlink(),
            "the link was replaced by a file"
        );
        assert_eq!(std::fs::read_to_string(&real).expect("read"), "new");
    }

    #[test]
    fn a_bare_file_name_is_in_the_current_directory() {
        assert_eq!(parent(Path::new("config.toml")), Path::new("."));
        assert_eq!(parent(Path::new("a/config.toml")), Path::new("a"));
    }
}
