// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Wrapping, truncation, padding and tab expansion, done here rather than by
//! the widgets.
//!
//! The transcript scrolls, so the shell has to know how many lines a message
//! occupies before it draws it. A [`ratatui::widgets::Paragraph`] wraps
//! internally and does not say, which makes the scroll offset a guess. Wrapping
//! up front costs one pass over the text and makes the offset exact.

use ratatui::buffer::CellWidth as _;
use unicode_segmentation::UnicodeSegmentation as _;

/// Display width of a string in terminal cells, as ratatui will draw it.
///
/// Measured a grapheme at a time with ratatui's own measure, because a
/// string's width is not the sum of its characters': `❤️` is a heart and a
/// presentation selector, one cell apiece by character and two as the emoji
/// it draws; a family joined by zero-width joiners goes the other way. A
/// width summed by character put text in a line ratatui then clipped, and cut
/// a row that pushed the columns after it out of line.
pub fn width(text: &str) -> usize {
    if text.is_ascii() {
        return text.bytes().filter(|b| !b.is_ascii_control()).count();
    }
    graphemes(text).map(|(_, cells)| cells).sum()
}

/// Each grapheme cluster of `text` and the cells ratatui draws it in. A
/// cluster holding a control character draws in none: ratatui drops it.
pub(crate) fn graphemes(text: &str) -> impl DoubleEndedIterator<Item = (&str, usize)> {
    text.graphemes(true)
        .map(|grapheme| (grapheme, cells(grapheme)))
}

/// The cells one grapheme cluster is drawn in.
fn cells(grapheme: &str) -> usize {
    match grapheme.contains(char::is_control) {
        true => 0,
        false => usize::from(grapheme.cell_width()),
    }
}

/// `text` followed by the spaces that make it `columns` cells wide, or
/// `text` alone where it is already as wide or wider.
///
/// For a column a span after it has to start at. `format!`'s own padding
/// counts characters, so a name in CJK, two cells to the character, came out
/// as wide again as its column and pushed everything after it out of line.
pub fn pad(text: &str, columns: usize) -> String {
    let fill = columns.saturating_sub(width(text));
    let mut out = String::with_capacity(text.len() + fill);
    out.push_str(text);
    out.extend(std::iter::repeat_n(' ', fill));
    out
}

/// Columns between tab stops where the shell expands a tab itself.
const TAB_STOP: usize = 4;

