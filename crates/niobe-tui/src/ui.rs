// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Drawing the four regions: menu bar, panes, status line, F-key bar.
//!
//! Every number on screen comes off [`App::session`], which is a fold over
//! events and nothing else. Where the fold has nothing to say the pane says so
//! — an em dash and, where it is not obvious, a note that the feature is not
//! implemented yet. Nothing here invents a figure, because a figure nobody
//! measured is indistinguishable from one that was, and that is the product
//! gone.
//!
//! The cost pane has no cost-per-edit tile: that needs spend attributed to
//! individual calls, which the ledger does not do yet. The fourth tile is a
//! measured one instead.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Widget,
};

use niobe_core::event::UsageWindows;
use niobe_core::session::{FileChanges, SessionState};

use crate::app::{
    Activity, Answer, App, Ask, Entry, EntryKind, Picker, SelectedProfile, tool_label,
};
use crate::text;
use crate::theme::Theme;

/// Smallest terminal the shell draws in, as (columns, rows).
pub const MIN_SIZE: (u16, u16) = (80, 24);

/// The width at which the right stack fits beside the session pane. Below it
/// the session pane takes the whole body: three more panes squeezed into forty
/// columns each is less readable than the transcript they were taken from.
pub const WIDE_COLUMNS: u16 = 100;

/// Columns the transcript gives to an entry's glyph.
const GUTTER: usize = 2;

/// What a changed file's row opens with, and the columns it costs.
const MARKER: &str = "\u{25b8} ";

/// Files the changes pane lists before it says how many more there are.
///
/// Six with their explanations is about half the pane, which leaves the
/// decisions and the tool mix under them visible. A session that touched more
/// files than this is one whose whole diff belongs somewhere with room for it,
/// not in a corner of the shell.
const FILES_SHOWN: usize = 6;

/// Widest the permission modal is drawn, in columns. Wide enough for a shell
/// command that has a path in it, and narrow enough to leave the transcript
/// around it readable, so the operator can see what led to the prompt.
const ASK_COLUMNS: u16 = 72;

/// Columns of margin a modal leaves on each side of a narrow screen.
const ASK_MARGIN: u16 = 4;

/// Widest the model list is drawn, in columns. A model id is a word or two, so
/// the list is narrow enough to read as a list rather than as a pane.
const PICK_COLUMNS: u16 = 44;

/// What marks the model the session is on, and the one the cursor is over.
const PICK_CURSOR: &str = "› ";
const PICK_CURRENT: &str = "· ";

/// The share of a budget at which the status line starts saying so in the
/// colour it uses for anything waiting on the operator. The same fraction the
/// transcript warning uses, so the line and the warning agree.
///
/// A plan's usage window is read against the same fraction, because on a
/// flat-rate plan the window is the budget: what runs out is the hours, not
/// the money.
const BUDGET_SHOWN_HOT: f64 = 0.8;

/// The F-key bar, which is also the list of what the shell can be asked to do.
const FKEYS: [(&str, &str); 10] = [
    ("1", "Help"),
    ("2", "Plan"),
    ("3", "Diff"),
    ("4", "Undo"),
    ("5", "Cost"),
    ("6", "Files"),
    ("7", "Tools"),
    ("8", "Model"),
    ("9", "Theme"),
    ("10", "Quit"),
];

/// The menu bar's items. The first letter is the hot key.
const MENUS: [&str; 7] = [
    "Niobe", "Session", "Files", "Tools", "Cost", "Options", "Help",
];

/// Draws one frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let theme = *app.theme();
    let area = frame.area();

    if area.width < MIN_SIZE.0 || area.height < MIN_SIZE.1 {
        draw_too_small(frame, area, &theme);
        return;
    }

    frame.render_widget(
        Block::new().style(Style::new().bg(theme.pane_bg).fg(theme.fg)),
        area,
    );

    let [menu, body, status, fkeys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(2),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_menu(frame, menu, app, &theme);
    draw_body(frame, body, app, &theme);
    draw_status(frame, status, app, &theme);
    draw_fkeys(frame, fkeys, &theme);

    // Last, and over the body: a prompt is what the session is waiting on, so
    // nothing drawn afterwards may cover it. The model list is drawn under the
    // same rule and never beside it — the shell hands the keyboard to one
    // question at a time.
    if let Some(ask) = app.asking() {
        draw_ask(frame, body, app, ask, &theme);
    } else if let Some(picker) = app.picking() {
        draw_pick(frame, body, picker, app.session().model(), &theme);
    }
}

