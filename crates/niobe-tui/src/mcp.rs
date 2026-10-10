// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The MCP servers the backend configured, shown in the shell.
//!
//! F7 lists every server the backend last reported, with how it stands, the
//! scope its configuration came from and how many tools it gave the model;
//! Enter shows one server whole — why it failed, how it is reached, what it
//! calls itself and its tools — and `r` asks the backend to connect to it
//! again, where the backend can be asked. The view draws from the session's
//! fold on every frame rather than from a copy taken when it opened, so a
//! newer report, such as the one a reconnect is followed by, is what it
//! shows. Opening it sends no turn, so it opens while one runs.
//!
//! Adding, removing or turning a server off, and signing in to one, are not
//! offered: each changes what the backend keeps past the session, and the
//! view names where that is done instead.

use niobe_core::event::{McpServer, McpStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::text;
use crate::theme::Theme;

/// The MCP view, while it is open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct View {
    /// The row the cursor is on.
    pub at: usize,
    /// The server shown whole, where Enter opened one, by its name — a newer
    /// report may list the servers in another order — and how far down it is
    /// scrolled.
    pub detail: Option<(String, usize)>,
}

/// What a key in the view asks the shell to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing beyond what the view did to itself.
    Stay,
    /// Close the view.
    Close,
    /// Ask the backend to connect to this server again.
    Reconnect(String),
}

/// How many lines Page Up and Page Down move a server shown whole.
const PAGE: usize = 10;

impl View {
    /// One key, which the view has whole while it is open, against the
    /// servers as they stand now.
    pub fn on_key(
        &mut self,
        key: ratatui::crossterm::event::KeyEvent,
        servers: &[McpServer],
    ) -> Outcome {
        use ratatui::crossterm::event::KeyCode;

        let last = servers.len().saturating_sub(1);
        self.at = self.at.min(last);
        if key.code == KeyCode::Char('r') {
            let name = match &self.detail {
                Some((name, _)) => Some(name.clone()),
                None => servers.get(self.at).map(|server| server.name.clone()),
            };
            return name.map_or(Outcome::Stay, Outcome::Reconnect);
        }
        if let Some((_, scroll)) = self.detail.as_mut() {
            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Left => {
                    self.detail = None;
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
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Close,
            KeyCode::Up => self.at = self.at.saturating_sub(1),
            KeyCode::Down => self.at = self.at.saturating_add(1).min(last),
            KeyCode::Home => self.at = 0,
            KeyCode::End => self.at = last,
            KeyCode::Enter | KeyCode::Right => match servers.get(self.at) {
                Some(server) => self.detail = Some((server.name.clone(), 0)),
                None => return Outcome::Close,
            },
            _ => {}
        }
        Outcome::Stay
    }

    /// Told by the draw how far the server shown whole can scroll, so a key
    /// past its end does not leave a scroll the next key has to undo first.
    pub fn measured(&mut self, max_scroll: usize) {
        if let Some((_, scroll)) = self.detail.as_mut() {
            *scroll = (*scroll).min(max_scroll);
        }
    }
}

/// Widest the view is drawn, in columns.
const COLUMNS: u16 = 100;

/// Narrowest it is drawn.
const MIN_COLUMNS: u16 = 40;

/// What marks the row the cursor is on.
const CURSOR: &str = "› ";

/// What stands between a row's parts.
const GAP: &str = "  ";

/// What a server shown whole names each of its parts by, padded to one
/// column.
const LABELS: usize = 13;

/// What the view's title says.
const TITLE: &str = "MCP servers";

/// What is drawn over `body` while the view is open, and what the draw
/// measured.
struct Drawn {
    title: String,
    lines: Vec<Line<'static>>,
    keys: &'static str,
    max_scroll: Option<usize>,
}

/// Draws the MCP view over `body`, if it is open.
pub(crate) fn draw(frame: &mut Frame, body: Rect, app: &mut App, theme: &Theme) {
    let Some(view) = app.mcp() else {
        return;
    };
    let width = COLUMNS.min(body.width.saturating_sub(crate::ui::DIALOG_MARGIN * 2));
    if width < MIN_COLUMNS {
        return;
    }
    // A row each for the border and the shadow, and one of room.
    let rows = usize::from(body.height).saturating_sub(4);
    let text_width = usize::from(width).saturating_sub(crate::ui::DIALOG_INSET);
    let drawn = contents(app, view, (text_width, rows), theme);
    // What the shell says of the key just pressed goes where the keys are
    // said: the bar it is said on elsewhere is under the view.
    let footer = match app.hint() {
        Some(hint) => format!(" {hint} "),
        None => drawn.keys.to_owned(),
    };
    let title = text::truncate(&drawn.title, text_width);
    let inner = crate::ui::dialog(
        frame,
        body,
        (width, drawn.lines.len()),
        (&title, &footer),
        theme,
    );
    frame.render_widget(Paragraph::new(drawn.lines), inner);
    if let Some(max_scroll) = drawn.max_scroll {
        app.measured_mcp(max_scroll);
    }
}

/// The view's title, lines and keys, for the list or for one server.
fn contents(app: &App, view: &View, (width, rows): (usize, usize), theme: &Theme) -> Drawn {
    let servers = app.session().mcp_servers();
    let reconnects = app.reconnects_mcp();
    match &view.detail {
        Some((name, scroll)) => {
            let server = servers.iter().find(|server| &server.name == name);
            let (lines, max_scroll) = detail(server, *scroll, (width, rows), theme);
            Drawn {
                title: format!("{TITLE} · {name}"),
                lines,
                keys: match reconnects {
                    true => " ↑↓ scroll · r reconnect · Esc back ",
                    false => " ↑↓ scroll · Esc back ",
                },
                max_scroll: Some(max_scroll),
            }
        }
        None => Drawn {
            title: TITLE.to_owned(),
            lines: listing(app, servers, view.at, (width, rows), theme),
            keys: match (servers.is_empty(), reconnects) {
                (true, _) => " Esc close ",
                (false, true) => " ↑↓ choose · Enter detail · r reconnect · Esc close ",
                (false, false) => " ↑↓ choose · Enter detail · Esc close ",
            },
            max_scroll: None,
        },
    }
}

/// The list: a row a server, in the backend's order, then where a change to
/// one is made.
fn listing(
    app: &App,
    servers: &[McpServer],
    at: usize,
    (width, rows): (usize, usize),
    theme: &Theme,
) -> Vec<Line<'static>> {
    let dim = Style::new().fg(theme.dim);
    let mut lines = vec![Line::from("")];
    let elsewhere = elsewhere(app, width);
    if servers.is_empty() {
        for said in text::wrap(
            "No MCP server has been reported: the backend lists its servers as a turn \
             starts, and lists none where it has none.",
            width,
        ) {
            lines.push(Line::styled(said, Style::new().fg(theme.dialog_fg)));
        }
    } else {
        let columns = Columns::of(servers, width);
        let room = rows
            .saturating_sub(elsewhere.len())
            .saturating_sub(3)
            .max(1);
        let at = at.min(servers.len().saturating_sub(1));
        let first = at.saturating_sub(room.saturating_sub(1));
        for (i, server) in servers.iter().enumerate().skip(first).take(room) {
            lines.push(row(server, &columns, i == at, width, theme));
        }
    }
    lines.push(Line::from(""));
    lines.extend(elsewhere.into_iter().map(|said| Line::styled(said, dim)));
    lines
}

