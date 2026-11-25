// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A file change drawn as the lines that changed, under the call that made it.
//!
//! Each row is a line number, a one-cell sign — `-`, `+`, or blank for an
//! unchanged line — and the line itself, on the diff's own background. The
//! two sides are numbered independently, as a unified diff numbers them: a
//! removed line carries its number in the file before the change, an added or
//! unchanged line its number after. The sign is what tells the rows apart,
//! so the diff still reads on a terminal with no colour.
//!
//! Where the file is in a language the highlighter reads, its lines are
//! highlighted: an unchanged line always, and a changed one where the theme
//! washes it in the added or removed colour, since that wash, with the sign,
//! is then what says it changed. Where there is no wash — every
//! sixteen-colour table — a changed line keeps its colour, which is what
//! tells it from its neighbours. The file before the change and the file
//! after are read apart, so a comment a removed line opens runs on through
//! the lines removed with it and not through those added.

use niobe_core::diff::{Hunk, Line as DiffLine};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::app::{Change, Gate};
use crate::syntax::{Highlighter, Language, Token, paint};
use crate::text;
use crate::theme::Theme;

/// How many unchanged lines a diff keeps on each side of a change.
///
/// Two, where `git` and the backends keep three: three lines are what a patch
/// needs to find its place in a file that has moved, and a reader needs only
/// enough to see where the change sits. Every line kept is a row of a pane
/// that also has to show the rest of the session. Runs of unchanged lines
/// longer than this, between two changes, are drawn as one `…` row.
pub const CONTEXT_LINES: usize = 2;

/// The most rows of lines one diff draws before it stops and says how many it
/// left out.
///
/// A file written from scratch is a diff of every line in it, and a transcript
/// that drew all of them would bury the session under one call. Twenty rows
/// hold every edit of ordinary size in full — a change and its context on
/// each side — and cap the one that is not at less than a screen.
pub const MAX_ROWS: usize = 20;

/// The key that opens every diff cut at [`MAX_ROWS`] and cuts them again,
/// as the marker under a cut diff names it. [`crate::app::App`] binds it.
pub const OPEN_KEY: &str = "Ctrl+T";

/// Whether a change of `hunks` is drawn cut at [`MAX_ROWS`] unless the
/// operator has opened the diffs.
pub(crate) fn is_cut(hunks: &[Hunk]) -> bool {
    cuts(rows(hunks).len())
}

/// Whether a diff of `rows` rows is cut: where it has more than one row over
/// [`MAX_ROWS`]. One row over is drawn rather than said, since saying so would
/// take that row and hide it besides.
fn cuts(rows: usize) -> bool {
    rows > MAX_ROWS + 1
}

/// One row of a diff before it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row<'a> {
    /// A line, with the number the diff shows beside it and where it is
    /// among the change's lines, counted across its hunks.
    Line(u64, &'a DiffLine, usize),
    /// Unchanged lines left out between two that are shown.
    Elided,
}

/// The rows `hunks` draw as: each line numbered, the unchanged lines more than
/// [`CONTEXT_LINES`] from a change dropped, and one [`Row::Elided`] wherever
/// lines were dropped between two that are kept — including between hunks,
/// which the backend already separated for that reason.
fn rows(hunks: &[Hunk]) -> Vec<Row<'_>> {
    let mut rows = Vec::new();
    let mut position = 0;
    for hunk in hunks {
        let lines = hunk.lines();
        let near = near_a_change(lines);
        let (mut old, mut new) = (hunk.old_start(), hunk.new_start());
        let mut dropped = !rows.is_empty();
        for (line, keep) in lines.iter().zip(near) {
            let number = match line {
                DiffLine::Context(_) => new,
                DiffLine::Removed(_) => old,
                DiffLine::Added(_) => new,
            };
            match line {
                DiffLine::Context(_) => {
                    old = old.saturating_add(1);
                    new = new.saturating_add(1);
                }
                DiffLine::Removed(_) => old = old.saturating_add(1),
                DiffLine::Added(_) => new = new.saturating_add(1),
            }
            position += 1;
            if !keep {
                dropped = true;
                continue;
            }
            if dropped && !rows.is_empty() {
                rows.push(Row::Elided);
            }
            dropped = false;
            rows.push(Row::Line(number, line, position - 1));
        }
    }
    rows
}

/// For each line, whether it is a change or within [`CONTEXT_LINES`] of one.
///
/// One pass each way, carrying how far back the last change was, so a file
/// written from scratch — a change on every one of its lines — costs its
/// length rather than its length squared.
fn near_a_change(lines: &[DiffLine]) -> Vec<bool> {
    let mut near = vec![false; lines.len()];
    let mut since = None;
    for (line, near) in lines.iter().zip(near.iter_mut()) {
        since = lines_since_a_change(line, since);
        *near = since.is_some_and(|since| since <= CONTEXT_LINES);
    }
    let mut until = None;
    for (line, near) in lines.iter().zip(near.iter_mut()).rev() {
        until = lines_since_a_change(line, until);
        *near |= until.is_some_and(|until| until <= CONTEXT_LINES);
    }
    near
}

