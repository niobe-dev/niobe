// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The mechanical sweep over comments: what a reader of the public repository
//! must never find in one, written down where CI can fail on it.
//!
//! A comment is read for a section banner, for a reference to planning that
//! lives outside the repository — a milestone, a task or phase number, a
//! ticket, a `§` that names no file in the tree — and for a `FIXME`, `HACK`
//! or `XXX`, which mark a gap without saying what is missing or why. Only
//! whole-line comments are read: an end-of-line comment is passed over, so
//! a string literal holding `//` in a URL cannot be mistaken for one. The
//! user-visible strings the same rule covers are review's to read.

/// Whether the file at `path`, relative to the workspace root, is swept:
/// every Rust file in the repository but the checker's own, whose tests
/// hold the very marks it looks for.
pub fn swept(path: &str) -> bool {
    path.ends_with(".rs") && !path.starts_with("xtask/")
}

/// Every mark in `source`, each as `path:line: what`, in file order.
/// `tree` is every file git would publish, relative to the workspace root:
/// the files a `§` may name.
pub fn marks_in(path: &str, source: &str, tree: &[&str]) -> Vec<String> {
    source
        .lines()
        .enumerate()
        .filter_map(|(index, line)| comment_text(line).map(|text| (index + 1, text)))
        .flat_map(|(line, text)| {
            let mut found = Vec::new();
            if is_banner(text) {
                found.push(format!("{path}:{line}: section banner"));
            }
            if names_planning(text, tree) {
                found.push(format!(
                    "{path}:{line}: reference to planning outside the repository"
                ));
            }
            if marks_a_gap(text) {
                found.push(format!(
                    "{path}:{line}: FIXME, HACK or XXX: say what is missing and why, or delete it"
                ));
            }
            found
        })
        .collect()
}

/// The text of a whole-line comment — `//`, `///` or `//!` — or `None` for a
/// line of code, an end-of-line comment included.
fn comment_text(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("//")?;
    Some(rest.strip_prefix(['/', '!']).unwrap_or(rest).trim())
}

/// Whether the comment opens with a rule of three or more banner characters:
/// `// ---- helpers ----`, `// ═════`, `////////`. A `#` is not one, so a
/// markdown heading in a doc comment is not a banner.
fn is_banner(text: &str) -> bool {
    text.chars()
        .take_while(|c| matches!(c, '-' | '─' | '━' | '=' | '═' | '*' | '/'))
        .count()
        >= 3
}

/// Whether the comment names planning that lives outside the repository: a
/// `§` after anything but a file in `tree`, a task or phase number, a
/// milestone, a ticket, or a priority label.
fn names_planning(text: &str, tree: &[&str]) -> bool {
    let section_outside_a_file = text
        .split('§')
        .take(text.matches('§').count())
        .any(|before| {
            !before
                .split_whitespace()
                .next_back()
                .is_some_and(|word| names_a_file_in(word, tree))
        });

    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let numbered = |name: &str| {
        words.windows(2).any(|pair| {
            pair[0].eq_ignore_ascii_case(name) && pair[1].starts_with(|c: char| c.is_ascii_digit())
        })
    };
    let named = |name: &str| words.iter().any(|word| word.eq_ignore_ascii_case(name));
    let priority = words.iter().any(|word| matches!(*word, "P0" | "P1" | "P2"));

    section_outside_a_file
        || numbered("task")
        || numbered("phase")
        || named("milestone")
        || named("ticket")
        || priority
}

/// Whether `word`, with the punctuation prose puts around a name taken off,
/// is a file in `tree`, by its path from the root or by the end of one. A
/// document that merely looks like one — `VISION.md`, `AGENTS.local.md` —
/// is the private planning the rule keeps out.
fn names_a_file_in(word: &str, tree: &[&str]) -> bool {
    let name =
        word.trim_matches(|c| matches!(c, '(' | ')' | '[' | ']' | '`' | '"' | '\'' | '*' | ','));
    !name.is_empty()
        && tree.iter().any(|path| {
            *path == name
                || path
                    .strip_suffix(name)
                    .is_some_and(|parent| parent.ends_with('/'))
        })
}

