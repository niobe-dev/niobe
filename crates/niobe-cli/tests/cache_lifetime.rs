// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A recorded turn's cache writes, from the lifetime the CLI reported them
//! with to the money the ledger prices them at.
//!
//! Anthropic bills a cache write bought for an hour at twice the input rate
//! and one bought for five minutes at 1.25×. The `claude` CLI states which was
//! bought, per message, in `usage.cache_creation`; the bridge carries that
//! into `Usage::cache_write_1h`; the ledger prices the two shares apart. No
//! single crate can be asked whether that holds end to end — the bridge may
//! not name the ledger — so it is asked here, in the binary, which is the only
//! crate that may name both.
//!
//! The expected amounts are computed by hand from the rates in the ledger's
//! bundled `prices.toml` and written out below, so a bug in the pricing cannot
//! agree with itself.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use niobe_bridge_claude::Translator;
use niobe_core::event::Event;
use niobe_ledger::{Cost, Date, PriceTable, Provenance};

/// The same two-turn recording the bridge's own tests fold, read from the
/// bridge's fixtures so that the two cannot drift apart.
const FIXTURE: &str = include_str!("../../niobe-bridge-claude/tests/fixtures/stream-json.jsonl");

/// The day the recording was taken, which is the day its rates are those of.
fn recorded_on() -> Date {
    Date::new(2026, 9, 18).expect("a real date")
}

/// The records the recording's own messages produced: the ones whose tokens
/// the CLI stated a lifetime for.
///
/// The records that close a turn are left out on purpose. They carry what the
/// CLI charged for models that never produced a message of their own, they
/// already have a cost on them, and `modelUsage` states no lifetime for their
/// writes — pricing them here would be pricing the same tokens twice.
fn per_message_records() -> Vec<niobe_core::Usage> {
    let mut translator = Translator::new("max");
    FIXTURE
        .lines()
        .filter(|line| !line.trim().is_empty())
        .flat_map(|line| translator.line(line))
        .filter_map(|event| match event {
            Event::Usage(usage) if usage.cost_usd.is_none() => Some(usage),
            _ => None,
        })
        .collect()
}

fn usd(table: &PriceTable, usage: &niobe_core::Usage) -> f64 {
    let cost = table.cost(usage, recorded_on());
    assert_eq!(
        cost.provenance(),
        Provenance::ApiEquivalent,
        "a list price is never a measurement: {cost:?}"
    );
    match cost {
        Cost::ApiEquivalent(usd) => usd,
        Cost::Unpriced => panic!("the bundled table prices claude-sonnet-5"),
    }
}

/// A dollar amount is right to the picodollar the ledger sums in, or it is a
/// different amount.
fn assert_usd(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-12,
        "${actual:.9} rather than ${expected:.9}"
    );
}

#[test]
fn a_one_hour_write_and_a_five_minute_one_are_priced_at_their_own_rates() {
    let table = PriceTable::bundled().expect("the bundled price table parses");
    let records = per_message_records();

    // Claude Sonnet 5, from 2026-06-30: $2.00 input, $10.00 output, $0.20
    // cache read, $2.50 cache write, $4.00 cache write for an hour, each per
    // million tokens.
    let hour = records.first().expect("the first message");
    assert_eq!((hour.cache_write, hour.cache_write_1h), (100, Some(100)));
    // 3×2.00 + 40×10.00 + 1000×0.20 + 100×4.00 = 1006 millionths of a dollar.
    assert_usd(usd(&table, hour), 0.001_006);

    let five_minutes = records.get(1).expect("the second message");
    assert_eq!(
        (five_minutes.cache_write, five_minutes.cache_write_1h),
        (50, Some(0))
    );
    // 2×2.00 + 25×10.00 + 1100×0.20 + 50×2.50 = 599 millionths of a dollar.
    assert_usd(usd(&table, five_minutes), 0.000_599);
}

