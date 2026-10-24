// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Trims the recorded cargo outputs in `tests/fixtures/trim/` and checks what
//! is left: that it is much shorter, and that nothing a reader acts on went.

use niobe_core::test_run;
use niobe_core::trim::trim;

/// A recorded output, the command that printed it and the status it exited
/// with. `tests/fixtures/trim/README.md` says how each was recorded.
struct Recorded {
    name: &'static str,
    command: &'static str,
    status: i32,
    output: &'static str,
}

const CORPUS: &[Recorded] = &[
    Recorded {
        name: "passing_cold",
        command: "cargo test -p niobe-core 2>&1",
        status: 0,
        output: include_str!("fixtures/trim/passing_cold.txt"),
    },
    Recorded {
        name: "failing",
        command: "cargo test --no-fail-fast 2>&1",
        status: 101,
        output: include_str!("fixtures/trim/failing.txt"),
    },
    Recorded {
        name: "compile_error",
        command: "cargo build 2>&1",
        status: 101,
        output: include_str!("fixtures/trim/compile_error.txt"),
    },
    Recorded {
        name: "check_cold",
        command: "cargo check -p niobe-config 2>&1",
        status: 0,
        output: include_str!("fixtures/trim/check_cold.txt"),
    },
];

/// Whether `line` is one a reader of the output acts on, which no trim may
/// leave out. A passing test's line is not, whatever its name says.
fn telling(line: &str) -> bool {
    let passed = line.starts_with("test ") && line.ends_with(" ... ok");
    !passed
        && [
            "error",
            "warning",
            "-->",
            "panicked",
            "FAILED",
            "failures:",
            "---- ",
            "test result:",
            "Running",
            "Doc-tests",
            "Finished",
            "running ",
        ]
        .iter()
        .any(|mark| line.contains(mark))
}

#[test]
fn the_test_and_build_runs_lose_at_least_sixty_percent_of_their_bytes() {
    let (whole, trimmed) = CORPUS
        .iter()
        .filter_map(|run| trim(run.command, run.output).map(|t| (run.output.len(), t.text().len())))
        .fold((0, 0), |(w, t), (one_w, one_t)| (w + one_w, t + one_t));

    // 37,625 bytes recorded become 2,571, a 93.2% reduction: the sums `wc -c`
    // gives for the commands in the fixtures' README, which leave the same
    // lines out with `grep`.
    assert_eq!((whole, trimmed), (37_625, 2_571));
    assert!(
        trimmed * 100 <= whole * 40,
        "{trimmed} of {whole} bytes kept"
    );
}

#[test]
fn a_trimmed_run_keeps_every_line_a_reader_acts_on_in_order() {
    for run in CORPUS {
        let Some(trimmed) = trim(run.command, run.output) else {
            continue;
        };
        let kept: Vec<&str> = trimmed.text().lines().collect();
        let telling: Vec<&str> = run.output.lines().filter(|line| telling(line)).collect();
        let mut rest = kept.iter();
        for line in &telling {
            assert!(
                rest.any(|k| k == line),
                "{}: {line:?} was left out or moved",
                run.name
            );
        }
    }
}

#[test]
fn a_trimmed_run_names_the_same_failures_and_fails_the_same_way() {
    for run in CORPUS {
        let Some(trimmed) = trim(run.command, run.output) else {
            continue;
        };
        assert_eq!(
            test_run::failures(trimmed.text()),
            test_run::failures(run.output),
            "{}",
            run.name
        );
        assert_eq!(
            test_run::failed(run.command, trimmed.text(), Some(run.status)),
            test_run::failed(run.command, run.output, Some(run.status)),
            "{}",
            run.name
        );
    }
}

#[test]
fn a_trimmed_run_counts_the_same_as_the_whole() {
    for run in CORPUS {
        let Some(trimmed) = trim(run.command, run.output) else {
            continue;
        };
        assert_eq!(
            test_run::counts(run.command, trimmed.text(), Some(run.status)),
            test_run::counts(run.command, run.output, Some(run.status)),
            "{}",
            run.name
        );
    }
    let failing = &CORPUS[1];
    let counts = test_run::counts(failing.command, failing.output, Some(failing.status))
        .expect("the recorded run is whole");
    assert_eq!(
        (counts.passed, counts.failed, counts.ignored, counts.suites),
        (140, 2, 1, 4)
    );
}

#[test]
fn outputs_too_short_to_be_worth_it_are_left_whole() {
    let left: Vec<&str> = CORPUS
        .iter()
        .filter(|run| trim(run.command, run.output).is_none())
        .map(|run| run.name)
        .collect();

    assert_eq!(left, ["compile_error", "check_cold"]);
}
