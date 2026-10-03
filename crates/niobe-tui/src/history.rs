// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The prompts and sessions the operator can go back to.
//!
//! The shell reads no store, so the binary hands it something that reads the
//! repository's earlier sessions, the way [`crate::images`] is handed
//! something that reads the clipboard. Listing the `claude` CLI's own sessions
//! means reading every transcript it keeps for the repository, which takes
//! long enough to be seen, so a load is asked for and its outcome arrives
//! later through [`History::drain`]; the loop never waits on one.
//!
//! # How history is reached
//!
//! * Up on the composer's first row recalls the prompt before the one shown,
//!   and Down on its last row the one after, as a shell and Claude Code do.
//!   Down past the newest gives back the draft that was being written.
//!   Editing the prompt shown ends the walk, so the arrows move through the
//!   edit rather than replacing it; a draft the walk set aside and did not
//!   give back comes back to the composer, with its images, once a prompt is
//!   sent.
//! * [`SEARCH_KEY`] opens the history dialog on its prompts: typed words
//!   filter it, and Enter puts the chosen prompt in the composer unsent, as
//!   a step of a walk that set the draft aside and goes on from it.
//! * Tab in the dialog turns it to the sessions, where Enter ends this one and
//!   opens the chosen one in its place.

use std::collections::HashSet;

use crate::clock::Stamp;

/// The key that opens the history dialog, as the help names it.
pub const SEARCH_KEY: &str = "Ctrl+R";

/// A session the shell can be opened on in place of the one it holds.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// One Niobe recorded, by the number `niobe sessions` prints.
    Recorded(String),
    /// One the `claude` CLI recorded for itself, by its own id, which
    /// opening reads in as a session of Niobe's own.
    Claude(String),
}

/// How many characters of a `claude` session's id name it in a list: enough
/// to tell two apart, as a short git hash is.
const CLAUDE_ID_CHARS: usize = 8;

impl Target {
    /// What a list calls the session: `#12`, or `claude 2f6c1e10`.
    pub fn label(&self) -> String {
        match self {
            Self::Recorded(id) => format!("#{id}"),
            Self::Claude(id) => format!(
                "claude {}",
                id.chars().take(CLAUDE_ID_CHARS).collect::<String>()
            ),
        }
    }
}

/// A prompt an earlier session sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastPrompt {
    /// What was sent, as it was typed.
    pub text: String,
    /// When it was recorded.
    pub at: Option<Stamp>,
    /// The session it was sent in.
    pub session: Target,
}

/// A session the repository holds a record of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastSession {
    /// Which it is.
    pub target: Target,
    /// When anything last happened in it, where that is known.
    pub last: Option<Stamp>,
    /// The first thing it was asked. A `claude` session's other prompts are
    /// not read until it is opened, so this is all a list can say of it.
    pub first_prompt: Option<String>,
}

/// Everything a load found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Past {
    /// Every prompt of every session in the store, newest first.
    pub prompts: Vec<PastPrompt>,
    /// Every session that can be opened, newest first.
    pub sessions: Vec<PastSession>,
    /// What could not be read, in words the operator reads: the rest is
    /// listed without it rather than not at all.
    pub unread: Option<String>,
}

/// Something that reads the repository's earlier sessions for the shell.
pub trait History: std::fmt::Debug {
    /// Starts reading them again. Returns at once; what was read arrives
    /// through [`History::drain`]. A load asked for while one is under way is
    /// that one.
    fn load(&mut self);

    /// What the newest load that has finished found, once. Never blocks.
    fn drain(&mut self) -> Option<Past>;
}

/// No earlier sessions to read: a recorded log being looked at rather than
/// continued. Every load finds nothing, at once.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHistory {
    asked: bool,
}

impl History for NoHistory {
    fn load(&mut self) {
        self.asked = true;
    }

    fn drain(&mut self) -> Option<Past> {
        std::mem::take(&mut self.asked).then(Past::default)
    }
}

/// Whether `text` holds every word of `query`, in any case. An empty query
/// holds none, so it matches everything.
pub fn matches(query: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| text.contains(word))
}

