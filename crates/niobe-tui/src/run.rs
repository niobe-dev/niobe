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
use crate::journal::Journal;
use crate::terminal::{Shutdown, TerminalGuard, install_panic_hook};
use crate::ui;

/// How long the loop waits for a key before looking at the shutdown flag.
///
/// A signal is noticed within one tick, so this is also the worst case between
/// a SIGTERM and the terminal being handed back.
const TICK: Duration = Duration::from_millis(100);

/// Runs the shell on `app` until the operator quits or the process is asked to
/// stop, handing every event the operator produces to `journal`.
///
/// `app` may already hold a session: a resumed one is folded in by the caller
/// before the shell opens.
///
/// Installs the panic hook and the signal handlers first, so that every way out
/// of the function — including the ways that do not return from it — puts the
/// terminal back.
pub fn run(mut app: App, journal: &mut dyn Journal) -> io::Result<()> {
    install_panic_hook();
    let shutdown = Shutdown::install()?;

    let mut guard = TerminalGuard::enter(io::stdout())?;
    // No `Terminal::clear` here: the alternate screen starts blank and the
    // first draw covers it. `clear` also asks the terminal where its cursor is
    // and waits for the reply, which never comes when stdin is a pipe.
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let result = event_loop(&mut terminal, &mut app, journal, &shutdown);

    // Explicit, so that a restore failure is reported rather than swallowed by
    // `Drop`. Dropping the guard afterwards is a no-op.
    guard.restore()?;
    result
}

// `Backend::Error` is associated, so the loop names the one it propagates
// rather than being generic over an error it could not convert.
fn event_loop<B: Backend<Error = io::Error>>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    journal: &mut dyn Journal,
    shutdown: &Shutdown,
) -> io::Result<()> {
    while !app.should_quit() {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(TICK)? {
            match event::read()? {
                // Windows reports a press and a release; acting on both would
                // send every prompt twice.
                Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
                // The next draw reads the new size; nothing to do here.
                Event::Resize(_, _) => {}
                _ => {}
            }
            keep_produced(app, journal);
        }

        if shutdown.requested() {
            app.quit();
        }
    }

    Ok(())
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
