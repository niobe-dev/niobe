// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A shorter copy of a cargo command's output, for the model to read in place
//! of the whole.
//!
//! Most of what `cargo test` prints is two kinds of line that say nothing a
//! reader acts on: cargo's progress, one `Compiling` line per crate built,
//! and libtest's line per test that passed. Both are left out here. Every
//! other line is kept as it was, in order, so a failure, its panic, its
//! captured output, a compiler error and its `-->` location, a warning and
//! every `test result:` line survive whole. What is left out is never read
//! again from this copy: whoever trims an output keeps the whole of it.
//!
//! Only the output of a command that runs cargo to build, check, test or run
//! what it built is trimmed. Its progress lines are told apart by the width
//! cargo pads their status word to, and libtest's lines by where it prints
//! them: between a binary's `running <n> tests` and the blank line that ends
//! its list. A line shaped like either anywhere else — a test's own output,
//! a program's — is kept. A command that runs nothing of cargo's is not
//! trimmed at all: a filter that half-knew a format would drop lines that
//! mattered. One that runs cargo beside another program (`cargo build &&
//! ./server`) is read as cargo's, so a line the other program prints in
//! cargo's shape goes too; the whole output is kept for that. One that hands
//! cargo's output to a program that picks lines out by what they say
//! (`cargo build -v 2>&1 | grep Compiling`) is not trimmed: every line left
//! is one the agent asked for, progress or not.
//!
//! A trimmed run reads the same as the whole of it: the lines
//! [`crate::test_run::counts`], [`crate::test_run::failures`] and
//! [`crate::test_run::failed`] read — where the build ended, each binary's
//! header, its `running <n> tests` and its `test result:`, the failures and
//! their lists — are all kept.

use crate::test_run::{
    RESERVED, Wrapper, cargo_subcommand, is_assignment, is_test_run, simple_commands,
};

/// The fewest bytes a trim has to save to be worth making. Below it the note
/// saying what was left out, and where the whole is, would cost about as much
/// as it saved.
pub const MIN_SAVED: usize = 1024;

/// The cargo subcommands whose output is trimmed: those that build, and run
/// what they built. In `cargo update`, `add` or `remove` the `Updating` and
/// `Adding` lines are what the command did, not its progress, and are kept
/// by leaving its output whole.
const BUILDS: &[&str] = &[
    "b", "bench", "build", "c", "check", "clippy", "d", "doc", "r", "run", "t", "test",
];

/// The programs that pick lines out of what they read by what the lines say.
/// Whatever reaches the agent through one is what it asked to read, so a
/// command that runs one is never trimmed.
const FILTERS: &[&str] = &[
    "ack", "ag", "awk", "egrep", "fgrep", "gawk", "grep", "gsed", "mawk", "rg", "sed",
];

/// The width cargo right-aligns its status words to, followed by a space.
const STATUS_WIDTH: usize = 12;

/// Cargo's status words for progress that a reader of the output has no use
/// for: what it fetched, resolved, waited on and built on the way.
/// `Finished`, `Running`, `Doc-tests` and `Executable` say which binary the
/// lines after them belong to, and are kept.
const PROGRESS: &[&str] = &[
    "Adding",
    "Blocking",
    "Checking",
    "Compiling",
    "Documenting",
    "Downloaded",
    "Downloading",
    "Fresh",
    "Locking",
    "Removing",
    "Unpacking",
    "Updating",
];

/// A cargo command's output with its progress and passing tests left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trimmed {
    text: String,
    left_out: usize,
}

impl Trimmed {
    /// The output with the lines left out, every other line as it was.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// How many lines were left out.
    pub fn left_out(&self) -> usize {
        self.left_out
    }
}

