// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The assistant's replies, drawn from the markdown they are written in.
//!
//! A reply is CommonMark with GitHub's tables, strikethrough and task lists,
//! and shown raw it reads as asterisks, backticks and pipes. It is parsed here
//! and drawn as styled lines: emphasis as bold and italic, code in a colour of
//! its own, lists with hanging indents, code blocks behind a gutter and tables
//! with their columns lined up.
//!
//! Wrapping is done here, word by word across the styled runs, rather than by
//! the widget. The transcript scrolls by wrapped lines, so it has to know how
//! many a reply takes before it is drawn — the same reason [`crate::text`]
//! wraps plain text up front.
//!
//! A reply still streaming is parsed as it stands. An emphasis or a code span
//! not closed yet reads as the characters typed so far, and becomes styled
//! when its closing mark arrives.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::text;
use crate::theme::Theme;

/// A piece of text in one style.
type Run = (String, Style);

/// What a table cell holds: its runs.
type Cell = Vec<Run>;

/// The gutter a code block's lines sit behind.
const CODE_GUTTER: &str = "│ ";

/// What a block quote's lines sit behind.
const QUOTE_BAR: &str = "▎ ";

/// Between two table columns.
const COLUMN_GAP: &str = " │ ";

/// Draws `source` as lines no wider than `width` cells.
pub fn render(source: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);

    let mut renderer = Renderer::new(width.max(1), theme);
    for event in Parser::new_ext(source, options) {
        renderer.event(event);
    }
    renderer.finish()
}

/// What an enclosing block puts in front of the lines inside it: a bullet or a
/// number on the first line, spaces under it on the rest, or a quote's bar on
/// every one.
#[derive(Debug)]
struct Indent {
    first: String,
    rest: String,
    style: Style,
    /// Whether the first line has been drawn, after which every line takes
    /// `rest`.
    used: bool,
}

/// A table being read: its column alignments, its rows so far, and the cell
/// being filled.
#[derive(Debug, Default)]
struct Table {
    alignments: Vec<Alignment>,
    header: Vec<Cell>,
    rows: Vec<Vec<Cell>>,
    row: Vec<Cell>,
    in_header: bool,
}

#[derive(Debug)]
struct Renderer<'t> {
    width: usize,
    theme: &'t Theme,
    lines: Vec<Line<'static>>,
    /// The paragraph, heading or list item text being gathered.
    runs: Vec<Run>,
    /// The inline style in force, innermost last.
    styles: Vec<Style>,
    indents: Vec<Indent>,
    /// The next number of each open list, `None` for a bulleted one.
    lists: Vec<Option<u64>>,
    /// A fenced or indented code block being gathered.
    code: Option<String>,
    table: Option<Table>,
    /// For each link or image open, where it leads and how much text had
    /// been gathered when it opened, so its own text can be told apart.
    destinations: Vec<(String, usize)>,
}

