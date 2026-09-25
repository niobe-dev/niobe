// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Waiting for the operator to type.
//!
//! The loop waits on the terminal's descriptor itself rather than in
//! `crossterm::event::poll`, because that call never comes back once the
//! terminal it reads from is gone. crossterm reads the descriptor until it gets
//! a byte or an error it knows, and a terminal whose other end has closed
//! answers end-of-file for ever: the process spins at a whole core, and, being
//! inside crossterm, never reads the shutdown flag again, so a SIGTERM cannot
//! end it either.
//!
//! `poll(2)` reports that hangup as POLLHUP, which is the one thing a read
//! cannot say. So the wait happens here, and crossterm is called only once
//! there is something for it to read.

use std::io::{self, IsTerminal};
use std::time::Duration;

use ratatui::crossterm::event;

#[cfg(unix)]
use std::os::fd::{AsFd, BorrowedFd};

#[cfg(unix)]
use rustix::event::{PollFd, PollFlags, Timespec};

/// What a wait on the terminal found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Input {
    /// There is something to read.
    Ready,
    /// Nothing was typed before the wait timed out.
    Idle,
    /// The terminal the shell reads from is gone; nothing will come from it
    /// again.
    HungUp,
}

/// How this session waits for input.
///
/// Carries the one decision made when the shell opens: whether `poll(2)` can
/// see the descriptor crossterm reads.
#[derive(Debug)]
pub(crate) struct Wait {
    #[cfg(unix)]
    polls_the_terminal: bool,
}

impl Wait {
    /// Decides how this session waits.
    ///
    /// The descriptor crossterm reads is standard input when that is a
    /// terminal, and `/dev/tty` otherwise. Only the first can go away without a
    /// signal: `/dev/tty` is the controlling terminal by definition, and losing
    /// that arrives as SIGHUP, which [`crate::terminal::Shutdown`] already
    /// reports as the terminal going away. So this wait covers standard input
    /// and leaves the rest to crossterm.
    pub(crate) fn on_the_terminal() -> Self {
        Self {
            #[cfg(unix)]
            polls_the_terminal: io::stdin().is_terminal() && poll_sees_stdin(),
        }
    }

    /// Waits up to `timeout` for the operator to type.
    pub(crate) fn input(&self, timeout: Duration) -> io::Result<Input> {
        #[cfg(unix)]
        if self.polls_the_terminal {
            return poll_input(io::stdin().as_fd(), timeout);
        }
        crossterm_input(timeout)
    }
}

/// crossterm's own wait: for a terminal `poll(2)` cannot see, and for the
/// platforms that have no `poll(2)` at all.
fn crossterm_input(timeout: Duration) -> io::Result<Input> {
    if event::poll(timeout)? {
        Ok(Input::Ready)
    } else {
        Ok(Input::Idle)
    }
}

/// Waits on `tty` for up to `timeout`.
#[cfg(unix)]
pub(crate) fn poll_input(tty: BorrowedFd<'_>, timeout: Duration) -> io::Result<Input> {
    let mut fds = [PollFd::from_borrowed_fd(tty, PollFlags::IN)];
    match rustix::event::poll(&mut fds, Some(&timespec(timeout))) {
        Ok(0) => Ok(Input::Idle),
        Ok(_) => {
            let [tty] = &fds;
            Ok(found(tty.revents()))
        }
        // A signal interrupted the wait: the tick ends here, so that the loop
        // reads the shutdown flag now rather than waiting the rest of it out.
        Err(rustix::io::Errno::INTR) => Ok(Input::Idle),
        Err(errno) => Err(errno.into()),
    }
}

/// What the kernel reported about the terminal.
#[cfg(unix)]
fn found(revents: PollFlags) -> Input {
    // POLLHUP comes with POLLIN, for the end-of-file a read would return, so
    // the hangup is looked for first. POLLERR and POLLNVAL are the descriptor
    // itself gone, which is the same answer for the loop: there is nothing
    // left to read from.
    if revents.intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL) {
        Input::HungUp
    } else if revents.contains(PollFlags::IN) {
        Input::Ready
    } else {
        Input::Idle
    }
}