/// How many lines `line` is from the nearest change on the side already
/// walked, given how far the line before it was; `None` while no change has
/// been walked past.
fn lines_since_a_change(line: &DiffLine, before: Option<usize>) -> Option<usize> {
    match line {
        DiffLine::Context(_) => before.map(|before| before.saturating_add(1)),
        DiffLine::Removed(_) | DiffLine::Added(_) => Some(0),
    }
}

/// A change's rows, fitted to `width` cells, and the row under them that says
/// how the call was let through.
///
/// A change more than a row longer than [`MAX_ROWS`] stops there and says
/// how many rows it left out, unless `open`, when it draws every row and says how to cut it
/// again.
pub(crate) fn lines(
    change: &Change,
    width: usize,
    open: bool,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let rows = rows(change.hunks());
    let number_width = rows
        .iter()
        .filter_map(|row| match row {
            Row::Line(number, _, _) => Some(*number),
            Row::Elided => None,
        })
        .max()
        .map_or(1, digits);

    let cut = cuts(rows.len());
    let shown = match open || !cut {
        true => rows.len(),
        false => MAX_ROWS,
    };
    let mut reader = Reader::new(change);
    let mut lines: Vec<Line<'static>> = rows
        .iter()
        .take(shown)
        .map(|row| draw_row(*row, number_width, width, theme, &mut reader))
        .collect();
    let beyond = rows.len().saturating_sub(MAX_ROWS);
    if cut {
        lines.push(marker(beyond, open, width, theme));
    }
    if let Some(gate) = change.gate() {
        lines.push(footer(gate, width, theme));
    }
    lines
}

/// A change a permission question proposes, `width` cells wide: each line
/// signed as [`lines`] signs it, unchanged lines more than [`CONTEXT_LINES`]
/// from a change left out, and cut at [`MAX_ROWS`] rows unless `open`.
///
/// Unnumbered, since a proposed edit says what text it replaces and not
/// where in the file that text is. Highlighted in `language`, as [`lines`]
/// highlights a change made. And wrapped rather than cut at the edge,
/// unlike a change already made: what is still to be approved is read whole.
///
/// With the rows, how many lines of the change the marker stands for and are
/// not drawn: none where the change is whole or open.
///
/// `counted` is where the rows the change wraps to in all are kept for the
/// width they were counted at, by whoever holds `lines`: see [`RowCount`].
pub(crate) fn proposed(
    lines: &[DiffLine],
    language: Option<Language>,
    width: usize,
    open: bool,
    theme: &Theme,
    counted: &RowCount,
) -> (Vec<Line<'static>>, usize) {
    let mut rows = Vec::new();
    let mut dropped = false;
    // The rows laid out before the rest is only counted: a file of a hundred
    // thousand lines is not laid out to draw twenty rows of it, nor to be
    // scrolled through under Ctrl+T, which draws what a screenful of scrolling
    // can reach.
    let ceiling = match open {
        true => OPEN_ROWS,
        false => MAX_ROWS,
    };
    let known = counted.at(width);
    let mut uncounted = 0;
    let mut sides = language.map(Sides::new);
    for (line, keep) in lines.iter().zip(near_a_change(lines)) {
        if !keep {
            dropped = true;
            // Read only while rows are still laid out, so a comment it opens
            // colours them; past the ceiling nothing more is drawn.
            if let Some(sides) = sides.as_mut().filter(|_| rows.len() <= ceiling) {
                sides.read(line, line.text());
            }
            continue;
        }
        if rows.len() > ceiling && known.is_some() {
            break;
        }
        if rows.len() > ceiling {
            // Counted as it would be laid out: the row that marks the lines
            // left out before it, and every row it wraps to, which is what
            // the marker says Ctrl+T adds.
            uncounted += usize::from(dropped) + rows_of(line, width);
            dropped = false;
            continue;
        }
        if dropped && !rows.is_empty() {
            rows.push(elided(0, width, theme));
        }
        dropped = false;
        // One line can wrap to more rows than the ceiling on its own — a
        // minified bundle is one line of megabytes — so it is laid out only
        // as far as the ceiling and the rest of its rows are counted.
        let left = (ceiling + 1).saturating_sub(rows.len());
        let (laid, all) = wrapped_row(line, width, left, theme, sides.as_mut());
        uncounted += all.saturating_sub(laid.len());
        rows.extend(laid);
    }
    let total = known.unwrap_or_else(|| {
        let total = rows.len() + uncounted;
        counted.keep(width, total);
        total
    });
    if open && total > OPEN_ROWS + 1 {
        let beyond = total - OPEN_ROWS;
        rows.truncate(OPEN_ROWS);
        rows.push(capped_marker(beyond, width, theme));
        return (rows, beyond);
    }
    if !cuts(total) {
        return (rows, 0);
    }
    let beyond = total - MAX_ROWS;
    if !open {
        rows.truncate(MAX_ROWS);
    }
    rows.push(marker(beyond, open, width, theme));
    // What the marker stands for is not drawn only while the change is cut.
    (rows, if open { 0 } else { beyond })
}

