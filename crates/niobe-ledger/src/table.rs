// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The price table: every model id Niobe can price, and what it cost when.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use niobe_core::Usage;

use crate::{Date, Price, PriceError, Provenance, parse};

/// The price table compiled into the binary.
const BUNDLED: &str = include_str!("../prices.toml");

/// The file name of a price table, bundled or the user's.
pub const FILE_NAME: &str = "prices.toml";

/// Where a model's prices were read from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Origin {
    /// The table compiled into the binary.
    Bundled,
    /// A file on disk.
    File(PathBuf),
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bundled => write!(f, "bundled {FILE_NAME}"),
            Self::File(path) => write!(f, "{}", path.display()),
        }
    }
}

/// Every price one model id has had, oldest first, and where they came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    pub(crate) prices: Vec<Price>,
    pub(crate) origin: Origin,
}

impl Schedule {
    /// The prices, oldest first, each in force from its date until the next
    /// one's. Never empty.
    pub fn prices(&self) -> &[Price] {
        &self.prices
    }

    /// The price in force on `day`: the latest one from on or before it.
    /// `None` before the first.
    pub fn at(&self, day: Date) -> Option<&Price> {
        self.prices.iter().rev().find(|price| price.from <= day)
    }

    /// Where the schedule was read from.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }
}

/// What a usage record cost, and how that figure was arrived at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cost {
    /// The record's tokens at the published rates of the day, in USD. Not a
    /// bill: a subscription or a negotiated discount charges something else.
    ApiEquivalent(f64),
    /// The table has no price for the model on that day.
    Unpriced,
}

impl Cost {
    /// How the figure was arrived at.
    pub fn provenance(&self) -> Provenance {
        match self {
            Self::ApiEquivalent(_) => Provenance::ApiEquivalent,
            Self::Unpriced => Provenance::Unpriced,
        }
    }

    /// The amount in USD, or `None` when there is no price to give one.
    pub fn usd(&self) -> Option<f64> {
        match self {
            Self::ApiEquivalent(usd) => Some(*usd),
            Self::Unpriced => None,
        }
    }
}

/// Model ids and their price schedules.
///
/// An id is matched exactly, as the backend reports it. Nothing is inferred
/// from an id that is not listed — not a date suffix, not a region prefix, not
/// a context-window marker — because a near match is a guess, and a guessed
/// price reads exactly like a real one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PriceTable {
    pub(crate) models: BTreeMap<String, Schedule>,
}

impl PriceTable {
    /// The table compiled into the binary.
    pub fn bundled() -> Result<Self, PriceError> {
        parse::table(BUNDLED, Origin::Bundled)
    }

    /// Parses a price table's text. `origin` is what errors and schedules name
    /// as its source.
    pub fn parse(text: &str, origin: Origin) -> Result<Self, PriceError> {
        parse::table(text, origin)
    }

