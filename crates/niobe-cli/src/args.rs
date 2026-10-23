// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The command line, parsed.
//!
//! Hand-rolled: a handful of forms do not justify an argument-parsing
//! dependency in a binary with a size budget.

use std::ffi::OsString;
use std::path::PathBuf;

use niobe_core::permission::Rule;
use niobe_ledger::Date;
use niobe_store::SessionId;
use niobe_tui::Theme;
use niobe_tui::theme;

use crate::cost::Format;

/// What the operator asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Open the shell on a new session.
    Shell,
    /// Open the shell on an earlier session and keep recording into it.
    Resume(Resume),
    /// Run one turn with no terminal and print its reply, or with `--json`
    /// its outcome and spend in the shape `schema/print.schema.json` names.
    Print {
        /// What the turn is asked.
        prompt: String,
        /// `--json` was given.
        json: bool,
    },
    /// Run every task of a suite as one headless turn in a copy of its
    /// repository, judge each by its check, and print what each spent.
    Bench {
        /// The suite's directory.
        suite: PathBuf,
        /// `--json` was given: the results in the shape
        /// `schema/bench.schema.json` names.
        json: bool,
        /// How long a task's turn may run, where `--timeout` said.
        timeout: Option<std::time::Duration>,
    },
    /// List the sessions recorded in this repository, Niobe's own and the
    /// `claude` CLI's.
    Sessions,
    /// Print every session recorded in this repository, folded, as one
    /// Markdown page, narrowed to the sessions the flags name.
    Overview(Narrowing),
    /// List every session recorded in this repository with what it spent,
    /// narrowed as the overview is, in the format the flags name.
    Cost {
        /// Which sessions.
        narrowing: Narrowing,
        /// How they print.
        format: Format,
    },
    /// List the profiles the config defines, marking the selected one.
    Profiles,
    /// Check what a first session needs and say what is missing, with the fix.
    Doctor {
        /// `--json` was given: the same report for an issue, as JSON.
        json: bool,
    },
    /// Show what this repository's config would put in force, values
    /// included, and record it as one whose profiles may start a backend with
    /// the environment, arguments and refresh command they name once the
    /// operator agrees.
    Trust {
        /// `--yes` was given: the operator agrees without being asked, as a
        /// script that has read the file does. Without it the operator is
        /// asked on the terminal, and where there is none nothing is recorded.
        yes: bool,
    },
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
    /// The palette `--theme` named, if it was given. It beats the one a config
    /// names, and `F9` beats both for the rest of the session.
    pub theme: Option<Theme>,
    /// What `--terse on` or `--terse off` asked, beating the config's
    /// `terse`; `None` where it was not given.
    pub terse: Option<bool>,
    /// The rules each `--allow` gave, which answer a headless turn's prompts
    /// beside the config's for this run alone. The operator typed them, so
    /// they need no trust; they are never written anywhere.
    pub allow: Vec<Rule>,
}

/// The directory `niobe hook trim --raw <dir>` names, where `args` are exactly
/// that: the hook the `claude` CLI runs under `trim = true`, which nobody
/// types and which is answered before the rest of the command line is read.
/// The directory is the session's own and need not be text.
pub fn hook_trim(args: impl IntoIterator<Item = OsString>) -> Option<PathBuf> {
    let mut args = args.into_iter();
    let words = [args.next()?, args.next()?, args.next()?];
    let dir = args.next()?;
    let fits = words
        .iter()
        .map(|w| w.to_str())
        .eq([Some("hook"), Some("trim"), Some("--raw")])
        && args.next().is_none();
    fits.then(|| PathBuf::from(dir))
}

/// The arguments after the program name, as text, or a sentence for the
/// operator naming the first that is not UTF-8.
///
/// Every form the line takes is spelled in text, and a session id or a
/// profile name that is not text names nothing, so such an argument is
/// refused here rather than read as something it is not.
pub fn utf8(args: impl IntoIterator<Item = OsString>) -> Result<Vec<String>, String> {
    args.into_iter()
        .map(|arg| {
            arg.into_string().map_err(|arg| {
                format!(
                    "the argument `{}` is not UTF-8, and niobe reads only UTF-8 arguments",
                    arg.to_string_lossy()
                )
            })
        })
        .collect()
}

