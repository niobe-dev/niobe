// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The short tag a sub-agent is named by on every row it shares with what it
//! did — `data`, `web`, `explore2` — and the task its label says it was
//! spawned for.
//!
//! A session running nine agents interleaves their calls row by row, and the
//! eye tells two rows apart by a word it can find in a column of its own, not
//! by reading a sentence cut off at the pane's middle. So each agent gets one
//! word, no wider than [`TAG_MAX`], taken from the kind of agent it was asked
//! to be: the first word of it that no other kind shares. `up-data-engineer`
//! beside `up-web-engineer` is `data`, and `web`; the words the two share say
//! nothing about which is which. The Activity pane lists every tag beside the
//! agent's task, which is where a tag is read back to what it names.

use crate::text;

/// The widest a tag is drawn.
pub(crate) const TAG_MAX: usize = 8;

/// What an agent is called when its kind and label have no word in them.
const NO_WORD: &str = "agent";

/// A sub-agent as a tag is worked out from: the kind of agent it was asked to
/// be, where the backend said, and what it was spawned to do.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Named<'a> {
    pub(crate) kind: Option<&'a str>,
    pub(crate) label: &'a str,
}

/// Every agent's tag, in the order given.
///
/// Two agents whose tags would read alike — two of one kind, or two kinds cut
/// to the same letters — are numbered in the order they were spawned,
/// `explore1` and `explore2`, because a tag that names two agents names
/// neither. The tags are worked out afresh from the whole list, so a second
/// agent of a kind renumbers the first.
pub(crate) fn tags(agents: &[Named<'_>]) -> Vec<String> {
    let words: Vec<Vec<String>> = agents.iter().map(|agent| words(base(agent))).collect();
    let stems: Vec<String> = (0..agents.len())
        .map(|at| stem(agents, &words, at))
        .collect();
    let cut: Vec<String> = stems
        .iter()
        .map(|stem| text::truncate(stem, TAG_MAX))
        .collect();
    (0..agents.len())
        .map(|at| {
            let alike: Vec<usize> = (0..agents.len())
                .filter(|&other| cut[other] == cut[at])
                .collect();
            match alike.iter().position(|&other| other == at) {
                Some(place) if alike.len() > 1 => numbered(&stems[at], place + 1),
                _ => cut[at].clone(),
            }
        })
        .collect()
}

/// What an agent was spawned to do, without the kind a label written before
/// the kind was kept apart opens with.
pub(crate) fn task<'a>(agent: Named<'a>) -> &'a str {
    agent
        .kind
        .and_then(|kind| agent.label.strip_prefix(kind))
        .and_then(|rest| rest.strip_prefix(": "))
        .filter(|rest| !rest.trim().is_empty())
        .unwrap_or(agent.label)
}

/// What an agent's tag is taken from: its kind, or its label where the
/// backend named no kind.
fn base<'a>(agent: &Named<'a>) -> &'a str {
    agent.kind.unwrap_or(agent.label)
}

/// The words of `text`, lower-cased: runs of letters and digits.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The word agent `at` is tagged with: the first of its words no agent of
/// another kind shares, or its first word where every one is shared.
///
/// Agents of the same kind do not count against one another's words — that
/// would leave two `Explore` agents with no word at all — and are told apart
/// by number instead.
fn stem(agents: &[Named<'_>], words: &[Vec<String>], at: usize) -> String {
    let own = base(&agents[at]);
    let shared = |word: &String| {
        (0..agents.len())
            .any(|other| other != at && base(&agents[other]) != own && words[other].contains(word))
    };
    words[at]
        .iter()
        .find(|word| !shared(word))
        .or_else(|| words[at].first())
        .cloned()
        .unwrap_or_else(|| NO_WORD.to_owned())
}

/// `stem` with `number` after it, cut so the two fit in [`TAG_MAX`].
fn numbered(stem: &str, number: usize) -> String {
    let number = number.to_string();
    let room = TAG_MAX.saturating_sub(number.len());
    format!("{}{number}", text::truncate(stem, room))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(kinds: &[&str]) -> Vec<String> {
        let named: Vec<Named<'_>> = kinds
            .iter()
            .map(|kind| Named {
                kind: Some(kind),
                label: "a task",
            })
            .collect();
        tags(&named)
    }

    #[test]
    fn each_agent_is_tagged_by_the_first_word_of_its_kind_no_other_kind_shares() {
        assert_eq!(
            kinds(&[
                "general-purpose",
                "up-data-engineer",
                "up-web-engineer",
                "up-security-engineer",
                "up-qa-engineer",
            ]),
            ["general", "data", "web", "security", "qa"]
        );
    }

    #[test]
    fn a_tag_is_never_wider_than_its_column() {
        let tagged = kinds(&[
            "up-methodologist",
            "up-typescript-engineer",
            "up-web-engineer",
        ]);
        assert_eq!(tagged, ["methodo…", "typescr…", "web"]);
        assert!(tagged.iter().all(|tag| text::width(tag) <= TAG_MAX));
    }

    #[test]
    fn two_agents_of_one_kind_are_numbered_in_spawn_order() {
        assert_eq!(
            kinds(&["Explore", "Explore", "general-purpose", "Explore"]),
            ["explore1", "explore2", "general", "explore3"]
        );
        assert_eq!(
            kinds(&["methodologist", "methodologist"]),
            ["method…1", "method…2"],
            "the number is kept and the word gives way"
        );
    }

    #[test]
    fn two_kinds_cut_to_the_same_letters_are_numbered_too() {
        assert_eq!(
            kinds(&["typescripter", "typescripting"]),
            ["typesc…1", "typesc…2"]
        );
    }

    /// A record written before the kind was kept apart has it at the front of
    /// its label, and the words the labels share still give way.
    #[test]
    fn an_agent_with_no_kind_is_tagged_from_its_label() {
        let named = [
            Named {
                kind: None,
                label: "deep-reasoner: Review catalog/fetch.py",
            },
            Named {
                kind: None,
                label: "deep-reasoner: Review catalog/cache.py",
            },
            Named {
                kind: None,
                label: "",
            },
        ];
        assert_eq!(tags(&named), ["fetch", "cache", "agent"]);
    }

    #[test]
    fn the_task_is_the_label_without_the_kind_it_opens_with() {
        let task_of = |kind, label| task(Named { kind, label });
        assert_eq!(
            task_of(Some("general-purpose"), "general-purpose: QA: audit"),
            "QA: audit"
        );
        assert_eq!(task_of(Some("Explore"), "Find the loop"), "Find the loop");
        assert_eq!(
            task_of(None, "deep-reasoner: Review"),
            "deep-reasoner: Review"
        );
        assert_eq!(task_of(Some("Explore"), "Explore"), "Explore");
    }
}
