// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Naming a file in a prompt with `@`.
//!
//! The shell reads no directory: the files it completes from are the ones the
//! binary listed from the repository and handed over in [`crate::app::Repo`].
//! What is here is the part that needs no filesystem — which word of the
//! prompt is being completed, and which of the listed files it could be.
//!
//! An `@` begins a file only where it begins a word, at the start of a line or
//! after a space, which is what keeps an address such as `ops@example.com`
//! from opening a list.

/// The `@` word that ends at the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mention {
    /// The line of the prompt it is on.
    pub(crate) row: usize,
    /// The column of its `@`, in characters.
    pub(crate) at: usize,
    /// What has been typed after the `@`, up to the cursor.
    pub(crate) typed: String,
}

/// The `@` word the cursor at `cursor` — a line and a column in characters —
/// is at the end of, if it is at the end of one.
pub(crate) fn at_cursor(lines: &[String], cursor: (usize, usize)) -> Option<Mention> {
    let (row, column) = cursor;
    let line: Vec<char> = lines.get(row)?.chars().collect();
    let before = line.get(..column)?;
    let at = before
        .iter()
        .rposition(|c| c.is_whitespace())
        .map_or(0, |space| space + 1);
    match before.get(at..)? {
        ['@', typed @ ..] => Some(Mention {
            row,
            at,
            typed: typed.iter().collect(),
        }),
        _ => None,
    }
}

/// Up to `limit` of `files` that `typed` could name, the likeliest first.
///
/// A file matches when its path holds what was typed, ignoring case. A file
/// whose name starts with it comes first, then one whose name holds it, then
/// one whose directories do; within each, the shallower file and then the
/// shorter path first, since the operator reaching for a deep file keeps
/// typing.
pub(crate) fn candidates<'a>(files: &'a [String], typed: &str, limit: usize) -> Vec<&'a str> {
    let typed = typed.to_lowercase();
    let mut ranked: Vec<(u8, usize, usize, &str)> = files
        .iter()
        .filter_map(|path| {
            let lower = path.to_lowercase();
            let name = lower.rsplit('/').next().unwrap_or(&lower);
            let rank = if name.starts_with(&typed) {
                0
            } else if name.contains(&typed) {
                1
            } else if lower.contains(&typed) {
                2
            } else {
                return None;
            };
            let depth = path.matches('/').count();
            Some((rank, depth, path.chars().count(), path.as_str()))
        })
        .collect();
    ranked.sort_unstable();
    ranked
        .into_iter()
        .take(limit)
        .map(|(_, _, _, path)| path)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn an_at_that_begins_a_word_is_a_file_being_named() {
        assert_eq!(
            at_cursor(&lines("look at @src/ma"), (0, 15)),
            Some(Mention {
                row: 0,
                at: 8,
                typed: "src/ma".to_owned(),
            })
        );
        assert_eq!(
            at_cursor(&lines("@"), (0, 1)),
            Some(Mention {
                row: 0,
                at: 0,
                typed: String::new(),
            })
        );
        assert_eq!(
            at_cursor(&lines("first\nthen @x"), (1, 7)).map(|m| (m.row, m.at)),
            Some((1, 5))
        );
    }

    #[test]
    fn an_at_inside_a_word_or_behind_the_cursor_is_not() {
        assert_eq!(at_cursor(&lines("ops@example.com"), (0, 15)), None);
        assert_eq!(at_cursor(&lines("@src done"), (0, 9)), None);
        assert_eq!(at_cursor(&lines("no at here"), (0, 10)), None);
    }

    #[test]
    fn a_file_whose_name_starts_with_what_was_typed_comes_first() {
        let files: Vec<String> = [
            "crates/app/src/ui.rs",
            "docs/build.md",
            "build.rs",
            "crates/build/lib.rs",
            "README.md",
        ]
        .map(str::to_owned)
        .to_vec();

        assert_eq!(
            candidates(&files, "BUILD", 10),
            ["build.rs", "docs/build.md", "crates/build/lib.rs"]
        );
        assert_eq!(candidates(&files, "build", 1), ["build.rs"]);
        assert_eq!(candidates(&files, "nothing", 10), Vec::<&str>::new());
    }

    #[test]
    fn nothing_typed_offers_the_shallowest_files() {
        let files: Vec<String> = ["a/b/c.rs", "top.rs", "a/mid.rs"]
            .map(str::to_owned)
            .to_vec();

        assert_eq!(candidates(&files, "", 2), ["top.rs", "a/mid.rs"]);
    }
}
