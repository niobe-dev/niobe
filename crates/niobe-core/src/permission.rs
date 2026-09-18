// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Standing answers to permission prompts.
//!
//! A prompt the operator answers with "always" leaves a [`Rule`] behind, and
//! the [`Allowlist`] of those rules is what stops the same prompt coming back.
//! Both live here because two crates that cannot see each other need the same
//! words for them: the shell decides and matches, and the config stores.
//!
//! A rule is written the way the vendor CLIs write theirs, which is also how
//! it appears in a config file:
//!
//! * `Bash` — every call to that tool.
//! * `Bash(cargo test)` — that tool, called on exactly that target.
//! * `Bash(cargo *)` — that tool, called on a target starting `cargo `.
//!
//! **Niobe writes only the first two.** A prompt answered with "always this
//! target" stores the target as it stood, never a generalisation of it:
//! turning `cargo test` into `cargo *` would be Niobe deciding on its own that
//! `cargo publish` is allowed. The `*` exists so that an operator can write
//! that rule deliberately, in their own config, where they can see what it
//! covers.
//!
//! What a call's target is, is the backend's to say: the thing the call acts
//! on — a shell command, a path, a URL — carried on
//! [`Event::PermissionRequest`]. A call with no target can only be covered by
//! a rule for the whole tool.
//!
//! [`Event::PermissionRequest`]: crate::event::Event::PermissionRequest

use std::fmt;

use serde::{Deserialize, Serialize};

/// The character that makes the rest of a target a prefix match.
const WILDCARD: char = '*';

/// A standing answer: a tool, and optionally the target it is allowed on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Rule {
    tool: String,
    target: Option<String>,
}

impl Rule {
    /// Every call to `tool`, whatever it is called on.
    pub fn tool(tool: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            target: None,
        }
    }

    /// Calls to `tool` on `target`, which is matched as a prefix when it ends
    /// in `*` and exactly otherwise.
    pub fn targeted(tool: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            target: Some(target.into()),
        }
    }

    /// Reads a rule as a config file writes it: `Bash` or `Bash(cargo test)`.
    ///
    /// The target runs from the first `(` to the last `)`, so a command with
    /// brackets of its own survives being written down and read back. A target
    /// is whatever a call acts on, and a rule that could not hold one of those
    /// would be a rule the operator could make and never keep.
    pub fn parse(text: &str) -> Result<Self, RuleError> {
        let text = text.trim();
        let invalid = || RuleError(text.to_owned());
        let Some((tool, rest)) = text.split_once('(') else {
            return match text.contains(')') || text.is_empty() {
                true => Err(invalid()),
                false => Ok(Self::tool(text)),
            };
        };

        let target = rest.strip_suffix(')').ok_or_else(invalid)?;
        if tool.trim().is_empty() || target.is_empty() {
            return Err(invalid());
        }
        Ok(Self::targeted(tool.trim(), target))
    }

    /// The tool this rule answers for.
    pub fn tool_name(&self) -> &str {
        &self.tool
    }

    /// What it is limited to, where it is limited to anything.
    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }

    /// Whether this rule answers a call to `tool` on `target`.
    ///
    /// A rule with a target never covers a call that has none: a call the
    /// backend could not name a target for is not one this rule was written
    /// about, and allowing it would widen the rule past what the operator saw.
    pub fn covers(&self, tool: &str, target: Option<&str>) -> bool {
        if self.tool != tool {
            return false;
        }
        match (&self.target, target) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(allowed), Some(target)) => match allowed.strip_suffix(WILDCARD) {
                Some(prefix) => target.starts_with(prefix),
                None => allowed == target,
            },
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.target {
            Some(target) => write!(f, "{}({target})", self.tool),
            None => f.write_str(&self.tool),
        }
    }
}

impl From<Rule> for String {
    fn from(rule: Rule) -> Self {
        rule.to_string()
    }
}

impl TryFrom<String> for Rule {
    type Error = RuleError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}

impl std::str::FromStr for Rule {
    type Err = RuleError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

/// Text that is not a rule, kept whole so the operator sees what they wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleError(String);

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not a permission rule; expected a tool name, such as `Read`, \
             or a tool name and what it is allowed on, such as `Bash(cargo test)`",
            self.0
        )
    }
}

impl std::error::Error for RuleError {}

/// The rules a session answers permission prompts with before asking.
///
/// Order is the order the rules were written, which is also the order a config
/// file lists them: nothing here depends on it, and keeping it means a file
/// Niobe rewrites still reads the way the operator left it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Allowlist {
    rules: Vec<Rule>,
}

impl Allowlist {
    /// No standing answers: every prompt is asked.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a call to `tool` on `target` is already allowed.
    pub fn allows(&self, tool: &str, target: Option<&str>) -> bool {
        self.rules.iter().any(|rule| rule.covers(tool, target))
    }

    /// Adds a rule, and says whether it added anything. A rule the list
    /// already holds, and one a broader rule already covers, changes nothing:
    /// a config that grew a line for every approval would be a config nobody
    /// could read.
    pub fn insert(&mut self, rule: Rule) -> bool {
        if self.rules.contains(&rule) || self.allows(rule.tool_name(), rule.target()) {
            return false;
        }
        self.rules.push(rule);
        true
    }

