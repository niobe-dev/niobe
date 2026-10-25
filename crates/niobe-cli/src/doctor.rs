// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! `niobe doctor`: what a first session needs, one row each, and the fix for
//! what is missing.
//!
//! The rows are a function of [`Facts`], which are gathered once and apart, so
//! that what each row says is tested without a machine that has, or lacks, a
//! CLI. Login state is asked of the CLIs' own status commands and never read
//! from the files they keep it in.
//!
//! The report is meant to be pasted into an issue: no path under the home
//! directory is shown beyond `~`, and no value of a profile's environment, a
//! token or an account name is read, so none can be printed.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long a CLI's version or status command is given. A status command
/// that has not answered by then is reported as not answering, rather than
/// held on to.
const PROBE_DEADLINE: Duration = Duration::from_secs(5);

/// Where to read how to install the `claude` CLI.
const CLAUDE_INSTALL: &str = "https://docs.claude.com/en/docs/claude-code/setup";

/// How a row stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Nothing to do.
    Ok,
    /// Works, and something the operator should know.
    Warn,
    /// A session cannot run until it is fixed.
    Fail,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// What was checked.
    pub name: &'static str,
    /// How it stands.
    pub status: Status,
    /// What was found.
    pub detail: String,
    /// What to do about it, for a row that is not [`Status::Ok`].
    pub fix: Option<String>,
}

impl Row {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Ok,
            detail: detail.into(),
            fix: None,
        }
    }

    fn problem(
        name: &'static str,
        status: Status,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            name,
            status,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

/// Whether a CLI says it is signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Login {
    /// Signed in, by the way the CLI's own status names.
    SignedIn(String),
    /// The CLI says it is not.
    SignedOut,
    /// The status command could not be run or read, and why.
    Unknown(String),
}

/// A CLI found on `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    /// The file `PATH` found.
    pub path: PathBuf,
    /// What `--version` printed first, where it ran.
    pub version: Result<String, String>,
    /// Whether the release is one the recordings cover, for the CLI that has
    /// recordings.
    pub recorded: Option<bool>,
    /// Whether it is signed in.
    pub login: Login,
}

/// A config file looked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFile {
    /// Where it is, or would be.
    pub path: PathBuf,
    /// Whether it exists.
    pub exists: bool,
}

/// Everything the report is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// This binary's version.
    pub niobe: &'static str,
    /// The `claude` CLI, where `PATH` has one.
    pub claude: Option<Tool>,
    /// The `codex` CLI, where `PATH` has one.
    pub codex: Option<Tool>,
    /// The config files looked in, the user's first.
    pub config: Vec<ConfigFile>,
    /// The repository's config, where it sets what only a trusted file may
    /// and has not been trusted.
    pub untrusted: Option<PathBuf>,
    /// `TERM`.
    pub term: Option<String>,
    /// `COLORTERM`.
    pub colorterm: Option<String>,
    /// Columns and rows of the terminal standard output is, where it is one.
    pub size: Option<(u16, u16)>,
    /// Whether the session is inside tmux.
    pub tmux: bool,
    /// Whether the session is over SSH.
    pub ssh: bool,
    /// The home directory, which no path is shown under: as `HOME` spells
    /// it, and as it resolves where that is another path. A path read back
    /// from the system, as the working directory is, names the home directory
    /// the second way where `HOME` goes through a link.
    pub homes: Vec<PathBuf>,
    /// The operating system and architecture this binary was built for.
    pub target: String,
    /// What each config file sets, by key and never by value.
    pub shapes: Vec<Shape>,
    /// How many errors the newest recorded session reported, where there is
    /// one and it could be read.
    pub newest_session_errors: Option<u64>,
}

/// What one config file sets: the keys, never the values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    /// Where the file is.
    pub path: PathBuf,
    /// What was found there.
    pub state: ShapeState,
}

