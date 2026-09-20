// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Where the session is running, and where its store lives.
//!
//! Read here rather than in the TUI, which has no business touching the
//! filesystem.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant, UNIX_EPOCH};

use niobe_store::Store;
use niobe_tui::app::{Commit, Repo, WorkingFile};
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

/// Where git keeps the state of the checkout whose `.git` is `dot_git`: that
/// directory itself in an ordinary clone, and the directory its `gitdir:` line
/// names where `.git` is a file instead — which is what a linked worktree
/// (`git worktree add`) and a submodule have.
fn git_dir(dot_git: &Path) -> Option<PathBuf> {
    if dot_git.is_dir() {
        return Some(dot_git.to_path_buf());
    }
    let pointer = std::fs::read_to_string(dot_git).ok()?;
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
        let contents = std::fs::read_to_string(head).ok()?;
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

/// Where the session's commits are counted from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Since<'a> {
    /// The session has made no commits yet, so none are listed. The first read
    /// of a session, which is what the rest are measured against.
    Nothing,
    /// The commit the session started on: everything after it is its own.
    Commit(&'a str),
    /// The repository had no commits when the session started, so every commit
    /// in it is one the session made.
    Everything,
}

/// One read of the repository: what the shell shows, and the commit `HEAD` was
/// on, which the next reads measure the session's own commits from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Read {
    repo: Repo,
    head: Option<String>,
}

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
pub fn watch(cwd: &Path) -> Watcher {
    let (reads, from_reads) = channel();
    let (nudge, nudged) = channel();
    let root = root(cwd);
    let name = describe(cwd).name;

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
    // The commit the session started on, taken from the first read that
    // worked. `Some(None)` is a repository that had no commits then.
    let mut started_on: Option<Option<String>> = None;
    let mut last: Option<Repo> = None;

    loop {
        let since = match &started_on {
            None => Since::Nothing,
            Some(None) => Since::Everything,
            Some(Some(oid)) => Since::Commit(oid),
        };

        let began = Instant::now();
        if let Ok(read) = read(root, name, since) {
            started_on.get_or_insert(read.head);
            if last.as_ref() != Some(&read.repo) {
                if reads.send(read.repo.clone()).is_err() {
                    return;
                }
                last = Some(read.repo);
            }
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
fn read(root: &Path, name: &str, since: Since<'_>) -> Result<Read, String> {
    let status = branch_status(&git(
        root,
        // The per-file lines are discarded — what the tree did to each file is
        // counted from `git diff --numstat`, which the header does not carry —
        // so git is told not to go looking for untracked ones, which is the
        // part of the scan that grows with the tree.
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=no",
        ],
    )?);

    let working = match &status.head {
        // A repository with no commits has nothing to have changed against:
        // every file in it is untracked, and git counts no lines in those.
        None => Vec::new(),
        Some(_) => working_tree(&git(root, &["diff", "--numstat", "-z", "HEAD"])?),
    };

    let listed = match since {
        Since::Nothing => Vec::new(),
        Since::Everything if status.head.is_none() => Vec::new(),
        Since::Everything => commits(&git(
            root,
            &["log", "-z", "--format=%h%x00%s%x00%ct", "HEAD"],
        )?),
        Since::Commit(oid) => commits(&git(
            root,
            &[
                "log",
                "-z",
                "--format=%h%x00%s%x00%ct",
                &format!("{oid}..HEAD"),
            ],
        )?),
    };

    let commits = pushed(root, listed, &status)?;
    Ok(Read {
        repo: Repo {
            name: name.to_owned(),
            branch: status.branch,
            read: true,
            ahead: status.ahead,
            behind: status.behind,
            working,
            commits,
        },
        head: status.head,
    })
}

/// Says, for each of `listed`, whether the branch's upstream already has it.
///
/// Three of the four answers cost nothing: a branch with no upstream has
/// nowhere to have pushed anything, a branch that is not ahead of its upstream
/// has pushed all of it, and a branch whose upstream the repository cannot
/// find — which git reports by naming the upstream and then refusing to say
/// how far apart the two are — is not known either way. Only a branch that is
/// genuinely ahead has to be asked which of its commits are the ones ahead.
fn pushed(root: &Path, listed: Vec<Commit>, status: &Branch) -> Result<Vec<Commit>, String> {
    if listed.is_empty() {
        return Ok(listed);
    }
    if !status.upstream {
        return Ok(said(listed, Some(false)));
    }
    let Some(ahead) = status.ahead else {
        return Ok(said(listed, None));
    };
    if ahead == 0 {
        return Ok(said(listed, Some(true)));
    }

    let unpushed = git(root, &["log", "--format=%h", "@{upstream}..HEAD"])?;
    let unpushed: Vec<&str> = unpushed.lines().map(str::trim).collect();
    Ok(listed
        .into_iter()
        .map(|commit| Commit {
            pushed: Some(!unpushed.contains(&commit.hash.as_str())),
            ..commit
        })
        .collect())
}

/// Says the same thing about every commit, where the repository answered for
/// the branch as a whole.
fn said(listed: Vec<Commit>, pushed: Option<bool>) -> Vec<Commit> {
    listed
        .into_iter()
        .map(|commit| Commit { pushed, ..commit })
        .collect()
}

/// What `git status --porcelain=v2 --branch` says in its header lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Branch {
    /// The checked-out branch, or the shortened commit on a detached `HEAD`.
    branch: Option<String>,
    /// What `HEAD` is on, or `None` in a repository with no commits.
    head: Option<String>,
    /// Whether the branch has an upstream to be ahead or behind of.
    upstream: bool,
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
            Some(("branch.upstream", _)) => read.upstream = true,
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
        });
    }
    files
}