/// The rows a proposed change wraps to in all, laid out whole, and the width
/// they were counted at.
///
/// Counting them reads every line of the change, which can be millions, and
/// the question is laid out again whenever Ctrl+T, the theme or the width
/// changes. The count is the same cut or open — a row past the cut is
/// counted as the layout would draw it — so it is kept for the width alone,
/// and holds only for the lines it was counted from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RowCount(std::cell::Cell<Option<(usize, usize)>>);

impl RowCount {
    /// The rows counted at `width`, where they were last counted at it.
    fn at(&self, width: usize) -> Option<usize> {
        self.0
            .get()
            .and_then(|(counted_at, rows)| (counted_at == width).then_some(rows))
    }

    /// Keeps `rows` as the count at `width`, in place of any other.
    fn keep(&self, width: usize, rows: usize) {
        self.0.set(Some((width, rows)));
    }
}

/// How many rows of a proposed change are laid out with the diffs open: more
/// than a pane scrolls through in a reading, and a bound on what a frame
/// costs however large the change is.
pub(crate) const OPEN_ROWS: usize = 2_000;

/// The marker under a proposed change too long to lay out whole even open: how
/// many rows it leaves out.
fn capped_marker(beyond: usize, width: usize, theme: &Theme) -> Line<'static> {
    let rows = plural(beyond, "row", "rows");
    let said = format!(
        "▴ the first {OPEN_ROWS} rows are shown · {beyond} more {rows} are not drawn · \
         {OPEN_KEY} hides them"
    );
    let said = text::truncate(&said, width);
    Line::from(Span::styled(
        text::pad(&said, width),
        Style::new().fg(theme.dim).bg(theme.diff_bg),
    ))
}

/// The cells a proposed line's text has on a row `width` wide, after its
/// sign and the space after it.
fn row_room(width: usize) -> usize {
    width.saturating_sub(2).max(1)
}

/// How many rows a proposed line wraps to on a row `width` wide. An empty
/// line is still a row of the change.
fn rows_of(line: &DiffLine, width: usize) -> usize {
    text::wrap_words_exact_rows(line.text(), row_room(width)).max(1)
}

/// One line of a proposed change as the first `rows` of the rows it wraps
/// to — its sign on the first, and the rest under its text — and how many
/// rows it wraps to in all, counted without laying out the ones not drawn.
/// Highlighted where `sides` reads the change's language and the line is one
/// [`highlights`] colours.
fn wrapped_row(
    line: &DiffLine,
    width: usize,
    rows: usize,
    theme: &Theme,
    sides: Option<&mut Sides>,
) -> (Vec<Line<'static>>, usize) {
    let (sign, sign_colour, text_colour, bg) = look(line, theme);
    let base = Style::new().fg(text_colour).bg(bg);
    let room = row_room(width);
    let all = rows_of(line, width);
    let mut parts = text::wrap_words_exact_first(line.text(), room, rows);
    if parts.is_empty() && rows > 0 {
        parts.push(String::new());
    }
    let (source, tokens) = match sides {
        // A line past the highlighter's reach is drawn plain: it reads only
        // the start of it, and laying the whole of it out again to colour
        // that start would cost what the cut is there to save.
        Some(sides) if line.text().len() <= crate::syntax::MAX_LINE_BYTES => {
            let source = text::laid_out(line.text());
            let tokens = sides.read(line, &source);
            (source, tokens)
        }
        Some(sides) => {
            sides.read(line, line.text());
            (std::borrow::Cow::Borrowed(""), Vec::new())
        }
        None => (std::borrow::Cow::Borrowed(""), Vec::new()),
    };
    let tokens = match highlights(line, theme) {
        true => tokens,
        false => Vec::new(),
    };
    let mut from = 0;
    let laid = parts
        .into_iter()
        .enumerate()
        .map(|(at, part)| {
            let sign = match at {
                0 => sign,
                _ => " ",
            };
            let mut spans = vec![
                Span::styled(sign, Style::new().fg(sign_colour).bg(bg).bold()),
                Span::styled(" ", Style::new().bg(bg)),
            ];
            spans.extend(coloured(&source, &tokens, from, &part, room, base, theme));
            from += part.len();
            Line::from(spans).style(Style::new().bg(bg))
        })
        .collect();
    (laid, all)
}

/// `piece` of a line — what `source` holds from byte `from` — as spans in
/// `base` with each of `tokens` in its colour, padded to `room` cells so
/// the row's background reaches the edge.
fn coloured(
    source: &str,
    tokens: &[Token],
    from: usize,
    piece: &str,
    room: usize,
    base: Style,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let used = text::width(piece);
    let mut spans = match tokens.is_empty() {
        true => vec![Span::styled(piece.to_owned(), base)],
        false => paint::spans(source, tokens, from, piece, base, theme),
    };
    if used < room {
        spans.push(Span::styled(" ".repeat(room - used), base));
    }
    spans
}

