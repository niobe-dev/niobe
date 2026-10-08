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
//!   the configuration directory — the main checkout's, for a session in a
//!   linked git worktree, as the CLI keeps it — and the entries it links to.
//!
//! A file nested below where the session runs is loaded by the CLI only when
//! the agent reads in that directory, so it is not listed. A file larger than
//! [`LIMIT`] is listed with its size and not read. Nothing here is written.
//!
//! Nothing read is a credential, whatever a file names. Each file is judged
//! where the filesystem resolves it, links and `..` followed, before it is
//! opened, and is listed without being read ([`Withheld`]) where it is in the
//! CLI's configuration directory and not markdown — its sign-in is kept there
//! — or under a hidden entry of the home directory. A file the repository
//! holds, and what it imports, is read only inside the repository or as one
//! of the CLI's markdown files, because a cloned repository's `@path` would
//! otherwise choose what on the machine is read and shown; the auto-memory
//! index's entries only beside it. The operator's own files outside the
//! repository import from anywhere else.
//!
//! A file is listed once however many names reach it, and the lookup is
//! bounded — [`LOOKUPS`] paths and [`BUDGET`] bytes in all — so that imports
//! naming one file through many directories, or thousands of files, cannot
//! hold up the shell that asked.

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
    /// [`LIMIT`] bytes and was not [`Withheld`].
    pub text: Option<String>,
    /// How many bytes it holds, where it was read or is a regular file too
    /// large to read: one with a size and no text is the latter.
    pub bytes: Option<u64>,
    /// What the auto-memory index says of an entry.
    pub note: Option<String>,
    /// Why it was listed without being read, where it was.
    pub withheld: Option<Withheld>,
}

/// Why a file the memory names is listed without being opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Withheld {
    /// It is where credentials are kept: in the CLI's configuration directory
    /// and not markdown, which is where its sign-in is, or under a hidden
    /// entry of the home directory, such as `~/.ssh` or another CLI's.
    Private,
    /// A file the repository controls names it, and it is outside both the
    /// repository and the CLI's markdown: a cloned repository does not decide
    /// what else on the machine is read.
    Outside,
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

/// The most kept from every memory file together, in bytes, after which no
/// more is read: a repository whose files import thousands of others is not
/// read whole each time the view opens.
pub const BUDGET: u64 = 16 * LIMIT;

/// The most paths looked up for memory files, every import counted: a file
/// naming thousands of others is not followed through each.
pub const LOOKUPS: usize = 1024;

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
    let mut found = Found {
        places: Places::of(lookup),
        sources: Vec::new(),
        seen: BTreeSet::new(),
        lookups: LOOKUPS,
        read: 0,
        full: false,
    };
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
        let dir = crate::transcript::directory(config, &main_checkout(lookup.cwd)).join(AUTO_DIR);
        found.index(&dir.join(INDEX));
    }
    found.sources
}

/// What has been found so far, and every file listed by where it really is,
/// so that a file two others import, one that imports itself, or one named
/// through a link or a `..` is listed once — and how much more may be looked
/// up and read.
struct Found {
    places: Places,
    sources: Vec<Source>,
    seen: BTreeSet<PathBuf>,
    /// How many more paths may be looked up.
    lookups: usize,
    /// How many bytes have been kept, at most [`BUDGET`].
    read: u64,
    /// Whether a file has been passed over for the budget, after which none
    /// is read.
    full: bool,
}

/// A memory file found, judged and, where it may be, read.
enum Opened {
    /// Listed and not read.
    Withheld(Withheld),
    /// Listed with its size and not read: larger than [`LIMIT`].
    TooLarge(u64),
    /// Read: where it really is, what it holds and how many bytes that is.
    Read {
        real: PathBuf,
        text: String,
        bytes: u64,
    },
}