/// `output` without cargo's progress and libtest's passing tests, where
/// `command` runs cargo to build ([`BUILDS`]) and leaving them out saves at
/// least [`MIN_SAVED`] bytes. `None` for any other command, for one that
/// also runs one of the [`FILTERS`], and where too little would go.
///
/// Passing tests are left out only where `command` is a test run as
/// [`is_test_run`] reads one, so a `cargo build` keeps whatever its build
/// scripts printed.
pub fn trim(command: &str, output: &str) -> Option<Trimmed> {
    let commands = simple_commands(command);
    let builds = commands
        .iter()
        .any(|words| cargo_subcommand(words).is_some_and(|sub| BUILDS.contains(&sub)));
    let filtered = commands
        .iter()
        .any(|words| program(words).is_some_and(|name| FILTERS.contains(&name)));
    if !builds || filtered {
        return None;
    }
    let tests = is_test_run(command);
    let mut text = String::with_capacity(output.len() / 4);
    let mut left_out = 0;
    let mut listing = false;
    for line in output.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if progress(bare) || (tests && listing && passed(bare)) {
            left_out += 1;
            continue;
        }
        if tests {
            if lists_tests(bare) {
                listing = true;
            } else if bare.is_empty() {
                listing = false;
            }
        }
        text.push_str(line);
    }
    let saved = output.len().saturating_sub(text.len());
    (saved >= MIN_SAVED).then_some(Trimmed { text, left_out })
}

/// The name of the program the simple command `words` runs, read past the
/// shell's own words, assignments and the wrappers that run the command after
/// them, without the directory it was named by.
fn program(words: &[String]) -> Option<&str> {
    let mut words = words
        .iter()
        .map(String::as_str)
        .skip_while(|word| RESERVED.contains(word))
        .peekable();
    loop {
        let word = words.next()?;
        if is_assignment(word) {
            continue;
        }
        match Wrapper::named(word) {
            Some(wrapper) => {
                if !wrapper.skip_its_own(&mut words) {
                    return None;
                }
            }
            None => return word.rsplit('/').next(),
        }
    }
}

/// Whether `line` is one of cargo's progress lines: a [`PROGRESS`] word
/// right-aligned to [`STATUS_WIDTH`], then a space.
fn progress(line: &str) -> bool {
    PROGRESS.iter().any(|word| {
        let pad = STATUS_WIDTH - word.len();
        line.as_bytes()
            .get(..pad)
            .is_some_and(|lead| lead.iter().all(|b| *b == b' '))
            && line.get(pad..STATUS_WIDTH) == Some(word)
            && line.as_bytes().get(STATUS_WIDTH) == Some(&b' ')
    })
}