/// Reads `git log -z --format=%h%x00%s%x00%ct`, newest first: `-z` separates
/// one commit from the next with a NUL, so a subject with a newline in it
/// cannot be read as two commits.
///
/// The date is `%ct`, the commit date in seconds since the epoch, which is
/// what the pane draws an age from. A record whose date will not parse keeps
/// its hash and subject and loses only the age: a commit dated from the moment
/// it was read would be a figure about this read rather than about the commit.
fn commits(log: &str) -> Vec<Commit> {
    let mut fields = log.split('\0').filter(|field| !field.is_empty());
    let mut commits = Vec::new();

    while let (Some(hash), Some(subject), Some(at)) = (fields.next(), fields.next(), fields.next())
    {
        commits.push(Commit {
            hash: hash.to_owned(),
            subject: subject.to_owned(),
            at: at
                .trim()
                .parse()
                .ok()
                .map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds)),
            // Filled in for all of them by `pushed`: it can take one more read
            // of the repository to know, and this one has none.
            pushed: None,
        });
    }
    commits
}

/// Runs one `git` command in `root` and hands back what it printed.
///
/// Shelled out to rather than linked: a git library is megabytes in a binary
/// with a size budget, and this asks git the same questions the operator would.
fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        // The repository is only ever read here, and this runs every few
        // seconds beside an operator who is using the same tree: refreshing
        // the index under them would take the lock their own `git` wants.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(args)
        .current_dir(root)
        // Nothing here may stop for a prompt or a pager: there is no terminal
        // to answer on — the shell has it.
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;

    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            said.lines().next().unwrap_or("failed").trim()
        ));
    }
    // Lossy: a path that is not UTF-8 is a path the shell cannot draw anyway,
    // and one of them must not cost the other files their counts.
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
        assert!(status.upstream);
        assert_eq!(status.ahead, Some(3));
        assert_eq!(status.behind, Some(1));
    }

    #[test]
    fn a_branch_with_no_upstream_is_not_ahead_of_anything() {
        let status = branch_status(
            "# branch.oid f92f375\n# branch.head local-only\n1 .M N... 100644 100644 100644 a a f\n",
        );

        assert!(!status.upstream);
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

    #[test]
    fn a_log_is_read_as_a_hash_and_the_first_line_of_its_message() {
        let made = commits(
            "f92f375\0chore: release 0.7.0\x001789939986\0d432ed6\0feat: split the tokens\x001789939158\0",
        );

        assert_eq!(
            made,
            [
                Commit {
                    hash: "f92f375".to_owned(),
                    subject: "chore: release 0.7.0".to_owned(),
                    at: Some(UNIX_EPOCH + Duration::from_secs(1_789_939_986)),
                    pushed: None,
                },
                Commit {
                    hash: "d432ed6".to_owned(),
                    subject: "feat: split the tokens".to_owned(),
                    at: Some(UNIX_EPOCH + Duration::from_secs(1_789_939_158)),
                    pushed: None,
                },
            ]
        );
    }

    #[test]
    fn a_commit_whose_date_will_not_parse_keeps_everything_else() {
        let made = commits("f92f375\0chore: release 0.7.0\0not a date\0");

        assert_eq!(made.len(), 1);
        assert_eq!(
            made[0].at, None,
            "an age counted from the moment it was read would be about the read"
        );
        assert_eq!(made[0].subject, "chore: release 0.7.0");
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

    fn run(at: &Path, args: &[&str]) {
        let done = git(at, args).unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
        let _ = done;
    }

    #[test]
    fn a_working_tree_is_measured_from_the_repository_file_by_file() {
        let dir = repository();
        let work = dir.path().join("work");
        std::fs::write(work.join("kept.txt"), "a\nb\nc\nd\ne\n").expect("the file is written");

        let read = read(&work, "work", Since::Nothing).expect("the repository reads");

        assert_eq!(read.repo.branch.as_deref(), Some("main"));
        assert_eq!(read.repo.ahead, Some(0));
        assert_eq!(read.repo.behind, Some(0));
        assert_eq!(
            read.repo.working,
            [WorkingFile {
                path: "kept.txt".to_owned(),
                added: Some(2),
                removed: Some(0),
            }]
        );
        assert!(
            read.repo.commits.is_empty(),
            "the session has committed nothing"
        );
    }

    #[test]
    fn the_commits_a_session_made_are_listed_and_say_whether_they_are_pushed() {
        let dir = repository();
        let work = dir.path().join("work");
        let started_on = read(&work, "work", Since::Nothing)
            .expect("the repository reads")
            .head
            .expect("the repository has a commit");

        std::fs::write(work.join("kept.txt"), "a\nb\nc\nd\n").expect("the file is written");
        run(&work, &["commit", "-am", "second"]);
        std::fs::write(work.join("kept.txt"), "a\nb\n").expect("the file is written");
        run(&work, &["commit", "-am", "third"]);
        run(&work, &["push", "origin", "main"]);
        std::fs::write(work.join("kept.txt"), "a\n").expect("the file is written");
        run(&work, &["commit", "-am", "fourth"]);

        let read = read(&work, "work", Since::Commit(&started_on)).expect("the repository reads");

        let subjects: Vec<(&str, Option<bool>)> = read
            .repo
            .commits
            .iter()
            .map(|c| (c.subject.as_str(), c.pushed))
            .collect();
        assert_eq!(
            subjects,
            [
                ("fourth", Some(false)),
                ("third", Some(true)),
                ("second", Some(true))
            ],
            "newest first, and only the one the origin has not got is unpushed"
        );
        assert_eq!(read.repo.ahead, Some(1));
    }

    #[test]
    fn a_branch_with_no_upstream_has_pushed_none_of_its_commits() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let work = dir.path().to_path_buf();
        run(&work, &["init", "--initial-branch=main", "."]);
        run(&work, &["config", "user.email", "test@example.invalid"]);
        run(&work, &["config", "user.name", "A Test"]);
        std::fs::write(work.join("a.txt"), "a\n").expect("the file is written");
        run(&work, &["add", "a.txt"]);
        run(&work, &["commit", "-m", "only"]);

        let read = read(&work, "work", Since::Everything).expect("the repository reads");

        assert_eq!(read.repo.ahead, None);
        assert_eq!(read.repo.commits.len(), 1);
        assert_eq!(read.repo.commits[0].pushed, Some(false));
    }

    #[test]
    fn a_repository_with_no_commits_reads_without_failing() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        run(dir.path(), &["init", "--initial-branch=main", "."]);
        std::fs::write(dir.path().join("new.txt"), "a\n").expect("the file is written");

        let read = read(dir.path(), "work", Since::Everything).expect("the repository reads");

        assert_eq!(read.head, None);
        assert_eq!(read.repo.branch.as_deref(), Some("main"));
        assert!(read.repo.working.is_empty());
        assert!(read.repo.commits.is_empty());
    }

    #[test]
    fn a_directory_that_is_not_a_repository_is_a_failed_read_and_not_a_panic() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");

        let failed = read(dir.path(), "work", Since::Nothing).expect_err("there is no repository");

        assert!(failed.starts_with("git status"), "{failed}");
    }

    #[test]
    fn a_watch_hands_over_the_repository_it_is_started_in() {
        let dir = repository();
        let work = dir.path().join("work");
        std::fs::write(work.join("kept.txt"), "a\nb\nc\nd\n").expect("the file is written");

        let mut watching = watch(&work);
        let read = within(&mut watching, Duration::from_secs(10)).expect("a read comes back");

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

        let read = read(&checkout, "wt", Since::Nothing).expect("the worktree reads");

        assert_eq!(read.repo.branch.as_deref(), Some("side"));
        assert_eq!(
            read.repo.ahead, None,
            "a branch made here has no upstream yet"
        );
        assert_eq!(read.repo.working.len(), 1);
        assert_eq!(read.repo.working[0].removed, Some(2));
    }

    #[test]
    fn a_detached_head_is_read_as_the_commit_it_is_sitting_on() {
        let dir = repository();
        let work = dir.path().join("work");
        let head = read(&work, "work", Since::Nothing)
            .expect("the repository reads")
            .head
            .expect("the repository has a commit");
        run(&work, &["checkout", "--detach", &head]);

        let read = read(&work, "work", Since::Nothing).expect("a detached head reads");

        assert_eq!(read.repo.branch.as_deref(), Some(&head[..7]));
        assert_eq!(read.repo.ahead, None, "a commit has no upstream");
        assert!(read.repo.working.is_empty());
    }

    #[test]
    fn an_upstream_the_repository_cannot_find_leaves_pushed_unknown() {
        let dir = repository();
        let work = dir.path().join("work");
        let started_on = read(&work, "work", Since::Nothing)
            .expect("the repository reads")
            .head
            .expect("the repository has a commit");
        std::fs::write(work.join("kept.txt"), "a\n").expect("the file is written");
        run(&work, &["commit", "-am", "second"]);
        // The upstream branch is still configured but is no longer there, so
        // git names it and refuses to say how far apart the two are.
        run(&work, &["update-ref", "-d", "refs/remotes/origin/main"]);

        let read = read(&work, "work", Since::Commit(&started_on))
            .expect("a missing upstream is not a failed read");

        assert_eq!(read.repo.ahead, None);
        assert_eq!(
            read.repo.commits[0].pushed, None,
            "git would not say, so neither does the shell"
        );
    }

    /// How many ticks of the event loop are timed against one frame's budget.
    /// The loop asks the watch once a tick and ten times a second; a hundred
    /// is ten seconds of an idle session, and several seconds of a busy one.
    const TICKS: usize = 100;

    /// One frame at 60 Hz, which is what [`niobe_tui`]'s own budget test holds
    /// a redraw to.
    const FRAME: Duration = Duration::from_millis(16);

    /// A read of this workspace takes tens of milliseconds — several frames —
    /// so the loop can never be the thing doing it. A hundred ticks' worth of
    /// asking has to come in under a single frame, while a read of a real
    /// repository is going on behind them.
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
            "{TICKS} ticks spent {spent:?} asking, over the {FRAME:?} a whole frame gets"
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
}
