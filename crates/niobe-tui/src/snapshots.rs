// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! When a snapshot test rewrites the picture it compares against.
//!
//! The shell's frames, and the Usage pane the CLI's billing tests draw, are
//! compared with pictures committed under `tests/snapshots/`. A test that
//! rewrites its picture instead passes whatever it drew, so the switch that
//! makes it do so is read strictly and in one place: `UPDATE_SNAPSHOTS=1` and
//! nothing else, and never on a CI runner, where a variable that leaked in
//! would turn every comparison into a pass.

use std::ffi::OsStr;

/// The variable that asks for the pictures to be rewritten.
pub const UPDATE: &str = "UPDATE_SNAPSHOTS";

/// Whether this run rewrites its snapshots rather than comparing against
/// them, from the process's environment. An error where it was asked to on
/// a CI runner, which the calling test is to fail with.
pub fn updating() -> Result<bool, String> {
    decide(
        std::env::var_os(UPDATE).as_deref(),
        std::env::var_os("CI").as_deref(),
    )
}

/// [`updating`], from the values of `UPDATE_SNAPSHOTS` and `CI`.
fn decide(update: Option<&OsStr>, ci: Option<&OsStr>) -> Result<bool, String> {
    if update != Some(OsStr::new("1")) {
        return Ok(false);
    }
    match ci.is_some_and(|ci| !ci.is_empty()) {
        true => Err(format!(
            "{UPDATE}=1 on a CI runner: snapshots are rewritten by hand, and their diff read, \
             never by CI"
        )),
        false => Ok(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decided(update: Option<&str>, ci: Option<&str>) -> Result<bool, String> {
        decide(update.map(OsStr::new), ci.map(OsStr::new))
    }

    #[test]
    fn only_a_one_rewrites_the_pictures() {
        assert_eq!(decided(Some("1"), None), Ok(true));
        for not_one in [None, Some(""), Some("0"), Some("true"), Some("yes")] {
            assert_eq!(decided(not_one, None), Ok(false), "{not_one:?}");
        }
    }

    #[test]
    fn a_ci_runner_never_rewrites_them() {
        let said = decided(Some("1"), Some("true")).expect_err("CI refuses the rewrite");
        assert!(said.contains("CI"), "{said}");
        assert_eq!(decided(Some("0"), Some("true")), Ok(false));
        assert_eq!(decided(Some("1"), Some("")), Ok(true));
    }
}
