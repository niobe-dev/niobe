// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The readers of a shell command and of what it printed take time linear in
//! what they are handed: a command or an output four times as long takes
//! about four times as long to read, not sixteen.
//!
//! A command is read on the shell's draw path for every shell call shown
//! running, and an output as large as the 64 MiB the bridge reads of one the
//! CLI spilled, so a reader that goes back over what it read for every
//! character or line holds the screen or the fold up for seconds.
//!
//! Each test reads a short input and one four times as long in turn, eleven
//! times, and compares the fastest of each: load on the machine only ever
//! adds time, so the fastest is the reading it touched least, and reading
//! the two in turn puts a burst of load on both. The tests take turns on a
//! lock so that neither times the other.
//!
//! They are timed only in an optimised build,
//! `cargo test --release -p niobe-core --test linear`, which the gate runs
//! as it runs the frame and replay budgets: the debug suite reports them
//! ignored, since there they time the other test binaries building and
//! running beside them as much as the readers.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use niobe_core::test_run::{failures, is_test_run};

/// Held by each test while it times, so that the two never share the cores.
static ALONE: Mutex<()> = Mutex::new(());

/// How many times each input is read. The fastest of them is the one
/// compared.
const READINGS: usize = 11;

/// The ratio the longer reading's time may reach over the shorter's: linear
/// is 4 and quadratic 16, and noise on a loaded machine lands above 4.
const RATIO: f64 = 8.0;

/// The fastest of [`READINGS`] runs of `read` over `short` and over `long`,
/// in turn.
fn fastest(short: &str, long: &str, read: impl Fn(&str)) -> (Duration, Duration) {
    let time = |input: &str| {
        let started = Instant::now();
        read(input);
        started.elapsed()
    };
    (0..READINGS).fold((Duration::MAX, Duration::MAX), |(fast, slow), _| {
        (fast.min(time(short)), slow.min(time(long)))
    })
}

fn assert_linear(what: &str, short: &str, long: &str, read: impl Fn(&str)) {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (fast, slow) = fastest(short, long, read);
    let ratio = slow.as_secs_f64() / fast.as_secs_f64().max(f64::EPSILON);
    assert!(
        ratio < RATIO,
        "{what}: {} bytes took {fast:?} and {} bytes {slow:?}, {ratio:.1} times as long, \
         where linear is 4 and quadratic 16",
        short.len(),
        long.len()
    );
}

/// `echo` followed by `n` unquoted `>`, each of which a shell reads as part
/// of one redirection operator being written.
fn redirections(n: usize) -> String {
    format!("echo {}", ">".repeat(n))
}

#[test]
#[cfg_attr(debug_assertions, ignore = "a reader is timed in an optimised build")]
fn a_command_of_many_redirection_characters_is_read_in_time_linear_in_its_length() {
    assert_linear(
        "a run of `>`",
        &redirections(4_000),
        &redirections(16_000),
        |command| assert!(!is_test_run(command)),
    );
    let ampersands = |n: usize| format!("cargo test {}", "&>".repeat(n));
    assert_linear(
        "a run of `&>`",
        &ampersands(2_000),
        &ampersands(8_000),
        |command| assert!(is_test_run(command)),
    );
}

/// libtest's summary of a binary in which one test failed.
const FAILED_SUMMARY: &str = "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; \
                              0 filtered out; finished in 0.00s";

/// `n` failing summaries, each after a blank line, with no `failures:` list
/// before any of them.
fn summaries(n: usize) -> String {
    format!("\n{FAILED_SUMMARY}\n").repeat(n)
}

/// A `failures:` list of `n` names that no summary closes, and `n` failing
/// summaries after it, each after a blank line.
fn names_then_summaries(n: usize) -> String {
    format!(
        "failures:\n{}{}",
        "    tests::wrong\n".repeat(n),
        summaries(n)
    )
}

/// `n` sets of doc-tests, each a whole list of one failing test that no
/// line after the sets names a binary for.
fn doc_test_sets(n: usize) -> String {
    format!("running 1 test\nfailures:\n    src/lib.rs - add (line 3)\n\n{FAILED_SUMMARY}\n")
        .repeat(n)
}

#[test]
#[cfg_attr(debug_assertions, ignore = "a reader is timed in an optimised build")]
fn failing_summaries_with_no_list_are_read_in_time_linear_in_the_output() {
    assert_linear(
        "failing summaries",
        &summaries(2_500),
        &summaries(10_000),
        |output| assert_eq!(failures(output), []),
    );
}

#[test]
#[cfg_attr(debug_assertions, ignore = "a reader is timed in an optimised build")]
fn a_long_unclosed_list_and_the_summaries_after_it_are_read_in_time_linear_in_the_output() {
    assert_linear(
        "a list and summaries",
        &names_then_summaries(2_500),
        &names_then_summaries(10_000),
        |output| assert_eq!(failures(output), []),
    );
}

#[test]
#[cfg_attr(debug_assertions, ignore = "a reader is timed in an optimised build")]
fn doc_test_sets_no_binary_is_named_for_are_read_in_time_linear_in_the_output() {
    assert_linear(
        "doc-test sets",
        &doc_test_sets(1_000),
        &doc_test_sets(4_000),
        |output| assert_eq!(failures(output), []),
    );
}
