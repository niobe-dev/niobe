// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The `niobe` binary, run as a process.
//!
//! Standard output is a pipe here, so `replay` and `--resume` print the fold
//! instead of opening the shell. That is what lets a test compare a session
//! read back from the store with the log it was recorded from, through the
//! same binary an operator runs.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use niobe_store::{Store, read_log};

const FIXTURE: &str = "../niobe-core/tests/fixtures/session-200.jsonl";

/// The fixture must be read and folded in under this many milliseconds.
const REPLAY_BUDGET_MS: f64 = 50.0;

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)
}

/// Runs the binary with no user config: the operator's own would otherwise
/// change what these tests see.
fn niobe(cwd: &Path, args: &[&str]) -> Output {
    niobe_with_user_config(cwd, &cwd.join("no-user-config-here"), args)
}

/// Runs the binary with `config_home` as the user's config directory.
fn niobe_with_user_config(cwd: &Path, config_home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_niobe"))
        .args(args)
        .current_dir(cwd)
        .env("XDG_CONFIG_HOME", config_home)
        .output()
        .expect("the niobe binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr is UTF-8")
}

/// A directory that is the root of a repository, with the fixture recorded as
/// its first session.
fn repo_with_the_fixture_recorded() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    std::fs::create_dir(dir.path().join(".git")).expect("a .git directory can be made");
    std::fs::create_dir(dir.path().join(".niobe")).expect("a .niobe directory can be made");

    let store = Store::open(&dir.path().join(".niobe").join("sessions.db")).expect("store");
    let session = store.create_session().expect("a session is created");
    let text = std::fs::read_to_string(fixture_path()).expect("the fixture reads");
    for event in read_log(&text).expect("the fixture parses") {
        store.append(session, &event).expect("an append succeeds");
    }
    dir
}

/// The summary without its first line, which names where the events came from
/// and how long they took.
fn body(summary: &str) -> Vec<&str> {
    summary.lines().skip(1).collect()
}

#[test]
fn replaying_the_fixture_prints_its_totals_inside_the_budget() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = fixture_path();
    let output = niobe(here, &["replay", fixture.to_str().expect("a UTF-8 path")]);
    let out = stdout(&output);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(out.contains("122,554"), "{out}");
    assert!(out.contains("≥$2.47"), "{out}");
    assert!(
        out.contains("4 of 14 usage records reported no cost"),
        "{out}"
    );

    let millis: f64 = out
        .lines()
        .next()
        .and_then(|header| header.split(" in ").nth(1))
        .and_then(|rest| rest.strip_suffix(" ms"))
        .and_then(|ms| ms.parse().ok())
        .unwrap_or_else(|| panic!("no timing in the header: {out}"));
    assert!(
        millis < REPLAY_BUDGET_MS,
        "replay took {millis} ms, over the {REPLAY_BUDGET_MS} ms budget"
    );
}

#[test]
fn a_resumed_session_shows_the_totals_the_log_it_recorded_folds_to() {
    let repo = repo_with_the_fixture_recorded();
    let fixture = fixture_path();

    let resumed = niobe(repo.path(), &["--resume", "1"]);
    let replayed = niobe(
        repo.path(),
        &["replay", fixture.to_str().expect("a UTF-8 path")],
    );

    assert!(resumed.status.success(), "{}", stderr(&resumed));
    let resumed = stdout(&resumed);
    assert!(resumed.starts_with("session 1 · 200 events"), "{resumed}");
    assert_eq!(body(&resumed), body(&stdout(&replayed)));
}

#[test]
fn the_session_list_shows_what_the_store_holds() {
    let repo = repo_with_the_fixture_recorded();
    let output = niobe(repo.path(), &["sessions"]);
    let out = stdout(&output);

    assert!(output.status.success(), "{}", stderr(&output));
    let row = out.lines().nth(1).unwrap_or_default();
    assert!(row.trim_start().starts_with("1 "), "{out}");
    assert!(row.contains(" 200 "), "{out}");
    assert!(row.contains("turn 1: keep going on the etag work"), "{out}");
}

#[test]
fn the_session_list_is_the_same_from_a_subdirectory_of_the_repository() {
    let repo = repo_with_the_fixture_recorded();
    let nested = repo.path().join("src").join("deep");
    std::fs::create_dir_all(&nested).expect("a nested directory can be made");

    let output = niobe(&nested, &["sessions"]);
    assert!(stdout(&output).contains("turn 1:"), "{}", stdout(&output));
    assert!(!nested.join(".niobe").exists());
}

