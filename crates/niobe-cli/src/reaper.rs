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
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::ExitStatus;
    use std::time::{Duration, Instant};

    /// How long a test waits for something the reaper does. A `sh` takes a
    /// tenth of a second or more to start on an idle Mac and seconds on a
    /// loaded one, and the reaper starts one and forks a `sleep` before its
    /// SIGKILL, so a budget of a few seconds measures the machine rather than
    /// the reaper. Waiting returns as soon as the thing has happened; twenty
    /// seconds ran out under a load average of forty.
    const PATIENCE: Duration = Duration::from_secs(60);

    /// How long each group's `sleep` runs if nothing ends it: well past
    /// [`PATIENCE`], so a group can only have ended because it was signalled.
    const ASLEEP: &str = "300";

    /// A process leading a group of its own, as a `!` command is.
    fn group() -> Child {
        let mut sleep = Command::new("sleep");
        sleep.arg(ASLEEP).process_group(0);
        sleep.spawn().expect("sleep starts")
    }

    /// A group of one process that ignores SIGTERM, returned once it has said
    /// the trap is in place: a SIGTERM that arrives before the trap ends it,
    /// and no fixed wait is long enough for `sh` to start on a loaded machine.
    /// It then execs `sleep`, which keeps the ignored signal, so that nothing
    /// is left in the group to exit on its own when `sleep` is killed: the
    /// group is signalled one process at a time, and a `sh` could see its
    /// child killed and exit with 128 + 9 before its own turn came.
    fn group_ignoring_term() -> Child {
        let mut sh = Command::new("sh");
        sh.args([
            "-c",
            &format!("trap '' TERM; echo trapped; exec sleep {ASLEEP}"),
        ])
        .stdout(Stdio::piped())
        .process_group(0);
        let mut ignoring = sh.spawn().expect("sh starts");
        let mut said = String::new();
        BufReader::new(ignoring.stdout.take().expect("its output is piped"))
            .read_line(&mut said)
            .expect("sh says the trap is in place");
        assert_eq!(said, "trapped\n");
        ignoring
    }

    /// The session going the way SIGKILL takes it: the pipe closes, and the
    /// reaper is not killed first.
    fn session_killed(reaper: &Reaper) {
        let mut watcher = reaper.0.lock().expect("nothing else holds the reaper");
        watcher.told = None;
    }

    /// Whether the reaper's `sh` has exited, which it does only once it has
    /// sent every signal it is going to.
    fn reaper_ended_within(reaper: &Reaper, patience: Duration) -> bool {
        let until = Instant::now() + patience;
        while Instant::now() < until {
            let mut watcher = reaper.0.lock().expect("nothing else holds the reaper");
            if watcher
                .sh
                .try_wait()
                .expect("sh can be waited on")
                .is_some()
            {
                return true;
            }
            drop(watcher);
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn ended_within(child: &mut Child, patience: Duration) -> Option<ExitStatus> {
        let until = Instant::now() + patience;
        while Instant::now() < until {
            if let Some(status) = child.try_wait().expect("the child can be waited on") {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    #[test]
    fn a_group_the_session_started_ends_when_the_session_is_killed() {
        let reaper = Reaper::start().expect("sh starts");
        let mut started = group();
        reaper.watch(started.id());

        session_killed(&reaper);

        assert!(
            ended_within(&mut started, PATIENCE).is_some(),
            "the group outlived the session"
        );
    }

    #[test]
    fn a_group_the_session_started_is_asked_to_end_before_it_is_killed() {
        let reaper = Reaper::start().expect("sh starts");
        let mut started = group();
        reaper.watch(started.id());

        session_killed(&reaper);

        let status = ended_within(&mut started, PATIENCE).expect("the group outlived the session");
        assert_eq!(
            status.signal(),
            Some(15),
            "the group ended on something other than SIGTERM: {status:?}"
        );
    }

    #[test]
    fn the_reaper_leads_a_process_group_of_its_own() {
        let reaper = Reaper::start().expect("sh starts");
        let watcher = reaper.0.lock().expect("nothing else holds the reaper");
        let pid = rustix::process::Pid::from_child(&watcher.sh);

        assert_eq!(
            rustix::process::getpgid(Some(pid)).expect("the reaper's group can be read"),
            pid
        );
    }

    #[test]
    fn a_group_that_will_not_end_when_asked_is_killed_a_second_later() {
        let reaper = Reaper::start().expect("sh starts");
        let mut ignoring = group_ignoring_term();
        reaper.watch(ignoring.id());

        session_killed(&reaper);

        let status = ended_within(&mut ignoring, PATIENCE)
            .expect("a group that ignores SIGTERM outlived the session");
        assert_eq!(
            status.signal(),
            Some(9),
            "the group ended on something other than SIGKILL: {status:?}"
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

        assert!(ended_within(&mut watched, PATIENCE).is_some());
        assert!(
            reaper_ended_within(&reaper, PATIENCE),
            "the reaper never ended"
        );
        assert!(
            ended_within(&mut forgotten, Duration::from_millis(500)).is_none(),
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
            ended_within(&mut started, Duration::from_millis(1500)).is_none(),
            "the reaper signalled a group the session was left to stop"
        );
        let _ = started.kill();
        let _ = started.wait();
    }
}