/// The model list: what the profile offers, which one the session is on, and
/// the three keys that work.
///
/// The footer says when a choice takes effect. A switch applies from the next
/// turn, and a list that did not say so would read as though the reply being
/// written were already coming from the new model.
fn draw_pick(frame: &mut Frame, body: Rect, picker: &Picker, current: Option<&str>, theme: &Theme) {
    let width = PICK_COLUMNS.min(body.width.saturating_sub(ASK_MARGIN * 2));
    if width < 20 {
        return;
    }

    let text_width = usize::from(width).saturating_sub(DIALOG_INSET);
    let mut lines: Vec<Line> = vec![Line::from("")];
    for (i, model) in picker.models.iter().enumerate() {
        let on_it = i == picker.at;
        let marker = match (on_it, current == Some(model.as_str())) {
            (true, _) => PICK_CURSOR,
            (false, true) => PICK_CURRENT,
            (false, false) => "  ",
        };
        let row = format!(
            "{marker}{:<room$}",
            text::truncate(model, text_width.saturating_sub(2)),
            room = text_width.saturating_sub(2)
        );
        lines.push(match on_it {
            true => Line::from(row).style(
                Style::new()
                    .bg(theme.button_focus_bg)
                    .fg(theme.button_focus_fg)
                    .bold(),
            ),
            false => Line::from(row).style(Style::new().fg(theme.dialog_fg)),
        });
    }
    lines.push(Line::from(""));
    lines.push(
        Line::from("↑↓ choose · Enter switch · Esc keep this one")
            .style(Style::new().fg(theme.dialog_fg)),
    );

    let inner = dialog(
        frame,
        body,
        (width, lines.len()),
        ("Model", " applied from the next turn "),
        theme,
    );
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The permission dialog: what would run, and a button for each way to
/// answer.
///
/// The whole of the arguments is shown under the target, wrapped rather than
/// truncated, and so is the rule `Pin this` would save. Approving a call whose
/// arguments were cut off at the edge, or keeping a rule nobody could read
/// whole, is approving something the operator did not read.
fn draw_ask(frame: &mut Frame, body: Rect, app: &App, ask: &Ask, theme: &Theme) {
    let width = ASK_COLUMNS.min(body.width.saturating_sub(ASK_MARGIN * 2));
    if width < 20 {
        return;
    }

    let text_width = usize::from(width).saturating_sub(DIALOG_INSET);
    let plain = Style::new().fg(theme.dialog_fg);
    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(tool_label(&ask.tool), plain.bold()),
            Span::styled(" wants to run", plain),
        ]),
    ];
    for wrapped in text::wrap(ask.target.as_deref().unwrap_or(&ask.input), text_width) {
        lines.push(Line::from(wrapped).style(plain.bold()));
    }
    if ask.target.is_some() {
        lines.push(Line::from(""));
        for wrapped in text::wrap(&ask.input, text_width) {
            lines.push(Line::from(wrapped).style(plain));
        }
    }
    if let Some(rule) = ask.target_rule() {
        lines.push(Line::from(""));
        for wrapped in text::wrap(&format!("Pin this saves {rule}"), text_width) {
            lines.push(Line::from(wrapped).style(plain.italic()));
        }
    }
    lines.push(Line::from(""));
    lines.extend(buttons(ask, app.ask_focus(), text_width, theme));

    let waiting = app.asks_waiting();
    let footer = match waiting {
        0 => " the turn is waiting on you ".to_owned(),
        1 => " 1 more prompt behind this one ".to_owned(),
        n => format!(" {n} more prompts behind this one "),
    };
    let inner = dialog(
        frame,
        body,
        (width, lines.len()),
        ("Permission", &footer),
        theme,
    );
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Columns a dialog's frame and padding take from its width: a border and two
/// columns of padding on each side.
const DIALOG_INSET: usize = 6;

/// Draws a dialog's frame, centred in `body` over whatever is there, with its
/// shadow, and returns the area inside it. `size` is the width and the number
/// of lines it will hold; `titles` are the text on its top and bottom edges.
fn dialog(
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
        .border_type(BorderType::Double)
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
/// one, the way a dialog in Turbo Vision stands off the screen. What is under
/// the shadow keeps its characters, so the transcript still reads through it.
fn cast_shadow(frame: &mut Frame, area: Rect, bounds: Rect, theme: &Theme) {
    let style = Style::new().bg(theme.shadow).fg(theme.dim);
    let right = Rect::new(area.right(), area.y + 1, 2, area.height);
    let below = Rect::new(area.x + 2, area.bottom(), area.width, 1);
    for strip in [right, below] {
        frame
            .buffer_mut()
            .set_style(strip.intersection(bounds), style);
    }
}

/// The dialog's buttons, laid out left to right and onto another row where the
/// width runs out, each with its shadow under it.
fn buttons(ask: &Ask, focus: Answer, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let offered: Vec<(Answer, String, char)> = Answer::ALL
        .into_iter()
        .filter(|answer| ask.offers(*answer))
        .map(|answer| {
            let (label, hot) = button_label(answer, ask);
            (answer, label, hot)
        })
        .collect();

    let mut rows: Vec<Vec<(Answer, String, char)>> = vec![Vec::new()];
    let mut used = 0;
    for button in offered {
        // The label, a column of padding on each side and the column of
        // shadow after it, then a column of gap.
        let cells = text::width(&button.1) + 4;
        if used > 0 && used + cells > width {
            rows.push(Vec::new());
            used = 0;
        }
        used += cells;
        if let Some(row) = rows.last_mut() {
            row.push(button);
        }
    }

    let mut lines = Vec::new();
    for row in rows {
        let mut face = Vec::new();
        let mut under = vec![Span::raw(" ")];
        for (answer, label, hot) in row {
            face.extend(button_face(&label, hot, answer == focus, theme));
            face.push(Span::styled("▄", Style::new().fg(theme.shadow)));
            face.push(Span::raw(" "));
            under.push(Span::styled(
                "▀".repeat(text::width(&label) + 2),
                Style::new().fg(theme.shadow),
            ));
            under.push(Span::raw("  "));
        }
        lines.push(Line::from(face));
        lines.push(Line::from(under));
    }
    lines
}

/// What a button says, and the letter that presses it.
fn button_label(answer: Answer, ask: &Ask) -> (String, char) {
    match answer {
        Answer::Once => ("Yes, once".to_owned(), 'Y'),
        Answer::AlwaysTool => (
            format!("Always {}", text::truncate(&tool_label(&ask.tool), 24)),
            'A',
        ),
        Answer::AlwaysTarget => ("Pin this".to_owned(), 'P'),
        Answer::No => ("No".to_owned(), 'N'),
    }
}

/// One button: its label on the button colour with the hot letter picked out,
/// or, with the focus, on the focus colour between `►` and `◄`.
fn button_face(label: &str, hot: char, focused: bool, theme: &Theme) -> Vec<Span<'static>> {
    let (bg, fg) = match focused {
        true => (theme.button_focus_bg, theme.button_focus_fg),
        false => (theme.button_bg, theme.button_fg),
    };
    let face = Style::new().bg(bg).fg(fg);
    let (open, close) = match focused {
        true => ("►", "◄"),
        false => (" ", " "),
    };
    let mut spans = vec![Span::styled(open, face.bold())];
    match label.find(hot) {
        Some(at) => {
            let (before, rest) = label.split_at(at);
            let after = &rest[hot.len_utf8()..];
            spans.push(Span::styled(before.to_owned(), face));
            spans.push(Span::styled(
                hot.to_string(),
                face.fg(theme.button_hot).bold(),
            ));
            spans.push(Span::styled(after.to_owned(), face));
        }
        None => spans.push(Span::styled(label.to_owned(), face)),
    }
    spans.push(Span::styled(close, face.bold()));
    spans
}

