// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What kind of work a tool call was: exploring the code, editing files,
//! building and testing, and so on, with the programs and tools that did it.
//!
//! A count of calls by tool name says little where most of them go to one
//! shell tool: `Bash 73` is the agent reading, editing, building and
//! committing alike. Read from the command each call ran, the same calls say
//! what the agent was doing. The kind is a label put on a measured count of
//! calls, and every call is put under exactly one.
//!
//! A tool that is not a shell is put under the kind its name says — `Read`
//! explores, `Edit` edits, `Agent` delegates — and a tool an MCP server
//! provides under that server. A shell command is read as
//! [`crate::test_run`] reads one: split into its simple commands where `&&`,
//! `||`, `;`, `|` or a newline separate them, with nothing inside quotes, a
//! `$(…)` or a here-document's body read as a command. Each is named by its
//! program's base name, after `cd`, `NAME=value` assignments and the wrappers
//! that only run the command after them (`env`, `time`, `sudo`, `timeout`,
//! `rtk`); a subcommand is added where it decides the kind — `git log` reads,
//! `git commit` does not, and `cargo test` is named as such — and `-i` where
//! `sed` or `perl` edits in place.
//!
//! A command of several parts goes under the strongest kind among them:
//! editing, then version control, then building and testing, then the web,
//! then exploring, and a program nothing here knows last. Output saved to a
//! file — `>`, `>>`, `tee` — is an edit where what it saved only printed
//! something, and part of the work where it was a build's, a commit's or a
//! download's: `cargo test > log` is a test run. A script handed to an
//! interpreter inline — `python3 - <<EOF`, `node -e` — is named `script`,
//! since what it does is not in the command line.

use std::collections::BTreeSet;

use crate::event::OPERATOR_SHELL;
use crate::test_run::{RESERVED, Wrapper, is_assignment, simple_commands};

/// A kind of work, which the Activity pane counts calls under.
///
/// Ordered as the pane lists kinds that tie on calls.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Work {
    /// Reading the code: files, searches, listings, read-only `git`.
    Exploring,
    /// Changing files.
    Editing,
    /// Compiling, testing, linting, formatting.
    Building,
    /// Commits, branches, pushes, pull requests.
    VersionControl,
    /// Fetching from or searching the web.
    Web,
    /// Handing work to a sub-agent.
    Delegating,
    /// The tools of one MCP server, named as a person reads it: `Notion`.
    Server(String),
    /// The agent's own bookkeeping: finding a tool, keeping a to-do list,
    /// scheduling itself.
    Housekeeping,
    /// A command or a tool none of the above recognises. Never a guess at
    /// one of them: its own names are what is shown for it.
    Other,
}

impl Work {
    /// What the kind is called on screen: sixteen columns at most, so that
    /// a pane's column of kinds is never cut.
    pub fn label(&self) -> &str {
        match self {
            Self::Exploring => "Exploring code",
            Self::Editing => "Editing files",
            Self::Building => "Build and test",
            Self::VersionControl => "Version control",
            Self::Web => "Web",
            Self::Delegating => "Delegating",
            Self::Server(name) => name,
            Self::Housekeeping => "Housekeeping",
            Self::Other => "Other",
        }
    }

    /// How strongly a part of a shell command says what the whole was doing:
    /// the part that ranks highest names the command's kind.
    fn rank(&self) -> u8 {
        match self {
            Self::Editing => 6,
            Self::VersionControl => 5,
            Self::Building => 4,
            Self::Web => 3,
            Self::Exploring => 2,
            Self::Other => 1,
            Self::Delegating | Self::Server(_) | Self::Housekeeping => 0,
        }
    }
}

/// What one call was: its kind, and the programs or tools that did it, each
/// once however many times the call ran it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// The kind of work it is counted under.
    pub work: Work,
    /// What did it, in the order the command named them: `grep`, `git log`,
    /// `sed -i`, `cargo test`, or the tool's own name. Never empty.
    pub specifics: Vec<String>,
}

