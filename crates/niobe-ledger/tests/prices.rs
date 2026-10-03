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
        settles_model: false,
        fast: false,
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
fn claude_opus_5_in_fast_mode_costs_twice_the_standard_rates() {
    // Fast mode: $10 input and $50 output, twice the standard $5 and $25, and
    // twice every other rate with them.
    //   12,000 input × 10  = 120,000
    //    3,000 output × 50 = 150,000
    //  200,000 cache read × 1 = 200,000
    //                        470,000 / 1e6 = $0.47
    let usage = Usage {
        input: 12_000,
        output: 3_000,
        cache_read: 200_000,
        fast: true,
        ..usage("claude-opus-5")
    };
    assert_eq!(usd(bundled().cost(&usage, date(2026, 9, 16))), 0.47);
}

#[test]
fn a_fast_request_on_a_model_with_no_fast_price_is_unpriced() {
    // Claude Opus 4.7 lists no fast-mode price: a fast request on it is
    // not priced at its standard rates.
    let usage = Usage {
        input: 1_000,
        fast: true,
        ..usage("claude-opus-4-7")
    };
    assert_eq!(bundled().cost(&usage, date(2026, 9, 16)).usd(), None);
}

#[test]
fn claude_opus_5_5_reads_its_cache_at_five_percent_of_input() {
    // Claude Opus 5.5: input $4, output $20, cache read $0.20 (5% of input,
    // not 10%), 5-minute write $5, 1-hour write $8.
    //   10,000 input × 4       =  40,000
    //    2,000 output × 20     =  40,000
    //  500,000 cache read × .2 = 100,000
    //   20,000 5m write × 5    = 100,000
    //   10,000 1h write × 8    =  80,000
    //                            360,000 / 1e6 = $0.36
    let usage = Usage {
        input: 10_000,
        output: 2_000,
        cache_read: 500_000,
        cache_write: 30_000,
        cache_write_1h: 10_000,
        ..usage("claude-opus-5-5")
    };
    let cost = usd(bundled().cost(&usage, date(2026, 9, 24)));
    assert!((cost - 0.36).abs() < 1e-9, "{cost}");
    assert_eq!(
        bundled().cost(&usage, date(2026, 9, 21)).provenance(),
        Provenance::Unpriced,
        "priced before its release"
    );
}