    /// Reads the price file at `path`. A file that does not exist is `None`,
    /// not an error: a user's price file is optional.
    pub fn read(path: &Path) -> Result<Option<Self>, PriceError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, Origin::File(path.to_path_buf())).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(PriceError::Read {
                path: path.to_path_buf(),
                error,
            }),
        }
    }

    /// This table with `over` laid on top. Every id `over` lists takes its
    /// schedule from `over` whole, so an overriding file states a model's
    /// whole price history rather than patching one date of it.
    pub fn overlay(mut self, over: Self) -> Self {
        self.models.extend(over.models);
        self
    }

    /// Every id the table lists, sorted.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.models.keys().map(String::as_str)
    }

    /// The schedule of `model`, if the table lists it.
    pub fn schedule(&self, model: &str) -> Option<&Schedule> {
        self.models.get(model)
    }

    /// The price of `model` on `day`.
    pub fn price(&self, model: &str, day: Date) -> Option<&Price> {
        self.schedule(model)?.at(day)
    }

    /// What `usage` cost at the price of its model on `day`, the day the usage
    /// happened.
    pub fn cost(&self, usage: &Usage, day: Date) -> Cost {
        const PICODOLLARS_PER_DOLLAR: f64 = 1e12;
        match self.price(&usage.model, day).and_then(|p| p.cost(usage)) {
            // Exact until a single record costs more than about $9,000, and
            // within a fraction of a cent of it after.
            Some(picodollars) => Cost::ApiEquivalent(picodollars as f64 / PICODOLLARS_PER_DOLLAR),
            None => Cost::Unpriced,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: u16, month: u8, day: u8) -> Date {
        Date::new(year, month, day).expect("a real date")
    }

    const TABLE: &str = r#"
[[model]]
ids = ["m", "m-alias"]

[[model.price]]
from = 2026-01-10
input = 1
output = 2
cache_read = 0.1
cache_write = 1.25

[[model.price]]
from = 2026-02-01
input = 0.5
output = 1
cache_read = 0.05
cache_write = 0.625
"#;

    fn table() -> PriceTable {
        PriceTable::parse(TABLE, Origin::Bundled).expect("the table is valid")
    }

    #[test]
    fn the_price_of_a_day_is_the_latest_one_from_on_or_before_it() {
        let table = table();
        let input = |day| table.price("m", day).map(|p| p.rates().input().micros());
        assert_eq!(input(date(2026, 1, 9)), None);
        assert_eq!(input(date(2026, 1, 10)), Some(1_000_000));
        assert_eq!(input(date(2026, 1, 31)), Some(1_000_000));
        assert_eq!(input(date(2026, 2, 1)), Some(500_000));
        assert_eq!(input(date(2030, 1, 1)), Some(500_000));
    }

    #[test]
    fn every_id_of_a_model_has_its_schedule() {
        let table = table();
        assert_eq!(table.ids().collect::<Vec<_>>(), ["m", "m-alias"]);
        assert_eq!(table.schedule("m"), table.schedule("m-alias"));
    }

    #[test]
    fn an_overlay_replaces_the_whole_schedule_of_each_id_it_lists_and_no_other() {
        let path = PathBuf::from("/u/prices.toml");
        let user = PriceTable::parse(
            "[[model]]\nids = [\"m\", \"new\"]\n[[model.price]]\nfrom = 2026-03-01\ninput = 9\noutput = 9\ncache_read = 9\ncache_write = 9\n",
            Origin::File(path.clone()),
        )
        .expect("the user table is valid");
        let merged = table().overlay(user);

        let m = merged.schedule("m").expect("m is listed");
        assert_eq!(m.origin(), &Origin::File(path));
        assert_eq!(m.prices().len(), 1);
        // The January price was the bundled one; the user's replaces the
        // schedule, so before March there is none.
        assert!(merged.price("m", date(2026, 2, 15)).is_none());

        assert_eq!(
            merged.schedule("m-alias").map(Schedule::origin),
            Some(&Origin::Bundled)
        );
        assert!(merged.price("new", date(2026, 3, 1)).is_some());
    }

    #[test]
    fn a_cost_carries_its_provenance() {
        let usage = Usage {
            input: 1_000,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 0,
            model: "m".to_owned(),
            cost_usd: None,
            cost_basis: None,
        };
        let cost = table().cost(&usage, date(2026, 1, 10));
        assert_eq!(cost, Cost::ApiEquivalent(0.001));
        assert_eq!(cost.provenance(), Provenance::ApiEquivalent);

        let before = table().cost(&usage, date(2026, 1, 1));
        assert_eq!(before.provenance(), Provenance::Unpriced);
        assert_eq!(before.usd(), None);
    }

    #[test]
    fn a_missing_price_file_is_none_and_an_unreadable_one_is_an_error() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        assert!(matches!(
            PriceTable::read(&dir.path().join(FILE_NAME)),
            Ok(None)
        ));
        let error = PriceTable::read(dir.path()).expect_err("a directory is not a file");
        assert!(error.to_string().starts_with("cannot read "), "{error}");
    }

    #[test]
    fn origins_name_the_bundled_table_and_files_by_path() {
        assert_eq!(Origin::Bundled.to_string(), "bundled prices.toml");
        assert_eq!(
            Origin::File(PathBuf::from("/u/prices.toml")).to_string(),
            "/u/prices.toml"
        );
    }
}
