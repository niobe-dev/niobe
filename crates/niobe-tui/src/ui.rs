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
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Widget,
};

use niobe_core::event::{AgentOutcome, Billing, Context, Mode, UsageWindow};
use niobe_core::session::{FileChanges, SessionState, ToolTotals, Totals};

use crate::app::{
    Activity, Answer, App, Ask, AskFocus, Entry, EntryKind, Focus, Pane, Picker, Section,
    SelectedProfile, SubAgent, tool_label,
};
use crate::clock::{self, Stamp};
use crate::fx;
use crate::meter::meter;
use crate::prices::Prices;
use crate::text;
use crate::theme::{Motion, Theme};
use crate::tree;
use crate::usage;

/// Smallest terminal the shell draws in, as (columns, rows).
pub const MIN_SIZE: (u16, u16) = (80, 24);

/// The width at which the right stack fits beside the session pane. Below it
/// the session pane takes the whole body: three more panes squeezed into forty
/// columns each is less readable than the transcript they were taken from.
pub const WIDE_COLUMNS: u16 = 100;

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
const DIALOG_MARGIN: u16 = 4;

/// Widest the model list is drawn, in columns. A model id is a word or two, so
/// the list is narrow enough to read as a list rather than as a pane.
const PICK_COLUMNS: u16 = 44;

/// What marks the model the session is on, and the one the cursor is over.
const PICK_CURSOR: &str = "› ";
const PICK_CURRENT: &str = "· ";

/// The share of a budget at which the Usage pane starts saying so in the
/// colour it uses for anything waiting on the operator. The same fraction the
/// transcript warning uses, so the pane and the warning agree.
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
    ("5", "Usage"),
    ("6", "Files"),
    ("7", "Tools"),
    ("8", "Model"),
    ("9", "Theme"),
    ("10", "Quit"),
];

/// The menu bar's items. The first letter is the hot key.
const MENUS: [&str; 7] = [
    "Niobe", "Session", "Files", "Tools", "Usage", "Options", "Help",
];