#[test]
fn the_recorded_sessions_writes_price_at_the_lifetimes_the_cli_reported() {
    let table = PriceTable::bundled().expect("the bundled price table parses");
    let records = per_message_records();

    let written: u64 = records.iter().map(|usage| usage.cache_write).sum();
    let for_an_hour: u64 = records
        .iter()
        .filter_map(|usage| usage.cache_write_1h)
        .sum();
    assert_eq!((written, for_an_hour), (185, 135));

    // 9×2.00 + 115×10.00 + 5700×0.20 + 50×2.50 + 135×4.00
    //   = 18 + 1150 + 1140 + 125 + 540 = 2973 millionths of a dollar.
    let total: f64 = records.iter().map(|usage| usd(&table, usage)).sum();
    assert_usd(total, 0.002_973);
}

/// What the share is worth, stated as money: a bridge that leaves it at zero
/// prices the same tokens lower and puts nothing on screen to say it did.
#[test]
fn dropping_the_lifetime_prices_every_write_at_the_five_minute_rate() {
    let table = PriceTable::bundled().expect("the bundled price table parses");

    let flattened: f64 = per_message_records()
        .into_iter()
        .map(|usage| {
            usd(
                &table,
                &niobe_core::Usage {
                    cache_write_1h: Some(0),
                    ..usage
                },
            )
        })
        .sum();

    // The same 185 writes at $2.50 rather than 135 of them at $4.00:
    // 18 + 1150 + 1140 + 462.5 = 2770.5 millionths of a dollar.
    assert_usd(flattened, 0.002_770_5);

    // $0.0002025 of the turn, and 30.5% of what its cache writes cost, on a
    // recording of six messages. The share is not a rounding difference.
    assert_usd(0.002_973 - flattened, 0.000_202_5);
}

/// One message that took two API requests, as Claude Code 2.1.278 streams
/// it: the `message_start` splits the first request's writes beside its
/// total, and the `message_delta` sums both requests and splits them only
/// per request. The first request bought its 200 writes for five minutes and
/// the second its 300 for an hour, so the message's writes are priced at
/// both rates, not at the first request's alone.
#[test]
fn a_message_of_two_requests_prices_each_requests_writes_at_its_own_lifetime() {
    let table = PriceTable::bundled().expect("the bundled price table parses");
    let mut translator = Translator::new("metered");
    let lines = [
        r#"{"type":"system","subtype":"init","session_id":"s-1","model":"claude-sonnet-5"}"#,
        r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"claude-sonnet-5","usage":{"input_tokens":4,"cache_creation_input_tokens":200,"cache_read_input_tokens":1000,"cache_creation":{"ephemeral_5m_input_tokens":200,"ephemeral_1h_input_tokens":0},"output_tokens":1}}}}"#,
        r#"{"type":"stream_event","event":{"type":"message_delta","usage":{"input_tokens":10,"cache_creation_input_tokens":500,"cache_read_input_tokens":3000,"output_tokens":90,"iterations":[{"input_tokens":4,"output_tokens":40,"cache_read_input_tokens":1000,"cache_creation_input_tokens":200,"cache_creation":{"ephemeral_5m_input_tokens":200,"ephemeral_1h_input_tokens":0},"type":"message"},{"input_tokens":6,"output_tokens":50,"cache_read_input_tokens":2000,"cache_creation_input_tokens":300,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":300},"type":"message"}]}}}"#,
    ];
    let records: Vec<niobe_core::Usage> = lines
        .iter()
        .flat_map(|line| translator.line(line))
        .filter_map(|event| match event {
            Event::Usage(usage) => Some(usage),
            _ => None,
        })
        .collect();
    let [message] = records.as_slice() else {
        panic!("one message, one record: {records:?}");
    };
    assert_eq!(
        (message.cache_write, message.cache_write_1h),
        (500, Some(300))
    );

    // Claude Sonnet 5: 10×2.00 + 90×10.00 + 3000×0.20 + 200×2.50 + 300×4.00
    //   = 20 + 900 + 600 + 500 + 1200 = 3220 millionths of a dollar. Every
    //   write at the first request's five minutes would be 2770.
    assert_usd(usd(&table, message), 0.003_220);
}
