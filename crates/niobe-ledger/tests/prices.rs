// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The bundled price table, priced against by hand.
//!
//! Every expected figure below is worked out in the comment above it from the
//! provider's published rates, in USD per million tokens, not from the table:
//! a mistyped rate in `prices.toml` must disagree with the arithmetic here.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use niobe_core::Usage;
use niobe_ledger::{Cost, Date, PriceTable, Provenance, Rate, Rates};

fn bundled() -> PriceTable {
    PriceTable::bundled().expect("the bundled price table parses")
}

fn date(year: u16, month: u8, day: u8) -> Date {
    Date::new(year, month, day).expect("a real calendar date")
}

fn usage(model: &str) -> Usage {
    Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: 0,
        reasoning: 0,
        model: model.to_owned(),
        cost_usd: None,
    }
}

fn usd(cost: Cost) -> f64 {
    assert_eq!(cost.provenance(), Provenance::ApiEquivalent, "{cost:?}");
    cost.usd().expect("an API-equivalent cost has an amount")
}

#[test]
fn claude_opus_5_on_the_claude_api_matches_a_hand_computed_cost() {
    // Claude Opus 5: input $5, output $25, cache read $0.50, 5-minute write
    // $6.25, 1-hour write $10.
    //   12,000 input × 5       =  60,000
    //    3,000 output × 25     =  75,000
    //  200,000 cache read × .5 = 100,000
    //   10,000 5m write × 6.25 =  62,500
    //   30,000 1h write × 10   = 300,000
    //                            597,500 / 1e6 = $0.5975
    let usage = Usage {
        input: 12_000,
        output: 3_000,
        cache_read: 200_000,
        cache_write: 40_000,
        cache_write_1h: 30_000,
        ..usage("claude-opus-5")
    };
    assert_eq!(usd(bundled().cost(&usage, date(2026, 9, 16))), 0.5975);
}

#[test]
fn claude_sonnet_5_on_a_bedrock_eu_profile_matches_a_hand_computed_cost() {
    // Bedrock EU cross-region inference, Claude Sonnet 5: input $2.20, output
    // $11, cache read $0.22, 1-hour write $4.40 (the global rates plus 10%).
    //   50,000 input × 2.2       = 110,000
    //    8,000 output × 11       =  88,000
    //  400,000 cache read × .22  =  88,000
    //   20,000 1h write × 4.4    =  88,000
    //                              374,000 / 1e6 = $0.374
    let usage = Usage {
        input: 50_000,
        output: 8_000,
        cache_read: 400_000,
        cache_write: 20_000,
        cache_write_1h: 20_000,
        ..usage("eu.anthropic.claude-sonnet-5[1m]")
    };
    assert_eq!(usd(bundled().cost(&usage, date(2026, 9, 16))), 0.374);
}

#[test]
fn claude_haiku_4_5_on_a_bedrock_eu_profile_matches_a_hand_computed_cost() {
    // Bedrock EU, Claude Haiku 4.5: input $1.10, output $5.50, 5-minute write
    // $1.375.
    //   4,000 input × 1.1      =  4,400
    //   1,000 output × 5.5     =  5,500
    //  10,000 5m write × 1.375 = 13,750
    //                            23,650 / 1e6 = $0.02365
    let usage = Usage {
        input: 4_000,
        output: 1_000,
        cache_write: 10_000,
        ..usage("eu.anthropic.claude-haiku-4-5-20251001-v1:0")
    };
    assert_eq!(usd(bundled().cost(&usage, date(2026, 9, 16))), 0.02365);
}

#[test]
fn gpt_5_6_sol_matches_a_hand_computed_cost_at_the_price_in_force_that_day() {
    // A 280,000-token prompt is over GPT-5.6 Sol's 272K threshold, so the whole
    // request is billed at the long-context rates. Reasoning is billed as
    // output.
    let usage = Usage {
        input: 100_000,
        cache_read: 180_000,
        output: 6_000,
        reasoning: 2_000,
        ..usage("gpt-5.6-sol")
    };
    let table = bundled();

    // From 21 August 2026, long context: input $8, cached $0.80, output $30.
    //  100,000 × 8   =   800,000
    //  180,000 × 0.8 =   144,000
    //    8,000 × 30  =   240,000
    //                  1,184,000 / 1e6 = $1.184
    assert_eq!(usd(table.cost(&usage, date(2026, 9, 1))), 1.184);

    // Before, long context: input $10, cached $1, output $45.
    //  100,000 × 10  = 1,000,000
    //  180,000 × 1   =   180,000
    //    8,000 × 45  =   360,000
    //                  1,540,000 / 1e6 = $1.54
    assert_eq!(usd(table.cost(&usage, date(2026, 8, 20))), 1.54);
}

