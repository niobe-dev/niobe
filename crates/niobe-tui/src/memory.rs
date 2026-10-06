// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What the agent is told and remembers, shown in the shell.
//!
//! The agent's memory is more than one file: the user's own instructions, the
//! repository's and those of the directories above it, what each of them
//! imports, and the notes the backend keeps for itself. The shell reads no
//! file, so the binary hands it something that reads them — the way
//! [`crate::history`] is handed something that reads earlier sessions — and
//! F8 lists what it found, in the order the agent is given it. Enter shows a
//! file's text, read-only; `e` hands it to the editor.
//!
//! Which files a backend loads is the backend's own rule, and the binary is
//! what knows it: this module names no file and no backend.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::text;
use crate::theme::Theme;

/// Where a file the agent is given comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The machine's policy, which an administrator sets.
    Managed,
    /// The user's own, given in every session.
    User,
    /// A directory above the one the session runs in.
    Parent,
    /// The directory the session runs in.
    Project,
    /// The project's file that is not meant to be committed.
    Local,
    /// A file another one imports; it is listed under it.
    Imported,
    /// The index of the notes the backend keeps for itself.
    AutoMemory,
    /// A note that index points to.
    AutoEntry,
}

impl Scope {
    /// The word the list names it by.
    pub fn label(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::User => "user",
            Self::Parent => "parent",
            Self::Project => "project",
            Self::Local => "local",
            Self::Imported => "imported",
            Self::AutoMemory => "auto-memory",
            Self::AutoEntry => "note",
        }
    }
}

/// A file the agent is given, or can reach from what it is given, as the
/// binary read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryFile {
    /// Where it comes from.
    pub scope: Scope,
    /// Where it is, as the list shows it: an auto-memory note by its name
    /// alone, under the index that lists it.
    pub path: String,
    /// The path a change the backend reports names it by: relative to where
    /// the session runs where it is inside it, and absolute otherwise. What
    /// tells the list that the session changed it.
    pub named: String,
    /// How many imports deep it is, which the list indents it by.
    pub depth: usize,
    /// What it holds, where it could be read.
    pub text: Option<String>,
    /// How large it is, in bytes, where it could be read.
    pub bytes: Option<u64>,
    /// What the backend's own index says of it, for one of its notes.
    pub note: Option<String>,
    /// The path the editor is handed, where it is a regular file that is
    /// not a link: a repository decides what its own files are.
    pub editable: Option<String>,
}

/// Something that reads the agent's memory for the shell.
pub trait Memory: std::fmt::Debug {
    /// Every file the agent is given, in the order it is given them, read as
    /// they are now. A handful of small files, read when F8 is pressed.
    fn read(&mut self) -> Vec<MemoryFile>;
}

/// No memory to read: a recorded log being looked at, or a session with no
/// backend that loads any.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoMemory;

impl Memory for NoMemory {
    fn read(&mut self) -> Vec<MemoryFile> {
        Vec::new()
    }
}

/// The Memory view, while it is open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct View {
    /// What was read; `None` until the read is handed in.
    pub files: Option<Vec<MemoryFile>>,
    /// The row the cursor is on.
    pub at: usize,
    /// The file being read, where Enter opened one, and how far down it is
    /// scrolled.
    pub reading: Option<(usize, usize)>,
}

/// What a key in the view asks the shell to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing beyond what the view did to itself.
    Stay,
    /// Close the view.
    Close,
    /// Hand this path to the editor.
    Edit(String),
    /// Say this, where the key had nothing to act on.
    Say(&'static str),
}

/// How many lines Page Up and Page Down move a file being read.
const PAGE: usize = 10;

impl View {
    /// The file the cursor is on.
    pub fn current(&self) -> Option<&MemoryFile> {
        self.files.as_ref()?.get(self.at)
    }

    /// One key, which the view has whole while it is open.
    pub fn on_key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> Outcome {
        use ratatui::crossterm::event::KeyCode;

        if key.code == KeyCode::Char('e') {
            return match self.current().and_then(|file| file.editable.clone()) {
                Some(path) => Outcome::Edit(path),
                None if self.current().is_some() => Outcome::Say(NOT_EDITABLE),
                None => Outcome::Stay,
            };
        }
        if let Some((_, scroll)) = self.reading.as_mut() {
            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Left => {
                    self.reading = None;
                }
                KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::Down => *scroll = scroll.saturating_add(1),
                KeyCode::PageUp => *scroll = scroll.saturating_sub(PAGE),
                KeyCode::PageDown => *scroll = scroll.saturating_add(PAGE),
                KeyCode::Home => *scroll = 0,
                _ => {}
            }
            return Outcome::Stay;
        }
        let last = self
            .files
            .as_ref()
            .map_or(0, |files| files.len().saturating_sub(1));
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Close,
            KeyCode::Up => self.at = self.at.saturating_sub(1),
            KeyCode::Down => self.at = self.at.saturating_add(1).min(last),
            KeyCode::Home => self.at = 0,
            KeyCode::End => self.at = last,
            KeyCode::Enter | KeyCode::Right => match self.current() {
                Some(file) if file.text.is_some() => self.reading = Some((self.at, 0)),
                Some(_) => return Outcome::Say(UNREAD),
                None => return Outcome::Close,
            },
            _ => {}
        }
        Outcome::Stay
    }
}