/// The kind of work a call to `tool` was.
///
/// `command` is the whole shell command the call ran, where it ran one. A
/// record kept before calls carried it has only the call's `input`, which for
/// the operator's `!` commands is the command, and its one-line `summary`,
/// which for the session's own shell tool is the command's first line; those
/// are read instead.
pub fn of_call(tool: &str, command: Option<&str>, summary: Option<&str>, input: &str) -> Reading {
    if let Some(command) = command {
        return of_command(command);
    }
    if let Some(reading) = of_tool(tool) {
        return reading;
    }
    match (tool, summary) {
        (OPERATOR_SHELL, _) => of_command(input),
        ("Bash", Some(summary)) => of_command(summary.trim_end_matches('…').trim_end()),
        _ => Reading {
            work: Work::Other,
            specifics: vec![tool.to_owned()],
        },
    }
}

/// A tool that is not a shell, by its name.
fn of_tool(tool: &str) -> Option<Reading> {
    if let Some((server, name)) = mcp_tool(tool) {
        return Some(Reading {
            work: Work::Server(server),
            specifics: vec![name],
        });
    }
    let work = match tool {
        "Read" | "Grep" | "Glob" | "LS" | "NotebookRead" => Work::Exploring,
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => Work::Editing,
        "WebFetch" | "WebSearch" => Work::Web,
        "Agent" | "Task" => Work::Delegating,
        "ToolSearch" | "TodoWrite" | "TodoRead" | "ScheduleWakeup" | "BashOutput" | "KillShell"
        | "KillBash" | "TaskOutput" | "TaskStop" | "ExitPlanMode" | "EnterPlanMode" | "Skill"
        | "SlashCommand" | "AskUserQuestion" => Work::Housekeeping,
        _ => return None,
    };
    Some(Reading {
        work,
        specifics: vec![tool.to_owned()],
    })
}

/// An MCP tool's server and tool, as a person reads them.
///
/// An MCP tool arrives as `mcp__<server>__<tool>`, and the server as the
/// client registered it (`claude_ai_Notion`). It reads as the server's last
/// word, and the tool with the server's name taken off its front where it
/// repeats it: `Notion` and `search`.
pub fn mcp_tool(name: &str) -> Option<(String, String)> {
    let (server, tool) = name.strip_prefix("mcp__")?.split_once("__")?;
    let server = server.rsplit('_').next().unwrap_or(server);
    let prefix = format!("{}-", server.to_lowercase());
    let tool = match tool.to_lowercase().starts_with(&prefix) {
        true => tool.get(prefix.len()..).unwrap_or(tool),
        false => tool,
    };
    Some((server.to_owned(), tool.to_owned()))
}

/// The kind of work a shell command was.
pub fn of_command(command: &str) -> Reading {
    let parts: Vec<Part> = simple_commands(command)
        .iter()
        .filter_map(|words| part(words))
        .collect();
    let strongest = parts
        .iter()
        .filter(|part| !part.quiet)
        .map(|part| &part.work)
        .max_by_key(|work| work.rank())
        .cloned();
    let saved = parts.iter().any(|part| part.saves);
    let work = match strongest {
        Some(work @ (Work::Editing | Work::VersionControl | Work::Building | Work::Web)) => work,
        _ if saved => Work::Editing,
        Some(work) => work,
        None => Work::Other,
    };
    let mut seen = BTreeSet::new();
    let named: Vec<&Part> = match parts.iter().any(|part| !part.quiet) {
        true => parts.iter().filter(|part| !part.quiet).collect(),
        false => parts.iter().collect(),
    };
    let mut specifics: Vec<String> = named
        .into_iter()
        .filter(|part| seen.insert(part.name.clone()))
        .map(|part| part.name.clone())
        .collect();
    if specifics.is_empty() {
        specifics.push("shell".to_owned());
    }
    Reading { work, specifics }
}

