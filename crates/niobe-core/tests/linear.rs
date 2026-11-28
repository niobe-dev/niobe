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

use std::sync::Mutex;
use std::time::{Duration, Instant};

use niobe_core::test_run::is_test_run;

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