/// What `e` says on a file the editor may not be handed.
const NOT_EDITABLE: &str = "Only a regular file that is not a link is handed to the editor";

/// What Enter says on a file that could not be read.
const UNREAD: &str = "That file could not be read";

/// Widest the view is drawn, in columns: a path and its figures, side by side.
const COLUMNS: u16 = 100;

/// Narrowest it is drawn.
const MIN_COLUMNS: u16 = 40;

/// What stands between a row's parts.
const GAP: &str = "  ";

/// How wide the scope column is: `auto-memory`.
const SCOPE_COLUMNS: usize = 11;

/// What marks the row the cursor is on.
const CURSOR: &str = "› ";

/// What the list says of a file the session changed.
const CHANGED: &str = "changed";

/// Draws the Memory view over `body`, if it is open.
pub(crate) fn draw(frame: &mut Frame, body: Rect, app: &App, theme: &Theme) {
    let Some(view) = app.memory() else {
        return;
    };
    let width = COLUMNS.min(body.width.saturating_sub(crate::ui::DIALOG_MARGIN * 2));
    if width < MIN_COLUMNS {
        return;
    }
    // A row each for the border and the shadow, and one of room.
    let rows = usize::from(body.height).saturating_sub(4);
    let text_width = usize::from(width).saturating_sub(crate::ui::DIALOG_INSET);
    let (title, lines, keys) = match view.reading {
        Some((at, scroll)) => {
            let file = view.files.as_ref().and_then(|files| files.get(at));
            let title = file.map_or_else(
                || "Memory".to_owned(),
                |file| format!("Memory · {}", file.path),
            );
            let lines = reading(file, scroll, (text_width, rows), theme);
            (title, lines, " ↑↓ PgUp PgDn scroll · e edit · Esc back ")
        }
        None => (
            "Memory".to_owned(),
            listing(app, view, (text_width, rows), theme),
            " ↑↓ choose · Enter read · e edit · Esc close ",
        ),
    };
    // What the shell says of the key just pressed goes where the keys are
    // said: the bar it is said on elsewhere is under the view.
    let footer = match app.hint() {
        Some(hint) => format!(" {hint} "),
        None => keys.to_owned(),
    };
    let title = text::truncate(&title, text_width);
    let inner = crate::ui::dialog(frame, body, (width, lines.len()), (&title, &footer), theme);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The list: a row a file, in the order the agent is given them.
fn listing(
    app: &App,
    view: &View,
    (width, rows): (usize, usize),
    theme: &Theme,
) -> Vec<Line<'static>> {
    let dim = Style::new().fg(theme.dim);
    let mut lines = vec![Line::from("")];
    let Some(files) = view.files.as_ref() else {
        lines.push(Line::styled("reading…", dim));
        return lines;
    };
    if files.is_empty() {
        lines.push(Line::styled(
            text::truncate(
                "The agent is given no memory here: no instructions file and no notes.",
                width,
            ),
            Style::new().fg(theme.dialog_fg),
        ));
        if app.offers("init") {
            lines.push(Line::styled(
                text::truncate(
                    "`/init` has the backend write one for this repository.",
                    width,
                ),
                dim,
            ));
        }
        return lines;
    }
    let room = rows.saturating_sub(2).max(1);
    let first = view.at.saturating_sub(room.saturating_sub(1));
    for (i, file) in files.iter().enumerate().skip(first).take(room) {
        lines.push(row(app, file, i == view.at, width, theme));
    }
    lines
}

/// One file's row: where it comes from, where it is, what the backend's index
/// says of it, and on the right how large it is and whether the session
/// changed it.
fn row(app: &App, file: &MemoryFile, on_it: bool, width: usize, theme: &Theme) -> Line<'static> {
    let changed = app.changed_in_session(&file.named);
    let mut figures = sized(file);
    if changed {
        figures.push_str(" · ");
        figures.push_str(CHANGED);
    }
    let marker = if on_it { CURSOR } else { "  " };
    // The scopes stand in one column; what a file imports is indented under
    // it in the path column.
    let indent = "  ".repeat(file.depth);
    let lead = format!(
        "{marker}{}{GAP}{indent}",
        text::pad(file.scope.label(), SCOPE_COLUMNS)
    );
    let room = width
        .saturating_sub(text::width(&lead))
        .saturating_sub(text::width(&figures))
        .saturating_sub(GAP.len());
    let path = text::truncate_start(&file.path, room);
    let note = file
        .note
        .as_deref()
        .map(|note| format!(" — {note}"))
        .unwrap_or_default();
    let note = text::truncate(&note, room.saturating_sub(text::width(&path)));
    let gap = width
        .saturating_sub(text::width(&lead))
        .saturating_sub(text::width(&path))
        .saturating_sub(text::width(&note))
        .saturating_sub(text::width(&figures));
    if on_it {
        let said = format!("{lead}{path}{note}{}{figures}", " ".repeat(gap));
        return Line::from(said).style(Style::new().bg(theme.cursor_bg).fg(theme.cursor_fg).bold());
    }
    let mut spans = vec![
        Span::styled(lead, Style::new().fg(theme.dim)),
        Span::styled(path, Style::new().fg(theme.dialog_fg)),
        Span::styled(note, Style::new().fg(theme.dim)),
        Span::raw(" ".repeat(gap)),
    ];
    match changed {
        true => {
            let size = sized(file);
            spans.push(Span::styled(size, Style::new().fg(theme.dim)));
            spans.push(Span::styled(
                format!(" · {CHANGED}"),
                Style::new().fg(theme.warn).bold(),
            ));
        }
        false => spans.push(Span::styled(figures, Style::new().fg(theme.dim))),
    }
    Line::from(spans)
}

