// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Drawing the three regions: menu bar, panes, F-key bar.
//!
//! There is no status line. What one carried is spread across the menu row —
//! the session's identity, what it is doing and the time of day — and the
//! panes that own each figure, so the body has the rows back.
//!
//! Every number on screen comes off [`App::session`], which is a fold over
//! events and nothing else. Where the fold has nothing to say the pane says so
//! — an em dash and, where it is not obvious, a note that the feature is not
//! implemented yet. Nothing here invents a figure, because a figure nobody
//! measured is indistinguishable from one that was, and that is the product
//! gone.
//!
//! Neither pane shows a cost per edit or per tool: that needs spend attributed
//! to individual calls, which the ledger does not do yet. The Activity pane
//! shows the tool mix instead, which is measured.

use std::collections::BTreeMap;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Widget, Wrap,
};

use niobe_core::event::{Billing, Context, UsageWindow};
use niobe_core::session::{SessionState, ToolTotals, Totals};

use crate::app::{
    Activity, Answer, App, Ask, AskFocus, Change, Entry, EntryKind, Focus, Pane, Picker, Purpose,
    Section, SelectedProfile, SubAgent, tool_label,
};
use crate::calls::Detail;
use crate::clock::{self, Stamp};
use crate::meter::meter;
use crate::prices::Prices;
use crate::text;
use crate::theme::Theme;
use crate::tree;
use crate::trust;
use crate::usage;

/// Smallest terminal the shell draws in, as (columns, rows).
pub const MIN_SIZE: (u16, u16) = (80, 24);

/// The width at which the right stack fits beside the session pane. Below it
/// the session pane takes the whole body: three more panes squeezed into forty
/// columns each is less readable than the transcript they were taken from.
pub const WIDE_COLUMNS: u16 = 100;

/// Columns of desktop the wide layout leaves at each edge of the body and
/// between its panes.
const DESKTOP_MARGIN: u16 = 1;

/// Columns the transcript gives to an entry's glyph.
const GUTTER: usize = 2;

/// Widest a question in the transcript is drawn, in columns. Wide enough for
/// a shell command that has a path in it, and capped rather than filling the
/// pane, so on a wide screen it reads as an interruption in the transcript
/// rather than as one more pane.
const ASK_COLUMNS: usize = 72;

/// Columns a question's frame and padding take from its width: a border and a
/// column of room on each side.
const ASK_INSET: usize = 4;

/// Columns of margin a dialog leaves on each side of a narrow screen.
pub(crate) const DIALOG_MARGIN: u16 = 4;

/// Widest the model list is drawn, in columns. A model id is a word or two, so
/// the list is narrow enough to read as a list rather than as a pane.
const PICK_COLUMNS: u16 = 44;

/// What marks the model the session is on, and the one the cursor is over.
const PICK_CURSOR: &str = "› ";
const PICK_CURRENT: &str = "· ";

/// The share of a budget at which the Usage pane starts saying so in the
/// colour it uses for anything waiting on the operator. The same fraction the
/// transcript warning uses, so the pane and the warning agree.
const BUDGET_SHOWN_HOT: f64 = 0.8;

/// The share of a plan's usage window from which its meter is drawn in the
/// theme's warning colour.
const WINDOW_RUNNING_LOW: f64 = 0.5;

/// The share of a plan's usage window from which its meter is drawn in the
/// theme's error colour: what is left may not see a long turn through.
const WINDOW_RUN_OUT: f64 = 0.9;

/// Columns between the menus and the session's identity, and between one
/// segment of that identity and the next.
const MENU_GAP: usize = 2;

/// Draws one frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    draw_frame(frame, app);
    strip_direction_marks(frame.buffer_mut());
}

/// The characters that reorder the text around them rather than show
/// anything: the embeddings and overrides U+202A–U+202E, the isolates
/// U+2066–U+2069 and the marks U+200E and U+200F.
fn is_direction_mark(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}')
}

/// Takes every direction mark out of what was drawn.
///
/// ratatui drops control characters as it lays text out but keeps a
/// zero-width character with the cell before it, so a right-to-left override
/// in a title, a repository's name or anything a backend said reaches the
/// terminal and reverses what is drawn after it. Done once over the finished
/// frame rather than at every place text is drawn, so that no place can be
/// missed.
fn strip_direction_marks(buffer: &mut ratatui::buffer::Buffer) {
    for cell in &mut buffer.content {
        if cell.symbol().chars().any(is_direction_mark) {
            let kept: String = cell
                .symbol()
                .chars()
                .filter(|c| !is_direction_mark(*c))
                .collect();
            cell.set_symbol(if kept.is_empty() { " " } else { &kept });
        }
    }
}

fn draw_frame(frame: &mut Frame, app: &mut App) {
    let theme = *app.theme();
    let area = frame.area();

    let too_small = area.width < MIN_SIZE.0 || area.height < MIN_SIZE.1;
    app.drew_too_small(too_small);
    if too_small {
        draw_too_small(frame, area, app.hint(), &theme);
        return;
    }

    frame.render_widget(
        Block::new().style(Style::new().bg(theme.pane_bg).fg(theme.fg)),
        area,
    );

    let [menu, body, fkeys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_menu(frame, menu, app, &theme);
    draw_body(frame, body, app, &theme);
    draw_fkeys(frame, fkeys, app, &theme);
    app.drew_bars(menu, fkeys);

    // Last, and over the body: the list is something the operator opened, and
    // nothing drawn afterwards may cover it. A permission prompt is not drawn
    // here — it is in the transcript, under the work that led to it.
    if let Some(picker) = app.picking() {
        draw_pick(frame, body, picker, app.picked(), &theme);
    }
    if app.sheet().is_some() {
        draw_sheet(frame, body, app, &theme);
    }
    crate::browse::draw(frame, body, app, &theme);
    // The open menu hangs from the bar over everything, a list included: it
    // was opened last.
    let list = app
        .menu()
        .map(|open| draw_menu_list(frame, area, open, app, &theme));
    app.drew_menu_list(list);
    if let Some(asking) = app.trusting() {
        draw_trust(frame, body, asking, app.hint(), &theme);
    }
}

/// What a list says on its top edge, its bottom edge and under its rows:
/// what it is for, and the three keys that work.
///
/// The model list says when a choice takes effect. A switch applies from the
/// next turn, and a list that did not say so would read as though the reply
/// being written were already coming from the new model.
fn pick_words(purpose: Purpose) -> (&'static str, &'static str, &'static str) {
    match purpose {
        Purpose::Model => (
            "Model",
            " applied from the next turn ",
            "↑↓ choose · Enter switch · Esc keep this one",
        ),
        Purpose::Effort => (
            "Effort",
            " sent to the backend as /effort ",
            "↑↓ choose · Enter set · Esc leave it",
        ),
        Purpose::Theme => ("Theme", "", "↑↓ choose · Enter switch · Esc keep this one"),
    }
}

/// A list the operator opened: what it offers, which one is in force where
/// that is known, and the keys that work.
fn draw_pick(frame: &mut Frame, body: Rect, picker: &Picker, current: Option<&str>, theme: &Theme) {
    let width = PICK_COLUMNS.min(body.width.saturating_sub(DIALOG_MARGIN * 2));
    if width < 20 {
        return;
    }

    let (title, footer, keys) = pick_words(picker.purpose);
    let text_width = usize::from(width).saturating_sub(DIALOG_INSET);
    let mut lines: Vec<Line> = vec![Line::from("")];
    for (i, option) in picker.options.iter().enumerate() {
        let on_it = i == picker.at;
        let marker = match (on_it, current == Some(option.as_str())) {
            (true, _) => PICK_CURSOR,
            (false, true) => PICK_CURRENT,
            (false, false) => "  ",
        };
        let shown = match picker.purpose {
            Purpose::Theme => option.to_lowercase(),
            Purpose::Model | Purpose::Effort => option.clone(),
        };
        let room = text_width.saturating_sub(2);
        let row = format!("{marker}{}", text::pad(&text::truncate(&shown, room), room));
        lines.push(match on_it {
            true => {
                Line::from(row).style(Style::new().bg(theme.cursor_bg).fg(theme.cursor_fg).bold())
            }
            false => Line::from(row).style(Style::new().fg(theme.dialog_fg)),
        });
    }
    lines.push(Line::from(""));
    // Wrapped to the box, which is sized for what it offers: cut at its edge
    // the last key would read as a different one.
    lines.extend(
        text::wrap(keys, text_width)
            .into_iter()
            .map(|line| Line::from(line).style(Style::new().fg(theme.dialog_fg))),
    );

    let inner = dialog(frame, body, (width, lines.len()), (title, footer), theme);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Widest a sheet is drawn, in columns: a paragraph's measure, with room
/// for a config file's path on one line.
const SHEET_COLUMNS: u16 = 80;

/// The open sheet: its rows wrapped to its width, scrolled to where the
/// operator put it, and the keys that work on its bottom edge.
fn draw_sheet(frame: &mut Frame, body: Rect, app: &mut App, theme: &Theme) {
    let Some(sheet) = app.sheet() else {
        return;
    };
    let width = SHEET_COLUMNS.min(body.width.saturating_sub(DIALOG_MARGIN * 2));
    let text_width = usize::from(width).saturating_sub(DIALOG_INSET);
    let wrapped: Vec<String> = sheet
        .rows
        .iter()
        // A row that fits is drawn as written, so the columns its spaces
        // line up are kept; only a longer one is wrapped.
        .flat_map(|row| match text::width(row) <= text_width {
            true => vec![row.clone()],
            false => text::wrap(row, text_width),
        })
        .collect();
    // A row of room above and below, and one each for the border and the
    // shadow, which the dialog keeps inside the body.
    let room = usize::from(body.height).saturating_sub(5);
    let max_scroll = wrapped.len().saturating_sub(room);
    let scroll = sheet.scroll.min(max_scroll);
    let footer = match (&sheet.link, max_scroll > 0) {
        (Some(_), true) => " ↑↓ scroll · o open · Esc close ",
        (Some(_), false) => " o open · Esc close ",
        (None, true) => " ↑↓ scroll · Esc close ",
        (None, false) => " Esc close ",
    };
    let title = sheet.title.clone();

    let mut lines = vec![Line::from("")];
    lines.extend(
        wrapped
            .into_iter()
            .skip(scroll)
            .take(room)
            .map(|line| Line::from(line).style(Style::new().fg(theme.dialog_fg))),
    );
    lines.push(Line::from(""));

    let inner = dialog(frame, body, (width, lines.len()), (&title, footer), theme);
    frame.render_widget(Paragraph::new(lines), inner);
    app.measured_sheet(max_scroll);
}

/// The open menu's list, hung from its name on the bar and cast on what is
/// under it, and where it was drawn.
///
/// An item the session cannot do is dimmed rather than left out: the menu is
/// the catalogue of what the shell can be asked, and choosing one says why it
/// cannot. A pane the View menu shows and hides says which it is.
fn draw_menu_list(
    frame: &mut Frame,
    screen: Rect,
    open: crate::menu::Open,
    app: &App,
    theme: &Theme,
) -> Rect {
    let menu = open.menu();
    let rows: Vec<(String, String)> = menu
        .items
        .iter()
        .map(|item| {
            let keys = match item.action {
                crate::menu::Action::Pane(pane) => match app.shows(pane) {
                    true => "shown",
                    false => "hidden",
                },
                _ => item.keys,
            };
            (item.label.to_owned(), keys.to_owned())
        })
        .collect();
    let label_width = rows
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    let keys_width = rows
        .iter()
        .map(|(_, keys)| keys.chars().count())
        .max()
        .unwrap_or(0);
    // A column of room either side of the text, and four between an item and
    // its keys, inside the border.
    let inner_width = label_width + keys_width + MENU_KEYS_GAP + 2;
    let width = u16::try_from(inner_width + 2)
        .unwrap_or(u16::MAX)
        .min(screen.width);
    let height = u16::try_from(rows.len() + 2)
        .unwrap_or(u16::MAX)
        .min(screen.height.saturating_sub(1));
    let (start, _) = crate::menu::title_columns()
        .get(open.menu)
        .copied()
        .unwrap_or((0, 0));
    let x = start.min(screen.right().saturating_sub(width));
    let area = Rect::new(x, screen.y.saturating_add(1), width, height);

    cast_shadow(frame, area, screen, theme);
    let bar = Style::new().bg(theme.menu_bg).fg(theme.menu_fg);
    let block = Block::bordered()
        .border_type(theme.border)
        .border_style(bar)
        .style(bar);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let hot = Style::new().fg(theme.hot).bold().underlined();
    let lines: Vec<Line> = menu
        .items
        .iter()
        .zip(rows)
        .enumerate()
        .map(|(at, (item, (label, keys)))| {
            let on_it = at == open.item;
            let able = app.can(item.action);
            let base = match (on_it, able) {
                (true, _) => Style::new().bg(theme.cursor_bg).fg(theme.cursor_fg),
                (false, true) => bar,
                (false, false) => bar.fg(theme.dim),
            };
            let mut spans = vec![Span::styled(" ", base)];
            for (i, c) in label.chars().enumerate() {
                let style = match i == item.hot && able && !on_it {
                    true => base.patch(hot),
                    false if i == item.hot => base.underlined(),
                    false => base,
                };
                spans.push(Span::styled(c.to_string(), style));
            }
            let pad = inner_width.saturating_sub(label.chars().count() + keys.chars().count() + 2);
            spans.push(Span::styled(" ".repeat(pad), base));
            spans.push(Span::styled(keys, base.add_modifier(Modifier::DIM)));
            spans.push(Span::styled(" ", base));
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
    area
}

/// Columns between a menu item and the keys that do the same.
const MENU_KEYS_GAP: usize = 4;

/// Widest the trust question is drawn, in columns: room for a permission
/// rule or an environment variable and its value on one line.
const TRUST_COLUMNS: u16 = 84;

/// What the trust question says above the rows of what the file sets.
const TRUST_SAYS: &str = "It sets what only a config you trust may:";

/// What it says under them: what each answer leaves in force.
const TRUST_UNTIL: &str =
    "Trusted, it stays in force until the file changes. Without it, none of the above is.";

/// The keys the trust question answers to, on its bottom edge.
const TRUST_KEYS: &str = " ↑↓ choose · Enter answer · Esc no · Ctrl+C quit ";

/// Columns between what a row of the trust question names and what it is set
/// to, and the most the name may take.
const TRUST_GAP: usize = 2;
const TRUST_NAME_COLUMNS: usize = 16;

/// The question whether to trust the repository's config: the file, what
/// trusting it puts in force, and the two answers.
///
/// The answers are always drawn. A file that sets more than fits keeps the
/// rows that do and says how many it left out — cutting the answers instead
/// would ask a question nobody can answer. `hint` is what the shell has to say
/// about a key it held back, said where the operator is looking.
fn draw_trust(
    frame: &mut Frame,
    body: Rect,
    asking: &trust::Asking,
    hint: Option<&str>,
    theme: &Theme,
) {
    let width = TRUST_COLUMNS.min(body.width.saturating_sub(DIALOG_MARGIN * 2));
    let text_width = usize::from(width).saturating_sub(DIALOG_INSET);
    let plain = Style::new().fg(theme.dialog_fg);
    let wrapped = |text: &str, style: Style| -> Vec<Line<'static>> {
        text::wrap(text, text_width)
            .into_iter()
            .map(|line| Line::from(line).style(style))
            .collect()
    };

    let mut head = vec![Line::from("")];
    head.extend(wrapped(&asking.question.path, plain.bold()));
    head.push(Line::from(""));
    head.extend(wrapped(TRUST_SAYS, plain));
    head.push(Line::from(""));

    let mut tail = vec![Line::from("")];
    match hint {
        Some(hint) => tail.extend(wrapped(hint, Style::new().fg(theme.hot).bold())),
        None => tail.extend(wrapped(TRUST_UNTIL, plain)),
    }
    tail.push(Line::from(""));
    for (i, answer) in trust::Answer::OFFERED.into_iter().enumerate() {
        let on_it = i == asking.at;
        let marker = if on_it { PICK_CURSOR } else { "  " };
        let row = format!(
            "{marker}{}",
            text::pad(
                &format!("{}. {}", i + 1, answer.label()),
                text_width.saturating_sub(2)
            )
        );
        tail.push(match on_it {
            true => {
                Line::from(row).style(Style::new().bg(theme.cursor_bg).fg(theme.cursor_fg).bold())
            }
            false => Line::from(row).style(plain),
        });
    }
    tail.push(Line::from(""));

    // A border above and below, and the row the shadow falls on.
    let room = usize::from(body.height)
        .saturating_sub(3)
        .saturating_sub(head.len() + tail.len());
    let grants = trust_grants(&asking.question.grants, text_width, room, theme);

    let mut lines = head;
    lines.extend(grants);
    lines.extend(tail);
    let inner = dialog(
        frame,
        body,
        (width, lines.len()),
        ("Trust this repository's config?", TRUST_KEYS),
        theme,
    );
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The rows of what a config sets, in `columns`, the name of each in a column
/// of its own and its value wrapped beside it, in no more than `room` lines.
///
/// Rows are kept whole: half a permission rule reads as a different rule.
/// What is left out is counted on the last line.
fn trust_grants(
    grants: &[(String, String)],
    columns: usize,
    room: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let name_width = grants
        .iter()
        .map(|(name, _)| text::width(name))
        .max()
        .unwrap_or(0)
        .min(TRUST_NAME_COLUMNS);
    let value_width = columns.saturating_sub(name_width + TRUST_GAP).max(1);
    let name_style = Style::new().fg(theme.dialog_fg).bold();
    let value_style = Style::new().fg(theme.dialog_fg);

    let rows: Vec<Vec<Line<'static>>> = grants
        .iter()
        .map(|(name, value)| {
            text::wrap(value, value_width)
                .into_iter()
                .enumerate()
                .map(|(i, part)| {
                    let name = match i {
                        0 => text::truncate(name, name_width),
                        _ => String::new(),
                    };
                    Line::from(vec![
                        Span::styled(text::pad(&name, name_width + TRUST_GAP), name_style),
                        Span::styled(part, value_style),
                    ])
                })
                .collect()
        })
        .collect();
    if rows.iter().map(Vec::len).sum::<usize>() <= room {
        return rows.into_iter().flatten().collect();
    }

    // The last line the room has goes to the count of what is left out.
    let mut lines = Vec::new();
    let mut kept = 0;
    for row in &rows {
        if lines.len() + row.len() >= room {
            break;
        }
        lines.extend(row.iter().cloned());
        kept += 1;
    }
    if room > 0 {
        lines.push(
            Line::from(format!(
                "… and {} more; the file has them all",
                rows.len() - kept
            ))
            .style(value_style.italic()),
        );
    }
    lines
}

/// Columns a dialog's frame and padding take from its width: a border and two
/// columns of padding on each side.
pub(crate) const DIALOG_INSET: usize = 6;

/// Draws a dialog's frame, centred in `body` over whatever is there, with its
/// shadow, and returns the area inside it. `size` is the width and the number
/// of lines it will hold; `titles` are the text on its top and bottom edges.
pub(crate) fn dialog(
    frame: &mut Frame,
    body: Rect,
    size: (u16, usize),
    titles: (&str, &str),
    theme: &Theme,
) -> Rect {
    let (width, lines) = size;
    let (title, footer) = titles;
    let height = u16::try_from(lines + 2)
        .unwrap_or(u16::MAX)
        .min(body.height.saturating_sub(1));
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(body);
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);

    cast_shadow(frame, area, body, theme);

    let frame_style = Style::new().fg(theme.dialog_frame).bg(theme.dialog_bg);
    let block = Block::bordered()
        .border_type(theme.border_focus)
        .border_style(frame_style)
        .style(Style::new().bg(theme.dialog_bg).fg(theme.dialog_fg))
        .padding(Padding::horizontal(2))
        .title_top(
            Line::from(format!(" {title} "))
                .style(frame_style.bold())
                .centered(),
        )
        .title_bottom(Line::from(footer.to_owned()).style(frame_style).centered());
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    inner
}

/// Darkens the two columns right of `area` and the row under it, offset by
/// one, the way a dialog in Turbo Vision stands off the screen, within
/// `bounds`. What is under the shadow keeps its characters, so the transcript
/// still reads through it; a blank cell is shaded, because in a theme whose
/// panes are already black a shadow drawn in colour alone is not there.
fn cast_shadow(frame: &mut Frame, area: Rect, bounds: Rect, theme: &Theme) {
    let style = Style::new().bg(theme.shadow).fg(theme.dim);
    let right = Rect::new(area.right(), area.y.saturating_add(1), 2, area.height);
    let below = Rect::new(area.x.saturating_add(2), area.bottom(), area.width, 1);
    let buffer = frame.buffer_mut();
    for strip in [right, below] {
        for at in strip.intersection(bounds).positions() {
            if let Some(cell) = buffer.cell_mut(at) {
                if cell.symbol() == " " {
                    cell.set_char(SHADE);
                }
                cell.set_style(style);
            }
        }
    }
}

/// What a blank cell under a shadow is drawn as.
const SHADE: char = '░';

/// What the shell says when it has fewer than eighty by twenty-four to draw in.
///
/// Drawing the four regions anyway would produce panes one row high with their
/// borders overlapping their contents, which reads as a broken program rather
/// than a small window. `hint` is what the shell has to say about a key it
/// held back: a question waiting is not drawn here, and a key pressed at it
/// is answered with why it was not taken.
fn draw_too_small(frame: &mut Frame, area: Rect, hint: Option<&str>, theme: &Theme) {
    let (columns, rows) = MIN_SIZE;
    let mut lines = vec![
        Line::from("niobe").style(Style::new().fg(theme.hot).bold()),
        Line::from(format!("needs {columns}×{rows}")),
        Line::from(format!("this window is {}×{}", area.width, area.height))
            .style(Style::new().fg(theme.dim)),
    ];
    if let Some(hint) = hint {
        lines.push(Line::from(""));
        lines.push(Line::from(hint.to_owned()).style(Style::new().fg(theme.hot).bold()));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .alignment(Alignment::Center)
            .style(Style::new().bg(theme.pane_bg).fg(theme.fg)),
        area,
    );
}

/// The menu row: the menus on the left, and on the right what the session
/// is, what it is doing and the time of day.
///
/// The theme is not named here. The View menu is where a palette is changed,
/// and a row that says which one is on says nothing the screen does not
/// already show.
fn draw_menu(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let bar = Style::new().bg(theme.menu_bg).fg(theme.menu_fg);
    let left = Line::from(menu_spans(app.menu().map(|open| open.menu), theme));

    let room = usize::from(area.width).saturating_sub(left.width() + MENU_GAP);
    let identity = fitted(identity_segments(app, theme), room);

    frame.render_widget(Paragraph::new(left).style(bar), area);
    if !identity.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(segment_spans(identity)))
                .alignment(Alignment::Right)
                .style(bar),
            area,
        );
    }
}

/// The menus, each with its hot key accented and underlined, and the one
/// that is open, if one is, drawn as a block.
///
/// A terminal with no underline drops the underline and keeps the colour, so
/// the hot key is still marked on one that has only the sixteen attributes.
/// Each name has a column either side, which is where
/// [`crate::menu::title_columns`] says it is.
fn menu_spans(open: Option<usize>, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (at, menu) in crate::menu::MENUS.iter().enumerate() {
        let plain = match open == Some(at) {
            true => Style::new().fg(theme.menu_bg).bg(theme.menu_fg),
            false => Style::new().fg(theme.menu_fg),
        };
        let mut chars = menu.name.chars();
        let first = chars.next().unwrap_or(' ');
        spans.push(Span::styled(" ", plain));
        spans.push(Span::styled(
            first.to_string(),
            plain.fg(theme.hot).bold().underlined(),
        ));
        spans.push(Span::styled(chars.as_str().to_owned(), plain));
        spans.push(Span::styled(" ", plain));
    }
    spans
}

/// One thing the menu row says about the session.
///
/// Kept as text rather than drawn where it is built, so the row can drop whole
/// segments when the terminal narrows and so a test can read what it says
/// without reading a frame.
#[derive(Debug, Clone, PartialEq)]
struct Segment {
    text: String,
    style: Style,
}

/// What the session is, what it is doing, and what time it is — in the order
/// the row gives them up, which is right to left.
///
/// Errors come first because they are the last thing to be dropped: a session
/// that hit errors says so wherever the operator is looking, without opening
/// a pane. The mock has no error count, and a count with nowhere to be read is
/// worse than a row that is one segment longer when something went wrong.
fn identity_segments(app: &App, theme: &Theme) -> Vec<Segment> {
    let mut segments = Vec::new();

    let errors = app.session().errors();
    if errors > 0 {
        segments.push(Segment {
            text: match errors {
                1 => "\u{26a0} 1 error".to_owned(),
                n => format!("\u{26a0} {n} errors"),
            },
            style: Style::new().fg(theme.del).bold(),
        });
    }

    let (model, under) = identity(app.session(), app.profile());
    if let Some(model) = model {
        segments.push(Segment {
            text: model,
            style: Style::new().fg(theme.menu_fg).bold(),
        });
    }
    if let Some(under) = under {
        segments.push(Segment {
            text: under,
            style: Style::new().fg(theme.menu_fg).dim(),
        });
    }

    segments.push(Segment {
        text: state_label(app),
        style: Style::new().fg(theme.hot),
    });

    if let Some(clock) = app.clock() {
        segments.push(Segment {
            text: clock.to_string(),
            style: Style::new().fg(theme.menu_fg).dim(),
        });
    }

    segments
}

/// What the session is doing, which is not always a turn.
///
/// A shell with nothing listening, and one whose subprocess has started and
/// not yet said anything, are both states the operator has to be able to tell
/// from a session that is merely quiet: a prompt typed into either goes
/// nowhere it will be answered from.
fn state_label(app: &App) -> String {
    let pulse = app.pulse();
    // A turn that is running settles it: the session is working and nothing
    // else is truer — unless the backend has not said a word since it
    // started, which is a turn waiting on an answer that may never come.
    if pulse.working {
        return match (app.heard(), pulse.since) {
            (false, Some(since)) => {
                format!("\u{25cf} no answer yet {}", clock::spent(since))
            }
            (false, None) => "\u{25cf} no answer yet".to_owned(),
            (true, _) => pulse_label(pulse),
        };
    }
    if !app.is_attached() {
        return "\u{25cb} not attached".to_owned();
    }
    // A backend that ended is not starting, however little it said first.
    if app.session().fatal_error().is_some() {
        return "\u{25cb} ended".to_owned();
    }
    if app.session().meta().is_none() {
        return "\u{25cb} starting".to_owned();
    }
    pulse_label(pulse)
}

/// `● working 38s` while a turn is running, `○ idle 1m 12s` otherwise: a glyph
/// that fills when there is work, the word for it, and how long it has been
/// that way.
///
/// The duration is left off until the event loop has handed a clock in. How
/// long a session has been idle is a measurement, and a shell that has not
/// been told the time has not made it.
fn pulse_label(pulse: crate::app::Pulse) -> String {
    let (glyph, word) = match pulse.working {
        true => ("\u{25cf}", "working"),
        false => ("\u{25cb}", "idle"),
    };
    match pulse.since {
        Some(since) => format!("{glyph} {word} {}", clock::spent(since)),
        None => format!("{glyph} {word}"),
    }
}

/// The segments that fit in `room` columns, whole ones only.
///
/// Whole segments give way, rightmost first: half a model id, or a clock
/// missing a digit, reads as a different figure, and a figure that is not what
/// was measured is the one thing the shell never draws.
fn fitted(mut segments: Vec<Segment>, room: usize) -> Vec<Segment> {
    while !segments.is_empty() && segments_width(&segments) > room {
        segments.pop();
    }
    segments
}

/// The columns a group of segments takes, gaps included.
fn segments_width(segments: &[Segment]) -> usize {
    let text: usize = segments.iter().map(|s| text::width(&s.text)).sum();
    // A gap between each pair, and one column before the border.
    text + segments.len() * MENU_GAP
}

fn segment_spans(segments: Vec<Segment>) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(segments.len() * 2 + 1);
    for segment in segments {
        spans.push(Span::styled(segment.text, segment.style));
        spans.push(Span::raw(" ".repeat(MENU_GAP)));
    }
    spans
}

