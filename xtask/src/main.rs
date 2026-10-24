// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Workspace automation. Run as `cargo xtask <task>`.
//!
//! The tasks here are the ones CI runs, so that "CI green" and "green on my
//! machine" mean the same thing.

#![allow(clippy::print_stdout, clippy::print_stderr)]

mod audit;
mod formula;
mod headers;
mod network;
mod version;

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
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
        Some("audit") => audit(),
        Some("version") => version::run(rest),
        Some("formula") => formula::run(rest),
        _ => {
            eprintln!(
                "\
usage: cargo xtask <task>

tasks:
    ci        fmt --check, clippy, tests, layering, headers, audit, versions, shellcheck, then
              size, with RUSTFLAGS denying warnings as CI does
    layering  check that no crate depends on a workspace crate it may not name, that no
              network crate is in the tree and that no code reads a CLI's credentials
    headers   check that every file carries the SPDX copyright header
    audit     check that no comment is a section banner, names planning outside the
              repository or marks a gap with FIXME, HACK or XXX
    version   report what a release would be; --check that the manifest agrees with itself;
              <patch|minor|major|X.Y.Z> to move the workspace to that version
    formula   print the Homebrew formula for <version> from the .sha256 files of a release in
              <directory>
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
    // CI sets `RUSTFLAGS: -D warnings` for every job, so a warning outside
    // clippy's reach — a build script's, a test target's — fails it there;
    // the local run has to refuse the same warnings to be the same gate.
    let rustflags = denying_warnings(std::env::var("RUSTFLAGS").ok().as_deref());
    let cargo = |args: &[&str]| cargo_with(args, Some(&rustflags));
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
    // The timed tests, each a binary of its own, in the optimised build that
    // ships: the debug suite reports them ignored.
    for (package, test) in [
        ("niobe-tui", "frame_budget"),
        ("niobe-store", "replay_budget"),
        ("niobe-cli", "replay_budget"),
    ] {
        cargo(&["test", "--release", "--package", package, "--test", test])?;
    }
    layering()?;
    headers()?;
    audit()?;
    version::run(&["--check".to_owned()])?;
    shellcheck()?;
    size().map(|_| ())
}

/// `flags` with `-D warnings` added, unless they already deny warnings.
fn denying_warnings(flags: Option<&str>) -> String {
    match flags.map(str::trim).filter(|flags| !flags.is_empty()) {
        Some(flags) if flags.contains("-D warnings") => flags.to_owned(),
        Some(flags) => format!("{flags} -D warnings"),
        None => "-D warnings".to_owned(),
    }
}

/// Lints `install.sh` as the POSIX `sh` it is run with. CI runs this on the
/// runner images, which have shellcheck; a developer's machine without it says
/// it skipped the check rather than failing a gate it cannot run. Where `CI`
/// is set the gate is the one that guards a merge, so a missing shellcheck
/// fails it: a check that is skipped silently is a check nobody notices gone.
fn shellcheck() -> Result<(), String> {
    println!("$ shellcheck -s sh install.sh");
    match Command::new("shellcheck")
        .args(["-s", "sh", "install.sh"])
        .current_dir(workspace_root())
        .status()
    {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("shellcheck -s sh install.sh failed with {status}")),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            if std::env::var_os("CI").is_some_and(|value| !value.is_empty()) {
                return Err("shellcheck is not installed, and CI must lint install.sh".to_owned());
            }
            println!("shellcheck is not installed: install.sh was not linted");
            Ok(())
        }
        Err(e) => Err(format!("failed to run shellcheck: {e}")),
    }
}

/// Checks every workspace crate's dependency tree against
/// [`ALLOWED_WORKSPACE_DEPS`].
fn layering() -> Result<(), String> {
    let members = cargo_output(&[
        "tree",
        "--workspace",
        "--depth",
        "0",
        "--prefix",
        "none",
        "--format",
        "{p}",
    ])?;
    let mut violations: Vec<String> = unlisted(&names_in(&members))
        .into_iter()
        .map(|member| {
            format!("{member} is a workspace member ALLOWED_WORKSPACE_DEPS does not list")
        })
        .collect();

    for (package, allowed) in ALLOWED_WORKSPACE_DEPS {
        for dependency in workspace_dependencies_of(package)? {
            if dependency != *package && !allowed.contains(&dependency.as_str()) {
                violations.push(format!("{package} depends on {dependency}"));
            }
        }
    }

    if !violations.is_empty() {
        return Err(format!(
            "forbidden dependency edges:\n  {}\nthe crate graph is what keeps backend-specific \
             types out of niobe-tui; widen ALLOWED_WORKSPACE_DEPS only on purpose",
            violations.join("\n  ")
        ));
    }
    println!(
        "layering: {} crates, no forbidden edges",
        ALLOWED_WORKSPACE_DEPS.len()
    );
    let member_paths: Vec<&str> = members.lines().filter_map(network::package_path).collect();
    off_the_network(&member_paths)
}

