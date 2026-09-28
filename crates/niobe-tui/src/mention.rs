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
    let line = lines.get(row)?;
    let before = line.get(..byte_of_column(line, column)?)?;
    let word = before.rsplit(char::is_whitespace).next()?;
    let typed = word.strip_prefix('@')?;
    Some(Mention {
        row,
        at: column - word.chars().count(),
        typed: typed.to_owned(),
    })
}

/// Where in `line` the character at `column` starts, or its end where
/// `column` is just past its last character; `None` further out.
///
/// Asked on every key typed into the prompt, so a line of plain ASCII, where
/// a column is a byte, is answered without walking its characters; nothing
/// here copies the line, which would cost its length in allocations for
/// every key typed into it.
fn byte_of_column(line: &str, column: usize) -> Option<usize> {
    if line.is_ascii() {
        return (column <= line.len()).then_some(column);
    }
    line.char_indices()
        .map(|(byte, _)| byte)
        .chain(std::iter::once(line.len()))
        .nth(column)
}

/// The listed files, lowercased once, and the last list ranked from them.
///
/// A repository can list hundreds of thousands of files, and the list under
/// an `@` word is asked for on every key and drawn on every frame; lowering
/// every path and ranking them all each time cost 70 ms a key over 200,000
/// paths. So the lowercased paths are made when the listing changes, and a
/// ranking is kept for the text it was ranked for, which is what a frame
/// with nothing newly typed asks for again.
#[derive(Debug, Default)]
pub(crate) struct Files {
    /// Each file's path lowercased, and where its name starts in it.
    lowered: Vec<(String, usize)>,
    /// The text last ranked for, and what it ranked: indices into the files.
    ranked: std::cell::RefCell<Option<(String, Vec<usize>)>>,
}

impl Files {
    /// The index of `files`.
    pub(crate) fn new(files: &[String]) -> Self {
        let lowered = files
            .iter()
            .map(|path| {
                let lower = path.to_lowercase();
                let name = lower.rfind('/').map_or(0, |slash| slash + 1);
                (lower, name)
            })
            .collect();
        Self {
            lowered,
            ranked: std::cell::RefCell::new(None),
        }
    }

    /// Up to `limit` of `files` — the ones this index was made of — that
    /// `typed` could name, the likeliest first, ranked as [`candidates`]
    /// ranks them.
    pub(crate) fn candidates<'a>(
        &self,
        files: &'a [String],
        typed: &str,
        limit: usize,
    ) -> Vec<&'a str> {
        let mut ranked = self.ranked.borrow_mut();
        let fresh = match ranked.as_ref() {
            Some((was, _)) => was != typed,
            None => true,
        };
        if fresh {
            *ranked = Some((typed.to_owned(), self.rank(files, typed, limit)));
        }
        ranked
            .as_ref()
            .map(|(_, at)| {
                at.iter()
                    .filter_map(|at| files.get(*at).map(String::as_str))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn rank(&self, files: &[String], typed: &str, limit: usize) -> Vec<usize> {
        let typed = typed.to_lowercase();
        let mut ranked: Vec<(u8, usize, usize, &str, usize)> = self
            .lowered
            .iter()
            .zip(files)
            .enumerate()
            .filter_map(|(at, ((lower, name), path))| {
                let rank = match_rank(&lower[*name..], lower, &typed)?;
                let depth = path.matches('/').count();
                Some((rank, depth, path.chars().count(), path.as_str(), at))
            })
            .collect();
        ranked.sort_unstable();
        ranked.into_iter().take(limit).map(|(.., at)| at).collect()
    }
}

/// How well a file named `name`, at `path`, both lowercased, matches `typed`:
/// its name starting with it, holding it, or its directories holding it.
fn match_rank(name: &str, path: &str, typed: &str) -> Option<u8> {
    if name.starts_with(typed) {
        Some(0)
    } else if name.contains(typed) {
        Some(1)
    } else if path.contains(typed) {
        Some(2)
    } else {
        None
    }
}

/// Up to `limit` of `files` that `typed` could name, the likeliest first,
/// ranked from scratch: the plain statement of the ranking, which [`Files`]
/// is held to agree with.
///
/// A file matches when its path holds what was typed, ignoring case. A file
/// whose name starts with it comes first, then one whose name holds it, then
/// one whose directories do; within each, the shallower file and then the
/// shorter path first, since the operator reaching for a deep file keeps
/// typing.
#[cfg(test)]
pub(crate) fn candidates<'a>(files: &'a [String], typed: &str, limit: usize) -> Vec<&'a str> {
    let typed = typed.to_lowercase();
    let mut ranked: Vec<(u8, usize, usize, &str)> = files
        .iter()
        .filter_map(|path| {
            let lower = path.to_lowercase();
            let name = lower.rsplit('/').next().unwrap_or(&lower);
            let rank = match_rank(name, &lower, &typed)?;
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
        assert_eq!(
            at_cursor(&lines("@src"), (0, 9)),
            None,
            "a cursor past the end of the line"
        );
    }

    #[test]
    fn columns_count_characters_not_bytes() {
        assert_eq!(
            at_cursor(&lines("café\u{3000}@ünï then"), (0, 9)),
            Some(Mention {
                row: 0,
                at: 5,
                typed: "ünï".to_owned(),
            })
        );
        assert_eq!(
            at_cursor(&lines("@über more"), (0, 3)).map(|m| m.typed),
            Some("üb".to_owned())
        );
        assert_eq!(at_cursor(&lines("@é"), (0, 3)), None, "past the line");
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

    #[test]
    fn the_index_ranks_as_the_plain_ranking_does_and_keeps_its_last_answer() {
        let files: Vec<String> = [
            "src/Main.rs",
            "src/app/main_menu.rs",
            "docs/main/readme.md",
            "tests/domain.rs",
            "README.md",
        ]
        .iter()
        .map(|path| (*path).to_owned())
        .collect();
        let index = Files::new(&files);

        for typed in ["", "main", "MAIN", "ma", "readme", "zzz", "src/"] {
            assert_eq!(
                index.candidates(&files, typed, 3),
                candidates(&files, typed, 3),
                "{typed:?}"
            );
            // Asked again, as a frame with nothing newly typed asks.
            assert_eq!(
                index.candidates(&files, typed, 3),
                candidates(&files, typed, 3)
            );
        }
    }
}