fn draw_body(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    use crate::menu::SidePane;

    let shown: Vec<SidePane> = [SidePane::Usage, SidePane::Changes, SidePane::Activity]
        .into_iter()
        .filter(|pane| app.shows(*pane))
        .collect();
    if area.width < WIDE_COLUMNS || shown.is_empty() {
        app.right_stack_hidden();
        draw_session(frame, area, false, app, theme);
        return;
    }

    // Session pane to right stack, 1.9 : 1, with a column of desktop between
    // them. The transcript is what a session is read in; the panes beside it
    // are figures, and a figure needs a fraction of the width a paragraph
    // does.
    let [_, stack] = Layout::horizontal([Constraint::Fill(19), Constraint::Fill(10)])
        .spacing(DESKTOP_MARGIN)
        .areas(area);

    // And a column of desktop at each edge of the screen, so the panes stand
    // off the screen's edges as they stand off each other. Both come
    // out of the session pane: its prose rewraps a column narrower, where the
    // right stack's rows are as wide as their figures need and a column less
    // would cost the context meter a cell. Narrow, the columns are worth more
    // as transcript.
    let panes = area.inner(Margin::new(DESKTOP_MARGIN, 0));
    let [left, right] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(stack.width)])
        .spacing(DESKTOP_MARGIN)
        .areas(panes);

    draw_session(frame, left, true, app, theme);

    // Usage takes the rows its figures need and no more; what is left goes to
    // the two panes that grow with the session, 1.3 : 1 in favour of the files
    // it changed. A pane the operator hid gives its rows to the others, and
    // Usage alone leaves desktop under it.
    let usage_rows = usage_height(app).min(right.height);
    let mut constraints: Vec<Constraint> = shown
        .iter()
        .map(|pane| match pane {
            SidePane::Usage => Constraint::Length(usage_rows),
            SidePane::Changes => Constraint::Fill(13),
            SidePane::Activity => Constraint::Fill(10),
        })
        .collect();
    if shown == [SidePane::Usage] {
        constraints.push(Constraint::Fill(1));
    }
    let areas = Layout::vertical(constraints).split(right);

    for pane in [Pane::Changes, Pane::Activity] {
        let side = match pane {
            Pane::Changes => SidePane::Changes,
            Pane::Activity => SidePane::Activity,
        };
        if !shown.contains(&side) {
            app.pane_not_drawn(pane);
        }
    }
    for (pane, at) in shown.iter().zip(areas.iter()) {
        match pane {
            SidePane::Usage => draw_usage(frame, *at, app, theme),
            SidePane::Changes => draw_changes(frame, *at, app, theme),
            SidePane::Activity => draw_activity(frame, *at, app, theme),
        }
    }
}

/// How a pane's border is drawn: the pane with the keyboard in the theme's
/// focus line and colour, every other pane in its plain line and the frame
/// colour.
///
/// Three marks say which pane has the keyboard — the line, the colour and the
/// inverted title — and the line is the one that survives a monochrome
/// terminal, the way the window with the keyboard in Turbo Vision was the one
/// with the double frame. The design's glow has no cell to be drawn in, and
/// the three marks carry what it said.
#[derive(Debug, Clone, Copy)]
struct Border {
    kind: BorderType,
    /// The scrollbar's track, which is drawn over the border and has to read
    /// as the same line.
    track: &'static str,
    style: Style,
    focused: bool,
}

impl Border {
    fn of(focused: bool, theme: &Theme) -> Self {
        let (kind, colour) = match focused {
            true => (theme.border_focus, theme.frame_focus),
            false => (theme.border, theme.frame),
        };
        Border {
            kind,
            track: kind.to_border_set().vertical_right,
            style: Style::new().fg(colour),
            focused,
        }
    }
}

/// The pane frame every pane shares: the border [`Border`] says, the title
/// centred on the top edge, and a column either side between the border and
/// what is written inside it.
///
/// What a pane holds starts on the row under its title, with no blank row
/// between: the title already stands apart on the border, and a row of the
/// changed files or of the agents at work is worth more than the room.
fn pane(title: impl Into<String>, border: Border, theme: &Theme) -> Block<'static> {
    // Reversed rather than painted, so that a terminal with no colour still
    // draws the focused title as a solid bar.
    let title_style = match border.focused {
        true => Style::new()
            .fg(theme.title)
            .bg(theme.pane_bg)
            .add_modifier(Modifier::REVERSED)
            .bold(),
        false => Style::new().fg(theme.title).bold(),
    };
    Block::bordered()
        .border_type(border.kind)
        .border_style(border.style)
        .style(Style::new().bg(theme.pane_bg).fg(theme.fg))
        .padding(Padding::horizontal(1))
        .title_top(
            Line::from(format!(" {} ", title.into()))
                .style(title_style)
                .centered(),
        )
}

/// The session pane: the transcript, and the composer under it. `panes` is
/// whether the right-hand stack is on screen beside it, which decides whether
/// Tab has anywhere to go.
fn draw_session(frame: &mut Frame, area: Rect, panes: bool, app: &mut App, theme: &Theme) {
    let title = session_caption(app.caption(), &app.repo().name, area.width);

    app.measured_session(area);
    app.drew_jump(None);
    let border = Border::of(app.focus() == Focus::Session, theme);
    let block = pane(title, border, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    find_in_transcript(app, inner.width, theme);
    fit_placeholder(app, panes, inner.width, theme);

    // The composer grows with what is typed into it, up to a third of the pane,
    // so a long prompt is editable without hiding the transcript behind it.
    // The shell's own reply wraps in the bar rather than being cut, and takes
    // a row of its own only while it is too long for one.
    let said = bar_says(app, panes, theme, bar_room(app, inner.width));
    // Split rather than `lines`, which does not count the empty line a
    // just-opened one is: that line would take the only row, and scroll the
    // one above it out of sight.
    let typed = app.composed().split('\n').count().max(said.len()).max(1);
    let cap = usize::from(inner.height / 3).max(1);
    let composer_rows = u16::try_from(typed.min(cap)).unwrap_or(1);

    let [transcript, divider, composer] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(composer_rows),
    ])
    .areas(inner);

    // The scrollbar is drawn on the pane's own border rather than inside the
    // room the padding keeps, so a transcript that overflows does not gain a
    // second vertical line beside the one the pane already has.
    draw_transcript(
        frame,
        transcript,
        (area.right().saturating_sub(1), border),
        app,
        theme,
    );

    frame.render_widget(
        Paragraph::new(Line::from("─".repeat(usize::from(divider.width))))
            .style(Style::new().fg(theme.frame).bg(theme.pane_bg)),
        divider,
    );

    draw_ask_bar(frame, composer, said, app, theme);
    draw_mention(frame, transcript, composer, app, theme);
}

/// The files an `@` word could name, or the backend's commands the `/` that
/// opens the prompt could, as a list standing on the divider above the bar,
/// over the foot of the transcript, lined up with what is typed.
fn draw_mention(frame: &mut Frame, transcript: Rect, bar: Rect, app: &App, theme: &Theme) {
    let (files, selected) = app.mention_files();
    if !files.is_empty() {
        let rows = files
            .iter()
            .map(|file| ((*file).to_owned(), None))
            .collect();
        draw_offer(
            frame,
            (transcript, bar),
            app,
            theme,
            (" @ file ", rows, selected),
        );
        return;
    }
    let (commands, selected) = app.offered_commands();
    if !commands.is_empty() {
        let rows = commands
            .iter()
            .map(|command| {
                let named = match &command.argument_hint {
                    Some(hint) => format!("/{} {hint}", command.name),
                    None => format!("/{}", command.name),
                };
                (named, Some(command.description.as_str()))
            })
            .collect();
        draw_offer(
            frame,
            (transcript, bar),
            app,
            theme,
            (" / command ", rows, selected),
        );
    }
}

/// The widest a command's description is drawn beside its name, so that one
/// long description does not stretch the list across the whole transcript.
const OFFER_DETAIL: usize = 56;

/// One list of what the word being typed could become: its title, each row as
/// what goes into the prompt and what it is, and the row Enter would take.
type Offer<'a> = (&'static str, Vec<(String, Option<&'a str>)>, usize);

/// Draws `offer` over the foot of `transcript`, starting where the bar's
/// prompt starts.
fn draw_offer(
    frame: &mut Frame,
    (transcript, bar): (Rect, Rect),
    app: &App,
    theme: &Theme,
    (title, rows, selected): Offer<'_>,
) {
    let lead = u16::try_from(lead_width(app)).unwrap_or(u16::MAX);
    let x = bar.x.saturating_add(lead).min(transcript.right());
    let room = transcript.right().saturating_sub(x);
    let named = rows
        .iter()
        .map(|(row, _)| text::width(row))
        .max()
        .unwrap_or(0);
    let detail = rows
        .iter()
        .filter_map(|(_, detail)| detail.map(|detail| text::width(first_line(detail))))
        .max()
        .map_or(0, |widest| widest.min(OFFER_DETAIL) + 2);
    // A border either side, and a column of padding inside each.
    let width = u16::try_from(named + detail + 4)
        .unwrap_or(u16::MAX)
        .min(room);
    let shown = rows
        .len()
        .min(usize::from(transcript.height.saturating_sub(2)));
    if shown == 0 || width < 5 {
        return;
    }
    let height = u16::try_from(shown + 2).unwrap_or(u16::MAX);
    let area = Rect::new(x, transcript.bottom().saturating_sub(height), width, height);

    let inside = usize::from(width.saturating_sub(4));
    let lines: Vec<Line<'static>> = rows
        .iter()
        .take(shown)
        .enumerate()
        .map(|(at, (row, detail))| {
            let chosen = at == selected;
            let style = match chosen {
                true => Style::new().fg(theme.pane_bg).bg(theme.hot).bold(),
                false => Style::new().fg(theme.fg),
            };
            let Some(detail) = detail else {
                // A file's name is the end of its path, so a path too long
                // for the list keeps that and gives up its leading
                // directories.
                let shown = format!(
                    " {} ",
                    text::pad(&text::truncate_start(row, inside), inside)
                );
                return Line::from(shown).style(style);
            };
            // The names are a column, so the descriptions start together.
            let row = text::truncate(row, inside);
            let row = text::pad(&row, named.min(inside));
            let left = inside.saturating_sub(text::width(&row));
            let said = match left > 2 {
                true => format!("  {}", text::truncate(first_line(detail), left - 2)),
                false => String::new(),
            };
            let pad = inside.saturating_sub(text::width(&row) + text::width(&said));
            let detail_style = match chosen {
                true => style,
                false => Style::new().fg(theme.dim),
            };
            Line::from(vec![
                Span::styled(format!(" {row}"), style),
                Span::styled(format!("{said}{} ", " ".repeat(pad)), detail_style),
            ])
        })
        .collect();
    let block = Block::bordered()
        .border_type(theme.border)
        .border_style(Style::new().fg(theme.frame).bg(theme.pane_bg))
        .style(Style::new().bg(theme.pane_bg))
        .title_top(Line::from(title).style(Style::new().fg(theme.title).bold()));
    frame.render_widget(Clear, area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The first line of `text`, which is all of a description a list row has
/// room for.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

/// Gives the placeholder the room the bar leaves once its held hints — the
/// key that sets a mode, where none is reported, and the key that opens a
/// line where Enter would be mistaken for it — have what they need.
fn fit_placeholder(app: &mut App, panes: bool, width: u16, theme: &Theme) {
    let held: Vec<Segment> = bar_key_hints(app, panes, theme)
        .into_iter()
        .filter(|hint| hint.held)
        .map(|hint| hint.segment)
        .collect();
    // The cursor, and the column between the editor and the hints.
    let columns = usize::from(width)
        .saturating_sub(lead_width(app))
        .saturating_sub(hints_width(held.iter()) + 2);
    app.fit_placeholder(columns);
}

/// Looks for what a search is looking for in the transcript as it will be
/// drawn `width` wide, before anything is drawn: the bar says how many places
/// it was found, and the bar is drawn before the transcript.
fn find_in_transcript(app: &mut App, width: u16, theme: &Theme) {
    let Some(query) = app.find_query() else {
        return;
    };
    let detail = transcript_detail(app);
    let headings = app.grouped_by_agent().then(|| app.agent_headings());
    let (entries, drawn) = app.entries_to_draw();
    drawn.update(
        entries,
        usize::from(width),
        detail,
        headings.as_deref(),
        theme,
    );
    let changed = match crate::find::Query::new(&query) {
        Some(query) => drawn.find(&query),
        None => drawn.forget_search(),
    };
    app.found(changed);
}

/// The border cells a pane's title leaves on its top edge: a corner and a
/// cell of the edge either side, so the pane still reads as framed, and the
/// space either side of the title itself.
const TITLE_MARGIN: u16 = 6;

/// The session pane's title on a pane `width` wide: what the session is about
/// where it says ([`App::caption`]), and the repository it runs in where it
/// does not yet.
///
/// Cut between words where it is longer than the edge has room for, so that
/// the corners and a cell of the edge either side survive at every width.
fn session_caption(caption: Option<String>, repo: &str, width: u16) -> String {
    let title = match caption {
        Some(caption) => caption,
        None if repo.is_empty() => "Session".to_owned(),
        None => format!("Session ─ {repo}"),
    };
    text::truncate_words(&title, usize::from(width.saturating_sub(TITLE_MARGIN)))
}

/// The badge in front of the composer before anything has said which mode
/// the session gates tool calls in: it claims none, as a figure nobody
/// reported is a dash.
const UNREPORTED_MODE_BADGE: &str = " — ";

/// The badge the bar wears while it is searching the transcript.
const FIND_BADGE: &str = " find ";

/// The badge the bar wears while it holds a command for the operator's shell.
const SHELL_BADGE: &str = " shell ";

/// What the bar puts between its hints.
const HINT_SEPARATOR: &str = " · ";

/// The badge and the marker in front of what the bar is editing: a search
/// through the transcript, a command for the operator's shell, or the prompt.
///
/// The prompt's badge is the mode the session is in. It sits where the eye
/// goes to type, next to the key that changes it, and nothing else on the bar
/// takes its place: the mode decides what a prompt may do, so it is not a
/// hint to give way to a reply or a long prompt. A command and a search are
/// not gated by it, so their badges say what they are instead.
fn bar_lead(app: &App) -> (String, &'static str) {
    match (app.finding(), app.shell_mode()) {
        (Some(_), _) => (FIND_BADGE.to_owned(), " / "),
        (None, true) => (SHELL_BADGE.to_owned(), " $ "),
        (None, false) => {
            let badge = app.session().mode().map_or_else(
                || UNREPORTED_MODE_BADGE.to_owned(),
                |mode| format!(" {mode} "),
            );
            (badge, " > ")
        }
    }
}

/// The columns of the badge and the marker.
fn lead_width(app: &App) -> usize {
    let (badge, marker) = bar_lead(app);
    text::width(&badge) + text::width(marker)
}

/// What the bar is editing: the search while one is open, which the composer
/// behind it waits under, and the composer otherwise.
fn editing(app: &App) -> &ratatui_textarea::TextArea<'static> {
    app.finding().unwrap_or_else(|| app.composer())
}

/// The composer, with its badge in front and, at its right-hand end, what the
/// shell has to say: its own reply to the last key when it has one, otherwise
/// the keys that change what the bar does.
fn draw_ask_bar(
    frame: &mut Frame,
    area: Rect,
    said: Vec<Line<'static>>,
    app: &mut App,
    theme: &Theme,
) {
    let (badge_text, marker_text) = bar_lead(app);
    let [badge, marker, rest] = Layout::horizontal([
        Constraint::Length(u16::try_from(text::width(&badge_text)).unwrap_or(u16::MAX)),
        Constraint::Length(u16::try_from(text::width(marker_text)).unwrap_or(u16::MAX)),
        Constraint::Min(1),
    ])
    .areas(area);
    let pane = Style::new().bg(theme.pane_bg);
    frame.render_widget(
        Paragraph::new(
            Line::from(badge_text).style(Style::new().fg(theme.pane_bg).bg(theme.hot).bold()),
        )
        .style(pane),
        Rect { height: 1, ..badge },
    );
    frame.render_widget(
        Paragraph::new(Line::from(marker_text).style(Style::new().fg(theme.hot).bold()))
            .style(pane),
        marker,
    );

    // A reply takes every column the composer leaves it, so no part of the
    // placeholder is left showing beside it; the key hints take only their own.
    let said_width = match app.hint() {
        Some(_) if !said.is_empty() => bar_room(app, area.width),
        _ => said.iter().map(Line::width).max().unwrap_or(0),
    };
    let said_width = u16::try_from(said_width).unwrap_or(0);
    let [editor, _, right] = Layout::horizontal([
        Constraint::Min(1),
        Constraint::Length(u16::from(said_width > 0)),
        Constraint::Length(said_width),
    ])
    .areas(rest);
    editing(app).render(editor, frame.buffer_mut());
    frame.render_widget(Paragraph::new(said).style(pane), right);
}

/// The columns the bar's right-hand end may take on a pane `width` wide.
///
/// What was typed keeps the room it needs, and the right-hand end gets what is
/// left: a hint drawn over a prompt would hide the prompt.
fn bar_room(app: &App, width: u16) -> usize {
    usize::from(width)
        .saturating_sub(lead_width(app))
        .saturating_sub(editor_needs(app) + 1)
}

/// The columns the composer needs to show what is in it, cursor included.
///
/// An empty composer needs its placeholder, unless the shell has something to
/// say: the placeholder teaches what the bar is for, and a reply to the key
/// just pressed is the more pressing of the two.
fn editor_needs(app: &App) -> usize {
    let editor = editing(app);
    let typed = editor.lines().join("\n");
    let widest = match (typed.is_empty(), app.hint()) {
        (true, Some(_)) => 0,
        (true, None) => text::width(editor.placeholder_text()),
        (false, _) => typed.lines().map(text::width).max().unwrap_or(0),
    };
    widest + 1
}

/// What the bar's right-hand end says in `room` columns, a line per row.
///
/// The shell's own reply is wrapped, since it is a sentence the operator asked
/// for. The key hints are one row and give way whole, the last first, because
/// half a key reads as a different key.
fn bar_says(app: &App, panes: bool, theme: &Theme, room: usize) -> Vec<Line<'static>> {
    if room == 0 {
        return Vec::new();
    }
    if let Some(said) = app.hint() {
        return text::wrap(said, room)
            .into_iter()
            .map(|line| Line::from(line).style(Style::new().fg(theme.hot)))
            .collect();
    }
    let hints = match app.find_marks() {
        Some((found, current)) => loose(find_hints(app, found.len(), current, theme)),
        None if app.shell_mode() => loose(
            ["Enter runs", "Esc back"]
                .into_iter()
                .map(|key| Segment {
                    text: key.to_owned(),
                    style: Style::new().fg(theme.dim),
                })
                .collect(),
        ),
        None => bar_key_hints(app, panes, theme),
    };
    let hints = fitted_hints(hints, room);
    let mut spans = Vec::with_capacity(hints.len() * 2);
    for (i, hint) in hints.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(HINT_SEPARATOR, Style::new().fg(theme.dim)));
        }
        spans.push(Span::styled(hint.text, hint.style));
    }
    vec![Line::from(spans)]
}

/// What the bar says while it searches: where in the matches the view is,
/// then the keys that step between them and leave.
fn find_hints(app: &App, count: usize, current: Option<usize>, theme: &Theme) -> Vec<Segment> {
    let key = |text: &str| Segment {
        text: text.to_owned(),
        style: Style::new().fg(theme.dim),
    };
    let typed = app.finding().is_some_and(|query| !query.is_empty());
    let mut hints = Vec::new();
    match current {
        Some(at) => hints.push(Segment {
            text: format!("{} of {count}", at + 1),
            style: Style::new().fg(theme.hot).bold(),
        }),
        None if typed => hints.push(Segment {
            text: "no match".to_owned(),
            style: Style::new().fg(theme.hot).bold(),
        }),
        None => {}
    }
    if count > 1 {
        hints.push(key("↑↓ step"));
    }
    hints.push(key("Esc back"));
    hints
}

/// A hint on the bar, and whether it is held: kept while the placeholder and
/// the hints that are not give way.
struct Hint {
    segment: Segment,
    held: bool,
}

/// Hints none of which is held, so the bar gives them up last first.
fn loose(segments: Vec<Segment>) -> Vec<Hint> {
    segments
        .into_iter()
        .map(|segment| Hint {
            segment,
            held: false,
        })
        .collect()
}

/// The key hints the bar shows with nothing else to say.
///
/// A question holding the keyboard takes Tab for writing its answer, so while
/// it does, Tab is not offered as the way between the panes.
fn bar_key_hints(app: &App, panes: bool, theme: &Theme) -> Vec<Hint> {
    let question_holds = app.asking().is_some() && app.ask_focus() != AskFocus::Deferred;
    // The grouping key is offered only where there are agents to group.
    let grouping = (!app.agents().is_empty()).then(|| app.grouped_by_agent());
    key_hints(
        app.session().mode().is_some(),
        (app.focus(), grouping),
        panes && !question_holds,
        (app.newline_key(), app.sends_enter_for_shift_enter()),
        theme,
    )
}

/// The keys the bar answers to, in the order the bar shows them.
///
/// With a right-hand pane focused, the keys that differ are that pane's: the
/// arrows and Enter are not the composer's while it has them. `panes` is
/// whether there is a pane beside the session for Tab to move to, `reported`
/// whether a mode has been reported for the badge to name, and `newline` the
/// key that opens a line on this terminal and whether Enter is sent for
/// Shift+Enter on it.
///
/// Before a mode is reported, the key that sets one is held: the badge names
/// none, and this is where the bar says there are modes at all. So is the
/// newline key where Shift+Enter arrives as Enter: an operator not told
/// otherwise reaches for Shift+Enter and sends a prompt half-written. The
/// other keys are reminders, and give way first.
fn key_hints(
    reported: bool,
    (focus, grouping): (Focus, Option<bool>),
    panes: bool,
    newline: (&str, bool),
    theme: &Theme,
) -> Vec<Hint> {
    let key = |text: &str, held: bool| Hint {
        segment: Segment {
            text: text.to_owned(),
            style: Style::new().fg(theme.dim),
        },
        held,
    };
    let mut hints = match reported {
        true => vec![key("Shift+Tab cycles", false)],
        false => vec![key("Shift+Tab mode", true)],
    };
    match focus {
        Focus::Session => {
            let (newline, mistaken) = newline;
            hints.push(key(&format!("{newline} newline"), mistaken));
            if panes {
                hints.push(key("Tab panes", false));
            }
        }
        Focus::Pane(_) => {
            hints.push(key("↑↓ Enter folds", false));
            match grouping {
                Some(false) => hints.push(key("a groups by agent", false)),
                Some(true) => hints.push(key("a ungroups", false)),
                None => {}
            }
            hints.push(key("Esc back", false));
        }
    }
    hints
}

/// The columns `hints` take on the bar, separators included.
fn hints_width<'a>(hints: impl ExactSizeIterator<Item = &'a Segment>) -> usize {
    let count = hints.len();
    let text: usize = hints.map(|hint| text::width(&hint.text)).sum();
    text + count.saturating_sub(1) * text::width(HINT_SEPARATOR)
}

/// The hints that fit in `room` columns, whole ones only: the last hint that
/// is not held gives way first, and the held ones, last first, only once
/// every other has.
fn fitted_hints(mut hints: Vec<Hint>, room: usize) -> Vec<Segment> {
    while !hints.is_empty() && hints_width(hints.iter().map(|hint| &hint.segment)) > room {
        let gives_way = hints
            .iter()
            .rposition(|hint| !hint.held)
            .unwrap_or(hints.len() - 1);
        hints.remove(gives_way);
    }
    hints.into_iter().map(|hint| hint.segment).collect()
}

/// The transcript, in `area`, with its scrollbar drawn over the pane border at
/// column `border.0` and, while it is scrolled back, the way back down.
fn draw_transcript(
    frame: &mut Frame,
    area: Rect,
    border: (u16, Border),
    app: &mut App,
    theme: &Theme,
) {
    // A running turn keeps the bottom row, whatever is scrolled into view
    // above it: whether the session is at work is the question the operator
    // looks down to answer, and a line that scrolled away would not answer it.
    let area = match app.activity() {
        Some(activity) if area.height > 1 => {
            let [area, working] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
            frame.render_widget(
                Paragraph::new(working_line(&activity, usize::from(working.width), theme))
                    .style(Style::new().bg(theme.pane_bg)),
                working,
            );
            area
        }
        _ => area,
    };
    let width = usize::from(area.width);
    let height = usize::from(area.height);

    // The question is the transcript's last entry for as long as it waits,
    // drawn fresh every frame: it changes with every key the operator presses
    // at it, and it is never more than a screenful.
    let question = app
        .asking()
        .map(|ask| question_lines(app, ask, (width, height), theme))
        .unwrap_or_default();
    let framed = question.framed;
    let question = question.lines;

    if app.entries().is_empty() && question.is_empty() {
        let lines = empty_transcript(app.is_attached(), theme);
        app.drew_transcript(area, 0);
        app.measured(lines.len(), height);
        frame.render_widget(
            Paragraph::new(lines).style(Style::new().bg(theme.pane_bg)),
            area,
        );
        return;
    }

    let detail = transcript_detail(app);
    let headings = app.grouped_by_agent().then(|| app.agent_headings());
    let (entries, drawn) = app.entries_to_draw();
    drawn.update(entries, width, detail, headings.as_deref(), theme);
    let above = drawn.line_count();
    app.keep_view();
    let total = above + question.len();

    app.measured(total, height);
    app.reveal_found();
    let start = app.scroll().min(total);
    let (_, drawn) = app.entries_to_draw();
    let mut visible = drawn.lines(start, height);
    if let Some((found, current)) = app.find_marks() {
        mark_found(&mut visible, start, found, current, theme);
    }
    if let Some(selection) = app.selection() {
        mark_selected(&mut visible, start, selection, theme);
    }
    app.drew_transcript(area, above);
    let room = height.saturating_sub(visible.len());
    app.drew_question_top(question.is_empty() || (start..start + height).contains(&above));
    visible.extend(
        question
            .into_iter()
            .skip(start.saturating_sub(above))
            .take(room),
    );
    frame.render_widget(
        Paragraph::new(visible).style(Style::new().bg(theme.pane_bg)),
        area,
    );
    if let Some(framed) = framed {
        shadow_question(frame, area, framed, (above, start), theme);
    }
    draw_scrollbar(frame, area, border, (start, total, height), theme);
    if !app.follows_tail() {
        let at = draw_jump(frame, area, app.asking().is_some(), theme);
        app.drew_jump(at);
    }
}

/// The question's shadow, on the transcript `area` its frame of `columns` by
/// `rows` is drawn in: the question starts after the transcript's first
/// `above` lines, and the view after its first `start`.
fn shadow_question(
    frame: &mut Frame,
    area: Rect,
    (columns, rows): (u16, usize),
    (above, start): (usize, usize),
    theme: &Theme,
) {
    let clamp = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
    let (top, rows) = match above.checked_sub(start) {
        Some(offset) => (area.y.saturating_add(clamp(offset)), rows),
        // Scrolled into the question, its top is above the pane. The shadow
        // starts a row under the frame's top, so the frame is taken to start
        // on the row above the pane, with the rows scrolled past taken off.
        None => (
            area.y.saturating_sub(1),
            (rows + 1).saturating_sub(start - above),
        ),
    };
    let x = area.x.saturating_add(clamp(GUTTER));
    cast_shadow(frame, Rect::new(x, top, columns, clamp(rows)), area, theme);
}

/// The way back down to the newest line, as a button at the bottom right of
/// the transcript, drawn only while the view is not there.
///
/// A question that arrives while the operator reads back does not move the
/// view, so the button says it is waiting: this is where the way to it is.
/// The words give way to the arrow on a pane too narrow for them. Returns
/// where it was drawn, so a click on it can be recognised.
fn draw_jump(frame: &mut Frame, area: Rect, waiting: bool, theme: &Theme) -> Option<Rect> {
    let said = [
        (waiting, " A question is waiting · Jump to bottom ↓ "),
        (true, " Jump to bottom ↓ "),
        (true, " ↓ "),
    ]
    .into_iter()
    .filter(|(offered, _)| *offered)
    .map(|(_, said)| said)
    .find(|said| text::width(said) <= usize::from(area.width))?;
    let width = u16::try_from(text::width(said)).ok()?;
    if area.height == 0 {
        return None;
    }
    let at = Rect::new(
        area.right().saturating_sub(width),
        area.bottom().saturating_sub(1),
        width,
        1,
    );
    frame.render_widget(
        Paragraph::new(Line::from(said).style(Style::new().fg(theme.pane_bg).bg(theme.hot).bold())),
        at,
    );
    Some(at)
}

