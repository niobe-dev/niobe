// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! One tool call whole: the arguments it ran with and what it returned, as
//! the backend handed them over, with the size of what it returned.
//!
//! The transcript says what a call did in a row; this is where the row's
//! source is read. Nothing is cut on the way in — the view holds the text the
//! event carried — and nothing is laid out that is not on screen: an output
//! can be megabytes, and wrapping all of it for every frame would be the
//! frame. So the text is held as rows of at most [`CHUNK`] bytes, and a frame
//! wraps the rows it draws and no others. A line longer than a row is cut
//! into several where it crosses a multiple of [`CHUNK`], each wrapped on its
//! own, so such a line can show a short row there; that is the price of
//! drawing a megabyte of minified JSON at the speed of a screenful.

use std::ops::Range;

use niobe_core::event::ToolOutcome;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::text;
use crate::theme::Theme;

/// The most bytes one row of the text holds before a line is cut into
/// another: a screenful of rows wraps in well under a frame however long the
/// lines under them are.
const CHUNK: usize = 4096;

/// How many rows Page Up and Page Down move, where the view has not been
/// drawn yet to say how many it shows.
const PAGE: usize = 10;

/// Widest the view is drawn, in columns.
const COLUMNS: u16 = 120;

/// Narrowest it is drawn.
const MIN_COLUMNS: u16 = 30;

/// One call, as the view shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    /// The tool's name and what the call does, on the view's top edge.
    title: String,
    /// The arguments, then what the call returned: what the rows point into.
    text: String,
    /// What the view shows, top to bottom.
    rows: Vec<Row>,
    /// The first row drawn, and how many of its wrapped lines are above the
    /// view.
    top: (usize, usize),
    /// How wide the text was last drawn, and how many lines of it: what a
    /// key scrolls by.
    drawn: Option<(usize, usize)>,
    /// Whether a line too long for the view breaks between words, as prose
    /// does, rather than at the last cell that fits, as output read byte for
    /// byte must.
    words: bool,
}

/// A row of the view.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    /// What follows: the arguments, or what the call returned.
    Heading(String),
    /// A piece of the text, at most [`CHUNK`] bytes and no line break.
    Text(Range<usize>),
    /// Said in place of text there is none of.
    Note(&'static str),
    /// Between the two parts.
    Blank,
}

/// What a key in the view asks the shell to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing beyond what the view did to itself.
    Stay,
    /// Close the view.
    Close,
}

/// A call as the shell holds it for the view: everything the start and the
/// end of it carried that the view shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// The tool's name, as the backend spells it.
    pub name: String,
    /// What the call does, in one line.
    pub what: String,
    /// The arguments it ran with.
    pub input: String,
    /// What it returned, its size in bytes and how it ended; `None` while it
    /// runs.
    pub ended: Option<(String, u64, ToolOutcome)>,
}

impl Detail {
    /// The view of `call`, at its top.
    pub fn of(call: &Recorded) -> Self {
        let mut text = String::with_capacity(
            call.input.len() + call.ended.as_ref().map_or(0, |(output, ..)| output.len()),
        );
        let mut rows = vec![Row::Heading(format!(
            "input · {}",
            sized(call.input.len() as u64)
        ))];
        push_text(&mut text, &mut rows, &call.input, "no arguments");
        rows.push(Row::Blank);
        match &call.ended {
            Some((output, bytes, outcome)) => {
                rows.push(Row::Heading(format!(
                    "output · {}{}",
                    sized(*bytes),
                    ended(*outcome)
                )));
                push_text(&mut text, &mut rows, output, "nothing returned");
            }
            None => {
                rows.push(Row::Heading("output".to_owned()));
                rows.push(Row::Note("still running"));
            }
        }
        Self {
            title: format!("{} · {}", call.name, call.what),
            text,
            rows,
            top: (0, 0),
            drawn: None,
            words: false,
        }
    }

