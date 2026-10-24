// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Standard output, written without panicking.
//!
//! `println!` panics when the write fails, and the commonest way it fails is a
//! reader that has stopped reading: `niobe prices | head -1` closes the pipe
//! once `head` has its line, and the next line `niobe` prints raises `EPIPE`
//! (the standard library ignores `SIGPIPE`, so the write fails rather than the
//! signal ending the process). A panic there ends the run as exit 101, which
//! `--help` does not list and which reads as a bug in `niobe`.
//!
//! So every line goes through [`line`], which keeps the first write that failed
//! and prints nothing after it, and the run's end asks [`outcome`] what that
//! failure means: nothing, where the reader went away, since what it wanted it
//! had; a failure to report, for any other reason the output was lost.

use std::io::{self, Write};
use std::sync::OnceLock;

/// The first write to standard output that failed, if one has.
static FAILURE: OnceLock<io::Error> = OnceLock::new();

/// Writes `text` and a line break to standard output, unless an earlier write
/// failed: a line printed after one that was lost would read as if it followed
/// it.
pub(crate) fn line(text: &str) {
    if FAILURE.get().is_some() {
        return;
    }
    if let Err(error) = write_line(&mut io::stdout().lock(), text) {
        let _ = FAILURE.set(error);
    }
}

fn write_line(out: &mut dyn Write, text: &str) -> io::Result<()> {
    writeln!(out, "{text}")?;
    out.flush()
}

/// What the run's output comes to: `Ok` where every line was written or the
/// reader went away, and the reason otherwise.
pub(crate) fn outcome() -> Result<(), String> {
    outcome_of(FAILURE.get())
}

fn outcome_of(failure: Option<&io::Error>) -> Result<(), String> {
    match failure {
        None => Ok(()),
        Some(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Some(error) => Err(format!("standard output could not be written: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_nobody_failed_to_write_is_no_failure() {
        assert_eq!(outcome_of(None), Ok(()));
    }

    #[test]
    fn a_reader_that_went_away_is_no_failure() {
        let gone = io::Error::from(io::ErrorKind::BrokenPipe);
        assert_eq!(outcome_of(Some(&gone)), Ok(()));
    }

    #[test]
    fn output_lost_for_any_other_reason_is_reported() {
        let full = io::Error::other("no space left on device");
        let outcome = outcome_of(Some(&full));
        assert_eq!(
            outcome,
            Err("standard output could not be written: no space left on device".to_owned())
        );
    }
}
