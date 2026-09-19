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
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph, Widget};

use niobe_core::event::UsageWindows;
use niobe_core::session::{FileChanges, SessionState};

use crate::app::{App, Ask, Entry, Picker, SelectedProfile};
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
        draw_ask(frame, body, ask, app.asks_waiting(), &theme);
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

    let block = pane("Model", theme)
        .border_style(Style::new().fg(theme.hot))
        .padding(Padding::horizontal(1))
        .title_bottom(
            Line::from(" applied from the next turn ")
                .style(Style::new().fg(theme.dim))
                .centered(),
        );

    let text_width = usize::from(width).saturating_sub(4);
    let mut lines: Vec<Line> = Vec::new();
    for (i, model) in picker.models.iter().enumerate() {
        let on_it = i == picker.at;
        let marker = match (on_it, current == Some(model.as_str())) {
            (true, _) => PICK_CURSOR,
            (false, true) => PICK_CURRENT,
            (false, false) => "  ",
        };
        let style = match on_it {
            true => Style::new().fg(theme.hot).bold(),
            false => Style::new().fg(theme.fg),
        };
        lines.push(Line::from(vec![
            Span::styled(marker, Style::new().fg(theme.hot).bold()),
            Span::styled(text::truncate(model, text_width.saturating_sub(2)), style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(choice("↑↓", "choose", "⏎", "switch", theme));
    lines.push(choice("Esc", "keep this one", "", "", theme));

    let height = u16::try_from(lines.len() + 2)
        .unwrap_or(u16::MAX)
        .min(body.height);
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(body);
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);

    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(lines).style(Style::new().bg(theme.pane_bg)),
        inner,
    );
}

/// The permission modal: what would run, and the four ways to answer.
///
/// The whole of the arguments is shown under the target, wrapped rather than
/// truncated. Approving a call whose arguments were cut off at the pane edge
/// is approving something the operator did not read.
fn draw_ask(frame: &mut Frame, body: Rect, ask: &Ask, waiting: usize, theme: &Theme) {
    let width = ASK_COLUMNS.min(body.width.saturating_sub(ASK_MARGIN * 2));
    if width < 20 {
        return;
    }

    let block = pane("Permission", theme)
        .border_style(Style::new().fg(theme.hot))
        .padding(Padding::horizontal(1))
        .title_bottom(
            Line::from(match waiting {
                0 => " the turn is waiting on you ".to_owned(),
                1 => " 1 more prompt behind this one ".to_owned(),
                n => format!(" {n} more prompts behind this one "),
            })
            .style(Style::new().fg(theme.dim))
            .centered(),
        );

    // The two border columns and the padding inside them.
    let text_width = usize::from(width).saturating_sub(4);
    let mut lines = vec![Line::from(vec![
        Span::styled(ask.tool.clone(), Style::new().fg(theme.tool).bold()),
        Span::styled(" wants to run", Style::new().fg(theme.fg)),
    ])];
    for wrapped in text::wrap(ask.target.as_deref().unwrap_or(&ask.input), text_width) {
        lines.push(Line::from(wrapped).style(Style::new().fg(theme.hot).bold()));
    }
    if ask.target.is_some() {
        lines.push(Line::from(""));
        for wrapped in text::wrap(&ask.input, text_width) {
            lines.push(Line::from(wrapped).style(Style::new().fg(theme.dim)));
        }
    }
    lines.push(Line::from(""));
    lines.push(choice("y", "allow once", "n", "deny", theme));
    lines.extend(standing_answers(ask, text_width, theme));

    // The lines, and the two rows of border they sit inside.
    let height = u16::try_from(lines.len() + 2)
        .unwrap_or(u16::MAX)
        .min(body.height);
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(body);
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);

    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(lines).style(Style::new().bg(theme.pane_bg)),
        inner,
    );
}

