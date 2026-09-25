// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The rule the transcript draws where a turn ended, with what that turn
//! spent written on it: `── turn 46 14:05 · 6400 tok · 1% of 5h · 38s ───`.
//!
//! It is the finest-grained cost the shell shows without a pane being opened,
//! so every figure on it is one the session fold or the shell's clock
//! measured, and a figure neither measured is left off rather than drawn as a
//! zero. Where the pane is too narrow for all of them, whole figures give way
//! — the time first, then the duration, then the window's share — and the
//! tokens last, because they are what the turn cost. A figure is never cut
//! short.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::TurnRule;
use crate::clock::spent;
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
    let head = format!("{OPENING}{}", rule.number);
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
            Kind::Ended | Kind::Tokens | Kind::Took => dim,
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
    Tokens,
    Share,
    Took,
}

/// The order figures are dropped in when the rule does not fit: the time the
/// turn ended is on the clock in the menu row, and what it cost goes last.
const GIVES_WAY: [Kind; 4] = [Kind::Ended, Kind::Took, Kind::Share, Kind::Tokens];

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
    figures.push(Figure {
        kind: Kind::Tokens,
        text: format!("{} tok", compact(rule.tokens)),
    });
    if let Some(points) = rule.five_hour_points {
        figures.push(Figure {
            kind: Kind::Share,
            text: share(points),
        });
    }
    if let Some(took) = rule.took {
        figures.push(Figure {
            kind: Kind::Took,
            text: spent(took),
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
            tokens: 6_400,
            five_hour_points: Some(1),
            took: Some(Duration::from_secs(38)),
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
