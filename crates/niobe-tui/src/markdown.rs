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

use std::borrow::Cow;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::text;
use crate::theme::Theme;

/// What stands before an image's alt text where the image cannot be drawn.
///
/// One cell wide in every terminal: a picture emoji is drawn two cells wide
/// by some terminals and measured as one, which pushed the rest of its row out
/// of line with the cells the shell counted for it.
const IMAGE_MARK: &str = "\u{29c9} ";

/// A piece of text in one style, and how the whitespace in it is drawn.
type Run = (String, Style, Spacing);

/// How a run's whitespace is drawn between its words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spacing {
    /// A run of it is one space, as prose reads: where the source broke a
    /// line or put two spaces is not part of what it says.
    Collapsed,
    /// Every space as written, as inside a code span: `sed 's/  / /'` says
    /// what it does with how many spaces it holds.
    Kept,
}

/// What a table cell holds: its runs.
type Cell = Vec<Run>;

/// The gutter a code block's lines sit behind.
const CODE_GUTTER: &str = "│ ";

/// What a block quote's lines sit behind.
const QUOTE_BAR: &str = "▎ ";

/// Between two table columns.
const COLUMN_GAP: &str = " │ ";

/// The markdown a reply is read as: CommonMark with GitHub's extensions.
fn options() -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);
    options
}

/// Draws `source` as lines no wider than `width` cells.
pub fn render(source: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut renderer = Renderer::new(width.max(1), theme);
    for event in Parser::new_ext(source, options()) {
        renderer.event(event);
    }
    renderer.finish()
}

/// Whether [`render`] would leave some of `source`'s words undrawn, as
/// markdown means it to: a link reference definition, which only lends its
/// destination and title to the links that name it, a link's or an image's
/// title, which [`render`] draws nowhere, and a table cell past the number
/// its header has, which GitHub's tables ignore.
///
/// For text the operator approves as written, such as a plan: a line that
/// draws nothing is still a line the agent reads and carries out.
pub(crate) fn hides_text(source: &str) -> bool {
    let parser = Parser::new_ext(source, options()).into_offset_iter();
    // Every definition, the second of two with one label too, leaves its
    // label among them: the first is kept and the rest are not drawn either.
    if parser.reference_definitions().iter().next().is_some() {
        return true;
    }
    let mut cells_end = None;
    for (event, range) in parser {
        match event {
            // A title is drawn nowhere: a terminal has no tooltip to lend it.
            Event::Start(Tag::Link { title, .. } | Tag::Image { title, .. })
                if !title.trim().is_empty() =>
            {
                return true;
            }
            Event::Start(Tag::TableRow | Tag::TableHead) => cells_end = Some(range.start),
            Event::End(TagEnd::TableCell) => cells_end = Some(range.end),
            Event::End(TagEnd::TableRow | TagEnd::TableHead) => {
                let past_cells = cells_end
                    .and_then(|end| source.get(end..range.end))
                    .unwrap_or_default();
                if past_cells.chars().any(|c| !c.is_whitespace() && c != '|') {
                    return true;
                }
                cells_end = None;
            }
            _ => {}
        }
    }
    false
}

/// What an enclosing block puts in front of the lines inside it: a bullet or a
/// number on the first line, spaces under it on the rest, or a quote's bar on
/// every one.
#[derive(Debug)]
struct Indent {
    first: String,
    rest: String,
    style: Style,
    /// The cells `rest` takes, which is what `first` takes too, worked out
    /// once: the room is asked for on every block opened and closed.
    cells: usize,
    /// Whether the first line has been drawn, after which every line takes
    /// `rest`.
    used: bool,
}