/// Parses the arguments after the program name. The error is a sentence for
/// the operator.
///
/// `--profile <name>`, `--budget <amount>` and `--theme <name>` may stand
/// anywhere on the line, since they qualify the command rather than being one.
pub fn parse(args: &[String]) -> Result<Invocation, String> {
    const NO_PROFILE: &str = "`--profile` needs a profile name; `niobe profiles` lists them";
    const NO_BUDGET: &str = "`--budget` needs an amount in dollars, as in `--budget 0.50`";
    const NO_TERSE: &str = "`--terse` needs `on` or `off`";
    const NO_RULE: &str = "`--allow` needs a rule, as in `--allow \"Bash(cargo test)\"`";
    let no_theme = || format!("`--theme` needs a theme name; expected {}", theme::listed());

    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let (profile, rest) = take_flag(&args, "--profile", NO_PROFILE)?;
    let (budget, rest) = take_flag(&rest, "--budget", NO_BUDGET)?;
    let (theme, rest) = take_flag(&rest, "--theme", &no_theme())?;
    let (allow, rest) = take_every(&rest, "--allow", NO_RULE)?;
    let (terse, rest) = take_flag(&rest, "--terse", NO_TERSE)?;
    let command = command(&rest)?;

    let runs_a_session = matches!(command, Command::Shell | Command::Resume(_));
    let runs_a_turn =
        runs_a_session || matches!(command, Command::Print { .. } | Command::Bench { .. });
    let names_a_profile = runs_a_turn
        || matches!(
            command,
            Command::Profiles | Command::Help | Command::Version
        );
    if profile.is_some() && !names_a_profile {
        return Err(
            "`--profile` applies to the shell, `--resume`, `-p`, `bench` and `profiles`, and to \
             nothing else"
                .to_owned(),
        );
    }
    if budget.is_some() && !runs_a_turn {
        return Err(
            "`--budget` applies to the shell, `--resume`, `-p` and `bench`, and to nothing else"
                .to_owned(),
        );
    }
    let headless = matches!(command, Command::Print { .. } | Command::Bench { .. });
    if terse.is_some() && !runs_a_turn {
        return Err(
            "`--terse` applies to the shell, `--resume`, `-p` and `bench`, and to nothing else"
                .to_owned(),
        );
    }
    if !allow.is_empty() && !headless {
        return Err(
            "`--allow` applies to `-p` and `bench`, and to nothing else: in the shell, a \
             prompt answered with always is the way to add a rule"
                .to_owned(),
        );
    }
    if theme.is_some() && !runs_a_session && !matches!(command, Command::Replay(_)) {
        return Err(
            "`--theme` applies to the shell, `--resume` and `replay`, and to nothing else"
                .to_owned(),
        );
    }

    Ok(Invocation {
        command,
        profile: profile.map(str::to_owned),
        budget: budget.map(budget_usd).transpose()?,
        theme: theme.map(named_theme).transpose()?,
        terse: terse
            .map(|said| match said {
                "on" => Ok(true),
                "off" => Ok(false),
                _ => Err(NO_TERSE.to_owned()),
            })
            .transpose()?,
        allow: allow
            .into_iter()
            .map(|rule| Rule::parse(rule).map_err(|e| format!("`--allow {rule}`: {e}")))
            .collect::<Result<_, _>>()?,
    })
}

/// Removes every `<flag> <value>` and `<flag>=<value>` from `args`, for a
/// flag that may be given more than once.
fn take_every<'a>(
    args: &[&'a str],
    flag: &str,
    missing: &str,
) -> Result<(Vec<&'a str>, Vec<&'a str>), String> {
    let equals = format!("{flag}=");
    let mut found = Vec::new();
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
        if value.trim().is_empty() || value.starts_with('-') {
            return Err(missing.to_owned());
        }
        found.push(value);
    }
    Ok((found, rest))
}

/// The palette `--theme` named.
///
/// A name no theme answers to is refused here rather than falling back to the
/// default: an operator who asked for a palette and got the usual one would
/// read it as the theme not having landed.
fn named_theme(name: &str) -> Result<Theme, String> {
    Theme::by_name(name)
        .ok_or_else(|| format!("`{name}` is not a theme; expected {}", theme::listed()))
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

        // A negative number is a value, and is refused by what reads it
        // for what it is rather than taken for the next flag.
        let negative_number = value.strip_prefix('-').is_some_and(|rest| {
            rest.starts_with(|c: char| c.is_ascii_digit()) && value.parse::<f64>().is_ok()
        });
        if value.is_empty() || (value.starts_with('-') && !negative_number) {
            return Err(missing.to_owned());
        }
        if found.replace(value).is_some() {
            return Err(format!("`{flag}` is given more than once"));
        }
    }
    Ok((found, rest))
}