/// What every `cargo tree` the checks run reads: every feature switched on and
/// every target's dependencies, because an edge behind a feature or a
/// `cfg(windows)` is an edge the next build can take.
const WHOLE_TREE: &[&str] = &["--all-features", "--target", "all"];

/// The arguments of the `cargo tree` that lists every crate the product can
/// be built from.
fn product_tree_args() -> Vec<&'static str> {
    let mut args = vec!["tree", "--workspace"];
    args.extend(WHOLE_TREE);
    args.extend([
        "--edges",
        "normal,build",
        "--prefix",
        "none",
        "--format",
        "{p}",
    ]);
    args
}

/// Checks that nothing in the workspace can talk over the network or reach
/// for a CLI's credentials: see [`network`]. `members` are the directories of
/// the workspace's own crates.
fn off_the_network(members: &[&str]) -> Result<(), String> {
    let tree = cargo_output(&product_tree_args())?;
    let crates = network::network_crates_in(&tree);
    if !crates.is_empty() {
        return Err(format!(
            "network crates in the dependency tree: {}\nniobe makes no network calls of its \
             own; the official CLIs are what talk to the providers (AGENTS.md §2)",
            crates.join(", ")
        ));
    }

    let unlisted = network::unlisted_crates_in(&tree, members);
    if !unlisted.is_empty() {
        return Err(format!(
            "crates in the dependency tree that are not on the allowed list: {}\nadd each to \
             ALLOWED_CRATES in xtask/src/network.rs once you have read what it does, and say \
             why in the commit",
            unlisted.join(", ")
        ));
    }

    // Canonical, as `cargo tree` prints a package's directory: the root is
    // written with a `..` in it, which no directory starts with.
    let root = std::fs::canonicalize(workspace_root())
        .map_err(|e| format!("cannot find the workspace root: {e}"))?;
    let mut outside: Vec<&str> = tree
        .lines()
        .filter_map(network::package_path)
        .filter(|path| !Path::new(path).starts_with(&root))
        .collect();
    outside.sort_unstable();
    outside.dedup();
    if !outside.is_empty() {
        return Err(format!(
            "path crates outside the repository, whose source cannot be checked: {}",
            outside.join(", ")
        ));
    }

    let listing = published_files()?;
    let mut marks = Vec::new();
    for path in listing.split('\0').filter(|path| scanned_for_marks(path)) {
        let source = match std::fs::read_to_string(workspace_root().join(path)) {
            Ok(source) => source,
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot read {path}: {e}")),
        };
        marks.extend(network::credential_marks_in(path, &source));
        marks.extend(network::network_marks_in(path, &source));
    }
    if !marks.is_empty() {
        return Err(format!(
            "code that reads a CLI's credentials, sets a user agent or opens a connection:\n  \
             {}\nonly the official binaries touch their credentials or the network \
             (AGENTS.md §2)",
            marks.join("\n  ")
        ));
    }
    println!("network: no network crates, no credential reads");
    Ok(())
}

