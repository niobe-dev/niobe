// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The command line, parsed.
//!
//! Hand-rolled: five forms do not justify an argument-parsing dependency in a
//! binary with a size budget.

use std::path::PathBuf;

use niobe_store::SessionId;

/// What the operator asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Open the shell on a new session.
    Shell,
    /// Open the shell on a recorded session and keep recording into it.
    Resume(SessionId),
    /// List the sessions recorded in this repository.
    Sessions,
    /// Fold a JSON Lines event log, for development.
    Replay(PathBuf),
    /// Print the help.
    Help,
    /// Print the version.
    Version,
}

/// Parses the arguments after the program name. The error is a sentence for
/// the operator.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let command = match args.as_slice() {
        [] => Command::Shell,
        ["-h" | "--help", ..] => Command::Help,
        ["-V" | "--version", ..] => Command::Version,
        ["--resume"] => {
            return Err("`--resume` needs a session id; `niobe sessions` lists them".to_owned());
        }
        ["--resume", id, rest @ ..] => {
            no_more(rest)?;
            Command::Resume(session_id(id)?)
        }
        [flag, rest @ ..] if flag.starts_with("--resume=") => {
            no_more(rest)?;
            Command::Resume(session_id(&flag["--resume=".len()..])?)
        }
        ["sessions", rest @ ..] => {
            no_more(rest)?;
            Command::Sessions
        }
        ["replay"] => return Err("`replay` needs a log file".to_owned()),
        ["replay", file, rest @ ..] => {
            no_more(rest)?;
            Command::Replay(PathBuf::from(file))
        }
        [other, ..] => return Err(format!("unknown argument `{other}`")),
    };
    Ok(command)
}

fn session_id(id: &str) -> Result<SessionId, String> {
    id.parse()
        .map_err(|_| format!("`{id}` is not a session id; `niobe sessions` lists them"))
}

fn no_more(rest: &[&str]) -> Result<(), String> {
    match rest.first() {
        None => Ok(()),
        Some(extra) => Err(format!("unexpected argument `{extra}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Command, String> {
        parse(&args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>())
    }

    fn id(n: &str) -> SessionId {
        n.parse().expect("a number is a session id")
    }

    #[test]
    fn no_arguments_opens_the_shell() {
        assert_eq!(parsed(&[]), Ok(Command::Shell));
    }

    #[test]
    fn resume_takes_the_id_as_the_next_argument_or_after_an_equals_sign() {
        assert_eq!(parsed(&["--resume", "3"]), Ok(Command::Resume(id("3"))));
        assert_eq!(parsed(&["--resume=3"]), Ok(Command::Resume(id("3"))));
    }

    #[test]
    fn resume_without_a_usable_id_points_at_the_session_list() {
        for args in [&["--resume"][..], &["--resume", "latest"], &["--resume="]] {
            let error = parsed(args).expect_err("no usable id");
            assert!(error.contains("niobe sessions"), "{args:?}: {error}");
        }
    }

    #[test]
    fn replay_takes_a_file() {
        assert_eq!(
            parsed(&["replay", "log.jsonl"]),
            Ok(Command::Replay(PathBuf::from("log.jsonl")))
        );
        assert!(parsed(&["replay"]).is_err());
    }

    #[test]
    fn subcommands_take_nothing_extra() {
        assert_eq!(parsed(&["sessions"]), Ok(Command::Sessions));
        assert_eq!(
            parsed(&["sessions", "--all"]),
            Err("unexpected argument `--all`".to_owned())
        );
        assert!(parsed(&["replay", "a.jsonl", "b.jsonl"]).is_err());
        assert!(parsed(&["--resume", "1", "2"]).is_err());
    }

    #[test]
    fn help_and_version_win_over_anything_after_them() {
        assert_eq!(parsed(&["--help", "whatever"]), Ok(Command::Help));
        assert_eq!(parsed(&["-V"]), Ok(Command::Version));
    }

    #[test]
    fn an_unknown_argument_is_named() {
        assert_eq!(
            parsed(&["--frobnicate"]),
            Err("unknown argument `--frobnicate`".to_owned())
        );
    }
}