    /// A page of `text` under `title`, read whole as a call is, with `none`
    /// standing where there is no text.
    pub fn page(title: &str, text: &str, none: &'static str) -> Self {
        let mut held = String::with_capacity(text.len());
        let mut rows = Vec::new();
        push_text(&mut held, &mut rows, text, none);
        Self {
            title: title.to_owned(),
            text: held,
            rows,
            top: (0, 0),
            drawn: None,
            words: true,
        }
    }

    /// What the dialog is drawn under.
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// The text the view holds: the arguments, then what the call returned,
    /// byte for byte as the event carried them.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// One key, which the view has whole while it is open.
    pub fn on_key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> Outcome {
        use ratatui::crossterm::event::KeyCode;

        let (width, room) = self.drawn.unwrap_or((usize::from(COLUMNS), PAGE));
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => return Outcome::Close,
            KeyCode::Up => self.up(1, width),
            KeyCode::Down => self.down(1, width),
            KeyCode::PageUp => self.up(room.max(1), width),
            KeyCode::PageDown | KeyCode::Char(' ') => self.down(room.max(1), width),
            KeyCode::Home => self.top = (0, 0),
            KeyCode::End => self.top = self.end(width, room),
            _ => {}
        }
        self.top = self.top.min(self.end(width, room));
        Outcome::Stay
    }

    /// The lines of row `at` at `width`.
    fn lines_of(&self, at: usize, width: usize) -> Vec<String> {
        match self.rows.get(at) {
            Some(Row::Text(range)) => {
                let line = self.text.get(range.clone()).unwrap_or("");
                let lines = match self.words {
                    true => text::wrap(line, width),
                    false => text::wrap_exact(line, width),
                };
                match lines.is_empty() {
                    true => vec![String::new()],
                    false => lines,
                }
            }
            Some(Row::Heading(said)) => vec![text::truncate(said, width)],
            Some(Row::Note(said)) => vec![text::truncate(said, width)],
            Some(Row::Blank) | None => vec![String::new()],
        }
    }

    /// How many lines row `at` takes at `width`.
    fn height_of(&self, at: usize, width: usize) -> usize {
        match self.rows.get(at) {
            Some(Row::Text(_)) => self.lines_of(at, width).len(),
            _ => 1,
        }
    }

    /// Moves the view `by` lines down, as far as the text goes. Each row is
    /// wrapped once on the way, however many of its lines are passed.
    fn down(&mut self, by: usize, width: usize) {
        let (mut row, mut line) = self.top;
        let mut height = self.height_of(row, width);
        for _ in 0..by {
            if line + 1 < height {
                line += 1;
            } else if row + 1 < self.rows.len() {
                row += 1;
                line = 0;
                height = self.height_of(row, width);
            } else {
                break;
            }
        }
        self.top = (row, line);
    }

    /// Moves the view `by` lines up, as far as the text's top. Each row is
    /// wrapped once on the way, as [`Detail::down`] does.
    fn up(&mut self, by: usize, width: usize) {
        let (mut row, mut line) = self.top;
        for _ in 0..by {
            if line > 0 {
                line -= 1;
            } else if row > 0 {
                row -= 1;
                line = self.height_of(row, width).saturating_sub(1);
            } else {
                break;
            }
        }
        self.top = (row, line);
    }

    /// Where the view's top is when its last line is at the bottom: found
    /// from the end, so only the rows a screen holds are wrapped.
    fn end(&self, width: usize, room: usize) -> (usize, usize) {
        let mut left = room.max(1);
        let mut row = self.rows.len();
        while row > 0 {
            row -= 1;
            let height = self.height_of(row, width);
            if height >= left {
                return (row, height - left);
            }
            left -= height;
        }
        (0, 0)
    }

    /// The lines on screen, `room` of them at `width` at most, each with
    /// whether it is a heading or a note rather than the text.
    fn visible(&self, width: usize, room: usize) -> Vec<(String, Kind)> {
        let (first, skip) = self.top;
        let mut shown = Vec::new();
        for at in first..self.rows.len() {
            let kind = match self.rows.get(at) {
                Some(Row::Heading(_)) => Kind::Heading,
                Some(Row::Note(_)) => Kind::Note,
                Some(Row::Text(_) | Row::Blank) | None => Kind::Text,
            };
            let lines = self.lines_of(at, width);
            let skipped = if at == first { skip } else { 0 };
            for line in lines.into_iter().skip(skipped) {
                if shown.len() == room {
                    return shown;
                }
                shown.push((line, kind));
            }
        }
        shown
    }
}

