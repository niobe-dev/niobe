// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What keeps Niobe off the network and away from the CLIs' credentials.
//!
//! Niobe sends nothing anywhere: the official CLIs it drives are what talk to
//! the providers, signed in as they are. Two things would break that without
//! any test noticing — a crate that can open a connection or speak TLS
//! arriving in the dependency tree, and code that reads a CLI's own
//! credentials file or sets a user agent — so both are checked here, with the
//! crate graph, where CI fails on them.

/// Crates that exist to talk over the network: HTTP clients and servers,
/// TLS, WebSockets. None of them has a reason to be in a binary that makes no
/// network calls of its own.
pub(crate) const NETWORK_CRATES: &[&str] = &[
    "attohttpc",
    "awc",
    "curl",
    "curl-sys",
    "h2",
    "h3",
    "http-body",
    "hyper",
    "hyper-rustls",
    "hyper-tls",
    "hyper-util",
    "isahc",
    "native-tls",
    "openssl",
    "openssl-sys",
    "quinn",
    "reqwest",
    "rustls",
    "surf",
    "tokio-rustls",
    "tokio-tungstenite",
    "tungstenite",
    "ureq",
];

/// Text in source that reads a CLI's credentials or names the header a user
/// agent is set by. Comments are passed over: saying that Niobe never does
/// either is how the code documents it.
pub(crate) const CREDENTIAL_MARKS: &[&str] =
    &[".credentials.json", ".codex/auth.json", "User-Agent"];

/// The network crates in `tree`, the output of `cargo tree --prefix none
/// --format {p}`: one package per line, its name first.
pub(crate) fn network_crates_in(tree: &str) -> Vec<String> {
    let mut found: Vec<String> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| NETWORK_CRATES.contains(name))
        .map(str::to_owned)
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

/// Each line of `source`, a file at `path`, that is code and holds one of
/// the [`CREDENTIAL_MARKS`], as `path:line: text`.
pub(crate) fn credential_marks_in(path: &str, source: &str) -> Vec<String> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter(|(_, line)| CREDENTIAL_MARKS.iter().any(|mark| line.contains(mark)))
        .map(|(at, line)| format!("{path}:{}: {}", at + 1, line.trim()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `cargo tree --workspace -e normal,build --prefix none --format
    /// {p}` prints, cut down, with a client crate added under a leaf.
    const TREE: &str = "\
niobe-cli v0.12.0 (/repo/crates/niobe-cli)
crossterm v0.29.0
serde v1.0.228
ureq v3.1.2
rustls v0.23.31
rustls v0.23.31
niobe-core v0.12.0 (/repo/crates/niobe-core)
";

    #[test]
    fn a_network_crate_anywhere_in_the_tree_is_named_once() {
        assert_eq!(network_crates_in(TREE), ["rustls", "ureq"]);
    }

    #[test]
    fn a_tree_without_one_is_clean() {
        assert!(network_crates_in("niobe-cli v0.12.0\nserde v1.0.228\nmio v1.0.4\n").is_empty());
    }

    #[test]
    fn code_reading_a_clis_credentials_is_named_and_a_comment_saying_it_never_does_is_not() {
        let source = "\
//! Never reads ~/.claude/.credentials.json: the CLI does.
fn token() -> String {
    std::fs::read_to_string(home().join(\".claude/.credentials.json\")).unwrap_or_default()
}
    // No User-Agent is set.
let request = request.header(\"User-Agent\", \"claude-cli\");
";
        assert_eq!(
            credential_marks_in("src/lib.rs", source),
            [
                "src/lib.rs:3: std::fs::read_to_string(home().join(\".claude/.credentials.json\")).unwrap_or_default()",
                "src/lib.rs:6: let request = request.header(\"User-Agent\", \"claude-cli\");"
            ]
        );
    }
}