/// Whether `poll(2)` can see the terminal on standard input.
///
/// macOS answers POLLNVAL for `/dev/tty` and `/dev/null` instead of polling
/// them, and POLLNVAL is otherwise how a descriptor that is gone reads: a
/// terminal `poll(2)` cannot see would look like one that had hung up, on the
/// first tick. Asking once, before anything depends on the answer, separates
/// the two — a terminal that has really gone away reports POLLHUP.
#[cfg(unix)]
fn poll_sees_stdin() -> bool {
    let stdin = io::stdin();
    let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
    match rustix::event::poll(&mut fds, Some(&timespec(Duration::ZERO))) {
        Ok(_) => {
            let [tty] = &fds;
            !tty.revents().contains(PollFlags::NVAL)
        }
        Err(_) => false,
    }
}

/// A duration as `poll(2)` takes it.
#[cfg(unix)]
fn timespec(timeout: Duration) -> Timespec {
    Timespec {
        tv_sec: timeout.as_secs() as _,
        tv_nsec: timeout.subsec_nanos() as _,
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    use std::ffi::OsStr;
    use std::fs::File;
    use std::os::fd::OwnedFd;
    use std::os::unix::ffi::OsStrExt;

    use rustix::pty::OpenptFlags;

    /// How long a test waits on a terminal that should already have an answer.
    const PATIENCE: Duration = Duration::from_millis(500);

    /// A pty, as both ends: the master a terminal emulator would hold, and the
    /// slave a shell would be given.
    fn pty() -> (OwnedFd, File) {
        let master = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)
            .expect("a pty can be opened");
        rustix::pty::grantpt(&master).expect("the slave can be granted");
        rustix::pty::unlockpt(&master).expect("the slave can be unlocked");
        let name = rustix::pty::ptsname(&master, Vec::new()).expect("the slave has a name");
        let slave = File::options()
            .read(true)
            .write(true)
            .open(OsStr::from_bytes(name.as_bytes()))
            .expect("the slave can be opened");
        (master, slave)
    }

    #[test]
    fn a_terminal_with_nothing_typed_into_it_is_idle() {
        let (_master, slave) = pty();

        let input = poll_input(slave.as_fd(), Duration::ZERO).expect("polling a pty cannot fail");

        assert_eq!(input, Input::Idle);
    }

    #[test]
    fn a_terminal_with_a_line_waiting_is_ready() {
        let (master, slave) = pty();
        rustix::io::write(&master, b"send it\n").expect("the master accepts a line");

        let input = poll_input(slave.as_fd(), PATIENCE).expect("polling a pty cannot fail");

        assert_eq!(input, Input::Ready);
    }

    #[test]
    fn a_terminal_whose_other_end_has_gone_reads_as_a_hangup() {
        let (master, slave) = pty();
        drop(master);

        let input = poll_input(slave.as_fd(), PATIENCE).expect("polling a pty cannot fail");

        assert_eq!(
            input,
            Input::HungUp,
            "a terminal that has gone away read as something the loop would go on waiting for"
        );
    }

    #[test]
    fn a_hangup_is_a_hangup_even_with_the_end_of_file_to_read() {
        assert_eq!(found(PollFlags::HUP | PollFlags::IN), Input::HungUp);
    }

    #[test]
    fn a_descriptor_that_is_gone_is_a_hangup() {
        assert_eq!(found(PollFlags::ERR), Input::HungUp);
        assert_eq!(found(PollFlags::NVAL), Input::HungUp);
    }

    #[test]
    fn nothing_reported_is_idle() {
        assert_eq!(found(PollFlags::empty()), Input::Idle);
    }
}
