// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The `niobe` binary.
//!
//! Opens the shell on a new or a recorded session under a profile from the
//! config, lists the sessions recorded in a repository and the profiles defined
//! for it, and folds a JSON Lines event log for development. It is the only
//! crate that names the shell, the config and the session store together, so it
//! is where they are joined.

// The CLI is the one place in the workspace that writes to the terminal
// directly rather than through the TUI.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod args;
mod config;
mod journal;
mod profiles;
mod repo;
mod sessions;
mod summary;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use niobe_store::{Recorder, SessionId, read_log};
use niobe_tui::app::App;
use niobe_tui::journal::Unrecorded;

use crate::args::{Command, Invocation};
use crate::journal::StoreJournal;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args::parse(&args) {
        Ok(invocation) => run(invocation),
        Err(usage) => Err(format!("{usage}\nRun `niobe --help` for usage.")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            // The TUI restores the terminal before returning, so this reaches a
            // usable screen.
            eprintln!("niobe: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(Invocation { command, profile }: Invocation) -> Result<(), String> {
    let profile = profile.as_deref();
    match command {
        Command::Shell => shell(profile),
        Command::Resume(session) => resume(session, profile),
        Command::Sessions => list_sessions(),
        Command::Profiles => list_profiles(profile),
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
fn shell(profile: Option<&str>) -> Result<(), String> {
    let cwd = cwd()?;
    let root = repo::root(&cwd);
    let app = new_app(&cwd, &root, profile)?;

    if !std::io::stdout().is_terminal() {
        print_help();
        return Ok(());
    }

    let mut journal = StoreJournal::Pending(root.clone());
    niobe_tui::run(app, &mut journal).map_err(|e| e.to_string())?;

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
fn resume(session: SessionId, profile: Option<&str>) -> Result<(), String> {
    let cwd = cwd()?;
    let root = repo::root(&cwd);
    let mut app = new_app(&cwd, &root, profile)?;

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

    let mut journal = StoreJournal::Open(recorder);
    niobe_tui::run(app, &mut journal).map_err(|e| e.to_string())
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

/// An empty shell for `cwd`, under the profile `requested` names or the
/// config's default.
fn new_app(cwd: &Path, root: &Path, requested: Option<&str>) -> Result<App, String> {
    let app = App::new(repo::describe(cwd));
    Ok(match config::load(root)?.selected(requested)? {
        Some(profile) => app.with_profile(profile),
        None => app,
    })
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

    niobe_tui::run(app, &mut Unrecorded).map_err(|e| e.to_string())
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
    niobe replay <file>    Fold a JSON Lines event log into the shell (development)

OPTIONS:
    --profile <name>       Run under this profile instead of the default one
    -h, --help             Print this help
    -V, --version          Print the version

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

    The user's config is ~/.config/niobe/config.toml ($XDG_CONFIG_HOME/niobe
    when that is set); a repository's is .niobe/config.toml at its root, and
    overrides the user's, replacing any profile of the same name whole. The
    env of a profile is passed on exactly as written.

SESSIONS:
    Every session is recorded, append-only, into .niobe/sessions.db at the root
    of the repository it runs in, or of the working directory outside one.
    With standard output redirected, --resume and replay print what the session
    folds to instead of opening the shell.

IN THE SHELL:
    Enter                  Send what is in the composer
    Alt+Enter              Open a new line in the composer
    PgUp / PgDn            Scroll the transcript
    F10, Ctrl+Q            Quit

No backend is attached yet: the claude and codex bridges are not implemented.",
        name = niobe_core::APP_NAME,
        version = niobe_core::VERSION,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_print_in_milliseconds_to_two_decimals() {
        assert_eq!(millis(Duration::from_micros(412)), "0.41 ms");
        assert_eq!(millis(Duration::from_millis(50)), "50.00 ms");
    }
}
