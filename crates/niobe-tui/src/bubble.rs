// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The operator's own message, drawn as a messenger draws the messages one
//! sends: on the right of the transcript, in a bubble of a colour of its own
//! with rounded ends, its words wrapped inside it, and — where it answers a
//! question the agent put — that question quoted at its top.
//!
//! The bubble is what says whose words they are, so nothing is drawn over
//! it. Its ends are half-cell blocks in the bubble's colour, which round its
//! corners and give it half a row of room above and below its words without
//! taking a whole row for each.

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

/// How wide a bubble may grow in a transcript `width` cells wide: four
/// fifths of it, leaving the other side to the agent as a messenger leaves it
/// to the other party, so a long prompt still reads as a message sent.
fn widest(width: usize) -> usize {
    match width < NARROWEST {
        true => width,
        false => (width * 4 / 5).max(NARROWEST),
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
    let indent = || Span::raw(" ".repeat(width.saturating_sub(outer)));
    let bubble = Style::new().bg(theme.user_bg);
    let edge = Style::new().fg(theme.user_bg);
    let row = |spans: Vec<Span<'static>>, used: usize| {
        let mut row = vec![indent(), Span::styled(" ".repeat(PADDING), bubble)];
        row.extend(spans);
        let rest = inner.saturating_sub(used) + PADDING;
        row.push(Span::styled(" ".repeat(rest), bubble));
        Line::from(row)
    };

    let middle = "▄".repeat(outer.saturating_sub(2));
    let mut lines = vec![Line::from(vec![
        indent(),
        Span::styled(format!("▗{middle}▖"), edge),
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
    let middle = "▀".repeat(outer.saturating_sub(2));
    lines.push(Line::from(vec![
        indent(),
        Span::styled(format!("▝{middle}▘"), edge),
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

/// Whether `line`, one of those [`lines`] draws, is one of the bubble's
/// rounded ends, which are drawing rather than the message's words.
///
/// Read by where [`lines`] puts each span rather than by reading the cells
/// before them: a copy asks it of every row of every prompt in the session,
/// and the cells that set a bubble against the right edge are most of a
/// short prompt's row.
pub(crate) fn is_end(line: &Line<'_>) -> bool {
    match line.spans.as_slice() {
        [_, end] => end.content.starts_with(['▗', '▝']),
        _ => false,
    }
}

/// The spans of `line`, one of those [`lines`] draws, after the cells that
/// set the bubble against the right edge and its padding: its words, or
/// what became of the message, as a copy reads them.
pub(crate) fn words<'a>(line: &'a Line<'a>) -> &'a [Span<'a>] {
    match line.spans.as_slice() {
        // A row of the bubble: the indent, the padding, then what it holds.
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
    use crate::theme::CYBER;

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
    fn a_short_message_hugs_its_words_against_the_right_edge() {
        let drawn = plain(&lines("", vec![Line::from("hi there")], "", 40, &CYBER));
        assert_eq!(
            drawn,
            [
                format!("{}▗{}▖", " ".repeat(30), "▄".repeat(8)),
                format!("{} hi there ", " ".repeat(30)),
                format!("{}▝{}▘", " ".repeat(30), "▀".repeat(8)),
                String::new(),
            ]
        );
    }

    #[test]
    fn a_long_message_takes_four_fifths_of_the_transcript_and_no_more() {
        assert_eq!(room(100), 78);
        assert_eq!(room(30), 22);
        // Narrower than a bubble is worth narrowing, it takes the width.
        assert_eq!(room(20), 18);
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
        assert!(drawn[2].starts_with(&" ".repeat(20)));
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
                (true, "▗▄▄▄▄▄▄▄▄▖".to_owned()),
                (false, "▎ Which?".to_owned()),
                (false, "go".to_owned()),
                (true, "▝▀▀▀▀▀▀▀▀▘".to_owned()),
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
        assert!(
            drawn[1].spans[1..]
                .iter()
                .all(|span| span.style.bg == Some(CYBER.user_bg))
        );
    }
}
