// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The files the `claude` CLI gives the agent as its memory, found the way
//! the CLI finds them.
//!
//! The CLI does not say which files it loaded. Its `init` names only the
//! directory of its own auto-memory (`memory_paths.auto`, recorded from Claude
//! Code 2.1.288), its answer to `initialize` names none, and `/context` is a
//! request of its own. So the lookup is mirrored here, from the CLI's
//! documented rules, in the order the agent is given the files:
//!
//! * the machine's managed policy file, where there is one;
//! * the user's `CLAUDE.md` in the CLI's configuration directory;
//! * every directory from the filesystem's root down to where the session
//!   runs: its `CLAUDE.md`, its `.claude/CLAUDE.md` and its `CLAUDE.local.md`
//!   — the last of those directories is the project, the ones above it its
//!   parents;
//! * under each of those, the files it imports with `@path`, relative to the
//!   importing file, from `~` or absolute, at most [`IMPORT_DEPTH`] deep, and
//!   not inside a code span or a fenced block;
//! * the CLI's auto-memory index, `MEMORY.md` in the project's directory under
//!   the configuration directory, and the entries it links to.
//!
//! A file nested below where the session runs is loaded by the CLI only when
//! the agent reads in that directory, so it is not listed. Nothing here is
//! written, and nothing read is a credential: these are the files the operator
//! wrote for the agent, and the notes the CLI keeps beside its transcripts.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Where a file the agent is given comes from, which decides when the CLI
/// loads it and who wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The machine's policy file, which an administrator sets.
    Managed,
    /// The user's own, in the CLI's configuration directory: every session.
    User,
    /// A directory above where the session runs.
    Parent,
    /// The directory the session runs in.
    Project,
    /// The project's `CLAUDE.local.md`, which is not meant to be committed.
    Local,
    /// A file another one imports with `@path`; it is listed under it.
    Imported,
    /// The index of the CLI's auto-memory, which the agent is given.
    AutoMemory,
    /// A note the auto-memory index links to. The agent is given the index's
    /// line about it and reads the note itself when it needs it.
    AutoEntry,
}

/// One file the agent is given or can reach from its memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Where it comes from.
    pub scope: Scope,
    /// Where it is.
    pub path: PathBuf,
    /// How many imports deep it is: nothing for a file the CLI loads itself,
    /// one for a file one of those imports, and so on.
    pub depth: usize,
    /// What it holds, where it could be read as a regular file of at most
    /// [`LIMIT`] bytes.
    pub text: Option<String>,
    /// How many bytes it holds, where it could be read.
    pub bytes: Option<u64>,
    /// What the auto-memory index says of an entry.
    pub note: Option<String>,
}

/// Where to look.
#[derive(Debug, Clone, Copy)]
pub struct Lookup<'a> {
    /// Where the session runs.
    pub cwd: &'a Path,
    /// The CLI's configuration directory, where there is one: see
    /// [`crate::transcript::config_dir`].
    pub config: Option<&'a Path>,
    /// The home directory, which an import from `~` is relative to.
    pub home: Option<&'a Path>,
}

/// How many imports deep the CLI follows, as its documentation gives it.
pub const IMPORT_DEPTH: usize = 5;

/// The largest memory file read, in bytes. A file the agent is given whole on
/// every request that is larger than this is not one anyone wrote for it.
pub const LIMIT: u64 = 1024 * 1024;

/// What the memory files are called, and the local one beside them.
const MEMORY: &str = "CLAUDE.md";
const LOCAL: &str = "CLAUDE.local.md";

/// The auto-memory index's name, and the directory it is kept in under the
/// project's directory in the CLI's configuration directory.
const INDEX: &str = "MEMORY.md";
const AUTO_DIR: &str = "memory";

/// Where the machine's managed policy file is, by the CLI's documentation.
#[cfg(target_os = "macos")]
const MANAGED: Option<&str> = Some("/Library/Application Support/ClaudeCode/CLAUDE.md");
#[cfg(all(unix, not(target_os = "macos")))]
const MANAGED: Option<&str> = Some("/etc/claude-code/CLAUDE.md");
#[cfg(not(unix))]
const MANAGED: Option<&str> = None;

/// Every file the agent is given as memory for a session at `lookup.cwd`, in
/// the order the CLI gives them, each import under the file importing it.
pub fn sources(lookup: &Lookup<'_>) -> Vec<Source> {
    let mut found = Found::default();
    if let Some(managed) = MANAGED {
        found.loaded(Scope::Managed, Path::new(managed), lookup);
    }
    if let Some(config) = lookup.config {
        found.loaded(Scope::User, &config.join(MEMORY), lookup);
    }
    let above: Vec<&Path> = lookup.cwd.ancestors().collect();
    for (i, dir) in above.iter().rev().enumerate() {
        let scope = match i + 1 == above.len() {
            true => Scope::Project,
            false => Scope::Parent,
        };
        found.loaded(scope, &dir.join(MEMORY), lookup);
        found.loaded(scope, &dir.join(".claude").join(MEMORY), lookup);
        found.loaded(Scope::Local, &dir.join(LOCAL), lookup);
    }
    if let Some(config) = lookup.config {
        let dir = crate::transcript::directory(config, lookup.cwd).join(AUTO_DIR);
        found.index(&dir.join(INDEX));
    }
    found.sources
}

