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
//! The Usage pane has no cost-per-edit tile: that needs spend attributed to
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
use niobe_core::session::{FileChanges, SessionState, Totals};

use crate::app::{
    Activity, Answer, App, Ask, Entry, EntryKind, Picker, SelectedProfile, tool_label,
};
use crate::clock;
use crate::fx;
use crate::prices::Prices;
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
        draw_session(frame, area, app, theme);
        return;
    }

    // Session pane to right stack, 1.9 : 1, with a column of desktop between
    // them. The transcript is what a session is read in; the panes beside it
    // are figures, and a figure needs a fraction of the width a paragraph
    // does.
    let [left, right] = Layout::horizontal([Constraint::Fill(19), Constraint::Fill(10)])
        .spacing(1)
        .areas(area);

    draw_session(frame, left, app, theme);
    draw_desktop(
        frame,
        Rect::new(left.right(), area.y, 1, area.height),
        app,
        theme,
    );

    // Usage takes the rows its figures need and no more; what is left goes to
    // the two panes that grow with the session, 1.3 : 1 in favour of the files
    // it changed.
    let [usage, changes, parallel] = Layout::vertical([
        Constraint::Length(usage_height(app, pane_inner_width(right.width)).min(right.height)),
        Constraint::Fill(13),
        Constraint::Fill(10),
    ])
    .areas(right);

    draw_usage(frame, usage, app, theme);
    draw_changes(frame, changes, app.repo(), app.session(), theme);
    draw_parallel(frame, parallel, app, theme);
}

/// The strip of desktop between the panes, animated while a turn is running.
///
/// It is the one part of the screen that says the session is alive without
/// the operator reading anything, and it is drawn from the same clock as the
/// spinner under the transcript, so the two cannot disagree about whether a
/// turn is going. A theme that does not animate, and a session with no turn
/// running, leave the strip as it was: empty desktop.
fn draw_desktop(frame: &mut Frame, strip: Rect, app: &App, theme: &Theme) {
    let Some(activity) = app.activity() else {
        return;
    };
    for mote in fx::column(theme, strip.height, fx::frame_at(activity.elapsed)) {
        let at = (strip.x, strip.y.saturating_add(mote.row));
        if let Some(cell) = frame.buffer_mut().cell_mut(at) {
            cell.set_char(mote.symbol).set_fg(mote.colour);
        }
    }
}

/// The columns a pane leaves for what is written in it: the two borders and
/// the column of padding either side.
fn pane_inner_width(width: u16) -> u16 {
    width.saturating_sub(4)
}

/// Rows of content a pane keeps before it will spare a blank row under its
/// title.
const PANE_ROOM: u16 = 2;

/// The pane frame every pane shares: double borders in the frame colour, the
/// title centred on the top edge, and room between the border and what is
/// written inside it.
///
/// A column either side always, and a blank row under the title where the
/// pane is tall enough to spare one. The blank row is the first thing a short
/// pane gives up: at the smallest terminal the shell draws in, a row of the
/// changed files is worth more than the room above them.
fn pane(title: impl Into<String>, area: Rect, theme: &Theme) -> Block<'static> {
    // Two rows of border, the blank row itself, and PANE_ROOM left over.
    let top = u16::from(area.height > 2 + PANE_ROOM);
    Block::bordered()
        .border_type(BorderType::Double)
        .border_style(Style::new().fg(theme.frame))
        .style(Style::new().bg(theme.pane_bg).fg(theme.fg))
        .padding(Padding::new(1, 1, top, 0))
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

    let mut block = pane(title, area, theme);
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
    // The shell's own reply to the operator — what an F-key does, or why
    // something they asked for did not happen. It costs a row only while there
    // is one to give, and it sits against the composer because that is where
    // they were looking when they asked.
    let hint_rows = u16::from(app.hint().is_some());

    let [transcript, divider, hint, composer] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(hint_rows),
        Constraint::Length(composer_rows),
    ])
    .areas(inner);

    // The scrollbar is drawn on the pane's own border rather than inside the
    // room the padding keeps, so a transcript that overflows does not gain a
    // second vertical line beside the one the pane already has.
    draw_transcript(
        frame,
        transcript,
        area.right().saturating_sub(1),
        app,
        theme,
    );

    frame.render_widget(
        Paragraph::new(Line::from("─".repeat(usize::from(divider.width))))
            .style(Style::new().fg(theme.frame).bg(theme.pane_bg)),
        divider,
    );

    if let Some(said) = app.hint() {
        frame.render_widget(
            Paragraph::new(Line::from(text::truncate(said, usize::from(hint.width))))
                .style(Style::new().bg(theme.pane_bg).fg(theme.hot)),
            hint,
        );
    }

    let [marker, editor] =
        Layout::horizontal([Constraint::Length(2), Constraint::Min(1)]).areas(composer);
    frame.render_widget(
        Paragraph::new(Line::from(">").style(Style::new().fg(theme.hot).bold()))
            .style(Style::new().bg(theme.pane_bg)),
        marker,
    );
    app.composer().render(editor, frame.buffer_mut());
}

