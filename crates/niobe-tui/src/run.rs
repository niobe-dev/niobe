// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The event loop.
//!
//! One loop, one thread, no async: the shell has nothing to wait on but the
//! keyboard while no bridge produces events, and a runtime it does not need is
//! a runtime in the binary and in the backtrace.

use std::io;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::prelude::CrosstermBackend;

use niobe_core::event::Event as SessionEvent;

use crate::app::App;
use crate::bridge::Bridge;
use crate::input::{Input, Wait};
use crate::journal::Journal;
use crate::rules::Rules;
use crate::shell::Shell;
use crate::terminal::{Shutdown, Stop, TerminalGuard, install_panic_hook};
use crate::ui;
use crate::watch::Watch;

/// How long the loop waits for a key before looking at the shutdown flag.
///
/// A signal is noticed within one tick, so this is also the worst case between
/// a SIGTERM and the terminal being handed back, and between the terminal going
/// away and the session ending.
const TICK: Duration = Duration::from_millis(100);

/// How long it waits while a backend is producing.
///
/// A reply arrives as fragments on a channel the terminal's `poll(2)` cannot
/// see, so the loop finds them by looking — at `TICK` that is ten redraws a
/// second and text that arrives in visible steps. Thirty is smooth to read and
/// half the wakeups of a frame rate, and it is paid only while something is
/// actually arriving: a tick that drained nothing goes back to `TICK`, so an
/// idle shell and a session waiting on a tool call cost what they did before.
const BUSY_TICK: Duration = Duration::from_millis(33);

/// What the loop asks the machine about: whether the process has been told to
/// stop, whether the terminal has anything to read, and what time it is.
///
/// One value rather than three arguments, because the loop takes them all the
/// same way — once, at the top, before it looks at anything else.
struct Machine<'a> {
    shutdown: &'a Shutdown,
    wait: &'a Wait,
    clock: &'a crate::clock::Clock,
}

/// Everything the loop hands what the operator does to, and takes what
/// happens elsewhere from: one value, because the loop hands every tick's work
/// to all of them.
struct Around<'a> {
    journal: &'a mut dyn Journal,
    backend: &'a mut dyn Bridge,
    rules: &'a mut dyn Rules,
    watch: &'a mut dyn Watch,
    shell: &'a mut dyn Shell,
}

/// How a session ended.
///
/// The caller needs the difference to know whether there is still a terminal
/// to write to: a line printed after the shell closes goes to whatever the
/// shell was drawing on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The operator quit, or the process was asked to stop.
    Quit,
    /// The terminal the shell drew on went away.
    TerminalGone,
}

/// Runs the shell on `app` until the operator quits, the process is asked to
/// stop or the terminal goes away, handing every event the operator produces to
/// `backend` and to `journal`, the standing answers they make to `rules`, and
/// folding in everything `backend` produces.
///
/// `watch` is where the state of the repository comes from. It is asked once a
/// tick and never waited on, so a read that is slow, that failed, or that has
/// nothing new to say costs the frame nothing and leaves the last one on
/// screen. `shell` runs the commands the operator types after `!`, and is
/// asked how they ended the same way.
///
/// `app` may already hold a session: a resumed one is folded in by the caller
/// before the shell opens.
///
/// Installs the panic hook and the signal handlers first, so that every way out
/// of the function — including the ways that do not return from it — puts the
/// terminal back.
pub fn run(
    app: App,
    journal: &mut dyn Journal,
    backend: &mut dyn Bridge,
    rules: &mut dyn Rules,
    watch: &mut dyn Watch,
    shell: &mut dyn Shell,
) -> io::Result<Ended> {
    install_panic_hook();
    let shutdown = Shutdown::install()?;
    let wait = Wait::on_the_terminal();

    // Read once: the timezone is a file on disk and the loop asks for the time
    // ten times a second.
    let clock = crate::clock::Clock::system();
    // The same clock the loop stamps events with, so that a moment an event
    // names — when a window comes back — is read in the timezone the rest of
    // the shell is drawn in.
    let mut app = app.with_clock(clock.clone());

    let mut guard = TerminalGuard::enter(io::stdout())?;
    // No `Terminal::clear` here: the alternate screen starts blank and the
    // first draw covers it. `clear` also asks the terminal where its cursor is
    // and waits for the reply, which never comes when stdin is a pipe.
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let ended = event_loop(
        &mut terminal,
        &mut app,
        Around {
            journal,
            backend,
            rules,
            watch,
            shell,
        },
        &Machine {
            shutdown: &shutdown,
            wait: &wait,
            clock: &clock,
        },
    );

    match &ended {
        Ok(Ended::Quit) => {}
        // The drawing surface is abandoned rather than dropped: ratatui's
        // `Terminal` shows the cursor again as it drops and, when it cannot,
        // prints that failure to standard error. Standard error is the terminal
        // that has just gone, so the print fails too — and an `eprintln!` that
        // fails panics, which would end a session that merely lost its terminal
        // as a crash. A loop that failed is abandoned too, because every error
        // it produces is the terminal refusing it. Nothing is lost by never
        // dropping the surface: the guard below writes the same sequence.
        Ok(Ended::TerminalGone) | Err(_) => std::mem::forget(terminal),
    }

    // Explicit, so that a restore failure is reported rather than swallowed by
    // `Drop`. Dropping the guard afterwards is a no-op.
    outcome(ended, guard.restore())
}

