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
//! file's text, read-only; `e` opens it in the operator's text editor, on the
//! terminal the shell lends it (see [`crate::desktop`]), never with the
//! program the system opens files with, which would run one a repository
//! named.
//!
//! Which files a backend loads is the backend's own rule, and the binary is
//! what knows it: this module names no file and no backend.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::detail::{Detail, Kind};
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
    /// How large it is, in bytes, where it could be read or was too large to
    /// be: a size with no text is a file too large to show.
    pub bytes: Option<u64>,
    /// What the backend's own index says of it, for one of its notes.
    pub note: Option<String>,
    /// The path the text editor is handed, where it is a regular file that
    /// is not a link: a repository decides what its own files are.
    pub editable: Option<String>,
}

/// Something that reads the agent's memory for the shell, asked the way
/// [`crate::history::History`] is: the loop asks for a read when F8 opens the
/// view and looks for what it found once a tick, so a lookup held up by a
/// slow disk or a long chain of imports never holds up a frame.
pub trait Memory: std::fmt::Debug {
    /// Starts reading every file the agent is given, as they are now. Returns
    /// at once; what was read arrives through [`Memory::drain`]. A load asked
    /// for while one is under way is that one.
    fn load(&mut self);

    /// What the newest load that has finished found, once, in the order the
    /// agent is given the files. Never blocks.
    fn drain(&mut self) -> Option<Vec<MemoryFile>>;
}

/// No memory to read: a recorded log being looked at, or a session with no
/// backend that loads any. Every load finds nothing, at once.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoMemory {
    asked: bool,
}

impl Memory for NoMemory {
    fn load(&mut self) {
        self.asked = true;
    }

    fn drain(&mut self) -> Option<Vec<MemoryFile>> {
        std::mem::take(&mut self.asked).then(Vec::new)
    }
}

/// The Memory view, while it is open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct View {
    /// What was read; `None` until the read is handed in.
    pub files: Option<Vec<MemoryFile>>,
    /// The row the cursor is on.
    pub at: usize,
    /// The file being read, where Enter opened one, and its text as the
    /// page it is read in, which wraps only the rows on screen: a memory
    /// file can be as large as the binary reads, and wrapping all of it for
    /// every frame would be the frame.
    pub(crate) reading: Option<(usize, Detail)>,
}

/// What a key in the view asks the shell to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing beyond what the view did to itself.
    Stay,
    /// Close the view.
    Close,
    /// Open this path in the text editor.
    Edit(String),
    /// Say this, where the key had nothing to act on.
    Say(&'static str),
}

/// What the page of a file says where the file holds nothing.
const EMPTY: &str = "the file is empty";

impl View {
    /// Told by the draw how wide the file being read is drawn and how many
    /// of its lines show, so a key scrolls by what is on screen and no
    /// further than the end.
    pub(crate) fn measured(&mut self, width: usize, room: usize) {
        if let Some((_, page)) = self.reading.as_mut() {
            page.measured(width, room);
        }
    }

    /// Which file is being read, where Enter opened one.
    pub fn reading(&self) -> Option<usize> {
        self.reading.as_ref().map(|(at, _)| *at)
    }

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
        if let Some((_, page)) = self.reading.as_mut() {
            let back = match key.code {
                KeyCode::Left => true,
                _ => page.on_key(key) == crate::detail::Outcome::Close,
            };
            if back {
                self.reading = None;
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
                Some(MemoryFile {
                    text: Some(text),
                    path,
                    ..
                }) => {
                    let page = Detail::verbatim(path, &as_read(text), EMPTY);
                    self.reading = Some((self.at, page));
                }
                Some(file) if file.bytes.is_some() => return Outcome::Say(TOO_LARGE),
                Some(_) => return Outcome::Say(UNREAD),
                None => return Outcome::Close,
            },
            _ => {}
        }
        Outcome::Stay
    }
}

/// What `e` says on a file the text editor may not be handed.
const NOT_EDITABLE: &str = "Only a regular file that is not a link is opened in the text editor";

/// What Enter says on a file that could not be read.
const UNREAD: &str = "That file could not be read";