/// Checks every file git would publish — tracked, or untracked and not ignored
/// — for the header its type requires.
fn headers() -> Result<(), String> {
    let listing = published_files()?;

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

/// Checks every comment in the Rust files git would publish for a section
/// banner, a reference to planning outside the repository, or a `FIXME`,
/// `HACK` or `XXX`.
fn audit() -> Result<(), String> {
    let listing = published_files()?;

    let mut swept = 0;
    let mut marks = Vec::new();
    for path in listing.split('\0').filter(|path| audit::swept(path)) {
        let source = match std::fs::read_to_string(workspace_root().join(path)) {
            Ok(source) => source,
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot read {path}: {e}")),
        };
        swept += 1;
        marks.extend(audit::marks_in(path, &source));
    }

    if marks.is_empty() {
        println!("audit: {swept} files, no banner, planning reference or unexplained gap");
        return Ok(());
    }

    Err(format!(
        "comments a reader of the public repository must not find:\n  {}\n\
         describe what the code does and does not do, in plain words (AGENTS.md §4)",
        marks.join("\n  ")
    ))
}

/// Every file git would publish — tracked, or untracked and not ignored — as
/// paths relative to the workspace root, NUL-separated.
fn published_files() -> Result<String, String> {
    command_output(
        "git",
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )
}

/// Whether the file at `path`, relative to the workspace root, is Rust the
/// network check reads: every one in the repository — a crate vendored beside
/// the workspace and its build scripts included — but `xtask`'s, which is the
/// check, names every mark it looks for, and is built into nothing.
fn scanned_for_marks(path: &str) -> bool {
    path.ends_with(".rs") && !path.starts_with("xtask/")
}

/// The arguments of the `cargo tree` that lists every crate reachable from
/// `package`.
fn dependency_tree_args(package: &str) -> Vec<&str> {
    let mut args = vec!["tree", "--package", package];
    args.extend(WHOLE_TREE);
    args.extend([
        "--edges",
        "normal,build,dev",
        "--prefix",
        "none",
        "--format",
        "{p}",
    ]);
    args
}

/// Every `niobe-*` crate reachable from `package`, across normal, build and dev
/// edges, under every feature and on every target.
fn workspace_dependencies_of(package: &str) -> Result<Vec<String>, String> {
    let output = cargo_output(&dependency_tree_args(package))?;

    Ok(names_in(&output)
        .into_iter()
        .filter(|name| name.starts_with("niobe-"))
        .collect())
}

/// The package names in `tree`, the output of `cargo tree --prefix none
/// --format {p}`: one package per line, its name first. Sorted, once each.
fn names_in(tree: &str) -> Vec<String> {
    let mut found: Vec<String> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

/// The workspace members the layering table says nothing about, which would
/// otherwise never have their own edges checked. `xtask` is the tooling that
/// does the checking, and depends on no crate of the product.
fn unlisted(members: &[String]) -> Vec<String> {
    members
        .iter()
        .filter(|member| member.as_str() != "xtask")
        .filter(|member| {
            !ALLOWED_WORKSPACE_DEPS
                .iter()
                .any(|(listed, _)| listed == member)
        })
        .cloned()
        .collect()
}

/// Builds the release binary and returns its size in bytes, failing if it is
/// over budget.
fn size() -> Result<u64, String> {
    let manifest = std::fs::read_to_string(workspace_root().join("Cargo.toml"))
        .map_err(|e| format!("cannot read the workspace manifest: {e}"))?;
    release_unwinds(&manifest)?;
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

/// Fails unless the release profile in `manifest` lets a panic unwind.
///
/// The terminal is restored on a panic by a guard's `Drop`, and by a panic
/// hook the tests can only reach in a debug build: with `panic = "abort"` the
/// released binary would leave the operator's terminal in raw mode on every
/// panic while every test still passed.
fn release_unwinds(manifest: &str) -> Result<(), String> {
    match release_panic(manifest).as_deref() {
        None | Some("unwind") => Ok(()),
        Some(other) => Err(format!(
            "[profile.release] sets panic = \"{other}\"; it must unwind, or a panic leaves the \
             terminal in raw mode (AGENTS.md §2)"
        )),
    }
}

/// The `panic` setting of `[profile.release]` in `manifest`, unquoted, where
/// it sets one.
fn release_panic(manifest: &str) -> Option<String> {
    let mut in_release = false;
    for line in manifest.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.starts_with('[') {
            in_release = line == "[profile.release]";
            continue;
        }
        if !in_release {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == "panic"
        {
            return Some(value.trim().trim_matches('"').to_owned());
        }
    }
    None
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
    command_output(&cargo_program(), &uncoloured(args))
}

/// `args` with cargo told to write no colour. CI sets `CARGO_TERM_COLOR=always`
/// for its logs, and `cargo tree` colours the ` (*)` after a crate it printed
/// before, which a line read as text no longer ends in.
fn uncoloured<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut all = vec!["--color", "never"];
    all.extend_from_slice(args);
    all
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
    cargo_with(args, None)
}

/// Runs cargo with `RUSTFLAGS` set to `rustflags` where it is given, and
/// with the caller's environment otherwise.
fn cargo_with(args: &[&str], rustflags: Option<&str>) -> Result<(), String> {
    println!("$ cargo {}", args.join(" "));

    let mut command = Command::new(cargo_program());
    if let Some(rustflags) = rustflags {
        command.env("RUSTFLAGS", rustflags);
    }
    let status = command
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `args` holds `--all-features` and `--target all`.
    fn reads_the_whole_tree(args: &[&str]) -> bool {
        args.contains(&"--all-features") && args.windows(2).any(|pair| pair == ["--target", "all"])
    }

    #[test]
    fn a_dependency_behind_a_feature_or_another_target_is_in_the_layering_tree() {
        assert!(reads_the_whole_tree(&dependency_tree_args("niobe-tui")));
    }

    #[test]
    fn a_dependency_behind_a_feature_or_another_target_is_in_the_network_tree() {
        assert!(reads_the_whole_tree(&product_tree_args()));
    }

    #[test]
    fn a_build_script_and_a_vendored_crate_are_read_for_marks_and_the_checker_is_not() {
        assert!(scanned_for_marks("crates/niobe-cli/build.rs"));
        assert!(scanned_for_marks("vendor/telemetry/src/lib.rs"));
        assert!(!scanned_for_marks("xtask/src/network.rs"));
        assert!(!scanned_for_marks("vendor/telemetry/Cargo.toml"));
    }

    #[test]
    fn the_gate_denies_warnings_whatever_rustflags_already_hold() {
        assert_eq!(denying_warnings(None), "-D warnings");
        assert_eq!(denying_warnings(Some("  ")), "-D warnings");
        assert_eq!(
            denying_warnings(Some("-C target-cpu=native")),
            "-C target-cpu=native -D warnings"
        );
        assert_eq!(denying_warnings(Some("-D warnings")), "-D warnings");
    }

    /// What `cargo tree --prefix none --format {p}` prints, cut down.
    const TREE: &str = "\
niobe-tui v0.12.0 (/repo/crates/niobe-tui)
niobe-core v0.12.0 (/repo/crates/niobe-core)
ratatui v0.30.2
niobe-core v0.12.0 (/repo/crates/niobe-core)
";

    #[test]
    fn the_workspace_crates_a_tree_reaches_are_named_once() {
        let names = names_in(TREE);
        assert_eq!(names, ["niobe-core", "niobe-tui", "ratatui"]);
        let niobe: Vec<&String> = names.iter().filter(|n| n.starts_with("niobe-")).collect();
        assert_eq!(niobe, ["niobe-core", "niobe-tui"]);
    }

    #[test]
    fn the_cargo_output_read_as_text_is_asked_for_without_colour() {
        assert_eq!(
            uncoloured(&["tree", "--workspace"]),
            ["--color", "never", "tree", "--workspace"]
        );
    }

    #[test]
    fn a_member_the_layering_table_does_not_list_is_named() {
        let members = names_in("niobe-core v0.12.0\nniobe-new v0.12.0\nxtask v0.1.0\n");
        assert_eq!(unlisted(&members), ["niobe-new"]);
        assert!(unlisted(&names_in("niobe-cli v0.12.0\nxtask v0.1.0\n")).is_empty());
    }

    #[test]
    fn the_workspace_release_profile_unwinds() {
        let manifest = std::fs::read_to_string(workspace_root().join("Cargo.toml"))
            .expect("the workspace manifest is readable");

        assert_eq!(release_panic(&manifest).as_deref(), Some("unwind"));
        assert!(release_unwinds(&manifest).is_ok());
    }

    #[test]
    fn a_release_profile_that_aborts_is_refused() {
        let manifest = "[profile.dev]\npanic = \"unwind\"\n\n[profile.release]\nlto = \"thin\"\npanic = \"abort\" # smaller\n";

        assert_eq!(release_panic(manifest).as_deref(), Some("abort"));
        let said = release_unwinds(manifest).expect_err("abort is refused");
        assert!(said.contains("raw mode"), "{said}");
    }

    #[test]
    fn a_release_profile_that_says_nothing_unwinds_by_default() {
        let manifest = "[profile.release]\nlto = \"thin\"\n\n[profile.dev]\npanic = \"abort\"\n";

        assert_eq!(release_panic(manifest), None);
        assert!(release_unwinds(manifest).is_ok());
    }
}
