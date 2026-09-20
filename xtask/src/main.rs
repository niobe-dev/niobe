// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Workspace automation. Run as `cargo xtask <task>`.
//!
//! The tasks here are the ones CI runs, so that "CI green" and "green on my
//! machine" mean the same thing.

#![allow(clippy::print_stdout, clippy::print_stderr)]

mod headers;
mod version;

use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use headers::Rule;

/// The size budget for the release `niobe` binary.
const MAX_BINARY_BYTES: u64 = 20 * 1024 * 1024;

/// Which workspace crates each workspace crate is allowed to depend on, in
/// normal, build and dev edges alike.
///
/// This is the rule "no backend-specific types in `niobe-tui`" written down
/// where CI can fail on it. A type cannot leak out of a bridge into the TUI if
/// the TUI cannot name the bridge at all, so the rule is enforced by the crate
/// graph rather than by review.
const ALLOWED_WORKSPACE_DEPS: &[(&str, &[&str])] = &[
    ("niobe-core", &[]),
    ("niobe-ledger", &["niobe-core"]),
    ("niobe-config", &["niobe-core"]),
    ("niobe-store", &["niobe-core"]),
    ("niobe-tui", &["niobe-core"]),
    ("niobe-bridge-claude", &["niobe-core"]),
    ("niobe-bridge-codex", &["niobe-core"]),
    (
        "niobe-cli",
        &[
            "niobe-core",
            "niobe-ledger",
            "niobe-config",
            "niobe-store",
            "niobe-tui",
            "niobe-bridge-claude",
            "niobe-bridge-codex",
        ],
    ),
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (task, rest) = args
        .split_first()
        .map_or((None, &[][..]), |(t, r)| (Some(t.as_str()), r));
    let result = match task {
        Some("ci") => ci(),
        Some("size") => size().map(|_| ()),
        Some("layering") => layering(),
        Some("headers") => headers(),
        Some("version") => version::run(rest),
        _ => {
            eprintln!(
                "\
usage: cargo xtask <task>

tasks:
    ci        fmt --check, clippy -D warnings, tests, layering, headers, versions, then size
    layering  check that no crate depends on a workspace crate it may not name
    headers   check that every file carries the SPDX copyright header
    version   report what a release would be; --check that the manifest agrees with itself;
              <patch|minor|major|X.Y.Z> to move the workspace to that version
    size      build --release and check the `niobe` binary against the 20 MB budget"
            );
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::FAILURE
        }
    }
}

fn ci() -> Result<(), String> {
    cargo(&["fmt", "--all", "--check"])?;
    cargo(&[
        "clippy",
        "--workspace",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ])?;
    cargo(&["test", "--workspace"])?;
    layering()?;
    headers()?;
    version::run(&["--check".to_owned()])?;
    size().map(|_| ())
}

/// Checks every workspace crate's dependency tree against
/// [`ALLOWED_WORKSPACE_DEPS`].
fn layering() -> Result<(), String> {
    let mut violations = Vec::new();

    for (package, allowed) in ALLOWED_WORKSPACE_DEPS {
        for dependency in workspace_dependencies_of(package)? {
            if dependency != *package && !allowed.contains(&dependency.as_str()) {
                violations.push(format!("{package} depends on {dependency}"));
            }
        }
    }

    if violations.is_empty() {
        println!(
            "layering: {} crates, no forbidden edges",
            ALLOWED_WORKSPACE_DEPS.len()
        );
        return Ok(());
    }

    Err(format!(
        "forbidden dependency edges:\n  {}\nthe crate graph is what keeps backend-specific types \
         out of niobe-tui; widen ALLOWED_WORKSPACE_DEPS only on purpose",
        violations.join("\n  ")
    ))
}

