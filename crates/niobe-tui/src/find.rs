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
    /// Which match this is, by what it was found in.
    pub(crate) which: Which,
}

/// Which match of the transcript one is, named by what it was found in rather
/// than by how far down it is: the entry, whether in the heading drawn over a
/// sub-agent's rows from that entry, and how many matches in the same lines
/// come before it.
///
/// Folding a run of calls, or opening a diff, adds and takes away lines and
/// matches above a match without changing what it was found in, so this is
/// what the match stepped to is kept by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Which {
    pub(crate) entry: usize,
    pub(crate) heading: bool,
    pub(crate) nth: usize,
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

    /// The match found in lines that start `first` lines further down, in
    /// the entry and heading `which` names, keeping its own place among the
    /// matches in them.
    pub(crate) fn placed(&self, first: usize, which: Which) -> Self {
        Self {
            line: self.line.saturating_add(first),
            start: self.start,
            len: self.len,
            more: self
                .more
                .iter()
                .map(|&(line, start, len)| (line.saturating_add(first), start, len))
                .collect(),
            which: Which {
                nth: self.which.nth,
                ..which
            },
        }
    }
}

/// A query, ready to be compared against lines.
///
/// A run of whitespace in it is one space, and matches any run of whitespace
/// in the transcript — including the break where the pane wrapped a line and
/// the indent the next one starts with, and the quote's bar or code block's
/// gutter that line is drawn behind again, so a phrase is found wherever it
/// was cut.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// bottom and not overlapping, each numbered among them in its
    /// [`Which::nth`]. The lines do not say what they were drawn from, so
    /// the rest of each [`Which`] is left to [`Found::placed`].
    ///
    /// The lines are read as one text with a space between each, so a match
    /// can run from one line on to the next. Lines of different entries are
    /// never joined.
    ///
    /// `read` keeps that text between searches of the same lines: it is
    /// read again only where it is absent or was read for a query that
    /// ignored the case where this one does not, or the other way round.
    /// The caller empties it whenever the lines change.
    pub(crate) fn in_lines(
        &self,
        lines: &[Line<'_>],
        first: usize,
        read: &mut Option<Text>,
    ) -> Vec<Found> {
        let text = match read {
            Some(text) if text.exact == self.exact => text,
            _ => read.insert(Text::of(lines, self.exact)),
        };
        let mut found = Vec::new();
        let mut at = 0;
        let mut counted = Counted::default();
        while let Some(start) = self.next_start(&text.text, at) {
            match self.matches_at(&text.text, start).and_then(|end| {
                counted = text.count_to(counted, start);
                Some((end, parts(text.places(counted, end, first))?))
            }) {
                Some((end, mut parts)) => {
                    parts.which.nth = found.len();
                    found.push(parts);
                    at = end;
                }
                None => at = start + text.width_at(start),
            }
        }
        found
    }

    /// The first place from byte `at` on in `text` a match could start:
    /// where the query's first character is. Looking for that one character
    /// is what keeps a key typed into the search quick over a long reply,
    /// which is mostly places no match starts.
    fn next_start(&self, text: &str, at: usize) -> Option<usize> {
        let rest = text.get(at..)?;
        let offset = match self.chars.first()? {
            ' ' => rest.find(char::is_whitespace),
            want => rest.find(*want),
        }?;
        Some(at + offset)
    }

    /// The byte a match that starts at byte `at` of `text` ends before, if
    /// one starts there.
    fn matches_at(&self, text: &str, at: usize) -> Option<usize> {
        let mut rest = text.get(at..)?.chars().peekable();
        let mut here = at;
        for want in &self.chars {
            let c = *rest.peek()?;
            match want {
                ' ' => {
                    if !c.is_whitespace() {
                        return None;
                    }
                    while let Some(c) = rest.next_if(|c| c.is_whitespace()) {
                        here += c.len_utf8();
                    }
                }
                _ if c == *want => {
                    rest.next();
                    here += c.len_utf8();
                }
                _ => return None,
            }
        }
        Some(here)
    }
}

/// One entry's lines read as one text, as a comparison sees them, with a
/// space between each line and the next, and the marks a line carries on a
/// block behind read as the blanks they stand in front of.
///
/// Kept as one string and the byte each line starts at, rather than as each
/// character beside its place, and kept with the lines it was read from
/// rather than read again for every key typed into the search: reading a
/// reply of megabytes is what a key would otherwise spend its time on. A
/// place is worked out only for the characters a match covers.
#[derive(Debug)]
pub(crate) struct Text {
    text: String,
    /// Whether the case was kept, for a query with a capital in it.
    exact: bool,
    /// The byte of `text` each line starts at. The space joining a line to
    /// the next is the byte before the next one's start.
    starts: Vec<usize>,
}

