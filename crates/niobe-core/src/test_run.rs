// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What a test run the session made reported about itself.
//!
//! Niobe runs no tests of its own. When the agent runs a shell command that
//! is a test run, a bridge reads the counts out of the run's own summary here,
//! so that every backend recognises the same commands and reads the same
//! output the same way.
//!
//! One format is recognised: **`cargo test`**, which prints a libtest summary
//! per test binary. Every other runner — `cargo nextest`, `pytest`, `jest`,
//! `go test` — is not a test run as far as this module is concerned, and
//! nothing is shown for it. A parser that half-recognised a format and
//! reported a wrong count would be worse than showing nothing.
//!
//! The counts are read only from an output that holds the whole run. `cargo
//! test` prints, once the build has finished, one block per test binary: a
//! header naming the binary, `running <n> tests`, a line per test and a
//! `test result:` line whose counts add up to `<n>`. An output is read only
//! where it starts at the end of the build and every block in it is whole —
//! which is what an agent's `| tail -20`, `| grep "test result"` or a run cut
//! off part-way is not. Those leave a run that happened and whose result was
//! not read, never a total of the suites that happened to survive the filter.
//!
//! Nor does the output say when the run is over: `cargo test` prints nothing
//! after its last binary, so a run killed between two binaries reads as a
//! whole run of fewer. What says it finished is the status it exited with,
//! which has to agree with the counts — `0` for a run with no failures, `101`
//! for one with any — or the counts are not read.

use serde::{Deserialize, Serialize};

/// What a whole `cargo test` run reported, summed over its test binaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TestCounts {
    /// Tests that passed.
    pub passed: u64,
    /// Tests that failed. A run with any is a failing run.
    pub failed: u64,
    /// Tests that were skipped because they are marked `#[ignore]`.
    pub ignored: u64,
    /// Test binaries that ran to their summary: unit tests, each integration
    /// test file and the doc-tests are one each.
    pub suites: u64,
}

impl TestCounts {
    /// Whether the run reported any test failing.
    pub fn failing(&self) -> bool {
        self.failed > 0
    }
}

/// Whether a shell command runs `cargo test`.
///
/// The command is read as the agent wrote it, split into the simple commands
/// that `&&`, `||`, `;`, `|`, a newline or a subshell's parentheses separate.
/// One of them has to be `cargo test` or `cargo +<toolchain> test`, after any
/// `NAME=value` assignments and the wrappers `env`, `time` and `nice` that
/// change nothing about what is printed. A `cargo test` that builds the tests
/// and runs none — `--no-run`, `--help`, `-- --list` — is not a test run.
///
/// It does not look inside a quoted string, so `echo "cargo test"` is not one;
/// nor through any other wrapper, whose output may not be what `cargo test`
/// printed.
pub fn is_test_run(command: &str) -> bool {
    command
        .split(['&', '|', ';', '\n', '(', ')'])
        .any(|simple| runs_cargo_test(simple.split_whitespace()))
}

fn runs_cargo_test<'a>(mut words: impl Iterator<Item = &'a str>) -> bool {
    let mut word = words.next();
    while let Some(w) = word
        && (is_assignment(w) || matches!(w, "env" | "time" | "nice"))
    {
        word = words.next();
    }
    if word != Some("cargo") {
        return false;
    }
    let mut word = words.next();
    if let Some(toolchain) = word
        && toolchain.starts_with('+')
    {
        word = words.next();
    }
    word == Some("test") && words.all(|w| !matches!(w, "--no-run" | "--help" | "-h" | "--list"))
}

/// `NAME=value`, where the name is one a shell would take as a variable.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The counts a `cargo test` run printed, summed over its test binaries, as
/// long as the status it exited with says it ran to the end.
///
/// `None` wherever the output does not hold the whole run: no line saying the
/// build finished before the first binary ran, a binary that started and did
/// not report, a summary whose counts do not add up to the tests it said it
/// was running, a summary with no binary above it, or no binary at all. A
/// build that failed ran no tests, and is `None` too. So is a run whose
/// status is not known or disagrees with its counts: `cargo test` exits `0`
/// where no test failed and `101` where one did, and anything else — a
/// signal, a status nobody reported — is a run that may have stopped between
/// two binaries.
///
/// `output` is what the command printed, with its standard error in it: the
/// headers that say which binary is running are written there.
pub fn counts(output: &str, exit_code: Option<i32>) -> Option<TestCounts> {
    let counts = whole_run(output)?;
    match (exit_code, counts.failing()) {
        (Some(0), false) | (Some(FAILED_STATUS), true) => Some(counts),
        _ => None,
    }
}

