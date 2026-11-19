// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A commit read whole, laid out for its dialog as `git show --format=fuller`
//! lays one out, with the lines each file gained and lost below it, and each
//! part in the tone it is drawn in.

use crate::app::{CommitRead, WorkingFile};
use crate::detail::Tone;

/// A line of the dialog, in pieces of the tones they are drawn in.
type Toned = Vec<(String, Tone)>;

/// How wide the labels before who made a commit and when are, so their
/// values line up as git lines them up: `AuthorDate: ` is the widest.
const LABEL: usize = 12;

/// How far a commit's message is indented under who made it, as git does.
const INDENT: &str = "    ";

/// The lines of the dialog for the commit named `id`, read as `read` says,
/// or saying why it could not be.
pub(crate) fn lines(id: &str, read: &Result<CommitRead, String>) -> Vec<Toned> {
    let read = match read {
        Ok(read) => read,
        Err(why) => {
            return vec![vec![(
                format!("git could not read this commit: {why}"),
                Tone::Failed,
            )]];
        }
    };
    let mut lines = vec![vec![piece("commit ", Tone::Dim), piece(id, Tone::Hash)]];
    if read.merged.len() > 1 {
        lines.push(vec![label("Merge:"), (read.merged.join(" "), Tone::Hash)]);
    }
    for (said, who, when) in [
        ("Author", &read.author, &read.authored),
        ("Commit", &read.committer, &read.committed),
    ] {
        lines.push(vec![
            label(&format!("{said}:")),
            piece(&who.name, Tone::Plain),
            (format!(" <{}>", who.email), Tone::Dim),
        ]);
        lines.push(vec![
            label(&format!("{said}Date:")),
            piece(when, Tone::Hint),
        ]);
    }
    let message = read.message.trim_end();
    if !message.is_empty() {
        lines.push(Vec::new());
        lines.extend(message_lines(message));
    }
    if let Some(kept) = read.message_cut {
        lines.push(left_out(kept));
    }
    lines.push(Vec::new());
    lines.extend(file_lines(&read.files));
    if let Some(kept) = read.files_cut {
        lines.push(left_out(kept));
    }
    lines
}

/// A piece of a line, owned.
fn piece(said: &str, tone: Tone) -> (String, Tone) {
    (said.to_owned(), tone)
}

/// `said` as a label before a value, padded so the values line up.
fn label(said: &str) -> (String, Tone) {
    (format!("{said:<LABEL$}"), Tone::Dim)
}

/// The message, each line indented but a blank one; its first, the
/// subject, drawn as the heading of the rest, with the kind of change a
/// conventional prefix names pointed out before it.
fn message_lines(message: &str) -> impl Iterator<Item = Toned> + '_ {
    message.split('\n').enumerate().map(|(at, line)| {
        if line.is_empty() {
            return Vec::new();
        }
        let mut pieces = vec![piece(INDENT, Tone::Plain)];
        match at {
            0 => {
                let rest = crate::ui::unprefixed(line);
                let kind = &line[..line.len() - rest.len()];
                if !kind.is_empty() {
                    pieces.push(piece(kind.trim_end(), Tone::Hint));
                    pieces.push(piece(" ", Tone::Plain));
                }
                pieces.push(piece(rest, Tone::Strong));
            }
            _ => pieces.push(piece(line, Tone::Plain)),
        }
        pieces
    })
}

/// What the dialog says where it left the rest of a read out, having kept
/// `kept` bytes of it.
fn left_out(kept: usize) -> Toned {
    vec![(
        format!(
            "… the rest is left out: past {} MiB of it",
            kept / (1024 * 1024)
        ),
        Tone::Dim,
    )]
}