/// One simple command of a shell command, read.
#[derive(Debug)]
struct Part {
    /// The program, with the subcommand or option that decides its kind.
    name: String,
    work: Work,
    /// Whether it says nothing about the work — `cd`, `echo` — and is named
    /// only where nothing else in the command is.
    quiet: bool,
    /// Whether it saves output to a file: `> file`, `>> file`, `tee file`.
    saves: bool,
}

/// The simple command `words`, read, or `None` where it runs no program.
fn part(words: &[String]) -> Option<Part> {
    let saves = words.iter().enumerate().any(|(at, word)| {
        redirect_target(word, words.get(at + 1).map(String::as_str)).is_some_and(is_a_file)
    });
    let words = program_words(words)?;
    let (program, rest) = words.split_first()?;
    let program = program.rsplit('/').next().unwrap_or(program);
    let (name, work, quiet) = classify(program, rest);
    Some(Part {
        saves: saves || program == "tee",
        name,
        work,
        quiet,
    })
}

/// The words from the program on: past the shell's own words, assignments
/// and the wrappers that run the command after them as it is.
fn program_words(words: &[String]) -> Option<Vec<&str>> {
    let mut words = words
        .iter()
        .map(String::as_str)
        .filter(|word| !is_redirection(word))
        .skip_while(|word| RESERVED.contains(word))
        .peekable();
    loop {
        let word = *words.peek()?;
        let base = word.rsplit('/').next().unwrap_or(word);
        if is_assignment(word) {
            words.next();
        } else if base == "rtk" {
            words.next();
            words.next_if(|word| *word == "proxy");
        } else if let Some(wrapper) = Wrapper::named(base) {
            words.next();
            if !wrapper.skip_its_own(&mut words) {
                return None;
            }
        } else {
            break;
        }
    }
    Some(words.collect())
}

/// What `program`, run with `rest`, is named and counted as, and whether it
/// is quiet.
fn classify(program: &str, rest: &[&str]) -> (String, Work, bool) {
    let named = |work| (program.to_owned(), work, false);
    match program {
        "cd" | "pushd" | "popd" | "echo" | "printf" | "true" | "false" | ":" | "sleep"
        | "export" | "set" | "unset" | "source" | "." | "exit" | "wait" | "clear" => {
            (program.to_owned(), Work::Other, true)
        }
        "git" => git(rest),
        "gh" | "hub" | "glab" => (
            with_subcommand(program, rest, &[]),
            Work::VersionControl,
            false,
        ),
        "svn" | "hg" => named(Work::VersionControl),
        "cargo" => {
            let rest: Vec<&str> = rest
                .iter()
                .copied()
                .skip_while(|word| word.starts_with('+'))
                .collect();
            (
                with_subcommand(program, &rest, &["--color", "--config", "-Z", "-C"]),
                Work::Building,
                false,
            )
        }
        "npm" | "pnpm" | "yarn" | "bun" | "go" | "dotnet" | "swift" | "mix" | "uv" | "poetry" => {
            (with_subcommand(program, rest, &[]), Work::Building, false)
        }
        "make" | "cmake" | "ninja" | "bazel" | "mvn" | "gradle" | "gradlew" | "pytest" | "tox"
        | "jest" | "vitest" | "tsc" | "rustc" | "rustfmt" | "clippy-driver" | "gcc" | "g++"
        | "clang" | "clang++" | "javac" | "shellcheck" | "eslint" | "prettier" | "ruff"
        | "mypy" | "black" | "xcodebuild" => named(Work::Building),
        "sed" | "perl" if in_place(rest) => (format!("{program} -i"), Work::Editing, false),
        "python" | "python3" | "node" | "perl" | "ruby" | "bash" | "sh" | "zsh" | "deno"
        | "osascript" => match inline_script(rest) {
            true => ("script".to_owned(), Work::Other, false),
            false => named(Work::Other),
        },
        "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" | "find" | "fd" | "ls" | "tree"
        | "cat" | "head" | "tail" | "less" | "more" | "wc" | "sed" | "awk" | "jq" | "yq"
        | "sort" | "uniq" | "cut" | "tr" | "diff" | "cmp" | "file" | "stat" | "du" | "df"
        | "pwd" | "which" | "whereis" | "realpath" | "readlink" | "dirname" | "basename"
        | "xxd" | "od" | "hexdump" | "strings" | "column" | "nl" | "bat" | "date" | "env"
        | "printenv" | "uname" | "ps" | "lsof" | "shasum" | "sha256sum" | "md5" | "md5sum" => {
            named(Work::Exploring)
        }
        "mv" | "cp" | "rm" | "mkdir" | "rmdir" | "touch" | "ln" | "chmod" | "chown" | "patch"
        | "truncate" | "install" | "rsync" | "unzip" | "tar" => named(Work::Editing),
        // What `tee` saves is an edit only where nothing stronger made it: see
        // `Part::saves`.
        "tee" => named(Work::Other),
        "curl" | "wget" | "http" | "https" => named(Work::Web),
        _ => named(Work::Other),
    }
}

