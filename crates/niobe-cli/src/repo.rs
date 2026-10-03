// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Where the session is running, and where its store lives.
//!
//! Read here rather than in the TUI, which has no business touching the
//! filesystem.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant, SystemTime};

use niobe_store::Store;
use niobe_tui::app::{Repo, WorkingFile};
use niobe_tui::watch::Watch;

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
        ..Repo::default()
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

    store_stays_inside(root)?;
    private_dir(&dir).map_err(|e| failed(&e))?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(&ignore, GITIGNORE).map_err(|e| failed(&e))?;
    }
    private_file(&path).map_err(|e| failed(&e))?;
    let store = Store::open(&path).map_err(|e| failed(&e))?;
    keep_side_files_private(root);
    Ok(store)
}

/// The mode the session store and its side files are kept in: every prompt
/// and every tool output of every session is in them, including whatever a
/// tool read — an `.env`, a key — so they are the operator's alone, as the
/// CLI keeps its own transcripts.
#[cfg(unix)]
const PRIVATE_FILE: u32 = 0o600;

/// The mode of the directory the store is in, for the same reason.
#[cfg(unix)]
const PRIVATE_DIR: u32 = 0o700;

/// Makes `dir` if it is not there, readable by the operator alone, and takes
/// the other users' access away from one that is, where the mount lets it.
///
/// The config and the ignore file in it keep the mode any file would have:
/// they are meant to be committed, and a directory's own mode is not.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(PRIVATE_DIR)
            .create(dir)?;
        let _ = tighten(dir, PRIVATE_DIR);
        Ok(())
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)
}

/// Creates the store's file with a private mode before SQLite opens it, since
/// SQLite gives the side files it makes the mode of the store, and takes the
/// other users' access away from a store made before this was.
fn private_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Created only where there is none: opening a store that is there to
        // write it would turn SQLite's own report of a read-only store into
        // a bare refusal.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(PRIVATE_FILE)
            .open(path)
        {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // On a mount that refuses the change the store is still
                // opened, as it is read, and SQLite says what it can do.
                let _ = tighten(path, PRIVATE_FILE);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Takes away from `path` every permission `private` does not grant, leaving
/// the rest of its mode as it was.
#[cfg(unix)]
fn tighten(path: &Path, private: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode();
    if mode & 0o777 & !private == 0 {
        return Ok(());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & private))
}

/// Side files a store made before its mode was tightened keep the mode they
/// were made with until SQLite next deletes them; they are tightened here.
///
/// A side file that cannot be tightened — one another account owns, say —
/// does not stop the session: the store itself already is private, and the
/// session is not the place to settle who owns what beside it.
fn keep_side_files_private(root: &Path) {
    #[cfg(unix)]
    for suffix in STORE_SIDE_FILES {
        let mut name = store_path(root).into_os_string();
        name.push(suffix);
        let path = PathBuf::from(name);
        if path.exists() {
            let _ = tighten(&path, PRIVATE_FILE);
        }
    }
    #[cfg(not(unix))]
    let _ = root;
}

/// Opens the session store of `root` if one has been created, and never
/// creates one: listing or resuming sessions leaves a directory as it was.
pub fn open_existing_store(root: &Path) -> Result<Option<Store>, String> {
    let path = store_path(root);
    store_stays_inside(root)?;
    if !path.exists() {
        return Ok(None);
    }
    // A store made before its mode was kept private is made so here too; one
    // that cannot be — on a mount that refuses the change — is still opened.
    #[cfg(unix)]
    {
        let _ = tighten(&root.join(DIR), PRIVATE_DIR);
        let _ = tighten(&path, PRIVATE_FILE);
    }
    let store = Store::open(&path)
        .map_err(|e| format!("cannot open the session store at {}: {e}", path.display()))?;
    keep_side_files_private(root);
    Ok(Some(store))
}

/// The files beside the store that SQLite creates and writes at the store's
/// path plus a suffix: its write-ahead log, its shared-memory index and its
/// rollback journal.
const STORE_SIDE_FILES: [&str; 3] = ["-wal", "-shm", "-journal"];

/// Refuses a session store of the repository rooted at `root` that would be
/// written through a link to somewhere else.
///
/// Niobe creates `.niobe`, its ignore file and the store itself, and writes
/// every prompt and tool output of a session into the store. A cloned
/// repository chooses its links, so one committed at any of those names would
/// choose where that goes — a tracked file the next push publishes, or a file
/// of the operator's own outside the checkout. A link is followed only where
/// it leads to something that exists inside the repository; one that leads
/// nowhere is refused too, because writing through it creates a file where
/// the link says.
pub fn store_stays_inside(root: &Path) -> Result<(), String> {
    let dir = root.join(DIR);
    let store = store_path(root);
    let mut paths = vec![dir.clone(), dir.join(".gitignore"), store.clone()];
    paths.extend(STORE_SIDE_FILES.iter().map(|suffix| {
        let mut name = store.clone().into_os_string();
        name.push(suffix);
        PathBuf::from(name)
    }));
    let real_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    paths
        .iter()
        .try_for_each(|path| link_stays_inside(&real_root, path))
}

/// Whether `path`, if it is a link, leads to something that exists under
/// `real_root`, which is already canonical.
fn link_stays_inside(real_root: &Path, path: &Path) -> Result<(), String> {
    let is_link = std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink());
    if !is_link {
        return Ok(());
    }
    match std::fs::canonicalize(path) {
        Ok(target) if target.starts_with(real_root) => Ok(()),
        Ok(target) => Err(format!(
            "{} links to {}, outside the repository; niobe keeps its session store only \
             inside it",
            path.display(),
            target.display()
        )),
        Err(_) => Err(format!(
            "{} is a link to nothing; niobe does not create its session store through a \
             repository's links",
            path.display()
        )),
    }
}

/// Refuses a file of the repository rooted at `root` that is a link to one
/// outside it, whether the file is the link or a directory above it is.
///
/// A cloned repository chooses its links, so one there would choose which of
/// the operator's own files is read — and quoted back, in the error that says
/// it is not a config. A path that is not there, a link that leads nowhere
/// and one to something that is not a regular file — which is never read —
/// are left to the read that follows to report.
pub fn inside(root: &Path, path: &Path) -> Result<(), String> {
    let Ok(target) = std::fs::canonicalize(path) else {
        return Ok(());
    };
    if !target.is_file() {
        return Ok(());
    }
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    match target.starts_with(&root) {
        true => Ok(()),
        false => Err(format!(
            "{} links to {}, outside the repository; niobe reads a repository's config \
             only from inside it",
            path.display(),
            target.display()
        )),
    }
}

