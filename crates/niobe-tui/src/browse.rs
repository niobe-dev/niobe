// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The history dialog, drawn: the prompts or sessions it lists, the filter
//! over them, and what the row under the cursor holds in full.
//!
//! A list row has one line and a prompt has as many as were written, so the
//! row says when, where and how it starts, and the box under the list says
//! the rest: the whole prompt, or every prompt of the session. That is what
//! is read before choosing, and what a list of first lines cannot show.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::clock::{self, Stamp};
use crate::history::{Browser, PromptRow, SessionRow, View};
use crate::text;
use crate::theme::Theme;

/// Widest the dialog is drawn, in columns: a prompt's first line and the
/// columns that place it.
const COLUMNS: u16 = 110;

/// Narrowest it is drawn: below this its columns crowd the prompt out.
const MIN_COLUMNS: u16 = 40;

/// Rows above the list: the views, the filter and a blank row.
const HEAD_ROWS: usize = 3;

/// The share of the rows under the head the list takes; the row under the
/// cursor, in full, has the rest.
const LIST_SHARE: (usize, usize) = (3, 5);

/// Columns between the parts of a row.
const GAP: &str = "  ";

/// How wide the age column is: `59m`, `23h`, `364d`.
const AGE_COLUMNS: usize = 4;

/// How wide the column naming a prompt's session is: `claude 2f6c1e10`.
const SESSION_COLUMNS: usize = 15;

/// What the filter row says before anything is typed into it.
const FILTER_EMPTY: &str = "type to filter";

/// Draws the history dialog over `body`, if it is open.
pub(crate) fn draw(frame: &mut Frame, body: Rect, app: &App, theme: &Theme) {
    let Some(browser) = app.browser() else {
        return;
    };
    let width = COLUMNS.min(body.width.saturating_sub(crate::ui::DIALOG_MARGIN * 2));
    if width < MIN_COLUMNS {
        return;
    }
    // A row each for the border and the shadow, and one of room, which the
    // dialog keeps inside the body.
    let rows = usize::from(body.height).saturating_sub(4);
    let text_width = usize::from(width).saturating_sub(crate::ui::DIALOG_INSET);
    let lines = match browser.view {
        View::Prompts => prompts(app, browser, (text_width, rows), theme),
        View::Sessions => sessions(app, browser, (text_width, rows), theme),
    };
    let footer = match browser.view {
        View::Prompts => " ↑↓ choose · Enter put in the prompt · Tab sessions · Esc close ",
        View::Sessions => " ↑↓ choose · Enter open · Tab prompts · Esc close ",
    };
    let inner = crate::ui::dialog(frame, body, (width, rows), ("History", footer), theme);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The dialog on its prompts.
fn prompts(
    app: &App,
    browser: &Browser,
    (width, rows): (usize, usize),
    theme: &Theme,
) -> Vec<Line<'static>> {
    let listed = app.prompt_rows();
    let mut lines = head(browser, listed.len(), width, theme);
    let (list_rows, detail_rows) = split(rows);
    let now = app.stamp();
    let shown: Vec<Line> = window(&listed, browser.at, list_rows)
        .map(|(at, row)| {
            let said = format!(
                "{:>AGE_COLUMNS$}{GAP}{}{GAP}{}",
                age(now, row.at),
                text::pad(
                    &row.session
                        .as_ref()
                        .map_or_else(|| "this session".to_owned(), |target| target.label()),
                    SESSION_COLUMNS
                ),
                one_line(&row.text),
            );
            list_row(&said, at == browser.at, width, theme)
        })
        .collect();
    lines.extend(empty_or(shown, app, listed.is_empty(), "no prompt", theme));
    pad(&mut lines, HEAD_ROWS + list_rows);
    lines.push(rule(width, theme));
    if let Some(row) = listed.get(browser.at) {
        lines.extend(prompt_detail(row, width, detail_rows, theme));
    }
    lines
}

