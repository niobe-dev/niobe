// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The `niobe` binary.
//!
//! Opens the shell on a new session, on one it recorded earlier or on one the
//! `claude` CLI recorded for itself, under a profile from the config; lists the
//! sessions a repository can carry on, the profiles defined for it and the
//! prices costs are computed from; and folds a JSON Lines event log for
//! development. It is the only crate that names the shell, the config, the
//! ledger and the session store together, so it is where they are joined.

// The CLI is the one place in the workspace that writes to the terminal
// directly rather than through the TUI.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod args;
mod backend;
mod commands;
mod config;
mod consent;
mod desktop;
mod history;
mod images;
mod journal;
mod prices;
mod printable;
mod profiles;
mod reaper;
mod repo;
mod rules;
mod sessions;
mod summary;

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime};

use niobe_ledger::Date;
use niobe_store::{Recorder, SessionId, read_log};
use niobe_tui::app::{App, ConfigFile, Places};
use niobe_tui::history::{NoHistory, Target};
use niobe_tui::journal::Unrecorded;
use niobe_tui::theme::{self, Depth};
use niobe_tui::{Detached, Ended, Forgotten, NoImages, NoShell, Theme, Unwatched};

/// Prints a line the way `println!` does, with anything in it that would act
/// on the terminal shown as text instead; see [`printable`]. Every line this
/// binary prints goes through it, because most carry a name, a path or a
/// prompt that came from a config, the store or a transcript.
macro_rules! say {
    ($($arg:tt)*) => {
        println!("{}", crate::printable::printable(&format!($($arg)*)))
    };
}

use crate::args::{Command, Invocation, Resume};
use crate::journal::StoreJournal;
use crate::rules::ConfigRules;