/// Refuses to write the file `path` of the repository rooted at `root` where
/// the write would land outside it: the file is a link to one outside, or
/// the directory it would be created in is.
///
/// [`inside`] is checked when a session starts; this is checked at each
/// write, because a link can be planted while the session runs — by a shell
/// command the operator allowed, or a checkout — and a rule written through
/// it would land in a file of the operator's own, where it holds in every
/// repository and no trust gates it. The deepest part of `path` that exists
/// is what is resolved, since a file not there yet is created in it.
pub fn writes_inside(root: &Path, path: &Path) -> Result<(), String> {
    let Some(existing) = path.ancestors().find(|part| part.exists()) else {
        return Ok(());
    };
    let target = std::fs::canonicalize(existing)
        .map_err(|e| format!("cannot resolve {}: {e}", existing.display()))?;
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    match target.starts_with(&root) {
        true => Ok(()),
        false => Err(format!(
            "{} leads to {}, outside the repository; niobe writes a repository's config \
             only inside it",
            path.display(),
            target.display()
        )),
    }
}

/// Opens the session store of `root` to read it, if one has been created:
/// one that cannot be written, as on a read-only mount, is read as it is. See
/// [`Store::open_to_read`].
pub fn read_existing_store(root: &Path) -> Result<Option<Store>, String> {
    let path = store_path(root);
    store_stays_inside(root)?;
    if !path.exists() {
        return Ok(None);
    }
    Store::open_to_read(&path)
        .map(Some)
        .map_err(|e| format!("cannot open the session store at {}: {e}", path.display()))
}

/// Where git keeps the state of the checkout whose `.git` is `dot_git`: that
/// directory itself in an ordinary clone, and the directory its `gitdir:` line
/// names where `.git` is a file instead — which is what a linked worktree
/// (`git worktree add`) and a submodule have.
fn git_dir(dot_git: &Path) -> Option<PathBuf> {
    if dot_git.is_dir() {
        return Some(dot_git.to_path_buf());
    }
    // Read as config files are — no waiting on a FIFO, a regular file only,
    // and only so much of it — since a tree that did not come from a clone
    // can hold a `.git` that is anything.
    let pointer = niobe_config::read::text(dot_git).ok()?;
    let target = Path::new(pointer.trim().strip_prefix("gitdir:")?.trim());
    if target.is_absolute() {
        return Some(target.to_path_buf());
    }
    // A submodule's pointer is relative to the file that holds it.
    Some(dot_git.parent()?.join(target))
}

/// The checked-out branch, read from the `HEAD` of the nearest repository.
///
/// Parsed rather than shelled out to: `git` may not be installed, and a
/// subprocess for one line of a file is a subprocess the operator pays for.
fn git_branch(from: &Path) -> Option<String> {
    from.ancestors().find_map(|dir| {
        let head = git_dir(&dir.join(".git"))?.join("HEAD");
        let contents = niobe_config::read::text(&head).ok()?;
        let head = contents.trim();
        Some(match head.strip_prefix("ref: refs/heads/") {
            Some(branch) => branch.to_owned(),
            // A detached HEAD is the commit itself, shortened the way git
            // shortens it.
            None => head.chars().take(7).collect(),
        })
    })
}

/// The least time between two reads of the repository while nothing changes.
///
/// A read is several `git` invocations and costs tens of milliseconds on a
/// small tree; the pane it fills is glanceable rather than live, so reading
/// more often than this buys nothing the operator would notice.
const REFRESH: Duration = Duration::from_secs(2);

/// How much of one core a repository this size may cost to keep on screen:
/// the wait after a read is at least the read's own cost times this, so a tree
/// where a read takes a second is read every twenty seconds rather than every
/// two. A pane is not worth a core, and the operator's own build is.
const IDLE_SHARE: u32 = 20;

/// Reads the repository a session is running in, on a thread of its own.
///
/// Handed to the shell as its [`Watch`]: the event loop asks once a tick and
/// is never made to wait, because a read is several subprocesses and a frame
/// is sixteen milliseconds.
#[derive(Debug)]
pub struct Watcher {
    /// Reads that have finished. The thread sends only what differs from what
    /// it last sent, so a repository nobody is changing costs the loop nothing.
    reads: Receiver<Repo>,
    /// Asks for a read now. Dropping it is also how the thread is told the
    /// session is over: its wait ends disconnected and it returns.
    nudge: Sender<()>,
}

impl Watch for Watcher {
    fn look(&mut self) -> Option<Repo> {
        // The newest, not the oldest: a tick that was slow may have several
        // reads behind it, and the older ones describe a repository that has
        // already moved on.
        self.reads.try_iter().last()
    }

    fn changed(&mut self) {
        // A full channel or a thread that has gone is nothing to report: the
        // read this would have asked for happens on the next cadence anyway,
        // and a pane one cadence behind is not worth a line in the transcript.
        let _ = self.nudge.send(());
    }
}

/// Starts reading the repository `cwd` is in, until the returned [`Watcher`]
/// is dropped.
///
/// The name comes from `cwd` — the directory the operator opened the session
/// in — and everything else from the repository that encloses it, so a session
/// started in a subdirectory reports the whole tree it is part of.
pub fn watch(root: &Path) -> Watcher {
    let (reads, from_reads) = channel();
    let (nudge, nudged) = channel();
    let root = root.to_path_buf();
    let name = describe(&root).name;

    std::thread::spawn(move || keep_reading(&root, &name, &reads, &nudged));

    Watcher {
        reads: from_reads,
        nudge,
    }
}