/// How a line of the view is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Heading,
    Note,
    Text,
}

/// Appends `said` to `text` as rows, or a note saying there is none of it.
fn push_text(text: &mut String, rows: &mut Vec<Row>, said: &str, none: &'static str) {
    if said.is_empty() {
        rows.push(Row::Note(none));
        return;
    }
    let base = text.len();
    text.push_str(said);
    let mut start = 0;
    for line in said.split('\n') {
        let end = start + line.len();
        let mut at = start;
        loop {
            let mut cut = (at + CHUNK).min(end);
            while !said.is_char_boundary(cut) {
                cut -= 1;
            }
            rows.push(Row::Text(base + at..base + cut));
            at = cut;
            if at >= end {
                break;
            }
        }
        start = end + 1;
    }
}

/// A size in bytes, exactly, and shortly where it is a kilobyte or more.
fn sized(bytes: u64) -> String {
    match bytes < 1024 {
        true => format!("{bytes} bytes"),
        false => format!("{bytes} bytes ({})", crate::app::human_bytes(bytes)),
    }
}

/// What the heading over what a call returned says of how it ended, where it
/// did not succeed.
fn ended(outcome: ToolOutcome) -> &'static str {
    match outcome {
        ToolOutcome::Ok => "",
        ToolOutcome::Failed => " · failed",
        ToolOutcome::Denied => " · refused",
        ToolOutcome::Interrupted => " · stopped",
    }
}

/// Draws the view over `body`, if it is open.
pub(crate) fn draw(frame: &mut Frame, body: Rect, app: &mut App, theme: &Theme) {
    let Some(detail) = app.detail() else {
        return;
    };
    let width = COLUMNS.min(body.width.saturating_sub(crate::ui::DIALOG_MARGIN * 2));
    if width < MIN_COLUMNS {
        return;
    }
    // A row each for the border and the shadow, one of room, and one above
    // the text.
    let room = usize::from(body.height).saturating_sub(5).max(1);
    let text_width = usize::from(width).saturating_sub(crate::ui::DIALOG_INSET);
    let plain = Style::new().fg(theme.dialog_fg);
    let mut lines = vec![Line::from("")];
    lines.extend(
        detail
            .visible(text_width, room)
            .into_iter()
            .map(|(line, kind)| match kind {
                Kind::Heading => Line::styled(line, plain.bold()),
                Kind::Note => Line::styled(line, Style::new().fg(theme.dim)),
                Kind::Text => Line::styled(line, plain),
            }),
    );
    let title = text::truncate(&text::controls_shown(&detail.title), text_width);
    let footer = " ↑↓ PgUp PgDn Home End scroll · Esc close ";
    let inner = crate::ui::dialog(frame, body, (width, lines.len()), (&title, footer), theme);
    frame.render_widget(Paragraph::new(lines), inner);
    app.measured_detail(text_width, room);
}