/// What the session ended as, from how the loop ended and whether the terminal
/// could be handed back.
///
/// A terminal that has gone cannot be handed back: the sequences that would do
/// it fail with the same hangup that ended the session, and a session that
/// ended because its terminal closed did not fail — whatever started `niobe`
/// reads that from the exit status. Any other failure to restore is reported
/// with its error: a terminal left in raw mode on the alternate screen is the
/// operator's to fix by hand, and they are owed the reason.
///
/// The loop can fail rather than end, and everything it can fail at is the
/// terminal: it draws on it, waits on it and reads from it, and touches nothing
/// else. Which of those notices a terminal closing is a race — the wait reports
/// the hangup only if the close lands while the tick is being spent there, and
/// the draw at the top of the next tick fails otherwise. So a loop that failed
/// on a terminal that then could not be handed back either lost that terminal,
/// and ended the way the wait would have said it did. A loop that failed on a
/// terminal still there to take the sequences failed at something else, and the
/// operator is owed that error.
fn outcome(ended: io::Result<Ended>, restored: io::Result<()>) -> io::Result<Ended> {
    match ended {
        Ok(Ended::Quit) => restored.map(|()| Ended::Quit),
        Ok(Ended::TerminalGone) => Ok(Ended::TerminalGone),
        Err(_) if restored.is_err() => Ok(Ended::TerminalGone),
        Err(error) => Err(error),
    }
}

// `Backend::Error` is associated, so the loop names the one it propagates
// rather than being generic over an error it could not convert.
fn event_loop<B: Backend<Error = io::Error>>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    around: Around<'_>,
    machine: &Machine<'_>,
) -> io::Result<Ended> {
    let Around {
        journal,
        backend,
        rules,
        watch,
        shell,
    } = around;
    let mut ended = Ended::Quit;

    while !app.should_quit() {
        // First, so that everything this pass folds in — what the backend
        // produced and what the operator answered — is stamped with a clock
        // read this pass, rather than with the one before it.
        app.tick(std::time::Instant::now(), Some(machine.clock.now()));
        let producing = fold_backend(app, journal, backend, watch);
        fold_commands(app, shell);
        // Whatever a read of the repository has finished with since the last
        // tick. Nothing is waited on here: an unfinished or failed read says
        // nothing and the pane keeps what it had.
        if let Some(repo) = watch.look() {
            app.set_repo(repo);
        }
        // Before the draw, so that a prompt a standing rule already answers is
        // never on screen for the frame it takes to answer it.
        app.settle_rules();
        // After the fold and never inside it: the warning is about what this
        // session has spent under the budget this run was given, not about
        // what a recording being read back spent under one it knew nothing of.
        app.settle_budget();
        run_commands(app, shell);
        send_produced(app, journal, backend, rules);
        terminal.draw(|frame| ui::draw(frame, app))?;

        let tick = if producing { BUSY_TICK } else { TICK };
        match machine.wait.input(tick)? {
            Input::Ready => {
                read_input(app)?;
                run_commands(app, shell);
                send_produced(app, journal, backend, rules);
            }
            Input::Idle => {}
            // The terminal is gone: there is no one left to type and nothing
            // left to draw on, so the session ends the way a quit does and the
            // guard hands back what there is to hand back.
            Input::HungUp => {
                ended = Ended::TerminalGone;
                app.quit();
            }
        }

        match machine.shutdown.requested() {
            None => {}
            Some(Stop::Requested) => app.quit(),
            // SIGHUP is the terminal going away, reported by the kernel rather
            // than by the wait: a session in the session that owns the terminal
            // is told twice, and whichever telling arrives first is the one that
            // says what this session ended as.
            Some(Stop::TerminalGone) => {
                ended = Ended::TerminalGone;
                app.quit();
            }
        }
    }

    // A command still running is stopped as the session ends, when whatever
    // runs it is dropped; its call is ended here, so a session read back does
    // not show it running for ever.
    fold_commands(app, shell);
    app.abandon_commands();
    send_produced(app, journal, backend, rules);

    Ok(ended)
}