/// The dialog on its sessions.
fn sessions(
    app: &App,
    browser: &Browser,
    (width, rows): (usize, usize),
    theme: &Theme,
) -> Vec<Line<'static>> {
    let listed = app.session_rows();
    let mut lines = head(browser, listed.len(), width, theme);
    let (list_rows, detail_rows) = split(rows);
    let now = app.stamp();
    let shown: Vec<Line> = window(&listed, browser.at, list_rows)
        .map(|(at, row)| {
            let said = format!(
                "{:>AGE_COLUMNS$}{GAP}{}{GAP}{:>9}{GAP}{}",
                age(now, row.last),
                text::pad(&name(row), SESSION_COLUMNS),
                count(row),
                row.first().map_or_else(|| "—".to_owned(), one_line),
            );
            list_row(&said, at == browser.at, width, theme)
        })
        .collect();
    lines.extend(empty_or(
        shown,
        app,
        listed.len() <= 1,
        "no other session",
        theme,
    ));
    pad(&mut lines, HEAD_ROWS + list_rows);
    lines.push(rule(width, theme));
    if let Some(row) = listed.get(browser.at) {
        lines.extend(session_detail(row, width, detail_rows, theme));
    }
    lines
}

/// The views, the one shown marked, how many rows the filter keeps, and the
/// filter.
fn head(browser: &Browser, count: usize, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let tab = |view: View, name: &'static str| {
        let style = match browser.view == view {
            true => Style::new().bg(theme.cursor_bg).fg(theme.cursor_fg).bold(),
            false => Style::new().fg(theme.dialog_fg),
        };
        Span::styled(format!(" {name} "), style)
    };
    let counted = format!("{count} listed");
    let tabs_width = " Prompts ".len() + 1 + " Sessions ".len();
    let room = width.saturating_sub(tabs_width + counted.len());
    let views = Line::from(vec![
        tab(View::Prompts, "Prompts"),
        Span::raw(" "),
        tab(View::Sessions, "Sessions"),
        Span::raw(" ".repeat(room)),
        Span::styled(counted, secondary(theme)),
    ]);
    let filter = match browser.query.is_empty() {
        true => Line::from(vec![
            Span::styled("filter ", secondary(theme)),
            Span::styled("▏", Style::new().fg(theme.dialog_fg)),
            Span::styled(FILTER_EMPTY, secondary(theme)),
        ]),
        false => Line::from(vec![
            Span::styled("filter ", secondary(theme)),
            Span::styled(
                text::truncate(&browser.query, width.saturating_sub(9)),
                Style::new().fg(theme.dialog_fg).bold(),
            ),
            Span::styled("▏", Style::new().fg(theme.dialog_fg)),
        ]),
    };
    vec![views, filter, Line::from("")]
}

/// The rows the list and the detail under it get of `rows`, after the head
/// and the rule between them.
fn split(rows: usize) -> (usize, usize) {
    let room = rows.saturating_sub(HEAD_ROWS + 1);
    let list = (room * LIST_SHARE.0 / LIST_SHARE.1).max(1);
    (list, room.saturating_sub(list))
}

/// The rows of `listed` that fit in `room`, with the one at `at` among them,
/// each with its place in the whole list.
fn window<T>(listed: &[T], at: usize, room: usize) -> impl Iterator<Item = (usize, &T)> {
    let first = at.saturating_sub(room.saturating_sub(1));
    listed.iter().enumerate().skip(first).take(room)
}

/// The rows shown, or where there are none, why: still being read, or
/// nothing the filter keeps.
fn empty_or(
    shown: Vec<Line<'static>>,
    app: &App,
    empty: bool,
    nothing: &str,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = shown;
    if !app.history_read() {
        lines.push(Line::styled(
            "reading the earlier sessions…",
            secondary(theme),
        ));
    } else if empty {
        lines.push(Line::styled(format!("{nothing} to list"), secondary(theme)));
    }
    if let Some(unread) = app.history_unread() {
        lines.push(Line::styled(unread.to_owned(), secondary(theme)));
    }
    lines
}

