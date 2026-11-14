// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The operator's own message, drawn as a messenger draws the messages one
//! sends: on the right of the transcript, in a bubble of a colour of its own
//! casting a shadow, its words wrapped inside it, and — where it answers a
//! question the agent put — that question quoted at its top.
//!
//! The bubble is what says whose words they are, so nothing is drawn over
//! it. Its top and bottom are half-cell blocks in the bubble's colour, which
//! give it half a row of room above and below its words without taking a
//! whole row for each, and it casts the theme's shadow a column to its right
//! and half a row below it, as a dialog does. The corners are square: a
//! quarter block rounding one is drawn at a quarter of a cell's width, which
//! leaves a notch on each side rather than a curve.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::text;
use crate::theme::Theme;

/// The cells of bubble on either side of its words.
const PADDING: usize = 1;

/// What a quoted question's rows open with, inside the bubble.
const QUOTE: &str = "▎ ";

/// The narrowest a transcript gives a bubble less than its whole width:
/// below it, a message would wrap a word or two a row.
const NARROWEST: usize = 24;

/// The columns of shadow right of a bubble.
const SHADOW: usize = 1;

/// How wide a bubble may grow in a transcript `width` cells wide: four
/// fifths of it, leaving the other side to the agent as a messenger leaves it
/// to the other party, so a long prompt still reads as a message sent — and
/// never so wide that its shadow falls off the transcript.
fn widest(width: usize) -> usize {
    let most = width.saturating_sub(SHADOW);
    match width < NARROWEST {
        true => most,
        false => (width * 4 / 5).max(NARROWEST).min(most),
    }
}

/// The cells inside a bubble, in a transcript `width` cells wide, that its
/// words are wrapped to.
pub(crate) fn room(width: usize) -> usize {
    widest(width).saturating_sub(2 * PADDING).max(1)
}

/// The message, `width` cells wide, and the blank line after it.
///
/// `words` are its rows, wrapped to [`room`] and styled; each is laid on the
/// bubble's colour. `quote` is the question it answers, or empty, drawn dim
/// at its top. `note` is what became of it, or empty, drawn dim under the
/// bubble at its right end.
pub(crate) fn lines(
    quote: &str,
    words: Vec<Line<'static>>,
    note: &str,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let room = room(width);
    let quoted = match quote.trim().is_empty() {
        true => Vec::new(),
        false => text::wrap(quote.trim(), room.saturating_sub(text::width(QUOTE)).max(1)),
    };
    let widths: Vec<usize> = words.iter().map(cells).collect();
    let inner = quoted
        .iter()
        .map(|row| text::width(QUOTE) + text::width(row))
        .chain(widths.iter().copied())
        .max()
        .unwrap_or(0)
        .clamp(1, room);
    let outer = inner + 2 * PADDING;
    let indent = || Span::raw(" ".repeat(width.saturating_sub(outer + SHADOW)));
    let bubble = Style::new().bg(theme.user_bg);
    let edge = Style::new().fg(theme.user_bg);
    let shadow = Style::new().bg(theme.shadow);
    let row = |spans: Vec<Span<'static>>, used: usize| {
        let mut row = vec![indent(), Span::styled(" ".repeat(PADDING), bubble)];
        row.extend(spans);
        let rest = inner.saturating_sub(used) + PADDING;
        row.push(Span::styled(" ".repeat(rest), bubble));
        row.push(Span::styled(" ".repeat(SHADOW), shadow));
        Line::from(row)
    };

    // The shadow starts half a row below the bubble's top, which is the top
    // of the row under this one: beside the top it is still the pane.
    let mut lines = vec![Line::from(vec![
        indent(),
        Span::styled("▄".repeat(outer), edge),
        Span::raw(" ".repeat(SHADOW)),
    ])];
    let dim = bubble.fg(theme.dim);
    for said in quoted {
        let used = text::width(QUOTE) + text::width(&said);
        lines.push(row(
            vec![Span::styled(QUOTE, dim), Span::styled(said, dim)],
            used,
        ));
    }
    for (line, used) in words.into_iter().zip(widths) {
        let spans = line
            .spans
            .into_iter()
            .map(|span| Span::styled(span.content, span.style.bg(theme.user_bg)))
            .collect();
        lines.push(row(spans, used));
    }
    // The bubble's bottom half a row over the shadow's, which is a column
    // to the right: under the bubble's first column it is still the pane.
    lines.push(Line::from(vec![
        indent(),
        Span::styled("▀".repeat(outer.min(SHADOW)), edge),
        Span::styled(
            "▀".repeat(outer.saturating_sub(SHADOW)),
            edge.bg(theme.shadow),
        ),
        Span::styled(" ".repeat(SHADOW), shadow),
    ]));
    if !note.is_empty() {
        let note = text::truncate(note, width);
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(width.saturating_sub(text::width(&note)))),
            Span::styled(note, Style::new().fg(theme.dim)),
        ]));
    }
    lines.push(Line::from(""));
    lines
}

/// Whether `line`, one of those [`lines`] draws, is the bubble's top or
/// bottom, which are drawing rather than the message's words.
///
/// Read by where [`lines`] puts each span rather than by reading the cells
/// before them: a copy asks it of every row of every prompt in the session,
/// and the cells that set a bubble against the right edge are most of a
/// short prompt's row.
pub(crate) fn is_end(line: &Line<'_>) -> bool {
    match line.spans.as_slice() {
        [_, end, _, ..] => end.content.starts_with(['▄', '▀']),
        _ => false,
    }
}

