// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The price table `niobe prices` prints: the bundled one with the user's
//! price file laid over it.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use niobe_ledger::{Date, FILE_NAME, Origin, Price, PriceTable, Rate, Rates, Schedule};

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

/// The days the tokens a sheet values were spent on.
///
/// What is owed is folded per model with no day on it — an event says what
/// happened, not when — so the sheet is told the days the session's usage
/// records were made on, and prices only what was priced the same on every
/// one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spent {
    /// From the first day to the last, both included, in UTC.
    Between(Date, Date),
    /// On days nothing recorded: a log with a record and no time on it.
    Undated,
}

impl Spent {
    /// One day: a session spending its tokens as it is watched.
    pub fn on(day: Date) -> Self {
        Self::Between(day, day)
    }

    /// The days of the usage records timed `times`, and of `until` too where
    /// the session goes on spending — a resumed session carried on today.
    ///
    /// One record with no time makes the whole session undated: a span read
    /// from the records that have one could leave out the day the others
    /// were spent on.
    pub fn of(times: impl IntoIterator<Item = Option<SystemTime>>, until: Option<Date>) -> Self {
        let mut span: Option<(Date, Date)> = until.map(|day| (day, day));
        for time in times {
            let Some(time) = time else {
                return Self::Undated;
            };
            let day = Date::of(time);
            span = Some(match span {
                Some((first, last)) => (first.min(day), last.max(day)),
                None => (day, day),
            });
        }
        match span {
            Some((first, last)) => Self::Between(first, last),
            // No usage at all, and none to come: nothing is owed, so nothing
            // is priced on any day.
            None => Self::Undated,
        }
    }
}

/// A price table read for the days a session spent its tokens on, as the
/// shell's price sheet.
///
/// This is the whole of what `niobe-tui` is told about prices: the crate graph
/// keeps the ledger out of the shell, so the shell takes a
/// [`niobe_tui::Prices`] and the binary supplies one.
///
/// A live session's day is fixed when it opens rather than read per frame. A
/// price that takes effect while a turn is in flight would otherwise move the
/// running figure under the operator mid-turn, and the turn's own result
/// settles it against the backend's figure in any case.
#[derive(Debug)]
pub struct Sheet {
    table: PriceTable,
    spent: Spent,
    today: Date,
}

impl Sheet {
    /// A sheet reading `table` at the prices in force on the days `spent`
    /// names, and a model's context window as it is `today`.
    pub fn new(table: PriceTable, spent: Spent, today: Date) -> Self {
        Self {
            table,
            spent,
            today,
        }
    }
}

impl niobe_tui::Prices for Sheet {
    fn estimate(&self, usage: &niobe_core::event::Usage) -> Option<f64> {
        // Undated tokens could have been spent on any day, the days before
        // the table priced the model among them, so they stay unpriced and
        // the total a floor, rather than valued at a day that was guessed.
        let Spent::Between(first, last) = self.spent else {
            return None;
        };
        // A schedule's prices only succeed one another, so the price on the
        // first day being the one on the last means it was in force on every
        // day between. Where it is not, the tokens were spent at rates the
        // sheet cannot tell apart, and they stay unpriced too.
        let opened = self.table.price(&usage.model, first)?;
        let closed = self.table.price(&usage.model, last)?;
        if opened.from() != closed.from() {
            return None;
        }
        // `usd()` is `None` for a model the table does not list, which is how
        // an unlisted model stays unpriced instead of being guessed at.
        self.table.cost(usage, last).usd()
    }