/// What the shell says when it has fewer than eighty by twenty-four to draw in.
///
/// Drawing the four regions anyway would produce panes one row high with their
/// borders overlapping their contents, which reads as a broken program rather
/// than a small window.
fn draw_too_small(frame: &mut Frame, area: Rect, theme: &Theme) {
    let (columns, rows) = MIN_SIZE;
    let lines = vec![
        Line::from("niobe").style(Style::new().fg(theme.hot).bold()),
        Line::from(format!("needs {columns}×{rows}")),
        Line::from(format!("this window is {}×{}", area.width, area.height))
            .style(Style::new().fg(theme.dim)),
    ];

    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .style(Style::new().bg(theme.pane_bg).fg(theme.fg)),
        area,
    );
}

fn draw_menu(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let hot = Style::new().fg(theme.hot).bold();
    let plain = Style::new().fg(theme.menu_fg);

    let mut spans = vec![Span::raw(" ")];
    for name in MENUS {
        let mut chars = name.chars();
        let first = chars.next().unwrap_or(' ');
        spans.push(Span::styled(first.to_string(), hot));
        spans.push(Span::styled(chars.as_str().to_owned(), plain));
        spans.push(Span::raw("  "));
    }

    let left = Line::from(spans);
    let mut right = Vec::new();
    if area.width >= WIDE_COLUMNS {
        right.push(Span::styled(
            format!(
                "{}  ",
                backend_label(app.session(), app.profile(), app.is_attached())
            ),
            Style::new().fg(theme.menu_fg),
        ));
    }
    right.push(Span::styled("theme:", Style::new().fg(theme.menu_fg)));
    right.push(Span::styled(
        theme.name,
        Style::new().fg(theme.hot).bg(theme.menu_bg).bold(),
    ));
    right.push(Span::styled(" F9 ", Style::new().fg(theme.menu_fg)));

    let bar = Style::new().bg(theme.menu_bg).fg(theme.menu_fg);
    frame.render_widget(Paragraph::new(left).style(bar), area);
    frame.render_widget(
        Paragraph::new(Line::from(right))
            .alignment(Alignment::Right)
            .style(bar),
        area,
    );
}

fn draw_body(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    if area.width < WIDE_COLUMNS {
        draw_session(frame, area, app, theme);
        return;
    }

    // Session pane to right stack, 1.35 : 1.
    let [left, right] = Layout::horizontal([Constraint::Fill(135), Constraint::Fill(100)])
        .spacing(1)
        .areas(area);

    draw_session(frame, left, app, theme);

    let [cost, parallel, changes] = Layout::vertical([
        Constraint::Length(10),
        Constraint::Length(6),
        Constraint::Min(4),
    ])
    .areas(right);

    draw_cost(frame, cost, app.session(), theme);
    draw_parallel(frame, parallel, app, theme);
    draw_changes(frame, changes, app.session(), theme);
}

/// The pane frame every pane shares: double borders in the frame colour, the
/// title centred on the top edge.
fn pane(title: impl Into<String>, theme: &Theme) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Double)
        .border_style(Style::new().fg(theme.frame))
        .style(Style::new().bg(theme.pane_bg).fg(theme.fg))
        .title_top(
            Line::from(format!(" {} ", title.into()))
                .style(Style::new().fg(theme.title).bold())
                .centered(),
        )
}