#[test]
fn claude_sonnet_5_5_on_the_claude_api_matches_a_hand_computed_cost() {
    // Claude Sonnet 5.5: input $2, output $10, cache read $0.20, 1-hour write
    // $4.
    //   1,000 input × 2        =  2,000
    //     500 output × 10      =  5,000
    //  20,000 cache read × .2  =  4,000
    //   4,000 1h write × 4     = 16,000
    //                            27,000 / 1e6 = $0.027
    let usage = Usage {
        input: 1_000,
        output: 500,
        cache_read: 20_000,
        cache_write: 4_000,
        cache_write_1h: 4_000,
        ..usage("claude-sonnet-5-5[1m]")
    };
    assert_eq!(usd(bundled().cost(&usage, date(2026, 10, 1))), 0.027);
    assert_eq!(
        bundled().cost(&usage, date(2026, 9, 27)).provenance(),
        Provenance::Unpriced,
        "priced before its release"
    );
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
    let today = date(2026, 10, 1);
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

/// Which of an entry's rates a case is billed at.
#[derive(Debug, Clone, Copy)]
enum Tier {
    /// The standard rates, on a prompt under every long-context threshold.
    Standard,
    /// The standard rates times the entry's `fast` multiple.
    Fast,
    /// The entry's long-context rates, on a prompt over its threshold.
    Long,
}

/// One `[[model.price]]` entry, or one of its tiers, and what a
/// [`request`] costs at it, worked out by hand beside it.
#[derive(Debug)]
struct Case {
    id: &'static str,
    from: (u16, u8, u8),
    tier: Tier,
    /// Whether the entry has a 1-hour write rate; a request writing for an
    /// hour to a model with none is unpriced.
    one_hour: bool,
    usd: f64,
}

/// A request with 10,000 tokens of each kind the entry prices — input,
/// output, cache read, 5-minute write and, where there is a rate for it, a
/// 1-hour write — so that its cost is the sum of the entry's rates over 100.
/// Every rate counts, so a digit mistyped in any one of them moves the
/// figure. The long-context tier is ten times as much of each: a prompt of
/// 400,000 tokens, over every threshold in the table, costing the sum of the
/// long-context rates over 10.
fn request(case: &Case) -> Usage {
    let scale = match case.tier {
        Tier::Standard | Tier::Fast => 10_000,
        Tier::Long => 100_000,
    };
    let one_hour = if case.one_hour { scale } else { 0 };
    Usage {
        input: scale,
        output: scale,
        cache_read: scale,
        cache_write: scale + one_hour,
        cache_write_1h: one_hour,
        fast: matches!(case.tier, Tier::Fast),
        ..usage(case.id)
    }
}

/// Every entry of the bundled table, at the published rates its comments in
/// `prices.toml` cite, written out here rather than read from the table.
const CASES: [Case; 46] = [
    // claude-fable-5-1, from 2026-09-01, standard: (10 + 50 + 0.25 + 12.5 + 20) / 100 = 0.9275
    Case {
        id: "claude-fable-5-1",
        from: (2026, 9, 1),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.9275,
    },
    // claude-fable-5, from 2026-06-09, standard: (10 + 50 + 1 + 12.5 + 20) / 100 = 0.935
    Case {
        id: "claude-fable-5",
        from: (2026, 6, 9),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.935,
    },
    // claude-opus-5-5, from 2026-09-22, standard: (4 + 20 + 0.2 + 5 + 8) / 100 = 0.372
    Case {
        id: "claude-opus-5-5",
        from: (2026, 9, 22),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.372,
    },
    // claude-opus-5-5, from 2026-09-22, fast: (4 + 20 + 0.2 + 5 + 8) / 100 × 2 = 0.744
    Case {
        id: "claude-opus-5-5",
        from: (2026, 9, 22),
        tier: Tier::Fast,
        one_hour: true,
        usd: 0.744,
    },
    // claude-opus-5, from 2026-07-24, standard: (5 + 25 + 0.5 + 6.25 + 10) / 100 = 0.4675
    Case {
        id: "claude-opus-5",
        from: (2026, 7, 24),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.4675,
    },
    // claude-opus-5, from 2026-07-24, fast: (5 + 25 + 0.5 + 6.25 + 10) / 100 × 2 = 0.935
    Case {
        id: "claude-opus-5",
        from: (2026, 7, 24),
        tier: Tier::Fast,
        one_hour: true,
        usd: 0.935,
    },
    // claude-opus-4-8, from 2026-05-28, standard: (5 + 25 + 0.5 + 6.25 + 10) / 100 = 0.4675
    Case {
        id: "claude-opus-4-8",
        from: (2026, 5, 28),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.4675,
    },
    // claude-opus-4-8, from 2026-05-28, fast: (5 + 25 + 0.5 + 6.25 + 10) / 100 × 2 = 0.935
    Case {
        id: "claude-opus-4-8",
        from: (2026, 5, 28),
        tier: Tier::Fast,
        one_hour: true,
        usd: 0.935,
    },
    // claude-opus-4-7, from 2026-04-16, standard: (5 + 25 + 0.5 + 6.25 + 10) / 100 = 0.4675
    Case {
        id: "claude-opus-4-7",
        from: (2026, 4, 16),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.4675,
    },
    // claude-opus-4-6, from 2026-02-05, standard: (5 + 25 + 0.5 + 6.25 + 10) / 100 = 0.4675
    Case {
        id: "claude-opus-4-6",
        from: (2026, 2, 5),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.4675,
    },
    // claude-opus-4-6, from 2026-02-05, long: (10 + 37.5 + 1 + 12.5 + 20) / 10 = 8.1
    Case {
        id: "claude-opus-4-6",
        from: (2026, 2, 5),
        tier: Tier::Long,
        one_hour: true,
        usd: 8.1,
    },
    // claude-opus-4-6, from 2026-03-13, standard: (5 + 25 + 0.5 + 6.25 + 10) / 100 = 0.4675
    Case {
        id: "claude-opus-4-6",
        from: (2026, 3, 13),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.4675,
    },
    // claude-opus-4-5-20251101, from 2025-11-24, standard: (5 + 25 + 0.5 + 6.25 + 10) / 100 = 0.4675
    Case {
        id: "claude-opus-4-5-20251101",
        from: (2025, 11, 24),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.4675,
    },
    // claude-sonnet-5-5, from 2026-09-28, standard: (2 + 10 + 0.2 + 2.5 + 4) / 100 = 0.187
    Case {
        id: "claude-sonnet-5-5",
        from: (2026, 9, 28),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.187,
    },
    // claude-sonnet-5, from 2026-06-30, standard: (2 + 10 + 0.2 + 2.5 + 4) / 100 = 0.187
    Case {
        id: "claude-sonnet-5",
        from: (2026, 6, 30),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.187,
    },
    // claude-sonnet-4-6, from 2026-02-17, standard: (3 + 15 + 0.3 + 3.75 + 6) / 100 = 0.2805
    Case {
        id: "claude-sonnet-4-6",
        from: (2026, 2, 17),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.2805,
    },
    // claude-sonnet-4-6, from 2026-02-17, long: (6 + 22.5 + 0.6 + 7.5 + 12) / 10 = 4.86
    Case {
        id: "claude-sonnet-4-6",
        from: (2026, 2, 17),
        tier: Tier::Long,
        one_hour: true,
        usd: 4.86,
    },
    // claude-sonnet-4-6, from 2026-03-13, standard: (3 + 15 + 0.3 + 3.75 + 6) / 100 = 0.2805
    Case {
        id: "claude-sonnet-4-6",
        from: (2026, 3, 13),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.2805,
    },
    // claude-sonnet-4-5-20250929, from 2025-09-29, standard: (3 + 15 + 0.3 + 3.75 + 6) / 100 = 0.2805
    Case {
        id: "claude-sonnet-4-5-20250929",
        from: (2025, 9, 29),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.2805,
    },
    // claude-sonnet-4-5-20250929, from 2025-09-29, long: (6 + 22.5 + 0.6 + 7.5 + 12) / 10 = 4.86
    Case {
        id: "claude-sonnet-4-5-20250929",
        from: (2025, 9, 29),
        tier: Tier::Long,
        one_hour: true,
        usd: 4.86,
    },
    // claude-haiku-4-5-20251001, from 2025-10-15, standard: (1 + 5 + 0.1 + 1.25 + 2) / 100 = 0.0935
    Case {
        id: "claude-haiku-4-5-20251001",
        from: (2025, 10, 15),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.0935,
    },
    // eu.anthropic.claude-fable-5, from 2026-06-09, standard: (11 + 55 + 1.1 + 13.75 + 22) / 100 = 1.0285
    Case {
        id: "eu.anthropic.claude-fable-5",
        from: (2026, 6, 9),
        tier: Tier::Standard,
        one_hour: true,
        usd: 1.0285,
    },
    // eu.anthropic.claude-opus-5, from 2026-07-24, standard: (5.5 + 27.5 + 0.55 + 6.875 + 11) / 100 = 0.51425
    Case {
        id: "eu.anthropic.claude-opus-5",
        from: (2026, 7, 24),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.51425,
    },
    // eu.anthropic.claude-opus-4-8, from 2026-05-28, standard: (5.5 + 27.5 + 0.55 + 6.875 + 11) / 100 = 0.51425
    Case {
        id: "eu.anthropic.claude-opus-4-8",
        from: (2026, 5, 28),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.51425,
    },
    // eu.anthropic.claude-opus-4-7, from 2026-04-16, standard: (5.5 + 27.5 + 0.55 + 6.875 + 11) / 100 = 0.51425
    Case {
        id: "eu.anthropic.claude-opus-4-7",
        from: (2026, 4, 16),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.51425,
    },
    // eu.anthropic.claude-sonnet-5-5, from 2026-09-28, standard: (2.2 + 11 + 0.22 + 2.75 + 4.4) / 100 = 0.2057
    Case {
        id: "eu.anthropic.claude-sonnet-5-5",
        from: (2026, 9, 28),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.2057,
    },
    // eu.anthropic.claude-sonnet-5, from 2026-06-30, standard: (2.2 + 11 + 0.22 + 2.75 + 4.4) / 100 = 0.2057
    Case {
        id: "eu.anthropic.claude-sonnet-5",
        from: (2026, 6, 30),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.2057,
    },
    // global.anthropic.claude-opus-4-6-v1, from 2026-03-13, standard: (5 + 25 + 0.5 + 6.25 + 10) / 100 = 0.4675
    Case {
        id: "global.anthropic.claude-opus-4-6-v1",
        from: (2026, 3, 13),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.4675,
    },
    // eu.anthropic.claude-opus-4-6-v1, from 2026-03-13, standard: (5.5 + 27.5 + 0.55 + 6.875 + 11) / 100 = 0.51425
    Case {
        id: "eu.anthropic.claude-opus-4-6-v1",
        from: (2026, 3, 13),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.51425,
    },
    // global.anthropic.claude-sonnet-4-6, from 2026-03-13, standard: (3 + 15 + 0.3 + 3.75 + 6) / 100 = 0.2805
    Case {
        id: "global.anthropic.claude-sonnet-4-6",
        from: (2026, 3, 13),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.2805,
    },
    // eu.anthropic.claude-sonnet-4-6, from 2026-03-13, standard: (3.3 + 16.5 + 0.33 + 4.125 + 6.6) / 100 = 0.30855
    Case {
        id: "eu.anthropic.claude-sonnet-4-6",
        from: (2026, 3, 13),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.30855,
    },
    // global.anthropic.claude-haiku-4-5-20251001-v1:0, from 2025-10-15, standard: (1 + 5 + 0.1 + 1.25 + 2) / 100 = 0.0935
    Case {
        id: "global.anthropic.claude-haiku-4-5-20251001-v1:0",
        from: (2025, 10, 15),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.0935,
    },
    // eu.anthropic.claude-haiku-4-5-20251001-v1:0, from 2025-10-15, standard: (1.1 + 5.5 + 0.11 + 1.375 + 2.2) / 100 = 0.10285
    Case {
        id: "eu.anthropic.claude-haiku-4-5-20251001-v1:0",
        from: (2025, 10, 15),
        tier: Tier::Standard,
        one_hour: true,
        usd: 0.10285,
    },
    // gpt-5.6-sol, from 2026-07-09, standard: (5 + 30 + 0.5 + 6.25) / 100 = 0.4175
    Case {
        id: "gpt-5.6-sol",
        from: (2026, 7, 9),
        tier: Tier::Standard,
        one_hour: false,
        usd: 0.4175,
    },
    // gpt-5.6-sol, from 2026-07-09, long: (10 + 45 + 1 + 12.5) / 10 = 6.85
    Case {
        id: "gpt-5.6-sol",
        from: (2026, 7, 9),
        tier: Tier::Long,
        one_hour: false,
        usd: 6.85,
    },
    // gpt-5.6-sol, from 2026-08-21, standard: (4 + 20 + 0.4 + 5) / 100 = 0.294
    Case {
        id: "gpt-5.6-sol",
        from: (2026, 8, 21),
        tier: Tier::Standard,
        one_hour: false,
        usd: 0.294,
    },
    // gpt-5.6-sol, from 2026-08-21, long: (8 + 30 + 0.8 + 10) / 10 = 4.88
    Case {
        id: "gpt-5.6-sol",
        from: (2026, 8, 21),
        tier: Tier::Long,
        one_hour: false,
        usd: 4.88,
    },
    // gpt-5.6-terra, from 2026-07-09, standard: (2.5 + 15 + 0.25 + 3.125) / 100 = 0.20875
    Case {
        id: "gpt-5.6-terra",
        from: (2026, 7, 9),
        tier: Tier::Standard,
        one_hour: false,
        usd: 0.20875,
    },
    // gpt-5.6-terra, from 2026-07-09, long: (5 + 22.5 + 0.5 + 6.25) / 10 = 3.425
    Case {
        id: "gpt-5.6-terra",
        from: (2026, 7, 9),
        tier: Tier::Long,
        one_hour: false,
        usd: 3.425,
    },
    // gpt-5.6-terra, from 2026-07-30, standard: (2 + 12 + 0.2 + 2.5) / 100 = 0.167
    Case {
        id: "gpt-5.6-terra",
        from: (2026, 7, 30),
        tier: Tier::Standard,
        one_hour: false,
        usd: 0.167,
    },
    // gpt-5.6-terra, from 2026-07-30, long: (4 + 18 + 0.4 + 5) / 10 = 2.74
    Case {
        id: "gpt-5.6-terra",
        from: (2026, 7, 30),
        tier: Tier::Long,
        one_hour: false,
        usd: 2.74,
    },
    // gpt-5.6-luna, from 2026-07-09, standard: (1 + 6 + 0.1 + 1.25) / 100 = 0.0835
    Case {
        id: "gpt-5.6-luna",
        from: (2026, 7, 9),
        tier: Tier::Standard,
        one_hour: false,
        usd: 0.0835,
    },
    // gpt-5.6-luna, from 2026-07-09, long: (2 + 9 + 0.2 + 2.5) / 10 = 1.37
    Case {
        id: "gpt-5.6-luna",
        from: (2026, 7, 9),
        tier: Tier::Long,
        one_hour: false,
        usd: 1.37,
    },
    // gpt-5.6-luna, from 2026-07-30, standard: (0.2 + 1.2 + 0.02 + 0.25) / 100 = 0.0167
    Case {
        id: "gpt-5.6-luna",
        from: (2026, 7, 30),
        tier: Tier::Standard,
        one_hour: false,
        usd: 0.0167,
    },
    // gpt-5.6-luna, from 2026-07-30, long: (0.4 + 1.8 + 0.04 + 0.5) / 10 = 0.274
    Case {
        id: "gpt-5.6-luna",
        from: (2026, 7, 30),
        tier: Tier::Long,
        one_hour: false,
        usd: 0.274,
    },
    // gpt-5.3-codex, from 2026-02-24, standard: (1.75 + 14 + 0.175 + 1.75) / 100 = 0.17675
    Case {
        id: "gpt-5.3-codex",
        from: (2026, 2, 24),
        tier: Tier::Standard,
        one_hour: false,
        usd: 0.17675,
    },
];

#[test]
fn every_price_in_the_table_matches_a_hand_computed_cost() {
    let table = bundled();
    for case in &CASES {
        let (year, month, day) = case.from;
        let cost = usd(table.cost(&request(case), date(year, month, day)));
        assert!((cost - case.usd).abs() < 1e-9, "{case:?} cost {cost}");
    }
}