/// `2 files changed  +31 −9`, then a row each of the lines a file gained and
/// lost and its path, the counts lined up. A binary file, which git counts no
/// lines in, has an em dash for each: no lines counted is not none.
fn file_lines(files: &[WorkingFile]) -> Vec<Toned> {
    let count = |n: Option<u64>| n.map_or_else(|| "—".to_owned(), |n| n.to_string());
    let sum = |n: fn(&WorkingFile) -> Option<u64>| {
        files.iter().filter_map(n).fold(0u64, u64::saturating_add)
    };
    let (added, removed) = (sum(|file| file.added), sum(|file| file.removed));
    let mut lines = vec![vec![
        (
            format!(
                "{} {} changed  ",
                files.len(),
                if files.len() == 1 { "file" } else { "files" }
            ),
            Tone::Strong,
        ),
        counted('+', &added.to_string(), Some(added), Tone::Added),
        piece(" ", Tone::Plain),
        counted('−', &removed.to_string(), Some(removed), Tone::Removed),
    ]];
    let rows: Vec<(String, String, &WorkingFile)> = files
        .iter()
        .map(|file| (count(file.added), count(file.removed), file))
        .collect();
    let widest = |cells: &mut dyn Iterator<Item = &String>| {
        cells
            .map(|cell| cell.chars().count() + 1)
            .max()
            .unwrap_or(0)
    };
    let plus = widest(&mut rows.iter().map(|row| &row.0));
    let minus = widest(&mut rows.iter().map(|row| &row.1));
    for (gained, lost, file) in &rows {
        lines.push(vec![
            (
                " ".repeat(2 + plus - gained.chars().count() - 1),
                Tone::Plain,
            ),
            counted('+', gained, file.added, Tone::Added),
            (
                " ".repeat(1 + minus - lost.chars().count() - 1),
                Tone::Plain,
            ),
            counted('−', lost, file.removed, Tone::Removed),
            piece("  ", Tone::Plain),
            piece(&file.path, Tone::Plain),
        ]);
    }
    lines
}