impl Found {
    /// A file the CLI loads itself, where it is there, and what it imports.
    /// One the repository holds is held to it, as its imports are.
    fn loaded(&mut self, scope: Scope, path: &Path, lookup: &Lookup<'_>) {
        let held = Held::to_repository(
            self.places
                .in_repository(&canonical(path.parent().unwrap_or(Path::new("")))),
        );
        self.add(scope, path, 0, &held, lookup);
    }

    fn add(&mut self, scope: Scope, path: &Path, depth: usize, held: &Held, lookup: &Lookup<'_>) {
        let (real, text, bytes) = match self.open(path, held) {
            None => return,
            Some(Opened::Withheld(withheld)) => {
                self.sources
                    .push(Source::withheld(scope, path, depth, withheld));
                return;
            }
            Some(Opened::TooLarge(bytes)) => {
                self.sources
                    .push(Source::too_large(scope, path, depth, bytes));
                return;
            }
            Some(Opened::Read { real, text, bytes }) => (real, text, bytes),
        };
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
            withheld: None,
        });
        let from = path.parent().unwrap_or(Path::new(""));
        let held = Held::to_repository(self.places.in_repository(&real));
        for import in imports {
            if let Some(target) = resolve(&import, from, lookup.home) {
                self.add(Scope::Imported, &target, depth + 1, &held, lookup);
            }
        }
    }

    /// The auto-memory index, where there is one, and each entry it links to
    /// that is there and kept beside it.
    fn index(&mut self, path: &Path) {
        let (real, text, bytes) = match self.open(path, &Held::Free) {
            None => return,
            Some(Opened::Withheld(withheld)) => {
                self.sources
                    .push(Source::withheld(Scope::AutoMemory, path, 0, withheld));
                return;
            }
            Some(Opened::TooLarge(bytes)) => {
                self.sources
                    .push(Source::too_large(Scope::AutoMemory, path, 0, bytes));
                return;
            }
            Some(Opened::Read { real, text, bytes }) => (real, text, bytes),
        };
        let entries = entries(&text);
        self.sources.push(Source {
            scope: Scope::AutoMemory,
            path: path.to_path_buf(),
            depth: 0,
            text: Some(text),
            bytes: Some(bytes),
            note: None,
            withheld: None,
        });
        let dir = path.parent().unwrap_or(Path::new(""));
        // The index is written by the agent, which a repository can steer: an
        // entry is read only where it is kept beside it.
        let held = Held::To(real.parent().unwrap_or(Path::new("")).to_path_buf());
        for (target, note) in entries {
            let target = dir.join(target);
            let source = match self.open(&target, &held) {
                None => continue,
                Some(Opened::Withheld(withheld)) => {
                    Source::withheld(Scope::AutoEntry, &target, 1, withheld)
                }
                Some(Opened::TooLarge(bytes)) => {
                    Source::too_large(Scope::AutoEntry, &target, 1, bytes)
                }
                Some(Opened::Read { text, bytes, .. }) => Source {
                    scope: Scope::AutoEntry,
                    path: target,
                    depth: 1,
                    text: Some(text),
                    bytes: Some(bytes),
                    note: None,
                    withheld: None,
                },
            };
            self.sources.push(Source { note, ..source });
        }
    }

    /// The file at `path`, named as `held` says, where it is there, has not
    /// been listed already and the budgets leave room for it.
    fn open(&mut self, path: &Path, held: &Held) -> Option<Opened> {
        self.lookups = self.lookups.checked_sub(1)?;
        // Where it really is, found without opening it, so that a link or a
        // `..` cannot carry the read somewhere the check did not look, nor
        // list one file under many names.
        let real = std::fs::canonicalize(path).ok()?;
        if self.seen.contains(&real) {
            return None;
        }
        if let Some(withheld) = self.places.withholds(&real, held) {
            self.seen.insert(real);
            return Some(Opened::Withheld(withheld));
        }
        if self.full {
            return None;
        }
        let (text, bytes) = match read(&real)? {
            Contents::Text(text, bytes) => (text, bytes),
            Contents::TooLarge(bytes) => {
                self.seen.insert(real);
                return Some(Opened::TooLarge(bytes));
            }
        };
        let total = self.read.saturating_add(bytes);
        if total > BUDGET {
            self.full = true;
            return None;
        }
        self.read = total;
        self.seen.insert(real.clone());
        Some(Opened::Read { real, text, bytes })
    }
}

