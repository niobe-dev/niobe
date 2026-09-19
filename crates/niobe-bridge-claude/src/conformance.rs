// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Which releases of the `claude` CLI this bridge's message shapes come from.
//!
//! Everything the bridge knows about the protocol it learned by recording the
//! CLI and reading what it printed — there is no published schema it is
//! written against. That makes the release a recording came from part of what
//! the recording says, and a CLI outside those releases something the operator
//! has to be told about: a message type the bridge does not know becomes a
//! warning entry when it arrives, but a type that *moved* — a count that is
//! now on a different line, a field that changed meaning — arrives looking
//! exactly like one that did not.
//!
//! So the session says, once, when the CLI is outside what was recorded. It is
//! not a refusal: a bridge that stopped on an unrecognised release would make
//! every CLI upgrade an outage, and the shapes in practice survive a patch
//! release. It is the one thing the operator needs in order to read the
//! session's numbers with the right amount of trust.

/// The Claude Code releases the recordings in this crate's `tests/fixtures/`
/// were taken from.
///
/// Each is an exact release someone ran and recorded, newest last. A release
/// is added here only once a recording of it is checked in and the conformance
/// test over the recordings is green, which is what makes this list a claim
/// about evidence rather than about intent.
pub const RECORDED: &[&str] = &["2.1.275", "2.1.277", "2.1.278"];

/// Whether `version` is a release these recordings cover.
///
/// Covered means the same `major.minor` series as a recorded release, not the
/// same release. Measured: the twenty-eight 2.1.x releases whose transcripts
/// were on the machine this bridge was written on carry the same record types
/// and the same message shapes, so holding a patch bump against the operator
/// would be a warning on almost every session that said nothing.
///
/// A version string that is not `major.minor.…` is not covered: the bridge
/// cannot tell what it is looking at, which is exactly the case worth saying.
pub fn recorded(version: &str) -> bool {
    series(version).is_some_and(|series| RECORDED.iter().any(|known| series_of(known) == series))
}

/// What to tell the operator about a CLI outside [`RECORDED`].
pub(crate) fn unrecorded(version: &str) -> String {
    format!(
        "this session is running Claude Code {version}, and Niobe's reading of the protocol was \
         recorded from {}. Anything it sends that Niobe does not recognise is reported here and \
         is not counted; a figure that moved to another message would be counted wrong instead. \
         Check the numbers against `claude`'s own before trusting them.",
        RECORDED.join(", ")
    )
}

/// The `major.minor` of a version string, where it has one.
fn series(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// The `major.minor` of a release named in [`RECORDED`], which is well formed
/// by construction; an ill-formed entry matches nothing rather than matching
/// everything.
fn series_of(version: &str) -> (u32, u32) {
    series(version).unwrap_or((u32::MAX, u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_recorded_release_is_a_release_this_bridge_covers() {
        for version in RECORDED {
            assert!(recorded(version), "{version} is covered by itself");
        }
    }

    #[test]
    fn a_patch_release_of_a_recorded_series_is_covered() {
        assert!(recorded("2.1.0"));
        assert!(recorded("2.1.9999"));
    }

    #[test]
    fn a_release_outside_the_recorded_series_is_not_covered() {
        assert!(!recorded("2.2.0"));
        assert!(!recorded("3.0.1"));
        assert!(!recorded("1.9.9"));
    }

    #[test]
    fn a_version_this_bridge_cannot_read_is_not_covered() {
        assert!(!recorded(""));
        assert!(!recorded("2"));
        assert!(!recorded("nightly"));
    }

    #[test]
    fn the_warning_names_the_release_that_is_running_and_the_ones_recorded() {
        let said = unrecorded("2.2.0");
        assert!(said.contains("2.2.0"), "{said}");
        for version in RECORDED {
            assert!(said.contains(version), "{said}");
        }
    }
}