/// `prompts` with each text kept once, where it first appears: given newest
/// first, a prompt sent again is recalled as the newer of the two.
pub fn unique<'a>(prompts: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut seen = HashSet::new();
    prompts
        .into_iter()
        .filter(|prompt| !prompt.trim().is_empty() && seen.insert(*prompt))
        .collect()
}

/// A walk through earlier prompts with Up and Down, and the draft it set
/// aside to start.
///
/// The prompts are taken when the walk starts, so a load that lands during it
/// does not move the prompt the operator is looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recall {
    prompts: Vec<String>,
    at: usize,
    draft: String,
}

/// Where a step newer than the newest prompt leaves the composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Newer<'a> {
    /// On the next newer prompt.
    Prompt(&'a str),
    /// Back on the draft the walk set aside, which ends the walk.
    Draft(String),
}

impl Recall {
    /// Starts a walk on the newest of `prompts`, newest first, setting `draft`
    /// aside. `None` where there is nothing to recall.
    pub fn start(prompts: Vec<String>, draft: String) -> Option<Self> {
        Self::start_on(prompts, 0, draft)
    }

    /// Starts a walk on the prompt at `at` of `prompts`, newest first, setting
    /// `draft` aside: where a prompt chosen from the history dialog sits, so
    /// the arrows go on from it. `None` where `prompts` has no such prompt.
    pub fn start_on(prompts: Vec<String>, at: usize, draft: String) -> Option<Self> {
        (at < prompts.len()).then_some(Self { prompts, at, draft })
    }

    /// Ends the walk anywhere but past the newest, handing back the draft it
    /// set aside.
    pub fn into_draft(self) -> String {
        self.draft
    }

    /// The prompt the walk is on.
    pub fn current(&self) -> &str {
        self.prompts.get(self.at).map_or("", String::as_str)
    }

    /// Steps to the prompt before the one shown. `None` at the oldest, where
    /// the walk stays.
    pub fn older(&mut self) -> Option<&str> {
        let next = self.at.saturating_add(1);
        let prompt = self.prompts.get(next)?;
        self.at = next;
        Some(prompt)
    }

    /// Steps to the prompt after the one shown, or past the newest to the
    /// draft.
    pub fn newer(&mut self) -> Newer<'_> {
        match self.at.checked_sub(1) {
            Some(at) => {
                self.at = at;
                Newer::Prompt(self.current())
            }
            None => Newer::Draft(std::mem::take(&mut self.draft)),
        }
    }
}

/// What the history dialog shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Every prompt sent, this session's and earlier ones'.
    Prompts,
    /// Every session there is to open.
    Sessions,
}

impl View {
    /// The other one, which Tab turns the dialog to.
    pub fn other(self) -> Self {
        match self {
            Self::Prompts => Self::Sessions,
            Self::Sessions => Self::Prompts,
        }
    }
}

/// The history dialog, while it is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Browser {
    /// What it shows.
    pub view: View,
    /// What has been typed to filter it.
    pub query: String,
    /// The row the cursor is on, among the ones the filter keeps.
    pub at: usize,
}

impl Browser {
    /// The dialog open on `view`, unfiltered, on its first row.
    pub fn new(view: View) -> Self {
        Self {
            view,
            query: String::new(),
            at: 0,
        }
    }
}

/// One row of the dialog's prompts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptRow {
    /// What was sent.
    pub text: String,
    /// When.
    pub at: Option<Stamp>,
    /// The session it was sent in; `None` for this one.
    pub session: Option<Target>,
}

/// One row of the dialog's sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// Which it is; `None` for the session open now.
    pub target: Option<Target>,
    /// When anything last happened in it.
    pub last: Option<Stamp>,
    /// Its prompts, oldest first, where they are known.
    pub prompts: Vec<String>,
    /// What it was first asked, where its prompts are not known.
    pub first_prompt: Option<String>,
}

impl SessionRow {
    /// What the row is named by: its first prompt.
    pub fn first(&self) -> Option<&str> {
        self.prompts
            .first()
            .or(self.first_prompt.as_ref())
            .map(String::as_str)
    }

