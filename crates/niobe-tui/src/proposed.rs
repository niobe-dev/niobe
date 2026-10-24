// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What a permission question asks to let happen, drawn as what it does: an
//! edit as the lines it removes and adds, a file written as the lines it
//! will hold, a command once, a plan whole as the markdown it was written in.
//!
//! The backend reads these out of the call's arguments (see
//! [`niobe_core::proposal`]); this draws them, and lays out by key whatever
//! arguments that reading did not cover, so every one is still shown. The
//! arguments as sent stay behind Ctrl+T, character for character, since they
//! are what is approved.

use niobe_core::diff::{self, Line as DiffLine};
use niobe_core::{Action, Proposal};
use ratatui::style::Style;
use ratatui::text::Line;

use crate::text;
use crate::theme::Theme;
use crate::{arguments, hunks, markdown};

/// A [`Proposal`], with the lines of the change it proposes worked out once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposed {
    proposal: Proposal,
    /// The change as lines removed, added and kept; `None` for a command,
    /// and for a replacement too large or too binary to compare, which is
    /// then shown by its arguments instead.
    lines: Option<Vec<DiffLine>>,
    /// The arguments the proposal says nothing of, read from the call's
    /// input the first time they are drawn and kept: the input can be the
    /// whole of a file, and the question is drawn again with every tick.
    others: std::cell::OnceCell<Option<serde_json::Map<String, serde_json::Value>>>,
}

/// What a question's call is drawn as, with what the rows leave out.
#[derive(Debug, Clone, Default)]
pub(crate) struct Call {
    /// The rows, in order.
    pub rows: Vec<Line<'static>>,
    /// The rows that say how many lines are not drawn, as where each starts,
    /// how many rows it takes and how many lines it stands for: a marker is
    /// not itself a line of the call.
    pub markers: Vec<(usize, usize, usize)>,
}

impl Call {
    /// A call drawn as `rows` and nothing left out.
    pub(crate) fn of(rows: Vec<Line<'static>>) -> Self {
        Self {
            rows,
            markers: Vec::new(),
        }
    }

    /// How many lines of the call are hidden where only its first `kept` rows
    /// are drawn: the lines among the rows cut away, a blank one and a marker
    /// excepted, and the lines a marker cut away stands for.
    pub(crate) fn hidden_after(&self, kept: usize) -> usize {
        let inside = |at: usize| {
            self.markers
                .iter()
                .find(|(first, rows, _)| (*first..first + rows).contains(&at))
        };
        self.rows
            .iter()
            .enumerate()
            .skip(kept)
            .map(|(at, row)| match inside(at) {
                Some((first, _, stands_for)) if *first == at => *stands_for,
                Some(_) => 0,
                None => usize::from(row.spans.iter().any(|span| !span.content.trim().is_empty())),
            })
            .sum()
    }
}

impl Proposed {
    /// `proposal`, with its change compared into lines.
    pub fn new(proposal: Proposal) -> Self {
        let lines = match proposal.action() {
            Action::Replace { old, new, .. } => diff::replacement_lines(old, new),
            Action::Write { content, .. } => Some(
                content
                    .lines()
                    .map(|l| DiffLine::Added(l.to_owned()))
                    .collect(),
            ),
            Action::Command { .. } | Action::Plan { .. } => None,
        };
        Self {
            proposal,
            lines,
            others: std::cell::OnceCell::new(),
        }
    }

