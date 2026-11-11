// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The goal the operator set, in words: what the rule above the composer
//! says of it while it is set, and what the transcript keeps once a check
//! found it met.
//!
//! Every word of a verdict is the backend's: the checks and their reasons
//! come from the model it asked, and none is reached here.

use crate::theme::Theme;
use niobe_core::goal::{Goal, Standing};
use ratatui::style::Style;

/// What the rule above the composer says of `goal`, on one line, and the
/// style it is said in.
pub(crate) fn status(goal: &Goal, theme: &Theme) -> (String, Style) {
    let condition = one_line(goal.condition());
    let checks = checks(goal.checks());
    match goal.standing() {
        Standing::Pursued { last: None } => (
            format!("◎ goal: {condition}"),
            Style::new().fg(theme.title).bold(),
        ),
        Standing::Pursued { last: Some(reason) } => (
            format!("◎ goal not met after {checks}: {}", one_line(reason)),
            Style::new().fg(theme.title).bold(),
        ),
        Standing::Met { .. } => (
            format!("✓ goal met after {checks}: {condition}"),
            Style::new().fg(theme.add).bold(),
        ),
        Standing::Cleared => (
            format!("goal cleared: {condition}"),
            Style::new().fg(theme.dim),
        ),
    }
}

/// What the transcript keeps of `goal` once a check found it met: the
/// condition, how many checks it took, and why the last one found it met,
/// where the backend recorded that.
pub(crate) fn met(goal: &Goal) -> String {
    let reached = format!(
        "Goal met after {}: {}",
        checks(goal.checks()),
        goal.condition()
    );
    match goal.standing() {
        Standing::Met {
            reason: Some(reason),
        } => format!("{reached}\n\n{reason}"),
        Standing::Met { reason: None } | Standing::Pursued { .. } | Standing::Cleared => reached,
    }
}

/// `count` checks, in words.
fn checks(count: u64) -> String {
    match count {
        1 => "1 check".to_owned(),
        count => format!("{count} checks"),
    }
}

/// `text` with every run of whitespace, line breaks included, made one
/// space, so a condition written over several lines reads on the rule.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use niobe_core::goal::GoalReport;

    fn goal(reports: &[GoalReport]) -> Goal {
        reports
            .iter()
            .fold(None, Goal::after)
            .expect("the reports set a goal")
    }

    fn set() -> GoalReport {
        GoalReport::Set {
            condition: "the tests\npass".to_owned(),
        }
    }

    fn not_met() -> GoalReport {
        GoalReport::NotMet {
            reason: "two\n  fail".to_owned(),
        }
    }

    #[test]
    fn the_rule_says_the_goal_until_a_check_and_then_the_last_checks_reason() {
        let theme = Theme::default();
        assert_eq!(status(&goal(&[set()]), &theme).0, "◎ goal: the tests pass");
        assert_eq!(
            status(&goal(&[set(), not_met()]), &theme).0,
            "◎ goal not met after 1 check: two fail"
        );
        assert_eq!(
            status(&goal(&[set(), not_met(), not_met()]), &theme).0,
            "◎ goal not met after 2 checks: two fail"
        );
    }

    #[test]
    fn the_rule_says_how_the_goal_ended() {
        let theme = Theme::default();
        let met = goal(&[set(), not_met(), GoalReport::Met { reason: None }]);
        assert_eq!(
            status(&met, &theme).0,
            "✓ goal met after 2 checks: the tests pass"
        );
        let cleared = goal(&[set(), GoalReport::Cleared]);
        assert_eq!(status(&cleared, &theme).0, "goal cleared: the tests pass");
    }

    #[test]
    fn the_transcript_keeps_why_the_goal_was_met_where_the_backend_said() {
        let said = goal(&[
            set(),
            GoalReport::Met {
                reason: Some("all 41 pass".to_owned()),
            },
        ]);
        assert_eq!(
            met(&said),
            "Goal met after 1 check: the tests\npass\n\nall 41 pass"
        );
        let unsaid = goal(&[set(), GoalReport::Met { reason: None }]);
        assert_eq!(met(&unsaid), "Goal met after 1 check: the tests\npass");
    }
}