impl Text {
    fn of(lines: &[Line<'_>], exact: bool) -> Self {
        let bytes = lines
            .iter()
            .flat_map(|line| &line.spans)
            .map(|span| span.content.len())
            .fold(lines.len(), usize::saturating_add);
        let mut text = String::with_capacity(bytes);
        let mut starts = Vec::with_capacity(lines.len());
        for (row, line) in lines.iter().enumerate() {
            if row > 0 {
                text.push(' ');
            }
            starts.push(text.len());
            let mut carried = row
                .checked_sub(1)
                .and_then(|above| lines.get(above))
                .map_or(0, |above| carried_lead(above, line));
            for span in &line.spans {
                let content: &str = &span.content;
                let split = content
                    .char_indices()
                    .nth(carried)
                    .map_or(content.len(), |(at, _)| at);
                let (lead, rest) = content.split_at(split);
                carried = carried.saturating_sub(lead.chars().count());
                text.extend(
                    lead.chars()
                        .map(|c| if c.is_whitespace() { c } else { ' ' }),
                );
                push_folded(&mut text, rest, exact);
            }
        }
        Self {
            text,
            exact,
            starts,
        }
    }

    /// How many bytes the character at byte `at` takes, or one past the
    /// end, so that a search moved on by it always moves on.
    fn width_at(&self, at: usize) -> usize {
        self.text
            .get(at..)
            .and_then(|rest| rest.chars().next())
            .map_or(1, char::len_utf8)
    }

    /// `counted` moved on to byte `at`, which is not before it: the line
    /// `at` is on and its character of that line, counted from where
    /// `counted` was where that is the same line, so that the matches of a
    /// long line are not each counted from its start.
    fn count_to(&self, counted: Counted, at: usize) -> Counted {
        let row = self
            .starts
            .partition_point(|line| *line <= at)
            .saturating_sub(1);
        let (from, column) = match row == counted.row && counted.at <= at {
            true => (counted.at, counted.column),
            false => (self.starts.get(row).copied().unwrap_or(at), 0),
        };
        let between = self
            .text
            .get(from..at)
            .map_or(0, |text| text.chars().count());
        Counted {
            at,
            row,
            column: column.saturating_add(between),
        }
    }

    /// The places of the characters from `start` to byte `end`, in order,
    /// each as its line — counted from `first` — its character of that
    /// line, and whether it is whitespace. The space joining two lines is
    /// on neither and has no place.
    fn places(
        &self,
        start: Counted,
        end: usize,
        first: usize,
    ) -> impl Iterator<Item = (usize, usize, bool)> + '_ {
        let Counted {
            at: from,
            mut row,
            mut column,
        } = start;
        let covered = self.text.get(from..end).unwrap_or_default();
        covered.char_indices().filter_map(move |(offset, c)| {
            let at = from + offset;
            if self.starts.get(row + 1) == Some(&(at + 1)) {
                row += 1;
                column = 0;
                return None;
            }
            column += 1;
            Some((first.saturating_add(row), column - 1, c.is_whitespace()))
        })
    }
}

/// How many characters `line` starts with that are blank or the marks of a
/// block it carries on from `above` — a quote's bar or a code block's gutter
/// drawn in the same column of both — up to the last such mark: what a
/// comparison reads as the space between the words either side of the
/// break, as it reads an indent.
fn carried_lead(above: &Line<'_>, line: &Line<'_>) -> usize {
    let mut over = above.spans.iter().flat_map(|span| span.content.chars());
    let mut lead = 0;
    let mut carried = 0;
    for c in line.spans.iter().flat_map(|span| span.content.chars()) {
        let above = over.next();
        if c.is_whitespace() {
            lead += 1;
        } else if above == Some(c) && crate::markdown::is_carried_mark(c) {
            lead += 1;
            carried = lead;
        } else {
            break;
        }
    }
    carried
}

/// A byte of a [`Text`] with the line it is on and its character of that
/// line, counted from the line's first.
#[derive(Debug, Clone, Copy, Default)]
struct Counted {
    at: usize,
    row: usize,
    column: usize,
}

/// Appends `content` to `text` as a comparison sees it. Text that is all
/// ASCII, which most of a transcript is, is copied whole and lowered in
/// place rather than a character at a time.
fn push_folded(text: &mut String, content: &str, exact: bool) {
    if exact {
        text.push_str(content);
    } else if content.is_ascii() {
        let from = text.len();
        text.push_str(content);
        if let Some(added) = text.get_mut(from..) {
            added.make_ascii_lowercase();
        }
    } else {
        text.extend(content.chars().map(|c| fold(c, exact)));
    }
}

