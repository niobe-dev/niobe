// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Token and cost accounting.
//!
//! Every number this crate produces is either derived from a `usage` field
//! reported by a backend or is labelled an estimate. An unknown model is
//! reported as `unpriced`, never guessed at.
//!
//! Only [`Provenance`] exists so far: the label every figure will carry. Pricing
//! and per-call attribution are not implemented yet.

/// How a cost figure was arrived at. Carried alongside every number the ledger
/// reports so that the UI can never present a guess as a measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Derived from a `usage` field the backend reported.
    Measured,
    /// Derived from a token count but priced against a rate card rather than a
    /// billed amount.
    ApiEquivalent,
    /// The model has no known price. Reported as `unpriced`.
    Unpriced,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_variants_are_distinct() {
        assert_ne!(Provenance::Measured, Provenance::ApiEquivalent);
        assert_ne!(Provenance::Measured, Provenance::Unpriced);
    }
}
