// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The workspace version: where it is written, and how far the commits since
//! the last release tag move it.
//!
//! The version lives in eight places in the root manifest — once under
//! `[workspace.package]` and once per `niobe-*` path dependency — which is
//! seven chances to edit it inconsistently by hand. Everything here works from
//! the manifest text rather than a parsed document, so the maintainer's
//! comments, ordering and formatting survive a bump untouched.

use std::fmt;
use std::ops::Range;

/// A semantic version, as the workspace carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

/// How far a change moves the version.
///
/// Ordered, so that a release can refuse to bump less than the commits since
/// the last tag call for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bump {
    None,
    Patch,
    Minor,
    Major,
}

/// One place the workspace version is written in the root manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub what: String,
    pub line: usize,
    pub value: String,
    span: Range<usize>,
}

impl Version {
    /// Parses `major.minor.patch`. Nothing else: a pre-release or a build
    /// suffix would have to be ordered against a plain version, and the
    /// release tag, the manifest and the binary's `--version` all have to
    /// agree on one spelling.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut parts = text.split('.');
        let mut next = |what: &str| -> Result<u64, String> {
            parts
                .next()
                .ok_or_else(|| format!("{text:?} has no {what}"))?
                .parse::<u64>()
                .map_err(|_| format!("{text:?} has a {what} that is not a number"))
        };

        let major = next("major")?;
        let minor = next("minor")?;
        let patch = next("patch")?;
        if parts.next().is_some() {
            return Err(format!("{text:?} has more than three components"));
        }

        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    /// This version moved by `bump`, with everything below it cleared.
    pub fn bumped(self, bump: Bump) -> Self {
        match bump {
            Bump::None => self,
            Bump::Patch => Self {
                patch: self.patch.saturating_add(1),
                ..self
            },
            Bump::Minor => Self {
                major: self.major,
                minor: self.minor.saturating_add(1),
                patch: 0,
            },
            Bump::Major => Self {
                major: self.major.saturating_add(1),
                minor: 0,
                patch: 0,
            },
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl fmt::Display for Bump {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let word = match self {
            Bump::None => "none",
            Bump::Patch => "patch",
            Bump::Minor => "minor",
            Bump::Major => "major",
        };
        f.write_str(word)
    }
}

impl Bump {
    /// Parses the word a release asks for on the command line.
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "patch" => Some(Bump::Patch),
            "minor" => Some(Bump::Minor),
            "major" => Some(Bump::Major),
            _ => None,
        }
    }
}

/// What the conventional-commit subjects in `log` call for, in plain SemVer
/// terms. Records are separated by the ASCII record separator.
///
/// This is deliberately mechanical: the commit type the author already chose
/// decides the bump, so the release does not turn on one more judgement call
/// made from memory at the end of a session.
pub fn implied_bump(log: &str) -> Bump {
    commits_in(log)
        .into_iter()
        .map(|(bump, _)| bump)
        .max()
        .unwrap_or(Bump::None)
}

/// Each commit in `log`, as the bump it calls for and its subject line.
fn commits_in(log: &str) -> Vec<(Bump, String)> {
    log.split('\u{1e}')
        .map(str::trim)
        .filter(|commit| !commit.is_empty())
        .map(|commit| {
            let subject = commit.lines().next().unwrap_or_default().to_owned();
            (bump_of_commit(commit), subject)
        })
        .collect()
}

/// The bump one commit message calls for.
fn bump_of_commit(commit: &str) -> Bump {
    let mut lines = commit.lines();
    let Some(subject) = lines.next() else {
        return Bump::None;
    };

    let Some((prefix, _)) = subject.split_once(':') else {
        // Not a conventional commit: it says nothing about the version, and
        // guessing from prose is how a breaking change ships as a patch.
        return Bump::None;
    };

    // A trailer only breaks the API when it is the whole start of its own
    // line; prose that merely quotes the words is not a declaration.
    let breaking = prefix.ends_with('!')
        || lines.any(|line| {
            line.starts_with("BREAKING CHANGE:") || line.starts_with("BREAKING-CHANGE:")
        });
    if breaking {
        return Bump::Major;
    }

    let kind = prefix
        .trim_end_matches('!')
        .split_once('(')
        .map_or(prefix, |(kind, _)| kind);
    match kind.trim() {
        "feat" => Bump::Minor,
        "fix" | "perf" => Bump::Patch,
        _ => Bump::None,
    }
}

/// The bump [`implied_bump`] calls for, under the version line `current` is on.
///
/// Before 1.0 there is no major-version promise to break, and cargo treats
/// `0.MINOR` as the compatibility unit, so a breaking change moves the minor.
/// Reaching 1.0 is a decision about the product, never one a commit log makes.
pub fn required_bump(implied: Bump, current: Version) -> Bump {
    if current.major == 0 && implied == Bump::Major {
        Bump::Minor
    } else {
        implied
    }
}

