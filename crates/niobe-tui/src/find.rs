// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Finding text in the transcript, as it is drawn.
//!
//! The search runs over the lines the transcript pane draws rather than over
//! the events behind them, because a place found has to be a place the view can
//! be scrolled to and the operator can see marked: a match inside a folded run
//! of calls, or in the markup a reply was drawn from, would be a match nobody
//! can be shown.
//!
//! The case is ignored unless the query has a capital in it, as `less` and
//! most editors do it: a lower-case query is a quick look, and a capital is
//! typed on purpose.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// One place the query was found: its line in the whole transcript, and the
/// characters of that line it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Found {
    /// The wrapped transcript line, counted from the first.
    pub(crate) line: usize,
    /// The first character of the match, counted in characters of the line.
    pub(crate) start: usize,
    /// How many characters the match covers.
    pub(crate) len: usize,
}

/// A query, ready to be compared against lines.
#[derive(Debug)]
pub(crate) struct Query {
    chars: Vec<char>,
    exact: bool,
}

impl Query {
    /// The query, or `None` for one with nothing in it, which finds nothing.
    pub(crate) fn new(query: &str) -> Option<Self> {
        let exact = query.chars().any(char::is_uppercase);
        let chars: Vec<char> = query.chars().map(|c| fold(c, exact)).collect();
        (!chars.is_empty()).then_some(Self { chars, exact })
    }

    /// Every place the query is in `line`, left to right and not overlapping,
    /// found on line `at` of the transcript.
    pub(crate) fn in_line(&self, line: &Line<'_>, at: usize) -> Vec<Found> {
        let text: Vec<char> = line
            .spans
            .iter()
            .flat_map(|span| span.content.chars())
            .map(|c| fold(c, self.exact))
            .collect();
        let len = self.chars.len();
        let mut found = Vec::new();
        let mut start = 0;
        while start + len <= text.len() {
            if text.get(start..start + len) == Some(self.chars.as_slice()) {
                found.push(Found {
                    line: at,
                    start,
                    len,
                });
                start += len;
            } else {
                start += 1;
            }
        }
        found
    }
}

/// A character as a comparison sees it.
fn fold(c: char, exact: bool) -> char {
    if exact {
        c
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// `line` with each of `marks` — a first character, a length and the style
/// laid over what the characters already had — drawn in.
///
/// The spans are split where a mark begins or ends, so a match that crosses
/// from one styled run into the next keeps each run's own colour under the
/// mark.
pub(crate) fn highlight(line: Line<'static>, marks: &[(usize, usize, Style)]) -> Line<'static> {
    if marks.is_empty() {
        return line;
    }
    let mark_at = |at: usize| {
        marks
            .iter()
            .find(|(start, len, _)| at >= *start && at < start + len)
            .map(|(_, _, style)| *style)
    };
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + marks.len() * 2);
    let mut at = 0;
    for span in &line.spans {
        let mut run = String::new();
        let mut run_style = span.style;
        for c in span.content.chars() {
            let style = mark_at(at).map_or(span.style, |mark| span.style.patch(mark));
            if style != run_style && !run.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut run), run_style));
            }
            run_style = style;
            run.push(c);
            at += 1;
        }
        if !run.is_empty() {
            spans.push(Span::styled(run, run_style));
        }
    }
    Line { spans, ..line }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::style::{Color, Stylize};

    fn found(query: &str, line: &str) -> Vec<(usize, usize)> {
        Query::new(query)
            .expect("the query has something in it")
            .in_line(&Line::from(line.to_owned()), 0)
            .into_iter()
            .map(|found| (found.start, found.len))
            .collect()
    }

    #[test]
    fn a_lower_case_query_ignores_the_case_and_a_capital_makes_it_exact() {
        assert_eq!(found("etag", "ETag and etag"), vec![(0, 4), (9, 4)]);
        assert_eq!(found("ETag", "ETag and etag"), vec![(0, 4)]);
    }

    #[test]
    fn matches_do_not_overlap_and_are_counted_in_characters() {
        assert_eq!(found("aa", "aaaa"), vec![(0, 2), (2, 2)]);
        assert_eq!(found("é", "café é"), vec![(3, 1), (5, 1)]);
    }

    #[test]
    fn an_empty_query_finds_nothing() {
        assert!(Query::new("").is_none());
    }

    #[test]
    fn a_mark_across_two_spans_keeps_each_ones_colour_under_it() {
        let line = Line::from(vec![
            Span::from("read ").fg(Color::Red),
            Span::from("cache.rs").fg(Color::Blue),
        ]);
        let mark = Style::new().bg(Color::Yellow);

        let marked = highlight(line, &[(3, 4, mark)]);

        let runs: Vec<(String, Style)> = marked
            .spans
            .iter()
            .map(|span| (span.content.to_string(), span.style))
            .collect();
        assert_eq!(
            runs,
            vec![
                ("rea".to_owned(), Style::new().fg(Color::Red)),
                (
                    "d ".to_owned(),
                    Style::new().fg(Color::Red).bg(Color::Yellow)
                ),
                (
                    "ca".to_owned(),
                    Style::new().fg(Color::Blue).bg(Color::Yellow)
                ),
                ("che.rs".to_owned(), Style::new().fg(Color::Blue)),
            ]
        );
    }
}
