// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! `niobe bench`: a suite of tasks, each run as one `niobe -p` turn in a copy
//! of the repository the task starts from, and judged by the task's own check
//! — with what each turn spent, how long it took and where it left the plan's
//! five-hour window, as a table or as JSON in the shape
//! `schema/bench.schema.json` publishes.
//!
//! A task is a directory of the suite holding `prompt.md`, the prompt; a
//! `files/` directory, the repository the turn starts in; and `check.sh`, run
//! with `sh` in the repository the turn left, which passes the task by
//! exiting 0. The check is run from the suite rather than from the copy, so
//! the agent cannot rewrite what judges it, and what it reads beside itself —
//! hidden tests — is out of the agent's reach too.
//!
//! Each turn is its own `niobe -p` process, so a bench measures exactly what
//! a job running `-p` would get, and a turn that never ends is ended at the
//! deadline without the bench having to reach into a backend. Nothing here is
//! an estimate: the figures are the ones each turn's `--json` printed.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use crate::cost::rfc3339;
use crate::summary::grouped;

/// The version of the JSON's shape, which `schema/bench.schema.json` names.
pub const SCHEMA_VERSION: u64 = 1;

/// How long a task's turn may run when `--timeout` does not say: long
/// enough for a task a model works through in a few minutes, short enough
/// that a turn waiting on nothing does not hold a whole suite up for long.
pub const TURN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// How long a task's check may run. A check is a test run over a small
/// repository; one that takes longer is stuck rather than slow.
const CHECK_TIMEOUT: Duration = Duration::from_secs(120);

/// How often a running process is looked at.
const POLL: Duration = Duration::from_millis(20);

/// A technique `--ab` measures: the suite run without it, then with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Technique {
    /// The terse instruction a new conversation is given with `--terse on`.
    Terse,
}

impl Technique {
    /// The technique `name` calls, as `--ab` takes it.
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "terse" => Some(Self::Terse),
            _ => None,
        }
    }

    /// What `--ab` calls it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Terse => "terse",
        }
    }

    /// The flag each turn is given to have it on or off.
    pub fn flag(self) -> &'static str {
        match self {
            Self::Terse => "--terse",
        }
    }
}

/// One task of a suite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// The task's directory name, which is what it is called.
    pub name: String,
    /// The task's directory.
    dir: PathBuf,
    /// What its turn is asked.
    prompt: String,
}

/// How a task's turn ended, as the bench saw it.
#[derive(Debug, Clone, PartialEq)]
pub enum Ended {
    /// `niobe -p` ended and printed its report, which is the outcome.
    Reported(Value),
    /// The turn ran past its deadline and was ended.
    TimedOut,
    /// `niobe -p` ended without a report: what it wrote last on standard
    /// error, which is where it says why.
    Unreported(String),
}

/// One task, run.
#[derive(Debug, Clone, PartialEq)]
pub struct Ran {
    /// The task's name.
    pub name: String,
    /// Whether its check passed.
    pub passed: bool,
    /// How its turn ended.
    pub ended: Ended,
    /// How long its turn took, by the bench's own clock.
    pub wall: Duration,
    /// The copy the turn worked in, where it was kept to be looked at: a
    /// task that did not pass keeps its copy, one that passed does not.
    pub kept: Option<PathBuf>,
}

/// The tasks of the suite at `suite`, in the order of their names: every
/// directory under it that holds a `prompt.md`.
///
/// Each task's directory is absolute, since its check runs in the copy the
/// turn worked in rather than where the suite was named from.
pub fn tasks(suite: &Path) -> Result<Vec<Task>, String> {
    let unreadable = |e: io::Error| format!("cannot read the suite at {}: {e}", suite.display());
    let suite = suite.canonicalize().map_err(unreadable)?;
    let entries = fs::read_dir(&suite).map_err(unreadable)?;
    let mut tasks = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let prompt = dir.join("prompt.md");
        if !dir.is_dir() || !prompt.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        for needed in ["files", "check.sh"] {
            if !dir.join(needed).exists() {
                return Err(format!("the task `{name}` has no {needed}"));
            }
        }
        let text = fs::read_to_string(&prompt)
            .map_err(|e| format!("cannot read {}: {e}", prompt.display()))?;
        tasks.push(Task {
            name,
            dir,
            prompt: prompt_of(&text),
        });
    }
    if tasks.is_empty() {
        return Err(format!(
            "{} holds no task: a task is a directory with prompt.md, files/ and check.sh",
            suite.display()
        ));
    }
    tasks.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(tasks)
}