/// What has been found so far, and every path listed, so that a file two
/// others import, or one that imports itself, is listed once.
#[derive(Default)]
struct Found {
    sources: Vec<Source>,
    seen: BTreeSet<PathBuf>,
}

impl Found {
    /// A file the CLI loads itself, where it is there, and what it imports.
    fn loaded(&mut self, scope: Scope, path: &Path, lookup: &Lookup<'_>) {
        self.add(scope, path, 0, lookup);
    }

    fn add(&mut self, scope: Scope, path: &Path, depth: usize, lookup: &Lookup<'_>) {
        if self.seen.contains(path) {
            return;
        }
        let Some((text, bytes)) = read(path) else {
            return;
        };
        self.seen.insert(path.to_path_buf());
        let imports = match depth < IMPORT_DEPTH {
            true => imports(&text),
            false => Vec::new(),
        };
        self.sources.push(Source {
            scope,
            path: path.to_path_buf(),
            depth,
            text: Some(text),
            bytes: Some(bytes),
            note: None,
        });
        let from = path.parent().unwrap_or(Path::new(""));
        for import in imports {
            if let Some(target) = resolve(&import, from, lookup.home) {
                self.add(Scope::Imported, &target, depth + 1, lookup);
            }
        }
    }

    /// The auto-memory index, where there is one, and each entry it links to
    /// that is there.
    fn index(&mut self, path: &Path) {
        let Some((text, bytes)) = read(path) else {
            return;
        };
        self.seen.insert(path.to_path_buf());
        let entries = entries(&text);
        self.sources.push(Source {
            scope: Scope::AutoMemory,
            path: path.to_path_buf(),
            depth: 0,
            text: Some(text),
            bytes: Some(bytes),
            note: None,
        });
        let dir = path.parent().unwrap_or(Path::new(""));
        for (target, note) in entries {
            let target = dir.join(target);
            if self.seen.contains(&target) {
                continue;
            }
            let Some((text, bytes)) = read(&target) else {
                continue;
            };
            self.seen.insert(target.clone());
            self.sources.push(Source {
                scope: Scope::AutoEntry,
                path: target,
                depth: 1,
                text: Some(text),
                bytes: Some(bytes),
                note,
            });
        }
    }
}

/// The text of a regular file at `path` of at most [`LIMIT`] bytes, and how
/// many bytes it held, read without waiting on it. `None` for anything else:
/// a memory file a repository linked to a device or a FIFO is not one the
/// agent was given.
fn read(path: &Path) -> Option<(String, u64)> {
    let file = crate::spilled::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(LIMIT.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    let size = bytes.len() as u64;
    if size > LIMIT {
        return None;
    }
    Some((String::from_utf8_lossy(&bytes).into_owned(), size))
}

/// Every `@path` in `text` that is an import: a word that starts with `@`,
/// outside a fenced block and a code span.
///
/// An address such as `someone@example.com` is not one, because its `@` does
/// not start the word; a word that names no file is passed over when it is
/// resolved.
fn imports(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        // Every other run between backticks is a code span.
        for (i, outside) in line.split('`').enumerate() {
            if i % 2 == 1 {
                continue;
            }
            found.extend(
                outside
                    .split_whitespace()
                    .filter_map(|word| word.strip_prefix('@'))
                    .map(|path| path.trim_end_matches([',', ';', ':', '!', '?', ')', '.']))
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned),
            );
        }
    }
    found
}

/// Where an import of `path` from a file in `from` points: from the home
/// directory for `~/`, as written where absolute, and from the importing
/// file's directory otherwise.
fn resolve(path: &str, from: &Path, home: Option<&Path>) -> Option<PathBuf> {
    match path.strip_prefix("~/") {
        Some(rest) => Some(home?.join(rest)),
        None => Some(from.join(path)),
    }
}

