// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The working tree's files, arranged as the directories they are in.
//!
//! A flat list of paths in a pane forty columns wide is mostly repeated
//! directory, and the repeated part is the part the operator already knows.
//! So the files hang under a tree of their directories: each directory is
//! written once, as a node that folds what is under it away, and a file
//! carries its own name alone.
//!
//! **A directory that holds nothing but one other directory is one node with
//! it**: `niobe-bridge-claude/src/` rather than a node for each, which would
//! spend a row and an indent on a directory with nothing of its own in it.
//! Under a directory its directories come first and its files after them,
//! each in the order the repository first gave them, as a file browser lists
//! them. A file in the repository's root hangs from no node at all.
//!
//! A file git reported as a rename — `old => new` — is not split at its
//! slashes: the two halves are one claim about one file, and a tree built
//! from them would put half of it in a directory it was never in. It hangs
//! from the root, whole.
//!
//! The tree is built once for each read of the repository rather than on
//! every frame, and folding a node only skips the rows it holds, so a
//! working tree of fifty thousand files costs a frame what a screenful does.

use std::collections::HashMap;

use crate::app::WorkingFile;

/// One row of the tree, unfolded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// A directory, with what is under it.
    Dir(Dir),
    /// A file.
    File {
        /// Where it is in the repository's list of files.
        at: usize,
        /// How many directories it hangs under.
        depth: usize,
        /// Bytes of its path the directories above it already said, which
        /// its row leaves off.
        under: usize,
    },
}

/// A directory of the tree, and what the files under it add up to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dir {
    /// Its path from the repository's root, with its trailing slash: what
    /// folds it, and what tells it from another directory of the same name.
    pub path: String,
    /// What its row says: its name, and the names of the directories it was
    /// joined with, slash-ended.
    pub name: String,
    /// How many directories it hangs under.
    pub depth: usize,
    /// The row after the last one under it: what is skipped when it is
    /// folded.
    pub end: usize,
    /// How many files are under it, at any depth.
    pub files: usize,
    /// The lines the files under it add, summed, and whether every one of
    /// them said.
    pub added: (u64, bool),
    /// The lines they remove, on the same terms.
    pub removed: (u64, bool),
}

/// The tree of a list of files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tree {
    nodes: Vec<Node>,
}

impl Tree {
    /// The tree `files` hang in.
    pub fn of(files: &[WorkingFile]) -> Self {
        let mut built = Built::default();
        for (at, file) in files.iter().enumerate() {
            built.place(at, &file.path);
        }
        let mut nodes = Vec::with_capacity(files.len() + built.dirs.len());
        built.flatten(ROOT, 0, "", files, &mut nodes);
        Self { nodes }
    }

    /// Every row, as if nothing were folded.
    #[cfg(test)]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// The rows left once every directory `folded` names is folded: its own
    /// row stays, and whatever is under it does not.
    pub fn visible(&self, folded: impl Fn(&str) -> bool) -> Vec<&Node> {
        let mut shown = Vec::new();
        let mut at = 0;
        while let Some(node) = self.nodes.get(at) {
            shown.push(node);
            at = match node {
                Node::Dir(dir) if folded(&dir.path) => dir.end.max(at + 1),
                Node::Dir(_) | Node::File { .. } => at + 1,
            };
        }
        shown
    }
}

/// The root directory's place in [`Built::dirs`].
const ROOT: usize = 0;

/// The tree while it is put together: each directory's directories and
/// files in the order they first came.
struct Built<'a> {
    dirs: Vec<Building<'a>>,
    by_name: HashMap<(usize, &'a str), usize>,
}

impl Default for Built<'_> {
    fn default() -> Self {
        Self {
            dirs: vec![Building::default()],
            by_name: HashMap::new(),
        }
    }
}

/// One directory while the tree is put together.
#[derive(Default)]
struct Building<'a> {
    name: &'a str,
    dirs: Vec<usize>,
    files: Vec<usize>,
}