/// The prompt a `prompt.md` asks: its text, less the comment block a file in
/// a published tree opens with, which is the file's and not the task's.
fn prompt_of(text: &str) -> String {
    let text = text.trim_start();
    let text = match text
        .strip_prefix("<!--")
        .and_then(|rest| rest.split_once("-->"))
    {
        Some((_, after)) => after,
        None => text,
    };
    text.trim().to_owned()
}

/// Runs `task` as one turn of the `niobe` at `exe`, with `flags` — the
/// profile, budget and rules the bench was given — and judges it by its
/// check.
///
/// The turn may also edit and write files inside its copy, but not under the
/// dot-named entries at its root, and nowhere else: a task is a change to
/// that copy, and a bench nobody can leave running
/// would be one whose every edit waits on a prompt nobody is there to answer.
///
/// `side` tells this run's copies from another's in the same bench, so that
/// an `--ab` run's second pass does not clear a copy its first one kept.
pub fn run(exe: &Path, task: &Task, flags: &[String], timeout: Duration, side: &str) -> Ran {
    let work = scratch(&format!("{side}{}", task.name));
    let started = Instant::now();
    let laid_out = lay_out(&task.dir.join("files"), &work).and_then(|()| work.canonicalize());
    let ended = match laid_out {
        Ok(copy) => {
            let mut flags = flags.to_vec();
            for tool in ["Edit", "Write"] {
                for rule in inside(tool, &copy) {
                    flags.extend(["--allow".to_owned(), rule]);
                }
            }
            turn(exe, &work, &task.prompt, &flags, timeout)
        }
        Err(e) => Ended::Unreported(format!("the task's files could not be laid out: {e}")),
    };
    let wall = started.elapsed();
    let passed = !matches!(ended, Ended::TimedOut) && checks(&task.dir.join("check.sh"), &work);
    let kept = match passed {
        true => {
            let _ = fs::remove_dir_all(&work);
            None
        }
        false => Some(work),
    };
    Ran {
        name: task.name.clone(),
        passed,
        ended,
        wall,
        kept,
    }
}

/// The rules that let `tool` act on any path under `dir` but those under a
/// dot-named entry at its root, as the backend names a path: absolute, with
/// the links in `dir` resolved, which is why `dir` is the canonical path
/// rather than the temporary directory as named.
///
/// The dot-named entries are what the turn runs with rather than what it is
/// asked to change: the CLI's project settings, skills and agents under
/// `.claude/`, its MCP servers in `.mcp.json`, the copy's `.git/` and
/// Niobe's own `.niobe/`. The CLI reads its settings files again when they
/// change mid-session, so a turn allowed to write `.claude/settings.local.json`
/// could allow itself a shell, which would reach past the copy. A rule cannot
/// leave a name out, so there is one for each first character a name not
/// starting with a dot may have here; a name starting with anything else is
/// asked about, and refused, as a path outside the copy is.
fn inside(tool: &str, dir: &Path) -> Vec<String> {
    ('a'..='z')
        .chain('A'..='Z')
        .chain('0'..='9')
        .chain(['_'])
        .map(|first| format!("{tool}({}/{first}*)", dir.display()))
        .collect()
}