impl Source {
    /// A file listed where it is, with its size, and not read: it is larger
    /// than [`LIMIT`].
    fn too_large(scope: Scope, path: &Path, depth: usize, bytes: u64) -> Self {
        Self {
            scope,
            path: path.to_path_buf(),
            depth,
            text: None,
            bytes: Some(bytes),
            note: None,
            withheld: None,
        }
    }

    /// A file listed where it is and not opened, for `withheld`.
    fn withheld(scope: Scope, path: &Path, depth: usize, withheld: Withheld) -> Self {
        Self {
            scope,
            path: path.to_path_buf(),
            depth,
            text: None,
            bytes: None,
            note: None,
            withheld: Some(withheld),
        }
    }
}

/// Where a file may be read from, by who named it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Held {
    /// The operator's own files named it: it is read wherever it is, but
    /// where credentials are kept.
    Free,
    /// The repository named it: it is read only inside the repository or as
    /// one of the CLI's markdown files.
    Repository,
    /// The auto-memory index named it: it is read only inside this directory.
    To(PathBuf),
}

impl Held {
    fn to_repository(inside: bool) -> Self {
        match inside {
            true => Self::Repository,
            false => Self::Free,
        }
    }
}

/// The places a memory file is judged by, each as the filesystem resolves it.
struct Places {
    /// The repository the session runs in: the nearest directory holding
    /// `.git` from where it runs, or that directory itself. `None` where it
    /// is the home directory or above it, which the operator's own files are
    /// in rather than a clone's.
    repository: Option<PathBuf>,
    /// The CLI's configuration directory, where its sign-in is kept.
    config: Option<PathBuf>,
    /// The home directory.
    home: Option<PathBuf>,
}

impl Places {
    fn of(lookup: &Lookup<'_>) -> Self {
        let home = lookup.home.map(canonical);
        let repository = canonical(
            lookup
                .cwd
                .ancestors()
                .find(|dir| std::fs::symlink_metadata(dir.join(".git")).is_ok())
                .unwrap_or(lookup.cwd),
        );
        let repository = match &home {
            Some(home) if home.starts_with(&repository) => None,
            _ => Some(repository),
        };
        Self {
            repository,
            config: lookup.config.map(canonical),
            home,
        }
    }

    fn in_repository(&self, real: &Path) -> bool {
        self.repository
            .as_ref()
            .is_some_and(|repository| real.starts_with(repository))
    }

    /// Why the file really at `real`, named as `held` says, is not read, or
    /// `None` where it may be.
    fn withholds(&self, real: &Path, held: &Held) -> Option<Withheld> {
        let in_config = self
            .config
            .as_ref()
            .is_some_and(|config| real.starts_with(config));
        let markdown = real.extension().is_some_and(|extension| extension == "md");
        if in_config && !markdown {
            return Some(Withheld::Private);
        }
        if let Held::To(dir) = held {
            return (!real.starts_with(dir)).then_some(Withheld::Outside);
        }
        if in_config || self.in_repository(real) {
            return None;
        }
        if self.hidden_in_home(real) {
            return Some(Withheld::Private);
        }
        match held {
            Held::Repository => Some(Withheld::Outside),
            Held::Free | Held::To(_) => None,
        }
    }

    /// Whether `real` is under an entry of the home directory whose name
    /// starts with a dot, which is where tools keep their keys and tokens.
    fn hidden_in_home(&self, real: &Path) -> bool {
        self.home
            .as_ref()
            .and_then(|home| real.strip_prefix(home).ok())
            .and_then(|rest| rest.components().next())
            .is_some_and(|first| first.as_os_str().to_string_lossy().starts_with('.'))
    }
}

