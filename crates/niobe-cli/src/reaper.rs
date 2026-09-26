// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Ends what a session started when the session is ended by something it
//! cannot answer.
//!
//! A session stops the processes it started on its way out: the CLI and its
//! process group, and every `!` command's group. SIGKILL gives it no way out
//! to take, and the groups it started are left running with nobody to see
//! them end. The CLI notices its standard input closing and leaves, but not
//! what it started, and a `!` command never notices anything.
//!
//! So a `sh` of its own, in a process group of its own, is told each group
//! the session starts and each it has seen end, and holds a pipe from the
//! session whose other end nothing else has. The pipe closes when the session
//! has gone however it went; `sh` reads the end of it, asks every group it
//! still knows of to end, and kills what is left a second later. A session
//! that ends by its own way out kills that `sh` first, so the groups it has
//! already stopped are not signalled again.

use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

/// What the reaper runs: the groups are kept as a space-separated list, `+`
/// adds one and `-` takes one off, and the end of its input ends them all.
///
/// A group taken off is one the session saw empty. Its number can then be
/// given to a new process, which could lead a group of its own, so it is not
/// signalled on the session's behalf.
const WATCH: &str = r#"groups=' '
while read -r line; do
  case $line in
    +*) groups="$groups${line#+} " ;;
    -*) g=${line#-}
        case $groups in
          *" $g "*) groups="${groups%%" $g "*} ${groups#*" $g "}" ;;
        esac ;;
  esac
done
[ "$groups" = ' ' ] && exit 0
for g in $groups; do kill -s TERM -- "-$g" 2>/dev/null; done
sleep 1
for g in $groups; do kill -s KILL -- "-$g" 2>/dev/null; done
"#;

/// The session's end of the reaper: what it is told the session has started,
/// and what it is told has ended. Cloned to everything that starts a process
/// group; the reaper ends once the last clone is dropped.
#[derive(Debug, Clone)]
pub struct Reaper(Arc<Mutex<Watcher>>);

#[derive(Debug)]
struct Watcher {
    sh: Child,
    told: Option<ChildStdin>,
}

impl Reaper {
    /// Starts the reaper, or says why it could not be.
    ///
    /// Its own process group, so that a signal sent to the operator's
    /// foreground group — which the session is in — does not end it before
    /// it has anything to do.
    pub fn start() -> std::io::Result<Self> {
        let mut sh = Command::new("sh");
        sh.arg("-c")
            .arg(WATCH)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut sh, 0);
        let mut sh = sh.spawn()?;
        let told = sh.stdin.take();
        Ok(Self(Arc::new(Mutex::new(Watcher { sh, told }))))
    }

    /// Tells the reaper the session has started the process group `group`
    /// leads.
    pub fn watch(&self, group: u32) {
        self.tell(&format!("+{group}\n"));
    }

    /// Tells the reaper the group `group` leads has nobody left in it.
    pub fn forget(&self, group: u32) {
        self.tell(&format!("-{group}\n"));
    }

    fn tell(&self, line: &str) {
        if let Ok(mut watcher) = self.0.lock()
            && let Some(told) = watcher.told.as_mut()
        {
            // A reaper that has gone can do nothing for the session either
            // way, and the session's own way out still stops what it started.
            let _ = told.write_all(line.as_bytes());
        }
    }
}

impl Drop for Watcher {
    /// Kills the reaper before its pipe closes: the session is ending by its
    /// own way out, which stops what it started, and the reaper reading the
    /// end of its input would signal those groups again.
    fn drop(&mut self) {
        let _ = self.sh.kill();
        let _ = self.sh.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A process leading a group of its own, as a `!` command is.
    fn group() -> Child {
        let mut sleep = Command::new("sleep");
        sleep.arg("30");
        std::os::unix::process::CommandExt::process_group(&mut sleep, 0);
        sleep.spawn().expect("sleep starts")
    }

    /// The session going the way SIGKILL takes it: the pipe closes, and the
    /// reaper is not killed first.
    fn session_killed(reaper: &Reaper) {
        let mut watcher = reaper.0.lock().expect("nothing else holds the reaper");
        watcher.told = None;
    }

    fn ended_within(child: &mut Child, patience: Duration) -> bool {
        let until = Instant::now() + patience;
        while Instant::now() < until {
            if child
                .try_wait()
                .expect("the child can be waited on")
                .is_some()
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_group_the_session_started_ends_when_the_session_is_killed() {
        let reaper = Reaper::start().expect("sh starts");
        let mut started = group();
        reaper.watch(started.id());

        session_killed(&reaper);

        assert!(
            ended_within(&mut started, Duration::from_secs(2)),
            "the group outlived the session"
        );
    }

    #[test]
    fn a_group_that_will_not_end_when_asked_is_killed_a_second_later() {
        let reaper = Reaper::start().expect("sh starts");
        let mut sh = Command::new("sh");
        sh.args(["-c", "trap '' TERM; sleep 30"]);
        std::os::unix::process::CommandExt::process_group(&mut sh, 0);
        let mut ignoring = sh.spawn().expect("sh starts");
        reaper.watch(ignoring.id());
        std::thread::sleep(Duration::from_millis(100));

        session_killed(&reaper);

        assert!(
            ended_within(&mut ignoring, Duration::from_secs(3)),
            "a group that ignores SIGTERM outlived the session"
        );
    }

    #[test]
    fn a_group_the_session_saw_end_is_not_signalled() {
        let reaper = Reaper::start().expect("sh starts");
        let mut forgotten = group();
        let mut watched = group();
        reaper.watch(forgotten.id());
        reaper.watch(watched.id());
        reaper.forget(forgotten.id());

        session_killed(&reaper);

        assert!(ended_within(&mut watched, Duration::from_secs(2)));
        assert!(
            !ended_within(&mut forgotten, Duration::from_millis(1500)),
            "a group taken off the list was signalled"
        );
        let _ = forgotten.kill();
        let _ = forgotten.wait();
    }

    #[test]
    fn a_session_that_ends_its_own_way_leaves_its_groups_to_itself() {
        let reaper = Reaper::start().expect("sh starts");
        let mut started = group();
        reaper.watch(started.id());

        drop(reaper);

        assert!(
            !ended_within(&mut started, Duration::from_millis(1500)),
            "the reaper signalled a group the session was left to stop"
        );
        let _ = started.kill();
        let _ = started.wait();
    }
}
