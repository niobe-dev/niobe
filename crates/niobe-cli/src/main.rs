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
mod journal;
mod prices;
mod profiles;
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
use niobe_tui::app::App;
use niobe_tui::journal::Unrecorded;
use niobe_tui::theme::{self, Depth};
use niobe_tui::{Detached, Ended, Forgotten, NoShell, Theme, Unwatched};

use crate::args::{Command, Invocation, Resume};
use crate::journal::StoreJournal;
use crate::rules::ConfigRules;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args::parse(&args) {
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
    match writeln!(stderr, "niobe: {message}") {
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
        depth: Depth::from_colorterm(std::env::var("COLORTERM").ok().as_deref()),
    };
    match command {
        Command::Shell => shell(profile, &asked),
        Command::Resume(Resume::Recorded(session)) => resume(session, profile, &asked),
        Command::Resume(Resume::Imported(session)) => import(&session, profile, &asked),
        Command::Sessions => list_sessions(),
        Command::Profiles => list_profiles(profile),
        Command::Trust => trust(),
        Command::Untrust => untrust(),
        Command::Prices(model) => list_prices(model.as_deref()),
        Command::Replay(log) => replay(&log),
        Command::Help => {
            print_help();
            Ok(())
        }
        Command::Version => {
            println!("{} {}", niobe_core::APP_NAME, niobe_core::VERSION);
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
/// and got the usual one would read it as the file not being loaded. `F9`
/// changes it from here for the rest of the session.
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

/// Opens the shell on a new session, recorded into this repository's store.
///
/// Without a terminal there is nothing to draw into and raw mode would fail, so
/// a piped or redirected run prints the help instead of an errno. The config is
/// read first either way, so a config that cannot be used is reported rather
/// than hidden behind the help.
fn shell(profile: Option<&str>, asked: &Asked) -> Result<(), String> {
    let cwd = cwd()?;
    let root = repo::root(&cwd);
    let loaded = config::load(&root)?;
    let selected = loaded.select(profile)?;
    let app = say_untrusted(
        say_prices(
            App::new(repo::describe(&cwd))
                .with_rules(loaded.config.allowed().clone())
                .with_depth(asked.depth)
                .with_theme(chosen_theme(asked, &loaded)?)
                .with_effects(loaded.config.effects()),
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
        return Ok(());
    }

    // Spawned after the terminal check and before the shell takes the screen:
    // a piped run starts no subprocess, and a backend that will not start says
    // why on a screen that is still the operator's.
    let mut backend = backend::attach(
        &root,
        selected.as_ref(),
        &backend::Attach {
            budget_usd: asked.budget,
            ..backend::Attach::default()
        },
        std::env::var_os("HOME"),
    )?;
    let app = if backend.attached() {
        app.attached()
    } else {
        app
    };
    // A command typed after `!` runs whether or not a backend does: it is the
    // operator's, not the agent's.
    let app = app.runs_commands();

    let mut journal = StoreJournal::Pending(root.clone());
    let mut rules = ConfigRules::at(&root);
    // Started with the shell and stopped with it: the thread behind it reads
    // the repository while the session runs, and a piped run never gets here.
    let mut watching = repo::watch(&cwd);
    let mut commands = commands::Commands::at(&cwd);
    let ended = niobe_tui::run(
        app,
        &mut journal,
        backend.bridge(),
        &mut rules,
        &mut watching,
        &mut commands,
    )
    .map_err(|e| e.to_string())?;

    match ended {
        // There is nothing left to print on: the terminal the shell drew on is
        // the one this line would go to, and writing to it now fails. The
        // session is recorded either way, and `niobe sessions` lists it.
        Ended::TerminalGone => return Ok(()),
        Ended::Quit => {}
    }
    if let Some(session) = journal.session() {
        println!(
            "session {session} saved in {} — `niobe --resume {session}` continues it",
            repo::store_path(&root).display()
        );
    }
    Ok(())
}

/// Opens the shell on a recorded session and keeps recording into it. Without
/// a terminal, prints what the session folds to.
fn resume(session: SessionId, profile: Option<&str>, asked: &Asked) -> Result<(), String> {
    let cwd = cwd()?;
    let root = repo::root(&cwd);
    let loaded = config::load(&root)?;
    let selected = loaded.select(profile)?;
    let mut app = say_untrusted(
        say_prices(
            App::new(repo::describe(&cwd))
                .with_rules(loaded.config.allowed().clone())
                .with_depth(asked.depth)
                .with_theme(chosen_theme(asked, &loaded)?)
                .with_effects(loaded.config.effects()),
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
    let store = repo::open_existing_store(&root)?.ok_or_else(|| {
        format!(
            "no session {session}: nothing has been recorded in {}",
            root.display()
        )
    })?;
    let recorder = Recorder::resume(store, session).map_err(|e| e.to_string())?;
    let stored = recorder
        .store()
        .events(session)
        .map_err(|e| e.to_string())?;
    // At the times the store recorded, not at the time it was read: a session
    // resumed on Thursday did not happen on Thursday.
    let clock = niobe_tui::clock::Clock::system();
    app.extend_at(stored.iter().map(|s| (&s.event, clock.at(s.at))));
    let elapsed = started.elapsed();

    if !std::io::stdout().is_terminal() {
        print_summary(
            &format!(
                "session {session} · {} events · loaded and folded in {}",
                stored.len(),
                millis(elapsed)
            ),
            &app,
        );
        return Ok(());
    }

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
        },
        std::env::var_os("HOME"),
    )?;
    let app = if backend.attached() {
        app.attached()
    } else {
        app
    };
    // A command typed after `!` runs whether or not a backend does: it is the
    // operator's, not the agent's.
    let app = app.runs_commands();

    let mut journal = StoreJournal::Open(recorder);
    let mut rules = ConfigRules::at(&root);
    let mut watching = repo::watch(&cwd);
    let mut commands = commands::Commands::at(&cwd);
    niobe_tui::run(
        app,
        &mut journal,
        backend.bridge(),
        &mut rules,
        &mut watching,
        &mut commands,
    )
    .map_err(|e| e.to_string())
    .map(|_| ())
}

/// Opens the shell on a session the `claude` CLI recorded, reading its history
/// into a session of Niobe's own and asking the CLI to carry the conversation
/// on. Without a terminal, prints what the transcript folds to.
///
/// Nothing is written back to the CLI's own store. What is read becomes a
/// Niobe session like any other, so from here on `niobe --resume <number>`
/// continues it and every total on screen is a fold over the same events.
fn import(session: &str, profile: Option<&str>, asked: &Asked) -> Result<(), String> {
    let cwd = cwd()?;
    let root = repo::root(&cwd);
    let loaded = config::load(&root)?;
    let selected = loaded.select(profile)?;
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
            App::new(repo::describe(&cwd))
                .with_rules(loaded.config.allowed().clone())
                .with_depth(asked.depth)
                .with_theme(theme)
                .with_effects(loaded.config.effects()),
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
        return Ok(());
    }

    // Recorded only once there is a terminal to continue it on: a piped run is
    // a look at the transcript, and leaving a session behind for one would put
    // a conversation in the list that nobody carried on.
    let mut recorder = Recorder::new(repo::open_or_create_store(&root)?);
    for event in &events {
        recorder.record(event).map_err(|e| e.to_string())?;
    }

    let mut backend = backend::attach(
        &root,
        selected.as_ref(),
        &backend::Attach {
            // The CLI's own id, which is the only thing that can hand the
            // conversation back.
            resume: Some(session.to_owned()),
            mode: app.session().mode(),
            budget_usd: asked.budget,
        },
        std::env::var_os("HOME"),
    )?;
    let app = if backend.attached() {
        app.attached()
    } else {
        app
    };
    // A command typed after `!` runs whether or not a backend does: it is the
    // operator's, not the agent's.
    let app = app.runs_commands();

    let mut journal = StoreJournal::Open(recorder);
    let mut rules = ConfigRules::at(&root);
    let mut watching = repo::watch(&cwd);
    let mut commands = commands::Commands::at(&cwd);
    let ended = niobe_tui::run(
        app,
        &mut journal,
        backend.bridge(),
        &mut rules,
        &mut watching,
        &mut commands,
    )
    .map_err(|e| e.to_string())?;

    match ended {
        Ended::TerminalGone => return Ok(()),
        Ended::Quit => {}
    }
    if let Some(recorded) = journal.session() {
        println!(
            "claude session {session} is niobe session {recorded} in {} — \
             `niobe --resume {recorded}` continues it",
            repo::store_path(&root).display()
        );
    }
    Ok(())
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

    let recorded = match repo::open_existing_store(&root)? {
        Some(store) => store.sessions().map_err(|e| e.to_string())?,
        None => Vec::new(),
    };
    let imported = backend::recorded(
        selected.as_ref(),
        &root,
        std::env::var_os(niobe_bridge_claude::transcript::CONFIG_DIR_VAR),
        std::env::var_os("HOME"),
    )?;

    if recorded.is_empty() && imported.is_empty() {
        println!("no sessions recorded in {}", root.display());
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
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

/// What the shell opens on when this repository's config has not been trusted.
///
/// Said in the transcript rather than printed: the shell draws on the
/// alternate screen, and a line printed before it takes the terminal is gone
/// before anyone reads it. Said at all because a session running without the
/// environment its profile names is a session whose behaviour has no other
/// explanation on screen.
const UNTRUSTED: &str = "\
This repository's config sets what a backend is started with — a profile's \
`env`, `args`, `settings` or `auth_refresh`. A config arrives with a clone, \
and those are how the official CLI would be pointed at somewhere other than \
where it is signed in, or signed in as something else, so they are not in \
force until you have read the file. `niobe profiles` shows what it sets; \
`niobe trust` puts it in force as it now stands, and editing it afterwards \
asks again.";

/// The shell, saying so when this repository's config has not been trusted.
/// The shell with a price sheet, so that a turn the backend has not priced yet
/// shows what it is costing rather than nothing.
///
/// A price file that will not read does not stop the session: the estimate is
/// a convenience, the session is the point. It is said in the transcript
/// rather than swallowed, because an operator who wrote a price file is owed
/// the reason it is not in force.
fn say_prices(app: App) -> App {
    match prices::load() {
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

fn say_untrusted(app: App, loaded: &config::Loaded) -> App {
    match &loaded.untrusted {
        None => app,
        Some(path) => app.with_notice("config not trusted", &path.display().to_string(), UNTRUSTED),
    }
}

/// Records this repository's config as one the operator has read, so that the
/// `env`, `args`, `settings` and `auth_refresh` of the profiles it defines
/// take effect.
fn trust() -> Result<(), String> {
    let (path, text) = repo_config()?;
    let record = trust_record()?;
    let mut trusted = niobe_config::trust::Trusted::read(&record).map_err(|e| e.to_string())?;
    trusted.trust(&path, &text).map_err(|e| e.to_string())?;
    trusted.write(&record).map_err(|e| e.to_string())?;

    println!("trusted {}", path.display());
    println!(
        "the env, args, settings and auth_refresh of the profiles it defines are in force \
         here; `niobe profiles` lists them, and editing the file asks again"
    );
    Ok(())
}

/// Takes that back. The file stays where it is; what it may start a backend
/// with is what goes.
fn untrust() -> Result<(), String> {
    let path = repo::config_path(&repo::root(&cwd()?));
    let record = trust_record()?;
    let mut trusted = niobe_config::trust::Trusted::read(&record).map_err(|e| e.to_string())?;

    if !trusted.forget(&path) {
        println!("{} was not trusted", path.display());
        return Ok(());
    }
    trusted.write(&record).map_err(|e| e.to_string())?;
    println!(
        "{} is no longer trusted; the env, args, settings and auth_refresh of the profiles \
         it defines are not in force",
        path.display()
    );
    Ok(())
}

/// This repository's config and what is in it, for a command that acts on the
/// file rather than on what it parses to.
fn repo_config() -> Result<(PathBuf, String), String> {
    let path = repo::config_path(&repo::root(&cwd()?));
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok((path, text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "no config to trust: {} does not exist",
            path.display()
        )),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

/// Where the record of what this machine has trusted is kept.
fn trust_record() -> Result<PathBuf, String> {
    config::trust_path().ok_or_else(|| {
        "nowhere to keep the record: neither XDG_CONFIG_HOME nor HOME names a directory of \
         your own"
            .to_owned()
    })
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
        println!("{}", profiles::none_defined(&loaded.searched));
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
        println!("{line}");
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
        println!("{line}");
    }
    Ok(())
}

/// Folds a JSON Lines event log into the shell, for development. Nothing typed
/// into a replayed log is recorded. Without a terminal, prints the fold.
fn replay(log: &Path) -> Result<(), String> {
    let text =
        std::fs::read_to_string(log).map_err(|e| format!("cannot read {}: {e}", log.display()))?;

    let started = Instant::now();
    let events = read_log(&text).map_err(|e| format!("{}: {e}", log.display()))?;
    // The same price sheet the shell and `--resume` get: a replayed log and a
    // resumed session that fold to the same totals must print the same cost,
    // or one of the two figures is teaching the operator to distrust both.
    let mut app = say_prices(App::new(repo::describe(&cwd()?)));
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

    // A recorded log is being looked at, not continued: nothing is attached
    // and nothing is kept.
    niobe_tui::run(
        app,
        &mut Unrecorded,
        &mut Detached,
        &mut Forgotten,
        &mut Unwatched,
        &mut NoShell,
    )
    .map_err(|e| e.to_string())
    .map(|_| ())
}

fn print_summary(header: &str, app: &App) {
    println!("{header}");
    for line in summary::lines(app) {
        println!("{line}");
    }
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
    println!(
        "\
{name} {version}
A terminal coding agent that keeps you aware of what is being built and how:
what the agent did, decided and touched, what it is doing now and what it cost.

USAGE:
    niobe                  Open the shell on a new session
    niobe --resume <id>    Open the shell on an earlier session and continue it
    niobe sessions         List the sessions this repository can carry on
    niobe profiles         List the profiles the config defines, the selected one marked
    niobe trust            Let this repository's config start a backend with what it names
    niobe untrust          Take that back
    niobe prices [model]   List the prices in force today, or every price a model has had
    niobe replay <file>    Fold a JSON Lines event log into the shell (development)

OPTIONS:
    --profile <name>       Run under this profile instead of the default one
    --budget <amount>      Stop the session once it has cost this many dollars
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
        models = [\"opus\", \"sonnet\", \"haiku\"]
        env = {{ CLAUDE_CODE_USE_BEDROCK = \"1\", AWS_PROFILE = \"work-sso\" }}
        auth_refresh = \"aws sso login --profile work-sso\"

        [profiles.personal-account]
        backend = \"claude\"
        settings = \"~/.config/niobe/claude-personal.json\"

    The user's config is ~/.config/niobe/config.toml ($XDG_CONFIG_HOME/niobe
    when that is set); a repository's is .niobe/config.toml at its root, and
    overrides the user's, replacing any profile of the same name whole. The
    env of a profile is passed on exactly as written. The models of a profile
    are the ones F8 offers in the shell, named the way its backend takes them;
    a profile that names none has nothing to switch between, because niobe
    never invents a model id.

    billing says how the account behind a profile is billed, \"plan\" or
    \"metered\". It decides what the Usage pane leads with: a plan's usage
    windows, with the CLI's dollar figure dimmed as API-equivalent, or the
    money a metered account is spending. Left out, the backend works it out
    where it can — an API key or a cloud provider is metered, a claude.ai
    login a plan — and until it has, the pane shows no dollar figure. A seat
    billed by use signs in exactly as a plan does, so that is the one to set.

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

TRUST:
    A repository's config arrives with the clone, and env, args, settings and
    auth_refresh are what a backend is started with — enough to point the
    official CLI at a host the repository chose, to sign it in as something
    else, or to run a command of its own. So those four do nothing until you
    have read the file and said so:

        niobe trust        this repository's config, as it now stands
        niobe untrust      take it back

    The user's own config is never gated; it is the file you write. What was
    trusted is recorded as the config's SHA-256 in
    ~/.config/niobe/trusted.list, so editing the file — a pull, a rebase, your
    own edit — asks again. Until then the profiles it defines keep their
    backend and their models and lose the rest, the shell says so in the
    transcript, and niobe profiles names what trusting would put in force.
    Answering \"always\" in the shell adds a rule to the repository's config.
    That write is niobe's own and can add no env, no args, no settings and no
    auth_refresh, so a file you had trusted stays trusted across it.

PRICES:
    Costs are computed from a price table bundled into niobe: USD per million
    tokens for each model id, each price dated from the day it took effect, so
    a session is priced at the rates of the day it ran. A model id the table
    does not list is unpriced; nothing is guessed from a similar id.
    ~/.config/niobe/prices.toml ($XDG_CONFIG_HOME/niobe when that is set) has
    the same shape and replaces the whole price history of every id it lists.

SESSIONS:
    Every session is recorded, append-only, into .niobe/sessions.db at the root
    of the repository it runs in, or of the working directory outside one.
    With standard output redirected, --resume and replay print what the session
    folds to instead of opening the shell.

    niobe sessions lists two kinds. The first is niobe's own, by the number the
    store gave them. The second is the sessions the claude CLI recorded for
    this repository, by the id it calls them by: --resume with one of those
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
    /                      On an empty composer, search the transcript: Up and
                           Enter step to the match above, Down to the one
                           below, Esc puts the view back where it was, and a
                           second / types a prompt that starts with one
    @                      At the start of a word, offer the files git lists
                           under the session's directory: Up / Down choose,
                           Tab or Enter put the path in the prompt, Esc leaves
                           the word as typed
    !                      On an empty composer, run a command in the
                           session's directory instead of sending a prompt:
                           Enter runs it, Esc goes back, and a second ! types
                           a prompt that starts with one
    Ctrl+G                 Stop the newest ! command still running, with
                           everything it started; again, the one before it

    A command run with ! is yours: no rule is consulted and no mode applies,
    as in any other terminal. It has no terminal of its own and nothing to
    read, runs until it ends, Ctrl+G stops it or niobe quits, and is kept in
    the session as a call, with the end of what it printed shown under it. A
    stopped command is asked to end, killed if it has not two seconds later,
    and its call ends as stopped by the operator. Ctrl+C quits, as always.
    The agent is not told it ran and does not see what it printed.
    Shift+Tab              Cycle how tool calls are gated: plan, ask, auto
    F1, Esc 1              Say which keys move around the shell
    F5, Esc 5              On a plan, say when its usage windows come back
    F6, Esc 6              Fold or open the working tree section
    F7, Esc 7              Fold or open the tools section
    F8, Esc 8              Pick a model from the ones the profile names
    F9, Esc 9              Cycle the palette the shell draws in
    F10, Esc 0, Ctrl+Q     Quit

    F2, F3 and F4 name views that are not implemented yet, and say so.

    Every F-key is also Esc and then its digit, 0 for F10, which is how the
    bar at the foot of the shell names them: on a Mac the top row is media
    keys unless Fn is held, and Esc and a digit reach every terminal. Alt and
    the digit does the same where the terminal sends Option as Meta.

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

    F8 picks a model from the ones the profile names. The switch applies from
    the next turn and keeps everything said so far: the running session is told
    to change, not replaced. The backend resolves the name it is given and says
    what it ended up on, which may be spelt differently from the way it was
    asked for.

THEME:
    cyber is green and magenta on black, classic is DOS blue, neo is green on
    black and modern is an editor's grey. --theme <name> opens the shell in
    one, theme = \"neo\" at the top of a config makes it the one every session
    opens in, and F9 cycles them while a session runs.

    classic is drawn in the sixteen colours the terminal names, so your own
    colour scheme is what they mean. cyber, neo and modern are drawn in their
    own 24-bit colours where COLORTERM is truecolor or 24bit, and in the
    sixteen everywhere else, so a session stays legible over SSH and in
    screen.

    While a turn runs, the desktop behind the panes moves: rain in neo,
    drifting words in cyber, a radar in modern; classic stays still. You see
    it between the panes and at the screen's edges. effects = false at the top
    of a config keeps it still in every theme.

BUDGET:
    --budget <amount> caps what a session may spend, in dollars, and niobe says
    so in the transcript once most of it is gone. The backend enforces the cap
    and checks it between turns rather than inside one, so a session can finish
    above the figure by what the turn that crosses the line costs. On a
    subscription plan the figure the backend reports is what the same work
    would have cost on the provider's API, so the cap is on that and not on
    money that moved.

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
    above the prefix with ..; such a call is asked about. The user's config and
    the repository's both apply; a rule in either allows the call.

BACKENDS:
    A claude profile drives the official `claude` CLI as a subprocess, with the
    profile's environment, arguments and settings file and the repository as
    its working directory. Niobe never reads the CLI's credential files and never sets its
    user agent: whatever that binary is signed in as is what the session runs
    on. Every cost the CLI reports is one it computed from published prices, so
    Niobe stores it as API-equivalent and never as money that moved.

    Not implemented yet: the codex bridge and the native agent loop spawn
    nothing, so a session under one of those profiles has nowhere to send a
    prompt and says so.",
        name = niobe_core::APP_NAME,
        version = niobe_core::VERSION,
    );
}

#[cfg(test)]
mod tests {
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
    fn the_shell_opens_saying_why_an_untrusted_config_is_not_in_force_and_how_to_trust_it() {
        let path = PathBuf::from("/r/.niobe/config.toml");
        let loaded = config::Loaded {
            config: niobe_config::Config::default(),
            searched: vec![path.clone()],
            untrusted: Some(path.clone()),
        };

        let app = say_untrusted(App::new(niobe_tui::app::Repo::default()), &loaded);

        let entry = app
            .entries()
            .first()
            .expect("the shell opens on the notice");
        assert_eq!(entry.kind, niobe_tui::app::EntryKind::Notice);
        assert_eq!(entry.meta, path.display().to_string());
        assert!(entry.body.contains("niobe trust"), "{}", entry.body);
        assert!(
            entry.body.contains("arrives with a clone"),
            "{}",
            entry.body
        );
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