fn main() -> ExitCode {
    let parsed = args::utf8(std::env::args_os().skip(1)).and_then(|args| args::parse(&args));
    let result = match parsed {
        Ok(invocation) => run(invocation),
        Err(usage) => Err(format!("{usage}\nRun `niobe --help` for usage.")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => ExitCode::from(report(&message, &mut io::stderr())),
    }
}

/// What the process exits with when a failure's reason was written to standard
/// error.
///
/// Written, not read: where standard error points is the operator's
/// arrangement, and a write that reported every byte proves nothing about what
/// is at the other end. `/dev/null` takes the message and drops it; a
/// descriptor closed before the process started takes it too, because the
/// standard library turns the `EBADF` such a write raises into a write that
/// reported every byte. Neither is distinguishable from a terminal, so this
/// status promises only what `niobe` did.
const FAILED: u8 = 1;

/// What it exits with when the failure could not be written.
///
/// The reason is lost, so the status is all that is left to carry it: whatever
/// started `niobe` can tell a failure whose reason is on standard error from
/// one whose reason went nowhere, and go looking for the session in the store
/// rather than for the message.
const FAILED_UNREPORTED: u8 = 2;

/// Writes a failure's reason to standard error, and says what the process is to
/// exit with.
///
/// The TUI restores the terminal before returning, so this reaches a usable
/// screen — unless the terminal itself is what went. Written with `writeln!`
/// rather than `eprintln!` because the macro panics when the write fails, and
/// the write fails exactly when the terminal the message would go to has gone:
/// the shell draws at the top of every tick, so a terminal closing under a
/// session can raise an error and take away the stream that error is reported
/// on in the same instant. Reported by a macro that panics, that session ends
/// as exit 101 with nothing on either stream, which reads as a bug in `niobe`.
///
/// The failure is not swallowed either way. It is reported where it can be
/// read, or in the exit status where it cannot — never dropped, which is what
/// ignoring the write would do to every other reason a write to standard error
/// fails.
fn report(message: &str, stderr: &mut dyn Write) -> u8 {
    match writeln!(stderr, "niobe: {}", printable::printable(message)) {
        Ok(()) => FAILED,
        Err(_) => FAILED_UNREPORTED,
    }
}

fn run(
    Invocation {
        command,
        profile,
        budget,
        theme,
    }: Invocation,
) -> Result<(), String> {
    let profile = profile.as_deref();
    let asked = Asked {
        budget,
        theme,
        depth: Depth::from_env(
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var("NO_COLOR").ok().as_deref(),
        ),
    };
    let mut next = match command {
        Command::Shell => shell(profile, &asked)?,
        Command::Resume(Resume::Recorded(session)) => resume(session, profile, &asked)?,
        Command::Resume(Resume::Imported(session)) => import(&session, profile, &asked)?,
        other => return run_other(other, profile, &asked),
    };
    // The operator chose another session in the history dialog: the one the
    // shell held has ended and is saved, and the chosen one opens the way
    // `niobe --resume` would open it.
    while let Some(target) = next {
        next = open(&target, profile, &asked)?;
    }
    Ok(())
}

/// Opens `target` in place of the session that just ended, and returns the
/// one the operator chose next, if they chose one.
fn open(target: &Target, profile: Option<&str>, asked: &Asked) -> Result<Option<Target>, String> {
    match target {
        Target::Recorded(id) => {
            let session = id
                .parse()
                .map_err(|e| format!("session {id} cannot be opened: {e}"))?;
            resume(session, profile, asked)
        }
        Target::Claude(id) => import(id, profile, asked),
    }
}

/// The commands that do not open a shell on a session.
fn run_other(command: Command, profile: Option<&str>, asked: &Asked) -> Result<(), String> {
    match command {
        Command::Shell | Command::Resume(_) => Ok(()),
        Command::Sessions => list_sessions(),
        Command::Profiles => list_profiles(profile),
        Command::Trust { yes } => trust(yes),
        Command::Untrust => untrust(),
        Command::Prices(model) => list_prices(model.as_deref()),
        Command::Replay(log) => replay(&log, asked),
        Command::Help => {
            print_help();
            Ok(())
        }
        Command::Version => {
            say!("{} {}", niobe_core::APP_NAME, niobe_core::VERSION);
            Ok(())
        }
    }
}

/// What the command line asked of a session, beyond which one it is.
///
/// All of it applies to every way a session is opened — a new one, a recorded one
/// resumed, one read in from the `claude` CLI — so they travel together rather
/// than as a widening list of arguments each of those three repeats.
#[derive(Debug, Clone, Copy)]
struct Asked {
    /// The most the session may spend, in USD.
    budget: Option<f64>,
    /// The palette the shell opens in.
    theme: Option<Theme>,
    /// How many colours the terminal says it draws, which the palette is
    /// drawn at. Read from the environment here because the shell does not
    /// read the environment.
    depth: Depth,
}

/// The palette a session opens in: what `--theme` asked for, or what the
/// config names, or the default.
///
/// A name in the config that no theme answers to is reported at its line
/// rather than falling back: an operator who wrote a theme into their config
/// and got the usual one would read it as the file not being loaded. View ›
/// Theme… changes it from here for the rest of the session.
fn chosen_theme(asked: &Asked, loaded: &config::Loaded) -> Result<Theme, String> {
    if let Some(theme) = asked.theme {
        return Ok(theme);
    }
    let Some(named) = loaded.config.theme() else {
        return Ok(Theme::default());
    };
    Theme::by_name(named.name()).ok_or_else(|| {
        named
            .invalid(&format!(
                "`{}` is not a theme; expected {}",
                named.name(),
                theme::listed()
            ))
            .to_string()
    })
}

/// Refuses a `--budget` for a session on a profile billed as a plan.
///
/// A budget is counted in dollars, and a plan has none to count: no money
/// moves with the work, and what stops a plan user is the plan's usage
/// windows, which the shell shows. Refused before anything starts, so the
/// operator learns it from the command they typed rather than from a session
/// that runs against a figure it does not show. A profile that leaves its
/// billing to the backend is not refused: nothing yet says which it is.
fn budget_fits(asked: &Asked, selected: Option<&niobe_config::Selected<'_>>) -> Result<(), String> {
    let Some(selected) = selected.filter(|_| asked.budget.is_some()) else {
        return Ok(());
    };
    match selected.profile.billing() {
        Some(niobe_core::Billing::Plan) => Err(format!(
            "--budget is counted in money, and profile `{}` is billed as a plan, which is \
             limited by its usage windows, not by money; the Usage pane shows how much of \
             each is left",
            selected.name
        )),
        Some(niobe_core::Billing::Metered) | None => Ok(()),
    }
}

/// Opens the shell on a new session, recorded into this repository's store.
///
/// Without a terminal there is nothing to draw into and raw mode would fail, so
/// a piped or redirected run prints the help instead of an errno. The config is
/// read first either way, so a config that cannot be used is reported rather
/// than hidden behind the help.
fn shell(profile: Option<&str>, asked: &Asked) -> Result<Option<Target>, String> {
    let root = repo::root(&cwd()?);
    let Some(loaded) = consented(&root, asked)? else {
        return Ok(None);
    };
    let selected = loaded.select(profile)?;
    budget_fits(asked, selected.as_ref())?;
    let app = say_untrusted(
        say_prices(
            App::new(repo::describe(&root))
                .with_rules(loaded.config.allowed().clone())
                .with_places(places(&root, &loaded))
                .with_depth(asked.depth)
                .with_theme(chosen_theme(asked, &loaded)?),
        ),
        &loaded,
    );
    let app = match &selected {
        Some(selected) => app.with_profile(config::named(*selected)),
        None => app,
    };
    let app = match asked.budget {
        Some(budget) => app.with_budget(budget),
        None => app,
    };

    if !std::io::stdout().is_terminal() {
        print_help();
        return Ok(None);
    }
    keys_can_be_read()?;

    // Checked before anything starts, so a store the repository has linked
    // elsewhere is refused on a screen that is still the operator's, rather
    // than on the first event the session records.
    repo::store_stays_inside(&root)?;

    // Spawned after the terminal check and before the shell takes the screen:
    // a piped run starts no subprocess, and a backend that will not start says
    // why on a screen that is still the operator's.
    let mut backend = backend::attach(
        &root,
        selected.as_ref(),
        &backend::Attach {
            budget_usd: asked.budget,
            title: loaded.config.titles() == niobe_config::Titles::Model,
            ..backend::Attach::default()
        },
        std::env::var_os(niobe_bridge_claude::transcript::CONFIG_DIR_VAR),
        std::env::var_os("HOME"),
    )?;
    let app = if backend.attached() {
        app.attached()
    } else {
        app
    };
    // A command typed after `!` runs whether or not a backend does: it is the
    // operator's, not the agent's.
    let app = app.runs_commands().remembers();

    let mut journal = StoreJournal::pending(root.clone());
    let mut rules = ConfigRules::at(&root);
    // Started with the shell and stopped with it: the thread behind it reads
    // the repository while the session runs, and a piped run never gets here.
    let mut watching = repo::watch(&root);
    let mut commands = commands_for(&root, &mut backend);
    let ended = niobe_tui::run(
        app,
        niobe_tui::Around {
            journal: &mut journal,
            backend: backend.bridge(),
            rules: &mut rules,
            watch: &mut watching,
            shell: &mut commands,
            images: &mut images_for(&root),
            desktop: &mut desktop::System::new(),
            history: &mut history_for(&root, selected.as_ref()),
        },
    )
    .map_err(|e| e.to_string())?;

    let next = match ended {
        // There is nothing left to print on: the terminal the shell drew on is
        // the one this line would go to, and writing to it now fails. The
        // session is recorded either way, and `niobe sessions` lists it.
        Ended::TerminalGone => return Ok(None),
        Ended::Quit => None,
        Ended::Open(target) => Some(target),
    };
    let finished = journal.finish();
    say_unsaved(&finished);
    if let Some(session) = finished.session {
        say!(
            "session {session} saved in {} — `niobe --resume {session}` continues it",
            repo::store_path(&root).display()
        );
    }
    Ok(next)
}

/// The earlier sessions of `root`, and the `claude` CLI's own there where the
/// profile's environment, or this process's, says where it keeps them.
fn history_for(
    root: &Path,
    selected: Option<&niobe_config::Selected<'_>>,
) -> history::StoreHistory {
    history::StoreHistory::new(
        root,
        backend::transcripts(
            selected,
            root,
            std::env::var_os(niobe_bridge_claude::transcript::CONFIG_DIR_VAR),
            std::env::var_os("HOME"),
        ),
        niobe_tui::clock::Clock::system(),
    )
}

/// The images the operator attaches, where a relative path is `root`'s, as it
/// is the agent's.
fn images_for(root: &Path) -> images::Clipboard {
    images::Clipboard::at(root, std::env::var_os("HOME").map(PathBuf::from))
}

/// The operator's `!` commands, run at `root` — where the agent runs, so a
/// path means the same thing to both — with a reaper told of their groups and
/// of the backend's.
///
/// A session whose reaper cannot be started still stops what it started on
/// every way out it is given; only SIGKILL gives it none.
fn commands_for(root: &Path, backend: &mut backend::Attachment) -> commands::Commands {
    let commands = commands::Commands::at(root);
    let Ok(reaper) = reaper::Reaper::start() else {
        return commands;
    };
    let commands = commands.reaped_by(reaper);
    match backend.take_process_groups() {
        Some(groups) => commands.reaping_the_backend(groups),
        None => commands,
    }
}

/// Opens the shell on a recorded session and keeps recording into it. Without
/// a terminal, prints what the session folds to.
fn resume(
    session: SessionId,
    profile: Option<&str>,
    asked: &Asked,
) -> Result<Option<Target>, String> {
    let root = repo::root(&cwd()?);
    let Some(loaded) = consented(&root, asked)? else {
        return Ok(None);
    };
    let selected = loaded.select(profile)?;
    budget_fits(asked, selected.as_ref())?;
    let mut app = say_untrusted(
        say_prices(
            App::new(repo::describe(&root))
                .with_rules(loaded.config.allowed().clone())
                .with_places(places(&root, &loaded))
                .with_depth(asked.depth)
                .with_theme(chosen_theme(asked, &loaded)?),
        ),
        &loaded,
    );
    if let Some(selected) = &selected {
        app = app.with_profile(config::named(*selected));
    }
    if let Some(budget) = asked.budget {
        app = app.with_budget(budget);
    }

    let started = Instant::now();
    // Without a terminal the session is only read and printed, so the store
    // is opened as a reader: one on a read-only mount still prints.
    let piped = !std::io::stdout().is_terminal();
    let opened = match piped {
        true => repo::read_existing_store(&root)?,
        false => repo::open_existing_store(&root)?,
    };
    let store = opened.ok_or_else(|| {
        format!(
            "no session {session}: nothing has been recorded in {}",
            root.display()
        )
    })?;
    if !store.has_session(session).map_err(|e| e.to_string())? {
        return Err(niobe_store::StoreError::NoSuchSession(session).to_string());
    }
    let stored = store.events(session).map_err(|e| e.to_string())?;
    // At the times the store recorded, not at the time it was read: a session
    // resumed on Thursday did not happen on Thursday.
    let clock = niobe_tui::clock::Clock::system();
    app.extend_at(stored.iter().map(|s| (&s.event, clock.at(s.at))));
    let elapsed = started.elapsed();

    if piped {
        print_summary(
            &format!(
                "session {session} · {} events · loaded and folded in {}",
                stored.len(),
                millis(elapsed)
            ),
            &app,
        );
        return Ok(None);
    }
    keys_can_be_read()?;
    let recorder = Recorder::resume(store, session).map_err(|e| e.to_string())?;
    let recorded = app.session().meta().map(|meta| meta.profile.clone());
    let selected_name = selected.as_ref().map(|selected| selected.name);
    if let Some(said) = profile_changed(recorded.as_deref(), selected_name) {
        app = app.with_notice("another profile", "", &said);
    }
    // A record that stops in the middle of a turn is one whose process was
    // killed under it; the turn is closed where it stops, and the prompt the
    // operator is about to type opens a turn of its own.
    app.leave();

    // The recorded session says what the backend called it, which is the only
    // id that can hand its transcript back: Niobe's session id names the
    // recording, not the conversation.
    let continuing = app
        .session()
        .meta()
        .and_then(|meta| meta.backend_session.clone());
    // The recorded session also says how it was gating tool calls when it
    // stopped, and the backend is started that way: a session that came back
    // asking about everything it had been told to stop asking about would have
    // lost a decision the operator made.
    let mut backend = backend::attach(
        &root,
        selected.as_ref(),
        &backend::Attach {
            resume: continuing,
            mode: app.session().mode(),
            budget_usd: asked.budget,
            title: false,
        },
        std::env::var_os(niobe_bridge_claude::transcript::CONFIG_DIR_VAR),
        std::env::var_os("HOME"),
    )?;
    let app = if backend.attached() {
        app.attached()
    } else {
        app
    };
    // A command typed after `!` runs whether or not a backend does: it is the
    // operator's, not the agent's.
    let app = app.runs_commands().remembers();

    let mut journal = StoreJournal::open(recorder);
    let mut rules = ConfigRules::at(&root);
    let mut watching = repo::watch(&root);
    let mut commands = commands_for(&root, &mut backend);
    let ended = niobe_tui::run(
        app,
        niobe_tui::Around {
            journal: &mut journal,
            backend: backend.bridge(),
            rules: &mut rules,
            watch: &mut watching,
            shell: &mut commands,
            images: &mut images_for(&root),
            desktop: &mut desktop::System::new(),
            history: &mut history_for(&root, selected.as_ref()),
        },
    )
    .map_err(|e| e.to_string())?;
    let next = match ended {
        Ended::TerminalGone => return Ok(None),
        Ended::Quit => None,
        Ended::Open(target) => Some(target),
    };
    say_unsaved(&journal.finish());
    Ok(next)
}

/// Says how many of a session's last events the store refused after the
/// shell had closed, which the shell could no longer put on screen: a resumed
/// session will not show them, and the operator is owed knowing that.
fn say_unsaved(finished: &journal::Finished) {
    if finished.unsaved == 0 {
        return;
    }
    let events = if finished.unsaved == 1 {
        "event"
    } else {
        "events"
    };
    say!(
        "the session store did not save the last {} {events}: {}",
        finished.unsaved,
        finished
            .error
            .as_deref()
            .unwrap_or("the session store stopped")
    );
}

/// Opens the shell on a session the `claude` CLI recorded, reading its history
/// into a session of Niobe's own and asking the CLI to carry the conversation
/// on. Without a terminal, prints what the transcript folds to.
///
/// Nothing is written back to the CLI's own store. What is read becomes a
/// Niobe session like any other, so from here on `niobe --resume <number>`
/// continues it and every total on screen is a fold over the same events.
fn import(session: &str, profile: Option<&str>, asked: &Asked) -> Result<Option<Target>, String> {
    let root = repo::root(&cwd()?);
    let Some(loaded) = consented(&root, asked)? else {
        return Ok(None);
    };
    let selected = loaded.select(profile)?;
    budget_fits(asked, selected.as_ref())?;
    let theme = chosen_theme(asked, &loaded)?;

    let started = Instant::now();
    let events = backend::history(
        selected.as_ref(),
        &root,
        session,
        std::env::var_os(niobe_bridge_claude::transcript::CONFIG_DIR_VAR),
        std::env::var_os("HOME"),
    )?;
    let mut app = say_untrusted(
        say_prices(
            App::new(repo::describe(&root))
                .with_rules(loaded.config.allowed().clone())
                .with_places(places(&root, &loaded))
                .with_depth(asked.depth)
                .with_theme(theme),
        ),
        &loaded,
    );
    if let Some(selected) = &selected {
        app = app.with_profile(config::named(*selected));
    }
    if let Some(budget) = asked.budget {
        app = app.with_budget(budget);
    }
    app.extend(&events);
    let elapsed = started.elapsed();

    if !std::io::stdout().is_terminal() {
        print_summary(
            &format!(
                "claude session {session} · {} events · read and folded in {}",
                events.len(),
                millis(elapsed)
            ),
            &app,
        );
        return Ok(None);
    }
    keys_can_be_read()?;

    let mut backend = backend::attach(
        &root,
        selected.as_ref(),
        &backend::Attach {
            // The CLI's own id, which is the only thing that can hand the
            // conversation back.
            resume: Some(session.to_owned()),
            mode: app.session().mode(),
            budget_usd: asked.budget,
            title: false,
        },
        std::env::var_os(niobe_bridge_claude::transcript::CONFIG_DIR_VAR),
        std::env::var_os("HOME"),
    )?;

    // Recorded only once there is a terminal to continue it on and a backend
    // started to continue it with: a piped run is a look at the transcript,
    // and a resume that failed is one nobody carried on — recorded, either
    // would put a conversation in the list, and each retry another copy.
    let mut recorder = Recorder::new(repo::open_or_create_store(&root)?);
    for event in &events {
        recorder.record(event).map_err(|e| e.to_string())?;
    }
    // A transcript can stop in the middle of a turn — the CLI was quit or
    // killed under it — and the turn is closed there, as a resumed one is.
    app.leave();
    let app = if backend.attached() {
        app.attached()
    } else {
        app
    };
    // A command typed after `!` runs whether or not a backend does: it is the
    // operator's, not the agent's.
    let app = app.runs_commands().remembers();

    let mut journal = StoreJournal::open(recorder);
    let mut rules = ConfigRules::at(&root);
    let mut watching = repo::watch(&root);
    let mut commands = commands_for(&root, &mut backend);
    let ended = niobe_tui::run(
        app,
        niobe_tui::Around {
            journal: &mut journal,
            backend: backend.bridge(),
            rules: &mut rules,
            watch: &mut watching,
            shell: &mut commands,
            images: &mut images_for(&root),
            desktop: &mut desktop::System::new(),
            history: &mut history_for(&root, selected.as_ref()),
        },
    )
    .map_err(|e| e.to_string())?;

    let next = match ended {
        Ended::TerminalGone => return Ok(None),
        Ended::Quit => None,
        Ended::Open(target) => Some(target),
    };
    let finished = journal.finish();
    say_unsaved(&finished);
    if let Some(recorded) = finished.session {
        say!(
            "claude session {session} is niobe session {recorded} in {} — \
             `niobe --resume {recorded}` continues it",
            repo::store_path(&root).display()
        );
    }
    Ok(next)
}

/// Prints the sessions this repository can carry on, newest first: the ones
/// Niobe recorded, and the ones the `claude` CLI recorded for itself.
///
/// The config is read because a profile can point the CLI at another
/// configuration directory, which is where its own sessions would then be —
/// but a config that cannot be read does not stop the list. What a repository
/// can be carried on with is the one question that has to have an answer while
/// the config is being fixed, and where the CLI keeps its sessions is then read
/// from this process's own environment alone.
fn list_sessions() -> Result<(), String> {
    let root = repo::root(&cwd()?);
    let loaded = config::load(&root).ok();
    let selected = loaded
        .as_ref()
        .and_then(|loaded| loaded.select(None).ok().flatten());

    let recorded = match repo::read_existing_store(&root)? {
        Some(store) => store.sessions().map_err(|e| e.to_string())?,
        None => Vec::new(),
    };
    // The CLI's sessions are listed where they can be read; where they
    // cannot, that is said under niobe's own rather than hiding them.
    let (imported, unread) = match backend::recorded(
        selected.as_ref(),
        &root,
        std::env::var_os(niobe_bridge_claude::transcript::CONFIG_DIR_VAR),
        std::env::var_os("HOME"),
    ) {
        Ok(imported) => (imported, None),
        Err(error) => (Vec::new(), Some(error)),
    };

    if recorded.is_empty() && imported.is_empty() {
        say!("no sessions recorded in {}", root.display());
        if let Some(error) = unread {
            say!("{error}");
        }
        return Ok(());
    }
    let mut lines = match recorded.is_empty() {
        true => vec![format!("nothing recorded by niobe in {}", root.display())],
        false => sessions::table(&recorded),
    };
    if !imported.is_empty() {
        lines.push(String::new());
        lines.extend(sessions::recorded(&imported));
    }
    if let Some(error) = unread {
        lines.push(String::new());
        lines.push(error);
    }
    for line in lines {
        say!("{line}");
    }
    Ok(())
}

/// What the shell opens on when the operator chose to open it without
/// trusting this repository's config.
///
/// Said in the transcript rather than printed: the shell draws on the
/// alternate screen, and a line printed before it takes the terminal is gone
/// before anyone reads it. Said at all because a session running without the
/// environment its profile names is a session whose behaviour has no other
/// explanation on screen.
const UNTRUSTED: &str = "\
Opened without what it sets that needs your trust. The next session asks \
again, and `niobe trust` shows what it sets, values included, and asks too.";

/// The shell with a price sheet, so that a turn the backend has not priced yet
/// shows what it is costing rather than nothing.
///
/// A price file that will not read does not stop the session: the estimate is
/// a convenience, the session is the point. It is said in the transcript
/// rather than swallowed, because an operator who wrote a price file is owed
/// the reason it is not in force.
fn say_prices(app: App) -> App {
    match prices::load() {
        // Today's rates, for a session read back as for a live one: what is
        // owed is folded per model with no day on it, so there is no older
        // day to price it at. The help and the price table say so.
        Ok(loaded) => app.with_prices(Box::new(prices::Sheet::new(
            loaded.table,
            Date::of(SystemTime::now()),
        ))),
        Err(error) => app.with_notice(
            "no running cost estimate",
            "price table",
            &format!(
                "The price table did not read, so a turn shows a figure only once the \
                 backend reports one: {error}"
            ),
        ),
    }
}

/// The files the shell may name or hand to the desktop: the config files the
/// session was looked for in, and the repository's `CLAUDE.md`.
///
/// Only a regular file that is not a link is offered to be opened: what a
/// repository's file is, is the repository's choice, and the desktop runs
/// some files rather than showing them.
fn places(root: &Path, loaded: &config::Loaded) -> Places {
    let openable =
        |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file());
    let memory = root.join("CLAUDE.md");
    Places {
        config_files: loaded
            .searched
            .iter()
            .map(|path| ConfigFile {
                path: path.display().to_string(),
                exists: path.exists(),
                openable: openable(path),
            })
            .collect(),
        memory: openable(&memory).then(|| memory.display().to_string()),
    }
}