/// Columns between the menus and the session's identity, and between one
/// segment of that identity and the next.
const MENU_GAP: usize = 2;

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

    let [menu, body, fkeys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_menu(frame, menu, app, &theme);
    draw_body(frame, body, app, &theme);
    draw_fkeys(frame, fkeys, &theme);

    // Last, and over the body: the list is something the operator opened, and
    // nothing drawn afterwards may cover it. A permission prompt is not drawn
    // here — it is in the transcript, under the work that led to it.
    if let Some(picker) = app.picking() {
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
    let width = PICK_COLUMNS.min(body.width.saturating_sub(DIALOG_MARGIN * 2));
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
            true => {
                Line::from(row).style(Style::new().bg(theme.cursor_bg).fg(theme.cursor_fg).bold())
            }
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

/// The menu row: the seven menus on the left, and on the right what the
/// session is, what it is doing and the time of day.
///
/// The theme is not named here. `9 Theme` in the F-key row is where a palette
/// is changed, and a row that says which one is on says nothing the screen
/// does not already show.
fn draw_menu(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let bar = Style::new().bg(theme.menu_bg).fg(theme.menu_fg);
    let left = Line::from(menu_spans(theme));

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

/// The seven menus, each with its hot key accented and underlined.
///
/// A terminal with no underline drops the underline and keeps the colour, so
/// the hot key is still marked on one that has only the sixteen attributes.
fn menu_spans(theme: &Theme) -> Vec<Span<'static>> {
    let hot = Style::new().fg(theme.hot).bold().underlined();
    let plain = Style::new().fg(theme.menu_fg);

    let mut spans = vec![Span::raw(" ")];
    for name in MENUS {
        let mut chars = name.chars();
        let first = chars.next().unwrap_or(' ');
        spans.push(Span::styled(first.to_string(), hot));
        spans.push(Span::styled(chars.as_str().to_owned(), plain));
        spans.push(Span::raw("  "));
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
    // A turn that is running settles it: whatever the backend has or has not
    // said about itself, the session is working and nothing else is truer.
    if pulse.working {
        return pulse_label(pulse);
    }
    if !app.is_attached() {
        return "\u{25cb} not attached".to_owned();
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
    if area.width < WIDE_COLUMNS {
        app.right_stack_hidden();
        draw_session(frame, area, false, app, theme);
        return;
    }

    // Session pane to right stack, 1.9 : 1, with a column of desktop between
    // them. The transcript is what a session is read in; the panes beside it
    // are figures, and a figure needs a fraction of the width a paragraph
    // does.
    let [left, right] = Layout::horizontal([Constraint::Fill(19), Constraint::Fill(10)])
        .spacing(1)
        .areas(area);

    draw_session(frame, left, true, app, theme);

    // Usage takes the rows its figures need and no more; what is left goes to
    // the two panes that grow with the session, 1.3 : 1 in favour of the files
    // it changed.
    let [usage, changes, activity] = Layout::vertical([
        Constraint::Length(usage_height(app).min(right.height)),
        Constraint::Fill(13),
        Constraint::Fill(10),
    ])
    .areas(right);

    draw_usage(frame, usage, app, theme);
    draw_changes(frame, changes, app, theme);
    draw_activity(frame, activity, app, theme);
    draw_desktop(frame, area, &[left, usage, changes, activity], app, theme);
}

/// The desktop the panes leave uncovered in `body`, animated while a turn is
/// running.
///
/// The motion is a picture of the whole screen and the panes are opaque, so
/// it is worked out only for the cells no pane owns: a pane's cell is never
/// written here, whatever the motion is. It is the one part of the screen
/// that says the session is alive without the operator reading anything, and
/// it is drawn from the same clock as the spinner under the transcript, so
/// the two cannot disagree about whether a turn is going. A theme that does
/// not animate, effects turned off, and a session with no turn running leave
/// the desktop empty and work nothing out.
fn draw_desktop(frame: &mut Frame, body: Rect, panes: &[Rect], app: &App, theme: &Theme) {
    if !app.effects() || theme.motion == Motion::Still {
        return;
    }
    let Some(activity) = app.activity() else {
        return;
    };
    let screen = frame.area();
    let field = fx::Field::new(
        theme,
        screen.width,
        screen.height,
        fx::frame_at(theme.motion, activity.elapsed),
    );
    let buffer = frame.buffer_mut();
    for at in body.positions() {
        if panes.iter().any(|pane| pane.contains(at)) {
            continue;
        }
        let glyph = field.at(at.x.saturating_sub(screen.x), at.y.saturating_sub(screen.y));
        if let (Some(glyph), Some(cell)) = (glyph, buffer.cell_mut(at)) {
            cell.set_char(glyph.symbol).set_fg(glyph.colour);
        }
    }
}

/// Rows of content a pane keeps before it will spare a blank row under its
/// title.
const PANE_ROOM: u16 = 2;

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
/// centred on the top edge, and room between the border and what is written
/// inside it.
///
/// A column either side always, and a blank row under the title where the
/// pane is tall enough to spare one. The blank row is the first thing a short
/// pane gives up: at the smallest terminal the shell draws in, a row of the
/// changed files is worth more than the room above them.
fn pane(title: impl Into<String>, area: Rect, border: Border, theme: &Theme) -> Block<'static> {
    // Two rows of border, the blank row itself, and PANE_ROOM left over.
    let top = u16::from(area.height > 2 + PANE_ROOM);
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
        .padding(Padding::new(1, 1, top, 0))
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
    let title = session_caption(app.session(), &app.repo().name, area.width);

    app.measured_session(area);
    app.drew_jump(None);
    let border = Border::of(app.focus() == Focus::Session, theme);
    let block = pane(title, area, border, theme);
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
    let typed = app.composed().lines().count().max(said.len()).max(1);
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

/// The files an `@` word could name, as a list standing on the divider above
/// the bar, over the foot of the transcript, lined up with what is typed.
fn draw_mention(frame: &mut Frame, transcript: Rect, bar: Rect, app: &App, theme: &Theme) {
    let (files, selected) = app.mention_files();
    if files.is_empty() {
        return;
    }
    let lead = u16::try_from(lead_width(app)).unwrap_or(u16::MAX);
    let x = bar.x.saturating_add(lead).min(transcript.right());
    let room = transcript.right().saturating_sub(x);
    // A border either side, and a column of padding inside each.
    let widest = files
        .iter()
        .map(|file| text::width(file))
        .max()
        .unwrap_or(0);
    let width = u16::try_from(widest + 4).unwrap_or(u16::MAX).min(room);
    let rows = files
        .len()
        .min(usize::from(transcript.height.saturating_sub(2)));
    if rows == 0 || width < 5 {
        return;
    }
    let height = u16::try_from(rows + 2).unwrap_or(u16::MAX);
    let area = Rect::new(x, transcript.bottom().saturating_sub(height), width, height);

    let inside = usize::from(width.saturating_sub(4));
    let lines: Vec<Line<'static>> = files
        .iter()
        .take(rows)
        .enumerate()
        .map(|(at, file)| {
            // The name is the end of the path, so a path too long for the list
            // keeps that and gives up its leading directories.
            let shown = format!(" {:<inside$} ", text::truncate_start(file, inside));
            if at == selected {
                Line::from(shown).style(Style::new().fg(theme.pane_bg).bg(theme.hot).bold())
            } else {
                Line::from(shown).style(Style::new().fg(theme.fg))
            }
        })
        .collect();
    let block = Block::bordered()
        .border_type(theme.border)
        .border_style(Style::new().fg(theme.frame).bg(theme.pane_bg))
        .style(Style::new().bg(theme.pane_bg))
        .title_top(Line::from(" @ file ").style(Style::new().fg(theme.title).bold()));
    frame.render_widget(Clear, area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Gives the placeholder the room the bar leaves once its first hint — the
/// mode, where one is reported — has what it needs.
fn fit_placeholder(app: &mut App, panes: bool, width: u16, theme: &Theme) {
    let first = key_hints(app.session().mode(), app.focus(), panes, theme)
        .first()
        .map_or(0, |hint| text::width(&hint.text));
    // The cursor, and the column between the editor and the hints.
    let columns = usize::from(width)
        .saturating_sub(lead_width(app))
        .saturating_sub(first + 2);
    app.fit_placeholder(columns);
}

/// Looks for what a search is looking for in the transcript as it will be
/// drawn `width` wide, before anything is drawn: the bar says how many places
/// it was found, and the bar is drawn before the transcript.
fn find_in_transcript(app: &mut App, width: u16, theme: &Theme) {
    let Some(query) = app.find_query() else {
        return;
    };
    let folded = app.calls_folded();
    let (entries, drawn) = app.entries_to_draw();
    drawn.update(entries, usize::from(width), folded, theme);
    let found = crate::find::Query::new(&query)
        .map(|query| drawn.find(&query))
        .unwrap_or_default();
    app.found(found);
}

/// The border cells a pane's title leaves on its top edge: a corner and a
/// cell of the edge either side, so the pane still reads as framed, and the
/// space either side of the title itself.
const TITLE_MARGIN: u16 = 6;

/// The session pane's title on a pane `width` wide: what the session is about
/// where it says ([`SessionState::caption`]), and the repository it runs in
/// where it does not yet.
///
/// Cut between words where it is longer than the edge has room for, so that
/// the corners and a cell of the edge either side survive at every width.
fn session_caption(session: &SessionState, repo: &str, width: u16) -> String {
    let title = match session.caption() {
        Some(caption) => caption,
        None if repo.is_empty() => "Session".to_owned(),
        None => format!("Session ─ {repo}"),
    };
    text::truncate_words(&title, usize::from(width.saturating_sub(TITLE_MARGIN)))
}

/// The badge in front of the composer: what the bar is for, as a chip.
const ASK_BADGE: &str = " ask ";

/// The badge the bar wears while it is searching the transcript.
const FIND_BADGE: &str = " find ";

/// The badge the bar wears while it holds a command for the operator's shell.
const SHELL_BADGE: &str = " shell ";

/// What the bar puts between its hints.
const HINT_SEPARATOR: &str = " · ";

/// The badge and the marker in front of what the bar is editing: the prompt,
/// or a search through the transcript.
fn bar_lead(app: &App) -> (&'static str, &'static str) {
    match (app.finding(), app.shell_mode()) {
        (Some(_), _) => (FIND_BADGE, " / "),
        (None, true) => (SHELL_BADGE, " $ "),
        (None, false) => (ASK_BADGE, " > "),
    }
}

/// The columns of the badge and the marker.
fn lead_width(app: &App) -> usize {
    let (badge, marker) = bar_lead(app);
    text::width(badge) + text::width(marker)
}

/// What the bar is editing: the search while one is open, which the composer
/// behind it waits under, and the composer otherwise.
fn editing(app: &App) -> &ratatui_textarea::TextArea<'static> {
    app.finding().unwrap_or_else(|| app.composer())
}

/// The composer, with its badge in front and, at its right-hand end, what the
/// shell has to say: its own reply to the last key when it has one, otherwise
/// the mode the session is in and the keys that change what the bar does.
fn draw_ask_bar(
    frame: &mut Frame,
    area: Rect,
    said: Vec<Line<'static>>,
    app: &mut App,
    theme: &Theme,
) {
    let (badge_text, marker_text) = bar_lead(app);
    let [badge, marker, rest] = Layout::horizontal([
        Constraint::Length(u16::try_from(text::width(badge_text)).unwrap_or(u16::MAX)),
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
        Some((found, current)) => find_hints(app, found.len(), current, theme),
        None if app.shell_mode() => ["Enter runs", "Esc back"]
            .into_iter()
            .map(|key| Segment {
                text: key.to_owned(),
                style: Style::new().fg(theme.dim),
            })
            .collect(),
        None => {
            // A question holding the keyboard takes Tab for writing its
            // answer, so while it does, Tab is not offered as the way between
            // the panes.
            let question_holds = app.asking().is_some() && app.ask_focus() != AskFocus::Deferred;
            key_hints(
                app.session().mode(),
                app.focus(),
                panes && !question_holds,
                theme,
            )
        }
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

/// The mode the session gates tool calls in, and the keys the bar answers
/// to, most important first.
///
/// With a right-hand pane focused, the keys that differ are that pane's: the
/// arrows and Enter are not the composer's while it has them. `panes` is
/// whether there is a pane beside the session for Tab to move to.
fn key_hints(mode: Option<Mode>, focus: Focus, panes: bool, theme: &Theme) -> Vec<Segment> {
    let key = |text: &str| Segment {
        text: text.to_owned(),
        style: Style::new().fg(theme.dim),
    };
    let mut hints = match mode {
        Some(mode) => vec![
            Segment {
                text: format!("\u{25b8}\u{25b8} {mode} mode"),
                style: Style::new().fg(theme.hot).bold(),
            },
            key("Shift+Tab cycles"),
        ],
        // Nothing has said how this session gates tool calls, so nothing
        // claims to know: the key that sets it is what is left to say.
        None => vec![key("Shift+Tab mode")],
    };
    match focus {
        Focus::Session => {
            hints.push(key("Alt+Enter newline"));
            if panes {
                hints.push(key("Tab panes"));
            }
        }
        Focus::Pane(_) => {
            hints.push(key("↑↓ Enter folds"));
            hints.push(key("Esc back"));
        }
    }
    hints
}

/// The hints that fit in `room` columns, separators included, whole ones only.
fn fitted_hints(mut hints: Vec<Segment>, room: usize) -> Vec<Segment> {
    let width = |hints: &[Segment]| {
        let text: usize = hints.iter().map(|h| text::width(&h.text)).sum();
        text + hints.len().saturating_sub(1) * text::width(HINT_SEPARATOR)
    };
    while !hints.is_empty() && width(&hints) > room {
        hints.pop();
    }
    hints
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
        .map(|ask| question_lines(app, ask, width, theme))
        .unwrap_or_default();

    if app.entries().is_empty() && question.is_empty() {
        let lines = empty_transcript(app.is_attached(), theme);
        app.measured(lines.len(), height);
        frame.render_widget(
            Paragraph::new(lines).style(Style::new().bg(theme.pane_bg)),
            area,
        );
        return;
    }

    let folded = app.calls_folded();
    let (entries, drawn) = app.entries_to_draw();
    drawn.update(entries, width, folded, theme);
    let above = drawn.line_count();
    let total = above + question.len();

    app.measured(total, height);
    app.reveal_found();
    let start = app.scroll().min(total);
    let (_, drawn) = app.entries_to_draw();
    let mut visible = drawn.lines(start, height);
    if let Some((found, current)) = app.find_marks() {
        mark_found(&mut visible, start, found, current, theme);
    }
    let room = height.saturating_sub(visible.len());
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
    draw_scrollbar(frame, area, border, (start, total, height), theme);
    if !app.follows_tail() {
        let at = draw_jump(frame, area, app.asking().is_some(), theme);
        app.drew_jump(at);
    }
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
        let first = found.partition_point(|found| found.line < at);
        let marks: Vec<(usize, usize, Style)> = found
            .iter()
            .enumerate()
            .skip(first)
            .take_while(|(_, found)| found.line == at)
            .map(|(index, found)| {
                let style = if Some(index) == current { here } else { other };
                (found.start, found.len, style)
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
/// theme changes, or runs of calls are folded or opened, which is everything
/// its lines depend on.
#[derive(Debug, Default)]
pub struct DrawnEntries {
    drawn: Vec<(u64, Vec<Line<'static>>)>,
}

impl DrawnEntries {
    /// Brings every entry's lines up to date.
    fn update(&mut self, entries: &[Entry], width: usize, folded: bool, theme: &Theme) {
        self.drawn.truncate(entries.len());
        for (at, entry) in entries.iter().enumerate() {
            let key = drawn_from(entry, width, folded, theme);
            match self.drawn.get_mut(at) {
                Some((drawn_key, _)) if *drawn_key == key => {}
                Some(slot) => *slot = (key, entry_lines(entry, width, folded, theme)),
                None => self
                    .drawn
                    .push((key, entry_lines(entry, width, folded, theme))),
            }
        }
    }

    /// Every place `query` is in the transcript as drawn, top to bottom.
    fn find(&self, query: &crate::find::Query) -> Vec<crate::find::Found> {
        self.drawn
            .iter()
            .flat_map(|(_, lines)| lines)
            .enumerate()
            .flat_map(|(at, line)| query.in_line(line, at))
            .collect()
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
fn drawn_from(entry: &Entry, width: usize, folded: bool, theme: &Theme) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    entry.hash(&mut hasher);
    width.hash(&mut hasher);
    folded.hash(&mut hasher);
    theme.hash(&mut hasher);
    hasher.finish()
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
/// something the operator did not read.
fn question_lines(app: &App, ask: &Ask, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let outer = width.saturating_sub(GUTTER).min(ASK_COLUMNS);
    let inner = outer.saturating_sub(ASK_INSET);
    let border = Style::new().fg(theme.hot);
    let indent = " ".repeat(GUTTER);

    let mut lines = vec![question_top(app, ask, outer, theme)];
    lines.extend(
        question_body(app, ask, inner, theme)
            .into_iter()
            .map(|line| {
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
            }),
    );
    lines.push(Line::from(vec![
        Span::raw(indent),
        Span::styled(format!("└{}┘", "─".repeat(outer.saturating_sub(2))), border),
    ]));
    lines.push(Line::from(""));
    lines
}

/// What is inside a question's frame, `inner` columns wide: what would run,
/// the numbered answers, any consequence too long for its column said whole,
/// the answer being written, and the keys.
fn question_body(app: &App, ask: &Ask, inner: usize, theme: &Theme) -> Vec<Line<'static>> {
    let plain = Style::new().fg(theme.fg);

    let mut body: Vec<Line<'static>> = vec![Line::from(vec![
        Span::styled("to run ", plain),
        Span::styled(tool_label(&ask.tool), plain.bold()),
    ])];
    for wrapped in text::wrap(ask.target.as_deref().unwrap_or(&ask.input), inner) {
        body.push(Line::from(wrapped).style(plain.bold()));
    }
    if ask.target.is_some() {
        body.push(Line::from(""));
        for wrapped in text::wrap(&ask.input, inner) {
            body.push(Line::from(wrapped).style(Style::new().fg(theme.dim)));
        }
    }
    body.push(Line::from(""));
    let focus = app.ask_focus();
    let selected = app.ask_selected();
    let mut cut = Vec::new();
    for (at, answer) in app.ask_options().into_iter().enumerate() {
        let lit = focus == AskFocus::Choosing && answer == selected;
        let (row, whole) = option_row(at + 1, answer, ask, lit, inner, theme);
        body.push(row);
        if !whole {
            cut.push((at + 1, option_words(answer, ask).1));
        }
    }
    // A consequence cut off at the edge of its column is said again whole: a
    // standing answer is a rule the operator keeps, and keeping one nobody
    // could read to its end is agreeing to something unread.
    if !cut.is_empty() {
        body.push(Line::from(""));
        for (number, hint) in cut {
            for wrapped in text::wrap(&format!("{number}. {hint}"), inner) {
                body.push(Line::from(wrapped).style(Style::new().fg(theme.dim)));
            }
        }
    }
    if focus == AskFocus::Writing {
        body.push(Line::from(""));
        body.extend(written_answer(app.ask_draft(), inner, theme));
    }
    body.push(Line::from(""));
    body.extend(key_rows(
        &ask_keys(focus, app.ask_options().len()),
        inner,
        theme,
    ));
    body
}

/// `┌ ? claude asks ──── blocks turn 3 ┐`: who is asking at the left, in the
/// title colour, and what is waiting on the answer at the right, dimmed.
fn question_top(app: &App, ask: &Ask, outer: usize, theme: &Theme) -> Line<'static> {
    let border = Style::new().fg(theme.hot);
    let asks = format!(" ? {} asks ", app.agent_name());
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
    let whole = text::width(&hint) <= hint_room;
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
            format!("niobe saves {}", ask.tool_rule()),
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
fn entry_lines(entry: &Entry, width: usize, folded: bool, theme: &Theme) -> Vec<Line<'static>> {
    if !entry.calls.is_empty() {
        return crate::calls::lines(entry, width, folded, theme);
    }
    if let EntryKind::Turn(rule) = &entry.kind {
        return crate::turns::lines(rule, width, theme);
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
    // Two rows of border and the blank row under the title; the windows, the
    // models and the money in the order the shape puts them, with a rule
    // between the windows and the models on a plan wherever both have rows,
    // and always under the money on a metered account; then the context,
    // under a rule of its own, where there is one to show.
    let blocks = match metered(app) {
        true => money + windows + 1 + spend,
        false => windows + usize::from(windows > 0 && spend > 0) + spend + money,
    };
    let rows = 3 + blocks + usize::from(context > 0) + context;
    u16::try_from(rows).unwrap_or(u16::MAX)
}

/// Whether the session is billed by use, which makes money the pane's
/// headline.
///
/// Only where something said so. A session nothing has said of keeps the
/// plan's order, windows first where there are any, and shows no dollar
/// figure at all: see [`money_lines`].
fn metered(app: &App) -> bool {
    app.session().billing() == Some(Billing::Metered)
}

/// The widest label a window row carries — `extra` — and a column of gap.
const WINDOW_LABEL: usize = 6;

/// What a window's share is drawn in: a space, and three columns for the
/// figure, which is one more than a full window needs so that a window
/// reported past its end still lines up with the ones that are not.
const WINDOW_SHARE: usize = 5;

/// The longest a meter is drawn. Twelve cells read a share to within a tenth,
/// which is as fine as a window is worth reading, and it leaves the reset time
/// beside it room in a pane forty columns wide.
const METER_CELLS: usize = 12;

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

/// When a window comes back, as the row says it: ` · resets 16:40` later today
/// and ` · resets Tue 09:00` on another day.
///
/// `None` where there is no reset to name — the backend reported the share
/// without one, the machine named no timezone, or the reset has already come
/// around, which is what a recorded session read back a day later has. The row
/// then carries the share alone rather than a time that is no longer true.
fn reset_clause(app: &App, window: &UsageWindow) -> Option<String> {
    let at = window.resets_at?;
    let when = clock::upcoming(app.stamp()?, app.moment(at))?;
    Some(format!(" · resets {when}"))
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
    let cells = width
        .saturating_sub(WINDOW_LABEL + WINDOW_SHARE + clause_columns)
        .min(METER_CELLS);

    let dim = Style::new().fg(theme.dim);
    let mut lines: Vec<Line<'static>> = reported
        .into_iter()
        .map(|(label, window, clause)| {
            let (filled, track) = meter(window.utilization, cells);
            let style = window_style(&window, theme);
            Line::from(vec![
                Span::styled(format!("{label:<WINDOW_LABEL$}"), dim),
                Span::styled(filled, style),
                Span::styled(track, dim),
                Span::styled(
                    format!(" {:>3}%", crate::app::percent(window.utilization)),
                    style.bold(),
                ),
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
            Span::styled(format!("{:<WINDOW_LABEL$}", "extra"), dim),
            Span::styled("—", dim),
            Span::styled(" · on", Style::new().fg(theme.hot).bold()),
        ]));
    }

    lines
}

/// What a model's tokens get: the widest figure [`compact`] produces
/// (`1000k`) and a column of gap before it.
const MODEL_TOKENS: usize = 6;

/// What a model's share is drawn in: three columns and the sign, with a space
/// either side of them.
const MODEL_SHARE: usize = 6;

/// What a model's cost is drawn in on a metered account: the widest figure
/// the pane prints for one (`≥$12.34`) and a column of gap before it.
const MODEL_COST: usize = 8;

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

    let labels = usage::labels(spent.iter().map(|(model, _)| model.as_str()));
    let columns = labels
        .iter()
        .map(|label| text::width(label))
        .chain(std::iter::once(text::width(CACHE_LABEL)))
        .max()
        .unwrap_or(0)
        + 1;
    // The meter gives up cells until the row fits, the way a window row's
    // does: how much of a bar is drawn is worth less than the figure beside it.
    let costed = metered(app);
    let cost_columns = match costed {
        true => MODEL_COST,
        false => 0,
    };
    let cells = width
        .saturating_sub(columns + MODEL_SHARE + MODEL_TOKENS + cost_columns)
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
            Span::styled(format!("{label:<columns$}"), dim),
            Span::styled(figure, style.bold()),
            Span::styled("  ", dim),
            Span::styled(filled, style),
            Span::styled(track, dim),
            Span::styled(format!("{count:>MODEL_TOKENS$}"), dim),
        ];
        if let Some(cost) = cost {
            spans.push(Span::styled(
                format!("{cost:>MODEL_COST$}"),
                Style::new().fg(theme.fg),
            ));
        }
        Line::from(spans)
    };

    let totals = app.session().totals();
    let session_tokens = totals.tokens().max(1);
    let percents = usage::shares(&spent.iter().map(|(_, tokens)| *tokens).collect::<Vec<_>>());
    let mut lines: Vec<Line<'static>> = spent
        .iter()
        .zip(labels)
        .zip(percents)
        .map(|(((model, tokens), label), percent)| {
            row(
                &label,
                format!("{percent:>3}%"),
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
            Some(hit) => format!("{:>3}%", crate::app::percent(hit)),
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

/// The widest label the context row carries, `context`, and a column of gap.
const CONTEXT_LABEL: usize = 8;

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
/// share of a window assumed for it. The share is never clamped: a context
/// past its window fills the bar, and the figure says by how much.
fn context_lines(app: &App, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let Some(context) = app.session().context() else {
        return Vec::new();
    };
    let dim = Style::new().fg(theme.dim);
    let label = Span::styled(format!("{:<CONTEXT_LABEL$}", "context"), dim);
    let Some(window) = context_window(app, context) else {
        return vec![Line::from(vec![
            label,
            Span::styled(compact(context.tokens), Style::new().fg(theme.fg)),
        ])];
    };

    let share = context.tokens as f64 / window.max(1) as f64;
    let figures = format!("  {} / {}", compact(context.tokens), compact(window));
    let cells = width
        .saturating_sub(CONTEXT_LABEL + WINDOW_SHARE + text::width(&figures))
        .min(METER_CELLS);
    let (filled, track) = meter(share, cells);
    let style = match share >= BUDGET_SHOWN_HOT {
        true => Style::new().fg(theme.hot),
        false => Style::new().fg(theme.fg),
    };
    vec![Line::from(vec![
        label,
        Span::styled(filled, style),
        Span::styled(track, dim),
        Span::styled(format!(" {:>3}%", crate::app::percent(share)), style.bold()),
        Span::styled(figures, dim),
    ])]
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
    let block = pane("Usage", area, Border::of(false, theme), theme);
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
    let money = money_lines(app, theme);
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

/// How many rows the money takes: the session's cost, and the budget where
/// the session runs against one.
fn money_rows(app: &App) -> usize {
    1 + usize::from(app.budget().is_some())
}

/// What the session cost, labelled for what is known about it, and what it
/// has spent of its budget.
///
/// What the figure is depends on how the session is billed. On a metered
/// account it is the bill, drawn as the headline. On a plan no money moves
/// with the work, and the same figure — the CLI prices a plan's work at list
/// prices — is what the work would have cost on the API: kept on screen so a
/// plan user sees what they consume, but dim and named for what it is, never
/// as the session's cost. Where nothing has said which it is, it is neither,
/// and the row says so rather than showing a figure it cannot vouch for.
fn money_lines(app: &App, theme: &Theme) -> Vec<Line<'static>> {
    let dim = Style::new().fg(theme.dim);
    let cost = || session_cost(app.session(), app.prices());
    let mut lines = vec![match app.session().billing() {
        Some(Billing::Metered) => Line::from(vec![
            Span::styled("session ", dim),
            Span::styled(cost(), Style::new().fg(theme.hot).bold()),
        ]),
        Some(Billing::Plan) => Line::from(format!("API-equivalent {}", cost())).style(dim),
        None => Line::from(vec![
            Span::styled("session ", dim),
            Span::styled("—", Style::new().fg(theme.fg)),
            Span::styled(" · billing not known", dim),
        ]),
    }];
    if let Some(budget) = app.budget() {
        let spent = app.session().totals().reported_cost_usd;
        lines.push(
            Line::from(format!("budget ${spent:.2}/${budget:.2}")).style(
                match spent >= budget * BUDGET_SHOWN_HOT {
                    true => Style::new().fg(theme.hot).bold(),
                    false => Style::new().fg(theme.fg),
                },
            ),
        );
    }
    lines
}

/// Columns a tool's name gets in the Usage pane's mix.
const BAR_NAME: usize = 18;

/// Columns a tool's count gets, right-aligned.
const BAR_COUNT: usize = 4;

/// Columns a family's own failure count gets beside its bar — ` ✗ 3`.
const BAR_FAILED: usize = 4;

/// The longest a bar is drawn. The bars compare the tools with one another,
/// which a dozen cells does as well as the whole pane, and a bar across the
/// pane is a block of colour the count beside it gets lost in.
const BAR_CELLS: usize = 12;

/// One `Notion·*            16 ━━ ✗ 3` row: the family, its calls, a thin bar
/// in the tool colour for how it compares with the busiest family, and its own
/// failures where it has any.
///
/// The failures are on the family's own row because the header's count says
/// only that the session failed three calls, not which tool it kept failing
/// at. A family with no failures carries no column at all — `✗ 0` is a
/// reassurance dressed as a measurement.
fn bar_line(
    family: &str,
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

    Line::from(vec![
        Span::styled(
            format!(
                "{ROW_INDENT}{:<BAR_NAME$}",
                text::truncate(family, BAR_NAME - 1)
            ),
            Style::new().fg(theme.dim),
        ),
        Span::styled(
            format!("{count:>BAR_COUNT$} "),
            Style::new().fg(theme.hot).bold(),
        ),
        Span::styled(
            format!("{:<width$}", "━".repeat(filled.min(width))),
            Style::new().fg(theme.tool),
        ),
        Span::styled(failures, Style::new().fg(theme.del)),
    ])
}

/// The Activity pane: the sub-agents the session spawned, the decisions it
/// recorded, and the tools it called.
///
/// One column for what the agent is doing and what it has decided. It scrolls
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
    rows.section(
        Section::Decisions,
        decision_rows(app, session, width, theme),
    );
    rows.section(Section::Tools, tool_rows(app, session, width, theme));
    rows
}

/// The glyph, the colour and the word a sub-agent's state is drawn with.
///
/// The glyph carries the state on its own: a sixteen-colour terminal in a
/// theme the operator chose is not somewhere a colour can be the only
/// difference between an agent that finished and one that failed.
fn agent_state(
    outcome: Option<AgentOutcome>,
    theme: &Theme,
) -> (&'static str, Color, &'static str) {
    match outcome {
        None => ("◆", theme.agent, "running"),
        Some(AgentOutcome::Completed) => ("◇", theme.dim, "done"),
        Some(AgentOutcome::Failed) => ("✗", theme.del, "failed"),
        Some(AgentOutcome::Cancelled) => ("⊘", theme.dim, "cancelled"),
    }
}

/// What the sub-agents section says about itself: how many are running, how
/// many were spawned in all, and how many failed.
///
/// A count that is zero is left out rather than drawn: `0 failed` is a line
/// the operator has to read to learn nothing.
fn agent_summary(session: &SessionState, theme: &Theme) -> Vec<Span<'static>> {
    let running = session.running_agents().len();
    let mut spans = vec![Span::styled(
        format!("{running} running"),
        Style::new().fg(theme.fg),
    )];
    let mut said = format!(" · {} spawned", session.agents_spawned());
    for (count, word) in [
        (session.agents_failed(), "failed"),
        (session.agents_cancelled(), "cancelled"),
    ] {
        if count > 0 {
            said.push_str(&format!(" · {count} {word}"));
        }
    }
    spans.push(Span::styled(said, Style::new().fg(theme.dim)));
    spans
}

/// A sub-agent per row: the state glyph, what it was spawned to do, the model
/// it answers with, and the status column — then, on a row of its own, the
/// last thing it was seen doing.
///
/// The model is drawn only where the agent's own messages named one, and
/// shortened the way the Usage pane shortens the session's. The sub-line is
/// drawn only where the backend reported a step or an answer: an empty `└` on
/// every agent that said nothing would be half the pane's rows saying nothing.
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

    let models = agent_models(app.agents());
    for (agent, model) in app.agents().iter().zip(models) {
        let (glyph, colour, word) = agent_state(agent.outcome, theme);
        let status = agent_status(agent, word, app.stamp());

        let room = width
            .saturating_sub(AGENT_GLYPH + text::width(&status) + AGENT_GAP)
            .max(1);
        let (label, model) = agent_name(&agent.label, model, room);
        let named = text::width(&label) + model.as_ref().map_or(0, |m| 1 + text::width(m));
        // What is left between the two goes between them, so the status keeps
        // the pane's right edge and the labels do not have to be one length.
        let pad = width
            .saturating_sub(AGENT_GLYPH + named + text::width(&status))
            .max(AGENT_GAP);

        let mut row = vec![
            Span::styled(format!("{glyph} "), Style::new().fg(colour).bold()),
            Span::styled(label, Style::new().fg(theme.fg)),
        ];
        if let Some(model) = model {
            row.push(Span::styled(
                format!(" {model}"),
                Style::new().fg(theme.dim),
            ));
        }
        row.push(Span::raw(" ".repeat(pad)));
        row.push(Span::styled(status, Style::new().fg(colour)));
        rows.push(Line::from(row));

        if let Some(latest) = &agent.latest {
            let room = width.saturating_sub(AGENT_SUBLINE).max(1);
            rows.push(
                Line::from(format!("  └ {}", text::truncate(latest, room)))
                    .style(Style::new().fg(theme.dim)),
            );
        }
    }
    rows
}

/// What each agent's model is called on its row, in the pane's order: the
/// Usage pane's short names, which keep an id whole where two would read
/// alike.
fn agent_models(agents: &[SubAgent]) -> Vec<Option<String>> {
    let named: Vec<&str> = agents
        .iter()
        .filter_map(|agent| agent.model.as_deref())
        .collect();
    let mut labels = usage::labels(named).into_iter();
    agents
        .iter()
        .map(|agent| agent.model.as_ref().and_then(|_| labels.next()))
        .collect()
}

/// An agent's label and model fitted into `room` cells.
///
/// The label is what the operator reads the row for, so it keeps the room:
/// the model is dropped whole where the two do not fit with enough of the
/// label left to say which agent this is, rather than both being cut.
fn agent_name(label: &str, model: Option<String>, room: usize) -> (String, Option<String>) {
    let Some(model) = model else {
        return (text::truncate(label, room), None);
    };
    let left = room.saturating_sub(1 + text::width(&model));
    match left >= AGENT_LABEL_LEAST.min(text::width(label)) && left > 0 {
        true => (text::truncate(label, left), Some(model)),
        false => (text::truncate(label, room), None),
    }
}

/// A sub-agent's status column: `running 1m 42s`, `done 4100 ctx`, `failed`.
///
/// The elapsed time is there only while the agent runs, and only where the
/// shell has both the moment it started and the moment it is drawing at — a
/// session read back from a log that kept no times has neither, and the state
/// alone is what there is to say.
///
/// An agent that finished its task carries the size its conversation reached,
/// where its backend counted one, marked `ctx` because that is what it is: the
/// tokens the agent's latest message was answered over, not what the agent
/// was billed. A failed or cancelled agent carries its state alone; why it
/// stopped is on the row beneath it.
fn agent_status(agent: &SubAgent, word: &str, now: Option<Stamp>) -> String {
    if let Some(outcome) = agent.outcome {
        return match (outcome, agent.context_tokens) {
            (AgentOutcome::Completed, Some(tokens)) => format!("{word} {} ctx", compact(tokens)),
            (AgentOutcome::Completed, None)
            | (AgentOutcome::Failed | AgentOutcome::Cancelled, _) => word.to_owned(),
        };
    }
    match agent.at.zip(now).and_then(|(at, now)| now.since(at)) {
        Some(ran) => format!("{word} {}", clock::spent(ran)),
        None => word.to_owned(),
    }
}

/// The glyph a sub-agent row opens with, and the space after it.
const AGENT_GLYPH: usize = 2;

/// The least that stands between what an agent is doing and its status, so
/// the two never run together on a row whose label fills the pane.
const AGENT_GAP: usize = 1;

/// The least of an agent's label its model is drawn beside: fewer cells than
/// this and the model is left off, because a row that names the model and not
/// the agent says nothing about which agent it is.
const AGENT_LABEL_LEAST: usize = 12;

/// The indent and the `└ ` an agent's sub-line opens with.
const AGENT_SUBLINE: usize = 4;

/// The Changes pane: what the repository says about the working tree, what
/// this session says it changed, and what it has committed.
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
    let block = pane(title, area, border, theme);
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

/// Every row the pane has, folded sections included as their header alone.
fn changes_rows(app: &App, width: usize, theme: &Theme) -> PaneRows {
    let repo = app.repo();
    let session = app.session();
    let mut rows = PaneRows::default();

    // A directory that is not a repository has no branch and no commits, and
    // sections drawn empty would say it had none rather than that there is no
    // repository to have any.
    let in_repository = repo.branch.is_some();
    if let Some(branch) = &repo.branch {
        rows.extend([branch_row(branch, repo, width, theme)]);
    }

    if in_repository {
        rows.section(
            Section::WorkingTree,
            working_tree_rows(app, repo, width, theme),
        );
        rows.section(Section::Commits, commit_rows(app, repo, width, theme));
    }
    rows.section(
        Section::Edited,
        session_file_rows(app, session, width, theme),
    );
    rows.section(Section::Tests, test_rows(app, width, theme));
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

/// A section's header: the fold marker, its name, and what it summarises.
///
/// The marker is the fold's only affordance, so it is drawn whether or not the
/// section has a key: a section that is folded must say so even when the way
/// it was folded was the one key the F-key bar names.
fn section_header(
    folded: bool,
    name: &str,
    summary: Vec<Span<'static>>,
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
    let said: usize = summary.iter().map(|span| text::width(&span.content)).sum();
    if said <= room {
        spans.push(Span::raw("  "));
        spans.extend(summary);
    }
    Line::from(spans)
}

/// What the repository measured the working tree to have changed.
///
/// These are git's figures, exact, about every file in the tree — including
/// files this session never touched. They are never mixed with the session's
/// own counts, which are a different claim by a different party and have their
/// own section below.
fn working_tree_rows(
    app: &App,
    repo: &crate::app::Repo,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let (added, removed) = repo.working.iter().fold((0u64, 0u64), |(a, r), file| {
        (
            a.saturating_add(file.added.unwrap_or(0)),
            r.saturating_add(file.removed.unwrap_or(0)),
        )
    });
    let summary = match repo.read {
        false => vec![Span::styled("—", Style::new().fg(theme.dim))],
        true => vec![
            Span::styled(files_said(repo.working.len()), Style::new().fg(theme.fg)),
            Span::raw("  "),
            Span::styled(format!("+{added}"), Style::new().fg(theme.add)),
            Span::raw(" "),
            Span::styled(format!("−{removed}"), Style::new().fg(theme.del)),
        ],
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
                &measured('+', file.added),
                &measured('−', file.removed),
                width,
                theme,
            ),
        });
    }
    rows
}

/// What this session's own edit tools said they changed.
///
/// A floor rather than a measurement: where a call did not say how many lines
/// it touched the figure carries `≥`, and where none of them did it is an em
/// dash. The paths are the backend's — absolute where it could not say
/// otherwise — which is the other reason these rows are not folded into the
/// working tree's: the two lists do not even name their files the same way.
fn session_file_rows(
    app: &App,
    session: &SessionState,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let files = session.files();
    // Summed from the per-file fold rather than kept as a second counter, so
    // the header and the rows under it cannot disagree. A section total is a
    // floor as soon as any one file's is.
    let added: u64 = files.iter().fold(0, |a, f| a.saturating_add(f.added));
    let removed: u64 = files.iter().fold(0, |r, f| r.saturating_add(f.removed));
    let added_stated = files.iter().all(FileChanges::added_stated);
    let removed_stated = files.iter().all(FileChanges::removed_stated);
    let summary = vec![
        Span::styled(files_said(files.len()), Style::new().fg(theme.fg)),
        Span::raw("  "),
        Span::styled(count('+', added, added_stated), Style::new().fg(theme.add)),
        Span::raw(" "),
        Span::styled(
            count('−', removed, removed_stated),
            Style::new().fg(theme.del),
        ),
    ];

    let folded = app.folded(Section::Edited);
    let mut rows = vec![section_header(
        folded,
        "This session",
        summary,
        width,
        theme,
    )];
    if folded || files.is_empty() {
        return rows;
    }

    for file in files {
        rows.push(counted_row(
            ROW_INDENT,
            &file.path,
            &count('+', file.added, file.added_stated()),
            &count('−', file.removed, file.removed_stated()),
            width,
            theme,
        ));
        if let Some(why) = &file.why {
            rows.push(
                Line::from(format!(
                    "    “{}”",
                    text::truncate(why, width.saturating_sub(7))
                ))
                .style(Style::new().fg(theme.dim).italic()),
            );
        }
    }
    rows
}

/// `▾ Tests  637 passed · 0 failed · 30 suites · 4.2s ago`: what the
/// session's latest test run said about itself, and how long ago its call
/// finished. No section at all where the session has run no tests, which is
/// not a run that passed nothing.
///
/// A run whose output did not hold the whole run says that it ran and that
/// its result was not read, with the status it exited with where that was a
/// failure — never a count it did not find. One the backend still knows
/// failed after its tests started says that it failed, and that its counts
/// were not read. Where the pane is too narrow for
/// all of it, the suites go first and then the age: the counts are what the
/// section is for.
fn test_rows(app: &App, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let Some((run, at)) = app.test_run() else {
        return Vec::new();
    };
    let dim = Style::new().fg(theme.dim);
    let said = match (run.counts, run.failed) {
        (Some(counts), _) => test_counts(counts, theme),
        (None, true) => test_failed_uncounted(run.exit_code, theme),
        (None, false) => test_not_read(run.exit_code, theme),
    };
    let suites = run
        .counts
        .map(|counts| Span::styled(format!(" · {}", suites_said(counts.suites)), dim));
    let age = at
        .zip(app.stamp())
        .and_then(|(at, now)| now.since(at))
        .map(|age| Span::styled(format!(" · {} ago", clock::ago(age)), dim));

    let room = width.saturating_sub(text::width("▾ Tests") + 2);
    let fits = |spans: &[Span<'static>]| {
        spans.iter().map(|s| text::width(&s.content)).sum::<usize>() <= room
    };
    let whole: Vec<Span<'static>> = said
        .iter()
        .cloned()
        .chain(suites.clone())
        .chain(age.clone())
        .collect();
    let aged: Vec<Span<'static>> = said.iter().cloned().chain(age).collect();
    let summary = [whole, aged]
        .into_iter()
        .find(|spans| fits(spans))
        .unwrap_or(said);
    vec![section_header(
        app.folded(Section::Tests),
        "Tests",
        summary,
        width,
        theme,
    )]
}

/// `637 passed · 0 failed`, with a failing run's failures in the failure
/// colour and its passes no longer in the colour that says all is well.
fn test_counts(counts: niobe_core::TestCounts, theme: &Theme) -> Vec<Span<'static>> {
    let dim = Style::new().fg(theme.dim);
    let (passed, failed) = match counts.failing() {
        true => (Style::new().fg(theme.fg), Style::new().fg(theme.del).bold()),
        false => (Style::new().fg(theme.add), dim),
    };
    let mut spans = vec![
        Span::styled(format!("{} passed", counts.passed), passed),
        Span::styled(" · ", dim),
        Span::styled(format!("{} failed", counts.failed), failed),
    ];
    if counts.ignored > 0 {
        spans.push(Span::styled(format!(" · {} ignored", counts.ignored), dim));
    }
    spans
}

/// `exit 101 · result not read`: a run that happened and whose counts are not
/// known. A zero status is not drawn, because a command whose output was
/// filtered exits with the filter's status rather than the run's.
fn test_not_read(exit_code: Option<i32>, theme: &Theme) -> Vec<Span<'static>> {
    let dim = Style::new().fg(theme.dim);
    let mut spans = Vec::new();
    if let Some(code) = exit_code.filter(|code| *code != 0) {
        spans.push(Span::styled(
            format!("exit {code}"),
            Style::new().fg(theme.del),
        ));
        spans.push(Span::styled(" · ", dim));
    }
    spans.push(Span::styled("result not read", dim));
    spans
}

/// `failed · exit 101 · counts not read`: a run known to have failed after its
/// tests started, whose output did not hold the counts — how many failed, or
/// how many ran, is not something the rest of it can say.
fn test_failed_uncounted(exit_code: Option<i32>, theme: &Theme) -> Vec<Span<'static>> {
    let dim = Style::new().fg(theme.dim);
    let mut spans = vec![Span::styled("failed", Style::new().fg(theme.del).bold())];
    if let Some(code) = exit_code {
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled(
            format!("exit {code}"),
            Style::new().fg(theme.del),
        ));
    }
    spans.push(Span::styled(" · counts not read", dim));
    spans
}

/// `1 suite`, `30 suites`.
fn suites_said(suites: u64) -> String {
    match suites {
        1 => "1 suite".to_owned(),
        n => format!("{n} suites"),
    }
}

/// The commits the session has made, newest first.
fn commit_rows(
    app: &App,
    repo: &crate::app::Repo,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let unknown = repo.commits.iter().any(|commit| commit.pushed.is_none());
    let unpushed = repo
        .commits
        .iter()
        .filter(|commit| commit.pushed == Some(false))
        .count();
    let mut summary = vec![Span::styled(
        match (repo.read, repo.commits.len()) {
            (false, _) => "—".to_owned(),
            (true, 0) => "none this session".to_owned(),
            (true, n) => format!("{n} this session"),
        },
        Style::new().fg(match repo.read {
            true => theme.fg,
            false => theme.dim,
        }),
    )];
    if !repo.commits.is_empty() {
        summary.push(Span::raw("  "));
        summary.push(Span::styled(
            // An upstream the repository could not compare against leaves this
            // not known, and a count would be a guess dressed as a figure.
            match unknown {
                true => "unpushed —".to_owned(),
                false => format!("{unpushed} unpushed"),
            },
            Style::new().fg(theme.hot),
        ));
    }

    let folded = app.folded(Section::Commits);
    let mut rows = vec![section_header(folded, "Commits", summary, width, theme)];
    if folded {
        return rows;
    }
    let ages: Vec<String> = repo
        .commits
        .iter()
        .map(|commit| age_of(commit, app.stamp()))
        .collect();
    // One column for every age in the section, so they line up under each
    // other however the magnitudes differ.
    let column = ages.iter().map(|age| text::width(age)).max().unwrap_or(0);
    for (commit, age) in repo.commits.iter().zip(ages) {
        rows.push(commit_row(commit, &age, column, width, theme));
    }
    rows
}

/// How long ago a commit was made, or an em dash where that cannot be told: a
/// commit the repository did not date, a shell that has not read its clock
/// yet, or a commit dated after the moment being drawn against — which is a
/// clock that moved rather than an age.
fn age_of(commit: &crate::app::Commit, now: Option<crate::clock::Stamp>) -> String {
    commit
        .at
        .zip(now)
        .and_then(|(at, now)| now.at().duration_since(at).ok())
        .map(clock::ago)
        .unwrap_or_else(|| "—".to_owned())
}

/// `↑ c4fed15  12m  the subject`: one commit, marked where its upstream has
/// not got it.
///
/// The mark is what says pushed from unpushed, not the colour: the pane has to
/// read on a terminal whose palette the operator chose, and in a theme where
/// the warning colour is close to the body's.
fn commit_row(
    commit: &crate::app::Commit,
    age: &str,
    column: usize,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let (mark, style) = match commit.pushed {
        Some(false) => ("↑ ", Style::new().fg(theme.hot)),
        Some(true) => ("  ", Style::new().fg(theme.dim)),
        None => ("? ", Style::new().fg(theme.dim)),
    };
    let age = format!(
        "{}{age}",
        " ".repeat(column.saturating_sub(text::width(age)))
    );
    let used = text::width(mark) + text::width(&commit.hash) + 2 + text::width(&age) + 2;
    let subject = text::truncate(&commit.subject, width.saturating_sub(used));

    Line::from(vec![
        Span::styled(mark, style),
        Span::styled(commit.hash.clone(), style),
        Span::raw("  "),
        Span::styled(age, Style::new().fg(theme.dim)),
        Span::raw("  "),
        Span::styled(
            subject,
            match commit.pushed {
                Some(true) => Style::new().fg(theme.dim),
                Some(false) | None => Style::new().fg(theme.fg),
            },
        ),
    ])
}

/// What a decision's time column is drawn in: `13:41` and a space.
const DECISION_TIME: usize = 6;

/// The decisions the session recorded, newest first.
///
/// **Newest first**, now that the pane scrolls and three sections share it.
/// The alternative — appending, which is the order the mock draws and the
/// order a transcript reads in — puts each new decision at the bottom of a
/// section whose top is wherever the sub-agents above it happen to end. The
/// pane does not follow its own tail, so a decision recorded while the
/// operator was reading something else would land below the fold and never be
/// seen. Under the header it is always one row from a heading the eye already
/// has.
fn decision_rows(
    app: &App,
    session: &SessionState,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let folded = app.folded(Section::Decisions);
    let mut rows = vec![section_header(
        folded,
        "Decisions",
        vec![Span::styled(
            session.decisions().len().to_string(),
            Style::new().fg(theme.fg),
        )],
        width,
        theme,
    )];
    if folded {
        return rows;
    }
    if session.decisions().is_empty() {
        rows.push(Line::from("  none recorded").style(Style::new().fg(theme.dim)));
        return rows;
    }

    let indent = ROW_INDENT;
    let column = text::width(indent) + DECISION_TIME;
    for (decision, at) in app.decisions().collect::<Vec<_>>().into_iter().rev() {
        // A log that kept no times gives a decision no time. The column stays
        // so the summaries keep their edge, but it is left blank rather than
        // filled with a zero, which would be a moment nobody recorded.
        let when = at
            .and_then(|at| at.local())
            .map(|time| time.to_string())
            .unwrap_or_default();
        for (i, wrapped) in text::wrap(&decision.summary, width.saturating_sub(column + 1))
            .into_iter()
            .enumerate()
        {
            let head = match i {
                0 => format!("{indent}{when:<w$}", w = DECISION_TIME),
                _ => " ".repeat(column),
            };
            rows.push(Line::from(vec![
                Span::styled(head, Style::new().fg(theme.dim)),
                Span::styled(wrapped, Style::new().fg(theme.fg)),
            ]));
        }
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
    let bar_width = width
        .saturating_sub(text::width(ROW_INDENT) + BAR_NAME + BAR_COUNT + 2 + BAR_FAILED)
        .min(BAR_CELLS);
    for (family, (count, failed)) in mix {
        rows.push(bar_line(family, *count, *failed, busiest, bar_width, theme));
    }
    rows
}

/// The tools section's header: the calls, the failures in the error colour,
/// and the rest dimmed, because a failure is the one figure in the line the
/// operator has to notice without looking for it.
fn tool_summary(tools: &ToolTotals, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(
        match tools.finished {
            1 => "1 call".to_owned(),
            calls => format!("{calls} calls"),
        },
        Style::new().fg(theme.fg),
    )];
    if tools.failed > 0 {
        spans.push(Span::styled(
            format!(" ✗ {}", tools.failed),
            Style::new().fg(theme.del),
        ));
    }
    spans.push(Span::styled(
        format!(
            " · {} denied · {} out",
            tools.denied,
            crate::app::human_bytes(tools.output_bytes)
        ),
        Style::new().fg(theme.dim),
    ));
    spans
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
/// the one case git declines to count — a binary file, where no number exists
/// rather than a number nobody read.
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

/// A row whose counts keep their columns and whose name gives way.
///
/// The name loses its front rather than its tail: a path cut at the front
/// still names the file, and a count cut anywhere is a different number.
fn counted_row(
    indent: &'static str,
    name: &str,
    added: &str,
    removed: &str,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let counts = text::width(added) + 1 + text::width(removed);
    let room = width.saturating_sub(text::width(indent) + counts + 1);
    let name = text::truncate_start(name, room);
    let gap = width
        .saturating_sub(text::width(indent) + counts)
        .saturating_sub(text::width(&name));

    Line::from(vec![
        Span::raw(indent),
        Span::styled(name, Style::new().fg(theme.fg)),
        Span::raw(" ".repeat(gap)),
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

/// How a window's meter and share are coloured: the same threshold and the
/// same colour as a budget nearly spent, because on a flat-rate plan that is
/// what a window nearly gone is.
///
/// Two states and not the mock's three. The shell has one written-down
/// threshold for *nearly gone* — [`BUDGET_SHOWN_HOT`] — and the budget row is
/// already drawn by it; a middle colour would need a second threshold nobody
/// measured, and would say that a window at one figure and a budget at the
/// same figure are different kinds of trouble.
fn window_style(window: &UsageWindow, theme: &Theme) -> Style {
    match window.utilization >= BUDGET_SHOWN_HOT {
        true => Style::new().fg(theme.hot),
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

/// The session's cost, labelled for what it is.
///
/// Four labels, and each says exactly how much is known:
///
/// * `$1.15` — every record is covered by a cost the backend reported.
/// * `~$1.15` — some are not, and `prices` valued all of them, so the figure
///   is what was reported plus an estimate at published rates.
/// * `≥$1.15` — some are not and could not be valued, so the figure is a
///   floor under the session's cost.
/// * `unpriced` — nothing was reported and nothing could be valued.
/// * `—` — there is no usage at all. Never a zero.
///
/// Public so that anything else printing a session's cost prints the same
/// label the Usage pane does.
pub fn session_cost(session: &SessionState, prices: Option<&dyn Prices>) -> String {
    let totals = session.totals();
    if totals.records == 0 {
        return "—".to_owned();
    }
    labelled(
        totals.reported_cost_usd,
        totals.cost_fully_reported(),
        estimate_unsettled(totals, prices),
    )
    .unwrap_or_else(|| "unpriced".to_owned())
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
    labelled(
        reported.unwrap_or_default(),
        owed.is_none(),
        owed.and_then(|owed| prices?.estimate(owed)),
    )
    .unwrap_or_else(|| "—".to_owned())
}

/// A cost, labelled for how much of it is known: `$` where a reported cost
/// covers all of it, `~$` where the rest was valued at published rates, `≥$`
/// where it could not be and the figure is a floor. `None` where nothing was
/// reported and nothing could be valued, which each caller words its own way.
fn labelled(reported: f64, settled: bool, estimated: Option<f64>) -> Option<String> {
    if settled {
        return Some(format!("${reported:.2}"));
    }
    if let Some(estimated) = estimated {
        return Some(format!("~${:.2}", reported + estimated));
    }
    if reported == 0.0 {
        return None;
    }
    Some(format!("≥${reported:.2}"))
}

/// What the tokens no reported cost covers come to at published rates.
///
/// `None` where there is no price sheet, or where any one model among them is
/// not in it: a total missing one model's share understates the session, and
/// an understated total shown as an estimate is worse than an honest floor.
fn estimate_unsettled(totals: &Totals, prices: Option<&dyn Prices>) -> Option<f64> {
    let prices = prices?;
    let mut sum = 0.0;
    for owed in totals.unsettled.values() {
        sum += prices.estimate(owed)?;
    }
    Some(sum)
}

/// Token counts, short enough for a column of them: thousands above ten
/// thousand, millions above a million.
pub(crate) fn compact(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{:.0}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
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
            cost_basis: None,
            settles_model: false,
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
        assert_eq!(session_caption(&session, "niobe", 80), "Session ─ niobe");
        assert_eq!(session_caption(&session, "", 80), "Session");
    }

    #[test]
    fn a_session_is_titled_by_what_it_is_about() {
        let session = SessionState::replay(&[said("Cost floors and replay pricing")]);
        assert_eq!(
            session_caption(&session, "niobe", 80),
            "Cost floors and replay pricing"
        );
    }

    #[test]
    fn a_caption_longer_than_its_pane_is_cut_between_words_inside_the_margin() {
        let session = SessionState::replay(&[said("Cost floors and replay pricing")]);
        // Thirty columns of caption and six of margin: one short, and the
        // last word goes whole.
        assert_eq!(
            session_caption(&session, "niobe", 35),
            "Cost floors and replay…"
        );
        for width in 0..=40 {
            let caption = session_caption(&session, "niobe", width);
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

    #[test]
    fn a_turn_the_backend_has_not_priced_is_estimated_rather_than_left_unpriced() {
        // Two records of 1,100 tokens each: 2,200 tokens at a tenth of a cent
        // per thousand is $0.0022, which rounds to a cent.
        let running = SessionState::replay(&[priced(None), priced(None)]);
        assert_eq!(
            session_cost(&running, Some(&ATenthOfACentPerThousand)),
            "~$0.00"
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
        assert!(MENUS.contains(&"Usage"), "{MENUS:?}");
        assert!(FKEYS.contains(&("5", "Usage")), "{FKEYS:?}");
        assert!(
            !MENUS.contains(&"Cost") && !FKEYS.iter().any(|(_, label)| *label == "Cost"),
            "nothing the operator reads still calls this pane Cost"
        );
    }

    fn changed(path: &str, added: Option<u64>, removed: Option<u64>) -> niobe_core::Event {
        niobe_core::Event::FileChange {
            path: path.to_owned(),
            added,
            removed,
            hunks: Vec::new(),
        }
    }

    /// What one row of the session's own files reads as, counts and all.
    fn row(file: &FileChanges, width: usize) -> String {
        counted_row(
            ROW_INDENT,
            &file.path,
            &count('+', file.added, file.added_stated()),
            &count('−', file.removed, file.removed_stated()),
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
    fn a_file_row_reads_as_the_counts_the_session_can_defend() {
        let mut state = SessionState::new();
        state.apply(&changed("catalog/fetch.ts", Some(38), Some(9)));
        state.apply(&changed("notes.md", Some(1), None));
        state.apply(&changed("run.ipynb", None, None));

        assert_eq!(
            row(&state.files()[0], 40),
            "  catalog/fetch.ts                +38 −9"
        );
        assert_eq!(
            row(&state.files()[1], 40),
            "  notes.md                          +1 —"
        );
        assert_eq!(
            row(&state.files()[2], 40),
            "  run.ipynb                          — —"
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

    /// The pane has to read on a sixteen-colour terminal and in a theme whose
    /// warning colour is close to its body colour, so what says pushed from
    /// unpushed cannot be the colour alone.
    #[test]
    fn a_commit_the_upstream_has_not_got_is_marked_and_not_only_coloured() {
        let theme = Theme::default();
        let commit = |pushed| crate::app::Commit {
            hash: "c4fed15".to_owned(),
            subject: "keep the etag beside the body".to_owned(),
            at: None,
            pushed,
        };
        let text = |pushed| -> String {
            commit_row(&commit(pushed), "12m", 3, 60, &theme)
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };

        assert!(
            text(Some(false)).starts_with("↑ "),
            "{:?}",
            text(Some(false))
        );
        assert!(text(Some(true)).starts_with("  "), "{:?}", text(Some(true)));
        assert!(
            text(None).starts_with("? "),
            "an upstream git would not compare against is not a commit that is pushed"
        );
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
        assert_eq!(compact(999_999), "1000k");
        assert_eq!(compact(1_000_000), "1.0M");
    }

    fn window(utilization: f64, resets_at: Option<u64>) -> UsageWindow {
        UsageWindow {
            utilization,
            resets_at,
        }
    }

    /// Either window nearly gone is the plan nearly gone: on a flat-rate plan
    /// the seven-day window running out stops the session just as surely as
    /// the five-hour one, so neither may be marked at the other's expense.
    #[test]
    fn a_window_nearly_gone_is_marked_the_way_a_budget_nearly_spent_is() {
        let theme = crate::theme::CLASSIC;
        let hot = Style::new().fg(theme.hot);
        let plain = Style::new().fg(theme.fg);

        assert_eq!(window_style(&window(0.62, None), &theme), plain);
        assert_eq!(window_style(&window(0.79, None), &theme), plain);
        assert_eq!(window_style(&window(0.80, None), &theme), hot);
        assert_eq!(window_style(&window(1.04, None), &theme), hot);
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
                        usage_lines(&app, 40, &crate::theme::CLASSIC).len() + 3,
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
                "5h    ▓▓▓▓▓▓░░░░░░  51% · resets 16:40".to_owned(),
                "7d    ▓▓▓▓▓▓▓▓▓░░░  71% · resets Tue 09:00".to_owned(),
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
            vec!["5h    ▓▓▓▓▓▓░░░░░░  51%".to_owned()],
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

        assert_eq!(
            rows_of(&app, 66),
            vec!["5h    ▓▓▓▓▓▓░░░░░░  51%".to_owned()]
        );
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
                cost_basis: None,
                settles_model: false,
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
            ["context ▓▓▓▓▓░░░░░░░  38%  76k / 200k"]
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
            ["context ▓▓▓░░░░░░░░░  25%  50k / 200k"]
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
            ["context ▓░░░░░░░░░░░   5%  50k / 1.0M"]
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
            ["context ▓▓░░░░░░░░░░  15%  30k / 200k"]
        );
    }

    /// Past the window, the bar is full and the figure says by how much.
    #[test]
    fn a_context_past_its_window_is_not_clamped() {
        let mut app = bare();
        sent(&mut app, 210_000, "opus-5", Some(200_000));
        assert_eq!(
            context_of(&app, 39),
            ["context ▓▓▓▓▓▓▓▓▓▓▓▓ 105%  210k / 200k"]
        );
    }

    /// The meter gives up cells before the figures give up characters.
    #[test]
    fn a_narrow_pane_shortens_the_meter_and_keeps_the_figures() {
        let mut app = bare();
        sent(&mut app, 76_000, "opus-5", Some(200_000));
        assert_eq!(context_of(&app, 30), ["context ▓▓░░░  38%  76k / 200k"]);
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
            cost_basis: None,
            settles_model: false,
        }));

        let rows = spend_of(&app, 66);
        let cache = rows.last().expect("the cache row is the last of them");
        assert!(cache.starts_with("cache hit"), "{rows:?}");
        assert!(cache.contains('—'), "{rows:?}");
        assert!(!cache.contains('%'), "{rows:?}");
        assert!(!cache.contains('0'), "{rows:?}");
    }
}