/// A match, from the places of the characters it covers, in order, each
/// with whether it is whitespace: one part per line. Where a match runs on
/// to another line, the whitespace either side of the break — the end of one
/// line and the indent of the next — is the space between two words rather
/// than something found, and is not marked. `None` where the match covers no
/// character of a line at all.
fn parts(places: impl Iterator<Item = (usize, usize, bool)>) -> Option<Found> {
    let mut runs: Vec<Run> = Vec::new();
    let mut run: Option<Run> = None;
    for (line, column, space) in places {
        match run.as_mut() {
            Some(open) if open.line == line => open.add(column, space),
            _ => runs.extend(run.replace(Run::new(line, column, space))),
        }
    }
    runs.extend(run);
    let last = runs.len().checked_sub(1)?;
    let mut kept = runs.iter().enumerate().filter_map(|(index, run)| {
        let from = match index {
            0 => Some(run.first),
            _ => run.first_word,
        }?;
        let to = match index == last {
            true => Some(run.last),
            false => run.last_word,
        }?;
        Some((run.line, from, to + 1 - from))
    });
    let (line, start, len) = kept.next()?;
    Some(Found {
        line,
        start,
        len,
        more: kept.collect(),
        which: Which::default(),
    })
}

/// The characters a match covers on one line, as the columns that bound
/// them: kept as these four rather than as every column, because a key
/// typed into the search can find a match in every line of a long reply.
struct Run {
    line: usize,
    first: usize,
    last: usize,
    /// The first and last that are not whitespace, where any is not.
    first_word: Option<usize>,
    last_word: Option<usize>,
}

impl Run {
    fn new(line: usize, column: usize, space: bool) -> Self {
        let word = (!space).then_some(column);
        Self {
            line,
            first: column,
            last: column,
            first_word: word,
            last_word: word,
        }
    }

    fn add(&mut self, column: usize, space: bool) {
        self.last = column;
        if !space {
            self.first_word.get_or_insert(column);
            self.last_word = Some(column);
        }
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
            .in_lines(&[Line::from(line.to_owned())], 0, &mut None)
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
            .in_lines(&lines, 10, &mut None)
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
            found_in("zebra quokka", &["a zebra", "quokka b"]),
            vec![vec![(10, 2, 5), (11, 0, 6)]]
        );
        assert_eq!(
            found_in("zebra quokka", &["  a zebra quokka"]),
            vec![vec![(10, 4, 12)]]
        );
        assert!(found_in("zebra quokka", &["a zebra", "• quokka"]).is_empty());
    }

    /// A quote's bar and a code block's gutter are drawn again on every line
    /// the pane wrapped the block to: behind them the text carries on, so a
    /// phrase cut there is found as one, and the marks are not part of it.
    /// A mark the line above has nowhere in that column, and a bullet, which
    /// opens an item of its own, are not carried on.
    #[test]
    fn a_phrase_wrapped_inside_a_quote_is_found_behind_the_bar_carried_on() {
        assert_eq!(
            found_in("zebra quokka", &["  ▎ wrap zebra", "  ▎ quokka b"]),
            vec![vec![(10, 9, 5), (11, 4, 6)]]
        );
        assert_eq!(
            found_in("zebra quokka", &["▎ ▎ │ a zebra", "▎ ▎ │ quokka"]),
            vec![vec![(10, 8, 5), (11, 6, 6)]]
        );
        assert!(found_in("zebra quokka", &["a zebra", "▎ quokka"]).is_empty());
        assert!(found_in("zebra quokka", &["• a zebra", "• quokka"]).is_empty());
        assert_eq!(
            found_in("▎ zebra", &["  ▎ zebra", "  ▎ quokka"]),
            vec![vec![(10, 2, 7)]],
            "a bar no line above carries on to is the line's own text"
        );
    }

    /// Places are counted in characters of the line, whatever the bytes
    /// before them: an accented letter, a wide one, an empty line between
    /// two that a phrase runs across.
    #[test]
    fn a_match_after_wide_and_accented_letters_and_across_an_empty_line_is_placed_by_character() {
        assert_eq!(
            found_in("zebra", &["Café 漢字 ZEBRA zebra"]),
            vec![vec![(10, 8, 5)], vec![(10, 14, 5)]]
        );
        assert_eq!(
            found_in("zebra quokka", &["é zebra", "", "  quokka"]),
            vec![vec![(10, 2, 5), (12, 2, 6)]]
        );
    }

    /// The text a search reads is kept with the lines, and read again where
    /// the next query keeps the case and the last one did not.
    #[test]
    fn the_text_kept_between_searches_is_read_again_for_a_query_that_keeps_the_case() {
        let lines = [Line::from("ETag and etag".to_owned())];
        let mut read = None;
        let starts = |query: &str, read: &mut Option<Text>| -> Vec<usize> {
            Query::new(query)
                .expect("the query has something in it")
                .in_lines(&lines, 0, read)
                .into_iter()
                .map(|found| found.start)
                .collect()
        };
        assert_eq!(starts("etag", &mut read), vec![0, 9]);
        assert_eq!(starts("ETag", &mut read), vec![0]);
        assert_eq!(starts("etag", &mut read), vec![0, 9]);
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
