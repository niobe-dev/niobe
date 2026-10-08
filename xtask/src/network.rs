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
/// {p}`, that are neither workspace members — whose directories `members`
/// lists, as `cargo tree` prints them — nor on [`ALLOWED_CRATES`].
///
/// A crate is the workspace's own by where it is, not by being a path
/// dependency: a crate vendored beside the workspace and depended on by path
/// is outside it, and is checked like one from a registry.
pub(crate) fn unlisted_crates_in(tree: &str, members: &[&str]) -> Vec<String> {
    let mut found: Vec<String> = tree
        .lines()
        .filter(|line| !package_path(line).is_some_and(|path| members.contains(&path)))
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| !ALLOWED_CRATES.contains(name))
        .map(str::to_owned)
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

/// The directory a line of `cargo tree --format {p}` names its package at,
/// where the package is a path one: `name vX.Y.Z (/dir)`, with ` (*)` after
/// it where the package was printed before.
pub(crate) fn package_path(line: &str) -> Option<&str> {
    let line = line.trim_end().trim_end_matches(" (*)");
    let open = line.rfind(" (/")?;
    line[open + " (".len()..].strip_suffix(')')
}

/// Text in the workspace's own source that opens a connection without any
/// crate — the standard library has one — or reads the macOS keychain, which
/// is where the CLI keeps its login there. Compared without case. Looked for
/// in what is compiled into the product, a crate's `src/` and its build
/// script: a test may use a socket to drive a pty.
pub(crate) const NETWORK_MARKS: &[&str] = &[
    "std::net",
    "TcpStream",
    "TcpListener",
    "UdpSocket",
    "ToSocketAddrs",
    "find-generic-password",
    "find-internet-password",
    "Keychain",
];

/// Programs that talk over the network, or read the keychain, which source
/// could run instead of opening a connection itself. Named where a
/// `Command::new` — or any `new(` — is given one as a literal; the same name
/// elsewhere, as a string compared against, is not running it.
pub(crate) const NETWORK_PROGRAMS: &[&str] = &[
    "curl", "wget", "nc", "ncat", "netcat", "socat", "telnet", "ssh", "scp", "sftp", "ftp",
    "rsync", "security",
];

/// Lines a mark is allowed on, each by the file it is in and the line's text,
/// trimmed: a test that builds the very path it proves is never read. An
/// entry is a decision made here, where it can be read, as an addition to
/// [`ALLOWED_CRATES`] is.
pub(crate) const ALLOWED_MARKS: &[(&str, &str)] = &[(
    // The memory files' test builds the credentials file's path to prove an
    // import naming it is listed and never read.
    "crates/niobe-bridge-claude/src/memory.rs",
    "[\".credentials\", \"json\"].join(\".\")",
)];

/// Each code line of `source`, a file at `path`, that holds one of the
/// [`NETWORK_MARKS`], spawns one of the [`NETWORK_PROGRAMS`] or opens a
/// `std::{…}` group that brings in `net`, as `path:line: text`. Only a file
/// that is compiled into the product is looked in: one under a `src`
/// directory, or a build script.
pub(crate) fn network_marks_in(path: &str, source: &str) -> Vec<String> {
    if !compiled_into_the_product(path) {
        return Vec::new();
    }
    let grouped = grouped_net_imports(source);
    marked_lines(path, source, |at, line| {
        let lower = line.to_ascii_lowercase();
        grouped.contains(&at)
            || NETWORK_MARKS
                .iter()
                .any(|mark| lower.contains(&mark.to_ascii_lowercase()))
            || spawns_a_network_program(line)
    })
}

/// Whether the file at `path` is built into what ships: a crate's sources,
/// under `src/`, or a build script, which runs on the machine that builds.
fn compiled_into_the_product(path: &str) -> bool {
    let mut components = path.split('/');
    let name = path.rsplit('/').next().unwrap_or(path);
    name == "build.rs" || components.any(|component| component == "src")
}

/// Whether `line` hands one of the [`NETWORK_PROGRAMS`] to a constructor as
/// a literal, `new("curl")` with any spacing inside the parentheses.
fn spawns_a_network_program(line: &str) -> bool {
    line.match_indices("new(").any(|(at, open)| {
        let rest = line[at + open.len()..].trim_start();
        NETWORK_PROGRAMS.iter().any(|program| {
            rest.strip_prefix('"')
                .and_then(|rest| rest.strip_prefix(program))
                .is_some_and(|rest| rest.starts_with('"'))
        })
    })
}

/// The zero-based lines of `source` that open a `std::{…}` group with `net`
/// — or `net::…`, or `net as …` — directly inside it, which brings the
/// sockets in without `std::net` ever being written. The group may run over
/// several lines; the line it opens on is the one named.
fn grouped_net_imports(source: &str) -> Vec<usize> {
    let mut found = Vec::new();
    for (start, _) in source.match_indices("std::{") {
        let group = &source[start + "std::{".len()..];
        if group_names_net(group) {
            found.push(source[..start].matches('\n').count());
        }
    }
    found
}

/// Whether the group that `group` opens, the text after its `{`, has `net` as
/// one of its own items rather than inside a nested group.
fn group_names_net(group: &str) -> bool {
    let mut depth = 0_usize;
    let mut item = String::new();
    for c in group.chars() {
        match c {
            '{' => depth = depth.saturating_add(1),
            '}' if depth == 0 => return item_is_net(&item),
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                if item_is_net(&item) {
                    return true;
                }
                item.clear();
            }
            _ if depth == 0 => item.push(c),
            _ => {}
        }
    }
    false
}

