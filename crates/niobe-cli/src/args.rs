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
    /// Open the shell on an earlier session and keep recording into it.
    Resume(Resume),
    /// List the sessions recorded in this repository, Niobe's own and the
    /// `claude` CLI's.
    Sessions,
    /// List the profiles the config defines, marking the selected one.
    Profiles,
    /// Record this repository's config as one whose profiles may start a
    /// backend with the environment, arguments and refresh command they name.
    Trust,
    /// Take that back, for this repository's config.
    Untrust,
    /// List the prices in force today, or every price one model has had.
    Prices(Option<String>),
    /// Fold a JSON Lines event log, for development.
    Replay(PathBuf),
    /// Print the help.
    Help,
    /// Print the version.
    Version,
}

/// Which session `--resume` names.
///
/// Two lists answer to one flag because the operator has one question — what
/// can I carry on with? — and `niobe sessions` prints both answers under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resume {
    /// One Niobe recorded, by the number `niobe sessions` prints.
    Recorded(SessionId),
    /// One the `claude` CLI recorded for this repository, by the id it calls
    /// it by. Its history is read into a session of Niobe's own and the CLI is
    /// asked to continue the conversation.
    Imported(String),
}

/// A command, and what it was asked to run under.
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    /// What to do.
    pub command: Command,
    /// The profile `--profile` named, if it was given.
    pub profile: Option<String>,
    /// The most the session may spend, in USD, if `--budget` was given.
    pub budget: Option<f64>,
}

/// Parses the arguments after the program name. The error is a sentence for
/// the operator.
///
/// `--profile <name>` and `--budget <amount>` may stand anywhere on the line,
/// since they qualify the command rather than being one.
pub fn parse(args: &[String]) -> Result<Invocation, String> {
    const NO_PROFILE: &str = "`--profile` needs a profile name; `niobe profiles` lists them";
    const NO_BUDGET: &str = "`--budget` needs an amount in dollars, as in `--budget 0.50`";

    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let (profile, rest) = take_flag(&args, "--profile", NO_PROFILE)?;
    let (budget, rest) = take_flag(&rest, "--budget", NO_BUDGET)?;
    let command = command(&rest)?;

    let runs_a_session = matches!(command, Command::Shell | Command::Resume(_));
    let names_a_profile = runs_a_session
        || matches!(
            command,
            Command::Profiles | Command::Help | Command::Version
        );
    if profile.is_some() && !names_a_profile {
        return Err(
            "`--profile` applies to the shell, `--resume` and `profiles`, and to nothing else"
                .to_owned(),
        );
    }
    if budget.is_some() && !runs_a_session {
        return Err(
            "`--budget` applies to the shell and `--resume`, and to nothing else".to_owned(),
        );
    }

    Ok(Invocation {
        command,
        profile: profile.map(str::to_owned),
        budget: budget.map(budget_usd).transpose()?,
    })
}

/// The amount `--budget` was given, as a ceiling on what a session may spend.
///
/// Zero and less are refused rather than taken as "spend nothing": a session
/// that could not make a single request is not what anyone asked for, and a
/// negative budget is a typo. So is an amount that is not a number.
fn budget_usd(amount: &str) -> Result<f64, String> {
    let refused =
        || format!("`--budget` needs a positive amount in dollars; `{amount}` is not one");
    let budget: f64 = amount.parse().map_err(|_| refused())?;
    if !budget.is_finite() || budget <= 0.0 {
        return Err(refused());
    }
    Ok(budget)
}

