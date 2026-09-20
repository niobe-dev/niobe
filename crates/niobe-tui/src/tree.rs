// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The working tree's files, arranged for reading.
//!
//! A flat list of paths in a pane forty columns wide is mostly repeated
//! directory, and the repeated part is the part the operator already knows. So
//! a directory is written once, dimmed, and the files under it carry what is
//! left of their paths and their counts.
//!
//! **A group is one top-level directory, and its row is the longest directory
//! prefix every file in it shares.** A child keeps whatever tail the prefix did
//! not take, which can itself have slashes in it — `docs/` above
//! `adr/0004-etags.md` rather than a second group row for one file. Files that
//! changed in the repository root have no directory to sit under and group
//! under [`ROOT`].
//!
//! The alternative was a group per parent directory, with a basename beneath.
//! It is simpler, and it was rejected on a real diff: a change spread over
//! `docs/`, `docs/adr/` and `docs/diagrams/` spends three of its rows saying
//! `docs` and the pane is more directory than file.

use crate::app::WorkingFile;

/// What the repository root is called when files have changed directly in it.
///
/// A file with no directory still needs a group to sit under, or it would be
/// the one row in the list whose indentation meant nothing.
pub const ROOT: &str = ".";

/// One row of the working-tree list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row<'a> {
    /// A directory, written once above the files in it. It carries no counts
    /// of its own: a total beside a directory would be a fourth number about
    /// the same files, and the section's header already carries the whole.
    Dir(String),
    /// A file, and how much of its path the directory above already said.
    File {
        /// The file itself.
        file: &'a WorkingFile,
        /// Bytes of its path the group row took, which the row skips.
        under: usize,
    },
}

impl Row<'_> {
    /// What a file row draws: the path with the group's prefix taken off.
    ///
    /// A file git reported as a rename — `old => new` — keeps the whole of it:
    /// the two halves are one claim about one file, and a prefix taken off the
    /// front of it would say only half of what moved.
    pub fn name(&self) -> &str {
        match self {
            Row::Dir(dir) => dir,
            Row::File { file, under } => match file.path.contains(" => ") {
                true => &file.path,
                false => file.path.get(*under..).unwrap_or(&file.path),
            },
        }
    }
}

/// The files, grouped under the directories they are in, in the order the
/// repository reported them.
pub fn grouped(files: &[WorkingFile]) -> Vec<Row<'_>> {
    let mut rows = Vec::with_capacity(files.len() + files.len() / 4);

    for group in groups(files) {
        let prefix = shared_prefix(&group);
        rows.push(Row::Dir(match prefix.is_empty() {
            true => ROOT.to_owned(),
            false => prefix.clone(),
        }));
        rows.extend(group.into_iter().map(|file| Row::File {
            file,
            under: prefix.len(),
        }));
    }
    rows
}

/// The files split by the top-level directory they are under, keeping the
/// order the repository gave and the order the directories first appear in.
///
/// Git lists a diff in path order, so a top-level directory arrives as one run
/// and this is a walk rather than a search. A list that is not in path order
/// still groups correctly; it is only the group rows that may then repeat.
fn groups(files: &[WorkingFile]) -> Vec<Vec<&WorkingFile>> {
    let mut groups: Vec<(&str, Vec<&WorkingFile>)> = Vec::new();

    for file in files {
        let top = file.path.split_once('/').map_or(ROOT, |(top, _)| top);
        match groups.last_mut().filter(|(at, _)| *at == top) {
            Some((_, group)) => group.push(file),
            None => groups.push((top, vec![file])),
        }
    }
    groups.into_iter().map(|(_, group)| group).collect()
}

/// The longest directory prefix, trailing slash included, that every file in
/// `group` begins with. Empty where they share no directory at all.
fn shared_prefix(group: &[&WorkingFile]) -> String {
    let Some(first) = group.first() else {
        return String::new();
    };
    // Each candidate is one directory deeper than the last, and the deepest
    // one every path starts with is the answer.
    let mut prefix = String::new();
    for (at, _) in first.path.match_indices('/') {
        let candidate = &first.path[..=at];
        if !group.iter().all(|file| file.path.starts_with(candidate)) {
            break;
        }
        prefix = candidate.to_owned();
    }
    prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> WorkingFile {
        WorkingFile {
            path: path.to_owned(),
            added: Some(1),
            removed: Some(0),
        }
    }

    fn shape(rows: &[Row<'_>]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Dir(_) => format!("[{}]", row.name()),
                Row::File { .. } => row.name().to_owned(),
            })
            .collect()
    }

    #[test]
    fn a_directory_is_written_once_and_its_files_keep_what_is_left() {
        let files = [
            file("docs/caching.md"),
            file("docs/adr/0004-etags.md"),
            file("docs/diagrams/cache.png"),
        ];

        assert_eq!(
            shape(&grouped(&files)),
            [
                "[docs/]",
                "caching.md",
                "adr/0004-etags.md",
                "diagrams/cache.png",
            ],
            "a second group row for one file is a row spent saying `docs` again"
        );
    }

    #[test]
    fn a_group_takes_the_deepest_prefix_every_file_in_it_shares() {
        let files = [
            file("crates/niobe-tui/src/app.rs"),
            file("crates/niobe-tui/src/ui.rs"),
        ];

        assert_eq!(
            shape(&grouped(&files)),
            ["[crates/niobe-tui/src/]", "app.rs", "ui.rs"]
        );
    }

    #[test]
    fn one_file_out_of_the_run_shortens_the_prefix_for_the_whole_group() {
        let files = [
            file("crates/niobe-tui/src/app.rs"),
            file("crates/niobe-cli/src/main.rs"),
        ];

        assert_eq!(
            shape(&grouped(&files)),
            ["[crates/]", "niobe-tui/src/app.rs", "niobe-cli/src/main.rs"]
        );
    }

    #[test]
    fn a_file_in_the_repository_root_sits_under_a_group_of_its_own() {
        let files = [file("CHANGELOG.md"), file("docs/friction.md")];

        assert_eq!(
            shape(&grouped(&files)),
            ["[.]", "CHANGELOG.md", "[docs/]", "friction.md"]
        );
    }

    #[test]
    fn the_order_the_repository_gave_is_the_order_the_rows_are_in() {
        // Not re-sorted: the operator checks this list against their own
        // `git`, and a pane that quietly reorders it is a pane they cannot.
        let files = [file("b/two.rs"), file("a/one.rs"), file("b/three.rs")];

        assert_eq!(
            shape(&grouped(&files)),
            ["[b/]", "two.rs", "[a/]", "one.rs", "[b/]", "three.rs"]
        );
    }

    #[test]
    fn a_renamed_file_keeps_both_halves_of_what_git_called_it() {
        let files = [
            file("tests/fixtures/a.json => tests/fixtures/b.json"),
            file("tests/fixtures/c.json"),
        ];
        let rows = grouped(&files);

        assert_eq!(
            rows[1].name(),
            "tests/fixtures/a.json => tests/fixtures/b.json",
            "half a rename names neither file"
        );
        assert_eq!(rows[2].name(), "c.json");
    }

    #[test]
    fn nothing_changed_is_no_rows_at_all() {
        assert!(grouped(&[]).is_empty());
    }
}
