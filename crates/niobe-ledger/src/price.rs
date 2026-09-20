// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What a model costs per token, and what a usage record costs at that price.
//!
//! Rates are held as whole millionths of a dollar per million tokens, which is
//! exactly one picodollar per token, so a cost is summed in integers and a
//! published price such as $0.175 is never rounded on the way in. The amount
//! becomes a floating-point number of dollars once, at the end.

use std::fmt;

use niobe_core::Usage;

use crate::Date;

/// A price per token, in USD per million tokens, exact to a millionth of a
/// dollar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rate(u64);

impl Rate {
    /// The rate `micros` millionths of a dollar per million tokens.
    pub fn from_micros(micros: u64) -> Self {
        Self(micros)
    }

    /// The rate in millionths of a dollar per million tokens.
    pub fn micros(self) -> u64 {
        self.0
    }

    /// What `tokens` cost at this rate, in picodollars.
    fn of(self, tokens: u64) -> u128 {
        u128::from(tokens) * u128::from(self.0)
    }
}

impl fmt::Display for Rate {
    /// Dollars, with the cents always shown and further digits only when the
    /// rate has them: `5.00`, `0.175`, `1.5625`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MICROS_PER_DOLLAR: u64 = 1_000_000;
        let fraction = format!("{:06}", self.0 % MICROS_PER_DOLLAR);
        let digits = fraction.trim_end_matches('0').len().max(2);
        let text = format!("{}.{}", self.0 / MICROS_PER_DOLLAR, &fraction[..digits]);
        f.pad(&text)
    }
}

/// The rates a model bills each kind of token at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rates {
    pub(crate) input: Rate,
    pub(crate) output: Rate,
    pub(crate) cache_read: Rate,
    pub(crate) cache_write: Rate,
    pub(crate) cache_write_1h: Option<Rate>,
}

impl Rates {
    /// Input tokens that were neither read from nor written to the cache.
    pub fn input(&self) -> Rate {
        self.input
    }

    /// Output tokens. Reasoning tokens are billed at this rate too: every
    /// provider in the table bills them as output.
    pub fn output(&self) -> Rate {
        self.output
    }

    /// Input tokens read from the prompt cache.
    pub fn cache_read(&self) -> Rate {
        self.cache_read
    }

    /// Input tokens written to the prompt cache for the default lifetime. A
    /// provider that does not bill cache writes apart from input lists its
    /// input rate here.
    pub fn cache_write(&self) -> Rate {
        self.cache_write
    }

    /// Input tokens written to the prompt cache for an hour, where the provider
    /// offers that lifetime.
    pub fn cache_write_1h(&self) -> Option<Rate> {
        self.cache_write_1h
    }

    /// What `usage` costs at these rates, in picodollars, or `None` when it
    /// has one-hour cache writes and these rates have no price for them.
    fn cost(&self, usage: &Usage) -> Option<u128> {
        // A record claiming more one-hour writes than writes is read as all of
        // its writes being one-hour ones, the dearer reading.
        let one_hour = usage.cache_write_1h.min(usage.cache_write);
        let five_minute = usage.cache_write - one_hour;
        let one_hour_cost = match (one_hour, self.cache_write_1h) {
            (0, _) => 0,
            (_, Some(rate)) => rate.of(one_hour),
            (_, None) => return None,
        };

        Some(
            [
                self.input.of(usage.input),
                self.output.of(usage.output),
                self.output.of(usage.reasoning),
                self.cache_read.of(usage.cache_read),
                self.cache_write.of(five_minute),
                one_hour_cost,
            ]
            .into_iter()
            .fold(0, u128::saturating_add),
        )
    }
}

/// Rates that replace a model's standard ones for a request whose prompt is
/// longer than a threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LongContext {
    pub(crate) above: u64,
    pub(crate) rates: Rates,
}

impl LongContext {
    /// The prompt length, in tokens, that a request must exceed.
    pub fn above(&self) -> u64 {
        self.above
    }

    /// The rates every token of such a request is billed at.
    pub fn rates(&self) -> &Rates {
        &self.rates
    }
}

/// A model's price from one date until the next price in its schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Price {
    pub(crate) from: Date,
    pub(crate) rates: Rates,
    pub(crate) long_context: Option<LongContext>,
}

impl Price {
    /// The first day this price is in force.
    pub fn from(&self) -> Date {
        self.from
    }

    /// The standard rates.
    pub fn rates(&self) -> &Rates {
        &self.rates
    }

    /// The rates for long prompts, where the model has them.
    pub fn long_context(&self) -> Option<&LongContext> {
        self.long_context.as_ref()
    }

