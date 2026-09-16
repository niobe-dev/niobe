// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Where the session is running, and where its store lives.
//!
//! Read here rather than in the TUI, which has no business touching the
//! filesystem.

use std::path::{Path, PathBuf};

use niobe_store::Store;
use niobe_tui::app::Repo;

/// The directory in a repository that holds Niobe's own files.
const DIR: &str = ".niobe";

/// The session store's file name inside [`DIR`].
const STORE_FILE: &str = "sessions.db";

/// Written into a new [`DIR`], so the session store is not committed by
/// accident: it holds every prompt and every tool output of every session.
/// Only the store is ignored, so anything else kept there stays committable.
const GITIGNORE: &str = "\
# Written by niobe. The session store holds every prompt and tool output of
# every session recorded here, and is not meant to be committed.
sessions.db
sessions.db-*
";

/// The name and branch the shell shows.
pub fn describe(cwd: &Path) -> Repo {
    let name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| niobe_core::APP_NAME.to_owned());

    Repo {
        name,
        branch: git_branch(cwd),
    }
}

/// The directory sessions are recorded against: the nearest enclosing
/// repository, so a session started in a subdirectory is listed with the rest,
/// or `cwd` itself outside one.
pub fn root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

/// Where the config of the repository rooted at `root` is, whether or not it
/// exists. It sits beside the session store but is not ignored with it: a
/// repository's profiles are meant to be committed and shared.
pub fn config_path(root: &Path) -> PathBuf {
    root.join(DIR).join(niobe_config::FILE_NAME)
}

/// Where the session store of `root` is, whether or not it exists yet.
pub fn store_path(root: &Path) -> PathBuf {
    root.join(DIR).join(STORE_FILE)
}

/// Opens the session store of `root`, creating it — and the directory, with
/// its ignore file — if this is the first session recorded there.
pub fn open_or_create_store(root: &Path) -> Result<Store, String> {
    let dir = root.join(DIR);
    let path = store_path(root);
    let failed = |e: &dyn std::fmt::Display| {
        format!("cannot open the session store at {}: {e}", path.display())
    };

    std::fs::create_dir_all(&dir).map_err(|e| failed(&e))?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(&ignore, GITIGNORE).map_err(|e| failed(&e))?;
    }
    Store::open(&path).map_err(|e| failed(&e))
}

/// Opens the session store of `root` if one has been created, and never
/// creates one: listing or resuming sessions leaves a directory as it was.
pub fn open_existing_store(root: &Path) -> Result<Option<Store>, String> {
    let path = store_path(root);
    if !path.exists() {
        return Ok(None);
    }
    Store::open(&path)
        .map(Some)
        .map_err(|e| format!("cannot open the session store at {}: {e}", path.display()))
}

/// The checked-out branch, read from `.git/HEAD` in the nearest repository.
///
/// Parsed rather than shelled out to: `git` may not be installed, and a
/// subprocess for one line of a file is a subprocess the operator pays for.
fn git_branch(from: &Path) -> Option<String> {
    from.ancestors().find_map(|dir| {
        let contents = std::fs::read_to_string(dir.join(".git").join("HEAD")).ok()?;
        let head = contents.trim();
        Some(match head.strip_prefix("ref: refs/heads/") {
            Some(branch) => branch.to_owned(),
            // A detached HEAD is the commit itself, shortened the way git
            // shortens it.
            None => head.chars().take(7).collect(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_branch_is_read_from_the_repository_this_test_runs_in() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let branch = git_branch(here).expect("the workspace is a git repository");
        assert!(!branch.is_empty());
    }

    #[test]
    fn a_directory_outside_a_repository_has_no_branch() {
        assert_eq!(git_branch(Path::new("/")), None);
    }

    #[test]
    fn the_root_is_the_nearest_repository_or_the_directory_itself() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let nested = dir.path().join("a").join("b");
        std::fs::create_dir_all(&nested).expect("nested directories can be made");

        assert_eq!(root(&nested), nested);

        std::fs::create_dir(dir.path().join(".git")).expect("a .git directory can be made");
        assert_eq!(root(&nested), dir.path());
    }

    #[test]
    fn a_new_store_comes_with_an_ignore_file_for_the_store_alone() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");

        open_or_create_store(dir.path()).expect("the store is created");

        assert!(store_path(dir.path()).exists());
        let ignore = std::fs::read_to_string(dir.path().join(".niobe").join(".gitignore"))
            .expect("the ignore file was written");
        let patterns: Vec<&str> = ignore
            .lines()
            .filter(|line| !line.starts_with('#'))
            .collect();
        assert_eq!(patterns, ["sessions.db", "sessions.db-*"]);
    }

    #[test]
    fn an_ignore_file_the_operator_wrote_is_left_alone() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        std::fs::create_dir(dir.path().join(".niobe")).expect("the directory can be made");
        let ignore = dir.path().join(".niobe").join(".gitignore");
        std::fs::write(&ignore, "*\n").expect("the ignore file is written");

        open_or_create_store(dir.path()).expect("the store is created");

        assert_eq!(
            std::fs::read_to_string(&ignore).expect("the ignore file reads"),
            "*\n"
        );
    }

    #[test]
    fn looking_for_a_store_that_is_not_there_creates_nothing() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        assert!(
            open_existing_store(dir.path())
                .expect("absence is not an error")
                .is_none()
        );
        assert!(!dir.path().join(".niobe").exists());
    }
}
