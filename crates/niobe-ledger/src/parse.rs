// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A price table's text, walked into a [`PriceTable`].
//!
//! The document is parsed into TOML's spanned tables and walked by hand rather
//! than deserialized, so that an error names the key and the line, and so that
//! a price is read from its decimal text instead of through a float: `0.175`
//! stays exactly 175,000 millionths.

use std::collections::BTreeMap;
use std::ops::Range;

use toml::Spanned;
use toml::de::{DeString, DeTable, DeValue};

use crate::{Date, LongContext, Origin, Price, PriceError, PriceTable, Rate, Rates, Schedule};

type Entry<'t, 'i> = (&'t Spanned<DeString<'i>>, &'t Spanned<DeValue<'i>>);

/// Parses a price table whose text came from `origin`.
pub(crate) fn table(text: &str, origin: Origin) -> Result<PriceTable, PriceError> {
    let file = File { text, origin };
    let root = DeTable::parse(text).map_err(|error| file.syntax(&error))?;
    file.root(root.get_ref())
}

/// The text being walked, which every error is reported against.
struct File<'a> {
    text: &'a str,
    origin: Origin,
}

/// A model as its entry in the file declares it.
struct Model {
    ids: Vec<Spanned<String>>,
    prices: Vec<Price>,
}

impl File<'_> {
    fn root(&self, root: &DeTable<'_>) -> Result<PriceTable, PriceError> {
        let mut models: BTreeMap<String, Schedule> = BTreeMap::new();
        for (key, value) in in_file_order(root) {
            let at = Key::root(key.get_ref());
            if key.get_ref() != "model" {
                return Err(self.invalid(&key.span(), &at, "unknown key; expected `model`"));
            }
            for (index, entry) in self.array(value, &at)?.iter().enumerate() {
                let model = self.model(entry, &at.index(index))?;
                let schedule = Schedule {
                    prices: model.prices,
                    origin: self.origin.clone(),
                };
                for id in model.ids {
                    if models.contains_key(id.get_ref()) {
                        return Err(self.invalid(
                            &id.span(),
                            &at.index(index).child("ids"),
                            &format!("`{}` is listed by an earlier model", id.get_ref()),
                        ));
                    }
                    models.insert(id.into_inner(), schedule.clone());
                }
            }
        }
        Ok(PriceTable { models })
    }

    fn model(&self, value: &Spanned<DeValue<'_>>, at: &Key) -> Result<Model, PriceError> {
        let mut ids = None;
        let mut prices = None;
        for (key, value) in in_file_order(self.table(value, at)?) {
            let at = at.child(key.get_ref());
            match key.get_ref().as_ref() {
                "ids" => ids = Some(self.ids(value, &at)?),
                "price" => prices = Some(self.prices(value, &at)?),
                _ => {
                    return Err(self.invalid(
                        &key.span(),
                        &at,
                        "unknown key; expected `ids` or `price`",
                    ));
                }
            }
        }
        let ids = ids.ok_or_else(|| self.invalid(&value.span(), at, "no `ids`"))?;
        let prices = prices.ok_or_else(|| {
            self.invalid(&value.span(), at, "no `price`; a model needs at least one")
        })?;
        Ok(Model { ids, prices })
    }

    fn ids(
        &self,
        value: &Spanned<DeValue<'_>>,
        at: &Key,
    ) -> Result<Vec<Spanned<String>>, PriceError> {
        let items = self.array(value, at)?;
        if items.is_empty() {
            return Err(self.invalid(&value.span(), at, "is empty; a model needs an id"));
        }
        let mut ids: Vec<Spanned<String>> = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let at = at.index(index);
            let DeValue::String(id) = item.get_ref() else {
                return Err(self.wrong_type(item, &at, "a string"));
            };
            if id.trim().is_empty() {
                return Err(self.invalid(&item.span(), &at, "is empty"));
            }
            if id.trim() != id.as_ref() {
                return Err(self.invalid(
                    &item.span(),
                    &at,
                    "has whitespace at an end; an id is matched exactly",
                ));
            }
            if ids.iter().any(|seen| seen.get_ref() == id.as_ref()) {
                return Err(self.invalid(&item.span(), &at, &format!("`{id}` is listed twice")));
            }
            ids.push(Spanned::new(item.span(), id.to_string()));
        }
        Ok(ids)
    }

    fn prices(&self, value: &Spanned<DeValue<'_>>, at: &Key) -> Result<Vec<Price>, PriceError> {
        let items = self.array(value, at)?;
        if items.is_empty() {
            return Err(self.invalid(&value.span(), at, "is empty; a model needs a price"));
        }
        let mut prices: Vec<Price> = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let at = at.index(index);
            let price = self.price(item, &at)?;
            if let Some(previous) = prices.last() {
                if price.from <= previous.from {
                    return Err(self.invalid(
                        &item.span(),
                        &at.child("from"),
                        &format!(
                            "{} is not after {}, the date of the price before it; list prices oldest first",
                            price.from, previous.from
                        ),
                    ));
                }
            }
            prices.push(price);
        }
        Ok(prices)
    }

    fn price(&self, value: &Spanned<DeValue<'_>>, at: &Key) -> Result<Price, PriceError> {
        let table = self.table(value, at)?;
        let mut from = None;
        let mut long_context = None;
        let mut rates = RateFields::default();
        for (key, value) in in_file_order(table) {
            let at = at.child(key.get_ref());
            match key.get_ref().as_ref() {
                "from" => from = Some(self.date(value, &at)?),
                "long_context" => long_context = Some(self.long_context(value, &at)?),
                name if RateFields::is_rate(name) => rates.set(name, self.rate(value, &at)?),
                _ => {
                    return Err(self.invalid(
                        &key.span(),
                        &at,
                        &format!("unknown key; expected `from`, {RATE_KEYS}, or `long_context`"),
                    ));
                }
            }
        }
        let from = from.ok_or_else(|| self.invalid(&value.span(), at, "no `from` date"))?;
        let rates = self.complete(rates, value, at)?;
        Ok(Price {
            from,
            rates,
            long_context,
        })
    }

    fn long_context(
        &self,
        value: &Spanned<DeValue<'_>>,
        at: &Key,
    ) -> Result<LongContext, PriceError> {
        let mut above = None;
        let mut rates = RateFields::default();
        for (key, value) in in_file_order(self.table(value, at)?) {
            let at = at.child(key.get_ref());
            match key.get_ref().as_ref() {
                "above" => above = Some(self.token_count(value, &at)?),
                name if RateFields::is_rate(name) => rates.set(name, self.rate(value, &at)?),
                _ => {
                    return Err(self.invalid(
                        &key.span(),
                        &at,
                        &format!("unknown key; expected `above` or {RATE_KEYS}"),
                    ));
                }
            }
        }
        let above = above.ok_or_else(|| {
            self.invalid(
                &value.span(),
                at,
                "no `above`, the prompt length in tokens past which these rates apply",
            )
        })?;
        let rates = self.complete(rates, value, at)?;
        Ok(LongContext { above, rates })
    }

    /// The rates, once every one that is not optional has been given.
    fn complete(
        &self,
        fields: RateFields,
        value: &Spanned<DeValue<'_>>,
        at: &Key,
    ) -> Result<Rates, PriceError> {
        let missing = |name: &str| self.invalid(&value.span(), at, &format!("no `{name}` rate"));
        Ok(Rates {
            input: fields.input.ok_or_else(|| missing("input"))?,
            output: fields.output.ok_or_else(|| missing("output"))?,
            cache_read: fields.cache_read.ok_or_else(|| missing("cache_read"))?,
            cache_write: fields.cache_write.ok_or_else(|| missing("cache_write"))?,
            cache_write_1h: fields.cache_write_1h,
        })
    }

    /// A date written as a TOML local date, such as `2026-07-24`.
    fn date(&self, value: &Spanned<DeValue<'_>>, at: &Key) -> Result<Date, PriceError> {
        const EXPECTED: &str = "a date such as `2026-07-24`";
        let DeValue::Datetime(datetime) = value.get_ref() else {
            return Err(self.wrong_type(value, at, EXPECTED));
        };
        match (datetime.date, datetime.time, datetime.offset) {
            (Some(date), None, None) => Date::new(date.year, date.month, date.day)
                .ok_or_else(|| self.invalid(&value.span(), at, "is not a day of the calendar")),
            _ => Err(self.invalid(
                &value.span(),
                at,
                &format!(
                    "expected {EXPECTED}, found a date-time; a price is in force from a whole day"
                ),
            )),
        }
    }

    /// A rate in USD per million tokens, written as a decimal number.
    fn rate(&self, value: &Spanned<DeValue<'_>>, at: &Key) -> Result<Rate, PriceError> {
        const EXPECTED: &str = "expected dollars per million tokens as a decimal with at most six places, such as `2.50`";
        let text = match value.get_ref() {
            DeValue::Integer(integer) if integer.radix() == 10 => integer.as_str(),
            DeValue::Float(float) => float.as_str(),
            DeValue::Integer(_) => return Err(self.invalid(&value.span(), at, EXPECTED)),
            _ => return Err(self.wrong_type(value, at, "a number")),
        };
        micros(text)
            .map(Rate::from_micros)
            .ok_or_else(|| self.invalid(&value.span(), at, &format!("`{text}`: {EXPECTED}")))
    }

    /// A positive whole number of tokens.
    fn token_count(&self, value: &Spanned<DeValue<'_>>, at: &Key) -> Result<u64, PriceError> {
        let count = match value.get_ref() {
            DeValue::Integer(integer) => {
                u64::from_str_radix(integer.as_str(), integer.radix()).ok()
            }
            _ => return Err(self.wrong_type(value, at, "a whole number of tokens")),
        };
        count.filter(|&count| count > 0).ok_or_else(|| {
            self.invalid(
                &value.span(),
                at,
                "expected a positive whole number of tokens",
            )
        })
    }

    fn table<'t, 'i>(
        &self,
        value: &'t Spanned<DeValue<'i>>,
        at: &Key,
    ) -> Result<&'t DeTable<'i>, PriceError> {
        match value.get_ref() {
            DeValue::Table(table) => Ok(table),
            _ => Err(self.wrong_type(value, at, "a table")),
        }
    }

    fn array<'t, 'i>(
        &self,
        value: &'t Spanned<DeValue<'i>>,
        at: &Key,
    ) -> Result<&'t [Spanned<DeValue<'i>>], PriceError> {
        match value.get_ref() {
            DeValue::Array(items) => Ok(items),
            _ => Err(self.wrong_type(value, at, "an array")),
        }
    }

    fn wrong_type(&self, value: &Spanned<DeValue<'_>>, at: &Key, expected: &str) -> PriceError {
        self.invalid(
            &value.span(),
            at,
            &format!("expected {expected}, found {}", kind(value.get_ref())),
        )
    }

    fn invalid(&self, span: &Range<usize>, at: &Key, message: &str) -> PriceError {
        PriceError::Invalid {
            origin: self.origin.clone(),
            line: self.line(span),
            key: Some(at.0.clone()),
            message: message.to_owned(),
        }
    }

    /// A document that is not TOML, reported at the line the parser stopped
    /// on with the parser's own message.
    fn syntax(&self, error: &toml::de::Error) -> PriceError {
        PriceError::Invalid {
            origin: self.origin.clone(),
            line: error.span().map_or(1, |span| self.line(&span)),
            key: None,
            message: error.message().trim().to_owned(),
        }
    }

    /// The line, counted from 1, that a span starts on.
    fn line(&self, span: &Range<usize>) -> usize {
        let start = span.start.min(self.text.len());
        self.text.as_bytes()[..start]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count()
            + 1
    }
}