fn draw_transcript(frame: &mut Frame, area: Rect, border: u16, app: &mut App, theme: &Theme) {
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
    draw_scrollbar(frame, area, border, (start, total, height), theme);
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
fn draw_scrollbar(
    frame: &mut Frame,
    area: Rect,
    border: u16,
    extent: (usize, usize, usize),
    theme: &Theme,
) {
    let (start, lines, height) = extent;
    if lines <= height || area.height == 0 {
        return;
    }
    let border = Rect::new(border, area.y, 1, area.height);
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

/// The widest label a Usage tile carries, and a column of gap after it. A tile
/// narrower than this shows a label cut mid-word, which reads as a different
/// figure's name.
const TILE_COLUMNS: u16 = 11;

/// How many of the four tiles fit side by side: all four where the pane is
/// wide enough for their labels, two otherwise. The right-hand column is a
/// third of the screen, and at eighty columns that is not four tiles wide.
fn tiles_across(width: u16) -> usize {
    match width / TILE_COLUMNS {
        0..=1 => 1,
        2..=3 => 2,
        _ => 4,
    }
}

/// The denominator the tiles' columns are cut with. Ratio wants a `u32`.
fn across_ratio(across: usize) -> u32 {
    u32::try_from(across).unwrap_or(1).max(1)
}

/// Tool rows the Usage pane draws before it stops, whatever the tool mix is.
///
/// The pane is sized to its content, so without a cap a session that reached
/// for a dozen tools would take the column the files and the sub-agents are
/// read in. Four is the busiest of them; the footer counts the rest.
const MIX_SHOWN: usize = 4;

/// The rows the Usage pane needs: its frame, its tiles, whatever windows and
/// budget the session has been told about, its tool mix and its footer.
///
/// Read before the pane is drawn, because the column above it is laid out
/// from it — the pane takes the rows its figures need and leaves the rest to
/// the panes that grow with the session.
fn usage_height(app: &App, width: u16) -> u16 {
    let session = app.session();
    let windows = session
        .usage_windows()
        .map_or(0, |w| usize::from(crate::app::windows_label(w).is_some()));
    let overage = session
        .usage_windows()
        .map_or(0, |w| usize::from(w.using_overage));
    let budget = usize::from(app.budget().is_some());
    let mix = session.tools().by_name.len().clamp(1, MIX_SHOWN);
    // Two rows of border, the blank row under the title, a label row and a
    // value row per band of tiles, then the figures and the footer.
    let rows = 3 + 2 * 4_usize.div_ceil(tiles_across(width)) + windows + overage + budget + mix + 1;
    u16::try_from(rows).unwrap_or(u16::MAX)
}

fn draw_usage(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let session = app.session();
    let prices = app.prices();
    let block = pane("Usage", area, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let totals = session.totals();
    let tiles = [
        ("session", session_cost(session, prices), theme.hot),
        ("tokens in", compact(totals.input), theme.fg),
        ("tokens out", compact(totals.output), theme.fg),
        ("cache read", compact(totals.cache_read), theme.fg),
    ];

    let across = tiles_across(inner.width);
    let tile_rows = u16::try_from(tiles.len().div_ceil(across)).unwrap_or(1);
    let [tiled, rest] =
        Layout::vertical([Constraint::Length(tile_rows * 2), Constraint::Min(0)]).areas(inner);

    for (row, row_of_tiles) in tiles.chunks(across).enumerate() {
        let top = tiled.y.saturating_add(u16::try_from(row * 2).unwrap_or(0));
        if top >= tiled.bottom() {
            break;
        }
        let band = Rect::new(tiled.x, top, tiled.width, 2.min(tiled.bottom() - top));
        let [labels, values] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(band);
        let columns = Layout::horizontal(vec![Constraint::Ratio(1, across_ratio(across)); across])
            .split(labels);
        let value_columns =
            Layout::horizontal(vec![Constraint::Ratio(1, across_ratio(across)); across])
                .split(values);
        for (i, (label, value, colour)) in row_of_tiles.iter().enumerate() {
            frame.render_widget(
                Paragraph::new(Line::from(*label).style(Style::new().fg(theme.dim))),
                columns[i],
            );
            frame.render_widget(
                Paragraph::new(Line::from(value.clone()).style(Style::new().fg(*colour).bold())),
                value_columns[i],
            );
        }
    }

    // Dollars per category would need spend attributed to each read, edit or
    // shell command, which the ledger does not do yet. The bars carry the tool
    // mix instead, which is measured, and the footer says so.
    let tools = session.tools();
    let mut mix: Vec<(&String, &u64)> = tools.by_name.iter().collect();
    mix.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

    let rows = usize::from(rest.height);
    let mut lines: Vec<Line> = Vec::new();

    // On a flat-rate plan the windows are the budget, so they sit where a
    // budget sits. Absent, not zeroed, until a backend reports one: a metered
    // profile has no windows and a `0%/5h` would be a figure nobody measured.
    if let Some(windows) = session.usage_windows() {
        if let Some(label) = crate::app::windows_label(windows) {
            lines.push(Line::from(label).style(window_style(windows, theme)));
        }
        if windows.using_overage {
            lines.push(Line::from("overage").style(Style::new().fg(theme.hot).bold()));
        }
    }
    if let Some(budget) = app.budget() {
        let spent = totals.reported_cost_usd;
        lines.push(
            Line::from(format!("budget ${spent:.2}/${budget:.2}")).style(
                match spent >= budget * BUDGET_SHOWN_HOT {
                    true => Style::new().fg(theme.hot).bold(),
                    false => Style::new().fg(theme.fg),
                },
            ),
        );
    }

    if mix.is_empty() {
        lines.push(Line::from("no tool calls yet").style(Style::new().fg(theme.dim)));
    } else {
        let busiest = mix.first().map(|(_, n)| **n).unwrap_or(1).max(1);
        let bar_width = usize::from(rest.width)
            .saturating_sub(BAR_NAME + BAR_COUNT + 2)
            .min(BAR_CELLS);
        let room = rows.saturating_sub(lines.len() + 1).min(MIX_SHOWN);
        for (name, count) in mix.iter().take(room) {
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

/// Columns a tool's name gets in the Usage pane's mix.
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
    let block = pane(format!("Parallel ─ {running} running"), area, theme);
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

fn draw_changes(
    frame: &mut Frame,
    area: Rect,
    repo: &crate::app::Repo,
    session: &SessionState,
    theme: &Theme,
) {
    let block = pane("Changes", area, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let width = usize::from(inner.width);
    let mut lines: Vec<Line> = Vec::new();

    // What the session is changing things on. The shell is handed the branch
    // by whatever opened it; how far ahead of its remote it is, and what else
    // the working tree holds, is not read yet.
    if let Some(branch) = &repo.branch {
        lines.push(
            Line::from(format!(
                "⎇ {}",
                text::truncate(branch, width.saturating_sub(2))
            ))
            .style(Style::new().fg(theme.tool)),
        );
    }

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
/// The three readings are the Usage pane's: a bare figure is the whole of it,
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

/// How the windows row is coloured: the same threshold and the same colour as
/// a budget nearly spent, because on a flat-rate plan that is what a window
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
    if totals.cost_fully_reported() {
        return format!("${:.2}", totals.reported_cost_usd);
    }
    if let Some(estimated) = estimate_unsettled(totals, prices) {
        return format!("~${:.2}", totals.reported_cost_usd + estimated);
    }
    if totals.reported_cost_usd == 0.0 {
        return "unpriced".to_owned();
    }
    format!("≥${:.2}", totals.reported_cost_usd)
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
                crate::clock::LocalTime::new(14, 7),
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
                crate::clock::LocalTime::new(14, 7),
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
