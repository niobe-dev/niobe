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

use niobe_core::Owed;
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
    /// `usage` is one record as the backend reported it — one request, which
    /// is what a provider bills a long-context rate against — so an
    /// implementation may price it by its own prompt length.
    fn estimate(&self, usage: &Usage) -> Option<f64>;

    /// What one model's owed records would cost at published rates, in USD,
    /// each priced as the request it was, or `None` where any of them is not
    /// priced.
    ///
    /// Never the sum priced as one request: three 90K prompts are not one
    /// 270K prompt, and the difference is a long-context rate on every token.
    fn estimate_owed(&self, owed: &Owed) -> Option<f64> {
        owed.records()
            .iter()
            .try_fold(0.0, |sum, record| Some(sum + self.estimate(record)?))
    }

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
