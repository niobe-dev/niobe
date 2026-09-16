// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Bridge to the official `codex` CLI.
//!
//! Niobe drives the vendor binary as a subprocess and translates its output
//! into the shared event model. It never reads the CLI's credential files and
//! never sets its user agent: the official binary is the only thing that
//! touches subscription credentials.
//!
//! Not implemented yet: the crate names the binary it drives and does not spawn
//! it.

/// The binary this bridge drives, as looked up on `PATH`.
pub const BINARY: &str = "codex";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drives_the_official_binary() {
        assert_eq!(BINARY, "codex");
    }
}