/// Hands the app everything the terminal has for it.
///
/// crossterm parses a read into a queue and gives out one event at a time — a
/// paste is one read and many keys — so the queue is emptied here. Were it not,
/// the rest of a paste would sit there until the next keystroke woke the wait,
/// which only looks at the terminal.
///
/// The zero-timeout check is the one place a hangup can still catch the loop,
/// in the instant between the wait and this call; crossterm offers no way to
/// ask what it has already parsed without also reading the terminal.
fn read_input(app: &mut App) -> io::Result<()> {
    loop {
        match event::read()? {
            // Windows reports a press and a release; acting on both would
            // send every prompt twice.
            Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
            Event::Mouse(mouse) => app.on_mouse(mouse),
            // The next draw reads the new size; nothing to do here.
            Event::Resize(_, _) => {}
            _ => {}
        }

        if !event::poll(Duration::ZERO)? {
            return Ok(());
        }
    }
}

/// Hands what the operator produced to the backend and to the journal, and
/// puts a failure on screen for anything either of them refused.
///
/// Neither failure ends the session. A store that stops writing leaves the
/// operator to decide whether to go on without it; a backend that will not take
/// a turn leaves the prompt on screen, said to be unsent rather than left
/// looking as though something were working on it.
fn send_produced(
    app: &mut App,
    journal: &mut dyn Journal,
    backend: &mut dyn Bridge,
    rules: &mut dyn Rules,
) {
    for event in app.take_produced() {
        if let Err(error) = journal.append(&event) {
            app.not_kept(&error.to_string());
        }
        if !app.is_attached() {
            continue;
        }
        // Only what the backend is waiting on is sent on: a turn, and a
        // decision about a call it has stopped for. Everything else the shell
        // produces is a record of what happened here, and the backend has no
        // use for what it did not ask for.
        match &event {
            SessionEvent::UserMessage { text } => {
                if let Err(error) = backend.send(text) {
                    app.not_sent(&error.to_string());
                }
            }
            SessionEvent::PermissionResponse {
                id,
                decision,
                message,
            } => {
                if let Err(error) = backend.answer(id, *decision, message.as_deref()) {
                    app.not_answered(&error.to_string());
                }
            }
            SessionEvent::ModeSelected { mode } => {
                if let Err(error) = backend.set_mode(*mode) {
                    app.not_changed("mode", &error.to_string());
                }
            }
            SessionEvent::ModelSelected { model } => {
                if let Err(error) = backend.set_model(model) {
                    app.not_changed("model", &error.to_string());
                }
            }
            _ => {}
        }
    }

    // After the answers, because a rule is made by answering: a rule that
    // could not be kept is reported under the decision it came from.
    for rule in app.take_rules() {
        if let Err(error) = rules.remember(&rule) {
            app.not_remembered(&rule, &error.to_string());
        }
    }
}

/// Hands the commands the operator ran to `shell`, and records a command it
/// would not start as a call that failed, so that nothing typed after `!` is
/// left looking as though it were running.
///
/// Before [`send_produced`], so a command that would not start is kept as a
/// start and a failed end in the one pass, in that order.
fn run_commands(app: &mut App, shell: &mut dyn Shell) {
    for (id, command) in app.take_commands() {
        if let Err(error) = shell.run(&id, &command) {
            app.not_run(&id, &error.to_string());
        }
    }
}

/// Records how each command that has ended since the last tick ended. What
/// that produces is kept with everything else the operator produced, by the
/// next [`send_produced`].
fn fold_commands(app: &mut App, shell: &mut dyn Shell) {
    for ran in shell.drain() {
        app.ran(ran);
    }
}

