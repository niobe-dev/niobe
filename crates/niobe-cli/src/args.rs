// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The command line, parsed.
//!
//! Hand-rolled: a handful of forms do not justify an argument-parsing
//! dependency in a binary with a size budget.

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
    /// List the profiles the config defines, marking the selected one.
    Profiles,
    /// Fold a JSON Lines event log, for development.
    Replay(PathBuf),
    /// Print the help.
    Help,
    /// Print the version.
    Version,
}

/// A command, and the profile it was asked to run under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// What to do.
    pub command: Command,
    /// The profile `--profile` named, if it was given.
    pub profile: Option<String>,
}

/// Parses the arguments after the program name. The error is a sentence for
/// the operator.
///
/// `--profile <name>` may stand anywhere on the line, since it qualifies the
/// command rather than being one.
pub fn parse(args: &[String]) -> Result<Invocation, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let (profile, rest) = take_profile(&args)?;
    let command = command(&rest)?;

    let applies = match command {
        Command::Shell
        | Command::Resume(_)
        | Command::Profiles
        | Command::Help
        | Command::Version => true,
        Command::Sessions | Command::Replay(_) => false,
    };
    if profile.is_some() && !applies {
        return Err(
            "`--profile` applies to the shell, `--resume` and `profiles`, and to nothing else"
                .to_owned(),
        );
    }
    Ok(Invocation { command, profile })
}

/// Removes `--profile <name>` or `--profile=<name>` from `args`.
fn take_profile<'a>(args: &[&'a str]) -> Result<(Option<String>, Vec<&'a str>), String> {
    const MISSING: &str = "`--profile` needs a profile name; `niobe profiles` lists them";

    let mut profile = None;
    let mut rest = Vec::with_capacity(args.len());
    let mut args = args.iter();
    while let Some(&arg) = args.next() {
        let name = if arg == "--profile" {
            args.next().copied().ok_or(MISSING)?
        } else if let Some(name) = arg.strip_prefix("--profile=") {
            name
        } else {
            rest.push(arg);
            continue;
        };

        // A name that looks like a flag is the next flag, with the name left
        // out before it.
        if name.is_empty() || name.starts_with('-') {
            return Err(MISSING.to_owned());
        }
        if profile.replace(name.to_owned()).is_some() {
            return Err("`--profile` is given more than once".to_owned());
        }
    }
    Ok((profile, rest))
}

fn command(args: &[&str]) -> Result<Command, String> {
    let command = match args {
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
        ["profiles", rest @ ..] => {
            no_more(rest)?;
            Command::Profiles
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

    fn invocation(args: &[&str]) -> Result<Invocation, String> {
        parse(&args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>())
    }

    fn parsed(args: &[&str]) -> Result<Command, String> {
        invocation(args).map(|i| i.command)
    }

    fn profile(args: &[&str]) -> Result<Option<String>, String> {
        invocation(args).map(|i| i.profile)
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
    fn the_profile_flag_may_stand_anywhere_and_takes_a_name_either_way() {
        assert_eq!(
            invocation(&["--profile", "work"]),
            Ok(Invocation {
                command: Command::Shell,
                profile: Some("work".to_owned())
            })
        );
        assert_eq!(profile(&["--profile=work"]), Ok(Some("work".to_owned())));
        assert_eq!(
            invocation(&["--resume", "3", "--profile", "work"]),
            Ok(Invocation {
                command: Command::Resume(id("3")),
                profile: Some("work".to_owned())
            })
        );
        assert_eq!(
            parsed(&["profiles", "--profile=work"]),
            Ok(Command::Profiles)
        );
        assert_eq!(profile(&[]), Ok(None));
    }

    #[test]
    fn the_profile_flag_without_a_name_points_at_the_profile_list() {
        for args in [
            &["--profile"][..],
            &["--profile="],
            &["--profile", "--resume", "3"],
        ] {
            let error = invocation(args).expect_err("no name");
            assert!(error.contains("niobe profiles"), "{args:?}: {error}");
        }
    }

    #[test]
    fn the_profile_flag_is_given_once_and_only_where_a_session_runs() {
        assert!(invocation(&["--profile", "a", "--profile=b"]).is_err());
        for args in [
            &["--profile", "work", "sessions"][..],
            &["replay", "log.jsonl", "--profile=work"],
        ] {
            let error = invocation(args).expect_err("does not apply");
            assert!(error.contains("applies to"), "{args:?}: {error}");
        }
        assert_eq!(
            parsed(&["profiles", "extra"]),
            Err("unexpected argument `extra`".to_owned())
        );
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
