// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The rule the transcript draws where a turn ended, with what that turn did
//! and spent written on it:
//! `── turn 46 14:05 · 2 sub-agents · 31 tool calls · 6400 tok · 1% of 5h · 38s ───`.
//!
//! It is the finest-grained cost the shell shows without a pane being opened,
//! so every figure on it is one the session fold, the transcript or the
//! shell's clock measured, and a figure none measured is left off rather than
//! drawn as a zero. Where the pane is too narrow for all of them, whole
//! figures give way — the time first, then the counts of agents and calls,
//! the duration, the window's share — and the tokens last, because they are
//! what the turn cost. A figure is never cut short.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::TurnRule;
use crate::clock;
use crate::text;
use crate::theme::Theme;
use crate::ui::compact;

/// What the rule opens with, before the turn's number.
const OPENING: &str = "── turn ";

/// What stands between one figure and the next.
const BETWEEN: &str = " · ";

/// The rule under a finished turn, `width` cells wide, and the blank line
/// after it.
pub(crate) fn lines(rule: &TurnRule, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    vec![line(rule, width, theme), Line::from("")]
}

fn line(rule: &TurnRule, width: usize, theme: &Theme) -> Line<'static> {
    let dim = Style::new().fg(theme.dim);
    // A turn that was cut off says so beside its number, where it is never
    // given up for room: its figures stop where it was cut.
    let head = match rule.cut {
        true => format!("{OPENING}{} (cut)", rule.number),
        false => format!("{OPENING}{}", rule.number),
    };
    let mut figures = figures(rule);
    while text::width(&head) + figures_width(&figures) + 2 > width {
        match dropped_first(&figures) {
            Some(at) => {
                figures.remove(at);
            }
            None => break,
        }
    }

    let mut spans = vec![Span::styled(text::truncate(&head, width), dim)];
    for (at, figure) in figures.iter().enumerate() {
        let before = match (at, figure.kind) {
            (0, _) | (_, Kind::Ended) => " ",
            _ => BETWEEN,
        };
        spans.push(Span::styled(before, dim));
        let style = match figure.kind {
            Kind::Share => Style::new().fg(theme.fg),
            Kind::Ended | Kind::Agents | Kind::Calls | Kind::Tokens | Kind::Took => dim,
        };
        spans.push(Span::styled(figure.text.clone(), style));
    }
    let used: usize = spans.iter().map(|span| text::width(&span.content)).sum();
    let rest = width.saturating_sub(used);
    if rest >= 2 {
        spans.push(Span::styled(format!(" {}", "─".repeat(rest - 1)), dim));
    }
    Line::from(spans)
}

/// Which figure a figure is, which decides what it is drawn after and the
/// order figures give way in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ended,
    Agents,
    Calls,
    Tokens,
    Share,
    Took,
}

/// The order figures are dropped in when the rule does not fit: the time the
/// turn ended is on the clock in the menu row, what the turn did is in the
/// rows above the rule, and what it cost goes last.
const GIVES_WAY: [Kind; 6] = [
    Kind::Ended,
    Kind::Agents,
    Kind::Calls,
    Kind::Took,
    Kind::Share,
    Kind::Tokens,
];

struct Figure {
    kind: Kind,
    text: String,
}

/// Every figure the turn has, in the order they are drawn.
fn figures(rule: &TurnRule) -> Vec<Figure> {
    let mut figures = Vec::new();
    if let Some(ended) = rule.ended {
        figures.push(Figure {
            kind: Kind::Ended,
            text: ended.to_string(),
        });
    }
    // A count of nothing is left off: `0 sub-agents` is most turns, and the
    // rule would spend its room saying so.
    for (count, one, many, kind) in [
        (rule.agents, "sub-agent", "sub-agents", Kind::Agents),
        (rule.calls, "tool call", "tool calls", Kind::Calls),
    ] {
        if count > 0 {
            figures.push(Figure {
                kind,
                text: match count {
                    1 => format!("1 {one}"),
                    count => format!("{count} {many}"),
                },
            });
        }
    }
    if let Some(tokens) = rule.tokens {
        figures.push(Figure {
            kind: Kind::Tokens,
            text: format!("{} tok", compact(tokens)),
        });
    }
    if let Some(points) = rule.five_hour_points {
        figures.push(Figure {
            kind: Kind::Share,
            text: share(points),
        });
    }
    if let Some(took) = rule.took {
        figures.push(Figure {
            kind: Kind::Took,
            text: clock::took(took),
        });
    }
    figures
}

/// A window's move in whole points. A move that rounds to none is still one
/// the backend measured, so it says it was under a point rather than zero.
fn share(points: u64) -> String {
    match points {
        0 => "<1% of 5h".to_owned(),
        _ => format!("{points}% of 5h"),
    }
}

/// How wide the figures are drawn, with what stands before each.
fn figures_width(figures: &[Figure]) -> usize {
    figures
        .iter()
        .enumerate()
        .map(|(at, figure)| {
            let before = match (at, figure.kind) {
                (0, _) | (_, Kind::Ended) => 1,
                _ => text::width(BETWEEN),
            };
            before + text::width(&figure.text)
        })
        .sum()
}

