// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A turn that spent far more than the session's turns before it.
//!
//! Counted in tokens, the figure every ended turn has whatever the session is
//! billed by: on a plan there are no dollars to compare, and a spend the
//! backend reported in dollars is reported with the tokens it bought. The
//! comparison is against the turns before the one judged, so a turn is never
//! part of its own average.

use niobe_core::session::TurnRecord;

/// How many times the average a turn has to spend to be a spike.
const SPIKE: u64 = 3;

/// How many earlier turns an average needs before it is one: a single turn
/// is the session's only measure of what a turn costs, and the second turn
/// of most sessions does more than a greeting did.
const ENOUGH: u64 = 2;

/// A turn that spent more than [`SPIKE`] times what the session's earlier
/// turns averaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Spike {
    /// The turn's number, as its rule draws it.
    pub(crate) turn: u64,
    /// What the turn spent.
    pub(crate) tokens: u64,
    /// What the earlier turns spent on average, rounded down.
    pub(crate) average: u64,
    /// How many times that average the turn spent, rounded down, so it never
    /// reads as more than it was.
    pub(crate) times: u64,
}

/// The spike `turns[index]` was, where it was one.
///
/// A turn that compacted the conversation is neither judged nor counted in
/// the average: it reads the whole conversation once to shrink it, which is
/// what compaction costs rather than what the work did. A turn with no
/// measured tokens is neither either, since it has no figure to compare.
pub(crate) fn at(turns: &[TurnRecord], index: usize) -> Option<Spike> {
    let turn = turns.get(index)?;
    let tokens = measured(turn)?;
    let (count, sum) =
        turns[..index]
            .iter()
            .filter_map(measured)
            .fold((0_u64, 0_u128), |(count, sum), tokens| {
                (
                    count.saturating_add(1),
                    sum.saturating_add(u128::from(tokens)),
                )
            });
    if count < ENOUGH || sum == 0 {
        return None;
    }
    // Compared as `tokens / (sum / count) > SPIKE` without the division, so
    // that rounding the average down never makes a turn a spike.
    let over = u128::from(tokens).saturating_mul(u128::from(count))
        > sum.saturating_mul(u128::from(SPIKE));
    if !over {
        return None;
    }
    let average = u64::try_from(sum / u128::from(count)).unwrap_or(u64::MAX);
    let times = u64::try_from(u128::from(tokens).saturating_mul(u128::from(count)) / sum)
        .unwrap_or(u64::MAX);
    Some(Spike {
        turn: turn.number,
        tokens,
        average,
        times,
    })
}

/// What a turn spent, where it is one an average is made of.
fn measured(turn: &TurnRecord) -> Option<u64> {
    match turn.compacted {
        Some(_) => None,
        None => turn.tokens,
    }
}

#[cfg(test)]
mod tests {
    use niobe_core::event::CompactTrigger;

    use super::*;

    fn turn(number: u64, tokens: Option<u64>) -> TurnRecord {
        TurnRecord {
            number,
            tokens,
            five_hour_share: None,
            cut: false,
            compacted: None,
        }
    }

    fn turns(tokens: &[Option<u64>]) -> Vec<TurnRecord> {
        (1..)
            .zip(tokens)
            .map(|(number, tokens)| turn(number, *tokens))
            .collect()
    }

    #[test]
    fn a_turn_past_three_times_the_earlier_average_is_a_spike_named_by_its_number() {
        let turns = turns(&[Some(10_000), Some(20_000), Some(46_000)]);
        assert_eq!(
            at(&turns, 2),
            Some(Spike {
                turn: 3,
                tokens: 46_000,
                average: 15_000,
                times: 3,
            })
        );
    }

    #[test]
    fn exactly_three_times_the_average_is_not_a_spike() {
        let turns = turns(&[Some(10_000), Some(20_000), Some(45_000)]);
        assert_eq!(at(&turns, 2), None);
    }

    #[test]
    fn an_average_rounded_down_does_not_make_a_spike_of_a_turn_that_is_not_one() {
        // The average is 10.5: 31 is not three times it, though it is three
        // times the 10 the average rounds down to.
        let turns = turns(&[Some(10), Some(11), Some(31)]);
        assert_eq!(at(&turns, 2), None);
    }

    #[test]
    fn one_earlier_turn_is_not_an_average() {
        let turns = turns(&[Some(1_000), Some(90_000)]);
        assert_eq!(at(&turns, 1), None);
    }

    #[test]
    fn a_turn_with_no_measured_tokens_is_neither_judged_nor_averaged() {
        let turns = turns(&[Some(1_000), None, Some(1_000), None, Some(4_000)]);
        assert_eq!(at(&turns, 3), None);
        assert_eq!(at(&turns, 4).map(|spike| spike.average), Some(1_000));
    }

    #[test]
    fn a_compaction_is_neither_judged_nor_part_of_the_average() {
        let mut turns = turns(&[Some(1_000), Some(1_000), Some(90_000), Some(4_000)]);
        turns[2].compacted = Some(CompactTrigger::Manual);
        assert_eq!(at(&turns, 2), None);
        assert_eq!(at(&turns, 3).map(|spike| spike.times), Some(4));
    }

    #[test]
    fn earlier_turns_that_spent_nothing_give_no_average_to_be_over() {
        let turns = turns(&[Some(0), Some(0), Some(10)]);
        assert_eq!(at(&turns, 2), None);
    }

    #[test]
    fn counts_at_the_largest_there_is_do_not_wrap() {
        let turns = turns(&[Some(1), Some(1), Some(u64::MAX)]);
        let spike = at(&turns, 2).expect("the largest count is past three times one");
        assert_eq!(spike.times, u64::MAX);
    }
}