impl<'t> Renderer<'t> {
    fn new(width: usize, theme: &'t Theme) -> Self {
        Self {
            width,
            theme,
            lines: Vec::new(),
            runs: Vec::new(),
            styles: vec![Style::new().fg(theme.fg)],
            indents: Vec::new(),
            lists: Vec::new(),
            code: None,
            table: None,
            destinations: Vec::new(),
        }
    }

    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or_default()
    }

    fn push_style(&mut self, change: impl FnOnce(Style) -> Style) {
        let next = change(self.style());
        self.styles.push(next);
    }

    fn pop_style(&mut self) {
        if self.styles.len() > 1 {
            self.styles.pop();
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => match &mut self.code {
                Some(code) => code.push_str(&text),
                None => self.text(&text, self.style()),
            },
            Event::Code(code) => {
                let style = self.style().fg(self.theme.code);
                self.text(&code, style);
            }
            Event::SoftBreak => self.text(" ", self.style()),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.gap();
                let rule = "─".repeat(self.room());
                let style = Style::new().fg(self.theme.dim);
                self.emit(vec![Span::styled(rule, style)]);
            }
            Event::TaskListMarker(done) => {
                let marker = if done { "☑ " } else { "☐ " };
                self.text(marker, self.style());
            }
            // Markup the shell has no way to draw is shown as it was written,
            // which is what the operator would read in the raw reply anyway.
            Event::Html(html) | Event::InlineHtml(html) => self.text(&html, self.style()),
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                self.text(&math, self.style().fg(self.theme.code));
            }
            Event::FootnoteReference(name) => self.text(&format!("[^{name}]"), self.style()),
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.gap(),
            Tag::Heading { level, .. } => self.start_heading(level),
            Tag::BlockQuote(_) => self.start_block_quote(),
            Tag::CodeBlock(kind) => self.start_code_block(kind),
            // No blank line before a list: it most often follows the sentence
            // that introduces it (`It does three things:`), and reads as part
            // of it. A list inside an item ends the item's own text first.
            Tag::List(start) => {
                self.flush();
                self.lists.push(start);
            }
            Tag::Item => self.start_item(),
            Tag::Table(alignments) => self.start_table(alignments),
            Tag::TableHead => {
                if let Some(table) = &mut self.table {
                    table.in_header = true;
                }
                self.push_style(|style| style.add_modifier(Modifier::BOLD));
            }
            Tag::TableRow | Tag::TableCell => {}
            Tag::Emphasis => self.push_style(|style| style.add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(|style| style.add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => {
                self.push_style(|style| style.add_modifier(Modifier::CROSSED_OUT));
            }
            Tag::Link { dest_url, .. } => {
                self.open_destination(&dest_url);
                let link = self.theme.agent;
                self.push_style(|style| style.fg(link).add_modifier(Modifier::UNDERLINED));
            }
            // An image cannot be drawn; its alt text, which arrives as text
            // inside it, is what stands in for it.
            Tag::Image { dest_url, .. } => {
                self.text("🖼 ", self.style());
                self.open_destination(&dest_url);
            }
            // Drawn where it is written, which is the end of a reply as a
            // rule, marked with the name its reference in the text has.
            Tag::FootnoteDefinition(name) => {
                self.gap();
                let dim = self.theme.dim;
                self.text(&format!("[^{name}] "), Style::new().fg(dim));
            }
            Tag::HtmlBlock
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::MetadataBlock(_)
            | Tag::Superscript
            | Tag::Subscript => {}
        }
    }

    fn start_heading(&mut self, level: HeadingLevel) {
        self.gap();
        let underline = level == HeadingLevel::H1;
        let title = self.theme.title;
        self.push_style(|style| {
            let style = style.fg(title).add_modifier(Modifier::BOLD);
            match underline {
                true => style.add_modifier(Modifier::UNDERLINED),
                false => style,
            }
        });
    }

    fn start_block_quote(&mut self) {
        self.gap();
        self.indents.push(Indent {
            first: QUOTE_BAR.to_owned(),
            rest: QUOTE_BAR.to_owned(),
            style: Style::new().fg(self.theme.dim),
            used: false,
        });
        let dim = self.theme.dim;
        self.push_style(|style| style.fg(dim).add_modifier(Modifier::ITALIC));
    }

    fn start_code_block(&mut self, kind: CodeBlockKind<'_>) {
        self.flush();
        self.gap();
        if let CodeBlockKind::Fenced(language) = kind
            && !language.is_empty()
        {
            let label = text::truncate(&format!("╭ {language}"), self.room());
            self.emit(vec![Span::styled(label, Style::new().fg(self.theme.dim))]);
        }
        self.code = Some(String::new());
    }

    fn start_item(&mut self) {
        self.flush();
        let marker = match self.lists.last_mut() {
            Some(Some(number)) => {
                let marker = format!("{number}. ");
                *number += 1;
                marker
            }
            _ => "• ".to_owned(),
        };
        let rest = " ".repeat(text::width(&marker));
        self.indents.push(Indent {
            first: marker,
            rest,
            style: Style::new().fg(self.theme.hot),
            used: false,
        });
    }

    fn start_table(&mut self, alignments: Vec<Alignment>) {
        self.flush();
        self.gap();
        self.table = Some(Table {
            alignments,
            ..Table::default()
        });
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::HtmlBlock => self.flush(),
            TagEnd::Heading(_) => {
                self.flush();
                self.pop_style();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.indents.pop();
                self.pop_style();
            }
            TagEnd::CodeBlock => {
                let code = self.code.take().unwrap_or_default();
                self.code_lines(&code);
            }
            TagEnd::List(_) => {
                self.lists.pop();
            }
            TagEnd::Item => {
                self.flush();
                self.indents.pop();
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.runs);
                if let Some(table) = &mut self.table {
                    table.row.push(cell);
                }
            }
            TagEnd::TableHead => {
                if let Some(table) = &mut self.table {
                    table.header = std::mem::take(&mut table.row);
                    table.in_header = false;
                }
                self.pop_style();
            }
            TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push(row);
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.table_lines(&table);
                }
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            TagEnd::Link => {
                self.pop_style();
                self.close_destination();
            }
            TagEnd::Image => self.close_destination(),
            TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::MetadataBlock(_)
            | TagEnd::Superscript
            | TagEnd::Subscript => {}
        }
    }

    /// How much text has been gathered for the block being drawn.
    fn gathered(&self) -> usize {
        self.runs.iter().map(|(text, _)| text.len()).sum()
    }

    fn open_destination(&mut self, destination: &str) {
        let at = self.gathered();
        self.destinations.push((destination.to_owned(), at));
    }

    /// Says where a link or an image leads, after its text, where that is not
    /// what its text already says: the text is the agent's words, and the
    /// address is where following it would go.
    fn close_destination(&mut self) {
        let Some((destination, at)) = self.destinations.pop() else {
            return;
        };
        let said: String = self.runs.iter().map(|(text, _)| text.as_str()).collect();
        let own = said.get(at..).unwrap_or_default().trim_start_matches("🖼 ");
        if destination.is_empty() || own == destination {
            return;
        }
        let dim = self.theme.dim;
        self.text(&format!(" ({destination})"), Style::new().fg(dim));
    }

    fn text(&mut self, text: &str, style: Style) {
        match self.runs.last_mut() {
            Some((last, last_style)) if *last_style == style => last.push_str(text),
            _ => self.runs.push((text.to_owned(), style)),
        }
    }

    /// Puts a blank line before a block, unless it opens the reply, follows a
    /// blank line already, or sits inside a list item, where blocks follow one
    /// another tightly the way the item's text does.
    fn gap(&mut self) {
        let in_item = self
            .indents
            .iter()
            .any(|indent| indent.rest.trim().is_empty());
        let after_blank = self.lines.last().is_none_or(|line| line.width() == 0);
        if !in_item && !after_blank {
            self.lines.push(Line::from(""));
        }
    }

    /// The columns left for text once the drawn indents are paid.
    fn room(&self) -> usize {
        let indent: usize = self.indents[self.outermost_drawn()..]
            .iter()
            .map(|i| text::width(&i.rest))
            .sum();
        self.width.saturating_sub(indent).max(1)
    }

    /// The first of the enclosing blocks whose indent is drawn. Indents take
    /// at most half the width, the innermost first: past that, each level
    /// deeper would leave its text a narrower column, until a word a cell
    /// wide stood behind a line wider than the pane. The bullet or bar
    /// nearest the text is the one that says what the text is in.
    fn outermost_drawn(&self) -> usize {
        let limit = self.width / 2;
        let mut taken: usize = 0;
        for (at, indent) in self.indents.iter().enumerate().rev() {
            taken = taken.saturating_add(text::width(&indent.rest));
            if taken > limit {
                return at + 1;
            }
        }
        0
    }

    /// Wraps the gathered runs and emits them.
    fn flush(&mut self) {
        let runs = std::mem::take(&mut self.runs);
        for line in wrap_runs(&runs, self.room()) {
            self.emit(line);
        }
    }

    /// Emits one line behind the enclosing blocks' drawn indents. An indent
    /// not drawn still counts as used, so the first line it would have
    /// marked is not marked again further down.
    fn emit(&mut self, content: Vec<Span<'static>>) {
        let drawn = self.outermost_drawn();
        let mut spans = Vec::with_capacity(self.indents.len() + content.len());
        for (at, indent) in self.indents.iter_mut().enumerate() {
            let marker = match indent.used {
                true => indent.rest.clone(),
                false => indent.first.clone(),
            };
            indent.used = true;
            if at >= drawn {
                spans.push(Span::styled(marker, indent.style));
            }
        }
        spans.extend(content);
        self.lines.push(Line::from(spans));
    }

    /// A code block's lines behind the gutter, as written: code is not
    /// rewrapped, because where its lines break is part of what it says. A line
    /// wider than the pane is cut into pieces that each keep the gutter. Tabs
    /// are expanded, since a cell cannot draw one.
    fn code_lines(&mut self, code: &str) {
        let room = self.room().saturating_sub(text::width(CODE_GUTTER)).max(1);
        let gutter = Style::new().fg(self.theme.dim);
        let style = Style::new().fg(self.theme.code);
        for line in code.trim_end_matches('\n').split('\n') {
            let pieces = match line.is_empty() {
                true => vec![String::new()],
                false => text::split_to_width(&text::expand_tabs(line), room),
            };
            for piece in pieces {
                self.emit(vec![
                    Span::styled(CODE_GUTTER, gutter),
                    Span::styled(piece, style),
                ]);
            }
        }
    }

    /// A table with its columns lined up, the header in bold over a rule.
    ///
    /// Columns take the width their widest cell needs while the table fits.
    /// When it does not, the narrow columns keep theirs and the wide ones share
    /// what is left, wrapping their cells: a table cut at the edge would lose
    /// the columns a reader most often wants, the last ones. Where even that
    /// leaves a column too narrow to read, the table is drawn a row at a
    /// time instead, each cell on lines of its own after its column's header.
    fn table_lines(&mut self, table: &Table) {
        let columns = std::iter::once(&table.header)
            .chain(&table.rows)
            .map(Vec::len)
            .max()
            .unwrap_or(0);
        if columns == 0 {
            return;
        }
        let natural: Vec<usize> = (0..columns)
            .map(|column| {
                std::iter::once(&table.header)
                    .chain(&table.rows)
                    .filter_map(|row| row.get(column))
                    .map(|cell| cell.iter().map(|(text, _)| text::width(text)).sum())
                    .max()
                    .unwrap_or(0)
                    .max(1)
            })
            .collect();
        let gaps = text::width(COLUMN_GAP) * (columns - 1);
        let Some(widths) = fit_columns(&natural, self.room().saturating_sub(gaps)) else {
            self.stacked_table(table);
            return;
        };

        let separator = Style::new().fg(self.theme.dim);
        if !table.header.is_empty() {
            self.table_row(&table.header, &widths, &table.alignments);
            let rule: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
            self.emit(vec![Span::styled(rule.join("─┼─"), separator)]);
        }
        for row in &table.rows {
            self.table_row(row, &widths, &table.alignments);
        }
    }

    /// A table a row at a time, each cell as `header: value` wrapped to the
    /// pane, the rows a blank line apart. A table still streaming that has
    /// no row yet shows its headers, one to a line.
    fn stacked_table(&mut self, table: &Table) {
        if table.rows.is_empty() {
            for cell in &table.header {
                for line in wrap_runs(cell, self.room()) {
                    self.emit(line);
                }
            }
            return;
        }
        let label = Style::new().fg(self.theme.dim);
        for (at, row) in table.rows.iter().enumerate() {
            if at > 0 {
                self.emit(Vec::new());
            }
            for (column, cell) in row.iter().enumerate() {
                let mut runs: Vec<Run> = table.header.get(column).cloned().unwrap_or_default();
                if !runs.is_empty() {
                    runs.push((": ".to_owned(), label));
                }
                runs.extend(cell.iter().cloned());
                for line in wrap_runs(&runs, self.room()) {
                    self.emit(line);
                }
            }
        }
    }

    fn table_row(&mut self, row: &[Cell], widths: &[usize], alignments: &[Alignment]) {
        let wrapped: Vec<Vec<Vec<Span<'static>>>> = widths
            .iter()
            .enumerate()
            .map(|(column, width)| {
                wrap_runs(row.get(column).map_or(&[][..], Vec::as_slice), *width)
            })
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(0).max(1);
        let separator = Style::new().fg(self.theme.dim);

        for at in 0..height {
            let mut spans = Vec::new();
            for (column, width) in widths.iter().enumerate() {
                if column > 0 {
                    spans.push(Span::styled(COLUMN_GAP, separator));
                }
                let cell = wrapped
                    .get(column)
                    .and_then(|lines| lines.get(at))
                    .cloned()
                    .unwrap_or_default();
                let used: usize = cell.iter().map(|span| text::width(&span.content)).sum();
                let pad = " ".repeat(width.saturating_sub(used));
                let right = matches!(alignments.get(column), Some(Alignment::Right));
                let centre = matches!(alignments.get(column), Some(Alignment::Center));
                match (right, centre) {
                    (true, _) => {
                        spans.push(Span::raw(pad));
                        spans.extend(cell);
                    }
                    (false, true) => {
                        let (left, right) = pad.split_at(pad.len() / 2);
                        spans.push(Span::raw(left.to_owned()));
                        spans.extend(cell);
                        spans.push(Span::raw(right.to_owned()));
                    }
                    (false, false) => {
                        spans.extend(cell);
                        spans.push(Span::raw(pad));
                    }
                }
            }
            // Trailing padding would only be spaces at the pane's edge.
            if let Some(last) = spans.last()
                && last.content.trim().is_empty()
            {
                spans.pop();
            }
            self.emit(spans);
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush();
        if let Some(code) = self.code.take() {
            // A code block still streaming: what has arrived is shown.
            self.code_lines(&code);
        }
        if let Some(table) = self.table.take() {
            self.table_lines(&table);
        }
        while self.lines.last().is_some_and(|line| line.width() == 0) {
            self.lines.pop();
        }
        self.lines
    }
}