#[test]
fn claude_opus_4_6_prices_a_long_prompt_by_the_rule_in_force_that_day() {
    // Until 13 March 2026 a prompt over 200K tokens on Opus 4.6 was billed at
    // input $10 and output $37.50; from that day the whole 1M window is at the
    // standard $5 and $25.
    //  before: 250,000 × 10 + 1,000 × 37.5 = 2,537,500 / 1e6 = $2.5375
    //  after:  250,000 × 5  + 1,000 × 25   = 1,275,000 / 1e6 = $1.275
    let usage = Usage {
        input: 250_000,
        output: 1_000,
        ..usage("claude-opus-4-6")
    };
    let table = bundled();
    assert_eq!(usd(table.cost(&usage, date(2026, 3, 12))), 2.5375);
    assert_eq!(usd(table.cost(&usage, date(2026, 3, 13))), 1.275);
}

#[test]
fn bedrock_eu_model_ids_resolve() {
    let table = bundled();
    let today = date(2026, 9, 16);
    for id in [
        // The ids this machine's Claude Code settings name for its EU profile.
        "eu.anthropic.claude-opus-5[1m]",
        "eu.anthropic.claude-sonnet-5[1m]",
        "eu.anthropic.claude-haiku-4-5-20251001-v1:0",
        // And the other EU cross-region ids of current models.
        "eu.anthropic.claude-opus-5",
        "eu.anthropic.claude-sonnet-5",
        "eu.anthropic.claude-fable-5",
        "eu.anthropic.claude-opus-4-8",
        "eu.anthropic.claude-opus-4-7",
        "eu.anthropic.claude-opus-4-6-v1",
        "eu.anthropic.claude-sonnet-4-6",
        "eu.anthropic.claude-haiku-4-5",
    ] {
        assert!(table.price(id, today).is_some(), "{id} is unpriced");
    }
}

/// Every rate of `rates`, in millionths of a dollar per million tokens.
fn micros(rates: &Rates) -> [Option<u64>; 5] {
    [
        Some(rates.input()),
        Some(rates.output()),
        Some(rates.cache_read()),
        Some(rates.cache_write()),
        rates.cache_write_1h(),
    ]
    .map(|rate| rate.map(Rate::micros))
}

/// The Claude API id of the model a Bedrock profile id names:
/// `eu.anthropic.claude-opus-4-6-v1[1m]` is `claude-opus-4-6[1m]`.
fn claude_api_id(bedrock: &str) -> String {
    let (_, model) = bedrock
        .split_once(".anthropic.")
        .expect("a Bedrock profile id");
    let (model, window) = match model.strip_suffix("[1m]") {
        Some(model) => (model, "[1m]"),
        None => (model, ""),
    };
    let model = model
        .strip_suffix("-v1:0")
        .or_else(|| model.strip_suffix("-v1"))
        .unwrap_or(model);
    format!("{model}{window}")
}

#[test]
fn every_bedrock_rate_is_the_claude_api_rate_plus_ten_percent_in_a_region_and_equal_globally() {
    let table = bundled();
    let today = date(2026, 9, 16);
    let bedrock: Vec<&str> = table
        .ids()
        .filter(|id| id.contains(".anthropic."))
        .collect();
    assert!(bedrock.len() >= 30, "{bedrock:?}");

    for id in bedrock {
        let claude_api = claude_api_id(id);
        let expected = table
            .price(&claude_api, today)
            .unwrap_or_else(|| panic!("{id} has no Claude API counterpart {claude_api}"))
            .rates();
        let premium = |rate: u64| {
            if id.starts_with("global.") {
                rate
            } else {
                rate + rate / 10
            }
        };
        let actual = table.price(id, today).expect("listed, so priced").rates();
        assert_eq!(
            micros(actual),
            micros(expected).map(|rate| rate.map(premium)),
            "{id} against {claude_api}"
        );
    }
}

#[test]
fn a_model_the_table_does_not_list_is_unpriced() {
    let table = bundled();
    let today = date(2026, 9, 16);
    for id in [
        "codex-auto-review",
        // Fable 5.1 has no EU regional endpoint on Bedrock.
        "eu.anthropic.claude-fable-5-1",
        // A bare Bedrock id is billed at the global or the regional rate
        // depending on the endpoint it was sent to, which the id does not say.
        "anthropic.claude-opus-5",
        "",
    ] {
        let usage = Usage {
            input: 1_000,
            ..usage(id)
        };
        assert_eq!(table.cost(&usage, today), Cost::Unpriced, "{id:?}");
        assert_eq!(table.cost(&usage, today).usd(), None);
    }
}

#[test]
fn a_session_from_before_a_model_was_priced_is_unpriced() {
    let usage = Usage {
        input: 1_000,
        ..usage("claude-opus-5")
    };
    let table = bundled();
    assert_eq!(table.cost(&usage, date(2026, 7, 23)), Cost::Unpriced);
    assert_ne!(table.cost(&usage, date(2026, 7, 24)), Cost::Unpriced);
}

#[test]
fn one_hour_writes_on_a_model_with_no_one_hour_rate_are_unpriced_not_guessed() {
    let usage = Usage {
        input: 1_000,
        cache_write: 1_000,
        cache_write_1h: 1_000,
        ..usage("gpt-5.6-terra")
    };
    assert_eq!(bundled().cost(&usage, date(2026, 9, 16)), Cost::Unpriced);
}