    fn context_window(&self, model: &str) -> Option<u64> {
        self.table.context_window(model, self.today)
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
    if let Some(path) = &user_file
        && let Some(user) = user_table(path)?
    {
        table = table.overlay(user);
    }
    Ok(Loaded { table, user_file })
}

/// The operator's price file at `path`, where there is one.
///
/// Read the way every config file is — without waiting on a FIFO, only a
/// regular file, and only up to a limit — so that a price file that is a
/// pipe or a link to a device is refused and named rather than read for ever.
fn user_table(path: &Path) -> Result<Option<PriceTable>, String> {
    let text = match niobe_config::read::text(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot read the price file {}: {error}",
                path.display()
            ));
        }
    };
    PriceTable::parse(&text, Origin::File(path.to_path_buf()))
        .map(Some)
        .map_err(|e| e.to_string())
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

    use std::time::{Duration, UNIX_EPOCH};

    use niobe_core::event::Usage;

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

    /// The sheet hands the shell the window of the day the session opened
    /// on, and none for a model the table does not list.
    #[test]
    fn the_sheet_reads_a_models_window_on_its_day() {
        use niobe_tui::Prices as _;

        let table = PriceTable::bundled().expect("the bundled table parses");
        let sheet = Sheet::new(table, Spent::Undated, date(2026, 3, 12));
        assert_eq!(sheet.context_window("claude-opus-4-6"), Some(200_000));
        assert_eq!(sheet.context_window("claude-opus-4-6[1m]"), Some(1_000_000));
        assert_eq!(sheet.context_window("not-a-model"), None);
    }

    fn opus_4_6(input: u64) -> Usage {
        Usage {
            input,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 0,
            model: "claude-opus-4-6".to_owned(),
            cost_usd: None,
            settles_model: false,
            fast: false,
        }
    }

    /// Claude Opus 4.6 billed a prompt over 200K at $10 per million input
    /// tokens until 13 March 2026 and at $5 from then on: a 250,000-token
    /// prompt is $2.50 on the 1st and $1.25 on the 20th.
    #[test]
    fn tokens_are_priced_at_the_rates_of_the_days_they_were_spent() {
        use niobe_tui::Prices as _;

        let at = |first, last| {
            Sheet::new(
                PriceTable::bundled().expect("the bundled table parses"),
                Spent::Between(first, last),
                date(2026, 10, 23),
            )
        };
        let prompt = opus_4_6(250_000);
        assert_eq!(
            at(date(2026, 3, 1), date(2026, 3, 2)).estimate(&prompt),
            Some(2.5)
        );
        assert_eq!(
            at(date(2026, 3, 20), date(2026, 10, 23)).estimate(&prompt),
            Some(1.25)
        );
        // Spent across the change, at rates the sheet cannot tell apart.
        assert_eq!(
            at(date(2026, 3, 1), date(2026, 3, 20)).estimate(&prompt),
            None
        );
        // Before the table prices the model at all.
        assert_eq!(
            at(date(2026, 1, 1), date(2026, 3, 1)).estimate(&prompt),
            None
        );
    }

    #[test]
    fn tokens_spent_on_days_nothing_recorded_are_not_priced() {
        use niobe_tui::Prices as _;

        let sheet = Sheet::new(
            PriceTable::bundled().expect("the bundled table parses"),
            Spent::Undated,
            date(2026, 10, 23),
        );
        assert_eq!(sheet.estimate(&opus_4_6(1_000)), None);
    }

    #[test]
    fn the_days_spent_run_from_the_first_record_to_the_last_or_to_today() {
        let day = |n: u64| Some(UNIX_EPOCH + Duration::from_secs(n * 86_400 + 3_600));
        assert_eq!(
            Spent::of([day(20_540), day(20_530), day(20_535)], None),
            Spent::Between(date(2026, 3, 18), date(2026, 3, 28))
        );
        assert_eq!(
            Spent::of([day(20_530)], Some(date(2026, 10, 23))),
            Spent::Between(date(2026, 3, 18), date(2026, 10, 23))
        );
        assert_eq!(Spent::of([day(20_530), None], None), Spent::Undated);
        assert_eq!(Spent::of([], None), Spent::Undated);
        assert_eq!(
            Spent::of([], Some(date(2026, 10, 23))),
            Spent::on(date(2026, 10, 23))
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

    #[cfg(unix)]
    #[test]
    fn a_price_file_that_is_a_pipe_or_a_device_is_refused_at_once_and_named() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let fifo = dir.path().join("fifo.toml");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo runs");
        assert!(made.success());
        let zero = dir.path().join("zero.toml");
        std::os::unix::fs::symlink("/dev/zero", &zero).expect("the link is made");

        for path in [fifo, zero] {
            let (sender, received) = std::sync::mpsc::channel();
            let read = path.clone();
            std::thread::spawn(move || sender.send(user_table(&read)));
            let said = received
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("the read returns without waiting on the file")
                .expect_err("not a file to read");
            assert!(said.contains(&path.display().to_string()), "{said}");
        }
        assert!(matches!(
            user_table(&dir.path().join("none.toml")),
            Ok(None)
        ));
    }
}