fn command(args: &[&str]) -> Result<Command, String> {
    // Asked for anywhere on the line, as after a subcommand, it is still the
    // help that was asked for.
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        return Ok(Command::Help);
    }
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
        ["-p" | "--print", rest @ ..] => print(rest, false)?,
        ["--json", "-p" | "--print", rest @ ..] => print(rest, true)?,
        ["bench", rest @ ..] => bench(rest)?,
        ["sessions", rest @ ..] => {
            no_more(rest)?;
            Command::Sessions
        }
        ["overview", rest @ ..] => Command::Overview(narrowing(rest)?),
        ["cost", rest @ ..] => cost(rest)?,
        ["profiles", rest @ ..] => {
            no_more(rest)?;
            Command::Profiles
        }
        ["doctor", "--json", rest @ ..] => {
            no_more(rest)?;
            Command::Doctor { json: true }
        }
        ["doctor", rest @ ..] => {
            no_more(rest)?;
            Command::Doctor { json: false }
        }
        ["trust", "--yes", rest @ ..] => {
            no_more(rest)?;
            Command::Trust { yes: true }
        }
        ["trust", rest @ ..] => {
            no_more(rest)?;
            Command::Trust { yes: false }
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
    // A number, signed or too large for one of Niobe's, is still a number:
    // the CLI names its sessions otherwise, and looking one up among them
    // would say there is no such claude session, which is not the mistake.
    let numeric = id
        .strip_prefix(['+', '-'])
        .unwrap_or(id)
        .chars()
        .all(|c| c.is_ascii_digit());
    match (id.parse::<SessionId>(), numeric) {
        (Ok(recorded), _) if id.chars().all(|c| c.is_ascii_digit()) => {
            Ok(Resume::Recorded(recorded))
        }
        (_, true) => Err(format!(
            "`{id}` is not a session number; `niobe sessions` lists them"
        )),
        _ => Ok(Resume::Imported(id.to_owned())),
    }
}

/// Which sessions `niobe overview` folds: all of them unless `--last` or
/// `--since` says otherwise, and with both, the sessions that pass each.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Narrowing {
    /// Only the newest this many.
    pub last: Option<usize>,
    /// Only those started on this UTC day or after it.
    pub since: Option<Date>,
}

fn narrowing(mut rest: &[&str]) -> Result<Narrowing, String> {
    const LAST: &str = "`--last` needs a number of sessions, as in `--last 5`";
    const SINCE: &str = "`--since` needs a UTC day, as in `--since 2026-09-01`";
    let mut narrowing = Narrowing::default();
    while let Some((flag, after)) = rest.split_first() {
        let (name, value, after) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(value), after),
            None => match after.split_first() {
                Some((value, after)) => (*flag, Some(*value), after),
                None => (*flag, None, after),
            },
        };
        match name {
            "--last" => {
                let last = value.and_then(|n| n.parse().ok()).filter(|n| *n > 0);
                narrowing.last = Some(last.ok_or(LAST)?);
            }
            "--since" => narrowing.since = Some(value.and_then(day).ok_or(SINCE)?),
            _ => return Err(format!("unexpected argument `{flag}`")),
        }
        rest = after;
    }
    Ok(narrowing)
}

/// `niobe -p <prompt> [--json]`: the prompt is one argument, as a shell
/// quotes it, so that a prompt is never split into flags it happens to
/// contain.
fn print(rest: &[&str], json: bool) -> Result<Command, String> {
    const NO_PROMPT: &str = "`-p` needs a prompt, as in `niobe -p \"fix the failing test\"`";
    let (prompt, rest) = rest.split_first().ok_or(NO_PROMPT)?;
    if prompt.trim().is_empty() {
        return Err(NO_PROMPT.to_owned());
    }
    let json = match rest {
        [] => json,
        ["--json"] if !json => true,
        [extra, ..] => return Err(format!("unexpected argument `{extra}`")),
    };
    Ok(Command::Print {
        prompt: (*prompt).to_owned(),
        json,
    })
}