/// Reads the repository until the shell stops asking.
///
/// A read that failed sends nothing, which is what leaves the last read that
/// worked on screen: an emptied pane and a repository with nothing in it would
/// look the same, and only one of them would be true.
fn keep_reading(root: &Path, name: &str, reads: &Sender<Repo>, nudged: &Receiver<()>) {
    let mut last: Option<Repo> = None;
    let mut counts = Counts::default();

    loop {
        let began = Instant::now();
        if let Ok(repo) = read(root, name, &mut counts)
            && last.as_ref() != Some(&repo)
        {
            if reads.send(repo.clone()).is_err() {
                return;
            }
            last = Some(repo);
        }
        let idle = REFRESH.max(began.elapsed().saturating_mul(IDLE_SHARE));

        match nudged.recv_timeout(idle) {
            // A turn changes many files and every one of them asks; they are
            // drained into the single read that follows.
            Ok(()) => while nudged.try_recv().is_ok() {},
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Everything the shell shows about the repository at `root`, in one pass.
///
/// `counts` is what the last read counted in the files git does not track,
/// and leaves holding what this one did.
fn read(root: &Path, name: &str, counts: &mut Counts) -> Result<Repo, String> {
    let began = SystemTime::now();
    let status = branch_status(&git(
        root,
        // The per-file lines are discarded — what the tree did to each file is
        // counted from `git diff --numstat`, which the header does not carry —
        // so git is told not to go looking for untracked ones, which is the
        // part of the scan that grows with the tree. Nor into a submodule's
        // working tree: that is a repository with a config of its own, which
        // the `git` run inside it to look would read and run the filters of.
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=no",
            "--ignore-submodules=dirty",
        ],
    )?);

    let mut working = match &status.head {
        // A repository with no commits has nothing to have changed against,
        // and git counts no lines in a file it has only staged.
        None => Vec::new(),
        Some(_) => working_tree(&git(
            root,
            &[
                "diff",
                "--numstat",
                "-z",
                // A count needs no converter and no external diff, and a
                // config that names either must not get to run it. A
                // submodule is looked at only for the commit it is on, as
                // the status above is.
                "--no-textconv",
                "--no-ext-diff",
                "--ignore-submodules=dirty",
                "HEAD",
            ],
        )?),
    };

    // Listed from the root, which is where the agent runs and the operator's
    // commands do, so each path is the one both would name the file by. `-t`
    // tags each with what git holds of it, which is what says a file is new:
    // this is the one scan of the tree for untracked files, and it serves both.
    let tree = tagged(&git(
        root,
        &[
            "ls-files",
            "-z",
            "-t",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?);
    let last = std::mem::take(counts);
    working.extend(
        tree.iter()
            .filter(|(untracked, _)| *untracked)
            .map(|(_, path)| new_file(root, path, &last, counts, began)),
    );
    working.sort_by(|one, other| one.path.cmp(&other.path));
    let files = files(tree.into_iter().map(|(_, path)| path));
    Ok(Repo {
        name: name.to_owned(),
        branch: status.branch,
        read: true,
        ahead: status.ahead,
        behind: status.behind,
        working,
        files,
    })
}

/// The paths `git ls-files -z -t` printed, in the order it printed them,
/// each with whether git tracks it: the tag `?` is a file it does not.
fn tagged(printed: &str) -> Vec<(bool, String)> {
    printed
        .split('\0')
        .filter_map(|record| record.split_once(' '))
        .map(|(tag, path)| (tag == "?", path.to_owned()))
        .collect()
}

/// The paths listed, each once: a file with a merge conflict is listed once
/// per side.
fn files(listed: impl Iterator<Item = String>) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    for path in listed {
        if files.last() != Some(&path) {
            files.push(path);
        }
    }
    files
}

/// A file git does not track yet, at `path` under `root`, counted as the
/// lines it holds where it can be read — the count `git diff --numstat`
/// gives it once it is added — and not counted where it cannot: a link, not a
/// regular file, or past [`niobe_config::read::LIMIT`], which is read no
/// further than that rather than whole on every look at the tree.
///
/// A file that looks as it did when `last` counted it keeps that count
/// without being opened; whatever is counted goes into `now`. `began` is when
/// the read started, which says whether a stamp can be trusted yet.
fn new_file(
    root: &Path,
    path: &str,
    last: &Counts,
    now: &mut Counts,
    began: SystemTime,
) -> WorkingFile {
    let added = std::fs::symlink_metadata(root.join(path))
        .ok()
        .filter(std::fs::Metadata::is_file)
        .and_then(|meta| {
            let stamp = Stamp::settled(&meta, began);
            let lines = match stamp.and_then(|stamp| last.of(path, stamp)) {
                Some(lines) => lines,
                None => niobe_config::read::bytes(&root.join(path))
                    .ok()
                    .and_then(|bytes| lines_in(&bytes)),
            };
            if let Some(stamp) = stamp {
                now.keep(path, stamp, lines);
            }
            lines
        });
    WorkingFile {
        path: path.to_owned(),
        added,
        removed: Some(0),
        new: true,
    }
}

/// The lines counted in each file git does not track, by its path, with the
/// [`Stamp`] it had when it was counted.
///
/// Opening every untracked file on every read is what makes a read of a tree
/// with many of them cost minutes, and almost none of them change between
/// two reads. A read keeps only the files it saw, so one that is gone is
/// forgotten with it.
#[derive(Debug, Default)]
struct Counts(HashMap<String, (Stamp, Option<u64>)>);

impl Counts {
    /// The count kept for `path`, where it was taken from a file stamped
    /// `stamp`: `Some(None)` is a file that was read and has no count.
    fn of(&self, path: &str, stamp: Stamp) -> Option<Option<u64>> {
        self.0
            .get(path)
            .filter(|(kept, _)| *kept == stamp)
            .map(|(_, lines)| *lines)
    }

    fn keep(&mut self, path: &str, stamp: Stamp, lines: Option<u64>) {
        self.0.insert(path.to_owned(), (stamp, lines));
    }

    #[cfg(test)]
    fn paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.0.keys().cloned().collect();
        paths.sort();
        paths
    }
}

/// What says a file has not changed without opening it: its size and when it
/// was last modified, which is what git's own index trusts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: SystemTime,
}

/// How long before a read a file has to have been last modified for its
/// stamp to be trusted. Some filesystems keep the time to the second, or two,
/// and a file written twice within that time can read the same both times;
/// one modified that recently is counted again on the next read instead.
const SETTLED: Duration = Duration::from_secs(2);

impl Stamp {
    /// The stamp of the file `meta` describes, or `None` where it cannot be
    /// trusted to change when the file does: the system keeps no modification
    /// time, or the file was modified too close to `began`.
    fn settled(meta: &std::fs::Metadata, began: SystemTime) -> Option<Stamp> {
        let modified = meta.modified().ok()?;
        let settled = modified
            .checked_add(SETTLED)
            .is_some_and(|after| after < began);
        settled.then_some(Stamp {
            len: meta.len(),
            modified,
        })
    }
}

/// The lines in `bytes` as git counts them: a last line with no line break
/// after it is a line, and a file git would call binary — one with a NUL in
/// its first 8000 bytes, which is git's test — has no count.
fn lines_in(bytes: &[u8]) -> Option<u64> {
    if bytes.iter().take(8000).any(|&byte| byte == 0) {
        return None;
    }
    let breaks = bytes.iter().filter(|&&byte| byte == b'\n').count();
    let unended = usize::from(bytes.last().is_some_and(|&byte| byte != b'\n'));
    u64::try_from(breaks + unended).ok()
}

/// What `git status --porcelain=v2 --branch` says in its header lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Branch {
    /// The checked-out branch, or the shortened commit on a detached `HEAD`.
    branch: Option<String>,
    /// What `HEAD` is on, or `None` in a repository with no commits.
    head: Option<String>,
    ahead: Option<u32>,
    behind: Option<u32>,
}

/// Reads the header of `git status --porcelain=v2 --branch`.
///
/// The per-file lines that follow it are ignored: what the working tree did to
/// each file is counted from `git diff --numstat`, which the header does not
/// carry. Every header line is optional — `branch.ab` is there only when the
/// branch has an upstream — and a missing one leaves its field unread rather
/// than standing a zero in for it.
fn branch_status(status: &str) -> Branch {
    let mut read = Branch::default();
    for line in status.lines() {
        let Some(header) = line.strip_prefix("# ") else {
            continue;
        };
        match header.split_once(' ') {
            // `(initial)` is a repository whose HEAD names a branch that has
            // no commit yet.
            Some(("branch.oid", oid)) if oid != "(initial)" => {
                read.head = Some(oid.to_owned());
            }
            Some(("branch.head", head)) if head != "(detached)" => {
                read.branch = Some(head.to_owned());
            }
            Some(("branch.ab", ab)) => {
                let (ahead, behind) = ab.split_once(' ').unwrap_or((ab, ""));
                read.ahead = ahead.strip_prefix('+').and_then(|n| n.parse().ok());
                read.behind = behind.strip_prefix('-').and_then(|n| n.parse().ok());
            }
            _ => {}
        }
    }
    // A detached HEAD has no branch to name, so it is said the way git says
    // it: the commit, shortened.
    if read.branch.is_none() {
        read.branch = read.head.as_ref().map(|oid| oid.chars().take(7).collect());
    }
    read
}

