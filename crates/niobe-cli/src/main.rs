// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The `niobe` binary.
//!
//! The entry point that opens the shell. It understands `--help` and
//! `--version` and nothing else.

// The CLI is the one place in the workspace that writes to the terminal
// directly rather than through the TUI.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;

use niobe_tui::app::Repo;

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("--version" | "-V") => {
            println!("{} {}", niobe_core::APP_NAME, niobe_core::VERSION);
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            print_help();
            ExitCode::SUCCESS
        }
        None => shell(),
        Some(other) => {
            eprintln!("niobe: unknown argument `{other}`");
            print_help();
            ExitCode::FAILURE
        }
    }
}

/// Opens the shell.
///
/// Without a terminal there is nothing to draw into and raw mode would fail, so
/// a piped or redirected run prints the help instead of an errno.
fn shell() -> ExitCode {
    if !std::io::stdout().is_terminal() {
        print_help();
        return ExitCode::SUCCESS;
    }

    match niobe_tui::run(repo()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // The TUI restores the terminal before returning, so this reaches a
            // usable screen.
            eprintln!("niobe: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Where the session is running: the directory it was started in, and the
/// branch checked out there.
///
/// Read here rather than in the TUI, which has no business touching the
/// filesystem.
fn repo() -> Repo {
    let cwd = std::env::current_dir().unwrap_or_default();
    let name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "niobe".to_owned());

    Repo {
        name,
        branch: git_branch(&cwd),
    }
}

/// The checked-out branch, read from `.git/HEAD` in the nearest repository.
///
/// Parsed rather than shelled out to: `git` may not be installed, and a
/// subprocess for one line of a file is a subprocess the operator pays for.
fn git_branch(from: &Path) -> Option<String> {
    let mut dir = Some(from);
    while let Some(current) = dir {
        let head = current.join(".git").join("HEAD");
        if let Ok(contents) = std::fs::read_to_string(&head) {
            let head = contents.trim();
            return match head.strip_prefix("ref: refs/heads/") {
                Some(branch) => Some(branch.to_owned()),
                // A detached HEAD is the commit itself, shortened the way git
                // shortens it.
                None => Some(head.chars().take(7).collect()),
            };
        }
        dir = current.parent();
    }
    None
}

fn print_help() {
    println!(
        "\
{name} {version}
A terminal coding agent that shows you the bill.

USAGE:
    niobe            Open the shell in the current directory
    niobe [OPTIONS]

OPTIONS:
    -h, --help       Print this help
    -V, --version    Print the version

IN THE SHELL:
    Enter            Send what is in the composer
    Alt+Enter        Open a new line in the composer
    PgUp / PgDn      Scroll the transcript
    F10, Ctrl+Q      Quit

No backend is attached yet: the claude and codex bridges are not implemented.",
        name = niobe_core::APP_NAME,
        version = niobe_core::VERSION,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_branch_is_read_from_the_repository_this_test_runs_in() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let branch = git_branch(here).expect("the workspace is a git repository");
        assert!(!branch.is_empty());
    }

    #[test]
    fn a_directory_outside_a_repository_has_no_branch() {
        assert_eq!(git_branch(Path::new("/")), None);
    }
}
