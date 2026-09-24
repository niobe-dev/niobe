// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Valuing what a backend did not price.
//!
//! The `claude` CLI reports tokens per message and money once, at the end of a
//! turn. A turn can run for minutes, and for all of them the operator has a
//! token count and no figure — which is the one thing this product is for. So
//! the shell values the tokens no reported cost covers and shows the result,
//! marked as the estimate it is.
//!
//! The price table is not in this crate and may not be: `niobe-tui` depends on
//! `niobe-core` alone, and widening that to reach the ledger would put a
//! second crate between a bridge type and the shell. So the shell takes a
//! [`Prices`] and the CLI supplies one, which is the same seam the budget and
//! the repository name arrive through.

use niobe_core::event::Usage;

/// Values tokens a backend reported no cost for.
///
/// The one rule an implementation must keep: return `None` rather than a
/// figure for a model it has no price for. A guessed price is indistinguishable
/// from a read one, and [`crate::session_cost`] shows a total it cannot stand
/// behind as a floor instead.
pub trait Prices: std::fmt::Debug {
    /// What `usage` would cost at published rates, in USD, or `None` where the
    /// model is not priced.
    ///
    /// `usage` carries token counts and a model id; its `cost_usd` is always
    /// `None`, because it is what no reported cost accounts for.
    fn estimate(&self, usage: &Usage) -> Option<f64>;

    /// The size of `model`'s context window in tokens, as the provider
    /// published it, or `None` where the model's window is not known.
    ///
    /// The same rule as [`Prices::estimate`]: no window for a model the
    /// implementation does not list. The shell reads this only where the
    /// backend did not report a window of its own, and a model with neither
    /// is drawn as a size with no share of anything.
    fn context_window(&self, model: &str) -> Option<u64> {
        let _ = model;
        None
    }
}