/// How a config file reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShapeState {
    /// There is no such file.
    Absent,
    /// It is there and does not read as a config. Why is not kept: the
    /// reason quotes the text it stopped at.
    Unreadable,
    /// It reads, and sets these.
    Set {
        /// The top-level keys it sets.
        keys: Vec<&'static str>,
        /// The profiles it defines, each with the keys it sets.
        profiles: Vec<ProfileShape>,
    },
}

/// One profile of a config file, by key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileShape {
    /// The name the file gives it.
    pub name: String,
    /// The backend it runs.
    pub backend: String,
    /// Which of `env`, `args`, `models`, `billing`, `settings` and
    /// `auth_refresh` it sets.
    pub keys: Vec<&'static str>,
}

/// The keys `config` sets, and which each profile sets, with no value.
fn shape_of(config: &niobe_config::Config) -> ShapeState {
    let mut keys = Vec::new();
    if config.select(None).ok().flatten().is_some() {
        keys.push("default_profile");
    }
    if !config.allowed().is_empty() {
        keys.push("permissions");
    }
    if config.theme().is_some() {
        keys.push("theme");
    }
    if !config.keys().is_empty() {
        keys.push("keys");
    }
    let profiles = config
        .profiles()
        .iter()
        .map(|(name, profile)| {
            let set = [
                ("env", !profile.env().is_empty()),
                ("args", !profile.args().is_empty()),
                ("models", !profile.models().is_empty()),
                ("billing", profile.billing().is_some()),
                ("settings", profile.settings().is_some()),
                ("auth_refresh", profile.auth_refresh().is_some()),
            ];
            ProfileShape {
                name: name.clone(),
                backend: profile.backend().to_string(),
                keys: set
                    .into_iter()
                    .filter_map(|(key, set)| set.then_some(key))
                    .collect(),
            }
        })
        .collect();
    ShapeState::Set { keys, profiles }
}

/// The report for `facts`, in the order it is read.
pub fn rows(facts: &Facts) -> Vec<Row> {
    let mut rows = vec![Row::ok("niobe", facts.niobe)];
    rows.extend(claude_rows(facts));
    rows.extend(codex_rows(facts));
    rows.extend(config_rows(facts));
    rows.push(terminal_row(facts));
    rows
}

fn claude_rows(facts: &Facts) -> Vec<Row> {
    let Some(tool) = &facts.claude else {
        return vec![Row::problem(
            "claude",
            Status::Fail,
            "not on PATH",
            format!(
                "install Claude Code ({CLAUDE_INSTALL}), then sign in with `claude auth login`"
            ),
        )];
    };
    let shown = shown_path(&tool.path, &facts.homes);
    let mut rows = Vec::new();
    match &tool.version {
        Ok(version) => {
            rows.push(Row::ok("claude", format!("{shown} {version}")));
            rows.push(match tool.recorded {
                Some(false) => Row::problem(
                    "claude release",
                    Status::Warn,
                    format!("{version} is not a release Niobe's reading was recorded from"),
                    "check the figures against `claude`'s own; anything unrecognised is reported \
                     in the session rather than counted",
                ),
                Some(true) => Row::ok("claude release", format!("{version} was recorded")),
                None => Row::ok("claude release", "not checked"),
            });
        }
        Err(why) => rows.push(Row::problem(
            "claude",
            Status::Fail,
            format!("{shown} did not report a version: {why}"),
            "run `claude --version` in this terminal and fix what it says",
        )),
    }
    rows.push(login_row("claude login", &tool.login, "claude auth login"));
    rows
}

fn codex_rows(facts: &Facts) -> Vec<Row> {
    let Some(tool) = &facts.codex else {
        return vec![Row::ok(
            "codex",
            "not on PATH; only the codex backend needs it",
        )];
    };
    let shown = shown_path(&tool.path, &facts.homes);
    let version = tool.version.clone().unwrap_or_else(|why| why);
    vec![
        Row::ok("codex", format!("{shown} {version}")),
        // A signed-out codex is a warning and not a failure: no backend of
        // Niobe's drives it yet, so no session waits on it.
        match &tool.login {
            Login::SignedOut => Row::problem(
                "codex login",
                Status::Warn,
                "not signed in",
                "sign in with `codex login`",
            ),
            other => login_row("codex login", other, "codex login"),
        },
    ]
}

