// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! `niobe -p` run with no terminal against stand-in `claude` scripts: the
//! reply on standard output, the session in the store, the permission
//! prompts nobody is there to answer, `--json` against the schema it
//! publishes, and an exit status apart for each of finished, failed and
//! stopped on the budget.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

mod schema;

/// A `claude` that answers a turn with a reply and a result that says what
/// the turn cost, as the CLI's stream-json does.
const ANSWERS_CLAUDE: &str = "#!/bin/sh\n\
    read -r first\n\
    read -r turn\n\
    printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"model\":\"claude-sonnet-5\",\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"the test passes now\"}]}}'\n\
    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"the test passes now\",\"total_cost_usd\":0.25,\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}'\n\
    while read -r line; do :; done\n";

/// A `claude` that asks to run `cargo test`, writes down the answer it gets,
/// and ends the turn.
const ASKS_CLAUDE: &str = "#!/bin/sh\n\
    read -r first\n\
    read -r turn\n\
    printf '%s\\n' '{\"type\":\"control_request\",\"request_id\":\"req_1\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"Bash\",\"input\":{\"command\":\"cargo test\"},\"description\":\"cargo test\",\"tool_use_id\":\"toolu_1\"}}'\n\
    while read -r answer; do\n\
    case \"$answer\" in *'\"req_1\"'*) printf '%s\\n' \"$answer\" > answer.json; break ;; esac\n\
    done\n\
    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"asked\"}'\n\
    while read -r line; do :; done\n";

/// A `claude` whose turn fails.
const FAILS_CLAUDE: &str = "#!/bin/sh\n\
    read -r first\n\
    read -r turn\n\
    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"errors\":[\"the model is overloaded\"]}'\n\
    while read -r line; do :; done\n";

/// A `claude` that stops the turn on its budget, as 2.1.309 does, having
/// spent past it.
const SPENDS_THE_BUDGET_CLAUDE: &str = "#!/bin/sh\n\
    read -r first\n\
    read -r turn\n\
    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"error_max_budget_usd\",\"is_error\":true,\"errors\":[\"Reached maximum budget ($0.50)\"],\"total_cost_usd\":0.62,\"usage\":{\"input_tokens\":100,\"output_tokens\":50,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}'\n\
    while read -r line; do :; done\n";

const METERED: &str =
    "default_profile = \"api\"\n\n[profiles.api]\nbackend = \"claude\"\nbilling = \"metered\"\n";

/// A repository under `dir` with `script` as the `claude` on its `PATH` and
/// `config` as the user's config.
struct Setup {
    dir: tempfile::TempDir,
}

impl Setup {
    fn new(script: &str, config: &str) -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let repo = dir.path().join("catalog");
        std::fs::create_dir_all(repo.join(".git")).expect("a .git directory can be made");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).expect("a directory for the stand-in");
        let claude = bin.join("claude");
        std::fs::write(&claude, script).expect("the stand-in is written");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755))
                .expect("the stand-in is made executable");
        }
        let niobe = dir.path().join("config").join("niobe");
        std::fs::create_dir_all(&niobe).expect("a user config directory");
        std::fs::write(niobe.join("config.toml"), config).expect("the user config is written");
        Self { dir }
    }

    fn repo(&self) -> std::path::PathBuf {
        self.dir.path().join("catalog")
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_niobe"))
            .args(args)
            .current_dir(self.repo())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("CLAUDE_CONFIG_DIR", self.dir.path().join("no-claude-here"))
            .env_remove("HOME")
            .env(
                "PATH",
                format!("{}:/bin:/usr/bin", self.dir.path().join("bin").display()),
            )
            .output()
            .expect("the niobe binary runs")
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("niobe writes UTF-8")
}

fn json(output: &Output) -> Value {
    serde_json::from_str(&text(&output.stdout)).expect("`--json` prints one JSON document")
}

#[test]
fn a_finished_turn_prints_its_reply_alone_and_exits_zero() {
    let setup = Setup::new(ANSWERS_CLAUDE, METERED);
    let output = setup.run(&["-p", "fix the failing test"]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "the test passes now\n");
    assert!(
        text(&output.stderr).contains("session 1 saved"),
        "{}",
        text(&output.stderr)
    );

    let listed = setup.run(&["cost", "--json"]);
    let sessions = json(&listed)["sessions"].clone();
    assert_eq!(sessions[0]["id"], 1, "the turn is in the store: {sessions}");
    assert_eq!(sessions[0]["cost"]["reported_usd"], 0.25);
}

#[test]
fn the_json_is_the_shape_the_published_schema_names() {
    let setup = Setup::new(ANSWERS_CLAUDE, METERED);
    let output = setup.run(&["-p", "fix the failing test", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));

    let printed = json(&output);
    let refused = schema::refusals("print.schema.json", &printed);
    assert!(refused.is_empty(), "{refused:#?}\n{printed:#}");
    assert_eq!(printed["outcome"], "finished");
    assert_eq!(printed["reason"], Value::Null);
    assert_eq!(printed["reply"], "the test passes now");
    assert_eq!(printed["session"], 1);
    assert_eq!(printed["billing"], "metered");
    assert_eq!(printed["cost"]["reported_usd"], 0.25);
    assert_eq!(printed["cost"]["complete"], true);
}

#[test]
fn a_prompt_no_rule_allows_is_refused_and_the_agent_is_told_why() {
    let setup = Setup::new(ASKS_CLAUDE, METERED);
    let output = setup.run(&["-p", "run the tests"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));

    let answer = std::fs::read_to_string(setup.repo().join("answer.json"))
        .expect("the stand-in was answered");
    assert!(answer.contains("\"deny\""), "{answer}");
    assert!(answer.contains("no one at the terminal"), "{answer}");
}