/// A directory of its own for `name`'s copy, outside any repository, emptied
/// of what an earlier bench left there.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("niobe-bench-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// Copies the directory `from` to `to`, made a repository of its own so that
/// the turn's session store and every `git` the agent runs stay inside it.
///
/// Only directories and regular files are copied: a link in a task's files
/// would reach out of the copy, and the turn's edits through it with it.
fn lay_out(from: &Path, to: &Path) -> io::Result<()> {
    copy_tree(from, to)?;
    let initialised = Command::new("git")
        .args(["init", "-q"])
        .current_dir(to)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !initialised.is_ok_and(|status| status.success()) {
        // Without `git` the copy is still marked as a repository's root, so
        // that the turn's store is kept in it rather than in a parent.
        fs::create_dir_all(to.join(".git"))?;
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)?;
        } else {
            return Err(io::Error::other(format!(
                "{} is neither a file nor a directory",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

/// Runs one `niobe -p` turn in `work` and reads its report.
fn turn(exe: &Path, work: &Path, prompt: &str, flags: &[String], timeout: Duration) -> Ended {
    let spawned = Command::new(exe)
        .args(["-p", prompt, "--json"])
        .args(flags)
        .current_dir(work)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => return Ended::Unreported(format!("niobe could not be started: {e}")),
    };
    // Read while it runs: a report larger than the pipe holds would otherwise
    // hold the turn up on its last write.
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let status = ended_by(&mut child, timeout);
    let (stdout, stderr) = (joined(stdout), joined(stderr));
    match status {
        None => Ended::TimedOut,
        Some(_) => match serde_json::from_slice::<Value>(&stdout) {
            Ok(report) if report.is_object() => Ended::Reported(report),
            _ => Ended::Unreported(last_line(&stderr)),
        },
    }
}

/// Whether the check at `check` passes in `work`: `sh` runs it there and
/// exits 0 within its deadline.
fn checks(check: &Path, work: &Path) -> bool {
    Command::new("sh")
        .arg(check)
        .current_dir(work)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
        .and_then(|mut child| ended_by(&mut child, CHECK_TIMEOUT))
        .is_some_and(|status| status.success())
}

/// How `child` ended, or `None` where it ran past `timeout` and was killed.
///
/// A `niobe` killed here takes its backend with it: the reaper it starts ends
/// every process group it was told of once the pipe the killed process held
/// to it closes.
///
/// A `timeout` too long for the clock to hold — `--timeout` takes any whole
/// number of seconds — is no deadline at all, rather than a panic.
fn ended_by(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now().checked_add(timeout);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if deadline.is_none_or(|deadline| Instant::now() < deadline) => {
                std::thread::sleep(POLL)
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> Option<JoinHandle<Vec<u8>>> {
    pipe.map(|mut pipe| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            bytes
        })
    })
}

fn joined(reader: Option<JoinHandle<Vec<u8>>>) -> Vec<u8> {
    reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default()
}

/// The last line `niobe` wrote on standard error, without its `niobe: `.
fn last_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().rev().find(|line| !line.trim().is_empty());
    match line {
        Some(line) => line.strip_prefix("niobe: ").unwrap_or(line).to_owned(),
        None => "niobe ended without a report or a reason".to_owned(),
    }
}

/// The word the JSON calls how a turn ended by.
fn outcome(ended: &Ended) -> &str {
    match ended {
        Ended::Reported(report) => report["outcome"].as_str().unwrap_or("failed"),
        Ended::TimedOut => "timed_out",
        Ended::Unreported(_) => "unreported",
    }
}

fn reason(ended: &Ended, timeout: Duration) -> Option<String> {
    match ended {
        Ended::Reported(report) => report["reason"].as_str().map(str::to_owned),
        Ended::TimedOut => Some(format!(
            "the turn ran past its {} and was ended",
            minutes(timeout)
        )),
        Ended::Unreported(why) => Some(why.clone()),
    }
}

fn report(ended: &Ended) -> Option<&Value> {
    match ended {
        Ended::Reported(report) => Some(report),
        Ended::TimedOut | Ended::Unreported(_) => None,
    }
}

/// Every token a turn spent, where it reported its spend.
fn tokens(ran: &Ran) -> Option<u64> {
    report(&ran.ended).and_then(|report| report["tokens"]["total"].as_u64())
}

/// The tokens a turn's model wrote, where it reported its spend.
fn output_tokens(ran: &Ran) -> Option<u64> {
    report(&ran.ended).and_then(|report| report["tokens"]["output"].as_u64())
}

/// The sum of `of` over every turn, where every turn reported it.
fn reported_sum(ran: &[Ran], of: impl Fn(&Ran) -> Option<u64>) -> Option<u64> {
    ran.iter()
        .map(of)
        .try_fold(0u64, |sum, n| n.map(|n| sum.saturating_add(n)))
}

/// Where the five-hour window stood when a turn ended, where its backend
/// reported it.
fn five_hour(ran: &Ran) -> Option<f64> {
    report(&ran.ended).and_then(|report| report["windows"]["five_hour"].as_f64())
}

/// The suite's results in the shape `schema/bench.schema.json` publishes.
pub fn document(suite: &str, started_at: SystemTime, ran: &[Ran], timeout: Duration) -> Value {
    let passed = ran.iter().filter(|ran| ran.passed).count();
    let reported: Vec<u64> = ran.iter().filter_map(tokens).collect();
    let windows: Vec<f64> = ran.iter().filter_map(five_hour).collect();
    json!({
        "schema_version": SCHEMA_VERSION,
        "suite": suite,
        "started_at": rfc3339(started_at),
        "timeout_seconds": timeout.as_secs(),
        "tasks": ran.iter().map(|ran| task_json(ran, timeout)).collect::<Vec<_>>(),
        "summary": {
            "tasks": ran.len(),
            "passed": passed,
            "pass_rate": share(passed, ran.len()),
            "tokens": (reported.len() == ran.len()).then(|| reported.iter().sum::<u64>()),
            "tokens_reported_by": reported.len(),
            "output_tokens": reported_sum(ran, output_tokens),
            "wall_ms": millis(ran.iter().map(|ran| ran.wall).sum()),
            "five_hour": {
                "first": windows.first(),
                "last": windows.last(),
            },
        },
    })
}

/// An `--ab` run's results: the suite without `technique`, then with it,
/// each in the shape [`document`] gives, and how far each figure moved — a
/// fraction of the first run's, `null` where either run left it unreported
/// or the first was zero.
pub fn ab_document(technique: Technique, without: Value, with: Value) -> Value {
    let moved = |key: &str| change(&without["summary"][key], &with["summary"][key]);
    let passed = |document: &Value| document["summary"]["passed"].as_i64().unwrap_or(0);
    json!({
        "schema_version": SCHEMA_VERSION,
        "technique": technique.name(),
        "change": {
            "passed": passed(&with) - passed(&without),
            "output_tokens": moved("output_tokens"),
            "tokens": moved("tokens"),
            "wall_ms": moved("wall_ms"),
        },
        "without": without,
        "with": with,
    })
}

/// How far `to` moved from `from`, as a fraction of `from`.
fn change(from: &Value, to: &Value) -> Option<f64> {
    let (from, to) = (from.as_u64()?, to.as_u64()?);
    (from > 0).then(|| (to as f64 - from as f64) / from as f64)
}

/// An `--ab` run's results as tables to read: each run's, then the two side
/// by side.
pub fn ab_table(
    technique: Technique,
    without: &[Ran],
    with: &[Ran],
    timeout: Duration,
) -> Vec<String> {
    let name = technique.name();
    let mut lines = vec![format!("Without {name}"), String::new()];
    lines.extend(table(without, timeout));
    lines.extend([String::new(), format!("With {name}"), String::new()]);
    lines.extend(table(with, timeout));
    lines.push(String::new());
    lines.push(format!(
        "{:<14}  {:>12}  {:>12}  {:>8}",
        "", "WITHOUT", "WITH", "CHANGE"
    ));
    let passed = |ran: &[Ran]| ran.iter().filter(|ran| ran.passed).count() as u64;
    let wall = |ran: &[Ran]| millis(ran.iter().map(|ran| ran.wall).sum());
    for (label, from, to) in [
        ("passed", Some(passed(without)), Some(passed(with))),
        (
            "output tokens",
            reported_sum(without, output_tokens),
            reported_sum(with, output_tokens),
        ),
        (
            "tokens",
            reported_sum(without, tokens),
            reported_sum(with, tokens),
        ),
        ("wall ms", Some(wall(without)), Some(wall(with))),
    ] {
        let figure = |n: Option<u64>| n.map_or_else(|| "—".to_owned(), grouped);
        let moved = match (from, to) {
            (Some(from), Some(to)) if from > 0 => {
                let percent = (to as f64 - from as f64) / from as f64 * 100.0;
                format!("{percent:+.1}%").replace('-', "−")
            }
            _ => "—".to_owned(),
        };
        lines.push(format!(
            "{label:<14}  {:>12}  {:>12}  {moved:>8}",
            figure(from),
            figure(to)
        ));
    }
    lines
}

fn task_json(ran: &Ran, timeout: Duration) -> Value {
    json!({
        "name": ran.name,
        "passed": ran.passed,
        "outcome": outcome(&ran.ended),
        "reason": reason(&ran.ended, timeout),
        "wall_ms": millis(ran.wall),
        "kept": ran.kept.as_ref().map(|dir| dir.display().to_string()),
        "turn": report(&ran.ended),
    })
}

fn share(part: usize, whole: usize) -> Option<f64> {
    (whole > 0).then(|| part as f64 / whole as f64)
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The suite's results as a table to read.
pub fn table(ran: &[Ran], timeout: Duration) -> Vec<String> {
    let width = ran
        .iter()
        .map(|ran| ran.name.chars().count())
        .max()
        .unwrap_or(0)
        .max("TASK".len());
    let mut lines = vec![format!(
        "{:<width$}  {:<6}  {:<14}  {:>12}  {:>8}",
        "TASK", "CHECK", "TURN", "TOKENS", "TIME"
    )];
    for ran in ran {
        lines.push(format!(
            "{:<width$}  {:<6}  {:<14}  {:>12}  {:>8}",
            ran.name,
            if ran.passed { "pass" } else { "fail" },
            outcome(&ran.ended),
            tokens(ran).map_or_else(|| "—".to_owned(), grouped),
            clock(ran.wall),
        ));
        if let Some(why) = reason(&ran.ended, timeout) {
            lines.push(format!("{:width$}  {why}", ""));
        }
        if let Some(kept) = &ran.kept {
            lines.push(format!("{:width$}  kept at {}", "", kept.display()));
        }
    }
    lines.push(String::new());
    lines.push(summary_line(ran));
    lines
}

fn summary_line(ran: &[Ran]) -> String {
    let passed = ran.iter().filter(|ran| ran.passed).count();
    let percent = share(passed, ran.len()).map_or(0.0, |share| share * 100.0);
    let reported: Vec<u64> = ran.iter().filter_map(tokens).collect();
    let tokens = match reported.len() == ran.len() {
        true => format!("{} tokens", grouped(reported.iter().sum())),
        false => format!(
            "≥{} tokens ({} of {} turns reported)",
            grouped(reported.iter().sum()),
            reported.len(),
            ran.len()
        ),
    };
    let windows: Vec<f64> = ran.iter().filter_map(five_hour).collect();
    let window = match (windows.first(), windows.last()) {
        (Some(first), Some(last)) => format!(
            " · five-hour window {:.0}% after the first turn, {:.0}% after the last",
            first * 100.0,
            last * 100.0
        ),
        _ => String::new(),
    };
    format!(
        "{passed} of {} passed ({percent:.0}%) · {tokens} · {}{window}",
        ran.len(),
        clock(ran.iter().map(|ran| ran.wall).sum())
    )
}

/// A duration as minutes and seconds.
fn clock(duration: Duration) -> String {
    let secs = duration.as_secs();
    match secs {
        0..=59 => format!("{secs}s"),
        _ => format!("{}m {:02}s", secs / 60, secs % 60),
    }
}

fn minutes(duration: Duration) -> String {
    let secs = duration.as_secs();
    match secs % 60 {
        0 => format!("{} minutes", secs / 60),
        _ => format!("{secs} seconds"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_is_its_file_less_the_header_the_file_opens_with() {
        let file = "<!--\nSPDX-License-Identifier: Apache-2.0\n-->\n\nFix the bug.\n";
        assert_eq!(prompt_of(file), "Fix the bug.");
        assert_eq!(prompt_of("Just this."), "Just this.");
    }

    #[test]
    fn a_deadline_past_what_the_clock_can_hold_is_no_deadline() {
        let mut child = Command::new("true")
            .spawn()
            .expect("`true` is on every unix PATH");
        let status = ended_by(&mut child, Duration::from_secs(u64::MAX));
        assert!(status.is_some_and(|status| status.success()), "{status:?}");
    }

    #[test]
    fn a_duration_reads_in_seconds_then_minutes() {
        assert_eq!(clock(Duration::from_secs(42)), "42s");
        assert_eq!(clock(Duration::from_secs(185)), "3m 05s");
        assert_eq!(minutes(Duration::from_secs(900)), "15 minutes");
        assert_eq!(minutes(Duration::from_secs(2)), "2 seconds");
    }

    #[test]
    fn a_turn_that_never_reported_leaves_the_suites_tokens_a_floor() {
        let ran = |name: &str, total: Option<u64>| Ran {
            name: name.to_owned(),
            passed: total.is_some(),
            ended: match total {
                Some(total) => Ended::Reported(json!({
                    "outcome": "finished",
                    "reason": null,
                    "tokens": {"total": total},
                    "windows": {"five_hour": 0.4, "seven_day": null},
                })),
                None => Ended::TimedOut,
            },
            wall: Duration::from_secs(61),
            kept: None,
        };
        let suite = [ran("a", Some(1_200)), ran("b", None)];
        let document = document("demo", SystemTime::UNIX_EPOCH, &suite, TURN_TIMEOUT);
        assert_eq!(document["summary"]["tokens"], Value::Null);
        assert_eq!(document["summary"]["tokens_reported_by"], 1);
        assert_eq!(document["summary"]["pass_rate"], 0.5);
        assert_eq!(
            summary_line(&suite),
            "1 of 2 passed (50%) · ≥1,200 tokens (1 of 2 turns reported) · 2m 02s · \
             five-hour window 40% after the first turn, 40% after the last"
        );
    }
}
