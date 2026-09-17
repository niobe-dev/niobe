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

use crate::app::App;
use crate::input::{Input, Wait};
use crate::journal::Journal;
use crate::terminal::{Shutdown, TerminalGuard, install_panic_hook};
use crate::ui;

/// How long the loop waits for a key before looking at the shutdown flag.
///
/// A signal is noticed within one tick, so this is also the worst case between
/// a SIGTERM and the terminal being handed back, and between the terminal going
/// away and the session ending.
const TICK: Duration = Duration::from_millis(100);

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
/// `journal`.
///
/// `app` may already hold a session: a resumed one is folded in by the caller
/// before the shell opens.
///
/// Installs the panic hook and the signal handlers first, so that every way out
/// of the function — including the ways that do not return from it — puts the
/// terminal back.
pub fn run(mut app: App, journal: &mut dyn Journal) -> io::Result<Ended> {
    install_panic_hook();
    let shutdown = Shutdown::install()?;
    let wait = Wait::on_the_terminal();

    let mut guard = TerminalGuard::enter(io::stdout())?;
    // No `Terminal::clear` here: the alternate screen starts blank and the
    // first draw covers it. `clear` also asks the terminal where its cursor is
    // and waits for the reply, which never comes when stdin is a pipe.
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let ended = event_loop(&mut terminal, &mut app, journal, &shutdown, &wait)?;

    match ended {
        Ended::Quit => {}
        // The drawing surface is abandoned rather than dropped: ratatui's
        // `Terminal` shows the cursor again as it drops and, when it cannot,
        // prints that failure to standard error. Standard error is the terminal
        // that has just gone, so the print fails too — and an `eprintln!` that
        // fails panics, which would end a session that merely lost its terminal
        // as a crash.
        Ended::TerminalGone => std::mem::forget(terminal),
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
fn outcome(ended: Ended, restored: io::Result<()>) -> io::Result<Ended> {
    match ended {
        Ended::Quit => restored.map(|()| Ended::Quit),
        Ended::TerminalGone => Ok(Ended::TerminalGone),
    }
}

// `Backend::Error` is associated, so the loop names the one it propagates
// rather than being generic over an error it could not convert.
fn event_loop<B: Backend<Error = io::Error>>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    journal: &mut dyn Journal,
    shutdown: &Shutdown,
    wait: &Wait,
) -> io::Result<Ended> {
    let mut ended = Ended::Quit;

    while !app.should_quit() {
        terminal.draw(|frame| ui::draw(frame, app))?;

        match wait.input(TICK)? {
            Input::Ready => {
                read_input(app)?;
                keep_produced(app, journal);
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

        if shutdown.requested() {
            app.quit();
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

/// Hands what the operator produced to the journal, and puts a failure on
/// screen for anything it could not keep. A store that stops writing does not
/// end the session: the operator decides whether to go on without it.
fn keep_produced(app: &mut App, journal: &mut dyn Journal) {
    for event in app.take_produced() {
        if let Err(error) = journal.append(&event) {
            app.not_kept(&error.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Repo;
    use crate::journal::JournalError;
    use niobe_core::event::Event as SessionEvent;
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

    fn app_with_a_sent_prompt() -> App {
        let mut app = App::new(Repo::default());
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
        let error = outcome(Ended::Quit, restore_failed()).expect_err("the restore failed");

        assert_eq!(error.to_string(), "the terminal went away");
    }

    #[test]
    fn a_session_whose_terminal_went_away_did_not_fail() {
        // The restore fails because the terminal is gone, which is the same
        // thing that ended the session.
        let ended = outcome(Ended::TerminalGone, restore_failed())
            .expect("a terminal that went away is not a failed session");

        assert_eq!(ended, Ended::TerminalGone);
    }

    #[test]
    fn a_clean_quit_that_handed_the_terminal_back_is_a_quit() {
        let ended = outcome(Ended::Quit, Ok(())).expect("nothing failed");

        assert_eq!(ended, Ended::Quit);
    }

    #[test]
    fn a_sent_prompt_reaches_the_journal() {
        let mut app = app_with_a_sent_prompt();
        let mut journal = Kept::default();

        keep_produced(&mut app, &mut journal);

        assert_eq!(
            journal.events,
            [SessionEvent::UserMessage {
                text: "x".to_owned()
            }]
        );
    }

    #[test]
    fn a_journal_that_refuses_leaves_a_failure_on_screen() {
        let mut app = app_with_a_sent_prompt();
        let mut journal = Kept {
            refuse: true,
            ..Kept::default()
        };

        keep_produced(&mut app, &mut journal);

        let last = app.entries().last().expect("an entry was pushed");
        assert_eq!(last.head, "not saved");
        assert!(last.body.ends_with("the store is read-only"));
    }
}