fn login_row(name: &'static str, login: &Login, command: &str) -> Row {
    match login {
        Login::SignedIn(how) => Row::ok(name, format!("signed in ({how})")),
        Login::SignedOut => Row::problem(
            name,
            Status::Fail,
            "not signed in",
            format!("sign in with `{command}`"),
        ),
        Login::Unknown(why) => Row::problem(
            name,
            Status::Warn,
            format!("could not tell: {why}"),
            format!("run `{command}` if the first prompt asks you to sign in"),
        ),
    }
}

fn config_rows(facts: &Facts) -> Vec<Row> {
    let mut rows: Vec<Row> = facts
        .config
        .iter()
        .map(|file| {
            let shown = shown_path(&file.path, &facts.homes);
            let detail = match file.exists {
                true => shown,
                false => format!("{shown} (not there)"),
            };
            Row::ok("config", detail)
        })
        .collect();
    if !facts.config.iter().any(|file| file.exists) {
        rows.push(Row::ok(
            "profile",
            "no config file, so the implied `claude` profile runs",
        ));
    }
    if let Some(path) = &facts.untrusted {
        rows.push(Row::problem(
            "trust",
            Status::Warn,
            format!(
                "{} sets what only a trusted file may, and is not trusted",
                shown_path(path, &facts.homes)
            ),
            "`niobe trust` shows what it sets, values included, and asks before it puts it in force",
        ));
    }
    rows
}

fn terminal_row(facts: &Facts) -> Row {
    let term = facts.term.as_deref().unwrap_or("unset");
    let mut parts = vec![format!("TERM={term}")];
    if let Some(colorterm) = &facts.colorterm {
        parts.push(format!("COLORTERM={colorterm}"));
    }
    if let Some((columns, rows)) = facts.size {
        parts.push(format!("{columns}x{rows}"));
    }
    if facts.tmux {
        parts.push("tmux".to_owned());
    }
    if facts.ssh {
        parts.push("ssh".to_owned());
    }
    let detail = parts.join(" · ");
    match term {
        "unset" | "dumb" => Row::problem(
            "terminal",
            Status::Warn,
            detail,
            "the shell needs a terminal that draws with escape sequences; run it from one",
        ),
        _ => Row::ok("terminal", detail),
    }
}

