// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Wrapping and truncation, done here rather than by the widgets.
//!
//! The transcript scrolls, so the shell has to know how many lines a message
//! occupies before it draws it. A [`ratatui::widgets::Paragraph`] wraps
//! internally and does not say, which makes the scroll offset a guess. Wrapping
//! up front costs one pass over the text and makes the offset exact.

use unicode_width::UnicodeWidthChar;

/// Display width of a string in terminal cells.
pub fn width(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Wraps `text` to `columns` cells, breaking on whitespace where it can and
/// mid-word where a single word is wider than the line.
///
/// Explicit newlines are kept. An empty line stays an empty line, so a message
/// with a blank line between paragraphs still reads as one.
pub fn wrap(text: &str, columns: usize) -> Vec<String> {
    if columns == 0 || text.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let before = lines.len();
        wrap_paragraph(paragraph, columns, &mut lines);
        if lines.len() == before {
            lines.push(String::new());
        }
    }
    lines
}

fn wrap_paragraph(paragraph: &str, columns: usize, lines: &mut Vec<String>) {
    let mut line = String::new();
    let mut line_width = 0;

    for word in paragraph.split_whitespace() {
        let word_width = width(word);

        if line_width != 0 && line_width + 1 + word_width > columns {
            lines.push(std::mem::take(&mut line));
            line_width = 0;
        }

        if word_width > columns {
            // A single word wider than the line: spill it across as many lines
            // as it needs rather than letting it run off the pane.
            if line_width != 0 {
                lines.push(std::mem::take(&mut line));
                line_width = 0;
            }
            for chunk in split_to_width(word, columns) {
                if line_width != 0 {
                    lines.push(std::mem::take(&mut line));
                }
                line_width = width(&chunk);
                line = chunk;
            }
            continue;
        }

        if line_width != 0 {
            line.push(' ');
            line_width += 1;
        }
        line.push_str(word);
        line_width += word_width;
    }

    if line_width != 0 {
        lines.push(line);
    }
}

/// Cuts a string into pieces no wider than `columns` cells.
fn split_to_width(text: &str, columns: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    let mut chunk_width = 0;

    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if chunk_width + w > columns && !chunk.is_empty() {
            chunks.push(std::mem::take(&mut chunk));
            chunk_width = 0;
        }
        chunk.push(c);
        chunk_width += w;
    }

    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

/// Shortens `text` to `columns` cells, ending in `…` when anything was cut.
pub fn truncate(text: &str, columns: usize) -> String {
    if width(text) <= columns {
        return text.to_owned();
    }
    if columns == 0 {
        return String::new();
    }
    if columns == 1 {
        return "…".to_owned();
    }

    let mut out = String::new();
    let mut out_width = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if out_width + w > columns - 1 {
            break;
        }
        out.push(c);
        out_width += w;
    }
    out.push('…');
    out
}

/// Shortens `text` to `columns` cells by cutting the **front**, starting in
/// `…` when anything was cut.
///
/// For a path, which is what this is for: the file's own name and the
/// directory it sits in are what tell two rows apart, and they are at the end.
pub fn truncate_start(text: &str, columns: usize) -> String {
    if width(text) <= columns {
        return text.to_owned();
    }
    if columns == 0 {
        return String::new();
    }
    if columns == 1 {
        return "…".to_owned();
    }

    let mut kept: Vec<char> = Vec::new();
    let mut kept_width = 0;
    for c in text.chars().rev() {
        let w = c.width().unwrap_or(0);
        if kept_width + w > columns - 1 {
            break;
        }
        kept.push(c);
        kept_width += w;
    }
    std::iter::once('…').chain(kept.into_iter().rev()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_break_on_whitespace() {
        assert_eq!(
            wrap("the quick brown fox jumps", 11),
            ["the quick", "brown fox", "jumps"]
        );
    }

    #[test]
    fn a_word_wider_than_the_line_is_split_rather_than_overflowing() {
        let lines = wrap("see crates/niobe-core/tests/fixtures/session-200.jsonl", 12);
        assert!(
            lines.iter().all(|l| width(l) <= 12),
            "a line overflowed: {lines:?}"
        );
        assert!(lines.concat().contains("session-200.jsonl"));
    }

    #[test]
    fn explicit_newlines_and_blank_lines_survive() {
        assert_eq!(wrap("one\n\ntwo", 20), ["one", "", "two"]);
    }

    #[test]
    fn wide_characters_count_as_two_cells() {
        assert_eq!(width("日本語"), 6);
        assert!(wrap("日本語テスト", 6).iter().all(|l| width(l) <= 6));
    }

    #[test]
    fn a_zero_width_pane_produces_no_lines() {
        assert!(wrap("anything", 0).is_empty());
    }

    #[test]
    fn empty_text_occupies_no_lines_at_all() {
        // A tool call with no body must not push a blank line into the
        // transcript under its head line.
        assert!(wrap("", 40).is_empty());
    }

    #[test]
    fn truncation_marks_what_it_cut() {
        assert_eq!(truncate("catalog/fetch.ts", 20), "catalog/fetch.ts");
        assert_eq!(truncate("catalog/fetch.ts", 8), "catalog…");
        assert_eq!(width(&truncate("catalog/fetch.ts", 8)), 8);
        assert_eq!(truncate("catalog/fetch.ts", 1), "…");
    }

    #[test]
    fn a_path_too_long_for_its_row_keeps_its_end() {
        assert_eq!(
            truncate_start("crates/niobe-tui/src/ui.rs", 12),
            "…i/src/ui.rs"
        );
        assert_eq!(truncate_start("ui.rs", 12), "ui.rs");
        assert_eq!(truncate_start("ui.rs", 1), "…");
        assert_eq!(truncate_start("ui.rs", 0), "");
    }
}
