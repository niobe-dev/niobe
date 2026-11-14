// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The row a compaction leaves in the transcript, saying how much the
//! conversation held before and after:
//! `⇣ context compacted · 192k → 18k tokens`.
//!
//! Each figure is one the session fold measured, or the backend's floor
//! marked as one, and an em dash where nothing has measured it yet.

use niobe_core::event::CompactTrigger;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::CompactionRow;
use crate::text;
use crate::theme::Theme;
use crate::ui::{compact, compact_floor};

/// What stands between the row's words and its figures.
const BETWEEN: &str = " · ";

/// The row a compaction leaves in the transcript, `width` cells wide, and the
/// blank line after it: what started it, and the prompt it left against the
/// one it took, each an em dash until something has measured it.
pub(crate) fn compacted(row: &CompactionRow, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let dim = Style::new().fg(theme.dim);
    let what = match row.trigger {
        CompactTrigger::Manual | CompactTrigger::Unstated => "context compacted",
        CompactTrigger::Auto => "context compacted automatically",
    };
    let figure = |tokens: Option<u64>| tokens.map_or_else(|| "—".to_owned(), compact);
    // Until a request measures what is left, the backend's own figure for
    // it is all there is, and it is a floor.
    let after = match (row.after, row.floor) {
        (None, Some(floor)) => format!("≥{}", compact_floor(floor)),
        (after, _) => figure(after),
    };
    let figures = format!("{BETWEEN}{} → {after} tokens", figure(row.before));
    let glyph = "⇣ ";
    let room = width.saturating_sub(text::width(glyph));
    let what = text::truncate(what, room);
    let figures = text::truncate(&figures, room.saturating_sub(text::width(&what)));
    vec![
        Line::from(vec![
            Span::styled(glyph, dim),
            Span::styled(what, Style::new().fg(theme.fg).bold()),
            Span::styled(figures, dim),
        ]),
        Line::from(""),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compaction(row: &CompactionRow, width: usize) -> String {
        compacted(row, width, &Theme::default())[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_compaction_row_says_what_it_shrank_from_and_to_with_a_dash_until_measured() {
        let row = CompactionRow {
            trigger: CompactTrigger::Manual,
            before: Some(192_000),
            floor: None,
            after: None,
        };
        assert_eq!(
            compaction(&row, 80),
            "⇣ context compacted · 192k → — tokens"
        );
        assert_eq!(
            compaction(
                &CompactionRow {
                    after: Some(18_000),
                    ..row
                },
                80
            ),
            "⇣ context compacted · 192k → 18k tokens"
        );
        assert_eq!(
            compaction(
                &CompactionRow {
                    trigger: CompactTrigger::Auto,
                    before: None,
                    floor: None,
                    after: Some(18_000),
                },
                80
            ),
            "⇣ context compacted automatically · — → 18k tokens"
        );
    }

    /// What the backend says the compaction left is a floor, and reads as
    /// one until the next request measures what it left.
    #[test]
    fn a_compaction_row_shows_the_backends_floor_until_a_request_measures_it() {
        let row = CompactionRow {
            trigger: CompactTrigger::Manual,
            before: Some(343_000),
            floor: Some(10_763),
            after: None,
        };
        assert_eq!(
            compaction(&row, 80),
            "⇣ context compacted · 343k → ≥10k tokens"
        );
        assert_eq!(
            compaction(
                &CompactionRow {
                    after: Some(21_683),
                    ..row
                },
                80
            ),
            "⇣ context compacted · 343k → 22k tokens"
        );
    }

    #[test]
    fn a_compaction_row_is_cut_to_the_pane() {
        let row = CompactionRow {
            trigger: CompactTrigger::Auto,
            before: Some(192_000),
            floor: Some(4_282),
            after: Some(18_000),
        };
        let said = compaction(&row, 20);
        assert!(text::width(&said) <= 20, "{said}");
    }
}