#[test]
fn resuming_a_session_that_does_not_exist_says_which_and_fails() {
    let repo = repo_with_the_fixture_recorded();
    let output = niobe(repo.path(), &["--resume", "9"]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("no session 9"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn listing_sessions_where_nothing_was_recorded_creates_nothing() {
    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    std::fs::create_dir(dir.path().join(".git")).expect("a .git directory can be made");

    let output = niobe(dir.path(), &["sessions"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).starts_with("no sessions recorded"),
        "{}",
        stdout(&output)
    );
    assert!(!dir.path().join(".niobe").exists());
}

#[test]
fn a_log_with_a_bad_line_names_the_line() {
    let dir = tempfile::tempdir().expect("a temporary directory can be created");
    let log = dir.path().join("bad.jsonl");
    std::fs::write(
        &log,
        "{\"type\":\"user_message\",\"text\":\"hi\"}\nnot json\n",
    )
    .expect("the log is written");

    let output = niobe(dir.path(), &["replay", "bad.jsonl"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("line 2"), "{}", stderr(&output));
}

/// A repository and a user config directory, each with the config given.
struct Configured {
    repo: tempfile::TempDir,
    user: tempfile::TempDir,
}

impl Configured {
    fn new(user_config: &str, repo_config: &str) -> Self {
        let repo = tempfile::tempdir().expect("a temporary directory can be created");
        std::fs::create_dir(repo.path().join(".git")).expect("a .git directory can be made");
        if !repo_config.is_empty() {
            std::fs::create_dir(repo.path().join(".niobe")).expect("a .niobe directory");
            std::fs::write(self::repo_config(repo.path()), repo_config)
                .expect("the repo config is written");
        }

        let user = tempfile::tempdir().expect("a temporary directory can be created");
        if !user_config.is_empty() {
            std::fs::create_dir(user.path().join("niobe")).expect("a niobe config directory");
            std::fs::write(user.path().join("niobe").join("config.toml"), user_config)
                .expect("the user config is written");
        }
        Self { repo, user }
    }

    fn run(&self, args: &[&str]) -> Output {
        niobe_with_user_config(self.repo.path(), self.user.path(), args)
    }

    fn user_config(&self) -> PathBuf {
        self.user.path().join("niobe").join("config.toml")
    }
}

fn repo_config(repo: &Path) -> PathBuf {
    repo.join(".niobe").join("config.toml")
}

const USER_CONFIG: &str = r#"
default_profile = "personal"

[profiles.personal]
backend = "claude"

[profiles.work]
backend = "claude"
env = { CLAUDE_CODE_USE_BEDROCK = "1", AWS_PROFILE = "account-value" }
auth_refresh = "aws sso login --profile example-sso"
"#;

/// The line of `niobe profiles` output that lists `name`.
fn profile_row<'a>(listing: &'a str, name: &str) -> &'a str {
    listing
        .lines()
        .find(|line| {
            line.get(2..)
                .is_some_and(|rest| rest.starts_with(&format!("{name} ")))
        })
        .unwrap_or_else(|| panic!("no row for {name}: {listing}"))
}

#[test]
fn the_profile_list_shows_each_profile_its_backend_and_where_it_was_defined() {
    let setup = Configured::new(USER_CONFIG, "");
    let output = setup.run(&["profiles"]);
    let out = stdout(&output);

    assert!(output.status.success(), "{}", stderr(&output));
    let work = profile_row(&out, "work");
    assert!(work.contains(" claude "), "{out}");
    assert!(
        work.ends_with(&setup.user_config().display().to_string()),
        "{out}"
    );
    assert!(
        out.contains("AWS_PROFILE, CLAUDE_CODE_USE_BEDROCK"),
        "variable names are listed: {out}"
    );
    assert!(!out.contains("account-value"), "no variable's value: {out}");
    assert!(profile_row(&out, "personal").starts_with("* "), "{out}");
    assert!(profile_row(&out, "work").starts_with("  "), "{out}");
}

#[test]
fn repo_config_overrides_user_config() {
    let setup = Configured::new(
        USER_CONFIG,
        "default_profile = \"work\"\n\n[profiles.work]\nbackend = \"codex\"\n",
    );
    let output = setup.run(&["profiles"]);
    let out = stdout(&output);

    assert!(output.status.success(), "{}", stderr(&output));
    let work = profile_row(&out, "work");
    assert!(work.starts_with("* "), "the repo's default wins: {out}");
    assert!(work.contains(" codex "), "{out}");
    assert!(
        work.ends_with(&repo_config(setup.repo.path()).display().to_string()),
        "{out}"
    );
    assert!(
        !out.contains("AWS_PROFILE"),
        "the repo's profile replaces the user's whole: {out}"
    );
    assert!(profile_row(&out, "personal").contains(" claude "), "{out}");
}

#[test]
fn the_profile_flag_selects_the_profile() {
    let setup = Configured::new(USER_CONFIG, "");
    for args in [
        &["--profile", "work", "profiles"][..],
        &["profiles", "--profile=work"],
    ] {
        let output = setup.run(args);
        let out = stdout(&output);

        assert!(output.status.success(), "{args:?}: {}", stderr(&output));
        assert!(
            profile_row(&out, "work").starts_with("* "),
            "{args:?}: {out}"
        );
        assert!(
            profile_row(&out, "personal").starts_with("  "),
            "{args:?}: {out}"
        );
    }
}

#[test]
fn a_profile_no_config_defines_is_an_error_that_lists_the_ones_there_are() {
    let setup = Configured::new(USER_CONFIG, "");
    for args in [
        &["--profile", "wrok", "profiles"][..],
        &["--profile", "wrok"],
    ] {
        let output = setup.run(args);

        assert!(!output.status.success(), "{args:?}");
        assert!(
            stderr(&output)
                .contains("no profile named `wrok`; the profiles defined are `personal`, `work`"),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn an_invalid_config_errors_with_the_key_and_the_line() {
    let setup = Configured::new(
        USER_CONFIG,
        "[profiles.work]\nbackend = \"codex\"\nargs = \"--verbose\"\n",
    );
    // The binary finds the repository from its working directory, which the
    // operating system reports with symbolic links resolved.
    let repo = std::fs::canonicalize(setup.repo.path()).expect("the repository exists");
    let expected = format!(
        "niobe: {}:3: profiles.work.args: expected an array of strings, found a string\n",
        repo_config(&repo).display()
    );

    for args in [
        &["profiles"][..],
        &[],
        &["--resume", "1"],
        &["--profile", "work"],
    ] {
        let output = setup.run(args);
        assert!(!output.status.success(), "{args:?}");
        assert_eq!(stderr(&output), expected, "{args:?}");
    }
}

#[test]
fn a_config_that_is_not_toml_names_the_line() {
    let setup = Configured::new(
        "[profiles.work]\nbackend = \"claude\"\nenv = { A = \"1\"\n",
        "",
    );
    let output = setup.run(&["profiles"]);
    let err = stderr(&output);

    assert!(!output.status.success());
    assert!(
        err.starts_with(&format!("niobe: {}:3: ", setup.user_config().display())),
        "{err}"
    );
    assert_eq!(
        err.lines().count(),
        1,
        "one line, no rendered source: {err}"
    );
}

#[test]
fn commands_that_run_no_session_do_not_read_the_config() {
    let setup = Configured::new("this is not toml", "");
    for args in [&["sessions"][..], &["--help"], &["--version"]] {
        let output = setup.run(args);
        assert!(output.status.success(), "{args:?}: {}", stderr(&output));
    }
}

#[test]
fn where_no_config_defines_a_profile_the_list_says_where_it_looked() {
    let setup = Configured::new("", "");
    let output = setup.run(&["profiles"]);
    let out = stdout(&output);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(out.starts_with("no profiles defined"), "{out}");
    assert!(
        out.contains(&setup.user_config().display().to_string()),
        "{out}"
    );
    assert!(
        out.contains(&repo_config(setup.repo.path()).display().to_string()),
        "{out}"
    );
}

/// A user config directory holding `prices` as its price file.
fn with_user_prices(prices: &str) -> Configured {
    let setup = Configured::new("", "");
    std::fs::create_dir(setup.user.path().join("niobe")).expect("a niobe config directory");
    std::fs::write(user_prices(&setup), prices).expect("the price file is written");
    setup
}

fn user_prices(setup: &Configured) -> PathBuf {
    setup.user.path().join("niobe").join("prices.toml")
}

/// The whitespace-separated fields of the row of `niobe prices` for `model`.
fn price_row<'a>(listing: &'a str, model: &str) -> Vec<&'a str> {
    listing
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .find(|fields| fields.first() == Some(&model))
        .unwrap_or_else(|| panic!("no row for {model}: {listing}"))
}

#[test]
fn the_price_list_shows_the_bundled_rates_in_force_today() {
    let setup = Configured::new("", "");
    let output = setup.run(&["prices"]);
    let out = stdout(&output);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        out.starts_with("USD per million tokens, in force on "),
        "{out}"
    );
    assert_eq!(
        price_row(&out, "eu.anthropic.claude-sonnet-5[1m]"),
        [
            "eu.anthropic.claude-sonnet-5[1m]",
            "2.20",
            "11.00",
            "0.22",
            "2.75",
            "4.40",
            "2026-06-30",
            "bundled",
            "prices.toml"
        ]
    );
    assert_eq!(
        price_row(&out, "gpt-5.3-codex"),
        [
            "gpt-5.3-codex",
            "1.75",
            "14.00",
            "0.175",
            "1.75",
            "—",
            "2026-02-24",
            "bundled",
            "prices.toml"
        ]
    );
}

#[test]
fn a_models_price_history_is_listed_with_the_date_each_price_took_effect() {
    let setup = Configured::new("", "");
    let output = setup.run(&["prices", "gpt-5.6-terra"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "\
gpt-5.6-terra, USD per million tokens, from bundled prices.toml
SINCE                        INPUT  OUTPUT  CACHE READ  CACHE WRITE  1H WRITE
2026-07-09                    2.50   15.00        0.25        3.125         —
  prompt over 272000 tokens   5.00   22.50        0.50         6.25         —
2026-07-30                    2.00   12.00        0.20         2.50         —
  prompt over 272000 tokens   4.00   18.00        0.40         5.00         —
"
    );
}

#[test]
fn a_model_no_price_table_lists_is_unpriced() {
    let setup = Configured::new("", "");
    let output = setup.run(&["prices", "codex-auto-review"]);

    assert!(!output.status.success());
    assert_eq!(
        stderr(&output),
        format!(
            "niobe: `codex-auto-review` is unpriced: neither the bundled price table nor {} lists it\n",
            user_prices(&setup).display()
        )
    );
}

#[test]
fn a_user_price_file_replaces_the_schedule_of_each_model_it_lists() {
    let setup = with_user_prices(
        "[[model]]\nids = [\"claude-opus-5\", \"my-proxy-model\"]\n\n[[model.price]]\nfrom = 2026-01-01\ninput = 4\noutput = 20\ncache_read = 0.4\ncache_write = 5\n",
    );
    let path = user_prices(&setup).display().to_string();

    let output = setup.run(&["prices", "claude-opus-5"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!(
            "\
claude-opus-5, USD per million tokens, from {path}
SINCE       INPUT  OUTPUT  CACHE READ  CACHE WRITE  1H WRITE
2026-01-01   4.00   20.00        0.40         5.00         —
"
        )
    );

    let listing = stdout(&setup.run(&["prices"]));
    assert_eq!(
        price_row(&listing, "my-proxy-model"),
        [
            "my-proxy-model",
            "4.00",
            "20.00",
            "0.40",
            "5.00",
            "—",
            "2026-01-01",
            path.as_str()
        ]
    );
    assert_eq!(
        price_row(&listing, "claude-opus-5[1m]")[1..],
        [
            "5.00",
            "25.00",
            "0.50",
            "6.25",
            "10.00",
            "2026-07-24",
            "bundled",
            "prices.toml"
        ],
        "an id the file does not list keeps the bundled price"
    );
}

#[test]
fn an_invalid_price_file_errors_with_the_key_and_the_line() {
    let setup = with_user_prices(
        "[[model]]\nids = [\"m\"]\n\n[[model.price]]\nfrom = 2026-01-01\ninput = \"4\"\noutput = 20\ncache_read = 0.4\ncache_write = 5\n",
    );
    for args in [&["prices"][..], &["prices", "m"]] {
        let output = setup.run(args);
        assert!(!output.status.success(), "{args:?}");
        assert_eq!(
            stderr(&output),
            format!(
                "niobe: {}:6: model[0].price[0].input: expected a number, found a string\n",
                user_prices(&setup).display()
            ),
            "{args:?}"
        );
    }
}