/// `sign` and `said`, a count of lines, in `tone` where `n` changed any, and
/// dim where it changed none or git counted none.
fn counted(sign: char, said: &str, n: Option<u64>, tone: Tone) -> (String, Tone) {
    let tone = match n {
        Some(0) | None => Tone::Dim,
        Some(_) => tone,
    };
    (format!("{sign}{said}"), tone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Person;

    const ID: &str = "7c11388bd55cb62cc1f45bc996f8fe75d6c68ca5";

    fn file(path: &str, added: Option<u64>, removed: Option<u64>) -> WorkingFile {
        WorkingFile {
            path: path.to_owned(),
            added,
            removed,
            new: false,
        }
    }

    fn read() -> CommitRead {
        let person = Person {
            name: "A Test".to_owned(),
            email: "test@example.invalid".to_owned(),
        };
        CommitRead {
            merged: Vec::new(),
            author: person.clone(),
            authored: "Tue 2096-10-02 07:06".to_owned(),
            committer: person,
            committed: "Wed 2096-10-03 08:00".to_owned(),
            message: "feat(tui): say more\n\nThe body,\n\twhole.\n".to_owned(),
            files: vec![
                file("src/long.rs", Some(120), Some(3)),
                file("logo.png", None, None),
                file("README.md", Some(1), Some(0)),
            ],
            message_cut: None,
            files_cut: None,
        }
    }

    fn text(lines: &[Toned]) -> String {
        lines
            .iter()
            .map(|line| {
                line.iter()
                    .map(|(said, _)| said.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The tone of the piece of `lines` that says exactly `said`.
    fn tone_of(lines: &[Toned], said: &str) -> Option<Tone> {
        lines
            .iter()
            .flatten()
            .find(|(piece, _)| piece == said)
            .map(|(_, tone)| *tone)
    }

    /// A commit reads as `git show --format=fuller` shows it, its message
    /// indented under who made it, then the files it changed, the counts
    /// lined up and summed above them; a binary file, which git counts no
    /// lines in, reads as an em dash rather than a zero.
    #[test]
    fn a_commit_reads_as_git_shows_it_with_its_files_below() {
        assert_eq!(
            text(&lines(ID, &Ok(read()))),
            [
                &format!("commit {ID}"),
                "Author:     A Test <test@example.invalid>",
                "AuthorDate: Tue 2096-10-02 07:06",
                "Commit:     A Test <test@example.invalid>",
                "CommitDate: Wed 2096-10-03 08:00",
                "",
                "    feat(tui): say more",
                "",
                "    The body,",
                "    \twhole.",
                "",
                "3 files changed  +121 −3",
                "  +120 −3  src/long.rs",
                "    +— −—  logo.png",
                "    +1 −0  README.md",
            ]
            .join("\n")
        );
    }

    /// The lines a file gained and lost are drawn in a diff's colours, so
    /// they read at a glance against the paths; a count of none, and one git
    /// could not count, say less than one that changed anything. The name,
    /// the kind of change and the times stand out from the labels.
    #[test]
    fn the_counts_are_in_a_diffs_colours_and_the_commits_parts_in_their_own() {
        let lines = lines(ID, &Ok(read()));

        assert_eq!(tone_of(&lines, "+120"), Some(Tone::Added));
        assert_eq!(tone_of(&lines, "−3"), Some(Tone::Removed));
        assert_eq!(tone_of(&lines, "+121"), Some(Tone::Added));
        assert_eq!(tone_of(&lines, "−0"), Some(Tone::Dim));
        assert_eq!(tone_of(&lines, "+—"), Some(Tone::Dim));
        assert_eq!(tone_of(&lines, "src/long.rs"), Some(Tone::Plain));
        assert_eq!(tone_of(&lines, ID), Some(Tone::Hash));
        assert_eq!(tone_of(&lines, "feat(tui):"), Some(Tone::Hint));
        assert_eq!(tone_of(&lines, "say more"), Some(Tone::Strong));
        assert_eq!(tone_of(&lines, "AuthorDate: "), Some(Tone::Dim));
        assert_eq!(tone_of(&lines, "Tue 2096-10-02 07:06"), Some(Tone::Hint));
        assert_eq!(tone_of(&lines, "A Test"), Some(Tone::Plain));
        assert_eq!(tone_of(&lines, " <test@example.invalid>"), Some(Tone::Dim));
        assert_eq!(tone_of(&lines, "3 files changed  "), Some(Tone::Strong));
        assert!(
            lines
                .iter()
                .flatten()
                .filter(|(_, tone)| matches!(tone, Tone::Added | Tone::Removed))
                .all(|(said, _)| !said.starts_with(' ')),
            "the padding that lines the counts up is not drawn in their colours: {lines:?}"
        );
    }

    /// A subject with no conventional prefix is drawn whole as the subject.
    #[test]
    fn a_subject_with_no_kind_is_the_subject_whole() {
        let read = CommitRead {
            message: "Say more".to_owned(),
            ..read()
        };

        assert_eq!(
            tone_of(&lines(ID, &Ok(read)), "Say more"),
            Some(Tone::Strong)
        );
    }

    /// A merge says which commits it joined, as git does.
    #[test]
    fn a_merge_names_the_commits_it_joined() {
        let read = CommitRead {
            merged: vec!["abc1234".to_owned(), "def5678".to_owned()],
            ..read()
        };

        let lines = lines(ID, &Ok(read));

        assert_eq!(text(&lines[1..2]), "Merge:      abc1234 def5678");
        assert_eq!(tone_of(&lines, "abc1234 def5678"), Some(Tone::Hash));
    }

    /// A read cut short says so where it was cut, after the message or
    /// after the files, and by how much was kept.
    #[test]
    fn a_read_cut_short_says_the_rest_is_left_out_where_it_was_cut() {
        let read = CommitRead {
            message_cut: Some(4 * 1024 * 1024),
            files_cut: Some(4 * 1024 * 1024),
            ..read()
        };

        let text = text(&lines(ID, &Ok(read)));

        assert!(
            text.contains("    \twhole.\n… the rest is left out: past 4 MiB of it\n\n3 files"),
            "{text}"
        );
        assert!(
            text.ends_with("README.md\n… the rest is left out: past 4 MiB of it"),
            "{text}"
        );
    }

    /// A commit with no message and no files still says who made it, and
    /// that it changed nothing.
    #[test]
    fn a_commit_with_no_message_and_no_files_says_who_made_it() {
        let read = CommitRead {
            message: String::new(),
            files: Vec::new(),
            ..read()
        };

        let text = text(&lines(ID, &Ok(read)));

        assert!(
            text.ends_with("CommitDate: Wed 2096-10-03 08:00\n\n0 files changed  +0 −0"),
            "{text}"
        );
    }

    /// Why a commit could not be read is said as a failure.
    #[test]
    fn a_commit_git_could_not_read_says_why_as_a_failure() {
        let lines = lines(ID, &Err("fatal: bad object".to_owned()));

        assert_eq!(
            lines,
            [vec![(
                "git could not read this commit: fatal: bad object".to_owned(),
                Tone::Failed
            )]]
        );
    }
}