/// One row of the list, the cursor's in its colours.
fn list_row(said: &str, on_it: bool, width: usize, theme: &Theme) -> Line<'static> {
    let row = text::pad(&text::truncate(said, width), width);
    match on_it {
        true => Line::styled(
            row,
            Style::new().bg(theme.cursor_bg).fg(theme.cursor_fg).bold(),
        ),
        false => Line::styled(row, Style::new().fg(theme.dialog_fg)),
    }
}

/// The prompt under the cursor, whole, wrapped to the dialog.
fn prompt_detail(row: &PromptRow, width: usize, room: usize, theme: &Theme) -> Vec<Line<'static>> {
    let wrapped: Vec<String> = row
        .text
        .lines()
        .flat_map(|line| text::wrap(line, width))
        .collect();
    cut(wrapped, room, theme)
}

/// Every prompt of the session under the cursor, oldest first, one line
/// each; a `claude` session's first, which is all that is known of it until
/// it is opened.
fn session_detail(
    row: &SessionRow,
    width: usize,
    room: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let said: Vec<String> = match (row.prompts.is_empty(), row.first_prompt.as_deref()) {
        (false, _) => row
            .prompts
            .iter()
            .enumerate()
            .map(|(n, prompt)| {
                text::truncate(&format!("{:>3}  {}", n + 1, one_line(prompt)), width)
            })
            .collect(),
        (true, Some(first)) => vec![
            text::truncate(&format!("  1  {}", one_line(first)), width),
            "     the rest is read in when it is opened".to_owned(),
        ],
        (true, None) => vec!["nothing asked in it".to_owned()],
    };
    cut(said, room, theme)
}

/// `lines` cut to `room`, saying how many were left out.
fn cut(lines: Vec<String>, room: usize, theme: &Theme) -> Vec<Line<'static>> {
    let total = lines.len();
    if total <= room {
        return lines
            .into_iter()
            .map(|line| Line::styled(line, Style::new().fg(theme.dialog_fg)))
            .collect();
    }
    let kept = room.saturating_sub(1);
    let mut shown: Vec<Line> = lines
        .into_iter()
        .take(kept)
        .map(|line| Line::styled(line, Style::new().fg(theme.dialog_fg)))
        .collect();
    shown.push(Line::styled(
        format!("… {} more", total - kept),
        secondary(theme),
    ));
    shown
}

/// A line across the dialog between the list and the detail.
fn rule(width: usize, theme: &Theme) -> Line<'static> {
    Line::styled("─".repeat(width), secondary(theme))
}

/// Blank rows up to `rows`, so the rule under the list stays where it is
/// however many rows the filter keeps.
fn pad(lines: &mut Vec<Line<'static>>, rows: usize) {
    while lines.len() < rows {
        lines.push(Line::from(""));
    }
    lines.truncate(rows);
}

/// What the list calls a session.
fn name(row: &SessionRow) -> String {
    row.target
        .as_ref()
        .map_or_else(|| "this session".to_owned(), |target| target.label())
}

/// How many prompts a session holds, where that is known.
fn count(row: &SessionRow) -> String {
    match (row.prompts.len(), row.first_prompt.is_some()) {
        (0, true) => "—".to_owned(),
        (1, _) => "1 prompt".to_owned(),
        (n, _) => format!("{n} prompts"),
    }
}

/// How long before `now` a row is dated; nothing where either is unknown.
fn age(now: Option<Stamp>, at: Option<Stamp>) -> String {
    now.zip(at)
        .and_then(|(now, at)| now.since(at))
        .map(clock::ago)
        .unwrap_or_default()
}

/// A prompt on one line: its first, with a mark where more follows.
fn one_line(text: &str) -> String {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let first = lines.next().unwrap_or_default().trim().to_owned();
    match lines.next() {
        Some(_) => format!("{first} ⏎…"),
        None => first,
    }
}

/// The style of what a row says about itself rather than its prompt.
fn secondary(theme: &Theme) -> Style {
    Style::new().fg(theme.dialog_fg).add_modifier(Modifier::DIM)
}
