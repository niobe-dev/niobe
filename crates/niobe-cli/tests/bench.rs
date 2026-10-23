// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! `niobe bench` over suites laid out in a temporary directory, against
//! stand-in `claude` scripts: a task the turn passes and one it does not, the
//! JSON against the schema it publishes, a turn that never ends, and the
//! suite in the repository's own `bench/`, each task of which must fail as it
//! is laid out and pass with its reference solution laid over it.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::Value;

mod schema;

/// A `claude` that answers a turn by writing `done.txt` where it runs, and
/// says what the turn cost, as the CLI's stream-json does.
const DOES_IT_CLAUDE: &str = "#!/bin/sh\n\
    read -r first\n\
    read -r turn\n\
    : > done.txt\n\
    printf '%s\\n' '{\"type\":\"stream_event\",\"event\":{\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-5\",\"id\":\"msg_1\"}}}'\n\
    printf '%s\\n' '{\"type\":\"stream_event\",\"event\":{\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}'\n\
    printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"model\":\"claude-sonnet-5\",\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"done\"}],\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}'\n\
    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\",\"total_cost_usd\":0.25,\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}'\n\
    while read -r line; do :; done\n";

/// A `claude` that asks to edit the file `path` names — relative
/// to the directory it runs in, or absolute — writes down the answer it gets
/// as `answer.json`, and ends the turn.
fn asks_to_edit(path: &str) -> String {
    format!(
        "#!/bin/sh\n\
         read -r first\n\
         read -r turn\n\
         case '{path}' in /*) target='{path}' ;; *) target=\"$(pwd -P)/{path}\" ;; esac\n\
         printf '%s\\n' \"{{\\\"type\\\":\\\"control_request\\\",\\\"request_id\\\":\\\"req_1\\\",\\\"request\\\":{{\\\"subtype\\\":\\\"can_use_tool\\\",\\\"tool_name\\\":\\\"Edit\\\",\\\"input\\\":{{\\\"file_path\\\":\\\"$target\\\",\\\"old_string\\\":\\\"a\\\",\\\"new_string\\\":\\\"b\\\"}},\\\"tool_use_id\\\":\\\"toolu_1\\\"}}}}\"\n\
         while read -r answer; do\n\
         case \"$answer\" in *'\"req_1\"'*) printf '%s\\n' \"$answer\" > answer.json; break ;; esac\n\
         done\n\
         printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"asked\"}}'\n\
         while read -r line; do :; done\n"
    )
}

const METERED: &str =
    "default_profile = \"api\"\n\n[profiles.api]\nbackend = \"claude\"\nbilling = \"metered\"\n";

/// A directory holding a `claude` on a `PATH` of its own, the user's config,
/// and a suite.
struct Setup {
    dir: tempfile::TempDir,
}

impl Setup {
    fn new(claude: &str) -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).expect("a directory for the stand-in");
        executable(&bin.join("claude"), claude);
        let niobe = dir.path().join("config").join("niobe");
        std::fs::create_dir_all(&niobe).expect("a user config directory");
        std::fs::write(niobe.join("config.toml"), METERED).expect("the user config is written");
        Self { dir }
    }

    /// A task whose check passes where the turn's permission prompt was
    /// answered `decision`.
    fn answered(&self, name: &str, decision: &str) {
        self.task(name, "answer.json");
        std::fs::write(
            self.suite().join(name).join("check.sh"),
            format!("grep -q '\"{decision}\"' answer.json\n"),
        )
        .expect("the check is written");
    }

    fn suite(&self) -> PathBuf {
        self.dir.path().join("suite")
    }

    /// A task of the suite whose check passes where `wanted` exists.
    fn task(&self, name: &str, wanted: &str) {
        let task = self.suite().join(name);
        std::fs::create_dir_all(task.join("files")).expect("the task's files directory");
        std::fs::write(task.join("files").join("README"), "a repository\n")
            .expect("a file to start from");
        std::fs::write(
            task.join("prompt.md"),
            format!("<!--\nheader\n-->\n\nMake {wanted}.\n"),
        )
        .expect("the prompt is written");
        std::fs::write(task.join("check.sh"), format!("test -f {wanted}\n"))
            .expect("the check is written");
    }

    fn bench(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_niobe"))
            .arg("bench")
            // Named as typed, relative to where niobe runs, which is not
            // where each check runs.
            .arg("suite")
            .args(args)
            .current_dir(self.dir.path())
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

fn executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, script).expect("the script is written");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("the script is made executable");
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("niobe writes UTF-8")
}

fn results(output: &Output) -> Value {
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    serde_json::from_str(&text(&output.stdout)).expect("`--json` prints one JSON document")
}