/// Reads `git diff --numstat -z`.
///
/// `-z` so that a path is the bytes git holds rather than a quoted rendering
/// of them. A record is `<added>\t<removed>\t<path>`, NUL-terminated; where
/// git sees a rename the path is empty and the two halves of it — where the
/// file came from and where it went — follow as NUL-terminated fields of their
/// own. A binary file has `-` for both counts, because git counts no lines in
/// one, which is not the same as counting none.
fn working_tree(numstat: &str) -> Vec<WorkingFile> {
    let mut fields = numstat.split('\0');
    let mut files = Vec::new();

    while let Some(record) = fields.next() {
        let mut record = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) =
            (record.next(), record.next(), record.next())
        else {
            // The empty field after the last record's terminator.
            continue;
        };
        let path = match path.is_empty() {
            false => path.to_owned(),
            true => match (fields.next(), fields.next()) {
                // Shown the way `git diff --numstat` shows a rename without
                // `-z`, so the pane says the same thing the operator's own
                // `git` would.
                (Some(from), Some(to)) => format!("{from} => {to}"),
                _ => continue,
            },
        };

        files.push(WorkingFile {
            path,
            added: added.parse().ok(),
            removed: removed.parse().ok(),
            new: false,
        });
    }
    files
}

/// Runs one `git` command in `root` and hands back what it printed.
///
/// Shelled out to rather than linked: a git library is megabytes in a binary
/// with a size budget, and this asks git the same questions the operator would.
///
/// A tree that did not come from a clone can carry a `.git/config` of its own
/// choosing, and some of its settings run commands on what is only a read.
/// None may run here, so every `git` this runs has them turned off, whatever
/// it was asked.
fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let drivers = filter_drivers(&filters_configured(root)?);
    let mut command = reading(root);
    // A filter runs on any file whose attributes name it, and attributes come
    // from the tree, the index and `.git/info/attributes`, none of which can
    // be turned off. What can be is the command a name stands for: every
    // driver the configuration defines, through whatever file it includes, is
    // given none, which git takes as no filter. Set through the environment
    // because a driver's name can hold an `=`, which `-c` would split it at.
    let blanks = blanks(&drivers);
    command.env("GIT_CONFIG_COUNT", blanks.len().to_string());
    for (at, (key, value)) in blanks.iter().enumerate() {
        command
            .env(format!("GIT_CONFIG_KEY_{at}"), key)
            .env(format!("GIT_CONFIG_VALUE_{at}"), value);
    }
    let output = command
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;

    if !output.status.success() {
        return Err(failed(args, &output.stderr));
    }
    // Lossy: a path that is not UTF-8 is a path the shell cannot draw anyway,
    // and one of them must not cost the other files their counts.
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A `git` in `root` that only reads, set up so that nothing the repository
/// configures runs a command while it does.
fn reading(root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        // The repository is only ever read here, and this runs every few
        // seconds beside an operator who is using the same tree: refreshing
        // the index under them would take the lock their own `git` wants.
        .env("GIT_OPTIONAL_LOCKS", "0")
        // A partial clone fetches an object it lacks from the remote it came
        // from, over whatever transport and `core.sshCommand` the config
        // names. A read goes to no network: the object is missing instead.
        .env("GIT_NO_LAZY_FETCH", "1")
        // A filesystem monitor runs on every status, and a hook on anything
        // that would write the index.
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .current_dir(root)
        // Nothing here may stop for a prompt or a pager: there is no terminal
        // to answer on — the shell has it.
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped());
    command
}

/// Every `filter.*` setting the repository's configuration holds, from every
/// file git reads it from, as `git config -z --get-regexp` prints them, or
/// nothing where there is none, which it says by exiting with 1.
fn filters_configured(root: &Path) -> Result<String, String> {
    let args = ["config", "-z", "--get-regexp", r"^filter\."];
    let output = reading(root)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    match output.status.code() {
        Some(0) => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        Some(1) => Ok(String::new()),
        _ => Err(failed(&args, &output.stderr)),
    }
}