/// Marks what the operator selected on the transcript lines drawn from line
/// `start`, in reverse, as a terminal marks a selection of its own.
fn mark_selected(
    lines: &mut [Line<'static>],
    start: usize,
    selection: crate::select::Selection,
    theme: &Theme,
) {
    let style = Style::new().fg(theme.pane_bg).bg(theme.fg);
    for (row, line) in lines.iter_mut().enumerate() {
        let Some((from, to)) = selection.columns_on(start + row) else {
            continue;
        };
        let (first, count) = crate::select::chars_in(&crate::select::plain(line), from, to);
        if count > 0 {
            *line = crate::find::highlight(std::mem::take(line), &[(first, count, style)]);
        }
    }
}

/// Marks every match of a search on the transcript lines drawn from line
/// `start`: the one the view was stepped to as a solid chip, the rest
/// underlined, so the current one can be told from them without its colour.
fn mark_found(
    lines: &mut [Line<'static>],
    start: usize,
    found: &[crate::find::Found],
    current: Option<usize>,
    theme: &Theme,
) {
    let here = Style::new().fg(theme.pane_bg).bg(theme.hot).bold();
    let other = Style::new().fg(theme.hot).underlined();
    for (row, line) in lines.iter_mut().enumerate() {
        let at = start + row;
        // Matches do not overlap, so those that reach this line are the last
        // ones to start on or above it, back to the first that ends above it.
        let upto = found.partition_point(|found| found.line <= at);
        let marks: Vec<(usize, usize, Style)> = found[..upto]
            .iter()
            .enumerate()
            .rev()
            .take_while(|(_, found)| found.last_line() >= at)
            .flat_map(|(index, found)| {
                let style = if Some(index) == current { here } else { other };
                found
                    .parts()
                    .filter(move |(line, _, _)| *line == at)
                    .map(move |(_, start, len)| (start, len, style))
            })
            .collect();
        if !marks.is_empty() {
            *line = crate::find::highlight(std::mem::take(line), &marks);
        }
    }
}

/// Every transcript entry's lines as they were last drawn, with what they were
/// drawn from, so that a redraw renders again only the entries that changed.
///
/// A finished reply never changes, and parsing and wrapping every one of them
/// on every frame is what a long session's redraw would otherwise spend its
/// time on. An entry is drawn again when its text, the pane's width or the
/// theme changes, runs of calls are folded or opened, or — for an entry with
/// a diff longer than it draws unopened — the diffs are opened or cut, which
/// is everything its lines depend on.
#[derive(Debug, Default)]
pub struct DrawnEntries {
    drawn: Vec<Drawn>,
    /// The query each entry's [`Drawn::found`] holds the matches of.
    searched: Option<crate::find::Query>,
    /// Every place `searched` is in the transcript as drawn, top to bottom,
    /// as [`DrawnEntries::find`] last put them together.
    found: Vec<crate::find::Found>,
    /// Whether an entry has been drawn again, or the order changed, since
    /// `found` was put together.
    moved: bool,
}

/// One thing the transcript draws, in the order it is drawn: an entry, or
/// the heading a sub-agent's rows are grouped under.
#[derive(Debug)]
struct Drawn {
    /// What the lines were drawn from, as one number.
    key: u64,
    /// The entry drawn, or for a heading the first entry under it.
    entry: usize,
    heading: bool,
    lines: Vec<Line<'static>>,
    /// Where [`DrawnEntries::searched`] is in `lines`, counted from their
    /// first, once they have been searched. Kept with the lines, so a search
    /// held open reads again only the entries drawn again: every frame
    /// counts the matches, and reading every line of a long session for them
    /// is what the frame would otherwise spend its time on.
    found: Option<Vec<crate::find::Found>>,
}

/// What the transcript draws at one place, before it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Entry(usize),
    /// The heading over a sub-agent's rows in a turn: the first of them, and
    /// how many calls they hold.
    Heading {
        first: usize,
        calls: usize,
    },
}

impl DrawnEntries {
    /// Brings every entry's lines up to date, in the order they are drawn.
    ///
    /// The columns are worked out here, from every entry, because a row is
    /// drawn in the columns the widest row needs; and so is whether an entry
    /// keeps the blank line after it, which depends on what is drawn next.
    /// `headings` is each sub-agent's tag and task where the rows are grouped
    /// by agent, and `None` where they are drawn as they happened.
    fn update(
        &mut self,
        entries: &[Entry],
        width: usize,
        detail: Detail,
        headings: Option<&[(String, String)]>,
        theme: &Theme,
    ) {
        let detail = Detail {
            columns: crate::calls::Columns::of(entries),
            grouped: headings.is_some(),
            ..detail
        };
        let order = order(entries, headings.is_some());
        self.moved |= self.drawn.len() > order.len();
        self.drawn.truncate(order.len());
        for (at, slot) in order.iter().enumerate() {
            let next_is_row = match order.get(at + 1) {
                Some(Slot::Entry(next)) => entries.get(*next).is_some_and(is_row),
                Some(Slot::Heading { .. }) | None => false,
            };
            let (key, entry, heading) = match *slot {
                Slot::Entry(index) => {
                    let packed = entries.get(index).is_some_and(is_row) && next_is_row;
                    let key = entries
                        .get(index)
                        .map_or(0, |entry| drawn_from(entry, width, detail, theme))
                        ^ u64::from(packed);
                    (key, index, false)
                }
                Slot::Heading { first, calls } => {
                    let heading = heading_of(entries, first, headings);
                    (
                        heading_key(&heading, calls, width, detail, theme),
                        first,
                        true,
                    )
                }
            };
            let draw = || match *slot {
                Slot::Entry(index) => {
                    let Some(entry) = entries.get(index) else {
                        return Vec::new();
                    };
                    let mut lines = entry_lines(entry, width, detail, theme);
                    if is_row(entry)
                        && next_is_row
                        && lines.last().is_some_and(|line| line.width() == 0)
                    {
                        lines.pop();
                    }
                    lines
                }
                Slot::Heading { first, calls } => {
                    let (tag, task) = heading_of(entries, first, headings);
                    vec![crate::calls::heading(
                        &tag, &task, calls, width, detail, theme,
                    )]
                }
            };
            match self.drawn.get_mut(at) {
                Some(drawn) if drawn.key == key => {
                    self.moved |= drawn.entry != entry || drawn.heading != heading;
                    drawn.entry = entry;
                    drawn.heading = heading;
                }
                Some(drawn) => {
                    self.moved = true;
                    *drawn = Drawn {
                        key,
                        entry,
                        heading,
                        lines: draw(),
                        found: None,
                    }
                }
                None => {
                    self.moved = true;
                    self.drawn.push(Drawn {
                        key,
                        entry,
                        heading,
                        lines: draw(),
                        found: None,
                    });
                }
            }
        }
    }

    /// Finds every place `query` is in the transcript as drawn, top to
    /// bottom, for [`DrawnEntries::found`]. Returns whether that changed.
    ///
    /// An entry is read for it only where its lines have changed since it
    /// was last read for the same query, and the places are put together
    /// again only where an entry has.
    fn find(&mut self, query: &crate::find::Query) -> bool {
        if self.searched.as_ref() != Some(query) {
            for drawn in &mut self.drawn {
                drawn.found = None;
            }
            self.searched = Some(query.clone());
            self.moved = true;
        }
        if !std::mem::take(&mut self.moved) {
            return false;
        }
        let mut first = 0;
        let found = &mut self.found;
        found.clear();
        for drawn in &mut self.drawn {
            let which = crate::find::Which {
                entry: drawn.entry,
                heading: drawn.heading,
                nth: 0,
            };
            let lines = &drawn.lines;
            let here = drawn.found.get_or_insert_with(|| query.in_lines(lines, 0));
            found.extend(here.iter().map(|at| at.placed(first, which)));
            first += lines.len();
        }
        true
    }

    /// Forgets the search, for a query with nothing in it, which finds
    /// nothing. Returns whether anything had been found.
    fn forget_search(&mut self) -> bool {
        self.searched = None;
        let had = !self.found.is_empty();
        self.found.clear();
        had
    }

    /// Every place the search is in the transcript as last drawn, top to
    /// bottom.
    pub(crate) fn found(&self) -> &[crate::find::Found] {
        &self.found
    }

    fn line_count(&self) -> usize {
        self.drawn.iter().map(|drawn| drawn.lines.len()).sum()
    }

    /// Line `line` of the whole transcript as last drawn, named by what it
    /// was drawn from rather than by how far down it is. `None` where nothing
    /// is drawn there.
    pub(crate) fn anchor(&self, line: usize) -> Option<Anchor> {
        let mut first = 0usize;
        for drawn in &self.drawn {
            let offset = line.checked_sub(first)?;
            if offset < drawn.lines.len() {
                return Some(Anchor {
                    entry: drawn.entry,
                    heading: drawn.heading,
                    offset,
                    lines: drawn.lines.clone(),
                });
            }
            first = first.saturating_add(drawn.lines.len());
        }
        None
    }

    /// Where `anchor` is in the transcript as drawn now: the same line of the
    /// same entry, and where that line is gone — a diff cut again, a run of
    /// calls folded — the nearest line above it that is still drawn, so the
    /// view stops on what led up to the line rather than past it. A heading
    /// no longer drawn — the rows were ungrouped — is found as the first row
    /// that was under it.
    pub(crate) fn line_of(&self, anchor: &Anchor) -> Option<usize> {
        let at = self
            .drawn
            .iter()
            .position(|drawn| drawn.entry == anchor.entry && drawn.heading == anchor.heading)
            .or_else(|| {
                self.drawn
                    .iter()
                    .position(|drawn| drawn.entry == anchor.entry)
            })?;
        let lines = &self.drawn.get(at)?.lines;
        let last = lines.len().checked_sub(1)?;
        let first: usize = self
            .drawn
            .iter()
            .take(at)
            .map(|drawn| drawn.lines.len())
            .sum();
        let above = (0..=anchor.offset).rev();
        let below = anchor.offset.saturating_add(1)..anchor.lines.len();
        let offset = above
            .chain(below)
            .find_map(|at| {
                let held = anchor.lines.get(at)?;
                lines
                    .iter()
                    .enumerate()
                    .filter(|(_, line)| *line == held)
                    .map(|(now, _)| now)
                    .min_by_key(|now| now.abs_diff(at))
            })
            .unwrap_or(anchor.offset.min(last));
        Some(first.saturating_add(offset))
    }

    /// Line `line` of the whole transcript as drawn, without its styles.
    pub(crate) fn plain_line(&self, line: usize) -> Option<String> {
        self.lines(line, 1).first().map(crate::select::plain)
    }

    /// The link at `point` of the transcript as drawn `width` cells wide, if
    /// there is one there. A link is looked for within the one entry it is
    /// in: two entries' lines are never one run of text.
    pub(crate) fn link_at(&self, point: crate::select::Point, width: usize) -> Option<String> {
        let mut first = 0usize;
        for drawn in &self.drawn {
            let offset = point.line.checked_sub(first)?;
            if offset < drawn.lines.len() {
                let lines: Vec<String> = drawn.lines.iter().map(crate::select::plain).collect();
                return crate::select::link_at(&lines, width, offset, point.column);
            }
            first = first.saturating_add(drawn.lines.len());
        }
        None
    }

    /// `count` lines from line `start` of the whole transcript.
    fn lines(&self, start: usize, count: usize) -> Vec<Line<'static>> {
        self.drawn
            .iter()
            .flat_map(|drawn| &drawn.lines)
            .skip(start)
            .take(count)
            .cloned()
            .collect()
    }
}

/// The order the transcript draws `entries` in.
///
/// As they happened, unless `grouped`: then within each turn — between one
/// prompt or turn's rule and the next — every sub-agent's rows are drawn
/// together under a heading, at the place that agent first did something.
/// The session's own entries, anything an agent did that is not a row of
/// the calls table, and the turn's boundaries stay where they were.
fn order(entries: &[Entry], grouped: bool) -> Vec<Slot> {
    if !grouped {
        return (0..entries.len()).map(Slot::Entry).collect();
    }
    let mut order = Vec::with_capacity(entries.len());
    let mut start = 0;
    while start < entries.len() {
        let end = (start + 1..entries.len())
            .find(|&at| matches!(entries[at].kind, EntryKind::User | EntryKind::Turn(_)))
            .unwrap_or(entries.len());
        order.extend(grouped_turn(entries, start..end));
        start = end;
    }
    order
}

/// One turn's entries, each sub-agent's rows drawn together.
fn grouped_turn(entries: &[Entry], turn: std::ops::Range<usize>) -> Vec<Slot> {
    enum Place {
        One(usize),
        Group(usize),
    }
    let mut places = Vec::new();
    let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
    for at in turn {
        let Some(entry) = entries.get(at) else {
            continue;
        };
        match entry.agent.as_deref().filter(|_| is_row(entry)) {
            Some(tag) => match groups.iter_mut().find(|(group, _)| *group == tag) {
                Some((_, rows)) => rows.push(at),
                None => {
                    groups.push((tag, vec![at]));
                    places.push(Place::Group(groups.len() - 1));
                }
            },
            None => places.push(Place::One(at)),
        }
    }
    let mut slots = Vec::new();
    for place in places {
        match place {
            Place::One(at) => slots.push(Slot::Entry(at)),
            Place::Group(group) => {
                let Some((_, rows)) = groups.get(group) else {
                    continue;
                };
                let calls = rows
                    .iter()
                    .filter_map(|&at| entries.get(at))
                    .map(|entry| entry.calls.len())
                    .sum();
                slots.push(Slot::Heading {
                    first: rows.first().copied().unwrap_or_default(),
                    calls,
                });
                slots.extend(rows.iter().copied().map(Slot::Entry));
            }
        }
    }
    slots
}

/// The tag and task a heading over entry `first`'s agent's rows names.
fn heading_of(
    entries: &[Entry],
    first: usize,
    headings: Option<&[(String, String)]>,
) -> (String, String) {
    let tag = entries
        .get(first)
        .and_then(|entry| entry.agent.clone())
        .unwrap_or_default();
    let task = headings
        .unwrap_or_default()
        .iter()
        .find(|(named, _)| *named == tag)
        .map(|(_, task)| task.clone())
        .unwrap_or_default();
    (tag, task)
}

/// What a heading's line is drawn from, as one number.
fn heading_key(
    heading: &(String, String),
    calls: usize,
    width: usize,
    detail: Detail,
    theme: &Theme,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "heading".hash(&mut hasher);
    heading.hash(&mut hasher);
    calls.hash(&mut hasher);
    width.hash(&mut hasher);
    detail.columns.hash(&mut hasher);
    theme.hash(&mut hasher);
    hasher.finish()
}

/// A transcript line as the entry it belongs to and its place among that
/// entry's lines, with every line the entry was drawn as, so it can be found
/// again after a switch lays the entries out again.
///
/// The lines are kept as well as the place because a switch moves lines
/// within their own entry — opening a cut diff puts rows above the ones kept
/// at its end — and takes some away, and what is still drawn of the entry is
/// found by what it reads. The entry is named by its place in the transcript
/// rather than where it is drawn, which grouping the rows by agent changes.
#[derive(Debug)]
pub(crate) struct Anchor {
    entry: usize,
    heading: bool,
    offset: usize,
    lines: Vec<Line<'static>>,
}

/// Whether `entry` is drawn as rows of the calls table — a call, a run of
/// calls, a sub-agent's words — with nothing under it that needs room to be
/// read. Two such entries in a row are drawn on consecutive lines, so a
/// session's calls read as one table rather than a list with a gap after
/// every line; a diff or what a command printed keeps the blank line after
/// it, and so does everything else the transcript shows.
fn is_row(entry: &Entry) -> bool {
    match entry.kind {
        EntryKind::SubAgent => true,
        EntryKind::User
        | EntryKind::Agent
        | EntryKind::Tool
        | EntryKind::Failure
        | EntryKind::Notice
        | EntryKind::Turn(_) => {
            !entry.calls.is_empty()
                && entry
                    .calls
                    .iter()
                    .all(|call| call.change.is_none() && call.printed.is_none())
        }
    }
}

/// What an entry's lines are drawn from, as one number.
///
/// Whether the diffs are open goes in only for an entry that holds a diff it
/// would cut, so opening them lays out again those entries and no other; the
/// clock goes in only for one holding a test run still going.
fn drawn_from(entry: &Entry, width: usize, detail: Detail, theme: &Theme) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    entry.hash(&mut hasher);
    width.hash(&mut hasher);
    detail.folded.hash(&mut hasher);
    detail.columns.hash(&mut hasher);
    detail.grouped.hash(&mut hasher);
    if holds_a_cut_diff(entry) {
        detail.diffs_open.hash(&mut hasher);
    }
    testing_for(entry, detail.now).hash(&mut hasher);
    theme.hash(&mut hasher);
    hasher.finish()
}

/// How many whole seconds the entry's test runs still going have run at
/// `now`, which is what their lines say: only an entry holding one is laid
/// out again as the clock moves, and only once a second.
fn testing_for(entry: &Entry, now: Option<crate::clock::Stamp>) -> Vec<u64> {
    entry
        .calls
        .iter()
        .filter(|call| call.testing && call.running())
        .filter_map(|call| now.zip(call.began()))
        .filter_map(|(now, began)| now.since(began))
        .map(|ran| ran.as_secs())
        .collect()
}

/// Whether any call of `entry` changed more rows than a diff draws unopened.
fn holds_a_cut_diff(entry: &Entry) -> bool {
    entry
        .calls
        .iter()
        .any(|call| call.change.as_ref().is_some_and(Change::is_cut))
}

/// The switches the transcript is drawn under, as the operator last set them.
fn transcript_detail(app: &App) -> Detail {
    Detail {
        folded: app.calls_folded(),
        diffs_open: app.diffs_open(),
        // Worked out from the entries where they are drawn.
        columns: crate::calls::Columns::default(),
        grouped: false,
        now: app.stamp(),
    }
}

/// Where the view is in a transcript longer than the pane, drawn over the
/// pane's right border beside the lines it measures. A transcript that fits
/// has no scrollbar, so the border reads as a border.
fn draw_scrollbar(
    frame: &mut Frame,
    area: Rect,
    border: (u16, Border),
    extent: (usize, usize, usize),
    theme: &Theme,
) {
    let (start, lines, height) = extent;
    if lines <= height || area.height == 0 {
        return;
    }
    let (column, border) = border;
    let track = Rect::new(column, area.y, 1, area.height);
    let mut state = ScrollbarState::new(lines.saturating_sub(height))
        .viewport_content_length(height)
        .position(start);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some(border.track))
            .track_style(border.style)
            .thumb_symbol("█")
            .thumb_style(Style::new().fg(theme.hot)),
        track,
        &mut state,
    );
}

/// The frames of the working line's spinner, one per tenth of a second.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// `⠹ running Bash  cargo test · 1m 15s`: a spinner that moves while nothing
/// else on screen does, what the turn is doing, and for how long.
///
/// The time is kept whatever the width, and what the turn is doing gives way
/// to it: a line that loses its clock no longer says the session is alive.
fn working_line(activity: &Activity, width: usize, theme: &Theme) -> Line<'static> {
    let tenths = activity.elapsed.as_millis() / 100;
    let spinner = SPINNER[usize::try_from(tenths % 10).unwrap_or(0)];
    let clock = format!(" · {}", clock::spent(activity.elapsed));
    let room = width.saturating_sub(text::width(spinner) + 1 + text::width(&clock));
    Line::from(vec![
        Span::styled(format!("{spinner} "), Style::new().fg(theme.hot).bold()),
        Span::styled(
            text::truncate(&activity.doing, room),
            Style::new().fg(theme.fg),
        ),
        Span::styled(clock, Style::new().fg(theme.dim)),
    ])
}

/// What the session pane says before anything has happened in it.
fn empty_transcript(attached: bool, theme: &Theme) -> Vec<Line<'static>> {
    vec![
        Line::from(""),
        Line::from("  niobe").style(Style::new().fg(theme.hot).bold()),
        Line::from("  A terminal coding agent that keeps you aware of what is")
            .style(Style::new().fg(theme.fg)),
        Line::from("  being built and how.").style(Style::new().fg(theme.fg)),
        Line::from(""),
        Line::from("  What the agent did, decided and touched, what it is doing now")
            .style(Style::new().fg(theme.dim)),
        Line::from("  and what it cost, visible while it happens and never a guess.")
            .style(Style::new().fg(theme.dim)),
        Line::from("  You stay on the loop instead of in it.").style(Style::new().fg(theme.dim)),
        Line::from(""),
        Line::from(match attached {
            true => "  Ask for a change; the bill is on the right.",
            false => "  No backend attached. `niobe profiles` shows what is defined.",
        })
        .style(Style::new().fg(theme.dim)),
    ]
}

/// A permission prompt as the transcript's last entry: framed, indented under
/// the gutter, with the question's labels on its top border, its numbered
/// answers, and the keys that work in whatever state it is in.
///
/// The whole of the arguments is shown, wrapped rather than truncated:
/// approving a call whose arguments were cut off at the edge is approving
/// something the operator did not read. Except where the whole question is
/// taller than the `height` rows the transcript has: the transcript follows
/// its end, so the top of the question — what is asked, and the first
/// answer, which Enter confirms — would be above the pane. There the
/// arguments are cut instead, with a row saying how many lines are not shown
/// and that Ctrl+T shows them, which draws the question whole again.
fn question_lines(
    app: &App,
    ask: &Ask,
    (width, height): (usize, usize),
    theme: &Theme,
) -> Question {
    let outer = width.saturating_sub(GUTTER).min(ASK_COLUMNS);
    let inner = outer.saturating_sub(ASK_INSET);
    let border = Style::new().fg(theme.hot);
    let indent = " ".repeat(GUTTER);

    let body = question_body(app, ask, inner, theme);
    // The frame's top and bottom rows and the blank row under it.
    let framing = 3;
    let fits = app.diffs_open() || body.len() + framing <= height;
    let body = match fits {
        true => body.whole(),
        false => body.cut(height.saturating_sub(framing - 1), inner, theme),
    };

    let mut lines = vec![question_top(app, ask, outer, theme)];
    lines.extend(body.into_iter().map(|line| {
        let used = line.width();
        let mut spans = vec![Span::raw(indent.clone()), Span::styled("│ ", border)];
        spans.extend(
            line.spans
                .into_iter()
                .map(|span| span.patch_style(line.style)),
        );
        spans.push(Span::raw(" ".repeat(inner.saturating_sub(used))));
        spans.push(Span::styled(" │", border));
        Line::from(spans)
    }));
    lines.push(Line::from(vec![
        Span::raw(indent),
        Span::styled(format!("└{}┘", "─".repeat(outer.saturating_sub(2))), border),
    ]));
    let framed = Some((u16::try_from(outer).unwrap_or(u16::MAX), lines.len()));
    // The row under the frame is where its shadow falls.
    if fits {
        lines.push(Line::from(""));
    }
    Question { lines, framed }
}

/// A permission prompt drawn as the transcript's last lines.
#[derive(Default)]
struct Question {
    lines: Vec<Line<'static>>,
    /// The frame's width and its rows from the top border to the bottom one,
    /// which is what casts the shadow; `None` when nothing is asked.
    framed: Option<(u16, usize)>,
}

/// What is inside a question's frame, in the parts a question too tall for
/// its pane is cut by.
struct QuestionBody {
    /// `to run <tool>`.
    head: Line<'static>,
    /// What would run: the target, and the arguments whole under it.
    call: Vec<Line<'static>>,
    /// The numbered answers, after a blank row.
    answers: Vec<Line<'static>>,
    /// Any consequence too long for its column, said whole.
    consequences: Vec<Line<'static>>,
    /// The answer being written, and the keys.
    rest: Vec<Line<'static>>,
}

impl QuestionBody {
    fn len(&self) -> usize {
        1 + self.call.len() + self.answers.len() + self.consequences.len() + self.rest.len()
    }

    fn whole(self) -> Vec<Line<'static>> {
        let mut lines = vec![self.head];
        lines.extend(self.call);
        lines.extend(self.answers);
        lines.extend(self.consequences);
        lines.extend(self.rest);
        lines
    }

    /// The body in `rows` rows where it can be: the consequences said again
    /// go first, as each is a copy of something above it, then the call's
    /// last lines, down to its first, which is kept with what is asked and
    /// every answer.
    fn cut(mut self, rows: usize, inner: usize, theme: &Theme) -> Vec<Line<'static>> {
        let fixed = 1 + self.answers.len() + self.rest.len() + 1;
        let room = rows.saturating_sub(fixed);
        let hidden_consequences = std::mem::take(&mut self.consequences).len();
        let kept = room.clamp(1, self.call.len().max(1));
        let hidden = self.call.len().saturating_sub(kept) + hidden_consequences;
        self.call.truncate(kept);
        let said = match hidden {
            1 => "… 1 more line · Ctrl+T shows the whole call".to_owned(),
            n => format!("… {n} more lines · Ctrl+T shows the whole call"),
        };
        self.call.extend(
            text::wrap(&said, inner)
                .into_iter()
                .map(|line| Line::from(line).style(Style::new().fg(theme.hot))),
        );
        self.whole()
    }
}

/// What is inside a question's frame, `inner` columns wide: what would run,
/// the numbered answers, any consequence too long for its column said whole,
/// the answer being written, and the keys.
fn question_body(app: &App, ask: &Ask, inner: usize, theme: &Theme) -> QuestionBody {
    let plain = Style::new().fg(theme.fg);

    let head = Line::from(vec![
        Span::styled("to run ", plain),
        Span::styled(tool_label(&ask.tool), plain.bold()),
    ]);
    let mut call = Vec::new();
    for wrapped in text::wrap_exact(ask.target.as_deref().unwrap_or(&ask.input), inner) {
        call.push(Line::from(wrapped).style(plain.bold()));
    }
    if ask.target.is_some() {
        call.push(Line::from(""));
        for wrapped in text::wrap_exact(&ask.input, inner) {
            call.push(Line::from(wrapped).style(Style::new().fg(theme.dim)));
        }
    }
    let mut answers = vec![Line::from("")];
    let focus = app.ask_focus();
    let selected = app.ask_selected();
    let mut cut = Vec::new();
    for (at, answer) in app.ask_options().into_iter().enumerate() {
        let lit = focus == AskFocus::Choosing && answer == selected;
        let (row, whole) = option_row(at + 1, answer, ask, lit, inner, theme);
        answers.push(row);
        if !whole {
            cut.push((at + 1, option_words(answer, ask).1));
        }
    }
    // A consequence cut off at the edge of its column is said again whole: a
    // standing answer is a rule the operator keeps, and keeping one nobody
    // could read to its end is agreeing to something unread.
    let mut consequences = Vec::new();
    if !cut.is_empty() {
        consequences.push(Line::from(""));
        for (number, hint) in cut {
            for wrapped in text::wrap_exact(&format!("{number}. {hint}"), inner) {
                consequences.push(Line::from(wrapped).style(Style::new().fg(theme.dim)));
            }
        }
    }
    let mut rest = Vec::new();
    if focus == AskFocus::Writing {
        rest.push(Line::from(""));
        rest.extend(written_answer(app.ask_draft(), inner, theme));
    }
    rest.push(Line::from(""));
    rest.extend(key_rows(
        &ask_keys(focus, app.ask_options().len()),
        inner,
        theme,
    ));
    QuestionBody {
        head,
        call,
        answers,
        consequences,
        rest,
    }
}

/// `┌ ? claude asks ──── blocks turn 3 ┐`: who is asking at the left, in the
/// title colour — a sub-agent by the name its calls' rows give it — and what
/// is waiting on the answer at the right, dimmed.
fn question_top(app: &App, ask: &Ask, outer: usize, theme: &Theme) -> Line<'static> {
    let border = Style::new().fg(theme.hot);
    let asks = format!(" ? {} asks ", app.asker(ask));
    let blocks = match ask.turn {
        0 => "blocks this turn".to_owned(),
        turn => format!("blocks turn {turn}"),
    };
    let blocks = match app.asks_waiting() {
        0 => format!(" {blocks} "),
        n => format!(" {blocks} · {n} more waiting "),
    };
    let room = outer.saturating_sub(2);
    let asks = text::truncate(&asks, room);
    let blocks = text::truncate(&blocks, room.saturating_sub(text::width(&asks) + 1));
    let rule = room.saturating_sub(text::width(&asks) + text::width(&blocks) + 1);
    Line::from(vec![
        Span::raw(" ".repeat(GUTTER)),
        Span::styled("┌─", border),
        Span::styled(asks, Style::new().fg(theme.title).bold()),
        Span::styled("─".repeat(rule), border),
        Span::styled(blocks, Style::new().fg(theme.dim)),
        Span::styled("┐", border),
    ])
}

/// One numbered answer: the selection mark, the number, what the answer is
/// and, at the right, what choosing it does — with whether the consequence
/// fitted whole.
///
/// The label is kept whole before the consequence is, because it is what the
/// operator is choosing; the consequence takes what is left.
///
/// The selected row is inverted whole — mark, number, label and consequence —
/// so the selection is a solid bar, and it carries the `▶` besides, which is
/// what makes it legible without colour.
fn option_row(
    number: usize,
    answer: Answer,
    ask: &Ask,
    lit: bool,
    width: usize,
    theme: &Theme,
) -> (Line<'static>, bool) {
    let (label, hint) = option_words(answer, ask);
    let lead = format!("{} {number}. ", if lit { "▶" } else { " " });
    let rest = width.saturating_sub(text::width(&lead));
    let label = text::truncate(&label, rest);
    let hint_room = rest.saturating_sub(text::width(&label) + 2);
    // A hint holding a tab or a line break is drawn on its row with each
    // turned into a space or a `↵`, which is not the rule as it is kept, so
    // it counts as cut however short it is.
    let whole = text::width(&hint) <= hint_room && !hint.contains(['\t', '\n', '\r']);
    let hint = text::truncate(&hint, hint_room);
    let gap = rest.saturating_sub(text::width(&label) + text::width(&hint));

    let (base, dim) = match lit {
        true => {
            let bar = Style::new().bg(theme.title).fg(theme.pane_bg).bold();
            (bar, bar)
        }
        false => (Style::new().fg(theme.fg), Style::new().fg(theme.dim)),
    };
    let row = Line::from(vec![
        Span::styled(lead, base),
        Span::styled(label, base),
        Span::styled(" ".repeat(gap), base),
        Span::styled(hint, dim),
    ]);
    (row, whole)
}