#[test]
fn each_task_is_judged_by_its_own_check_and_a_failure_keeps_its_copy() {
    let setup = Setup::new(DOES_IT_CLAUDE);
    setup.task("a-passes", "done.txt");
    setup.task("b-fails", "other.txt");
    let results = results(&setup.bench(&["--json"]));

    let refused = schema::refusals("bench.schema.json", &results);
    assert!(refused.is_empty(), "{refused:#?}\n{results:#}");
    let (passes, fails) = (&results["tasks"][0], &results["tasks"][1]);
    assert_eq!(passes["name"], "a-passes");
    assert_eq!(passes["passed"], true);
    assert_eq!(passes["outcome"], "finished");
    assert_eq!(passes["kept"], Value::Null);
    assert_eq!(passes["turn"]["cost"]["reported_usd"], 0.25);
    assert_eq!(passes["turn"]["tokens"]["total"], 15);

    assert_eq!(fails["passed"], false);
    assert_eq!(
        fails["outcome"], "finished",
        "the turn ended; the check failed"
    );
    let kept = Path::new(fails["kept"].as_str().expect("a failure keeps its copy"));
    assert!(
        kept.join("done.txt").exists(),
        "the copy is what the turn left"
    );
    assert!(
        kept.join("README").exists(),
        "laid out from the task's files"
    );
    std::fs::remove_dir_all(kept).expect("the kept copy can be removed");

    assert_eq!(results["summary"]["passed"], 1);
    assert_eq!(results["summary"]["pass_rate"], 0.5);
    assert_eq!(results["summary"]["tokens"], 30);
    assert_eq!(results["suite"], "suite");
}

#[test]
fn the_table_says_which_passed_and_what_the_suite_spent() {
    let setup = Setup::new(DOES_IT_CLAUDE);
    setup.task("only", "done.txt");
    let output = setup.bench(&[]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));

    let printed = text(&output.stdout);
    let row = printed
        .lines()
        .find(|line| line.starts_with("only"))
        .expect("a row for the task");
    assert!(
        row.contains("pass") && row.contains("finished") && row.contains("15"),
        "{printed}"
    );
    assert!(
        printed.contains("1 of 1 passed (100%) · 15 tokens"),
        "{printed}"
    );
    assert!(text(&output.stderr).contains("task 1 of 1: only"));
}

#[test]
fn a_turn_that_never_ends_is_ended_at_its_deadline_and_takes_its_cli_with_it() {
    let setup = Setup::new("");
    let pid_file = setup.dir.path().join("claude.pid");
    executable(
        &setup.dir.path().join("bin").join("claude"),
        &format!(
            "#!/bin/sh\necho $$ > '{}'\nwhile read -r line; do :; done\n",
            pid_file.display()
        ),
    );
    setup.task("hangs", "done.txt");
    let results = results(&setup.bench(&["--json", "--timeout", "2"]));

    let task = &results["tasks"][0];
    assert_eq!(task["outcome"], "timed_out", "{results:#}");
    assert_eq!(task["passed"], false);
    assert_eq!(task["turn"], Value::Null);
    assert_eq!(results["summary"]["tokens"], Value::Null);
    assert!(schema::refusals("bench.schema.json", &results).is_empty());

    let pid = std::fs::read_to_string(&pid_file).expect("the stand-in started");
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(pid.trim()) {
        assert!(
            Instant::now() < deadline,
            "the stand-in claude outlived its niobe"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    if let Some(kept) = task["kept"].as_str() {
        let _ = std::fs::remove_dir_all(kept);
    }
}

#[test]
fn a_turn_may_edit_inside_its_copy_and_nowhere_else() {
    let inside = Setup::new(&asks_to_edit("README"));
    inside.answered("inside", "allow");
    let ran = results(&inside.bench(&["--json"]));
    assert_eq!(ran["tasks"][0]["passed"], true, "{ran:#}");

    let outside = Setup::new(&asks_to_edit("/etc/hosts"));
    outside.answered("outside", "deny");
    let ran = results(&outside.bench(&["--json"]));
    assert_eq!(ran["tasks"][0]["passed"], true, "{ran:#}");

    let climbing = Setup::new(&asks_to_edit("../README"));
    climbing.answered("climbing", "deny");
    let ran = results(&climbing.bench(&["--json"]));
    assert_eq!(ran["tasks"][0]["passed"], true, "{ran:#}");
}

fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn a_directory_with_no_task_is_refused() {
    let setup = Setup::new(DOES_IT_CLAUDE);
    std::fs::create_dir_all(setup.suite()).expect("an empty suite");
    let output = setup.bench(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("holds no task"));
}

/// The repository's own suite: every task fails as it is laid out — so a
/// pass is the turn's doing — and passes with `solution/` laid over it, so a
/// failure is the turn's too.
#[test]
fn every_task_of_the_bundled_suite_fails_as_laid_out_and_passes_when_solved() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench");
    let mut tasks: Vec<PathBuf> = std::fs::read_dir(&suite)
        .expect("the bundled suite is in the tree")
        .flatten()
        .map(|entry| entry.path())
        .filter(|dir| dir.join("prompt.md").is_file())
        .collect();
    tasks.sort();
    assert_eq!(tasks.len(), 20, "the suite holds twenty tasks");

    for task in tasks {
        let name = task.file_name().map(|n| n.to_string_lossy().into_owned());
        let work = tempfile::tempdir().expect("a copy to check");
        copy(&task.join("files"), work.path());
        assert!(
            !checks(&task, work.path()),
            "{name:?} passes before anything is done to it"
        );
        copy(&task.join("solution"), work.path());
        assert!(checks(&task, work.path()), "{name:?} fails when solved");
    }
}

fn checks(task: &Path, work: &Path) -> bool {
    Command::new("sh")
        .arg(task.join("check.sh"))
        .current_dir(work)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("sh runs the check")
        .success()
}

fn copy(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from)
        .expect("the directory reads")
        .flatten()
    {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            std::fs::create_dir_all(&target).expect("a directory is made");
            copy(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("a file is copied");
        }
    }
}