/// The rate keys, as an error lists them.
const RATE_KEYS: &str = "`input`, `output`, `cache_read`, `cache_write`, `cache_write_1h`";

/// The rates of a price or a long-context tier, as they are read.
#[derive(Default)]
struct RateFields {
    input: Option<Rate>,
    output: Option<Rate>,
    cache_read: Option<Rate>,
    cache_write: Option<Rate>,
    cache_write_1h: Option<Rate>,
}

impl RateFields {
    fn slot(&mut self, name: &str) -> Option<&mut Option<Rate>> {
        match name {
            "input" => Some(&mut self.input),
            "output" => Some(&mut self.output),
            "cache_read" => Some(&mut self.cache_read),
            "cache_write" => Some(&mut self.cache_write),
            "cache_write_1h" => Some(&mut self.cache_write_1h),
            _ => None,
        }
    }

    fn is_rate(name: &str) -> bool {
        RateFields::default().slot(name).is_some()
    }

    fn set(&mut self, name: &str, rate: Rate) {
        if let Some(slot) = self.slot(name) {
            *slot = Some(rate);
        }
    }
}

/// A decimal number of dollars as whole millionths: `2.5` is 2,500,000. `None`
/// for anything that is not a plain non-negative decimal with at most six
/// places — a negative, an exponent, `inf` or `nan` is not a price.
fn micros(text: &str) -> Option<u64> {
    const PLACES: usize = 6;
    let digits: String = text.chars().filter(|&c| c != '_').collect();
    let digits = digits.strip_prefix('+').unwrap_or(&digits);
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    let all_digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !all_digits(whole) || !all_digits(fraction) || fraction.len() > PLACES {
        return None;
    }
    if digits.contains('.') && fraction.is_empty() {
        return None;
    }
    let whole: u64 = whole.parse().ok()?;
    let fraction: u64 = format!("{fraction:0<PLACES$}").parse().ok()?;
    whole.checked_mul(1_000_000)?.checked_add(fraction)
}