/// The modal's two standing answers: every call to the tool, and every call to
/// it on this target.
///
/// They share a row while both fit its columns. A tool name wider than the
/// left column, or a rule wider than what is left of the row, gives each answer
/// rows of its own, the rule wrapped rather than cut at the modal's edge: a
/// rule is saved for good, and saving one the operator could not read whole is
/// the approval the modal exists to prevent.
fn standing_answers(ask: &Ask, text_width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let tool = format!("always {}", ask.tool);
    let target = match ask.target_rule() {
        Some(rule) => format!("always {rule}"),
        None => "always this call — no target to save".to_owned(),
    };
    let row = choice("a", &tool, "p", &target, theme);
    if text::width(&tool) < CHOICE_COLUMNS && row.width() <= text_width {
        return vec![row];
    }
    let mut lines = keyed_rows("a", &tool, text_width, theme);
    lines.extend(keyed_rows("p", &target, text_width, theme));
    lines
}

/// A key and its label wrapped to the width, the continuation rows indented
/// under the label so the key stays alone in its column.
fn keyed_rows(key: &str, label: &str, text_width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let indent = " ".repeat(text::width(key) + 1);
    text::wrap(label, text_width.saturating_sub(indent.len()))
        .into_iter()
        .enumerate()
        .map(|(row, wrapped)| {
            let lead = match row {
                0 => Span::styled(format!("{key} "), Style::new().fg(theme.hot).bold()),
                _ => Span::raw(indent.clone()),
            };
            Line::from(vec![lead, Span::styled(wrapped, Style::new().fg(theme.fg))])
        })
        .collect()
}

/// The width of the left label's column in the modal's key list.
const CHOICE_COLUMNS: usize = 14;

/// One row of the modal's two-column key list.
fn choice(
    left: &str,
    left_label: &str,
    right: &str,
    right_label: &str,
    theme: &Theme,
) -> Line<'static> {
    let key = Style::new().fg(theme.hot).bold();
    let label = Style::new().fg(theme.fg);
    Line::from(vec![
        Span::styled(format!("{left} "), key),
        Span::styled(format!("{left_label:<CHOICE_COLUMNS$}"), label),
        Span::styled(format!("{right} "), key),
        Span::styled(right_label.to_owned(), label),
    ])
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
    let width = usize::from(area.width);
    let height = usize::from(area.height);

    let lines = if app.entries().is_empty() {
        empty_transcript(app.is_attached(), theme)
    } else {
        app.entries()
            .iter()
            .flat_map(|entry| entry_lines(entry, width, theme))
            .collect()
    };

    app.measured(lines.len(), height);

    let start = app.scroll().min(lines.len());
    let end = (start + height).min(lines.len());
    frame.render_widget(
        Paragraph::new(lines[start..end].to_vec()).style(Style::new().bg(theme.pane_bg)),
        area,
    );
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
    // A tool call carries its whole story on the head line, so an empty body
    // must not become a blank line under it.
    for wrapped in text::wrap(entry.body.trim_end(), body_width) {
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(GUTTER)),
            Span::styled(wrapped, Style::new().fg(theme.fg)),
        ]));
    }
    lines.push(Line::from(""));
    lines
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
        let bar_width = usize::from(rest.width).saturating_sub(22);
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

/// One `name ████░░░ count` row.
fn bar_line(name: &str, count: u64, busiest: u64, width: usize, theme: &Theme) -> Line<'static> {
    let filled = if busiest == 0 {
        0
    } else {
        (count as usize * width) / busiest as usize
    };

    Line::from(vec![
        Span::styled(
            format!("{:<12}", text::truncate(name, 12)),
            Style::new().fg(theme.dim),
        ),
        Span::styled("█".repeat(filled), Style::new().fg(theme.hot)),
        Span::styled(
            "░".repeat(width.saturating_sub(filled)),
            Style::new().fg(theme.bar_bg),
        ),
        Span::styled(format!(" {count:>4}"), Style::new().fg(theme.fg)),
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
            .map(|(name, count)| format!("{name} {count}"))
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