/// The entries the auto-memory index links to, and what it says of each: a
/// markdown link to a file beside it, `[title](file.md)`, and the words after
/// it on the line.
fn entries(index: &str) -> Vec<(String, Option<String>)> {
    index
        .lines()
        .filter_map(|line| {
            let open = line.find("](")?;
            let rest = &line[open + 2..];
            let close = rest.find(')')?;
            let target = rest[..close].trim();
            if target.is_empty() || target.contains("://") {
                return None;
            }
            let note = rest[close + 1..]
                .trim()
                .trim_start_matches(['—', '–', '-', ':'])
                .trim();
            Some((
                target.to_owned(),
                Some(note.to_owned()).filter(|note| !note.is_empty()),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        reason = "test helpers: a failed expectation is the test failing"
    )]

    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("a file has a directory"))
            .expect("the temporary directory is writable");
        std::fs::write(path, text).expect("the temporary directory is writable");
    }

    /// A machine laid out as the one this was written on: a repository whose
    /// `CLAUDE.md` only imports `AGENTS.md`, a `CLAUDE.md` in the home
    /// directory above it, a user `CLAUDE.md` importing a second file, and an
    /// auto-memory index with two entries.
    fn machine() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let home = dir.path().join("home");
        let config = home.join(".claude");
        let repo = home.join("work").join("repo");
        write(&repo.join("CLAUDE.md"), "@AGENTS.md\n");
        write(&repo.join("AGENTS.md"), "# Agents\n\nRun the gate.\n");
        write(&home.join("CLAUDE.md"), "Use the graph first.\n");
        write(
            &config.join("CLAUDE.md"),
            "@RTK.md\nMail someone@example.com, not `@ignored.md`.\n",
        );
        write(&config.join("RTK.md"), "rtk notes\n");
        let memory = crate::transcript::directory(&config, &repo).join("memory");
        write(
            &memory.join("MEMORY.md"),
            "- [Priorities](priorities.md) — what matters first\n\
             - [Facts](facts.md) — verified CLI facts\n",
        );
        write(&memory.join("priorities.md"), "awareness first\n");
        write(&memory.join("facts.md"), "init repeats every turn\n");
        (dir, home, config, repo)
    }

    #[test]
    fn every_file_the_agent_is_given_is_listed_in_order_with_its_imports_under_it() {
        let (dir, home, config, repo) = machine();
        let found = sources(&Lookup {
            cwd: &repo,
            config: Some(&config),
            home: Some(&home),
        });
        // Only what this machine holds: the real root may hold a policy file.
        let listed: Vec<(Scope, String, usize)> = found
            .iter()
            .filter_map(|source| {
                let path = source.path.strip_prefix(dir.path()).ok()?;
                Some((source.scope, path.display().to_string(), source.depth))
            })
            .collect();
        let memory = crate::transcript::directory(&config, &repo)
            .join("memory")
            .strip_prefix(dir.path())
            .expect("under the machine")
            .display()
            .to_string();

        assert_eq!(
            listed,
            [
                (Scope::User, "home/.claude/CLAUDE.md".to_owned(), 0),
                (Scope::Imported, "home/.claude/RTK.md".to_owned(), 1),
                (Scope::Parent, "home/CLAUDE.md".to_owned(), 0),
                (Scope::Project, "home/work/repo/CLAUDE.md".to_owned(), 0),
                (Scope::Imported, "home/work/repo/AGENTS.md".to_owned(), 1),
                (Scope::AutoMemory, format!("{memory}/MEMORY.md"), 0),
                (Scope::AutoEntry, format!("{memory}/priorities.md"), 1),
                (Scope::AutoEntry, format!("{memory}/facts.md"), 1),
            ]
        );
        let agents = found
            .iter()
            .find(|source| source.path.ends_with("AGENTS.md"))
            .expect("AGENTS.md is listed");
        assert_eq!(agents.text.as_deref(), Some("# Agents\n\nRun the gate.\n"));
        let notes: Vec<Option<&str>> = found
            .iter()
            .filter(|source| source.scope == Scope::AutoEntry)
            .map(|source| source.note.as_deref())
            .collect();
        assert_eq!(
            notes,
            [Some("what matters first"), Some("verified CLI facts")]
        );
    }

    #[test]
    fn an_import_that_comes_back_around_is_listed_once() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let repo = dir.path().join("repo");
        write(&repo.join("CLAUDE.md"), "@a.md\n");
        write(&repo.join("a.md"), "@b.md\n");
        write(&repo.join("b.md"), "@a.md @CLAUDE.md\n");
        let found = sources(&Lookup {
            cwd: &repo,
            config: None,
            home: None,
        });
        let names: Vec<String> = found
            .iter()
            .filter(|source| source.path.starts_with(dir.path()))
            .map(|source| {
                source
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(names, ["CLAUDE.md", "a.md", "b.md"]);
    }

    #[test]
    fn an_import_inside_code_or_in_an_address_is_not_one() {
        assert_eq!(
            imports("see @one.md, and `@two.md`\n```\n@three.md\n```\nme@four.md @~/five.md\n"),
            ["one.md", "~/five.md"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_memory_file_that_is_a_fifo_is_passed_over_rather_than_waited_on() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("the temporary directory is writable");
        let made = std::process::Command::new("mkfifo")
            .arg(repo.join("CLAUDE.md"))
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "a FIFO can be made");
        let found = sources(&Lookup {
            cwd: &repo,
            config: None,
            home: None,
        });
        assert!(
            !found
                .iter()
                .any(|source| source.path.starts_with(dir.path())),
            "{found:?}"
        );
    }
}