/// Where adding, removing, turning off or signing in to a server is done,
/// wrapped to the view.
fn elsewhere(app: &App, width: usize) -> Vec<String> {
    let backend = app.backend_name();
    let mut said = format!(
        "Adding, removing or turning a server off and signing in to one are the {backend} \
         CLI's own: run `{backend}` in a terminal and use /mcp there, or `{backend} mcp \
         --help`. A change reaches the next session."
    );
    if let Some(files) = app.mcp_config_files() {
        said.push(' ');
        said.push_str(files);
    }
    text::wrap(&said, width)
}

/// How wide the list's columns are drawn, from the servers in it, the name's
/// no more than the share of the width that leaves a status, a scope and a tool
/// count room: a plugin's name can be longer than the screen.
struct Columns {
    name: usize,
    status: usize,
    origin: usize,
}

impl Columns {
    fn of(servers: &[McpServer], width: usize) -> Self {
        let widest = |cell: fn(&McpServer) -> &str| {
            servers
                .iter()
                .map(|server| text::width(cell(server)))
                .max()
                .unwrap_or(0)
        };
        Self {
            name: widest(|server| &server.name).min(width / 2),
            status: widest(|server| server.status.label()),
            origin: widest(origin),
        }
    }
}

/// Where a server's configuration came from, or an em dash where the backend
/// did not say.
fn origin(server: &McpServer) -> &str {
    server.origin().unwrap_or("—")
}

