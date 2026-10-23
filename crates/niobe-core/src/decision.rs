// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The decisions an agent says it made, read out of its own replies.
//!
//! No backend's stream has a record of a decision: what an agent chose, and
//! what it left out, is in its words or nowhere. So Niobe asks for it — the
//! binary appends a request to the backend's system prompt that names
//! [`MARKER`] — and reads every line of a finished reply that opens with it.
//!
//! What comes out is the agent's own account of its reasoning, which it may
//! have wrong. Everything that shows a decision says so; nothing here or
//! downstream counts one as a fact about the code.
//!
//! A line inside a fenced code block is code, not a statement, and is never
//! read: a reply that shows a log or a script holding the marker has decided
//! nothing by it. A marker the model dressed in markdown — a list bullet, or
//! bold around the word — is read past, since the request cannot stop a model
//! from formatting it.

/// The word a reply's line opens with to state a decision, colon included.
pub const MARKER: &str = "DECISION:";

/// The longest decision kept, in characters. A line longer than this is a
/// paragraph the model ran on into, and the rest of it is cut with `…` rather
/// than carried whole into a pane row.
pub const LONGEST: usize = 300;

/// Every decision `reply` states, in the order it states them.
pub fn decisions(reply: &str) -> Vec<String> {
    let mut fence: Option<&str> = None;
    let mut found = Vec::new();
    for line in reply.lines() {
        let trimmed = line.trim();
        match fence {
            Some(open) if trimmed.starts_with(open) => fence = None,
            Some(_) => {}
            None if trimmed.starts_with("```") => fence = Some("```"),
            None if trimmed.starts_with("~~~") => fence = Some("~~~"),
            None => found.extend(stated(trimmed)),
        }
    }
    found
}

/// The decision `line` states, where it opens with the marker.
fn stated(line: &str) -> Option<String> {
    let line = ["- ", "* ", "+ ", "> "]
        .iter()
        .find_map(|bullet| line.strip_prefix(bullet))
        .unwrap_or(line)
        .trim_start();
    let rest = line
        .strip_prefix(MARKER)
        .or_else(|| line.strip_prefix("**DECISION:**"))
        .or_else(|| line.strip_prefix("**DECISION**:"))?;
    let summary = rest.trim();
    match summary.is_empty() {
        true => None,
        false => Some(bounded(summary)),
    }
}

/// `summary` cut to [`LONGEST`] characters, marked where it was cut.
fn bounded(summary: &str) -> String {
    match summary.char_indices().nth(LONGEST) {
        Some((at, _)) => format!("{}…", summary[..at].trim_end()),
        None => summary.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_opening_with_the_marker_is_a_decision_and_the_rest_of_the_reply_is_not() {
        let reply = "Fixed the parser.\n\
                     DECISION: left the CSV exporter alone; it is out of scope.\n\
                     The tests pass.";

        assert_eq!(
            decisions(reply),
            ["left the CSV exporter alone; it is out of scope."]
        );
    }

    #[test]
    fn several_decisions_are_read_in_the_order_the_reply_states_them() {
        let reply = "DECISION: reused the cache\nwork\n  DECISION:  kept the old flag  \n";

        assert_eq!(decisions(reply), ["reused the cache", "kept the old flag"]);
    }

    #[test]
    fn a_marker_inside_a_fenced_block_is_code_and_not_a_decision() {
        let reply = "Here is the log:\n```\nDECISION: from a fixture\n```\n\
                     ~~~text\nDECISION: also quoted\n~~~\nDECISION: the real one";

        assert_eq!(decisions(reply), ["the real one"]);
    }

    #[test]
    fn a_marker_the_model_put_in_a_bullet_or_in_bold_is_still_read() {
        let reply = "- DECISION: one\n* **DECISION:** two\n> **DECISION**: three";

        assert_eq!(decisions(reply), ["one", "two", "three"]);
    }

    #[test]
    fn the_marker_mid_line_in_another_case_or_with_nothing_after_it_states_nothing() {
        let reply =
            "We reached a DECISION: none.\nDecision: lower case\nDECISION:   \nDECISIONS: plural";

        assert!(decisions(reply).is_empty());
    }

    #[test]
    fn a_run_on_decision_is_cut_at_the_longest_kept_and_says_so() {
        let long = "é".repeat(LONGEST + 50);
        let found = decisions(&format!("DECISION: {long}"));

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].chars().count(), LONGEST + 1);
        assert!(found[0].ends_with('…'));
    }
}