    /// Every rule, in the order it was written.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Whether there are no rules at all.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// This list with `over`'s rules added to it, keeping the order of both.
    ///
    /// Allowlists add up rather than replacing one another: a rule is a
    /// permission granted, and a repository's file cannot take back one the
    /// operator granted in their own.
    #[must_use]
    pub fn merge(mut self, over: Self) -> Self {
        for rule in over.rules {
            self.insert(rule);
        }
        self
    }
}

impl FromIterator<Rule> for Allowlist {
    fn from_iter<I: IntoIterator<Item = Rule>>(rules: I) -> Self {
        let mut list = Self::new();
        for rule in rules {
            list.insert(rule);
        }
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rule_for_a_tool_covers_every_call_to_it() {
        let rule = Rule::tool("Read");

        assert!(rule.covers("Read", Some("/repo/notes.txt")));
        assert!(rule.covers("Read", None));
        assert!(!rule.covers("Write", Some("/repo/notes.txt")));
    }

    #[test]
    fn a_targeted_rule_covers_that_target_and_nothing_near_it() {
        let rule = Rule::targeted("Bash", "cargo test");

        assert!(rule.covers("Bash", Some("cargo test")));
        assert!(!rule.covers("Bash", Some("cargo test --all")));
        assert!(!rule.covers("Bash", Some("cargo publish")));
        assert!(!rule.covers("Edit", Some("cargo test")));
    }

    #[test]
    fn a_targeted_rule_does_not_cover_a_call_with_no_target() {
        // The backend could not say what the call acts on, so nothing about it
        // matches what the operator approved.
        assert!(!Rule::targeted("Bash", "cargo test").covers("Bash", None));
    }

    #[test]
    fn a_trailing_star_matches_by_prefix() {
        let rule = Rule::targeted("Bash", "cargo *");

        assert!(rule.covers("Bash", Some("cargo test")));
        assert!(rule.covers("Bash", Some("cargo publish")));
        assert!(!rule.covers("Bash", Some("cargoo")));
    }

    #[test]
    fn a_rule_reads_back_the_way_it_was_written() {
        for text in ["Read", "Bash(cargo test)", "Edit(crates/*)"] {
            let rule = Rule::parse(text).expect("the rule is valid");
            assert_eq!(rule.to_string(), text);
        }

        let targeted = Rule::parse("Bash(cargo test)").expect("valid");
        assert_eq!(targeted.tool_name(), "Bash");
        assert_eq!(targeted.target(), Some("cargo test"));
        assert_eq!(Rule::parse("Read").expect("valid").target(), None);
    }

    #[test]
    fn a_target_with_brackets_of_its_own_survives_being_written_down() {
        // A command is a target, and commands have brackets in them.
        let rule = Rule::parse("Bash(echo (one))").expect("the rule is valid");

        assert_eq!(rule.target(), Some("echo (one)"));
        assert_eq!(rule.to_string(), "Bash(echo (one))");
        assert!(rule.covers("Bash", Some("echo (one)")));
    }

    #[test]
    fn text_that_is_not_a_rule_is_reported_with_what_was_written() {
        for text in ["", "Bash(", "Bash)", "Bash()", "(cargo test)", "Bash(a)b"] {
            let error = Rule::parse(text).expect_err("not a rule");
            assert!(error.to_string().contains("is not a permission rule"));
            assert!(
                error.to_string().contains(text.trim()),
                "the operator's own text is missing from: {error}"
            );
        }
    }

    #[test]
    fn a_rule_survives_the_round_trip_as_the_string_a_config_writes() {
        let rule = Rule::targeted("Bash", "cargo test");
        let line = serde_json::to_string(&rule).expect("a rule");

        assert_eq!(line, r#""Bash(cargo test)""#);
        assert_eq!(
            serde_json::from_str::<Rule>(&line).expect("what was written reads back"),
            rule
        );
    }

    #[test]
    fn an_allowlist_answers_for_whichever_rule_covers_the_call() {
        let list: Allowlist = [Rule::tool("Read"), Rule::targeted("Bash", "cargo test")]
            .into_iter()
            .collect();

        assert!(list.allows("Read", Some("anything")));
        assert!(list.allows("Bash", Some("cargo test")));
        assert!(!list.allows("Bash", Some("rm -rf build")));
        assert!(!list.allows("Write", None));
    }

    #[test]
    fn a_rule_already_covered_does_not_grow_the_list() {
        let mut list = Allowlist::new();

        assert!(list.insert(Rule::targeted("Bash", "cargo test")));
        assert!(!list.insert(Rule::targeted("Bash", "cargo test")));
        assert!(list.insert(Rule::tool("Bash")));
        // The whole tool is allowed now, so a target under it adds nothing.
        assert!(!list.insert(Rule::targeted("Bash", "cargo build")));
        assert_eq!(list.rules().len(), 2);
    }

    #[test]
    fn allowlists_add_up_rather_than_replacing_one_another() {
        let user: Allowlist = [Rule::tool("Read")].into_iter().collect();
        let repo: Allowlist = [Rule::targeted("Bash", "cargo test"), Rule::tool("Read")]
            .into_iter()
            .collect();

        let merged = user.merge(repo);

        assert_eq!(
            merged.rules(),
            [Rule::tool("Read"), Rule::targeted("Bash", "cargo test")],
            "a repository's file took back a permission the operator granted"
        );
    }

    #[test]
    fn a_list_with_no_rules_asks_about_everything() {
        let list = Allowlist::new();

        assert!(list.is_empty());
        assert!(!list.allows("Read", Some("/repo/notes.txt")));
    }
}