    /// The call as what it would do, `width` cells wide, then the arguments
    /// that says nothing of, by key — or `None` where there is nothing it can
    /// show better than the arguments themselves.
    ///
    /// A change longer than a transcript diff is cut where one is, unless
    /// `open`.
    #[cfg(test)]
    pub(crate) fn lines(
        &self,
        input: &str,
        width: usize,
        open: bool,
        theme: &Theme,
    ) -> Option<Vec<Line<'static>>> {
        self.call(input, width, open, theme).map(|call| call.rows)
    }

    /// [`Proposed::lines`], with the rows that stand for lines not drawn.
    pub(crate) fn call(
        &self,
        input: &str,
        width: usize,
        open: bool,
        theme: &Theme,
    ) -> Option<Call> {
        let plain = Style::new().fg(theme.fg);
        let dim = Style::new().fg(theme.dim);
        let mut rows = Vec::new();
        let mut markers = Vec::new();
        if let Action::Plan { plan } = self.proposal.action() {
            // A plan is what is being approved, so it is drawn whole, as the
            // markdown the agent wrote it in, and never cut. One the backend
            // asked about with no text is said to have none, rather than left
            // a question with nothing above its answers.
            match plan.trim().is_empty() {
                true => rows.extend(
                    text::wrap(NO_PLAN, width)
                        .into_iter()
                        .map(|row| Line::from(row).style(dim)),
                ),
                false => rows.extend(markdown::render(plan, width, theme)),
            }
            self.others_under(input, width, theme, &mut rows, &mut markers);
            return Some(Call { rows, markers });
        }
        let (subject, said) = match (self.proposal.action(), &self.lines) {
            (Action::Replace { path, every, .. }, Some(_)) => (
                path,
                every.then_some("replaces every occurrence in the file".to_owned()),
            ),
            (Action::Replace { .. }, None) => return None,
            (
                Action::Write {
                    path,
                    content,
                    existed,
                },
                _,
            ) => (path, Some(written(content, *existed))),
            (Action::Command { command }, _) => (command, None),
            (Action::Plan { .. }, _) => return None,
        };
        rows.extend(
            text::wrap_exact(subject, width)
                .into_iter()
                .map(|row| Line::from(row).style(plain.bold())),
        );
        rows.extend(
            said.iter()
                .flat_map(|said| text::wrap(said, width))
                .map(|row| Line::from(row).style(dim)),
        );
        if let Some(lines) = self.lines.as_deref().filter(|lines| !lines.is_empty()) {
            rows.push(Line::from(""));
            let (change, beyond) = hunks::proposed(lines, width, open, theme);
            if beyond > 0 {
                markers.push((rows.len() + change.len() - 1, 1, beyond));
            }
            rows.extend(change);
        }
        self.others_under(input, width, theme, &mut rows, &mut markers);
        Some(Call { rows, markers })
    }

    /// Whether what is asked is a plan going ahead, which no standing answer
    /// may approve: each plan is read before it is carried out.
    pub fn is_plan(&self) -> bool {
        matches!(self.proposal.action(), Action::Plan { .. })
    }

    /// Lays the arguments the proposal says nothing of under `rows`, by key,
    /// with a marker where some of their lines are not drawn.
    fn others_under(
        &self,
        input: &str,
        width: usize,
        theme: &Theme,
        rows: &mut Vec<Line<'static>>,
        markers: &mut Vec<(usize, usize, usize)>,
    ) {
        let others = self
            .others
            .get_or_init(|| arguments::fields_without(input, |key, _| self.proposal.covers(key)));
        if let Some(fields) = others {
            let (laid, hidden, marker_rows) = arguments::rows(fields, width, theme);
            if !laid.is_empty() {
                rows.push(Line::from(""));
                if hidden > 0 {
                    markers.push((rows.len() + laid.len() - marker_rows, marker_rows, hidden));
                }
                rows.extend(laid);
            }
        }
    }
}

/// What a plan the backend asked about with no text of its own shows instead.
const NO_PLAN: &str = "No plan came with this question.";