/// Removes `<flag> <value>` or `<flag>=<value>` from `args`.
///
/// `missing` is what to say when the flag is there without a value, which is
/// also what a value that looks like a flag means: the next flag, with the
/// value left out before it.
fn take_flag<'a>(
    args: &[&'a str],
    flag: &str,
    missing: &str,
) -> Result<(Option<&'a str>, Vec<&'a str>), String> {
    let equals = format!("{flag}=");
    let mut found = None;
    let mut rest = Vec::with_capacity(args.len());
    let mut args = args.iter();
    while let Some(&arg) = args.next() {
        let value = if arg == flag {
            args.next().copied().ok_or_else(|| missing.to_owned())?
        } else if let Some(value) = arg.strip_prefix(&equals) {
            value
        } else {
            rest.push(arg);
            continue;
        };

        if value.is_empty() || value.starts_with('-') {
            return Err(missing.to_owned());
        }
        if found.replace(value).is_some() {
            return Err(format!("`{flag}` is given more than once"));
        }
    }
    Ok((found, rest))
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
            Command::Resume(resume(id)?)
        }
        [flag, rest @ ..] if flag.starts_with("--resume=") => {
            no_more(rest)?;
            Command::Resume(resume(&flag["--resume=".len()..])?)
        }
        ["sessions", rest @ ..] => {
            no_more(rest)?;
            Command::Sessions
        }
        ["profiles", rest @ ..] => {
            no_more(rest)?;
            Command::Profiles
        }
        ["trust", rest @ ..] => {
            no_more(rest)?;
            Command::Trust
        }
        ["untrust", rest @ ..] => {
            no_more(rest)?;
            Command::Untrust
        }
        ["prices"] => Command::Prices(None),
        ["prices", model, rest @ ..] => {
            no_more(rest)?;
            Command::Prices(Some((*model).to_owned()))
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

/// The session an id names.
///
/// A number is one of Niobe's own, because that is what the store allocates
/// and what the session list prints. Anything else is taken as an id the
/// `claude` CLI calls a session of its own by: whether there is such a session
/// is a question for this repository's transcripts, not for the command line,
/// and refusing an id here on its shape would refuse one the CLI is holding.
fn resume(id: &str) -> Result<Resume, String> {
    if id.trim().is_empty() {
        return Err("`--resume` needs a session id; `niobe sessions` lists them".to_owned());
    }
    Ok(match id.parse::<SessionId>() {
        Ok(recorded) => Resume::Recorded(recorded),
        Err(_) => Resume::Imported(id.to_owned()),
    })
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

    fn id(n: &str) -> Resume {
        Resume::Recorded(n.parse().expect("a number is a session id"))
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
    fn resume_without_an_id_at_all_points_at_the_session_list() {
        for args in [&["--resume"][..], &["--resume="], &["--resume", " "]] {
            let error = parsed(args).expect_err("no id");
            assert!(error.contains("niobe sessions"), "{args:?}: {error}");
        }
    }

    #[test]
    fn an_id_that_is_not_one_of_niobes_numbers_is_one_the_claude_cli_holds() {
        assert_eq!(
            parsed(&["--resume", "2f6c1e10-8f4b-4d2a-9c3e-7a5b0d1e6f42"]),
            Ok(Command::Resume(Resume::Imported(
                "2f6c1e10-8f4b-4d2a-9c3e-7a5b0d1e6f42".to_owned()
            )))
        );
        // Which of the two lists holds it is not the command line's to say:
        // an id that names neither is reported when it is looked for.
        assert_eq!(
            parsed(&["--resume=latest"]),
            Ok(Command::Resume(Resume::Imported("latest".to_owned())))
        );
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
    fn prices_takes_an_optional_model() {
        assert_eq!(parsed(&["prices"]), Ok(Command::Prices(None)));
        assert_eq!(
            parsed(&["prices", "claude-opus-5"]),
            Ok(Command::Prices(Some("claude-opus-5".to_owned())))
        );
        assert_eq!(
            parsed(&["prices", "a", "b"]),
            Err("unexpected argument `b`".to_owned())
        );
        assert!(invocation(&["prices", "--profile", "work"]).is_err());
    }

    #[test]
    fn trust_and_untrust_name_this_repositorys_config_and_nothing_else() {
        assert_eq!(parsed(&["trust"]), Ok(Command::Trust));
        assert_eq!(parsed(&["untrust"]), Ok(Command::Untrust));
        assert_eq!(
            parsed(&["trust", "/elsewhere/.niobe/config.toml"]),
            Err("unexpected argument `/elsewhere/.niobe/config.toml`".to_owned()),
            "the file trusted is the one in front of the operator"
        );
        // A profile is a thing inside the file; trust is about the file.
        for args in [
            &["trust", "--profile=work"][..],
            &["untrust", "--profile=w"],
        ] {
            let error = invocation(args).expect_err("does not apply");
            assert!(error.contains("applies to"), "{args:?}: {error}");
        }
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
                profile: Some("work".to_owned()),
                budget: None,
            })
        );
        assert_eq!(profile(&["--profile=work"]), Ok(Some("work".to_owned())));
        assert_eq!(
            invocation(&["--resume", "3", "--profile", "work"]),
            Ok(Invocation {
                command: Command::Resume(id("3")),
                profile: Some("work".to_owned()),
                budget: None,
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
    fn the_budget_flag_takes_an_amount_either_way_round() {
        assert_eq!(
            invocation(&["--budget", "0.50"]).map(|i| i.budget),
            Ok(Some(0.5))
        );
        assert_eq!(invocation(&["--budget=2"]).map(|i| i.budget), Ok(Some(2.0)));
        assert_eq!(
            invocation(&["--resume", "3", "--budget", "1.25"]),
            Ok(Invocation {
                command: Command::Resume(id("3")),
                profile: None,
                budget: Some(1.25),
            })
        );
        assert_eq!(invocation(&[]).map(|i| i.budget), Ok(None));
    }

    #[test]
    fn a_budget_that_is_not_an_amount_to_spend_is_refused() {
        for args in [
            &["--budget"][..],
            &["--budget", "lots"],
            &["--budget", "-1"],
            &["--budget", "0"],
            &["--budget="],
        ] {
            let error = invocation(args).expect_err("not an amount");
            assert!(error.contains("--budget"), "{args:?}: {error}");
        }
        assert!(invocation(&["--budget", "1", "--budget=2"]).is_err());
    }

    #[test]
    fn a_budget_applies_only_where_a_session_runs() {
        for args in [
            &["--budget", "1", "sessions"][..],
            &["prices", "--budget=1"],
        ] {
            let error = invocation(args).expect_err("does not apply");
            assert!(error.contains("applies to"), "{args:?}: {error}");
        }
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