/// `path`, with the home directory it is under, by any of the spellings in
/// `homes`, written as `~`.
fn shown_path(path: &Path, homes: &[PathBuf]) -> String {
    let under = homes
        .iter()
        .filter(|home| !home.as_os_str().is_empty())
        .find_map(|home| path.strip_prefix(home).ok());
    match under {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The home directory `home` names, as written and, where it goes through a
/// link, as it resolves.
fn homes(home: Option<PathBuf>) -> Vec<PathBuf> {
    let Some(home) = home.filter(|home| !home.as_os_str().is_empty()) else {
        return Vec::new();
    };
    let resolved = std::fs::canonicalize(&home)
        .ok()
        .filter(|resolved| *resolved != home);
    std::iter::once(home).chain(resolved).collect()
}

/// The report as the terminal reads it: a row each, the fix under the row it
/// is for.
pub fn text(rows: &[Row]) -> Vec<String> {
    let width = rows.iter().map(|row| row.name.len()).max().unwrap_or(0);
    let mut lines = Vec::new();
    for row in rows {
        lines.push(format!(
            "{:<4}  {:<width$}  {}",
            row.status.word(),
            row.name,
            row.detail
        ));
        if let Some(fix) = &row.fix {
            lines.push(format!("{:<4}  {:<width$}  fix: {fix}", "", ""));
        }
    }
    lines
}

/// The report as JSON, for an issue: the rows, and the facts a maintainer
/// would otherwise ask for — the target, the shapes of the config files and
/// how the newest session fared.
pub fn json(rows: &[Row], facts: &Facts) -> Value {
    let homes = &facts.homes;
    let shapes: Vec<Value> = facts
        .shapes
        .iter()
        .map(|shape| {
            let path = shown_path(&shape.path, homes);
            match &shape.state {
                ShapeState::Absent => json!({"path": path, "state": "absent"}),
                ShapeState::Unreadable => json!({"path": path, "state": "does not parse"}),
                ShapeState::Set { keys, profiles } => json!({
                    "path": path,
                    "state": "set",
                    "keys": keys,
                    "profiles": profiles.iter().map(|profile| json!({
                        "name": profile.name,
                        "backend": profile.backend,
                        "keys": profile.keys,
                    })).collect::<Vec<_>>(),
                }),
            }
        })
        .collect();
    json!({
        "niobe": niobe_core::VERSION,
        "target": facts.target,
        "ok": rows.iter().all(|row| row.status != Status::Fail),
        "config": shapes,
        "newest_session_errors": facts.newest_session_errors,
        "rows": rows.iter().map(|row| json!({
            "name": row.name,
            "status": row.status.word(),
            "detail": row.detail,
            "fix": row.fix,
        })).collect::<Vec<_>>(),
    })
}

/// Whether the report has a row that stops a session.
pub fn failed(rows: &[Row]) -> bool {
    rows.iter().any(|row| row.status == Status::Fail)
}

/// What `command` printed and whether it exited successfully, or why it could
/// not be run. Started with no terminal and given [`PROBE_DEADLINE`].
fn probe(program: &Path, args: &[&str]) -> Result<(bool, String), String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait().map_err(|error| error.to_string())? {
            Some(status) => break status,
            None if started.elapsed() > PROBE_DEADLINE => {
                // Already gone is as good as killed.
                let _ = child.kill();
                let _ = child.wait();
                return Err("it did not answer in time".to_owned());
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    let mut out = String::new();
    if let Some(stdout) = child.stdout.take() {
        // A status line is short; anything past a mebibyte is not one.
        let _ = stdout.take(1 << 20).read_to_string(&mut out);
    }
    Ok((status.success(), out))
}

/// The first file named `name` in a directory of `path`, as a shell would find
/// a command.
fn find_on_path(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn version_of(path: &Path) -> Result<String, String> {
    match probe(path, &["--version"])? {
        (true, out) => out
            .lines()
            .next()
            .map(|line| line.trim().to_owned())
            .filter(|line| !line.is_empty())
            .ok_or_else(|| "it printed nothing".to_owned()),
        (false, _) => Err("it exited with an error".to_owned()),
    }
}

/// The `claude` CLI's own account of how it is signed in, from
/// `claude auth status`, which names a method and a provider and nothing a
/// person is called by.
fn claude_login(path: &Path) -> Login {
    match probe(path, &["auth", "status", "--json"]) {
        Err(why) => Login::Unknown(why),
        Ok((_, out)) => match serde_json::from_str::<Value>(&out) {
            Err(_) => Login::Unknown("its status was not JSON".to_owned()),
            Ok(status) => match status.get("loggedIn").and_then(Value::as_bool) {
                Some(true) => {
                    let named = |key: &str| {
                        status
                            .get(key)
                            .and_then(Value::as_str)
                            .filter(|value| !value.is_empty())
                    };
                    let how: Vec<&str> = [named("authMethod"), named("apiProvider")]
                        .into_iter()
                        .flatten()
                        .collect();
                    Login::SignedIn(match how.is_empty() {
                        true => "as the CLI reports".to_owned(),
                        false => how.join(", "),
                    })
                }
                Some(false) => Login::SignedOut,
                None => Login::Unknown("its status did not say".to_owned()),
            },
        },
    }
}

/// `codex login status`, which exits successfully only where signed in.
fn codex_login(path: &Path) -> Login {
    match probe(path, &["login", "status"]) {
        Err(why) => Login::Unknown(why),
        Ok((true, _)) => Login::SignedIn("as the CLI reports".to_owned()),
        Ok((false, _)) => Login::SignedOut,
    }
}

/// What this machine and this repository say, gathered once.
pub fn gather(
    config: &crate::config::Loaded,
    root: &Path,
    recorded: impl Fn(&str) -> bool,
) -> Facts {
    let path_var = std::env::var_os("PATH");
    let claude = find_on_path("claude", path_var.as_deref()).map(|path| {
        let version = version_of(&path);
        // The version line reads `2.1.296 (Claude Code)`.
        let recorded = version
            .as_ref()
            .ok()
            .and_then(|line| line.split_whitespace().next())
            .map(&recorded);
        let login = claude_login(&path);
        Tool {
            path,
            version,
            recorded,
            login,
        }
    });
    let codex = find_on_path("codex", path_var.as_deref()).map(|path| Tool {
        version: version_of(&path),
        recorded: None,
        login: codex_login(&path),
        path,
    });
    Facts {
        niobe: niobe_core::VERSION,
        claude,
        codex,
        config: config
            .searched
            .iter()
            .map(|path| ConfigFile {
                path: path.clone(),
                exists: path.exists(),
            })
            .collect(),
        untrusted: config.untrusted.as_ref().map(|u| u.path.clone()),
        term: std::env::var("TERM").ok().filter(|term| !term.is_empty()),
        colorterm: std::env::var("COLORTERM")
            .ok()
            .filter(|term| !term.is_empty()),
        size: terminal_size(),
        tmux: std::env::var_os("TMUX").is_some(),
        ssh: std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some(),
        homes: homes(std::env::var_os("HOME").map(PathBuf::from)),
        target: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        shapes: config
            .searched
            .iter()
            .map(|path| Shape {
                path: path.clone(),
                state: match niobe_config::Config::read(path) {
                    Ok(None) => ShapeState::Absent,
                    Ok(Some(config)) => shape_of(&config),
                    Err(_) => ShapeState::Unreadable,
                },
            })
            .collect(),
        newest_session_errors: newest_session_errors(root),
    }
}

/// How many errors the newest session recorded in the repository at `root`
/// reported, where there is a store and a session in it.
fn newest_session_errors(root: &Path) -> Option<u64> {
    let store = crate::repo::read_existing_store(root).ok().flatten()?;
    let newest = store.sessions().ok()?.into_iter().next()?;
    let events = store.events(newest.id).ok()?;
    let state =
        niobe_core::session::SessionState::replay(events.iter().map(|stored| &stored.event));
    Some(state.errors())
}

/// The report as a block to paste into an issue, gathered now for the
/// repository at `root`: the same JSON `niobe doctor --json` prints.
pub fn block(root: &Path, recorded: impl Fn(&str) -> bool) -> String {
    match crate::config::load(root) {
        Ok(loaded) => {
            let facts = gather(&loaded, root, recorded);
            let rows = rows(&facts);
            serde_json::to_string_pretty(&json(&rows, &facts)).unwrap_or_else(|_| "{}".to_owned())
        }
        Err(_) => json!({
            "niobe": niobe_core::VERSION,
            "target": format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
            "config": "does not read",
        })
        .to_string(),
    }
}

fn terminal_size() -> Option<(u16, u16)> {
    let size = rustix::termios::tcgetwinsize(std::io::stdout()).ok()?;
    (size.ws_col > 0 && size.ws_row > 0).then_some((size.ws_col, size.ws_row))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(version: &str, login: Login) -> Tool {
        Tool {
            path: PathBuf::from("/home/me/.local/bin/claude"),
            version: Ok(version.to_owned()),
            recorded: Some(true),
            login,
        }
    }

    fn facts() -> Facts {
        Facts {
            niobe: "0.16.0",
            claude: Some(tool(
                "2.1.296 (Claude Code)",
                Login::SignedIn("third_party, bedrock".to_owned()),
            )),
            codex: None,
            config: vec![
                ConfigFile {
                    path: PathBuf::from("/home/me/.config/niobe/config.toml"),
                    exists: true,
                },
                ConfigFile {
                    path: PathBuf::from("/home/me/repo/.niobe/config.toml"),
                    exists: false,
                },
            ],
            untrusted: None,
            term: Some("xterm-256color".to_owned()),
            colorterm: Some("truecolor".to_owned()),
            size: Some((120, 40)),
            tmux: false,
            ssh: false,
            homes: vec![PathBuf::from("/home/me")],
            target: "aarch64-macos".to_owned(),
            shapes: Vec::new(),
            newest_session_errors: None,
        }
    }

    fn named<'a>(rows: &'a [Row], name: &str) -> &'a Row {
        rows.iter()
            .find(|row| row.name == name)
            .unwrap_or_else(|| panic!("no row named {name}: {rows:?}"))
    }

    #[test]
    fn a_machine_with_everything_has_no_row_that_is_not_ok() {
        let rows = rows(&facts());

        assert!(rows.iter().all(|row| row.status == Status::Ok), "{rows:?}");
        assert!(!failed(&rows));
    }

    #[test]
    fn a_machine_with_no_claude_is_told_to_install_it_and_sign_in() {
        let rows = rows(&Facts {
            claude: None,
            ..facts()
        });

        let claude = named(&rows, "claude");
        assert_eq!(claude.status, Status::Fail);
        assert_eq!(claude.detail, "not on PATH");
        let fix = claude.fix.as_deref().expect("a failure has a fix");
        assert!(fix.contains(CLAUDE_INSTALL), "{fix}");
        assert!(fix.contains("claude auth login"), "{fix}");
        assert!(failed(&rows));
    }

    #[test]
    fn a_claude_that_is_not_signed_in_names_the_command_that_signs_it_in() {
        let rows = rows(&Facts {
            claude: Some(tool("2.1.296 (Claude Code)", Login::SignedOut)),
            ..facts()
        });

        let login = named(&rows, "claude login");
        assert_eq!(login.status, Status::Fail);
        assert!(
            login
                .fix
                .as_deref()
                .is_some_and(|fix| fix.contains("`claude auth login`")),
            "{login:?}"
        );
    }

    #[test]
    fn a_release_nobody_recorded_is_a_warning_and_not_a_failure() {
        let mut unrecorded = tool("9.9.9 (Claude Code)", Login::SignedOut);
        unrecorded.login = Login::SignedIn("claude.ai".to_owned());
        unrecorded.recorded = Some(false);
        let rows = rows(&Facts {
            claude: Some(unrecorded),
            ..facts()
        });

        assert_eq!(named(&rows, "claude release").status, Status::Warn);
        assert!(!failed(&rows));
    }

    #[test]
    fn a_status_that_could_not_be_read_is_a_warning_because_the_cli_may_still_work() {
        let rows = rows(&Facts {
            claude: Some(tool(
                "2.1.296 (Claude Code)",
                Login::Unknown("it did not answer in time".to_owned()),
            )),
            ..facts()
        });

        assert_eq!(named(&rows, "claude login").status, Status::Warn);
        assert!(!failed(&rows));
    }

    #[test]
    fn an_absent_codex_is_fine_and_a_signed_out_one_is_a_warning() {
        let absent = rows(&facts());
        assert_eq!(named(&absent, "codex").status, Status::Ok);

        let signed_out = rows(&Facts {
            codex: Some(Tool {
                path: PathBuf::from("/usr/local/bin/codex"),
                version: Ok("codex-cli 0.146.0".to_owned()),
                recorded: None,
                login: Login::SignedOut,
            }),
            ..facts()
        });
        assert_eq!(named(&signed_out, "codex login").status, Status::Warn);
        assert!(!failed(&signed_out));
    }

    #[test]
    fn a_repository_config_that_is_not_trusted_says_how_to_read_it() {
        let rows = rows(&Facts {
            untrusted: Some(PathBuf::from("/home/me/repo/.niobe/config.toml")),
            ..facts()
        });

        let trust = named(&rows, "trust");
        assert_eq!(trust.status, Status::Warn);
        assert!(trust.detail.starts_with("~/repo/.niobe/config.toml"));
        assert!(
            trust
                .fix
                .as_deref()
                .is_some_and(|fix| fix.contains("`niobe trust`"))
        );
    }

    #[test]
    fn with_no_config_file_the_report_says_the_implied_profile_runs() {
        let mut none = facts();
        for file in &mut none.config {
            file.exists = false;
        }

        let rows = rows(&none);

        assert!(named(&rows, "profile").detail.contains("implied"));
    }

    #[test]
    fn a_terminal_that_is_dumb_or_unset_is_a_warning() {
        for term in [None, Some("dumb".to_owned())] {
            let rows = rows(&Facts { term, ..facts() });
            assert_eq!(named(&rows, "terminal").status, Status::Warn);
        }
    }

    #[test]
    fn the_terminal_row_says_tmux_and_ssh_and_the_size() {
        let rows = rows(&Facts {
            tmux: true,
            ssh: true,
            ..facts()
        });

        let detail = &named(&rows, "terminal").detail;
        assert_eq!(
            detail,
            "TERM=xterm-256color · COLORTERM=truecolor · 120x40 · tmux · ssh"
        );
    }

    #[test]
    fn no_path_under_the_home_directory_is_shown_beyond_a_tilde() {
        let rows = rows(&facts());
        let report = format!("{}\n{}", text(&rows).join("\n"), json(&rows, &facts()));

        assert!(!report.contains("/home/me"), "{report}");
        assert!(report.contains("~/.local/bin/claude"), "{report}");
        assert!(report.contains("~/.config/niobe/config.toml"), "{report}");
    }

    #[test]
    fn a_path_under_either_spelling_of_the_home_directory_is_shown_under_a_tilde() {
        let homes = [PathBuf::from("/home/me"), PathBuf::from("/usr/home/me")];
        assert_eq!(
            shown_path(Path::new("/usr/home/me/repo/.niobe/config.toml"), &homes),
            "~/repo/.niobe/config.toml"
        );
        assert_eq!(
            shown_path(Path::new("/home/me/bin/claude"), &homes),
            "~/bin/claude"
        );
        assert_eq!(shown_path(Path::new("/usr/home/me"), &homes), "~");
        assert_eq!(shown_path(Path::new("/opt/claude"), &homes), "/opt/claude");
    }

    #[cfg(unix)]
    #[test]
    fn a_home_directory_through_a_link_is_known_by_where_it_leads_too() {
        let scratch = tempfile::tempdir().expect("a temporary directory can be created");
        let real = std::fs::canonicalize(scratch.path()).expect("the directory resolves");
        let home = real.join("home");
        std::fs::create_dir(&home).expect("a home directory");
        let link = real.join("link");
        std::os::unix::fs::symlink(&home, &link).expect("a link to it");

        assert_eq!(homes(Some(link.clone())), vec![link, home.clone()]);
        assert_eq!(homes(Some(home.clone())), vec![home]);
        assert_eq!(homes(Some(PathBuf::new())), Vec::<PathBuf>::new());
        assert_eq!(homes(None), Vec::<PathBuf>::new());
    }

    #[test]
    fn the_json_carries_each_row_with_its_status_and_fix() {
        let facts = Facts {
            claude: None,
            ..facts()
        };
        let rows = rows(&facts);

        let report = json(&rows, &facts);

        assert_eq!(report["ok"], json!(false));
        assert_eq!(report["niobe"], json!(niobe_core::VERSION));
        let claude = report["rows"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["name"] == "claude"))
            .expect("the claude row is there");
        assert_eq!(claude["status"], json!("fail"));
        assert!(claude["fix"].is_string());
    }

    /// A config that sets a secret in every place a value could leak from:
    /// the report names the keys and holds none of what they were set to.
    #[test]
    fn a_config_is_reported_by_its_keys_and_none_of_its_values() {
        let text = "default_profile = \"work\"\n\
            [profiles.work]\nbackend = \"claude\"\n\
            env = { AWS_SECRET_ACCESS_KEY = \"hunter2-secret-value\" }\n\
            args = [\"--model\", \"secret-model-name\"]\n\
            settings = \"/home/me/private/secret-settings.json\"\n\
            auth_refresh = \"aws sso login --profile secret-profile\"\n";
        let config = niobe_config::Config::parse(text, Path::new("/home/me/config.toml"))
            .expect("the config is valid");
        let facts = Facts {
            shapes: vec![Shape {
                path: PathBuf::from("/home/me/.config/niobe/config.toml"),
                state: shape_of(&config),
            }],
            ..facts()
        };

        let report = json(&rows(&facts), &facts).to_string();

        for value in [
            "hunter2",
            "secret-model-name",
            "secret-settings",
            "secret-profile",
            "aws sso login",
            "AWS_SECRET_ACCESS_KEY",
        ] {
            assert!(!report.contains(value), "{value} leaked: {report}");
        }
        let shape = &json(&rows(&facts), &facts)["config"][0];
        assert_eq!(shape["state"], json!("set"));
        assert_eq!(shape["keys"], json!(["default_profile"]));
        assert_eq!(shape["profiles"][0]["name"], json!("work"));
        assert_eq!(
            shape["profiles"][0]["keys"],
            json!(["env", "args", "settings", "auth_refresh"])
        );
    }

    #[test]
    fn a_file_that_does_not_parse_is_reported_as_that_and_not_quoted() {
        let facts = Facts {
            shapes: vec![Shape {
                path: PathBuf::from("/home/me/repo/.niobe/config.toml"),
                state: ShapeState::Unreadable,
            }],
            ..facts()
        };

        let report = json(&rows(&facts), &facts);

        assert_eq!(report["config"][0]["state"], json!("does not parse"));
        assert_eq!(
            report["config"][0]["path"],
            json!("~/repo/.niobe/config.toml")
        );
    }

    #[test]
    fn the_target_and_the_newest_sessions_errors_are_in_the_block() {
        let facts = Facts {
            newest_session_errors: Some(3),
            ..facts()
        };

        let report = json(&rows(&facts), &facts);

        assert_eq!(report["target"], json!("aarch64-macos"));
        assert_eq!(report["newest_session_errors"], json!(3));
    }

    #[test]
    fn the_text_puts_each_fix_under_its_row() {
        let lines = text(&rows(&Facts {
            claude: None,
            ..facts()
        }));

        let at = lines
            .iter()
            .position(|line| line.starts_with("fail  claude"))
            .expect("the failed row is listed");
        assert!(
            lines[at + 1].contains("fix: install Claude Code"),
            "{lines:?}"
        );
    }

    #[test]
    fn a_program_that_does_not_exist_is_a_reason_and_not_a_panic() {
        let said = probe(Path::new("/nonexistent/claude"), &["--version"]);

        assert!(said.is_err(), "{said:?}");
    }

    #[test]
    fn a_program_that_never_answers_is_stopped_at_the_deadline() {
        let started = Instant::now();

        let said = probe(Path::new("/bin/sleep"), &["60"]);

        assert_eq!(said, Err("it did not answer in time".to_owned()));
        assert!(started.elapsed() < PROBE_DEADLINE + Duration::from_secs(5));
    }

    #[test]
    fn the_first_file_on_path_wins() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let first = dir.path().join("a");
        let second = dir.path().join("b");
        for d in [&first, &second] {
            std::fs::create_dir(d).expect("a directory is made");
            std::fs::write(d.join("claude"), "").expect("a file is written");
        }
        let path = std::env::join_paths([&first, &second]).expect("paths join");

        assert_eq!(
            find_on_path("claude", Some(&path)),
            Some(first.join("claude"))
        );
        assert_eq!(find_on_path("claude", None), None);
    }
}