    /// Whether the filter keeps it: every word in one of its prompts, or in
    /// the name it is listed under.
    pub fn matches(&self, query: &str) -> bool {
        let label = self
            .target
            .as_ref()
            .map_or_else(|| "this session".to_owned(), Target::label);
        matches(query, &label)
            || self.first().is_some_and(|first| matches(query, first))
            || self.prompts.iter().any(|prompt| matches(query, prompt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(prompts: &[&str], draft: &str) -> Recall {
        Recall::start(
            prompts.iter().map(|prompt| (*prompt).to_owned()).collect(),
            draft.to_owned(),
        )
        .expect("there are prompts to walk")
    }

    #[test]
    fn a_walk_starts_on_the_newest_and_stops_at_the_oldest() {
        let mut recall = walk(&["third", "second", "first"], "");

        assert_eq!(recall.current(), "third");
        assert_eq!(recall.older(), Some("second"));
        assert_eq!(recall.older(), Some("first"));
        assert_eq!(recall.older(), None);
        assert_eq!(recall.current(), "first");
    }

    #[test]
    fn stepping_back_past_the_newest_gives_the_draft_back_whole() {
        let mut recall = walk(&["second", "first"], "half a\nthought");
        recall.older();

        assert_eq!(recall.newer(), Newer::Prompt("second"));
        assert_eq!(recall.newer(), Newer::Draft("half a\nthought".to_owned()));
    }

    #[test]
    fn nothing_to_recall_starts_no_walk() {
        assert_eq!(Recall::start(Vec::new(), "draft".to_owned()), None);
    }

    #[test]
    fn a_walk_started_on_a_chosen_prompt_steps_on_from_it() {
        let prompts = ["third", "second", "first"].map(str::to_owned).to_vec();
        let mut recall =
            Recall::start_on(prompts, 1, "draft".to_owned()).expect("the walk has a second");

        assert_eq!(recall.current(), "second");
        assert_eq!(recall.newer(), Newer::Prompt("third"));
        assert_eq!(recall.newer(), Newer::Draft("draft".to_owned()));
    }

    #[test]
    fn a_walk_is_not_started_past_the_oldest_prompt() {
        let prompts = vec!["only".to_owned()];
        assert_eq!(Recall::start_on(prompts, 1, "draft".to_owned()), None);
    }

    #[test]
    fn a_prompt_sent_twice_is_recalled_once_where_it_was_newest() {
        assert_eq!(
            unique(["fix it", "run the tests", "fix it", " ", "add a test"]),
            ["fix it", "run the tests", "add a test"]
        );
    }

    #[test]
    fn every_word_of_the_query_has_to_be_there_in_any_case() {
        assert!(matches("", "anything"));
        assert!(matches("RESUME race", "fix the race in the resume test"));
        assert!(!matches(
            "resume deadlock",
            "fix the race in the resume test"
        ));
    }

    #[test]
    fn a_session_is_found_by_any_of_its_prompts_or_its_name() {
        let row = SessionRow {
            target: Some(Target::Recorded("12".to_owned())),
            last: None,
            prompts: vec!["add etag support".to_owned(), "and a 304 test".to_owned()],
            first_prompt: None,
        };

        assert!(row.matches("304"));
        assert!(row.matches("#12"));
        assert!(!row.matches("deadlock"));
        assert_eq!(row.first(), Some("add etag support"));
    }

    #[test]
    fn a_claude_session_is_named_by_the_start_of_its_id() {
        assert_eq!(
            Target::Claude("2f6c1e10-8f4b-4d2a".to_owned()).label(),
            "claude 2f6c1e10"
        );
        assert_eq!(Target::Recorded("7".to_owned()).label(), "#7");
    }

    #[test]
    fn with_nothing_to_read_a_load_finds_nothing_once() {
        let mut history = NoHistory::default();
        assert_eq!(history.drain(), None);
        history.load();
        assert_eq!(history.drain(), Some(Past::default()));
        assert_eq!(history.drain(), None);
    }
}