/// One server's row: its name, how it stands, where it is configured, and on
/// the right how many tools it gave the model.
fn row(
    server: &McpServer,
    columns: &Columns,
    on_it: bool,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let marker = if on_it { CURSOR } else { "  " };
    let name = text::truncate(&server.name, columns.name);
    let lead = format!("{marker}{}{GAP}", text::pad(&name, columns.name));
    let status = text::pad(server.status.label(), columns.status);
    let origin = format!("{GAP}{}", text::pad(origin(server), columns.origin));
    let tools = tool_count(server.tools.len());
    let used = [&lead, &status, &origin, &tools]
        .iter()
        .map(|part| text::width(part))
        .sum::<usize>();
    let gap = " ".repeat(width.saturating_sub(used).max(2));
    if on_it {
        let said = text::truncate(&format!("{lead}{status}{origin}{gap}{tools}"), width);
        return Line::from(said).style(Style::new().bg(theme.cursor_bg).fg(theme.cursor_fg).bold());
    }
    Line::from(vec![
        Span::styled(lead, Style::new().fg(theme.dialog_fg)),
        Span::styled(status, status_style(&server.status, theme)),
        Span::styled(origin, Style::new().fg(theme.dim)),
        Span::raw(gap),
        Span::styled(tools, Style::new().fg(theme.dim)),
    ])
}

/// How a status is coloured: what keeps a server's tools from the model in
/// the warning colour, a connected one in the body's.
fn status_style(status: &McpStatus, theme: &Theme) -> Style {
    match status {
        McpStatus::Connected => Style::new().fg(theme.dialog_fg),
        McpStatus::Pending | McpStatus::Disabled | McpStatus::Other(_) => {
            Style::new().fg(theme.dim)
        }
        McpStatus::Failed | McpStatus::NeedsAuth => Style::new().fg(theme.warn).bold(),
    }
}

/// How many tools a server gave the model, as its row says it.
fn tool_count(tools: usize) -> String {
    match tools {
        0 => "no tools".to_owned(),
        1 => "1 tool".to_owned(),
        n => format!("{n} tools"),
    }
}

/// One server shown whole, scrolled, and how far it can scroll.
fn detail(
    server: Option<&McpServer>,
    scroll: usize,
    (width, rows): (usize, usize),
    theme: &Theme,
) -> (Vec<Line<'static>>, usize) {
    let Some(server) = server else {
        return (
            vec![
                Line::from(""),
                Line::styled(
                    text::truncate("The backend no longer reports this server.", width),
                    Style::new().fg(theme.dim),
                ),
            ],
            0,
        );
    };
    let mut all: Vec<Line<'static>> = Vec::new();
    let field = |all: &mut Vec<Line<'static>>, label: &str, value: &str, style: Style| {
        let room = width.saturating_sub(LABELS);
        for (i, said) in text::wrap(value, room.max(1)).into_iter().enumerate() {
            let label = if i == 0 { label } else { "" };
            all.push(Line::from(vec![
                Span::styled(text::pad(label, LABELS), Style::new().fg(theme.dim)),
                Span::styled(said, style),
            ]));
        }
    };
    let plain = Style::new().fg(theme.dialog_fg);
    let unsaid = "—";
    field(
        &mut all,
        "status",
        server.status.label(),
        status_style(&server.status, theme),
    );
    if let Some(error) = &server.error {
        field(&mut all, "why", error, Style::new().fg(theme.warn));
    }
    field(&mut all, "scope", server.origin().unwrap_or(unsaid), plain);
    field(
        &mut all,
        "reached by",
        server.endpoint.as_deref().unwrap_or(unsaid),
        plain,
    );
    field(
        &mut all,
        "calls itself",
        server.reports_as.as_deref().unwrap_or(unsaid),
        plain,
    );
    let tools = match server.tools.is_empty() {
        true => "none".to_owned(),
        false => server.tools.join(", "),
    };
    field(&mut all, "tools", &tools, plain);
    let room = rows.max(1);
    let max_scroll = all.len().saturating_sub(room);
    let mut lines = vec![Line::from("")];
    lines.extend(all.into_iter().skip(scroll.min(max_scroll)).take(room));
    (lines, max_scroll)
}