/// `niobe bench <suite> [--json] [--timeout <seconds>]`.
fn bench(rest: &[&str]) -> Result<Command, String> {
    const NO_SUITE: &str = "`bench` needs a suite directory, as in `niobe bench bench/`";
    const NO_TIMEOUT: &str = "`--timeout` needs a whole number of seconds, as in `--timeout 600`";
    let (timeout, rest) = take_flag(rest, "--timeout", NO_TIMEOUT)?;
    let timeout = timeout
        .map(|secs| match secs.parse::<u64>() {
            Ok(secs) if secs > 0 => Ok(std::time::Duration::from_secs(secs)),
            _ => Err(NO_TIMEOUT.to_owned()),
        })
        .transpose()?;
    let mut json = false;
    let mut suite = None;
    for arg in rest {
        match arg {
            "--json" if !json => json = true,
            path if suite.is_none() && !path.starts_with('-') => suite = Some(PathBuf::from(path)),
            extra => return Err(format!("unexpected argument `{extra}`")),
        }
    }
    Ok(Command::Bench {
        suite: suite.ok_or(NO_SUITE)?,
        json,
        timeout,
    })
}

/// `niobe cost`: a format flag, at most one, among the overview's narrowing.
fn cost(rest: &[&str]) -> Result<Command, String> {
    let mut format = Format::Table;
    let mut others = Vec::with_capacity(rest.len());
    for arg in rest {
        let named = match *arg {
            "--json" => Format::Json,
            "--csv" => Format::Csv,
            other => {
                others.push(other);
                continue;
            }
        };
        if format != Format::Table {
            return Err("`niobe cost` prints one format: `--json` or `--csv`".to_owned());
        }
        format = named;
    }
    Ok(Command::Cost {
        narrowing: narrowing(&others)?,
        format,
    })
}