/// `git` and its subcommand, which decides whether it only read.
fn git(rest: &[&str]) -> (String, Work, bool) {
    let name = with_subcommand("git", rest, &["-C", "-c", "--git-dir", "--work-tree"]);
    let reads = matches!(
        name.strip_prefix("git ").unwrap_or_default(),
        "log"
            | "diff"
            | "status"
            | "show"
            | "blame"
            | "grep"
            | "ls-files"
            | "ls-tree"
            | "rev-parse"
            | "describe"
            | "shortlog"
            | "reflog"
            | "cat-file"
            | "whatchanged"
    );
    let work = match reads {
        true => Work::Exploring,
        false => Work::VersionControl,
    };
    (name, work, false)
}

/// `program` and the first word after its options, where there is one:
/// `git log`, `cargo test`, `npm run`. `takes_a_value` are the options
/// before it that are followed by a value of their own.
fn with_subcommand(program: &str, rest: &[&str], takes_a_value: &[&str]) -> String {
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        if takes_a_value.contains(word) {
            words.next();
        } else if !word.starts_with('-') {
            return format!("{program} {word}");
        }
    }
    program.to_owned()
}

/// Whether `sed` or `perl` is told to edit its files in place: `-i`, `-i ''`,
/// `-i.bak`, `--in-place`, or `-i` among other single-letter options (`-pi`).
fn in_place(rest: &[&str]) -> bool {
    rest.iter().any(|word| {
        word.starts_with("--in-place")
            || (word.starts_with('-')
                && !word.starts_with("--")
                && word
                    .trim_start_matches('-')
                    .split('.')
                    .next()
                    .is_some_and(|flags| {
                        flags.contains('i') && flags.chars().all(|c| c.is_ascii_alphabetic())
                    }))
    })
}

/// Whether an interpreter is handed its script inline rather than by a file:
/// `-c`, `-e`, or `-` with a here-document.
fn inline_script(rest: &[&str]) -> bool {
    rest.iter().any(|word| {
        matches!(
            *word,
            "-c" | "-e" | "-" | "<<" | "<<-" | "-ne" | "-pe" | "-lne"
        )
    })
}

/// Whether `word` is a redirection the program never sees: `2>&1`, `>`,
/// `>>`, `<`, `>file`, `<<` and the like.
fn is_redirection(word: &str) -> bool {
    let rest = word.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
    rest.starts_with('>') || rest.starts_with('<')
}

/// Where `word` sends the command's standard output, where it is a
/// redirection of it: the rest of the word, or `next` where the word is the
/// operator alone. `None` for one of standard error alone, or of input.
fn redirect_target<'a>(word: &'a str, next: Option<&'a str>) -> Option<&'a str> {
    let rest = match word.strip_prefix('&') {
        Some(rest) => rest,
        None => word.strip_prefix('1').unwrap_or(word),
    };
    let target = rest
        .strip_prefix(">>")
        .or_else(|| rest.strip_prefix(">|"))
        .or_else(|| rest.strip_prefix('>'))?;
    match target.is_empty() {
        true => next,
        false => Some(target),
    }
}