fn draw_session(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let repo = &app.repo().name;
    let title = if repo.is_empty() {
        "Session".to_owned()
    } else {
        format!("Session ─ {repo}")
    };

    let mut block = pane(title, theme);
    if !app.follows_tail() {
        block = block.title_bottom(
            Line::from(" ↑ scrolled back · PgDn returns to the newest line ")
                .style(Style::new().fg(theme.dim))
                .centered(),
        );
    }

    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // The composer grows with what is typed into it, up to a third of the pane,
    // so a long prompt is editable without hiding the transcript behind it.
    let typed = app.composed().lines().count().max(1);
    let cap = usize::from(inner.height / 3).max(1);
    let composer_rows = u16::try_from(typed.min(cap)).unwrap_or(1);

    let [transcript, divider, composer] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(composer_rows),
    ])
    .areas(inner);

    draw_transcript(frame, transcript, app, theme);

    frame.render_widget(
        Paragraph::new(Line::from("─".repeat(usize::from(divider.width))))
            .style(Style::new().fg(theme.frame).bg(theme.pane_bg)),
        divider,
    );

    let [marker, editor] =
        Layout::horizontal([Constraint::Length(2), Constraint::Min(1)]).areas(composer);
    frame.render_widget(
        Paragraph::new(Line::from(">").style(Style::new().fg(theme.hot).bold()))
            .style(Style::new().bg(theme.pane_bg)),
        marker,
    );
    app.composer().render(editor, frame.buffer_mut());
}

fn draw_transcript(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
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

    if app.entries().is_empty() {
        let lines = empty_transcript(app.is_attached(), theme);
        app.measured(lines.len(), height);
        frame.render_widget(
            Paragraph::new(lines).style(Style::new().bg(theme.pane_bg)),
            area,
        );
        return;
    }

    let (entries, drawn) = app.entries_to_draw();
    drawn.update(entries, width, theme);
    let total = drawn.line_count();

    app.measured(total, height);
    let start = app.scroll().min(total);
    let (_, drawn) = app.entries_to_draw();
    let visible = drawn.lines(start, height);
    frame.render_widget(
        Paragraph::new(visible).style(Style::new().bg(theme.pane_bg)),
        area,
    );
    draw_scrollbar(frame, area, (start, total, height), theme);
}

/// Every transcript entry's lines as they were last drawn, with what they were
/// drawn from, so that a redraw renders again only the entries that changed.
///
/// A finished reply never changes, and parsing and wrapping every one of them
/// on every frame is what a long session's redraw would otherwise spend its
/// time on. An entry is drawn again when its text, the pane's width or the
/// theme changes, which is everything its lines depend on.
#[derive(Debug, Default)]
pub struct DrawnEntries {
    drawn: Vec<(u64, Vec<Line<'static>>)>,
}

impl DrawnEntries {
    /// Brings every entry's lines up to date.
    fn update(&mut self, entries: &[Entry], width: usize, theme: &Theme) {
        self.drawn.truncate(entries.len());
        for (at, entry) in entries.iter().enumerate() {
            let key = drawn_from(entry, width, theme);
            match self.drawn.get_mut(at) {
                Some((drawn_key, _)) if *drawn_key == key => {}
                Some(slot) => *slot = (key, entry_lines(entry, width, theme)),
                None => self.drawn.push((key, entry_lines(entry, width, theme))),
            }
        }
    }

    fn line_count(&self) -> usize {
        self.drawn.iter().map(|(_, lines)| lines.len()).sum()
    }

    /// `count` lines from line `start` of the whole transcript.
    fn lines(&self, start: usize, count: usize) -> Vec<Line<'static>> {
        self.drawn
            .iter()
            .flat_map(|(_, lines)| lines)
            .skip(start)
            .take(count)
            .cloned()
            .collect()
    }
}

/// What an entry's lines are drawn from, as one number.
fn drawn_from(entry: &Entry, width: usize, theme: &Theme) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    entry.hash(&mut hasher);
    width.hash(&mut hasher);
    theme.hash(&mut hasher);
    hasher.finish()
}

/// Where the view is in a transcript longer than the pane, drawn over the
/// pane's right border beside the lines it measures. A transcript that fits
/// has no scrollbar, so the border reads as a border.
fn draw_scrollbar(frame: &mut Frame, area: Rect, extent: (usize, usize, usize), theme: &Theme) {
    let (start, lines, height) = extent;
    if lines <= height || area.height == 0 {
        return;
    }
    let border = Rect::new(area.right(), area.y, 1, area.height);
    let mut state = ScrollbarState::new(lines.saturating_sub(height))
        .viewport_content_length(height)
        .position(start);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("║"))
            .track_style(Style::new().fg(theme.frame))
            .thumb_symbol("█")
            .thumb_style(Style::new().fg(theme.hot)),
        border,
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
    let clock = format!(" · {}", elapsed(activity.elapsed));
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

/// `12s`, `1m 15s`, `2h 03m`.
fn elapsed(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60),
    }
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
            false => "  No backend is attached: `niobe profiles` shows what is defined.",
        })
        .style(Style::new().fg(theme.dim)),
    ]
}