/// Every place the workspace version is written in the root manifest: once
/// under `[workspace.package]`, once per `niobe-*` path dependency.
///
/// A third-party dependency pinned to the same number is left alone, which is
/// why this walks sections rather than replacing the version string wherever
/// it occurs.
pub fn version_sites(manifest: &str) -> Result<Vec<Site>, String> {
    let mut sites = Vec::new();
    let mut section = "";
    let mut offset = 0;
    let mut package_seen = false;

    for (number, line) in manifest.lines().enumerate() {
        let start = offset;
        offset += line.len() + 1;

        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed;
            continue;
        }

        let what = match section {
            "[workspace.package]" if trimmed.starts_with("version") => {
                package_seen = true;
                "[workspace.package]".to_owned()
            }
            "[workspace.dependencies]" if trimmed.starts_with("niobe-") => trimmed
                .split_once([' ', '='])
                .map_or(trimmed, |(key, _)| key)
                .to_owned(),
            _ => continue,
        };

        let span = literal_span(line)
            .ok_or_else(|| format!("line {}: {what} has no version literal: {line}", number + 1))?;
        sites.push(Site {
            what,
            line: number + 1,
            value: line[span.clone()].to_owned(),
            span: start + span.start..start + span.end,
        });
    }

    if !package_seen {
        return Err("the manifest has no version under [workspace.package]".to_owned());
    }

    Ok(sites)
}

/// The span of the string a `version = "…"` key is set to, within `line`.
fn literal_span(line: &str) -> Option<Range<usize>> {
    let key = line.find("version")?;
    let equals = key + line[key..].find('=')?;
    let open = equals + line[equals..].find('"')? + 1;
    let close = open + line[open..].find('"')?;
    Some(open..close)
}

/// The version the manifest is on, failing if its sites disagree.
pub fn current_version(manifest: &str) -> Result<Version, String> {
    let sites = version_sites(manifest)?;
    let (first, rest) = sites
        .split_first()
        .ok_or_else(|| "the manifest has no version sites".to_owned())?;

    let drifted: Vec<String> = rest
        .iter()
        .filter(|site| site.value != first.value)
        .map(|site| format!("line {}: {} is {}", site.line, site.what, site.value))
        .collect();
    if !drifted.is_empty() {
        return Err(format!(
            "the workspace version is {} but\n  {}\nevery site has to carry the same version; \
             run `cargo xtask version <bump>` rather than editing them by hand",
            first.value,
            drifted.join("\n  ")
        ));
    }

    Version::parse(&first.value)
}

/// The manifest with every version site set to `to`.
pub fn rewrite(manifest: &str, to: Version) -> Result<String, String> {
    current_version(manifest)?;

    let mut out = String::with_capacity(manifest.len());
    let mut cut = 0;
    for site in version_sites(manifest)? {
        out.push_str(&manifest[cut..site.span.start]);
        out.push_str(&to.to_string());
        cut = site.span.end;
    }
    out.push_str(&manifest[cut..]);
    Ok(out)
}

/// `cargo xtask version` — report what a release would be.
/// `cargo xtask version --check` — the manifest's version sites agree.
/// `cargo xtask version <patch|minor|major|X.Y.Z>` — move the workspace to it.
pub fn run(args: &[String]) -> Result<(), String> {
    match args {
        [] => report(),
        [flag] if flag == "--check" => check(),
        [target] => apply(target),
        _ => Err("usage: cargo xtask version [--check | <patch|minor|major|X.Y.Z>]".to_owned()),
    }
}