/// Checks every file git would publish — tracked, or untracked and not ignored
/// — for the header its type requires.
fn headers() -> Result<(), String> {
    let listing = command_output(
        "git",
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;

    let mut checked = 0;
    let mut problems = Vec::new();
    for path in listing.split('\0').filter(|p| !p.is_empty()) {
        let rule = match headers::rule_for(path) {
            Some(Rule::Exempt) => continue,
            Some(rule) => rule,
            None => {
                problems.push(format!(
                    "{path}: no header rule for this file type; add a rule or an exemption \
                     in xtask/src/headers.rs"
                ));
                continue;
            }
        };

        let contents = match std::fs::read_to_string(workspace_root().join(path)) {
            Ok(contents) => contents,
            // Deleted from the working tree but still in the index: nothing to publish.
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot read {path}: {e}")),
        };

        checked += 1;
        if !headers::has_header(rule, &contents) {
            problems.push(format!("{path}: missing the SPDX copyright header"));
        }
    }

    if problems.is_empty() {
        println!("headers: {checked} files, all carry the copyright header");
        return Ok(());
    }

    Err(format!(
        "copyright headers:\n  {}\nsee AGENTS.md for the header each file type takes",
        problems.join("\n  ")
    ))
}

/// Every `niobe-*` crate reachable from `package`, across normal, build and dev
/// edges.
fn workspace_dependencies_of(package: &str) -> Result<Vec<String>, String> {
    let output = cargo_output(&[
        "tree",
        "--package",
        package,
        "--edges",
        "normal,build,dev",
        "--prefix",
        "none",
        "--format",
        "{p}",
    ])?;

    let mut found: Vec<String> = output
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| name.starts_with("niobe-"))
        .map(str::to_owned)
        .collect();
    found.sort_unstable();
    found.dedup();
    Ok(found)
}

/// Builds the release binary and returns its size in bytes, failing if it is
/// over budget.
fn size() -> Result<u64, String> {
    cargo(&["build", "--release", "--package", "niobe-cli"])?;

    let binary = target_dir().join("release").join("niobe");
    let bytes = std::fs::metadata(&binary)
        .map_err(|e| format!("cannot stat {}: {e}", binary.display()))?
        .len();

    let mib = bytes as f64 / (1024.0 * 1024.0);
    let budget_mib = MAX_BINARY_BYTES as f64 / (1024.0 * 1024.0);
    if bytes > MAX_BINARY_BYTES {
        return Err(format!(
            "release binary is {mib:.2} MiB, over the {budget_mib:.0} MiB budget"
        ));
    }

    println!("niobe release binary: {mib:.2} MiB of a {budget_mib:.0} MiB budget");
    Ok(bytes)
}

fn target_dir() -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => workspace_root().join("target"),
    }
}

pub(crate) fn workspace_root() -> PathBuf {
    // xtask lives at <root>/xtask, so the workspace root is one level up from its
    // manifest directory regardless of where cargo was invoked from.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The cargo that is running xtask, so a pinned toolchain stays pinned.
pub(crate) fn cargo_program() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

/// Runs cargo and captures its stdout, for the tasks that read the output
/// rather than just its exit status.
fn cargo_output(args: &[&str]) -> Result<String, String> {
    command_output(&cargo_program(), args)
}

/// Runs `program` in the workspace root and returns its stdout, failing on a
/// non-zero exit.
pub(crate) fn command_output(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(workspace_root())
        .output()
        .map_err(|e| format!("failed to run {program} {}: {e}", args.join(" ")))?;

    if !output.status.success() {
        return Err(format!(
            "{program} {} failed with {}:\n{}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    String::from_utf8(output.stdout).map_err(|e| {
        format!(
            "{program} {} produced non-UTF-8 output: {e}",
            args.join(" ")
        )
    })
}

fn cargo(args: &[&str]) -> Result<(), String> {
    let cargo = cargo_program();
    println!("$ cargo {}", args.join(" "));

    let status = Command::new(cargo)
        .args(args)
        .current_dir(workspace_root())
        .status()
        .map_err(|e| format!("failed to run cargo {}: {e}", args.join(" ")))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo {} failed with {status}", args.join(" ")))
    }
}
