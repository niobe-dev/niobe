// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Token and cost accounting.
//!
//! Every number this crate produces is either derived from a `usage` field
//! reported by a backend or is labelled with how it was arrived at. An unknown
//! model is reported as `unpriced`, never guessed at.
//!
//! What exists so far is the price table: per-model rates with the date each
//! came into force, bundled into the binary as `prices.toml` and overridable
//! by the user, and the API-equivalent cost of a usage record priced against
//! it. The same file dates each model's context window, which is what a
//! session's context is measured against where the backend has not said.
//! Attributing costs to calls, files and sessions is not implemented yet.

mod date;
mod error;
mod parse;
mod price;
mod table;

pub use date::Date;
pub use error::PriceError;
pub use price::{LongContext, Price, Rate, Rates};
pub use table::{Cost, FILE_NAME, Origin, PriceTable, Schedule};

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