/// The shell, saying so when this repository's config has not been trusted.
fn say_untrusted(app: App, loaded: &config::Loaded) -> App {
    match &loaded.untrusted {
        None => app,
        Some(untrusted) => app.with_notice(
            "config not trusted",
            &untrusted.path.display().to_string(),
            UNTRUSTED,
        ),
    }
}

/// What the transcript says when a session recorded under one profile is
/// resumed under another: the account behind it may be billed another way,
/// and what was spent before is not what is spent from here on. Nothing where
/// the profile is the one it was recorded under, or either is not known.
fn profile_changed(recorded: Option<&str>, selected: Option<&str>) -> Option<String> {
    let (recorded, selected) = (recorded?, selected?);
    (recorded != selected).then(|| {
        format!(
            "This session was recorded under the profile `{recorded}` and goes on under \
             `{selected}`. Where the two are billed differently, its cost is drawn as both \
             from here on, since part of it was spent each way."
        )
    })
}

/// The config a session opens under, once the operator has been asked whether
/// to trust a repository config nobody has trusted; `None` where they quit
/// from the question.
///
/// Asked before anything is started, because the answer decides what the
/// backend is started with. Asked only where there is a terminal to ask on
/// and keys to answer with: a piped run says nothing and asks nothing, and
/// opens under the config with the file withheld, as before.
///
/// Asked again where the file changed between being read and being trusted:
/// what is recorded is the text the question showed, so a file changed since
/// is one nobody has read.
fn consented(root: &Path, asked: &Asked) -> Result<Option<config::Loaded>, String> {
    let mut loaded = config::load(root)?;
    if !std::io::stdout().is_terminal() || keys_can_be_read().is_err() {
        return Ok(Some(loaded));
    }
    while let Some(untrusted) = &loaded.untrusted {
        let behind = App::new(repo::describe(root))
            .with_depth(asked.depth)
            .with_theme(chosen_theme(asked, &loaded)?);
        let answer = niobe_tui::ask_trust(behind, consent::question(untrusted))
            .map_err(|e| e.to_string())?;
        match answer {
            None => return Ok(None),
            Some(niobe_tui::trust::Answer::NotNow) => break,
            Some(niobe_tui::trust::Answer::Trust) => {
                consent::record(&untrusted.path, &untrusted.text)?;
                loaded = config::load(root)?;
            }
        }
    }
    Ok(Some(loaded))
}