/// The names of the filter drivers in what `git config -z --get-regexp
/// '^filter\.'` printed, each once: a record is a key, then a newline and its
/// value, and a key is `filter.<name>.<variable>` with dots allowed in the
/// name, so the name is everything between the first dot and the last.
fn filter_drivers(listed: &str) -> Vec<String> {
    let mut names: Vec<String> = listed
        .split('\0')
        .filter_map(|record| record.split('\n').next())
        .filter_map(|key| key.strip_prefix("filter."))
        .filter_map(|rest| rest.rsplit_once('.'))
        .map(|(name, _)| name.to_owned())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The settings that leave each of `drivers` with no command to run. An empty
/// `process` also stops git from using `clean`, and a driver marked
/// `required` would fail the read for having none, so it is marked not to be.
fn blanks(drivers: &[String]) -> Vec<(String, &'static str)> {
    drivers
        .iter()
        .flat_map(|name| {
            [
                (format!("filter.{name}.clean"), ""),
                (format!("filter.{name}.process"), ""),
                (format!("filter.{name}.required"), "false"),
            ]
        })
        .collect()
}

/// The error a `git` that failed is reported as: its arguments and the first
/// line it said.
fn failed(args: &[&str], stderr: &[u8]) -> String {
    let said = String::from_utf8_lossy(stderr);
    format!(
        "git {}: {}",
        args.join(" "),
        said.lines().next().unwrap_or("failed").trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header `git status --porcelain=v2 --branch` prints on a branch that
    /// is three commits ahead of its upstream and one behind.
    const AHEAD_AND_BEHIND: &str = "\
# branch.oid f92f3750bb407beefafde085c9001ec7a2a00a0c
# branch.head feat/cost-transparency
# branch.upstream origin/feat/cost-transparency
# branch.ab +3 -1
1 .M N... 100644 100644 100644 abc abc src/lib.rs
";

    #[test]
    fn a_branch_with_an_upstream_reports_how_far_it_has_drifted() {
        let status = branch_status(AHEAD_AND_BEHIND);

        assert_eq!(status.branch.as_deref(), Some("feat/cost-transparency"));
        assert_eq!(
            status.head.as_deref(),
            Some("f92f3750bb407beefafde085c9001ec7a2a00a0c")
        );
        assert_eq!(status.ahead, Some(3));
        assert_eq!(status.behind, Some(1));
    }

    #[test]
    fn a_branch_with_no_upstream_is_not_ahead_of_anything() {
        let status = branch_status(
            "# branch.oid f92f375\n# branch.head local-only\n1 .M N... 100644 100644 100644 a a f\n",
        );

        assert_eq!(
            status.ahead, None,
            "a branch nobody pushes is not zero commits ahead"
        );
        assert_eq!(status.behind, None);
    }

    #[test]
    fn a_detached_head_is_named_by_the_commit_it_is_on() {
        let status = branch_status("# branch.oid f92f3750bb407bee\n# branch.head (detached)\n");

        assert_eq!(status.branch.as_deref(), Some("f92f375"));
    }

    #[test]
    fn a_repository_with_no_commits_has_no_head_and_keeps_its_branch() {
        let status = branch_status("# branch.oid (initial)\n# branch.head main\n");

        assert_eq!(status.head, None);
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.ahead, None);
    }

    #[test]
    fn a_numstat_record_is_a_path_and_what_the_tree_did_to_it() {
        let files = working_tree("12\t3\tsrc/lib.rs\0");

        assert_eq!(
            files,
            [WorkingFile {
                path: "src/lib.rs".to_owned(),
                added: Some(12),
                removed: Some(3),
                new: false,
            }]
        );
    }

    #[test]
    fn a_file_git_counts_no_lines_in_reports_no_counts_rather_than_zero() {
        let files = working_tree("-\t-\tdoc/diagram.png\0");

        assert_eq!(
            files[0].added, None,
            "a binary file is not zero lines added"
        );
        assert_eq!(files[0].removed, None);
    }

    #[test]
    fn a_renamed_file_is_shown_as_where_it_came_from_and_where_it_went() {
        let files = working_tree("1\t0\tkeep.txt\x001\t0\t\0old.txt\0new.txt\0");

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "keep.txt");
        assert_eq!(files[1].path, "old.txt => new.txt");
        assert_eq!(files[1].added, Some(1));
    }

    /// A repository with one commit and an origin it has been pushed to, so
    /// that a read has an upstream to measure against.
    fn repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let origin = dir.path().join("origin");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&origin).expect("the origin directory can be made");
        std::fs::create_dir_all(&work).expect("the work directory can be made");

        run(&origin, &["init", "--bare", "--initial-branch=main", "."]);
        run(&work, &["init", "--initial-branch=main", "."]);
        run(&work, &["config", "user.email", "test@example.invalid"]);
        run(&work, &["config", "user.name", "A Test"]);
        pin_the_config(&work);
        std::fs::write(work.join("kept.txt"), "a\nb\nc\n").expect("the file is written");
        run(&work, &["add", "kept.txt"]);
        run(&work, &["commit", "-m", "first"]);
        run(
            &work,
            &["remote", "add", "origin", &origin.display().to_string()],
        );
        run(&work, &["push", "-u", "origin", "main"]);
        dir
    }

    /// Sets, in the repository at `work`, the settings a developer's global
    /// git configuration would otherwise change the reads of: the repository
    /// is read the way niobe reads the operator's, with their configuration,
    /// and the repository's own file is what wins over it.
    fn pin_the_config(work: &Path) {
        for (key, value) in [
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
            ("status.showUntrackedFiles", "normal"),
            ("diff.renames", "true"),
        ] {
            run(work, &["config", key, value]);
        }
    }

    /// Runs `git` to set a test's repository up, with no configuration but
    /// the repository's own: a developer's `commit.gpgsign`, hooks or
    /// defaults would otherwise decide whether a commit here works.
    fn run(at: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .current_dir(at)
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} did not run: {e}"));
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn a_working_tree_is_measured_from_the_repository_file_by_file() {
        let dir = repository();
        let work = dir.path().join("work");
        std::fs::write(work.join("kept.txt"), "a\nb\nc\nd\ne\n").expect("the file is written");

        let read = read(&work, "work", &mut Counts::default()).expect("the repository reads");

        assert_eq!(read.branch.as_deref(), Some("main"));
        assert_eq!(read.ahead, Some(0));
        assert_eq!(read.behind, Some(0));
        assert_eq!(
            read.working,
            [WorkingFile {
                path: "kept.txt".to_owned(),
                added: Some(2),
                removed: Some(0),
                new: false,
            }]
        );
    }

    /// A file git does not track yet is in the working tree too: new, and
    /// counted as the lines it holds, which is what `git diff --numstat` says
    /// of it once it is added. One too large to read is new and not counted,
    /// and one the repository ignores is not listed.
    #[test]
    fn a_new_file_is_in_the_working_tree_counted_and_an_ignored_one_is_not() {
        let dir = repository();
        let work = dir.path().join("work");
        std::fs::write(work.join(".gitignore"), "*.log\n").expect("the file is written");
        run(&work, &["add", ".gitignore"]);
        run(&work, &["commit", "-m", "ignore logs"]);
        std::fs::write(work.join("kept.txt"), "a\nb\nc\nd\n").expect("the file is written");
        std::fs::create_dir_all(work.join("sub/dir")).expect("the directory can be made");
        std::fs::write(work.join("sub/dir/inner.txt"), "one\ntwo\nthree").expect("written");
        std::fs::write(
            work.join("large.txt"),
            "x\n".repeat(niobe_config::read::LIMIT as usize),
        )
        .expect("written");
        std::fs::write(work.join("noise.log"), "ignored\n").expect("the file is written");

        let read = read(&work, "work", &mut Counts::default()).expect("the repository reads");

        assert_eq!(
            read.working,
            [
                WorkingFile {
                    path: "kept.txt".to_owned(),
                    added: Some(1),
                    removed: Some(0),
                    new: false,
                },
                WorkingFile {
                    path: "large.txt".to_owned(),
                    added: None,
                    removed: Some(0),
                    new: true,
                },
                WorkingFile {
                    path: "sub/dir/inner.txt".to_owned(),
                    added: Some(3),
                    removed: Some(0),
                    new: true,
                },
            ]
        );
    }

    #[test]
    fn a_new_file_git_would_call_binary_is_new_and_not_counted() {
        assert_eq!(lines_in(b"a\nb\n"), Some(2));
        assert_eq!(lines_in(b"a\nb"), Some(2));
        assert_eq!(lines_in(b""), Some(0));
        assert_eq!(lines_in(b"PNG\0\x01\x02\n"), None);
    }

    /// Writes `contents` to `path` and stamps it as last modified `at`, so
    /// that two writes of the same size can leave a file looking untouched.
    fn write_as_of(path: &Path, contents: &str, at: SystemTime) {
        std::fs::write(path, contents).expect("the file is written");
        std::fs::File::options()
            .write(true)
            .open(path)
            .and_then(|file| file.set_modified(at))
            .expect("the file's modification time can be set");
    }

    /// The lines a read counts in the new file at `path`.
    fn counted(read: &Repo, path: &str) -> Option<u64> {
        read.working
            .iter()
            .find(|file| file.path == path)
            .and_then(|file| file.added)
    }

    #[test]
    fn a_new_file_unchanged_since_the_last_read_is_not_opened_again() {
        let dir = repository();
        let work = dir.path().join("work");
        let then = SystemTime::now() - Duration::from_secs(60);
        write_as_of(&work.join("new.txt"), "abcde\n", then);
        let mut counts = Counts::default();
        let first = read(&work, "work", &mut counts).expect("the repository reads");
        assert_eq!(counted(&first, "new.txt"), Some(1));

        // The same size and the same modification time: what a file that was
        // not touched looks like without opening it.
        write_as_of(&work.join("new.txt"), "a\nb\nc\n", then);
        let again = read(&work, "work", &mut counts).expect("the repository reads");

        assert_eq!(
            counted(&again, "new.txt"),
            Some(1),
            "a file that looked untouched was opened and counted again"
        );
    }

    #[test]
    fn a_new_file_that_changed_since_the_last_read_is_counted_again() {
        let dir = repository();
        let work = dir.path().join("work");
        let then = SystemTime::now() - Duration::from_secs(60);
        write_as_of(&work.join("new.txt"), "a\n", then);
        let mut counts = Counts::default();
        read(&work, "work", &mut counts).expect("the repository reads");

        write_as_of(&work.join("new.txt"), "a\nb\nc\n", then);
        let again = read(&work, "work", &mut counts).expect("the repository reads");

        assert_eq!(counted(&again, "new.txt"), Some(3));
    }

    /// A filesystem that keeps modification times to the second, or two, can
    /// give a file written twice within that time the same stamp both times,
    /// so a file modified that recently is counted again on the next read.
    #[test]
    fn a_new_file_written_just_before_a_read_is_counted_again_on_the_next() {
        let dir = repository();
        let work = dir.path().join("work");
        // Stamped ahead of the clock rather than at it: a read that starts
        // more than SETTLED after a stamp taken here, as it can on a loaded
        // machine, would otherwise find the file already settled.
        let recent = SystemTime::now() + Duration::from_secs(60);
        write_as_of(&work.join("new.txt"), "abcde\n", recent);
        let mut counts = Counts::default();
        read(&work, "work", &mut counts).expect("the repository reads");

        write_as_of(&work.join("new.txt"), "a\nb\nc\n", recent);
        let again = read(&work, "work", &mut counts).expect("the repository reads");

        assert_eq!(counted(&again, "new.txt"), Some(3));
    }

    #[test]
    fn a_new_file_that_is_gone_is_not_remembered() {
        let dir = repository();
        let work = dir.path().join("work");
        let then = SystemTime::now() - Duration::from_secs(60);
        write_as_of(&work.join("gone.txt"), "a\n", then);
        write_as_of(&work.join("kept.new"), "a\n", then);
        let mut counts = Counts::default();
        read(&work, "work", &mut counts).expect("the repository reads");

        std::fs::remove_file(work.join("gone.txt")).expect("the file is removed");
        read(&work, "work", &mut counts).expect("the repository reads");

        assert_eq!(counts.paths(), ["kept.new"]);
    }

    #[test]
    fn a_read_lists_the_files_in_the_repository_that_git_does_not_ignore() {
        let dir = repository();
        let work = dir.path().join("work");
        let docs = work.join("docs");
        std::fs::create_dir_all(&docs).expect("the directory can be made");
        std::fs::write(docs.join("guide.md"), "new\n").expect("the file is written");
        std::fs::write(work.join("scratch.log"), "noise\n").expect("the file is written");
        std::fs::write(work.join(".gitignore"), "*.log\n").expect("the file is written");

        let whole = read(&work, "work", &mut Counts::default()).expect("the repository reads");

        assert_eq!(
            whole.files,
            [".gitignore", "docs/guide.md", "kept.txt"],
            "committed and new files, and not the ignored one"
        );
    }

    #[test]
    fn a_repository_with_no_commits_reads_without_failing() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        run(dir.path(), &["init", "--initial-branch=main", "."]);
        std::fs::write(dir.path().join("new.txt"), "a\n").expect("the file is written");

        let read = read(dir.path(), "work", &mut Counts::default()).expect("the repository reads");

        assert_eq!(read.branch.as_deref(), Some("main"));
        assert_eq!(
            read.working,
            [WorkingFile {
                path: "new.txt".to_owned(),
                added: Some(1),
                removed: Some(0),
                new: true,
            }],
            "with nothing committed, every file in the tree is new"
        );
    }

    #[test]
    fn a_directory_that_is_not_a_repository_is_a_failed_read_and_not_a_panic() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");

        let failed =
            read(dir.path(), "work", &mut Counts::default()).expect_err("there is no repository");

        assert!(failed.starts_with("git status"), "{failed}");
    }

    #[test]
    fn a_watch_hands_over_the_repository_it_is_started_in() {
        let dir = repository();
        let work = dir.path().join("work");
        std::fs::write(work.join("kept.txt"), "a\nb\nc\nd\n").expect("the file is written");

        let mut watching = watch(&work);
        // Reading the repository starts `git` more than once; with three
        // copies of this binary running at once the slowest read measured came
        // back in 7.25 s, and one that never comes waits the same either way.
        let read = within(&mut watching, Duration::from_secs(60)).expect("a read comes back");

        assert_eq!(read.name, "work");
        assert_eq!(read.branch.as_deref(), Some("main"));
        assert_eq!(read.working.len(), 1);
    }

    #[test]
    fn a_watch_on_a_directory_that_is_not_a_repository_hands_over_nothing() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");

        let mut watching = watch(dir.path());

        assert_eq!(
            within(&mut watching, Duration::from_secs(1)),
            None,
            "an emptied pane and a failed read must not look the same"
        );
    }

    #[test]
    fn a_linked_worktree_reads_its_own_branch_and_its_own_working_tree() {
        let dir = repository();
        let work = dir.path().join("work");
        let checkout = dir.path().join("wt");
        run(
            &work,
            &[
                "worktree",
                "add",
                "-b",
                "side",
                &checkout.display().to_string(),
            ],
        );
        std::fs::write(checkout.join("kept.txt"), "a\n").expect("the file is written");

        let read = read(&checkout, "wt", &mut Counts::default()).expect("the worktree reads");

        assert_eq!(read.branch.as_deref(), Some("side"));
        assert_eq!(read.ahead, None, "a branch made here has no upstream yet");
        assert_eq!(read.working.len(), 1);
        assert_eq!(read.working[0].removed, Some(2));
    }

    #[test]
    fn a_detached_head_is_read_as_the_commit_it_is_sitting_on() {
        let dir = repository();
        let work = dir.path().join("work");
        let head = git(&work, &["rev-parse", "HEAD"]).expect("the repository has a commit");
        let head = head.trim();
        run(&work, &["checkout", "--detach", head]);

        let read = read(&work, "work", &mut Counts::default()).expect("a detached head reads");

        assert_eq!(read.branch.as_deref(), Some(&head[..7]));
        assert_eq!(read.ahead, None, "a commit has no upstream");
        assert!(read.working.is_empty());
    }

    /// How many ticks of the event loop are timed against one frame's budget.
    /// The loop asks the watch once a tick and ten times a second; a hundred
    /// is ten seconds of an idle session, and several seconds of a busy one.
    const TICKS: usize = 100;

    /// A quarter of a second. Asking without waiting costs microseconds a
    /// time, and a look that waited for a read would cost tens of
    /// milliseconds each — seconds over [`TICKS`] — so this bound still tells
    /// the two apart, with room for a debug build sharing its cores with the
    /// tests around it, where one frame's worth did not.
    const FRAME: Duration = Duration::from_millis(250);

    /// A read of this workspace takes tens of milliseconds — several frames —
    /// so the loop can never be the thing doing it. A hundred ticks' worth of
    /// asking has to come in far under what a single read costs, while a read
    /// of a real repository is going on behind them.
    #[test]
    fn asking_what_the_repository_looks_like_never_waits_for_the_answer() {
        let mut watching = watch(Path::new(env!("CARGO_MANIFEST_DIR")));

        let began = Instant::now();
        for _ in 0..TICKS {
            let _ = watching.look();
        }
        let spent = began.elapsed();

        assert!(
            spent < FRAME,
            "{TICKS} ticks spent {spent:?} asking, over the {FRAME:?} they may"
        );
    }

    /// Asks a watch until it has something or `patience` runs out, the way the
    /// event loop asks it once a tick.
    fn within(watching: &mut Watcher, patience: Duration) -> Option<Repo> {
        let until = Instant::now() + patience;
        while Instant::now() < until {
            if let Some(read) = watching.look() {
                return Some(read);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    /// Writes a script under `dir` that leaves `<name>-ran` beside it when
    /// anything runs it, and hands back the script and that marker. It passes
    /// what it is given through, so a git that did run it reads on unharmed.
    #[cfg(unix)]
    fn marking(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join(format!("{name}.sh"));
        let marker = dir.join(format!("{name}-ran"));
        std::fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\nexec cat \"$@\"\n", marker.display()),
        )
        .expect("the script is written");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("the script is made executable");
        (script, marker)
    }

    /// Asserts that none of `markers` was left, naming the first that was.
    fn none_ran(markers: &[PathBuf]) {
        for marker in markers {
            assert!(!marker.exists(), "a read ran {}", marker.display());
        }
    }

    /// Every file here is rewritten to the size it was committed at, so git
    /// cannot tell from the size alone that it changed and has to read it,
    /// which is where a filter or a converter would run.
    #[cfg(unix)]
    #[test]
    fn a_read_runs_no_command_the_repositorys_own_config_names() {
        let dir = repository();
        let work = dir.path().join("work");
        let (monitor, monitored) = marking(dir.path(), "monitor");
        let (clean, cleaned) = marking(dir.path(), "clean");
        let (process, processed) = marking(dir.path(), "process");
        let (textconv, converted) = marking(dir.path(), "textconv");
        let (external, diffed) = marking(dir.path(), "external");
        for name in ["cleaned.txt", "processed.txt", "converted.txt"] {
            std::fs::write(work.join(name), "a\nb\n").expect("the file is written");
        }
        std::fs::write(
            work.join(".gitattributes"),
            "cleaned.txt filter=x\nprocessed.txt filter=y\nconverted.txt diff=x\n",
        )
        .expect("the attributes are written");
        run(&work, &["add", "."]);
        run(&work, &["commit", "-m", "attributed"]);
        for (key, value) in [
            ("core.fsmonitor", &monitor),
            ("filter.x.clean", &clean),
            ("filter.y.process", &process),
            ("diff.x.textconv", &textconv),
            ("diff.external", &external),
        ] {
            run(&work, &["config", key, &value.display().to_string()]);
        }
        run(&work, &["config", "filter.x.required", "true"]);
        run(&work, &["config", "filter.y.required", "true"]);
        for name in ["cleaned.txt", "processed.txt", "converted.txt"] {
            std::fs::write(work.join(name), "a\nc\n").expect("the file is written");
        }

        let read = read(&work, "work", &mut Counts::default()).expect("the repository reads");

        none_ran(&[monitored, cleaned, processed, converted, diffed]);
        let changed = |path: &str| WorkingFile {
            path: path.to_owned(),
            added: Some(1),
            removed: Some(1),
            new: false,
        };
        assert_eq!(
            read.working,
            [
                changed("cleaned.txt"),
                changed("converted.txt"),
                changed("processed.txt"),
            ]
        );
    }

    /// A filter can be defined in a file the repository's config only
    /// includes, under a name with dots in it, and given to a file by
    /// attributes that are not in the tree at all.
    #[test]
    fn every_filter_driver_the_config_lists_is_named_once() {
        let listed = "filter.lfs.clean\ngit-lfs clean -- %f\0\
                      filter.lfs.required\ntrue\0\
                      filter.x.y=z.process\nrun it\0\
                      filter.bare\0";

        assert_eq!(filter_drivers(listed), ["lfs", "x.y=z"]);
        assert_eq!(filter_drivers(""), Vec::<String>::new());
    }

    #[cfg(unix)]
    #[test]
    fn a_read_runs_no_filter_an_included_file_defines_for_attributes_outside_the_tree() {
        let dir = repository();
        let work = dir.path().join("work");
        let (clean, cleaned) = marking(dir.path(), "clean");
        let included = dir.path().join("included");
        std::fs::write(
            &included,
            format!("[filter \"x.y=z\"]\n\tclean = {}\n", clean.display()),
        )
        .expect("the included file is written");
        run(
            &work,
            &["config", "include.path", &included.display().to_string()],
        );
        std::fs::create_dir_all(work.join(".git/info")).expect("the info directory can be made");
        std::fs::write(work.join(".git/info/attributes"), "kept.txt filter=x.y=z\n")
            .expect("the attributes are written");
        std::fs::write(work.join("kept.txt"), "a\nb\nd\n").expect("the file is written");

        read(&work, "work", &mut Counts::default()).expect("the repository reads");

        none_ran(&[cleaned]);
    }

    /// A submodule is a repository of its own, with a config of its own that
    /// a read of the repository around it never looks at.
    #[cfg(unix)]
    #[test]
    fn a_read_runs_no_filter_a_submodules_own_config_names() {
        let dir = repository();
        let work = dir.path().join("work");
        let (clean, cleaned) = marking(dir.path(), "clean");
        let origin = dir.path().join("origin").display().to_string();
        run(
            &work,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                &origin,
                "sub",
            ],
        );
        run(&work, &["commit", "-m", "submodule"]);
        let sub = work.join("sub");
        run(
            &sub,
            &["config", "filter.x.clean", &clean.display().to_string()],
        );
        std::fs::write(
            work.join(".git/modules/sub/info/attributes"),
            "kept.txt filter=x\n",
        )
        .expect("the attributes are written");
        std::fs::write(sub.join("kept.txt"), "a\nb\nd\n").expect("the file is written");

        read(&work, "work", &mut Counts::default()).expect("the repository reads");

        none_ran(&[cleaned]);
    }

    /// A repository can say an object it lacks is to be fetched from a
    /// remote, and how to reach that remote. A read goes to no network, and
    /// runs no command to get there.
    #[cfg(unix)]
    #[test]
    fn a_read_fetches_no_object_the_repository_is_missing() {
        let dir = repository();
        let work = dir.path().join("work");
        let (ssh, connected) = marking(dir.path(), "ssh");
        for (key, value) in [
            ("core.repositoryformatversion", "1"),
            ("extensions.partialClone", "origin"),
            ("remote.origin.promisor", "true"),
            ("remote.origin.url", "ssh://example.invalid/repository"),
            ("core.sshCommand", &ssh.display().to_string()),
        ] {
            run(&work, &["config", key, value]);
        }
        let blob = git(&work, &["rev-parse", "HEAD:kept.txt"]).expect("the file has a blob");
        let blob = blob.trim();
        std::fs::remove_file(work.join(".git/objects").join(&blob[..2]).join(&blob[2..]))
            .expect("the blob is a loose object");
        std::fs::write(work.join("kept.txt"), "a\nb\nd\n").expect("the file is written");

        let _ = read(&work, "work", &mut Counts::default());

        none_ran(&[connected]);
    }

    #[cfg(unix)]
    #[test]
    fn a_dot_git_that_is_a_pipe_leaves_the_branch_unknown_without_waiting() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let made = std::process::Command::new("mkfifo")
            .arg(dir.path().join(".git"))
            .status()
            .expect("mkfifo runs");
        assert!(made.success());

        let started = Instant::now();
        assert_eq!(git_branch(dir.path()), None);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn the_branch_is_read_from_the_repository_git_reports() {
        let dir = repository();
        let here = dir.path().join("work");
        run(&here, &["checkout", "-q", "-b", "feature/read-me"]);
        assert_eq!(git_branch(&here).as_deref(), Some("feature/read-me"));

        // Detached: the commit, shortened as git shortens it.
        run(&here, &["checkout", "-q", "--detach"]);
        let out = std::process::Command::new("git")
            .args(["rev-parse", "--short=7", "HEAD"])
            .current_dir(&here)
            .output()
            .expect("git runs");
        let short = String::from_utf8(out.stdout).expect("git prints UTF-8");
        assert_eq!(git_branch(&here).as_deref(), Some(short.trim()));
    }

    #[test]
    fn a_directory_outside_a_repository_has_no_branch() {
        assert_eq!(git_branch(Path::new("/")), None);
    }

    /// The layout `git worktree add` leaves behind: the checkout's `.git` is a
    /// file naming a directory under the main repository's `.git/worktrees`,
    /// and that directory holds the `HEAD` of this checkout.
    fn worktree(root: &Path, pointer: &str, head: &str) -> PathBuf {
        let gitdir = root.join("main").join(".git").join("worktrees").join("wt");
        std::fs::create_dir_all(&gitdir).expect("the worktree state directory can be made");
        std::fs::write(gitdir.join("HEAD"), head).expect("the worktree HEAD is written");

        let checkout = root.join("wt");
        std::fs::create_dir_all(&checkout).expect("the checkout can be made");
        std::fs::write(checkout.join(".git"), pointer).expect("the pointer file is written");
        checkout
    }

    #[cfg(unix)]
    #[test]
    fn a_store_linked_to_a_file_inside_the_repository_is_opened_there() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        std::fs::create_dir_all(dir.path().join(".niobe")).expect("the directory is made");
        std::fs::create_dir_all(dir.path().join("kept")).expect("the directory is made");
        let real = dir.path().join("kept").join("sessions.db");
        std::fs::write(&real, "").expect("the file is written");
        std::os::unix::fs::symlink(&real, store_path(dir.path())).expect("the link is made");

        open_or_create_store(dir.path()).expect("a link inside the repository is followed");

        assert!(std::fs::metadata(&real).expect("stat").len() > 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_niobe_directory_linked_out_of_the_repository_is_refused() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let outside = tempfile::tempdir().expect("a temporary directory can be created");
        std::os::unix::fs::symlink(outside.path(), dir.path().join(DIR)).expect("linked");

        let said = open_or_create_store(dir.path()).expect_err("the store would be outside");

        assert!(said.contains("outside the repository"), "{said}");
        assert_eq!(std::fs::read_dir(outside.path()).expect("lists").count(), 0);
        let said = read_existing_store(dir.path()).expect_err("so would the one read");
        assert!(said.contains(".niobe"), "{said}");
    }

    #[test]
    fn a_worktrees_branch_is_read_through_its_gitdir_pointer() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let gitdir = dir
            .path()
            .join("main")
            .join(".git")
            .join("worktrees")
            .join("wt");
        let checkout = worktree(
            dir.path(),
            &format!("gitdir: {}\n", gitdir.display()),
            "ref: refs/heads/feature\n",
        );

        assert_eq!(git_branch(&checkout), Some("feature".to_owned()));
    }

    #[test]
    fn a_gitdir_pointer_is_resolved_against_the_file_that_holds_it() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let checkout = worktree(
            dir.path(),
            "gitdir: ../main/.git/worktrees/wt\n",
            "ref: refs/heads/feature\n",
        );

        assert_eq!(git_branch(&checkout), Some("feature".to_owned()));
    }

    #[test]
    fn a_git_file_that_names_no_gitdir_leaves_the_branch_unknown() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        std::fs::write(dir.path().join(".git"), "not a pointer\n").expect("the file is written");

        assert_eq!(git_branch(dir.path()), None);
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

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .expect("the file is there")
            .permissions()
            .mode()
            & 0o777
    }

    /// The side files of the store at `root` that SQLite has made so far.
    fn side_files(root: &Path) -> Vec<PathBuf> {
        STORE_SIDE_FILES
            .iter()
            .map(|suffix| {
                let mut name = store_path(root).into_os_string();
                name.push(suffix);
                PathBuf::from(name)
            })
            .filter(|path| path.exists())
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn a_new_store_and_its_directory_are_the_operators_alone() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");

        let store = open_or_create_store(dir.path()).expect("the store is created");

        assert_eq!(mode(&dir.path().join(DIR)), 0o700);
        assert_eq!(mode(&store_path(dir.path())), 0o600);
        let side = side_files(dir.path());
        assert!(!side.is_empty(), "SQLite made no side files to check");
        for path in side {
            assert_eq!(mode(&path), 0o600, "{}", path.display());
        }
        drop(store);
    }

    #[cfg(unix)]
    #[test]
    fn a_store_any_user_could_read_is_the_operators_alone_once_opened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let store = open_or_create_store(dir.path()).expect("the store is created");
        let open = std::fs::Permissions::from_mode(0o644);
        std::fs::set_permissions(store_path(dir.path()), open.clone()).expect("loosened");
        std::fs::set_permissions(dir.path().join(DIR), std::fs::Permissions::from_mode(0o755))
            .expect("loosened");
        for path in side_files(dir.path()) {
            std::fs::set_permissions(&path, open.clone()).expect("loosened");
        }
        drop(store);

        let _store = open_or_create_store(dir.path()).expect("the store opens");

        assert_eq!(mode(&store_path(dir.path())), 0o600);
        assert_eq!(mode(&dir.path().join(DIR)), 0o700);
        for path in side_files(dir.path()) {
            assert_eq!(mode(&path), 0o600, "{}", path.display());
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_files_meant_to_be_committed_keep_the_mode_any_file_would_have() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let probe = dir.path().join("probe");
        std::fs::write(&probe, "").expect("a file can be written");
        let ordinary = mode(&probe);
        std::fs::create_dir(dir.path().join(DIR)).expect("the directory can be made");
        std::fs::write(config_path(dir.path()), "").expect("the config is written");

        let _store = open_or_create_store(dir.path()).expect("the store is created");

        assert_eq!(mode(&config_path(dir.path())), ordinary);
        assert_eq!(mode(&dir.path().join(DIR).join(".gitignore")), ordinary);
    }
}