/// What the open view shows at `width` columns, as plain text a line a row,
/// with the keys it offers last.
#[cfg(test)]
pub(crate) fn shown(app: &App, width: usize) -> Vec<String> {
    let Some(view) = app.mcp() else {
        return Vec::new();
    };
    let drawn = contents(app, view, (width, 40), &Theme::default());
    let mut lines: Vec<String> = drawn
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect();
    lines.insert(0, drawn.title);
    lines.push(drawn.keys.to_owned());
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn server(name: &str, status: McpStatus) -> McpServer {
        McpServer {
            name: name.to_owned(),
            status,
            source: Some("dynamic".to_owned()),
            tools: Vec::new(),
            scope: None,
            error: None,
            endpoint: None,
            reports_as: None,
        }
    }

    fn said(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn enter_shows_the_server_under_the_cursor_and_esc_goes_back_to_the_list() {
        let servers = [
            server("notes", McpStatus::Connected),
            server("broken", McpStatus::Failed),
        ];
        let mut view = View::default();
        assert_eq!(view.on_key(key(KeyCode::Down), &servers), Outcome::Stay);
        assert_eq!(view.on_key(key(KeyCode::Enter), &servers), Outcome::Stay);
        assert_eq!(view.detail, Some(("broken".to_owned(), 0)));
        assert_eq!(view.on_key(key(KeyCode::Esc), &servers), Outcome::Stay);
        assert_eq!(view.detail, None);
        assert_eq!(view.on_key(key(KeyCode::Esc), &servers), Outcome::Close);
    }

    #[test]
    fn r_asks_to_reconnect_the_server_under_the_cursor_or_the_one_shown() {
        let servers = [
            server("notes", McpStatus::Connected),
            server("broken", McpStatus::Failed),
        ];
        let mut view = View::default();
        assert_eq!(
            view.on_key(key(KeyCode::Char('r')), &servers),
            Outcome::Reconnect("notes".to_owned())
        );
        view.on_key(key(KeyCode::Down), &servers);
        view.on_key(key(KeyCode::Enter), &servers);
        assert_eq!(
            view.on_key(key(KeyCode::Char('r')), &servers),
            Outcome::Reconnect("broken".to_owned())
        );
        assert_eq!(
            View::default().on_key(key(KeyCode::Char('r')), &[]),
            Outcome::Stay
        );
    }

    #[test]
    fn a_cursor_past_a_shorter_list_the_backend_sent_since_lands_on_its_last_server() {
        let mut view = View {
            at: 4,
            detail: None,
        };
        let servers = [server("notes", McpStatus::Connected)];
        assert_eq!(view.on_key(key(KeyCode::Enter), &servers), Outcome::Stay);
        assert_eq!(view.detail, Some(("notes".to_owned(), 0)));
    }

    #[test]
    fn a_server_shown_whole_says_why_it_failed_how_it_is_reached_and_its_tools() {
        let theme = Theme::default();
        let broken = McpServer {
            error: Some("ECONNREFUSED: Unable to connect.".to_owned()),
            scope: Some("project".to_owned()),
            endpoint: Some("http · http://127.0.0.1:9/mcp".to_owned()),
            ..server("remote docs", McpStatus::Failed)
        };
        let (lines, max_scroll) = detail(Some(&broken), 0, (60, 20), &theme);
        assert_eq!(max_scroll, 0);
        let field = |label: &str, value: &str| format!("{label:<13}{value}");
        assert_eq!(
            said(&lines),
            [
                String::new(),
                field("status", "failed"),
                field("why", "ECONNREFUSED: Unable to connect."),
                field("scope", "project"),
                field("reached by", "http · http://127.0.0.1:9/mcp"),
                field("calls itself", "—"),
                field("tools", "none"),
            ]
        );
    }

    /// A plugin-style name longer than the view: it is cut to its share, and
    /// the status, the scope and the tool count of every row are still there.
    #[test]
    fn a_name_longer_than_the_view_leaves_every_servers_status_on_its_row() {
        let theme = Theme::default();
        let long = "plugin:".to_owned() + &"x".repeat(129);
        let servers = [
            server(&long, McpStatus::Connected),
            McpServer {
                scope: Some("project".to_owned()),
                ..server("broken", McpStatus::Failed)
            },
        ];
        let columns = Columns::of(&servers, 100);
        let rows: Vec<Line<'static>> = servers
            .iter()
            .enumerate()
            .map(|(at, server)| row(server, &columns, at == 0, 100, &theme))
            .collect();
        let rows = said(&rows);

        assert!(rows[0].contains("connected"), "{:?}", rows[0]);
        assert!(rows[1].contains("failed"), "{:?}", rows[1]);
        assert!(rows[1].contains("project"), "{:?}", rows[1]);
        assert!(rows.iter().all(|row| text::width(row) <= 100), "{rows:?}");
    }

    #[test]
    fn a_server_the_backend_stopped_reporting_says_so_rather_than_showing_nothing() {
        let (lines, _) = detail(None, 0, (60, 20), &Theme::default());
        assert!(
            said(&lines)
                .iter()
                .any(|line| line.contains("no longer reports")),
            "{:?}",
            said(&lines)
        );
    }

    #[test]
    fn a_long_server_shown_whole_scrolls_and_stops_at_its_end() {
        let many = McpServer {
            tools: (0..40).map(|n| format!("tool_number_{n}")).collect(),
            ..server("big", McpStatus::Connected)
        };
        let (_, max_scroll) = detail(Some(&many), 0, (40, 5), &Theme::default());
        assert!(max_scroll > 0);
        let (lines, _) = detail(Some(&many), usize::MAX, (40, 5), &Theme::default());
        assert_eq!(lines.len(), 6, "the blank line and a screenful");
    }
}