impl Indent {
    fn new(first: String, rest: String, style: Style) -> Self {
        let cells = text::width(&rest);
        Self {
            first,
            rest,
            style,
            cells,
            used: false,
        }
    }
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
    /// How many of `indents` are list items', counted as they open and close
    /// so that a block opened deep inside quotes does not look through every
    /// one of them to learn whether it is in an item.
    items: usize,
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
            items: 0,
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
                self.run(&text::expand_tabs(&code), style, Spacing::Kept);
            }
            Event::SoftBreak => self.text(" ", self.style()),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
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
                self.text(IMAGE_MARK, self.style());
                self.open_destination(&dest_url);
            }
            // Drawn where it is written, which is the end of a reply as a
            // rule, marked with the name its reference in the text has.
            Tag::FootnoteDefinition(name) => {
                self.gap();
                let dim = self.theme.dim;
                self.text(&format!("[^{name}] "), Style::new().fg(dim));
            }
            // A tight list item's own text arrives with no paragraph around
            // it, so every block that can open inside an item — this one, a
            // quote, a heading, a rule, a list — ends that text first, or the
            // text would be drawn after the block or run into its first word.
            Tag::HtmlBlock => self.flush(),
            Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::MetadataBlock(_)
            | Tag::Superscript
            | Tag::Subscript => {}
        }
    }

    fn start_heading(&mut self, level: HeadingLevel) {
        self.flush();
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
        self.flush();
        self.gap();
        self.indents.push(Indent::new(
            QUOTE_BAR.to_owned(),
            QUOTE_BAR.to_owned(),
            Style::new().fg(self.theme.dim),
        ));
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
        self.indents
            .push(Indent::new(marker, rest, Style::new().fg(self.theme.hot)));
        self.items = self.items.saturating_add(1);
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
                self.items = self.items.saturating_sub(1);
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
        self.runs.iter().map(|(text, _, _)| text.len()).sum()
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
        let said: String = self.runs.iter().map(|(text, _, _)| text.as_str()).collect();
        let own = said
            .get(at..)
            .unwrap_or_default()
            .trim_start_matches(IMAGE_MARK);
        if destination.is_empty() || own == destination {
            return;
        }
        let dim = self.theme.dim;
        self.text(&format!(" ({destination})"), Style::new().fg(dim));
    }

    fn text(&mut self, text: &str, style: Style) {
        self.run(text, style, Spacing::Collapsed);
    }

    fn run(&mut self, text: &str, style: Style, spacing: Spacing) {
        match self.runs.last_mut() {
            Some((last, last_style, last_spacing))
                if *last_style == style && *last_spacing == spacing =>
            {
                last.push_str(text);
            }
            _ => self.runs.push((text.to_owned(), style, spacing)),
        }
    }

    /// Puts a blank line before a block, unless it opens the reply, follows a
    /// blank line already, or sits inside a list item, where blocks follow one
    /// another tightly the way the item's text does.
    fn gap(&mut self) {
        let in_item = self.items > 0;
        let after_blank = self.lines.last().is_none_or(|line| line.width() == 0);
        if !in_item && !after_blank {
            self.lines.push(Line::from(""));
        }
    }

    /// The columns left for text once the drawn indents are paid.
    fn room(&self) -> usize {
        let indent: usize = self.indents[self.outermost_drawn()..]
            .iter()
            .map(|i| i.cells)
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
            taken = taken.saturating_add(indent.cells);
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
    ///
    /// Only the drawn indents and the unused ones are visited, so a line
    /// costs what its own marks cost however deep the blocks around it go.
    /// The unused ones are always the innermost: an indent is pushed unused
    /// on top of the others, every line marks them all, and only the
    /// innermost is ever popped.
    fn emit(&mut self, content: Vec<Span<'static>>) {
        let drawn = self.outermost_drawn();
        let marks = self.indents.get(drawn..).unwrap_or_default();
        let mut spans = Vec::with_capacity(marks.len() + content.len());
        for indent in marks {
            let marker = match indent.used {
                true => &indent.rest,
                false => &indent.first,
            };
            spans.push(Span::styled(marker.clone(), indent.style));
        }
        for indent in self.indents.iter_mut().rev() {
            if indent.used {
                break;
            }
            indent.used = true;
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
            if line.is_empty() {
                self.emit(vec![
                    Span::styled(CODE_GUTTER, gutter),
                    Span::styled("", style),
                ]);
            } else {
                let expanded = text::expand_tabs(line);
                let pieces = text::split_to_width(&expanded, room);
                for piece in pieces {
                    self.emit(vec![
                        Span::styled(CODE_GUTTER, gutter),
                        Span::styled(piece.to_owned(), style),
                    ]);
                }
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
                    .map(|cell| cell.iter().map(|(text, _, _)| text::width(text)).sum())
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
                    runs.push((": ".to_owned(), label, Spacing::Collapsed));
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
                spans.extend(format_cell(cell, *width, alignments.get(column)));
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

/// Formats a single table cell's spans according to column width and alignment.
fn format_cell(
    cell: Vec<Span<'static>>,
    width: usize,
    alignment: Option<&Alignment>,
) -> Vec<Span<'static>> {
    let used: usize = cell.iter().map(|span| text::width(&span.content)).sum();
    let pad = " ".repeat(width.saturating_sub(used));
    let mut spans = Vec::new();
    match alignment {
        Some(Alignment::Right) => {
            spans.push(Span::raw(pad));
            spans.extend(cell);
        }
        Some(Alignment::Center) => {
            let (left, right) = pad.split_at(pad.len() / 2);
            spans.push(Span::raw(left.to_owned()));
            spans.extend(cell);
            spans.push(Span::raw(right.to_owned()));
        }
        Some(Alignment::Left | Alignment::None) | None => {
            spans.extend(cell);
            spans.push(Span::raw(pad));
        }
    }
    spans
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

// Optimization: `words` borrows `&str` slices instead of allocating owned `String`s
// for every word fragment. `push_span` merges adjacent text fragments sharing the
// exact same `Style` into single `Span`s when building lines, eliminating allocation
// churn and dramatically reducing span count per frame during steady TUI redraws.

/// Wraps styled runs to `width` cells, breaking between words and inside a
/// word only where it is wider than the line. A word can span runs — a code
/// span with a possessive after it — and stays whole across them.
fn wrap_runs(runs: &[Run], width: usize) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1);
    let words = words(runs);
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut line: Vec<Span<'static>> = Vec::new();
    let mut used = 0;

    for Word {
        parts: word,
        gap,
        gap_style,
    } in words
    {
        let word_width: usize = word.iter().map(|(text, _)| text::width(text)).sum();
        let gap_width = text::width(&gap);
        if used > 0 && used + gap_width + word_width > width {
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
                push_spans(&mut line, piece);
            }
            continue;
        }
        if used > 0 {
            push_span(&mut line, &gap, gap_style);
            used += gap_width;
        }
        used += word_width;
        for (text, style) in word {
            push_span(&mut line, text, style);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Appends text to `line`, merging with the preceding span if it shares `style`.
fn push_span(line: &mut Vec<Span<'static>>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    match line.last_mut() {
        Some(last) if last.style == style => last.content.to_mut().push_str(text),
        _ => line.push(Span::styled(text.to_owned(), style)),
    }
}

/// Appends `pieces` to `line`, merging adjacent spans that share the same style.
fn push_spans(line: &mut Vec<Span<'static>>, pieces: Vec<Span<'static>>) {
    for span in pieces {
        if span.content.is_empty() {
            continue;
        }
        match line.last_mut() {
            Some(last) if last.style == span.style => last.content.to_mut().push_str(&span.content),
            _ => line.push(span),
        }
    }
}

/// A word of the runs: its styled fragments, and the whitespace drawn
/// before it where it does not start a line, in the style of the run that
/// whitespace was in, so an underlined link keeps its underline across the
/// spaces inside it.
struct Word<'a> {
    parts: Vec<(&'a str, Style)>,
    gap: Cow<'a, str>,
    gap_style: Style,
}

/// The whitespace between two words as it builds up across runs: each
/// stretch of it from a [`Spacing::Collapsed`] run is one space, and a
/// [`Spacing::Kept`] run's is kept as written.
#[derive(Default)]
struct Gap<'a> {
    text: Cow<'a, str>,
    style: Style,
    collapsing: bool,
}

impl Gap<'_> {
    /// Adds the whitespace character a run of `spacing` held, drawn as a
    /// space whatever it was: a code span's line break is one, as CommonMark
    /// reads it, and its tabs were expanded before it was gathered.
    fn add(&mut self, style: Style, spacing: Spacing) {
        self.style = style;
        match spacing {
            Spacing::Collapsed if self.collapsing => return,
            Spacing::Collapsed => self.collapsing = true,
            Spacing::Kept => self.collapsing = false,
        }
        // One space, the gap almost every word has, is borrowed rather than
        // allocated.
        if self.text.is_empty() {
            self.text = Cow::Borrowed(" ");
        } else {
            self.text.to_mut().push(' ');
        }
    }
}

/// The runs cut into words at their whitespace.
fn words<'a>(runs: &'a [Run]) -> Vec<Word<'a>> {
    let mut words = Words::default();
    for (text, style, spacing) in runs {
        let mut start_idx = 0;
        for (idx, c) in text.char_indices() {
            if c.is_whitespace() {
                if idx > start_idx {
                    words.part(&text[start_idx..idx], *style);
                }
                words.end_word();
                words.gap.add(*style, *spacing);
                start_idx = idx + c.len_utf8();
            }
        }
        if start_idx < text.len() {
            words.part(&text[start_idx..], *style);
        }
    }
    words.end_word();
    words.done
}

