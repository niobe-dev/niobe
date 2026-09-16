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
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph, Widget};

use niobe_core::session::SessionState;

use crate::app::{App, Entry, SelectedProfile};
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
            format!("{}  ", backend_label(app.session(), app.profile())),
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
        empty_transcript(theme)
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
fn empty_transcript(theme: &Theme) -> Vec<Line<'static>> {
    vec![
        Line::from(""),
        Line::from("  niobe").style(Style::new().fg(theme.hot).bold()),
        Line::from("  A terminal coding agent that shows you the bill.")
            .style(Style::new().fg(theme.fg)),
        Line::from(""),
        Line::from("  Every token, every decision and every minute of agent time is")
            .style(Style::new().fg(theme.dim)),
        Line::from("  visible while it happens, and nothing on screen is a guess.")
            .style(Style::new().fg(theme.dim)),
        Line::from(""),
        Line::from("  No backend is attached yet.").style(Style::new().fg(theme.dim)),
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

    lines.push(Line::from("Files").style(Style::new().fg(theme.title).bold()));
    // Which files a session touched, and by how much, needs checkpoints and
    // diffs, which are not recorded yet. An invented list here would be the one
    // thing this pane must never be.
    lines.push(
        Line::from("—  file attribution not implemented yet").style(Style::new().fg(theme.dim)),
    );

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
        backend_label(session, app.profile()),
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
    if session.errors() > 0 {
        top.push(separator);
        top.push(Span::styled(
            format!("{} errors", session.errors()),
            Style::new().fg(theme.del),
        ));
    }

    let bottom = match app.hint() {
        Some(hint) => Line::from(hint.to_owned()).style(Style::new().fg(theme.hot)),
        None => Line::from(
            "Enter send · Alt+Enter newline · PgUp/PgDn scrollback · F10 quit".to_owned(),
        )
        .style(Style::new().fg(theme.status_fg)),
    };

    frame.render_widget(
        Paragraph::new(vec![Line::from(top), bottom]).style(bar),
        area,
    );
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

/// What is running, or what is not running yet. A backend's own report of what
/// it runs wins over the profile that was selected to start it.
fn backend_label(session: &SessionState, profile: Option<&SelectedProfile>) -> String {
    match (session.meta(), profile) {
        (Some(meta), _) => format!("⚡ {} · {}", meta.backend, meta.model),
        (None, Some(profile)) => format!("{} · {}, not attached", profile.name, profile.backend),
        (None, None) => "no backend attached".to_owned(),
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
    use niobe_core::event::{Backend, SessionMeta, Usage};

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
            backend_label(&SessionState::new(), None),
            "no backend attached"
        );

        let running = SessionState::replay(&[niobe_core::event::Event::SessionMeta(SessionMeta {
            backend: Backend::Codex,
            profile: "default".to_owned(),
            model: "gpt-5-codex".to_owned(),
        })]);
        assert_eq!(backend_label(&running, None), "⚡ codex · gpt-5-codex");
    }

    #[test]
    fn a_selected_profile_is_named_until_a_backend_says_what_it_runs() {
        let work = SelectedProfile {
            name: "work".to_owned(),
            backend: Backend::Claude,
        };
        assert_eq!(
            backend_label(&SessionState::new(), Some(&work)),
            "work · claude, not attached"
        );

        let running = SessionState::replay(&[niobe_core::event::Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "work".to_owned(),
            model: "opus-5".to_owned(),
        })]);
        assert_eq!(backend_label(&running, Some(&work)), "⚡ claude · opus-5");
    }

    #[test]
    fn token_counts_stay_short() {
        assert_eq!(compact(0), "0");
        assert_eq!(compact(9_999), "9999");
        assert_eq!(compact(25_500), "26k");
        assert_eq!(compact(1_260_000), "1.3M");
    }
}