/// What Enter says on a file there but too large to read.
const TOO_LARGE: &str = "That file is too large to show; `e` opens it in the text editor";

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
pub(crate) fn draw(frame: &mut Frame, body: Rect, app: &mut App, theme: &Theme) {
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
    let mut measured = None;
    let (title, lines, keys) = match &view.reading {
        Some((at, page)) => {
            let file = view.files.as_ref().and_then(|files| files.get(*at));
            let title = file.map_or_else(
                || "Memory".to_owned(),
                |file| format!("Memory · {}", file.path),
            );
            let room = rows.max(1);
            measured = Some((text_width, room));
            (
                title,
                reading(page, (text_width, room), theme),
                " ↑↓ PgUp PgDn scroll · e edit · Esc back ",
            )
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
    if let Some((width, room)) = measured {
        app.measured_memory(width, room);
    }
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

/// `12 lines · 1.2 kB`: how large a file is, as read; its bytes alone where
/// it was too large to read, and an em dash where it could not be read at
/// all. No token figure, which nothing measured.
fn sized(file: &MemoryFile) -> String {
    match (&file.text, file.bytes) {
        (Some(text), Some(bytes)) => {
            let lines = text.lines().count();
            let unit = if lines == 1 { "line" } else { "lines" };
            format!("{lines} {unit} · {}", crate::app::human_bytes(bytes))
        }
        (None, Some(bytes)) => format!("{} · text not shown", crate::app::human_bytes(bytes)),
        (Some(_), None) | (None, None) => "—".to_owned(),
    }
}

/// A file's text, read-only: the lines of its page on screen, `room` of them
/// at `width` at most.
fn reading(page: &Detail, (width, room): (usize, usize), theme: &Theme) -> Vec<Line<'static>> {
    let plain = Style::new().fg(theme.dialog_fg);
    page.visible(width, room)
        .into_iter()
        .map(|(line, kind)| match kind {
            Kind::Note => Line::styled(line, Style::new().fg(theme.dim)),
            Kind::Heading | Kind::Text => Line::styled(line, plain),
        })
        .collect()
}

/// `text` as it is read: its lines, each without the carriage return a file
/// written with CRLF ends it with, and no empty row after the last.
fn as_read(text: &str) -> String {
    text.lines().collect::<Vec<_>>().join("\n")
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
        assert_eq!(view.reading(), Some(1));

        assert_eq!(view.on_key(key(KeyCode::Esc)), Outcome::Stay);
        assert_eq!(view.reading(), None);
        assert_eq!(view.on_key(key(KeyCode::Esc)), Outcome::Close);
    }

    #[test]
    fn q_closes_the_list_as_esc_does() {
        let mut view = View {
            files: Some(vec![file("CLAUDE.md", Some(""), true)]),
            ..View::default()
        };
        assert_eq!(view.on_key(key(KeyCode::Char('q'))), Outcome::Close);
    }

    /// Down on the last file stays on it, so Enter still reads a file rather
    /// than finding none under the cursor and closing the view.
    #[test]
    fn down_past_the_last_file_stays_on_it() {
        let mut view = View {
            files: Some(vec![
                file("CLAUDE.md", Some(""), true),
                file("AGENTS.md", Some("# Agents\n"), true),
            ]),
            ..View::default()
        };
        for _ in 0..3 {
            view.on_key(key(KeyCode::Down));
        }
        assert_eq!(
            view.current().map(|file| file.path.as_str()),
            Some("AGENTS.md")
        );
        assert_eq!(view.on_key(key(KeyCode::Enter)), Outcome::Stay);
        assert_eq!(view.reading(), Some(1));
    }

    /// A list longer than the view scrolls to keep the cursor's row drawn.
    #[test]
    fn a_long_list_scrolls_to_keep_the_cursor_in_view() {
        let files: Vec<MemoryFile> = (0..10)
            .map(|n| file(&format!("file{n}.md"), Some(""), true))
            .collect();
        let mut view = View {
            files: Some(files),
            ..View::default()
        };
        view.on_key(key(KeyCode::End));
        let app = App::new(crate::app::Repo::default());
        let drawn: Vec<String> = listing(&app, &view, (60, 6), &Theme::default())
            .iter()
            .map(Line::to_string)
            .collect();
        assert!(
            drawn.iter().any(|row| row.contains("file9.md")),
            "{drawn:?}"
        );
        assert!(
            !drawn.iter().any(|row| row.contains("file0.md")),
            "{drawn:?}"
        );
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
        assert_eq!(view.reading(), None);
    }

    fn too_large(path: &str, bytes: u64) -> MemoryFile {
        MemoryFile {
            text: None,
            bytes: Some(bytes),
            ..file(path, None, true)
        }
    }

    #[test]
    fn a_file_too_large_to_read_shows_its_size_and_says_its_text_is_not_shown() {
        assert_eq!(
            sized(&too_large("big.md", 2 * 1024 * 1024)),
            "2.0 MB · text not shown"
        );
        let mut view = View {
            files: Some(vec![too_large("big.md", 2 * 1024 * 1024)]),
            ..View::default()
        };
        assert_eq!(view.on_key(key(KeyCode::Enter)), Outcome::Say(TOO_LARGE));
        assert_eq!(view.reading(), None);
    }

    fn said(lines: &[Line<'_>]) -> Vec<String> {
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

    /// The lines `view` shows of the file it is reading, at `width` and
    /// `room`.
    fn shown(view: &View, width: usize, room: usize) -> Vec<String> {
        let (_, page) = view.reading.as_ref().expect("a file is being read");
        said(&reading(page, (width, room), &Theme::default()))
    }

    fn read(text: &str) -> View {
        let mut view = View {
            files: Some(vec![file("CLAUDE.md", Some(text), true)]),
            ..View::default()
        };
        view.on_key(key(KeyCode::Enter));
        view
    }

    #[test]
    fn a_file_being_read_keeps_its_indentation_and_runs_of_spaces() {
        let text = "- top\n  - nested item\n    - deeper\n\n    key   =   \"v\"\n\tx\n";
        assert_eq!(
            shown(&read(text), 40, 20),
            [
                "- top",
                "  - nested item",
                "    - deeper",
                "",
                "    key   =   \"v\"",
                "    x",
            ]
        );
    }

    #[test]
    fn a_file_written_with_crlf_reads_as_its_lines() {
        assert_eq!(shown(&read("one\r\ntwo\r\n"), 40, 20), ["one", "two"]);
    }

    #[test]
    fn an_empty_file_says_so() {
        assert_eq!(shown(&read(""), 40, 20), [EMPTY]);
    }

    #[test]
    fn a_line_wider_than_the_view_is_cut_at_the_last_cell_that_fits() {
        assert_eq!(
            shown(&read("abcdefgh ij\n"), 4, 20),
            ["abcd", "efgh", " ij"]
        );
    }

    #[test]
    fn scrolling_a_file_stops_at_its_end_so_one_up_moves_it_back() {
        let text: String = (1..=40).map(|n| format!("line {n}\n")).collect();
        let mut view = read(&text);
        view.measured(40, 10);
        for _ in 0..60 {
            view.on_key(key(KeyCode::Down));
        }
        assert_eq!(
            shown(&view, 40, 10).first().map(String::as_str),
            Some("line 31")
        );
        view.on_key(key(KeyCode::Up));
        assert_eq!(
            shown(&view, 40, 10).first().map(String::as_str),
            Some("line 30")
        );
        for _ in 0..5 {
            view.on_key(key(KeyCode::PageDown));
        }
        assert_eq!(
            shown(&view, 40, 10).last().map(String::as_str),
            Some("line 40")
        );
        view.on_key(key(KeyCode::Home));
        assert_eq!(
            shown(&view, 40, 10).first().map(String::as_str),
            Some("line 1")
        );
    }

    #[test]
    fn left_goes_back_to_the_list_as_esc_does() {
        let mut view = read("text\n");
        assert_eq!(view.on_key(key(KeyCode::Left)), Outcome::Stay);
        assert_eq!(view.reading(), None);
    }

    #[test]
    fn a_files_size_is_its_lines_and_bytes_and_an_em_dash_where_it_was_not_read() {
        assert_eq!(sized(&file("a", Some("one\ntwo\n"), true)), "2 lines · 8 B");
        assert_eq!(sized(&file("a", Some("one"), true)), "1 line · 3 B");
        assert_eq!(sized(&file("a", None, true)), "—");
    }
}