/// What an answer is called, and what choosing it does.
///
/// Both are Niobe's own words, not the agent's: the backend offers no answers
/// of its own for a permission prompt. So the consequences say what Niobe
/// does — `niobe saves …` for a standing answer — or what happens to the call,
/// and never what the agent will say or think about it.
fn option_words(answer: Answer, ask: &Ask) -> (String, String) {
    match answer {
        Answer::Once => ("Allow once".to_owned(), "this call only".to_owned()),
        Answer::AlwaysTool => (
            format!("Always allow {}", tool_label(&ask.tool)),
            ask.tool_rule()
                .map(|rule| format!("niobe saves {rule}"))
                .unwrap_or_default(),
        ),
        Answer::AlwaysTarget => (
            "Always allow this target".to_owned(),
            ask.target_rule()
                .map(|rule| format!("niobe saves {rule}"))
                .unwrap_or_default(),
        ),
        Answer::No => ("Deny".to_owned(), "the call does not run".to_owned()),
    }
}

/// The answer being written in place of the numbered ones, wrapped, with a
/// cursor at its end and a line saying where it goes.
fn written_answer(draft: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = text::wrap(&format!("› {draft}▏"), width)
        .into_iter()
        .map(|wrapped| Line::from(wrapped).style(Style::new().fg(theme.hot)))
        .collect();
    lines.extend(
        text::wrap(
            "Sent with a refusal: the call does not run, and the agent is given these words.",
            width,
        )
        .into_iter()
        .map(|wrapped| Line::from(wrapped).style(Style::new().fg(theme.dim).italic())),
    );
    lines
}

/// The keys that work at a question, as (key, what it does), for the state it
/// is in.
fn ask_keys(focus: AskFocus, options: usize) -> Vec<(String, &'static str)> {
    match focus {
        AskFocus::Choosing => vec![
            ("↑↓".to_owned(), "move"),
            (format!("1-{options}"), "jump"),
            ("Enter".to_owned(), "confirm"),
            ("Tab".to_owned(), "type your own"),
            ("Esc".to_owned(), "decide later"),
        ],
        AskFocus::Writing => vec![
            ("Enter".to_owned(), "send"),
            ("Esc".to_owned(), "back to the options"),
        ],
        AskFocus::Deferred => vec![
            (String::new(), "put off · the turn is still waiting on it"),
            ("Esc".to_owned(), "answer it"),
        ],
    }
}

/// Key hints on as many rows as the width needs, each key accented and bold,
/// broken between hints rather than inside one.
fn key_rows(keys: &[(String, &str)], width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let key_style = Style::new().fg(theme.hot).bold();
    let word_style = Style::new().fg(theme.dim);
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut used = 0;
    for (key, does) in keys {
        let cells = match key.is_empty() {
            true => text::width(does),
            false => text::width(key) + 1 + text::width(does),
        };
        let sep = if used == 0 { 0 } else { 3 };
        if used > 0 && used + sep + cells > width {
            rows.push(Vec::new());
            used = 0;
        }
        let Some(row) = rows.last_mut() else {
            break;
        };
        if used > 0 {
            row.push(Span::styled(" · ", word_style));
            used += 3;
        }
        if !key.is_empty() {
            row.push(Span::styled(key.clone(), key_style));
            row.push(Span::raw(" "));
        }
        row.push(Span::styled((*does).to_owned(), word_style));
        used += cells;
    }
    rows.into_iter().map(Line::from).collect()
}

/// One transcript entry, wrapped to the pane: a head line and its body.
fn entry_lines(entry: &Entry, width: usize, detail: Detail, theme: &Theme) -> Vec<Line<'static>> {
    if !entry.calls.is_empty() {
        return crate::calls::lines(entry, width, detail, theme);
    }
    match &entry.kind {
        EntryKind::Turn(rule) => return crate::turns::lines(rule, width, theme),
        EntryKind::SubAgent => return crate::calls::said(entry, width, detail, theme),
        EntryKind::User
        | EntryKind::Agent
        | EntryKind::Tool
        | EntryKind::Failure
        | EntryKind::Notice => {}
    }
    let colour = entry.kind.colour(theme);
    let body_width = width.saturating_sub(GUTTER);

    let mut head = vec![
        Span::styled(
            format!("{} ", entry.kind.glyph()),
            Style::new().fg(colour).bold(),
        ),
        Span::styled(entry.head.clone(), Style::new().fg(colour).bold()),
    ];
    let room = body_width.saturating_sub(text::width(&entry.head) + 2);
    // Anything else of an agent's — a refusal of its call — says the agent's
    // tag the way its calls' rows do.
    let agent = entry
        .agent
        .as_ref()
        .map(|agent| crate::calls::agent_tag(agent, &entry.meta, room))
        .filter(|tag| !tag.is_empty());
    if agent.is_some() || !entry.meta.is_empty() {
        head.push(Span::raw("  "));
    }
    let room = match agent {
        Some(tag) => {
            let left = room.saturating_sub(text::width(&tag));
            head.push(Span::styled(tag, Style::new().fg(theme.agent)));
            left
        }
        None => room,
    };
    if !entry.meta.is_empty() {
        head.push(Span::styled(
            text::truncate(&entry.meta, room),
            Style::new().fg(theme.dim),
        ));
    }

    let mut lines = vec![Line::from(head)];
    lines.extend(
        body_lines(entry, body_width, theme)
            .into_iter()
            .map(|line| {
                let mut spans = vec![Span::raw(" ".repeat(GUTTER))];
                spans.extend(line.spans);
                Line::from(spans)
            }),
    );
    lines.push(Line::from(""));
    lines
}

/// An entry's body, wrapped to the pane.
///
/// The assistant writes markdown and is drawn from it. The operator's own
/// words and the shell's notices are shown as typed: a prompt with an asterisk
/// in it means the asterisk. A tool call carries its whole story on the head
/// line, so its empty body is no lines at all rather than a blank one.
fn body_lines(entry: &Entry, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    match entry.kind {
        EntryKind::Agent | EntryKind::SubAgent => {
            crate::markdown::render(entry.body.trim_end(), width, theme)
        }
        // The rule carries its figures on its one line and has no body.
        EntryKind::Turn(_) => Vec::new(),
        EntryKind::User | EntryKind::Tool | EntryKind::Failure | EntryKind::Notice => {
            text::wrap(entry.body.trim_end(), width)
                .into_iter()
                .map(|wrapped| Line::from(wrapped).style(Style::new().fg(theme.fg)))
                .collect()
        }
    }
}

/// The rows the Usage pane needs: its frame, whatever windows the session has
/// been told about, a row per model and the cache, the session's cost and its
/// budget, and how full the context is.
///
/// Read before the pane is drawn, because the column above it is laid out
/// from it — the pane takes the rows its figures need and leaves the rest to
/// the panes that grow with the session.
fn usage_height(app: &App) -> u16 {
    let money = money_rows(app);
    let windows = window_rows(app);
    let spend = spend_rows(app);
    let context = context_rows(app);
    // Two rows of border; the windows, the
    // models and the money in the order the shape puts them, with a rule
    // between the windows and the models on a plan wherever both have rows,
    // and always under the money on a metered account; then the context,
    // under a rule of its own, where there is one to show.
    let blocks = match metered(app) {
        true => money + windows + 1 + spend,
        false => windows + usize::from(windows > 0 && spend > 0) + spend + money,
    };
    let rows = 2 + blocks + usize::from(context > 0) + context;
    u16::try_from(rows).unwrap_or(u16::MAX)
}

/// Whether the session is billed by use, which makes money the pane's
/// headline.
///
/// Only where something said so and nothing said otherwise. A session
/// nothing has said of keeps the plan's order, windows first where there are
/// any, and shows no dollar figure at all; one billed both ways keeps it too,
/// and its models' rows carry no cost, since each model's figure runs across
/// both parts and only the metered one is money: see [`money_lines`].
fn metered(app: &App) -> bool {
    Billed::of(app.session()) == Billed::Metered
}

/// How wide the Usage pane's label column is, drawn `width` columns wide:
/// the widest label any of its rows carries and a column of gap, or what is
/// left once every row's figures are paid for, whichever is less.
///
/// Every block of the pane — the windows, the models and the cache, the
/// money on a plan, the context — draws its rows in one grid of a label, a
/// share, a meter and what follows it, so the shares stand in one column and
/// the meters start in one. The column is as wide as the widest label the
/// pane is drawing, so a session on one short-named model is not drawn as if
/// it ran on the longest. A label is what gives way where the pane is too
/// narrow for it: a figure pushed past the pane's edge is cut by it, and
/// `200k` cut to `20` is a different number.
fn usage_label_column(app: &App, width: usize) -> usize {
    let room = width.saturating_sub(usage_figure_columns(app)).max(1);
    usage_label_widest(app).saturating_add(1).min(room)
}

/// The widest label the Usage pane's rows carry.
fn usage_label_widest(app: &App) -> usize {
    let windows = app.session().usage_windows().map_or(0, |windows| {
        [
            (windows.five_hour.is_some(), "5h"),
            (windows.seven_day.is_some(), "7d"),
            (windows.using_overage, "extra"),
        ]
        .into_iter()
        .filter(|(shown, _)| *shown)
        .map(|(_, label)| text::width(label))
        .max()
        .unwrap_or(0)
    });
    // With no model to list, the block is one line saying so, not a row.
    let spent = models(app);
    let models = match spent.is_empty() {
        true => 0,
        false => usage::labels(spent.iter().map(|(model, _)| model.as_str()))
            .iter()
            .map(|label| text::width(label))
            .chain([text::width(CACHE_LABEL)])
            .max()
            .unwrap_or(0),
    };
    let context = match app.session().context() {
        Some(_) => text::width(CONTEXT_LABEL),
        None => 0,
    };
    windows.max(models).max(context)
}

/// The most any row of the Usage pane needs beside its label with no meter
/// drawn: the share, its gap and the figures after it, each whole.
///
/// The context's window figure is counted, though the row can drop it: a
/// label is cut before a figure is given up.
fn usage_figure_columns(app: &App) -> usize {
    let share = SHARE_COLUMNS + SHARE_GAP.len();
    let windows = app.session().usage_windows().map_or(0, |windows| {
        let reported = [windows.five_hour, windows.seven_day]
            .into_iter()
            .flatten()
            .map(|window| share + reset_clause(app, &window).as_deref().map_or(0, text::width))
            .max()
            .unwrap_or(0);
        let extra = match windows.using_overage {
            true => text::width(NO_FIGURE) + text::width(OVERAGE_ON),
            false => 0,
        };
        reported.max(extra)
    });
    let spend = match models(app).is_empty() {
        true => 0,
        false => share + MODEL_TOKENS + model_cost_columns(app),
    };
    let context = app
        .session()
        .context()
        .map_or(0, |context| match context_window(app, context) {
            Some(window) => share + text::width(&context_figures(context.tokens, window)),
            None => text::width(&compact(context.tokens)),
        });
    windows.max(spend).max(context)
}

/// A label set in the Usage pane's label column `columns` wide: cut to leave
/// the column of gap, and padded to the column.
fn usage_label(label: &str, columns: usize) -> String {
    text::pad(&text::truncate(label, columns.saturating_sub(1)), columns)
}

/// What a share is drawn in: three columns for the figure and the sign, one
/// more than a full window needs so that a window reported past its end
/// still lines up with the ones that are not.
const SHARE_COLUMNS: usize = 4;

/// What stands between a share and its meter.
const SHARE_GAP: &str = " ";

/// The longest a meter is drawn. Twelve cells read a share to within a tenth,
/// which is as fine as a window is worth reading, and it leaves the reset time
/// beside it room in a pane forty columns wide.
const METER_CELLS: usize = 12;

/// A share of a window as its column draws it: a percent, no wider than the
/// column holds, and an em dash for a level that is not one — below nothing,
/// or not a number — rather than a `0%` nobody measured.
fn window_share(utilization: f64) -> String {
    if !utilization.is_finite() || utilization < 0.0 {
        return format!("{:>3} ", "—");
    }
    format!("{:>3}%", crate::app::percent(utilization).min(999))
}

/// How many rows the plan's windows take: one per window a backend reported,
/// and one more where the plan has started spending beyond its flat fee.
///
/// The pane is sized from this before it is drawn, so it has to agree with
/// [`window_lines`] exactly; a test holds the two together.
fn window_rows(app: &App) -> usize {
    app.session().usage_windows().map_or(0, |windows| {
        usize::from(windows.five_hour.is_some())
            + usize::from(windows.seven_day.is_some())
            + usize::from(windows.using_overage)
    })
}

/// When a window comes back, as the row says it: ` resets 16:40` later today
/// and ` resets Tue 09:00` on another day.
///
/// `None` where there is no reset to name — the backend reported the share
/// without one or with one past what the clock holds, the machine named no
/// timezone, or the reset has already come around, which is what a recorded session read back a day later has. The row
/// then carries the share alone rather than a time that is no longer true.
fn reset_clause(app: &App, window: &UsageWindow) -> Option<String> {
    let at = window.resets_at?;
    let when = clock::upcoming(app.stamp()?, app.moment(at)?)?;
    Some(format!(" resets {when}"))
}

/// The plan's windows, which on a flat-rate plan are what a budget is: a
/// meter for each one a backend reported, the share beside it, and when it
/// comes back.
///
/// A window no backend reported is not a window at zero — it draws no row at
/// all. The meter shrinks before the reset time does: how much of a window is
/// gone is worth a cell more or less, and `resets Tue 09:00` cut to
/// `resets Tue 09` is a different time.
fn window_lines(app: &App, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let Some(windows) = app.session().usage_windows() else {
        return Vec::new();
    };

    let reported: Vec<(&str, UsageWindow, Option<String>)> =
        [("5h", windows.five_hour), ("7d", windows.seven_day)]
            .into_iter()
            .filter_map(|(label, window)| window.map(|window| (label, window)))
            .map(|(label, window)| {
                let clause = reset_clause(app, &window);
                (label, window, clause)
            })
            .collect();

    let clause_columns = reported
        .iter()
        .filter_map(|(_, _, clause)| clause.as_deref().map(text::width))
        .max()
        .unwrap_or(0);
    let labels = usage_label_column(app, width);
    let cells = width
        .saturating_sub(labels + SHARE_COLUMNS + SHARE_GAP.len() + clause_columns)
        .min(METER_CELLS);

    let dim = Style::new().fg(theme.dim);
    let mut lines: Vec<Line<'static>> = reported
        .into_iter()
        .map(|(label, window, clause)| {
            let (filled, track) = meter(window.utilization, cells);
            let style = window_style(&window, theme);
            Line::from(vec![
                Span::styled(usage_label(label, labels), dim),
                Span::styled(window_share(window.utilization), style.bold()),
                Span::raw(SHARE_GAP),
                Span::styled(filled, style),
                Span::styled(track, dim),
                Span::styled(clause.unwrap_or_default(), dim),
            ])
        })
        .collect();

    // What the extra costs is a figure no backend reports: the CLI says that
    // the plan is spending beyond its flat fee and never how much, so the
    // money side is an em dash. A `$0.00` there would be a figure nobody
    // measured, and the one it would be mistaken for is zero.
    //
    // The row is absent rather than `off` where the flag is not set: the flag
    // is "spending extra, or the backend did not say", so drawing `off` would
    // promise something nothing reported.
    if windows.using_overage {
        lines.push(Line::from(vec![
            Span::styled(usage_label("extra", labels), dim),
            Span::styled(NO_FIGURE, dim),
            Span::styled(OVERAGE_ON, Style::new().fg(theme.hot).bold()),
        ]));
    }

    lines
}

/// What the overage row draws where a figure would be: no backend reports
/// what the extra costs.
const NO_FIGURE: &str = "—";

/// What the overage row says of the plan spending beyond its flat fee.
const OVERAGE_ON: &str = " · on";

/// What a model's tokens get: the widest figure [`compact`] produces for a
/// session under a hundred million tokens (`999k`, `12.3M`) and a column of
/// gap before it, which is drawn whatever the figure's width.
const MODEL_TOKENS: usize = 6;

/// The least a model's cost is drawn in on a metered account: the widest
/// figure under a hundred dollars the pane prints for one (`≥~$12.34`) and a
/// column of gap before it, so that the costs of a session stand in one
/// column as they grow. A wider one widens the column: see
/// [`model_cost_columns`].
const MODEL_COST: usize = 9;

/// What the models' costs are drawn in this frame: [`MODEL_COST`], or the
/// widest cost a row draws and its gap where that is wider, so that no cost
/// is cut by the pane's edge. Nothing where the account is not metered and
/// the rows carry no cost.
fn model_cost_columns(app: &App) -> usize {
    if !metered(app) {
        return 0;
    }
    let totals = app.session().totals();
    models(app)
        .iter()
        .map(|(model, _)| text::width(&model_cost(totals, model, app.prices())) + 1)
        .fold(MODEL_COST, usize::max)
}

/// The label the cache row carries. It names what its figure means, because a
/// hit rate and a share of the session are two different questions and the
/// column they are drawn in is the same.
const CACHE_LABEL: &str = "cache hit";

/// How many rows the tokens block takes: one per model that spent something,
/// one for the cache, or one line saying the session has been billed for
/// nothing yet.
///
/// The pane is sized from this before it is drawn, so it has to agree with
/// [`spend_lines`] exactly; a test holds the two together.
fn spend_rows(app: &App) -> usize {
    match models(app).len() {
        0 => 1,
        rows => rows + 1,
    }
}

/// The models that spent something, largest first, with their tokens.
///
/// A model that spent nothing is not a model at 0% — it gets no row. Ties go
/// to the id, so the order of two models that spent alike does not depend on
/// the order their records arrived in.
fn models(app: &App) -> Vec<(String, u64)> {
    let mut spent: Vec<(String, u64)> = app
        .session()
        .totals()
        .tokens_by_model
        .iter()
        .filter(|(_, tokens)| **tokens > 0)
        .map(|(model, tokens)| (model.clone(), *tokens))
        .collect();
    spent.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    spent
}

/// Who spent the session's tokens, and what the cache saved.
///
/// A row per model, the share it spent of the session's own tokens, and how
/// much — then the cache row, which is a **hit rate** rather than a share of
/// anything on the rows above it. Two meanings in one column, so the cache row
/// says which it is in its label and is drawn in a colour of its own.
///
/// Every model row is drawn in the same colour. The busiest is not a warning —
/// [`Theme::hot`] means *nearly gone* everywhere else in the shell, and a model
/// that spent the most has not gone wrong — and the bar beside it already says
/// which spent most.
///
/// On a metered account each model's row also says what it cost, labelled as
/// the session's cost is. The cache row has no cost of its own to show: its
/// reads are billed in the rows above it.
fn spend_lines(app: &App, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let dim = Style::new().fg(theme.dim);
    let spent = models(app);
    if spent.is_empty() {
        return vec![Line::from("no tokens reported yet").style(dim)];
    }

    let columns = usage_label_column(app, width);
    let labels = usage::fitted(
        &usage::labels(spent.iter().map(|(model, _)| model.as_str())),
        columns.saturating_sub(1),
    );
    // The meter gives up cells until the row fits, the way a window row's
    // does: how much of a bar is drawn is worth less than the figure beside it.
    let costed = metered(app);
    let cost_columns = model_cost_columns(app);
    let cells = width
        .saturating_sub(columns + SHARE_COLUMNS + SHARE_GAP.len() + MODEL_TOKENS + cost_columns)
        .min(METER_CELLS);

    // A label, a figure, a meter of the share that figure rounds, a count, and
    // on a metered account a cost. The meter is drawn from the share itself
    // rather than from the whole percent beside it, so a model that spent too
    // little to round up to one still keeps the cell the meter gives anything
    // above nothing.
    let row = |label: &str,
               figure: String,
               share: f64,
               count: String,
               cost: Option<String>,
               style: Style| {
        let (filled, track) = meter(share, cells);
        let mut spans = vec![
            Span::styled(usage_label(label, columns), dim),
            Span::styled(figure, style.bold()),
            Span::raw(SHARE_GAP),
            Span::styled(filled, style),
            Span::styled(track, dim),
            Span::styled(format!(" {count:>width$}", width = MODEL_TOKENS - 1), dim),
        ];
        if let Some(cost) = cost {
            spans.push(Span::styled(
                format!(" {cost:>width$}", width = cost_columns - 1),
                Style::new().fg(theme.fg),
            ));
        }
        Line::from(spans)
    };

    let totals = app.session().totals();
    let session_tokens = totals.tokens().max(1);
    let percents = usage::shares(&spent.iter().map(|(_, tokens)| *tokens).collect::<Vec<_>>());
    let models = spent.iter().filter(|(_, tokens)| *tokens > 0).count();
    let mut lines: Vec<Line<'static>> = spent
        .iter()
        .zip(labels)
        .zip(percents)
        .map(|(((model, tokens), label), percent)| {
            row(
                &label,
                share_label(percent, *tokens > 0, *tokens > 0 && models > 1),
                *tokens as f64 / session_tokens as f64,
                compact(*tokens),
                costed.then(|| model_cost(totals, model, app.prices())),
                Style::new().fg(theme.fg),
            )
        })
        .collect();

    // The reads are beside the rate so that it can be read as what it came
    // from, and an em dash stands where nothing has been eligible for a cache
    // yet: a `0%` there would say the cache was offered the work and missed.
    let hit = usage::cache_hit_rate(totals);
    lines.push(row(
        CACHE_LABEL,
        match hit {
            Some(hit) => share_label(crate::app::percent(hit), hit > 0.0, hit < 1.0),
            None => format!("{:>4}", "—"),
        },
        hit.unwrap_or(0.0),
        match hit {
            Some(_) => compact(totals.cache_read),
            None => String::new(),
        },
        None,
        Style::new().fg(theme.add),
    ));
    lines
}

/// A share as its column draws it, four cells wide. One that rounds to
/// nothing but is not nothing reads `<1%`, and one that rounds to the whole
/// but is not all of it reads `>99%`: a `0%` beside tokens, or a `100%`
/// beside a second model, says something that is not so.
fn share_label(percent: u64, some: bool, not_all: bool) -> String {
    match percent {
        0 if some => " <1%".to_owned(),
        100.. if not_all => ">99%".to_owned(),
        _ => format!("{percent:>3}%"),
    }
}

/// The label the context row carries.
const CONTEXT_LABEL: &str = "context";

/// How many rows the context takes: one once the main agent has sent a
/// request, and none before — nothing has been measured, and a row at `0%`
/// would say the context is empty.
///
/// The pane is sized from this before it is drawn, so it has to agree with
/// [`context_lines`] exactly; a test holds the two together.
fn context_rows(app: &App) -> usize {
    usize::from(app.session().context().is_some())
}

/// How big the window the last request went into is: what the backend
/// reported for the model, or else what the provider published for it.
///
/// The backend's figure wins because it is the window the session actually
/// has — a plan or a flag can select another than the published default. With
/// neither, there is no window, and nothing is assumed in its place.
fn context_window(app: &App, context: &Context) -> Option<u64> {
    context
        .window
        .or_else(|| app.prices()?.context_window(&context.model))
}

/// How full the context is: the prompt the main agent's last request sent,
/// against the window it went into.
///
/// The row claims the last request, the reading Claude Code's own context
/// figure has: a request's input and cache tokens against the window. The
/// next request is larger by the last reply and whatever comes back from the
/// calls it made, and nothing reports that until it goes.
///
/// A model whose window nobody knows gets the size alone — no bar, and no
/// share of a window assumed for it. A context past its window fills the bar,
/// and the figure says by how much, up to the most its column holds.
fn context_lines(app: &App, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let Some(context) = app.session().context() else {
        return Vec::new();
    };
    let dim = Style::new().fg(theme.dim);
    let labels = usage_label_column(app, width);
    let label = Span::styled(usage_label(CONTEXT_LABEL, labels), dim);
    let Some(window) = context_window(app, context) else {
        return vec![Line::from(vec![
            label,
            Span::styled(compact(context.tokens), Style::new().fg(theme.fg)),
        ])];
    };

    let share = context.tokens as f64 / window.max(1) as f64;
    // The bar goes first and then the window's size, whole: `12k / 20` is
    // not what `12k / 200k` reads as with a cell less.
    let beside = labels + SHARE_COLUMNS + SHARE_GAP.len();
    let figures = context_figures(context.tokens, window);
    let (figures, cells) = match beside + text::width(&figures) <= width {
        true => {
            let cells = width - beside - text::width(&figures);
            (figures, cells.min(METER_CELLS))
        }
        false => (format!(" {}", compact(context.tokens)), 0),
    };
    let (filled, track) = meter(share, cells);
    let style = match share >= BUDGET_SHOWN_HOT {
        true => Style::new().fg(theme.hot),
        false => Style::new().fg(theme.fg),
    };
    vec![Line::from(vec![
        label,
        Span::styled(window_share(share), style.bold()),
        Span::raw(SHARE_GAP),
        Span::styled(filled, style),
        Span::styled(track, dim),
        Span::styled(figures, dim),
    ])]
}

/// What the context row says beside its meter: the size the last request
/// sent, and the size of the window it went into.
fn context_figures(tokens: u64, window: u64) -> String {
    format!(" {} / {}", compact(tokens), compact(window))
}

/// The rule the mock draws between the pane's blocks. Its blocks answer
/// different questions — what the plan has left, and who spent the session's
/// tokens — and without it they read as one list.
fn divider(width: usize, theme: &Theme) -> Line<'static> {
    Line::from("─".repeat(width)).style(Style::new().fg(theme.frame))
}