impl<'a> Built<'a> {
    /// Hangs the `at`th file, at `path`, under its directories.
    fn place(&mut self, at: usize, path: &'a str) {
        let mut parent = ROOT;
        if !path.contains(" => ")
            && let Some((dirs, _)) = path.rsplit_once('/')
        {
            for name in dirs.split('/') {
                parent = self.dir(parent, name);
            }
        }
        if let Some(dir) = self.dirs.get_mut(parent) {
            dir.files.push(at);
        }
    }

    /// The directory `name` under `parent`, made where it is not yet.
    fn dir(&mut self, parent: usize, name: &'a str) -> usize {
        if let Some(found) = self.by_name.get(&(parent, name)) {
            return *found;
        }
        let made = self.dirs.len();
        self.dirs.push(Building {
            name,
            ..Building::default()
        });
        self.by_name.insert((parent, name), made);
        if let Some(dir) = self.dirs.get_mut(parent) {
            dir.dirs.push(made);
        }
        made
    }

    /// Writes the rows under directory `at`, `depth` deep, whose path is
    /// `path`, into `nodes`, and returns what its files add up to.
    fn flatten(
        &self,
        at: usize,
        depth: usize,
        path: &str,
        files: &[WorkingFile],
        nodes: &mut Vec<Node>,
    ) -> Sums {
        let Some(dir) = self.dirs.get(at) else {
            return Sums::default();
        };
        let mut sums = Sums::default();
        for &child in &dir.dirs {
            let (joined, name) = self.joined(child);
            let full = format!("{path}{name}");
            let row = nodes.len();
            nodes.push(Node::File {
                at: 0,
                depth,
                under: 0,
            });
            let under = self.flatten(joined, depth + 1, &full, files, nodes);
            let end = nodes.len();
            if let Some(slot) = nodes.get_mut(row) {
                *slot = Node::Dir(Dir {
                    path: full,
                    name,
                    depth,
                    end,
                    files: under.files,
                    added: under.added,
                    removed: under.removed,
                });
            }
            sums = sums.with(under);
        }
        for &file in &dir.files {
            nodes.push(Node::File {
                at: file,
                depth,
                under: path.len(),
            });
            if let Some(file) = files.get(file) {
                sums = sums.with(Sums::of(file));
            }
        }
        sums
    }

    /// Directory `at` joined with every directory under it that is the only
    /// thing in the one above: the deepest of them, and the slash-ended
    /// names of all of them, as one row says them.
    fn joined(&self, mut at: usize) -> (usize, String) {
        let mut name = String::new();
        while let Some(dir) = self.dirs.get(at) {
            name.push_str(dir.name);
            name.push('/');
            match (dir.dirs.as_slice(), dir.files.is_empty()) {
                ([only], true) => at = *only,
                _ => break,
            }
        }
        (at, name)
    }
}

/// What a directory's files add up to.
#[derive(Debug, Clone, Copy)]
struct Sums {
    files: usize,
    added: (u64, bool),
    removed: (u64, bool),
}

impl Default for Sums {
    fn default() -> Self {
        Self {
            files: 0,
            added: (0, true),
            removed: (0, true),
        }
    }
}

impl Sums {
    /// One file's counts.
    fn of(file: &WorkingFile) -> Self {
        let side = |lines: Option<u64>| (lines.unwrap_or(0), lines.is_some());
        Self {
            files: 1,
            added: side(file.added),
            removed: side(file.removed),
        }
    }

