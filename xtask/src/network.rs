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

/// Every crate the dependency tree may hold, on any target, outside the
/// workspace's own. A crate not on it fails the check whatever it is: a
/// list of what is known to talk over the network misses every client
/// nobody thought to write down, and a new dependency is a decision to be
/// made on purpose, here, where it can be read.
pub(crate) const ALLOWED_CRATES: &[&str] = &[
    "allocator-api2",
    "bitflags",
    "block-buffer",
    "castaway",
    "cc",
    "cfg-if",
    "compact_str",
    "convert_case",
    "cpufeatures",
    "critical-section",
    "crossterm",
    "crossterm_winapi",
    "crypto-common",
    "darling",
    "darling_core",
    "darling_macro",
    "deranged",
    "derive_more",
    "derive_more-impl",
    "digest",
    "document-features",
    "either",
    "equivalent",
    "errno",
    "fallible-iterator",
    "fallible-streaming-iterator",
    "find-msvc-tools",
    "fnv",
    "foldhash",
    "generic-array",
    "hashbrown",
    "heck",
    "ident_case",
    "indoc",
    "instability",
    "itertools",
    "itoa",
    "jiff",
    "jiff-core",
    "jiff-static",
    "kasuari",
    "libc",
    "libsqlite3-sys",
    "line-clipping",
    "linux-raw-sys",
    "litrs",
    "lock_api",
    "log",
    "lru",
    "memchr",
    "mio",
    "num_threads",
    "num-conv",
    "parking_lot",
    "parking_lot_core",
    "pkg-config",
    "portable-atomic",
    "portable-atomic-util",
    "powerfmt",
    "proc-macro2",
    "pulldown-cmark",
    "quote",
    "ratatui",
    "ratatui-core",
    "ratatui-crossterm",
    "ratatui-macros",
    "ratatui-textarea",
    "ratatui-widgets",
    "redox_syscall",
    "rusqlite",
    "rustc_version",
    "rustix",
    "rustversion",
    "ryu",
    "scopeguard",
    "semver",
    "serde",
    "serde_core",
    "serde_derive",
    "serde_json",
    "serde_spanned",
    "sha2",
    "shlex",
    "signal-hook",
    "signal-hook-mio",
    "signal-hook-registry",
    "smallvec",
    "static_assertions",
    "strsim",
    "strum",
    "strum_macros",
    "syn",
    "thiserror",
    "thiserror-impl",
    "time",
    "time-core",
    "toml",
    "toml_datetime",
    "toml_parser",
    "typenum",
    "unicase",
    "unicode-ident",
    "unicode-segmentation",
    "unicode-truncate",
    "unicode-width",
    "vcpkg",
    "version_check",
    "wasi",
    "winapi",
    "winapi-i686-pc-windows-gnu",
    "winapi-x86_64-pc-windows-gnu",
    "windows-link",
    "windows-sys",
    "winnow",
    "zmij",
];

/// The crates in `tree`, the output of `cargo tree --prefix none --format
/// {p}`, that are neither the workspace's own nor on [`ALLOWED_CRATES`].
pub(crate) fn unlisted_crates_in(tree: &str) -> Vec<String> {
    let mut found: Vec<String> = tree
        .lines()
        .filter(|line| !line.contains(" (/"))
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| !ALLOWED_CRATES.contains(name))
        .map(str::to_owned)
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

/// Text in the workspace's own source that opens a connection without any
/// crate — the standard library has one — or reads the macOS keychain, which
/// is where the CLI keeps its login there. Looked for in `src/` only: a test
/// may use a socket to drive a pty.
pub(crate) const NETWORK_MARKS: &[&str] = &[
    "std::net",
    "TcpStream",
    "UdpSocket",
    "find-generic-password",
    "Keychain",
];

/// Each code line of `source`, a file at `path` under a crate's `src/`,
/// that holds one of the [`NETWORK_MARKS`], as `path:line: text`.
pub(crate) fn network_marks_in(path: &str, source: &str) -> Vec<String> {
    if !path.contains("/src/") {
        return Vec::new();
    }
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter(|(_, line)| NETWORK_MARKS.iter().any(|mark| line.contains(mark)))
        .map(|(at, line)| format!("{path}:{}: {}", at + 1, line.trim()))
        .collect()
}

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
    fn a_crate_nobody_allowed_is_named_and_the_workspaces_own_are_not() {
        let tree = "niobe-cli v0.12.0 (/repo/crates/niobe-cli)
serde v1.0.228
minreq v2.12.0
";
        assert_eq!(unlisted_crates_in(tree), ["minreq"]);
    }

    #[test]
    fn a_socket_in_the_workspaces_source_is_named_and_one_in_a_test_is_not() {
        let source = "use std::net::TcpStream;
// std::net is never used
";
        assert_eq!(
            network_marks_in("crates/x/src/lib.rs", source),
            ["crates/x/src/lib.rs:1: use std::net::TcpStream;"]
        );
        assert!(network_marks_in("crates/x/tests/pty.rs", source).is_empty());
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