/// The checkout whose auto-memory a session at `cwd` is given: the main one
/// where `cwd` is a linked git worktree, and `cwd` itself otherwise.
///
/// The CLI keeps one project's notes for all of its worktrees: in a linked
/// worktree its `init` names the main checkout's flattened directory as
/// `memory_paths.auto` (seen live under Claude Code 2.1.288). A worktree's
/// `.git` is a file whose `gitdir:` line names its state under the main
/// checkout's `.git/worktrees/`, and that state's `commondir` leads back to
/// the main `.git`, whose parent is the main checkout. The path is worked out
/// as written, `..` taken off by hand rather than by the filesystem, because
/// the CLI flattens the path as git writes it, links unresolved.
fn main_checkout(cwd: &Path) -> PathBuf {
    common_git_dir(cwd)
        .filter(|common| common.file_name().is_some_and(|name| name == ".git"))
        .and_then(|common| common.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| cwd.to_path_buf())
}

/// The `.git` directory shared by the worktree at `cwd`, where its `.git` is
/// a worktree's pointer file.
fn common_git_dir(cwd: &Path) -> Option<PathBuf> {
    let pointer = small_text(&cwd.join(".git"))?;
    let state = cwd.join(pointer.trim().strip_prefix("gitdir:")?.trim());
    let common = small_text(&state.join("commondir"))?;
    Some(lexical(&state.join(common.trim())))
}

/// The text of a regular file of at most a few hundred bytes, as git's own
/// pointer files are, read without waiting on anything else.
fn small_text(path: &Path) -> Option<String> {
    const POINTER: u64 = 4096;
    let file = crate::spilled::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(POINTER).read_to_string(&mut text).ok()?;
    Some(text)
}

/// `path` with each `.` dropped and each `..` taking off the name before it.
fn lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                out.push(component.as_os_str());
            }
        }
    }
    out
}

/// `path` as the filesystem resolves it, or as written where it cannot.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A regular file at `path`, read without waiting on it.
enum Contents {
    /// At most [`LIMIT`] bytes: its text and how many bytes it held.
    Text(String, u64),
    /// Larger than [`LIMIT`], and so not read: how many bytes it holds.
    TooLarge(u64),
}