/// The words cut so far, the one being gathered, the whitespace before it
/// and the whitespace after the last.
#[derive(Default)]
struct Words<'a> {
    done: Vec<Word<'a>>,
    word: Vec<(&'a str, Style)>,
    before: Gap<'a>,
    gap: Gap<'a>,
}

impl<'a> Words<'a> {
    fn part(&mut self, text: &'a str, style: Style) {
        if self.word.is_empty() {
            self.before = std::mem::take(&mut self.gap);
        }
        self.word.push((text, style));
    }

    fn end_word(&mut self) {
        if self.word.is_empty() {
            return;
        }
        let before = std::mem::take(&mut self.before);
        self.done.push(Word {
            parts: std::mem::take(&mut self.word),
            gap: before.text,
            gap_style: before.style,
        });
    }
}

/// A word wider than the line, cut into line-wide pieces with their styles.
fn split_word(word: &[(&str, Style)], width: usize) -> Vec<(Vec<Span<'static>>, usize)> {
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

    #[test]
    fn a_link_reference_definition_is_text_the_drawing_hides() {
        assert!(hides_text(
            "Fix it.\n\n[2]: <https://x.invalid> \"then run it\"\n"
        ));
        assert!(hides_text("- item\n\n  [a]: /x\n  [a]: /y\n"));
        assert!(!hides_text("Fix [it](https://x.invalid)."));
    }

    /// A link's or an image's title is lent to a tooltip a terminal has no
    /// place for, so the drawing leaves it out; a plan's words in one are
    /// still words the agent reads.
    #[test]
    fn a_link_or_image_title_is_text_the_drawing_hides() {
        assert!(hides_text(
            "Fix [it](https://x.invalid \"then pipe it to sh\")."
        ));
        assert!(hides_text("See ![shot](/a.png 'and delete it')."));
        assert!(!hides_text("See ![shot](/a.png) and <https://x.invalid>."));
    }

    #[test]
    fn a_table_cell_past_its_header_is_text_the_drawing_hides() {
        assert!(hides_text("| a |\n|---|\n| b | rm -rf ~ |\n"));
        assert!(hides_text("| a |\n|---|\n| b | c\n"));
        assert!(!hides_text("| a | b |\n|---|---|\n| c | d |\n| e |\n"));
        assert!(!hides_text("| a |\n|---|\n| `x \\| y` |\n"));
    }

    #[test]
    fn markdown_that_draws_all_its_words_hides_nothing() {
        assert!(!hides_text(
            "# Plan\n\n1. **Guard** the list.\n2. Run `cargo test`.\n\n```sh\nls\n```\n\n> quoted\n\n<!-- note -->\n[^1]\n\n[^1]: a note\n"
        ));
    }

    fn drawn(source: &str, width: usize) -> Vec<String> {
        plain(&render(source, width, &CLASSIC))
    }

    fn span<'a>(lines: &'a [Line<'_>], content: &str) -> &'a Span<'a> {
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.contains(content))
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
    fn inline_code_keeps_its_spaces_and_draws_a_tab_as_spaces_to_the_next_stop() {
        assert_eq!(
            drawn("run `a  b` then `x\ty` and `  lead` or `trail  ` now", 80),
            ["run a  b then x   y and   lead or trail   now"]
        );
    }

    #[test]
    fn inline_code_with_its_spaces_kept_still_wraps_inside_the_width() {
        let lines = drawn("say `one   two   three   four` again", 14);
        assert_eq!(lines, ["say one   two", "three   four", "again"]);
        let lines = drawn(&format!("x `a{}b` y", " ".repeat(40)), 10);
        assert_eq!(lines, ["x a", "b y"]);
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
    fn the_image_mark_is_one_cell_wide_wherever_it_is_drawn() {
        use unicode_width::UnicodeWidthStr as _;

        let mark = IMAGE_MARK.trim_end();
        assert_eq!(mark.width(), 1);
        assert_eq!(mark.width_cjk(), 1, "not of ambiguous width");
        assert!(
            mark.chars()
                .all(|c| !matches!(u32::from(c), 0x2600..=0x27bf | 0x1f000..=0x1faff)),
            "not a character a terminal may draw as an emoji"
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
        assert!(text.contains("\u{29c9} img (x.png)"), "{text}");
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
    fn a_list_item_keeps_its_own_text_above_a_block_that_follows_it() {
        assert_eq!(drawn("- text\n  > quote", 40), ["• text", "  ▎ quote"]);
        assert_eq!(drawn("- text\n  # head", 40), ["• text", "  head"]);
        assert_eq!(
            drawn("- text\n  ***\n- next", 10),
            ["• text", "  ────────", "• next"]
        );
        assert_eq!(
            drawn("- text\n  <div>\n  html\n  </div>", 40),
            ["• text", "  <div> html </div>"]
        );
    }

    #[test]
    fn a_task_list_shows_its_boxes() {
        assert_eq!(
            drawn("- [x] done\n- [ ] not yet", 40),
            ["• ☑ done", "• ☐ not yet"]
        );
    }

    #[test]
    fn a_soft_break_is_a_space_between_the_words_it_joins() {
        assert_eq!(drawn("foo\nbar", 40), ["foo bar"]);
    }

    #[test]
    fn a_rule_is_as_wide_as_the_room_inside_a_list_and_a_quote() {
        let in_list = drawn("- one\n\n  ---\n\n  two", 20);
        let rule = in_list
            .iter()
            .find(|row| row.contains('─'))
            .expect("the rule is drawn");
        assert!(text::width(rule) <= 20, "{in_list:?}");
        let quoted = drawn("> ---", 20);
        assert!(
            quoted.iter().all(|row| text::width(row) <= 20),
            "{quoted:?}"
        );
        let bare = drawn("---", 20);
        assert_eq!(text::width(&bare[0]), 20, "{bare:?}");
    }

    #[test]
    fn a_centred_column_puts_the_odd_cell_of_padding_on_the_right() {
        let table = "| mids | z |\n|:----:|---|\n| b | y |";
        assert_eq!(drawn(table, 40), ["mids │ z", "─────┼──", " b   │ y"],);
    }
}
