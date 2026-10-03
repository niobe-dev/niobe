// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Selecting the transcript with the mouse, and the links in it.
//!
//! The shell takes the mouse for the wheel and for clicks, and a terminal
//! that reports the mouse to the program in it stops selecting text itself.
//! So the shell selects: a drag over the transcript marks the text it covers,
//! and letting go copies it. A click that does not drag opens the link under
//! it.
//!
//! Both work on the transcript as it is drawn, as a search does, in the
//! wrapped lines counted from the first and the cells of each: what is copied
//! is what the operator saw marked, and a link is the one the pointer was on.

use ratatui::text::Line;

use crate::text;

/// One cell of the transcript as drawn: its line counted from the first, and
/// its column counted from the pane's left edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Point {
    /// The wrapped transcript line.
    pub(crate) line: usize,
    /// The cell along it.
    pub(crate) column: usize,
}

/// A run of the transcript between the cell a drag started on and the one it
/// is on now, both included, read in the order the lines are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Selection {
    anchor: Point,
    head: Point,
}

impl Selection {
    /// A selection from `anchor` to `head`, whichever comes first.
    pub(crate) fn new(anchor: Point, head: Point) -> Self {
        Self { anchor, head }
    }

    /// The first cell covered and the last.
    fn ends(&self) -> (Point, Point) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }

    /// The cells of `line` covered, as a first column and the column after
    /// the last, or `None` where none are. The end of a line the selection
    /// runs on past is `usize::MAX`.
    pub(crate) fn columns_on(&self, line: usize) -> Option<(usize, usize)> {
        let (start, end) = self.ends();
        if line < start.line || line > end.line {
            return None;
        }
        let from = if line == start.line { start.column } else { 0 };
        let to = if line == end.line {
            end.column.saturating_add(1)
        } else {
            usize::MAX
        };
        Some((from, to))
    }

    /// The lines covered, from the first to the last.
    pub(crate) fn lines(&self) -> std::ops::RangeInclusive<usize> {
        let (start, end) = self.ends();
        start.line..=end.line
    }

    /// What is covered, as text: `line_at` gives each transcript line as it
    /// was drawn.
    ///
    /// Each line's trailing blanks are dropped, and the indent the lines
    /// share is taken off them all: the transcript draws a gutter before every
    /// line of a message, and copied text is wanted without it. A first line
    /// that starts part way along has no indent of its own to share.
    pub(crate) fn text(&self, line_at: impl Fn(usize) -> Option<String>) -> String {
        let (start, _) = self.ends();
        let pieces: Vec<String> = self
            .lines()
            .filter_map(|line| {
                let drawn = line_at(line)?;
                let (from, to) = self.columns_on(line)?;
                Some(cells(&drawn, from, to).trim_end().to_owned())
            })
            .collect();
        let shares_indent =
            |(at, piece): &(usize, &String)| !piece.is_empty() && (*at > 0 || start.column == 0);
        let indent = pieces
            .iter()
            .enumerate()
            .filter(shares_indent)
            .map(|(_, piece)| piece.len() - piece.trim_start_matches(' ').len())
            .min()
            .unwrap_or(0);
        pieces
            .iter()
            .enumerate()
            .map(|(at, piece)| match shares_indent(&(at, piece)) {
                true => piece.get(indent..).unwrap_or_default(),
                false => piece.as_str(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The text of a drawn line, without its styles.
pub(crate) fn plain(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// The part of `drawn` in the cells from `from` up to `to`. A character two
/// cells wide is in it where either of its cells is.
fn cells(drawn: &str, from: usize, to: usize) -> String {
    let mut at = 0;
    let mut kept = String::new();
    for (grapheme, width) in text::graphemes(drawn) {
        let end = at + width;
        if end > from && at < to {
            kept.push_str(grapheme);
        }
        at = end;
    }
    kept
}

/// The characters of `drawn` in the cells from `from` up to `to`, as a first
/// character and a count, which is how a mark is laid on a line.
pub(crate) fn chars_in(drawn: &str, from: usize, to: usize) -> (usize, usize) {
    let mut at = 0;
    let mut first = None;
    let mut count = 0;
    let mut chars = 0;
    for (grapheme, width) in text::graphemes(drawn) {
        let end = at + width;
        let n = grapheme.chars().count();
        if end > from && at < to {
            first.get_or_insert(chars);
            count += n;
        }
        chars += n;
        at = end;
    }
    (first.unwrap_or(chars), count)
}

/// The schemes a link is recognised by. A path or a bare host name is not a
/// link: too much else in a transcript reads like one.
const SCHEMES: [&str; 2] = ["https://", "http://"];

/// Whether `c` can be part of a link as written in text. Anything outside
/// ASCII is left out, so the frame and gutter characters the transcript draws
/// around text never join a link.
fn in_link(c: char) -> bool {
    c.is_ascii_graphic() && !matches!(c, '<' | '>' | '"' | '`' | '{' | '}' | '|' | '\\' | '^')
}

/// The link at cell `column` of line `row` of `lines` — one transcript entry's
/// lines as drawn, `width` cells wide — or `None` where the cell is not on
/// one.
///
/// A link too long for the line was cut where the line ended and carried on
/// at the start of the next, so a link that fills a line from where its text
/// starts takes in the start of the next. A link that ends a line the words
/// wrapped on is whole: the next line's first word is another word.
pub(crate) fn link_at(lines: &[String], width: usize, row: usize, column: usize) -> Option<String> {
    let drawn = lines.get(row)?;
    let (start, end) = word_at(drawn, column)?;
    let mut word = drawn.get(start..end)?.to_owned();

    let mut at = row;
    let mut from = start;
    while from == indent(lines.get(at)?) && at > 0 {
        let above = lines.get(at - 1)?;
        if !cut_between(above, lines.get(at)?, width) {
            break;
        }
        let (before, _) = last_word(above)?;
        word.insert_str(0, above.get(before..)?.trim_end());
        at -= 1;
        from = before;
    }
    let mut at = row;
    let mut to = end;
    while to == lines.get(at)?.trim_end().len() {
        let (this, Some(below)) = (lines.get(at)?, lines.get(at + 1)) else {
            break;
        };
        if !cut_between(this, below, width) {
            break;
        }
        let first = indent(below);
        let Some(next) = word_at(below, text::width(below.get(..first)?)) else {
            break;
        };
        word.push_str(below.get(next.0..next.1)?);
        at += 1;
        to = next.1;
    }
    link_in(&word)
}

/// The link a run of link characters holds: from its scheme, without the
/// punctuation a sentence put after it, and without a closing bracket that
/// closes one opened before the link rather than in it.
fn link_in(word: &str) -> Option<String> {
    let (start, scheme) = SCHEMES
        .iter()
        .filter_map(|scheme| Some((word.find(scheme)?, scheme.len())))
        .min()?;
    let mut link = word.get(start..)?;
    loop {
        let unopened = |close: char, open: char| {
            link.ends_with(close) && link.matches(close).count() > link.matches(open).count()
        };
        let trailing = link.ends_with(['.', ',', ';', ':', '!', '?', '\'', '*'])
            || unopened(')', '(')
            || unopened(']', '[');
        if !trailing {
            break;
        }
        link = link.get(..link.len() - 1)?;
    }
    (link.len() > scheme).then(|| link.to_owned())
}

/// The run of link characters at cell `column` of `drawn`, as byte offsets.
fn word_at(drawn: &str, column: usize) -> Option<(usize, usize)> {
    let mut at = 0;
    let mut byte = 0;
    let mut hit = None;
    for (grapheme, width) in text::graphemes(drawn) {
        if column >= at && column < at + width.max(1) {
            hit = Some(byte);
            break;
        }
        at += width;
        byte += grapheme.len();
    }
    let hit = hit?;
    if !drawn.get(hit..)?.starts_with(in_link) {
        return None;
    }
    let start = drawn
        .get(..hit)?
        .char_indices()
        .rev()
        .take_while(|(_, c)| in_link(*c))
        .last()
        .map_or(hit, |(at, _)| at);
    let end = drawn
        .get(hit..)?
        .char_indices()
        .find(|(_, c)| !in_link(*c))
        .map_or(drawn.len(), |(at, _)| hit + at);
    Some((start, end))
}

/// The last run of link characters on `drawn`, where the line ends with one.
fn last_word(drawn: &str) -> Option<(usize, usize)> {
    let trimmed = drawn.trim_end();
    let last = trimmed.chars().last()?;
    in_link(last).then_some(())?;
    word_at(drawn, text::width(trimmed).checked_sub(1)?)
}

/// The bytes of blank `drawn` starts with.
fn indent(drawn: &str) -> usize {
    drawn.len() - drawn.trim_start().len()
}

/// Whether `above` and `below` are one word the line's width cut in two.
///
/// The wrappers break a line inside a word only where the word is wider than
/// the line, and then start it on a line of its own and fill each line with
/// it. So `above` must end in a link character with no break before it since
/// where its text starts — the column `below`'s text starts at, past a bullet
/// as well as an indent — and must be filled to the width, or to one cell
/// short of it where a two-cell character did not fit in the last. A word that
/// merely did not fit after the others is an ordinary wrap, not a cut.
fn cut_between(above: &str, below: &str, width: usize) -> bool {
    let ends = above.trim_end();
    let starts = below.trim_start();
    let Some((word, _)) = last_word(above) else {
        return false;
    };
    let text_column = |line: &str, byte: usize| line.get(..byte).map(text::width);
    starts.starts_with(in_link)
        && text_column(above, word) == text_column(below, indent(below))
        && text::width(ends) + 2 > width
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: usize, column: usize) -> Point {
        Point { line, column }
    }

    fn drawn<'a>(lines: &'a [&'a str]) -> impl Fn(usize) -> Option<String> + 'a {
        |line| lines.get(line).map(|line| (*line).to_owned())
    }

    #[test]
    fn a_selection_reads_the_same_whichever_way_it_was_dragged() {
        let lines = ["  one two three", "  four five"];
        let down = Selection::new(at(0, 6), at(1, 5));
        let up = Selection::new(at(1, 5), at(0, 6));
        assert_eq!(down.text(drawn(&lines)), "two three\nfour");
        assert_eq!(up.text(drawn(&lines)), "two three\nfour");
    }

    #[test]
    fn whole_lines_lose_the_gutter_they_share_and_keep_their_own_indent() {
        let lines = ["  fn main() {", "      run();", "  }", ""];
        let selection = Selection::new(at(0, 0), at(3, 40));
        assert_eq!(
            selection.text(drawn(&lines)),
            "fn main() {\n    run();\n}\n"
        );
    }

    #[test]
    fn a_wide_character_is_copied_where_either_of_its_cells_is_covered() {
        let lines = ["a日本b"];
        assert_eq!(
            Selection::new(at(0, 2), at(0, 3)).text(drawn(&lines)),
            "日本"
        );
        assert_eq!(chars_in("a日本b", 2, 4), (1, 2));
    }

    #[test]
    fn the_cells_of_a_line_are_its_characters_for_the_mark() {
        assert_eq!(chars_in("  hello", 2, usize::MAX), (2, 5));
        assert_eq!(chars_in("  hello", 10, 12), (7, 0));
    }

    fn link(lines: &[&str], width: usize, row: usize, column: usize) -> Option<String> {
        let lines: Vec<String> = lines.iter().map(|line| (*line).to_owned()).collect();
        link_at(&lines, width, row, column)
    }

    #[test]
    fn a_click_on_a_link_finds_all_of_it_and_none_of_the_sentence_around_it() {
        let line = "  see https://example.com/a?b=1. Then";
        assert_eq!(
            link(&[line], 80, 0, 10).as_deref(),
            Some("https://example.com/a?b=1")
        );
        assert_eq!(link(&[line], 80, 0, 3), None);
        assert_eq!(link(&[line], 80, 0, 34), None);
    }

    #[test]
    fn a_link_drawn_after_its_text_in_brackets_leaves_the_bracket_out() {
        let line = "  the docs (https://example.com/wiki/Rust_(language)) say";
        assert_eq!(
            link(&[line], 80, 0, 20).as_deref(),
            Some("https://example.com/wiki/Rust_(language)")
        );
    }

    #[test]
    fn a_link_cut_at_the_end_of_a_line_is_found_from_either_half() {
        let lines = ["  https://example.com/a/very/lo", "  ng/path and more"];
        let whole = Some("https://example.com/a/very/long/path".to_owned());
        assert_eq!(link(&lines, 32, 0, 5), whole);
        assert_eq!(link(&lines, 32, 1, 3), whole);
    }

    #[test]
    fn a_link_that_ends_a_line_with_room_left_is_not_joined_to_the_next() {
        let lines = ["  go to https://example.com", "  next line"];
        assert_eq!(
            link(&lines, 80, 0, 10).as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn a_link_that_ends_a_word_wrapped_line_is_not_joined_to_the_next_word() {
        let lines = ["  read https://example.com/x", "  documentation now"];
        assert_eq!(
            link(&lines, 30, 0, 10).as_deref(),
            Some("https://example.com/x")
        );
    }

    #[test]
    fn a_link_alone_on_a_line_it_does_not_fill_is_not_joined_to_the_next_word() {
        let lines = ["  https://example.com/x", "  documentation now"];
        assert_eq!(
            link(&lines, 30, 0, 5).as_deref(),
            Some("https://example.com/x")
        );
        assert_eq!(link(&lines, 30, 1, 4), None);
    }

    #[test]
    fn a_link_cut_under_a_bullet_is_found_from_either_half() {
        let lines = ["• https://example.com/a/very/lon", "  g/path and more"];
        let whole = Some("https://example.com/a/very/long/path".to_owned());
        assert_eq!(link(&lines, 32, 0, 5), whole);
        assert_eq!(link(&lines, 32, 1, 3), whole);
    }

    #[test]
    fn a_scheme_with_nothing_after_it_is_no_link() {
        assert_eq!(link(&["  https:// alone"], 80, 0, 4), None);
    }
}