    /// These and `other` together.
    fn with(self, other: Self) -> Self {
        let add = |a: (u64, bool), b: (u64, bool)| (a.0.saturating_add(b.0), a.1 && b.1);
        Self {
            files: self.files.saturating_add(other.files),
            added: add(self.added, other.added),
            removed: add(self.removed, other.removed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> WorkingFile {
        WorkingFile {
            path: path.to_owned(),
            added: Some(1),
            removed: Some(2),
            new: false,
        }
    }

    /// Each row as it would read: a directory's name, a file's path less
    /// what the directories above said, indented by its depth.
    fn shape(files: &[WorkingFile], nodes: &[&Node]) -> Vec<String> {
        nodes
            .iter()
            .map(|node| match node {
                Node::Dir(dir) => format!("{}{}", "  ".repeat(dir.depth), dir.name),
                Node::File { at, depth, under } => format!(
                    "{}{}",
                    "  ".repeat(*depth),
                    files[*at].path.get(*under..).unwrap_or_default()
                ),
            })
            .collect()
    }

    fn unfolded(files: &[WorkingFile]) -> Vec<String> {
        let tree = Tree::of(files);
        shape(files, &tree.visible(|_| false))
    }

    #[test]
    fn files_with_a_common_path_hang_under_it_and_say_only_their_names() {
        let files = [
            file("AGENTS.md"),
            file("crates/niobe-bridge-claude/src/driver.rs"),
            file("crates/niobe-bridge-claude/src/wire.rs"),
            file("crates/niobe-cli/tests/pty.rs"),
            file("crates/niobe-cli/tests/snapshots/usage-awaited-120x30.txt"),
            file("crates/niobe-tui/src/app.rs"),
        ];
        assert_eq!(
            unfolded(&files),
            [
                "crates/",
                "  niobe-bridge-claude/src/",
                "    driver.rs",
                "    wire.rs",
                "  niobe-cli/tests/",
                "    snapshots/",
                "      usage-awaited-120x30.txt",
                "    pty.rs",
                "  niobe-tui/src/",
                "    app.rs",
                "AGENTS.md",
            ]
        );
    }

    #[test]
    fn a_folded_directory_keeps_its_row_and_hides_everything_under_it() {
        let files = [
            file("a/b/one.rs"),
            file("a/c/two.rs"),
            file("a/three.rs"),
            file("top.rs"),
        ];
        let tree = Tree::of(&files);
        let folded = tree.visible(|path| path == "a/");
        assert_eq!(shape(&files, &folded), ["a/", "top.rs"]);
        let inner = tree.visible(|path| path == "a/b/");
        assert_eq!(
            shape(&files, &inner),
            ["a/", "  b/", "  c/", "    two.rs", "  three.rs", "top.rs"]
        );
    }

    #[test]
    fn a_directory_adds_up_the_files_under_it_and_says_where_one_was_not_counted() {
        let mut binary = file("a/b/logo.png");
        binary.added = None;
        binary.removed = None;
        let files = [file("a/b/one.rs"), binary, file("a/two.rs")];
        let tree = Tree::of(&files);
        let Some(Node::Dir(a)) = tree.nodes().first() else {
            panic!("the first row is a directory: {:?}", tree.nodes());
        };
        assert_eq!((a.path.as_str(), a.files), ("a/", 3));
        assert_eq!(a.added, (2, false), "a floor: the image said nothing");
        assert_eq!(a.removed, (4, false));
        let Some(Node::Dir(b)) = tree.nodes().get(1) else {
            panic!("the second row is a directory: {:?}", tree.nodes());
        };
        assert_eq!((b.path.as_str(), b.files, b.end), ("a/b/", 2, 4));
    }

    #[test]
    fn a_rename_hangs_whole_from_the_root() {
        let files = [file("src/{old => new}/x.rs"), file("src/lib.rs")];
        assert_eq!(
            unfolded(&files),
            ["src/", "  lib.rs", "src/{old => new}/x.rs"]
        );
    }

    #[test]
    fn two_directories_of_one_name_in_different_places_are_two_nodes() {
        let files = [file("a/src/x.rs"), file("b/src/y.rs"), file("b/z.rs")];
        let tree = Tree::of(&files);
        let paths: Vec<&str> = tree
            .nodes()
            .iter()
            .filter_map(|node| match node {
                Node::Dir(dir) => Some(dir.path.as_str()),
                Node::File { .. } => None,
            })
            .collect();
        assert_eq!(paths, ["a/src/", "b/", "b/src/"]);
    }

    #[test]
    fn nothing_changed_is_no_rows() {
        assert!(Tree::of(&[]).nodes().is_empty());
    }
}
