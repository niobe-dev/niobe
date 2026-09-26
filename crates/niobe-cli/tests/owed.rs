// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What the shell makes of tokens no reported cost covers yet, priced against
//! the bundled table.
//!
//! A long-context rate is a rule about one request: the provider bills a
//! request at the dearer rates when its own prompt is past the threshold. The
//! session fold, the shell and the price table each hold part of that, and
//! only the binary may name all three, so it is asked here whether a turn of
//! short requests is still priced as short requests once they are owed for
//! together.
//!
//! The expected amounts are worked out by hand from the published rates, in
//! the comment above each, so a bug in the pricing cannot agree with itself.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use niobe_core::event::{Event, Usage};
use niobe_core::session::SessionState;
use niobe_ledger::{Date, PriceTable};

/// The bundled price table, read on one day, as the binary hands it to the
/// shell.
#[derive(Debug)]
struct Bundled(PriceTable);

impl niobe_tui::Prices for Bundled {
    fn estimate(&self, usage: &Usage) -> Option<f64> {
        let day = Date::new(2026, 9, 26).expect("26 September 2026 is a date");
        self.0.cost(usage, day).usd()
    }
}

fn bundled() -> Bundled {
    Bundled(PriceTable::bundled().expect("the bundled price table parses"))
}

/// One Claude Sonnet 4.5 request the backend reported no cost for.
fn request(input: u64, cache_read: u64, output: u64) -> Event {
    Event::Usage(Usage {
        input,
        output,
        cache_read,
        cache_write: 0,
        cache_write_1h: 0,
        reasoning: 0,
        model: "claude-sonnet-4-5".to_owned(),
        cost_usd: None,
        cost_basis: None,
        settles_model: false,
    })
}

/// A dollar amount is right to the picodollar the ledger prices in, or it is
/// a different amount.
fn assert_usd(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-12,
        "${actual:.9} rather than ${expected:.9}"
    );
}

#[test]
fn requests_each_under_the_long_context_threshold_are_priced_at_standard_rates() {
    // Claude Sonnet 4.5: input $3, output $15, cache read $0.30, and above a
    // 200K prompt $6, $22.50 and $0.60. Each request's prompt is 90K, so each
    // is billed at the standard rates, although the three together read 270K:
    //   2,000 input × 3        =  6,000
    //  88,000 cache read × 0.3 = 26,400
    //   1,000 output × 15      = 15,000
    //                            47,400 each, × 3 = 142,200 / 1e6 = $0.1422
    let state = SessionState::replay(&[
        request(2_000, 88_000, 1_000),
        request(2_000, 88_000, 1_000),
        request(2_000, 88_000, 1_000),
    ]);
    let prices = bundled();

    let owed = &state.totals().unsettled["claude-sonnet-4-5"];
    let estimate = niobe_tui::Prices::estimate_owed(&prices, owed)
        .expect("the bundled table prices claude-sonnet-4-5");
    assert_usd(estimate, 0.1422);
    assert_eq!(niobe_tui::session_cost(&state, Some(&prices)), "~$0.14");
}

#[test]
fn a_request_over_the_long_context_threshold_is_still_priced_at_long_context_rates() {
    // A 250K prompt is past Sonnet 4.5's 200K threshold, so every token of it
    // is billed at $6 input, $0.60 cache read and $22.50 output:
    //    2,000 input × 6         =  12,000
    //  248,000 cache read × 0.6  = 148,800
    //    1,000 output × 22.5     =  22,500
    //                              183,300 / 1e6 = $0.1833
    let state = SessionState::replay(&[request(2_000, 248_000, 1_000)]);
    let prices = bundled();

    let owed = &state.totals().unsettled["claude-sonnet-4-5"];
    let estimate = niobe_tui::Prices::estimate_owed(&prices, owed)
        .expect("the bundled table prices claude-sonnet-4-5");
    assert_usd(estimate, 0.1833);
    assert_eq!(niobe_tui::session_cost(&state, Some(&prices)), "~$0.18");
}