/// Column widths that fit `room`: every column its natural width if they all
/// fit, otherwise the narrow ones keep theirs and the rest share what is left
/// equally. `None` where that would leave a column narrower than a few cells,
/// or narrower than its widest cell where that is fewer.
///
/// The share never falls below that floor once the floors fit: a column is
/// kept at its natural width only while that is no wider than the share, so
/// the share only grows as columns are kept.
fn fit_columns(natural: &[usize], room: usize) -> Option<Vec<usize>> {
    const NARROWEST: usize = 4;
    if natural.iter().sum::<usize>() <= room {
        return Some(natural.to_vec());
    }
    let floors: usize = natural.iter().map(|w| (*w).min(NARROWEST)).sum();
    if floors > room {
        return None;
    }
    let mut widths = natural.to_vec();
    let mut fixed = vec![false; natural.len()];
    loop {
        let open: Vec<usize> = (0..natural.len()).filter(|c| !fixed[*c]).collect();
        if open.is_empty() {
            return Some(widths);
        }
        let taken: usize = (0..natural.len())
            .filter(|c| fixed[*c])
            .map(|c| widths[c])
            .sum();
        let share = room.saturating_sub(taken) / open.len();
        let narrow: Vec<usize> = open
            .iter()
            .copied()
            .filter(|c| natural[*c] <= share)
            .collect();
        if narrow.is_empty() {
            for column in open {
                widths[column] = share;
            }
            return Some(widths);
        }
        for column in narrow {
            widths[column] = natural[column];
            fixed[column] = true;
        }
    }
}

