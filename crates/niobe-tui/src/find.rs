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
use unicode_segmentation::UnicodeSegmentation;

/// One place the query was found: its line in the whole transcript, and the
/// characters of that line it covers — and where the pane wrapped the phrase
/// it matched, the lines after that it runs on to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Found {
    /// The wrapped transcript line the match starts on, counted from the
    /// first.
    pub(crate) line: usize,
    /// The first character of the match, counted in characters of the line.
    pub(crate) start: usize,
    /// How many characters of that line the match covers.
    pub(crate) len: usize,
    /// The rest of a match the pane wrapped, one `(line, start, len)` per
    /// line it runs on to, in order. Empty for a match on one line.
    pub(crate) more: Vec<(usize, usize, usize)>,
}

impl Found {
    /// Every line the match covers part of, as `(line, start, len)`.
    pub(crate) fn parts(&self) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
        std::iter::once((self.line, self.start, self.len)).chain(self.more.iter().copied())
    }

    /// The last line the match covers part of.
    pub(crate) fn last_line(&self) -> usize {
        self.more.last().map_or(self.line, |(line, _, _)| *line)
    }
}

/// A query, ready to be compared against lines.
///
/// A run of whitespace in it is one space, and matches any run of whitespace
/// in the transcript — including the break where the pane wrapped a line and
/// the indent the next one starts with, so a phrase is found wherever it was
/// cut.
#[derive(Debug)]
pub(crate) struct Query {
    chars: Vec<char>,
    exact: bool,
}

impl Query {
    /// The query, or `None` for one with nothing in it, which finds nothing.
    pub(crate) fn new(query: &str) -> Option<Self> {
        let exact = query.chars().any(char::is_uppercase);
        let mut chars: Vec<char> = Vec::new();
        for c in query.chars().map(|c| fold(c, exact)) {
            let space = c.is_whitespace();
            if !(space && chars.last().is_some_and(|last| last.is_whitespace())) {
                chars.push(if space { ' ' } else { c });
            }
        }
        (!chars.is_empty()).then_some(Self { chars, exact })
    }

    /// Every place the query is in `lines` — one transcript entry's lines as
    /// drawn, the first of them line `first` of the transcript — top to
    /// bottom and not overlapping.
    ///
    /// The lines are read as one text with a space between each, so a match
    /// can run from one line on to the next. Lines of different entries are
    /// never joined.
    pub(crate) fn in_lines(&self, lines: &[Line<'_>], first: usize) -> Vec<Found> {
        let mut text: Vec<(char, Option<(usize, usize)>)> = Vec::new();
        for (row, line) in lines.iter().enumerate() {
            if row > 0 {
                text.push((' ', None));
            }
            let chars = line.spans.iter().flat_map(|span| span.content.chars());
            for (column, c) in chars.enumerate() {
                text.push((fold(c, self.exact), Some((first + row, column))));
            }
        }
        let mut found = Vec::new();
        let mut at = 0;
        while at < text.len() {
            match self.matches_at(&text, at).and_then(|end| {
                let located = text.get(at..end)?.iter().filter_map(|(c, place)| {
                    let (line, column) = (*place)?;
                    Some((line, column, c.is_whitespace()))
                });
                Some((end, parts(located)?))
            }) {
                Some((end, parts)) => {
                    found.push(parts);
                    at = end;
                }
                None => at += 1,
            }
        }
        found
    }

    /// Where a match that starts at `at` in `text` ends, if one does.
    fn matches_at(&self, text: &[(char, Option<(usize, usize)>)], at: usize) -> Option<usize> {
        let mut here = at;
        for want in &self.chars {
            let (c, _) = text.get(here)?;
            match want {
                ' ' => {
                    if !c.is_whitespace() {
                        return None;
                    }
                    while text.get(here).is_some_and(|(c, _)| c.is_whitespace()) {
                        here += 1;
                    }
                }
                _ if c == want => here += 1,
                _ => return None,
            }
        }
        Some(here)
    }
}

/// A match, from the places of the characters it covers, in order, each
/// with whether it is whitespace: one part per line. Where a match runs on
/// to another line, the whitespace either side of the break — the end of one
/// line and the indent of the next — is the space between two words rather
/// than something found, and is not marked. `None` where the match covers no
/// character of a line at all.
fn parts(places: impl Iterator<Item = (usize, usize, bool)>) -> Option<Found> {
    let mut lines: Vec<(usize, Vec<(usize, bool)>)> = Vec::new();
    for (line, column, space) in places {
        match lines.last_mut() {
            Some((last, columns)) if *last == line => columns.push((column, space)),
            _ => lines.push((line, vec![(column, space)])),
        }
    }
    let last = lines.len().checked_sub(1)?;
    let mut kept = lines
        .into_iter()
        .enumerate()
        .filter_map(|(index, (line, columns))| {
            let words = |(_, space): &&(usize, bool)| !*space;
            let from = match index {
                0 => columns.first(),
                _ => columns.iter().find(words),
            }?;
            let to = match index == last {
                true => columns.last(),
                false => columns.iter().rev().find(words),
            }?;
            Some((line, from.0, to.0 + 1 - from.0))
        });
    let (line, start, len) = kept.next()?;
    Some(Found {
        line,
        start,
        len,
        more: kept.collect(),
    })
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
        // By what is drawn as one character rather than by code point: an
        // accent that combines with its letter split into a span of its own
        // would be dropped by the terminal.
        for grapheme in span.content.graphemes(true) {
            let style = mark_at(at).map_or(span.style, |mark| span.style.patch(mark));
            if style != run_style && !run.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut run), run_style));
            }
            run_style = style;
            run.push_str(grapheme);
            at += grapheme.chars().count();
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
            .in_lines(&[Line::from(line.to_owned())], 0)
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

    /// Where the query's parts are in `lines`, one entry's lines as drawn.
    fn found_in(query: &str, lines: &[&str]) -> Vec<Vec<(usize, usize, usize)>> {
        let lines: Vec<Line<'static>> = lines
            .iter()
            .map(|line| Line::from((*line).to_owned()))
            .collect();
        Query::new(query)
            .expect("the query has something in it")
            .in_lines(&lines, 10)
            .into_iter()
            .map(|found| found.parts().collect())
            .collect()
    }

    /// A phrase the pane wrapped is found where it was cut: the line break
    /// and the indent the next line starts with are the space between two
    /// words.
    #[test]
    fn a_phrase_wrapped_across_two_lines_is_found_as_one_match() {
        assert_eq!(
            found_in("zebra quokka", &["  a zebra", "  quokka b"]),
            vec![vec![(10, 4, 5), (11, 2, 6)]]
        );
        assert_eq!(
            found_in("zebra quokka", &["  a zebra quokka"]),
            vec![vec![(10, 4, 12)]]
        );
        assert!(found_in("zebra quokka", &["a zebra", "• quokka"]).is_empty());
    }

    /// Drawn with a combining accent, a letter is one cell and the accent
    /// rides on it: marking the letter keeps the accent with it rather than
    /// leaving it in a span of its own, which the terminal drops.
    #[test]
    fn a_mark_keeps_a_combining_accent_with_its_letter() {
        let marked = highlight(
            Line::from("cafe\u{301} ok".to_owned()),
            &[(0, 4, Style::new().bg(Color::Yellow))],
        );
        let text: String = marked
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(text, "cafe\u{301} ok");
        assert_eq!(marked.spans[0].content, "cafe\u{301}");
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
