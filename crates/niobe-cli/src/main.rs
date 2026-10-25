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

mod alerts;
mod args;
mod backend;
mod bench;
mod commands;
mod config;
mod consent;
mod cost;
mod desktop;
mod doctor;
mod headless;
mod history;
mod images;
mod journal;
mod memory;
mod overview;
mod playback;
mod prices;
mod printable;
mod profiles;
mod reaper;
mod repo;
mod rules;
mod sessions;
mod stdout;
mod summary;
mod themes;
mod trim;

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime};

use niobe_core::event::Event;
use niobe_core::permission::Rule;
use niobe_ledger::Date;
use niobe_store::{Recorder, SessionId, read_dated_log};
use niobe_tui::app::{App, ConfigFile, Places};
use niobe_tui::history::{NoHistory, Target};
use niobe_tui::journal::Unrecorded;
use niobe_tui::theme::{self, Depth};
use niobe_tui::{Detached, Ended, Forgotten, NoImages, NoShell, Theme, Unwatched};

/// Prints a line the way `println!` does, with anything in it that would act
/// on the terminal shown as text instead; see [`printable`]. Every line this
/// binary prints goes through it, because most carry a name, a path or a
/// prompt that came from a config, the store or a transcript. Unlike
/// `println!` it does not panic where the write fails; see [`stdout`].
macro_rules! say {
    ($($arg:tt)*) => {
        crate::stdout::line(&crate::printable::printable(&format!($($arg)*)))
    };
}

use crate::args::{Command, Invocation, Resume};
use crate::journal::StoreJournal;
use crate::prices::Spent;
use crate::rules::ConfigRules;

