// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Who spent the session's tokens, and what the cache saved.
//!
//! The arithmetic behind the Usage pane's second block, separate from the
//! drawing so that each figure can be asserted rather than read off a picture.
//! Three questions, and each is answered here once:
//!
//! * **Whose tokens are these?** [`shares`] apportions whole percent.
//! * **What is this model called?** [`labels`] shortens ids without letting
//!   two of them read as one, and [`fitted`] cuts them to a narrow pane
//!   without letting two of them read as one there either.
//! * **What did the cache save?** [`cache_hit_rate`].

use std::collections::{BTreeMap, BTreeSet};

use niobe_core::session::Totals;

use crate::text;

/// Each count as whole percent of their sum, apportioned so that the figures
/// add up to exactly 100.
///
/// Rounding each share on its own gives a column that reads 99% or 101%, which
/// is the pane arguing with itself in front of the operator. The largest
/// remainder gets the leftover percent instead: every share is rounded down,
/// and the percent still unassigned goes to whoever was cut by most.
///
/// An empty input, or counts that are all zero, apportion nothing: there is no
/// share of nothing, and a row of `0%` would claim one.
pub fn shares(counts: &[u64]) -> Vec<u64> {
    let total: u64 = counts.iter().copied().fold(0, u64::saturating_add);
    if total == 0 {
        return vec![0; counts.len()];
    }

    // In 128 bits: a count past a hundredth of what 64 hold would overflow
    // when it is multiplied by a hundred.
    let wide = |count: u64| u128::from(count) * 100;
    let total_wide = u128::from(total.max(1));
    let mut apportioned: Vec<u64> = counts
        .iter()
        .map(|&count| u64::try_from(wide(count) / total_wide).unwrap_or(100))
        .collect();
    let assigned: u64 = apportioned.iter().copied().fold(0, u64::saturating_add);

    // The remainder each share was cut by, as a numerator over the same
    // denominator, so the comparison is exact rather than in floating point.
    let mut by_remainder: Vec<usize> = (0..counts.len()).collect();
    by_remainder.sort_by(|&a, &b| {
        let remainder = |at: usize| wide(counts[at]) % total_wide;
        remainder(b)
            .cmp(&remainder(a))
            .then_with(|| counts[b].cmp(&counts[a]))
            .then(a.cmp(&b))
    });

    for at in by_remainder
        .into_iter()
        .take((100 - assigned.min(100)) as usize)
    {
        apportioned[at] += 1;
    }
    apportioned
}

/// What each model id is called in a row, in the order they were given.
///
/// A model is shown as the backend reported it, shortened only where the
/// shortening cannot be mistaken for another model on screen: the vendor
/// prefix and a release date carry nothing the operator reads a row for, and
/// `claude-sonnet-5-20250929` would take two thirds of a pane forty columns
/// wide. Everything else is left alone — a billing suffix like `[1m]` is the
/// difference between two rates and never dropped.
///
/// Where two ids shorten to one label, **both keep their full ids**. Two rows
/// reading `opus-5` with different tokens beside them is a pane that cannot be
/// read at all, and the operator would have no way to tell which rate either
/// row was billed at.
pub fn labels<'a>(ids: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let ids: Vec<&str> = ids.into_iter().collect();
    let short: Vec<String> = ids.iter().map(|id| shorten(id)).collect();

    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for label in &short {
        *seen.entry(label.as_str()).or_insert(0) += 1;
    }

    ids.iter()
        .zip(&short)
        .map(|(id, label)| match seen.get(label.as_str()) {
            Some(&1) => label.clone(),
            _ => (*id).to_owned(),
        })
        .collect()
}

/// `labels` cut to `columns` cells each, for a pane too narrow for them.
///
/// Cut at the end, where an id's vendor and family are kept — unless two cut
/// labels would read alike, as two ids that differ only in their date do;
/// then every one is cut at the front, where the date is.
pub fn fitted(labels: &[String], columns: usize) -> Vec<String> {
    let ends: Vec<String> = labels
        .iter()
        .map(|label| text::truncate(label, columns))
        .collect();
    let distinct = ends.iter().collect::<BTreeSet<_>>().len() == ends.len();
    match distinct {
        true => ends,
        false => labels
            .iter()
            .map(|label| text::truncate_start(label, columns))
            .collect(),
    }
}

/// What each model id is called where only a word fits, in the order they
/// were given: its family — `opus`, `sonnet` — where no other id in the list
/// is of the same family, and its [`labels`] name where one is.
///
/// Every id that is the same id is one model and takes one name: two agents
/// answered by `claude-opus-5-5` are both `opus`.
pub fn families<'a>(ids: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let ids: Vec<&str> = ids.into_iter().collect();
    let family = |id: &str| -> String {
        let short = shorten(id);
        match short.split_once('-') {
            Some((family, _)) if !family.is_empty() => family.to_owned(),
            _ => short,
        }
    };
    let labels = labels(ids.iter().copied().collect::<BTreeSet<_>>());
    let distinct: Vec<&str> = ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    ids.iter()
        .map(|id| {
            let shared = distinct
                .iter()
                .any(|other| other != id && family(other) == family(id));
            match shared {
                false => family(id),
                true => distinct
                    .iter()
                    .position(|other| other == id)
                    .and_then(|at| labels.get(at).cloned())
                    .unwrap_or_else(|| (*id).to_owned()),
            }
        })
        .collect()
}

