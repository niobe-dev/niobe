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

use crate::app::{App, Repo};
use crate::terminal::{Shutdown, TerminalGuard, install_panic_hook};
use crate::ui;

/// How long the loop waits for a key before looking at the shutdown flag.
///
/// A signal is noticed within one tick, so this is also the worst case between
/// a SIGTERM and the terminal being handed back.
const TICK: Duration = Duration::from_millis(100);

/// Runs the shell until the operator quits or the process is asked to stop.
///
/// Installs the panic hook and the signal handlers first, so that every way out
/// of the function — including the ways that do not return from it — puts the
/// terminal back.
pub fn run(repo: Repo) -> io::Result<()> {
    install_panic_hook();
    let shutdown = Shutdown::install()?;

    let mut guard = TerminalGuard::enter(io::stdout())?;
    // No `Terminal::clear` here: the alternate screen starts blank and the
    // first draw covers it. `clear` also asks the terminal where its cursor is
    // and waits for the reply, which never comes when stdin is a pipe.
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let mut app = App::new(repo);
    let result = event_loop(&mut terminal, &mut app, &shutdown);

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
        }

        if shutdown.requested() {
            app.quit();
        }
    }

    Ok(())
}
