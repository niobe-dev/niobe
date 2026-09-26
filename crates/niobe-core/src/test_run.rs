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
//!
//! A hook that rewrites the command before it runs leaves the command the
//! agent wrote in the call, so a run whose output was replaced by another
//! tool's summary of it is still a test run, and one whose result was not
//! read: that summary is not the run's own output.
//!
//! A run whose counts cannot be read can still be known to have failed:
//! [`failed`] reads that from the start of its output and its status, which
//! is what survives when a long failing output is cut. It says nothing about
//! how many tests failed. Which did is [`failures`]: each failing binary's own
//! list, read wherever a cut or a filter left it whole, and named as that
//! binary's, never as the run's.

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

/// The tests one test binary reported failing, by name.
///
/// They are that binary's, never the run's whole list: a run that went on
/// past a failing binary, or whose output was cut, may have failed in another
/// binary too.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FailedTests {
    /// The binary, as the arguments `cargo test` said to rerun it with:
    /// `--lib`, `--test cli`, `--doc`, `-p niobe-cli --test cli`.
    pub binary: String,
    /// Its failing tests, in the order its `failures:` list gives them. Never
    /// empty.
    pub tests: Vec<String>,
}

/// Reads the failures an event carries as a list of them, or as the one list
/// a record written before every list was kept holds.
pub(crate) fn one_or_many<'de, D>(deserializer: D) -> Result<Vec<FailedTests>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Kept {
        Many(Vec<FailedTests>),
        One(FailedTests),
    }
    Ok(match Kept::deserialize(deserializer)? {
        Kept::Many(lists) => lists,
        Kept::One(list) => vec![list],
    })
}

/// Whether a shell command runs `cargo test`.
///
/// The command is read as the agent wrote it, split into the simple commands
/// that `&&`, `||`, `&`, `;`, `|`, a newline or a subshell's parentheses
/// separate. A redirection — `2>&1`, `&> log`, `>| log` — is part of the
/// command it redirects, not a separator.
/// One of them has to be `cargo test` or `cargo +<toolchain> test`, after any
/// `NAME=value` assignments and the wrappers `env`, `time` and `nice` that
/// change nothing about what is printed. A `cargo test` that builds the tests
/// and runs none — `--no-run`, `--help`, `-- --list` — is not a test run.
///
/// It does not look inside a quoted string, so `echo "cargo test"` is not one;
/// nor through any other wrapper, whose output may not be what `cargo test`
/// printed.
pub fn is_test_run(command: &str) -> bool {
    simple_commands(command)
        .into_iter()
        .any(|simple| runs_cargo_test(simple.split_whitespace()))
}

/// The simple commands in `command`, in order, split where a shell would run
/// the next one, and empty where two separators meet (`&&`, `||`).
fn simple_commands(command: &str) -> Vec<&str> {
    let bytes = command.as_bytes();
    let mut commands = Vec::new();
    let mut start = 0;
    for at in 0..bytes.len() {
        if separates(bytes, at) {
            commands.extend(command.get(start..at));
            start = at + 1;
        }
    }
    commands.extend(command.get(start..));
    commands
}