/// The spans of `line`, one of those [`lines`] draws, after the cells that
/// set the bubble against the right edge and its padding: its words, or
/// what became of the message, as a copy reads them.
pub(crate) fn words<'a>(line: &'a Line<'a>) -> &'a [Span<'a>] {
    match line.spans.as_slice() {
        // A row of the bubble: the indent, the padding, then what it holds,
        // and the padding and the shadow after it, which are blank.
        [_, _, held @ ..] if !held.is_empty() => held,
        // What became of the message, behind its indent.
        [_, note] => std::slice::from_ref(note),
        other => other,
    }
}

/// How many cells `line` is drawn across.
fn cells(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| text::width(&span.content))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{CLASSIC, CYBER};

    fn plain(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_short_message_hugs_its_words_against_its_shadow_at_the_right_edge() {
        let drawn = plain(&lines("", vec![Line::from("hi there")], "", 40, &CYBER));
        assert_eq!(
            drawn,
            [
                format!("{}{} ", " ".repeat(29), "▄".repeat(10)),
                format!("{} hi there  ", " ".repeat(29)),
                format!("{}{} ", " ".repeat(29), "▀".repeat(10)),
                String::new(),
            ]
        );
    }

    /// The edges are straight, and the shadow falls a column right of the
    /// bubble and half a row below it: beside the bubble's words, under its
    /// bottom but for its first column, and nowhere beside its top.
    #[test]
    fn the_bubble_casts_a_shadow_a_column_right_and_half_a_row_down() {
        let theme = CLASSIC;
        let drawn = lines("", vec![Line::from("go")], "", 20, &theme);
        let cells = |line: &Line<'_>| -> Vec<(char, Style)> {
            line.spans
                .iter()
                .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
                .collect()
        };
        let [top, words, bottom] = [&drawn[0], &drawn[1], &drawn[2]].map(cells);
        let shadow = Style::new().bg(theme.shadow);
        let edge = Style::new().fg(theme.user_bg);
        assert!(top[15..19].iter().all(|cell| *cell == ('▄', edge)));
        assert_eq!(top[19], (' ', Style::new()), "no shadow beside the top");
        assert_eq!(words[19], (' ', shadow));
        assert!(
            words[15..19]
                .iter()
                .all(|(_, style)| style.bg == Some(theme.user_bg))
        );
        assert_eq!(bottom[15], ('▀', edge));
        assert!(
            bottom[16..19]
                .iter()
                .all(|cell| *cell == ('▀', edge.bg(theme.shadow)))
        );
        assert_eq!(bottom[19], (' ', shadow));
    }

    #[test]
    fn a_long_message_takes_four_fifths_of_the_transcript_and_no_more() {
        assert_eq!(room(100), 78);
        assert_eq!(room(30), 22);
        // Narrower than a bubble is worth narrowing, it takes the width
        // but the column its shadow falls in.
        assert_eq!(room(20), 17);
        assert_eq!(room(1), 1);
    }

    #[test]
    fn an_answer_quotes_the_question_at_the_top_of_its_bubble() {
        let drawn = plain(&lines(
            "Which platforms?",
            vec![Line::from("macOS")],
            "",
            40,
            &CYBER,
        ));
        assert_eq!(drawn[1].trim(), "▎ Which platforms?");
        assert_eq!(drawn[2].trim(), "macOS");
        // The bubble is as wide as the widest of its rows, the quote's here.
        assert_eq!(drawn[2].len(), 40);
        assert!(drawn[2].starts_with(&" ".repeat(19)));
    }

    #[test]
    fn what_became_of_a_message_is_said_under_its_bubble_at_the_right() {
        let drawn = plain(&lines("", vec![Line::from("go")], "queued", 20, &CYBER));
        assert_eq!(drawn[3], format!("{}queued", " ".repeat(14)));
    }

    #[test]
    fn a_copy_reads_the_ends_as_drawing_and_each_row_as_what_it_holds() {
        let drawn = lines("Which?", vec![Line::from("go")], "queued", 30, &CYBER);
        let read: Vec<(bool, String)> = drawn
            .iter()
            .map(|line| {
                let words: String = words(line)
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect();
                (is_end(line), words.trim_end().to_owned())
            })
            .collect();
        assert_eq!(
            read,
            [
                // An end's spans past its first edge are drawing too; a
                // copy leaves the whole row out by `is_end`.
                (true, String::new()),
                (false, "▎ Which?".to_owned()),
                (false, "go".to_owned()),
                (true, "▀▀▀▀▀▀▀▀▀".to_owned()),
                (false, "queued".to_owned()),
                (false, String::new()),
            ]
        );
    }

    #[test]
    fn the_words_are_laid_on_the_bubble_and_its_ends_are_drawn_in_its_colour() {
        let drawn = lines("", vec![Line::from("go")], "", 20, &CYBER);
        let ends = [&drawn[0], &drawn[2]];
        for end in ends {
            assert_eq!(end.spans[1].style.fg, Some(CYBER.user_bg));
        }
        let row = &drawn[1].spans;
        assert!(
            row[1..row.len() - 1]
                .iter()
                .all(|span| span.style.bg == Some(CYBER.user_bg))
        );
    }
}
