// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A goal the operator set the agent: a condition the backend checks each
//! time a turn would end, keeping the turn going until a check finds it met.
//!
//! The backend runs the checks and words their reasons; Niobe only folds
//! what it reported, so where the goal stands is the backend's verdict and
//! never one Niobe reached itself. A check is a model's reading of the
//! conversation against the condition, and the shell shows its reason as
//! that model wrote it.

use serde::{Deserialize, Serialize};

/// One thing the backend reported about the session's goal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "goal", rename_all = "snake_case")]
pub enum GoalReport {
    /// A goal was set, replacing any before it. The backend states one again
    /// after compacting the conversation, which is the same goal still being
    /// worked toward.
    Set {
        /// The condition, in the operator's words.
        condition: String,
    },
    /// A check found the goal not met, and the turn goes on.
    NotMet {
        /// Why, in the checking model's words.
        reason: String,
    },
    /// A check found the goal met, and the backend stopped working toward it.
    Met {
        /// Why, in the checking model's words, where the backend recorded it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The operator cleared the goal before a check found it met.
    Cleared,
}

/// Where the session's goal stands, folded from every [`GoalReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Goal {
    condition: String,
    checks: u64,
    standing: Standing,
}

/// Whether a goal is still being worked toward, and how it ended if not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// Still set. The reason is the last check's, where one has run.
    Pursued {
        /// Why the last check found it not met.
        last: Option<String>,
    },
    /// A check found it met.
    Met {
        /// Why, where the backend recorded it.
        reason: Option<String>,
    },
    /// The operator cleared it.
    Cleared,
}

impl Goal {
    /// The goal after `report`, from `before`.
    ///
    /// A check or an end that names no goal Niobe saw set is passed over:
    /// nothing says which condition it was about.
    #[must_use]
    pub fn after(before: Option<Self>, report: &GoalReport) -> Option<Self> {
        match (before, report) {
            (Some(goal), GoalReport::Set { condition })
                if goal.is_pursued() && goal.condition == *condition =>
            {
                Some(goal)
            }
            (_, GoalReport::Set { condition }) => Some(Self {
                condition: condition.clone(),
                checks: 0,
                standing: Standing::Pursued { last: None },
            }),
            (Some(goal), GoalReport::NotMet { reason }) if goal.is_pursued() => {
                Some(goal.checked(Standing::Pursued {
                    last: Some(reason.clone()),
                }))
            }
            (Some(goal), GoalReport::Met { reason }) if goal.is_pursued() => {
                Some(goal.checked(Standing::Met {
                    reason: reason.clone(),
                }))
            }
            (Some(goal), GoalReport::Cleared) if goal.is_pursued() => Some(Self {
                standing: Standing::Cleared,
                ..goal
            }),
            (before, GoalReport::NotMet { .. } | GoalReport::Met { .. } | GoalReport::Cleared) => {
                before
            }
        }
    }

    /// The condition, in the operator's words.
    #[must_use]
    pub fn condition(&self) -> &str {
        &self.condition
    }

    /// How many checks the backend reported running on it.
    #[must_use]
    pub fn checks(&self) -> u64 {
        self.checks
    }

    /// Where it stands.
    #[must_use]
    pub fn standing(&self) -> &Standing {
        &self.standing
    }

    /// Whether it is still being worked toward.
    #[must_use]
    pub fn is_pursued(&self) -> bool {
        matches!(self.standing, Standing::Pursued { .. })
    }

    fn checked(self, standing: Standing) -> Self {
        Self {
            checks: self.checks.saturating_add(1),
            standing,
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(condition: &str) -> GoalReport {
        GoalReport::Set {
            condition: condition.to_owned(),
        }
    }

    fn not_met(reason: &str) -> GoalReport {
        GoalReport::NotMet {
            reason: reason.to_owned(),
        }
    }

    fn folded(reports: &[GoalReport]) -> Option<Goal> {
        reports.iter().fold(None, Goal::after)
    }

    #[test]
    fn each_check_counts_and_the_last_ones_reason_is_kept() {
        let goal = folded(&[set("tests pass"), not_met("two fail"), not_met("one fails")])
            .expect("a goal was set");
        assert_eq!(goal.condition(), "tests pass");
        assert_eq!(goal.checks(), 2);
        assert_eq!(
            goal.standing(),
            &Standing::Pursued {
                last: Some("one fails".to_owned())
            }
        );
    }

    #[test]
    fn a_met_check_ends_the_goal_and_counts_as_a_check() {
        let goal = folded(&[
            set("tests pass"),
            not_met("one fails"),
            GoalReport::Met {
                reason: Some("all pass".to_owned()),
            },
        ])
        .expect("a goal was set");
        assert_eq!(goal.checks(), 2);
        assert!(!goal.is_pursued());
        assert_eq!(
            goal.standing(),
            &Standing::Met {
                reason: Some("all pass".to_owned())
            }
        );
    }

    #[test]
    fn the_same_goal_stated_again_keeps_its_checks_and_a_new_one_starts_over() {
        let restated = folded(&[set("tests pass"), not_met("one fails"), set("tests pass")])
            .expect("a goal was set");
        assert_eq!(restated.checks(), 1);

        let replaced = folded(&[set("tests pass"), not_met("one fails"), set("docs build")])
            .expect("a goal was set");
        assert_eq!(replaced.condition(), "docs build");
        assert_eq!(replaced.checks(), 0);

        let again = folded(&[set("tests pass"), GoalReport::Cleared, set("tests pass")])
            .expect("a goal was set");
        assert!(again.is_pursued());
    }

    #[test]
    fn a_report_about_no_goal_seen_set_or_one_already_ended_changes_nothing() {
        assert_eq!(folded(&[not_met("one fails"), GoalReport::Cleared]), None);
        let met = folded(&[set("tests pass"), GoalReport::Met { reason: None }]);
        let after = [
            not_met("late"),
            GoalReport::Cleared,
            GoalReport::Met { reason: None },
        ]
        .iter()
        .fold(met.clone(), Goal::after);
        assert_eq!(after, met);
    }

    #[test]
    fn a_report_reads_back_as_it_was_written() {
        for report in [
            set("tests pass"),
            not_met("one fails"),
            GoalReport::Met { reason: None },
            GoalReport::Met {
                reason: Some("all pass".to_owned()),
            },
            GoalReport::Cleared,
        ] {
            let written = serde_json::to_string(&report).expect("a report is plain data");
            let read: GoalReport = serde_json::from_str(&written).expect("read back");
            assert_eq!(read, report, "{written}");
        }
    }
}