/// Whether the byte at `at` ends a simple command. An `&` that follows `>` or
/// `<` or precedes `>`, and a `|` that follows `>`, belong to a redirection:
/// `cargo test 2>&1` is one command, whose status is `cargo test`'s.
fn separates(bytes: &[u8], at: usize) -> bool {
    let before = at.checked_sub(1).and_then(|i| bytes.get(i)).copied();
    let after = bytes.get(at + 1).copied();
    match bytes.get(at) {
        Some(b'&') => !matches!(before, Some(b'>' | b'<')) && after != Some(b'>'),
        Some(b'|') => before != Some(b'>'),
        Some(b';' | b'\n' | b'(' | b')') => true,
        _ => false,
    }
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

/// Whether a `cargo test` run failed after its tests started, as far as what
/// is left of its output shows, even where its counts cannot be read.
///
/// That is a run that exited `101` whose output shows the build finishing and
/// a test binary starting after it. The start of a run is what survives when
/// the output is cut from the middle or from the end, so this holds where the
/// failures list and the summaries were lost. A build that failed exits `101`
/// as well and runs no tests, so the build has to be seen finishing, and an
/// output saying a crate could not compile is never a failed run.
///
/// The status has to be `cargo test`'s own, so `command` must end in it: the
/// status of `cargo test && cargo clippy` may be clippy's, which also exits
/// `101`. Nothing here says how many tests failed, or which.
pub fn failed(command: &str, output: &str, exit_code: Option<i32>) -> bool {
    exit_code == Some(FAILED_STATUS) && ends_in_cargo_test(command) && tests_started(output)
}

/// The tests each failing binary in `output` named, in the order they ran,
/// wherever that binary's list is whole.
///
/// libtest ends a failing binary's block with a `failures:` line, a line per
/// failing test, a blank line and its `test result: FAILED.` summary, and
/// cargo then says which binary it was: `error: test failed, to rerun pass
/// `--test cli``, with only rustdoc's timing line between for the doc-tests.
/// A list is named only where all of that is there, line for line, and it
/// holds as many tests as the summary says failed, so a list the CLI or a
/// `| tail` cut into, or one whose summary was lost, is not mistaken for a
/// shorter one. Empty wherever no list survives.
///
/// A whole list proves by itself that its binary failed, so it is read
/// whatever the command exited with: the status of `cargo test | tail -30` is
/// `tail`'s. It is read from anywhere in the output, which is what survives
/// when a failed command's output is cut from the middle or filtered. Nothing
/// in it says how many tests the run as a whole ran or failed, nor that no
/// other binary failed: one whose list was lost is not in it.
pub fn failures(output: &str) -> Vec<FailedTests> {
    let lines: Vec<&str> = output.lines().map(str::trim_end).collect();
    (0..lines.len())
        .filter_map(|at| failures_closed_at(&lines, at))
        .collect()
}

/// The failures list whose summary is `lines[at]`, where it is whole and the
/// binary is named after it.
fn failures_closed_at(lines: &[&str], at: usize) -> Option<FailedTests> {
    let (before, from) = lines.split_at_checked(at)?;
    let (closing, after) = from.split_first()?;
    let summary = Summary::of(closing).filter(|summary| summary.failed > 0)?;
    let binary = after
        .iter()
        .find(|line| !line.is_empty() && !is_doc_tests_timing(line))
        .and_then(|line| rerun_as(line))?;
    let (blank, list) = before.split_last()?;
    if !blank.is_empty() {
        return None;
    }
    let opened = list.iter().rposition(|line| *line == "failures:")?;
    let tests: Vec<String> = list[opened + 1..]
        .iter()
        .map(|line| failing_test(line).map(str::to_owned))
        .collect::<Option<_>>()?;
    (tests.len() as u64 == summary.failed).then_some(FailedTests {
        binary: binary.to_owned(),
        tests,
    })
}

/// rustdoc's `all doctests ran in 0.87s; merged doctests compilation took
/// 0.38s`, which an edition 2024 crate's doc-tests print between their
/// summary and the line cargo names them with.
fn is_doc_tests_timing(line: &str) -> bool {
    line.starts_with("all doctests ran in ")
}

/// The name on one line of a `failures:` list, which libtest indents by four.
fn failing_test(line: &str) -> Option<&str> {
    line.strip_prefix("    ")
        .filter(|name| !name.is_empty() && !name.starts_with(' '))
}

/// The arguments in cargo's `error: test failed, to rerun pass `--lib``, or
/// its `doctest failed` for the doc-tests.
fn rerun_as(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("error: test failed, to rerun pass `")
        .or_else(|| line.strip_prefix("error: doctest failed, to rerun pass `"))?;
    rest.strip_suffix('`')
        .filter(|args| !args.is_empty() && !args.contains('`'))
}

/// The status `cargo test` exits with when a test failed.
const FAILED_STATUS: i32 = 101;

/// Whether the last simple command in `command` is `cargo test`, so that the
/// status the whole command exited with is the run's.
fn ends_in_cargo_test(command: &str) -> bool {
    simple_commands(command)
        .into_iter()
        .rfind(|simple| !simple.trim().is_empty())
        .is_some_and(|simple| runs_cargo_test(simple.split_whitespace()))
}

/// Whether an output shows a build finishing and a test binary starting after
/// it, and no crate failing to compile.
fn tests_started(output: &str) -> bool {
    let mut built = false;
    let mut started = false;
    for line in output.lines() {
        if line.trim_start().starts_with("error: could not compile ") {
            return false;
        }
        match Line::of(line) {
            Line::Finished => built = true,
            Line::Header => started |= built,
            Line::Running(_) | Line::Result(_) | Line::Other => {}
        }
    }
    started
}

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
    fn a_summary_another_tool_printed_in_place_of_the_run_is_not_read() {
        // What came back, verbatim, for `cargo test -p niobe-core --lib -q`
        // run under a hook that rewrote the command before it ran; the call
        // still named `cargo test`.
        let replaced = "cargo test: 111 passed (1 suite, 0.00s)";
        assert_eq!(counts(replaced, Some(0)), None);
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
            "cargo test &> log.txt",
            "cargo test >| log.txt",
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

    /// A long failing run as the Claude CLI hands it over: its first part, a
    /// line in place of the characters cut out, and a part that stops
    /// mid-line and holds neither a failure nor the last summary.
    fn cut(output: &str) -> String {
        let (head, _) = output
            .split_once("test tests::adds ... ok")
            .expect("the recording has a passing test");
        format!("{head}test tests::add\n\n... [20014 characters truncated] ...\n\nsult: ok. 1 pas")
    }

    #[test]
    fn a_run_that_exited_failing_after_its_tests_started_failed_though_it_was_cut() {
        let kept = cut(FAILED);
        assert_eq!(counts(&kept, Some(101)), None, "no count survives the cut");
        assert!(failed("cargo test --workspace", &kept, Some(101)));
        assert!(failed("cd crates/x && cargo test", &kept, Some(101)));
    }

    #[test]
    fn a_cut_failing_run_whose_output_is_redirected_failed() {
        let kept = cut(FAILED);
        for command in [
            "cargo test",
            "cargo test 2>&1",
            "cargo test --workspace 2>&1",
            "cargo test &> log.txt",
            "cargo test &>> log.txt",
            "cargo test >& log.txt",
            "cargo test > log.txt 2>&1",
            "cargo test 2<&1",
            "cargo test >| log.txt",
        ] {
            assert!(failed(command, &kept, Some(101)), "{command:?}");
        }
    }

    #[test]
    fn a_whole_failing_run_failed_too() {
        assert!(failed("cargo test", FAILED, Some(101)));
        assert!(failed("cargo test", FAILED_EVERY_BINARY, Some(101)));
    }

    #[test]
    fn a_build_that_failed_is_not_a_failed_run() {
        assert!(!failed("cargo test", BUILD_FAILED, Some(101)));
        // Two runs in one command, the first passing and the second not
        // building: the status is the second's, and it ran no tests.
        let then_unbuilt = format!("{PASSED}{BUILD_FAILED}");
        assert!(!failed(
            "cargo test && cargo test -p other",
            &then_unbuilt,
            Some(101)
        ));
    }

    #[test]
    fn a_run_that_did_not_exit_failing_did_not_fail() {
        let kept = cut(FAILED);
        for status in [None, Some(0), Some(1), Some(143)] {
            assert!(!failed("cargo test", &kept, status), "{status:?}");
        }
    }

    #[test]
    fn a_status_that_may_be_another_commands_says_nothing_about_the_run() {
        let kept = cut(PASSED);
        for command in [
            "cargo test && cargo clippy --all-targets -- -D warnings",
            "cargo test; cargo build",
            "cargo test 2>&1 | tail -20",
            "cargo test |& tail -20",
            "cargo test & cargo build",
            "cargo build",
        ] {
            assert!(!failed(command, &kept, Some(101)), "{command:?}");
        }
    }

    #[test]
    fn a_failing_runs_whole_failures_list_is_named_as_its_binarys() {
        let named = failures(FAILED);
        assert_eq!(named.len(), 1, "the list and its summary are whole");
        assert_eq!(named[0].binary, "--lib");
        assert_eq!(named[0].tests, ["tests::wrong"]);
    }

    #[test]
    fn a_failures_list_is_named_where_a_cut_left_the_end_of_the_run() {
        let (_, end) = FAILED
            .split_once("test tests::adds ... ok")
            .expect("the recording has a passing test");
        let kept =
            format!("    Finished `test` profile\n\n... [9000 characters truncated] ...\n\n{end}");
        assert_eq!(
            failures(&kept),
            [FailedTests {
                binary: "--lib".to_owned(),
                tests: vec!["tests::wrong".to_owned()],
            }]
        );
    }

    /// [`FAILED_EVERY_BINARY`] with the integration test failing too.
    fn failed_in_two_binaries() -> String {
        FAILED_EVERY_BINARY
            .replace(
                "test from_outside ... ok\n\ntest result: ok. 1 passed; 0 failed",
                "test from_outside ... FAILED\n\nfailures:\n\nfailures:\n    from_outside\n\n\
                 test result: FAILED. 0 passed; 1 failed",
            )
            .replace(
                "finished in 0.01s\n\n   Doc-tests",
                "finished in 0.01s\n\nerror: test failed, to rerun pass `--test api`\n   Doc-tests",
            )
    }

    #[test]
    fn a_run_that_kept_going_names_every_failing_binarys_list_in_order() {
        let named = failures(&failed_in_two_binaries());
        let whose: Vec<(&str, &[String])> = named
            .iter()
            .map(|list| (list.binary.as_str(), list.tests.as_slice()))
            .collect();
        assert_eq!(
            whose,
            [
                ("--lib", &["tests::wrong".to_owned()][..]),
                ("--test api", &["from_outside".to_owned()][..]),
            ]
        );
    }

    #[test]
    fn a_tailed_failing_run_names_the_list_its_tail_kept() {
        // `cargo test 2>&1 | tail -8` of FAILED: the status is tail's, and the
        // list proves itself.
        let lines: Vec<&str> = FAILED.lines().collect();
        let tail = lines[lines.len() - 8..].join("\n");
        assert_eq!(counts(&tail, Some(0)), None);
        assert!(!failed("cargo test 2>&1 | tail -8", &tail, Some(0)));
        assert_eq!(
            failures(&tail),
            [FailedTests {
                binary: "--lib".to_owned(),
                tests: vec!["tests::wrong".to_owned()],
            }]
        );
    }

    #[test]
    fn a_tail_that_cut_into_the_list_names_nothing() {
        let two = failed_in_two_binaries();
        let (_, from) = two
            .split_once("failures:\n    tests::wrong\n")
            .expect("the first binary's list is in the recording");
        let cut_into = format!("    tests::wrong\n{from}");
        let named = failures(&cut_into);
        assert_eq!(
            named
                .iter()
                .map(|list| list.binary.as_str())
                .collect::<Vec<_>>(),
            ["--test api"],
            "the list the tail opened inside is not named"
        );
    }

    // The end of a failing doc-test, recorded from `cargo test` 1.91 on an
    // edition 2024 crate: rustdoc says how long its merged doc-tests took
    // between the summary and the line naming the binary.
    const DOC_FAILED: &str = "failures:
    src/lib.rs - two (line 1)

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s

all doctests ran in 0.87s; merged doctests compilation took 0.38s
error: doctest failed, to rerun pass `--doc`
";

    #[test]
    fn a_doc_test_that_failed_is_named_with_the_doc_tests() {
        let named = failures(DOC_FAILED);
        assert_eq!(named.len(), 1, "a doc-test's list is whole");
        assert_eq!(named[0].binary, "--doc");
        assert_eq!(named[0].tests, ["src/lib.rs - two (line 1)"]);

        let earlier = DOC_FAILED.replace("all doctests ran in", "all doctests took");
        assert_eq!(
            failures(&earlier),
            [],
            "only rustdoc's own line comes between"
        );
    }

    #[test]
    fn a_list_that_is_not_whole_names_nothing() {
        let short = FAILED.replace("3 passed; 1 failed", "2 passed; 2 failed");
        assert_eq!(failures(&short), [], "the summary counts two, the list one");
        let cut_into = FAILED.replace(
            "failures:\n    tests::wrong",
            "failures:\n    tests::wr\n\n... [300 characters truncated] ...\n\nong",
        );
        assert_eq!(failures(&cut_into), []);
        let headless = FAILED.replace("\nfailures:\n    tests::wrong", "\n    tests::wrong");
        assert_eq!(
            failures(&headless),
            [],
            "no `failures:` line opens the list"
        );
    }

    #[test]
    fn a_list_with_no_binary_to_name_names_nothing() {
        let (unnamed, _) = FAILED
            .split_once("error: test failed")
            .expect("the recording names the binary");
        assert_eq!(failures(unnamed), []);
        let malformed = FAILED.replace("finished in 0.00s\n\nerror", "finished in soon\n\nerror");
        assert_eq!(failures(&malformed), [], "the summary is not libtest's");
    }

    #[test]
    fn a_passing_run_or_one_cut_before_its_end_names_nothing() {
        assert_eq!(failures(PASSED), []);
        assert_eq!(failures(&cut(FAILED)), []);
        assert_eq!(failures(BUILD_FAILED), []);
    }

    #[test]
    fn an_unrelated_commands_output_is_not_read() {
        let listing = "total 24\ndrwxr-xr-x  5 op  staff  160 Sep 25 10:00 .\n-rw-r--r--  1 op  staff  42 Sep 25 10:00 Cargo.toml\n";
        assert_eq!(counts(listing, Some(0)), None);
    }
}