/// What a write does to the file beyond the lines it writes: a file that was
/// there loses everything it held, which the call does not carry, so nothing
/// can be shown as removed.
fn written(content: &str, existed: Option<bool>) -> String {
    let what = match (existed, content.is_empty()) {
        (Some(false), false) => "a new file",
        (Some(false), true) => "a new, empty file",
        (Some(true), false) => "replaces the whole file · what it held is not in the call",
        (Some(true), true) => "empties the file · what it held is not in the call",
        (None, false) => {
            "writes the whole file · what it held, if it was there, is not in the call"
        }
        (None, true) => "writes an empty file · what it held, if it was there, is not in the call",
    };
    what.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::CLASSIC;

    fn drawn(proposal: Proposal, input: &str, width: usize, open: bool) -> Vec<String> {
        Proposed::new(proposal)
            .lines(input, width, open, &CLASSIC)
            .expect("the proposal has a view")
            .iter()
            .map(|line| {
                let row: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                row.trim_end().to_owned()
            })
            .collect()
    }

    fn replace(old: &str, new: &str, every: bool) -> Proposal {
        Proposal::new(
            Action::Replace {
                path: "/repo/notes.txt".to_owned(),
                old: old.to_owned(),
                new: new.to_owned(),
                every,
            },
            ["file_path", "old_string", "new_string", "replace_all"],
        )
    }

    #[test]
    fn an_edit_is_its_file_and_the_lines_it_removes_and_adds() {
        let input = r#"{"file_path":"/repo/notes.txt","old_string":"a\nbeta\nc","new_string":"a\ngamma\nc","replace_all":false}"#;
        assert_eq!(
            drawn(
                replace("a\nbeta\nc", "a\ngamma\nc", false),
                input,
                40,
                false
            ),
            ["/repo/notes.txt", "", "  a", "- beta", "+ gamma", "  c"]
        );
    }

    #[test]
    fn an_edit_of_every_occurrence_says_so() {
        let input = r#"{"file_path":"/repo/notes.txt","old_string":"beta","new_string":"gamma","replace_all":true}"#;
        assert_eq!(
            drawn(replace("beta", "gamma", true), input, 60, false),
            [
                "/repo/notes.txt",
                "replaces every occurrence in the file",
                "",
                "- beta",
                "+ gamma",
            ]
        );
    }

    #[test]
    fn a_long_line_of_a_change_is_wrapped_and_not_cut() {
        let input = r#"{"file_path":"/repo/notes.txt","old_string":"x","new_string":"abcdefghijkl","replace_all":false}"#;
        assert_eq!(
            drawn(replace("x", "abcdefghijkl", false), input, 8, false),
            ["/repo/no", "tes.txt", "", "- x", "+ abcdef", "  ghijkl"]
        );
    }

    #[test]
    fn a_long_change_is_cut_where_a_transcript_diff_is_until_it_is_opened() {
        let new: String = (0..30).map(|at| format!("line {at}\n")).collect();
        let proposal = || replace("", &new, false);
        let cut = drawn(proposal(), "{}", 40, false);
        assert_eq!(cut.len(), 2 + hunks::MAX_ROWS + 1);
        assert_eq!(
            cut.last().map(String::as_str),
            Some("… 10 more rows · Ctrl+T shows them")
        );
        let open = drawn(proposal(), "{}", 40, true);
        assert_eq!(open.len(), 2 + 30 + 1);
    }

    #[test]
    fn a_write_says_what_happens_to_what_the_file_held() {
        let write = |existed| {
            Proposal::new(
                Action::Write {
                    path: "a.txt".to_owned(),
                    content: "one\ntwo\n".to_owned(),
                    existed,
                },
                ["file_path", "content"],
            )
        };
        assert_eq!(
            drawn(write(Some(false)), "{}", 80, false),
            ["a.txt", "a new file", "", "+ one", "+ two"]
        );
        assert_eq!(
            drawn(write(Some(true)), "{}", 80, false)[1],
            "replaces the whole file · what it held is not in the call"
        );
        assert_eq!(
            drawn(write(None), "{}", 80, false)[1],
            "writes the whole file · what it held, if it was there, is not in the call"
        );
    }

    #[test]
    fn a_plan_is_drawn_whole_as_markdown_with_its_file_by_key() {
        let plan = "Fix **mean**:\n\n1. Guard the empty list.\n2. Run it.\n3. Say so.\n4. Stop.";
        let input = format!(r#"{{"plan":{plan:?},"planFilePath":"/p.md"}}"#);
        let proposal = Proposal::new(
            Action::Plan {
                plan: plan.to_owned(),
            },
            ["plan"],
        );
        assert_eq!(
            drawn(proposal, &input, 60, false),
            [
                "Fix mean:",
                "1. Guard the empty list.",
                "2. Run it.",
                "3. Say so.",
                "4. Stop.",
                "",
                "planFilePath: /p.md"
            ]
        );
    }

    /// A plan asked about with nothing written says so rather than draw an
    /// empty question: what goes ahead is the agent's word that it planned.
    #[test]
    fn a_plan_with_no_text_says_none_came_with_it() {
        let proposal = Proposal::new(
            Action::Plan {
                plan: String::new(),
            },
            [] as [&str; 0],
        );
        assert_eq!(
            drawn(proposal, r#"{"planFilePath":"/p.md"}"#, 60, false),
            [
                "No plan came with this question.",
                "",
                "planFilePath: /p.md"
            ]
        );
    }

    #[test]
    fn a_command_is_shown_once_with_the_arguments_it_does_not_cover_by_key() {
        let input = r#"{"command":"cargo test","description":"Run the tests","timeout":60000}"#;
        let proposal = Proposal::new(
            Action::Command {
                command: "cargo test".to_owned(),
            },
            ["command"],
        );
        assert_eq!(
            drawn(proposal, input, 60, false),
            [
                "cargo test",
                "",
                "description: Run the tests",
                "timeout: 60000"
            ]
        );
    }

    #[test]
    fn a_replacement_too_large_to_compare_is_left_to_its_arguments() {
        let before: String = (0..3_000).map(|i| format!("before {i}\n")).collect();
        let after: String = (0..3_000).map(|i| format!("after {i}\n")).collect();
        let proposed = Proposed::new(replace(&before, &after, false));
        assert!(proposed.lines("{}", 80, false, &CLASSIC).is_none());
    }

    #[test]
    fn an_empty_write_says_so_for_each_state_the_file_was_in_and_draws_no_lines() {
        let write = |existed| {
            Proposal::new(
                Action::Write {
                    path: "a.txt".to_owned(),
                    content: String::new(),
                    existed,
                },
                ["file_path", "content"],
            )
        };
        assert_eq!(
            drawn(write(Some(false)), "{}", 80, false),
            ["a.txt", "a new, empty file"]
        );
        assert_eq!(
            drawn(write(Some(true)), "{}", 80, false),
            [
                "a.txt",
                "empties the file · what it held is not in the call"
            ]
        );
        assert_eq!(
            drawn(write(None), "{}", 80, false),
            [
                "a.txt",
                "writes an empty file · what it held, if it was there, is not in the call"
            ]
        );
    }
}