/// Shows what this repository's config would put in force, values included,
/// and records it as one the operator has read once they agree, so that the
/// `env`, `args`, `settings`, `auth_refresh` and `billing` of the profiles it
/// defines take effect, along with its `default_profile`, its `[permissions]`
/// rules and the profiles it defines under names the user's config already
/// uses.
///
/// `yes` is the operator agreeing on the command line. Without it they are
/// asked on the terminal, and where standard input is not one nothing is
/// recorded: a trust nobody was shown the values of is the one the question
/// in the shell exists to prevent. What is recorded is the text the listing
/// was made from, so a file changed while the question waited is not trusted
/// unread.
fn trust(yes: bool) -> Result<(), String> {
    let (path, text) = repo_config()?;
    let untrusted = config::as_written(&path, text)?;
    for line in consent::listing(&untrusted) {
        say!("{line}");
    }
    if !yes && !agreed(&path)? {
        say!("{} is not trusted; nothing was recorded", path.display());
        return Ok(());
    }
    consent::record(&untrusted.path, &untrusted.text)?;

    say!("trusted {}", path.display());
    say!(
        "the profiles it defines are in force here as it defines them, env, args, settings, \
         auth_refresh and billing included, and so are its default_profile and its \
         permissions; `niobe profiles` lists them, and editing the file asks again"
    );
    Ok(())
}

