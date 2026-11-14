// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The info panel: what the session stands at, in the top-right corner of
//! the session pane, over the transcript, with a shadow under it.
//!
//! The main view gives the transcript the whole width, and the panel is
//! what it keeps of the right-hand stack: the Usage pane's figures, the
//! branch and the lines the working tree changes, and the sub-agents and
//! shell commands at work now. It draws only what there is — a group with
//! nothing in it takes no row, not even one saying so, and a panel with
//! nothing in it is not drawn — so that it covers as little of the
//! transcript as it can.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::App;
use crate::text;
use crate::theme::Theme;
use crate::ui;

/// The most columns the panel is drawn in, border included: room for the
/// Usage rows whole, meters at their full length. Every column it takes is
/// one the transcript under it loses, so it is as narrow as its rows let it
/// be below that.
const MOST_COLUMNS: u16 = 50;

/// The fewest columns inside the panel a row of what is at work is given: a
/// sub-agent's tag, a few words of what it was spawned to do and how long it
/// has run. What it was spawned to do is cut to the row, so it sets no width
/// of its own.
const AT_WORK_COLUMNS: usize = 32;

/// Columns the border and the padding take from the panel's width.
const INSET: u16 = 4;

/// Rows the border takes from the panel's height.
const BORDER_ROWS: u16 = 2;

/// Columns right of the panel and rows under it that its shadow is cast on:
/// see [`ui::cast_shadow`].
const SHADOW_COLUMNS: u16 = 2;
const SHADOW_ROWS: u16 = 1;

/// Draws the panel over the top-right corner of `transcript`, and tells
/// `app` where, so a click on it does not land on the line it hides.
///
/// Nothing is drawn while a question waits: the operator allows a call only
/// once every row of the question has been on screen, and a row the panel
/// covered would have been counted as read.
pub(crate) fn draw(frame: &mut Frame, transcript: Rect, app: &mut App, theme: &Theme) {
    app.drew_info(None);
    if app.asking().is_some() {
        return;
    }
    let most = MOST_COLUMNS
        .min(transcript.width.saturating_sub(SHADOW_COLUMNS))
        .saturating_sub(INSET);
    let inner = columns(app, usize::from(most), theme);
    let rows = transcript.height.saturating_sub(BORDER_ROWS + SHADOW_ROWS);
    let mut lines = lines(app, inner, theme);
    lines.truncate(usize::from(rows));
    if lines.is_empty() {
        return;
    }
    let width = u16::try_from(inner).unwrap_or(most) + INSET;
    let height = u16::try_from(lines.len()).unwrap_or(rows) + BORDER_ROWS;
    let area = Rect::new(
        transcript.right() - SHADOW_COLUMNS - width,
        transcript.y,
        width,
        height,
    );

    ui::uncover_left_edge(frame.buffer_mut(), area);
    ui::cast_shadow(frame, area, transcript, theme);
    let block = ui::plain_frame(theme);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
    app.drew_info(Some(area));
}

/// How wide the inside of the panel is: as wide as the widest of its rows
/// needs, and no wider than `most`.
///
/// The Usage rows are measured as they are laid out at `most`, where their
/// meters are at their full length; the rules between groups, which are as
/// wide as they are given, are not rows to measure.
fn columns(app: &App, most: usize, theme: &Theme) -> usize {
    let usage = ui::usage_available(app, most, theme)
        .iter()
        .map(spoken)
        .filter(|said| !said.chars().all(|c| c == '─'))
        .map(|said| text::width(said.trim_end()))
        .max()
        .unwrap_or(0);
    let branch = ui::branch_and_changes_columns(app).unwrap_or(0);
    let agents = match ui::running_agent_lines(app, most, theme).is_empty() {
        true => 0,
        false => AT_WORK_COLUMNS,
    };
    let shells = app
        .shells()
        .into_iter()
        .map(|command| text::width(SHELL_MARK) + text::width(&one_line(command)))
        .max()
        .unwrap_or(0);
    usage.max(branch).max(agents).max(shells).min(most)
}