/// `line` with each tab replaced by the spaces that reach the next tab stop.
///
/// A terminal cell cannot hold a tab: ratatui drops it as a control
/// character, so tab-indented code would draw every level of its nesting in
/// one column. Stops rather than a fixed run of spaces, so a tab that lines up
/// a column mid-line still lines it up. Every four columns rather than the
/// terminal's eight, because a pane is narrower than the terminal and a code
/// line is cut, not rewrapped.
pub fn expand_tabs(line: &str) -> String {
    if !line.contains('\t') {
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len() + TAB_STOP);
    let mut column = 0;
    for (grapheme, w) in graphemes(line) {
        match grapheme {
            "\t" => {
                let fill = TAB_STOP - column % TAB_STOP;
                out.extend(std::iter::repeat_n(' ', fill));
                column += fill;
            }
            _ => {
                out.push_str(grapheme);
                column += w;
            }
        }
    }
    out
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

/// Wraps `text` to `columns` cells keeping every character it has: each
/// space, each indent and each blank line, with tabs expanded to their stops.
/// A line breaks only where it reaches the column, mid-word if that is where.
///
/// For what the operator is asked to approve. [`wrap`] joins words with one
/// space, so a quoted `"a    b"` would read as `"a b"` and an indented Python
/// or YAML body as one flush column — a different command from the one that
/// would run. Breaking only at the column also keeps a line break honest:
/// every row but a line's last is full, so a run of spaces never hides at the
/// end of a row cut short.
pub fn wrap_exact(text: &str, columns: usize) -> Vec<String> {
    if columns == 0 || text.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for line in text.split('\n') {
        match line.is_empty() {
            true => lines.push(String::new()),
            false => lines.extend(split_to_width(&expand_tabs(line), columns)),
        }
    }
    lines
}

/// Cuts a string into pieces no wider than `columns` cells.
pub fn split_to_width(text: &str, columns: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    let mut chunk_width = 0;

    for (grapheme, w) in graphemes(text) {
        if chunk_width + w > columns && !chunk.is_empty() {
            chunks.push(std::mem::take(&mut chunk));
            chunk_width = 0;
        }
        chunk.push_str(grapheme);
        chunk_width += w;
    }

    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

/// Shortens `text` to `columns` cells, ending in `…` when anything was cut.
pub fn truncate(text: &str, columns: usize) -> String {
    let line = one_line(text);
    let text: &str = &line;
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
    for (grapheme, w) in graphemes(text) {
        if out_width + w > columns - 1 {
            break;
        }
        out.push_str(grapheme);
        out_width += w;
    }
    out.push('…');
    out
}

/// `text` with each tab a space and each line break a `↵`, for a row drawn
/// on one line.
///
/// A terminal cell cannot hold a tab, and a width that counted one as no
/// cells would cut the row in the wrong place; a single row has no column
/// for a tab stop to line up either, so it is the space it separates words
/// with. A line break is dropped by the terminal the same way, which would
/// join the words either side of it — `set -e` and `cd /tmp` would read as
/// `set -ecd /tmp` — so it is drawn as the mark that says one was there.
fn one_line(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains(['\t', '\n', '\r']) {
        return std::borrow::Cow::Borrowed(text);
    }
    std::borrow::Cow::Owned(
        text.replace("\r\n", "\n")
            .replace(['\n', '\r'], LINE_BREAK)
            .replace('\t', " "),
    )
}

/// What a line break reads as on a row drawn on one line.
const LINE_BREAK: &str = " ↵ ";

/// Shortens `text` to `columns` cells at a word boundary, ending in `…` when
/// anything was cut.
///
/// For a phrase, where half a word reads as a different word. A first word
/// that is itself wider than the room is cut inside it, as [`truncate`] would:
/// a caption of nothing but the ellipsis says less than part of a word.
pub fn truncate_words(text: &str, columns: usize) -> String {
    let line = one_line(text);
    let text: &str = &line;
    if width(text) <= columns {
        return text.to_owned();
    }
    let room = columns.saturating_sub(1);
    let mut kept = "";
    for (at, _) in text.match_indices(char::is_whitespace) {
        let before = text[..at].trim_end();
        if width(before) > room {
            break;
        }
        kept = before;
    }
    match kept.is_empty() {
        true => truncate(text, columns),
        false => format!("{kept}…"),
    }
}

/// Shortens `text` to `columns` cells by cutting the **front**, starting in
/// `…` when anything was cut.
///
/// For a path, which is what this is for: the file's own name and the
/// directory it sits in are what tell two rows apart, and they are at the end.
pub fn truncate_start(text: &str, columns: usize) -> String {
    let line = one_line(text);
    let text: &str = &line;
    if width(text) <= columns {
        return text.to_owned();
    }
    if columns == 0 {
        return String::new();
    }
    if columns == 1 {
        return "…".to_owned();
    }

    let mut kept: Vec<&str> = Vec::new();
    let mut kept_width = 0;
    for (grapheme, w) in graphemes(text).rev() {
        if kept_width + w > columns - 1 {
            break;
        }
        kept.push(grapheme);
        kept_width += w;
    }
    std::iter::once("…").chain(kept.into_iter().rev()).collect()
}

/// The rows the composer draws one line of its text in, `columns` cells wide,
/// with tabs stopping every `tab` cells.
///
/// The composer's height is set from this before the editor draws, and the
/// editor scrolls whatever does not fit, so the count follows the editor's own
/// word-or-glyph rule exactly rather than [`wrap`]'s: a line breaks before the
/// word that would overflow the row, and only a word wider than the whole row
/// is cut, a grapheme at a time. Widths are summed by character, as the editor
/// sums them, since a count that disagrees with it by one row hides that row.
pub fn editor_rows(line: &str, columns: usize, tab: u8) -> usize {
    let columns = columns.max(1);
    let words: Vec<&str> = line.split_word_bounds().collect();
    let mut rows = 0usize;
    let mut row_width = 0usize;
    let mut row_open = false;
    let mut next = 0usize;
    while let Some(word) = words.get(next) {
        let word_width = advance(word, row_width, tab).saturating_sub(row_width);
        if row_width.saturating_add(word_width) <= columns {
            row_width += word_width;
            row_open = true;
            next += 1;
        } else if row_open {
            rows += 1;
            row_width = 0;
            row_open = false;
        } else {
            rows += glyph_rows(word, columns, tab);
            next += 1;
        }
    }
    rows + usize::from(row_open || rows == 0)
}

/// The rows a word wider than the row is cut into, a grapheme at a time, as
/// the editor cuts it: a grapheme wider than the row still takes a row alone.
fn glyph_rows(word: &str, columns: usize, tab: u8) -> usize {
    let graphemes: Vec<&str> = word.graphemes(true).collect();
    let mut rows = 0usize;
    let mut next = 0usize;
    while next < graphemes.len() {
        rows += 1;
        let mut row_width = 0usize;
        let mut taken = 0usize;
        while let Some(grapheme) = graphemes.get(next) {
            let reached = advance(grapheme, row_width, tab);
            if taken > 0 && reached > columns {
                break;
            }
            row_width = reached;
            next += 1;
            taken += 1;
            if row_width > columns {
                break;
            }
        }
    }
    rows
}

/// The column `text` ends at when it starts at column `from`: a tab runs to
/// the next stop, and every other character takes the cells its own width says.
fn advance(text: &str, from: usize, tab: u8) -> usize {
    use unicode_width::UnicodeWidthChar as _;
    text.chars().fold(from, |column, c| match (c, tab) {
        ('\t', 0) => column,
        ('\t', tab) => {
            let tab = usize::from(tab);
            column + (tab - column % tab)
        }
        (c, _) => column + c.width().unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rows a word-or-glyph editor `columns` wide draws `line` in, read
    /// off the editor itself: its cursor at the end of the text sits on the
    /// last row it drew.
    fn drawn_rows(line: &str, columns: u16) -> usize {
        use ratatui::widgets::Widget as _;
        use ratatui_textarea::{CursorMove, TextArea, WrapMode};
        let mut editor = TextArea::new(vec![line.to_owned()]);
        editor.set_wrap_mode(WrapMode::WordOrGlyph);
        let area = ratatui::layout::Rect::new(0, 0, columns, 200);
        (&editor).render(area, &mut ratatui::buffer::Buffer::empty(area));
        editor.move_cursor(CursorMove::Bottom);
        editor.move_cursor(CursorMove::End);
        editor.screen_cursor().row + 1
    }

    #[test]
    fn the_rows_counted_for_the_composer_are_the_rows_its_editor_draws() {
        let lines = [
            String::new(),
            "short".to_owned(),
            "a".repeat(20),
            "a".repeat(21),
            "a".repeat(300),
            "the quick brown fox jumps over the lazy dog ".repeat(7),
            format!("see {} for the rest of it", "https://x.example/".repeat(6)),
            "word  \t two\tthree\t\tfour ".repeat(5),
            "漢字かな交じり文".repeat(9),
            "a漢".repeat(30),
            "🦀 crabs and 🎉 parties, ❤️ and 👨‍👩‍👧 ".repeat(6),
            "     leading and trailing spaces     ".repeat(3),
        ];
        for line in &lines {
            for columns in [1u16, 2, 3, 7, 20, 33, 80] {
                assert_eq!(
                    editor_rows(line, usize::from(columns), 4),
                    drawn_rows(line, columns),
                    "{line:?} at {columns} columns"
                );
            }
        }
    }

    #[test]
    fn a_line_break_on_a_one_line_row_keeps_the_lines_apart() {
        assert_eq!(
            truncate("set -e\ncd /tmp\r\necho done", 80),
            "set -e ↵ cd /tmp ↵ echo done"
        );
        assert_eq!(truncate("a\tb", 80), "a b");
    }

    #[test]
    fn a_wide_string_is_padded_to_the_cells_it_takes_not_its_characters() {
        assert_eq!(pad("漢字", 6), "漢字  ");
        assert_eq!(width(&pad("漢字", 6)), 6);
        assert_eq!(pad("Read", 6), "Read  ");
        assert_eq!(
            pad("漢字漢字", 6),
            "漢字漢字",
            "a string wider than the column is not cut"
        );
    }

    #[test]
    fn an_emoji_with_a_presentation_selector_is_as_wide_as_ratatui_draws_it() {
        use unicode_width::UnicodeWidthStr as _;

        let heart = "\u{2764}\u{fe0f}";
        assert_eq!(width(heart), heart.width());
        assert_eq!(width(heart), 2);
        assert_eq!(width(&heart.repeat(40)), 80);
    }

    #[test]
    fn a_joined_emoji_sequence_is_as_wide_as_ratatui_draws_it() {
        use unicode_width::UnicodeWidthStr as _;

        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        assert_eq!(width(family), family.width());
    }

    #[test]
    fn an_emoji_is_never_split_or_cut_in_half() {
        let heart = "\u{2764}\u{fe0f}";
        let hearts = heart.repeat(3);

        assert_eq!(
            split_to_width(&hearts, 4),
            [heart.repeat(2), heart.to_owned()]
        );
        assert_eq!(truncate(&hearts, 5), format!("{}…", heart.repeat(2)));
        assert_eq!(truncate_start(&hearts, 5), format!("…{}", heart.repeat(2)));
    }

    #[test]
    fn a_tab_in_a_row_cut_to_one_line_is_a_space() {
        assert_eq!(truncate("a\tb", 10), "a b");
        assert_eq!(truncate("a\tbcdef", 4), "a b…");
        assert_eq!(truncate_words("one\ttwo three", 9), "one two…");
        assert_eq!(truncate_start("dir\tname.rs", 20), "dir name.rs");
    }

    #[test]
    fn a_leading_tab_is_one_level_of_indentation_per_tab() {
        assert_eq!(expand_tabs("\t\treturn"), "        return");
    }

    #[test]
    fn a_tab_mid_line_reaches_the_next_stop_so_columns_still_line_up() {
        assert_eq!(expand_tabs("ID\tstring"), "ID  string");
        assert_eq!(expand_tabs("Body\t[]byte"), "Body    []byte");
        assert_eq!(expand_tabs("名\tx"), "名  x");
    }

    #[test]
    fn a_line_without_tabs_is_unchanged() {
        assert_eq!(expand_tabs("if ok {"), "if ok {");
    }

    #[test]
    fn words_break_on_whitespace() {
        assert_eq!(
            wrap("the quick brown fox jumps", 11),
            ["the quick", "brown fox", "jumps"]
        );
    }

    #[test]
    fn a_word_wider_than_the_line_is_split_rather_than_overflowing() {
        let lines = wrap(
            "see crates/niobe-core/tests/fixtures/claude-session.jsonl",
            12,
        );
        assert!(
            lines.iter().all(|l| width(l) <= 12),
            "a line overflowed: {lines:?}"
        );
        assert!(lines.concat().contains("claude-session.jsonl"));
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
        assert!(wrap_exact("anything", 0).is_empty());
    }

    #[test]
    fn an_exact_wrap_keeps_every_space_indent_and_blank_line() {
        assert_eq!(
            wrap_exact("if x:\n    print(\"a    b\")\n\n  # done  ", 40),
            ["if x:", "    print(\"a    b\")", "", "  # done  "]
        );
    }

    #[test]
    fn an_exact_wrap_expands_a_tab_to_its_stop() {
        assert_eq!(wrap_exact("\tpass\nID\tx", 40), ["    pass", "ID  x"]);
    }

    #[test]
    fn an_exact_wrap_breaks_only_at_the_column() {
        assert_eq!(wrap_exact("echo a    b  c", 6), ["echo a", "    b ", " c"]);
        assert_eq!(wrap_exact("日本語", 4), ["日本", "語"]);
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
    fn a_phrase_too_long_for_its_room_is_cut_between_words() {
        let caption = "Cost floors and replay pricing";
        assert_eq!(truncate_words(caption, 40), caption);
        assert_eq!(truncate_words(caption, 30), caption);
        assert_eq!(truncate_words(caption, 29), "Cost floors and replay…");
        assert_eq!(truncate_words(caption, 16), "Cost floors and…");
        assert_eq!(truncate_words(caption, 15), "Cost floors…");
        assert_eq!(truncate_words("Cost  floors", 11), "Cost…");
    }

    #[test]
    fn a_first_word_wider_than_the_room_is_cut_inside_it() {
        assert_eq!(truncate_words("interstellar-objects search", 8), "interst…");
        assert_eq!(truncate_words("anything at all", 1), "…");
        assert_eq!(truncate_words("anything at all", 0), "");
    }

    #[test]
    fn a_phrase_of_wide_characters_is_cut_by_cells() {
        let cut = truncate_words("日本語 テスト です", 14);
        assert_eq!(cut, "日本語 テスト…");
        assert_eq!(width(&cut), 14);
        assert_eq!(truncate_words("日本語 テスト です", 13), "日本語…");
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