/// Folds in everything the backend has produced and keeps it, and says whether
/// there was any so that the loop can look again sooner while a reply arrives.
///
/// The backend's events go to the journal as the operator's do: a resumed
/// session shows the whole conversation or it shows half of one. They do not go
/// through `take_produced`, which is the queue of what was done *here* and is
/// what gets sent back to the backend.
///
/// It is also where `watch` is told a file changed, because this is the one
/// place a file change is seen arriving.
fn fold_backend(
    app: &mut App,
    journal: &mut dyn Journal,
    backend: &mut dyn Bridge,
    watch: &mut dyn Watch,
) -> bool {
    let events = backend.drain();
    if events.is_empty() {
        return false;
    }
    for event in &events {
        app.apply(event);
        // A file the session just wrote is a file the repository now reports
        // differently, and waiting out the watch's own cadence would leave the
        // pane behind the edit the operator just watched happen.
        if matches!(event, SessionEvent::FileChange { .. }) {
            watch.changed();
        }
        if let Err(error) = journal.append(event) {
            app.not_kept(&error.to_string());
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Answer, Repo};
    use crate::bridge::{BridgeError, Detached};
    use crate::journal::JournalError;
    use crate::rules::{Forgotten, RulesError};
    use crate::watch::{Unwatched, Watch};
    use niobe_core::event::{Mode, PermissionDecision, ToolCallId, ToolOutcome};
    use niobe_core::permission::Rule;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::collections::VecDeque;

    /// Keeps everything, or refuses everything.
    #[derive(Default)]
    struct Kept {
        events: Vec<SessionEvent>,
        refuse: bool,
    }

    impl Journal for Kept {
        fn append(&mut self, event: &SessionEvent) -> Result<(), JournalError> {
            if self.refuse {
                return Err("the store is read-only".into());
            }
            self.events.push(event.clone());
            Ok(())
        }
    }

    /// A backend that takes every turn, or refuses every turn, and hands back
    /// whatever it was given to produce.
    #[derive(Debug, Default)]
    struct Attached {
        sent: Vec<String>,
        answered: Vec<(ToolCallId, PermissionDecision)>,
        said: Vec<Option<String>>,
        modes: Vec<Mode>,
        models: Vec<String>,
        produces: Vec<SessionEvent>,
        refuse: bool,
    }

    impl Bridge for Attached {
        fn send(&mut self, prompt: &str) -> Result<(), BridgeError> {
            if self.refuse {
                return Err("the subprocess has gone".into());
            }
            self.sent.push(prompt.to_owned());
            Ok(())
        }

        fn answer(
            &mut self,
            id: &ToolCallId,
            decision: PermissionDecision,
            message: Option<&str>,
        ) -> Result<(), BridgeError> {
            if self.refuse {
                return Err("the subprocess has gone".into());
            }
            self.answered.push((id.clone(), decision));
            self.said.push(message.map(str::to_owned));
            Ok(())
        }

        fn set_mode(&mut self, mode: Mode) -> Result<(), BridgeError> {
            if self.refuse {
                return Err("the subprocess has gone".into());
            }
            self.modes.push(mode);
            Ok(())
        }

        fn set_model(&mut self, model: &str) -> Result<(), BridgeError> {
            if self.refuse {
                return Err("the subprocess has gone".into());
            }
            self.models.push(model.to_owned());
            Ok(())
        }

        fn drain(&mut self) -> Vec<SessionEvent> {
            std::mem::take(&mut self.produces)
        }
    }

    /// Keeps every rule, or refuses every rule.
    #[derive(Debug, Default)]
    struct Remembered {
        rules: Vec<Rule>,
        refuse: bool,
    }

    impl Rules for Remembered {
        fn remember(&mut self, rule: &Rule) -> Result<(), RulesError> {
            if self.refuse {
                return Err("the config is read-only".into());
            }
            self.rules.push(rule.clone());
            Ok(())
        }
    }

    /// A watch that hands over reads in order and counts what it was told.
    #[derive(Debug, Default)]
    struct Watching {
        reads: VecDeque<Repo>,
        nudges: usize,
    }

    impl Watch for Watching {
        fn look(&mut self) -> Option<Repo> {
            self.reads.pop_front()
        }

        fn changed(&mut self) {
            self.nudges += 1;
        }
    }

    /// A shell that starts every command, or refuses every one, and ends
    /// whatever it is told to.
    #[derive(Debug, Default)]
    struct Running {
        started: Vec<(ToolCallId, String)>,
        ends: Vec<crate::shell::Ran>,
        refuse: bool,
    }

    impl Shell for Running {
        fn run(&mut self, id: &ToolCallId, command: &str) -> Result<(), crate::shell::ShellError> {
            if self.refuse {
                return Err("cannot start sh".into());
            }
            self.started.push((id.clone(), command.to_owned()));
            Ok(())
        }

        fn drain(&mut self) -> Vec<crate::shell::Ran> {
            std::mem::take(&mut self.ends)
        }
    }

    /// A session that runs commands, with `command` typed after `!` and run.
    fn app_that_ran(command: &str) -> App {
        let mut app = App::new(Repo::default()).runs_commands();
        app.on_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        for c in command.chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app
    }

    fn ended_calls(kept: &Kept) -> Vec<(ToolOutcome, Option<String>)> {
        kept.events
            .iter()
            .filter_map(|event| match event {
                SessionEvent::ToolCallEnd { outcome, error, .. } => Some((*outcome, error.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_command_goes_to_the_shell_and_its_start_and_end_are_kept() {
        let mut app = app_that_ran("make check");
        let mut kept = Kept::default();
        let mut shell = Running::default();

        run_commands(&mut app, &mut shell);
        send_produced(&mut app, &mut kept, &mut Detached, &mut Forgotten);
        let [(id, command)] = shell.started.as_slice() else {
            panic!("the command did not reach the shell: {:?}", shell.started);
        };
        assert_eq!(command, "make check");
        assert!(matches!(
            kept.events.as_slice(),
            [SessionEvent::ToolCallStart { name, .. }] if name == crate::shell::OPERATOR_SHELL
        ));

        shell.ends.push(crate::shell::Ran {
            id: id.clone(),
            output: "ok\n".to_owned(),
            bytes: 3,
            whole: true,
            exit_code: Some(0),
            error: None,
        });
        fold_commands(&mut app, &mut shell);
        send_produced(&mut app, &mut kept, &mut Detached, &mut Forgotten);

        assert_eq!(ended_calls(&kept), [(ToolOutcome::Ok, None)]);
    }

    #[test]
    fn a_command_the_shell_would_not_start_is_kept_as_a_call_that_failed() {
        let mut app = app_that_ran("make check");
        let mut kept = Kept::default();
        let mut shell = Running {
            refuse: true,
            ..Running::default()
        };

        run_commands(&mut app, &mut shell);
        send_produced(&mut app, &mut kept, &mut Detached, &mut Forgotten);

        assert_eq!(
            ended_calls(&kept),
            [(
                ToolOutcome::Failed,
                Some("not run: cannot start sh".to_owned())
            )]
        );
    }

    #[test]
    fn a_command_still_running_as_the_session_ends_is_kept_as_stopped() {
        let mut app = app_that_ran("sleep 60");
        let mut kept = Kept::default();
        let mut shell = Running::default();
        run_commands(&mut app, &mut shell);

        app.abandon_commands();
        send_produced(&mut app, &mut kept, &mut Detached, &mut Forgotten);

        assert_eq!(
            ended_calls(&kept),
            [(
                ToolOutcome::Failed,
                Some("stopped: the session ended before the command did".to_owned())
            )]
        );
    }

    /// A session stopped on a prompt the operator has not answered.
    fn app_with_a_prompt(target: Option<&str>) -> App {
        let mut app = App::new(Repo::default()).attached();
        app.apply(&SessionEvent::PermissionRequest {
            id: "t1".into(),
            tool: "Bash".to_owned(),
            input: r#"{"command":"cargo test"}"#.to_owned(),
            target: target.map(str::to_owned),
        });
        app
    }

    fn app_with_a_sent_prompt() -> App {
        let mut app = App::new(Repo::default()).attached();
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app
    }

    /// A restore that could not write, the way handing back a terminal that has
    /// gone fails.
    fn restore_failed() -> io::Result<()> {
        Err(io::Error::other("the terminal went away"))
    }

    #[test]
    fn a_terminal_that_could_not_be_handed_back_is_reported_with_its_error() {
        let error = outcome(Ok(Ended::Quit), restore_failed()).expect_err("the restore failed");

        assert_eq!(error.to_string(), "the terminal went away");
    }

    #[test]
    fn a_session_whose_terminal_went_away_did_not_fail() {
        // The restore fails because the terminal is gone, which is the same
        // thing that ended the session.
        let ended = outcome(Ok(Ended::TerminalGone), restore_failed())
            .expect("a terminal that went away is not a failed session");

        assert_eq!(ended, Ended::TerminalGone);
    }

    #[test]
    fn a_clean_quit_that_handed_the_terminal_back_is_a_quit() {
        let ended = outcome(Ok(Ended::Quit), Ok(())).expect("nothing failed");

        assert_eq!(ended, Ended::Quit);
    }

    #[test]
    fn a_loop_that_failed_on_a_terminal_that_is_gone_lost_its_terminal() {
        // The draw at the top of a tick is the other way the loop finds out,
        // and it finds out as an error rather than as a hangup.
        let ended = outcome(
            Err(io::Error::other("the terminal went away")),
            restore_failed(),
        )
        .expect("a terminal that went away is not a failed session");

        assert_eq!(ended, Ended::TerminalGone);
    }

    #[test]
    fn a_loop_that_failed_on_a_terminal_that_is_still_there_is_reported_with_its_error() {
        let error = outcome(Err(io::Error::other("the draw was refused")), Ok(()))
            .expect_err("the loop failed");

        assert_eq!(error.to_string(), "the draw was refused");
    }

    #[test]
    fn a_sent_prompt_reaches_the_journal_and_the_backend() {
        let mut app = app_with_a_sent_prompt();
        let mut journal = Kept::default();
        let mut backend = Attached::default();

        send_produced(&mut app, &mut journal, &mut backend, &mut Forgotten);

        assert_eq!(
            journal.events,
            [SessionEvent::UserMessage {
                text: "x".to_owned()
            }]
        );
        assert_eq!(backend.sent, ["x"]);
    }

    #[test]
    fn a_journal_that_refuses_leaves_a_failure_on_screen() {
        let mut app = app_with_a_sent_prompt();
        let mut journal = Kept {
            refuse: true,
            ..Kept::default()
        };

        send_produced(&mut app, &mut journal, &mut Detached, &mut Forgotten);

        let saved = app
            .entries()
            .iter()
            .find(|entry| entry.head == "not saved")
            .expect("the failure is on screen");
        assert!(saved.body.ends_with("the store is read-only"));
    }

    #[test]
    fn a_backend_that_will_not_take_a_turn_says_so_rather_than_looking_busy() {
        let mut app = app_with_a_sent_prompt();
        let mut backend = Attached {
            refuse: true,
            ..Attached::default()
        };

        send_produced(&mut app, &mut Kept::default(), &mut backend, &mut Forgotten);

        let last = app.entries().last().expect("an entry was pushed");
        assert_eq!(last.head, "not sent");
        assert!(
            last.body.ends_with("the subprocess has gone"),
            "{}",
            last.body
        );
    }

    #[test]
    fn a_session_with_nothing_attached_does_not_try_to_send() {
        let mut app = App::new(Repo::default());
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let mut journal = Kept::default();

        send_produced(&mut app, &mut journal, &mut Detached, &mut Forgotten);

        // The turn is still recorded — it is what the operator did — and the
        // transcript says plainly that nothing received it.
        assert_eq!(journal.events.len(), 1);
        let notice = app
            .entries()
            .iter()
            .find(|entry| entry.head == "no backend")
            .expect("the shell said nothing is listening");
        assert_eq!(notice.meta, "not sent");
        assert!(
            !app.entries().iter().any(|entry| entry.head == "not sent"),
            "a session with nothing attached reported a send that was never tried"
        );
    }

    #[test]
    fn what_the_backend_produces_is_folded_in_and_kept() {
        let mut app = App::new(Repo::default()).attached();
        let mut journal = Kept::default();
        let mut backend = Attached {
            produces: vec![
                SessionEvent::AssistantDelta {
                    text: "read".to_owned(),
                },
                SessionEvent::AssistantMessage {
                    text: "reading".to_owned(),
                },
            ],
            ..Attached::default()
        };

        let producing = fold_backend(&mut app, &mut journal, &mut backend, &mut Unwatched);

        assert!(producing, "the loop did not notice a reply arriving");
        assert_eq!(
            journal.events.len(),
            2,
            "a resumed session would show half a reply"
        );
        assert_eq!(app.session().assistant_messages(), 1);
        assert_eq!(app.session().last_assistant(), Some("reading"));
        assert!(
            app.take_produced().is_empty(),
            "the backend's own events were queued to be sent back to it"
        );
    }

    #[test]
    fn a_decision_reaches_the_backend_the_call_is_waiting_in() {
        let mut app = app_with_a_prompt(Some("cargo test"));
        let mut journal = Kept::default();
        let mut backend = Attached::default();

        app.answer(Answer::Once);
        send_produced(&mut app, &mut journal, &mut backend, &mut Forgotten);

        assert_eq!(
            backend.answered,
            [(ToolCallId::new("t1"), PermissionDecision::Allow)]
        );
        assert_eq!(
            journal.events,
            [SessionEvent::PermissionResponse {
                id: "t1".into(),
                decision: PermissionDecision::Allow,
                message: None,
            }],
            "a resumed session would not know the call was allowed"
        );
        assert!(app.asking().is_none(), "the prompt stayed on screen");
    }

    #[test]
    fn a_decision_the_backend_would_not_take_says_the_call_is_still_waiting() {
        let mut app = app_with_a_prompt(Some("cargo test"));
        let mut backend = Attached {
            refuse: true,
            ..Attached::default()
        };

        app.answer(Answer::No);
        send_produced(&mut app, &mut Kept::default(), &mut backend, &mut Forgotten);

        let entry = app
            .entries()
            .iter()
            .find(|entry| entry.head == "not answered")
            .expect("the failure is on screen");
        assert!(entry.body.ends_with("the subprocess has gone"), "{entry:?}");
    }

    #[test]
    fn an_always_answer_is_kept_so_the_prompt_does_not_come_back() {
        let mut app = app_with_a_prompt(Some("cargo test"));
        let mut remembered = Remembered::default();

        app.answer(Answer::AlwaysTarget);
        send_produced(
            &mut app,
            &mut Kept::default(),
            &mut Attached::default(),
            &mut remembered,
        );

        assert_eq!(remembered.rules, [Rule::targeted("Bash", "cargo test")]);
        assert!(app.allowed().allows("Bash", Some("cargo test")));
    }

    #[test]
    fn a_rule_that_could_not_be_kept_says_it_will_not_outlive_the_session() {
        let mut app = app_with_a_prompt(None);
        let mut remembered = Remembered {
            refuse: true,
            ..Remembered::default()
        };

        app.answer(Answer::AlwaysTool);
        send_produced(
            &mut app,
            &mut Kept::default(),
            &mut Attached::default(),
            &mut remembered,
        );

        let entry = app
            .entries()
            .iter()
            .find(|entry| entry.head == "not saved" && entry.meta == "Bash")
            .expect("the failure names the rule that was not kept");
        assert!(entry.body.contains("comes back next time"), "{entry:?}");
        // The answer still stands for this session; only the keeping failed.
        assert!(app.allowed().allows("Bash", Some("anything")));
    }

    #[test]
    fn a_prompt_a_rule_already_answers_is_never_put_in_front_of_the_operator() {
        let mut app = App::new(Repo::default())
            .attached()
            .with_rules([Rule::targeted("Bash", "cargo test")].into_iter().collect());
        let mut backend = Attached {
            produces: vec![SessionEvent::PermissionRequest {
                id: "t1".into(),
                tool: "Bash".to_owned(),
                input: r#"{"command":"cargo test"}"#.to_owned(),
                target: Some("cargo test".to_owned()),
            }],
            ..Attached::default()
        };

        fold_backend(&mut app, &mut Kept::default(), &mut backend, &mut Unwatched);
        app.settle_rules();
        send_produced(&mut app, &mut Kept::default(), &mut backend, &mut Forgotten);

        assert!(
            app.asking().is_none(),
            "the transcript asked what was decided"
        );
        assert_eq!(
            backend.answered,
            [(ToolCallId::new("t1"), PermissionDecision::AllowByRule)]
        );
    }

    #[test]
    fn folding_a_recorded_session_answers_nothing_by_itself() {
        // `apply` is how a resumed session is read back. A rule that answered
        // prompts there would write decisions into a session that already
        // made its own.
        let mut app = App::new(Repo::default())
            .attached()
            .with_rules([Rule::tool("Bash")].into_iter().collect());

        app.apply(&SessionEvent::PermissionRequest {
            id: "t1".into(),
            tool: "Bash".to_owned(),
            input: r#"{"command":"cargo test"}"#.to_owned(),
            target: Some("cargo test".to_owned()),
        });

        assert!(app.take_produced().is_empty());
        assert!(app.asking().is_some());
    }

    #[test]
    fn a_mode_and_a_model_the_operator_chose_reach_the_backend_and_the_journal() {
        let mut app = App::new(Repo::default()).attached();
        let mut journal = Kept::default();
        let mut backend = Attached::default();

        app.apply(&SessionEvent::ModeSelected { mode: Mode::Ask });
        app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        send_produced(&mut app, &mut journal, &mut backend, &mut Forgotten);

        assert_eq!(backend.modes, [Mode::Auto]);
        assert_eq!(
            journal.events,
            [SessionEvent::ModeSelected { mode: Mode::Auto }],
            "a resumed session would start the backend in the mode it was moved off"
        );
    }

    #[test]
    fn a_change_the_backend_would_not_take_says_the_session_is_running_as_it_was() {
        let mut app = App::new(Repo::default()).attached();
        let mut backend = Attached {
            refuse: true,
            ..Attached::default()
        };

        app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        send_produced(&mut app, &mut Kept::default(), &mut backend, &mut Forgotten);

        let entry = app
            .entries()
            .iter()
            .find(|entry| entry.head == "not changed")
            .expect("the failure is on screen");
        assert_eq!(entry.meta, "mode");
        assert!(entry.body.ends_with("the subprocess has gone"), "{entry:?}");
    }

    #[test]
    fn a_tick_that_drained_nothing_is_not_a_backend_producing() {
        let mut app = App::new(Repo::default()).attached();

        assert!(!fold_backend(
            &mut app,
            &mut Kept::default(),
            &mut Attached::default(),
            &mut Unwatched,
        ));
        assert!(!fold_backend(
            &mut app,
            &mut Kept::default(),
            &mut Detached,
            &mut Unwatched,
        ));
    }

    #[test]
    fn a_file_the_session_changed_asks_the_repository_to_be_read_again() {
        let mut app = App::new(Repo::default()).attached();
        let mut backend = Attached {
            produces: vec![
                SessionEvent::FileChange {
                    path: "src/lib.rs".to_owned(),
                    added: Some(3),
                    removed: Some(1),
                    hunks: Vec::new(),
                },
                SessionEvent::AssistantMessage {
                    text: "done".to_owned(),
                },
            ],
            ..Attached::default()
        };
        let mut watch = Watching::default();

        fold_backend(&mut app, &mut Kept::default(), &mut backend, &mut watch);

        assert_eq!(
            watch.nudges, 1,
            "the edit the operator watched happen left the pane a cadence behind"
        );
    }

    #[test]
    fn a_turn_that_changed_no_file_leaves_the_repository_unread() {
        let mut app = App::new(Repo::default()).attached();
        let mut backend = Attached {
            produces: vec![SessionEvent::AssistantMessage {
                text: "nothing to change".to_owned(),
            }],
            ..Attached::default()
        };
        let mut watch = Watching::default();

        fold_backend(&mut app, &mut Kept::default(), &mut backend, &mut watch);

        assert_eq!(watch.nudges, 0);
    }

    #[test]
    fn a_read_that_has_not_come_back_leaves_what_the_shell_had() {
        let read = Repo {
            name: "niobe".to_owned(),
            branch: Some("main".to_owned()),
            ahead: Some(3),
            ..Repo::default()
        };
        let mut app = App::new(Repo::default());
        let mut watch = Watching {
            reads: VecDeque::from(vec![read.clone()]),
            nudges: 0,
        };

        if let Some(repo) = watch.look() {
            app.set_repo(repo);
        }
        assert_eq!(app.repo(), &read);

        // The next tick: nothing finished, nothing failed into the pane.
        if let Some(repo) = watch.look() {
            app.set_repo(repo);
        }
        assert_eq!(app.repo(), &read);
    }

    #[test]
    fn a_reply_is_looked_for_often_enough_to_read_as_it_arrives() {
        assert!(BUSY_TICK < TICK);
        assert!(
            BUSY_TICK.as_millis() >= 16,
            "looking more often than the frame budget would spend the time drawing"
        );
    }
}