/// A day written `YYYY-MM-DD`, and only one that exists.
fn day(text: &str) -> Option<Date> {
    let mut parts = text.splitn(3, '-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    Date::new(year, month, day)
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

    #[cfg(unix)]
    #[test]
    fn an_argument_that_is_not_utf8_is_named_as_well_as_it_reads() {
        use std::os::unix::ffi::OsStringExt;

        let args = vec![
            OsString::from("--resume"),
            OsString::from_vec(b"a\xffb".to_vec()),
        ];
        assert_eq!(
            utf8(args),
            Err(
                "the argument `a\u{fffd}b` is not UTF-8, and niobe reads only UTF-8 arguments"
                    .to_owned()
            )
        );
        assert_eq!(
            utf8([OsString::from("replay"), OsString::from("log.jsonl")]),
            Ok(vec!["replay".to_owned(), "log.jsonl".to_owned()])
        );
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
        assert_eq!(parsed(&["trust"]), Ok(Command::Trust { yes: false }));
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
    fn trust_agrees_without_asking_only_where_yes_is_given() {
        assert_eq!(
            parsed(&["trust", "--yes"]),
            Ok(Command::Trust { yes: true })
        );
        assert_eq!(
            parsed(&["trust", "-y"]),
            Err("unexpected argument `-y`".to_owned())
        );
        assert_eq!(
            parsed(&["untrust", "--yes"]),
            Err("unexpected argument `--yes`".to_owned())
        );
    }

    #[test]
    fn the_overview_is_narrowed_to_the_newest_sessions_or_those_since_a_day() {
        let day = Date::new(2026, 9, 1).expect("a real day");
        for args in [
            &["overview", "--last", "5", "--since", "2026-09-01"][..],
            &["overview", "--since=2026-09-01", "--last=5"][..],
        ] {
            assert_eq!(
                parsed(args),
                Ok(Command::Overview(Narrowing {
                    last: Some(5),
                    since: Some(day)
                }))
            );
        }
        for wrong in [
            &["overview", "--last"][..],
            &["overview", "--last", "0"][..],
            &["overview", "--last", "five"][..],
            &["overview", "--since", "2026-02-30"][..],
            &["overview", "--since", "yesterday"][..],
        ] {
            assert!(parsed(wrong).is_err(), "{wrong:?}");
        }
    }

    #[test]
    fn allow_gives_a_headless_run_its_rules_and_nothing_else_takes_one() {
        let ran = invocation(&[
            "-p",
            "fix it",
            "--allow",
            "Edit",
            "--allow=Bash(cargo test)",
        ])
        .expect("rules for a headless turn");
        assert_eq!(
            ran.allow,
            vec![
                Rule::parse("Edit").expect("a rule"),
                Rule::parse("Bash(cargo test)").expect("a rule"),
            ]
        );
        assert!(invocation(&["--allow", "Edit"]).is_err(), "not the shell");
        assert!(invocation(&["-p", "fix it", "--allow"]).is_err());
        let copied = invocation(&["-p", "fix it", "--allow", "Bash(git status:*)"])
            .expect_err("Claude Code's own form is refused");
        assert!(copied.contains("Bash(git status *)"), "{copied}");
    }

    #[test]
    fn terse_is_switched_on_or_off_for_a_session_or_a_turn() {
        let on = invocation(&["-p", "fix it", "--terse", "on"]).expect("a terse turn");
        assert_eq!(on.terse, Some(true));
        let off = invocation(&["bench", "b", "--terse=off"]).expect("a bench not terse");
        assert_eq!(off.terse, Some(false));
        assert_eq!(invocation(&[]).map(|i| i.terse), Ok(None));
        assert!(invocation(&["--terse", "on"]).is_ok(), "the shell takes it");
        assert!(invocation(&["-p", "x", "--terse", "yes"]).is_err());
        assert!(invocation(&["sessions", "--terse", "on"]).is_err());
    }

    #[test]
    fn the_trim_hook_is_read_only_in_the_one_form_the_cli_is_given() {
        let os = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();

        assert_eq!(
            hook_trim(os(&["hook", "trim", "--raw", "/r/.niobe/trim"])),
            Some(PathBuf::from("/r/.niobe/trim"))
        );
        assert_eq!(hook_trim(os(&["hook", "trim"])), None);
        assert_eq!(
            hook_trim(os(&["hook", "trim", "--raw", "/d", "extra"])),
            None
        );
        assert_eq!(hook_trim(os(&["hook", "tidy", "--raw", "/d"])), None);
        assert_eq!(hook_trim(os(&["-p", "hook", "trim", "--raw"])), None);
    }

    #[test]
    fn a_bench_names_its_suite_and_may_give_each_turn_a_deadline() {
        assert_eq!(
            parsed(&["bench", "bench/", "--json", "--timeout", "60"]),
            Ok(Command::Bench {
                suite: PathBuf::from("bench/"),
                json: true,
                timeout: Some(std::time::Duration::from_secs(60)),
            })
        );
        assert_eq!(
            parsed(&["bench", "suite"]),
            Ok(Command::Bench {
                suite: PathBuf::from("suite"),
                json: false,
                timeout: None,
            })
        );
        for wrong in [
            &["bench"][..],
            &["bench", "a", "b"][..],
            &["bench", "a", "--timeout", "0"][..],
            &["bench", "a", "--timeout", "soon"][..],
        ] {
            assert!(parsed(wrong).is_err(), "{wrong:?}");
        }
        let flagged = invocation(&["bench", "a", "--profile", "api", "--budget", "1"])
            .expect("a bench runs each turn under a profile and a budget");
        assert_eq!(flagged.profile.as_deref(), Some("api"));
        assert_eq!(flagged.budget, Some(1.0));
    }

    #[test]
    fn a_headless_turn_takes_its_prompt_whole_and_json_either_side_of_it() {
        let print = |prompt: &str, json| {
            Ok(Command::Print {
                prompt: prompt.to_owned(),
                json,
            })
        };
        assert_eq!(parsed(&["-p", "fix it"]), print("fix it", false));
        assert_eq!(
            parsed(&["--print", "fix it", "--json"]),
            print("fix it", true)
        );
        assert_eq!(parsed(&["--json", "-p", "fix it"]), print("fix it", true));
        assert_eq!(
            parsed(&["-p", "--budget is not a flag in here"]),
            print("--budget is not a flag in here", false),
            "a prompt is one argument, whatever it says"
        );
        for wrong in [
            &["-p"][..],
            &["-p", "  "][..],
            &["-p", "fix it", "again"][..],
            &["--json", "-p", "fix it", "--json"][..],
        ] {
            assert!(parsed(wrong).is_err(), "{wrong:?}");
        }
        let budgeted = invocation(&["-p", "fix it", "--budget", "0.5", "--profile", "work"])
            .expect("a headless turn takes a budget and a profile");
        assert_eq!(budgeted.budget, Some(0.5));
        assert_eq!(budgeted.profile.as_deref(), Some("work"));
        assert!(invocation(&["-p", "fix it", "--theme", "neo"]).is_err());
    }

    #[test]
    fn the_cost_takes_one_format_among_the_overview_narrowing() {
        assert_eq!(
            parsed(&["cost"]),
            Ok(Command::Cost {
                narrowing: Narrowing::default(),
                format: Format::Table
            })
        );
        assert_eq!(
            parsed(&["cost", "--last", "2", "--csv"]),
            Ok(Command::Cost {
                narrowing: Narrowing {
                    last: Some(2),
                    since: None
                },
                format: Format::Csv
            })
        );
        assert_eq!(
            parsed(&["cost", "--json"]).map(|command| matches!(
                command,
                Command::Cost {
                    format: Format::Json,
                    ..
                }
            )),
            Ok(true)
        );
        for wrong in [
            &["cost", "--json", "--csv"][..],
            &["cost", "--json", "--json"][..],
            &["cost", "--markdown"][..],
            &["cost", "--last"][..],
        ] {
            assert!(parsed(wrong).is_err(), "{wrong:?}");
        }
    }

    #[test]
    fn subcommands_take_nothing_extra() {
        assert_eq!(parsed(&["sessions"]), Ok(Command::Sessions));
        assert_eq!(
            parsed(&["overview"]),
            Ok(Command::Overview(Narrowing::default()))
        );
        assert!(parsed(&["overview", "--all"]).is_err());
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
                theme: None,
                terse: None,
                allow: Vec::new(),
            })
        );
        assert_eq!(profile(&["--profile=work"]), Ok(Some("work".to_owned())));
        assert_eq!(
            invocation(&["--resume", "3", "--profile", "work"]),
            Ok(Invocation {
                command: Command::Resume(id("3")),
                profile: Some("work".to_owned()),
                budget: None,
                theme: None,
                terse: None,
                allow: Vec::new(),
            })
        );
        assert_eq!(
            parsed(&["profiles", "--profile=work"]),
            Ok(Command::Profiles)
        );
        assert_eq!(profile(&[]), Ok(None));
    }

    #[test]
    fn doctor_takes_json_and_nothing_else() {
        let command = parsed;

        assert_eq!(command(&["doctor"]), Ok(Command::Doctor { json: false }));
        assert_eq!(
            command(&["doctor", "--json"]),
            Ok(Command::Doctor { json: true })
        );
        assert!(command(&["doctor", "--yes"]).is_err());
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
                theme: None,
                terse: None,
                allow: Vec::new(),
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
    fn the_theme_flag_names_a_palette_either_way_round_and_ignores_its_case() {
        use niobe_tui::NEO;

        assert_eq!(
            invocation(&["--theme", "neo"]).map(|i| i.theme),
            Ok(Some(NEO))
        );
        assert_eq!(invocation(&["--theme=NEO"]).map(|i| i.theme), Ok(Some(NEO)));
        assert_eq!(
            invocation(&["--resume", "3", "--theme", "neo"]).map(|i| i.theme),
            Ok(Some(NEO))
        );
        assert_eq!(invocation(&[]).map(|i| i.theme), Ok(None));
    }

    #[test]
    fn a_theme_flag_that_names_no_palette_lists_the_ones_there_are() {
        for args in [
            &["--theme"][..],
            &["--theme="],
            &["--theme", "matrix"],
            &["--theme", "--budget", "1"],
        ] {
            let error = invocation(args).expect_err("not a theme");
            assert!(
                error.contains("--theme") || error.contains("not a theme"),
                "{args:?}: {error}"
            );
            assert!(error.contains("`classic`"), "{args:?}: {error}");
            assert!(error.contains("`neo`"), "{args:?}: {error}");
        }
        assert!(invocation(&["--theme", "neo", "--theme=classic"]).is_err());
    }

    #[test]
    fn a_theme_applies_only_where_a_session_runs() {
        for args in [
            &["--theme", "neo", "sessions"][..],
            &["prices", "--theme=neo"],
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

    #[test]
    fn help_after_a_subcommand_is_the_help() {
        for args in [
            &["sessions", "--help"][..],
            &["replay", "log.jsonl", "-h"],
            &["prices", "--help"],
        ] {
            assert_eq!(parsed(args), Ok(Command::Help), "{args:?}");
        }
    }

    #[test]
    fn a_negative_budget_is_refused_by_its_value_not_read_as_a_missing_one() {
        let said = invocation(&["--budget", "-1"]).expect_err("a negative budget");
        assert!(said.contains("positive") && said.contains("`-1`"), "{said}");
    }

    #[test]
    fn a_theme_applies_to_a_replay_too() {
        let asked = invocation(&["replay", "log.jsonl", "--theme", "neo"])
            .expect("the command line in this test is valid");
        assert!(asked.theme.is_some());
    }

    #[test]
    fn an_id_of_digits_is_never_looked_up_as_a_claude_session() {
        for id in ["99999999999999999999999", "+3", "-5"] {
            let said = parsed(&["--resume", id]).expect_err("not a session number");
            assert!(said.contains(id) && said.contains("session"), "{said}");
        }
    }
}