/// The root manifest's text.
fn manifest() -> Result<(std::path::PathBuf, String), String> {
    let path = crate::workspace_root().join("Cargo.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok((path, text))
}

/// Checks that every version site carries the same version. Cheap enough to
/// run in the gate, and it catches a hand-edited manifest before a release
/// builds a binary whose `--version` disagrees with its tag.
fn check() -> Result<(), String> {
    let (_, text) = manifest()?;
    let version = current_version(&text)?;
    let sites = version_sites(&text)?;
    println!(
        "version: {version} in all {} sites of Cargo.toml",
        sites.len()
    );
    Ok(())
}

/// What the commits since the last release tag call for.
struct Pending {
    current: Version,
    tag: Option<String>,
    commits: Vec<(Bump, String)>,
    required: Bump,
}

fn pending() -> Result<Pending, String> {
    let (_, text) = manifest()?;
    let current = current_version(&text)?;
    let tag = last_tag()?;

    let range = tag.as_deref().map(|tag| format!("{tag}..HEAD"));
    let mut log_args = vec!["log", "--format=%B%x1e"];
    if let Some(range) = range.as_deref() {
        log_args.push(range);
    }
    let log = git(&log_args)?;

    Ok(Pending {
        current,
        tag,
        required: required_bump(implied_bump(&log), current),
        commits: commits_in(&log),
    })
}

fn report() -> Result<(), String> {
    let pending = pending()?;
    let since = pending.tag.as_deref().unwrap_or("the first commit");

    println!("workspace version: {}", pending.current);
    println!("last release:      {since}");
    println!("commits since:     {}", pending.commits.len());
    for (bump, subject) in &pending.commits {
        let mark = if *bump == Bump::None {
            "     "
        } else {
            "  →  "
        };
        println!("  {bump:<5}{mark}{subject}");
    }

    if pending.required == Bump::None {
        println!("\nnothing since {since} calls for a release");
        return Ok(());
    }

    println!(
        "\nimplied bump:      {}\nnext version:      {}",
        pending.required,
        pending.current.bumped(pending.required)
    );
    println!("\na release is called for; the maintainer decides whether to cut it (AGENTS.md §8)");
    Ok(())
}

fn apply(target: &str) -> Result<(), String> {
    let pending = pending()?;
    let next = match Bump::parse(target) {
        Some(bump) => pending.current.bumped(bump),
        None => Version::parse(target)?,
    };

    if !git(&["status", "--porcelain"])?.trim().is_empty() {
        return Err(
            "the working tree has changes; a release commit carries Cargo.toml and Cargo.lock \
             and nothing else"
                .to_owned(),
        );
    }
    if next <= pending.current {
        return Err(format!(
            "{next} does not come after the current version {}",
            pending.current
        ));
    }

    let least = pending.current.bumped(pending.required);
    if next < least {
        return Err(format!(
            "the commits since {} call for a {} bump, so the next version is at least {least}, \
             not {next}; run `cargo xtask version` to see which commit calls for it",
            pending.tag.as_deref().unwrap_or("the first commit"),
            pending.required
        ));
    }

    let tag = format!("v{next}");
    if !git(&["tag", "--list", &tag])?.trim().is_empty() {
        return Err(format!("the tag {tag} already exists"));
    }

    let (path, text) = manifest()?;
    std::fs::write(&path, rewrite(&text, next)?)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;

    // Resolving the workspace rewrites Cargo.lock, which carries each crate's
    // version too, without compiling anything.
    crate::command_output(
        &crate::cargo_program(),
        &["metadata", "--format-version", "1"],
    )?;

    println!("{} → {next} in Cargo.toml and Cargo.lock", pending.current);
    println!(
        "\nnext:\n  git add Cargo.toml Cargo.lock\n  git commit -m \"chore: release {next}\"\n  \
         git tag -a {tag} -F <notes>\n  git push origin HEAD {tag}"
    );
    Ok(())
}

/// The newest `v*` tag by version order, if the repository has one.
fn last_tag() -> Result<Option<String>, String> {
    let tags = git(&["tag", "--list", "v*", "--sort=-v:refname"])?;
    Ok(tags.lines().next().map(str::to_owned))
}

fn git(args: &[&str]) -> Result<String, String> {
    crate::command_output("git", args)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"[workspace]
resolver = "3"
members = ["crates/niobe-core"]

[workspace.package]
version = "0.4.0"
edition = "2024"

[workspace.dependencies]
niobe-core = { path = "crates/niobe-core", version = "0.4.0" }
niobe-tui = { path = "crates/niobe-tui", version = "0.4.0" }

# A third-party pin that happens to read like the workspace version.
serde = { version = "0.4.0", features = ["derive"] }
"#;

    fn v(text: &str) -> Version {
        Version::parse(text).expect("the literal in the test is a valid version")
    }

    #[test]
    fn a_version_reads_back_as_it_was_written() {
        assert_eq!(v("1.12.3").to_string(), "1.12.3");
    }

    #[test]
    fn a_version_that_is_not_three_numbers_is_rejected() {
        for text in ["0.4", "0.4.0.1", "v0.4.0", "0.4.x", "", "0.4.0-rc1"] {
            assert!(Version::parse(text).is_err(), "{text} was accepted");
        }
    }

    #[test]
    fn a_bump_clears_the_components_below_it() {
        assert_eq!(v("0.4.3").bumped(Bump::Patch), v("0.4.4"));
        assert_eq!(v("0.4.3").bumped(Bump::Minor), v("0.5.0"));
        assert_eq!(v("0.4.3").bumped(Bump::Major), v("1.0.0"));
        assert_eq!(v("0.4.3").bumped(Bump::None), v("0.4.3"));
    }

    #[test]
    fn a_feature_moves_the_minor_and_a_fix_the_patch() {
        assert_eq!(implied_bump("fix: count the cache write once"), Bump::Patch);
        assert_eq!(
            implied_bump("perf: draw the strip in one pass"),
            Bump::Patch
        );
        assert_eq!(implied_bump("feat: add a second palette"), Bump::Minor);
    }

    #[test]
    fn a_release_that_only_touches_the_repository_moves_nothing() {
        let log = "docs: describe the bridge\u{1e}test: replay a recorded session\
                   \u{1e}chore: bump the toolchain\u{1e}refactor: extract the fold";
        assert_eq!(implied_bump(log), Bump::None);
    }

    #[test]
    fn the_largest_bump_in_the_log_wins() {
        let log = "fix: count the cache write once\u{1e}feat: add a second palette\
                   \u{1e}docs: describe the bridge";
        assert_eq!(implied_bump(log), Bump::Minor);
    }

    #[test]
    fn a_scope_does_not_hide_the_type() {
        assert_eq!(implied_bump("feat(tui): add a palette"), Bump::Minor);
        assert_eq!(
            implied_bump("fix(ledger): price a cache write"),
            Bump::Patch
        );
    }

    #[test]
    fn a_break_is_marked_by_a_bang_or_said_in_the_body() {
        assert_eq!(implied_bump("feat!: rename the profile key"), Bump::Major);
        assert_eq!(implied_bump("fix(config)!: drop the key"), Bump::Major);
        let body = "feat: rename the profile key\n\nBREAKING CHANGE: `backend` is now `runner`.";
        assert_eq!(implied_bump(body), Bump::Major);
    }

    #[test]
    fn a_body_that_merely_mentions_a_break_is_not_one() {
        let body = "docs: explain what a BREAKING CHANGE: trailer means";
        assert_eq!(implied_bump(body), Bump::None);
    }

    #[test]
    fn before_one_point_zero_a_break_moves_the_minor() {
        assert_eq!(required_bump(Bump::Major, v("0.4.0")), Bump::Minor);
        assert_eq!(required_bump(Bump::Minor, v("0.4.0")), Bump::Minor);
        assert_eq!(required_bump(Bump::Patch, v("0.4.0")), Bump::Patch);
    }

    #[test]
    fn after_one_point_zero_a_break_moves_the_major() {
        assert_eq!(required_bump(Bump::Major, v("1.2.0")), Bump::Major);
        assert_eq!(required_bump(Bump::Patch, v("1.2.0")), Bump::Patch);
    }

    #[test]
    fn every_workspace_version_site_is_found_and_no_other() {
        let sites = version_sites(MANIFEST).expect("the test manifest is well formed");
        let what: Vec<&str> = sites.iter().map(|s| s.what.as_str()).collect();
        assert_eq!(what, ["[workspace.package]", "niobe-core", "niobe-tui"]);
        assert!(sites.iter().all(|s| s.value == "0.4.0"));
    }

    #[test]
    fn a_rewrite_moves_every_site_and_leaves_the_rest_of_the_file_alone() {
        let out = rewrite(MANIFEST, v("0.5.0")).expect("the test manifest is well formed");
        assert!(out.contains("niobe-core = { path = \"crates/niobe-core\", version = \"0.5.0\" }"));
        assert!(out.contains("niobe-tui = { path = \"crates/niobe-tui\", version = \"0.5.0\" }"));
        assert!(out.contains("\n[workspace.package]\nversion = \"0.5.0\"\n"));
        // The third-party pin reads like the workspace version and must survive.
        assert!(out.contains("serde = { version = \"0.4.0\", features = [\"derive\"] }"));
        assert!(out.contains("# A third-party pin that happens to read like the workspace"));
        assert_eq!(out.matches("0.5.0").count(), 3);
    }

    #[test]
    fn a_manifest_whose_sites_disagree_is_reported_as_the_site_that_drifted() {
        let drifted = MANIFEST.replace(
            "niobe-tui = { path = \"crates/niobe-tui\", version = \"0.4.0\" }",
            "niobe-tui = { path = \"crates/niobe-tui\", version = \"0.3.0\" }",
        );
        let error = current_version(&drifted).expect_err("the sites disagree");
        assert!(error.contains("niobe-tui"), "{error}");
        assert!(error.contains("0.3.0"), "{error}");
    }

    #[test]
    fn a_manifest_with_no_workspace_package_version_is_rejected() {
        let without = MANIFEST.replace("version = \"0.4.0\"\nedition", "edition");
        assert!(version_sites(&without).is_err());
    }

    #[test]
    fn the_workspace_manifest_agrees_with_itself() {
        let manifest = include_str!("../../Cargo.toml");
        let version = current_version(manifest).expect("the workspace manifest agrees with itself");
        assert_eq!(version_sites(manifest).map(|s| s.len()), Ok(8));
        assert!(version.to_string().starts_with("0."));
    }
}