/// Whether the operator, asked on the terminal whether to trust the config at
/// `path`, answered yes. Anything but a yes, the end of input included, is a
/// no.
fn agreed(path: &Path) -> Result<bool, String> {
    if !io::stdin().is_terminal() {
        return Err(format!(
            "{} is not trusted: there is no terminal to ask on; read what it puts in force \
             above, and `niobe trust --yes` trusts it as it stands",
            path.display()
        ));
    }
    print!("trust it? [y/N] ");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|e| format!("no answer was read: {e}"))?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// Takes that back. The file stays where it is; what it may start a backend
/// with is what goes.
fn untrust() -> Result<(), String> {
    let path = repo::config_path(&repo::root(&cwd()?));
    let record = consent::record_path()?;
    let forgotten =
        niobe_config::trust::Trusted::update(&record, |trusted| Ok(trusted.forget(&path)))
            .map_err(|e| e.to_string())?;

    if !forgotten {
        say!("{} was not trusted", path.display());
        return Ok(());
    }
    say!(
        "{} is no longer trusted; the env, args, settings, auth_refresh and billing of the \
         profiles it defines are not in force, nor its default_profile, nor a profile it \
         names like one of yours",
        path.display()
    );
    Ok(())
}

/// This repository's config and what is in it, for a command that acts on the
/// file rather than on what it parses to.
fn repo_config() -> Result<(PathBuf, String), String> {
    let root = repo::root(&cwd()?);
    let path = repo::config_path(&root);
    repo::inside(&root, &path)?;
    match niobe_config::read::text(&path) {
        Ok(text) => Ok((path, text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "no config to trust: {} does not exist",
            path.display()
        )),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

/// Prints the profiles the config defines for this repository, the selected one
/// marked.
///
/// The `claude` CLI's own settings are read here, and only here, so that a
/// profile which names no settings file of its own says what this machine is
/// running it on.
fn list_profiles(profile: Option<&str>) -> Result<(), String> {
    let loaded = config::load(&repo::root(&cwd()?))?;
    let selected = loaded.selected(profile)?;

    if loaded.config.profiles().is_empty() {
        say!("{}", profiles::none_defined(&loaded.searched));
        return Ok(());
    }
    let bedrock = backend::bedrock_settings(
        std::env::var_os(niobe_bridge_claude::transcript::CONFIG_DIR_VAR),
        std::env::var_os("HOME"),
    );
    for line in profiles::table(
        &loaded.config,
        selected.as_ref().map(|s| s.name.as_str()),
        bedrock.as_deref(),
    ) {
        say!("{line}");
    }
    Ok(())
}

/// Prints the prices in force today, or every price `model` has had.
fn list_prices(model: Option<&str>) -> Result<(), String> {
    let loaded = prices::load()?;
    let lines = match model {
        None => prices::table(&loaded.table, Date::of(SystemTime::now())),
        Some(model) => match loaded.table.schedule(model) {
            Some(schedule) => prices::schedule(model, schedule),
            None => return Err(loaded.unpriced(model)),
        },
    };
    for line in lines {
        say!("{line}");
    }
    Ok(())
}

/// Folds a JSON Lines event log into the shell, for development. Nothing typed
/// into a replayed log is recorded. Without a terminal, prints the fold.
fn replay(log: &Path, asked: &Asked) -> Result<(), String> {
    let text = log_text(log)?;

    let started = Instant::now();
    let events = read_log(&text).map_err(|e| format!("{}: {e}", log.display()))?;
    // The same price sheet the shell and `--resume` get: a replayed log and a
    // resumed session that fold to the same totals must print the same cost,
    // or one of the two figures is teaching the operator to distrust both.
    let app = say_prices(App::new(repo::describe(&cwd()?)).with_depth(asked.depth));
    let mut app = match asked.theme {
        Some(theme) => app.with_theme(theme),
        None => app,
    };
    app.extend(&events);
    let elapsed = started.elapsed();

    if !std::io::stdout().is_terminal() {
        print_summary(
            &format!(
                "replayed {} events from {} in {}",
                events.len(),
                log.display(),
                millis(elapsed)
            ),
            &app,
        );
        return Ok(());
    }
    keys_can_be_read()?;

    // A recorded log is being looked at, not continued: nothing is attached
    // and nothing is kept.
    niobe_tui::run(
        app,
        niobe_tui::Around {
            journal: &mut Unrecorded,
            backend: &mut Detached,
            rules: &mut Forgotten,
            watch: &mut Unwatched,
            shell: &mut NoShell,
            images: &mut NoImages::default(),
            desktop: &mut desktop::System::new(),
            history: &mut NoHistory::default(),
        },
    )
    .map_err(|e| e.to_string())
    .map(|_| ())
}

fn print_summary(header: &str, app: &App) {
    say!("{header}");
    for line in summary::lines(app) {
        say!("{line}");
    }
}

/// Fails, before anything is started, where the shell would have no keys to
/// read: standard input is not a terminal and there is no controlling one to
/// read from instead — a job with no terminal, input redirected from a file.
/// Found out later, a backend would have been started and the screen taken
/// for a frame, only for the shell to stop at its first read.
///
/// Standard input that is not a terminal is fine where the process has one:
/// the keys are read from `/dev/tty`, as in `echo prompt | niobe`.
fn keys_can_be_read() -> Result<(), String> {
    if std::io::stdin().is_terminal() || std::fs::File::open("/dev/tty").is_ok() {
        return Ok(());
    }
    Err(
        "standard input is not a terminal, and there is no terminal to read keys from; \
         run niobe in one"
            .to_owned(),
    )
}

/// The most of a log `niobe replay` reads. The longest recorded session in
/// the repository's fixtures is under a megabyte; a log a hundred times that
/// is more than anyone replays to look at.
const LOG_LIMIT: u64 = 128 * 1024 * 1024;

/// The text of the event log at `log`: a regular file, read without waiting
/// on it and no further than [`LOG_LIMIT`], so that a FIFO or a device named
/// by mistake is refused at once rather than read for ever.
fn log_text(log: &Path) -> Result<String, String> {
    let failed = |e: &dyn std::fmt::Display| format!("cannot read {}: {e}", log.display());
    // Looked at before it is opened: opening a FIFO waits for a writer.
    if !std::fs::metadata(log).map_err(|e| failed(&e))?.is_file() {
        return Err(failed(&"not a regular file"));
    }
    let file = std::fs::File::open(log).map_err(|e| failed(&e))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(file, LOG_LIMIT.saturating_add(1)),
        &mut bytes,
    )
    .map_err(|e| failed(&e))?;
    if bytes.len() as u64 > LOG_LIMIT {
        return Err(failed(&format!(
            "larger than {} MiB",
            LOG_LIMIT / (1024 * 1024)
        )));
    }
    String::from_utf8(bytes).map_err(|e| failed(&e))
}

fn cwd() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|e| format!("cannot read the working directory: {e}"))
}