fn draw_usage(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    // Usage has nothing to scroll and nothing to fold, so it never has the
    // keyboard.
    let block = pane("Usage", Border::of(false, theme), theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let mut lines = usage_lines(app, usize::from(inner.width), theme);
    lines.truncate(usize::from(inner.height));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Everything the Usage pane draws inside its frame, in order.
///
/// The pane is sized from [`usage_height`] before it is drawn, so the two have
/// to agree exactly; a test holds them together.
fn usage_lines(app: &App, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let money = money_lines(app, width, theme);
    let windows = window_lines(app, width, theme);
    let spend = spend_lines(app, width, theme);

    // What runs out comes first. On a flat-rate plan that is the windows, and
    // the money is a row under the tokens; on a metered account it is the
    // money, and a window — which such an account reports only where its
    // provider has one — follows it.
    let mut lines = Vec::new();
    match metered(app) {
        true => {
            lines.extend(money);
            lines.extend(windows);
            lines.push(divider(width, theme));
            lines.extend(spend);
        }
        false => {
            let ruled = !windows.is_empty() && !spend.is_empty();
            lines.extend(windows);
            if ruled {
                lines.push(divider(width, theme));
            }
            lines.extend(spend);
            lines.extend(money);
        }
    }

    // Last, under a rule: how full the context is answers a question about
    // the next request rather than about what has been spent.
    let context = context_lines(app, width, theme);
    if !context.is_empty() {
        lines.push(divider(width, theme));
        lines.extend(context);
    }
    lines
}

/// How many rows the money takes: the session's cost, except on a plan, and
/// the budget where the session runs against one.
fn money_rows(app: &App) -> usize {
    let cost = match Billed::of(app.session()) {
        Billed::Plan => 0,
        Billed::Metered | Billed::Both | Billed::Unknown => 1,
    };
    cost + usize::from(app.budget().is_some())
}

/// What the session cost, labelled for what is known about it, and what it
/// has spent of its budget.
///
/// What is drawn depends on how the session is billed. On a metered account
/// the cost is the bill, drawn as the headline. On a plan no money moves with
/// the work and what runs out is the windows above, so there is no row: the
/// figure the backend prints is what the work would have cost on the API,
/// which tells a plan user nothing the tokens and the windows do not. Where
/// nothing has said which it is, the row says so rather than showing a figure
/// it cannot vouch for. A session billed both ways — resumed under a profile
/// billed differently — shows what its metered part cost, named as a part.
fn money_lines(app: &App, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let dim = Style::new().fg(theme.dim);
    let billed = Billed::of(app.session());
    let mut lines = Vec::new();
    match billed {
        Billed::Both => lines.push(Line::from(vec![
            Span::styled(format!("{METERED_PART} "), dim),
            Span::styled(
                cost_of(app.session().metered_totals(), app.prices()),
                Style::new().fg(theme.fg),
            ),
        ])),
        Billed::Metered => {
            let cost = session_cost(app.session(), app.prices());
            let mut spans = vec![
                Span::styled("session ", dim),
                Span::styled(cost.clone(), Style::new().fg(theme.hot).bold()),
            ];
            let rate = spend_rate(app).map(|rate| format!(" · {rate}/h worked"));
            if let Some(rate) = rate.filter(|rate| {
                text::width("session ") + text::width(&cost) + text::width(rate) <= width
            }) {
                spans.push(Span::styled(rate, dim));
            }
            lines.push(Line::from(spans));
        }
        Billed::Plan => {}
        Billed::Unknown => lines.push(Line::from(vec![
            Span::styled("session ", dim),
            Span::styled("—", Style::new().fg(theme.fg)),
            Span::styled(" · billing not known", dim),
        ])),
    }
    if let Some(budget) = app.budget() {
        lines.push(budget_line(app, budget, width, theme));
    }
    lines
}

/// The `budget ~$1.15/$5.00` row: what the session has spent, labelled as
/// the session's cost is, against the budget it was given.
///
/// An em dash where nothing was reported and nothing could be valued, never a
/// `$0.00` that would read as a session that has cost nothing. A session
/// billed both ways counts its metered part, and says so as its cost does.
/// Where nothing said how the session is billed, the spend is an em dash as
/// the session's cost is, and on a plan the row draws no dollar figure at all:
/// it shows none the pane withholds, though it still turns hot as what the
/// backend counts nears the budget, as the warning in the transcript still
/// comes.
fn budget_line(app: &App, budget: f64, width: usize, theme: &Theme) -> Line<'static> {
    let billed = Billed::of(app.session());
    let spent = known_spend(billed.counted(app.session()), app.prices());
    let hot = spent
        .as_ref()
        .is_some_and(|(_, usd)| *usd >= budget * BUDGET_SHOWN_HOT);
    let figure = match billed {
        Billed::Metered | Billed::Both => spent.as_ref().map(|(text, _)| text.as_str()),
        Billed::Plan | Billed::Unknown => None,
    }
    .unwrap_or("—");
    let row = match billed {
        Billed::Plan => format!("budget {figure}"),
        Billed::Metered | Billed::Both | Billed::Unknown => {
            let amounts = format!("{figure}/{}", dollars(budget));
            let heads: &[&'static str] = match billed {
                Billed::Both => &["metered part budget ", "metered budget "],
                Billed::Metered | Billed::Plan | Billed::Unknown => &["budget "],
            };
            let head = heads
                .iter()
                .find(|head| text::width(head) + text::width(&amounts) <= width)
                .or(heads.last())
                .copied()
                .unwrap_or("budget ");
            format!("{head}{amounts}")
        }
    };
    Line::from(row).style(match hot {
        true => Style::new().fg(theme.hot).bold(),
        false => Style::new().fg(theme.fg),
    })
}

/// The most columns a tool's name gets in the mix; the column is as wide as
/// the widest family it lists, up to this.
const BAR_NAME: usize = 16;

/// Columns a tool's count gets, right-aligned.
const BAR_COUNT: usize = 4;

/// Columns a family's own failure count gets beside its bar — ` ✗ 3`.
const BAR_FAILED: usize = 4;

/// The longest a bar is drawn. The bars compare the tools with one another,
/// which a dozen cells does as well as the whole pane, and a bar across the
/// pane is a block of colour the count beside it gets lost in.
const BAR_CELLS: usize = 12;

/// One `Notion·*  16 ▓▓ ✗ 3` row: the family in a column `name` wide, its
/// calls, a bar for how it compares with the busiest family, and its own
/// failures where it has any.
///
/// The busiest family's bar is drawn in the hot colour and the rest dim: the
/// bars are read for which tool the session leans on, and one bright bar
/// answers that before any is measured against another.
///
/// The failures are on the family's own row because the header's count says
/// only that the session failed three calls, not which tool it kept failing
/// at. A family with no failures carries no column at all — `✗ 0` is a
/// reassurance dressed as a measurement.
fn bar_line(
    (family, name): (&str, usize),
    count: u64,
    failed: u64,
    busiest: u64,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    // At least one cell for a tool that ran, so the least used still shows.
    let filled = match busiest {
        0 => 0,
        _ => ((count as usize * width) / busiest as usize).max(1),
    };
    let failures = match failed {
        0 => String::new(),
        failed => format!(" ✗ {failed}"),
    };
    let bar = match count == busiest {
        true => theme.hot,
        false => theme.dim,
    };

    Line::from(vec![
        Span::styled(
            format!(
                "{ROW_INDENT}{}",
                text::pad(&text::truncate(family, name), name)
            ),
            Style::new().fg(theme.dim),
        ),
        Span::styled(format!("{count:>BAR_COUNT$} "), Style::new().fg(theme.fg)),
        Span::styled(
            format!("{:<width$}", "▓".repeat(filled.min(width))),
            Style::new().fg(bar),
        ),
        Span::styled(failures, Style::new().fg(theme.del)),
    ])
}

/// The Activity pane: the sub-agents the session spawned and the tools it
/// called.
///
/// One column for what the agent is doing. It scrolls
/// and its sections fold on the same mechanism the Changes pane uses.
fn draw_activity(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    draw_scrolling_pane(
        frame,
        area,
        app,
        Pane::Activity,
        "Activity",
        theme,
        activity_rows,
    );
}

/// Every row the Activity pane has, folded sections included as their header
/// alone.
fn activity_rows(app: &App, width: usize, theme: &Theme) -> PaneRows {
    let session = app.session();
    let mut rows = PaneRows::default();
    rows.section(Section::SubAgents, agent_rows(app, session, width, theme));
    rows.section(Section::Tools, tool_rows(app, session, width, theme));
    rows
}

/// What the sub-agents section says about itself: how many are running, how
/// many were spawned in all, and how many failed.
///
/// A count that is zero is left out rather than drawn: `0 failed` is a line
/// the operator has to read to learn nothing. Where the pane is narrow what is
/// running now is kept longest, and what failed after it: the total spawned,
/// the cancellations and the agents the session ended under are history the
/// rows below already tell.
fn agent_summary(session: &SessionState, theme: &Theme) -> Vec<Figure> {
    agent_figures(
        session.running_agents().len(),
        session.agents_spawned(),
        session.agents_failed(),
        (session.agents_cancelled(), session.agents_interrupted()),
        theme,
    )
}

/// The section's figures, from its counts: the cancelled and the cut short
/// travel together because they are drawn at the same rank.
fn agent_figures(
    running: usize,
    spawned: u64,
    failed: u64,
    (cancelled, interrupted): (u64, u64),
    theme: &Theme,
) -> Vec<Figure> {
    let dim = Style::new().fg(theme.dim);
    let mut figures = vec![
        Figure::lead(
            0,
            Span::styled(format!("{running} running"), Style::new().fg(theme.fg)),
        ),
        Figure::after(" · ", 3, Span::styled(format!("{spawned} spawned"), dim)),
    ];
    for (count, word, rank) in [
        (failed, "failed", 1),
        (cancelled, "cancelled", 2),
        (interrupted, "cut short", 2),
    ] {
        if count > 0 {
            figures.push(Figure::after(
                " · ",
                rank,
                Span::styled(format!("{count} {word}"), dim),
            ));
        }
    }
    figures
}

/// A running sub-agent per row: the glyph, its tag — the word the
/// transcript's rows name it by — what it was spawned to do, the model it
/// answers with, and how long it has run on the right; then, on a row of its
/// own, what it is doing.
///
/// Only what is running is listed. An agent that finished, failed or was
/// cancelled, or that the session ended under, leaves the list, and the
/// header's counts are where it went: in a long session a list of every
/// agent spawned buries the ones at work under the ones that stopped.
///
/// The tag and the model each have a column as wide as the widest in the
/// list, so the tasks start in one column and the times end in another. The
/// model is drawn only where the agent's own messages named one, by its
/// family where no other listed agent's model shares it.
///
/// The sub-line is the call the agent has open, as its row in the transcript
/// names it, and otherwise the last thing its backend reported it doing. It
/// is drawn only where there is one: an empty `└` on every agent that said
/// nothing would be half the pane's rows saying nothing.
fn agent_rows(
    app: &App,
    session: &SessionState,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let folded = app.folded(Section::SubAgents);
    let mut rows = vec![section_header(
        folded,
        "Sub-agents",
        agent_summary(session, theme),
        width,
        theme,
    )];
    if folded {
        return rows;
    }
    if app.agents().is_empty() {
        rows.push(Line::from("  none spawned").style(Style::new().fg(theme.dim)));
        return rows;
    }

    // Tagged among every agent the session spawned, so that an agent keeps
    // the tag the transcript names it by when another one stops.
    let (running, tags): (Vec<&SubAgent>, Vec<String>) = app
        .agents()
        .iter()
        .zip(app.agent_tags())
        .filter(|(agent, _)| agent.is_running())
        .unzip();
    let models = agent_models(&running);
    let tag_column = tags.iter().map(|tag| text::width(tag)).max().unwrap_or(0);
    let model_column = models
        .iter()
        .flatten()
        .map(|model| text::width(model))
        .max()
        .unwrap_or(0);

    for ((agent, tag), model) in running.into_iter().zip(tags).zip(models) {
        let status = agent_status(agent, app.stamp());
        // The status is right-aligned at the pane's edge, so it needs no
        // column of its own: one as wide as `12m 40s` on every row would take
        // the room of another agent's task.
        let columns = AgentColumns {
            tag: tag_column,
            model: model.as_ref().map_or(0, |_| model_column),
            status: text::width(&status),
            task: 0,
        };
        rows.push(agent_row(
            &tag,
            agent.task(),
            (model, &status),
            columns.fitted(width),
            theme,
        ));

        if let Some(doing) = app.agent_doing(&agent.id).or_else(|| agent.latest.clone()) {
            let room = width.saturating_sub(AGENT_SUBLINE).max(1);
            rows.push(
                Line::from(format!("  └ {}", text::truncate(&doing, room)))
                    .style(Style::new().fg(theme.dim)),
            );
        }
    }
    rows
}

/// The columns an agent's row is drawn in. The task takes what the others
/// leave of the row.
#[derive(Debug, Clone, Copy)]
struct AgentColumns {
    tag: usize,
    model: usize,
    status: usize,
    task: usize,
}

impl AgentColumns {
    /// The columns in a row `width` wide, with the model given up where it
    /// would leave the task less than [`AGENT_LABEL_LEAST`]: a row that names
    /// the model and not what the agent is for says nothing about which
    /// agent it is.
    fn fitted(self, width: usize) -> Self {
        let task = |model: usize| {
            width.saturating_sub(
                AGENT_GLYPH + self.tag + AGENT_GAP + gapped(model) + AGENT_GAP + self.status,
            )
        };
        match task(self.model) >= AGENT_LABEL_LEAST {
            true => Self {
                task: task(self.model),
                ..self
            },
            false => Self {
                model: 0,
                task: task(0),
                ..self
            },
        }
    }
}

/// A column and the gap before it, or nothing for a column not drawn.
fn gapped(column: usize) -> usize {
    match column {
        0 => 0,
        column => AGENT_GAP + column,
    }
}

/// One agent's row, in `columns`.
fn agent_row(
    tag: &str,
    task: &str,
    (model, status): (Option<String>, &str),
    columns: AgentColumns,
    theme: &Theme,
) -> Line<'static> {
    let room = columns.task;
    let task = text::truncate(task, room);
    let pad = room.saturating_sub(text::width(&task));
    let colour = theme.agent;
    let mut row = vec![
        Span::styled("◆ ", Style::new().fg(colour).bold()),
        Span::styled(text::pad(tag, columns.tag), Style::new().fg(colour)),
        Span::raw(AGENT_SPACE),
        Span::styled(task, Style::new().fg(theme.fg)),
        Span::raw(" ".repeat(pad)),
    ];
    if columns.model > 0 {
        let model = text::truncate(&model.unwrap_or_default(), columns.model);
        row.push(Span::styled(
            format!(" {}", text::pad(&model, columns.model)),
            Style::new().fg(theme.dim),
        ));
    }
    row.push(Span::styled(
        format!(" {status:>width$}", width = columns.status),
        Style::new().fg(colour),
    ));
    Line::from(row)
}

/// What each agent's model is called on its row, in the pane's order: its
/// family — `opus`, `sonnet` — where no other agent's model is of the same
/// family, and the Usage pane's short name where one is.
fn agent_models(agents: &[&SubAgent]) -> Vec<Option<String>> {
    let named: Vec<&str> = agents
        .iter()
        .filter_map(|agent| agent.model.as_deref())
        .collect();
    let mut labels = usage::families(named).into_iter();
    agents
        .iter()
        .map(|agent| agent.model.as_ref().and_then(|_| labels.next()))
        .collect()
}

/// A running sub-agent's status column: how long it has run, where the
/// shell has both the moment it started and the moment it is drawing at — a
/// session read back from a log that kept no times has neither, and
/// `running` is what there is to say.
fn agent_status(agent: &SubAgent, now: Option<Stamp>) -> String {
    match agent.at.zip(now).and_then(|(at, now)| now.since(at)) {
        Some(ran) => clock::spent(ran),
        None => "running".to_owned(),
    }
}

/// The glyph a sub-agent row opens with, and the space after it.
const AGENT_GLYPH: usize = 2;

/// What stands between one column of an agent's row and the next.
const AGENT_SPACE: &str = " ";

/// How wide [`AGENT_SPACE`] is.
const AGENT_GAP: usize = AGENT_SPACE.len();

/// The least of an agent's task its model is drawn beside: fewer cells than
/// this and the model is left off.
const AGENT_LABEL_LEAST: usize = 12;

/// The indent and the `└ ` an agent's sub-line opens with.
const AGENT_SUBLINE: usize = 4;

/// The Changes pane: the branch, and what the repository says the working
/// tree holds that is not committed — whoever changed it.
fn draw_changes(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    draw_scrolling_pane(
        frame,
        area,
        app,
        Pane::Changes,
        "Changes",
        theme,
        changes_rows,
    );
}

/// One of the two panes that scroll and hold folding sections.
///
/// The drawing is in two halves: the rows are built whole, and then the height
/// the pane got decides which of them the operator sees. Building them all is
/// what lets the pane know how far it can be scrolled.
fn draw_scrolling_pane(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    which: Pane,
    title: &str,
    theme: &Theme,
    rows: fn(&App, usize, &Theme) -> PaneRows,
) {
    let border = Border::of(app.focus() == Focus::Pane(which), theme);
    let block = pane(title, border, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let PaneRows { mut rows, headers } = rows(app, usize::from(inner.width), theme);
    let held = rows.len();
    app.measured_pane(which, inner, held, usize::from(inner.height));
    app.measured_sections(which, headers.clone());
    if let Some(cursor) = app.section_cursor(which) {
        mark_cursor(&mut rows, &headers, cursor);
    }
    let at = app.pane_scroll(which);
    // Saturating rather than wrapping: a pane scrolled further than a `u16`
    // can say would come back to the top.
    let scroll = u16::try_from(at).unwrap_or(u16::MAX);

    frame.render_widget(Paragraph::new(rows).scroll((scroll, 0)), inner);
    // The same treatment the transcript gets. A terminal has no pointer to
    // hover a pane with and nothing else to hint that one scrolls, so a pane
    // that scrolled invisibly would be a pane nobody scrolled, and the rows
    // below the fold might as well not be drawn.
    draw_scrollbar(
        frame,
        inner,
        (area.right().saturating_sub(1), border),
        (at, held, usize::from(inner.height)),
        theme,
    );
}

/// A scrolling pane's rows, with the row each section's header is on.
#[derive(Default)]
struct PaneRows {
    rows: Vec<Line<'static>>,
    headers: Vec<(Section, usize)>,
}

impl PaneRows {
    /// Rows that belong to no section, such as the branch above them.
    fn extend(&mut self, rows: impl IntoIterator<Item = Line<'static>>) {
        self.rows.extend(rows);
    }

    /// A section's rows, the first of which is its header.
    fn section(&mut self, section: Section, rows: Vec<Line<'static>>) {
        if !rows.is_empty() {
            self.headers.push((section, self.rows.len()));
        }
        self.rows.extend(rows);
    }
}

/// Draws the section cursor: the header's marker and name as a solid bar.
///
/// Reversed rather than painted, like the focused pane's title, so the cursor
/// is there on a terminal with no colour.
fn mark_cursor(rows: &mut [Line<'static>], headers: &[(Section, usize)], cursor: Section) {
    let span = headers
        .iter()
        .find(|(section, _)| *section == cursor)
        .and_then(|(_, row)| rows.get_mut(*row))
        .and_then(|line| line.spans.first_mut());
    if let Some(span) = span {
        span.style = span.style.add_modifier(Modifier::REVERSED);
    }
}

/// Every row the pane has, the working tree folded to its header alone.
///
/// A directory that is not a repository has no branch and no working tree,
/// and a section drawn empty would say it had nothing changed rather than
/// that there is no repository to have changed anything.
fn changes_rows(app: &App, width: usize, theme: &Theme) -> PaneRows {
    let repo = app.repo();
    let mut rows = PaneRows::default();
    if let Some(branch) = &repo.branch {
        rows.extend([branch_row(branch, repo, width, theme)]);
        rows.section(
            Section::WorkingTree,
            working_tree_rows(app, repo, width, theme),
        );
    }
    rows
}

/// `⎇ main  ↑3 ↓1`: the branch, and how far it has drifted from its upstream.
///
/// A branch with no upstream carries neither figure: it is not zero commits
/// ahead of anything, it is ahead of nothing.
fn branch_row(branch: &str, repo: &crate::app::Repo, width: usize, theme: &Theme) -> Line<'static> {
    let drift: String = [(repo.ahead, '↑'), (repo.behind, '↓')]
        .into_iter()
        .filter_map(|(count, arrow)| match count {
            Some(0) | None => None,
            Some(count) => Some(format!(" {arrow}{count}")),
        })
        .collect();

    Line::from(vec![
        Span::styled(
            format!(
                "⎇ {}",
                text::truncate(branch, width.saturating_sub(2 + text::width(&drift)))
            ),
            Style::new().fg(theme.tool),
        ),
        Span::styled(drift, Style::new().fg(theme.dim)),
    ])
}

/// One figure in a section header's summary — `✗ 2`, `+977`, `4s ago` — and
/// how much the operator needs it when the header cannot carry them all.
///
/// `rank` 0 is what the section is read for; a higher rank gives way first.
/// `sep` is what sets it off from the figure before it, and is dropped with
/// it, so a summary that lost a figure has no doubled or dangling separator.
#[derive(Debug, Clone)]
struct Figure {
    rank: u8,
    sep: &'static str,
    spans: Vec<Span<'static>>,
}

impl Figure {
    /// The first figure of a summary, which nothing precedes.
    fn lead(rank: u8, span: Span<'static>) -> Self {
        Self {
            rank,
            sep: "",
            spans: vec![span],
        }
    }

    /// A figure after another, set off by `sep`.
    fn after(sep: &'static str, rank: u8, span: Span<'static>) -> Self {
        Self {
            rank,
            sep,
            spans: vec![span],
        }
    }

    fn width(&self, first: bool) -> usize {
        let sep = if first { 0 } else { text::width(self.sep) };
        sep + self
            .spans
            .iter()
            .map(|span| text::width(&span.content))
            .sum::<usize>()
    }
}

/// What the figures take drawn in order, the first without its separator.
fn figures_width(figures: &[Figure]) -> usize {
    figures
        .iter()
        .enumerate()
        .map(|(i, figure)| figure.width(i == 0))
        .sum()
}

/// The figures of a summary that fit `room`, in the order they were given.
///
/// **The rule: the least important figure is dropped until the rest fit, the
/// rightmost first between two of the same rank, and a figure is kept whole or
/// not at all.** A number cut anywhere is a different number — `+14` cut to
/// `+1` is a lie — so no span is ever truncated. Dropping the whole summary
/// when it does not fit, the alternative this replaces, left a narrow pane's
/// headers reading `▾ Tools` with the failure count that is the reason to read
/// it gone along with the byte count nobody needed. Nothing marks a summary
/// that lost figures: an ellipsis would spend the columns that are short, and
/// what is left is a whole, true statement on its own.
fn narrowed(mut figures: Vec<Figure>, room: usize) -> Vec<Figure> {
    while figures_width(&figures) > room {
        let Some(least) = figures
            .iter()
            .enumerate()
            .max_by_key(|(i, figure)| (figure.rank, *i))
            .map(|(i, _)| i)
        else {
            break;
        };
        figures.remove(least);
    }
    figures
}

/// A section's header: the fold marker, its name, and what it summarises —
/// as many of the summary's figures as fit, by [`narrowed`]'s rule.
///
/// The marker is the fold's only affordance, so it is drawn whether or not the
/// section has a key: a section that is folded must say so even when the way
/// it was folded was the one key the F-key bar names.
fn section_header(
    folded: bool,
    name: &str,
    summary: Vec<Figure>,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let marker = match folded {
        true => "▸ ",
        false => "▾ ",
    };
    let mut spans = vec![Span::styled(
        format!("{marker}{name}"),
        Style::new().fg(theme.title).bold(),
    )];
    let room = width.saturating_sub(text::width(marker) + text::width(name) + 2);
    let kept = narrowed(summary, room);
    if !kept.is_empty() {
        spans.push(Span::raw("  "));
    }
    for (i, figure) in kept.into_iter().enumerate() {
        if i > 0 {
            spans.push(match figure.sep.trim().is_empty() {
                true => Span::raw(figure.sep),
                false => Span::styled(figure.sep, Style::new().fg(theme.dim)),
            });
        }
        spans.extend(figure.spans);
    }
    Line::from(spans)
}

/// What the repository measured the working tree to have changed.
///
/// These are git's figures, exact, about every file in the tree — including
/// files this session never touched — and for a file git does not track yet,
/// its lines counted as git counts them once it is added, with the file marked
/// new. They are never mixed with the session's own counts, which are a
/// different claim by a different party and have their own section below.
fn working_tree_rows(
    app: &App,
    repo: &crate::app::Repo,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    // Git gives a binary file no line counts, and the header marks that the
    // way a file's own row does: a floor where some file had none, and a dash
    // where no file had any, never a zero standing in for the count.
    let side = |count_of: fn(&crate::app::WorkingFile) -> Option<u64>, sign: char| {
        let lines = repo
            .working
            .iter()
            .filter_map(count_of)
            .fold(0u64, u64::saturating_add);
        count(
            sign,
            lines,
            repo.working.iter().all(|file| count_of(file).is_some()),
        )
    };
    let added = side(|file| file.added, '+');
    let removed = side(|file| file.removed, '−');
    let summary = match repo.read {
        false => vec![Figure::lead(
            0,
            Span::styled("—", Style::new().fg(theme.dim)),
        )],
        true => changed_figures(repo.working.len(), added, removed, theme),
    };

    let folded = app.folded(Section::WorkingTree);
    let mut rows = vec![section_header(
        folded,
        "Working tree",
        summary,
        width,
        theme,
    )];
    if folded {
        return rows;
    }
    // Nobody has looked yet, which is not the repository saying nothing
    // changed.
    if !repo.read {
        rows.push(Line::from("  not read yet").style(Style::new().fg(theme.dim)));
        return rows;
    }
    if repo.working.is_empty() {
        rows.push(Line::from("  nothing changed yet").style(Style::new().fg(theme.dim)));
        return rows;
    }

    for row in tree::grouped(&repo.working) {
        rows.push(match &row {
            tree::Row::Dir(_) => Line::from(format!(
                "  {}",
                text::truncate_start(row.name(), width.saturating_sub(2))
            ))
            .style(Style::new().fg(theme.dim)),
            tree::Row::File { file, .. } => counted_row(
                FILE_INDENT,
                row.name(),
                Tag::new(file.new),
                &measured('+', file.added),
                &measured('−', file.removed),
                width,
                theme,
            ),
        });
    }
    rows
}

/// A tool's family, which is the row it is counted under.
///
/// **The rule: a tool an MCP server provides is counted under that server, and
/// every other tool under its own name.** A session reaches for one MCP server
/// a dozen ways — `Notion·search`, `Notion·fetch`, `Notion·update-page` — and
/// a row each says less about where the session spent its calls than one row
/// saying sixteen went to Notion. A backend's own tools are already the family
/// they belong to: `Bash` is `Bash`.
fn tool_family(name: &str) -> String {
    let label = crate::app::tool_label(name);
    match label.split_once('·') {
        Some((server, _)) => format!("{server}·*"),
        None => label,
    }
}

/// What the session called, and how often: a row per family, busiest first,
/// with a bar for how it compares and its own failures beside it.
fn tool_rows(app: &App, session: &SessionState, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let tools = session.tools();
    let folded = app.folded(Section::Tools);
    let mut rows = vec![section_header(
        folded,
        "Tools",
        tool_summary(tools, theme),
        width,
        theme,
    )];
    if folded {
        return rows;
    }
    if tools.by_name.is_empty() {
        rows.push(Line::from("  none called").style(Style::new().fg(theme.dim)));
        return rows;
    }

    let mut families: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for (name, count) in &tools.by_name {
        let entry = families.entry(tool_family(name)).or_default();
        entry.0 = entry.0.saturating_add(*count);
        entry.1 = entry
            .1
            .saturating_add(tools.failed_by_name.get(name).copied().unwrap_or(0));
    }
    let mut mix: Vec<(&String, &(u64, u64))> = families.iter().collect();
    mix.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(b.0)));

    // The pane scrolls, so every family the session reached for gets a row.
    // The cap the Usage pane drew them under was the price of a pane sized to
    // its content; here the rows below the fold are scrolled to.
    let busiest = mix.first().map(|(_, n)| n.0).unwrap_or(1).max(1);
    let name = mix
        .iter()
        .map(|(family, _)| text::width(family))
        .max()
        .unwrap_or(0)
        .min(BAR_NAME);
    let bar_width = width
        .saturating_sub(text::width(ROW_INDENT) + name + BAR_COUNT + 2 + BAR_FAILED)
        .min(BAR_CELLS);
    for (family, (count, failed)) in mix {
        rows.push(bar_line(
            (family, name),
            *count,
            *failed,
            busiest,
            bar_width,
            theme,
        ));
    }
    rows
}

/// The tools section's header: the calls, the failures in the error colour,
/// and the rest dimmed, because a failure is the one figure in the line the
/// operator has to notice without looking for it — and so the last to give
/// way where the pane is narrow, with the bytes out the first.
fn tool_summary(tools: &ToolTotals, theme: &Theme) -> Vec<Figure> {
    let dim = Style::new().fg(theme.dim);
    let mut figures = vec![Figure::lead(
        1,
        Span::styled(
            match tools.finished {
                1 => "1 call".to_owned(),
                calls => format!("{calls} calls"),
            },
            Style::new().fg(theme.fg),
        ),
    )];
    if tools.failed > 0 {
        figures.push(Figure::after(
            " ",
            0,
            Span::styled(format!("✗ {}", tools.failed), Style::new().fg(theme.del)),
        ));
    }
    figures.push(Figure::after(
        " · ",
        2,
        Span::styled(format!("{} denied", tools.denied), dim),
    ));
    figures.push(Figure::after(
        " · ",
        3,
        Span::styled(
            format!("{} out", crate::app::human_bytes(tools.output_bytes)),
            dim,
        ),
    ));
    figures
}

/// `23 files  +977 −259`: a changes section's summary. The line counts are
/// what it is read for, so the file count gives way first; the two counts
/// share a rank, and a pane too narrow for both keeps the lines added.
fn changed_figures(files: usize, added: String, removed: String, theme: &Theme) -> Vec<Figure> {
    vec![
        Figure::lead(
            1,
            Span::styled(files_said(files), Style::new().fg(theme.fg)),
        ),
        Figure::after("  ", 0, Span::styled(added, Style::new().fg(theme.add))),
        Figure::after(" ", 0, Span::styled(removed, Style::new().fg(theme.del))),
    ]
}

/// `23 files`, `1 file`, `no files`.
fn files_said(files: usize) -> String {
    match files {
        0 => "no files".to_owned(),
        1 => "1 file".to_owned(),
        files => format!("{files} files"),
    }
}

/// One side of a file's counts as the repository reported them.
///
/// Exact, because git counts every line it reports: `+0` is a file that
/// changed by no lines and is still a changed file. The em dash is kept for
/// what nobody counted — a binary file, which git declines to count, and a new
/// file too large to read — where no number exists rather than a number
/// nobody read.
fn measured(sign: char, lines: Option<u64>) -> String {
    match lines {
        Some(lines) => format!("{sign}{lines}"),
        None => "—".to_owned(),
    }
}

/// What a row under a section header is indented by, and what a file under a
/// directory row is indented by again.
const ROW_INDENT: &str = "  ";
const FILE_INDENT: &str = "    ";

/// What a counted row says of its file between the name and the counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    /// Nothing: a file the repository already tracks, or the session's own.
    None,
    /// `new`: a file the repository does not track yet.
    New,
}

impl Tag {
    fn new(new: bool) -> Self {
        match new {
            true => Tag::New,
            false => Tag::None,
        }
    }

    /// The tag as drawn, with the space before the counts.
    fn text(self) -> &'static str {
        match self {
            Tag::None => "",
            Tag::New => "new ",
        }
    }
}