/// One transcript entry, wrapped to the pane: a head line and its body.
fn entry_lines(entry: &Entry, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let colour = entry.kind.colour(theme);
    let body_width = width.saturating_sub(GUTTER);

    let mut head = vec![
        Span::styled(
            format!("{} ", entry.kind.glyph()),
            Style::new().fg(colour).bold(),
        ),
        Span::styled(entry.head.clone(), Style::new().fg(colour).bold()),
    ];
    if !entry.meta.is_empty() {
        let room = body_width.saturating_sub(text::width(&entry.head) + 2);
        head.push(Span::raw("  "));
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
        EntryKind::Agent => crate::markdown::render(entry.body.trim_end(), width, theme),
        EntryKind::User | EntryKind::Tool | EntryKind::Failure | EntryKind::Notice => {
            text::wrap(entry.body.trim_end(), width)
                .into_iter()
                .map(|wrapped| Line::from(wrapped).style(Style::new().fg(theme.fg)))
                .collect()
        }
    }
}

fn draw_cost(frame: &mut Frame, area: Rect, session: &SessionState, theme: &Theme) {
    let block = pane("Cost", theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let totals = session.totals();
    let tiles = [
        ("session", session_cost(session), theme.hot),
        ("tokens in", compact(totals.input), theme.fg),
        ("tokens out", compact(totals.output), theme.fg),
        ("cache read", compact(totals.cache_read), theme.fg),
    ];

    let [labels, values, rest] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);

    let columns = Layout::horizontal([Constraint::Ratio(1, 4); 4]).split(labels);
    let value_columns = Layout::horizontal([Constraint::Ratio(1, 4); 4]).split(values);
    for (i, (label, value, colour)) in tiles.iter().enumerate() {
        frame.render_widget(
            Paragraph::new(Line::from(*label).style(Style::new().fg(theme.dim))),
            columns[i],
        );
        frame.render_widget(
            Paragraph::new(Line::from(value.clone()).style(Style::new().fg(*colour).bold())),
            value_columns[i],
        );
    }

    // Dollars per category would need spend attributed to each read, edit or
    // shell command, which the ledger does not do yet. The bars carry the tool
    // mix instead, which is measured, and the footer says so.
    let tools = session.tools();
    let mut mix: Vec<(&String, &u64)> = tools.by_name.iter().collect();
    mix.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

    let rows = usize::from(rest.height);
    let mut lines: Vec<Line> = Vec::new();
    if mix.is_empty() {
        lines.push(Line::from("no tool calls yet").style(Style::new().fg(theme.dim)));
    } else {
        let busiest = mix.first().map(|(_, n)| **n).unwrap_or(1).max(1);
        let bar_width = usize::from(rest.width)
            .saturating_sub(BAR_NAME + BAR_COUNT + 2)
            .min(BAR_CELLS);
        for (name, count) in mix.iter().take(rows.saturating_sub(1)) {
            lines.push(bar_line(name, **count, busiest, bar_width, theme));
        }
    }

    lines.push(
        Line::from(format!(
            "{} calls · {} failed · {} denied · {} out",
            tools.finished,
            tools.failed,
            tools.denied,
            crate::app::human_bytes(tools.output_bytes)
        ))
        .style(Style::new().fg(theme.dim)),
    );

    lines.truncate(rows);
    frame.render_widget(Paragraph::new(lines), rest);
}

/// Columns a tool's name gets in the cost pane's mix.
const BAR_NAME: usize = 18;

/// Columns a tool's count gets, right-aligned.
const BAR_COUNT: usize = 4;

/// The longest a bar is drawn. The bars compare the tools with one another,
/// which a dozen cells does as well as the whole pane, and a bar across the
/// pane is a block of colour the count beside it gets lost in.
const BAR_CELLS: usize = 12;

/// One `Notion·search         2 ━━━━━━` row: the name, the count, and a thin
/// bar in the tool colour for how it compares with the busiest tool.
fn bar_line(name: &str, count: u64, busiest: u64, width: usize, theme: &Theme) -> Line<'static> {
    // At least one cell for a tool that ran, so the least used still shows.
    let filled = match busiest {
        0 => 0,
        _ => ((count as usize * width) / busiest as usize).max(1),
    };

    Line::from(vec![
        Span::styled(
            format!(
                "{:<BAR_NAME$}",
                text::truncate(&crate::app::tool_label(name), BAR_NAME - 1)
            ),
            Style::new().fg(theme.fg),
        ),
        Span::styled(
            format!("{count:>BAR_COUNT$} "),
            Style::new().fg(theme.hot).bold(),
        ),
        Span::styled("━".repeat(filled.min(width)), Style::new().fg(theme.tool)),
    ])
}