/// Whether `line` is drawn highlighted where its file's language is read:
/// an unchanged line always, and a changed one only where the theme washes
/// it, since otherwise its colour is what says it changed.
fn highlights(line: &DiffLine, theme: &Theme) -> bool {
    match line {
        DiffLine::Context(_) => true,
        DiffLine::Removed(_) => theme.del_bg != theme.diff_bg,
        DiffLine::Added(_) => theme.add_bg != theme.diff_bg,
    }
}

/// The two files a diff is read from, each by a highlighter of its own:
/// the one before the change, whose lines are the unchanged and the removed
/// ones, and the one after, whose lines are the unchanged and the added.
struct Sides {
    before: Highlighter,
    after: Highlighter,
}

impl Sides {
    fn new(language: Language) -> Self {
        Self {
            before: Highlighter::new(language),
            after: Highlighter::new(language),
        }
    }

    /// The tokens of `line`, drawn as `text`, read in the file it is a line
    /// of — and an unchanged line in both, so each goes on from it.
    fn read(&mut self, line: &DiffLine, text: &str) -> Vec<Token> {
        match line {
            DiffLine::Context(_) => {
                self.before.line(text);
                self.after.line(text)
            }
            DiffLine::Removed(_) => self.before.line(text),
            DiffLine::Added(_) => self.after.line(text),
        }
    }
}

/// Every line of a change's hunks, in order.
type Lines<'a> =
    std::iter::FlatMap<std::slice::Iter<'a, Hunk>, &'a [DiffLine], fn(&'a Hunk) -> &'a [DiffLine]>;

/// A change's lines read in order as its rows are drawn: the lines between
/// two rows — the unchanged ones left out — read too, so what one of them
/// opens still colours the rows after it.
struct Reader<'a> {
    sides: Option<Sides>,
    lines: Lines<'a>,
    /// How many of the change's lines have been read.
    read: usize,
}

impl<'a> Reader<'a> {
    fn new(change: &'a Change) -> Self {
        let lines: fn(&'a Hunk) -> &'a [DiffLine] = Hunk::lines;
        Self {
            sides: change.language().map(Sides::new),
            lines: change.hunks().iter().flat_map(lines),
            read: 0,
        }
    }

    /// The tokens of the line at `position` among the change's lines, drawn
    /// as `text`, having read every line before it. None where the change is
    /// in no language the highlighter reads.
    fn tokens(&mut self, position: usize, text: &str) -> Vec<Token> {
        let Some(sides) = self.sides.as_mut() else {
            return Vec::new();
        };
        while self.read < position {
            let Some(line) = self.lines.next() else {
                return Vec::new();
            };
            sides.read(line, line.text());
            self.read += 1;
        }
        let Some(line) = self.lines.next() else {
            return Vec::new();
        };
        self.read += 1;
        sides.read(line, text)
    }
}

/// A line's sign, the colour of its sign or number, of its text, and of what
/// it is drawn on.
fn look(line: &DiffLine, theme: &Theme) -> (&'static str, Color, Color, Color) {
    match line {
        DiffLine::Context(_) => (" ", theme.dim, theme.fg, theme.diff_bg),
        DiffLine::Removed(_) => ("-", theme.del, theme.del, theme.del_bg),
        DiffLine::Added(_) => ("+", theme.add, theme.add, theme.add_bg),
    }
}

/// The row standing for unchanged lines left out, under a gutter of
/// `number_width` digits.
fn elided(number_width: usize, width: usize, theme: &Theme) -> Line<'static> {
    let gutter = text::pad("", number_width + 1);
    Line::from(vec![
        Span::styled(gutter, Style::new().bg(theme.diff_bg)),
        Span::styled(
            text::pad("…", width.saturating_sub(number_width + 1)),
            Style::new().fg(theme.dim).bg(theme.diff_bg),
        ),
    ])
}

/// How many digits `number` is written in.
fn digits(number: u64) -> usize {
    number.checked_ilog10().map_or(1, |log| log as usize + 1)
}

/// One row: the number right-aligned in its column, the sign, and the line
/// cut to what is left, padded so the row's background reaches the edge, and
/// highlighted as [`highlights`] says, by `reader`.
///
/// A line too long for the pane is cut rather than wrapped, so a line of the
/// file is always one row and one number.
fn draw_row(
    row: Row<'_>,
    number_width: usize,
    width: usize,
    theme: &Theme,
    reader: &mut Reader<'_>,
) -> Line<'static> {
    let Row::Line(number, line, position) = row else {
        return elided(number_width, width, theme);
    };
    let (sign, number_colour, text_colour, bg) = look(line, theme);
    let room = width.saturating_sub(number_width + 3);
    let source = text::expand_tabs(line.text());
    let shown = text::truncate(&source, room);
    let tokens = reader.tokens(position, &source);
    let tokens = match highlights(line, theme) {
        true => tokens,
        false => Vec::new(),
    };
    let base = Style::new().fg(text_colour).bg(bg);
    let mut spans = vec![
        Span::styled(
            format!("{number:>number_width$} "),
            Style::new().fg(number_colour).bg(bg),
        ),
        Span::styled(sign, Style::new().fg(number_colour).bg(bg).bold()),
        Span::styled(" ", Style::new().bg(bg)),
    ];
    spans.extend(coloured(&source, &tokens, 0, &shown, room, base, theme));
    Line::from(spans).style(Style::new().bg(bg))
}