/// Where the figure to drop next is, or `None` when none is left.
fn dropped_first(figures: &[Figure]) -> Option<usize> {
    GIVES_WAY
        .iter()
        .find_map(|kind| figures.iter().position(|figure| figure.kind == *kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::LocalTime;
    use std::time::Duration;

    fn rule() -> TurnRule {
        TurnRule {
            number: 46,
            ended: LocalTime::new(14, 5),
            tokens: Some(6_400),
            five_hour_points: Some(1),
            took: Some(Duration::from_secs(38)),
            agents: 0,
            calls: 0,
            cut: false,
        }
    }

    fn drawn(rule: &TurnRule, width: usize) -> String {
        line(rule, width, &Theme::default())
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_rule_that_fits_says_every_figure_and_runs_to_the_edge() {
        let said = drawn(&rule(), 80);
        assert!(
            said.starts_with("── turn 46 14:05 · 6400 tok · 1% of 5h · 38s ─"),
            "{said}"
        );
        assert_eq!(text::width(&said), 80, "{said:?}");
    }

    #[test]
    fn a_rule_says_how_many_agents_and_calls_the_turn_had_and_they_give_way_early() {
        let busy = TurnRule {
            agents: 9,
            calls: 83,
            ..rule()
        };
        let said = drawn(&busy, 80);
        assert!(
            said.starts_with(
                "── turn 46 14:05 · 9 sub-agents · 83 tool calls · 6400 tok · 1% of 5h · 38s ─"
            ),
            "{said}"
        );
        let one = drawn(
            &TurnRule {
                agents: 1,
                calls: 1,
                ..rule()
            },
            80,
        );
        assert!(one.contains("· 1 sub-agent · 1 tool call ·"), "{one}");
        let narrow = drawn(&busy, 50);
        assert!(
            narrow.starts_with("── turn 46 6400 tok · 1% of 5h · 38s"),
            "{narrow}"
        );
    }

    #[test]
    fn a_turn_that_was_cut_off_says_so_beside_its_number() {
        let said = drawn(
            &TurnRule {
                cut: true,
                ..rule()
            },
            80,
        );
        assert!(said.starts_with("── turn 46 (cut) 14:05 ·"), "{said}");
        let narrow = drawn(
            &TurnRule {
                cut: true,
                ..rule()
            },
            24,
        );
        assert!(narrow.starts_with("── turn 46 (cut)"), "{narrow}");
    }

    #[test]
    fn a_move_under_one_point_is_not_drawn_as_none() {
        let said = drawn(
            &TurnRule {
                five_hour_points: Some(0),
                ..rule()
            },
            80,
        );
        assert!(said.contains("· <1% of 5h ·"), "{said}");
    }

    #[test]
    fn a_figure_nobody_measured_is_left_off_not_zeroed() {
        let said = drawn(
            &TurnRule {
                ended: None,
                five_hour_points: None,
                took: None,
                ..rule()
            },
            80,
        );
        assert!(said.starts_with("── turn 46 6400 tok ─"), "{said}");
        assert!(!said.contains("of 5h"), "{said}");
        assert!(!said.contains("0s"), "{said}");
    }

    /// A turn no usage was reported for — interrupted, or failed before its
    /// first message — draws no token figure, never `0 tok`.
    #[test]
    fn a_turn_with_no_usage_reported_draws_no_token_figure() {
        let said = drawn(
            &TurnRule {
                tokens: None,
                ..rule()
            },
            80,
        );
        assert!(
            said.starts_with("── turn 46 14:05 · 1% of 5h · 38s ─"),
            "{said}"
        );
        assert!(!said.contains("tok"), "{said}");
    }

    #[test]
    fn a_turn_that_reported_spending_nothing_reads_zero() {
        let said = drawn(
            &TurnRule {
                tokens: Some(0),
                ..rule()
            },
            80,
        );
        assert!(said.contains(" · 0 tok · "), "{said}");
    }

    /// At every width from the narrowest that holds the turn's number to the
    /// widest, what is drawn is whole figures from the full rule, each once,
    /// in its order — never the front of one — and the rule never runs past
    /// the pane.
    #[test]
    fn a_narrow_rule_drops_whole_figures_and_never_cuts_one() {
        let whole = ["14:05", "6400 tok", "1% of 5h", "38s"];
        for width in 12..=80 {
            let said = drawn(&rule(), width);
            assert!(text::width(&said) <= width, "{width}: {said:?}");
            let figures = said
                .trim_start_matches("── turn 46")
                .trim_end_matches('─')
                .trim();
            let drawn: Vec<&str> = figures
                .split(" · ")
                .flat_map(|part| match part.strip_prefix("14:05 ") {
                    Some(rest) => vec!["14:05", rest],
                    None => vec![part],
                })
                .filter(|part| !part.is_empty())
                .collect();
            let mut order = whole.iter().filter(|figure| drawn.contains(figure));
            for figure in &drawn {
                assert_eq!(order.next(), Some(figure), "{width}: {said:?}");
            }
        }
    }

    #[test]
    fn the_tokens_are_the_last_figure_to_give_way() {
        let said = drawn(&rule(), 24);
        assert!(said.starts_with("── turn 46 6400 tok"), "{said}");
    }
}