fn draw_parallel(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let session = app.session();
    let running = session.running_agents().len();
    let block = pane(format!("Parallel ─ {running} running"), theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let rows = usize::from(inner.height);
    let mut lines: Vec<Line> = Vec::new();

    if running == 0 {
        lines.push(Line::from("no sub-agents").style(Style::new().fg(theme.dim)));
    } else {
        for id in session.running_agents().iter().take(rows.saturating_sub(1)) {
            let label = app.agent_label(id).unwrap_or_else(|| id.to_string());
            lines.push(Line::from(vec![
                Span::styled("◆ ", Style::new().fg(theme.agent).bold()),
                Span::styled(
                    text::truncate(&label, usize::from(inner.width).saturating_sub(2)),
                    Style::new().fg(theme.fg),
                ),
            ]));
        }
    }

    lines.push(
        Line::from(format!(
            "{} spawned · peak {} · {} done · {} failed",
            session.agents_spawned(),
            session.peak_running_agents(),
            session.agents_completed(),
            session.agents_failed(),
        ))
        .style(Style::new().fg(theme.dim)),
    );

    lines.truncate(rows);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_changes(frame: &mut Frame, area: Rect, session: &SessionState, theme: &Theme) {
    let block = pane("Changes", theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let width = usize::from(inner.width);
    let mut lines: Vec<Line> = Vec::new();

    lines.push(
        Line::from(format!("Files ─ {}", session.files().len()))
            .style(Style::new().fg(theme.title).bold()),
    );
    if session.files().is_empty() {
        lines.push(Line::from("nothing changed yet").style(Style::new().fg(theme.dim)));
    } else {
        for file in session.files().iter().take(FILES_SHOWN) {
            // One column short of the pane, so the counts do not sit
            // against the border they are read beside.
            lines.push(file_line(file, width.saturating_sub(1), theme));
            if let Some(why) = &file.why {
                lines.push(
                    Line::from(format!(
                        "  “{}”",
                        text::truncate(why, width.saturating_sub(5))
                    ))
                    .style(Style::new().fg(theme.dim).italic()),
                );
            }
        }
        if let Some(rest) = session
            .files()
            .len()
            .checked_sub(FILES_SHOWN)
            .filter(|n| *n > 0)
        {
            lines.push(Line::from(format!("  and {rest} more")).style(Style::new().fg(theme.dim)));
        }
    }

    lines.push(Line::from(""));
    lines.push(
        Line::from(format!("Decisions ─ {}", session.decisions().len()))
            .style(Style::new().fg(theme.title).bold()),
    );
    if session.decisions().is_empty() {
        lines.push(Line::from("none recorded").style(Style::new().fg(theme.dim)));
    } else {
        for decision in session.decisions().iter().rev().take(4) {
            for (i, wrapped) in text::wrap(&decision.summary, width.saturating_sub(2))
                .into_iter()
                .enumerate()
            {
                let prefix = if i == 0 { "· " } else { "  " };
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::new().fg(theme.dim)),
                    Span::styled(wrapped, Style::new().fg(theme.fg)),
                ]));
            }
        }
    }

    let tools = session.tools();
    if !tools.by_name.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from("Tools").style(Style::new().fg(theme.title).bold()));
        let mix = tools
            .by_name
            .iter()
            .map(|(name, count)| format!("{} {count}", crate::app::tool_label(name)))
            .collect::<Vec<_>>()
            .join(" · ");
        for wrapped in text::wrap(&mix, width) {
            lines.push(Line::from(wrapped).style(Style::new().fg(theme.fg)));
        }
    }

    lines.truncate(usize::from(inner.height));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// One file's row: what changed, and by how much, with the path given whatever
/// the counts leave.
fn file_line(file: &FileChanges, width: usize, theme: &Theme) -> Line<'static> {
    let added = count('+', file.added, file.added_stated());
    let removed = count('−', file.removed, file.removed_stated());

    // The counts are what the row is for, so they keep their columns and the
    // path gives way: a path cut at the front still names the file, while a
    // count cut anywhere is a different number. One column of gap is kept
    // whatever the width, so the two never run together into a third figure.
    let counts = text::width(&added) + 1 + text::width(&removed);
    let room = width.saturating_sub(text::width(MARKER) + counts + 1);
    let path = text::truncate_start(&file.path, room);
    let gap = width
        .saturating_sub(text::width(MARKER) + counts)
        .saturating_sub(text::width(&path));

    Line::from(vec![
        Span::styled(MARKER, Style::new().fg(theme.dim)),
        Span::styled(path, Style::new().fg(theme.fg)),
        Span::raw(" ".repeat(gap)),
        Span::styled(added, Style::new().fg(theme.add)),
        Span::raw(" "),
        Span::styled(removed, Style::new().fg(theme.del)),
    ])
}