/// The row under a diff that is cut (see [`is_cut`]): how many rows are beyond
/// the cut, and the key that shows or hides them.
///
/// The key gives way whole where the pane is too narrow for it, so what is
/// left still reads as a count rather than as half an instruction.
fn marker(beyond: usize, open: bool, width: usize, theme: &Theme) -> Line<'static> {
    let rows = plural(beyond, "row", "rows");
    let (said, key) = match open {
        false => (
            format!("… {beyond} more {rows}"),
            format!(" · {OPEN_KEY} shows them"),
        ),
        true => (
            format!("▴ the last {beyond} {rows} are shown"),
            format!(" · {OPEN_KEY} hides them"),
        ),
    };
    let whole = format!("{said}{key}");
    let said = match text::width(&whole) <= width {
        true => whole,
        false => text::truncate(&said, width),
    };
    Line::from(Span::styled(
        text::pad(&said, width),
        Style::new().fg(theme.dim).bg(theme.diff_bg),
    ))
}

/// `└ allowed by you`: who let the call through, in the words of what
/// actually happened.
fn footer(gate: Gate, width: usize, theme: &Theme) -> Line<'static> {
    let said = match gate {
        Gate::Operator => "allowed by you".to_owned(),
        Gate::OperatorAlways => "allowed by you, with a standing rule saved".to_owned(),
        Gate::Rule => "allowed by a standing rule".to_owned(),
        Gate::Unasked(mode) => format!("not asked · {mode} mode"),
    };
    Line::from(vec![
        Span::styled("└ ", Style::new().fg(theme.dim)),
        Span::styled(
            text::truncate(&said, width.saturating_sub(2)),
            Style::new().fg(theme.dim),
        ),
    ])
}

fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    match count {
        1 => one,
        _ => many,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::Language;
    use crate::theme::CLASSIC;
    use niobe_core::event::Mode;
    use ratatui::style::Color;

    /// The colour a row of the diff is drawn on.
    fn background(line: &Line<'_>) -> Option<Color> {
        line.spans.first().and_then(|span| span.style.bg)
    }

    fn context(text: &str) -> DiffLine {
        DiffLine::Context(text.to_owned())
    }
    fn removed(text: &str) -> DiffLine {
        DiffLine::Removed(text.to_owned())
    }
    fn added(text: &str) -> DiffLine {
        DiffLine::Added(text.to_owned())
    }

    fn hunk(old: u64, new: u64, lines: Vec<DiffLine>) -> Hunk {
        let olds = lines
            .iter()
            .filter(|l| !matches!(l, DiffLine::Added(_)))
            .count() as u64;
        let news = lines
            .iter()
            .filter(|l| !matches!(l, DiffLine::Removed(_)))
            .count() as u64;
        Hunk::checked(old, olds, new, news, lines).expect("built to agree with itself")
    }

    fn drawn(change: &Change, width: usize) -> Vec<String> {
        drawn_as(change, width, false)
    }

    fn drawn_as(change: &Change, width: usize, open: bool) -> Vec<String> {
        lines(change, width, open, &CLASSIC)
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn change(hunks: Vec<Hunk>, gate: Option<Gate>) -> Change {
        Change::new(hunks, gate, None)
    }

    /// Each word of the text `rows` draw after their first `lead` spans —
    /// a number, a sign and a blank, or a sign and a blank — with the
    /// colour and background it is drawn in.
    fn words(rows: &[Line<'_>], lead: usize) -> Vec<(String, Option<Color>, Option<Color>)> {
        rows.iter()
            .flat_map(|row| row.spans.iter().skip(lead))
            .filter(|span| !span.content.trim().is_empty())
            .map(|span| (span.content.trim().to_owned(), span.style.fg, span.style.bg))
            .collect()
    }

    fn rust_change(lines: Vec<DiffLine>) -> Change {
        Change::new(vec![hunk(1, 1, lines)], None, Some(Language::Rust))
    }

    #[test]
    fn an_unchanged_line_in_a_language_the_highlighter_reads_is_highlighted_on_the_diffs_background()
     {
        let theme = CLASSIC;
        let diff = rust_change(vec![context("let n = 1;"), added("x")]);
        let rows = lines(&diff, 40, false, &theme);
        assert_eq!(
            words(&rows[..1], 3),
            [
                (
                    "let".to_owned(),
                    Some(theme.syntax.keyword),
                    Some(theme.diff_bg)
                ),
                ("n =".to_owned(), Some(theme.fg), Some(theme.diff_bg)),
                (
                    "1".to_owned(),
                    Some(theme.syntax.number),
                    Some(theme.diff_bg)
                ),
                (";".to_owned(), Some(theme.fg), Some(theme.diff_bg)),
            ]
        );
        assert_eq!(
            drawn_as(&diff, 40, false)[0],
            format!("{:<40}", "1   let n = 1;"),
            "the row reads as it did before it was coloured"
        );
    }

    #[test]
    fn a_changed_line_is_highlighted_where_its_wash_marks_it_and_keeps_its_colour_where_none_does()
    {
        let deep = crate::theme::STELLAR_TRUE;
        let diff = rust_change(vec![removed("let a = 1;"), added("let b = 2;")]);
        let rows = lines(&diff, 40, false, &deep);
        assert_eq!(
            words(&rows, 3)
                .into_iter()
                .filter(|(word, _, _)| word == "let")
                .collect::<Vec<_>>(),
            [
                (
                    "let".to_owned(),
                    Some(deep.syntax.keyword),
                    Some(deep.del_bg)
                ),
                (
                    "let".to_owned(),
                    Some(deep.syntax.keyword),
                    Some(deep.add_bg)
                ),
            ]
        );
        let sixteen = CLASSIC;
        let rows = lines(&diff, 40, false, &sixteen);
        assert_eq!(
            words(&rows, 3),
            [
                (
                    "let a = 1;".to_owned(),
                    Some(sixteen.del),
                    Some(sixteen.del_bg)
                ),
                (
                    "let b = 2;".to_owned(),
                    Some(sixteen.add),
                    Some(sixteen.add_bg)
                ),
            ],
            "with no wash, the line's colour is what says it changed"
        );
    }

    #[test]
    fn a_comment_a_removed_line_opens_runs_on_in_the_old_file_and_not_in_the_new() {
        let theme = crate::theme::STELLAR_TRUE;
        let diff = rust_change(vec![
            removed("/* gone"),
            added("let kept = 1;"),
            removed("still gone */"),
            context("fn f() {}"),
        ]);
        let rows = lines(&diff, 40, false, &theme);
        let colour_of = |word: &str| {
            words(&rows, 3)
                .into_iter()
                .find(|(drawn, _, _)| drawn == word)
                .and_then(|(_, fg, _)| fg)
        };
        assert_eq!(colour_of("still gone */"), Some(theme.syntax.comment));
        assert_eq!(colour_of("let"), Some(theme.syntax.keyword));
        assert_eq!(colour_of("fn"), Some(theme.syntax.keyword));
    }

    #[test]
    fn a_change_in_no_language_the_highlighter_reads_is_drawn_as_before() {
        let theme = CLASSIC;
        let diff = change(
            vec![hunk(1, 1, vec![context("let n = 1;"), added("x")])],
            None,
        );
        assert_eq!(
            words(&lines(&diff, 40, false, &theme)[..1], 3),
            [("let n = 1;".to_owned(), Some(theme.fg), Some(theme.diff_bg))]
        );
    }

    #[test]
    fn a_proposed_line_wrapped_to_two_rows_is_highlighted_on_both() {
        let theme = crate::theme::STELLAR_TRUE;
        let line = [added("let s = \"one two three four five\";")];
        let (rows, _) = proposed(
            &line,
            Some(Language::Rust),
            22,
            false,
            &theme,
            &RowCount::default(),
        );
        let strings: Vec<String> = words(&rows, 2)
            .into_iter()
            .filter(|(_, fg, _)| *fg == Some(theme.syntax.string))
            .map(|(word, _, _)| word)
            .collect();
        assert_eq!(strings, ["\"one two", "three four five\""]);
        assert!(
            rows.iter()
                .flat_map(|row| row.spans.iter())
                .all(|span| span.style.bg == Some(theme.add_bg)),
            "every cell of the row, the padding too, is on the added line's wash"
        );
    }

    /// The mock's own example: two lines replaced by five, numbered on each
    /// side as a unified diff numbers them.
    #[test]
    fn removed_and_added_lines_are_numbered_each_on_their_own_side() {
        let diff = change(
            vec![hunk(
                40,
                40,
                vec![
                    context("fn label_for() {"),
                    context("    let unsettled = 0;"),
                    removed("    old one"),
                    removed("    old two"),
                    added("    new one"),
                    added("    new two"),
                    added("    new three"),
                    context("}"),
                ],
            )],
            None,
        );

        let rows = drawn(&diff, 30);

        assert_eq!(
            rows,
            vec![
                "40   fn label_for() {         ",
                "41       let unsettled = 0;   ",
                "42 -     old one              ",
                "43 -     old two              ",
                "42 +     new one              ",
                "43 +     new two              ",
                "44 +     new three            ",
                "45   }                        ",
            ]
        );
    }

    #[test]
    fn unchanged_lines_far_from_a_change_are_one_elision_row() {
        let diff = change(
            vec![hunk(
                1,
                1,
                vec![
                    removed("a"),
                    added("A"),
                    context("1"),
                    context("2"),
                    context("3"),
                    context("4"),
                    context("5"),
                    removed("b"),
                    added("B"),
                ],
            )],
            None,
        );

        let rows = drawn(&diff, 12);

        assert_eq!(
            rows,
            vec![
                "1 - a       ",
                "1 + A       ",
                "2   1       ",
                "3   2       ",
                "  …         ",
                "5   4       ",
                "6   5       ",
                "7 - b       ",
                "7 + B       ",
            ]
        );
    }

    #[test]
    fn unchanged_lines_before_the_first_change_and_after_the_last_keep_only_their_context() {
        let diff = change(
            vec![hunk(
                1,
                1,
                vec![
                    context("1"),
                    context("2"),
                    context("3"),
                    context("4"),
                    added("A"),
                    context("5"),
                    context("6"),
                    context("7"),
                ],
            )],
            None,
        );

        let rows = drawn(&diff, 12);

        assert_eq!(
            rows,
            vec![
                "3   3       ",
                "4   4       ",
                "5 + A       ",
                "6   5       ",
                "7   6       ",
            ]
        );
    }

    #[test]
    fn two_hunks_are_separated_by_an_elision_row_and_numbered_from_their_own_start() {
        let diff = change(
            vec![
                hunk(3, 3, vec![removed("x"), added("y")]),
                hunk(98, 98, vec![context("keep"), removed("p"), added("q")]),
            ],
            None,
        );

        let rows = drawn(&diff, 12);

        assert_eq!(
            rows,
            vec![
                " 3 - x      ",
                " 3 + y      ",
                "   …        ",
                "98   keep   ",
                "99 - p      ",
                "99 + q      ",
            ]
        );
    }

    /// A row keeps its number and its sign whatever the width; the line is
    /// what gives way, and it never wraps onto a row of its own.
    #[test]
    fn a_long_line_is_cut_and_keeps_one_number() {
        let diff = change(
            vec![hunk(7, 7, vec![added("a line far too long for the pane")])],
            None,
        );

        assert_eq!(drawn(&diff, 16), vec!["7 + a line far …"]);
    }

    #[test]
    fn every_row_has_a_sign_cell_of_one_column_even_where_it_is_blank() {
        let diff = change(
            vec![hunk(
                1,
                1,
                vec![context("same"), removed("\tgone"), added("new")],
            )],
            None,
        );

        for row in drawn(&diff, 20) {
            let sign = row
                .chars()
                .nth(2)
                .expect("a number, a space, then the sign");
            assert!([' ', '-', '+'].contains(&sign), "{row:?}");
            assert_eq!(text::width(&row), 20, "{row:?}");
        }
    }

    #[test]
    fn a_diff_longer_than_the_cap_says_how_many_rows_it_left_out() {
        let created = Hunk::created(&(1..=30).map(|n| format!("line {n}\n")).collect::<String>())
            .expect("thirty lines");

        let rows = drawn(&change(vec![created], None), 20);

        assert_eq!(rows.len(), MAX_ROWS + 1);
        assert_eq!(rows[0].trim_end(), " 1 + line 1");
        assert_eq!(rows[MAX_ROWS].trim_end(), "… 10 more rows");
    }

    fn created(lines: usize) -> Change {
        let text: String = (1..=lines).map(|n| format!("line {n}\n")).collect();
        change(vec![Hunk::created(&text).expect("lines of text")], None)
    }

    #[test]
    fn a_diff_one_row_over_the_cap_is_drawn_whole_since_saying_so_takes_a_row() {
        let rows = drawn(&created(MAX_ROWS + 1), 20);

        assert!(!is_cut(created(MAX_ROWS + 1).hunks()));
        assert_eq!(rows.len(), MAX_ROWS + 1);
        assert_eq!(rows[MAX_ROWS].trim_end(), "21 + line 21");
    }

    #[test]
    fn a_diff_two_rows_over_the_cap_is_cut_at_it() {
        let rows = drawn(&created(MAX_ROWS + 2), 20);

        assert!(is_cut(created(MAX_ROWS + 2).hunks()));
        assert_eq!(rows.len(), MAX_ROWS + 1);
        assert_eq!(rows[MAX_ROWS].trim_end(), "… 2 more rows");
    }

    fn thirty_lines() -> Change {
        let created = Hunk::created(&(1..=30).map(|n| format!("line {n}\n")).collect::<String>())
            .expect("thirty lines");
        change(vec![created], Some(Gate::Rule))
    }

    #[test]
    fn a_cut_diff_names_the_key_that_shows_the_rest_where_it_fits() {
        let rows = drawn(&thirty_lines(), 50);

        assert_eq!(
            rows[MAX_ROWS].trim_end(),
            "… 10 more rows · Ctrl+T shows them"
        );
    }

    #[test]
    fn an_opened_diff_draws_every_row_then_says_how_to_cut_it_again() {
        let rows = drawn_as(&thirty_lines(), 50, true);

        assert_eq!(rows.len(), 30 + 2, "every line, the marker and the footer");
        assert_eq!(rows[29].trim_end(), "30 + line 30");
        assert_eq!(
            rows[30].trim_end(),
            "▴ the last 10 rows are shown · Ctrl+T hides them"
        );
        assert_eq!(rows[31], "└ allowed by a standing rule");
    }

    #[test]
    fn a_marker_too_wide_for_the_pane_drops_the_key_before_the_count() {
        let rows = drawn(&thirty_lines(), 24);

        assert_eq!(rows[MAX_ROWS].trim_end(), "… 10 more rows");
    }

    #[test]
    fn a_diff_that_fits_is_the_same_opened_or_not() {
        let diff = change(vec![hunk(1, 1, vec![removed("a"), added("b")])], None);

        assert!(!is_cut(diff.hunks()));
        assert_eq!(drawn_as(&diff, 20, true), drawn(&diff, 20));
    }

    #[test]
    fn the_footer_says_who_let_the_call_through() {
        let one = || vec![hunk(1, 1, vec![removed("a"), added("b")])];
        let footer = |gate| drawn(&change(one(), Some(gate)), 60).pop();

        assert_eq!(footer(Gate::Operator).as_deref(), Some("└ allowed by you"));
        assert_eq!(
            footer(Gate::OperatorAlways).as_deref(),
            Some("└ allowed by you, with a standing rule saved")
        );
        assert_eq!(
            footer(Gate::Rule).as_deref(),
            Some("└ allowed by a standing rule")
        );
        assert_eq!(
            footer(Gate::Unasked(Mode::Auto)).as_deref(),
            Some("└ not asked · auto mode")
        );
    }

    #[test]
    fn a_change_nobody_can_say_was_gated_has_no_footer() {
        let rows = drawn(&change(vec![hunk(1, 1, vec![added("b")])], None), 20);
        assert_eq!(rows, vec!["1 + b               "]);
    }

    /// A line laid out only as far as the cut still counts every row it
    /// wraps to in the marker, before the cut and under Ctrl+T.
    #[test]
    fn a_proposed_line_longer_than_the_cut_counts_every_row_it_wraps_to() {
        // Twenty cells a row once the sign takes two of twenty-two: 35 rows.
        let line = [context("keep"), added(&"x".repeat(700))];
        let text = |rows: &[Line<'_>]| -> Vec<String> {
            rows.iter()
                .map(|row| row.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect()
        };

        let (cut, beyond) = proposed(&line, None, 22, false, &CLASSIC, &RowCount::default());
        let cut = text(&cut);
        assert_eq!(cut.len(), MAX_ROWS + 1);
        assert_eq!(cut[1], format!("+ {}", "x".repeat(20)));
        assert_eq!(cut[MAX_ROWS].trim_end(), "… 16 more rows");
        assert_eq!(beyond, 16);

        let (open, beyond) = proposed(&line, None, 22, true, &CLASSIC, &RowCount::default());
        let open = text(&open);
        assert_eq!(
            open.len(),
            1 + 35 + 1,
            "the context, every row and the marker"
        );
        assert_eq!(open[35], format!("  {}", "x".repeat(20)));
        assert_eq!(beyond, 0);
    }

    /// The marker under a cut question says how many rows Ctrl+T adds, so
    /// a line past the cut counts every row it wraps to, as one before it
    /// does, and so does the row that marks lines left out.
    #[test]
    fn lines_past_the_cut_count_every_row_they_wrap_to() {
        let wide = [added(&"x".repeat(700)), added(&"x".repeat(700))];
        let (open, _) = proposed(&wide, None, 22, true, &CLASSIC, &RowCount::default());
        assert_eq!(open.len(), 70 + 1, "every row of both lines and the marker");

        let (_, beyond) = proposed(&wide, None, 22, false, &CLASSIC, &RowCount::default());
        assert_eq!(beyond, 70 - MAX_ROWS);

        let mut apart = vec![added(&"x".repeat(700))];
        apart.extend((0..2 * CONTEXT_LINES + 1).map(|_| context("same")));
        apart.push(added("y"));
        let (open, _) = proposed(&apart, None, 22, true, &CLASSIC, &RowCount::default());
        let (_, beyond) = proposed(&apart, None, 22, false, &CLASSIC, &RowCount::default());
        assert_eq!(
            beyond,
            open.len() - 1 - MAX_ROWS,
            "the elided row is a row too"
        );
    }

    /// The rows a change wraps to are counted once for a width and kept:
    /// every later layout at that width, cut or open, says what a layout
    /// counting them afresh says, and one at another width counts again.
    #[test]
    fn the_rows_counted_once_for_a_width_are_the_rows_of_every_layout_at_it() {
        let mut lines = vec![added(&"x".repeat(700))];
        lines.extend((0..2 * CONTEXT_LINES + 1).map(|_| context("same")));
        lines.extend((0..OPEN_ROWS + 500).map(|at| added(&"word ".repeat(at % 9 + 1))));
        let kept = RowCount::default();
        for (width, open) in [
            (22, false),
            (22, true),
            (40, true),
            (40, false),
            (22, false),
            (22, true),
        ] {
            let laid = proposed(&lines, None, width, open, &CLASSIC, &kept);
            let afresh = proposed(&lines, None, width, open, &CLASSIC, &RowCount::default());
            assert_eq!(laid, afresh, "{width} columns, open: {open}");
        }
    }

    #[test]
    fn the_diff_is_drawn_on_its_own_background_and_not_the_panes() {
        let diff = change(vec![hunk(1, 1, vec![context("a"), added("b")])], None);
        for line in lines(&diff, 20, false, &CLASSIC) {
            assert_eq!(background(&line), Some(CLASSIC.diff_bg));
            assert_ne!(background(&line), Some(CLASSIC.pane_bg));
        }
    }
}
