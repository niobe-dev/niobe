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
use crate::history::{Browser, Listed, PromptRow, SessionRow, View};
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
    // What the shell says of the key just pressed goes where the dialog's
    // keys are said: the bar it is said on elsewhere is under the dialog.
    let footer = match (app.hint(), browser.view) {
        (Some(hint), _) => format!(" {hint} "),
        (None, View::Prompts) => {
            " ↑↓ choose · Enter put in the prompt · Tab sessions · Esc close ".to_owned()
        }
        (None, View::Sessions) => " ↑↓ choose · Enter open · Tab prompts · Esc close ".to_owned(),
    };
    let inner = crate::ui::dialog(frame, body, (width, rows), ("History", &footer), theme);
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
        lines.extend(prompt_detail(
            row,
            app.prompt_counted(),
            (width, detail_rows),
            theme,
        ));
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
    // As wide as the widest count of every row, not only of those shown, so
    // the column stays put as the list scrolls.
    let counted = listed
        .iter()
        .map(|row| text::width(&count(row)))
        .max()
        .unwrap_or(0);
    let shown: Vec<Line> = window(&listed, browser.at, list_rows)
        .map(|(at, row)| {
            let said = format!(
                "{:>AGE_COLUMNS$}{GAP}{}{GAP}{:>counted$}{GAP}{}",
                age(now, row.last),
                text::pad(&name(row), SESSION_COLUMNS),
                count(row),
                row.caption().map_or_else(|| "—".to_owned(), one_line),
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
fn window<T>(listed: &Listed<T>, at: usize, room: usize) -> impl Iterator<Item = (usize, &T)> {
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
    let row = text::pad_owned(text::truncate(said, width), width);
    match on_it {
        true => Line::styled(
            row,
            Style::new().bg(theme.cursor_bg).fg(theme.cursor_fg).bold(),
        ),
        false => Line::styled(row, Style::new().fg(theme.dialog_fg)),
    }
}

/// The prompt under the cursor, whole, wrapped to the dialog with every space
/// it was sent with, so an indented list or a pasted block reads as sent.
///
/// Only the rows the box can show are laid out, and the rest counted once
/// for the prompt: one read in from a `claude` session can be a megabyte,
/// and wrapped whole on every frame it held the shell up for three of them.
fn prompt_detail(
    row: &PromptRow,
    counted: &Counted,
    (width, room): (usize, usize),
    theme: &Theme,
) -> Vec<Line<'static>> {
    // One row past the room, so that `cut` can tell a prompt that fills the
    // box from one it has to cut.
    let wanted = room.saturating_add(1);
    let mut shown = Vec::new();
    for line in row.text.lines() {
        let left = wanted.saturating_sub(shown.len());
        if left == 0 {
            break;
        }
        match line.is_empty() {
            true => shown.push(String::new()),
            false => shown.extend(text::wrap_exact_first(line, width, left)),
        }
    }
    cut(shown, counted.rows(&row.text, width), room, theme)
}

/// How many rows the prompt under the history dialog's cursor wraps into,
/// kept with the prompt and the width it was counted at.
///
/// Counting them measures every character of the prompt, which for a
/// megabyte in a script other than ASCII takes two frames; the frames after
/// the first that draw the same prompt compare it instead, which is a copy's
/// worth of work.
#[derive(Debug, Default)]
pub(crate) struct Counted(std::cell::RefCell<Option<(String, usize, usize)>>);

impl Counted {
    /// The rows `prompt` wraps into at `width`, as [`prompt_detail`] wraps
    /// it: a row for each empty line, and each other line as
    /// [`text::wrap_exact`] cuts it.
    fn rows(&self, prompt: &str, width: usize) -> usize {
        let mut last = self.0.borrow_mut();
        if let Some((text, at, rows)) = last.as_ref()
            && *at == width
            && text == prompt
        {
            return *rows;
        }
        let rows = prompt
            .lines()
            .map(|line| match line.is_empty() {
                true => 1,
                false => text::wrap_exact_rows(line, width),
            })
            .fold(0, usize::saturating_add);
        *last = Some((prompt.to_owned(), width, rows));
        rows
    }
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
    let total = said.len();
    cut(said, total, room, theme)
}

/// `lines`, the first of `total`, cut to `room`, saying how many were left
/// out. `lines` holds every one where `total` fits in `room`, and at least
/// the `room` less one shown where it does not.
fn cut(lines: Vec<String>, total: usize, room: usize, theme: &Theme) -> Vec<Line<'static>> {
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

/// What the list calls a session, marked where another niobe has it open.
fn name(row: &SessionRow) -> String {
    let label = row
        .target
        .as_ref()
        .map_or_else(|| "this session".to_owned(), |target| target.label());
    match row.open_elsewhere {
        true => format!("{label} in use"),
        false => label,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A shell whose history has been read: `sessions` earlier sessions,
    /// each recorded under its id.
    fn read(sessions: &[&str]) -> App {
        let mut app = App::new(crate::app::Repo::default()).remembers();
        app.take_history_request();
        app.set_past(crate::history::Past {
            prompts: Vec::new(),
            sessions: sessions
                .iter()
                .map(|id| crate::history::PastSession {
                    target: crate::history::Target::Recorded((*id).to_owned()),
                    last: None,
                    first_prompt: None,
                    title: None,
                    open_elsewhere: false,
                })
                .collect(),
            unread: None,
        });
        app
    }

    fn sessions_on(app: &App, at: usize, rows: usize) -> Vec<String> {
        let browser = Browser {
            view: View::Sessions,
            query: String::new(),
            at,
        };
        said(&sessions(app, &browser, (60, rows), &Theme::default()))
    }

    /// The session open now is always listed, so a list of it alone has no
    /// other to offer and says so.
    #[test]
    fn a_session_list_of_this_session_alone_says_there_is_no_other() {
        let drawn = sessions_on(&read(&[]), 0, 20);
        assert!(
            drawn.iter().any(|row| row.contains("this session")),
            "{drawn:?}"
        );
        assert!(
            drawn
                .iter()
                .any(|row| row.contains("no other session to list")),
            "{drawn:?}"
        );

        let drawn = sessions_on(&read(&["101"]), 0, 20);
        assert!(
            !drawn.iter().any(|row| row.contains("no other session")),
            "{drawn:?}"
        );
    }

    /// With the cursor below the rows that fit, the list scrolls so its row
    /// is the last drawn, and the rows above it go first.
    #[test]
    fn a_list_longer_than_its_rows_scrolls_to_keep_the_cursor_drawn() {
        let ids: Vec<String> = (101..=120).map(|id| id.to_string()).collect();
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let app = read(&ids);
        // 13 rows: 3 for the head and 1 for the rule leave 9, of which the
        // list takes 5.
        let drawn = sessions_on(&app, 10, 13);
        let list: Vec<&String> = drawn[HEAD_ROWS..HEAD_ROWS + 5].iter().collect();
        assert!(list[4].contains("#110"), "{drawn:?}");
        assert!(list[0].contains("#106"), "{drawn:?}");
        assert!(!drawn.iter().any(|row| row.contains("#105 ")), "{drawn:?}");
    }

    #[test]
    fn lines_that_fit_are_shown_whole_and_more_are_cut_with_a_count() {
        let lines = |n: usize| (1..=n).map(|n| format!("line {n}")).collect::<Vec<_>>();
        assert_eq!(
            said(&cut(lines(3), 3, 3, &Theme::default())),
            ["line 1", "line 2", "line 3"]
        );
        assert_eq!(
            said(&cut(lines(4), 4, 3, &Theme::default())),
            ["line 1", "line 2", "… 2 more"]
        );
    }

    #[test]
    fn a_prompt_shown_whole_keeps_its_indentation_and_runs_of_spaces() {
        let row = PromptRow {
            text: "- top\n  - nested item\n    key   =   \"v\"\n\tx".to_owned(),
            at: None,
            session: None,
        };
        assert_eq!(
            said(&prompt_detail(
                &row,
                &Counted::default(),
                (40, 10),
                &Theme::default()
            )),
            ["- top", "  - nested item", "    key   =   \"v\"", "    x"]
        );
    }

    /// A prompt longer than the box is cut with a count of the rows left out,
    /// worked out from the prompt and not from the rows laid out, and a
    /// prompt drawn after it is counted for itself.
    #[test]
    fn a_prompt_longer_than_its_box_says_how_many_rows_are_left_out() {
        let prompt = |text: &str| PromptRow {
            text: text.to_owned(),
            at: None,
            session: None,
        };
        let counted = Counted::default();
        // 26 letters at 10 columns are 3 rows, then an empty line, then 25
        // characters of which each is two cells: 5 rows of 5. Nine rows.
        let long = prompt(&format!(
            "{}\n\n{}",
            "abcdefghijklmnopqrstuvwxyz",
            "語".repeat(25)
        ));
        assert_eq!(
            said(&prompt_detail(&long, &counted, (10, 4), &Theme::default())),
            ["abcdefghij", "klmnopqrst", "uvwxyz", "… 6 more"]
        );
        assert_eq!(
            said(&prompt_detail(&long, &counted, (10, 9), &Theme::default())).len(),
            9,
            "nine rows fill a box of nine without a cut"
        );

        let other = prompt(&format!(
            "{}\n\n{}",
            "abcdefghijklmnopqrstuvwxyz",
            "x".repeat(75)
        ));
        assert_eq!(
            said(&prompt_detail(&other, &counted, (10, 4), &Theme::default())),
            ["abcdefghij", "klmnopqrst", "uvwxyz", "… 9 more"],
            "75 cells at 10 columns are 8 rows, not the 5 the prompt before made"
        );
    }
}