/// Wraps styled runs to `width` cells, breaking between words and inside a
/// word only where it is wider than the line. A word can span runs — a code
/// span with a possessive after it — and stays whole across them.
fn wrap_runs(runs: &[Run], width: usize) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1);
    let words = words(runs);
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut line: Vec<Span<'static>> = Vec::new();
    let mut used = 0;

    for (word, space_style) in words {
        let word_width: usize = word.iter().map(|(text, _)| text::width(text)).sum();
        if used > 0 && used + 1 + word_width > width {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        if word_width > width {
            for (piece, piece_width) in split_word(&word, width) {
                if used > 0 && used + piece_width > width {
                    lines.push(std::mem::take(&mut line));
                    used = 0;
                }
                used += piece_width;
                line.extend(piece);
            }
            continue;
        }
        if used > 0 {
            line.push(Span::styled(" ", space_style));
            used += 1;
        }
        used += word_width;
        line.extend(
            word.into_iter()
                .map(|(text, style)| Span::styled(text, style)),
        );
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// The runs cut into words, each a list of styled fragments, with the style
/// of the space before it, so an underlined link keeps its underline across
/// the spaces inside it.
fn words(runs: &[Run]) -> Vec<(Vec<Run>, Style)> {
    let mut words: Vec<(Vec<Run>, Style)> = Vec::new();
    let mut word: Vec<Run> = Vec::new();
    let mut space = Style::default();
    for (text, style) in runs {
        let mut fragment = String::new();
        for c in text.chars() {
            if c.is_whitespace() {
                if !fragment.is_empty() {
                    word.push((std::mem::take(&mut fragment), *style));
                }
                if !word.is_empty() {
                    words.push((std::mem::take(&mut word), space));
                }
                space = *style;
            } else {
                fragment.push(c);
            }
        }
        if !fragment.is_empty() {
            word.push((fragment, *style));
        }
    }
    if !word.is_empty() {
        words.push((word, space));
    }
    words
}

/// A word wider than the line, cut into line-wide pieces with their styles.
fn split_word(word: &[Run], width: usize) -> Vec<(Vec<Span<'static>>, usize)> {
    let mut pieces = Vec::new();
    let mut piece: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for (text, style) in word {
        for (grapheme, w) in crate::text::graphemes(text) {
            if used + w > width && used > 0 {
                pieces.push((std::mem::take(&mut piece), used));
                used = 0;
            }
            match piece.last_mut() {
                Some(last) if last.style == *style => last.content.to_mut().push_str(grapheme),
                _ => piece.push(Span::styled(grapheme.to_owned(), *style)),
            }
            used += w;
        }
    }
    if !piece.is_empty() {
        pieces.push((piece, used));
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::CLASSIC;

    fn plain(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn drawn(source: &str, width: usize) -> Vec<String> {
        plain(&render(source, width, &CLASSIC))
    }

    fn span<'a>(lines: &'a [Line<'_>], content: &str) -> &'a Span<'a> {
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == content)
            .unwrap_or_else(|| panic!("no span {content:?} in {lines:?}"))
    }

    #[test]
    fn emphasis_and_code_are_styled_and_their_marks_are_gone() {
        let lines = render("**How it works:** runs `claude` *quietly*", 80, &CLASSIC);

        assert_eq!(plain(&lines), ["How it works: runs claude quietly"]);
        assert!(
            span(&lines, "works:")
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(span(&lines, "claude").style.fg, Some(CLASSIC.code));
        assert!(
            span(&lines, "quietly")
                .style
                .add_modifier
                .contains(Modifier::ITALIC)
        );
    }

    #[test]
    fn a_heading_is_bold_in_the_title_colour_with_no_hashes() {
        let lines = render("## What makes it different", 80, &CLASSIC);

        assert_eq!(plain(&lines), ["What makes it different"]);
        let heading = span(&lines, "What");
        assert_eq!(heading.style.fg, Some(CLASSIC.title));
        assert!(heading.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn a_bullet_wraps_under_its_own_text_rather_than_under_the_bullet() {
        assert_eq!(
            drawn("- the cost figures can be trusted\n- no telemetry", 20),
            ["• the cost figures", "  can be trusted", "• no telemetry"]
        );
    }

    #[test]
    fn numbered_and_nested_lists_keep_their_numbers_and_their_depth() {
        assert_eq!(
            drawn("1. first\n   - inner\n2. second", 40),
            ["1. first", "   • inner", "2. second"]
        );
    }

    #[test]
    fn paragraphs_are_a_blank_line_apart_and_a_list_follows_its_sentence() {
        assert_eq!(
            drawn("One.\n\nTwo:\n- a\n- b\n\nThree.", 40),
            ["One.", "", "Two:", "• a", "• b", "", "Three."]
        );
    }

    #[test]
    fn a_code_block_keeps_its_lines_behind_a_gutter_and_cuts_only_what_overflows() {
        let lines = drawn("```rust\nfn main() {\n    run();\n}\n```", 14);
        assert_eq!(lines, ["╭ rust", "│ fn main() {", "│     run();", "│ }"]);
        assert_eq!(drawn("```\nabcdefghij\n```", 8), ["│ abcdef", "│ ghij"]);
    }

    #[test]
    fn a_table_lines_up_its_columns_under_a_ruled_header() {
        assert_eq!(
            drawn(
                "| crate | role |\n|---|---:|\n| core | model |\n| tui | ui |",
                40
            ),
            [
                "crate │  role",
                "──────┼──────",
                "core  │ model",
                "tui   │    ui",
            ]
        );
    }

    #[test]
    fn a_table_too_wide_for_the_pane_wraps_its_wide_cells_rather_than_cutting_them() {
        let lines = drawn(
            "| id | what it does |\n|---|---|\n| 1 | reads the whole session back from the store |",
            24,
        );
        assert!(
            lines.iter().all(|line| text::width(line) <= 24),
            "{lines:#?}"
        );
        let text: String = lines.join(" ");
        for word in ["reads", "whole", "session", "store"] {
            assert!(text.contains(word), "{word} was cut: {lines:#?}");
        }
    }

    #[test]
    fn a_table_with_more_columns_than_the_pane_can_line_up_reads_one_cell_to_a_line() {
        assert_eq!(
            drawn(
                "| a | b | c | d | e | f | g | h |\n|---|---|---|---|---|---|---|---|\n\
                 | alpha | beta | gamma | delta | epsilon | zeta | eta | lambda |\n\
                 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 |",
                30,
            ),
            [
                "a: alpha",
                "b: beta",
                "c: gamma",
                "d: delta",
                "e: epsilon",
                "f: zeta",
                "g: eta",
                "h: lambda",
                "",
                "a: 1",
                "b: 2",
                "c: 3",
                "d: 4",
                "e: 5",
                "f: 6",
                "g: 7",
                "h: 8",
            ]
        );
    }

    #[test]
    fn a_table_too_wide_to_line_up_shows_its_headers_before_its_first_row_arrives() {
        assert_eq!(
            drawn(
                "| one | two | three | four | five |\n|---|---|---|---|---|",
                20
            ),
            ["one", "two", "three", "four", "five"]
        );
    }

    #[test]
    fn a_long_code_block_label_is_cut_at_the_edge() {
        assert_eq!(
            drawn("```a-language-name-longer-than-the-pane\nx\n```", 20),
            ["╭ a-language-name-l…", "│ x"]
        );
    }

    #[test]
    fn deep_nesting_gives_up_its_outermost_indents_before_the_text_its_room() {
        let source = "> > > > > > > > > > - - - - - - - - - - deep";
        let lines = drawn(source, 20);
        assert_eq!(lines, ["• • • • • deep"]);
    }

    #[test]
    fn no_line_is_wider_than_the_pane_whatever_the_table_fence_or_nesting() {
        let wide_table = format!(
            "| {} |\n|{}|\n| {} |",
            (1..=12)
                .map(|n| format!("col{n}"))
                .collect::<Vec<_>>()
                .join(" | "),
            ["---"; 12].join("|"),
            (1..=12)
                .map(|n| format!("value number {n}"))
                .collect::<Vec<_>>()
                .join(" | "),
        );
        let nested_table = format!("> - - {wide_table}");
        let fence = format!("```{}\ncode\n```", "language".repeat(20));
        let nesting = format!("{}{}word", "> ".repeat(10), "- ".repeat(10));
        let numbered = format!("{}text", "100. ".repeat(10));
        let mut narrow_columns = String::from("| a | b | c | d | e | f | g | h | i | j | k |\n");
        narrow_columns.push_str(&"|---".repeat(11));
        narrow_columns.push_str("|\n| alpha | beta | gamma | delta | epsilon | zeta | eta | theta | iota | kappa | lambda |");
        for source in [
            wide_table.as_str(),
            nested_table.as_str(),
            fence.as_str(),
            nesting.as_str(),
            numbered.as_str(),
            narrow_columns.as_str(),
        ] {
            for width in 20..=160 {
                for line in drawn(source, width) {
                    assert!(
                        text::width(&line) <= width,
                        "{line:?} is wider than {width} for {source:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_quote_sits_behind_a_bar_and_a_rule_spans_the_width() {
        assert_eq!(
            drawn("> careful\n\n---", 10),
            ["▎ careful", "", "──────────"]
        );
    }

    #[test]
    fn a_link_and_an_image_say_where_they_lead_and_a_footnote_is_drawn_once() {
        let lines = render(
            "[link](https://example.com) and ![img](x.png) and a footnote[^1]\n\n[^1]: said below",
            80,
            &CLASSIC,
        );
        let text = plain(&lines).join("\n");
        assert!(text.contains("link (https://example.com)"), "{text}");
        assert!(text.contains("🖼 img (x.png)"), "{text}");
        assert_eq!(text.matches("said below").count(), 1, "{text}");

        let bare = plain(&render("<https://example.com>", 80, &CLASSIC)).join("\n");
        assert_eq!(bare, "https://example.com", "an address is not said twice");
    }

    #[test]
    fn a_link_reads_as_its_text_underlined() {
        let lines = render("see [the docs](https://example.com) now", 80, &CLASSIC);
        assert_eq!(plain(&lines), ["see the docs (https://example.com) now"]);
        assert!(
            span(&lines, "docs")
                .style
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
    }

    #[test]
    fn a_word_that_spans_styles_is_not_broken_between_them() {
        assert_eq!(
            drawn("uses `niobe-core`'s fold", 16),
            ["uses", "niobe-core's", "fold"]
        );
    }

    #[test]
    fn a_reply_cut_off_mid_stream_still_draws_what_has_arrived() {
        assert_eq!(drawn("**How it", 40), ["**How it"]);
        assert_eq!(drawn("```\nlet x", 40), ["│ let x"]);
    }

    #[test]
    fn a_task_list_shows_its_boxes() {
        assert_eq!(
            drawn("- [x] done\n- [ ] not yet", 40),
            ["• ☑ done", "• ☐ not yet"]
        );
    }
}