/// One side of a file's counts: `+38`, `+≥38` where a call that changed the
/// file did not say how much it added, or an em dash where none of them did.
///
/// The three readings are the cost pane's: a bare figure is the whole of it,
/// `≥` means at least this much, and an em dash means nothing was reported. A
/// zero here would say the session left that side of the file alone, which is
/// a different claim from not knowing.
fn count(sign: char, lines: u64, stated: bool) -> String {
    match (stated, lines) {
        (true, lines) => format!("{sign}{lines}"),
        (false, 0) => "—".to_owned(),
        (false, lines) => format!("{sign}≥{lines}"),
    }
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let session = app.session();
    let bar = Style::new().bg(theme.status_bg).fg(theme.status_fg);
    let separator = Span::styled(" │ ", Style::new().fg(theme.status_fg).dim());

    let mut top = vec![Span::styled(
        app.repo().name.clone(),
        Style::new().fg(theme.hot).bold(),
    )];
    top.push(separator.clone());
    top.push(Span::styled(
        backend_label(session, app.profile(), app.is_attached()),
        Style::new().fg(theme.agent),
    ));
    if let Some(branch) = &app.repo().branch {
        top.push(separator.clone());
        top.push(Span::styled(
            format!("⎇ {branch}"),
            Style::new().fg(theme.tool),
        ));
    }
    top.push(separator.clone());
    top.push(Span::styled(
        format!("{} tokens", compact(session.totals().tokens())),
        Style::new().fg(theme.fg),
    ));
    if !session.pending_permissions().is_empty() {
        top.push(separator.clone());
        top.push(Span::styled(
            format!("{} waiting on you", session.pending_permissions().len()),
            Style::new().fg(theme.hot).bold(),
        ));
    }
    // On a flat-rate plan the windows are the budget, so they sit where the
    // budget does. Absent, not zeroed, until a backend reports one: a metered
    // profile has no windows and a `0%/5h` would be a figure nobody measured.
    if let Some(windows) = session.usage_windows() {
        if let Some(label) = crate::app::windows_label(windows) {
            top.push(separator.clone());
            top.push(Span::styled(label, window_style(windows, theme)));
        }
        if windows.using_overage {
            top.push(separator.clone());
            top.push(Span::styled(
                "overage".to_owned(),
                Style::new().fg(theme.hot).bold(),
            ));
        }
    }
    if let Some(budget) = app.budget() {
        let spent = session.totals().reported_cost_usd;
        top.push(separator.clone());
        top.push(Span::styled(
            format!("budget ${spent:.2}/${budget:.2}"),
            match spent >= budget * BUDGET_SHOWN_HOT {
                true => Style::new().fg(theme.hot).bold(),
                false => Style::new().fg(theme.fg),
            },
        ));
    }
    if session.errors() > 0 {
        top.push(separator);
        top.push(Span::styled(
            format!("{} errors", session.errors()),
            Style::new().fg(theme.del),
        ));
    }

    let bottom = match (app.hint(), session.mode()) {
        (Some(hint), _) => Line::from(hint.to_owned()).style(Style::new().fg(theme.hot)),
        (None, Some(mode)) => Line::from(vec![
            Span::styled(format!("▸▸ {mode} mode"), Style::new().fg(theme.hot).bold()),
            Span::styled(
                " · Shift+Tab cycles · F8 model · Enter send · F10 quit".to_owned(),
                Style::new().fg(theme.status_fg),
            ),
        ]),
        // Nothing has said how this session gates tool calls, so nothing
        // claims to know: the keys are what is left to say.
        (None, None) => Line::from(
            "Enter send · Alt+Enter newline · PgUp/PgDn scrollback · F10 quit".to_owned(),
        )
        .style(Style::new().fg(theme.status_fg)),
    };

    frame.render_widget(
        Paragraph::new(vec![Line::from(top), bottom]).style(bar),
        area,
    );
}

/// How the windows segment is coloured: the same threshold and the same colour
/// as a budget nearly spent, because on a flat-rate plan that is what a window
/// nearly gone is.
fn window_style(windows: &UsageWindows, theme: &Theme) -> Style {
    let hot = [windows.five_hour, windows.seven_day]
        .into_iter()
        .flatten()
        .any(|window| window.utilization >= BUDGET_SHOWN_HOT);
    match hot {
        true => Style::new().fg(theme.hot).bold(),
        false => Style::new().fg(theme.fg),
    }
}

fn draw_fkeys(frame: &mut Frame, area: Rect, theme: &Theme) {
    let slot = usize::from(area.width) / FKEYS.len();
    let mut spans = Vec::new();

    for (number, label) in FKEYS {
        let room = slot.saturating_sub(number.len() + 1);
        spans.push(Span::styled(
            number,
            Style::new().fg(theme.hot).bg(theme.menu_bg).bold(),
        ));
        spans.push(Span::styled(
            format!("{:<room$}", text::truncate(label, room), room = room),
            Style::new().fg(theme.fkey_fg).bg(theme.fkey_bg),
        ));
        spans.push(Span::styled(" ", Style::new().bg(theme.menu_bg)));
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::new().bg(theme.menu_bg)),
        area,
    );
}

/// What is running, or what is not running yet.
///
/// A backend's own report of what it runs wins over the profile that was
/// selected to start it: the profile asks for a backend and the backend says
/// which model it ended up on. Between the two — a subprocess started and not
/// yet heard from — the line says it is starting rather than that nothing is
/// attached, which would be read as a session that is not going to answer.
fn backend_label(
    session: &SessionState,
    profile: Option<&SelectedProfile>,
    attached: bool,
) -> String {
    match (session.meta(), profile, attached) {
        // The model comes off the fold rather than off the meta: a model the
        // operator has just chosen is what the session is on from its next
        // turn, and naming the one it is moving off would read as a switch
        // that did not land.
        (Some(meta), _, _) => format!(
            "⚡ {} · {}",
            meta.backend,
            session.model().unwrap_or(&meta.model)
        ),
        (None, Some(profile), true) => format!("{} · {}, starting", profile.name, profile.backend),
        (None, Some(profile), false) => {
            format!("{} · {}, not attached", profile.name, profile.backend)
        }
        (None, None, _) => "no backend attached".to_owned(),
    }
}

/// The session's cost, labelled for what it is.
///
/// A backend that reported no cost for some of its usage records leaves the sum
/// a floor, and the figure says `≥` rather than pretending to be the bill.
/// Public so that anything else printing a session's cost prints the same
/// label the cost pane does.
pub fn session_cost(session: &SessionState) -> String {
    let totals = session.totals();
    if totals.records == 0 {
        return "—".to_owned();
    }
    if totals.cost_fully_reported() {
        return format!("${:.2}", totals.reported_cost_usd);
    }
    if totals.reported_cost_usd == 0.0 {
        return "unpriced".to_owned();
    }
    format!("≥${:.2}", totals.reported_cost_usd)
}

