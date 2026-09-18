// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The `niobe` binary.
//!
//! Opens the shell on a new or a recorded session under a profile from the
//! config, lists the sessions recorded in a repository, the profiles defined for
//! it and the prices costs are computed from, and folds a JSON Lines event log
//! for development. It is the only crate that names the shell, the config, the
//! ledger and the session store together, so it is where they are joined.

// The CLI is the one place in the workspace that writes to the terminal
// directly rather than through the TUI.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod args;
mod backend;
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
use niobe_tui::{Detached, Ended, Forgotten};

use crate::args::{Command, Invocation};
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
    }: Invocation,
) -> Result<(), String> {
    let profile = profile.as_deref();
    match command {
        Command::Shell => shell(profile, budget),
        Command::Resume(session) => resume(session, profile, budget),
        Command::Sessions => list_sessions(),
        Command::Profiles => list_profiles(profile),
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

/// Opens the shell on a new session, recorded into this repository's store.
///
/// Without a terminal there is nothing to draw into and raw mode would fail, so
/// a piped or redirected run prints the help instead of an errno. The config is
/// read first either way, so a config that cannot be used is reported rather
/// than hidden behind the help.
fn shell(profile: Option<&str>, budget: Option<f64>) -> Result<(), String> {
    let cwd = cwd()?;
    let root = repo::root(&cwd);
    let loaded = config::load(&root)?;
    let selected = loaded.select(profile)?;
    let app = App::new(repo::describe(&cwd)).with_rules(loaded.config.allowed().clone());
    let app = match &selected {
        Some(selected) => app.with_profile(config::named(*selected)),
        None => app,
    };
    let app = match budget {
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
            budget_usd: budget,
            ..backend::Attach::default()
        },
    )?;
    let app = if backend.attached() {
        app.attached()
    } else {
        app
    };

    let mut journal = StoreJournal::Pending(root.clone());
    let mut rules = ConfigRules::at(&root);
    let ended = niobe_tui::run(app, &mut journal, backend.bridge(), &mut rules)
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
fn resume(session: SessionId, profile: Option<&str>, budget: Option<f64>) -> Result<(), String> {
    let cwd = cwd()?;
    let root = repo::root(&cwd);
    let loaded = config::load(&root)?;
    let selected = loaded.select(profile)?;
    let mut app = App::new(repo::describe(&cwd)).with_rules(loaded.config.allowed().clone());
    if let Some(selected) = &selected {
        app = app.with_profile(config::named(*selected));
    }
    if let Some(budget) = budget {
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
    app.extend(stored.iter().map(|s| &s.event));
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
            budget_usd: budget,
        },
    )?;
    let app = if backend.attached() {
        app.attached()
    } else {
        app
    };

    let mut journal = StoreJournal::Open(recorder);
    let mut rules = ConfigRules::at(&root);
    niobe_tui::run(app, &mut journal, backend.bridge(), &mut rules)
        .map_err(|e| e.to_string())
        .map(|_| ())
}

/// Prints the sessions recorded in this repository, newest first.
fn list_sessions() -> Result<(), String> {
    let root = repo::root(&cwd()?);
    let sessions = match repo::open_existing_store(&root)? {
        Some(store) => store.sessions().map_err(|e| e.to_string())?,
        None => Vec::new(),
    };

    if sessions.is_empty() {
        println!("no sessions recorded in {}", root.display());
        return Ok(());
    }
    for line in sessions::table(&sessions) {
        println!("{line}");
    }
    Ok(())
}

/// Prints the profiles the config defines for this repository, the selected one
/// marked.
fn list_profiles(profile: Option<&str>) -> Result<(), String> {
    let loaded = config::load(&repo::root(&cwd()?))?;
    let selected = loaded.selected(profile)?;

    if loaded.config.profiles().is_empty() {
        println!("{}", profiles::none_defined(&loaded.searched));
        return Ok(());
    }
    for line in profiles::table(&loaded.config, selected.as_ref().map(|s| s.name.as_str())) {
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
    let mut app = App::new(repo::describe(&cwd()?));
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
    niobe_tui::run(app, &mut Unrecorded, &mut Detached, &mut Forgotten)
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
A terminal coding agent that shows you the bill.

USAGE:
    niobe                  Open the shell on a new session
    niobe --resume <id>    Open the shell on a recorded session and continue it
    niobe sessions         List the sessions recorded in this repository
    niobe profiles         List the profiles the config defines, the selected one marked
    niobe prices [model]   List the prices in force today, or every price a model has had
    niobe replay <file>    Fold a JSON Lines event log into the shell (development)

OPTIONS:
    --profile <name>       Run under this profile instead of the default one
    --budget <amount>      Stop the session once it has cost this many dollars
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

    The user's config is ~/.config/niobe/config.toml ($XDG_CONFIG_HOME/niobe
    when that is set); a repository's is .niobe/config.toml at its root, and
    overrides the user's, replacing any profile of the same name whole. The
    env of a profile is passed on exactly as written. The models of a profile
    are the ones F8 offers in the shell, named the way its backend takes them;
    a profile that names none has nothing to switch between, because niobe
    never invents a model id.

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

IN THE SHELL:
    Enter                  Send what is in the composer
    Alt+Enter              Open a new line in the composer
    PgUp / PgDn            Scroll the transcript
    Shift+Tab              Cycle how tool calls are gated: plan, ask, auto
    F8                     Pick a model from the ones the profile names
    F10, Ctrl+Q            Quit

    When a backend stops for permission, the turn waits on a prompt that takes
    the keyboard:

    y, Enter               Allow this call
    n, Esc                 Deny it; the denial is shown in the timeline
    a                      Allow it, and every call to that tool from now on
    p                      Allow it, and every call to that tool on the same
                           target from now on

MODE AND MODEL:
    Shift+Tab cycles the mode the session runs in — plan changes nothing, ask
    stops for every call no rule already allows, auto leaves the decision to
    the backend — and the status line names the one in force. A mode a backend
    reports that niobe has no word for is named rather than shown as one of
    these three.

    F8 picks a model from the ones the profile names. The switch applies from
    the next turn and keeps everything said so far: the running session is told
    to change, not replaced. The backend resolves the name it is given and says
    what it ended up on, which may be spelt differently from the way it was
    asked for.

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
    what else you meant. The user's config and the repository's both apply; a
    rule in either allows the call.

BACKENDS:
    A claude profile drives the official `claude` CLI as a subprocess, with the
    profile's environment and arguments and the repository as its working
    directory. Niobe never reads the CLI's credential files and never sets its
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
    fn durations_print_in_milliseconds_to_two_decimals() {
        assert_eq!(millis(Duration::from_micros(412)), "0.41 ms");
        assert_eq!(millis(Duration::from_millis(50)), "50.00 ms");
    }
}
