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
use crate::terminal::{Shutdown, Stop, TerminalGuard, install_panic_hook};
use crate::ui;

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
/// `backend` and to `journal`, and folding in everything `backend` produces.
///
/// `app` may already hold a session: a resumed one is folded in by the caller
/// before the shell opens.
///
/// Installs the panic hook and the signal handlers first, so that every way out
/// of the function — including the ways that do not return from it — puts the
/// terminal back.
pub fn run(mut app: App, journal: &mut dyn Journal, backend: &mut dyn Bridge) -> io::Result<Ended> {
    install_panic_hook();
    let shutdown = Shutdown::install()?;
    let wait = Wait::on_the_terminal();

    let mut guard = TerminalGuard::enter(io::stdout())?;
    // No `Terminal::clear` here: the alternate screen starts blank and the
    // first draw covers it. `clear` also asks the terminal where its cursor is
    // and waits for the reply, which never comes when stdin is a pipe.
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let ended = event_loop(&mut terminal, &mut app, journal, backend, &shutdown, &wait);

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
    journal: &mut dyn Journal,
    backend: &mut dyn Bridge,
    shutdown: &Shutdown,
    wait: &Wait,
) -> io::Result<Ended> {
    let mut ended = Ended::Quit;

    while !app.should_quit() {
        let producing = fold_backend(app, journal, backend);
        terminal.draw(|frame| ui::draw(frame, app))?;

        let tick = if producing { BUSY_TICK } else { TICK };
        match wait.input(tick)? {
            Input::Ready => {
                read_input(app)?;
                send_produced(app, journal, backend);
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

        match shutdown.requested() {
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
fn send_produced(app: &mut App, journal: &mut dyn Journal, backend: &mut dyn Bridge) {
    for event in app.take_produced() {
        if let Err(error) = journal.append(&event) {
            app.not_kept(&error.to_string());
        }
        // Only the operator's own turns are sent on. Everything else the shell
        // produces is a record of what happened here, and the backend has no
        // use for what it did not ask for.
        if let SessionEvent::UserMessage { text } = &event
            && app.is_attached()
            && let Err(error) = backend.send(text)
        {
            app.not_sent(&error.to_string());
        }
    }
}

/// Folds in everything the backend has produced and keeps it, and says whether
/// there was any so that the loop can look again sooner while a reply arrives.
///
/// The backend's events go to the journal as the operator's do: a resumed
/// session shows the whole conversation or it shows half of one. They do not go
/// through `take_produced`, which is the queue of what was done *here* and is
/// what gets sent back to the backend.
fn fold_backend(app: &mut App, journal: &mut dyn Journal, backend: &mut dyn Bridge) -> bool {
    let events = backend.drain();
    if events.is_empty() {
        return false;
    }
    for event in &events {
        app.apply(event);
        if let Err(error) = journal.append(event) {
            app.not_kept(&error.to_string());
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Repo;
    use crate::bridge::{BridgeError, Detached};
    use crate::journal::JournalError;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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

        fn drain(&mut self) -> Vec<SessionEvent> {
            std::mem::take(&mut self.produces)
        }
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

        send_produced(&mut app, &mut journal, &mut backend);

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

        send_produced(&mut app, &mut journal, &mut Detached);

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

        send_produced(&mut app, &mut Kept::default(), &mut backend);

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

        send_produced(&mut app, &mut journal, &mut Detached);

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

        let producing = fold_backend(&mut app, &mut journal, &mut backend);

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
    fn a_tick_that_drained_nothing_is_not_a_backend_producing() {
        let mut app = App::new(Repo::default()).attached();

        assert!(!fold_backend(
            &mut app,
            &mut Kept::default(),
            &mut Attached::default()
        ));
        assert!(!fold_backend(&mut app, &mut Kept::default(), &mut Detached));
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