/// One id with its vendor prefix and release date taken off.
fn shorten(id: &str) -> String {
    let trimmed = id.strip_prefix("claude-").unwrap_or(id);
    match trimmed.rsplit_once('-') {
        Some((head, tail)) if tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) => {
            head.to_owned()
        }
        _ => trimmed.to_owned(),
    }
}

/// Cache reads as a share of every input token the session sent — reads, cache
/// writes and uncached input together.
///
/// Those three are disjoint parts of one prompt and together are the whole of
/// it, and the prompt is the only part of a request a cache could ever have
/// served: output and reasoning tokens were never eligible, so counting them
/// would push the rate down for a reason that has nothing to do with caching.
/// `cache_write_1h` is a share of the writes rather than more of them, so it is
/// not added again.
///
/// `None` where the session has reported no input token at all: nothing has
/// been eligible yet, and the row reads an em dash. A `0%` there would say the
/// cache was offered the work and missed it.
pub fn cache_hit_rate(totals: &Totals) -> Option<f64> {
    let eligible = totals
        .input
        .saturating_add(totals.cache_read)
        .saturating_add(totals.cache_write);
    match eligible {
        0 => None,
        eligible => Some(totals.cache_read as f64 / eligible as f64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use niobe_core::event::{Event, Usage};
    use niobe_core::session::SessionState;

    #[test]
    fn a_model_is_named_by_its_family_unless_another_model_is_of_it_too() {
        assert_eq!(
            families([
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "claude-opus-5-5",
                "claude-haiku-4-5-20251001",
            ]),
            ["opus", "sonnet", "opus", "haiku"]
        );
        assert_eq!(
            families(["claude-opus-5-5", "claude-opus-4-1", "claude-sonnet-5-5"]),
            ["opus-5-5", "opus-4-1", "sonnet"]
        );
        assert_eq!(families(["gpt5"]), ["gpt5"]);
    }

    #[test]
    fn counts_past_a_hundredth_of_what_64_bits_hold_are_shared_without_overflow() {
        assert_eq!(shares(&[u64::MAX, 1]), [100, 0]);
        assert_eq!(shares(&[u64::MAX / 100 * 98, u64::MAX / 100 * 2]), [98, 2]);
    }

    #[test]
    fn one_count_takes_the_whole_hundred() {
        assert_eq!(shares(&[62_000]), vec![100]);
    }

    /// Three shares rounded one at a time read 82 + 15 + 3 here only by luck;
    /// the case that matters is the one where they do not, and the column has
    /// to add up either way.
    #[test]
    fn three_shares_add_up_to_a_hundred() {
        assert_eq!(shares(&[62_000, 11_000, 3_000]).iter().sum::<u64>(), 100);
        assert_eq!(shares(&[1, 1, 1]), vec![34, 33, 33]);
        assert_eq!(shares(&[1, 1, 1]).iter().sum::<u64>(), 100);
    }

    /// Five, four and four of thirteen floor to 38, 30 and 30 — two percent
    /// nobody was given. They go to the two shares the flooring cut by most,
    /// which here are the *smaller* counts (10/13 each against the largest
    /// share's 6/13), not the biggest row and not the first one.
    #[test]
    fn the_percent_nobody_was_given_goes_to_the_shares_cut_by_most() {
        assert_eq!(shares(&[5, 4, 4]), vec![38, 31, 31]);
        assert_eq!(shares(&[5, 4, 4]).iter().sum::<u64>(), 100);
    }

    /// The column always adds up to 100, so a model that spent under half a
    /// percent can round to nothing when the others take every point.
    #[test]
    fn a_share_under_half_a_percent_can_round_to_zero() {
        let apportioned = shares(&[99_600, 200, 200]);
        assert_eq!(apportioned.iter().sum::<u64>(), 100);
        assert_eq!(apportioned, vec![100, 0, 0]);
    }

    /// The points left over after rounding down go by remainder, so a smaller
    /// row can be the one that gets one.
    #[test]
    fn the_points_left_over_go_to_the_largest_remainders() {
        let apportioned = shares(&[5, 4, 4]);
        assert_eq!(apportioned.iter().sum::<u64>(), 100);
        assert_eq!(apportioned, vec![38, 31, 31]);
    }

    #[test]
    fn nothing_spent_apportions_nothing() {
        assert_eq!(shares(&[]), Vec::<u64>::new());
        assert_eq!(shares(&[0, 0]), vec![0, 0]);
    }

    #[test]
    fn a_vendor_prefix_and_a_release_date_are_not_what_a_row_is_read_for() {
        assert_eq!(labels(["claude-opus-5"]), ["opus-5"]);
        assert_eq!(labels(["claude-sonnet-5-20250929"]), ["sonnet-5"]);
        assert_eq!(labels(["claude-haiku-4-5-20251001"]), ["haiku-4-5"]);
    }

    /// A billing suffix is the difference between two rates, so it survives:
    /// a row that dropped it would name the wrong price.
    #[test]
    fn a_billing_suffix_survives_the_shortening() {
        assert_eq!(labels(["claude-opus-5[1m]"]), ["opus-5[1m]"]);
    }

    /// The collision this rule exists for: the same model at two context
    /// windows bills at two rates, and the two ids shorten alike.
    #[test]
    fn two_ids_that_shorten_alike_both_keep_their_full_ids() {
        assert_eq!(
            labels(["claude-opus-5", "opus-5"]),
            ["claude-opus-5", "opus-5"]
        );
        // And a third id that shortens to something of its own is unaffected
        // by the two that collide.
        assert_eq!(
            labels(["claude-opus-5", "opus-5", "claude-sonnet-5"]),
            ["claude-opus-5", "opus-5", "sonnet-5"]
        );
    }

    #[test]
    fn an_id_with_nothing_to_drop_is_left_exactly_as_reported() {
        assert_eq!(labels(["gpt-5-codex"]), ["gpt-5-codex"]);
        assert_eq!(labels(["claude-opus-5-2025"]), ["opus-5-2025"]);
    }

    #[test]
    fn a_label_wider_than_its_column_loses_its_end() {
        assert_eq!(
            fitted(&["us.anthropic.claude-sonnet-4-5".to_owned()], 12),
            ["us.anthropi…"]
        );
        assert_eq!(fitted(&["opus-5".to_owned()], 12), ["opus-5"]);
    }

    /// Two ids that differ only in their date read alike with their ends cut,
    /// which is the pane that cannot be read that [`labels`] keeps them whole
    /// to avoid.
    #[test]
    fn labels_that_would_cut_alike_lose_their_fronts_instead() {
        assert_eq!(
            fitted(
                &[
                    "claude-opus-5-20251001".to_owned(),
                    "claude-opus-5-20260101".to_owned(),
                    "sonnet-5".to_owned(),
                ],
                12
            ),
            ["…-5-20251001", "…-5-20260101", "sonnet-5"]
        );
    }

    fn folded(records: &[Usage]) -> Totals {
        let events: Vec<Event> = records.iter().cloned().map(Event::Usage).collect();
        SessionState::replay(&events).totals().clone()
    }

    fn prompt(input: u64, cache_read: u64, cache_write: u64) -> Usage {
        Usage {
            input,
            output: 500,
            cache_read,
            cache_write,
            cache_write_1h: 0,
            reasoning: 0,
            model: "opus-5".to_owned(),
            cost_usd: Some(0.01),
            cost_basis: None,
            settles_model: false,
            fast: false,
        }
    }

    /// 990 read out of 1,000 sent: ten of input and none written.
    #[test]
    fn the_hit_rate_is_reads_over_everything_the_prompt_could_have_cached() {
        let totals = folded(&[prompt(10, 990, 0)]);
        assert_eq!(cache_hit_rate(&totals), Some(0.99));
    }

    /// Output is not a prompt, so a session that wrote a great deal and read
    /// nothing back does not report a worse cache than it has.
    #[test]
    fn output_and_reasoning_are_not_in_the_denominator() {
        let mut heavy = prompt(10, 990, 0);
        heavy.output = 1_000_000;
        heavy.reasoning = 1_000_000;
        assert_eq!(cache_hit_rate(&folded(&[heavy])), Some(0.99));
    }

    /// A cache write is a prompt the cache could not serve — it is why there
    /// was a write — so it counts against the rate exactly as uncached input
    /// does.
    #[test]
    fn a_cache_write_is_a_miss_and_counts_against_the_rate() {
        let totals = folded(&[prompt(0, 500, 500)]);
        assert_eq!(cache_hit_rate(&totals), Some(0.5));
    }

    #[test]
    fn a_session_that_cached_nothing_reads_zero_rather_than_nothing() {
        assert_eq!(cache_hit_rate(&folded(&[prompt(1_000, 0, 0)])), Some(0.0));
    }

    /// Nothing has been eligible for a cache yet, which is not the same as a
    /// cache that missed. The row has no figure to draw.
    #[test]
    fn a_session_with_no_input_reported_has_no_rate_at_all() {
        assert_eq!(cache_hit_rate(&Totals::default()), None);
        let output_only = folded(&[prompt(0, 0, 0)]);
        assert_eq!(output_only.output, 500);
        assert_eq!(cache_hit_rate(&output_only), None);
    }
}