/// The status `cargo test` exits with when a test failed.
const FAILED_STATUS: i32 = 101;

/// The counts in an output that holds a whole run, however it ended.
fn whole_run(output: &str) -> Option<TestCounts> {
    let mut state = Block::Building;
    let mut counts = TestCounts {
        passed: 0,
        failed: 0,
        ignored: 0,
        suites: 0,
    };
    for line in output.lines() {
        state = match (state, Line::of(line)) {
            (_, Line::Other) => state,
            (Block::Building | Block::Between, Line::Finished) => Block::Between,
            (Block::Between, Line::Header) => Block::Headed,
            (Block::Headed, Line::Running(tests)) => Block::Running(tests),
            (Block::Running(tests), Line::Result(result)) if result.total() == Some(tests) => {
                counts.passed = counts.passed.saturating_add(result.passed);
                counts.failed = counts.failed.saturating_add(result.failed);
                counts.ignored = counts.ignored.saturating_add(result.ignored);
                counts.suites = counts.suites.saturating_add(1);
                Block::Between
            }
            // A test's own output can say anything, including words that look
            // like these, but only as a whole line with nothing around it
            // would it be mistaken for one; that is a run nobody can read.
            (Block::Building | Block::Between | Block::Headed | Block::Running(_), _) => {
                return None;
            }
        };
    }
    (state == Block::Between && counts.suites > 0).then_some(counts)
}

/// Where in a `cargo test` run the output has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    /// Nothing has said the build is over.
    Building,
    /// Between two test binaries, or after the last.
    Between,
    /// A binary's header, and nothing it printed yet.
    Headed,
    /// A binary running this many tests, with no summary yet.
    Running(u64),
}

/// The lines of a `cargo test` output that say where the run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Line {
    /// `    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.31s`
    Finished,
    /// `     Running unittests src/lib.rs (target/debug/deps/…)` or
    /// `   Doc-tests demo`.
    Header,
    /// `running 5 tests`.
    Running(u64),
    /// `test result: ok. 4 passed; 0 failed; …`.
    Result(Summary),
    /// Anything else: the build, a test's name, what a test printed.
    Other,
}

impl Line {
    fn of(line: &str) -> Line {
        let line = line.trim_end();
        // Cargo right-aligns its own status words, so they are indented; the
        // test harness's lines are not.
        let (indented, words) = (line.starts_with(' '), line.trim_start());
        match indented {
            true => {
                if words.starts_with("Finished ") && words.contains(" target(s) in ") {
                    Line::Finished
                } else if is_header(words) {
                    Line::Header
                } else {
                    Line::Other
                }
            }
            false => running(line)
                .map(Line::Running)
                .or_else(|| Summary::of(line).map(Line::Result))
                .unwrap_or(Line::Other),
        }
    }
}

/// `Running <what> (<binary>)` or `Doc-tests <crate>`.
fn is_header(words: &str) -> bool {
    match words.strip_prefix("Running ") {
        Some(rest) => rest.contains(" (") && rest.ends_with(')'),
        None => words
            .strip_prefix("Doc-tests ")
            .is_some_and(|name| !name.is_empty() && !name.contains(' ')),
    }
}

/// The count in `running <n> tests`, or `running 1 test`.
fn running(line: &str) -> Option<u64> {
    let rest = line.strip_prefix("running ")?;
    let (count, noun) = rest.split_once(' ')?;
    let count: u64 = count.parse().ok()?;
    let expected = match count {
        1 => "test",
        _ => "tests",
    };
    (noun == expected).then_some(count)
}

/// One test binary's `test result:` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Summary {
    passed: u64,
    failed: u64,
    ignored: u64,
    measured: u64,
}

impl Summary {
    /// `test result: ok. 4 passed; 0 failed; 1 ignored; 0 measured; 0 filtered
    /// out; finished in 0.00s`, read field by field and refused whole where
    /// any field is not where libtest puts it or the verdict disagrees with
    /// the failures.
    fn of(line: &str) -> Option<Summary> {
        let rest = line.strip_prefix("test result: ")?;
        let (verdict, rest) = rest.split_once(". ")?;
        let mut fields = rest.split("; ");
        let mut field = |name: &str| -> Option<u64> {
            fields
                .next()?
                .strip_suffix(name)?
                .strip_suffix(' ')?
                .parse()
                .ok()
        };
        let summary = Summary {
            passed: field("passed")?,
            failed: field("failed")?,
            ignored: field("ignored")?,
            measured: field("measured")?,
        };
        field("filtered out")?;
        let finished = fields.next()?.strip_prefix("finished in ")?;
        finished.strip_suffix('s')?.parse::<f64>().ok()?;
        if fields.next().is_some() {
            return None;
        }
        match (verdict, summary.failed) {
            ("ok", 0) => Some(summary),
            ("FAILED", 1..) => Some(summary),
            _ => None,
        }
    }