fn main() -> ExitCode {
    if let Some(dir) = args::hook_trim(std::env::args_os().skip(1)) {
        return hook_trim(&dir);
    }
    // Before the arguments: `--theme` may name one of the user's own.
    themes::register(config::user_file(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
        "themes",
    ));
    let parsed = args::utf8(std::env::args_os().skip(1)).and_then(|args| args::parse(&args));
    let result = match parsed {
        Ok(invocation) => run(invocation).and_then(|status| stdout::outcome().map(|()| status)),
        Err(usage) => Err(format!("{usage}\nRun `niobe --help` for usage.")),
    };

    match result {
        Ok(status) => ExitCode::from(status),
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

/// Runs what the command line asked for, and says what the process exits
/// with where it did not fail: zero, but for a headless turn the budget
/// stopped.
fn run(
    Invocation {
        command,
        profile,
        budget,
        theme,
        allow,
        terse,
    }: Invocation,
) -> Result<u8, String> {
    let profile = profile.as_deref();
    let asked = Asked {
        budget,
        theme,
        terse,
        depth: Depth::from_env(
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var("NO_COLOR").ok().as_deref(),
        ),
        notification: alerts::notification_for(
            std::env::var("TERM_PROGRAM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        ),
    };
    let mut next = match command {
        Command::Shell => shell(profile, &asked)?,
        Command::Resume(Resume::Recorded(session)) => resume(session, profile, &asked)?,
        Command::Resume(Resume::Imported(session)) => import(&session, profile, &asked)?,
        Command::Print { prompt, json } => {
            return print_turn(&prompt, json, profile, &allow, &asked);
        }
        Command::Bench {
            suite,
            json,
            timeout,
            ab,
        } => return run_bench(&suite, json, timeout, ab, profile, &allow, &asked),
        // Handed on whole: `run_other` matches every command itself.
        other => return run_other(other, profile, &asked).map(|()| 0),
    };
    // The operator chose another session in the history dialog: the one the
    // shell held has ended and is saved, and the chosen one opens the way
    // `niobe --resume` would open it.
    while let Some(target) = next {
        next = open(&target, profile, &asked)?;
    }
    Ok(0)
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

/// Where a session started under `trim = true` keeps the outputs it trims,
/// and the binary that trims them; `None` where the config leaves it off.
/// Refused where `.niobe/` leads out of the repository, since the whole
/// outputs would be written wherever it leads, and where git would offer what
/// is written there for a commit.
fn trim_for(root: &Path, on: bool) -> Result<Option<trim::Trim>, String> {
    if !on {
        return Ok(None);
    }
    let dir = repo::trim_dir(root);
    repo::writes_inside(root, &dir)?;
    trim::prepare(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    repo::keep_trim_unpublished(root)?;
    let exe = std::env::current_exe().map_err(|e| {
        format!("`trim = true` runs niobe as a hook, and its path is not known: {e}")
    })?;
    Ok(Some(trim::Trim { exe, dir }))
}

/// `niobe hook trim --raw <dir>`, which the `claude` CLI runs after each shell
/// call of a session under `trim = true`: the call on standard input, and on
/// standard output what the agent reads in place of its output, or nothing.
///
/// Answered before anything else is read — the user's themes, the config —
/// because the CLI waits on it after every shell command. Written to
/// standard output as it is, not through `printable`: the reader is the CLI,
/// which parses it as JSON, and JSON escapes every control character itself.
fn hook_trim(dir: &Path) -> ExitCode {
    if let Some(answer) = trim::hook(io::stdin().lock(), dir) {
        let mut out = io::stdout().lock();
        // A CLI that stopped reading has nothing to be told; the call is then
        // handed back as it was, which is what no answer means.
        let _unread = out.write_all(answer.as_bytes()).and_then(|()| out.flush());
    }
    ExitCode::SUCCESS
}

/// The commands that do not open a shell on a session.
fn run_other(command: Command, profile: Option<&str>, asked: &Asked) -> Result<(), String> {
    match command {
        Command::Shell | Command::Resume(_) | Command::Print { .. } | Command::Bench { .. } => {
            Ok(())
        }
        Command::Sessions => list_sessions(),
        Command::Overview(narrowing) => print_overview(narrowing),
        Command::Cost { narrowing, format } => print_cost(narrowing, format),
        Command::Profiles => list_profiles(profile),
        Command::Doctor { json } => run_doctor(json),
        Command::Trust { yes } => trust(yes),
        Command::Untrust => untrust(),
        Command::Prices(model) => list_prices(model.as_deref()),
        Command::Replay { log, speed } => replay(&log, speed, asked),
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
    /// Whether a new conversation asks for terse replies, where `--terse`
    /// said; `None` leaves it to the config.
    terse: Option<bool>,
    /// How many colours the terminal says it draws, which the palette is
    /// drawn at. Read from the environment here because the shell does not
    /// read the environment.
    depth: Depth,
    /// The desktop notification the terminal shows, where it shows one. Read
    /// from the environment here for the same reason.
    notification: Option<niobe_tui::Notification>,
}

/// The palette a session opens in: what `--theme` asked for, or what the
/// config names, or the default.
///
/// A name in the config that no theme answers to is reported at its line
/// rather than falling back: an operator who wrote a theme into their config
/// and got the usual one would read it as the file not being loaded. View ›
/// Theme… changes it from here for the rest of the session.
fn chosen_theme(asked: &Asked, loaded: &config::Loaded) -> Result<Theme, String> {
    if let Some(problem) = themes::problem() {
        return Err(problem.to_owned());
    }
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

/// The first Niobe session in this repository's store that carries the
/// conversation the `claude` CLI calls `conversation`, where there is one.
///
/// A store that cannot be read says nothing here: the conversation is read in
/// as a new session, and the store reports itself when that session records.
fn carried_conversation(root: &Path, conversation: &str) -> Option<SessionId> {
    let store = repo::read_existing_store(root).ok().flatten()?;
    store
        .conversations()
        .ok()?
        .into_iter()
        .filter(|(_, id)| id == conversation)
        .map(|(session, _)| session)
        .min()
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
    let tap = backend::stream_tap(&root, std::env::var_os(backend::STREAM_DIR_VAR))?;
    let app = say_untrusted(
        say_prices(
            Spent::on(today()),
            App::new(repo::describe(&root))
                .with_rules(loaded.config.allowed().clone())
                .with_places(places(&loaded))
                .with_depth(asked.depth)
                .with_keymap(loaded.config.keys().clone())
                .with_alerts(alerts::alerts(loaded.config.notify(), asked.notification))
                .with_catch_up(
                    away_after(loaded.config.catch_up()),
                    loaded.config.catch_up().automatic(),
                )
                .with_decisions_asked(loaded.config.decisions())
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
    let app = carry_windows(
        app,
        repo::read_existing_store(&root).ok().flatten().as_ref(),
        selected.as_ref(),
    );

    // Spawned after the terminal check and before the shell takes the screen:
    // a piped run starts no subprocess, and a backend that will not start says
    // why on a screen that is still the operator's.
    let attempt = backend::attach(
        &root,
        selected.as_ref(),
        &backend::Attach {
            budget_usd: asked.budget,
            title: loaded.config.titles() == niobe_config::Titles::Model,
            terse: asked.terse.unwrap_or(loaded.config.terse()),
            decisions: loaded.config.decisions(),
            tap: tap.clone(),
            trim: trim_for(&root, loaded.config.trim())?,
            ..backend::Attach::default()
        },
        std::env::var_os(niobe_bridge_claude::transcript::CONFIG_DIR_VAR),
        std::env::var_os("HOME"),
    );
    let (mut backend, app) = attached_or_noticed(&loaded, attempt, app)?;
    let app = say_tap(app, tap.as_deref());
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
            desktop: &mut desktop::System::new().reporting({
                let root = root.clone();
                move || doctor::block(&root, backend::claude_release_recorded)
            }),
            history: &mut history_for(&root, selected.as_ref()),
            memory: &mut memory::Files::at(
                root.clone(),
                selected.as_ref().map(|selected| selected.profile.clone()),
            ),
            overview: &mut overview::Page::at(root.clone()),
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

/// The same shell, with the plan's windows as the newest earlier session
/// under `selected` recorded them in `store`, where one did: a plan's windows
/// are the account's, and the Usage pane shows the last level seen, dated,
/// until this session's backend reports its own.
///
/// A store that cannot be read here leaves the pane waiting for the first
/// reply, as an empty one does: the session records into the same store, and
/// a store it cannot use is reported when it tries.
fn carry_windows(
    app: App,
    store: Option<&niobe_store::Store>,
    selected: Option<&niobe_config::Selected<'_>>,
) -> App {
    let found = store
        .zip(selected)
        .and_then(|(store, selected)| store.last_usage_windows(selected.name).ok().flatten());
    match found {
        Some((at, windows)) => app.with_carried_windows(windows, at),
        None => app,
    }
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
    let tap = backend::stream_tap(&root, std::env::var_os(backend::STREAM_DIR_VAR))?;
    let theme = chosen_theme(asked, &loaded)?;

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
    let spent = Spent::of(
        usage_times(stored.iter().map(|s| (&s.event, Some(s.at)))),
        (!piped).then(today),
    );
    let mut app = say_untrusted(
        say_prices(
            spent,
            App::new(repo::describe(&root))
                .with_rules(loaded.config.allowed().clone())
                .with_places(places(&loaded))
                .with_depth(asked.depth)
                .with_keymap(loaded.config.keys().clone())
                .with_alerts(alerts::alerts(loaded.config.notify(), asked.notification))
                .with_catch_up(
                    away_after(loaded.config.catch_up()),
                    loaded.config.catch_up().automatic(),
                )
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
    // At the times the store recorded, not at the time it was read: a session
    // resumed on Thursday did not happen on Thursday.
    let clock = niobe_tui::clock::Clock::system();
    app.extend_at(stored.iter().map(|s| (&s.event, clock.at(s.at))));
    // What the record says the plan's windows stood at is a reading from
    // when it was recorded, drawn dated until the backend reports again.
    app.resumed();
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
    app = carry_windows(app, Some(&store), selected.as_ref());
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
    let attempt = backend::attach(
        &root,
        selected.as_ref(),
        &backend::Attach {
            resume: continuing,
            mode: app.session().mode(),
            budget_usd: asked.budget,
            title: false,
            // The conversation's system prompt was recorded when it began,
            // and the CLI sends that record whatever a later start passes.
            terse: false,
            decisions: false,
            tap: tap.clone(),
            trim: trim_for(&root, loaded.config.trim())?,
        },
        std::env::var_os(niobe_bridge_claude::transcript::CONFIG_DIR_VAR),
        std::env::var_os("HOME"),
    );
    let (mut backend, app) = attached_or_noticed(&loaded, attempt, app)?;
    let app = say_tap(app, tap.as_deref());
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
            memory: &mut memory::Files::at(
                root.clone(),
                selected.as_ref().map(|selected| selected.profile.clone()),
            ),
            overview: &mut overview::Page::at(root.clone()),
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

/// The time each of `events` read from a transcript is stored at: the time
/// its record was written, so that `niobe --resume` shows the session when it
/// happened rather than when it was imported.
///
/// The store dates every event, and an event the transcript gave no time —
/// the CLI's bookkeeping records, what the reader says itself — is stored at
/// the last recorded time before it, or where none comes before it, the first
/// after it: beside the records it was read among, which keeps the stored
/// times in the transcript's order. A transcript that records no time at all
/// is stored at `now`, as it was read.
fn stored_at(events: &[(Event, Option<SystemTime>)], now: SystemTime) -> Vec<SystemTime> {
    let first = events.iter().find_map(|(_, at)| *at).unwrap_or(now);
    let mut last = first;
    events
        .iter()
        .map(|(_, at)| {
            if let Some(at) = at {
                last = *at;
            }
            last
        })
        .collect()
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
    // A conversation some Niobe session already carries is continued there:
    // reading it in again would list one conversation twice, with only one of
    // the two being added to.
    if let Some(carried) = carried_conversation(&root, session) {
        return resume(carried, profile, asked);
    }
    let Some(loaded) = consented(&root, asked)? else {
        return Ok(None);
    };
    let selected = loaded.select(profile)?;
    budget_fits(asked, selected.as_ref())?;
    let tap = backend::stream_tap(&root, std::env::var_os(backend::STREAM_DIR_VAR))?;
    let theme = chosen_theme(asked, &loaded)?;

    let started = Instant::now();
    let events = backend::history(
        selected.as_ref(),
        &root,
        session,
        std::env::var_os(niobe_bridge_claude::transcript::CONFIG_DIR_VAR),
        std::env::var_os("HOME"),
    )?;
    let piped = !std::io::stdout().is_terminal();
    let spent = Spent::of(
        usage_times(events.iter().map(|(event, at)| (event, *at))),
        (!piped).then(today),
    );
    let mut app = say_untrusted(
        say_prices(
            spent,
            App::new(repo::describe(&root))
                .with_rules(loaded.config.allowed().clone())
                .with_places(places(&loaded))
                .with_depth(asked.depth)
                .with_keymap(loaded.config.keys().clone())
                .with_alerts(alerts::alerts(loaded.config.notify(), asked.notification))
                .with_catch_up(
                    away_after(loaded.config.catch_up()),
                    loaded.config.catch_up().automatic(),
                )
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
    // At the times the CLI recorded, as a stored session is resumed at the
    // times the store recorded: an entry the transcript gives no time is
    // shown with none rather than with the moment it was read.
    let clock = niobe_tui::clock::Clock::system();
    for (event, at) in &events {
        match at {
            Some(at) => app.apply_at(event, clock.at(*at)),
            None => app.apply(event),
        }
    }
    // Read back from the CLI's record as a resumed session is from the
    // store's: what it holds was there to read before it was opened, so it
    // is no news on the catch-up sheet, and any windows it holds are dated.
    app.resumed();
    let elapsed = started.elapsed();

    if piped {
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

    let attempt = backend::attach(
        &root,
        selected.as_ref(),
        &backend::Attach {
            // The CLI's own id, which is the only thing that can hand the
            // conversation back.
            resume: Some(session.to_owned()),
            mode: app.session().mode(),
            budget_usd: asked.budget,
            title: false,
            // The conversation's system prompt was recorded when it began,
            // and the CLI sends that record whatever a later start passes.
            terse: false,
            decisions: false,
            tap: tap.clone(),
            trim: trim_for(&root, loaded.config.trim())?,
        },
        std::env::var_os(niobe_bridge_claude::transcript::CONFIG_DIR_VAR),
        std::env::var_os("HOME"),
    );
    let (mut backend, app) = attached_or_noticed(&loaded, attempt, app)?;
    let mut app = say_tap(app, tap.as_deref());

    // Recorded only once there is a terminal to continue it on and a backend
    // started to continue it with: a piped run is a look at the transcript,
    // and a resume that failed is one nobody carried on — recorded, either
    // would put a conversation in the list, and each retry another copy.
    let store = repo::open_or_create_store(&root)?;
    app = carry_windows(app, Some(&store), selected.as_ref());
    let mut recorder = Recorder::new(store);
    for ((event, _), at) in events.iter().zip(stored_at(&events, SystemTime::now())) {
        recorder.record_at(event, at).map_err(|e| e.to_string())?;
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
            memory: &mut memory::Files::at(
                root.clone(),
                selected.as_ref().map(|selected| selected.profile.clone()),
            ),
            overview: &mut overview::Page::at(root.clone()),
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
/// Every session niobe recorded in this repository, folded, as one Markdown
/// page on standard output.
/// `niobe -p`: one turn under the selected profile with no terminal, its
/// reply on standard output — or with `--json` its outcome, reply and spend —
/// and what the operator is owed knowing on standard error, so that a reply
/// piped on stays the reply.
///
/// A repository's config nobody trusted cannot be agreed to here, since
/// nobody is at the terminal: what only a trusted file may set stays out, as
/// it does in the shell until the operator says otherwise, and the run says
/// so.
fn print_turn(
    prompt: &str,
    json: bool,
    profile: Option<&str>,
    allow: &[Rule],
    asked: &Asked,
) -> Result<u8, String> {
    let root = repo::root(&cwd()?);
    let loaded = config::load(&root)?;
    let selected = loaded.select(profile)?;
    budget_fits(asked, selected.as_ref())?;
    repo::store_stays_inside(&root)?;
    if loaded.untrusted.is_some() {
        note(
            "this repository's config is not trusted, so what only a trusted file may set is \
             left out; `niobe trust` shows it",
        );
    }
    let tap = backend::stream_tap(&root, std::env::var_os(backend::STREAM_DIR_VAR))?;
    let terse = asked.terse.unwrap_or(loaded.config.terse());
    let mut backend = backend::attach(
        &root,
        selected.as_ref(),
        &backend::Attach {
            budget_usd: asked.budget,
            // Not titled: a title is one more request to the model, billed on
            // every run of a job, and it answers after the turn this run ends
            // on. The history shows the session by its prompt instead.
            title: false,
            terse,
            // Not asked: a run with no shell has no pane to list them in,
            // and the words would be spent on every run of a job.
            decisions: false,
            tap,
            trim: trim_for(&root, loaded.config.trim())?,
            ..backend::Attach::default()
        },
        std::env::var_os(niobe_bridge_claude::transcript::CONFIG_DIR_VAR),
        std::env::var_os("HOME"),
    )?;
    if !backend.attached() {
        return Err(
            "the selected profile runs no backend niobe can start yet, so the prompt has \
             nowhere to go; `niobe profiles` lists them"
                .to_owned(),
        );
    }

    let mut allowed = loaded.config.allowed().clone();
    for rule in allow {
        allowed.insert(rule.clone());
    }
    let mut journal = StoreJournal::pending(root.clone());
    let turn = headless::run(
        backend.bridge(),
        &mut journal,
        &allowed,
        prompt,
        asked.budget,
        &mut || std::thread::sleep(Duration::from_millis(10)),
    );
    // The backend is ended before the journal is closed, so that nothing it
    // says while going is written into a session the store has finished.
    drop(backend);
    let finished = journal.finish();
    if finished.unsaved > 0 {
        note(&format!(
            "the session store did not save {} of the turn's events: {}",
            finished.unsaved,
            finished
                .error
                .as_deref()
                .unwrap_or("the session store stopped")
        ));
    }

    let session = finished.session.map(SessionId::get);
    match json {
        true => say!("{}", printable::json(&print_json(&turn, session, terse))),
        false if !turn.reply.is_empty() => say!("{}", turn.reply),
        false => {}
    }
    if let Some(session) = session {
        note(&format!(
            "session {session} saved — `niobe --resume {session}` continues it"
        ));
    }
    match &turn.outcome {
        headless::Outcome::Finished => Ok(0),
        headless::Outcome::Budget(why) => {
            note(&format!("the turn stopped on its budget: {why}"));
            Ok(headless::BUDGET_STOPPED)
        }
        headless::Outcome::Failed(why) => Err(format!("the turn failed: {why}")),
    }
}

/// A headless turn in the shape `schema/print.schema.json` publishes.
fn print_json(turn: &headless::Turn, session: Option<i64>, terse: bool) -> serde_json::Value {
    let mut fields = serde_json::Map::from_iter([
        ("schema_version".to_owned(), serde_json::json!(1)),
        ("outcome".to_owned(), serde_json::json!(turn.outcome.name())),
        (
            "reason".to_owned(),
            serde_json::json!(turn.outcome.reason()),
        ),
        ("reply".to_owned(), serde_json::json!(turn.reply)),
        ("session".to_owned(), serde_json::json!(session)),
        ("terse".to_owned(), serde_json::json!(terse)),
        (
            "started_at".to_owned(),
            serde_json::json!(cost::rfc3339(turn.started_at)),
        ),
    ]);
    fields.extend(cost::figures(&turn.state));
    let windows = turn.state.usage_windows().map(|windows| {
        serde_json::json!({
            "five_hour": windows.five_hour.map(|window| window.utilization),
            "seven_day": windows.seven_day.map(|window| window.utilization),
        })
    });
    fields.insert("windows".to_owned(), serde_json::json!(windows));
    serde_json::Value::Object(fields)
}

fn on_off(on: bool) -> &'static str {
    match on {
        true => "on",
        false => "off",
    }
}

/// `niobe bench`: every task of the suite at `suite` run as its own `niobe -p`
/// under the profile and budget this run was given, then the results.
///
/// It exits 0 once the suite has run, whatever passed: a bench measures, and
/// a pass rate is a figure to compare rather than a verdict to stop a job on.
fn run_bench(
    suite: &Path,
    json: bool,
    timeout: Option<Duration>,
    ab: Option<bench::Technique>,
    profile: Option<&str>,
    allow: &[Rule],
    asked: &Asked,
) -> Result<u8, String> {
    let tasks = bench::tasks(suite)?;
    bench_runs_turns(profile)?;
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot find the niobe binary to run each task with: {e}"))?;
    let timeout = timeout.unwrap_or(bench::TURN_TIMEOUT);
    let mut flags = Vec::new();
    if let Some(profile) = profile {
        flags.extend(["--profile".to_owned(), profile.to_owned()]);
    }
    if let Some(budget) = asked.budget {
        flags.extend(["--budget".to_owned(), budget.to_string()]);
    }
    for rule in allow {
        flags.extend(["--allow".to_owned(), rule.to_string()]);
    }
    if let Some(terse) = asked.terse {
        if let Some(technique) = ab {
            return Err(format!(
                "`--ab {}` runs the suite with it off and then on; leave `{}` out",
                technique.name(),
                technique.flag()
            ));
        }
        flags.extend(["--terse".to_owned(), on_off(terse).to_owned()]);
    }
    let name = suite
        .canonicalize()
        .ok()
        .and_then(|dir| {
            dir.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| suite.display().to_string());

    let Some(technique) = ab else {
        let started_at = SystemTime::now();
        let ran = run_suite(&exe, &tasks, &flags, timeout, "");
        match json {
            true => say!(
                "{}",
                printable::json(&bench::document(&name, started_at, &ran, timeout))
            ),
            false => {
                for line in bench::table(&ran, timeout) {
                    say!("{line}");
                }
            }
        }
        return Ok(0);
    };
    let side = |on: bool| {
        let mut flags = flags.clone();
        flags.extend([technique.flag().to_owned(), on_off(on).to_owned()]);
        flags
    };
    let started_without = SystemTime::now();
    let without = run_suite(&exe, &tasks, &side(false), timeout, "without-");
    let started_with = SystemTime::now();
    let with = run_suite(&exe, &tasks, &side(true), timeout, "with-");
    match json {
        true => say!(
            "{}",
            printable::json(&bench::ab_document(
                technique,
                bench::document(&name, started_without, &without, timeout),
                bench::document(&name, started_with, &with, timeout),
            ))
        ),
        false => {
            for line in bench::ab_table(technique, &without, &with, timeout) {
                say!("{line}");
            }
        }
    }
    Ok(0)
}

/// Refuses a bench whose turns could not run: one under a profile nobody
/// defined, or one whose backend niobe cannot start.
///
/// Each task's `niobe -p` would refuse it the same way, and the suite would
/// then report every task failed — a pass rate of nothing, read as the
/// agent's. The profile is selected from the user's config alone, which is
/// the config each turn runs under in its copy outside any repository.
fn bench_runs_turns(profile: Option<&str>) -> Result<(), String> {
    let config = config::load_user()?;
    let selected = config.select(profile).map_err(|e| e.to_string())?;
    match selected {
        Some(selected) if backend::starts(selected.profile.backend()) => Ok(()),
        Some(selected) => Err(format!(
            "the profile `{}` runs the {} backend, which niobe cannot start yet, so no task's \
             turn would run; `niobe profiles` lists the profiles",
            selected.name,
            selected.profile.backend()
        )),
        None => Err(
            "no profile is selected, so no task's turn would run; name one with `--profile`"
                .to_owned(),
        ),
    }
}

/// Every task of a suite, run in turn with `flags`, each saying on standard
/// error which it is as it starts.
fn run_suite(
    exe: &Path,
    tasks: &[bench::Task],
    flags: &[String],
    timeout: Duration,
    side: &str,
) -> Vec<bench::Ran> {
    let mut ran = Vec::with_capacity(tasks.len());
    for (number, task) in tasks.iter().enumerate() {
        note(&format!(
            "task {} of {}: {side}{}",
            number + 1,
            tasks.len(),
            task.name
        ));
        ran.push(bench::run(exe, task, flags, timeout, side));
    }
    ran
}

/// Says something the operator is owed knowing on standard error, where a
/// headless run's notes go so as not to land in the reply it prints. A note
/// that cannot be written is let go: the outcome is in the exit status.
fn note(text: &str) {
    let _ = writeln!(io::stderr(), "niobe: {}", printable::printable(text));
}

fn print_overview(narrowing: args::Narrowing) -> Result<(), String> {
    let root = repo::root(&cwd()?);
    let folded = overview::narrowed(overview::read(&root)?, narrowing);
    let uncommitted = repo::uncommitted(&root);
    for line in overview::markdown(
        &repo::describe(&root).name,
        &folded,
        narrowing,
        uncommitted.as_deref(),
    ) {
        say!("{line}");
    }
    Ok(())
}

fn print_cost(narrowing: args::Narrowing, format: cost::Format) -> Result<(), String> {
    let root = repo::root(&cwd()?);
    let folded = overview::narrowed(overview::read(&root)?, narrowing);
    for line in cost::lines(&repo::describe(&root).name, &folded, format) {
        say!("{line}");
    }
    Ok(())
}

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

/// The backend `attempt` opened, or, where the profile is the implied one and
/// the backend would not start, no backend and a line in the shell saying why.
///
/// An operator who wrote a profile asked for its backend, so a backend that
/// will not start is a failure they are told of before the shell opens. One
/// who wrote nothing asked for nothing: opening on what is missing, and how
/// to get it, is what a first run is owed instead of an error about a file.
fn attached_or_noticed(
    loaded: &config::Loaded,
    attempt: Result<backend::Attachment, String>,
    app: App,
) -> Result<(backend::Attachment, App), String> {
    match attempt {
        Ok(attachment) => Ok((attachment, app)),
        Err(said) if loaded.implied => {
            let cause = said.strip_prefix(backend::CANNOT_START).unwrap_or(&said);
            let body = format!(
                "With no config file niobe runs the `claude` CLI found on PATH, and it \
                 would not start: {cause} `niobe doctor` checks the rest of the setup."
            );
            Ok((
                backend::detached(),
                app.with_notice("no backend to run", "", &body),
            ))
        }
        Err(said) => Err(said),
    }
}

/// The shell, saying where the backend's stream is being copied to, once, where
/// it is: the files hold the whole conversation.
fn say_tap(app: App, tap: Option<&Path>) -> App {
    match tap {
        None => app,
        Some(directory) => app.with_notice(
            "stream kept",
            &directory.display().to_string(),
            "A copy of everything that crosses the claude CLI's pipes is being written here, \
             one file per process. It holds the whole conversation; read it before it is shared.",
        ),
    }
}

/// The shell with a price sheet, so that a turn the backend has not priced yet
/// shows what it is costing rather than nothing.
///
/// How long with no key pressed the config calls away.
fn away_after(catch_up: niobe_config::CatchUp) -> Duration {
    Duration::from_secs(catch_up.after_minutes().saturating_mul(60))
}

/// A price file that will not read does not stop the session: the estimate is
/// a convenience, the session is the point. It is said in the transcript
/// rather than swallowed, because an operator who wrote a price file is owed
/// the reason it is not in force.
///
/// `spent` is the days the session's tokens were spent on: today for a live
/// one, and for one read back the days its records were made, so that a rate
/// changed since is not laid over tokens bought before it.
fn say_prices(spent: Spent, app: App) -> App {
    match prices::load() {
        Ok(loaded) => app.with_prices(Box::new(prices::Sheet::new(loaded.table, spent, today()))),
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

/// The UTC day it is now, which the price table is read on.
fn today() -> Date {
    Date::of(SystemTime::now())
}

/// The times of a session's usage records, which the days its tokens were
/// spent on are read from.
fn usage_times<'a>(
    events: impl Iterator<Item = (&'a Event, Option<SystemTime>)>,
) -> impl Iterator<Item = Option<SystemTime>> {
    events.filter_map(|(event, at)| matches!(event, Event::Usage(_)).then_some(at))
}

/// The files the shell may name or hand to the desktop: the config files the
/// session was looked for in.
///
/// Only a regular file that is not a link is offered to be opened: what a
/// repository's file is, is the repository's choice, and the desktop runs
/// some files rather than showing them.
fn places(loaded: &config::Loaded) -> Places {
    let openable =
        |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file());
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
                consent::record(untrusted)?;
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
    consent::record(&untrusted)?;

    say!("trusted {}", path.display());
    say!(
        "the profiles it defines are in force here as it defines them, env, args, settings, \
         auth_refresh and billing included, and so are its default_profile, its \
         permissions and its titles; `niobe profiles` lists them, and editing the file asks \
         again"
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
    let mut out = io::stdout().lock();
    write!(out, "trust it? [y/N] ")
        .and_then(|()| out.flush())
        .map_err(|e| format!("the question could not be asked: {e}"))?;
    drop(out);
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
         profiles it defines are not in force, nor its default_profile, nor a titles = \
         \"model\", nor a profile it names like one of yours",
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
    if loaded.implied {
        for line in profiles::implied_note(&loaded.searched) {
            say!("{line}");
        }
    }
    Ok(())
}

/// Prints what a first session needs and what is missing, as text or as the
/// JSON an issue carries, and fails where a session could not run.
fn run_doctor(json: bool) -> Result<(), String> {
    let loaded = config::load(&repo::root(&cwd()?))?;
    let root = repo::root(&cwd()?);
    let facts = doctor::gather(&loaded, &root, backend::claude_release_recorded);
    let rows = doctor::rows(&facts);
    if json {
        say!("{}", printable::json(&doctor::json(&rows, &facts)));
    } else {
        for line in doctor::text(&rows) {
            say!("{line}");
        }
    }
    match doctor::failed(&rows) {
        true => Err("a session cannot run until the rows marked fail are fixed".to_owned()),
        false => Ok(()),
    }
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
fn replay(log: &Path, speed: Option<playback::Speed>, asked: &Asked) -> Result<(), String> {
    let text = log_text(log)?;

    let started = Instant::now();
    let events = read_dated_log(&text).map_err(|e| format!("{}: {e}", log.display()))?;
    // The same price sheet the shell and `--resume` get, read for the days the
    // log's lines say: a replayed log and a resumed session that fold to the
    // same totals on the same days must print the same cost, or one of the
    // two figures is teaching the operator to distrust both.
    let spent = Spent::of(
        usage_times(events.iter().map(|(event, at)| (event, *at))),
        None,
    );
    let app = say_prices(
        spent,
        App::new(repo::describe(&cwd()?)).with_depth(asked.depth),
    );
    let mut app = match asked.theme {
        Some(theme) => app.with_theme(theme),
        None => app,
    };
    let clock = niobe_tui::clock::Clock::system();
    let terminal = std::io::stdout().is_terminal();
    // At a speed the shell is handed the events as it would a live session's,
    // so it opens on none of them; without one, or with no terminal to watch
    // on, they are folded here at once.
    let at_speed = speed.filter(|_| terminal);
    if at_speed.is_none() {
        for (event, at) in &events {
            match at {
                Some(at) => app.apply_at(event, clock.at(*at)),
                None => app.apply(event),
            }
        }
    }
    let elapsed = started.elapsed();

    if !terminal {
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
    let mut detached = Detached;
    let mut playback;
    let backend: &mut dyn niobe_tui::Bridge = match at_speed {
        Some(speed) => {
            playback = playback::Playback::new(events, speed);
            &mut playback
        }
        None => &mut detached,
    };
    niobe_tui::run(
        app,
        niobe_tui::Around {
            journal: &mut Unrecorded,
            backend,
            rules: &mut Forgotten,
            watch: &mut Unwatched,
            shell: &mut NoShell,
            images: &mut NoImages::default(),
            desktop: &mut desktop::System::new(),
            history: &mut NoHistory::default(),
            memory: &mut niobe_tui::NoMemory::default(),
            overview: &mut niobe_tui::NoOverview::default(),
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
    niobe -p <prompt>      Run one turn with no terminal and print the reply; --json prints
                           its outcome, reply and spend in the shape schema/print.schema.json
                           publishes. [permissions] rules answer the calls they allow and
                           every other call is refused. The session is saved as any other
    niobe bench <suite>    Run each task of a suite as one -p turn in a copy of its files and
                           judge it by its check.sh; --json in the shape
                           schema/bench.schema.json publishes, --timeout <seconds> per turn
                           (900 by default). Each turn may edit and write inside its copy
                           and, beyond --allow, nowhere else. --ab terse runs the suite
                           without terse, then with it, and sets the two side by side.
                           bench/ in niobe's repository is a suite
    niobe sessions         List the sessions this repository can carry on
    niobe overview         Print what every session here built and how it went, as Markdown;
                           --last <n> keeps the newest n, --since <YYYY-MM-DD> those from that
                           UTC day on
    niobe cost             List each session here with its tokens and the money its backend
                           reported; --json in the shape schema/cost.schema.json publishes,
                           --csv as rows, --by-day as each UTC day's tokens summed by profile,
                           --by-turn as each turn's tokens; --last and --since as for overview
    niobe profiles         List the profiles the config defines, the selected one marked
    niobe doctor [--json]  Check what a session needs — the CLIs, their sign-in, the config,
                           the terminal — and say how to fix what is missing
    niobe trust [--yes]    Show what this repository's config sets and let it start a
                           backend with that, once you agree
    niobe untrust          Take that back
    niobe prices [model]   List the prices in force today, or every price a model has had
    niobe replay <file>    Fold a JSON Lines event log into the shell (development);
                           --speed <n> plays it into the panes at n times the pace it was
                           recorded at, no gap longer than two seconds

OPTIONS:
    --profile <name>       Run under this profile instead of the default one
    --budget <amount>      Stop the session once it has cost this many dollars
                           (refused on a profile billed as a plan)
    --theme <name>         Draw the shell in this palette: cyber, classic, neo
                           or modern
    --terse <on|off>       Ask a new conversation for terse replies, or not, beating the
                           config's terse
    --allow <rule>         For -p and bench: answer the calls this rule covers, as a
                           [permissions] rule would, for this run alone; may be repeated
    -h, --help             Print this help
    -V, --version          Print the version

ENVIRONMENT:
    NIOBE_STREAM_DIR       A directory, outside the repository, to keep a copy of what
                           crosses the claude CLI's pipes in, one file per process, for
                           recording a fixture. It holds the whole conversation; the
                           shell says so once when it is on. Off by default.

EXIT STATUS:
    0   The session ended, including because the terminal it drew on went away;
        for -p, the turn finished
    1   Something failed, a -p turn included; the reason was written to standard
        error, which discards it when standard error is closed or /dev/null
    2   Something failed and the reason could not be written to standard error
    3   A -p turn stopped because it had spent what --budget allowed

KEYS:
    Each of the F-key bar's ten actions has its own function key, and a
    digit after Esc. A terminal that eats one can give the action another key
    in the config, in a [keys] table:

        [keys]
        mcp = \"ctrl+l\"
        memory = \"ctrl+s\"

    The actions are help, compact, clear, model, diff, rewind, mcp, memory,
    settings and quit. A key is any of the twelve function keys, written as
    the letter f and its number, or ctrl+l or ctrl+s, the Ctrl letters
    nothing else answers to. A key that two actions would share, or that is
    another action's own function key, is refused at its line. The bar, the
    menus and the help sheet draw the new key. Not gated on trust: a key is
    nothing a backend is started with. A repository's [keys] replace the
    user's whole.

THEMES:
    A theme of your own is a file in ~/.config/niobe/themes ($XDG_CONFIG_HOME
    when that is set), named for the theme, and is offered by --theme, the
    config key and the theme list beside the built-in ones:

        base = \"cyber\"
        [colors]
        hot = \"#ff8800\"
        fg = \"light-cyan\"
        [borders]
        border = \"rounded\"

    Every colour left out is the base's. A colour in hex is drawn only on a
    terminal that draws 24-bit colour; where it has the sixteen, the base's
    sixteen-colour table is used with the colours spelled as names laid over
    it. A colour or key that is not one, a pairing drawn on top of its own
    colour and text under 3 to 1 against its background at 24 bits are each
    refused at the file and line, and the shell does not open on the file.
    Only your own config directory is looked in: a repository cannot change
    how the shell is drawn.

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
    it. A ~ at the front is your home directory, and a relative path is read
    from beside the config that names it: your config's directory, or a
    repository's root for its own .niobe/config.toml. The file has to be
    there, and a path that is not is reported at its line in the config
    before anything is started. args are passed as written, and the CLI reads
    a relative path among them from the repository's root, so one in your own
    profile that names a file the repository holds is refused: write it as an
    absolute path. niobe profiles shows which profiles name one — and, where this
    machine's own claude settings set CLAUDE_CODE_USE_BEDROCK, which do not.

    auth_refresh names the command that renews a backend's credentials, such
    as a single sign-on login. It is read, trust-gated and listed, and not run
    yet: niobe cannot yet tell a backend whose credentials have expired from
    one that failed another way, which is when it would run the command, and
    running a login before every session would ask you to sign in each time.
    Run it yourself when a session will not sign in.

    titles = \"first-prompt\", at the top of a config, leaves a new session
    captioned by the first line of its first prompt. Left out, or \"model\",
    the backend is asked once, at that prompt, for a few words on what it
    asks, which the claude CLI answers from its small model and bills into
    the session: some nine hundred tokens. A repository's config nobody has
    trusted can turn it off and not on.

    terse = true, at the top of a config, asks each new conversation for
    terse replies, with code, commands, paths, errors and security warnings
    kept whole; --terse on or off beats it for one run. Off by default. On
    niobe's bench of twenty tasks it cut output tokens by 14% and the
    replies' length by a quarter, with every task still passing. A
    conversation carried on keeps the prompt it began with.

    decisions = false, at the top of a config, stops asking each new
    conversation to state the choices it makes — work left out of scope, one
    approach taken over another — each on a line that opens DECISION:. On by
    default. The Activity pane lists them under Decisions, as the agent says:
    the agent's own account of its reasoning, which it may have wrong. A
    run with -p never asks.

    trim = true, at the top of a config, has the agent read a cargo command's
    output without cargo's build progress and the tests that passed; every
    failure, error and test result is kept, and the whole output is written
    to .niobe/trim/, until deleted, and named under what the agent reads. Off
    by default. On the recorded cargo runs in niobe's tests it left 7% of the
    bytes, before the line naming where the rest is. Only a
    command that succeeds is trimmed: the claude CLI hands a failing one to
    the agent as it is, and lets no hook change it.

    [notify] bell = false and notification = false turn off how niobe calls
    you back when a question waits on you or a turn you sent ends: the
    terminal's bell, and a desktop notification on a terminal that shows one
    (iTerm2, WezTerm, Ghostty and kitty; foot and rxvt-unicode). Inside tmux
    only the bell is sent.

    The first key you press after five minutes away opens what happened
    meanwhile: the turns that ended, the files changed, the sub-agents started
    and finished, what was asked and how far the five-hour window moved.
    [catch_up] after = 10 sets the minutes, and automatic = false leaves it to
    c in a side pane and View › What happened….

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
    force on the days they were spent: a session resumed later is estimated
    at the rates of the days it ran, and one whose rates changed while it ran,
    or a replayed log whose lines carry no \"at\", is shown as a floor. A cost
    the backend reported is shown
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
    PgUp / PgDn            Scroll the pane that has the keyboard
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
                           skills the backend listed; at the start of a later
                           word, a skill the backend takes anywhere in a
                           prompt: Up / Down choose, Tab or Enter put the
                           name in the prompt, Esc leaves it as typed
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
    Esc, then a digit      Press that F-key, 0 for the tenth; Esc and a menu's
                           letter opens that menu
    Alt+letter             Open the menu that starts with that letter
    Ctrl+W                 Delete the word before the cursor
    Ctrl+U, Ctrl+K         Delete to the start, or to the end, of the line
    Tab                    Move the keyboard between the transcript and the
                           side panes
    Up / Down, Enter       In a side pane, move the cursor and fold the
                           section under it
    a                      In a side pane, group sub-agent rows by agent
    n                      In a side pane, leave the routine entries (reads,
                           searches, notices) out of the transcript, each run
                           of them counted where it was
    c                      In a side pane, what happened since you last
                           caught up
    d                      In a side pane, list the tool calls; Enter on one
                           shows its whole input and output
    s                      In a side pane, show one sub-agent's entries alone;
                           again shows everything
    o                      In a side pane, every session recorded here: what
                           was built, what went wrong, what was left open
    t                      In the Activity pane, count tools by name or by
                           kind of work
    Ctrl+End               Back to the newest line of the transcript
    Ctrl+O                 Fold each run of calls to one tool into one row
    Ctrl+T                 Open every diff cut at twenty rows, show the whole
                           command under a shell call's row, and a call's
                           arguments as they were sent
    Ctrl+V                 Attach the image on the clipboard to the prompt;
                           a pasted or dropped path to an image attaches it
    drag                   Copy what it covers
    click                  Open the link under it, or the tool call whose
                           row it is, read whole
    Ctrl+Z                 Suspend niobe, handing the terminal back
{fkeys}
    Ctrl+Q                 Quit

    Typed as a prompt, /model, /memory, /resume, /help, /config, /exit and
    /quit do what the shell has for them — the model list, the Memory view,
    the history's sessions, this list of keys, Settings and quitting — rather
    than go to the backend.

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
    the first (;, &&, |, a redirection, a substitution or a bracket) or a path
    that climbs above the prefix with ..; a * inside a word, as in
    Bash(cat src/*), finishes that word and covers no argument after it. Such
    a call is asked about.
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
    fn an_imported_event_with_no_time_is_stored_beside_the_records_it_was_read_among() {
        let second = |s: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(s);
        let event = || Event::TurnEnded;
        let events = vec![
            (event(), None),
            (event(), Some(second(10))),
            (event(), None),
            (event(), Some(second(12))),
            (event(), None),
        ];
        assert_eq!(
            stored_at(&events, second(99)),
            vec![second(10), second(10), second(10), second(12), second(12)]
        );
    }

    #[test]
    fn a_transcript_that_records_no_time_is_stored_as_it_was_read() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(99);
        let events = vec![(Event::TurnEnded, None), (Event::TurnEnded, None)];
        assert_eq!(stored_at(&events, now), vec![now, now]);
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
                attached: Vec::new(),
            }),
            implied: false,
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
            implied: false,
        };

        let app = say_untrusted(App::new(niobe_tui::app::Repo::default()), &loaded);

        assert!(app.entries().is_empty());
    }

    /// A store holding windows a session under `max` recorded, at `at`.
    fn store_with_windows_under_max(at: SystemTime) -> niobe_store::Store {
        use niobe_core::event::{Backend, Event, SessionMeta, UsageWindow, UsageWindows};
        let store = niobe_store::Store::open_in_memory().expect("an in-memory store opens");
        let session = store.create_session().expect("a session is created");
        let meta = Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "max".to_owned(),
            model: "opus-5".to_owned(),
            backend_session: None,
        });
        let windows = Event::UsageWindows(UsageWindows {
            five_hour: Some(UsageWindow {
                utilization: 0.68,
                resets_at: None,
            }),
            seven_day: None,
            using_overage: false,
            overage: niobe_core::event::Overage::Unreported,
        });
        for event in [meta, windows] {
            store
                .append_at(session, &event, at)
                .expect("an append succeeds");
        }
        store
    }

    fn carried_under(profile: &str) -> niobe_tui::app::PlanWindows {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_789_000_000);
        let store = store_with_windows_under_max(at);
        let config = niobe_config::Config::parse(
            "[profiles.max]\nbackend = \"claude\"\n[profiles.work]\nbackend = \"claude\"\n",
            Path::new("config.toml"),
        )
        .expect("the config parses");
        let selected = config
            .select(Some(profile))
            .expect("the profile is defined")
            .expect("a profile was selected");
        let app = App::new(niobe_tui::app::Repo::default()).attached();

        carry_windows(app, Some(&store), Some(&selected)).plan_windows()
    }

    #[test]
    fn a_new_session_carries_the_windows_an_earlier_one_under_its_profile_recorded() {
        match carried_under("max") {
            niobe_tui::app::PlanWindows::Carried { windows, .. } => {
                assert_eq!(
                    windows.five_hour.map(|window| window.utilization),
                    Some(0.68)
                );
            }
            other => panic!("the recorded windows were not carried: {other:?}"),
        }
    }

    #[test]
    fn a_new_session_carries_no_windows_another_profile_recorded() {
        assert_eq!(carried_under("work"), niobe_tui::app::PlanWindows::Awaited);
    }

    #[test]
    fn durations_print_in_milliseconds_to_two_decimals() {
        assert_eq!(millis(Duration::from_micros(412)), "0.41 ms");
        assert_eq!(millis(Duration::from_millis(50)), "50.00 ms");
    }
}