/// A row whose counts keep their columns and whose name gives way.
///
/// The name loses its front rather than its tail: a path cut at the front
/// still names the file, and a count cut anywhere is a different number. The
/// tag keeps its place beside the counts for the same reason.
fn counted_row(
    indent: &'static str,
    name: &str,
    tag: Tag,
    added: &str,
    removed: &str,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let counts = text::width(tag.text()) + text::width(added) + 1 + text::width(removed);
    let room = width.saturating_sub(text::width(indent) + counts + 1);
    let name = text::truncate_start(name, room);
    let gap = width
        .saturating_sub(text::width(indent) + counts)
        .saturating_sub(text::width(&name));

    Line::from(vec![
        Span::raw(indent),
        Span::styled(name, Style::new().fg(theme.fg)),
        Span::raw(" ".repeat(gap)),
        Span::styled(tag.text(), Style::new().fg(theme.dim)),
        Span::styled(added.to_owned(), Style::new().fg(theme.add)),
        Span::raw(" "),
        Span::styled(removed.to_owned(), Style::new().fg(theme.del)),
    ])
}

/// One side of a file's counts: `+38`, `+≥38` where a call that changed the
/// file did not say how much it added, or an em dash where none of them did.
///
/// The three readings are the Usage pane's: a bare figure is the whole of it,
/// `≥` means at least this much, and an em dash means nothing was reported. A
/// zero here would say the session left that side of the file alone, which is
/// a different claim from not knowing.
pub(crate) fn count(sign: char, lines: u64, stated: bool) -> String {
    match (stated, lines) {
        (true, lines) => format!("{sign}{lines}"),
        (false, 0) => "—".to_owned(),
        (false, lines) => format!("{sign}≥{lines}"),
    }
}

/// How a window's meter and share are coloured: plain, then in the theme's
/// warning colour from [`WINDOW_RUNNING_LOW`], then in its error colour from
/// [`WINDOW_RUN_OUT`].
///
/// A window gets three states where a budget gets two because the window is
/// the one the operator can do nothing about until it comes back: half gone
/// is when pacing the rest of it starts to matter, and nine tenths is when the
/// next long turn may not finish inside it. A share that is not a share — a
/// negative or a NaN, which [`window_share`] draws as a dash — is drawn plain,
/// since nothing was reported to be low.
fn window_style(window: &UsageWindow, theme: &Theme) -> Style {
    let share = window.utilization;
    let colour = if share >= WINDOW_RUN_OUT {
        theme.del
    } else if share >= WINDOW_RUNNING_LOW {
        theme.warn
    } else {
        theme.fg
    };
    Style::new().fg(colour)
}

/// The F-key bar: Esc and what it stops, then the ten keys, each sized by
/// [`crate::menu::fkey_widths`]. A key that cannot do anything in this
/// session is drawn dimmed, as its menu item is.
fn draw_fkeys(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let bar = Style::new().bg(theme.menu_bg);
    let key_style = Style::new().fg(theme.hot).bg(theme.menu_bg).bold();
    let label_style = Style::new().fg(theme.fkey_fg).bg(theme.fkey_bg);
    let (stop_key, stop_label) = crate::menu::STOP;
    let stopped = |able: bool| match able {
        true => label_style,
        false => label_style.add_modifier(Modifier::DIM),
    };
    let mut spans = vec![
        Span::styled(stop_key, key_style),
        Span::styled(stop_label, stopped(app.can(crate::menu::Action::Stop))),
        Span::styled(" ", bar),
    ];

    let widths = crate::menu::fkey_widths(area.width);
    for ((digit, label, action), width) in crate::menu::FKEYS.iter().zip(widths) {
        let room = usize::from(width).saturating_sub(digit.len() + 1);
        spans.push(Span::styled(*digit, key_style));
        spans.push(Span::styled(
            text::pad(&text::truncate(label, room), room),
            stopped(app.can(*action)),
        ));
        spans.push(Span::styled(" ", bar));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)).style(bar), area);
}

/// What the session is on, and what it is running under.
///
/// The model is `None` until a backend has said which one it ended up on: a
/// profile names a backend, not a model, so naming one before the backend has
/// would be a guess in the place the operator reads to know what they are
/// paying for. A backend's own report wins over the profile that was selected
/// to start it.
///
/// What it runs under is the backend and the profile that chose it, and it is
/// `None` where no profile has been selected and no backend has spoken — the
/// state segment is what says so. Which plan or contract that profile is on is
/// the CLI's own business and is nowhere in the stream, so the row says what
/// was reported and stops there.
fn identity(
    session: &SessionState,
    profile: Option<&SelectedProfile>,
) -> (Option<String>, Option<String>) {
    match (session.meta(), profile) {
        // The model comes off the fold rather than off the meta: a model the
        // operator has just chosen is what the session is on from its next
        // turn, and naming the one it is moving off would read as a switch
        // that did not land.
        (Some(meta), _) => (
            Some(session.model().unwrap_or(&meta.model).to_owned()),
            Some(match meta.profile.is_empty() {
                true => meta.backend.to_string(),
                false => format!("{} · {}", meta.backend, meta.profile),
            }),
        ),
        (None, Some(profile)) => (
            None,
            Some(format!("{} · {}", profile.backend, profile.name)),
        ),
        (None, None) => (None, None),
    }
}

/// Which of a session's dollar figures are money, from what was said of how
/// it is billed.
///
/// Every place that prints the session's money — the Usage pane's rows, its
/// budget row, the budget warning and the printed summary — reads it here, so
/// that no two of them show a different part of it or name it two ways.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Billed {
    /// Billed by use throughout: the figure is money spent.
    Metered,
    /// On a flat-rate plan throughout. No money moves with the work, and
    /// what limits it is the plan's usage windows, so no dollar figure is
    /// shown: the figure the backend prints is what the work would have cost
    /// on the API, which nobody pays.
    Plan,
    /// One way for part of the session and the other way for the rest, as a
    /// session resumed under a profile billed differently is. Only the part
    /// billed by use is money, and it is the only part shown, named as a
    /// part.
    Both,
    /// Nothing said: the figure may be either, so none is shown as spent.
    Unknown,
}

impl Billed {
    /// How `session` has been billed, by every billing it was told of.
    pub fn of(session: &SessionState) -> Self {
        match (session.billing(), session.billing_changed()) {
            (Some(_), true) => Billed::Both,
            (Some(Billing::Metered), false) => Billed::Metered,
            (Some(Billing::Plan), false) => Billed::Plan,
            (None, _) => Billed::Unknown,
        }
    }

    /// The usage whose cost is money spent, which every dollar figure of
    /// `session` is drawn from: all of it where it was billed by use
    /// throughout, the metered part where it was billed both ways, and none
    /// on a plan or where nothing said.
    pub fn spent(self, session: &SessionState) -> Option<&Totals> {
        match self {
            Billed::Metered => Some(session.totals()),
            Billed::Both => Some(session.metered_totals()),
            Billed::Plan | Billed::Unknown => None,
        }
    }

    /// The usage a budget is measured against: what was spent where there
    /// is money, and otherwise every figure the backend reported, which is
    /// what the backend stops the session on even where no money moved.
    pub(crate) fn counted(self, session: &SessionState) -> &Totals {
        self.spent(session).unwrap_or_else(|| session.totals())
    }

    /// The words a dollar figure of this kind is printed after, where it is
    /// not the whole session's: `None` where it is, and where no figure is
    /// printed at all.
    pub fn qualifier(self) -> Option<&'static str> {
        match self {
            Billed::Both => Some(METERED_PART),
            Billed::Metered | Billed::Plan | Billed::Unknown => None,
        }
    }
}

/// What the money of a session billed both ways is called: the part of it
/// billed by use.
const METERED_PART: &str = "metered part";

/// The session's cost, labelled for what it is.
///
/// Each label says exactly how much is known:
///
/// * `$1.15` — every record is covered by a cost the backend reported.
/// * `~$1.15` — some are not, and `prices` valued all of them, so the figure
///   is what was reported plus an estimate at published rates.
/// * `≥~$1.15` — some are not, `prices` valued the models it lists and not
///   the rest, so the figure is what was reported plus the estimate for the
///   priced models: a floor that is itself partly an estimate. Counting the
///   priced models' estimate keeps the session from reading less than one of
///   its own models' `~$` rows.
/// * `≥$1.15` — some are not and none of them could be valued, so the figure
///   is a floor of reported money only.
/// * `unpriced` — nothing was reported and nothing could be valued.
/// * `—` — there is no usage at all. Never a zero.
///
/// Public so that anything else printing a session's cost prints the same
/// label the Usage pane does.
pub fn session_cost(session: &SessionState, prices: Option<&dyn Prices>) -> String {
    cost_of(session.totals(), prices)
}

/// The cost of `totals`, labelled as [`session_cost`] labels a session's:
/// for a part of a session, such as what of it was billed by use.
pub fn cost_of(totals: &Totals, prices: Option<&dyn Prices>) -> String {
    if totals.records == 0 {
        return "—".to_owned();
    }
    labelled(
        totals.reported_cost_usd,
        totals.cost_fully_reported(),
        value_unsettled(totals, prices),
    )
    .unwrap_or_else(|| "unpriced".to_owned())
}

/// What `totals` cost, drawn with the label [`cost_of`] gives it, and the
/// figure behind that label, which is a floor or an estimate wherever the
/// label says so. `None` where there is no usage, or where nothing was
/// reported and nothing could be valued.
pub(crate) fn known_spend(totals: &Totals, prices: Option<&dyn Prices>) -> Option<(String, f64)> {
    if totals.records == 0 {
        return None;
    }
    let (label, usd) = known_cost(
        totals.reported_cost_usd,
        totals.cost_fully_reported(),
        value_unsettled(totals, prices),
    )?;
    Some((label.format(usd), usd))
}

/// The least time worked a spend rate is drawn over.
///
/// Over the first seconds of a session one request's price is the whole
/// figure — a five-cent first reply ten seconds in reads eighteen dollars an
/// hour — and multiplying it up to an hour sells one request as a rate. A
/// minute of work spans several requests, where the figure starts to describe
/// the session rather than its first reply.
const RATED: std::time::Duration = std::time::Duration::from_secs(60);

/// What the session costs per hour the agent worked, labelled as the
/// session's cost is: a floor under the cost is a floor under the rate, an
/// estimate an estimated rate.
///
/// Worked, not elapsed: the idle between turns is the operator's, and a rate
/// that fell every time they stepped away would say nothing about the agent.
/// `None` wherever the time worked was not measured throughout, before a
/// minute of it, and wherever the cost has no figure — never a zero standing
/// in for a rate nobody measured.
fn spend_rate(app: &App) -> Option<String> {
    let worked = app.worked().filter(|worked| *worked >= RATED)?;
    let totals = app.session().totals();
    if totals.records == 0 {
        return None;
    }
    let (label, usd) = known_cost(
        totals.reported_cost_usd,
        totals.cost_fully_reported(),
        value_unsettled(totals, app.prices()),
    )?;
    Some(label.format(usd / worked.as_secs_f64() * 3_600.0))
}

/// One model's cost, labelled the way [`session_cost`] labels the session's:
/// what was reported for it, and what `prices` makes of whatever of it no
/// reported cost covers.
///
/// An em dash where nothing reported the model's cost and nothing prices it —
/// the model's row still carries its tokens, which are measured.
fn model_cost(totals: &Totals, model: &str, prices: Option<&dyn Prices>) -> String {
    let reported = totals.reported_cost_by_model.get(model).copied();
    let owed = totals.unsettled.get(model);
    if reported.is_none() && owed.is_none() {
        return "—".to_owned();
    }
    if owed.is_none() && reported == Some(0.0) && billed_under_its_window(totals, model) {
        return BILLED_UNDER_THE_WINDOW.to_owned();
    }
    let valued = match owed.and_then(|owed| prices?.estimate_owed(owed)) {
        Some(usd) => Valued::All(usd),
        None => Valued::Nothing,
    };
    labelled(reported.unwrap_or_default(), owed.is_none(), valued).unwrap_or_else(|| "—".to_owned())
}

/// What a model's row says where its tokens were billed under the 1M-window
/// id beside it, whose row carries the money.
const BILLED_UNDER_THE_WINDOW: &str = "in [1m]";

/// Whether `model` has a 1M-window sibling, `<model>[1m]`, that was reported a
/// cost: the CLI bills a family's tokens there when a sub-agent's messages
/// name the family and the session runs on the window, and settles the
/// family at nothing. `$0.00` beside those tokens would read as free.
fn billed_under_its_window(totals: &Totals, model: &str) -> bool {
    totals
        .reported_cost_by_model
        .get(&format!("{model}[1m]"))
        .is_some_and(|usd| *usd > 0.0)
}

/// A cost, labelled for how much of it is known: `$` where a reported cost
/// covers all of it, `~$` where the rest was valued at published rates, `≥~$`
/// where only part of the rest was and the figure is a partly estimated floor,
/// `≥$` where none of it could be and the figure is a floor. `None` where
/// nothing was reported and nothing could be valued, which each caller words
/// its own way.
fn labelled(reported: f64, settled: bool, valued: Valued) -> Option<String> {
    known_cost(reported, settled, valued).map(|(label, usd)| label.format(usd))
}

/// How much of a cost is known, which is what its figure is prefixed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CostLabel {
    /// Reported in full.
    Reported,
    /// Reported, and the rest valued at published rates.
    Estimate,
    /// Reported in part, and of the rest only some could be valued.
    EstimatedFloor,
    /// Reported in part, and the rest could not be valued.
    Floor,
}

impl CostLabel {
    fn format(self, usd: f64) -> String {
        let prefix = match self {
            Self::Reported => "",
            Self::Estimate => "~",
            Self::EstimatedFloor => "≥~",
            Self::Floor => "≥",
        };
        match self {
            Self::Reported | Self::Estimate if usd > 0.0 && usd < HALF_A_CENT => {
                format!("<{prefix}$0.01")
            }
            Self::EstimatedFloor | Self::Floor if usd < HALF_A_CENT => ">$0.00".to_owned(),
            _ => format!("{prefix}{}", dollars(usd)),
        }
    }
}

/// The least a figure can be and still be drawn in cents as something.
const HALF_A_CENT: f64 = 0.005;

/// `$0.75`, or `<$0.01` for money that was spent and rounds to no cents: a
/// real spend drawn as `$0.00` reads as a session that has cost nothing.
pub(crate) fn dollars(usd: f64) -> String {
    match usd > 0.0 && usd < HALF_A_CENT {
        true => "<$0.01".to_owned(),
        false => format!("${usd:.2}"),
    }
}

/// The figure [`labelled`] draws and how it is labelled, kept apart so that a
/// figure derived from the cost — a rate — carries the same label.
fn known_cost(reported: f64, settled: bool, valued: Valued) -> Option<(CostLabel, f64)> {
    if settled {
        return Some((CostLabel::Reported, reported));
    }
    match valued {
        Valued::All(estimated) => Some((CostLabel::Estimate, reported + estimated)),
        Valued::Part(estimated) => Some((CostLabel::EstimatedFloor, reported + estimated)),
        Valued::Nothing if reported == 0.0 => None,
        Valued::Nothing => Some((CostLabel::Floor, reported)),
    }
}

/// How much of the tokens no reported cost covers published rates could
/// value, and what that much comes to.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Valued {
    /// Every model owed for is priced.
    All(f64),
    /// Some models owed for are priced and some are not; the figure is the
    /// priced ones' share alone, so it understates what is owed.
    Part(f64),
    /// No model owed for is priced, or there is no price sheet.
    Nothing,
}

/// What the tokens no reported cost covers come to at published rates.
///
/// A model the sheet does not list leaves the sum short of what is owed, so it
/// is [`Valued::Part`] and read as a floor: an understated total shown as an
/// estimate is worse than an honest floor.
fn value_unsettled(totals: &Totals, prices: Option<&dyn Prices>) -> Valued {
    let Some(prices) = prices else {
        return Valued::Nothing;
    };
    let (mut sum, mut priced, mut unpriced) = (0.0, false, false);
    for owed in totals.unsettled.values() {
        match prices.estimate_owed(owed) {
            Some(usd) => {
                sum += usd;
                priced = true;
            }
            None => unpriced = true,
        }
    }
    match (priced, unpriced) {
        (_, false) => Valued::All(sum),
        (true, true) => Valued::Part(sum),
        (false, true) => Valued::Nothing,
    }
}