/// Whether a redirection's target is a file, rather than another descriptor
/// or a device that keeps nothing.
fn is_a_file(target: &str) -> bool {
    !target.starts_with('&') && !target.starts_with("/dev/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(command: &str) -> (Work, Vec<String>) {
        let reading = of_command(command);
        (reading.work, reading.specifics)
    }

    fn named(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    /// Commands as agents wrote them in this repository's own sessions.
    #[test]
    fn commands_from_real_sessions_are_put_under_the_work_they_did() {
        let cases: &[(&str, Work, &[&str])] = &[
            (
                "cd crates && grep -n 'fn tool_rows' niobe-tui/src/ui.rs | head -5",
                Work::Exploring,
                &["grep", "head"],
            ),
            (
                "sed -n 1,80p crates/niobe-core/src/session.rs",
                Work::Exploring,
                &["sed"],
            ),
            (
                "sed -i '' -e 's/old/new/' crates/niobe-tui/src/app.rs",
                Work::Editing,
                &["sed -i"],
            ),
            ("git status --short", Work::Exploring, &["git status"]),
            (
                "git commit -m \"fix: count a reply; and say so\"",
                Work::VersionControl,
                &["git commit"],
            ),
            ("cargo test --workspace", Work::Building, &["cargo test"]),
            (
                "rtk cargo clippy --workspace --all-targets -- -D warnings",
                Work::Building,
                &["cargo clippy"],
            ),
            (
                "python3 - <<'EOF'\nimport os\nos.remove('x')\nEOF",
                Work::Other,
                &["script"],
            ),
            (
                "jq -c '.type' crates/fixtures/a.jsonl",
                Work::Exploring,
                &["jq"],
            ),
        ];
        for (command, work, specifics) in cases {
            assert_eq!(read(command), (work.clone(), named(specifics)), "{command}");
        }
    }

    #[test]
    fn a_compound_command_goes_under_its_strongest_kind_and_names_every_program() {
        let cases: &[(&str, Work, &[&str])] = &[
            (
                "git add a.rs && git commit -m x && git push",
                Work::VersionControl,
                &["git add", "git commit", "git push"],
            ),
            (
                "cargo fmt --all && git diff --stat",
                Work::Building,
                &["cargo fmt", "git diff"],
            ),
            (
                "mkdir -p out && cargo build && mv target/x out/",
                Work::Editing,
                &["mkdir", "cargo build", "mv"],
            ),
            (
                "curl -s https://example.com | jq .",
                Work::Web,
                &["curl", "jq"],
            ),
            ("ls -la | wc -l", Work::Exploring, &["ls", "wc"]),
            (
                "grep -rl foo . | python3 -c 'import sys'",
                Work::Exploring,
                &["grep", "script"],
            ),
        ];
        for (command, work, specifics) in cases {
            assert_eq!(read(command), (work.clone(), named(specifics)), "{command}");
        }
    }

    #[test]
    fn wrappers_assignments_and_paths_are_read_past_to_the_program() {
        let cases: &[(&str, Work, &[&str])] = &[
            (
                "/usr/bin/env cargo test -p niobe-core",
                Work::Building,
                &["cargo test"],
            ),
            (
                "RUST_LOG=debug timeout 60 cargo +nightly build",
                Work::Building,
                &["cargo build"],
            ),
            ("sudo -u me ls /root", Work::Exploring, &["ls"]),
            (
                "git -C crates log --oneline -3",
                Work::Exploring,
                &["git log"],
            ),
            ("./target/debug/niobe --version", Work::Other, &["niobe"]),
            ("time make", Work::Building, &["make"]),
        ];
        for (command, work, specifics) in cases {
            assert_eq!(read(command), (work.clone(), named(specifics)), "{command}");
        }
    }

    #[test]
    fn output_saved_to_a_file_is_an_edit_unless_it_is_a_builds_or_a_downloads() {
        let cases: &[(&str, Work, &[&str])] = &[
            ("cat > notes.txt <<EOF\nhello\nEOF", Work::Editing, &["cat"]),
            ("echo done >> log.txt", Work::Editing, &["echo"]),
            ("echo done > /dev/null", Work::Other, &["echo"]),
            ("ls 2>/dev/null", Work::Exploring, &["ls"]),
            (
                "cargo test 2>&1 | tee /tmp/run.txt",
                Work::Building,
                &["cargo test", "tee"],
            ),
            (
                "cargo test > /tmp/run.txt 2>&1",
                Work::Building,
                &["cargo test"],
            ),
            (
                "curl -o page.html https://example.com",
                Work::Web,
                &["curl"],
            ),
        ];
        for (command, work, specifics) in cases {
            assert_eq!(read(command), (work.clone(), named(specifics)), "{command}");
        }
    }

    #[test]
    fn in_place_edits_and_inline_scripts_are_told_apart_from_reading_and_running() {
        let cases: &[(&str, Work, &[&str])] = &[
            ("perl -pi -e 's/a/b/' f.rs", Work::Editing, &["perl -i"]),
            ("sed -i.bak s/a/b/ f.rs", Work::Editing, &["sed -i"]),
            ("sed --in-place s/a/b/ f.rs", Work::Editing, &["sed -i"]),
            ("sed -n '/fn/p' f.rs", Work::Exploring, &["sed"]),
            ("node -e 'console.log(1)'", Work::Other, &["script"]),
            ("python3 scripts/check.py", Work::Other, &["python3"]),
        ];
        for (command, work, specifics) in cases {
            assert_eq!(read(command), (work.clone(), named(specifics)), "{command}");
        }
    }

    #[test]
    fn a_command_of_nothing_but_quiet_programs_names_them_under_other() {
        assert_eq!(read("cd /tmp"), (Work::Other, named(&["cd"])));
        assert_eq!(read("echo ---"), (Work::Other, named(&["echo"])));
        assert_eq!(
            read("cd crates && echo hi && grep x y"),
            (Work::Exploring, named(&["grep"]))
        );
        assert_eq!(read(""), (Work::Other, named(&["shell"])));
    }

    #[test]
    fn a_tool_that_is_not_a_shell_is_put_under_what_its_name_says() {
        let cases: &[(&str, Work, &str)] = &[
            ("Read", Work::Exploring, "Read"),
            ("Glob", Work::Exploring, "Glob"),
            ("Edit", Work::Editing, "Edit"),
            ("Write", Work::Editing, "Write"),
            ("WebFetch", Work::Web, "WebFetch"),
            ("Agent", Work::Delegating, "Agent"),
            ("ToolSearch", Work::Housekeeping, "ToolSearch"),
            ("ScheduleWakeup", Work::Housekeeping, "ScheduleWakeup"),
            (
                "mcp__claude_ai_Notion__notion-fetch",
                Work::Server("Notion".to_owned()),
                "fetch",
            ),
            ("SomethingNew", Work::Other, "SomethingNew"),
        ];
        for (tool, work, specific) in cases {
            let reading = of_call(tool, None, None, "{}");
            assert_eq!(
                (reading.work, reading.specifics),
                (work.clone(), named(&[specific])),
                "{tool}"
            );
        }
    }

    #[test]
    fn a_shell_call_is_read_from_its_command_or_from_the_line_an_older_record_kept() {
        let reading = of_call("Bash", Some("cd x\ncargo build"), Some("cd x …"), "{}");
        assert_eq!(reading.work, Work::Building);

        let older = of_call("Bash", None, Some("git status …"), "{}");
        assert_eq!(
            (older.work, older.specifics),
            (Work::Exploring, named(&["git status"]))
        );
        let operator = of_call(OPERATOR_SHELL, None, None, "git log");
        assert_eq!(operator.work, Work::Exploring);
    }
}