fn item_is_net(item: &str) -> bool {
    let item = item.trim();
    item == "net" || item.starts_with("net::") || item.starts_with("net as ")
}

/// Text in source that reads a CLI's credentials or names the header a user
/// agent is set by, compared without case: a header's name has none.
/// Comments are passed over: saying that Niobe never does either is how the
/// code documents it.
pub(crate) const CREDENTIAL_MARKS: &[&str] = &[
    ".credentials.json",
    "\".credentials",
    "auth.json",
    "user-agent",
    "user_agent",
];

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
    marked_lines(path, source, |_, line| {
        let lower = line.to_ascii_lowercase();
        CREDENTIAL_MARKS.iter().any(|mark| lower.contains(mark))
    })
}

/// Each line of `source` that is code, is not on [`ALLOWED_MARKS`] for
/// `path`, and that `marked` holds of — given its zero-based number and its
/// text — as `path:line: text`.
fn marked_lines(path: &str, source: &str, marked: impl Fn(usize, &str) -> bool) -> Vec<String> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter(|(_, line)| !ALLOWED_MARKS.contains(&(path, line.trim())))
        .filter(|(at, line)| marked(*at, line))
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
        assert_eq!(
            unlisted_crates_in(tree, &["/repo/crates/niobe-cli"]),
            ["minreq"]
        );
    }

    #[test]
    fn a_path_crate_outside_the_workspace_is_named_like_any_other() {
        let tree = "niobe-cli v0.12.0 (/repo/crates/niobe-cli)
telemetry v0.1.0 (/repo/vendor/telemetry)
niobe-core v0.12.0 (/repo/crates/niobe-core) (*)
";
        assert_eq!(
            unlisted_crates_in(tree, &["/repo/crates/niobe-cli", "/repo/crates/niobe-core"]),
            ["telemetry"]
        );
    }

    #[test]
    fn a_listening_socket_is_named() {
        let source = "let server = TcpListener::bind(\"0.0.0.0:0\")?;\n";
        assert_eq!(network_marks_in("crates/x/src/lib.rs", source).len(), 1);
    }

    #[test]
    fn an_address_resolved_for_a_connection_is_named() {
        let source = "let to = (host, 443).to_socket_addrs()?;\nfn f(a: impl ToSocketAddrs) {}\n";
        assert_eq!(network_marks_in("crates/x/src/lib.rs", source).len(), 1);
    }

    #[test]
    fn the_network_module_imported_in_a_group_is_named() {
        let source = "use std::{io::Write, net};
use std::{
    fs,
    net as sockets,
};
use std::{io::{self, Read}, path::Path};
";
        assert_eq!(
            network_marks_in("crates/x/src/lib.rs", source),
            [
                "crates/x/src/lib.rs:1: use std::{io::Write, net};",
                "crates/x/src/lib.rs:2: use std::{",
            ]
        );
    }

    #[test]
    fn a_program_that_talks_over_the_network_spawned_from_source_is_named() {
        let source = "Command::new(\"curl\").arg(\"https://telemetry.example\");
Command::new( \"ssh\" );
let kind = match program { \"curl\" | \"wget\" => Web, _ => Other };
";
        assert_eq!(
            network_marks_in("crates/x/src/lib.rs", source),
            [
                "crates/x/src/lib.rs:1: Command::new(\"curl\").arg(\"https://telemetry.example\");",
                "crates/x/src/lib.rs:2: Command::new( \"ssh\" );",
            ]
        );
    }

    #[test]
    fn the_keychain_read_through_the_security_program_is_named() {
        let source = "Command::new(\"security\").args([\"dump-keychain\"]);\n";
        assert_eq!(network_marks_in("crates/x/src/lib.rs", source).len(), 1);
    }

    #[test]
    fn a_build_script_is_looked_in_as_the_source_is() {
        let source = "fn main() { std::net::TcpStream::connect(\"10.0.0.1:80\").ok(); }\n";
        assert_eq!(network_marks_in("crates/x/build.rs", source).len(), 1);
        assert_eq!(network_marks_in("build.rs", source).len(), 1);
        assert_eq!(network_marks_in("src/main.rs", source).len(), 1);
    }

    #[test]
    fn a_credentials_file_named_in_pieces_is_named() {
        let source =
            "let token = home.join(\".claude\").join(\".credentials\").with_extension(\"json\");
let codex = home.join(\".codex\").join(\"auth.json\");
";
        assert_eq!(credential_marks_in("src/lib.rs", source).len(), 2);
    }

    #[test]
    fn a_user_agent_header_is_named_however_it_is_cased() {
        let source = "request.header(\"user-agent\", \"x\");
request.header(\"USER-AGENT\", \"x\");
agent.user_agent(\"x\");
";
        assert_eq!(credential_marks_in("src/lib.rs", source).len(), 3);
    }

    #[test]
    fn a_mark_the_allowed_list_names_at_its_file_is_passed_over_and_elsewhere_is_not() {
        let (path, line) = ALLOWED_MARKS[0];
        let source = format!("    {line}\n");
        assert!(credential_marks_in(path, &source).is_empty());
        assert_eq!(credential_marks_in("crates/x/src/lib.rs", &source).len(), 1);
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
