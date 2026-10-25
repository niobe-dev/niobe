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
        cache_write_1h: Some(0),
        reasoning: 0,
        model: "claude-sonnet-4-5".to_owned(),
        cost_usd: None,
        settles_model: Some(false),
        fast: false,
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

#[test]
fn a_request_served_in_fast_mode_is_owed_for_at_the_fast_price() {
    // Claude Opus 5 in fast mode bills twice its $5 input and $25 output:
    //   2,000 input × 10  = 20,000
    //   1,000 output × 50 = 50,000
    //                       70,000 / 1e6 = $0.07
    // and one on a model with no fast price is a floor, not a guess.
    let fast = |model: &str| {
        Event::Usage(Usage {
            input: 2_000,
            output: 1_000,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: Some(0),
            reasoning: 0,
            model: model.to_owned(),
            cost_usd: None,
            settles_model: Some(false),
            fast: true,
        })
    };
    let prices = bundled();

    let state = SessionState::replay(&[fast("claude-opus-5")]);
    let owed = &state.totals().unsettled["claude-opus-5"];
    let estimate = niobe_tui::Prices::estimate_owed(&prices, owed)
        .expect("the bundled table prices claude-opus-5 in fast mode");
    assert_usd(estimate, 0.07);

    let unpriced = SessionState::replay(&[fast("claude-opus-4-7")]);
    let owed = &unpriced.totals().unsettled["claude-opus-4-7"];
    assert_eq!(niobe_tui::Prices::estimate_owed(&prices, owed), None);
}

#[test]
fn a_resumed_session_whose_earlier_spend_is_unknown_stays_a_floor_when_priced() {
    // The CLI restores what the session had spent and its first bill carries
    // that with the first turn, so the bridge counts none of it; the second
    // bill adds $0.05. The session cost $0.05 and the first turn's unknown
    // share, which no price of the tokens left owed can value: what is shown
    // is a floor, as it is with no price table at all.
    let mut translator = niobe_bridge_claude::Translator::new("work").resuming(
        &niobe_bridge_claude::Spent::unknown("cannot read s-1.jsonl"),
    );
    let start = r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"claude-opus-5"}}}"#;
    let delta = |input: u64, output: u64| {
        format!(
            r#"{{"type":"stream_event","event":{{"type":"message_delta","usage":{{"input_tokens":{input},"output_tokens":{output}}}}}}}"#
        )
    };
    let events: Vec<Event> = [
        r#"{"type":"system","subtype":"init","session_id":"s-1","model":"claude-opus-5"}"#,
        start,
        &delta(40, 4),
        r#"{"type":"result","subtype":"success","usage":{"input_tokens":40,"output_tokens":4},"modelUsage":{"claude-opus-5":{"inputTokens":140,"outputTokens":14,"costUSD":0.7}},"total_cost_usd":0.7}"#,
        start,
        &delta(10, 1),
        r#"{"type":"result","subtype":"success","usage":{"input_tokens":10,"output_tokens":1},"modelUsage":{"claude-opus-5":{"inputTokens":150,"outputTokens":15,"costUSD":0.75}},"total_cost_usd":0.75}"#,
    ]
    .iter()
    .flat_map(|line| translator.line(line))
    .collect();
    let state = SessionState::replay(&events);

    assert_eq!(niobe_tui::session_cost(&state, None), "≥$0.05");
    assert_eq!(niobe_tui::session_cost(&state, Some(&bundled())), "≥$0.05");
}