    /// The tests the binary ran, which is what its `running <n> tests` line
    /// announced.
    fn total(&self) -> Option<u64> {
        self.passed
            .checked_add(self.failed)?
            .checked_add(self.ignored)?
            .checked_add(self.measured)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded from `cargo test` 1.91 on a crate with five unit tests (one
    // ignored), one integration test and one doc-test, with its standard
    // error in the output as a shell tool captures it.
    const PASSED: &str = "    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.01s
     Running unittests src/lib.rs (target/debug/deps/demo-760e00b68511d171)

running 5 tests
test tests::slow ... ignored
test tests::adds_zero ... ok
test tests::adds ... ok
test tests::hangs ... ok
test tests::wrong ... ok

test result: ok. 4 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/api.rs (target/debug/deps/api-60c95a5809a23973)

running 1 test
test from_outside ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests demo

running 1 test
test src/lib.rs - add (line 3) ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s

";

    // The same crate with one unit test failing: cargo stops at the binary
    // that failed and runs no other.
    const FAILED: &str = "    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.00s
     Running unittests src/lib.rs (target/debug/deps/demo-760e00b68511d171)

running 5 tests
test tests::slow ... ignored
test tests::adds ... ok
test tests::adds_zero ... ok
test tests::hangs ... ok
test tests::wrong ... FAILED

failures:

---- tests::wrong stdout ----

thread 'tests::wrong' (397895991) panicked at src/lib.rs:14:66:
assertion `left == right` failed
  left: 4
 right: 5
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    tests::wrong

test result: FAILED. 3 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `--lib`
";

    // The same failure under `--no-fail-fast`, which runs every binary and
    // says between them which failed.
    const FAILED_EVERY_BINARY: &str =
        "    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.00s
     Running unittests src/lib.rs (target/debug/deps/demo-760e00b68511d171)

running 5 tests
test tests::slow ... ignored
test tests::adds_zero ... ok
test tests::adds ... ok
test tests::hangs ... ok
test tests::wrong ... FAILED

failures:

failures:
    tests::wrong

test result: FAILED. 3 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `--lib`
     Running tests/api.rs (target/debug/deps/api-60c95a5809a23973)

running 1 test
test from_outside ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

   Doc-tests demo

running 1 test
test src/lib.rs - add (line 3) ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s

error: 1 target failed:
    `--lib`
";

    // The same crate with one unit test that hangs, killed three seconds in.
    const INTERRUPTED: &str =
        "    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.04s
     Running unittests src/lib.rs (target/debug/deps/demo-760e00b68511d171)

running 5 tests
test tests::adds ... ok
test tests::slow ... ignored
test tests::adds_zero ... ok
test tests::wrong ... ok
";

    // The last lines of a run of several binaries, as `| tail -8` leaves it:
    // whole summaries, and no way to tell how many binaries came before.
    const TAILED: &str = "running 1 test
test from_outside ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests demo

running 1 test
test src/lib.rs - add (line 3) ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
";

    const BUILD_FAILED: &str = "   Compiling demo v0.1.0 (/work/demo)
error[E0308]: mismatched types
 --> src/lib.rs:7:52
  |
7 | pub fn add(a: u64, b: u64) -> u64 { a + b; }
  |                                            ^ expected `u64`, found `()`

error: could not compile `demo` (lib test) due to 1 previous error
";

    fn counted(passed: u64, failed: u64, ignored: u64, suites: u64) -> Option<TestCounts> {
        Some(TestCounts {
            passed,
            failed,
            ignored,
            suites,
        })
    }

    #[test]
    fn a_clean_pass_is_read_as_the_sum_of_its_binaries() {
        assert_eq!(counts(PASSED, Some(0)), counted(6, 0, 1, 3));
    }

    #[test]
    fn a_run_with_a_failure_is_read_as_failing_with_its_own_count() {
        let read = counts(FAILED, Some(101)).expect("a failing run's summary is whole");
        assert_eq!(Some(read), counted(3, 1, 1, 1));
        assert!(read.failing());
    }

    #[test]
    fn a_run_that_kept_going_past_a_failure_counts_every_binary() {
        assert_eq!(counts(FAILED_EVERY_BINARY, Some(101)), counted(5, 1, 1, 3));
    }

    #[test]
    fn an_interrupted_run_is_not_read() {
        assert_eq!(counts(INTERRUPTED, None), None);
    }

    #[test]
    fn a_run_interrupted_after_a_whole_binary_is_not_read_as_that_binary() {
        let (before, _) = PASSED
            .split_once("test result: ok. 1 passed")
            .expect("the recording has a second binary");
        assert_eq!(counts(before, Some(0)), None);
    }

    #[test]
    fn a_run_killed_between_two_binaries_is_not_read_as_the_binaries_before() {
        // Recorded: cargo killed while the first binary ran, which went on to
        // finish and print its summary into the same output. The next two
        // binaries never ran, and the output alone cannot say so.
        let orphaned = format!(
            "{INTERRUPTED}test tests::hangs has been running for over 60 seconds\n\
             test tests::hangs ... ok\n\n\
             test result: ok. 4 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; \
             finished in 60.01s\n\n"
        );
        assert_eq!(counts(&orphaned, Some(142)), None);
        assert_eq!(counts(&orphaned, None), None);
    }

    #[test]
    fn counts_the_status_disagrees_with_are_not_read() {
        assert_eq!(counts(PASSED, Some(101)), None);
        assert_eq!(counts(FAILED, Some(0)), None);
        assert_eq!(counts(FAILED, Some(1)), None);
    }

    #[test]
    fn a_filtered_run_is_not_read_as_the_summaries_that_survived() {
        assert_eq!(counts(TAILED, Some(0)), None);
    }

    #[test]
    fn a_build_that_failed_ran_no_tests_and_is_not_read() {
        assert_eq!(counts(BUILD_FAILED, Some(101)), None);
    }

    #[test]
    fn a_run_cut_out_of_its_middle_is_not_read() {
        let cut = PASSED.replace("running 1 test\ntest from_outside ... ok\n", "");
        assert_eq!(counts(&cut, Some(0)), None);
    }

    #[test]
    fn a_summary_that_does_not_add_up_to_its_binary_is_not_read() {
        let wrong = PASSED.replace("running 5 tests", "running 6 tests");
        assert_eq!(counts(&wrong, Some(0)), None);
    }

    #[test]
    fn a_verdict_that_disagrees_with_the_failures_is_not_read() {
        let wrong = FAILED.replace("test result: FAILED.", "test result: ok.");
        assert_eq!(counts(&wrong, Some(0)), None);
    }

    #[test]
    fn output_with_no_test_binary_in_it_is_not_read() {
        assert_eq!(counts("", Some(0)), None);
        assert_eq!(
            counts(
                "    Finished `test` profile [unoptimized] target(s) in 0.01s\n",
                Some(0)
            ),
            None
        );
    }

    #[test]
    fn a_line_ending_in_a_carriage_return_reads_as_the_line() {
        assert_eq!(
            counts(&PASSED.replace('\n', "\r\n"), Some(0)),
            counted(6, 0, 1, 3)
        );
    }

    #[test]
    fn cargo_test_is_a_test_run_however_it_is_invoked() {
        for command in [
            "cargo test",
            "cargo test -p niobe-core --lib",
            "cargo +nightly test",
            "cd crates/x && cargo test 2>&1 | tail -20",
            "RUST_BACKTRACE=1 cargo test",
            "env RUST_LOG=debug cargo test",
            "time cargo test --workspace",
            "(cd x && cargo test)",
            "cargo fmt --all\ncargo test",
            "cargo test -- --nocapture",
        ] {
            assert!(is_test_run(command), "{command:?} runs the tests");
        }
    }

    #[test]
    fn a_command_that_runs_no_tests_is_not_a_test_run() {
        for command in [
            "ls -la",
            "cargo build",
            "cargo testing",
            "cargo clippy --all-targets",
            "cargo test --no-run",
            "cargo test --help",
            "cargo test -- --list",
            "echo \"cargo test\"",
            "echo cargo test",
            "grep -rn 'cargo test' AGENTS.md",
            "cargo xtask ci",
            "cargo nextest run",
            "pytest",
            "",
        ] {
            assert!(!is_test_run(command), "{command:?} runs no tests");
        }
    }

    #[test]
    fn an_unrelated_commands_output_is_not_read() {
        let listing = "total 24\ndrwxr-xr-x  5 op  staff  160 Sep 25 10:00 .\n-rw-r--r--  1 op  staff  42 Sep 25 10:00 Cargo.toml\n";
        assert_eq!(counts(listing, Some(0)), None);
    }
}
