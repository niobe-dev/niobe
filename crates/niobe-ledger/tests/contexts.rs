// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The bundled table's context windows, checked against the published figures
//! by hand.
//!
//! Every expected size is the one the provider publishes for the model, quoted
//! in the comment above it, not read from the table: a mistyped window in
//! `prices.toml` must disagree with the page it was copied from.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use niobe_ledger::{Date, PriceTable};

fn bundled() -> PriceTable {
    PriceTable::bundled().expect("the bundled price table parses")
}

fn date(year: u16, month: u8, day: u8) -> Date {
    Date::new(year, month, day).expect("a real calendar date")
}

/// "Claude Fable 5.1, …, Claude Opus 5.5, Claude Opus 5, …, Claude Sonnet 5 …
/// have a 1M-token context window" — build-with-claude/context-windows. The
/// `[1m]` spelling Claude Code uses names the same window.
#[test]
fn the_claude_5_models_have_a_million_token_window() {
    let table = bundled();
    let day = date(2026, 9, 24);
    for model in [
        "claude-fable-5-1",
        "claude-opus-5-5",
        "claude-opus-5",
        "claude-opus-5[1m]",
        "claude-sonnet-5",
        "us.anthropic.claude-opus-5",
    ] {
        assert_eq!(table.context_window(model, day), Some(1_000_000), "{model}");
    }
}

/// "Other Claude models, including Claude Sonnet 4.5, have a 200k-token
/// context window"; the models overview lists Claude Haiku 4.5 at "200K
/// tokens".
#[test]
fn haiku_4_5_and_sonnet_4_5_have_two_hundred_thousand() {
    let table = bundled();
    let day = date(2026, 9, 24);
    for model in [
        "claude-haiku-4-5-20251001",
        "claude-haiku-4-5",
        "claude-sonnet-4-5",
        "claude-opus-4-5",
    ] {
        assert_eq!(table.context_window(model, day), Some(200_000), "{model}");
    }
}

/// Opus 4.6's 1M window was a beta, selected with `[1m]`, until it became the
/// default on 13 March 2026 — the same day the price table stops charging
/// long-context rates for it.
#[test]
fn opus_4_6_had_two_hundred_thousand_by_default_until_its_million_came_out_of_beta() {
    let table = bundled();
    assert_eq!(
        table.context_window("claude-opus-4-6", date(2026, 3, 12)),
        Some(200_000)
    );
    assert_eq!(
        table.context_window("claude-opus-4-6", date(2026, 3, 13)),
        Some(1_000_000)
    );
    assert_eq!(
        table.context_window("claude-opus-4-6[1m]", date(2026, 3, 12)),
        Some(1_000_000)
    );
}

/// An unlisted model has no window, as it has no price: a near match is a
/// guess, and a guessed window draws a percentage nobody measured.
#[test]
fn a_model_the_table_does_not_list_has_no_window() {
    let table = bundled();
    let day = date(2026, 9, 24);
    assert_eq!(table.context_window("claude-opus-9", day), None);
    assert_eq!(table.context_window("gpt-5.6", day), None);
    assert_eq!(table.context_window("claude-opus-5 ", day), None);
}

#[test]
fn a_model_has_no_window_before_it_was_released() {
    assert_eq!(
        bundled().context_window("claude-opus-5-5", date(2026, 9, 21)),
        None
    );
}