/// The path of a key, spelt the way it reads in TOML, with array positions
/// counted from zero.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Key(String);

impl Key {
    fn root(name: &str) -> Self {
        Self(name.to_owned())
    }

    fn child(&self, name: &str) -> Self {
        Self(format!("{}.{name}", self.0))
    }

    fn index(&self, index: usize) -> Self {
        Self(format!("{}[{index}]", self.0))
    }
}

/// A table's entries in the order they appear in the file, so the first error
/// reported is the first one a reader of the file would reach.
fn in_file_order<'t, 'i>(table: &'t DeTable<'i>) -> Vec<Entry<'t, 'i>> {
    let mut entries: Vec<_> = table.iter().collect();
    entries.sort_by_key(|(key, _)| key.span().start);
    entries
}

/// What a value is, as an error names it.
fn kind(value: &DeValue<'_>) -> &'static str {
    match value {
        DeValue::String(_) => "a string",
        DeValue::Integer(_) => "an integer",
        DeValue::Float(_) => "a float",
        DeValue::Boolean(_) => "a boolean",
        DeValue::Datetime(_) => "a date-time",
        DeValue::Array(_) => "an array",
        DeValue::Table(_) => "a table",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRICE: &str =
        "from = 2026-07-24\ninput = 5\noutput = 25\ncache_read = 0.5\ncache_write = 6.25\n";

    fn parse(text: &str) -> Result<PriceTable, PriceError> {
        table(text, Origin::File("p.toml".into()))
    }

    fn error(text: &str) -> String {
        parse(text).expect_err("the table is invalid").to_string()
    }

    fn one_model(price: &str) -> String {
        format!("[[model]]\nids = [\"m\"]\n\n[[model.price]]\n{price}")
    }

    #[test]
    fn decimal_prices_are_read_exactly() {
        assert_eq!(micros("5"), Some(5_000_000));
        assert_eq!(micros("0.175"), Some(175_000));
        assert_eq!(micros("1.5625"), Some(1_562_500));
        assert_eq!(micros("0.000001"), Some(1));
        assert_eq!(micros("1_000.5"), Some(1_000_500_000));
        assert_eq!(micros("+2.5"), Some(2_500_000));
    }

    #[test]
    fn a_price_that_is_not_a_plain_decimal_is_refused() {
        for text in [
            "-1",
            "1e3",
            "inf",
            "nan",
            "0.0000001",
            "1.",
            ".5",
            "",
            "18446744073710",
        ] {
            assert_eq!(micros(text), None, "{text:?}");
        }
    }

    #[test]
    fn an_empty_file_is_an_empty_table() {
        assert_eq!(parse("").expect("empty is valid"), PriceTable::default());
    }

    #[test]
    fn a_complete_price_parses_and_an_optional_rate_may_be_left_out() {
        let table = parse(&one_model(PRICE)).expect("the table is valid");
        let price = table
            .price("m", Date::new(2026, 7, 24).expect("a date"))
            .expect("priced");
        assert_eq!(price.rates().cache_write().micros(), 6_250_000);
        assert_eq!(price.rates().cache_write_1h(), None);
        assert_eq!(price.long_context(), None);
    }

    #[test]
    fn a_long_context_tier_parses() {
        let text = one_model(&format!(
            "{PRICE}\n[model.price.long_context]\nabove = 272_000\ninput = 8\noutput = 30\ncache_read = 0.8\ncache_write = 10\n"
        ));
        let table = parse(&text).expect("the table is valid");
        let long = table
            .price("m", Date::new(2026, 8, 1).expect("a date"))
            .and_then(Price::long_context)
            .copied()
            .expect("a long-context tier");
        assert_eq!(long.above(), 272_000);
        assert_eq!(long.rates().output().micros(), 30_000_000);
    }

    #[test]
    fn an_invalid_price_is_reported_with_its_key_and_line() {
        let cases = [
            (
                one_model(
                    "from = 2026-07-24\ninput = -5\noutput = 25\ncache_read = 0.5\ncache_write = 6.25\n",
                ),
                "p.toml:6: model[0].price[0].input: `-5`: expected dollars per million tokens as a decimal with at most six places, such as `2.50`",
            ),
            (
                one_model(
                    "from = 2026-07-24\ninput = \"5\"\noutput = 25\ncache_read = 0.5\ncache_write = 6.25\n",
                ),
                "p.toml:6: model[0].price[0].input: expected a number, found a string",
            ),
            (
                one_model(
                    "from = \"2026-07-24\"\ninput = 5\noutput = 25\ncache_read = 0.5\ncache_write = 6.25\n",
                ),
                "p.toml:5: model[0].price[0].from: expected a date such as `2026-07-24`, found a string",
            ),
            (
                one_model(
                    "from = 2026-07-24T10:00:00Z\ninput = 5\noutput = 25\ncache_read = 0.5\ncache_write = 6.25\n",
                ),
                "p.toml:5: model[0].price[0].from: expected a date such as `2026-07-24`, found a date-time; a price is in force from a whole day",
            ),
            (
                one_model("from = 2026-07-24\ninput = 5\noutput = 25\ncache_read = 0.5\n"),
                "p.toml:4: model[0].price[0]: no `cache_write` rate",
            ),
            (
                one_model(&format!("{PRICE}cache_writes = 1\n")),
                "p.toml:10: model[0].price[0].cache_writes: unknown key; expected `from`, `input`, `output`, `cache_read`, `cache_write`, `cache_write_1h`, or `long_context`",
            ),
            (
                one_model(&format!(
                    "{PRICE}\n[model.price.long_context]\ninput = 8\noutput = 30\ncache_read = 0.8\ncache_write = 10\n"
                )),
                "p.toml:11: model[0].price[0].long_context: no `above`, the prompt length in tokens past which these rates apply",
            ),
            (
                one_model(&format!(
                    "{PRICE}\n[model.price.long_context]\nabove = 0\ninput = 8\noutput = 30\ncache_read = 0.8\ncache_write = 10\n"
                )),
                "p.toml:12: model[0].price[0].long_context.above: expected a positive whole number of tokens",
            ),
            (
                one_model(&format!("{PRICE}\n[[model.price]]\n{PRICE}")),
                "p.toml:11: model[0].price[1].from: 2026-07-24 is not after 2026-07-24, the date of the price before it; list prices oldest first",
            ),
            (
                "[[model]]\nids = []\n".to_owned(),
                "p.toml:2: model[0].ids: is empty; a model needs an id",
            ),
            (
                "[[model]]\nids = [\"m\", \" m2\"]\n".to_owned(),
                "p.toml:2: model[0].ids[1]: has whitespace at an end; an id is matched exactly",
            ),
            (
                "[[model]]\nids = [\"m\"]\n".to_owned(),
                "p.toml:1: model[0]: no `price`; a model needs at least one",
            ),
            (
                format!(
                    "{}\n[[model]]\nids = [\"x\", \"m\"]\n\n[[model.price]]\n{PRICE}",
                    one_model(PRICE)
                ),
                "p.toml:12: model[1].ids: `m` is listed by an earlier model",
            ),
            (
                "models = []\n".to_owned(),
                "p.toml:1: models: unknown key; expected `model`",
            ),
            (
                "[[model]]\nids = [\"m\"\n".to_owned(),
                "p.toml:2: unclosed array, expected `]`",
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(error(&text), expected, "\n{text}");
        }
    }
}
