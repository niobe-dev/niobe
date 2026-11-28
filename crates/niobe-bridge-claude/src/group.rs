// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The process group the CLI leads, settled once the CLI has been reaped, on
//! a thread of its own.
//!
//! Settling it is a `kill` or several: one asking the group to end, one with
//! no signal for each look at whether anyone is left in it, and one killing
//! what is left. A session is drained on the thread that draws the screen,
//! and on macOS an endpoint-security agent walking the machine's processes
//! held a `kill` for up to a second, with every key typed meanwhile waiting
//! on the frame. The keeper's thread makes those calls; the session only
//! tells it the CLI has been reaped, and asks without a syscall whether it is
//! done.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long what the CLI started is given to end on SIGTERM once the CLI has
/// gone, before its group is killed.
///
/// A process that listens ends within a few milliseconds, so only one that
/// does not is waited for this long; the operator who quit is waiting too.
pub(crate) const LEFTOVERS: Duration = Duration::from_millis(250);

/// How long the keeper waits between two looks at whether anyone is left in
/// the group it asked to end.
const LOOK: Duration = Duration::from_millis(5);

/// The thread that settles the CLI's process group once the CLI has been
/// reaped: asks it to end, lets go of it as soon as it is seen empty, and
/// kills what is left after [`LEFTOVERS`].
///
/// The group is looked at before it is signalled, each time: a group's id is
/// not given to a new process while anyone is left in it, so one seen with
/// somebody in it is still the CLI's, and one seen empty is never signalled
/// again. Nor is it signalled before the CLI has been reaped, which the
/// keeper waits to be told of; until then the id is the CLI's own.
#[derive(Debug)]
pub(crate) struct Keeper {
    /// Sent on, the once, when the CLI has been reaped. Dropped unsent, the
    /// thread ends with nothing signalled.
    reaped: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Keeper {
    /// Starts the thread that settles the group `leader` leads, which waits
    /// to be told the CLI has been reaped.
    pub(crate) fn start(leader: u32) -> Self {
        let (reaped, told) = mpsc::channel();
        let thread = std::thread::spawn(move || keep(leader, &told));
        Self {
            reaped: Some(reaped),
            thread: Some(thread),
        }
    }

    /// Tells the keeper the CLI has been reaped, so that it settles the
    /// group. Only the first call does anything. Never waits.
    pub(crate) fn reaped(&mut self) {
        if let Some(reaped) = self.reaped.take() {
            // The thread gone has nothing left to settle.
            let _ = reaped.send(());
        }
    }

    /// Waits until the group is settled, which is bounded by [`LEFTOVERS`]
    /// and a look from when the keeper was told the CLI had been reaped: what
    /// a closing session does, so that nothing the CLI started outlives it.
    /// A keeper not told of the reap ends at once, having signalled nothing.
    pub(crate) fn wait(&mut self) {
        self.reaped = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What the keeper's thread does: waits for the reap, then settles the
/// group `leader` leads.
fn keep(leader: u32, reaped: &Receiver<()>) {
    if reaped.recv().is_err() {
        return;
    }
    if !ask_group_to_end(leader) {
        return;
    }
    let asked = Instant::now();
    while group_occupied(leader) {
        if asked.elapsed() >= LEFTOVERS {
            kill_group(leader);
            return;
        }
        std::thread::sleep(LOOK);
    }
}

/// How many times the current thread has asked after or signalled a CLI's
/// group: what a test reads to see that the thread that drains a session
/// never does.
#[cfg(test)]
pub(crate) fn asked_here() -> usize {
    ASKED_HERE.with(std::cell::Cell::get)
}

#[cfg(test)]
thread_local! {
    static ASKED_HERE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn counted() {
    ASKED_HERE.with(|asked| asked.set(asked.get().saturating_add(1)));
}

/// Whether anyone is left in the process group the CLI led.
///
/// Asked once the CLI has been reaped, and straight after, as each signal to
/// the group is: a process group's id is not given to a new process while any
/// member of the group is alive, so the group looked at is still the CLI's,
/// or is gone. Looked at later, it could have emptied and its id been given
/// to someone else's.
#[cfg(unix)]
fn group_occupied(leader: u32) -> bool {
    #[cfg(test)]
    counted();
    group_of(leader).is_some_and(|group| rustix::process::test_kill_process_group(group).is_ok())
}

#[cfg(not(unix))]
fn group_occupied(_leader: u32) -> bool {
    false
}

/// Sends SIGTERM to the process group the CLI led, without waiting on it,
/// and says whether anything was there to take it.
///
/// Called only once the CLI has been reaped, for the reason [`group_occupied`]
/// gives.
#[cfg(unix)]
fn ask_group_to_end(leader: u32) -> bool {
    #[cfg(test)]
    counted();
    group_of(leader).is_some_and(|group| {
        rustix::process::kill_process_group(group, rustix::process::Signal::TERM).is_ok()
    })
}

#[cfg(not(unix))]
fn ask_group_to_end(_leader: u32) -> bool {
    false
}

/// Kills whatever is left of the process group the CLI led.
#[cfg(unix)]
fn kill_group(leader: u32) {
    #[cfg(test)]
    counted();
    if let Some(group) = group_of(leader) {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
}

#[cfg(not(unix))]
fn kill_group(_leader: u32) {}

/// The process group a CLI with process id `leader` leads.
#[cfg(unix)]
fn group_of(leader: u32) -> Option<rustix::process::Pid> {
    i32::try_from(leader)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
}