    /// What `usage` costs at this price, in picodollars.
    ///
    /// The prompt is every input token, cached or not. A long-context rate
    /// applies to the whole record, which is right when the record is one
    /// request, as providers bill it; a record that sums several requests is
    /// priced as though it were one.
    pub(crate) fn cost(&self, usage: &Usage) -> Option<u128> {
        let prompt = usage
            .input
            .saturating_add(usage.cache_read)
            .saturating_add(usage.cache_write);
        let rates = match &self.long_context {
            Some(long) if prompt > long.above => &long.rates,
            Some(_) | None => &self.rates,
        };
        rates.cost(usage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(micros: u64) -> Rate {
        Rate::from_micros(micros)
    }

    fn rates() -> Rates {
        Rates {
            input: rate(3_000_000),
            output: rate(15_000_000),
            cache_read: rate(300_000),
            cache_write: rate(3_750_000),
            cache_write_1h: Some(rate(6_000_000)),
        }
    }

    fn usage() -> Usage {
        Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 0,
            model: "m".to_owned(),
            cost_usd: None,
            cost_basis: None,
            settles_model: false,
        }
    }

    #[test]
    fn a_rate_prints_as_dollars_with_only_the_digits_it_has() {
        assert_eq!(rate(5_000_000).to_string(), "5.00");
        assert_eq!(rate(175_000).to_string(), "0.175");
        assert_eq!(rate(1_562_500).to_string(), "1.5625");
        assert_eq!(rate(37_500).to_string(), "0.0375");
        assert_eq!(rate(1).to_string(), "0.000001");
        assert_eq!(rate(0).to_string(), "0.00");
        assert_eq!(format!("{:>6}", rate(2_500_000)), "  2.50");
    }

    #[test]
    fn each_kind_of_token_is_billed_at_its_own_rate() {
        let usage = Usage {
            input: 1,
            output: 10,
            reasoning: 100,
            cache_read: 1_000,
            cache_write: 30_000,
            cache_write_1h: 20_000,
            ..usage()
        };
        // 1 × 3 + 10 × 15 + 100 × 15 + 1,000 × 0.3 + 10,000 × 3.75 + 20,000 × 6,
        // in picodollars: rates are micros per million tokens.
        let expected = 3_000_000
            + 150_000_000
            + 1_500_000_000
            + 300_000_000
            + 37_500_000_000
            + 120_000_000_000_u128;
        assert_eq!(rates().cost(&usage), Some(expected));
    }

    #[test]
    fn more_one_hour_writes_than_writes_are_read_as_all_writes_being_one_hour() {
        let usage = Usage {
            cache_write: 10,
            cache_write_1h: 25,
            ..usage()
        };
        assert_eq!(rates().cost(&usage), Some(10 * 6_000_000));
    }

    #[test]
    fn one_hour_writes_with_no_one_hour_rate_have_no_cost() {
        let rates = Rates {
            cache_write_1h: None,
            ..rates()
        };
        let five_minute = Usage {
            cache_write: 10,
            ..usage()
        };
        let one_hour = Usage {
            cache_write_1h: 10,
            ..five_minute.clone()
        };
        assert_eq!(rates.cost(&five_minute), Some(10 * 3_750_000));
        assert_eq!(rates.cost(&one_hour), None);
    }

    #[test]
    fn long_context_rates_apply_only_past_the_threshold_and_then_to_every_token() {
        let long = Rates {
            input: rate(6_000_000),
            output: rate(22_500_000),
            ..rates()
        };
        let price = Price {
            from: Date::new(2026, 1, 1).expect("a real date"),
            rates: rates(),
            long_context: Some(LongContext {
                above: 200,
                rates: long,
            }),
        };
        let at_threshold = Usage {
            input: 100,
            cache_read: 50,
            cache_write: 50,
            output: 1,
            ..usage()
        };
        let past_it = Usage {
            input: 101,
            ..at_threshold.clone()
        };
        assert_eq!(price.cost(&at_threshold), rates().cost(&at_threshold));
        assert_eq!(price.cost(&past_it), long.cost(&past_it));
        assert_ne!(price.cost(&past_it), rates().cost(&past_it));
    }

    #[test]
    fn a_cost_saturates_instead_of_wrapping() {
        let usage = Usage {
            input: u64::MAX,
            output: u64::MAX,
            reasoning: u64::MAX,
            cache_read: u64::MAX,
            cache_write: u64::MAX,
            ..usage()
        };
        let huge = Rate::from_micros(u64::MAX);
        let rates = Rates {
            input: huge,
            output: huge,
            cache_read: huge,
            cache_write: huge,
            cache_write_1h: None,
        };
        assert_eq!(rates.cost(&usage), Some(u128::MAX));
    }
}