/// A duration the way the summary header prints it: milliseconds, to two
/// decimals, which is the resolution a 50 ms replay budget is read at.
fn millis(elapsed: Duration) -> String {
    format!("{:.2} ms", elapsed.as_secs_f64() * 1_000.0)
}

fn print_help() {
    say!(
        "\
{name} {version}
A terminal coding agent that keeps you aware of what is being built and how:
what the agent did, decided and touched, what it is doing now and what it cost.

USAGE:
    niobe                  Open the shell on a new session
    niobe --resume <id>    Open the shell on an earlier session and continue it
    niobe sessions         List the sessions this repository can carry on
    niobe profiles         List the profiles the config defines, the selected one marked
    niobe trust [--yes]    Show what this repository's config sets and let it start a
                           backend with that, once you agree
    niobe untrust          Take that back
    niobe prices [model]   List the prices in force today, or every price a model has had
    niobe replay <file>    Fold a JSON Lines event log into the shell (development)

OPTIONS:
    --profile <name>       Run under this profile instead of the default one
    --budget <amount>      Stop the session once it has cost this many dollars
                           (refused on a profile billed as a plan)
    --theme <name>         Draw the shell in this palette: cyber, classic, neo
                           or modern
    -h, --help             Print this help
    -V, --version          Print the version

EXIT STATUS:
    0   The session ended, including because the terminal it drew on went away
    1   Something failed; the reason was written to standard error, which
        discards it when standard error is closed or /dev/null
    2   Something failed and the reason could not be written to standard error

PROFILES:
    A profile is a backend plus the environment and arguments it runs with,
    defined in TOML:

        default_profile = \"personal\"

        [profiles.personal]
        backend = \"claude\"

        [profiles.work]
        backend = \"claude\"
        env = {{ CLAUDE_CODE_USE_BEDROCK = \"1\", AWS_PROFILE = \"work-sso\" }}
        auth_refresh = \"aws sso login --profile work-sso\"

        [profiles.personal-account]
        backend = \"claude\"
        settings = \"~/.config/niobe/claude-personal.json\"

    The user's config is ~/.config/niobe/config.toml ($XDG_CONFIG_HOME/niobe
    when that is set); a repository's is .niobe/config.toml at its root, and
    overrides the user's, replacing any profile of the same name whole. The
    env of a profile is passed on exactly as written. Model on the F-key bar
    offers every model the backend says the signed-in account can run, each
    by its name and the id it is taken by. A profile's models, such as
    models = [\"fable\", \"claude-opus-4-8\"], narrow that list to those, in
    that order, and are what is offered where the backend lists none, named
    the way it takes them; niobe never invents a model id.

    billing says how the account behind a profile is billed, \"plan\" or
    \"metered\". It decides what the Usage pane leads with: a plan's usage
    windows and tokens, with no dollar figure, since no money moves with the
    work, or the money a metered account is spending. Left out, the backend
    works it out where it can — an API key or a cloud provider is metered, a
    claude.ai login a plan — and until it has, the pane shows no dollar
    figure. A seat billed by use signs in exactly as a plan does, so that is
    the one to set.

    settings names a file the backend runs under, passed to the claude CLI as
    --settings <path>: its own settings file, which that CLI reads in front of
    the one it would otherwise use. That is what keeps a second account on a
    machine whose own settings configure the first — the CLI's settings env
    wins over the environment a process is started with, so a profile's env
    cannot take back what that file sets, and a file of your own can. Niobe
    merges nothing and rewrites nothing: the path is passed on, the CLI reads
    it. A ~ at the front is your home directory; the file has to be there, and
    a path that is not is reported at its line in the config before anything
    is started. niobe profiles shows which profiles name one — and, where this
    machine's own claude settings set CLAUDE_CODE_USE_BEDROCK, which do not.

    auth_refresh names the command that renews a backend's credentials, such
    as a single sign-on login. It is read, trust-gated and listed, and not run
    yet: niobe cannot yet tell a backend whose credentials have expired from
    one that failed another way, which is when it would run the command, and
    running a login before every session would ask you to sign in each time.
    Run it yourself when a session will not sign in.

TRUST:
    A repository's config arrives with the clone, and env, args, settings and
    auth_refresh are what a backend is started with — enough to point the
    official CLI at a host the repository chose, to sign it in as something
    else, or to name a command of its own to sign in with. So those four, and
    the rest that choose the account or its bill or let calls run unasked — a
    profile's billing, the default_profile, a profile named like one of yours,
    and [permissions] rules — do nothing until you have read the file and said
    so:

        niobe trust        this repository's config, as it now stands
        niobe untrust      take it back

    niobe trust prints what trusting the file would put in force, values
    included, and asks; only a yes records it. Where standard input is not a
    terminal there is no one to ask, and it records nothing unless --yes says
    you have read what it prints.

    The user's own config is never gated; it is the file you write. What was
    trusted is recorded as the config's SHA-256 in
    ~/.config/niobe/trusted.list, so editing the file — a pull, a rebase, your
    own edit — asks again. The shell asks as it opens, before the backend
    starts: it shows the file and what trusting it would put in force, values
    included, and Enter on \"Yes\" trusts it as niobe trust would. Answered
    \"No\", or until it is trusted, the profiles it defines keep their
    backend and their models and lose the rest, the transcript says so, and
    niobe profiles names what trusting would put in force.
    Answering \"always\" in the shell adds a rule to the repository's config.
    That write is niobe's own and can add no env, no args, no settings and no
    auth_refresh, so a file you had trusted stays trusted across it.

PRICES:
    Costs are computed from a price table bundled into niobe: USD per million
    tokens for each model id, each price dated from the day it took effect.
    They value the tokens a backend reported no cost for, at the rates in
    force today: a session replayed or resumed later is estimated at today's
    rates, not those of the day it ran. A cost the backend reported is shown
    as it reported it. A model id the table does not list is unpriced;
    nothing is guessed from a similar id.
    ~/.config/niobe/prices.toml ($XDG_CONFIG_HOME/niobe when that is set) has
    the same shape and replaces the whole price history of every id it lists.

SESSIONS:
    Every session is recorded, append-only, into .niobe/sessions.db at the root
    of the repository it runs in, or of the working directory outside one.
    With standard output redirected, --resume and replay print what the session
    folds to instead of opening the shell.

    niobe sessions lists two kinds. The first is niobe's own, by the number the
    store gave them. The second is the sessions the claude CLI recorded at the
    root of this repository, by the id it calls them by. The CLI keeps a
    session under the directory it was started in and resumes it only from
    there, and niobe runs it at the root, so one started in a subdirectory is
    not listed and cannot be carried on here. --resume with one of those
    reads the CLI's own transcript in as the history of a new niobe session and
    asks the CLI to carry the conversation on, so a session started in plain
    Claude Code continues here. From then on it is a niobe session like any
    other and its number resumes it.

    Nothing is ever written back into the CLI's own store. Its transcripts are
    under ~/.claude/projects (CLAUDE_CONFIG_DIR/projects when that is set), one
    directory per working directory it has run in; niobe reads the transcripts
    there and nothing else in that directory.

    The history an import brings in is the transcript's, so it carries what the
    CLI wrote down: the turns, the tool calls, the files they changed, the
    tokens each message was billed and what the session had cost when the CLI
    last closed it. A session the CLI has not closed yet has no cost recorded
    in it, and niobe shows what it can count rather than a figure nobody
    reported.

IN THE SHELL:
    Enter                  Send what is in the composer
    Shift+Enter            Open a new line in the composer, on a terminal that
                           can tell it from Enter, and under tmux with
                           extended-keys on (the bar names it only there)
    Ctrl+J                 Open a new line in the composer, on any terminal
    Alt+Enter              The same, where the terminal sends Option as Meta
    PgUp / PgDn            Scroll the transcript
    Up / Down              On the composer's first row, the prompt sent before
                           the one shown, from this session and then earlier
                           ones; on its last row, the one after, and past the
                           newest, what was being written
    Ctrl+R                 The history: every prompt sent here, filtered by
                           what is typed, Enter putting one in the composer
                           unsent; Tab turns it to the sessions, where Enter
                           ends this one and opens the chosen one in its place
    Ctrl+F                 Search the transcript: Up and Enter step to the
                           match above, Down to the one below, Esc puts the
                           view back where it was
    /                      At the start of a prompt, offer the commands and
                           skills the backend listed: Up / Down choose, Tab or
                           Enter put the name in the prompt, Esc leaves it as
                           typed
    @                      At the start of a word, offer the files git lists
                           in the repository, by their paths from its root,
                           where the agent runs: Up / Down choose,
                           Tab or Enter put the path in the prompt, Esc leaves
                           the word as typed
    !                      On an empty composer, run a command at the
                           repository's root, where the agent runs, instead
                           of sending a prompt:
                           Enter runs it, Esc goes back, and a second ! types
                           a prompt that starts with one
    Ctrl+G                 Stop the newest ! command still running, with
                           everything it started; again, the one before it

    A command run with ! is yours: no rule is consulted and no mode applies,
    as in any other terminal. It has no terminal of its own and nothing to
    read, runs until it ends, Ctrl+G stops it or niobe quits, and is kept in
    the session as a call, with the end of what it printed shown under it. A
    stopped command's shell is ended at once, so nothing more of its line
    runs; what it started is asked to end, and killed if it has not two
    seconds later. Its call ends as stopped by the operator. Ctrl+C does not stop one.
    The agent is not told it ran and does not see what it printed.
    Esc, Ctrl+C            While a turn runs, stop it: the call it is running
                           or the reply it is writing is cut off, and the
                           session stays for the next prompt. Ctrl+C closes
                           an open menu or dialog as Esc does; otherwise,
                           with no turn left to stop, a second Ctrl+C within
                           1.5 seconds quits, and one alone leaves what is
                           typed as it was. Esc leads to an F-key
    Shift+Tab              Cycle how tool calls are gated: plan, ask, auto
{fkeys}
    Ctrl+Q                 Quit

    Every F-key is also Esc and then its digit, 0 for the tenth, which is how
    the bar at the foot of the shell names them: on a Mac the top row is media
    keys unless Fn is held, and Esc and a digit reach every terminal. Alt and
    the digit does the same where the terminal sends Option as Meta. Esc and a
    menu's first letter, or Alt and the letter, open that menu, which holds
    every action the shell has, F-keys and all.

    When a backend stops for permission, the question is asked at the foot of
    the transcript and the turn waits on it:

    1-4, Up / Down         Choose: allow this call once, allow every call to
                           that tool, allow every call on the same target, or
                           deny it
    Enter                  Give the chosen answer
    Tab                    Write an answer instead: the call is refused and
                           your words are what the agent reads back
    Esc                    Decide later and go back to the composer; Esc
                           again returns to the question

MODE AND MODEL:
    Shift+Tab cycles the mode the session runs in — plan changes nothing, ask
    stops for every call no rule already allows, auto leaves the decision to
    the backend. The ask bar under the transcript names the one in force once a
    backend has said which it is. A mode a backend reports that niobe has no
    word for is carried as reported rather than forced into one of these three.

    Model on the F-key bar picks one of the models the backend offers, or of
    those the profile names. The
    switch applies from the next turn and keeps everything said so far: the
    running session is told to change, not replaced. The backend resolves the
    name it is given and says what it ended up on, which may be spelt
    differently from the way it was asked for.

THEME:
    cyber is green and magenta on black, classic is DOS blue, neo is green on
    black and modern is an editor's grey. --theme <name> opens the shell in
    one, theme = \"neo\" at the top of a config makes it the one every session
    opens in, and View › Theme… picks another while a session runs.

    classic is drawn in the sixteen colours the terminal names, so your own
    colour scheme is what they mean. cyber, neo and modern are drawn in their
    own 24-bit colours where COLORTERM is truecolor or 24bit, and in the
    sixteen everywhere else, so a session stays legible over SSH and in
    screen. NO_COLOR set to anything draws every theme in the sixteen.

BUDGET:
    --budget <amount> caps what a session may spend, in dollars, and niobe says
    so in the transcript once most of it is gone. The backend enforces the cap
    and checks it between turns rather than inside one, so a session can finish
    above the figure by what the turn that crosses the line costs. A profile
    billed as a plan refuses it: a plan is limited by its usage windows, not
    by money, and the Usage pane shows how much of each is left.

PERMISSIONS:
    An \"always\" answer is written into this repository's .niobe/config.toml as
    a rule, and answers the same prompt in every later session:

        [permissions]
        allow = [\"Read\", \"Bash(cargo test)\"]

    A bare tool name allows every call to it; a name and a target allow that
    target alone. A target ending in * matches by prefix — Bash(cargo *) — and
    only you write one: Niobe stores the target as it stood, never a guess at
    what else you meant. The * never covers a second command chained after
    the first (;, &&, |, a redirection or a substitution) or a path that climbs
    above the prefix with ..; a * inside a word, as in Bash(cat src/*), finishes
    that word and covers no argument after it. Such a call is asked about.
    Paths are read as written: a link under the prefix leads where it points.
    The user's config and the repository's both apply; a rule in either allows
    the call.

BACKENDS:
    A claude profile drives the official `claude` CLI as a subprocess, with the
    profile's environment, arguments and settings file and the repository as
    its working directory. Niobe never reads the CLI's credential files and never sets its
    user agent: whatever that binary is signed in as is what the session runs
    on. Every cost the CLI reports is one it computed from published prices:
    on a metered account Niobe shows it as the bill, and on a plan it shows
    none.

    Not implemented yet: the codex bridge and the native agent loop spawn
    nothing, so a session under one of those profiles has nowhere to send a
    prompt and says so.",
        name = niobe_core::APP_NAME,
        version = niobe_core::VERSION,
        fkeys = fkey_rows(),
    );
}

/// The help's rows for the F-key bar, read from the table the shell draws
/// the bar from, so the help cannot name a key the bar has given to
/// something else.
fn fkey_rows() -> String {
    (1u8..)
        .zip(niobe_tui::menu::FKEYS)
        .map(|(n, (digit, label, action))| {
            let keys = format!("F{n}, Esc {digit}");
            format!("    {keys:<23}{label}: {}", action.what())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_session_resumed_under_another_profile_says_both() {
        let said =
            super::profile_changed(Some("max"), Some("company")).expect("the change is said");
        assert!(
            said.contains("`max`") && said.contains("`company`"),
            "{said}"
        );
    }

    #[test]
    fn a_session_resumed_under_its_own_profile_says_nothing() {
        assert_eq!(super::profile_changed(Some("max"), Some("max")), None);
        assert_eq!(super::profile_changed(None, Some("max")), None);
        assert_eq!(super::profile_changed(Some("max"), None), None);
    }

    use super::*;

    /// A standard error that cannot be written to, the way the terminal a
    /// session drew on cannot once it has gone.
    struct Gone;

    impl Write for Gone {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("the terminal went away"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("the terminal went away"))
        }
    }

    #[test]
    fn a_failure_is_written_to_standard_error_and_fails() {
        let mut written = Vec::new();

        let status = report(
            "no session 9: nothing has been recorded in /tmp",
            &mut written,
        );

        assert_eq!(status, FAILED);
        assert_eq!(
            String::from_utf8(written).expect("what was written is UTF-8"),
            "niobe: no session 9: nothing has been recorded in /tmp\n"
        );
    }

    #[test]
    fn a_failure_that_could_not_be_written_fails_with_a_status_of_its_own() {
        let status = report("no session 9", &mut Gone);

        assert_eq!(status, FAILED_UNREPORTED);
        assert_ne!(FAILED_UNREPORTED, FAILED);
    }

    #[test]
    fn a_shell_opened_without_trusting_the_config_says_so_in_a_line_and_how_to_trust_it() {
        let path = PathBuf::from("/r/.niobe/config.toml");
        let loaded = config::Loaded {
            config: niobe_config::Config::default(),
            searched: vec![path.clone()],
            untrusted: Some(config::Untrusted {
                path: path.clone(),
                text: String::new(),
                config: niobe_config::Config::default(),
                replaces: Vec::new(),
            }),
        };

        let app = say_untrusted(App::new(niobe_tui::app::Repo::default()), &loaded);

        let entry = app
            .entries()
            .first()
            .expect("the shell opens on the notice");
        assert_eq!(entry.kind, niobe_tui::app::EntryKind::Notice);
        assert_eq!(entry.meta, path.display().to_string());
        assert!(
            entry.body.contains("`niobe trust` shows what it sets"),
            "{}",
            entry.body
        );
        assert!(entry.body.contains("asks again"), "{}", entry.body);
        // Short enough to read at a glance: the question that came before it
        // already said what the file sets.
        assert!(entry.body.len() < 160, "{}", entry.body);
    }

    #[test]
    fn a_shell_whose_config_needs_no_trust_opens_on_nothing() {
        let loaded = config::Loaded {
            config: niobe_config::Config::default(),
            searched: Vec::new(),
            untrusted: None,
        };

        let app = say_untrusted(App::new(niobe_tui::app::Repo::default()), &loaded);

        assert!(app.entries().is_empty());
    }

    #[test]
    fn durations_print_in_milliseconds_to_two_decimals() {
        assert_eq!(millis(Duration::from_micros(412)), "0.41 ms");
        assert_eq!(millis(Duration::from_millis(50)), "50.00 ms");
    }
}