/// What `line` says, its spans one after another.
fn spoken(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// `command` on one line, each run of whitespace in it a space.
fn one_line(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every row the panel holds, `width` columns wide, a rule between each two
/// groups that have any: usage, the branch, and what is at work.
pub(crate) fn lines(app: &App, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let usage = ui::usage_available(app, width, theme);
    let branch: Vec<Line<'static>> = ui::branch_and_changes(app, width, theme)
        .into_iter()
        .collect();
    let mut at_work = ui::running_agent_lines(app, width, theme);
    at_work.extend(shell_lines(app, width, theme));
    ui::ruled([usage, branch, at_work], width, theme)
}

/// A row for each shell command running now: `$ ` and what it runs on one
/// line, its line breaks read as spaces, cut to the row.
fn shell_lines(app: &App, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let room = width.saturating_sub(text::width(SHELL_MARK));
    app.shells()
        .into_iter()
        .map(|command| {
            let said = text::truncate(&one_line(command), room);
            Line::from(vec![
                Span::styled(SHELL_MARK, Style::new().fg(theme.tool).bold()),
                Span::styled(said, Style::new().fg(theme.fg)),
            ])
        })
        .collect()
}

/// What a shell command's row opens with.
const SHELL_MARK: &str = "$ ";

#[cfg(test)]
mod tests {
    use super::*;

    use niobe_core::event::{AgentId, Event, UsageWindow, UsageWindows};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::app::{Repo, WorkingFile};

    fn in_main(working: Vec<WorkingFile>) -> App {
        App::new(Repo {
            name: "niobe".to_owned(),
            branch: Some("main".to_owned()),
            read: true,
            working,
            ..Default::default()
        })
    }

    fn changed(path: &str, added: Option<u64>, removed: Option<u64>) -> WorkingFile {
        WorkingFile {
            path: path.to_owned(),
            added,
            removed,
            ..Default::default()
        }
    }

    fn texts(app: &App, width: usize) -> Vec<String> {
        lines(app, width, &Theme::default())
            .iter()
            .map(spoken)
            .collect()
    }

    fn windows(five_hour: f64) -> Event {
        Event::UsageWindows(UsageWindows {
            five_hour: Some(UsageWindow {
                utilization: five_hour,
                resets_at: None,
            }),
            ..Default::default()
        })
    }

    /// Outside a repository, with no window, no token and nothing at work,
    /// the panel has nothing to say, and says nothing: no row of it.
    #[test]
    fn a_panel_with_nothing_to_show_has_no_rows() {
        let app = App::new(Repo {
            name: "scratch".to_owned(),
            ..Default::default()
        });
        assert!(texts(&app, 40).is_empty());
    }

    /// A session that has just started has asked for its windows and spent
    /// nothing: the Usage pane says so in rows the panel leaves out.
    #[test]
    fn the_panel_draws_no_row_for_a_figure_not_come_yet() {
        let app = in_main(Vec::new()).attached();
        assert_eq!(texts(&app, 40), vec!["⎇ main".to_owned()]);
    }

    /// The windows, the branch and the working tree's counts, a rule between
    /// them; no row about sub-agents or shells while none is at work.
    #[test]
    fn the_panel_shows_the_windows_and_the_branch_with_what_the_tree_changes() {
        let mut app = in_main(vec![
            changed("src/lib.rs", Some(12), Some(3)),
            changed("logo.png", None, None),
        ]);
        app.apply(&windows(0.62));

        let rows = texts(&app, 40);

        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(
            rows[0].starts_with("5h") && rows[0].contains("62%"),
            "{rows:?}"
        );
        assert_eq!(rows[1], "─".repeat(40));
        assert!(rows[2].starts_with("⎇ main"), "{rows:?}");
        assert!(
            rows[2].ends_with("+≥12 −≥3"),
            "a binary file makes a floor: {rows:?}"
        );
        assert_eq!(
            text::width(&rows[2]),
            40,
            "the counts stand at the right edge"
        );
    }

    /// A tree with nothing uncommitted has no counts to give: `+0 −0` would
    /// be a row reporting nothing.
    #[test]
    fn a_clean_tree_leaves_the_branch_alone() {
        assert_eq!(texts(&in_main(Vec::new()), 40), vec!["⎇ main".to_owned()]);
    }

    /// A running sub-agent and a command in the background are listed under
    /// the rest; one that finished leaves the list, and the group goes with
    /// its last row.
    #[test]
    fn what_is_at_work_is_listed_while_it_runs() {
        let mut app = in_main(Vec::new());
        let agent = AgentId::new("a1");
        app.apply(&Event::AgentSpawn {
            id: agent.clone(),
            parent: None,
            kind: Some("Explore".to_owned()),
            label: "Map the parser".to_owned(),
        });
        app.apply(&Event::ToolCallStart {
            id: "b1".into(),
            name: "Bash".to_owned(),
            input: "{}".to_owned(),
            summary: Some("npm run dev\nnpm run watch".to_owned()),
            agent: None,
        });
        app.apply(&Event::ToolCallEnd {
            id: "b1".into(),
            name: "Bash".to_owned(),
            input: "{}".to_owned(),
            output: String::new(),
            bytes: 0,
            outcome: niobe_core::event::ToolOutcome::Ok,
            summary: None,
            exit_code: None,
            error: None,
            command: None,
            untrimmed: None,
        });
        app.apply(&Event::BackgroundCommandStarted { id: "b1".into() });

        let rows = texts(&app, 40);
        assert_eq!(rows.len(), 4, "{rows:?}");
        assert_eq!(rows[1], "─".repeat(40));
        assert!(
            rows[2].starts_with("◆ ") && rows[2].contains("Map the parser"),
            "{rows:?}"
        );
        assert_eq!(rows[3], "$ npm run dev npm run watch", "on one line");

        app.apply(&Event::AgentExit {
            id: agent,
            outcome: niobe_core::event::AgentOutcome::Completed,
        });
        app.apply(&Event::BackgroundCommandEnded { id: "b1".into() });
        assert_eq!(texts(&app, 40), vec!["⎇ main".to_owned()]);
    }

    fn spent(billing: niobe_core::event::Billing) -> App {
        let mut app = in_main(Vec::new());
        app.apply(&Event::Billing { billing });
        app.apply(&Event::Usage(niobe_core::event::Usage {
            input: 1_000,
            output: 100,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: Some(0),
            reasoning: 0,
            model: "opus-5".to_owned(),
            cost_usd: Some(0.5),
            settles_model: Some(true),
            fast: false,
        }));
        app
    }

    /// On a plan no money moves with the work, and the panel shows none, as
    /// the Usage pane shows none; billed by use, the money is what runs out
    /// and comes first.
    #[test]
    fn the_panel_shows_money_only_where_the_session_is_billed_by_use() {
        let plan = texts(&spent(niobe_core::event::Billing::Plan), 40);
        assert!(plan.iter().all(|row| !row.contains('$')), "{plan:?}");
        assert!(plan[0].starts_with("opus-5"), "{plan:?}");

        let metered = texts(&spent(niobe_core::event::Billing::Metered), 40);
        assert!(metered[0].starts_with("session $0.50"), "{metered:?}");
    }

    fn drawn(app: &mut App, (width, height): (u16, u16)) -> Vec<String> {
        let mut terminal =
            Terminal::new(TestBackend::new(width, height)).expect("a test backend opens");
        terminal
            .draw(|frame| draw(frame, frame.area(), app, &Theme::default()))
            .expect("a test backend draws");
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect()
    }

    /// The panel stands in the top-right corner, two columns in from the
    /// edge for its shadow, as wide and as tall as its rows.
    #[test]
    fn the_panel_is_drawn_in_the_top_right_corner_and_says_where() {
        let mut app = in_main(Vec::new());
        let rows = drawn(&mut app, (120, 10));

        assert_eq!(rows[0], format!("{}┌────────┐  ", " ".repeat(108)));
        assert_eq!(rows[1], format!("{}│ ⎇ main │  ", " ".repeat(108)));
        assert_eq!(rows[2], format!("{}└────────┘  ", " ".repeat(108)));
        assert_eq!(app_info(&app), Some(Rect::new(108, 0, 10, 3)));
    }

    /// A question waiting is not drawn over, and the panel says it drew
    /// nothing, so a click there reaches the question.
    #[test]
    fn no_panel_is_drawn_over_a_question() {
        let mut app = in_main(Vec::new()).attached();
        app.apply(&Event::PermissionRequest {
            id: "q1".into(),
            tool: "Bash".to_owned(),
            input: r#"{"command":"ls"}"#.to_owned(),
            target: None,
            proposal: None,
            reason: None,
            agent: None,
        });
        let rows = drawn(&mut app, (120, 10));
        assert!(rows.iter().all(|row| row.trim().is_empty()), "{rows:?}");
        assert_eq!(app_info(&app), None);
    }

    /// The panel is as wide as its widest row: the Usage rows with their
    /// meters whole, and a sub-agent's row at least as wide as a few words
    /// of its task, up to its most.
    #[test]
    fn the_panel_is_as_wide_as_its_widest_row_up_to_its_most() {
        let most = usize::from(MOST_COLUMNS - INSET);
        let theme = Theme::default();
        let mut app = in_main(vec![changed("src/lib.rs", Some(12), Some(3))]);
        assert_eq!(
            columns(&app, most, &theme),
            "⎇ main  +12 −3".chars().count()
        );

        app.apply(&windows(0.62));
        assert_eq!(columns(&app, most, &theme), "5h  62% ".len() + METER);

        app.apply(&Event::AgentSpawn {
            id: AgentId::new("a1"),
            parent: None,
            kind: None,
            label: "Map the parser".to_owned(),
        });
        assert_eq!(columns(&app, most, &theme), AT_WORK_COLUMNS);
        assert_eq!(columns(&app, 20, &theme), 20, "no wider than its most");
    }

    /// A meter's full length, which a window's row draws where it has room.
    const METER: usize = 12;

    fn app_info(app: &App) -> Option<Rect> {
        app.info_drawn()
    }
}