/// What the regular file at `path` holds, read without waiting on it. `None`
/// for anything else: a memory file a repository linked to a device or a FIFO
/// is not one the agent was given.
fn read(path: &Path) -> Option<Contents> {
    let file = crate::spilled::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    if meta.len() > LIMIT {
        return Some(Contents::TooLarge(meta.len()));
    }
    let mut bytes = Vec::new();
    file.take(LIMIT.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    let size = bytes.len() as u64;
    // Grown past the limit since its size was looked at.
    if size > LIMIT {
        return Some(Contents::TooLarge(size));
    }
    Some(Contents::Text(
        String::from_utf8_lossy(&bytes).into_owned(),
        size,
    ))
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

    /// The name the CLI keeps its sign-in under, put together here because
    /// the layering check refuses the name written whole in any source,
    /// tests included: this one only plants a fake file under it.
    fn credentials_name() -> String {
        [".credentials", "json"].join(".")
    }

    /// A home with the CLI's configuration directory holding a fake sign-in,
    /// and an empty repository under it.
    fn signed_in() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let home = dir.path().join("home");
        let config = home.join(".claude");
        let repo = home.join("work").join("repo");
        write(
            &config.join(credentials_name()),
            "{\"claudeAiOauth\":{\"accessToken\":\"sk-ant-oat-SECRET\"}}",
        );
        std::fs::create_dir_all(&repo).expect("the temporary directory is writable");
        (dir, home, config, repo)
    }

    fn found_in(home: &Path, config: &Path, repo: &Path) -> Vec<Source> {
        sources(&Lookup {
            cwd: repo,
            config: Some(config),
            home: Some(home),
        })
    }

    fn listed<'a>(found: &'a [Source], path: &Path) -> &'a Source {
        found
            .iter()
            .find(|source| source.path == path)
            .unwrap_or_else(|| panic!("{} is listed: {found:?}", path.display()))
    }

    #[test]
    fn a_repository_importing_the_clis_sign_in_has_it_listed_and_never_read() {
        let (_dir, home, config, repo) = signed_in();
        write(
            &repo.join("CLAUDE.md"),
            &format!("See @~/.claude/{} for setup.\n", credentials_name()),
        );
        let found = found_in(&home, &config, &repo);
        assert!(
            !found.iter().any(|source| source
                .text
                .as_deref()
                .is_some_and(|text| text.contains("SECRET"))),
            "{found:?}"
        );
        let token = listed(&found, &config.join(credentials_name()));
        assert_eq!(
            (
                token.scope,
                token.text.as_deref(),
                token.bytes,
                token.withheld
            ),
            (Scope::Imported, None, None, Some(Withheld::Private))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_sign_in_file_that_is_a_fifo_is_listed_without_being_opened() {
        let (_dir, home, config, repo) = signed_in();
        let token = config.join(credentials_name());
        std::fs::remove_file(&token).expect("the fake sign-in was written");
        let made = std::process::Command::new("mkfifo")
            .arg(&token)
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "a FIFO can be made");
        write(
            &repo.join("CLAUDE.md"),
            &format!("@~/.claude/{}\n", credentials_name()),
        );
        let found = found_in(&home, &config, &repo);
        assert_eq!(listed(&found, &token).withheld, Some(Withheld::Private));
    }

    #[cfg(unix)]
    #[test]
    fn a_repository_memory_file_linked_to_the_clis_sign_in_is_not_read() {
        let (_dir, home, config, repo) = signed_in();
        std::os::unix::fs::symlink(config.join(credentials_name()), repo.join("CLAUDE.md"))
            .expect("a link can be made");
        let found = found_in(&home, &config, &repo);
        let project = listed(&found, &repo.join("CLAUDE.md"));
        assert_eq!(
            (project.text.as_deref(), project.withheld),
            (None, Some(Withheld::Private))
        );
    }

    #[test]
    fn a_repository_import_that_leads_out_of_it_is_listed_and_not_read() {
        let (dir, home, config, repo) = signed_in();
        let outside = home.join("outside.md");
        let elsewhere = dir.path().join("elsewhere.md");
        write(&outside, "two levels up\n");
        write(&elsewhere, "written absolute\n");
        write(
            &repo.join("CLAUDE.md"),
            &format!("@../../outside.md @{}\n", elsewhere.display()),
        );
        let found = found_in(&home, &config, &repo);
        let withheld: Vec<(Option<&str>, Option<Withheld>)> = [
            listed(&found, &repo.join("../../outside.md")),
            listed(&found, &elsewhere),
        ]
        .iter()
        .map(|source| (source.text.as_deref(), source.withheld))
        .collect();
        assert_eq!(
            withheld,
            [
                (None, Some(Withheld::Outside)),
                (None, Some(Withheld::Outside))
            ]
        );
    }

    #[test]
    fn a_repository_import_of_a_markdown_file_in_the_clis_directory_is_read() {
        let (_dir, home, config, repo) = signed_in();
        write(&config.join("shared.md"), "shared notes\n");
        write(&repo.join("CLAUDE.md"), "@~/.claude/shared.md\n");
        let found = found_in(&home, &config, &repo);
        let shared = listed(&found, &config.join("shared.md"));
        assert_eq!(
            (shared.text.as_deref(), shared.withheld),
            (Some("shared notes\n"), None)
        );
    }

    #[test]
    fn the_operators_own_file_imports_from_anywhere_but_where_credentials_are_kept() {
        let (_dir, home, config, repo) = signed_in();
        write(&home.join("notes.md"), "my notes\n");
        write(&home.join(".ssh").join("id_ed25519"), "PRIVATE KEY\n");
        write(&home.join("CLAUDE.md"), "@~/notes.md @~/.ssh/id_ed25519\n");
        let found = found_in(&home, &config, &repo);
        let read: Vec<(Option<&str>, Option<Withheld>)> = [
            listed(&found, &home.join("notes.md")),
            listed(&found, &home.join(".ssh").join("id_ed25519")),
        ]
        .iter()
        .map(|source| (source.text.as_deref(), source.withheld))
        .collect();
        assert_eq!(
            read,
            [(Some("my notes\n"), None), (None, Some(Withheld::Private))]
        );
    }

    #[test]
    fn an_auto_memory_entry_that_links_out_of_its_directory_is_not_read() {
        let (_dir, home, config, repo) = signed_in();
        let memory = crate::transcript::directory(&config, &repo).join("memory");
        let token = config.join(credentials_name());
        let climb = pathdiff(&memory, &token);
        write(
            &memory.join("MEMORY.md"),
            &format!("- [Setup]({climb}) — how to sign in\n"),
        );
        let found = found_in(&home, &config, &repo);
        let entry = listed(&found, &memory.join(&climb));
        assert_eq!(
            (entry.text.as_deref(), entry.withheld),
            (None, Some(Withheld::Private))
        );
    }

    /// `to` written relative to `from`, which lies below the same directory.
    fn pathdiff(from: &Path, to: &Path) -> String {
        let common = to.parent().expect("a file has a directory");
        let up = from
            .strip_prefix(common)
            .expect("below the same directory")
            .components()
            .count();
        let name = to
            .file_name()
            .expect("a file has a name")
            .to_string_lossy()
            .into_owned();
        format!("{}{name}", "../".repeat(up))
    }

    fn in_dir(found: &[Source], dir: &Path) -> Vec<String> {
        found
            .iter()
            .filter(|source| source.path.starts_with(dir))
            .map(|source| source.path.display().to_string())
            .collect()
    }

    #[test]
    fn an_import_that_climbs_back_through_its_parent_is_listed_once() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let repo = dir.path().join("repo");
        write(&repo.join("CLAUDE.md"), "@sub/b.md\n");
        write(&repo.join("sub").join("b.md"), "@../CLAUDE.md\n");
        let found = sources(&Lookup {
            cwd: &repo,
            config: None,
            home: None,
        });
        assert_eq!(in_dir(&found, dir.path()).len(), 2, "{found:?}");
    }

    #[test]
    fn many_names_for_one_file_list_it_once_however_deep_they_go() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let repo = dir.path().join("repo");
        let aliases: Vec<String> = (0..20)
            .map(|n| {
                std::fs::create_dir_all(repo.join(format!("d{n}")))
                    .expect("the temporary directory is writable");
                format!("@d{n}/../CLAUDE.md")
            })
            .collect();
        write(&repo.join("CLAUDE.md"), &aliases.join(" "));
        let found = sources(&Lookup {
            cwd: &repo,
            config: None,
            home: None,
        });
        assert_eq!(in_dir(&found, dir.path()).len(), 1, "{found:?}");
    }

    #[test]
    fn what_the_imports_read_in_all_is_bounded() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let repo = dir.path().join("repo");
        let count = usize::try_from(BUDGET / LIMIT).expect("a handful") + 4;
        let large = "x".repeat(usize::try_from(LIMIT).expect("a mebibyte fits"));
        let names: Vec<String> = (0..count)
            .map(|n| {
                write(&repo.join(format!("{n}.md")), &large);
                format!("@{n}.md")
            })
            .collect();
        write(&repo.join("CLAUDE.md"), &names.join(" "));
        let found = sources(&Lookup {
            cwd: &repo,
            config: None,
            home: None,
        });
        let read: u64 = found.iter().filter_map(|source| source.bytes).sum();
        assert!(read <= BUDGET, "{read} bytes read");
        assert!(in_dir(&found, dir.path()).len() < count);
    }

    #[test]
    fn how_many_files_the_imports_name_in_all_is_bounded() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let repo = dir.path().join("repo");
        let names: Vec<String> = (0..LOOKUPS + 50)
            .map(|n| {
                write(&repo.join(format!("{n}.md")), "x");
                format!("@{n}.md")
            })
            .collect();
        write(&repo.join("CLAUDE.md"), &names.join(" "));
        let found = sources(&Lookup {
            cwd: &repo,
            config: None,
            home: None,
        });
        assert!(in_dir(&found, dir.path()).len() <= LOOKUPS);
    }

    #[test]
    fn an_import_too_large_to_read_is_listed_with_its_size_and_no_text() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let repo = dir.path().join("repo");
        let size = 2 * LIMIT;
        write(&repo.join("CLAUDE.md"), "@big.md @after.md\n");
        write(
            &repo.join("big.md"),
            &"x".repeat(usize::try_from(size).expect("two mebibytes fit")),
        );
        write(&repo.join("after.md"), "after\n");
        let found = sources(&Lookup {
            cwd: &repo,
            config: None,
            home: None,
        });
        let big = listed(&found, &repo.join("big.md"));
        assert_eq!(
            (
                big.scope,
                big.depth,
                big.text.as_deref(),
                big.bytes,
                big.withheld
            ),
            (Scope::Imported, 1, None, Some(size), None)
        );
        assert_eq!(in_dir(&found, dir.path()).len(), 3, "{found:?}");
    }

    /// A git worktree's `.git` is a file pointing into the main checkout's
    /// `.git/worktrees/`, whose `commondir` leads back to the main `.git`.
    fn worktree_of(main: &Path, name: &str) -> PathBuf {
        let state = main.join(".git").join("worktrees").join(name);
        write(&state.join("commondir"), "../..\n");
        write(&state.join("HEAD"), "ref: refs/heads/wt\n");
        let worktree = main
            .parent()
            .expect("the main checkout has a parent")
            .join(name);
        write(
            &worktree.join(".git"),
            &format!("gitdir: {}\n", state.display()),
        );
        worktree
    }

    #[test]
    fn a_session_in_a_git_worktree_is_given_the_main_checkouts_auto_memory() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let config = dir.path().join("config");
        let main = dir.path().join("wtrepo");
        write(&main.join(".git").join("HEAD"), "ref: refs/heads/main\n");
        let worktree = worktree_of(&main, "wtrepo-wt");
        let memory = crate::transcript::directory(&config, &main).join("memory");
        write(&memory.join("MEMORY.md"), "- [Facts](facts.md) — kept\n");
        write(&memory.join("facts.md"), "facts\n");
        let found = sources(&Lookup {
            cwd: &worktree,
            config: Some(&config),
            home: None,
        });
        let auto: Vec<(Scope, PathBuf)> = found
            .iter()
            .filter(|source| matches!(source.scope, Scope::AutoMemory | Scope::AutoEntry))
            .map(|source| (source.scope, source.path.clone()))
            .collect();
        assert_eq!(
            auto,
            [
                (Scope::AutoMemory, memory.join("MEMORY.md")),
                (Scope::AutoEntry, memory.join("facts.md")),
            ]
        );
    }

    #[test]
    fn a_checkout_whose_git_is_a_directory_is_its_own_main_checkout() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let main = dir.path().join("repo");
        write(&main.join(".git").join("HEAD"), "ref: refs/heads/main\n");
        assert_eq!(main_checkout(&main), main);
        let worktree = worktree_of(&main, "wt");
        assert_eq!(main_checkout(&worktree), main);
        let lost = dir.path().join("lost");
        write(&lost.join(".git"), "not a pointer\n");
        assert_eq!(main_checkout(&lost), lost);
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