impl Detail {
    /// Told by the draw how wide the text is and how many lines of it show,
    /// so a key scrolls by what is on screen and no further than the end.
    pub(crate) fn measured(&mut self, width: usize, room: usize) {
        self.drawn = Some((width, room));
        self.top = self.top.min(self.end(width, room));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn call(input: &str, output: &str) -> Recorded {
        Recorded {
            name: "Bash".to_owned(),
            what: "cargo test".to_owned(),
            input: input.to_owned(),
            ended: Some((output.to_owned(), output.len() as u64, ToolOutcome::Ok)),
        }
    }

    fn shown(detail: &Detail, width: usize, room: usize) -> Vec<String> {
        detail
            .visible(width, room)
            .into_iter()
            .map(|(line, _)| line)
            .collect()
    }

    #[test]
    fn the_view_holds_the_arguments_and_the_output_byte_for_byte() {
        let output = "line one\n\tindented  twice\n\nlast\n";
        let detail = Detail::of(&call("{\"command\":\"cargo test\"}", output));

        assert_eq!(
            detail.text(),
            format!("{{\"command\":\"cargo test\"}}{output}")
        );
    }

    #[test]
    fn the_output_is_headed_by_its_size_and_how_the_call_ended() {
        let mut failed = call("{}", "boom");
        failed.ended = Some(("boom".to_owned(), 2048, ToolOutcome::Failed));

        let lines = shown(&Detail::of(&failed), 60, 20);

        assert_eq!(
            lines,
            [
                "input · 2 bytes",
                "{}",
                "",
                "output · 2048 bytes (2.0 kB) · failed",
                "boom",
            ]
        );
    }

    #[test]
    fn a_call_still_running_says_so_where_its_output_will_be() {
        let mut running = call("{}", "");
        running.ended = None;

        let lines = shown(&Detail::of(&running), 60, 20);

        assert_eq!(lines[3..], ["output", "still running"]);
    }

    #[test]
    fn scrolling_stops_with_the_last_line_at_the_bottom() {
        let output: String = (1..=30).map(|n| format!("{n}\n")).collect();
        let mut detail = Detail::of(&call("{}", output.trim_end()));
        detail.measured(40, 5);

        detail.on_key(key(KeyCode::End));
        assert_eq!(shown(&detail, 40, 5), ["26", "27", "28", "29", "30"]);
        detail.on_key(key(KeyCode::Down));
        assert_eq!(shown(&detail, 40, 5).last().map(String::as_str), Some("30"));

        detail.on_key(key(KeyCode::Up));
        assert_eq!(shown(&detail, 40, 5), ["25", "26", "27", "28", "29"]);
        detail.on_key(key(KeyCode::Home));
        assert_eq!(shown(&detail, 40, 5)[0], "input · 2 bytes");
    }

    #[test]
    fn a_wrapped_line_scrolls_a_line_at_a_time_and_back() {
        let mut detail = Detail::of(&call("{}", &"x".repeat(25)));
        detail.measured(10, 2);

        detail.on_key(key(KeyCode::End));
        assert_eq!(shown(&detail, 10, 2), ["xxxxxxxxxx", "xxxxx"]);
        detail.on_key(key(KeyCode::Up));
        assert_eq!(shown(&detail, 10, 2), ["xxxxxxxxxx", "xxxxxxxxxx"]);
    }

    #[test]
    fn a_line_longer_than_a_chunk_is_shown_whole_across_its_rows() {
        let line = "é".repeat(CHUNK);
        let detail = Detail::of(&call("{}", &line));

        let joined: String = shown(&detail, 80, usize::MAX / 2)
            .into_iter()
            .skip(4)
            .collect();

        assert_eq!(joined, line);
    }

    #[test]
    fn a_control_character_in_the_output_is_shown_and_not_sent() {
        let detail = Detail::of(&call("{}", "red\u{1b}[31m"));

        assert_eq!(shown(&detail, 40, 20)[4], "red\u{241b}[31m");
    }

    #[test]
    fn esc_enter_and_q_close_the_view() {
        for code in [KeyCode::Esc, KeyCode::Enter, KeyCode::Char('q')] {
            let mut detail = Detail::of(&call("{}", "out"));
            assert_eq!(detail.on_key(key(code)), Outcome::Close, "{code:?}");
        }
    }
}
