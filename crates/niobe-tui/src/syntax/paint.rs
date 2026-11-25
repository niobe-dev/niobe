// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A highlighted line as the shell draws it: each token in its theme's
//! colour over the style of what holds the line — a reply's code block, a
//! diff's row — and the rest in that style alone.

use ratatui::style::{Color, Style};
use ratatui::text::Span;

use super::{Token, TokenKind};
use crate::theme::Theme;

/// The colour `theme` draws a token of `kind` in.
pub(crate) fn colour(kind: TokenKind, theme: &Theme) -> Color {
    let syntax = &theme.syntax;
    match kind {
        TokenKind::Keyword => syntax.keyword,
        TokenKind::Type => syntax.type_name,
        TokenKind::Function => syntax.function,
        TokenKind::Property => syntax.property,
        TokenKind::String => syntax.string,
        TokenKind::Number => syntax.number,
        TokenKind::Constant => syntax.constant,
        TokenKind::Comment => syntax.comment,
        TokenKind::Attribute => syntax.attribute,
        TokenKind::Inserted => theme.add,
        TokenKind::Deleted => theme.del,
    }
}

/// `piece` — the text `source` holds from byte `from`, as a caller cut it to
/// fit a row — as spans: what `tokens`, read from `source`, cover in their
/// colours over `base`, and the rest in `base`.
///
/// A caller can draw a piece that is not quite what the source holds there:
/// a line cut short and ended with `…`. Only as much of it as the source
/// holds is coloured; the rest is drawn in `base`, so nothing the piece says
/// is lost to a token that does not reach it.
pub(crate) fn spans(
    source: &str,
    tokens: &[Token],
    from: usize,
    piece: &str,
    base: Style,
    theme: &Theme,
) -> Vec<Span<'static>> {
    coloured(source, tokens, from, piece, base, theme)
        .unwrap_or_else(|| vec![Span::styled(piece.to_owned(), base)])
}

fn coloured(
    source: &str,
    tokens: &[Token],
    from: usize,
    piece: &str,
    base: Style,
    theme: &Theme,
) -> Option<Vec<Span<'static>>> {
    let held = shared_start(source.get(from..)?, piece);
    let end = from + held;
    let mut spans = Vec::new();
    let mut at = from;
    let first = tokens.partition_point(|token| token.range.end <= from);
    for token in tokens.get(first..)? {
        if token.range.start >= end {
            break;
        }
        let start = token.range.start.max(at);
        let stop = token.range.end.min(end);
        if start > at {
            spans.push(Span::styled(source.get(at..start)?.to_owned(), base));
        }
        let style = base.fg(colour(token.kind, theme));
        spans.push(Span::styled(source.get(start..stop)?.to_owned(), style));
        at = stop;
    }
    if at < end {
        spans.push(Span::styled(source.get(at..end)?.to_owned(), base));
    }
    if held < piece.len() {
        spans.push(Span::styled(piece.get(held..)?.to_owned(), base));
    }
    Some(spans)
}

/// How many bytes `piece` starts with that `source` starts with too, to a
/// character boundary of both.
fn shared_start(source: &str, piece: &str) -> usize {
    if source.starts_with(piece) {
        return piece.len();
    }
    source
        .char_indices()
        .zip(piece.chars())
        .find(|((_, a), b)| a != b)
        .map_or_else(|| source.len().min(piece.len()), |((at, _), _)| at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{Highlighter, Language};
    use crate::theme::CYBER_TRUE;

    fn drawn(spans: &[Span<'_>]) -> Vec<(String, Option<Color>)> {
        spans
            .iter()
            .map(|span| (span.content.to_string(), span.style.fg))
            .collect()
    }

    #[test]
    fn a_piece_of_a_line_is_coloured_by_the_tokens_it_holds_and_cut_where_they_are() {
        let theme = CYBER_TRUE;
        let base = Style::new().fg(theme.code);
        let line = "let s = \"hi\";";
        let tokens = Highlighter::new(Language::Rust).line(line);
        let spans = spans(line, &tokens, 4, &line[4..10], base, &theme);
        assert_eq!(
            drawn(&spans),
            vec![
                ("s = ".to_owned(), Some(theme.code)),
                ("\"h".to_owned(), Some(theme.syntax.string)),
            ]
        );
    }

    #[test]
    fn a_piece_cut_short_with_a_mark_keeps_the_mark_in_the_base_style() {
        let theme = CYBER_TRUE;
        let base = Style::new().fg(theme.fg);
        let line = "fn main() {}";
        let tokens = Highlighter::new(Language::Rust).line(line);
        let spans = spans(line, &tokens, 0, "fn ma…", base, &theme);
        assert_eq!(
            drawn(&spans),
            vec![
                ("fn".to_owned(), Some(theme.syntax.keyword)),
                (" ".to_owned(), Some(theme.fg)),
                ("ma".to_owned(), Some(theme.syntax.function)),
                ("…".to_owned(), Some(theme.fg)),
            ]
        );
    }

    #[test]
    fn a_piece_the_source_does_not_hold_is_drawn_whole_in_the_base_style() {
        let theme = CYBER_TRUE;
        let base = Style::new().fg(theme.fg);
        let tokens = Highlighter::new(Language::Rust).line("fn x");
        assert_eq!(
            drawn(&spans("fn x", &tokens, 9, "fn", base, &theme)),
            vec![("fn".to_owned(), Some(theme.fg))]
        );
        assert_eq!(
            drawn(&spans("é", &[], 1, "x", base, &theme)),
            vec![("x".to_owned(), Some(theme.fg))],
            "a start inside a character is not sliced"
        );
    }

    #[test]
    fn the_background_of_what_holds_the_line_stays_under_every_token() {
        let theme = CYBER_TRUE;
        let base = Style::new().fg(theme.fg).bg(theme.add_bg);
        let line = "x = 1 // one";
        let tokens = Highlighter::new(Language::Rust).line(line);
        let spans = spans(line, &tokens, 0, line, base, &theme);
        assert!(spans.iter().all(|span| span.style.bg == Some(theme.add_bg)));
        assert_eq!(
            spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            line
        );
    }
}