/// `12 lines · 1.2 kB`: how large a file is, as read. An em dash where it
/// could not be read; no token figure, which nothing measured.
fn sized(file: &MemoryFile) -> String {
    match (&file.text, file.bytes) {
        (Some(text), Some(bytes)) => {
            let lines = text.lines().count();
            let unit = if lines == 1 { "line" } else { "lines" };
            format!("{lines} {unit} · {}", crate::app::human_bytes(bytes))
        }
        _ => "—".to_owned(),
    }
}

/// A file's text, read-only, wrapped to the view and scrolled.
fn reading(
    file: Option<&MemoryFile>,
    scroll: usize,
    (width, rows): (usize, usize),
    theme: &Theme,
) -> Vec<Line<'static>> {
    let plain = Style::new().fg(theme.dialog_fg);
    let text = file
        .and_then(|file| file.text.as_deref())
        .unwrap_or_default();
    let wrapped: Vec<String> = text
        .lines()
        .flat_map(|line| match line.is_empty() {
            true => vec![String::new()],
            false => text::wrap(&text::expand_tabs(line), width),
        })
        .collect();
    let room = rows.max(1);
    let first = scroll.min(wrapped.len().saturating_sub(room));
    let mut lines: Vec<Line<'static>> = wrapped
        .into_iter()
        .skip(first)
        .take(room)
        .map(|line| Line::styled(line, plain))
        .collect();
    if lines.is_empty() {
        lines.push(Line::styled(
            "the file is empty",
            Style::new().fg(theme.dim),
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn file(path: &str, text: Option<&str>, editable: bool) -> MemoryFile {
        MemoryFile {
            scope: Scope::Project,
            path: path.to_owned(),
            named: path.to_owned(),
            depth: 0,
            text: text.map(str::to_owned),
            bytes: text.map(|text| text.len() as u64),
            note: None,
            editable: editable.then(|| format!("/work/repo/{path}")),
        }
    }

    #[test]
    fn enter_reads_the_file_under_the_cursor_and_esc_goes_back_to_the_list() {
        let mut view = View {
            files: Some(vec![
                file("CLAUDE.md", Some("@AGENTS.md\n"), true),
                file("AGENTS.md", Some("# Agents\n"), true),
            ]),
            ..View::default()
        };
        view.on_key(key(KeyCode::Down));
        assert_eq!(view.on_key(key(KeyCode::Enter)), Outcome::Stay);
        assert_eq!(view.reading, Some((1, 0)));

        assert_eq!(view.on_key(key(KeyCode::Esc)), Outcome::Stay);
        assert_eq!(view.reading, None);
        assert_eq!(view.on_key(key(KeyCode::Esc)), Outcome::Close);
    }

    #[test]
    fn e_hands_the_file_to_the_editor_only_where_it_may_be() {
        let mut view = View {
            files: Some(vec![
                file("CLAUDE.md", Some(""), true),
                file("linked.md", Some(""), false),
            ]),
            ..View::default()
        };
        assert_eq!(
            view.on_key(key(KeyCode::Char('e'))),
            Outcome::Edit("/work/repo/CLAUDE.md".to_owned())
        );
        view.on_key(key(KeyCode::Down));
        assert_eq!(
            view.on_key(key(KeyCode::Char('e'))),
            Outcome::Say(NOT_EDITABLE)
        );
    }

    #[test]
    fn a_file_that_could_not_be_read_says_so_rather_than_showing_nothing() {
        let mut view = View {
            files: Some(vec![file("CLAUDE.md", None, false)]),
            ..View::default()
        };
        assert_eq!(view.on_key(key(KeyCode::Enter)), Outcome::Say(UNREAD));
        assert_eq!(view.reading, None);
    }

    #[test]
    fn a_files_size_is_its_lines_and_bytes_and_an_em_dash_where_it_was_not_read() {
        assert_eq!(sized(&file("a", Some("one\ntwo\n"), true)), "2 lines · 8 B");
        assert_eq!(sized(&file("a", Some("one"), true)), "1 line · 3 B");
        assert_eq!(sized(&file("a", None, true)), "—");
    }
}