#[test]
fn a_prompt_a_standing_rule_allows_is_allowed() {
    let config = format!("{METERED}\n[permissions]\nallow = [\"Bash(cargo test)\"]\n");
    let setup = Setup::new(ASKS_CLAUDE, &config);
    let output = setup.run(&["-p", "run the tests"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));

    let answer = std::fs::read_to_string(setup.repo().join("answer.json"))
        .expect("the stand-in was answered");
    assert!(answer.contains("\"allow\""), "{answer}");
}

#[test]
fn a_rule_given_with_allow_answers_for_this_run_alone() {
    let setup = Setup::new(ASKS_CLAUDE, METERED);
    let output = setup.run(&["-p", "run the tests", "--allow", "Bash(cargo test)"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let answer = std::fs::read_to_string(setup.repo().join("answer.json"))
        .expect("the stand-in was answered");
    assert!(answer.contains("\"allow\""), "{answer}");

    let config = std::fs::read_to_string(setup.dir.path().join("config/niobe/config.toml"))
        .expect("the user config reads");
    assert!(
        !config.contains("cargo test"),
        "nothing is written: {config}"
    );
}

#[test]
fn a_terse_turn_appends_the_prompt_to_the_clis_system_prompt_and_says_so() {
    let answers_and_says_its_arguments = ANSWERS_CLAUDE.replacen(
        "#!/bin/sh\n",
        "#!/bin/sh\nfor arg in \"$@\"; do printf '%s\\n' \"$arg\"; done > argv.txt\n",
        1,
    );
    let setup = Setup::new(&answers_and_says_its_arguments, METERED);
    let output = setup.run(&["-p", "fix it", "--terse", "on", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(json(&output)["terse"], true);
    let argv = std::fs::read_to_string(setup.repo().join("argv.txt")).expect("the CLI ran");
    let after = argv
        .lines()
        .skip_while(|arg| *arg != "--append-system-prompt")
        .nth(1)
        .expect("a system prompt is appended");
    assert!(
        after.starts_with("Keep your replies to the user terse"),
        "{argv}"
    );

    let plain = Setup::new(&answers_and_says_its_arguments, METERED);
    let output = plain.run(&["-p", "fix it", "--json"]);
    assert_eq!(json(&output)["terse"], false);
    let argv = std::fs::read_to_string(plain.repo().join("argv.txt")).expect("the CLI ran");
    assert!(!argv.contains("--append-system-prompt"), "{argv}");
}

#[test]
fn a_failed_turn_exits_one_with_the_backends_reason() {
    let setup = Setup::new(FAILS_CLAUDE, METERED);
    let output = setup.run(&["-p", "fix the failing test", "--json"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("the model is overloaded"),
        "{}",
        text(&output.stderr)
    );
    let printed = json(&output);
    assert_eq!(printed["outcome"], "failed");
    assert!(schema::refusals("print.schema.json", &printed).is_empty());
}

#[test]
fn a_turn_the_budget_stopped_exits_three_apart_from_a_failure() {
    let setup = Setup::new(SPENDS_THE_BUDGET_CLAUDE, METERED);
    let output = setup.run(&["-p", "fix the failing test", "--budget", "0.50", "--json"]);

    assert_eq!(output.status.code(), Some(3), "{}", text(&output.stderr));
    let printed = json(&output);
    assert_eq!(printed["outcome"], "budget_stopped");
    assert!(
        printed["reason"]
            .as_str()
            .is_some_and(|why| why.contains("Reached maximum budget ($0.50)")),
        "{printed:#}"
    );
    assert!(schema::refusals("print.schema.json", &printed).is_empty());
}

#[test]
fn a_budget_on_a_plan_is_refused_before_anything_starts() {
    let plan =
        "default_profile = \"max\"\n\n[profiles.max]\nbackend = \"claude\"\nbilling = \"plan\"\n";
    let setup = Setup::new(ANSWERS_CLAUDE, plan);
    let output = setup.run(&["-p", "fix the failing test", "--budget", "1"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(!Path::new(&setup.repo().join(".niobe").join("sessions.db")).exists());
}

/// A `claude` whose reply holds a right-to-left mark, as Hebrew or Arabic
/// prose does, a DEL and a C1 control.
const MARKED_CLAUDE: &str = "#!/bin/sh\n\
    read -r first\n\
    read -r turn\n\
    printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"model\":\"claude-sonnet-5\",\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"\\u05e9\\u05dc\\u05d5\\u05dd\\u200f ok \\u007f\\u009b2J\"}]}}'\n\
    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\",\"total_cost_usd\":0.25,\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}'\n\
    while read -r line; do :; done\n";

#[test]
fn the_json_carries_the_reply_as_written_and_no_character_that_acts_on_a_terminal() {
    let setup = Setup::new(MARKED_CLAUDE, METERED);
    let output = setup.run(&["-p", "say hello", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));

    let printed = text(&output.stdout);
    let acting = printed
        .chars()
        .filter(|c| matches!(c, '\u{7f}'..='\u{9f}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{61c}'))
        .count();
    assert_eq!(acting, 0, "printed raw: {printed:?}");
    assert_eq!(
        json(&output)["reply"],
        "\u{5e9}\u{5dc}\u{5d5}\u{5dd}\u{200f} ok \u{7f}\u{9b}2J",
        "the reply is the agent's, character for character"
    );
}
