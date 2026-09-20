// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The price table `niobe prices` prints: the bundled one with the user's
//! price file laid over it.

use std::path::PathBuf;

use niobe_ledger::{Date, FILE_NAME, Price, PriceTable, Rate, Rates, Schedule};

use crate::config;

/// The price table, and the user's price file whether or not it exists.
#[derive(Debug)]
pub struct Loaded {
    /// The bundled table with the user's file laid over it.
    pub table: PriceTable,
    /// Where the user's price file is looked for, if there is anywhere.
    pub user_file: Option<PathBuf>,
}

impl Loaded {
    /// Why `model` has no price, naming every table looked in.
    pub fn unpriced(&self, model: &str) -> String {
        let looked_in = match &self.user_file {
            Some(path) => format!(
                "neither the bundled price table nor {} lists it",
                path.display()
            ),
            None => "the bundled price table does not list it".to_owned(),
        };
        format!("`{model}` is unpriced: {looked_in}")
    }
}

/// A price table read for one day, as the shell's price sheet.
///
/// This is the whole of what `niobe-tui` is told about prices: the crate graph
/// keeps the ledger out of the shell, so the shell takes a
/// [`niobe_tui::Prices`] and the binary supplies one.
///
/// The day is fixed when the session opens rather than read per frame. A price
/// that takes effect while a turn is in flight would otherwise move the
/// running figure under the operator mid-turn, and the turn's own result
/// settles it against the backend's figure in any case.
#[derive(Debug)]
pub struct Sheet {
    table: PriceTable,
    day: Date,
}

impl Sheet {
    /// A sheet reading `table` at the prices in force on `day`.
    pub fn new(table: PriceTable, day: Date) -> Self {
        Self { table, day }
    }
}

impl niobe_tui::Prices for Sheet {
    fn estimate(&self, usage: &niobe_core::event::Usage) -> Option<f64> {
        // `usd()` is `None` for a model the table does not list, which is how
        // an unlisted model stays unpriced instead of being guessed at.
        self.table.cost(usage, self.day).usd()
    }
}

/// Reads the bundled table and the user's price file, in
/// `$XDG_CONFIG_HOME/niobe/prices.toml` or `~/.config/niobe/prices.toml`.
pub fn load() -> Result<Loaded, String> {
    let user_file = config::user_file(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
        FILE_NAME,
    );
    let mut table = PriceTable::bundled().map_err(|e| e.to_string())?;
    if let Some(path) = &user_file {
        if let Some(user) = PriceTable::read(path).map_err(|e| e.to_string())? {
            table = table.overlay(user);
        }
    }
    Ok(Loaded { table, user_file })
}

/// One row per model id, at the price in force on `today`, with the date that
/// price took effect and the table it came from. A long-context tier is a row
/// of its own under its model.
pub fn table(table: &PriceTable, today: Date) -> Vec<String> {
    let rows: Vec<(&str, &Schedule)> = table
        .ids()
        .filter_map(|id| table.schedule(id).map(|schedule| (id, schedule)))
        .collect();
    let width = rows
        .iter()
        .map(|(id, _)| id.chars().count())
        .chain(rows.iter().flat_map(|(_, schedule)| {
            schedule
                .at(today)
                .and_then(Price::long_context)
                .map(|long| long_label(long.above()).chars().count())
        }))
        .max()
        .unwrap_or(0)
        .max("MODEL".len());

    let mut lines = vec![
        format!("USD per million tokens, in force on {today}. A model not listed is unpriced."),
        format!("{}  {:<10}  SOURCE", header("MODEL", width), "SINCE"),
    ];
    for (id, schedule) in rows {
        let origin = schedule.origin();
        match schedule.at(today) {
            Some(price) => {
                lines.push(format!(
                    "{}  {}  {origin}",
                    row(id, width, Some(price.rates())),
                    price.from()
                ));
                if let Some(long) = price.long_context() {
                    lines.push(row(&long_label(long.above()), width, Some(long.rates())));
                }
            }
            None => lines.push(format!("{}  {:<10}  {origin}", row(id, width, None), "—")),
        }
    }
    lines
}

/// Every price `model` has had, oldest first, with the rates of each and the
/// table they came from.
pub fn schedule(model: &str, schedule: &Schedule) -> Vec<String> {
    const DATE_WIDTH: usize = 10;
    let width = schedule
        .prices()
        .iter()
        .filter_map(|price| price.long_context())
        .map(|long| long_label(long.above()).chars().count())
        .max()
        .unwrap_or(0)
        .max(DATE_WIDTH);

    let mut lines = vec![
        format!(
            "{model}, USD per million tokens, from {}",
            schedule.origin()
        ),
        header("SINCE", width),
    ];
    for price in schedule.prices() {
        lines.push(row(&price.from().to_string(), width, Some(price.rates())));
        if let Some(long) = price.long_context() {
            lines.push(row(&long_label(long.above()), width, Some(long.rates())));
        }
    }
    lines
}

/// The column headings after a first column `width` wide.
fn header(first: &str, width: usize) -> String {
    format!("{first:<width$}  INPUT  OUTPUT  CACHE READ  CACHE WRITE  1H WRITE")
}

/// A label and the rates under the headings. Missing rates read as an em dash.
fn row(label: &str, width: usize, rates: Option<&Rates>) -> String {
    let cell = |rate: Option<Rate>| rate.map_or_else(|| "—".to_owned(), |rate| rate.to_string());
    format!(
        "{label:<width$}  {:>5}  {:>6}  {:>10}  {:>11}  {:>8}",
        cell(rates.map(Rates::input)),
        cell(rates.map(Rates::output)),
        cell(rates.map(Rates::cache_read)),
        cell(rates.map(Rates::cache_write)),
        cell(rates.and_then(Rates::cache_write_1h)),
    )
}

fn long_label(above: u64) -> String {
    format!("  prompt over {above} tokens")
}

#[cfg(test)]
mod tests {
    use niobe_ledger::Origin;

    use super::*;

    const PRICES: &str = r#"
[[model]]
ids = ["future-model"]

[[model.price]]
from = 2027-01-01
input = 1
output = 2
cache_read = 0.1
cache_write = 1.25
"#;

    fn date(year: u16, month: u8, day: u8) -> Date {
        Date::new(year, month, day).expect("a real date")
    }

    #[test]
    fn a_model_with_no_price_in_force_yet_reads_as_dashes_not_zeros() {
        let table = PriceTable::parse(PRICES, Origin::File("/u/prices.toml".into()))
            .expect("the table is valid");
        assert_eq!(
            super::table(&table, date(2026, 9, 16)),
            [
                "USD per million tokens, in force on 2026-09-16. A model not listed is unpriced.",
                "MODEL         INPUT  OUTPUT  CACHE READ  CACHE WRITE  1H WRITE  SINCE       SOURCE",
                "future-model      —       —           —            —         —  —           /u/prices.toml",
            ]
        );
    }

    #[test]
    fn with_no_user_file_to_look_in_only_the_bundled_table_is_named() {
        let loaded = Loaded {
            table: PriceTable::default(),
            user_file: None,
        };
        assert_eq!(
            loaded.unpriced("m"),
            "`m` is unpriced: the bundled price table does not list it"
        );
    }
}