/// Token counts, short enough for a column of them: thousands above ten
/// thousand, millions from the count that would round to a thousand
/// thousands — `1000k` is a million drawn in the wrong unit — and each larger
/// unit from the count that would round to a thousand of the one below, so
/// no count a `u64` holds takes more than six columns.
pub(crate) fn compact(n: u64) -> String {
    const LARGE: [(f64, &str); 5] = [
        (1e6, "M"),
        (1e9, "G"),
        (1e12, "T"),
        (1e15, "P"),
        (1e18, "E"),
    ];
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_499 => format!("{:.0}k", n as f64 / 1_000.0),
        _ => {
            let (scaled, unit) = LARGE
                .iter()
                .map(|&(size, unit)| (n as f64 / size, unit))
                .find(|&(scaled, _)| scaled < 999.95)
                .unwrap_or((n as f64 / 1e18, "E"));
            format!("{scaled:.1}{unit}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use niobe_core::event::UsageWindows;
    use niobe_core::event::{Backend, SessionMeta, Usage, UsageWindow};

    fn priced(cost: Option<f64>) -> niobe_core::event::Event {
        niobe_core::event::Event::Usage(Usage {
            input: 1_000,
            output: 100,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 0,
            model: "opus-5".to_owned(),
            cost_usd: cost,
            settles_model: false,
            fast: false,
        })
    }

    /// A tenth of a cent per thousand tokens, so the arithmetic in the tests
    /// below is done by hand rather than by the thing under test.
    #[derive(Debug)]
    struct ATenthOfACentPerThousand;

    impl Prices for ATenthOfACentPerThousand {
        fn estimate(&self, usage: &Usage) -> Option<f64> {
            Some(usage.tokens() as f64 / 1_000.0 * 0.001)
        }
    }

    /// What the bundled table does with a model it has never heard of.
    #[derive(Debug)]
    struct NothingIsPriced;

    impl Prices for NothingIsPriced {
        fn estimate(&self, _usage: &Usage) -> Option<f64> {
            None
        }
    }

    fn said(text: &str) -> niobe_core::event::Event {
        niobe_core::event::Event::UserMessage {
            text: text.to_owned(),
        }
    }

    #[test]
    fn a_session_with_nothing_said_is_titled_by_its_repository() {
        let session = SessionState::new();
        assert_eq!(
            session_caption(session.caption(), "niobe", 80),
            "Session ─ niobe"
        );
        assert_eq!(session_caption(session.caption(), "", 80), "Session");
    }

    #[test]
    fn a_session_is_titled_by_what_it_is_about() {
        let session = SessionState::replay(&[said("Cost floors and replay pricing")]);
        assert_eq!(
            session_caption(session.caption(), "niobe", 80),
            "Cost floors and replay pricing"
        );
    }

    #[test]
    fn a_caption_longer_than_its_pane_is_cut_between_words_inside_the_margin() {
        let session = SessionState::replay(&[said("Cost floors and replay pricing")]);
        // Thirty columns of caption and six of margin: one short, and the
        // last word goes whole.
        assert_eq!(
            session_caption(session.caption(), "niobe", 35),
            "Cost floors and replay…"
        );
        for width in 0..=40 {
            let caption = session_caption(session.caption(), "niobe", width);
            assert!(
                text::width(&caption) <= usize::from(width.saturating_sub(TITLE_MARGIN)),
                "{width}: {caption}"
            );
        }
    }

    #[test]
    fn a_session_with_no_usage_shows_a_dash_not_a_zero() {
        let session = SessionState::new();
        assert_eq!(session_cost(&session, None), "—");
        assert_eq!(session_cost(&session, Some(&ATenthOfACentPerThousand)), "—");
    }

    #[test]
    fn a_partly_reported_cost_reads_as_a_floor() {
        let fully = SessionState::replay(&[priced(Some(0.25)), priced(Some(0.50))]);
        assert_eq!(session_cost(&fully, None), "$0.75");

        let partly = SessionState::replay(&[priced(Some(0.25)), priced(None)]);
        assert_eq!(session_cost(&partly, None), "≥$0.25");

        let none = SessionState::replay(&[priced(None)]);
        assert_eq!(session_cost(&none, None), "unpriced");
    }

    /// A dollar per record, and ten for a record whose input is past 1,500
    /// tokens: a long-context tier with round numbers.
    #[derive(Debug)]
    struct DearerPastFifteenHundred;

    impl Prices for DearerPastFifteenHundred {
        fn estimate(&self, usage: &Usage) -> Option<f64> {
            Some(if usage.input > 1_500 { 10.0 } else { 1.0 })
        }
    }

    #[test]
    fn records_owed_for_are_each_priced_as_the_request_they_were() {
        // Two records of 1,000 input tokens: each is under the threshold, so
        // $1 each, although their sum of 2,000 is past it.
        let running = SessionState::replay(&[priced(None), priced(None)]);
        let prices: &dyn Prices = &DearerPastFifteenHundred;
        assert_eq!(session_cost(&running, Some(prices)), "~$2.00");
        assert_eq!(
            model_cost(running.totals(), "opus-5", Some(prices)),
            "~$2.00"
        );
    }

    #[test]
    fn a_turn_the_backend_has_not_priced_is_estimated_rather_than_left_unpriced() {
        // Two records of 1,100 tokens each: 2,200 tokens at a tenth of a cent
        // per thousand is $0.0022, which is under a cent.
        let running = SessionState::replay(&[priced(None), priced(None)]);
        assert_eq!(
            session_cost(&running, Some(&ATenthOfACentPerThousand)),
            "<~$0.01"
        );

        // The estimate is added to what was already reported, not shown
        // instead of it.
        let after_one = SessionState::replay(&[priced(Some(0.25)), priced(None)]);
        assert_eq!(
            session_cost(&after_one, Some(&ATenthOfACentPerThousand)),
            "~$0.25"
        );
    }

    #[test]
    fn a_model_the_table_does_not_list_is_unpriced_rather_than_estimated() {
        let running = SessionState::replay(&[priced(None)]);
        assert_eq!(session_cost(&running, Some(&NothingIsPriced)), "unpriced");

        // A figure that leaves one model's share out is a floor, not an
        // estimate, so what was reported is still shown as a floor.
        let partly = SessionState::replay(&[priced(Some(0.25)), priced(None)]);
        assert_eq!(session_cost(&partly, Some(&NothingIsPriced)), "≥$0.25");
    }

    /// The table as it stands for a session on two models, one it lists and
    /// one it does not: a tenth of a cent per thousand for `opus-5`, nothing
    /// for anything else.
    #[derive(Debug)]
    struct OnlyOpusIsPriced;

    impl Prices for OnlyOpusIsPriced {
        fn estimate(&self, usage: &Usage) -> Option<f64> {
            (usage.model == "opus-5").then(|| ATenthOfACentPerThousand.estimate(usage))?
        }
    }

    fn on(model: &str, cost: Option<f64>) -> niobe_core::event::Event {
        let mut event = priced(cost);
        if let niobe_core::event::Event::Usage(usage) = &mut event {
            usage.model = model.to_owned();
        }
        event
    }

    #[test]
    fn a_floor_counts_the_estimate_for_every_model_the_table_prices() {
        // $0.04 reported; 110,000 opus tokens owed, which is $0.11 at a tenth
        // of a cent per thousand; haiku owed and unpriced. The session is at
        // least roughly $0.15, never less than opus's own ~$0.11.
        let mut opus = on("opus-5", None);
        if let niobe_core::event::Event::Usage(usage) = &mut opus {
            usage.input = 100_000;
            usage.output = 10_000;
        }
        let session =
            SessionState::replay(&[on("opus-5", Some(0.04)), opus, on("haiku-4-5", None)]);
        assert_eq!(session_cost(&session, Some(&OnlyOpusIsPriced)), "≥~$0.15");
        assert_eq!(
            model_cost(session.totals(), "opus-5", Some(&OnlyOpusIsPriced)),
            "~$0.15"
        );

        // Nothing reported and only part of it priced is still a figure: at
        // least what the priced part comes to, rather than "unpriced".
        let unreported = SessionState::replay(&[on("opus-5", None), on("haiku-4-5", None)]);
        assert_eq!(session_cost(&unreported, Some(&OnlyOpusIsPriced)), ">$0.00");
    }

    #[test]
    fn a_settled_turn_is_the_backends_own_figure_and_carries_no_mark() {
        let mut settled = priced(Some(1.1521625));
        if let niobe_core::event::Event::Usage(usage) = &mut settled {
            usage.settles_model = true;
        }
        let session = SessionState::replay(&[priced(None), priced(None), settled]);
        assert_eq!(
            session_cost(&session, Some(&ATenthOfACentPerThousand)),
            "$1.15",
            "a settled session is the CLI's own total, neither a floor nor an estimate"
        );
    }

    #[test]
    fn an_unstarted_session_names_no_model_it_was_never_told_about() {
        assert_eq!(
            identity(&SessionState::new(), None),
            (None, None),
            "nothing has said what this session is or what it runs under"
        );

        let running = SessionState::replay(&[niobe_core::event::Event::SessionMeta(SessionMeta {
            backend: Backend::Codex,
            profile: "default".to_owned(),
            model: "gpt-5-codex".to_owned(),
            backend_session: None,
        })]);
        assert_eq!(
            identity(&running, None),
            (
                Some("gpt-5-codex".to_owned()),
                Some("codex · default".to_owned())
            )
        );
    }

    #[test]
    fn a_selected_profile_is_named_until_a_backend_says_what_it_runs() {
        let work = SelectedProfile {
            name: "work".to_owned(),
            backend: Backend::Claude,
            models: Vec::new(),
        };
        assert_eq!(
            identity(&SessionState::new(), Some(&work)),
            (None, Some("claude · work".to_owned()))
        );

        let running = SessionState::replay(&[niobe_core::event::Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "work".to_owned(),
            model: "opus-5".to_owned(),
            backend_session: None,
        })]);
        assert_eq!(
            identity(&running, Some(&work)),
            (Some("opus-5".to_owned()), Some("claude · work".to_owned()))
        );
    }

    #[test]
    fn a_backend_that_has_started_and_not_yet_spoken_is_starting_not_absent() {
        let max = SelectedProfile {
            name: "max".to_owned(),
            backend: Backend::Claude,
            models: Vec::new(),
        };
        let selected = App::new(crate::app::Repo {
            name: "niobe".to_owned(),
            branch: None,
            ..Default::default()
        })
        .with_profile(max);

        assert_eq!(
            row_of(&selected),
            vec!["claude · max".to_owned(), "○ not attached".to_owned()],
            "a prompt typed here would go nowhere, and the row says so"
        );
        assert_eq!(
            row_of(&selected.attached()),
            vec!["claude · max".to_owned(), "○ starting".to_owned()],
            "a subprocess that has not spoken is starting, not absent"
        );
    }

    fn started_on_max() -> App {
        App::new(crate::app::Repo {
            name: "niobe".to_owned(),
            branch: None,
            ..Default::default()
        })
        .with_profile(SelectedProfile {
            name: "max".to_owned(),
            backend: Backend::Claude,
            models: Vec::new(),
        })
        .attached()
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            app.type_into_composer(ratatui_textarea::Input {
                key: ratatui_textarea::Key::Char(c),
                ..Default::default()
            });
        }
    }

    /// A CLI that died as it started never says what it runs, and the row
    /// that said "starting" would say it for ever. A prompt it could not take
    /// does not become the session's caption either: nothing was asked.
    #[test]
    fn a_backend_that_ended_before_it_spoke_is_ended_not_starting() {
        let mut app = started_on_max();
        app.apply(&niobe_core::event::Event::Error {
            message: "the `claude` session ended with exit status: 1".to_owned(),
            fatal: true,
        });
        assert_eq!(row_of(&app).last(), Some(&"○ ended".to_owned()));

        typed(&mut app, "hello there");
        app.submit();
        app.not_sent("the `claude` session has ended");
        assert_eq!(row_of(&app).last(), Some(&"○ ended".to_owned()));
        assert_eq!(
            session_caption(app.caption(), &app.repo().name, 80),
            "Session ─ niobe"
        );
    }

    /// A turn sent to a CLI that has not said a word since it started — not
    /// even what it runs — is not the same as one it is working on, and the
    /// row says so rather than counting "working" up.
    #[test]
    fn a_turn_the_backend_has_said_nothing_about_is_waiting_on_an_answer() {
        let mut app = started_on_max();
        typed(&mut app, "hello there");
        app.submit();
        let sent = std::time::Instant::now();
        app.tick(sent, None);
        app.tick(sent + std::time::Duration::from_secs(30), None);
        assert_eq!(
            row_of(&app),
            vec!["claude · max".to_owned(), "● no answer yet 30s".to_owned()]
        );

        app.apply(&niobe_core::event::Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "max".to_owned(),
            model: "claude-opus-5".to_owned(),
            backend_session: None,
        }));
        assert_eq!(row_of(&app).last(), Some(&"● working 30s".to_owned()));
    }

    /// What the menu row's right-hand group reads as, segment by segment.
    fn row_of(app: &App) -> Vec<String> {
        identity_segments(app, &Theme::default())
            .into_iter()
            .map(|segment| segment.text)
            .collect()
    }

    fn attached_session() -> App {
        let mut app = App::new(crate::app::Repo {
            name: "niobe".to_owned(),
            branch: None,
            ..Default::default()
        })
        .attached();
        app.apply(&niobe_core::event::Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "max".to_owned(),
            model: "claude-opus-5".to_owned(),
            backend_session: None,
        }));
        app
    }

    #[test]
    fn the_menu_row_says_what_the_session_is_on_and_what_it_runs_under() {
        assert_eq!(
            row_of(&attached_session()),
            vec![
                "claude-opus-5".to_owned(),
                "claude · max".to_owned(),
                "○ idle".to_owned(),
            ],
        );
    }

    #[test]
    fn a_shell_that_has_not_been_told_the_time_draws_none_rather_than_a_guess() {
        let app = attached_session();
        assert!(
            !row_of(&app).iter().any(|segment| segment.contains(':')),
            "no clock until the event loop has read one: {:?}",
            row_of(&app)
        );

        let mut ticked = attached_session();
        ticked.tick(
            std::time::Instant::now(),
            Some(crate::clock::Stamp::new(
                std::time::SystemTime::UNIX_EPOCH,
                crate::clock::LocalMoment::at(0, 14, 7),
            )),
        );
        assert_eq!(
            row_of(&ticked).last().map(String::as_str),
            Some("14:07"),
            "the time of day is the rightmost thing the row says"
        );
    }

    #[test]
    fn a_running_turn_fills_the_glyph_and_an_idle_one_does_not() {
        let mut app = attached_session();
        let t0 = std::time::Instant::now();

        app.tick(t0, None);
        assert_eq!(
            row_of(&app).get(2).map(String::as_str),
            Some("○ idle 0s"),
            "a session that has not been asked anything is idle, and says for how long"
        );

        app.tick(t0 + std::time::Duration::from_secs(72), None);
        assert_eq!(
            row_of(&app).get(2).map(String::as_str),
            Some("○ idle 1m 12s")
        );

        app.type_into_composer(ratatui_textarea::Input {
            key: ratatui_textarea::Key::Char('x'),
            ..Default::default()
        });
        app.submit();
        app.tick(t0 + std::time::Duration::from_secs(72), None);
        app.tick(t0 + std::time::Duration::from_secs(110), None);
        assert_eq!(
            row_of(&app).get(2).map(String::as_str),
            Some("● working 38s"),
            "a turn sent from here fills the glyph and is timed from when the shell saw it"
        );
    }

    #[test]
    fn a_session_that_hit_errors_says_so_without_a_pane_being_opened() {
        let mut app = attached_session();
        assert!(!row_of(&app).iter().any(|segment| segment.contains("error")));

        app.apply(&niobe_core::event::Event::Error {
            message: "the backend stopped answering".to_owned(),
            fatal: false,
        });
        assert_eq!(
            row_of(&app).first().map(String::as_str),
            Some("⚠ 1 error"),
            "errors lead the group, so they are the last segment a narrow row drops"
        );
    }

    #[test]
    fn a_narrowing_row_drops_whole_segments_from_the_right() {
        let mut app = attached_session();
        app.tick(
            std::time::Instant::now(),
            Some(crate::clock::Stamp::new(
                std::time::SystemTime::UNIX_EPOCH,
                crate::clock::LocalMoment::at(0, 14, 7),
            )),
        );

        let whole = identity_segments(&app, &Theme::default());
        let full = segments_width(&whole);

        // One column short of the whole group, and the rightmost segment —
        // the clock — is what gives way, entire.
        let kept = drawn_segments(&app, full - 1);
        assert_eq!(
            kept,
            vec![
                "claude-opus-5".to_owned(),
                "claude · max".to_owned(),
                "○ idle 0s".to_owned()
            ]
        );

        assert_eq!(
            drawn_segments(&app, 0),
            Vec::<String>::new(),
            "a row with no room for a whole segment says nothing rather than half of something"
        );
    }

    /// The segments that survive a group with `room` columns for them.
    fn drawn_segments(app: &App, room: usize) -> Vec<String> {
        fitted(identity_segments(app, &Theme::default()), room)
            .into_iter()
            .map(|s| s.text)
            .collect()
    }

    #[test]
    fn the_pane_that_shows_what_a_session_used_is_called_usage_everywhere() {
        let labels: Vec<&str> = crate::menu::MENUS
            .iter()
            .flat_map(|menu| menu.items)
            .map(|item| item.label)
            .collect();
        assert!(labels.contains(&"Usage pane"), "{labels:?}");
        assert!(
            !labels.iter().any(|label| label.starts_with("Cost pane")),
            "nothing the operator reads still calls this pane Cost"
        );
    }

    #[test]
    fn the_bar_names_the_digit_after_esc_and_every_label_is_whole_at_the_narrowest() {
        // A Mac's top row is media keys unless Fn is held, so the bar names
        // the key that reaches every terminal: Esc, then one digit.
        assert!(
            crate::menu::FKEYS.iter().all(|(key, _, _)| key.len() == 1),
            "one digit per action, 0 for the tenth"
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(MIN_SIZE.0, MIN_SIZE.1))
                .expect("a test terminal");
        let mut app = App::new(crate::app::Repo::default());
        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("drawing never fails on a test backend");
        let buffer = terminal.backend().buffer();
        let row: String = (0..MIN_SIZE.0)
            .map(|x| buffer[(x, MIN_SIZE.1 - 1)].symbol().to_owned())
            .collect();
        assert!(row.starts_with("EscStop 1Help"), "{row}");
        for (key, label, _) in crate::menu::FKEYS {
            assert!(
                row.contains(&format!("{key}{label}")),
                "{key}{label} is cut: {row}"
            );
        }
    }

    /// What one row of the working tree reads as, counts and all.
    fn row(path: &str, added: Option<u64>, removed: Option<u64>, width: usize) -> String {
        counted_row(
            ROW_INDENT,
            path,
            Tag::None,
            &measured('+', added),
            &measured('−', removed),
            width,
            &Theme::default(),
        )
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
    }

    #[test]
    fn a_count_no_call_stated_is_an_em_dash_and_never_a_zero() {
        assert_eq!(count('+', 0, true), "+0");
        assert_eq!(count('−', 9, true), "−9");
        assert_eq!(count('+', 0, false), "—");
        assert_eq!(
            count('+', 38, false),
            "+≥38",
            "a figure some calls did not add to was shown as the whole of it"
        );
    }

    #[test]
    fn a_file_row_reads_as_the_counts_the_repository_gave() {
        assert_eq!(
            row("catalog/fetch.ts", Some(38), Some(9), 40),
            "  catalog/fetch.ts                +38 −9"
        );
        assert_eq!(
            row("notes.md", Some(1), Some(0), 40),
            "  notes.md                         +1 −0"
        );
        assert_eq!(
            row("run.ipynb", None, None, 40),
            "  run.ipynb                          — —"
        );
    }

    /// The counts are what the row is for. A path cut at the front still names
    /// the file; a count cut anywhere is a different number.
    #[test]
    fn a_path_too_long_for_the_pane_gives_way_to_its_counts() {
        let row = row(
            "crates/niobe-bridge-claude/src/translate.rs",
            Some(120),
            Some(44),
            32,
        );
        assert!(row.ends_with(" +120 −44"), "{row:?}");
        assert!(row.contains("translate.rs"), "{row:?}");
        assert_eq!(text::width(&row), 32, "{row:?}");
    }

    /// `+0 −0` is a file the repository counted and found unchanged by any
    /// line; the em dash is a file it counted no lines in at all. Reading one
    /// as the other is the difference between a measurement and a gap.
    #[test]
    fn a_file_that_changed_by_no_lines_is_not_the_same_as_one_with_no_count() {
        assert_eq!(measured('+', Some(0)), "+0");
        assert_eq!(measured('−', Some(12)), "−12");
        assert_eq!(measured('+', None), "—");
    }

    #[test]
    fn token_counts_stay_short() {
        assert_eq!(compact(0), "0");
        assert_eq!(compact(9_999), "9999");
        assert_eq!(compact(25_500), "26k");
        assert_eq!(compact(1_260_000), "1.3M");
        // The two boundaries: where a figure starts being thousands, and where
        // thousands become millions.
        assert_eq!(compact(10_000), "10k");
        assert_eq!(compact(999_499), "999k");
        assert_eq!(compact(999_500), "1.0M");
        assert_eq!(compact(999_999), "1.0M");
        assert_eq!(compact(1_000_000), "1.0M");
    }

    /// Past a thousand millions the figure moves up a unit rather than
    /// growing digits, so any count a backend can report fits the column.
    #[test]
    fn a_count_past_any_real_size_still_fits_in_six_columns() {
        assert_eq!(compact(999_949_999), "999.9M");
        assert_eq!(compact(999_950_000), "1.0G");
        assert_eq!(compact(1_500_000_000_000), "1.5T");
        assert_eq!(compact(u64::MAX), "18.4E");
        for n in [999_999_999, 999_999_999_999, 999_999_999_999_999, u64::MAX] {
            assert!(compact(n).len() <= 6, "{n}: {}", compact(n));
        }
    }

    #[test]
    fn a_share_that_rounds_to_nothing_or_to_everything_says_it_does_not() {
        assert_eq!(share_label(0, true, true), " <1%");
        assert_eq!(share_label(100, true, true), ">99%");
        assert_eq!(share_label(100, true, false), "100%");
        assert_eq!(share_label(0, false, false), "  0%");
        assert_eq!(share_label(42, true, true), " 42%");
    }

    #[test]
    fn a_level_that_is_not_a_level_draws_a_dash_and_a_huge_one_fits_its_column() {
        assert_eq!(window_share(-5.0), "  — ");
        assert_eq!(window_share(f64::NAN), "  — ");
        assert_eq!(window_share(1e300), "999%");
        assert_eq!(window_share(0.5), " 50%");
    }

    fn window(utilization: f64, resets_at: Option<u64>) -> UsageWindow {
        UsageWindow {
            utilization,
            resets_at,
        }
    }

    /// Either window running low is the plan running low: on a flat-rate plan
    /// the seven-day window running out stops the session just as surely as
    /// the five-hour one, so neither may be marked at the other's expense.
    #[test]
    fn a_window_reads_plain_then_running_low_then_run_out() {
        for theme in crate::theme::THEMES {
            let plain = Style::new().fg(theme.fg);
            let low = Style::new().fg(theme.warn);
            let out = Style::new().fg(theme.del);

            assert_eq!(window_style(&window(0.0, None), &theme), plain);
            assert_eq!(window_style(&window(0.49, None), &theme), plain);
            assert_eq!(window_style(&window(0.50, None), &theme), low);
            assert_eq!(window_style(&window(0.89, None), &theme), low);
            assert_eq!(window_style(&window(0.90, None), &theme), out);
            assert_eq!(window_style(&window(1.04, None), &theme), out);
        }
    }

    /// A share no backend could mean is not a window running low.
    #[test]
    fn a_window_whose_share_is_not_a_share_is_drawn_plain() {
        let theme = Theme::default();
        let plain = Style::new().fg(theme.fg);

        assert_eq!(window_style(&window(f64::NAN, None), &theme), plain);
        assert_eq!(window_style(&window(-0.5, None), &theme), plain);
    }

    /// The first moment of a day twenty thousand days after the epoch, which
    /// is a Friday. Every window figure below is read against it, so the rows
    /// read the same on every machine and in every month.
    const A_FRIDAY: u64 = 20_000 * 86_400;

    /// A session on a plan with the windows a test names, read at 13:41 on
    /// [`A_FRIDAY`] by a clock that never moves for daylight saving.
    fn windowed(windows: UsageWindows) -> App {
        let clock = crate::clock::Clock::fixed(0).expect("UTC is an offset");
        let mut app = App::new(crate::app::Repo {
            name: "niobe".to_owned(),
            branch: None,
            ..Default::default()
        })
        .with_clock(clock.clone());
        app.apply(&niobe_core::event::Event::UsageWindows(windows));
        app.tick(
            std::time::Instant::now(),
            Some(clock.at(std::time::UNIX_EPOCH
                + std::time::Duration::from_secs(A_FRIDAY + 13 * 3_600 + 41 * 60))),
        );
        app
    }

    /// A sub-agent's messages can name the family while the CLI bills them
    /// under the 1M-window id beside it, which settles the family's row at
    /// no cost of its own. Its money is in the `[1m]` row, and the row says
    /// so rather than drawing `$0.00` beside tokens that were spent. Where
    /// the `[1m]` row was reported no money either, there is none to point to.
    #[test]
    fn a_family_row_billed_under_its_1m_window_says_where_its_money_is() {
        let usage = |model: &str, input: u64, cost: Option<f64>, settles: bool| {
            niobe_core::event::Event::Usage(Usage {
                input,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                cache_write_1h: 0,
                reasoning: 0,
                model: model.to_owned(),
                cost_usd: cost,
                settles_model: settles,
                fast: false,
            })
        };
        let session = SessionState::replay(&[
            usage("opus-5[1m]", 100, None, false),
            usage("opus-5", 50, None, false),
            usage("opus-5[1m]", 0, Some(0.9), true),
            usage("opus-5", 0, Some(0.0), true),
        ]);

        assert_eq!(model_cost(session.totals(), "opus-5", None), "in [1m]");
        assert_eq!(model_cost(session.totals(), "opus-5[1m]", None), "$0.90");

        let free = SessionState::replay(&[usage("opus-5", 50, Some(0.0), true)]);
        assert_eq!(model_cost(free.totals(), "opus-5", None), "$0.00");

        let both_free = SessionState::replay(&[
            usage("opus-5[1m]", 100, Some(0.0), true),
            usage("opus-5", 50, Some(0.0), true),
        ]);
        assert_eq!(model_cost(both_free.totals(), "opus-5", None), "$0.00");
    }

    /// A model's cost carries the same labels the session's does, with an em
    /// dash where the session would say `unpriced`: a column of figures reads
    /// the dash as "no figure", and a word there would crowd the tokens.
    #[test]
    fn a_models_cost_is_labelled_for_what_is_known_of_it() {
        let reported = SessionState::replay(&[priced(Some(0.75))]);
        let owed = SessionState::replay(&[priced(Some(0.25)), priced(None)]);
        let nothing = SessionState::replay(&[priced(None)]);
        let cost = |session: &SessionState, prices: Option<&dyn Prices>| {
            model_cost(session.totals(), "opus-5", prices)
        };

        assert_eq!(cost(&reported, None), "$0.75");
        assert_eq!(cost(&owed, Some(&ATenthOfACentPerThousand)), "~$0.25");
        assert_eq!(cost(&owed, Some(&NothingIsPriced)), "≥$0.25");
        assert_eq!(cost(&nothing, Some(&NothingIsPriced)), "—");
        assert_eq!(model_cost(reported.totals(), "haiku-4-5", None), "—");
    }

    /// The column above the pane is laid out from the height it asks for, so
    /// a row it draws and did not count is a row cut off the bottom, and one
    /// it counted and did not draw is a blank the other panes lose.
    /// The budget row of a session billed `billing` that has seen `events`,
    /// drawn `width` columns wide.
    fn budget_row(billing: Billing, events: &[niobe_core::event::Event], width: usize) -> String {
        let mut app = App::new(crate::app::Repo::default()).with_budget(5.0);
        app.apply(&niobe_core::event::Event::Billing { billing });
        for event in events {
            app.apply(event);
        }
        let lines = money_lines(&app, width, &crate::theme::CLASSIC);
        line_text(lines.last().expect("a budget draws its row"))
    }

    /// A line ratatui measures wider than the width it was laid out for is
    /// clipped at the pane's edge, and what it held past it is lost with no
    /// mark. A drawn buffer cannot show that — every write is clipped to it —
    /// so the lines themselves are measured, the way ratatui measures them.
    #[test]
    fn every_line_laid_out_fits_the_width_ratatui_draws_it_in() {
        use niobe_core::event::Event;

        let heart = "\u{2764}\u{fe0f}";
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let mut app = App::new(crate::app::Repo::default()).with_budget(5.0);
        for event in [
            Event::UserMessage {
                text: format!("{} end {}", heart.repeat(30), family.repeat(12)),
            },
            Event::AssistantMessage {
                text: format!(
                    "{} 漢字かな混じり文 {}\n\n```\n\tindented\t{}\n```\n- {}",
                    heart.repeat(25),
                    "終わり".repeat(9),
                    heart.repeat(8),
                    family.repeat(20)
                ),
                agent: None,
            },
            Event::ToolCallStart {
                id: "t1".into(),
                name: "Bash".to_owned(),
                input: format!(r#"{{"command":"echo {}"}}"#, heart.repeat(40)),
                summary: Some(format!("echo {}", heart.repeat(40))),
                agent: None,
            },
            priced(None),
        ] {
            app.apply(&event);
        }

        // From narrower than the session pane is at the shell's least size,
        // and than the Usage pane is inside its border, up.
        let transcript = (40..=120).step_by(7).map(|width| {
            let lines: Vec<Line<'static>> = app
                .entries()
                .iter()
                .flat_map(|entry| {
                    entry_lines(entry, width, Detail::default(), &crate::theme::CLASSIC)
                })
                .collect();
            (width, lines)
        });
        let usage = (30..=60)
            .step_by(3)
            .map(|width| (width, usage_lines(&app, width, &crate::theme::CLASSIC)));
        for (width, lines) in transcript.chain(usage) {
            for line in &lines {
                assert!(
                    line.width() <= width,
                    "a line ratatui draws {} wide was laid out for {width}: {:?}",
                    line.width(),
                    line_text(line)
                );
            }
        }
    }

    #[test]
    fn a_budget_against_a_cost_nobody_reported_draws_a_dash_not_a_zero() {
        assert_eq!(
            budget_row(Billing::Metered, &[priced(None)], 40),
            "budget —/$5.00"
        );
        assert_eq!(budget_row(Billing::Metered, &[], 40), "budget —/$5.00");
    }

    #[test]
    fn a_budget_against_a_partly_reported_cost_draws_the_floor() {
        assert_eq!(
            budget_row(Billing::Metered, &[priced(Some(1.25)), priced(None)], 40),
            "budget ≥$1.25/$5.00"
        );
    }

    #[test]
    fn a_budget_on_a_plan_draws_a_dash_and_no_dollars() {
        assert_eq!(
            budget_row(Billing::Plan, &[priced(Some(1.25))], 40),
            "budget —"
        );
        assert_eq!(budget_row(Billing::Plan, &[priced(None)], 40), "budget —");
    }

    #[test]
    fn the_usage_pane_asks_for_exactly_the_rows_it_draws() {
        let windows = niobe_core::event::Event::UsageWindows(UsageWindows {
            five_hour: Some(UsageWindow {
                utilization: 0.4,
                resets_at: None,
            }),
            seven_day: None,
            using_overage: true,
        });
        let spent = priced(Some(0.25));
        let context = niobe_core::event::Event::Context(Context {
            tokens: 1_000,
            model: "opus-5".to_owned(),
            window: Some(200_000),
        });
        for billing in [None, Some(Billing::Plan), Some(Billing::Metered)] {
            for events in [
                vec![],
                vec![spent.clone()],
                vec![windows.clone(), spent.clone()],
                vec![windows.clone(), spent.clone(), context.clone()],
            ] {
                for budget in [None, Some(0.50)] {
                    let mut app = App::new(crate::app::Repo::default());
                    if let Some(budget) = budget {
                        app = app.with_budget(budget);
                    }
                    if let Some(billing) = billing {
                        app.apply(&niobe_core::event::Event::Billing { billing });
                    }
                    for event in &events {
                        app.apply(event);
                    }
                    assert_eq!(
                        usize::from(usage_height(&app)),
                        usage_lines(&app, 40, &crate::theme::CLASSIC).len() + 2,
                        "{billing:?} {events:?} {budget:?}"
                    );
                }
            }
        }
    }

    fn rows_of(app: &App, width: usize) -> Vec<String> {
        window_lines(app, width, &Theme::default())
            .iter()
            .map(Line::to_string)
            .collect()
    }

    #[test]
    fn a_window_reads_as_a_meter_its_share_and_when_it_comes_back() {
        let app = windowed(UsageWindows {
            five_hour: Some(window(0.51, Some(A_FRIDAY + 16 * 3_600 + 40 * 60))),
            // Day 20_004 is a Tuesday.
            seven_day: Some(window(0.71, Some(A_FRIDAY + 4 * 86_400 + 9 * 3_600))),
            using_overage: false,
        });

        assert_eq!(
            rows_of(&app, 66),
            vec![
                "5h  51% ▓▓▓▓▓▓░░░░░░ resets 16:40".to_owned(),
                "7d  71% ▓▓▓▓▓▓▓▓▓░░░ resets Tue 09:00".to_owned(),
            ]
        );
    }

    #[test]
    fn a_window_reported_without_a_reset_shows_the_share_and_no_reset() {
        let app = windowed(UsageWindows {
            five_hour: Some(window(0.51, None)),
            seven_day: None,
            using_overage: false,
        });

        assert_eq!(
            rows_of(&app, 66),
            vec!["5h  51% ▓▓▓▓▓▓░░░░░░".to_owned()],
            "a window nobody timed was given a reset, or the window nobody \
             reported was given a row"
        );
    }

    #[test]
    fn a_reset_that_has_already_come_around_is_not_drawn_as_one_still_to_come() {
        let app = windowed(UsageWindows {
            five_hour: Some(window(0.51, Some(A_FRIDAY + 9 * 3_600))),
            seven_day: None,
            using_overage: false,
        });

        assert_eq!(rows_of(&app, 66), vec!["5h  51% ▓▓▓▓▓▓░░░░░░".to_owned()]);
    }

    #[test]
    fn a_session_no_backend_metered_draws_no_window_row_at_all() {
        let app = App::new(crate::app::Repo {
            name: "niobe".to_owned(),
            branch: None,
            ..Default::default()
        });

        assert!(rows_of(&app, 66).is_empty());
        assert_eq!(window_rows(&app), 0);
    }

    #[test]
    fn what_the_extra_costs_is_an_em_dash_until_a_backend_reports_it() {
        let app = windowed(UsageWindows {
            five_hour: Some(window(1.0, None)),
            seven_day: None,
            using_overage: true,
        });
        let rows = rows_of(&app, 66);

        assert_eq!(rows.last().map(String::as_str), Some("extra — · on"));
        assert!(
            !rows.iter().any(|row| row.contains("$0.00")),
            "a figure nobody reported reached the pane: {rows:?}"
        );
    }

    /// A plan not spending beyond its flat fee and a backend that never said
    /// are the same `false`, so the row is absent rather than promising that
    /// nothing extra is being charged.
    #[test]
    fn a_plan_that_did_not_say_it_is_spending_extra_gets_no_extra_row() {
        let app = windowed(UsageWindows {
            five_hour: Some(window(0.51, None)),
            seven_day: None,
            using_overage: false,
        });

        assert!(!rows_of(&app, 66).iter().any(|row| row.contains("extra")));
    }

    /// The pane is sized from the row count before the rows are built, so the
    /// two have to agree or the pane clips its own last row.
    #[test]
    fn the_rows_the_pane_is_sized_for_are_the_rows_it_draws() {
        for windows in [
            UsageWindows::default(),
            UsageWindows {
                five_hour: Some(window(0.51, Some(A_FRIDAY + 16 * 3_600))),
                seven_day: None,
                using_overage: false,
            },
            UsageWindows {
                five_hour: Some(window(0.51, None)),
                seven_day: Some(window(0.71, None)),
                using_overage: true,
            },
        ] {
            let app = windowed(windows);
            assert_eq!(window_rows(&app), rows_of(&app, 66).len(), "{windows:?}");
        }
    }

    /// The right-hand column is thirty-nine columns wide at a hundred and
    /// twenty, and the reset time is what the row is for: the meter gives up
    /// cells until the row fits, and a row still too wide for the pane is one
    /// the terminal would cut mid-word.
    #[test]
    fn a_row_fits_the_pane_it_is_drawn_in() {
        let app = windowed(UsageWindows {
            five_hour: Some(window(0.51, Some(A_FRIDAY + 16 * 3_600 + 40 * 60))),
            seven_day: Some(window(0.71, Some(A_FRIDAY + 4 * 86_400 + 9 * 3_600))),
            using_overage: true,
        });

        for width in [39, 45, 66] {
            for row in rows_of(&app, width) {
                assert!(
                    text::width(&row) <= width,
                    "{row:?} is {} columns in a pane {width} wide",
                    text::width(&row)
                );
            }
        }
        assert!(
            rows_of(&app, 39)
                .iter()
                .all(|row| row.contains("resets") || row.contains("extra")),
            "the reset time was dropped before the meter gave up a cell"
        );
    }

    /// A session that spent tokens under three models, one of which settled.
    fn spending(records: &[(&str, u64)]) -> App {
        let mut app = App::new(crate::app::Repo {
            name: "example".to_owned(),
            branch: None,
            ..Default::default()
        });
        for (model, input) in records {
            app.apply(&niobe_core::event::Event::Usage(Usage {
                input: *input,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                cache_write_1h: 0,
                reasoning: 0,
                model: (*model).to_owned(),
                cost_usd: Some(0.01),
                settles_model: false,
                fast: false,
            }));
        }
        app
    }

    fn spend_of(app: &App, width: usize) -> Vec<String> {
        spend_lines(app, width, &Theme::default())
            .iter()
            .map(Line::to_string)
            .collect()
    }

    /// A table of published windows with one model in it, standing in for
    /// the bundled one.
    #[derive(Debug)]
    struct OpusHasTwoHundredThousand;

    impl Prices for OpusHasTwoHundredThousand {
        fn estimate(&self, _usage: &Usage) -> Option<f64> {
            None
        }

        fn context_window(&self, model: &str) -> Option<u64> {
            (model == "opus-5").then_some(200_000)
        }
    }

    fn sent(app: &mut App, tokens: u64, model: &str, window: Option<u64>) {
        app.apply(&niobe_core::event::Event::Context(
            niobe_core::event::Context {
                tokens,
                model: model.to_owned(),
                window,
            },
        ));
    }

    fn context_of(app: &App, width: usize) -> Vec<String> {
        context_lines(app, width, &Theme::default())
            .iter()
            .map(Line::to_string)
            .collect()
    }

    fn bare() -> App {
        App::new(crate::app::Repo {
            name: "niobe".to_owned(),
            branch: None,
            ..Default::default()
        })
    }

    /// 76,000 of 200,000 is 38%, which twelve cells draw as five.
    #[test]
    fn the_context_reads_as_a_meter_its_share_and_both_figures() {
        let mut app = bare();
        sent(&mut app, 76_000, "opus-5", Some(200_000));
        assert_eq!(
            context_of(&app, 39),
            ["context  38% ▓▓▓▓▓░░░░░░░ 76k / 200k"]
        );
    }

    /// Nothing has been sent, so nothing has been measured: no row, where a
    /// `0%` would say the context is empty.
    #[test]
    fn a_session_that_has_sent_nothing_draws_no_context_row() {
        let app = bare().with_prices(Box::new(OpusHasTwoHundredThousand));
        assert!(context_of(&app, 39).is_empty());
        assert_eq!(context_rows(&app), 0);
    }

    /// The backend did not say how big the window is, so the published one
    /// is read — and only where the table lists the model.
    #[test]
    fn a_window_the_backend_did_not_report_is_read_from_the_published_table() {
        let mut app = bare().with_prices(Box::new(OpusHasTwoHundredThousand));
        sent(&mut app, 50_000, "opus-5", None);
        assert_eq!(
            context_of(&app, 39),
            ["context  25% ▓▓▓░░░░░░░░░ 50k / 200k"]
        );
    }

    /// What the backend reported for the model it is running wins over the
    /// table: a plan can select another window than the published default.
    #[test]
    fn the_window_the_backend_reported_wins_over_the_published_one() {
        let mut app = bare().with_prices(Box::new(OpusHasTwoHundredThousand));
        sent(&mut app, 50_000, "opus-5", Some(1_000_000));
        assert_eq!(
            context_of(&app, 39),
            ["context   5% ▓░░░░░░░░░░░ 50k / 1.0M"]
        );
    }

    /// No window anywhere: the size alone, with no bar and no share, the way
    /// an unlisted model reads `unpriced` rather than a guessed price.
    #[test]
    fn a_model_with_no_known_window_shows_its_context_and_no_share() {
        let mut app = bare().with_prices(Box::new(OpusHasTwoHundredThousand));
        sent(&mut app, 76_000, "gpt-5.6", None);
        let rows = context_of(&app, 39);
        assert_eq!(rows, ["context 76k"]);
        assert!(!rows[0].contains('%'), "{rows:?}");
    }

    /// A compaction makes the next request smaller, and the row follows it
    /// down rather than keeping the largest context the session ever had.
    #[test]
    fn a_compacted_context_is_drawn_at_the_size_of_the_next_request() {
        let mut app = bare();
        sent(&mut app, 180_000, "opus-5", Some(200_000));
        app.apply(&niobe_core::event::Event::Notice {
            message: "the context was compacted (auto).".to_owned(),
        });
        sent(&mut app, 30_000, "opus-5", Some(200_000));
        assert_eq!(
            context_of(&app, 39),
            ["context  15% ▓▓░░░░░░░░░░ 30k / 200k"]
        );
    }

    /// Past the window, the bar is full and the figure says by how much.
    #[test]
    fn a_context_past_its_window_is_not_clamped() {
        let mut app = bare();
        sent(&mut app, 210_000, "opus-5", Some(200_000));
        assert_eq!(
            context_of(&app, 39),
            ["context 105% ▓▓▓▓▓▓▓▓▓▓▓▓ 210k / 200k"]
        );
    }

    /// A context no window could hold draws the most the column shows rather
    /// than twenty digits of percent.
    #[test]
    fn a_context_far_past_its_window_draws_at_most_999_percent() {
        let mut app = bare();
        sent(&mut app, u64::MAX, "opus-5", Some(1));
        assert_eq!(
            context_of(&app, 39),
            ["context 999% ▓▓▓▓▓▓▓▓▓▓▓▓ 18.4E / 1"]
        );
    }

    /// The meter gives up cells before the figures give up characters.
    #[test]
    fn a_narrow_pane_shortens_the_meter_and_keeps_the_figures() {
        let mut app = bare();
        sent(&mut app, 76_000, "opus-5", Some(200_000));
        assert_eq!(context_of(&app, 30), ["context  38% ▓▓░░░░ 76k / 200k"]);
    }

    #[test]
    fn the_context_rows_the_pane_is_sized_for_are_the_rows_it_draws() {
        let mut app = bare();
        assert_eq!(context_rows(&app), context_of(&app, 39).len());
        sent(&mut app, 76_000, "opus-5", None);
        assert_eq!(context_rows(&app), context_of(&app, 39).len());
    }

    /// The pane is sized from the row count before the rows are built, so the
    /// two have to agree or the pane clips the cache row off its own bottom.
    #[test]
    fn the_token_rows_the_pane_is_sized_for_are_the_rows_it_draws() {
        for records in [
            &[][..],
            &[("opus-5", 100)][..],
            &[("opus-5", 100), ("haiku-4-5", 50), ("sonnet-5", 20)][..],
        ] {
            let app = spending(records);
            assert_eq!(spend_rows(&app), spend_of(&app, 66).len(), "{records:?}");
        }
    }

    /// A model nothing was billed under is not a model at 0%: the row is
    /// absent, and with no model at all the block says so rather than drawing
    /// a bar at nothing.
    #[test]
    fn a_model_that_spent_nothing_gets_no_row() {
        assert_eq!(spend_of(&spending(&[]), 66), ["no tokens reported yet"]);

        let nothing_spent = spending(&[("opus-5", 0)]);
        assert_eq!(
            nothing_spent.session().totals().records,
            1,
            "the record was folded"
        );
        assert_eq!(spend_of(&nothing_spent, 66), ["no tokens reported yet"]);
    }

    /// The label column is as wide as the widest label in the block, so the
    /// figures line up, and the meter gives up cells until the row fits — the
    /// same order of precedence a window row has.
    #[test]
    fn a_token_row_fits_the_pane_it_is_drawn_in() {
        let app = spending(&[
            ("claude-opus-5", 62_000),
            ("claude-sonnet-5-20250929", 11_000),
            ("claude-haiku-4-5-20251001", 3_000),
        ]);

        for width in [39, 45, 66] {
            for row in spend_of(&app, width) {
                assert!(
                    text::width(&row) <= width,
                    "{row:?} is {} columns in a pane {width} wide",
                    text::width(&row)
                );
            }
        }
        // The shares and the counts survive the narrowest width; only the
        // meter gives ground.
        // 62k of 76k is 81.6%, 11k is 14.5% and 3k is 3.9%; floored that is
        // 81 + 14 + 3, and the two percent left over go to the two shares
        // flooring cut by most.
        let narrow = spend_of(&app, 39);
        assert!(
            narrow[0].contains(" 82%") && narrow[0].contains("62k"),
            "{narrow:?}"
        );
        assert!(narrow[1].contains(" 14%"), "{narrow:?}");
        assert!(narrow[2].contains("  4%"), "{narrow:?}");
        assert_eq!(narrow.len(), 4, "{narrow:?}");
    }

    /// Where no request has been eligible for a cache, the row has no figure —
    /// and no count beside one either.
    #[test]
    fn the_cache_row_reads_an_em_dash_before_anything_was_eligible() {
        let mut app = spending(&[]);
        app.apply(&niobe_core::event::Event::Usage(Usage {
            input: 0,
            output: 400,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 0,
            model: "opus-5".to_owned(),
            cost_usd: Some(0.01),
            settles_model: false,
            fast: false,
        }));

        let rows = spend_of(&app, 66);
        let cache = rows.last().expect("the cache row is the last of them");
        assert!(cache.starts_with("cache hit"), "{rows:?}");
        assert!(cache.contains('—'), "{rows:?}");
        assert!(!cache.contains('%'), "{rows:?}");
        assert!(!cache.contains('0'), "{rows:?}");
    }

    /// Every section's header, each at its fullest: every figure it can
    /// carry, with the failures, cancellations and ignored tests that a quiet
    /// session leaves out.
    fn every_header(theme: &Theme) -> Vec<(&'static str, Vec<Figure>)> {
        let tools = ToolTotals {
            finished: 1234,
            failed: 17,
            denied: 3,
            output_bytes: 168_000,
            ..ToolTotals::default()
        };
        vec![
            ("Sub-agents", agent_figures(2, 13, 4, (1, 0), theme)),
            (
                "Working tree",
                changed_figures(23, "+9770".to_owned(), "−2590".to_owned(), theme),
            ),
            ("Tools", tool_summary(&tools, theme)),
        ]
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn figure_text(figure: &Figure) -> String {
        figure
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// The header drawn from exactly `kept`, in order, each whole and set off
    /// by its own separator.
    fn header_of(name: &str, kept: &[&Figure]) -> String {
        let mut said = format!("▾ {name}");
        for (i, figure) in kept.iter().enumerate() {
            said.push_str(match i {
                0 => "  ",
                _ => figure.sep,
            });
            said.push_str(&figure_text(figure));
        }
        said
    }

    /// Which of `figures` the drawn header kept, read back by rebuilding the
    /// header from every subset in order until one matches exactly.
    fn kept_by<'a>(name: &str, figures: &'a [Figure], drawn: &str) -> Option<Vec<&'a Figure>> {
        (0u32..1 << figures.len()).find_map(|mask| {
            let kept: Vec<&Figure> = figures
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, figure)| figure)
                .collect();
            (header_of(name, &kept) == drawn).then_some(kept)
        })
    }

    #[test]
    fn a_wide_agent_tag_leaves_the_agents_status_whole_at_the_pane_edge() {
        let theme = Theme::default();
        let columns = AgentColumns {
            tag: text::width("漢字漢字"),
            model: 0,
            status: text::width("running"),
            task: 0,
        };
        let row = line_text(&agent_row(
            "漢字漢字",
            "書く",
            (None, "running"),
            columns.fitted(60),
            &theme,
        ));
        assert!(row.ends_with(" running"), "{row:?}");
        assert_eq!(text::width(&row), 60, "{row:?}");
    }

    #[test]
    fn a_wide_tool_family_keeps_the_count_column_in_line() {
        let theme = Theme::default();
        let name = text::width("漢字サーバー");
        let wide = line_text(&bar_line(("漢字サーバー", name), 3, 0, 3, 12, &theme));
        let plain = line_text(&bar_line(("Read", name), 3, 0, 3, 12, &theme));
        let count_cell = |row: &str| row.find(" 3 ").map(|at| text::width(&row[..at]));
        assert_eq!(count_cell(&wide), count_cell(&plain), "{wide:?} {plain:?}");
        assert_eq!(
            text::width(&wide),
            text::width(&plain),
            "{wide:?} {plain:?}"
        );
    }

    #[test]
    fn a_narrow_header_keeps_whole_figures_and_the_one_it_is_read_for() {
        let theme = Theme::default();
        for (name, figures) in every_header(&theme) {
            let line = section_header(false, name, figures.clone(), 39, &theme);
            let drawn = line_text(&line);
            assert!(text::width(&drawn) <= 39, "{drawn:?} overflows 39 columns");
            let kept = kept_by(name, &figures, &drawn)
                .unwrap_or_else(|| panic!("{drawn:?} is not whole figures of {name}"));
            assert!(
                kept.iter().any(|figure| figure.rank == 0),
                "{drawn:?} lost the figure {name} is read for"
            );
        }
    }

    #[test]
    fn a_tools_header_too_narrow_for_its_bytes_keeps_its_failures() {
        let theme = Theme::default();
        let tools = ToolTotals {
            finished: 6,
            failed: 2,
            denied: 0,
            output_bytes: 168_000,
            ..ToolTotals::default()
        };
        let line = section_header(false, "Tools", tool_summary(&tools, &theme), 34, &theme);
        assert_eq!(line_text(&line), "▾ Tools  6 calls ✗ 2 · 0 denied");
        let line = section_header(false, "Tools", tool_summary(&tools, &theme), 19, &theme);
        assert_eq!(line_text(&line), "▾ Tools  ✗ 2");
    }

    #[test]
    fn no_header_figure_is_ever_cut_at_any_width() {
        let theme = Theme::default();
        for (name, figures) in every_header(&theme) {
            for width in 0..=120 {
                let line = section_header(false, name, figures.clone(), width, &theme);
                let drawn = line_text(&line);
                assert!(
                    kept_by(name, &figures, &drawn).is_some(),
                    "{drawn:?} at {width} columns is not whole figures of {name}"
                );
            }
        }
    }

    #[test]
    fn the_least_important_figure_gives_way_first_and_the_rest_keep_their_order() {
        let theme = Theme::default();
        let figures = agent_figures(2, 13, 4, (1, 0), &theme);
        let full = section_header(false, "Sub-agents", figures.clone(), 200, &theme);
        assert_eq!(
            line_text(&full),
            "▾ Sub-agents  2 running · 13 spawned · 4 failed · 1 cancelled"
        );
        let narrow = section_header(false, "Sub-agents", figures, 39, &theme);
        assert_eq!(line_text(&narrow), "▾ Sub-agents  2 running · 4 failed");
    }

    /// An edit of `lines` lines to `path`, as the transcript's own entry.
    fn edited(app: &mut App, id: &str, lines: usize) {
        use niobe_core::event::{Event, ToolOutcome};
        let path = format!("{id}.rs");
        app.apply(&Event::ToolCallStart {
            id: id.into(),
            name: "Write".to_owned(),
            input: path.clone(),
            summary: Some(path.clone()),
            agent: None,
        });
        app.apply(&Event::ToolCallEnd {
            id: id.into(),
            name: "Write".to_owned(),
            input: path.clone(),
            output: String::new(),
            bytes: 64,
            outcome: ToolOutcome::Ok,
            summary: Some(path.clone()),
            exit_code: None,
            error: None,
        });
        let written: String = (1..=lines).map(|n| format!("line {n}\n")).collect();
        app.apply(&Event::FileChange {
            path,
            added: Some(lines as u64),
            removed: Some(0),
            hunks: vec![niobe_core::diff::Hunk::created(&written).expect("lines to write")],
        });
        app.apply(&Event::AssistantMessage {
            text: format!("wrote {id}"),
            agent: None,
        });
    }

    /// Calls with nothing under them are rows of one table: no blank line
    /// between them, and one after the last before the assistant speaks. A
    /// call with a diff under it keeps its blank line.
    #[test]
    fn calls_with_nothing_under_them_are_drawn_on_consecutive_lines() {
        use niobe_core::event::{Event, ToolOutcome};
        let mut app = App::new(crate::app::Repo::default());
        for (id, name) in [("t1", "Read"), ("t2", "Bash"), ("t3", "Read")] {
            app.apply(&Event::ToolCallStart {
                id: id.into(),
                name: name.to_owned(),
                input: String::new(),
                summary: Some(format!("{id} does")),
                agent: None,
            });
            app.apply(&Event::ToolCallEnd {
                id: id.into(),
                name: name.to_owned(),
                input: String::new(),
                output: String::new(),
                bytes: 64,
                outcome: ToolOutcome::Ok,
                summary: Some(format!("{id} does")),
                exit_code: None,
                error: None,
            });
        }
        edited(&mut app, "after", 2);

        let theme = Theme::default();
        let (entries, drawn) = app.entries_to_draw();
        drawn.update(entries, 60, Detail::default(), None, &theme);
        let rows: Vec<String> = drawn
            .lines(0, 12)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        // The name column is as wide as `Write`, the widest name drawn.
        assert!(rows[0].starts_with("⚙ Read  t1 does"), "{rows:?}");
        assert!(rows[1].starts_with("⚙ Bash  t2 does"), "{rows:?}");
        assert!(rows[2].starts_with("⚙ Read  t3 does"), "{rows:?}");
        assert_eq!(rows[3], "", "a call with a diff stands apart: {rows:?}");
        assert!(rows[4].starts_with("⚙ Write after.rs"), "{rows:?}");
        let diff_ends = rows
            .iter()
            .position(|row| row.contains("wrote after"))
            .expect("the reply is drawn");
        assert_eq!(
            rows[diff_ends - 2],
            "",
            "a diff keeps its blank line: {rows:?}"
        );
    }

    /// Grouped by agent, each agent's rows in a turn are drawn together
    /// under a heading where it first did something; the session's own row
    /// stays where it was, and so does the next turn's prompt.
    #[test]
    fn grouped_by_agent_each_agents_rows_are_drawn_under_its_heading() {
        use niobe_core::event::{AgentId, Event};
        let mut app = App::new(crate::app::Repo::default());
        app.apply(&Event::UserMessage {
            text: "go".to_owned(),
        });
        for (id, kind) in [("toolu_a", "Explore"), ("toolu_b", "general-purpose")] {
            app.apply(&Event::AgentSpawn {
                id: AgentId::new(id),
                parent: None,
                kind: Some(kind.to_owned()),
                label: format!("{kind}: look at {id}"),
            });
        }
        let start = |id: &str, agent: Option<&str>| Event::ToolCallStart {
            id: id.into(),
            name: "Read".to_owned(),
            input: String::new(),
            summary: Some(format!("{id}.rs")),
            agent: agent.map(AgentId::new),
        };
        app.apply(&start("a1", Some("toolu_a")));
        app.apply(&start("b1", Some("toolu_b")));
        app.apply(&start("m1", None));
        app.apply(&start("a2", Some("toolu_a")));
        app.apply(&start("b2", Some("toolu_b")));

        let theme = Theme::default();
        let drawn_as = |app: &mut App| -> Vec<String> {
            let headings = app.grouped_by_agent().then(|| app.agent_headings());
            let (entries, drawn) = app.entries_to_draw();
            drawn.update(entries, 60, Detail::default(), headings.as_deref(), &theme);
            drawn
                .lines(0, 20)
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect()
                })
                .map(|row: String| row.trim_end().to_owned())
                .filter(|row| !row.is_empty())
                .collect()
        };

        let interleaved = drawn_as(&mut app);
        assert!(interleaved[2].contains("explore a1.rs"), "{interleaved:?}");
        assert!(interleaved[3].contains("general b1.rs"), "{interleaved:?}");

        app.group_by_agent();
        let grouped = drawn_as(&mut app);
        let rows: Vec<&str> = grouped.iter().skip(2).map(String::as_str).collect();
        assert!(
            rows[0].starts_with("▾ explore") && rows[0].contains("look at toolu_a"),
            "{grouped:?}"
        );
        assert!(rows[0].ends_with("2 calls"), "{grouped:?}");
        assert!(
            rows[1].contains("a1.rs") && rows[2].contains("a2.rs"),
            "{grouped:?}"
        );
        assert!(
            !rows[1].contains("explore"),
            "the heading names the agent: {grouped:?}"
        );
        assert!(rows[3].starts_with("▾ general"), "{grouped:?}");
        assert!(
            rows[4].contains("b1.rs") && rows[5].contains("b2.rs"),
            "{grouped:?}"
        );
        assert!(
            rows[6].contains("m1.rs"),
            "the session's own row stays: {grouped:?}"
        );
    }

    /// A test run in progress says how long it has run, so the clock moving
    /// lays it out again — once a second, and nothing else with it.
    #[test]
    fn the_clock_lays_out_again_only_a_test_run_still_going_and_once_a_second() {
        let second = |millis: u64| {
            crate::clock::Stamp::new(
                std::time::UNIX_EPOCH + std::time::Duration::from_millis(millis),
                None,
            )
        };
        let start = |id: &str, command: &str| niobe_core::Event::ToolCallStart {
            id: id.into(),
            name: "Bash".to_owned(),
            input: String::new(),
            summary: Some(command.to_owned()),
            agent: None,
        };
        let mut app = App::new(crate::app::Repo::default());
        app.apply_at(&start("l1", "ls"), second(0));
        app.apply_at(
            &niobe_core::Event::UserMessage {
                text: "and the tests".to_owned(),
            },
            second(0),
        );
        app.apply_at(&start("t1", "cargo test"), second(1_000));
        let keys = |now: u64| -> Vec<u64> {
            let detail = Detail {
                now: Some(second(now)),
                ..Detail::default()
            };
            app.entries()
                .iter()
                .map(|entry| drawn_from(entry, 120, detail, &Theme::default()))
                .collect()
        };

        let at = keys(5_000);
        assert_eq!(at, keys(5_900), "the same whole second draws the same");
        let later = keys(6_000);
        let changed: Vec<usize> = (0..at.len()).filter(|&i| at[i] != later[i]).collect();
        assert_eq!(changed, vec![at.len() - 1], "only the test run moved");
    }

    #[test]
    fn opening_the_diffs_lays_out_again_only_the_entries_that_hold_a_cut_one() {
        let mut app = App::new(crate::app::Repo::default());
        edited(&mut app, "short", 3);
        edited(&mut app, "long", crate::hunks::MAX_ROWS + 10);
        let keys = |detail: Detail| -> Vec<u64> {
            app.entries()
                .iter()
                .map(|entry| drawn_from(entry, 120, detail, &Theme::default()))
                .collect()
        };

        let cut = keys(Detail::default());
        let open = keys(Detail {
            diffs_open: true,
            ..Detail::default()
        });

        let changed: Vec<usize> = (0..cut.len()).filter(|&at| cut[at] != open[at]).collect();
        let long = app
            .entries()
            .iter()
            .position(holds_a_cut_diff)
            .expect("the long write is cut");
        assert_eq!(changed, vec![long]);
    }

    /// A metered session that spent `cost` over one turn of `seconds`.
    fn metered_for(seconds: u64, cost: f64) -> App {
        use niobe_core::event::Event;
        let at = |seconds| {
            crate::clock::Stamp::new(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds),
                None,
            )
        };
        let mut app = App::new(crate::app::Repo::default());
        app.apply_at(
            &Event::Billing {
                billing: Billing::Metered,
            },
            at(0),
        );
        app.apply_at(
            &Event::UserMessage {
                text: "go".to_owned(),
            },
            at(0),
        );
        app.apply_at(&priced(Some(cost)), at(0));
        app.apply_at(&Event::TurnEnded, at(seconds));
        app
    }

    #[test]
    fn a_session_billed_both_ways_draws_only_what_it_spent_by_use() {
        use niobe_core::event::Event;
        let mut app = metered_for(1_800, 0.5);
        for billing in [Billing::Plan, Billing::Metered] {
            app.apply(&Event::Billing { billing });
        }

        let said: Vec<String> = money_lines(&app, 40, &crate::theme::CLASSIC)
            .iter()
            .map(line_text)
            .collect();
        assert_eq!(said[0], "metered part $0.50");
    }

    /// The rows of the Usage pane, 38 columns wide, that draw a dollar sign.
    fn dollar_rows(app: &App) -> Vec<String> {
        pane_of(app, 38)
            .into_iter()
            .filter(|row| row.contains('$'))
            .collect()
    }

    #[test]
    fn every_dollar_figure_of_a_session_billed_both_ways_is_its_metered_part() {
        use niobe_core::event::Event;
        for ([first, then], spent) in [
            ([Billing::Plan, Billing::Metered], "$2.00"),
            ([Billing::Metered, Billing::Plan], "$1.00"),
        ] {
            let mut app = App::new(crate::app::Repo::default()).with_budget(5.0);
            app.apply(&Event::Billing { billing: first });
            app.apply(&priced(Some(1.0)));
            app.apply(&Event::Billing { billing: then });
            app.apply(&priced(Some(2.0)));

            assert_eq!(
                dollar_rows(&app),
                [
                    format!("metered part {spent}"),
                    format!("metered part budget {spent}/$5.00")
                ],
                "{first:?} then {then:?}"
            );
        }
    }

    #[test]
    fn a_plan_session_draws_no_dollar_figure_anywhere_in_the_pane() {
        use niobe_core::event::Event;
        let mut app = App::new(crate::app::Repo::default()).with_budget(5.0);
        app.apply(&Event::Billing {
            billing: Billing::Plan,
        });
        app.apply(&Event::UsageWindows(UsageWindows {
            five_hour: Some(UsageWindow {
                utilization: 0.4,
                resets_at: None,
            }),
            seven_day: None,
            using_overage: false,
        }));
        app.apply(&priced(Some(1.25)));

        let pane = pane_of(&app, 38);
        assert_eq!(dollar_rows(&app), Vec::<String>::new(), "{pane:?}");
        assert!(!pane.iter().any(|row| row.contains("API")), "{pane:?}");
        assert_eq!(
            pane.last().map(String::as_str),
            Some("budget —"),
            "{pane:?}"
        );
    }

    #[test]
    fn a_session_nothing_said_the_billing_of_draws_no_dollar_spend_against_its_budget() {
        let mut app = App::new(crate::app::Repo::default()).with_budget(5.0);
        app.apply(&priced(Some(1.25)));

        assert_eq!(dollar_rows(&app), ["budget —/$5.00"]);
    }

    #[test]
    fn a_pane_too_narrow_for_the_rate_keeps_the_cost_and_drops_the_rate() {
        let app = metered_for(1_800, 0.5);
        let wide: Vec<String> = money_lines(&app, 40, &crate::theme::CLASSIC)
            .iter()
            .map(line_text)
            .collect();
        assert_eq!(wide[0], "session $0.50 · $1.00/h worked");

        let narrow: Vec<String> = money_lines(&app, 29, &crate::theme::CLASSIC)
            .iter()
            .map(line_text)
            .collect();
        assert_eq!(narrow[0], "session $0.50");
    }

    #[test]
    fn a_spend_under_a_cent_reads_as_under_a_cent_not_as_nothing() {
        assert_eq!(
            session_cost(&SessionState::replay(&[priced(Some(0.004))]), None),
            "<$0.01"
        );
        assert_eq!(
            session_cost(&SessionState::replay(&[priced(Some(0.0))]), None),
            "$0.00"
        );
        // 1,100 tokens at a tenth of a cent per thousand is $0.0011.
        let owed = SessionState::replay(&[priced(None)]);
        assert_eq!(
            session_cost(&owed, Some(&ATenthOfACentPerThousand)),
            "<~$0.01"
        );
        let floor = SessionState::replay(&[priced(Some(0.004)), priced(None)]);
        assert_eq!(session_cost(&floor, Some(&NothingIsPriced)), ">$0.00");
        assert_eq!(model_cost(floor.totals(), "opus-5", None), ">$0.00");

        // $0.002 over half an hour is $0.004 an hour: both under a cent.
        let app = metered_for(1_800, 0.002);
        let said: Vec<String> = money_lines(&app, 40, &crate::theme::CLASSIC)
            .iter()
            .map(line_text)
            .collect();
        assert_eq!(said[0], "session <$0.01 · <$0.01/h worked");
    }

    #[test]
    fn a_models_cost_is_set_off_from_its_count_however_wide_it_is() {
        for cost in [150.0, 1_000.0, 1e12] {
            let mut app = spending(&[]);
            app.apply(&niobe_core::event::Event::Billing {
                billing: Billing::Metered,
            });
            app.apply(&niobe_core::event::Event::Usage(Usage {
                input: 1_000_000,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                cache_write_1h: 0,
                reasoning: 0,
                model: "opus-5".to_owned(),
                cost_usd: Some(cost),
                settles_model: false,
                fast: false,
            }));
            let rows = spend_of(&app, 66);
            let (count, cost_drawn) = rows[0]
                .split_once('$')
                .expect("a metered model's row carries its cost");
            assert!(
                count.ends_with(' ') && count.trim_end().ends_with("1.0M"),
                "{cost}: {rows:?}"
            );
            assert_eq!(cost_drawn, format!("{cost:.2}"), "{cost}: {rows:?}");
        }
    }

    /// A metered session on `records` of a model, the input it sent and the
    /// cost reported for it, whose last request sent 12,000 tokens into a
    /// window of 200,000.
    fn metered_on(records: &[(&str, u64, Option<f64>)]) -> App {
        use niobe_core::event::Event;
        let mut app = bare();
        app.apply(&Event::Billing {
            billing: Billing::Metered,
        });
        for (model, input, cost) in records {
            app.apply(&Event::Usage(Usage {
                input: *input,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                cache_write_1h: 0,
                reasoning: 0,
                model: (*model).to_owned(),
                cost_usd: *cost,
                settles_model: false,
                fast: false,
            }));
        }
        sent(&mut app, 12_000, "opus-5", Some(200_000));
        app
    }

    fn pane_of(app: &App, width: usize) -> Vec<String> {
        usage_lines(app, width, &Theme::default())
            .iter()
            .map(Line::to_string)
            .collect()
    }

    /// Thirty-eight columns is the pane inside its border at 120. The two ids
    /// differ only in their date, so their labels are cut at the front; the
    /// meters go before any figure does.
    #[test]
    fn two_long_ids_that_collide_give_way_to_every_figure_beside_them() {
        let app = metered_on(&[
            ("claude-opus-5-20251001", 10_000, Some(12.34)),
            ("claude-opus-5-20260101", 5_100, Some(3.21)),
        ]);
        assert_eq!(
            pane_of(&app, 38),
            [
                "session $15.55",
                "──────────────────────────────────────",
                "…-opus-5-20251001  66%    10k   $12.34",
                "…-opus-5-20260101  34%   5100    $3.21",
                "cache hit           0%      0",
                "──────────────────────────────────────",
                "context             6% ▓░░░ 12k / 200k",
            ]
        );
    }

    /// A Bedrock inference profile's id is longer than the pane is wide; the
    /// cache and the context keep their shares and their figures at 120
    /// columns and at 160.
    #[test]
    fn an_inference_profile_id_leaves_the_cache_and_the_context_their_figures() {
        let profile =
            "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/a1b2c3d4e5f6";
        let app = metered_on(&[
            (
                "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
                9_000,
                Some(2.5),
            ),
            (profile, 1_000, Some(0.25)),
        ]);
        for width in [38, 52] {
            let rows = pane_of(&app, width);
            for row in &rows {
                assert!(text::width(row) <= width, "{row:?} overflows {width}");
            }
            let cache = rows
                .iter()
                .find(|row| row.starts_with("cache hit"))
                .unwrap_or_else(|| panic!("no cache row at {width}: {rows:?}"));
            assert!(cache.contains("  0%"), "{rows:?}");
            let context = rows.last().expect("the context row is the last");
            assert!(context.starts_with("context"), "{rows:?}");
            assert!(context.contains("  6%"), "{rows:?}");
            assert!(context.ends_with(" 12k / 200k"), "{rows:?}");
            assert!(rows[2].ends_with(" $2.50"), "{rows:?}");
            assert!(rows[3].ends_with(" $0.25"), "{rows:?}");
        }
    }

    /// `≥$1234.56` is a column wider than `≥~$12.34`, the widest the pane
    /// sets aside by default; the cost column grows to it rather than cut it.
    #[test]
    fn a_cost_wider_than_its_column_is_drawn_whole() {
        let app = metered_on(&[("opus-5", 2_000, Some(1_234.56)), ("opus-5", 200, None)]);
        let rows = pane_of(&app, 38);
        assert_eq!(rows[2], "opus-5    100% ▓▓▓▓▓▓▓  2200 ≥$1234.56");
    }

    /// Too narrow for the window's size beside the share, the row gives up
    /// the bar and then the size whole, never a digit of it.
    #[test]
    fn a_context_row_too_narrow_for_its_window_drops_the_window_whole() {
        let mut app = bare();
        sent(&mut app, 76_000, "opus-5", Some(200_000));
        assert_eq!(context_of(&app, 15), ["  38%  76k"]);
    }
}