/// Whether the comment marks a gap with a word that says nothing about it.
fn marks_a_gap(text: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .any(|word| matches!(word, "FIXME" | "HACK" | "XXX"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The files the comments under test may name a section of.
    const TREE: &[&str] = &[
        "AGENTS.md",
        "PRINCIPLES.md",
        "crates/niobe-core/src/x.rs",
        "crates/niobe-core/tests/fixtures/README.md",
    ];

    fn marks(source: &str) -> Vec<String> {
        marks_in("crates/niobe-core/src/x.rs", source, TREE)
    }

    #[test]
    fn a_section_banner_is_a_mark() {
        assert_eq!(
            marks("fn a() {}\n// ---- helpers ----\nfn b() {}\n"),
            ["crates/niobe-core/src/x.rs:2: section banner"]
        );
        assert_eq!(
            marks("    // ───── drawing ─────\n"),
            ["crates/niobe-core/src/x.rs:1: section banner"]
        );
        assert_eq!(marks("// ======\n").len(), 1);
        assert_eq!(marks("// ***\n").len(), 1);
    }

    #[test]
    fn a_dash_in_prose_is_not_a_banner() {
        assert!(marks("// a two-tone bar -- the share\n").is_empty());
        assert!(
            marks("//! The event model — the one type every backend produces into.\n").is_empty()
        );
        assert!(marks("/// `--resume` prints the summary.\n").is_empty());
        assert!(marks("/// ### Examples\n").is_empty());
    }

    #[test]
    fn a_reference_to_planning_outside_the_repository_is_a_mark() {
        assert_eq!(
            marks("// see task 3.1 for the rest\n"),
            ["crates/niobe-core/src/x.rs:1: reference to planning outside the repository"]
        );
        assert_eq!(marks("// Milestone 02 lands this.\n").len(), 1);
        assert_eq!(marks("// phase 2 of the rollout\n").len(), 1);
        assert_eq!(marks("// ticket NB-12\n").len(), 1);
        assert_eq!(marks("// per §3.1 of the plan\n").len(), 1);
        assert_eq!(marks("// (P0 for the beta)\n").len(), 1);
    }

    #[test]
    fn a_section_of_a_document_that_is_not_in_the_tree_is_a_mark() {
        assert_eq!(
            marks("// the order VISION.md §2 sets\n"),
            ["crates/niobe-core/src/x.rs:1: reference to planning outside the repository"]
        );
        assert_eq!(marks("/// as AGENTS.local.md §1 says\n").len(), 1);
        assert_eq!(marks("// (docs/AGENTS.md §3)\n").len(), 1);
    }

    #[test]
    fn a_section_of_a_file_in_the_tree_may_be_named() {
        assert!(marks("// the rule a test may `expect` under (AGENTS.md §3)\n").is_empty());
        assert!(marks("/// PRINCIPLES.md §4 says why.\n").is_empty());
        assert!(marks("// `AGENTS.md` §6 and fixtures/README.md §2\n").is_empty());
        assert!(marks("// the task the loop hands the journal\n").is_empty());
        assert!(marks("// a multitasking terminal\n").is_empty());
        assert!(marks("// the phases of the moon\n").is_empty());
    }

    #[test]
    fn a_gap_marked_without_saying_what_is_missing_is_a_mark() {
        assert_eq!(
            marks("// FIXME: broken on tmux\n"),
            [
                "crates/niobe-core/src/x.rs:1: FIXME, HACK or XXX: say what is missing and why, or delete it"
            ]
        );
        assert_eq!(marks("// HACK until the CLI is fixed\n").len(), 1);
        assert_eq!(marks("// XXX\n").len(), 1);
        // A TODO is allowed where it says what is missing and why; that is
        // review's to read.
        assert!(marks("// TODO: the codex bridge does not spawn the CLI yet\n").is_empty());
    }

    #[test]
    fn code_and_end_of_line_comments_are_not_read() {
        assert!(marks("let url = \"https://example.com/milestone\";\n").is_empty());
        assert!(marks("let x = 1; // phase 1\n").is_empty());
        assert!(marks("    let banner = \"------\";\n").is_empty());
    }

    #[test]
    fn every_rust_file_in_the_repository_is_swept_but_the_checker() {
        assert!(swept("crates/niobe-tui/src/ui.rs"));
        assert!(swept("crates/niobe-cli/tests/pty.rs"));
        assert!(swept("crates/niobe-cli/build.rs"));
        assert!(!swept("xtask/src/audit.rs"));
        assert!(!swept("crates/niobe-tui/tests/snapshots/empty-80x24.txt"));
        assert!(!swept("Cargo.toml"));
    }
}