/// Token counts, short enough for a tile.
fn compact(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{:.0}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            cost_basis: None,
        })
    }

    #[test]
    fn a_session_with_no_usage_shows_a_dash_not_a_zero() {
        let session = SessionState::new();
        assert_eq!(session_cost(&session), "—");
    }

    #[test]
    fn a_partly_reported_cost_reads_as_a_floor() {
        let fully = SessionState::replay(&[priced(Some(0.25)), priced(Some(0.50))]);
        assert_eq!(session_cost(&fully), "$0.75");

        let partly = SessionState::replay(&[priced(Some(0.25)), priced(None)]);
        assert_eq!(session_cost(&partly), "≥$0.25");

        let none = SessionState::replay(&[priced(None)]);
        assert_eq!(session_cost(&none), "unpriced");
    }

    #[test]
    fn an_unstarted_session_says_no_backend_is_attached() {
        assert_eq!(
            backend_label(&SessionState::new(), None, false),
            "no backend attached"
        );

        let running = SessionState::replay(&[niobe_core::event::Event::SessionMeta(SessionMeta {
            backend: Backend::Codex,
            profile: "default".to_owned(),
            model: "gpt-5-codex".to_owned(),
            backend_session: None,
        })]);
        assert_eq!(
            backend_label(&running, None, false),
            "⚡ codex · gpt-5-codex"
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
            backend_label(&SessionState::new(), Some(&work), false),
            "work · claude, not attached"
        );

        let running = SessionState::replay(&[niobe_core::event::Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "work".to_owned(),
            model: "opus-5".to_owned(),
            backend_session: None,
        })]);
        assert_eq!(
            backend_label(&running, Some(&work), false),
            "⚡ claude · opus-5"
        );
    }

    #[test]
    fn a_backend_that_has_started_and_not_yet_spoken_is_starting_not_absent() {
        let max = SelectedProfile {
            name: "max".to_owned(),
            backend: Backend::Claude,
            models: Vec::new(),
        };

        assert_eq!(
            backend_label(&SessionState::new(), Some(&max), true),
            "max · claude, starting"
        );
        assert_eq!(
            backend_label(&SessionState::new(), Some(&max), false),
            "max · claude, not attached"
        );
    }

    fn changed(path: &str, added: Option<u64>, removed: Option<u64>) -> niobe_core::Event {
        niobe_core::Event::FileChange {
            path: path.to_owned(),
            added,
            removed,
        }
    }

    /// What one row reads as, counts and all.
    fn row(file: &FileChanges, width: usize) -> String {
        file_line(file, width, &Theme::default())
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
    fn a_file_row_reads_as_the_counts_the_session_can_defend() {
        let mut state = SessionState::new();
        state.apply(&changed("catalog/fetch.ts", Some(38), Some(9)));
        state.apply(&changed("notes.md", Some(1), None));
        state.apply(&changed("run.ipynb", None, None));

        assert_eq!(
            row(&state.files()[0], 40),
            "▸ catalog/fetch.ts                +38 −9"
        );
        assert_eq!(
            row(&state.files()[1], 40),
            "▸ notes.md                          +1 —"
        );
        assert_eq!(
            row(&state.files()[2], 40),
            "▸ run.ipynb                          — —"
        );
    }

    /// The counts are what the row is for. A path cut at the front still names
    /// the file; a count cut anywhere is a different number.
    #[test]
    fn a_path_too_long_for_the_pane_gives_way_to_its_counts() {
        let mut state = SessionState::new();
        state.apply(&changed(
            "crates/niobe-bridge-claude/src/translate.rs",
            Some(120),
            Some(44),
        ));

        let row = row(&state.files()[0], 32);
        assert!(row.ends_with(" +120 −44"), "{row:?}");
        assert!(row.contains("translate.rs"), "{row:?}");
        assert_eq!(text::width(&row), 32, "{row:?}");
    }

    #[test]
    fn token_counts_stay_short() {
        assert_eq!(compact(0), "0");
        assert_eq!(compact(9_999), "9999");
        assert_eq!(compact(25_500), "26k");
        assert_eq!(compact(1_260_000), "1.3M");
    }

    fn windows(five_hour: f64, seven_day: f64) -> UsageWindows {
        UsageWindows {
            five_hour: Some(UsageWindow {
                utilization: five_hour,
                resets_at: None,
            }),
            seven_day: Some(UsageWindow {
                utilization: seven_day,
                resets_at: None,
            }),
            using_overage: false,
        }
    }

    /// Either window nearly gone is the plan nearly gone: on a flat-rate plan
    /// the seven-day window running out stops the session just as surely as
    /// the five-hour one, so neither may be marked at the other's expense.
    #[test]
    fn a_window_nearly_gone_is_marked_the_way_a_budget_nearly_spent_is() {
        let theme = crate::theme::CLASSIC;
        let hot = Style::new().fg(theme.hot).bold();
        let plain = Style::new().fg(theme.fg);

        assert_eq!(window_style(&windows(0.62, 0.18), &theme), plain);
        assert_eq!(window_style(&windows(0.80, 0.18), &theme), hot);
        assert_eq!(window_style(&windows(0.10, 0.95), &theme), hot);
        assert_eq!(
            window_style(&UsageWindows::default(), &theme),
            plain,
            "a plan with no window reported was marked as one nearly gone"
        );
    }
}
