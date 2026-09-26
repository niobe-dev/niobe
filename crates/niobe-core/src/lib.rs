// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Shared vocabulary for every part of Niobe.
//!
//! The event model and session state live here so that a bridge, the ledger
//! and the TUI can agree on them without depending on one another. [`Event`]
//! is the one type every backend produces into and [`SessionState`] is the
//! fold every consumer derives from.

pub mod diff;
pub mod event;
pub mod permission;
pub mod session;
pub mod test_run;

pub use event::{
    AgentId, AgentOutcome, Backend, Billing, CheckpointId, Event, Mode, PermissionDecision,
    SessionMeta, ToolCallId, ToolOutcome, Usage,
};
pub use permission::{Allowlist, Rule, RuleError};
pub use session::{
    CheckpointRecord, DecisionRecord, FileChanges, Owed, SessionState, TestRunRecord, ToolTotals,
    Totals, TurnRecord,
};
pub use test_run::{FailedTests, TestCounts};

/// The name the binary is installed as, and the directory name used under
/// `~/.config` and in a repo.
pub const APP_NAME: &str = "niobe";

/// Version of the workspace, as compiled.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_name_is_the_binary_name() {
        assert_eq!(APP_NAME, "niobe");
    }
}