/// Whether `line` opens a test binary's list: `running 3 tests`, `running 1
/// test`.
fn lists_tests(line: &str) -> bool {
    line.strip_prefix("running ")
        .and_then(|rest| {
            rest.strip_suffix(" tests")
                .or_else(|| rest.strip_suffix(" test"))
        })
        .is_some_and(|count| !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `line` is libtest's line for a test that passed or was ignored.
/// One that failed, or a benchmark's, is kept.
fn passed(line: &str) -> bool {
    line.starts_with("test ")
        && (line.ends_with(" ... ok")
            || line.ends_with(" ... ignored")
            || line.contains(" ... ignored, "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing_tests(count: usize) -> String {
        (0..count)
            .map(|n| format!("test tests::case_number_{n}_holds ... ok\n"))
            .collect()
    }

    fn run(tests: &str, count: usize) -> String {
        format!(
            "   Compiling demo v0.1.0 (/work/demo)\n\
             \x20   Finished `test` profile [unoptimized + debuginfo] target(s) in 0.66s\n\
             \x20    Running unittests src/lib.rs (target/debug/deps/demo-1)\n\
             \n\
             running {count} tests\n\
             {tests}\
             \n\
             test result: ok. {count} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\
             \n"
        )
    }

    #[test]
    fn a_passing_run_keeps_its_summary_and_leaves_out_its_passing_tests_and_build_progress() {
        let output = run(&passing_tests(40), 40);

        let trimmed = trim("cargo test", &output).expect("forty passing lines are worth trimming");

        assert_eq!(
            trimmed.text(),
            "\x20   Finished `test` profile [unoptimized + debuginfo] target(s) in 0.66s\n\
             \x20    Running unittests src/lib.rs (target/debug/deps/demo-1)\n\
             \n\
             running 40 tests\n\
             \n\
             test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\
             \n"
        );
        assert_eq!(trimmed.left_out(), 41);
    }

    #[test]
    fn a_failing_test_and_an_ignored_one_with_a_reason_are_told_apart() {
        let tests = format!(
            "{}test tests::slow ... ignored, takes a minute\ntest tests::breaks ... FAILED\n",
            passing_tests(40)
        );

        let trimmed = trim("cargo test", &run(&tests, 42)).expect("worth trimming");

        assert!(trimmed.text().contains("test tests::breaks ... FAILED\n"));
        assert!(!trimmed.text().contains("tests::slow"));
    }

    #[test]
    fn a_line_shaped_like_a_passing_test_outside_a_list_is_kept() {
        let output = format!(
            "{}failures:\n\n---- tests::breaks stdout ----\ntest inner::case ... ok\n",
            run(&passing_tests(40), 40)
        );

        let trimmed = trim("cargo test", &output).expect("worth trimming");

        assert!(trimmed.text().contains("test inner::case ... ok\n"));
    }

    #[test]
    fn a_build_keeps_lines_shaped_like_tests_and_leaves_out_its_progress() {
        let progress: String = (0..40)
            .map(|n| format!("   Compiling crate_number_{n} v1.0.{n}\n"))
            .collect();
        let output = format!("{progress}running 1 test\ntest printed::by_a_build_script ... ok\n");

        let trimmed = trim("cargo build --release", &output).expect("worth trimming");

        assert_eq!(
            trimmed.text(),
            "running 1 test\ntest printed::by_a_build_script ... ok\n"
        );
    }

    #[test]
    fn a_status_word_padded_otherwise_is_kept() {
        let kept = "Compiling is what this program says it is doing\n  Checking  twice\n";
        let output = format!("{kept}{}", run(&passing_tests(40), 40));

        let trimmed = trim("cargo test", &output).expect("worth trimming");

        assert!(trimmed.text().starts_with(kept));
    }

    #[test]
    fn another_programs_output_is_never_trimmed() {
        let output = run(&passing_tests(40), 40);

        assert_eq!(trim("ls -la", &output), None);
        assert_eq!(trim("echo 'cargo test'", &output), None);
        assert_eq!(trim("cat log.txt | grep test", &output), None);
    }

    #[test]
    fn cargo_behind_a_wrapper_or_in_a_pipeline_is_still_cargo() {
        let output = run(&passing_tests(40), 40);

        assert!(
            trim(
                "RUST_BACKTRACE=1 timeout 600 cargo test 2>&1 | tail -n 400",
                &output
            )
            .is_some()
        );
        assert!(
            trim(
                "cd crates/core && /usr/bin/cargo +nightly test -p core",
                &output
            )
            .is_some()
        );
    }

    #[test]
    fn lines_a_filter_picked_out_of_cargos_output_are_never_left_out() {
        let rebuilt: String = (0..40)
            .map(|n| format!("   Compiling crate_number_{n} v1.0.{n}\n"))
            .collect();
        let listed = format!("running 40 tests\n{}", passing_tests(40));

        assert_eq!(
            trim("cargo build -v 2>&1 | grep -E 'Compiling|Fresh'", &rebuilt),
            None
        );
        assert_eq!(trim("cargo build 2>&1 | rg Compiling", &rebuilt), None);
        assert_eq!(
            trim("cargo test 2>&1 | grep -E '^(running|test )'", &listed),
            None
        );
        assert_eq!(trim("cargo test 2>&1 | sed -n '/^test /p'", &listed), None);
        assert_eq!(trim("cargo test 2>&1 | awk '/ ok$/'", &listed), None);
        assert_eq!(
            trim("cargo test 2>&1 | env LC_ALL=C grep ok", &listed),
            None
        );
    }

    #[test]
    fn what_cargo_update_changed_is_never_left_out() {
        let updated: String = (0..40)
            .map(|n| format!("    Updating crate_number_{n} v1.0.{n} -> v1.0.{}\n", n + 1))
            .collect();
        let output = format!("    Updating crates.io index\n     Locking 40 packages\n{updated}");

        assert_eq!(trim("cargo update", &output), None);
        assert_eq!(trim("cargo add serde", &output), None);
        assert_eq!(trim("cargo --list", &output), None);
        assert!(trim("cargo +nightly --locked build", &output).is_some());
    }

    #[test]
    fn an_output_that_would_lose_too_little_is_left_whole() {
        let output = run(&passing_tests(3), 3);

        assert_eq!(trim("cargo test", &output), None);
    }
}
