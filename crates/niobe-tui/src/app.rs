// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What the shell knows: the transcript, the composer, the scroll position, and
//! the session fold every pane reads its numbers from.
//!
//! [`App`] holds no backend handle and no wire format. Events arrive through
//! [`App::apply`] and are the only way session content gets in, which is what
//! lets the same shell be driven by a bridge, by a recorded log or by a test.
//!
//! Events the operator produces in the shell go the other way: they are folded
//! in like any other and queued for [`App::take_produced`], which the event
//! loop drains into a [`crate::journal::Journal`] so a restart can show them
//! again. The transcript's notices are the shell talking, not the session, and
//! are neither queued nor shown again after a restart.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use niobe_core::diff::Hunk;
use niobe_core::event::{
    AgentId, AgentOutcome, Backend, Event, Mode, PermissionDecision, SlashCommand, ToolCallId,
    ToolOutcome, UsageWindow,
};
use niobe_core::permission::{Allowlist, Rule};
use niobe_core::session::{SessionState, TestRunRecord};
use ratatui_textarea::{Input, TextArea, WrapMode};
use unicode_segmentation::UnicodeSegmentation;

use ratatui::style::Style;

use crate::clock::{Clock, LocalTime, Stamp};
use crate::history::{Browser, Newer, PromptRow, Recall, SessionRow, Target, View};
use crate::images::{Attachments, Fetched, Source, image_path};
use crate::prices::Prices;
use crate::theme::{Depth, Theme};
use crate::trust;

/// Where the session is running, for the pane title and the Changes pane.
///
/// Filled in by the caller: reading the working directory and the repository is
/// the CLI's job, not the TUI's. What the shell is handed first is the name and
/// the branch alone; the rest arrives through [`crate::watch::Watch`] as reads
/// of the repository finish, and a field nobody could read stays as it is here
/// rather than becoming a zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Repo {
    /// The directory name the session was started in.
    pub name: String,
    /// The checked-out branch, where there is one.
    pub branch: Option<String>,
    /// Whether a read of the repository has finished.
    ///
    /// Until one has, the working tree below is empty because
    /// nobody has looked, which is not the same as the repository having
    /// nothing to report — and a `+0 −0` standing in for the difference would
    /// be the pane's first invented figure. The name and the branch are read
    /// without a subprocess and are there from the start.
    pub read: bool,
    /// Commits on the branch that its upstream does not have. `None` where the
    /// branch has no upstream to be ahead of: a branch nobody pushes is not
    /// zero commits ahead, it is not ahead of anything.
    pub ahead: Option<u32>,
    /// Commits the upstream has that the branch does not, on the same terms as
    /// [`Repo::ahead`].
    pub behind: Option<u32>,
    /// What the working tree has changed against the last commit, one entry per
    /// file, in the order the repository reported them.
    pub working: Vec<WorkingFile>,
    /// Every file under the directory the session runs in that the repository
    /// has or would take — committed, staged, or new and not ignored — by the
    /// path the agent names it by, relative to that directory. What `@`
    /// completes a file from.
    pub files: Vec<String>,
}

/// One file the working tree has changed, measured from the repository.
///
/// These are not the per-file counts the session's own fold carries: those are
/// summed from what the backend's edit tools reported and are a floor when a
/// tool did not say. These are what the repository itself reports about the
/// whole tree, including changes no tool of this session made.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkingFile {
    /// The path, as the repository gives it: relative to the repository root,
    /// and `old => new` for a file the repository sees as renamed.
    pub path: String,
    /// Lines added. `None` for a file the repository counts no lines in, which
    /// is what it says about a binary one.
    pub added: Option<u64>,
    /// Lines removed, on the same terms as [`WorkingFile::added`].
    pub removed: Option<u64>,
    /// Whether it is a file the repository does not track yet. Its lines are
    /// counted the way git counts a file once it is added, or not at all
    /// where it was too large to read; it removed none.
    pub new: bool,
}

/// A pane of the right-hand stack that scrolls and holds folding sections.
///
/// Which pane a section belongs to decides which scroll offset folding it
/// resets and which pane a wheel notch moves, so it is written down once here
/// rather than inferred at each call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// What the repository has not committed yet: the branch and the working
    /// tree.
    Changes,
    /// What the session is doing: its sub-agents and tools.
    Activity,
}

/// Which pane has the keyboard: the one the scroll keys and the wheel move.
///
/// Exactly one pane has it at a time. Typing is not something focus decides —
/// a key the focused pane does not take goes to the composer wherever the
/// focus is, and takes the focus back to the session with it, so that the
/// Enter that follows sends what was typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The session pane: the transcript scrolls, and the composer is right
    /// under it.
    Session,
    /// One of the right-hand panes: it scrolls, and its section headers take
    /// the arrows and Enter.
    Pane(Pane),
}

impl Focus {
    /// The order Tab walks the panes in: down the screen, left column first,
    /// and back round to the session.
    const ORDER: [Focus; 3] = [
        Focus::Session,
        Focus::Pane(Pane::Changes),
        Focus::Pane(Pane::Activity),
    ];
}

/// A section of the Changes or Activity pane, which folds on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    /// What the repository says the working tree has changed: every
    /// uncommitted change, whoever made it.
    WorkingTree,
    /// The sub-agents the session spawned, running and finished.
    SubAgents,
    /// The tools it called, and how often.
    Tools,
}

impl Section {
    /// Every section, in the order the panes draw them.
    pub const ALL: [Section; 3] = [Section::WorkingTree, Section::SubAgents, Section::Tools];

    /// The pane this section is drawn in.
    pub fn pane(self) -> Pane {
        match self {
            Section::WorkingTree => Pane::Changes,
            Section::SubAgents | Section::Tools => Pane::Activity,
        }
    }
}

/// One scrolling pane's own offset and what the last frame measured it as.
///
/// Each pane scrolls independently of the transcript and of the other, so each
/// keeps its own offset; `area` is where it was drawn last frame, which is how
/// a wheel notch can tell which pane the pointer was over, and `None` when it
/// was not drawn at all, which is how focus knows to pass it by.
#[derive(Debug, Clone, Default)]
struct Scroller {
    scroll: usize,
    content: usize,
    viewport: usize,
    area: Option<ratatui::layout::Rect>,
    /// Each section header the last frame drew, and the row it is on.
    headers: Vec<(Section, usize)>,
    /// The section the arrows last moved to, while the pane has had focus.
    cursor: Option<Section>,
}

impl Scroller {
    fn max_scroll(&self) -> usize {
        self.content.saturating_sub(self.viewport)
    }

    /// What the last frame drew this pane as: where it is, how many rows it
    /// had to show and how many it could.
    fn measured(&mut self, area: ratatui::layout::Rect, content: usize, rows: usize) {
        self.area = Some(area);
        self.content = content;
        self.viewport = rows;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// Scrolls the pane, never past either end.
    fn scroll_by(&mut self, lines: isize) {
        let max = self.max_scroll();
        let at = self.scroll.min(max);
        self.scroll = match lines < 0 {
            true => at.saturating_sub(lines.unsigned_abs()),
            false => at.saturating_add(lines.unsigned_abs()).min(max),
        };
    }

    fn holds(&self, column: u16, row: u16) -> bool {
        self.area
            .is_some_and(|area| area.contains((column, row).into()))
    }

    /// The section under the cursor, where the last frame drew it: the one
    /// the arrows moved to, or the first when that one is no longer drawn.
    fn cursor(&self) -> Option<(usize, Section, usize)> {
        let at = self
            .headers
            .iter()
            .position(|(section, _)| Some(*section) == self.cursor)
            .unwrap_or(0);
        self.headers
            .get(at)
            .map(|(section, row)| (at, *section, *row))
    }

    /// Moves the cursor to the next section down, or up, stopping at either
    /// end, and scrolls the pane so that its header is in view.
    fn move_cursor(&mut self, down: bool) {
        let Some((at, _, _)) = self.cursor() else {
            return;
        };
        let to = match down {
            true => (at + 1).min(self.headers.len().saturating_sub(1)),
            false => at.saturating_sub(1),
        };
        if let Some((section, row)) = self.headers.get(to).copied() {
            self.cursor = Some(section);
            self.reveal(row);
        }
    }

    /// Scrolls just far enough that `row` is in view.
    fn reveal(&mut self, row: usize) {
        let viewport = self.viewport.max(1);
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll.saturating_add(viewport) {
            self.scroll = (row + 1).saturating_sub(viewport);
        }
        self.scroll = self.scroll.min(self.max_scroll());
    }
}

/// The profile the operator selected, for the menu row to name until a
/// backend reports what the session is running.
///
/// Plain data, filled in by the caller: the config is read by the CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedProfile {
    /// The profile's name, as the config spells it.
    pub name: String,
    /// The backend the profile runs.
    pub backend: Backend,
    /// The models the profile offers, in the order it names them. Empty where
    /// it names none, and the shell then has nothing to offer: a model id
    /// invented here would be one the backend never heard of.
    pub models: Vec<String>,
}

/// A permission prompt the shell is waiting on the operator to answer.
///
/// What the backend said, and nothing derived: the question shows the tool,
/// what it would run on and the whole of its arguments, because approving a
/// call whose arguments were summarised away is approving something else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    /// The call being gated, which the answer is addressed by.
    pub id: ToolCallId,
    /// Tool name, as the backend spells it.
    pub tool: String,
    /// The whole of the arguments, as the backend rendered them.
    pub input: String,
    /// The one thing the call acts on, where the backend could name it. A
    /// prompt without one can only be answered for the whole tool.
    pub target: Option<String>,
    /// The turn the call belongs to, counted in prompts sent this session,
    /// which is what the question says it is blocking. Zero where the session
    /// has no prompt on record, as one attached to a turn already running.
    pub turn: u64,
    /// The sub-agent whose call this is, or `None` for the session's own.
    /// Who asked is said on the question and nowhere in the answer: a
    /// standing answer is about the tool and its target, whoever asked.
    pub agent: Option<AgentId>,
}

impl Ask {
    /// Whether this prompt can be given `answer`. A standing answer about the
    /// target needs a target to be about, and a standing answer is offered
    /// only where its rule can be written into a config and read back.
    pub fn offers(&self, answer: Answer) -> bool {
        match answer {
            Answer::Once | Answer::No => true,
            Answer::AlwaysTool => self.tool_rule().is_some(),
            Answer::AlwaysTarget => self.target_rule().is_some(),
        }
    }

    /// The answers this prompt offers, in the order they are numbered.
    pub fn options(&self) -> Vec<Answer> {
        Answer::ALL
            .into_iter()
            .filter(|answer| self.offers(*answer))
            .collect()
    }

    /// The standing rule "always this tool", where its name can be written
    /// down and read back as itself.
    pub fn tool_rule(&self) -> Option<Rule> {
        Some(Rule::tool(self.tool.clone())).filter(Rule::reads_back)
    }

    /// The standing rule "always this tool, on this target", where the prompt
    /// has a target to write one about and the rule reads back as itself —
    /// not for an empty target, which would be written as no rule at all.
    pub fn target_rule(&self) -> Option<Rule> {
        self.target
            .as_ref()
            .map(|target| Rule::targeted(self.tool.clone(), target.clone()))
            .filter(Rule::reads_back)
    }
}

/// What the operator chose in answer to a permission prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Allowed, this call only.
    Once,
    /// Allowed, and every call to this tool from now on.
    AlwaysTool,
    /// Allowed, and every call to this tool on this target from now on.
    AlwaysTarget,
    /// Refused.
    No,
}

impl Answer {
    /// Every answer, in the order a prompt numbers the ones it offers.
    pub const ALL: [Answer; 4] = [
        Answer::Once,
        Answer::AlwaysTool,
        Answer::AlwaysTarget,
        Answer::No,
    ];
}

/// Where the keyboard is while a prompt is waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskFocus {
    /// On the numbered answers.
    Choosing,
    /// On an answer the operator is writing in place of the numbered ones.
    Writing,
    /// Put off with Esc: the question stays in the transcript unanswered, the
    /// turn stays waiting on it, and the keyboard is the shell's until Esc
    /// brings the operator back.
    Deferred,
}

/// A list the operator is choosing one of: what it is for, what it offers and
/// where the cursor is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    /// What choosing does.
    pub purpose: Purpose,
    /// What is offered, in the order it is drawn.
    pub options: Vec<String>,
    /// Which one the cursor is on.
    pub at: usize,
}

/// What a [`Picker`] chooses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// The model the session runs on. The list is the profile's, in the order
    /// it names them: the shell knows no backend and so knows no models of its
    /// own, and offering an id the backend would refuse is worse than offering
    /// nothing.
    Model,
    /// How hard the model works on a turn, sent as the backend's `/effort`.
    Effort,
    /// The palette the shell is drawn in.
    Theme,
}

/// The effort levels offered, in the order the `claude` CLI's `/effort`
/// lists them in its argument hint (Claude Code 2.1.287). Its `auto` and
/// `ultracode` are left out: they are switches of their own rather than a
/// level, and a list of levels with them in would read as a scale they are
/// not on.
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Something the shell has to say that takes more than a line: what it is
/// running under, the keys it answers to. It holds the keyboard until it is
/// closed, and changes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sheet {
    /// What it is about, on its top edge.
    pub title: String,
    /// What it says, a row each; a row is wrapped to the sheet's width.
    pub rows: Vec<String>,
    /// What `o` opens, where there is something to open.
    pub link: Option<String>,
    /// How many wrapped lines are scrolled off its top.
    pub scroll: usize,
}

/// Files outside the session the shell can name or have opened, found by the
/// binary: the shell touches no filesystem.
///
/// A file is handed over to be opened only where it is a regular file and not
/// a link. A repository decides what its own files are, and a `CLAUDE.md`
/// linked to something the desktop runs rather than shows would be run by the
/// key that was meant to show it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Places {
    /// The config files the session was read from, lowest precedence first,
    /// whether or not each is there.
    pub config_files: Vec<ConfigFile>,
    /// The repository's instructions to the agent, where it has one that may
    /// be opened.
    pub memory: Option<String>,
}

/// A config file the session looked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFile {
    /// Where it is, or would be.
    pub path: String,
    /// Whether there was a file there to read.
    pub exists: bool,
    /// Whether it may be handed to the desktop to open: a regular file, not
    /// a link.
    pub openable: bool,
}

/// What a transcript entry is, which decides its glyph and its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// The operator.
    User,
    /// The assistant.
    Agent,
    /// A sub-agent the session spawned, speaking: its words are its own
    /// conversation with the session, not the session's reply.
    SubAgent,
    /// A tool call.
    Tool,
    /// An error the backend reported.
    Failure,
    /// The shell itself, explaining something.
    Notice,
    /// The rule drawn where a turn ended, with what the turn spent on it.
    Turn(TurnRule),
}

/// What the rule under a finished turn says about it.
///
/// Every figure is one the session fold or this shell's clock measured, and
/// each is `None` where neither did; the rule leaves an absent figure out
/// rather than standing a zero in for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TurnRule {
    /// Which turn it was, counted from one.
    pub number: u64,
    /// When it ended, by the clock the shell stamped its end with.
    pub ended: Option<LocalTime>,
    /// Every token it spent, cache traffic included. `None` where no usage
    /// was reported across it, which a turn interrupted or failed before its
    /// first message is.
    pub tokens: Option<u64>,
    /// How far the five-hour window moved across it, in whole points of the
    /// window as the Usage pane rounds them. `Some(0)` is a move under one
    /// point, which is not the same as no move at all and is drawn as such.
    pub five_hour_points: Option<u64>,
    /// How long it ran, from the prompt that opened it to its end.
    pub took: Option<Duration>,
    /// How many sub-agents it spawned.
    pub agents: u64,
    /// How many tool calls the transcript shows it making, its sub-agents'
    /// included.
    pub calls: u64,
    /// Whether it was cut off rather than ended by the backend: the session
    /// failed under it, or the process running it stopped.
    pub cut: bool,
}

impl EntryKind {
    /// The glyph in the transcript gutter.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::User => ">",
            Self::Agent => "◆",
            Self::SubAgent => "↳",
            Self::Tool => "⚙",
            Self::Failure => "!",
            Self::Notice => "·",
            Self::Turn(_) => "─",
        }
    }

    /// The colour the head and glyph are drawn in.
    pub fn colour(self, theme: &Theme) -> ratatui::style::Color {
        match self {
            Self::User => theme.user,
            Self::Agent | Self::SubAgent => theme.agent,
            Self::Tool => theme.tool,
            Self::Failure => theme.del,
            Self::Notice | Self::Turn(_) => theme.dim,
        }
    }
}

/// One block in the transcript: a message, a tool call or a notice.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Entry {
    /// Decides the glyph and the colour.
    pub kind: EntryKind,
    /// The bold label: `you`, the backend's name, a tool's name.
    pub head: String,
    /// The dim detail beside the head: what a refused call was about. Empty
    /// on a tool call's own entry, whose figures are its [`Entry::calls`].
    pub meta: String,
    /// The body, wrapped at draw time.
    pub body: String,
    /// Whether more of this entry is still arriving.
    pub streaming: bool,
    /// When the event behind this entry happened: read off the clock as the
    /// shell took it off the channel, or off what the store recorded beside
    /// it. Absent where the entry came from a log that kept no times.
    pub at: Option<Stamp>,
    /// The tool calls this entry is, in the order they started: one for a
    /// call on its own, more for a run of calls to the same tool, which the
    /// transcript folds into one group. Empty for every entry that is not a
    /// tool call.
    ///
    /// A run is calls to the same tool with nothing between them in the
    /// transcript. Anything else the transcript shows breaks it — a call to
    /// another tool, the assistant saying something, a refusal, a notice —
    /// and so does the end of a turn, so two turns' calls are never one
    /// group even where no words came between them. So does a call another
    /// agent made: a group is one agent's.
    pub calls: Vec<Call>,
    /// The sub-agent whose words, calls or refused call this entry is, or
    /// `None` for the session's own and for everything that is not the
    /// agents'.
    ///
    /// Named by its tag: a word of the kind of agent it was asked to be that
    /// no other kind shares, numbered where two agents would read alike —
    /// see [`crate::tags`]. It is worked out again whenever an agent is
    /// spawned, because a new one can take an older one's word.
    pub agent: Option<String>,
}

/// One tool call as the transcript shows it: what it does, how it ended and
/// what it cost.
///
/// Every figure is `None` until the backend or this shell's clock has said
/// it, and stays `None` where neither did. The row draws an absent figure as
/// absent, never as a zero.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Call {
    /// What the call does, in one line: the backend's reading of it, or its
    /// arguments where it had none.
    pub what: String,
    /// How it ended, or `None` while it runs.
    pub outcome: Option<ToolOutcome>,
    /// What it returned, in bytes. `None` while it runs.
    pub bytes: Option<u64>,
    /// The status its command exited with, where the backend reported one.
    pub exit_code: Option<i32>,
    /// Why it did not succeed, in the backend's words, where it said.
    pub error: Option<String>,
    /// The lines it added and removed, where it changed a file: `None` for a
    /// call that changed none, and each side `None` where the backend did not
    /// state it.
    pub lines: Option<(Option<u64>, Option<u64>)>,
    /// How long it ran by this shell's clock, or by the clock the store
    /// recorded it with. `None` while it runs, wherever either end came with
    /// no time — a log that kept none — and where the two ends are closer
    /// than the clock can tell apart ([`TIMED`]).
    pub took: Option<Duration>,
    /// What it did to a file, where the backend reported the lines it
    /// changed. Drawn under the call as a diff; `None` for a call that is not
    /// such a call, and for one whose backend sent only counts.
    pub change: Option<Change>,
    /// The end of what it printed, for a command the operator ran with `!`,
    /// whose output is what they ran it to see. `None` for every call the
    /// agent made: the agent reads its calls' output, and the transcript
    /// shows what the call did rather than repeating it.
    pub printed: Option<Printed>,
    /// What it reported, where it was a test run: its counts where its
    /// output held the whole run, and otherwise that it ran and whether it is
    /// known to have failed. `None` for every call that was not one.
    pub tested: Option<TestRunRecord>,
    /// Whether what the call does runs the tests, read from it as it starts
    /// so the transcript can draw a run in progress as one.
    pub testing: bool,
    /// How long the last run of the same command took in this session, where
    /// it ran to the end and its counts were read: what this run's progress
    /// is drawn against while it runs. A run that stopped short is nothing to
    /// measure by, and the backend reports no progress of its own.
    pub last_run: Option<Duration>,
    /// Whether the session ended while the call was running, so that no end
    /// will arrive for it. It is not running, and it did not fail: the
    /// backend that was running it is gone.
    pub interrupted: bool,
    /// Who let it through, where this shell can say; `None` while it runs
    /// and where it cannot.
    pub gate: Option<Gate>,
    /// When it started running: its start, or the moment it was allowed
    /// where it waited on a question first, so that the time the operator
    /// took to answer is not read as the time the tool took.
    started: Option<Stamp>,
}

impl Call {
    /// A call that has just started, at `at`.
    pub(crate) fn started(what: String, at: Option<Stamp>) -> Self {
        Self {
            testing: niobe_core::test_run::is_test_run(&what),
            what,
            outcome: None,
            bytes: None,
            exit_code: None,
            error: None,
            lines: None,
            took: None,
            change: None,
            printed: None,
            tested: None,
            last_run: None,
            interrupted: false,
            gate: None,
            started: at,
        }
    }

    /// Records how the call ended, at `at`.
    fn ended(&mut self, ending: Ending, at: Option<Stamp>) {
        self.outcome = Some(ending.outcome);
        self.bytes = Some(ending.bytes);
        self.exit_code = ending.exit_code;
        self.error = ending.error;
        self.took = match (self.started, at) {
            (Some(started), Some(at)) => at.since(started).filter(|took| *took >= TIMED),
            _ => None,
        };
    }

    /// When it started running, where the shell had a clock at the time:
    /// what a run in progress is timed from.
    pub fn began(&self) -> Option<Stamp> {
        self.started
    }

    /// Whether the call is still running.
    pub fn running(&self) -> bool {
        self.outcome.is_none() && !self.interrupted
    }

    /// Whether the call ran and did not succeed, or was not allowed to run.
    pub fn failed(&self) -> bool {
        matches!(
            self.outcome,
            Some(ToolOutcome::Failed | ToolOutcome::Denied)
        )
    }
}

/// The last lines a command the operator ran printed, as the transcript shows
/// them under the call.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Printed {
    /// How many lines it printed above the ones kept.
    pub above: usize,
    /// The last lines, as they would read on a terminal: a line a carriage
    /// return rewrote is what it was rewritten to, and nothing that would
    /// move a terminal's cursor or change its colours is left in.
    pub tail: Vec<String>,
}

impl Printed {
    /// How many of a command's last lines the transcript keeps under it: a
    /// glance at how it ended, not a scrollback. The whole output is in the
    /// session store.
    const LINES: usize = 12;

    /// The end of `output`.
    fn of(output: &str) -> Self {
        let output = without_sequences(output);
        let lines: Vec<&str> = output.lines().collect();
        let above = lines.len().saturating_sub(Self::LINES);
        let tail = lines
            .iter()
            .skip(above)
            .map(|line| terminal_line(line))
            .collect();
        Self { above, tail }
    }
}

/// `output` with its terminal sequences taken out whole: colours and cursor
/// moves (`ESC [ … <final>`) and titles and links (`ESC ] … BEL` or `ESC ]
/// … ESC \\`). Dropping only the escape byte, as a filter of control
/// characters does, would leave the rest of each sequence drawn as text.
fn without_sequences(output: &str) -> std::borrow::Cow<'_, str> {
    if !output.contains('\x1b') {
        return std::borrow::Cow::Borrowed(output);
    }
    let mut plain = String::with_capacity(output.len());
    let mut chars = output.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            plain.push(c);
            continue;
        }
        match chars.next() {
            // Parameters and intermediates run up to the final byte.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // A string, ended by BEL or by ESC and a backslash.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            // Any other escape is two characters long.
            Some(_) | None => {}
        }
    }
    std::borrow::Cow::Owned(plain)
}

/// A line of output as a terminal would have left it: each carriage return
/// takes the cursor back to the start of the line and what follows is written
/// over what was there — `abcdef\rXY` reads `XYcdef`, and a progress bar that
/// ends on a bare `\r` keeps its last bar — with tabs as spaces and no control
/// characters.
fn terminal_line(line: &str) -> String {
    let mut shown: Vec<char> = Vec::new();
    for pass in line.split('\r') {
        let written: Vec<char> = crate::text::expand_tabs(pass)
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        for (at, c) in written.into_iter().enumerate() {
            match shown.get_mut(at) {
                Some(cell) => *cell = c,
                None => shown.push(c),
            }
        }
    }
    shown.into_iter().collect()
}

/// The shortest time between a call's start and its end that this shell can
/// tell from no time at all.
///
/// The event loop reads the clock once a tick — a tenth of a second when
/// idle, a thirtieth while a backend is producing — and stamps every event
/// it drains in that tick with the one reading, so a call that started and
/// ended inside a tick has two equal stamps and was not timed. A session read
/// in from another record is the same: its events were recorded as fast as
/// they were read, milliseconds apart, and those milliseconds are the reading
/// and not the call. Under this, a call has no duration rather than one of
/// nearly nothing.
const TIMED: Duration = Duration::from_millis(100);

/// What a call's end reported about it.
struct Ending {
    outcome: ToolOutcome,
    bytes: u64,
    exit_code: Option<i32>,
    error: Option<String>,
}

/// A run of events the journal refused, which the transcript shows as one
/// failure entry.
#[derive(Debug, Clone, Copy)]
struct Unsaved {
    /// Where the failure entry is in the transcript.
    entry: usize,
    /// How many events were refused, the first included.
    count: u64,
}

/// `count` events, in the words a count of them is shown in.
fn events(count: u64) -> String {
    if count == 1 {
        "1 event".to_owned()
    } else {
        format!("{count} events")
    }
}

/// The lines a call changed in a file, and how the call came to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    hunks: Vec<Hunk>,
    gate: Option<Gate>,
    /// The hunks hashed once, when the change arrived. The transcript keys
    /// each entry's drawn lines on a hash of the entry, taken every frame,
    /// and a session of edits would otherwise hash every line of every diff
    /// ten times a second — measured at a millisecond of a 16 ms frame for
    /// fifty diffs.
    fingerprint: u64,
    /// Whether the change is longer than a diff draws unopened, worked out
    /// once for the same reason: only an entry holding such a change is laid
    /// out again when the operator opens the diffs.
    cut: bool,
}

impl Change {
    /// A change of `hunks`, let through as `gate` says.
    pub fn new(hunks: Vec<Hunk>, gate: Option<Gate>) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        hunks.hash(&mut hasher);
        Self {
            cut: crate::hunks::is_cut(&hunks),
            hunks,
            gate,
            fingerprint: hasher.finish(),
        }
    }

    /// Whether the change has more rows than [`crate::hunks::MAX_ROWS`], and
    /// so is drawn cut unless the diffs are open.
    pub fn is_cut(&self) -> bool {
        self.cut
    }

    /// The hunks, as the backend reported them. Never empty for a change a
    /// transcript entry carries.
    pub fn hunks(&self) -> &[Hunk] {
        &self.hunks
    }

    /// Who let the call through, where this shell can say. `None` where it
    /// cannot — a call read back from a record that kept no answer and was
    /// not run from here — which draws no claim at all rather than a guess.
    pub fn gate(&self) -> Option<Gate> {
        self.gate
    }
}

impl std::hash::Hash for Change {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.fingerprint.hash(state);
        self.gate.hash(state);
    }
}

/// How a call that changed a file came to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gate {
    /// The operator allowed it at the prompt.
    Operator,
    /// The operator allowed it at the prompt and saved a rule for next time.
    OperatorAlways,
    /// A standing rule answered the prompt before the operator saw it.
    Rule,
    /// No prompt reached this shell: the backend ran the call in this mode
    /// without asking. Claimed only for a call sent from this shell, which
    /// would have seen a prompt had there been one.
    Unasked(Mode),
}

/// A sub-agent as this shell saw it: what it was spawned to do, when it
/// started, what its backend reported about it since, and how it ended if it
/// has.
///
/// A finished agent is kept rather than dropped: the transcript's rows name it
/// by a tag worked out among every agent the session spawned, and the
/// Activity pane lists the ones still running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubAgent {
    /// The agent, as the backend named it.
    pub id: AgentId,
    /// The kind of agent it was asked to be, where the backend said.
    pub kind: Option<String>,
    /// What it was spawned to do.
    pub label: String,
    /// When it was spawned, where the shell had a clock at the time.
    pub at: Option<Stamp>,
    /// How it finished, or `None` while it is still running or where the
    /// session ended under it.
    pub outcome: Option<AgentOutcome>,
    /// Whether the session ended while it was running, so that no outcome
    /// will arrive for it.
    pub interrupted: bool,
    /// The model its own messages were answered by, where its backend said.
    pub model: Option<String>,
    /// The tokens in its conversation at its latest message, where its
    /// backend counted them. A size, not what it spent.
    pub context_tokens: Option<u64>,
    /// The last thing it was observed doing, in its backend's words.
    pub latest: Option<String>,
}

impl SubAgent {
    /// Whether it is still running: it has not finished, and the session did
    /// not end under it.
    pub fn is_running(&self) -> bool {
        self.outcome.is_none() && !self.interrupted
    }

    /// What it was spawned to do, without the kind of agent it was asked to
    /// be where its label opens with that.
    pub fn task(&self) -> &str {
        crate::tags::task(self.named())
    }

    fn named(&self) -> crate::tags::Named<'_> {
        crate::tags::Named {
            kind: self.kind.as_deref(),
            label: &self.label,
        }
    }
}

/// What a running turn is doing, for the line that shows the session is at
/// work between one event and the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    /// How long the turn has run, by the clock the event loop hands in.
    pub elapsed: Duration,
    /// What it is doing now: `thinking`, `writing`, `running <tool>  <what>`
    /// or `waiting on you`.
    pub doing: String,
}

/// Whether a turn is running, and how long the session has been that way.
///
/// The menu row's state segment, which is the one place the shell says the
/// session is alive without the operator reading a transcript. The duration is
/// absent until the event loop has handed a clock in: how long a session has
/// been idle is a measurement, and a shell that has not been told the time has
/// not made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pulse {
    /// Whether a turn sent from this process is running.
    pub working: bool,
    /// How long the session has been working, or been idle.
    pub since: Option<Duration>,
}

/// Everything the shell draws, and the keys that change it.
#[derive(Debug)]
pub struct App {
    repo: Repo,
    /// How long the last run of each test command took, by what the call
    /// does, where it ran to the end and its counts were read.
    test_times: BTreeMap<String, Duration>,
    /// The repository's files, indexed for the list under an `@` word.
    mention_index: crate::mention::Files,
    /// Sections the operator has folded away, in whichever pane they belong
    /// to. One set, because a section is in exactly one pane.
    folded: std::collections::BTreeSet<Section>,
    changes: Scroller,
    activity: Scroller,
    /// The pane the operator gave the keyboard to. A question that holds the
    /// keyboard overrides it without changing it: see [`App::focus`].
    focus: Focus,
    /// Where the session pane was drawn last frame, so a click or a wheel
    /// notch over it can be told from one over the panes beside it.
    session_area: Option<ratatui::layout::Rect>,
    /// Where the way back down to the newest line was drawn last frame, while
    /// the transcript is scrolled back.
    jump: Option<ratatui::layout::Rect>,
    /// Whether the last frame drew the top row of the question waiting, which
    /// says who asks and what is asked. `true` until a frame says otherwise.
    question_top_drawn: bool,
    /// Whether the last frame was too small to draw the shell, and so drew
    /// no question at all. `false` until a frame says otherwise.
    too_small: bool,
    /// Where the last frame drew the transcript's lines, and how many of them
    /// are the session's rather than the question waiting at their end: the
    /// mouse selects and opens links only on those.
    transcript_drawn: Option<(ratatui::layout::Rect, usize)>,
    /// The cell the left button went down on over the transcript, until it
    /// comes up again.
    pressed: Option<crate::select::Point>,
    /// The run of the transcript the operator dragged over, marked until the
    /// next click or key.
    selection: Option<crate::select::Selection>,
    /// What the operator asked of the desktop and the loop has not handed
    /// over yet.
    handoffs: Vec<crate::desktop::Handoff>,
    profile: Option<SelectedProfile>,
    theme: Theme,
    /// How many colours the terminal draws, which every theme is drawn at.
    depth: Depth,
    session: SessionState,
    entries: Vec<Entry>,
    /// Where each running call is drawn: its entry, and its place among the
    /// entry's calls.
    tool_entries: BTreeMap<ToolCallId, (usize, usize)>,
    /// Which sub-agent each entry of a sub-agent's is, by the entry's place:
    /// what renames the entry when another agent's name makes its own
    /// ambiguous, and what keeps two agents' calls out of one run.
    agent_entries: BTreeMap<usize, AgentId>,
    /// The entry a call to the same tool would join, while nothing has come
    /// between it and the next call: see [`Entry::calls`] for what breaks a
    /// run.
    run: Option<usize>,
    /// Whether a run of calls is drawn as its group row alone, rather than
    /// with a row for each call under it.
    calls_folded: bool,
    /// Whether the sub-agents' rows are drawn together under each agent
    /// rather than interleaved as they happened.
    by_agent: bool,
    diffs_open: bool,
    /// How each call the operator or a rule let through was allowed, until
    /// the call ends and its entry takes the answer.
    answered: BTreeMap<ToolCallId, PermissionDecision>,
    /// The call that ended with the event just folded — its entry and its
    /// place in it — and how it came to run. A backend reports a file change
    /// or a test run directly after the end of the call that made it, so this
    /// is where either is drawn; any other event in between clears it.
    just_ended: Option<(usize, usize, Option<Gate>)>,
    /// Every sub-agent the session spawned, in the order it spawned them. The
    /// session fold keeps the counts and which ids are running; the label, the
    /// moment and the outcome as this pane reads them are this shell's
    /// business, so they are kept here rather than widening the shared state —
    /// and the event model, which carries no time — for a pane.
    agents: Vec<SubAgent>,
    composer: TextArea<'static>,
    /// First transcript line drawn, in wrapped lines.
    scroll: usize,
    /// Whether the transcript sticks to the newest line as it grows.
    follow: bool,
    /// Wrapped transcript lines at the last draw, and the height they were
    /// drawn into. Paging needs both and only the draw knows them.
    transcript_lines: usize,
    viewport_lines: usize,
    hint: Option<String>,
    /// Whether the last key was an Esc that did nothing else, which makes a
    /// digit pressed next the F-key of that number.
    escaped: bool,
    /// The search through the transcript, while it is open.
    find: Option<Find>,
    /// The word the operator closed the list under — of files for an `@`
    /// word, of commands for the `/` that opens the prompt — by its line and
    /// the column it starts at, so the list stays closed while the cursor is
    /// still in that word. The two cannot share a word: one starts with `@`
    /// and the other with `/`.
    offer_closed: Option<(usize, usize)>,
    /// Which of the entries the open list offers Enter would take.
    offer_selected: usize,
    /// Whether a [`crate::shell::Shell`] is there to run the operator's
    /// commands. Without one, `!` is a character like any other.
    runs_commands: bool,
    /// Whether the shell was handed a record of the repository's sessions it
    /// can open another of in place of this one.
    remembers: bool,
    /// What the last load of the repository's earlier sessions found; `None`
    /// until one has landed.
    past: Option<crate::history::Past>,
    /// Whether a load has been asked for that the loop has not passed on.
    history_wanted: bool,
    /// The number this session is recorded under, once it is.
    recorded_as: Option<String>,
    /// A walk back through earlier prompts with Up and Down, while one is on.
    recall: Option<Recall>,
    /// The history dialog, while it is open.
    browser: Option<Browser>,
    /// The session the operator chose to open in place of this one, which
    /// the loop hands back as it ends.
    opening: Option<Target>,
    /// Whether the terminal tells Shift+Enter from Enter, which decides the
    /// key the bar names for a new line.
    reports_shift_enter: bool,
    /// Whether what is in the composer is a command for the operator's own
    /// shell rather than a prompt: set by `!` on an empty composer.
    shell_mode: bool,
    /// Commands the operator ran that have not been handed out to be run.
    commands: Vec<(ToolCallId, String)>,
    /// Each command handed out to be run and not yet ended, by the call it is
    /// recorded as, so its end can repeat what it ran; oldest first, which is
    /// what decides the one [`crate::shell::STOP_KEY`] stops.
    running_commands: Vec<(ToolCallId, String)>,
    /// Commands the operator stopped that have not been handed out to be
    /// stopped.
    stops: Vec<ToolCallId>,
    /// Every running command the operator has asked to stop, so a second
    /// press moves on to the next one and its end can say who stopped it.
    stopping: BTreeSet<ToolCallId>,
    /// How many commands this shell has started, which keeps each one's call
    /// id apart from the others started in the same second.
    commands_started: u64,
    /// Prompts waiting on the operator, oldest first. The transcript shows
    /// the front one; the rest wait behind it, because a backend can gate two
    /// calls of the same turn and answering them out of order would put the
    /// wrong arguments in front of the operator.
    asks: VecDeque<Ask>,
    /// Which of the front prompt's options Enter would give. Every prompt
    /// starts on the first, so Enter means the same thing whichever prompt it
    /// lands on.
    ask_selected: usize,
    /// Where the keyboard is while a prompt waits.
    ask_focus: AskFocus,
    /// The answer being written in place of the numbered ones.
    ask_draft: String,
    /// Since when the front prompt has waited on a quiet keyboard: when it
    /// came to the front, or when a key last reached it that was not taken
    /// as an answer. `None` where no clock was handed in.
    ask_quiet_since: Option<Instant>,
    /// Since when the keyboard has been quiet after a question that had it
    /// was taken away rather than answered, until a key comes after
    /// [`ASK_QUIET`] of quiet. `None` otherwise, or where no clock was handed
    /// in.
    withdrawn_quiet_since: Option<Instant>,
    /// When the key being handled was read, where the event loop said.
    arrival: Option<Arrival>,
    /// The standing answers this session starts with, plus the ones made in
    /// it.
    allowed: Allowlist,
    /// Rules made here that have not been handed out to be kept.
    learned: Vec<Rule>,
    /// Events the operator produced that have not been handed out to be kept.
    produced: Vec<Event>,
    /// The images attached to prompts, and the ones asked for.
    images: Attachments,
    /// The list the operator opened — of models, effort levels or themes —
    /// while it is open.
    picking: Option<Picker>,
    /// The menu the operator opened, while it is open.
    menu: Option<crate::menu::Open>,
    /// Where the last frame drew the open menu's list, border included, so a
    /// click can tell which item it landed on.
    menu_list: Option<ratatui::layout::Rect>,
    /// Where the last frame drew the menu bar and the F-key bar.
    bars: Option<(ratatui::layout::Rect, ratatui::layout::Rect)>,
    /// What the shell is saying at more length than a hint, while it is up.
    sheet: Option<Sheet>,
    /// The panes of the right-hand stack the operator hid.
    hidden: BTreeSet<crate::menu::SidePane>,
    /// The files outside the session the shell can name or have opened.
    places: Places,
    /// The question whether to trust the repository's config, while it is
    /// up. A shell asking it has no session behind it: it ends on the answer.
    trusting: Option<trust::Asking>,
    /// How that question was answered, once it has been.
    trusted: Option<trust::Answer>,
    /// The most this session may spend, where the operator set a budget.
    budget_usd: Option<f64>,
    /// What values the tokens a backend reported no cost for. Supplied by
    /// whatever opened the shell, because the price table is not this crate's
    /// business; `None` leaves an unpriced turn reading as one.
    prices: Option<Box<dyn Prices>>,
    /// Whether the budget warning has already been given, so that it is said
    /// once rather than on every usage record after the line is crossed.
    budget_warned: bool,
    /// Whether a backend is listening. The shell holds no backend handle; this
    /// is the one bit of it the transcript needs, so that a prompt with nowhere
    /// to go says so instead of looking sent.
    attached: bool,
    /// Whether a prompt was sent to the backend from this process. A turn the
    /// fold says is running may be one read back from a record, which nothing
    /// is working on now; only one sent from here can be shown as working.
    sent_here: bool,
    /// Whether the first prompt of the session never reached the backend,
    /// which leaves it out of the session's caption: nothing was asked.
    first_prompt_unsent: bool,
    /// Whether an event has come in through [`App::apply`] rather than from
    /// the operator.
    heard: bool,
    /// The timezone a moment an event names is read in. `None` until the
    /// event loop hands over the one it read: a fold with no clock behind it
    /// has no local times to draw, which is what a shell drawn in a test has.
    clock: Option<Clock>,
    /// The clock, as the event loop last handed it in. The draw never reads
    /// the time itself, so a test draws the same frame every time.
    now: Option<Instant>,
    /// The wall clock the same tick read, which is what every event folded in
    /// now is stamped with. `None` until the loop has ticked, which is also
    /// what a fold with no clock behind it — a JSON Lines log — leaves behind:
    /// no time at all, rather than the moment the log was parsed.
    at: Option<Stamp>,
    /// When the running turn's first prompt was folded in, by the clock it
    /// was stamped with: what a turn's duration is counted from. `None`
    /// between turns, and for a turn whose prompt came with no time.
    turn_began_at: Option<Stamp>,
    /// How many sub-agents had been spawned, and how many entries the
    /// transcript held, when the running turn's first prompt was folded in:
    /// what the turn's rule counts its agents and calls from.
    turn_began_with: (usize, usize),
    /// The failure entry that stands for the run of events the journal has
    /// refused since it last kept one, while it keeps refusing.
    unsaved: Option<Unsaved>,
    /// How long the turns that have ended ran, each from its first prompt to
    /// its end by the clocks they were folded at. `None` from the first turn
    /// that ended without both ends stamped — an imported transcript, a log
    /// folded with no clock — because a sum missing a turn is not the time
    /// the session worked, and a rate over it would overstate the spend.
    turns_took: Option<Duration>,
    /// The stamp the last event folded in carried: where a turn read back
    /// unfinished is known to have got to.
    last_folded_at: Option<Stamp>,
    /// When the running turn was first seen running by that clock.
    working_since: Option<Instant>,
    /// When the session was first seen not running by that clock. The mirror
    /// of `working_since`: exactly one of the two is set once the loop has
    /// ticked, so the state segment always has a duration to show.
    idle_since: Option<Instant>,
    /// Each entry as last drawn, so a redraw re-renders only what changed.
    drawn: crate::ui::DrawnEntries,
    /// The line at the top of the view when a switch was pressed while the
    /// transcript was scrolled back, until the next draw puts it back there.
    held: Option<crate::ui::Anchor>,
    should_quit: bool,
    /// Whether the operator pressed Ctrl+Z since the loop last asked.
    suspend: bool,
    /// Whether the operator asked the running turn to stop since the loop
    /// last asked.
    interrupt: bool,
    /// The turn a stop was asked for, counted as the turns that had ended
    /// before it, so a second Ctrl+C in the same turn quits and one in the
    /// next turn stops that turn.
    stop_asked_in: Option<usize>,
}

/// A search through the transcript, from Ctrl+F to Esc.
#[derive(Debug)]
struct Find {
    /// What is being looked for, edited like the composer.
    query: TextArea<'static>,
    /// Where the view was when the search opened — the first line drawn and
    /// whether it followed the tail — which is where Esc puts it back.
    from: (usize, bool),
    /// Every place the query was found by the last draw, top to bottom.
    found: Vec<crate::find::Found>,
    /// Which of them is the one stepped to. `None` until a draw has found
    /// the query at all, and after every edit of it, when the draw picks the
    /// match nearest where the view was.
    current: Option<usize>,
    /// Whether the view has been moved to the current match since it last
    /// changed. Kept so the view is moved once per step rather than every
    /// frame, which would undo the operator scrolling away from it.
    revealed: bool,
}

impl Find {
    /// Steps to the next match down the transcript, or up it, going round at
    /// either end.
    fn step(&mut self, down: bool) {
        let count = self.found.len();
        let Some(at) = self.current.filter(|_| count > 0) else {
            return;
        };
        self.current = Some(if down {
            (at + 1) % count
        } else {
            (at + count - 1) % count
        });
        self.revealed = false;
    }
}

/// How a scroll key or a wheel notch moves the pane it goes to.
#[derive(Debug, Clone, Copy)]
enum Scroll {
    PageUp,
    PageDown,
    Up(usize),
    Down(usize),
    Head,
    Tail,
}

/// The share of a budget at which the shell says so.
///
/// Four fifths: far enough in that the warning means something, and far enough
/// from the end that a turn can still be started on purpose.
const BUDGET_WARNING: f64 = 0.8;

/// How long a prompt waits on a quiet keyboard before a key answers it.
///
/// Long enough to hold back the keys of someone typing when the prompt came
/// up — at thirty words a minute a key lands every 400 ms, and every key held
/// back starts the wait again — and short against the time it takes to read
/// the question before answering it.
pub const ASK_QUIET: Duration = Duration::from_millis(500);

/// What the bar says when a key reached a prompt whose top is off screen.
const TOP_OFF_SCREEN_HINT: &str =
    "Not taken as an answer: the top of the question is off screen. Ctrl+T cuts it to fit";

/// What the shell says when a key reached a prompt the window is too small
/// to draw.
const TOO_SMALL_HINT: &str = "Not taken as an answer: the window is too small to show the question";

/// What the bar says when the question that had the keyboard was taken away
/// rather than answered.
const QUESTION_WITHDRAWN_HINT: &str = "The question was withdrawn before it was answered";

/// What the bar says when an Enter typed on through a withdrawn question was
/// not taken as a send.
const WITHDRAWN_NOT_SENT_HINT: &str =
    "Not sent: the question being answered was withdrawn. Press Enter again to send";

/// What the bar says when keys reached a prompt too soon to answer it.
const TOO_SOON_HINT: &str =
    "Not taken as an answer: typed as the question came up, or pasted. Press the key again";

/// What the bar says when a click on the menus or the F-key bar was refused
/// because a question waits.
const QUESTION_FIRST_HINT: &str =
    "The menus open once the question is answered; F10 and Esc still quit and stop";

/// When a key reached the shell, as the event loop read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arrival {
    /// When the read that held it came back.
    pub at: Instant,
    /// Whether it was the only key in that read.
    pub alone: bool,
}

impl App {
    /// An empty session in the given repo.
    pub fn new(repo: Repo) -> Self {
        let theme = Theme::default();
        let mut composer = TextArea::default();
        // A prompt is prose, so it wraps rather than scrolling sideways, and a
        // path or a URL longer than the pane falls back to breaking mid-word.
        composer.set_wrap_mode(WrapMode::WordOrGlyph);
        composer.set_placeholder_text(placeholder(
            usize::MAX,
            Offers {
                files: false,
                shell: false,
                commands: false,
            },
        ));
        paint_composer(&mut composer, &theme);

        Self {
            test_times: BTreeMap::new(),
            mention_index: crate::mention::Files::new(&repo.files),
            repo,
            folded: std::collections::BTreeSet::new(),
            changes: Scroller::default(),
            activity: Scroller::default(),
            focus: Focus::Session,
            session_area: None,
            jump: None,
            question_top_drawn: true,
            too_small: false,
            transcript_drawn: None,
            pressed: None,
            selection: None,
            handoffs: Vec::new(),
            profile: None,
            theme,
            depth: Depth::default(),
            session: SessionState::new(),
            entries: Vec::new(),
            tool_entries: BTreeMap::new(),
            agent_entries: BTreeMap::new(),
            run: None,
            calls_folded: false,
            by_agent: false,
            diffs_open: false,
            answered: BTreeMap::new(),
            just_ended: None,
            agents: Vec::new(),
            composer,
            scroll: 0,
            follow: true,
            transcript_lines: 0,
            viewport_lines: 0,
            hint: None,
            escaped: false,
            find: None,
            offer_closed: None,
            offer_selected: 0,
            runs_commands: false,
            remembers: false,
            past: None,
            // Asked for as the shell opens, so Up has earlier sessions'
            // prompts by the time anyone presses it.
            history_wanted: true,
            recorded_as: None,
            recall: None,
            browser: None,
            opening: None,
            reports_shift_enter: false,
            shell_mode: false,
            commands: Vec::new(),
            running_commands: Vec::new(),
            stops: Vec::new(),
            stopping: BTreeSet::new(),
            commands_started: 0,
            ask_quiet_since: None,
            withdrawn_quiet_since: None,
            arrival: None,
            asks: VecDeque::new(),
            ask_selected: 0,
            ask_focus: AskFocus::Choosing,
            ask_draft: String::new(),
            allowed: Allowlist::new(),
            learned: Vec::new(),
            produced: Vec::new(),
            images: Attachments::default(),
            picking: None,
            menu: None,
            menu_list: None,
            bars: None,
            sheet: None,
            hidden: BTreeSet::new(),
            places: Places::default(),
            trusting: None,
            trusted: None,
            budget_usd: None,
            prices: None,
            budget_warned: false,
            attached: false,
            sent_here: false,
            first_prompt_unsent: false,
            heard: false,
            clock: None,
            now: None,
            at: None,
            unsaved: None,
            turn_began_at: None,
            turn_began_with: (0, 0),
            turns_took: Some(Duration::ZERO),
            last_folded_at: None,
            working_since: None,
            idle_since: None,
            drawn: crate::ui::DrawnEntries::default(),
            held: None,
            should_quit: false,
            suspend: false,
            interrupt: false,
            stop_asked_in: None,
        }
    }

    /// Folds one event in: the session totals the panes read, and the
    /// transcript entry it produces, if it produces one.
    pub fn apply(&mut self, event: &Event) {
        self.heard = true;
        self.fold_event(event);
    }

    /// Whether anything has come in that the shell did not produce itself:
    /// the backend has said something — or a record of one was read back.
    pub fn heard(&self) -> bool {
        self.heard
    }

    /// Folds one event in, from wherever it came.
    fn fold_event(&mut self, event: &Event) {
        let turns = self.session.turns().len();
        if matches!(event, Event::UserMessage { .. }) && !self.session.turn_running() {
            self.turn_began_at = self.at;
            self.turn_began_with = (self.agents.len(), self.entries.len());
        }
        self.session.apply(event);
        // A session left is recorded when the process stopped, or when the
        // session is taken up again after one was killed: the turn it cuts
        // ended with the last thing that happened in it, and the time between
        // is time nothing ran.
        let ended_at = match event {
            Event::SessionLeft => self.last_folded_at,
            _ => self.at,
        };
        self.last_folded_at = ended_at;
        let ended = self.just_ended.take();
        self.fold_into_transcript(event, ended);
        if self.session.turns().len() > turns {
            self.rule_turn(ended_at);
        }
    }

    /// Records that the process this session runs in is leaving it with a
    /// turn still running, or that one before it did: a record that ends in
    /// the middle of a turn, taken up again. The turn ends there, cut, so the
    /// next prompt opens a turn of its own.
    pub fn leave(&mut self) {
        if self.session.turn_running() {
            self.produce(Event::SessionLeft);
        }
    }

    /// Draws the rule under the turn the fold has just ended, below whatever
    /// the event that ended it put in the transcript, as ended `at`.
    fn rule_turn(&mut self, at: Option<Stamp>) {
        let began = self.turn_began_at.take();
        let Some(turn) = self.session.turns().last() else {
            return;
        };
        let measured = match (began, at) {
            (Some(began), Some(ended)) => ended.since(began),
            _ => None,
        };
        self.turns_took = self
            .turns_took
            .zip(measured)
            .map(|(sum, took)| sum.saturating_add(took));
        let took = measured.filter(|took| *took >= TIMED);
        let (agents_before, entries_before) = std::mem::take(&mut self.turn_began_with);
        let calls = self
            .entries
            .get(entries_before..)
            .unwrap_or_default()
            .iter()
            .map(|entry| entry.calls.len())
            .sum::<usize>();
        let rule = TurnRule {
            number: turn.number,
            agents: count(self.agents.len().saturating_sub(agents_before)),
            calls: count(calls),
            cut: turn.cut,
            ended: at.and_then(Stamp::local),
            tokens: turn.tokens,
            five_hour_points: turn.five_hour_share.map(percent),
            took,
        };
        self.push(Entry {
            kind: EntryKind::Turn(rule),
            head: String::new(),
            meta: String::new(),
            body: String::new(),
            streaming: false,
            at: self.at,
            calls: Vec::new(),
            agent: None,
        });
    }

    /// Puts what `event` says in the transcript, where it says anything there.
    /// `ended` is the call that ended with the event before it, which a file
    /// change is drawn under.
    fn fold_into_transcript(&mut self, event: &Event, ended: Option<(usize, usize, Option<Gate>)>) {
        match event {
            Event::UserMessage { text } => {
                self.images.seen(text);
                self.push(Entry {
                    kind: EntryKind::User,
                    head: "you".to_owned(),
                    meta: String::new(),
                    body: text.clone(),
                    streaming: false,
                    at: self.at,
                    calls: Vec::new(),
                    agent: None,
                });
            }

            Event::AssistantDelta { text } => match self.streaming_agent_entry() {
                Some(entry) => entry.body.push_str(text),
                None => {
                    let head = self.agent_name();
                    self.push(Entry {
                        kind: EntryKind::Agent,
                        head,
                        meta: String::new(),
                        body: text.clone(),
                        streaming: true,
                        at: self.at,
                        calls: Vec::new(),
                        agent: None,
                    });
                }
            },

            Event::AssistantMessage {
                text,
                agent: Some(agent),
            } => {
                self.agent_entries.insert(self.entries.len(), agent.clone());
                self.push(Entry {
                    kind: EntryKind::SubAgent,
                    head: self.sub_agent_label(agent),
                    meta: "sub-agent".to_owned(),
                    body: text.clone(),
                    streaming: false,
                    at: self.at,
                    calls: Vec::new(),
                    agent: Some(self.sub_agent_tag(agent)),
                });
            }

            Event::AssistantMessage { text, agent: None } => match self.streaming_agent_entry() {
                Some(entry) => {
                    entry.body = text.clone();
                    entry.streaming = false;
                }
                None => {
                    let head = self.agent_name();
                    self.push(Entry {
                        kind: EntryKind::Agent,
                        head,
                        meta: String::new(),
                        body: text.clone(),
                        streaming: false,
                        at: self.at,
                        calls: Vec::new(),
                        agent: None,
                    });
                }
            },

            Event::ToolCallStart {
                id,
                name,
                input,
                summary,
                agent,
            } => {
                let head = tool_label(name);
                let mut call = Call::started(what_it_does(summary.as_deref(), input), self.at);
                if call.testing {
                    call.last_run = self.test_times.get(&call.what).copied();
                }
                match self.open_run(&head, agent.as_ref()) {
                    Some(at) => {
                        if let Some(entry) = self.entries.get_mut(at) {
                            self.tool_entries
                                .insert(id.clone(), (at, entry.calls.len()));
                            entry.calls.push(call);
                            entry.streaming = true;
                        }
                    }
                    None => {
                        let at = self.entries.len();
                        self.tool_entries.insert(id.clone(), (at, 0));
                        if let Some(agent) = agent {
                            self.agent_entries.insert(at, agent.clone());
                        }
                        let agent = agent.as_ref().map(|agent| self.sub_agent_tag(agent));
                        self.push(Entry {
                            kind: EntryKind::Tool,
                            head,
                            meta: String::new(),
                            body: String::new(),
                            streaming: true,
                            at: self.at,
                            calls: vec![call],
                            agent,
                        });
                        self.run = Some(at);
                    }
                }
            }

            Event::ToolCallEnd {
                id,
                name,
                input,
                output,
                bytes,
                outcome,
                summary,
                exit_code,
                error,
            } => {
                let ending = Ending {
                    outcome: *outcome,
                    bytes: *bytes,
                    exit_code: *exit_code,
                    error: error.clone(),
                };
                let gate = self.gate(id);
                let started = self.tool_entries.remove(id);
                let ended = match started {
                    Some((at, index)) => self.end_call(at, index, ending),
                    // An end whose start never arrived is shown, not dropped:
                    // the session fold counts it too, and a gap the operator
                    // cannot see is a gap nobody reports.
                    None => {
                        let mut call = Call::started(what_it_does(summary.as_deref(), input), None);
                        call.ended(ending, None);
                        self.push(Entry {
                            kind: match call.failed() {
                                true => EntryKind::Failure,
                                false => EntryKind::Tool,
                            },
                            head: tool_label(name),
                            meta: String::new(),
                            body: String::new(),
                            streaming: false,
                            at: self.at,
                            calls: vec![call],
                            agent: None,
                        });
                        Some((self.entries.len().saturating_sub(1), 0))
                    }
                };
                if name == crate::shell::OPERATOR_SHELL
                    && let Some(call) = ended.and_then(|(at, index)| {
                        self.entries
                            .get_mut(at)
                            .and_then(|entry| entry.calls.get_mut(index))
                    })
                {
                    call.printed = Some(Printed::of(output));
                }
                if let Some(call) = ended.and_then(|(at, index)| {
                    self.entries
                        .get_mut(at)
                        .and_then(|entry| entry.calls.get_mut(index))
                }) {
                    call.gate = gate;
                }
                // A failed call is kept too: a test run that failed ended
                // its call with a failing status.
                if let Some((at, index)) = ended {
                    self.just_ended = Some((at, index, gate));
                }
            }

            Event::FileChange {
                added,
                removed,
                hunks,
                ..
            } => {
                if let Some((at, index, gate)) = ended
                    && let Some(call) = self
                        .entries
                        .get_mut(at)
                        .and_then(|entry| entry.calls.get_mut(index))
                    && call.outcome == Some(ToolOutcome::Ok)
                {
                    call.lines = Some((*added, *removed));
                    if !hunks.is_empty() {
                        call.change = Some(Change::new(hunks.clone(), gate));
                    }
                }
            }

            // The working line reads the end off the fold; the transcript has
            // already shown everything the turn said. A turn's calls are not
            // grouped with the next turn's.
            // A question still open when the turn ended waits on nobody.
            Event::TurnEnded => {
                self.run = None;
                self.forget_asks();
            }

            Event::PermissionWithdrawn { id } => self.withdraw_ask(id),

            // Nothing the stopped process was doing will say any more of
            // itself: a reply it was writing reads as cut off where it
            // stopped, rather than as finished.
            Event::SessionLeft => {
                self.forget_asks();
                self.interrupt_what_the_fold_stopped();
                if let Some(entry) = self.streaming_agent_entry() {
                    entry.streaming = false;
                    entry.meta = CUT_OFF.to_owned();
                }
            }

            Event::Error { message, fatal } => {
                // A session that ended cannot take an answer, so a prompt it
                // ended on is not left on screen asking for one.
                if *fatal {
                    self.forget_asks();
                    self.interrupt_what_the_fold_stopped();
                }
                self.push(Entry {
                    kind: EntryKind::Failure,
                    head: if *fatal { "fatal" } else { "error" }.to_owned(),
                    meta: String::new(),
                    body: message.clone(),
                    streaming: false,
                    at: self.at,
                    calls: Vec::new(),
                    agent: None,
                });
            }

            Event::Notice { message } => self.push(Entry {
                kind: EntryKind::Notice,
                head: self.agent_name(),
                meta: String::new(),
                body: message.clone(),
                streaming: false,
                at: self.at,
                calls: Vec::new(),
                agent: None,
            }),

            Event::Cleared => self.push(Entry {
                kind: EntryKind::Notice,
                head: "cleared".to_owned(),
                meta: String::new(),
                body: "The conversation starts over here: nothing above this line is in \
                       front of the model any more. What it cost stays in the session's \
                       totals."
                    .to_owned(),
                streaming: false,
                at: self.at,
                calls: Vec::new(),
                agent: None,
            }),

            Event::PermissionRequest {
                id,
                tool,
                input,
                target,
                agent,
            } => {
                if self.asks.is_empty() {
                    self.ask_quiet_since = self.latest_instant();
                }
                self.asks.push_back(Ask {
                    id: id.clone(),
                    tool: tool.clone(),
                    input: input.clone(),
                    target: target.clone(),
                    turn: self.session.user_messages(),
                    agent: agent.clone(),
                });
            }

            // A refusal is shown as its own entry rather than left to the tool
            // result that carries it back: a backend that reports nothing
            // further about a call it was not allowed to make would leave a
            // denial indistinguishable from the agent deciding not to act.
            Event::PermissionResponse {
                id,
                decision,
                message,
            } => {
                let asked = self.forget_ask(id);
                if decision.allowed() {
                    self.answered.insert(id.clone(), *decision);
                    self.restart_clock(id);
                }
                if *decision == PermissionDecision::Deny {
                    let agent = asked.as_ref().and_then(|ask| ask.agent.clone());
                    let (tool, what) = match &asked {
                        Some(ask) => (
                            tool_label(&ask.tool),
                            ask.target.clone().unwrap_or_else(|| one_line(&ask.input)),
                        ),
                        None => (id.to_string(), String::new()),
                    };
                    let tag = agent.as_ref().map(|agent| self.sub_agent_tag(agent));
                    if let Some(agent) = agent {
                        self.agent_entries.insert(self.entries.len(), agent);
                    }
                    self.push(Entry {
                        kind: EntryKind::Failure,
                        head: "denied".to_owned(),
                        meta: match what.is_empty() {
                            true => tool.clone(),
                            false => format!("{tool} · {what}"),
                        },
                        body: message
                            .as_ref()
                            .map(|said| format!("You answered instead: {said}"))
                            .unwrap_or_default(),
                        streaming: false,
                        at: self.at,
                        calls: Vec::new(),
                        agent: tag,
                    });
                }
            }

            // Everything else is a number or a list a pane reads off the
            // session fold, not a line in the transcript. The mode and the
            // model are in the menu row, which is where a session says what
            // it is running as, and the title is the session pane's caption.
            Event::SessionMeta(_)
            | Event::Titled { .. }
            | Event::Usage(_)
            | Event::ModeSelected { .. }
            | Event::ModelSelected { .. }
            | Event::UsageWindows(_)
            | Event::Billing { .. }
            | Event::Context(_)
            | Event::Commands { .. } => {}

            // Straight after its call's end, so in the same tick and at the
            // same clock: the moment the call finished.
            Event::TestRun {
                counts,
                exit_code,
                failed,
                failures,
                ..
            } => {
                if let Some((at, index, _)) = ended
                    && let Some(call) = self
                        .entries
                        .get_mut(at)
                        .and_then(|entry| entry.calls.get_mut(index))
                {
                    call.tested = Some(TestRunRecord::new(
                        *counts,
                        *exit_code,
                        *failed,
                        failures.clone(),
                    ));
                    if let Some(took) = call.took.filter(|_| counts.is_some()) {
                        self.test_times.insert(call.what.clone(), took);
                    }
                }
            }

            Event::AgentSpawn {
                id, kind, label, ..
            } => {
                let spawned = SubAgent {
                    id: id.clone(),
                    kind: kind.clone(),
                    label: label.clone(),
                    at: self.at,
                    outcome: None,
                    interrupted: false,
                    model: None,
                    context_tokens: None,
                    latest: None,
                };
                // An id the backend hands out twice is one agent started
                // again, not two rows: the second spawn replaces the first
                // where it already stood, so the list keeps spawn order.
                match self.agents.iter_mut().find(|agent| &agent.id == id) {
                    Some(existing) => *existing = spawned,
                    None => self.agents.push(spawned),
                }
                self.rename_agents_entries();
            }
            Event::AgentProgress {
                id,
                model,
                context_tokens,
                latest,
            } => {
                // A report replaces only what it speaks to: an agent's model
                // is not forgotten because the report of its next step does
                // not repeat it.
                if let Some(agent) = self.agents.iter_mut().find(|agent| &agent.id == id) {
                    agent.model = model.clone().or(agent.model.take());
                    agent.context_tokens = context_tokens.or(agent.context_tokens);
                    agent.latest = latest.clone().or(agent.latest.take());
                }
            }
            Event::AgentExit { id, outcome } => {
                if let Some(agent) = self.agents.iter_mut().find(|agent| &agent.id == id) {
                    agent.outcome = Some(*outcome);
                }
            }
        }
    }

    /// Folds one event in at a moment something else recorded, rather than at
    /// the clock this shell is running on.
    ///
    /// This is how a session read back off disk shows the times it actually
    /// ran at: the store writes a time beside every row, and a resumed session
    /// is folded in through here rather than through [`App::apply`], which
    /// would stamp a three-day-old turn with the moment it was read.
    pub fn apply_at(&mut self, event: &Event, at: Stamp) {
        let live = self.at.replace(at);
        self.apply(event);
        self.at = live;
    }

    /// Folds a whole stream in.
    ///
    /// Every entry it produces carries the clock the last [`App::tick`] handed
    /// in — which, before the event loop has run, is no clock at all. A log
    /// that recorded no times folds in through here and shows none.
    pub fn extend<'a>(&mut self, events: impl IntoIterator<Item = &'a Event>) {
        for event in events {
            self.apply(event);
        }
    }

    /// Folds a whole recorded stream in, each event at the moment it was
    /// recorded at.
    pub fn extend_at<'a>(&mut self, events: impl IntoIterator<Item = (&'a Event, Stamp)>) {
        for (event, at) in events {
            self.apply_at(event, at);
        }
    }

    fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
    }

    /// The entry a call to `head` by `agent` joins, where the transcript's
    /// last entry is a run of that agent's calls to it that nothing has
    /// broken.
    fn open_run(&self, head: &str, agent: Option<&AgentId>) -> Option<usize> {
        self.run.filter(|&at| {
            at + 1 == self.entries.len()
                && self.agent_entries.get(&at) == agent
                && self
                    .entries
                    .get(at)
                    .is_some_and(|entry| entry.head == head && !entry.calls.is_empty())
        })
    }

    /// What sub-agent `id` is called where its words are drawn: the name the
    /// Activity pane lists it by, or its id where its spawn was never seen.
    fn sub_agent_label(&self, id: &AgentId) -> String {
        self.agents
            .iter()
            .find(|agent| &agent.id == id)
            .map_or_else(|| id.to_string(), |agent| agent.label.clone())
    }

    /// Who is asking `ask`: the sub-agent whose call it is, by the name its
    /// calls' rows give it, or the session's own agent.
    pub fn asker(&self, ask: &Ask) -> String {
        match &ask.agent {
            Some(agent) => self.sub_agent_tag(agent),
            None => self.agent_name(),
        }
    }

    /// What sub-agent `id` is called on a row it shares with what it did:
    /// see [`Entry::agent`].
    fn sub_agent_tag(&self, id: &AgentId) -> String {
        match self.agents.iter().position(|agent| &agent.id == id) {
            Some(at) => self.agent_tags().swap_remove(at),
            None => id.to_string(),
        }
    }

    /// The call sub-agent `id` has open, as its row in the transcript names
    /// it — `Bash cargo test` — or `None` where it has none running.
    pub fn agent_doing(&self, id: &AgentId) -> Option<String> {
        self.agent_entries
            .iter()
            .rev()
            .filter(|(_, agent)| *agent == id)
            .find_map(|(&at, _)| {
                let entry = self.entries.get(at)?;
                let call = entry.calls.iter().rev().find(|call| call.running())?;
                Some(format!("{} {}", entry.head, call.what))
            })
    }

    /// Every sub-agent's tag, in the order [`App::agents`] lists them: the
    /// word the transcript's rows and the Activity pane name it by.
    pub fn agent_tags(&self) -> Vec<String> {
        let named: Vec<crate::tags::Named<'_>> = self.agents.iter().map(SubAgent::named).collect();
        crate::tags::tags(&named)
    }

    /// Names every sub-agent's entry again, now that the agents are not the
    /// ones its name was worked out among.
    fn rename_agents_entries(&mut self) {
        let tags: BTreeMap<&AgentId, String> = self
            .agents
            .iter()
            .map(|agent| &agent.id)
            .zip(self.agent_tags())
            .collect();
        let named: Vec<(usize, String)> = self
            .agent_entries
            .iter()
            .map(|(&at, id)| (at, tags.get(id).cloned().unwrap_or_else(|| id.to_string())))
            .collect();
        for (at, tag) in named {
            if let Some(entry) = self.entries.get_mut(at) {
                entry.agent = Some(tag);
            }
        }
    }

    /// Records the end of the `index`th call of entry `at`, and hands back
    /// where it is when it is there.
    fn end_call(&mut self, at: usize, index: usize, ending: Ending) -> Option<(usize, usize)> {
        let now = self.at;
        let entry = self.entries.get_mut(at)?;
        let call = entry.calls.get_mut(index)?;
        call.ended(ending, now);
        if call.failed() {
            entry.kind = EntryKind::Failure;
        }
        entry.streaming = entry.calls.iter().any(Call::running);
        Some((at, index))
    }

    /// Marks every call and sub-agent the fold no longer counts as running as
    /// cut short, after a fatal error ended the backend under them. The fold
    /// decides which: a command the operator ran is not the backend's, and
    /// runs on to an end of its own.
    fn interrupt_what_the_fold_stopped(&mut self) {
        let running = self.session.in_flight_tools();
        let cut: Vec<(usize, usize)> = self
            .tool_entries
            .iter()
            .filter(|(id, _)| !running.contains_key(*id))
            .map(|(_, place)| *place)
            .collect();
        self.tool_entries.retain(|id, _| running.contains_key(id));
        for (at, index) in cut {
            if let Some(entry) = self.entries.get_mut(at) {
                if let Some(call) = entry.calls.get_mut(index) {
                    call.interrupted = true;
                }
                entry.streaming = entry.calls.iter().any(Call::running);
            }
        }
        let running = self.session.running_agents();
        for agent in &mut self.agents {
            if agent.outcome.is_none() && !running.contains(&agent.id) {
                agent.interrupted = true;
            }
        }
    }

    /// Starts a call's clock again from now, because the operator has just
    /// let it run: the time it spent waiting on them is not the tool's.
    fn restart_clock(&mut self, id: &ToolCallId) {
        let now = self.at;
        if let Some(&(at, index)) = self.tool_entries.get(id)
            && let Some(call) = self
                .entries
                .get_mut(at)
                .and_then(|entry| entry.calls.get_mut(index))
        {
            call.started = now;
        }
    }

    /// Whether a run of calls is drawn as its group row alone.
    pub fn calls_folded(&self) -> bool {
        self.calls_folded
    }

    /// Folds every run of calls to its group row, or opens them all again.
    ///
    /// One switch for the whole transcript rather than one per group: the
    /// transcript has no cursor to say which group a key is meant for, and
    /// a key that folded whichever group happened to be on screen would fold
    /// one the operator was not looking at.
    pub fn fold_calls(&mut self) {
        self.hold_view();
        self.calls_folded = !self.calls_folded;
    }

    /// Whether the sub-agents' rows are drawn under each agent: see
    /// [`App::group_by_agent`].
    pub fn grouped_by_agent(&self) -> bool {
        self.by_agent
    }

    /// Draws each turn's sub-agent rows together under a header per agent,
    /// where that agent first did something in the turn, or interleaved as
    /// they happened again.
    ///
    /// Nine agents working at once interleave their calls row by row, which
    /// is when it happened but not what any one agent did; grouped, each
    /// agent's work reads top to bottom. The session's own rows, what anyone
    /// said and every turn's boundary stay where they were.
    pub fn group_by_agent(&mut self) {
        self.hold_view();
        self.by_agent = !self.by_agent;
    }

    /// Each sub-agent's tag and task, for the header its rows are grouped
    /// under.
    pub(crate) fn agent_headings(&self) -> Vec<(String, String)> {
        self.agents
            .iter()
            .zip(self.agent_tags())
            .map(|(agent, tag)| (tag, agent.task().to_owned()))
            .collect()
    }

    /// Whether every diff longer than [`crate::hunks::MAX_ROWS`] is drawn
    /// whole rather than cut there.
    pub fn diffs_open(&self) -> bool {
        self.diffs_open
    }

    /// Opens every cut diff to all its rows, or cuts them all again.
    ///
    /// One switch for the whole transcript, as [`App::fold_calls`] is and
    /// for its reason: nothing says which diff a key would be meant for.
    pub fn open_diffs(&mut self) {
        self.hold_view();
        self.diffs_open = !self.diffs_open;
    }

    /// Remembers the line at the top of the view, before a switch changes how
    /// many lines the entries above it take.
    ///
    /// The scroll offset counts lines from the top, so without this the same
    /// offset would put other lines under the view. At the tail there is
    /// nothing to hold: the view follows the tail wherever it goes. A second
    /// switch before the next draw keeps the first one's line, since the lines
    /// last drawn no longer say where the view is.
    fn hold_view(&mut self) {
        if !self.follow && self.held.is_none() {
            self.held = self.drawn.anchor(self.scroll);
        }
    }

    /// Puts the line [`App::hold_view`] remembered back at the top of the
    /// view, called by the draw once it has laid the entries out again.
    pub(crate) fn keep_view(&mut self) {
        if let Some(line) = self.held.take().and_then(|held| self.drawn.line_of(&held)) {
            self.scroll = line;
        }
    }

    /// Folds in an event the operator produced here and queues it to be kept.
    fn produce(&mut self, event: Event) {
        self.fold_event(&event);
        self.produced.push(event);
    }

    /// The events the operator produced since the last call, oldest first.
    ///
    /// Events folded in through [`App::apply`] came from somewhere that already
    /// has them, and are never handed out.
    pub fn take_produced(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.produced)
    }

    /// How the call `id` came to run, as far as this shell saw.
    ///
    /// An answer it folded says who gave it. With no answer, the call ran
    /// without a prompt only if it was sent from here, where a prompt would
    /// have been seen; a call read back from a record or an imported
    /// transcript may have been asked about somewhere this shell was not.
    fn gate(&mut self, id: &ToolCallId) -> Option<Gate> {
        match self.answered.remove(id) {
            Some(PermissionDecision::Allow) => Some(Gate::Operator),
            Some(PermissionDecision::AllowAlways) => Some(Gate::OperatorAlways),
            Some(PermissionDecision::AllowByRule) => Some(Gate::Rule),
            Some(PermissionDecision::Deny) => None,
            None if self.sent_here => self.session.mode().map(Gate::Unasked),
            None => None,
        }
    }

    /// Drops a prompt from the queue, and gives it back.
    ///
    /// The next prompt starts afresh — on its first option, with the keyboard
    /// on it — whatever the operator was doing with the one before, so a
    /// question put off is not mistaken for the one behind it.
    fn forget_ask(&mut self, id: &ToolCallId) -> Option<Ask> {
        let at = self.asks.iter().position(|ask| &ask.id == id)?;
        if at == 0 {
            self.ask_selected = 0;
            self.ask_focus = AskFocus::Choosing;
            self.ask_draft.clear();
            self.ask_quiet_since = self.latest_instant();
        }
        self.asks.remove(at)
    }

    /// Takes the question about `id` off the queue, where it is still there:
    /// the backend stopped asking it.
    fn withdraw_ask(&mut self, id: &ToolCallId) {
        let Some(at) = self.asks.iter().position(|ask| ask.id == *id) else {
            return;
        };
        self.asks.remove(at);
        if at == 0 {
            self.front_ask_taken_away();
        }
    }

    /// Drops every prompt waiting, and whatever the operator had begun
    /// answering the one on screen with.
    fn forget_asks(&mut self) {
        if self.asks.is_empty() {
            return;
        }
        self.asks.clear();
        self.front_ask_taken_away();
    }

    /// Settles the keyboard after the question on screen was taken away
    /// rather than answered, with what is left of the queue in place.
    ///
    /// The operator may be halfway through answering it, and the keys they
    /// type on were meant for it. The question behind it comes up as a new
    /// one does, waiting for a quiet keyboard; with none behind it, the
    /// prompt takes those keys but no Enter among them sends it, and an
    /// answer being written is moved into an empty prompt rather than lost.
    fn front_ask_taken_away(&mut self) {
        let draft = std::mem::take(&mut self.ask_draft);
        let had_keyboard = self.ask_focus != AskFocus::Deferred;
        self.ask_selected = 0;
        self.ask_focus = AskFocus::Choosing;
        if !self.asks.is_empty() {
            self.ask_quiet_since = self.latest_instant();
        }
        if !had_keyboard {
            return;
        }
        self.hint = Some(QUESTION_WITHDRAWN_HINT.to_owned());
        if self.asks.is_empty() {
            self.withdrawn_quiet_since = self.latest_instant();
            if self.composer.is_empty() && self.composer.insert_str(draft) {
                self.focus = Focus::Session;
            }
        }
    }

    /// The answers the prompt on screen offers, in the order they are
    /// numbered. Empty with no prompt waiting.
    pub fn ask_options(&self) -> Vec<Answer> {
        self.asks.front().map(Ask::options).unwrap_or_default()
    }

    /// The answer Enter would give the prompt on screen.
    pub fn ask_selected(&self) -> Answer {
        self.ask_options()
            .get(self.ask_selected)
            .copied()
            .unwrap_or(Answer::Once)
    }

    /// Where the keyboard is while a prompt waits.
    pub fn ask_focus(&self) -> AskFocus {
        self.ask_focus
    }

    /// What the operator has written so far in place of the numbered answers.
    pub fn ask_draft(&self) -> &str {
        &self.ask_draft
    }

    /// Moves the selection one option along, `forward` or back, wrapping at
    /// either end.
    fn move_selection(&mut self, forward: bool) {
        let count = self.ask_options().len();
        if count == 0 {
            return;
        }
        let at = self.ask_selected.min(count - 1);
        self.ask_selected = match forward {
            true => (at + 1) % count,
            false => (at + count - 1) % count,
        };
    }

    /// Selects the option numbered `number`, counted from one. A number past
    /// the last option selects nothing rather than the nearest one: the
    /// operator pressed a key for an answer that is not on offer.
    fn select_option(&mut self, number: u32) {
        let count = self.ask_options().len();
        if let Some(at) = usize::try_from(number)
            .ok()
            .and_then(|n| n.checked_sub(1))
            .filter(|at| *at < count)
        {
            self.ask_selected = at;
        }
    }

    /// The prompt the transcript is asking, if any.
    pub fn asking(&self) -> Option<&Ask> {
        self.asks.front()
    }

    /// How many prompts are waiting behind the one on screen.
    pub fn asks_waiting(&self) -> usize {
        self.asks.len().saturating_sub(1)
    }

    /// The same shell, with the standing answers a config already holds.
    #[must_use]
    pub fn with_rules(mut self, allowed: Allowlist) -> Self {
        self.allowed = allowed;
        self
    }

    /// The standing answers this session is running with.
    pub fn allowed(&self) -> &Allowlist {
        &self.allowed
    }

    /// Answers every waiting prompt a standing rule already covers.
    ///
    /// Called by the event loop and never by [`App::apply`], which is also how
    /// a recorded session is folded: a rule that answered prompts again while
    /// a recording was being read would write decisions into a session that
    /// had already made its own.
    pub fn settle_rules(&mut self) {
        while let Some(ask) = self.asks.front() {
            if !self.allowed.allows(&ask.tool, ask.target.as_deref()) {
                return;
            }
            let id = ask.id.clone();
            self.produce(Event::PermissionResponse {
                id,
                decision: PermissionDecision::AllowByRule,
                message: None,
            });
        }
    }

    /// Answers the prompt the transcript is asking.
    ///
    /// "Always" stores the rule as well as answering, so that the same prompt
    /// does not come back. [`Answer::AlwaysTarget`] on a prompt the backend
    /// named no target for allows this call and stores nothing: there is no
    /// target to write a rule about, and widening it to the whole tool would
    /// grant more than was asked for.
    pub fn answer(&mut self, answer: Answer) {
        let Some(ask) = self.asks.front().cloned() else {
            return;
        };

        let rule = match answer {
            Answer::Once | Answer::No => None,
            Answer::AlwaysTool => ask.tool_rule(),
            Answer::AlwaysTarget => ask.target_rule(),
        };
        if let Some(rule) = rule.clone()
            && self.allowed.insert(rule.clone())
        {
            self.learned.push(rule);
        }

        let decision = match (answer, rule.is_some()) {
            (Answer::No, _) => PermissionDecision::Deny,
            (_, true) => PermissionDecision::AllowAlways,
            (_, false) => PermissionDecision::Allow,
        };
        self.produce(Event::PermissionResponse {
            id: ask.id,
            decision,
            message: None,
        });
        self.scroll_to_tail();
    }

    /// Answers the prompt the transcript is asking with the operator's own
    /// words instead of one of the numbered answers.
    ///
    /// The call is refused and the words go with the refusal: the agent asked
    /// to run something and was told something else, and a backend hands a
    /// refusal's message to the agent as the call's result. Sending the words
    /// as a new prompt instead would leave the call waiting and start a turn
    /// behind it.
    pub fn answer_saying(&mut self, words: &str) {
        let Some(ask) = self.asks.front() else {
            return;
        };
        let words = words.trim();
        if words.is_empty() {
            return;
        }
        let id = ask.id.clone();
        self.produce(Event::PermissionResponse {
            id,
            decision: PermissionDecision::Deny,
            message: Some(words.to_owned()),
        });
        self.scroll_to_tail();
    }

    /// The rules the operator made since the last call, oldest first.
    pub fn take_rules(&mut self) -> Vec<Rule> {
        std::mem::take(&mut self.learned)
    }

    /// Says in the transcript that a standing answer will not outlive the
    /// session, so the operator is not surprised by the prompt returning.
    pub fn not_remembered(&mut self, rule: &Rule, error: &str) {
        self.push(Entry {
            kind: EntryKind::Failure,
            head: "not saved".to_owned(),
            meta: rule.to_string(),
            body: format!(
                "The rule holds for this session and was not written to the config, so \
                 the prompt comes back next time: {error}"
            ),
            streaming: false,
            at: self.at,
            calls: Vec::new(),
            agent: None,
        });
    }

    /// Says in the transcript that a rule the operator just made was kept
    /// where the next session reads it, and will not be used there: the file
    /// it went into is one nobody trusted, and an untrusted config answers no
    /// prompt.
    pub fn kept_until_trusted(&mut self, rule: &Rule) {
        self.push(Entry {
            kind: EntryKind::Notice,
            head: "not kept".to_owned(),
            meta: rule.to_string(),
            body: "The rule holds for this session. The repository's config it is kept in is \
                   not trusted, so the next session does not use it until you run \
                   `niobe trust`."
                .to_owned(),
            streaming: false,
            at: self.at,
            calls: Vec::new(),
            agent: None,
        });
    }

    /// Says in the transcript that a decision never reached the backend, so a
    /// turn that is still waiting is not read as one that was answered.
    pub fn not_answered(&mut self, error: &str) {
        self.push(Entry {
            kind: EntryKind::Failure,
            head: "not answered".to_owned(),
            meta: String::new(),
            body: format!(
                "What you just decided did not reach the backend, so the call it gates is \
                 still waiting there: {error}"
            ),
            streaming: false,
            at: self.at,
            calls: Vec::new(),
            agent: None,
        });
        self.scroll_to_tail();
    }

    /// Says in the transcript that an event could not be kept, so the operator
    /// does not find out from a resumed session that is missing it.
    ///
    /// A store that refuses one event refuses the next, and a streamed reply is
    /// hundreds of events: the refusals until the store keeps one again are one
    /// entry, counted, rather than one each burying the reply they came from.
    pub fn not_kept(&mut self, error: &str) {
        let body = format!(
            "What happened since is on screen but not in the session store, so a \
             resumed session will not show it: {error}"
        );
        if let Some(unsaved) = &mut self.unsaved {
            unsaved.count = unsaved.count.saturating_add(1);
            if let Some(entry) = self.entries.get_mut(unsaved.entry) {
                entry.meta = events(unsaved.count);
                entry.body = body;
            }
            return;
        }
        self.unsaved = Some(Unsaved {
            entry: self.entries.len(),
            count: 1,
        });
        self.push(Entry {
            kind: EntryKind::Failure,
            head: "not saved".to_owned(),
            meta: events(1),
            body,
            streaming: false,
            at: self.at,
            calls: Vec::new(),
            agent: None,
        });
    }

    /// Says in the transcript that the journal kept an event again after
    /// refusing, and how many it refused, so the gap a resumed session will
    /// have is known to be closed and how wide it is.
    pub fn kept(&mut self) {
        let Some(unsaved) = self.unsaved.take() else {
            return;
        };
        self.push(Entry {
            kind: EntryKind::Notice,
            head: "saved again".to_owned(),
            meta: format!("{} not saved", events(unsaved.count)),
            body: "The session store is keeping events again, from this one on.".to_owned(),
            streaming: false,
            at: self.at,
            calls: Vec::new(),
            agent: None,
        });
    }

    /// Says in the transcript that a change the operator made never reached the
    /// backend, so a session that is still running as it was is not read as one
    /// that moved.
    pub fn not_changed(&mut self, what: &str, error: &str) {
        self.push(Entry {
            kind: EntryKind::Failure,
            head: "not changed".to_owned(),
            meta: what.to_owned(),
            body: format!(
                "The backend did not take that change, so the session is still running as \
                 it was: {error}"
            ),
            streaming: false,
            at: self.at,
            calls: Vec::new(),
            agent: None,
        });
        self.scroll_to_tail();
    }

    /// The same shell, with a ceiling on what the session may spend.
    #[must_use]
    pub fn with_budget(mut self, budget_usd: f64) -> Self {
        self.budget_usd = Some(budget_usd);
        self
    }

    /// The same shell, reading the moments its events name — when a window
    /// comes back — in the timezone this clock keeps.
    ///
    /// The event loop reads the machine's timezone once and hands it over;
    /// without it the shell draws no local times at all, rather than times in
    /// a timezone nobody chose.
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = Some(clock);
        self
    }

    /// The moment `seconds` past the Unix epoch names, on the machine's own
    /// calendar and clock.
    ///
    /// A conversion, not a reading of the clock: the draw calls it to say when
    /// a window the backend timed comes back, and what it gets back does not
    /// depend on when it was called.
    ///
    /// `None` for a time past what the clock can hold, which a backend, a
    /// stored session or a replayed log can name: it is a moment nothing can
    /// say anything about.
    pub fn moment(&self, seconds: u64) -> Option<Stamp> {
        let at = std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(seconds))?;
        Some(match &self.clock {
            Some(clock) => clock.at(at),
            None => Stamp::new(at, None),
        })
    }

    /// The same shell, able to value the tokens a backend reported no cost
    /// for, so that a turn in flight shows a figure rather than nothing.
    #[must_use]
    pub fn with_prices(mut self, prices: Box<dyn Prices>) -> Self {
        self.prices = Some(prices);
        self
    }

    /// What this shell values unpriced tokens with, where it was given
    /// anything to value them with.
    pub fn prices(&self) -> Option<&dyn Prices> {
        self.prices.as_deref()
    }

    /// The most this session may spend, where the operator set a budget.
    pub fn budget(&self) -> Option<f64> {
        self.budget_usd
    }

    /// Says once when the session has spent most of its budget.
    ///
    /// Called by the event loop and never by [`App::apply`], for the reason
    /// [`App::settle_rules`] is: folding a recorded session would warn about
    /// money that was spent under a budget this run knows nothing about.
    pub fn settle_budget(&mut self) {
        let Some(budget) = self.budget_usd else {
            return;
        };
        if self.budget_warned || budget <= 0.0 {
            return;
        }
        // The same labelled figure the Usage pane draws: a floor or an
        // estimate past the line is past the line, and a cost nobody reported
        // is no reason to warn.
        let Some((spent, usd)) = crate::ui::known_spend(&self.session, self.prices()) else {
            return;
        };
        if usd < budget * BUDGET_WARNING {
            return;
        }

        self.budget_warned = true;
        self.push(Entry {
            kind: EntryKind::Notice,
            head: "budget".to_owned(),
            meta: format!("{spent} of {}", crate::ui::dollars(budget)),
            body: "Most of this session's budget is spent. The backend stops the session \
                   when the budget is reached, and it checks between turns rather than \
                   inside one, so the session can finish above the figure by what the turn \
                   that crosses the line costs."
                .to_owned(),
            streaming: false,
            at: self.at,
            calls: Vec::new(),
            agent: None,
        });
        self.scroll_to_tail();
    }

    /// The model list the operator opened, while it is open.
    pub fn picking(&self) -> Option<&Picker> {
        self.picking.as_ref()
    }

    /// The same shell, asking whether to trust the repository's config and
    /// quitting on the answer, which [`App::trust_answer`] then holds.
    #[must_use]
    pub fn asking_trust(mut self, question: trust::Question) -> Self {
        self.trusting = Some(trust::Asking::new(question));
        self
    }

    /// The question whether to trust the repository's config, while it is up.
    pub fn trusting(&self) -> Option<&trust::Asking> {
        self.trusting.as_ref()
    }

    /// How the question whether to trust the repository's config was
    /// answered; `None` until it has been, and where the operator quit
    /// instead.
    pub fn trust_answer(&self) -> Option<trust::Answer> {
        self.trusted
    }

    /// One key, while the trust question is up.
    ///
    /// Held to the same guard as a permission question, and for a sharper
    /// reason: a session is often started and typed into straight away, and
    /// an Enter meant for the first prompt must not put a file in force that
    /// nobody has read. A key read before the question was first drawn was
    /// typed at a shell that was not asking anything.
    fn on_trust_key(&mut self, key: ratatui::crossterm::event::KeyEvent) {
        if self.now.is_none() {
            self.hint = Some(TOO_SOON_HINT.to_owned());
            return;
        }
        if self.too_small {
            self.hint = Some(TOO_SMALL_HINT.to_owned());
            return;
        }
        if self.too_soon_to_answer() {
            return;
        }
        let Some(asking) = self.trusting.as_mut() else {
            return;
        };
        if let Some(answer) = asking.on_key(key.code) {
            self.trusting = None;
            self.trusted = Some(answer);
            self.quit();
        }
    }

    /// Opens the model list, or says why there is none to open.
    fn pick_model(&mut self) {
        let models = self
            .profile
            .as_ref()
            .map(|profile| profile.models.clone())
            .unwrap_or_default();
        if models.is_empty() {
            self.hint = Some(
                "F4 Model — this profile names no models; add `models = [\"…\"]` to it in \
                 the config"
                    .to_owned(),
            );
            return;
        }

        let at = self
            .session
            .model()
            .and_then(|current| models.iter().position(|model| model == current))
            .unwrap_or(0);
        self.picking = Some(Picker {
            purpose: Purpose::Model,
            options: models,
            at,
        });
    }

    /// Opens the list of effort levels, where the backend takes `/effort`.
    ///
    /// The cursor starts on the first: the backend does not report the level
    /// in force, so no row can be marked as it.
    fn pick_effort(&mut self) {
        if let Err(why) = self.can_send("effort") {
            self.hint = Some(why);
            return;
        }
        self.picking = Some(Picker {
            purpose: Purpose::Effort,
            options: EFFORTS.iter().map(|level| (*level).to_owned()).collect(),
            at: 0,
        });
    }

    /// Opens the list of themes, on the one in force.
    fn pick_theme(&mut self) {
        let options: Vec<String> = crate::theme::THEMES
            .iter()
            .map(|theme| theme.name.to_owned())
            .collect();
        let at = options
            .iter()
            .position(|name| *name == self.theme.name)
            .unwrap_or(0);
        self.picking = Some(Picker {
            purpose: Purpose::Theme,
            options,
            at,
        });
    }

    /// What the open list marks as in force, if it can know.
    pub fn picked(&self) -> Option<&str> {
        match self.picking.as_ref()?.purpose {
            Purpose::Model => self.session.model(),
            Purpose::Effort => None,
            Purpose::Theme => Some(self.theme.name),
        }
    }

    /// One key, while a list is up.
    ///
    /// Anything that is not a move, a choice or a way out is swallowed: the
    /// list is a question, and a key that typed into the composer behind it
    /// would be a key nobody meant.
    fn on_pick_key(&mut self, key: ratatui::crossterm::event::KeyEvent) {
        use ratatui::crossterm::event::KeyCode;

        let Some(picker) = self.picking.as_mut() else {
            return;
        };
        let last = picker.options.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => picker.at = picker.at.saturating_sub(1),
            KeyCode::Down => picker.at = picker.at.saturating_add(1).min(last),
            KeyCode::Esc | KeyCode::F(4) => self.picking = None,
            KeyCode::Enter => {
                let purpose = picker.purpose;
                let chosen = picker.options.get(picker.at).cloned();
                self.picking = None;
                if let Some(chosen) = chosen {
                    self.choose(purpose, chosen);
                }
            }
            _ => {}
        }
    }

    /// Does what choosing `chosen` from a list for `purpose` does.
    fn choose(&mut self, purpose: Purpose, chosen: String) {
        match purpose {
            Purpose::Model => self.produce(Event::ModelSelected { model: chosen }),
            Purpose::Effort => self.send_command("effort", Some(&chosen)),
            Purpose::Theme => {
                if let Some(theme) = Theme::by_name(&chosen) {
                    self.set_theme(theme);
                }
            }
        }
    }

    /// The menu that is open, and the item its cursor is on.
    pub fn menu(&self) -> Option<crate::menu::Open> {
        self.menu
    }

    /// What the shell is saying at length, while it is up.
    pub fn sheet(&self) -> Option<&Sheet> {
        self.sheet.as_ref()
    }

    /// Whether `pane` of the right-hand stack is drawn when there is room for
    /// it: the View menu hides and shows each.
    pub fn shows(&self, pane: crate::menu::SidePane) -> bool {
        !self.hidden.contains(&pane)
    }

    /// The same shell, knowing where the files it can name or open are.
    #[must_use]
    pub fn with_places(mut self, places: Places) -> Self {
        self.places = places;
        self
    }

    /// Where the last frame drew the menu bar and the F-key bar, so a click
    /// on either presses what is drawn there.
    pub fn drew_bars(&mut self, menu: ratatui::layout::Rect, fkeys: ratatui::layout::Rect) {
        self.bars = Some((menu, fkeys));
    }

    /// Where the last frame drew the open menu's list, border included.
    pub fn drew_menu_list(&mut self, at: Option<ratatui::layout::Rect>) {
        self.menu_list = at;
    }

    /// Told by the draw how far the open sheet can scroll, so a key past its
    /// end does not leave a scroll the next key has to undo first.
    pub(crate) fn measured_sheet(&mut self, max_scroll: usize) {
        if let Some(sheet) = self.sheet.as_mut() {
            sheet.scroll = sheet.scroll.min(max_scroll);
        }
    }

    /// Told by the draw that it did not draw `pane`: nothing may scroll it
    /// with the mouse, and the keyboard cannot stay on it.
    pub(crate) fn pane_not_drawn(&mut self, pane: Pane) {
        self.scroller_mut(pane).area = None;
        if self.focus == Focus::Pane(pane) {
            self.focus = Focus::Session;
        }
    }

    /// Whether `action` can do what it is named for in this session, which is
    /// what the menu draws an item dimmed by. One that cannot still says why
    /// when it is chosen.
    pub fn can(&self, action: crate::menu::Action) -> bool {
        use crate::menu::Action;

        match action {
            Action::NewSession => self.listed("clear"),
            Action::Compact => self.listed("compact"),
            Action::Doctor => self.listed("doctor"),
            Action::Mcp => self.listed("mcp"),
            Action::Effort => self.listed("effort"),
            Action::Rewind => self.listed("rewind"),
            Action::Hooks => self.listed("hooks"),
            Action::SignIn => false,
            Action::Resume => self.remembers,
            Action::Memory => self.places.memory.is_some(),
            Action::SwitchModel => self
                .profile
                .as_ref()
                .is_some_and(|profile| !profile.models.is_empty()),
            Action::Stop => self.working(),
            Action::Commands => !self.session.commands().is_empty(),
            Action::About
            | Action::History
            | Action::Settings
            | Action::Permissions
            | Action::Quit
            | Action::Export
            | Action::AddFile
            | Action::SubAgents
            | Action::CycleMode
            | Action::Usage
            | Action::Find
            | Action::Diff
            | Action::GroupByAgent
            | Action::Pane(_)
            | Action::Theme
            | Action::Shortcuts
            | Action::ReleaseNotes
            | Action::ReportBug => true,
        }
    }

    /// Does what a menu item or an F-key names.
    pub fn perform(&mut self, action: crate::menu::Action) {
        use crate::menu::Action;

        self.menu = None;
        match action {
            Action::About => self.sheet = Some(self.about()),
            Action::Settings => self.sheet = Some(self.settings()),
            Action::Permissions => self.sheet = Some(self.permissions()),
            Action::SignIn => self.hint = Some(self.sign_in_hint()),
            Action::Doctor => self.send_command("doctor", None),
            Action::Quit => self.quit(),
            Action::NewSession => self.send_command("clear", None),
            Action::Compact => self.send_command("compact", None),
            Action::Resume => match self.remembers {
                true => self.open_history(View::Sessions),
                false => self.hint = Some(NO_RECORD_HINT.to_owned()),
            },
            Action::History => self.open_history(View::Prompts),
            Action::Rewind => self.send_or_say(
                "rewind",
                "The backend lists no /rewind to a session niobe drives, so it has no \
                 checkpoint to go back to",
            ),
            Action::Stop => match self.working() {
                true => {
                    self.focus = Focus::Session;
                    self.stop_turn();
                }
                false => self.hint = Some("Nothing is running to stop".to_owned()),
            },
            Action::Export => self.export(),
            Action::Memory => self.open_memory(),
            Action::AddFile => self.start_mention(),
            Action::Mcp => self.send_command("mcp", None),
            Action::SubAgents => self.show_sub_agents(),
            Action::Hooks => self.send_or_say(
                "hooks",
                "The backend lists no /hooks to a session niobe drives; its hooks are set in \
                 its own settings files",
            ),
            Action::SwitchModel => self.pick_model(),
            Action::CycleMode => self.cycle_mode(),
            Action::Effort => self.pick_effort(),
            Action::Usage => self.hint = Some(self.cost_hint(self.read_at())),
            Action::Find => self.open_find(),
            Action::Diff => self.open_diffs(),
            Action::GroupByAgent => self.group_by_agent(),
            Action::Pane(pane) => self.toggle_pane(pane),
            Action::Theme => self.pick_theme(),
            Action::Shortcuts => self.sheet = Some(self.shortcuts()),
            Action::Commands => self.start_command(),
            Action::ReleaseNotes => self.handoffs.push(crate::desktop::Handoff::Open(format!(
                "{REPOSITORY}/releases"
            ))),
            Action::ReportBug => self.handoffs.push(crate::desktop::Handoff::Open(format!(
                "{REPOSITORY}/issues/new"
            ))),
        }
    }

    /// Whether the backend listed the command `name` to this session.
    fn listed(&self, name: &str) -> bool {
        self.attached
            && self
                .session
                .commands()
                .iter()
                .any(|command| command.name == name)
    }

    /// Whether the backend's command `name` can be sent now, and if not, why,
    /// in words the operator reads.
    ///
    /// Not while a turn runs: the backend would hold it until the turn ends,
    /// and a `/clear` that lands after a turn the operator then went on
    /// steering would clear what they meant to keep.
    fn can_send(&self, name: &str) -> Result<(), String> {
        if !self.attached {
            return Err(format!(
                "/{name} goes to the backend, and this session is not attached to one"
            ));
        }
        if !self.listed(name) {
            return Err(format!(
                "The backend does not offer /{name} to this session"
            ));
        }
        if self.working() {
            return Err(format!(
                "/{name} waits for the running turn: let it end, or Esc to stop it"
            ));
        }
        Ok(())
    }

    /// Sends the backend's command `name`, with `argument` after it, as the
    /// turn it runs as, or says why it cannot be sent.
    fn send_command(&mut self, name: &str, argument: Option<&str>) {
        if let Err(why) = self.can_send(name) {
            self.hint = Some(why);
            return;
        }
        let text = match argument {
            Some(argument) => format!("/{name} {argument}"),
            None => format!("/{name}"),
        };
        // A turn with no images of its own, so the ones attached to the prompt
        // still being written stay with it.
        self.images.send_none();
        self.focus = Focus::Session;
        self.produce(Event::UserMessage { text });
        self.sent_here = true;
        self.scroll_to_tail();
    }

    /// Sends the backend's command `name` where it lists one, and says
    /// `otherwise` where it does not.
    fn send_or_say(&mut self, name: &str, otherwise: &str) {
        match self.listed(name) {
            true => self.send_command(name, None),
            false => self.hint = Some(otherwise.to_owned()),
        }
    }

    /// Hides `pane` of the right-hand stack, or shows it again.
    fn toggle_pane(&mut self, pane: crate::menu::SidePane) {
        if !self.hidden.remove(&pane) {
            self.hidden.insert(pane);
        }
    }

    /// Gives the keyboard to the pane the sub-agents are listed in, or says
    /// why it cannot.
    fn show_sub_agents(&mut self) {
        let activity = Focus::Pane(Pane::Activity);
        if self.shows(crate::menu::SidePane::Activity) && self.on_screen(activity) {
            self.focus = activity;
            return;
        }
        self.hint = Some(
            "The sub-agents are listed in the Activity pane, which is not on screen: View \
             shows it, on a terminal at least 100 columns wide"
                .to_owned(),
        );
    }

    /// Starts naming a file at the cursor, with the list of files open.
    fn start_mention(&mut self) {
        let ratatui_textarea::DataCursor(row, column) = self.composer.cursor();
        let before = self
            .composer
            .lines()
            .get(row)
            .and_then(|line| line.chars().nth(column.checked_sub(1)?));
        let at = match before {
            Some(c) if !c.is_whitespace() => " @",
            _ => "@",
        };
        self.offer_closed = None;
        self.insert_into_composer(at);
    }

    /// Starts a backend command in an empty prompt, with the list of them
    /// open, or says why it cannot: a command is only read from the start of
    /// a prompt.
    fn start_command(&mut self) {
        if !self.composer_is_blank() {
            self.hint = Some(
                "A command is named at the start of a prompt; send or clear this one first"
                    .to_owned(),
            );
            return;
        }
        self.composer.clear();
        self.offer_closed = None;
        self.insert_into_composer("/");
    }

    /// Hands the repository's instructions to the agent to be opened, or says
    /// there are none.
    fn open_memory(&mut self) {
        match self.places.memory.clone() {
            Some(path) => self.handoffs.push(crate::desktop::Handoff::Open(path)),
            None => {
                let init = match self.listed("init") {
                    true => "; `/init` has the backend write one",
                    false => "",
                };
                self.hint = Some(format!(
                    "This repository has no CLAUDE.md at its root{init}"
                ));
            }
        }
    }

    /// Copies the transcript, as the last frame laid it out, to the
    /// clipboard.
    fn export(&mut self) {
        let lines = self.transcript_drawn.map_or(0, |(_, lines)| lines);
        if lines == 0 {
            self.hint = Some("There is no transcript to export yet".to_owned());
            return;
        }
        let text: Vec<String> = (0..lines)
            .map(|line| self.drawn.plain_line(line).unwrap_or_default())
            .collect();
        self.handoffs
            .push(crate::desktop::Handoff::Copy(text.join("\n")));
    }

    /// What the operator is told to do to sign in or out, which only the
    /// backend's own CLI does.
    fn sign_in_hint(&self) -> String {
        let backend = self
            .session
            .meta()
            .map(|meta| meta.backend.to_string())
            .or_else(|| {
                self.profile
                    .as_ref()
                    .map(|profile| profile.backend.to_string())
            })
            .unwrap_or_else(|| "claude".to_owned());
        format!(
            "Signing in is the {backend} CLI's own, and niobe never reads its credentials: \
             quit, run `{backend}` in a terminal and use /login there"
        )
    }

    /// What this program is.
    fn about(&self) -> Sheet {
        let (backend, model) = self.running_under();
        Sheet {
            title: "About".to_owned(),
            rows: vec![
                format!("niobe {}", env!("CARGO_PKG_VERSION")),
                String::new(),
                "A terminal coding agent that keeps you aware of what is being built and \
                 how: what the agent did, what it decided, what it touched, what it is \
                 doing now and what it cost."
                    .to_owned(),
                String::new(),
                format!("{:<10}{backend}", "Backend"),
                format!("{:<10}{model}", "Model"),
                String::new(),
                format!("Apache-2.0 · {REPOSITORY}"),
            ],
            link: Some(REPOSITORY.to_owned()),
            scroll: 0,
        }
    }

    /// The backend and profile, and the model, the session runs under, or an
    /// em dash for what nothing has said.
    fn running_under(&self) -> (String, String) {
        let backend = match (self.session.meta(), self.profile.as_ref()) {
            (Some(meta), _) if !meta.profile.is_empty() => {
                format!("{} · {}", meta.backend, meta.profile)
            }
            (Some(meta), _) => meta.backend.to_string(),
            (None, Some(profile)) => format!("{} · {}", profile.backend, profile.name),
            (None, None) => "—".to_owned(),
        };
        let model = self
            .session
            .model()
            .or_else(|| self.session.meta().map(|meta| meta.model.as_str()))
            .unwrap_or("—")
            .to_owned();
        (backend, model)
    }

    /// What the session runs under, and the files that set it.
    fn settings(&self) -> Sheet {
        let (backend, model) = self.running_under();
        let mode = self
            .session
            .mode()
            .map_or_else(|| "—".to_owned(), |mode| mode.to_string());
        let mut rows = vec![
            format!("{:<10}{backend}", "Profile"),
            format!("{:<10}{model}", "Model"),
            format!("{:<10}{mode}", "Mode"),
            format!("{:<10}{}", "Theme", self.theme.name.to_lowercase()),
            String::new(),
            "Config files, the later over the earlier:".to_owned(),
        ];
        rows.extend(
            self.places
                .config_files
                .iter()
                .map(|file| match file.exists {
                    true => format!("  {}", file.path),
                    false => format!("  {} — not there", file.path),
                }),
        );
        if self.places.config_files.is_empty() {
            rows.push("  none was looked for".to_owned());
        }
        rows.push(String::new());
        rows.push(
            "niobe reads them as a session starts, so a change takes effect in the next one."
                .to_owned(),
        );
        Sheet {
            title: "Settings".to_owned(),
            rows,
            link: self
                .places
                .config_files
                .iter()
                .find(|file| file.openable)
                .map(|file| file.path.clone()),
            scroll: 0,
        }
    }

    /// The standing answers permission prompts are answered with.
    fn permissions(&self) -> Sheet {
        let mode = self
            .session
            .mode()
            .map_or_else(|| "—".to_owned(), |mode| mode.to_string());
        let mut rows = vec![
            format!("{:<10}{mode}", "Mode"),
            String::new(),
            "Allowed without asking:".to_owned(),
        ];
        let rules = self.allowed.rules();
        match rules.is_empty() {
            true => rows.push(
                "  nothing yet — “always” on a permission prompt adds a rule here".to_owned(),
            ),
            false => rows.extend(rules.iter().map(|rule| format!("  {rule}"))),
        }
        rows.push(String::new());
        rows.push(
            "The rules are kept in the config's [permissions] table. A repository's own \
             table answers nothing until its file is trusted."
                .to_owned(),
        );
        Sheet {
            title: "Permissions".to_owned(),
            rows,
            link: None,
            scroll: 0,
        }
    }

    /// The keys the shell answers to.
    fn shortcuts(&self) -> Sheet {
        let mut keys: Vec<(&str, String)> = vec![
            ("Esc", "stop the running turn".to_owned()),
            (
                "Esc, then",
                "a digit presses its F-key (0 is F10); a menu's letter opens the menu".to_owned(),
            ),
            ("Alt+letter", "open a menu".to_owned()),
            ("Enter", "send the prompt".to_owned()),
            (self.newline_key(), "a new line in the prompt".to_owned()),
            ("⇧Tab", "the next permission mode".to_owned()),
            ("/", "a backend command or skill".to_owned()),
            (FIND_KEY, "search the transcript".to_owned()),
            ("@", "name a file".to_owned()),
            (
                "↑↓",
                "an earlier prompt, from the first or last row".to_owned(),
            ),
            (
                crate::history::SEARCH_KEY,
                "earlier prompts and sessions".to_owned(),
            ),
        ];
        if self.runs_commands {
            keys.push(("!", "a command for your own shell".to_owned()));
            keys.push((crate::shell::STOP_KEY, "stop that command".to_owned()));
        }
        keys.extend([
            ("Tab", "move the keyboard between the panes".to_owned()),
            (
                "PgUp PgDn",
                "scroll the pane that has the keyboard".to_owned(),
            ),
            (
                "↑↓ Enter",
                "in a side pane, fold the section under the cursor".to_owned(),
            ),
            (
                "a",
                "in a side pane, group sub-agent rows by agent".to_owned(),
            ),
            ("Ctrl+End", "back to the newest line".to_owned()),
            ("Ctrl+O", "fold runs of tool calls".to_owned()),
            ("Ctrl+T", "open every cut diff".to_owned()),
            ("Ctrl+V", "attach the image on the clipboard".to_owned()),
            ("drag", "copy what it covers".to_owned()),
            ("click", "open a link".to_owned()),
            ("Ctrl+Z", "suspend".to_owned()),
            ("Ctrl+C", "stop the turn; again, quit".to_owned()),
        ]);
        let mut rows: Vec<String> = keys
            .into_iter()
            .map(|(key, what)| format!("{key:<12}{what}"))
            .collect();
        rows.push(String::new());
        rows.push("F-keys:".to_owned());
        rows.push(
            crate::menu::FKEYS
                .iter()
                .map(|(digit, label, _)| format!("{digit} {label}"))
                .collect::<Vec<_>>()
                .join(" · "),
        );
        Sheet {
            title: "Shortcuts".to_owned(),
            rows,
            link: None,
            scroll: 0,
        }
    }

    /// Opens menu `at` on its first item.
    fn open_menu(&mut self, at: usize) {
        self.focus = Focus::Session;
        self.menu = Some(crate::menu::Open::at(at));
    }

    /// One key, while a menu is open: the menu has the keyboard whole.
    fn on_menu_key(&mut self, key: ratatui::crossterm::event::KeyEvent) {
        use ratatui::crossterm::event::KeyCode;

        let Some(open) = self.menu else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.menu = None,
            KeyCode::Left => self.menu = Some(open.left()),
            KeyCode::Right => self.menu = Some(open.right()),
            KeyCode::Up => self.menu = Some(open.up()),
            KeyCode::Down => self.menu = Some(open.down()),
            KeyCode::Enter => {
                if let Some(item) = open.item() {
                    self.perform(item.action);
                }
            }
            KeyCode::F(n) => {
                self.menu = None;
                if let Some(action) = crate::menu::fkey(n) {
                    self.perform(action);
                }
            }
            KeyCode::Char(c) => self.menu = Some(open.to_letter(c)),
            _ => {}
        }
    }

    /// One key, while a sheet is up: it has the keyboard whole, and changes
    /// nothing but its own scroll.
    fn on_sheet_key(&mut self, key: ratatui::crossterm::event::KeyEvent) {
        use ratatui::crossterm::event::KeyCode;

        let Some(sheet) = self.sheet.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.sheet = None,
            KeyCode::Up => sheet.scroll = sheet.scroll.saturating_sub(1),
            KeyCode::Down => sheet.scroll = sheet.scroll.saturating_add(1),
            KeyCode::PageUp => sheet.scroll = sheet.scroll.saturating_sub(SHEET_PAGE),
            KeyCode::PageDown => sheet.scroll = sheet.scroll.saturating_add(SHEET_PAGE),
            KeyCode::Char('o') => {
                if let Some(link) = sheet.link.clone() {
                    self.handoffs.push(crate::desktop::Handoff::Open(link));
                }
            }
            KeyCode::F(n) => {
                self.sheet = None;
                if let Some(action) = crate::menu::fkey(n) {
                    self.perform(action);
                }
            }
            _ => {}
        }
    }

    /// A left click on the menus, the F-key bar or an open sheet. Returns
    /// whether the click was theirs.
    ///
    /// With a menu open every click is: one on an item runs it, one on a
    /// menu's name opens that menu or closes this one, and one anywhere else
    /// only closes it, as a menu does on a desktop — a click that closed the
    /// menu and also landed on what was under it would do something nobody
    /// aimed at.
    fn on_menu_click(&mut self, column: u16, row: u16) -> bool {
        let point = (column, row).into();
        let on_menu_bar = self.bars.is_some_and(|(bar, _)| bar.contains(point));
        if let Some(open) = self.menu {
            if let Some(list) = self.menu_list.filter(|list| list.contains(point)) {
                let item = usize::from(row.saturating_sub(list.y.saturating_add(1)));
                let inside = row > list.y && row + 1 < list.bottom();
                if let Some(item) = open.menu().items.get(item).filter(|_| inside) {
                    self.perform_clicked(item.action);
                }
                return true;
            }
            let switches = on_menu_bar && !self.question_waits();
            self.menu = match crate::menu::title_at(column).filter(|_| switches) {
                Some(at) if at != open.menu => Some(crate::menu::Open::at(at)),
                _ => None,
            };
            return true;
        }
        if self.sheet.is_some() {
            self.sheet = None;
            return true;
        }
        if on_menu_bar {
            match crate::menu::title_at(column) {
                Some(_) if self.question_waits() => {
                    self.hint = Some(QUESTION_FIRST_HINT.to_owned());
                }
                Some(at) => self.open_menu(at),
                None => {}
            }
            return true;
        }
        if let Some((_, fkeys)) = self.bars.filter(|(_, fkeys)| fkeys.contains(point)) {
            if let Some(action) = crate::menu::fkey_at(fkeys.width, column - fkeys.x) {
                self.perform_clicked(action);
            }
            return true;
        }
        false
    }

    /// Whether a permission question, or the question whether to trust the
    /// repository's config, waits for an answer.
    fn question_waits(&self) -> bool {
        self.trusting.is_some() || self.asking().is_some()
    }

    /// Does what a clicked menu item or F-key names, unless a question waits
    /// and it is neither quitting nor stopping the turn.
    ///
    /// A key goes to the question ahead of the bars, but a click reaches
    /// them past it: a list or the search it opened would then be drawn
    /// over the question, and what the operator typed at it would land on
    /// whichever of the two takes the keys. Quitting and stopping are
    /// always available, as their keys are.
    fn perform_clicked(&mut self, action: crate::menu::Action) {
        use crate::menu::Action;

        if self.question_waits() && !matches!(action, Action::Quit | Action::Stop) {
            self.menu = None;
            self.hint = Some(QUESTION_FIRST_HINT.to_owned());
            return;
        }
        self.perform(action);
    }

    /// Moves the session to the next mode in the cycle.
    ///
    /// A session no backend has reported a mode for cycles from `ask`, which is
    /// what the bridge spawns the CLI in: one keypress then lands where it
    /// would have from a mode that had been reported.
    fn cycle_mode(&mut self) {
        let mode = self.session.mode().unwrap_or(Mode::Ask).next();
        self.produce(Event::ModeSelected { mode });
    }

    fn streaming_agent_entry(&mut self) -> Option<&mut Entry> {
        // A sub-agent's rows land while the session is still writing, and do
        // not end its reply: the reply keeps its place above them rather than
        // being started again below. Nor does the store refusing the reply's
        // first fragment, or every fragment after it would start a reply of
        // its own under the failure.
        let unsaved = self.unsaved.as_ref().map(|unsaved| unsaved.entry);
        match self
            .entries
            .iter_mut()
            .enumerate()
            .rev()
            .find(|(at, entry)| entry.agent.is_none() && Some(*at) != unsaved)
            .map(|(_, entry)| entry)
        {
            Some(entry) if entry.kind == EntryKind::Agent && entry.streaming => Some(entry),
            _ => None,
        }
    }

    pub(crate) fn agent_name(&self) -> String {
        self.session
            .meta()
            .map(|meta| meta.backend.as_str().to_owned())
            .unwrap_or_else(|| "agent".to_owned())
    }

    /// The same shell, under the profile the operator selected.
    #[must_use]
    pub fn with_profile(mut self, profile: SelectedProfile) -> Self {
        self.profile = Some(profile);
        self
    }

    /// The same shell, drawn in `theme` at the terminal's depth.
    ///
    /// The composer is a widget that holds its own styles rather than being
    /// handed them at draw time, so it is repainted here; everything else
    /// reads [`App::theme`] as it draws.
    #[must_use]
    pub fn with_theme(mut self, theme: Theme) -> Self {
        self.set_theme(theme);
        self
    }

    /// The same shell on a terminal that draws `depth` colours: every theme,
    /// the one in force and each the theme list switches to, is drawn at it.
    #[must_use]
    pub fn with_depth(mut self, depth: Depth) -> Self {
        self.depth = depth;
        self.set_theme(self.theme);
        self
    }

    fn set_theme(&mut self, theme: Theme) {
        let theme = theme.at(self.depth);
        self.theme = theme;
        paint_composer(&mut self.composer, &theme);
        if let Some(find) = self.find.as_mut() {
            paint_composer(&mut find.query, &theme);
        }
    }

    /// The same shell, with something to run the operator's `!` commands:
    /// the event loop is handed a [`crate::shell::Shell`] that runs them.
    #[must_use]
    pub fn runs_commands(mut self) -> Self {
        self.runs_commands = true;
        self
    }

    /// The same shell, on a terminal that tells Shift+Enter from Enter, so the
    /// bar can name it as the key that opens a line.
    ///
    /// Shift+Enter opens a line whether or not this is set — where the
    /// terminal cannot tell it apart it arrives as Enter and sends, which is
    /// why only a terminal that said it can, or has sent one, is told to press
    /// it.
    #[must_use]
    pub fn reports_shift_enter(mut self) -> Self {
        self.reports_shift_enter = true;
        self
    }

    /// The key that opens a new line in the composer on this terminal:
    /// Shift+Enter where the terminal can report it, and Ctrl+J, which every
    /// terminal sends as a byte of its own, where it cannot. Alt+Enter is not
    /// named: whether it arrives at all is a setting of the terminal's.
    pub fn newline_key(&self) -> &'static str {
        if self.reports_shift_enter {
            "Shift+Enter"
        } else {
            "Ctrl+J"
        }
    }

    /// Whether this terminal sends Shift+Enter as Enter, so that the operator
    /// who reaches for it to open a line sends the prompt instead.
    pub fn sends_enter_for_shift_enter(&self) -> bool {
        !self.reports_shift_enter
    }

    /// The same shell, opening on a line from the shell itself.
    ///
    /// For what the operator has to know before the first prompt and would
    /// never see printed: the shell draws on the alternate screen, so anything
    /// said before it takes the terminal is gone the moment it does. Not
    /// session content, so nothing produced here is recorded — it describes
    /// the run, not the conversation.
    #[must_use]
    pub fn with_notice(mut self, head: &str, meta: &str, body: &str) -> Self {
        self.push(Entry {
            kind: EntryKind::Notice,
            head: head.to_owned(),
            meta: meta.to_owned(),
            body: body.to_owned(),
            streaming: false,
            at: self.at,
            calls: Vec::new(),
            agent: None,
        });
        self
    }

    /// The same shell, with a backend listening for what the operator sends.
    #[must_use]
    pub fn attached(mut self) -> Self {
        self.attached = true;
        self
    }

    /// Whether a backend is listening.
    pub fn is_attached(&self) -> bool {
        self.attached
    }

    /// What the session is about: the title the backend gave it, or else the
    /// first thing the operator asked, as [`SessionState::caption`] has it —
    /// unless that first prompt never reached the backend. `None` where
    /// nothing says yet.
    pub fn caption(&self) -> Option<String> {
        match self.first_prompt_unsent && self.session.title().is_none() {
            true => None,
            false => self.session.caption(),
        }
    }

    /// Where the session is running.
    pub fn repo(&self) -> &Repo {
        &self.repo
    }

    /// Whether `section` is folded away.
    pub fn folded(&self, section: Section) -> bool {
        self.folded.contains(&section)
    }

    /// Folds `section` away, or opens it again.
    pub fn fold(&mut self, section: Section) {
        if !self.folded.remove(&section) {
            self.folded.insert(section);
        }
        // What is above the viewport changed height, so an offset measured
        // against the old height would leave the pane scrolled past its end
        // until the next wheel notch. Only the pane the section is in moved.
        self.scroller_mut(section.pane()).scroll = 0;
    }

    fn scroller(&self, pane: Pane) -> &Scroller {
        match pane {
            Pane::Changes => &self.changes,
            Pane::Activity => &self.activity,
        }
    }

    fn scroller_mut(&mut self, pane: Pane) -> &mut Scroller {
        match pane {
            Pane::Changes => &mut self.changes,
            Pane::Activity => &mut self.activity,
        }
    }

    /// How far `pane` is scrolled, in rows.
    pub fn pane_scroll(&self, pane: Pane) -> usize {
        let scroller = self.scroller(pane);
        scroller.scroll.min(scroller.max_scroll())
    }

    /// What the last frame drew `pane` as: where it is, how many rows it had
    /// to show and how many it could.
    ///
    /// Told by the draw, because the pane's height is the layout's answer and
    /// its content's height is the drawing's. Kept so that the wheel can be
    /// given to whichever pane the pointer is over.
    pub fn measured_pane(
        &mut self,
        pane: Pane,
        area: ratatui::layout::Rect,
        content: usize,
        rows: usize,
    ) {
        self.scroller_mut(pane).measured(area, content, rows);
    }

    /// Scrolls `pane`, never past either end.
    pub fn scroll_pane(&mut self, pane: Pane, lines: isize) {
        self.scroller_mut(pane).scroll_by(lines);
    }

    /// Which section header of `pane` the last frame drew on which of its
    /// rows, top to bottom. The draw builds the rows, so only it knows; the
    /// cursor walks these.
    pub fn measured_sections(&mut self, pane: Pane, headers: Vec<(Section, usize)>) {
        self.scroller_mut(pane).headers = headers;
    }

    /// The section of `pane` the cursor is on, while `pane` has the keyboard.
    ///
    /// `None` when another pane has it: a cursor drawn in a pane the arrows do
    /// not reach would say that they do.
    pub fn section_cursor(&self, pane: Pane) -> Option<Section> {
        if self.focus() != Focus::Pane(pane) {
            return None;
        }
        self.scroller(pane).cursor().map(|(_, section, _)| section)
    }

    /// Which pane has the keyboard.
    ///
    /// A question the operator is answering is in the session pane and takes
    /// the arrows and Enter, so while it holds the keyboard the session is
    /// where the focus is, whichever pane had it before. Put off, the
    /// question gives it back.
    pub fn focus(&self) -> Focus {
        match self.asking() {
            Some(_) if self.ask_focus != AskFocus::Deferred => Focus::Session,
            _ => self.focus,
        }
    }

    /// Gives the keyboard to the next pane on screen, round to the session.
    fn focus_next(&mut self) {
        let at = Focus::ORDER
            .iter()
            .position(|focus| *focus == self.focus)
            .unwrap_or(0);
        self.focus = (1..Focus::ORDER.len())
            .filter_map(|step| Focus::ORDER.get((at + step) % Focus::ORDER.len()))
            .copied()
            .find(|focus| self.on_screen(*focus))
            .unwrap_or(Focus::Session);
    }

    fn on_screen(&self, focus: Focus) -> bool {
        match focus {
            Focus::Session => true,
            Focus::Pane(pane) => self.scroller(pane).area.is_some(),
        }
    }

    /// What the last frame drew the session pane as, so the mouse can tell it
    /// from the panes beside it.
    pub fn measured_session(&mut self, area: ratatui::layout::Rect) {
        self.session_area = Some(area);
    }

    /// Told by a frame too narrow for the right-hand stack: its panes are not
    /// on screen, so none of them can be focused or scrolled by the mouse.
    pub fn right_stack_hidden(&mut self) {
        for pane in [Pane::Changes, Pane::Activity] {
            self.scroller_mut(pane).area = None;
        }
        if let Focus::Pane(_) = self.focus {
            self.focus = Focus::Session;
        }
    }

    /// Where the last frame drew the way back down to the newest line, or
    /// `None` when it drew none because the transcript was at its end.
    pub fn drew_jump(&mut self, at: Option<ratatui::layout::Rect>) {
        self.jump = at;
    }

    /// Whether the last frame drew the top row of the question waiting.
    pub fn drew_question_top(&mut self, drawn: bool) {
        self.question_top_drawn = drawn;
    }

    /// Whether the last frame was too small to draw the shell, and with it
    /// any question waiting.
    ///
    /// A question the window grows back over comes up as it does when it is
    /// first asked, waiting from then for a quiet keyboard: an Enter pressed
    /// at the blank window must not answer what appears under it.
    pub fn drew_too_small(&mut self, too_small: bool) {
        let shown = self.too_small && !too_small;
        self.too_small = too_small;
        if !shown || (self.asking().is_none() && self.trusting.is_none()) {
            return;
        }
        self.ask_quiet_since = self.latest_instant();
        if self.hint.as_deref() == Some(TOO_SMALL_HINT) {
            self.hint = None;
        }
    }

    /// The pane under a point on the screen, if the point is on one that
    /// scrolls.
    fn pane_at(&self, column: u16, row: u16) -> Option<Focus> {
        if let Some(pane) = [Pane::Changes, Pane::Activity]
            .into_iter()
            .find(|pane| self.scroller(*pane).holds(column, row))
        {
            return Some(Focus::Pane(pane));
        }
        self.session_area
            .filter(|area| area.contains((column, row).into()))
            .map(|_| Focus::Session)
    }

    /// Scrolls whichever pane has the keyboard, if `key` is one of the keys
    /// that scroll. Returns whether it was.
    fn scroll_key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> bool {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};

        let how = match (key.code, key.modifiers) {
            (KeyCode::PageUp, _) => Scroll::PageUp,
            (KeyCode::PageDown, _) => Scroll::PageDown,
            (KeyCode::Up, KeyModifiers::SHIFT) => Scroll::Up(1),
            (KeyCode::Down, KeyModifiers::SHIFT) => Scroll::Down(1),
            (KeyCode::Home, KeyModifiers::CONTROL) => Scroll::Head,
            (KeyCode::End, KeyModifiers::CONTROL) => Scroll::Tail,
            _ => return false,
        };
        self.scroll_focused(how);
        true
    }

    fn scroll_focused(&mut self, how: Scroll) {
        match self.focus() {
            Focus::Session => {
                let page = self.viewport_lines.max(1);
                match how {
                    Scroll::PageUp => self.scroll_up(page),
                    Scroll::PageDown => self.scroll_down(page),
                    Scroll::Up(lines) => self.scroll_up(lines),
                    Scroll::Down(lines) => self.scroll_down(lines),
                    Scroll::Head => self.scroll_to_head(),
                    Scroll::Tail => self.scroll_to_tail(),
                }
            }
            Focus::Pane(pane) => {
                let scroller = self.scroller_mut(pane);
                let page = isize::try_from(scroller.viewport.max(1)).unwrap_or(isize::MAX);
                let lines = |n: usize| isize::try_from(n).unwrap_or(isize::MAX);
                match how {
                    Scroll::PageUp => scroller.scroll_by(-page),
                    Scroll::PageDown => scroller.scroll_by(page),
                    Scroll::Up(n) => scroller.scroll_by(-lines(n)),
                    Scroll::Down(n) => scroller.scroll_by(lines(n)),
                    Scroll::Head => scroller.scroll = 0,
                    Scroll::Tail => scroller.scroll = scroller.max_scroll(),
                }
            }
        }
    }

    /// One key, while a right-hand pane has the keyboard: the arrows walk its
    /// section headers, Enter folds the one under the cursor, and Esc gives
    /// the keyboard back to the session. Returns whether the pane took it.
    fn on_pane_key(&mut self, pane: Pane, key: ratatui::crossterm::event::KeyEvent) -> bool {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};

        if key.modifiers != KeyModifiers::NONE {
            return false;
        }
        match key.code {
            KeyCode::Up => self.scroller_mut(pane).move_cursor(false),
            KeyCode::Down => self.scroller_mut(pane).move_cursor(true),
            KeyCode::Enter => self.fold_under_cursor(pane),
            KeyCode::Char('a') => self.group_by_agent(),
            KeyCode::Esc => self.focus = Focus::Session,
            _ => return false,
        }
        true
    }

    /// Folds the section under `pane`'s cursor, or opens it again.
    ///
    /// Unlike [`App::fold`], which puts the pane back at its top, this keeps
    /// the header in view: the operator is looking at it, and nothing above
    /// it changed height.
    fn fold_under_cursor(&mut self, pane: Pane) {
        let scroller = self.scroller_mut(pane);
        let Some((_, section, row)) = scroller.cursor() else {
            return;
        };
        scroller.cursor = Some(section);
        scroller.scroll = scroller.scroll.min(row);
        if !self.folded.remove(&section) {
            self.folded.insert(section);
        }
    }

    /// Replaces what the shell knows about the repository with a fresh read.
    ///
    /// Called from the event loop with whatever [`crate::watch::Watch`] has
    /// finished reading. A read that failed or has not come back yet hands over
    /// nothing at all, so what is on screen is the last read that worked rather
    /// than an emptied pane.
    pub fn set_repo(&mut self, repo: Repo) {
        if repo.files != self.repo.files {
            self.mention_index = crate::mention::Files::new(&repo.files);
        }
        self.repo = repo;
    }

    /// The profile the operator selected, if any.
    pub fn profile(&self) -> Option<&SelectedProfile> {
        self.profile.as_ref()
    }

    /// The palette the shell draws in.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// The fold every pane reads its numbers from.
    pub fn session(&self) -> &SessionState {
        &self.session
    }

    /// Every sub-agent the session spawned, in spawn order, running and
    /// finished alike.
    pub fn agents(&self) -> &[SubAgent] {
        &self.agents
    }

    /// What a sub-agent was spawned to do.
    pub fn agent_label(&self, id: &AgentId) -> Option<String> {
        self.agents
            .iter()
            .find(|agent| &agent.id == id)
            .map(|agent| agent.label.clone())
    }

    /// When a sub-agent was spawned, where the shell had a clock at the time.
    pub fn agent_spawned_at(&self, id: &AgentId) -> Option<Stamp> {
        self.agents
            .iter()
            .find(|agent| &agent.id == id)
            .and_then(|agent| agent.at)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The entries together with what the last draw made of them, for the
    /// draw to reuse.
    pub(crate) fn entries_to_draw(&mut self) -> (&[Entry], &mut crate::ui::DrawnEntries) {
        (&self.entries, &mut self.drawn)
    }

    /// The composer widget, for the draw.
    pub fn composer(&self) -> &TextArea<'static> {
        &self.composer
    }

    /// What the operator has typed and not sent.
    pub fn composed(&self) -> String {
        self.composer.lines().join("\n")
    }

    /// Whether the composer holds nothing a prompt could be made of. Spaces
    /// the operator cannot see must not turn the `!` that starts a command
    /// into text, or the next command goes to the agent.
    fn composer_is_blank(&self) -> bool {
        self.composer
            .lines()
            .iter()
            .all(|line| line.trim().is_empty())
    }

    /// The transient message under the transcript, if there is one.
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    /// Fits the empty composer's placeholder into `columns`, dropping what it
    /// advertises before what the bar holds beside it: the key that sets a
    /// mode where none is reported, and the key that opens a line where Enter
    /// would be sent for it, matter more than a reminder of what the composer
    /// can do.
    pub(crate) fn fit_placeholder(&mut self, columns: usize) {
        let said = match self.shell_mode {
            true => SHELL_PLACEHOLDER.to_owned(),
            false => placeholder(
                columns,
                Offers {
                    files: !self.repo.files.is_empty(),
                    shell: self.runs_commands,
                    commands: !self.session.commands().is_empty(),
                },
            ),
        };
        if self.composer.placeholder_text() != said {
            self.composer.set_placeholder_text(said);
        }
    }

    /// Whether the operator is typing a prompt into the composer, which is
    /// the only time a list is offered under what they type.
    fn typing_a_prompt(&self) -> bool {
        self.focus() == Focus::Session
            && !self.shell_mode
            && self.find.is_none()
            && self.picking.is_none()
            && self.browser.is_none()
            && !self
                .asking()
                .is_some_and(|_| self.ask_focus != AskFocus::Deferred)
    }

    /// The `@` word at the end of which the cursor sits, while the list of
    /// files for it is open: only while the operator is typing into the
    /// composer, and not for a word the list was closed for.
    fn mention(&self) -> Option<crate::mention::Mention> {
        if !self.typing_a_prompt() {
            return None;
        }
        let ratatui_textarea::DataCursor(row, column) = self.composer.cursor();
        crate::mention::at_cursor(self.composer.lines(), (row, column))
            .filter(|mention| self.offer_closed != Some((mention.row, mention.at)))
    }

    /// Forgets the word a list was closed under once the cursor is no longer
    /// in it, so that a word typed later in the same place is offered a list
    /// of its own.
    fn reopen_offers(&mut self) {
        if self.offer_closed.is_none() {
            return;
        }
        let ratatui_textarea::DataCursor(row, column) = self.composer.cursor();
        let lines = self.composer.lines();
        let word = crate::mention::at_cursor(lines, (row, column))
            .map(|mention| (mention.row, mention.at))
            .or_else(|| crate::slash::at_cursor(lines, (row, column)).map(|_| (0, 0)));
        if word != self.offer_closed {
            self.offer_closed = None;
        }
    }

    /// What has been typed after the `/` that opens the prompt, while the
    /// list of the backend's commands for it is open.
    fn slash(&self) -> Option<String> {
        if !self.typing_a_prompt() || self.offer_closed == Some((0, 0)) {
            return None;
        }
        let ratatui_textarea::DataCursor(row, column) = self.composer.cursor();
        crate::slash::at_cursor(self.composer.lines(), (row, column))
    }

    /// The backend's commands the `/` that opens the prompt could name, and
    /// which of them Enter would take. Empty where no list is open, which is
    /// also where the backend has listed none.
    pub fn offered_commands(&self) -> (Vec<&SlashCommand>, usize) {
        let Some(typed) = self.slash() else {
            return (Vec::new(), 0);
        };
        let commands = crate::slash::candidates(self.session.commands(), &typed, MENTION_ROWS);
        let selected = self.offer_selected.min(commands.len().saturating_sub(1));
        (commands, selected)
    }

    /// One key, while the list of the backend's commands is open. Returns
    /// whether the list took it: the same keys as the list of files.
    fn on_command_key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> bool {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};

        let (commands, selected) = self.offered_commands();
        let count = commands.len();
        let Some(chosen) = commands.get(selected).map(|command| command.name.clone()) else {
            return false;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Up, KeyModifiers::NONE) => {
                self.offer_selected = (selected + count - 1) % count;
            }
            (KeyCode::Down, KeyModifiers::NONE) => {
                self.offer_selected = (selected + 1) % count;
            }
            (KeyCode::Tab | KeyCode::Enter, KeyModifiers::NONE) => self.name_command(&chosen),
            (KeyCode::Esc, _) => self.offer_closed = Some((0, 0)),
            _ => return false,
        }
        true
    }

    /// Replaces what was typed after the opening `/` with `name`, and a space
    /// after it for what the command takes.
    fn name_command(&mut self, name: &str) {
        let Some(typed) = self.slash() else {
            return;
        };
        for _ in typed.chars() {
            self.composer.delete_char();
        }
        self.composer.insert_str(format!("{name} "));
        self.offer_selected = 0;
    }

    /// The files the list under the `@` word offers, likeliest first, and
    /// which of them Enter would take. Empty where no list is open.
    pub fn mention_files(&self) -> (Vec<&str>, usize) {
        let Some(mention) = self.mention() else {
            return (Vec::new(), 0);
        };
        let files = self
            .mention_index
            .candidates(&self.repo.files, &mention.typed, MENTION_ROWS);
        let selected = self.offer_selected.min(files.len().saturating_sub(1));
        (files, selected)
    }

    /// One key, while the list of files under an `@` word is open. Returns
    /// whether the list took it.
    ///
    /// The arrows move through the list, Tab and Enter put the file in the
    /// prompt, and Esc closes the list and leaves the word as typed. Every
    /// other key is typing, and goes to the composer.
    fn on_mention_key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> bool {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};

        let (files, selected) = self.mention_files();
        let count = files.len();
        let chosen = files.get(selected).map(|file| (*file).to_owned());
        let Some(chosen) = chosen else {
            return false;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Up, KeyModifiers::NONE) => {
                self.offer_selected = (selected + count - 1) % count;
            }
            (KeyCode::Down, KeyModifiers::NONE) => {
                self.offer_selected = (selected + 1) % count;
            }
            (KeyCode::Tab | KeyCode::Enter, KeyModifiers::NONE) => self.name_file(&chosen),
            (KeyCode::Esc, _) => {
                self.offer_closed = self.mention().map(|mention| (mention.row, mention.at));
            }
            _ => return false,
        }
        true
    }

    /// Replaces what was typed after the `@` with `file`, written as the
    /// backend reads a mention ([`crate::mention::written`]), and a space
    /// after it so the next word is not taken for more of the path.
    fn name_file(&mut self, file: &str) {
        let Some(mention) = self.mention() else {
            return;
        };
        for _ in mention.typed.chars() {
            self.composer.delete_char();
        }
        self.composer
            .insert_str(format!("{} ", crate::mention::written(file)));
        self.offer_selected = 0;
    }

    /// Whether the composer holds a command for the operator's own shell
    /// rather than a prompt for the agent.
    pub fn shell_mode(&self) -> bool {
        self.shell_mode
    }

    /// One key, while the composer holds a command. Returns whether it was
    /// the command's: Enter runs it, and Esc, or Backspace on nothing, goes
    /// back to writing a prompt. `!` on nothing goes back too and types
    /// itself, which is how a prompt starts with one. Everything else is
    /// typing.
    fn on_shell_key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> bool {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};

        if !self.shell_mode || self.focus() != Focus::Session {
            return false;
        }
        let empty = self.composer.is_empty();
        match (key.code, key.modifiers) {
            _ if opens_a_line(key) => return false,
            (KeyCode::Enter, _) => self.run_command(),
            (KeyCode::Esc, _) => self.shell_mode = false,
            (KeyCode::Backspace, _) if empty => self.shell_mode = false,
            (KeyCode::Char('!'), KeyModifiers::NONE | KeyModifiers::SHIFT) if empty => {
                self.shell_mode = false;
                self.composer.insert_char('!');
            }
            _ => return false,
        }
        true
    }

    /// Runs what is in the composer as a command: records it as a call that
    /// has started, and queues it for [`App::take_commands`]. Blank, it runs
    /// nothing and is cleared, so no invisible spaces stay behind to make
    /// the next `!` text.
    fn run_command(&mut self) {
        let command = self.composed();
        self.composer.clear();
        if command.trim().is_empty() {
            return;
        }
        self.shell_mode = false;
        self.commands_started = self.commands_started.saturating_add(1);
        let id = ToolCallId::new(format!(
            "shell-{}-{}",
            self.read_at(),
            self.commands_started
        ));
        self.produce(Event::ToolCallStart {
            id: id.clone(),
            name: crate::shell::OPERATOR_SHELL.to_owned(),
            input: command.clone(),
            summary: None,
            agent: None,
        });
        self.running_commands.push((id.clone(), command.clone()));
        self.commands.push((id, command));
        self.hint = Some(stop_hint());
        self.scroll_to_tail();
    }

    /// Where the operator asked for an image since the last call, oldest
    /// first, for the event loop to hand to a [`crate::images::Images`].
    pub fn take_image_requests(&mut self) -> Vec<Source> {
        self.images.take_asked()
    }

    /// Folds in how a fetch ended: an image is attached where the cursor is,
    /// as the placeholder that stands for it; a failure says why on the bar.
    ///
    /// A path that was pasted and is not an image to attach goes into the
    /// prompt as the path it named, since a paste of it would have put it
    /// there — without the quoting a terminal dropping it added.
    pub fn fetched(&mut self, fetched: Fetched) {
        self.images.fetched();
        match (fetched.image, fetched.source) {
            (Ok(image), _) => {
                let placeholder = self.images.attach(image);
                self.hint = None;
                self.insert_into_composer(&format!("{placeholder} "));
            }
            (Err(reason), Source::Clipboard) => {
                self.hint = Some(format!("No image attached: {reason}"));
            }
            (Err(reason), Source::File(path)) => {
                self.hint = Some(format!("{path} is not attached: {reason}"));
                self.insert_into_composer(&path);
            }
        }
    }

    /// The images of the oldest prompt sent and not yet handed to the
    /// backend, for the event loop to send with it. Taken once for every
    /// prompt sent, whether or not anything is attached to hear it.
    pub fn take_turn_images(&mut self) -> Vec<niobe_core::image::Image> {
        self.images.take_turn()
    }

    /// Whether an image the operator asked for has not come back yet.
    pub fn fetching_image(&self) -> bool {
        self.images.fetching()
    }

    /// Asks for the image at `source`, and says so until it arrives.
    fn attach_image(&mut self, source: Source) {
        self.hint = Some(match &source {
            Source::Clipboard => "Reading the image on the clipboard…".to_owned(),
            Source::File(path) => format!("Reading {path}…"),
        });
        self.images.ask(source);
    }

    /// Puts `text` in the composer where the cursor is, as typing would.
    fn insert_into_composer(&mut self, text: &str) {
        self.focus = Focus::Session;
        if self.composer.insert_str(text) {
            self.offer_selected = 0;
            self.reopen_offers();
        }
    }

    /// The commands the operator ran since the last call, oldest first, for
    /// the event loop to hand to a [`crate::shell::Shell`].
    pub fn take_commands(&mut self) -> Vec<(ToolCallId, String)> {
        std::mem::take(&mut self.commands)
    }

    /// Stops the newest command still running that the operator has not
    /// already stopped, by queueing it for [`App::take_stops`]; says so where
    /// there is none.
    fn stop_command(&mut self) {
        let newest = self
            .running_commands
            .iter()
            .rev()
            .map(|(id, _)| id)
            .find(|id| !self.stopping.contains(*id))
            .cloned();
        let Some(id) = newest else {
            self.hint = Some("No ! command is running".to_owned());
            return;
        };
        self.stopping.insert(id.clone());
        self.stops.push(id);
    }

    /// The commands the operator stopped since the last call, for the event
    /// loop to hand to a [`crate::shell::Shell`] to stop.
    pub fn take_stops(&mut self) -> Vec<ToolCallId> {
        std::mem::take(&mut self.stops)
    }

    /// Records how a command the operator ran ended, as the end of its call
    /// and, where it ran `cargo test`, the run it reported.
    ///
    /// A command the operator stopped ends as [`crate::shell::STOPPED`] and
    /// as a call that did not succeed, rather than with the signal that
    /// stopped it — the signal is how, the operator is why — and with no
    /// status: whatever `sh` exited with after the stop is how the shell
    /// ended, not how the command did, and an `exit 0` beside "stopped"
    /// would read as a command that succeeded.
    pub fn ran(&mut self, mut ran: crate::shell::Ran) {
        let command = match self
            .running_commands
            .iter()
            .position(|(id, _)| *id == ran.id)
        {
            Some(at) => self.running_commands.remove(at).1,
            None => String::new(),
        };
        if self.running_commands.is_empty() && self.hint.as_deref() == Some(&stop_hint()) {
            self.hint = None;
        }
        let stopped = self.stopping.remove(&ran.id);
        if stopped {
            ran.error = Some(crate::shell::STOPPED.to_owned());
            ran.exit_code = None;
        }
        let outcome = match ran.exit_code {
            Some(0) if !stopped => ToolOutcome::Ok,
            _ => ToolOutcome::Failed,
        };
        let tested = niobe_core::test_run::is_test_run(&command).then(|| {
            let whole = ran.whole.then_some(ran.output.as_str());
            let run = TestRunRecord::read(&command, &ran.output, whole, ran.exit_code);
            Event::TestRun {
                id: ran.id.clone(),
                counts: run.counts,
                exit_code: run.exit_code,
                failed: run.failed,
                failures: run.failures,
            }
        });
        self.produce(Event::ToolCallEnd {
            id: ran.id,
            name: crate::shell::OPERATOR_SHELL.to_owned(),
            input: command,
            output: ran.output,
            bytes: ran.bytes,
            outcome,
            summary: None,
            exit_code: ran.exit_code,
            error: ran.error,
        });
        if let Some(tested) = tested {
            self.produce(tested);
        }
    }

    /// Ends the call of every command still running, as stopped by the
    /// session ending, which is what stops it.
    pub fn abandon_commands(&mut self) {
        let running: Vec<ToolCallId> = self
            .running_commands
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        for id in running {
            self.ran(crate::shell::Ran {
                id,
                output: String::new(),
                bytes: 0,
                whole: false,
                exit_code: None,
                error: Some("stopped: the session ended before the command did".to_owned()),
            });
        }
    }

    /// Records that a command the operator ran could not be started, as a
    /// call that failed with the reason.
    pub fn not_run(&mut self, id: &ToolCallId, error: &str) {
        self.ran(crate::shell::Ran {
            id: id.clone(),
            output: String::new(),
            bytes: 0,
            whole: true,
            exit_code: None,
            error: Some(format!("not run: {error}")),
        });
    }

    /// What is being looked for in the transcript, while a search is open.
    pub fn finding(&self) -> Option<&TextArea<'static>> {
        self.find.as_ref().map(|find| &find.query)
    }

    /// The query as typed, while a search is open.
    pub(crate) fn find_query(&self) -> Option<String> {
        self.find.as_ref().map(|find| find.query.lines().concat())
    }

    /// Every place the last draw found the query, and which of them the view
    /// was moved to, while a search is open.
    pub(crate) fn find_marks(&self) -> Option<(&[crate::find::Found], Option<usize>)> {
        self.find
            .as_ref()
            .map(|find| (find.found.as_slice(), find.current))
    }

    /// Told by the draw where the query is in the transcript as it is drawn
    /// now.
    ///
    /// A query just typed starts on the match nearest the bottom of where the
    /// view was when the search opened, and on the first one where none is
    /// above it: the transcript is read upwards from where the operator was.
    pub(crate) fn found(&mut self, found: Vec<crate::find::Found>) {
        let viewport = self.viewport_lines;
        let Some(find) = self.find.as_mut() else {
            return;
        };
        let bottom = find.from.0.saturating_add(viewport);
        find.current = match find.current {
            _ if found.is_empty() => None,
            Some(at) => Some(at.min(found.len() - 1)),
            None => {
                find.revealed = false;
                Some(found.iter().rposition(|at| at.line < bottom).unwrap_or(0))
            }
        };
        find.found = found;
    }

    /// Moves the view to the current match, once for each time it changes:
    /// called by the draw once it has measured the transcript.
    pub(crate) fn reveal_found(&mut self) {
        let Some(find) = self.find.as_mut() else {
            return;
        };
        let line = match (find.revealed, find.current) {
            (false, Some(at)) => find.found.get(at).map(|found| found.line),
            _ => None,
        };
        find.revealed = true;
        if let Some(line) = line {
            self.reveal_line(line);
        }
    }

    /// Scrolls the transcript so `line` is on screen, in the middle of it
    /// where the view has to move at all.
    fn reveal_line(&mut self, line: usize) {
        let top = self.scroll();
        let rows = self.viewport_lines.max(1);
        if (top..top.saturating_add(rows)).contains(&line) {
            return;
        }
        self.scroll = line.saturating_sub(rows / 2).min(self.max_scroll());
        self.follow = self.scroll >= self.max_scroll();
    }

    /// Opens a search through the transcript, remembering where the view is.
    fn open_find(&mut self) {
        let mut query = TextArea::default();
        query.set_placeholder_text("Find in the transcript");
        paint_composer(&mut query, &self.theme);
        self.find = Some(Find {
            query,
            from: (self.scroll(), self.follow),
            found: Vec::new(),
            current: None,
            revealed: true,
        });
    }

    /// Closes the search and puts the view back where it was when it opened.
    fn close_find(&mut self) {
        if let Some(find) = self.find.take() {
            (self.scroll, self.follow) = find.from;
        }
    }

    /// The key as the shell acts on it: a digit after an Esc that did nothing
    /// else, or with Alt held, is the F-key of that number.
    ///
    /// Alt and a digit is the same two bytes as Esc and the digit arriving
    /// together, which is what a terminal that sends Option as Meta writes
    /// and what a quick Esc then digit can read as. Anything else after Esc
    /// is itself, and ends it: the digit is only the one key after.
    fn as_function_key(
        &mut self,
        key: ratatui::crossterm::event::KeyEvent,
    ) -> ratatui::crossterm::event::KeyEvent {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let escaped = std::mem::take(&mut self.escaped);
        let KeyCode::Char(digit) = key.code else {
            return key;
        };
        let stands_for =
            escaped && key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::ALT;
        match function_key_of(digit) {
            Some(n) if stands_for => KeyEvent {
                code: KeyCode::F(n),
                modifiers: KeyModifiers::NONE,
                ..key
            },
            Some(_) | None => key,
        }
    }

    /// One key, while a search is open. Returns whether the search took it.
    ///
    /// The F-keys, Shift+Tab, Ctrl+O and Ctrl+T still do what they do everywhere;
    /// every other key is the search's, so nothing typed at it lands in the
    /// composer behind it.
    fn on_find_key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> bool {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};

        let Some(find) = self.find.as_mut() else {
            return false;
        };
        let empty = find.query.is_empty();
        match (key.code, key.modifiers) {
            (KeyCode::F(_) | KeyCode::BackTab, _)
            | (KeyCode::Char('o' | 't'), KeyModifiers::CONTROL) => {
                return false;
            }
            (KeyCode::Esc, _) => self.close_find(),
            (KeyCode::Backspace, _) if empty => self.close_find(),
            (KeyCode::Enter | KeyCode::Up, _) => find.step(false),
            (KeyCode::Down, _) => find.step(true),
            (KeyCode::Tab, _) => {}
            _ => {
                if find.query.input(Input::from(key)) {
                    find.current = None;
                }
            }
        }
        true
    }

    /// Whether the transcript is pinned to its newest line.
    pub fn follows_tail(&self) -> bool {
        self.follow
    }

    /// Whether the event loop should stop.
    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Stops the event loop: a clean quit, or a SIGTERM turned into one.
    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    /// Whether the operator asked to suspend the shell since this was last
    /// asked. Raw mode reads Ctrl+Z as a key rather than letting the terminal
    /// turn it into SIGTSTP, so the loop is what stops the process for it.
    pub fn take_suspend(&mut self) -> bool {
        std::mem::take(&mut self.suspend)
    }

    /// Whether the operator asked the running turn to stop since this was
    /// last asked. The loop hands it to the backend, which ends the turn and
    /// keeps the session.
    pub fn take_interrupt(&mut self) -> bool {
        std::mem::take(&mut self.interrupt)
    }

    /// Asks the running turn to stop, once per turn.
    fn stop_turn(&mut self) {
        let turn = self.session.turns().len();
        if self.stop_asked_in != Some(turn) {
            self.stop_asked_in = Some(turn);
            self.interrupt = true;
        }
        self.hint = Some(STOPPING_HINT.to_owned());
    }

    /// Whether a stop has been asked for the turn that is running.
    fn stopping(&self) -> bool {
        self.working() && self.stop_asked_in == Some(self.session.turns().len())
    }

    /// Says in the transcript that the backend did not take a stop, so the
    /// turn runs on.
    pub fn not_stopped(&mut self, error: &str) {
        self.push(Entry {
            kind: EntryKind::Failure,
            head: "not stopped".to_owned(),
            meta: "turn".to_owned(),
            body: format!("The backend did not take the stop, so the turn runs on: {error}"),
            streaming: false,
            at: self.at,
            calls: Vec::new(),
            agent: None,
        });
    }

    /// Records what the last draw measured, so that paging moves by a screen
    /// the operator actually saw rather than by a guess.
    pub fn measured(&mut self, transcript_lines: usize, viewport_lines: usize) {
        self.transcript_lines = transcript_lines;
        self.viewport_lines = viewport_lines;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// The first transcript line to draw.
    pub fn scroll(&self) -> usize {
        if self.follow {
            self.max_scroll()
        } else {
            self.scroll
        }
    }

    fn max_scroll(&self) -> usize {
        self.transcript_lines.saturating_sub(self.viewport_lines)
    }

    /// Moves the transcript up by `lines`, which unpins it from the tail —
    /// unless it is still there, as a transcript that fits its pane always is.
    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll = self.scroll().saturating_sub(lines);
        self.follow = self.scroll >= self.max_scroll();
    }

    /// Moves the transcript down by `lines`, re-pinning it at the bottom.
    pub fn scroll_down(&mut self, lines: usize) {
        let target = self.scroll().saturating_add(lines);
        if target >= self.max_scroll() {
            self.scroll_to_tail();
        } else {
            self.scroll = target;
            self.follow = false;
        }
    }

    /// Jumps to the oldest line.
    pub fn scroll_to_head(&mut self) {
        self.scroll = 0;
        self.follow = self.max_scroll() == 0;
    }

    /// Jumps to the newest line and stays there as the session grows.
    pub fn scroll_to_tail(&mut self) {
        self.scroll = self.max_scroll();
        self.follow = true;
    }

    /// Lines one notch of the mouse wheel scrolls, as most terminals scroll
    /// their own scrollback.
    const WHEEL_LINES: usize = 3;

    /// Handles one mouse report: a click or a wheel notch gives the keyboard
    /// to the pane under the pointer, the wheel then scrolls it, and a click on
    /// the way back down returns the transcript to its newest line.
    ///
    /// Over the transcript, a drag selects what it covers and letting go
    /// copies it, and a click that does not drag opens the link under it: a
    /// terminal reporting the mouse to the shell no longer selects anything
    /// itself, so the shell does. Nothing else the mouse does means anything
    /// to the shell yet.
    pub fn on_mouse(&mut self, mouse: ratatui::crossterm::event::MouseEvent) {
        use ratatui::crossterm::event::{MouseButton, MouseEventKind};

        // The wheel goes to whatever the pointer is over, and takes the focus
        // there with it: a notch that moved one pane while another stayed
        // marked as the one the keys move would make the mark a lie. Over
        // anything that does not scroll, it moves the pane that has the keys.
        let over = self.pane_at(mouse.column, mouse.row);
        let wheel = |app: &mut App, how: Scroll| {
            if let Some(focus) = over {
                app.focus = focus;
            }
            app.scroll_focused(how);
        };
        match mouse.kind {
            MouseEventKind::ScrollUp => wheel(self, Scroll::Up(Self::WHEEL_LINES)),
            MouseEventKind::ScrollDown => wheel(self, Scroll::Down(Self::WHEEL_LINES)),
            MouseEventKind::Down(MouseButton::Left)
                if self.on_menu_click(mouse.column, mouse.row) => {}
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection = None;
                let jump = self
                    .jump
                    .is_some_and(|at| at.contains((mouse.column, mouse.row).into()));
                if jump {
                    self.focus = Focus::Session;
                    self.scroll_to_tail();
                } else if let Some(focus) = over {
                    self.focus = focus;
                    self.pressed = self.transcript_point(mouse.column, mouse.row);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => self.drag_to(mouse.column, mouse.row),
            MouseEventKind::Up(MouseButton::Left) => self.let_go(),
            _ => {}
        }
    }

    /// The transcript cell at a place on the screen, or `None` where the
    /// last frame drew none of the session's lines there.
    fn transcript_point(&self, column: u16, row: u16) -> Option<crate::select::Point> {
        let (area, lines) = self.transcript_drawn?;
        if !area.contains((column, row).into()) {
            return None;
        }
        let line = self.scroll() + usize::from(row - area.y);
        (line < lines).then_some(crate::select::Point {
            line,
            column: usize::from(column - area.x),
        })
    }

    /// Carries a drag over the transcript on to a place on the screen. A
    /// drag past the top or the bottom of the pane scrolls it a line, so a
    /// selection can be taken further than one screenful.
    fn drag_to(&mut self, column: u16, row: u16) {
        let (Some(anchor), Some((area, lines))) = (self.pressed, self.transcript_drawn) else {
            return;
        };
        let column = column.clamp(area.x, area.right().saturating_sub(1));
        let row = if row < area.y {
            self.scroll_up(1);
            area.y
        } else if row >= area.bottom() {
            self.scroll_down(1);
            area.bottom().saturating_sub(1)
        } else {
            row
        };
        let line = (self.scroll() + usize::from(row - area.y)).min(lines.saturating_sub(1));
        let head = crate::select::Point {
            line,
            column: usize::from(column - area.x),
        };
        self.selection = Some(crate::select::Selection::new(anchor, head));
    }

    /// The left button came up: what a drag selected goes to the clipboard,
    /// and a click that did not drag opens the link it was on.
    fn let_go(&mut self) {
        let Some(pressed) = self.pressed.take() else {
            return;
        };
        let handoff = match self.selection {
            Some(selection) => {
                let drawn = &self.drawn;
                let copied = selection.text(|line| drawn.plain_line(line));
                (!copied.is_empty()).then_some(crate::desktop::Handoff::Copy(copied))
            }
            None => self
                .transcript_drawn
                .and_then(|(area, _)| self.drawn.link_at(pressed, usize::from(area.width)))
                .map(crate::desktop::Handoff::Open),
        };
        self.handoffs.extend(handoff);
    }

    /// The run of the transcript the operator selected, for the draw to mark.
    pub(crate) fn selection(&self) -> Option<crate::select::Selection> {
        self.selection
    }

    /// Where the last frame drew the transcript's lines, and how many lines
    /// the session's entries came to, the question waiting after them left
    /// out.
    pub fn drew_transcript(&mut self, area: ratatui::layout::Rect, lines: usize) {
        self.transcript_drawn = Some((area, lines));
    }

    /// What the operator asked of the desktop since the loop last looked:
    /// text to copy and links to open, oldest first.
    pub fn take_handoffs(&mut self) -> Vec<crate::desktop::Handoff> {
        std::mem::take(&mut self.handoffs)
    }

    /// Says how handing `handoff` to the desktop went.
    pub fn handed_off(&mut self, handoff: &crate::desktop::Handoff, outcome: Result<(), String>) {
        use crate::desktop::Handoff;

        self.hint = Some(match (handoff, outcome) {
            (Handoff::Copy(copied), Ok(())) => {
                let lines = copied.lines().count().max(1);
                match lines {
                    1 => format!("Copied {} characters", copied.chars().count()),
                    _ => format!("Copied {lines} lines"),
                }
            }
            (Handoff::Copy(_), Err(reason)) => format!("Nothing copied: {reason}"),
            (Handoff::Open(link), Ok(())) => format!("Opening {link}"),
            (Handoff::Open(link), Err(reason)) => format!("{link} is not opened: {reason}"),
        });
    }

    /// Handles one key the event loop read from the terminal, saying when
    /// and with what else it came.
    ///
    /// Where a prompt waits, that is what tells a key the operator pressed to
    /// answer it from one they were already typing, or one of a paste: see
    /// [`ASK_QUIET`]. [`App::on_key`] is the same key with nothing known of
    /// its arrival, which no prompt holds back.
    pub fn on_key_read(&mut self, key: ratatui::crossterm::event::KeyEvent, arrival: Arrival) {
        self.arrival = Some(arrival);
        self.on_key(key);
        self.arrival = None;
    }

    /// Handles the keys one read of the terminal held, in order, as
    /// [`App::on_key_read`] would one at a time.
    ///
    /// An Enter among several keys is a line break, never a send: see the
    /// comment on it below.
    ///
    /// Where a character has just been typed into the prompt, the characters
    /// that follow it in the same read go in with one insert, as a paste
    /// does. The composer's editor lays out its whole text again after every
    /// edit, so a paste the terminal did not bracket, taken a key at a time,
    /// would cost the length of the line once for every character in it.
    pub fn on_keys_read(&mut self, keys: &[ratatui::crossterm::event::KeyEvent], arrival: Arrival) {
        let mut rest = keys;
        while let Some((&key, after)) = rest.split_first() {
            rest = after;
            // An Enter in a read of several keys was pasted, not pressed: a
            // terminal that does not bracket pastes sends the line breaks of
            // one as Enter. Pressed, it would send the prompt or run the `!`
            // command the paste typed, which nobody asked for.
            if !arrival.alone && is_plain_enter(key) && self.composer_has_the_keyboard() {
                self.composer.insert_newline();
                continue;
            }
            self.on_key_read(key, arrival);
            if typed_char(key).is_none() || !self.types_into_the_prompt() {
                continue;
            }
            let run = rest
                .iter()
                .take_while(|key| typed_char(**key).is_some())
                .count();
            let (typed, after) = rest.split_at(run);
            let text: String = typed.iter().copied().filter_map(typed_char).collect();
            if self.composer.insert_str(text) {
                self.offer_selected = 0;
                self.reopen_offers();
            }
            rest = after;
        }
    }

    /// Whether keys go to the composer — the prompt or the `!` command line —
    /// rather than to a question, a list, a menu, a sheet, the history or the
    /// search.
    fn composer_has_the_keyboard(&self) -> bool {
        self.focus() == Focus::Session
            && self.find.is_none()
            && self.picking.is_none()
            && self.menu.is_none()
            && self.sheet.is_none()
            && self.browser.is_none()
            && !self
                .asking()
                .is_some_and(|_| self.ask_focus != AskFocus::Deferred)
    }

    /// Whether a character typed now would go into the prompt as itself: the
    /// prompt has the keyboard and something in it already, so no `!` could
    /// open a command, and no `Esc` has made the next digit an F-key.
    fn types_into_the_prompt(&self) -> bool {
        self.focus() == Focus::Session
            && !self.escaped
            && self.find.is_none()
            && self.picking.is_none()
            && self.menu.is_none()
            && self.sheet.is_none()
            && self.browser.is_none()
            && !self
                .asking()
                .is_some_and(|_| self.ask_focus != AskFocus::Deferred)
            && !self.composer.is_empty()
    }

    /// Handles a paste the terminal bracketed: text, not keys.
    ///
    /// Its line breaks stay line breaks, so a pasted log is one prompt to
    /// read over and send rather than a turn per line. It goes where typing
    /// would go, but it presses nothing: no Enter, no `!` opening a command,
    /// and no answer to a question — a paste is exactly
    /// what a prompt must not take as one (see [`ASK_QUIET`]).
    pub fn on_paste(&mut self, pasted: &str) {
        self.hint = None;
        self.escaped = false;
        let text = pasted_lines(pasted);
        // Nothing pasted answers the trust question, and there is no prompt
        // behind it to take the text.
        if self.trusting.is_some() {
            self.hint = Some(TOO_SOON_HINT.to_owned());
            return;
        }
        // A list or a search open over a question takes what is pasted as it
        // takes keys, ahead of the question.
        if self.picking().is_some() {
            return;
        }
        if self.asking().is_some() && self.paste_into_find(&text) {
            return;
        }
        if self.asking().is_some() {
            match self.ask_focus {
                AskFocus::Writing => {
                    self.ask_draft.push_str(&joined_lines(&text));
                    return;
                }
                AskFocus::Choosing => {
                    self.hint = Some(TOO_SOON_HINT.to_owned());
                    return;
                }
                AskFocus::Deferred => {}
            }
        }
        if self.menu.is_some() || self.sheet.is_some() {
            return;
        }
        if self.browser.is_some() {
            self.paste_into_browser(pasted);
            return;
        }
        if self.paste_into_find(&text) {
            return;
        }
        // A terminal that pastes an image it cannot give as text pastes
        // nothing, and a dropped image file arrives as its path.
        if text.trim().is_empty() {
            self.focus = Focus::Session;
            self.attach_image(Source::Clipboard);
            return;
        }
        if let Some(path) = image_path(pasted) {
            self.focus = Focus::Session;
            self.attach_image(Source::File(path));
            return;
        }
        self.focus = Focus::Session;
        if self.composer.insert_str(text) {
            self.offer_selected = 0;
            self.reopen_offers();
        }
    }

    /// Puts `text` into the search, joined onto one line, where a search is
    /// open. Returns whether one was.
    fn paste_into_find(&mut self, text: &str) -> bool {
        let Some(find) = self.find.as_mut() else {
            return false;
        };
        if find.query.insert_str(joined_lines(text)) {
            find.current = None;
        }
        true
    }

    /// The latest moment the shell has been told of: the tick's, or the
    /// arrival of the key being handled, which a prompt that key brought to
    /// the front came up at.
    fn latest_instant(&self) -> Option<Instant> {
        match (self.now, self.arrival.map(|arrival| arrival.at)) {
            (Some(now), Some(at)) => Some(now.max(at)),
            (now, at) => now.or(at),
        }
    }

    /// Whether the key being handled reached the front prompt before the
    /// operator could have meant it as an answer, and if so, starts the quiet
    /// the prompt waits for over again from it.
    ///
    /// A key comes too soon when it arrived with other keys in one read, as a
    /// paste does, or within [`ASK_QUIET`] of the prompt coming up or of the
    /// last key held back. Restarting the wait on each key held back is what
    /// stops a word being typed through a prompt one key at a time.
    fn too_soon_to_answer(&mut self) -> bool {
        let Some(arrival) = self.arrival else {
            return false;
        };
        let early = self
            .ask_quiet_since
            .is_some_and(|since| arrival.at < since + ASK_QUIET);
        if arrival.alone && !early {
            return false;
        }
        self.ask_quiet_since = Some(arrival.at);
        self.hint = Some(TOO_SOON_HINT.to_owned());
        true
    }

    /// Whether the key being handled is an Enter that would send the prompt
    /// before the keyboard has been quiet since a question that had it was
    /// taken away, and if so, says so. Every key until then starts the quiet
    /// over again, as a key held back from a question does: the operator
    /// typing on is still answering it.
    fn sends_too_soon_after_withdrawal(
        &mut self,
        key: ratatui::crossterm::event::KeyEvent,
    ) -> bool {
        let (Some(since), Some(arrival)) = (self.withdrawn_quiet_since, self.arrival) else {
            return false;
        };
        if self.asking().is_some() {
            return false;
        }
        if arrival.alone && arrival.at >= since + ASK_QUIET {
            self.withdrawn_quiet_since = None;
            return false;
        }
        self.withdrawn_quiet_since = Some(arrival.at);
        if !is_plain_enter(key) {
            return false;
        }
        self.hint = Some(WITHDRAWN_NOT_SENT_HINT.to_owned());
        true
    }

    /// Handles one key.
    ///
    /// The shell's own bindings are taken first and everything left over goes
    /// to the composer, so typing an `f` is typing an `f` even though `F1` is a
    /// menu.
    pub fn on_key(&mut self, key: ratatui::crossterm::event::KeyEvent) {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};

        self.hint = None;
        self.selection = None;
        let escaped = self.escaped;
        let key = self.as_function_key(key);
        // A terminal that did not say it could send Shift+Enter has now sent
        // one — tmux asked for modifyOtherKeys, under a terminal that reports
        // it — so it is the key the bar can name.
        if (key.code, key.modifiers) == (KeyCode::Enter, KeyModifiers::SHIFT) {
            self.reports_shift_enter = true;
        }

        // Quitting is always available: a session with a prompt up is still a
        // session the operator may need to leave, and the backend is told the
        // same way it is told about any other way out.
        // Except that Ctrl+C is what an operator presses to stop a thing: with
        // a turn running it stops the turn, and only a second press — or one
        // with nothing running — ends the session.
        if (key.code, key.modifiers) == (KeyCode::Char('c'), KeyModifiers::CONTROL)
            && self.working()
            && !self.stopping()
        {
            self.stop_turn();
            return;
        }
        if let (KeyCode::F(10), _) | (KeyCode::Char('q' | 'c'), KeyModifiers::CONTROL) =
            (key.code, key.modifiers)
        {
            self.quit();
            return;
        }
        // Suspending is always available for the same reason: it hands the
        // terminal back and leaves the prompt where it is.
        if (key.code, key.modifiers) == (KeyCode::Char('z'), KeyModifiers::CONTROL) {
            self.suspend = true;
            return;
        }
        // Stopping a command is always available for the same reason: `! yes`
        // runs on under a question, and the operator should not have to answer
        // it, or quit, to stop it.
        if (key.code, key.modifiers) == (KeyCode::Char('g'), KeyModifiers::CONTROL)
            && self.runs_commands
        {
            self.stop_command();
            return;
        }
        // The trust question takes the keyboard whole: there is no session
        // behind it to type into, and nothing else to do until it is answered.
        if self.trusting.is_some() {
            self.on_trust_key(key);
            return;
        }
        // An open menu, and a sheet, take the keyboard whole: each is
        // something the operator opened and is reading.
        if self.menu.is_some() {
            self.on_menu_key(key);
            return;
        }
        if self.sheet.is_some() {
            self.on_sheet_key(key);
            return;
        }
        // The history dialog takes it the same way: what is typed there
        // filters it.
        if self.browser.is_some() {
            self.on_browser_key(key);
            return;
        }
        // A list or a search open over a question is what the operator is
        // looking at, and the question is covered by it or not marked as the
        // one with the keys: its keys answering the question would approve a
        // call nobody was reading.
        if self.on_overlay_key(key) {
            return;
        }
        // A prompt takes the keyboard whole until it is put off. Typing into
        // the composer under a question would put the answer to it into the
        // next turn.
        if self.asking().is_some() {
            match self.ask_focus {
                AskFocus::Choosing | AskFocus::Writing => {
                    self.on_ask_key(key);
                    return;
                }
                AskFocus::Deferred if key.code == KeyCode::Esc => {
                    self.ask_focus = AskFocus::Choosing;
                    self.scroll_to_tail();
                    return;
                }
                // The turn is waiting on the answer, and the backend would
                // hold a prompt sent now until after it — so the operator
                // would have written the next turn believing it the answer.
                AskFocus::Deferred if key.code == KeyCode::Enter && !opens_a_line(key) => {
                    self.hint = Some(
                        "The turn is still waiting on the question above; Esc to answer it first"
                            .to_owned(),
                    );
                    return;
                }
                AskFocus::Deferred => {}
            }
        }
        if self.sends_too_soon_after_withdrawal(key) {
            return;
        }
        // A menu's letter after an Esc that did nothing else, or with Alt
        // held, opens it: the same two bytes, as a digit is an F-key.
        if let KeyCode::Char(letter) = key.code
            && (escaped && key.modifiers == KeyModifiers::NONE
                || key.modifiers == KeyModifiers::ALT)
            && let Some(at) = crate::menu::menu_of(letter)
        {
            self.open_menu(at);
            return;
        }

        if self.scroll_key(key) {
            return;
        }
        if self.on_find_key(key) {
            return;
        }
        if self.on_shell_key(key) {
            return;
        }
        if self.on_mention_key(key) || self.on_command_key(key) {
            return;
        }
        if let Focus::Pane(pane) = self.focus
            && self.on_pane_key(pane, key)
        {
            return;
        }

        match (key.code, key.modifiers) {
            // Tab has nothing to do in a prompt, and Shift+Tab already cycles
            // the mode, so the plain key is the one that moves the focus.
            (KeyCode::Tab, KeyModifiers::NONE) => self.focus_next(),
            (KeyCode::Char('r'), KeyModifiers::CONTROL) => self.open_history(View::Prompts),
            // A binding rather than a character, so it searches with a prompt
            // half written and leaves the prompt as it was.
            (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                self.focus = Focus::Session;
                self.open_find();
            }
            // Up on the composer's first row and Down on its last are where
            // the cursor has nowhere to go, and where a shell walks its
            // history; anywhere else they move the cursor as they always have.
            (KeyCode::Up | KeyCode::Down, KeyModifiers::NONE)
                if !self.shell_mode && self.recall_key(key.code == KeyCode::Up) =>
            {
                self.focus = Focus::Session;
            }
            // Shift+Enter, Ctrl+J or Alt+Enter opens a line; Enter sends. The
            // other way round would make the common action the awkward one.
            _ if opens_a_line(key) => {
                self.focus = Focus::Session;
                self.composer.insert_newline();
            }
            (KeyCode::Enter, _) => self.submit(),
            (KeyCode::Char('o'), KeyModifiers::CONTROL) => self.fold_calls(),
            (KeyCode::Char('t'), KeyModifiers::CONTROL) => self.open_diffs(),
            // A terminal's own paste hands over text, and an image on the
            // clipboard is none: this is the one way to reach it.
            (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                self.focus = Focus::Session;
                self.attach_image(Source::Clipboard);
            }

            // Shift+Tab reaches crossterm as its own code rather than as Tab
            // with a modifier, which is why it is matched on the code alone.
            (KeyCode::BackTab, _) => self.cycle_mode(),
            (KeyCode::F(n), _) => {
                if let Some(action) = crate::menu::fkey(n) {
                    self.perform(action);
                }
            }
            // An Esc nothing else wanted leaves the composer as it is and
            // makes the next digit an F-key, which is how the bar's actions
            // are reached where the F-keys never arrive.
            // With a turn running, it stops the turn instead, as it does in
            // the CLI's own interface: stopping the agent is the more urgent
            // of the two, and the F-keys are there again once the turn ends.
            (KeyCode::Esc, _) if self.working() => {
                self.focus = Focus::Session;
                self.stop_turn();
            }
            (KeyCode::Esc, _) => {
                self.focus = Focus::Session;
                self.escaped = true;
                self.hint = Some(ESCAPED_HINT.to_owned());
            }

            // `!` runs a command where it would start a prompt: in a
            // composer with anything but spaces in it, it is an exclamation
            // mark.
            (KeyCode::Char('!'), KeyModifiers::NONE | KeyModifiers::SHIFT)
                if self.composer_is_blank() && self.runs_commands =>
            {
                self.focus = Focus::Session;
                self.composer.clear();
                self.shell_mode = true;
            }
            // What was typed goes where typing always goes, and the keyboard
            // follows it back: the Enter after it has to send it.
            _ => {
                self.focus = Focus::Session;
                let changed = match delete_whole(&mut self.composer, key) {
                    Some(changed) => changed,
                    None => self.composer.input(Input::from(key)),
                };
                if changed {
                    self.offer_selected = 0;
                    self.reopen_offers();
                }
            }
        }
    }

    /// One key, while a list is up, or a search is up over a question.
    /// Returns whether one of them took it.
    ///
    /// The list takes the keyboard whole: an arrow key that scrolled the
    /// transcript behind it, or moved the answer of a question under it,
    /// would move something the operator was not looking at. A search with
    /// no question waiting takes its keys later, after the shell's own
    /// bindings. A question the closing uncovers waits for a quiet keyboard
    /// again, as one that has just come up does: an Enter held on the list
    /// is not an answer to it.
    fn on_overlay_key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> bool {
        if self.picking.is_some() {
            self.on_pick_key(key);
        } else if self.find.is_some() && self.asking().is_some() {
            if !(self.scroll_key(key) || self.on_find_key(key)) {
                return false;
            }
        } else {
            return false;
        }
        if self.picking.is_none() && self.find.is_none() && self.asking().is_some() {
            self.ask_quiet_since = self.latest_instant();
        }
        true
    }

    /// One key, while a prompt has the keyboard.
    ///
    /// Scrolling still scrolls and a cut diff still opens, because the work that led to the question is
    /// what the operator reads to answer it. Anything else that is not about
    /// the question is swallowed rather than passed on: a key that did
    /// something else while a question was being asked would be a key nobody
    /// meant.
    fn on_ask_key(&mut self, key: ratatui::crossterm::event::KeyEvent) {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};

        if self.scroll_key(key) {
            return;
        }
        // Opening a cut diff is reading too: the change a question is about
        // may be the one that was cut.
        if (key.code, key.modifiers) == (KeyCode::Char('t'), KeyModifiers::CONTROL) {
            self.open_diffs();
            return;
        }
        // Below the smallest window the shell draws only the size it needs,
        // so no part of the question is on screen to answer.
        if self.too_small {
            self.hint = Some(TOO_SMALL_HINT.to_owned());
            return;
        }
        // The question is the last thing in the transcript. Scrolled back,
        // the operator cannot see it, and a key that answered it then would
        // answer something they were not reading; the first such key brings
        // it into view and does nothing else.
        if !self.follow {
            self.scroll_to_tail();
            self.ask_quiet_since = self.latest_instant();
            return;
        }
        // Drawn whole and taller than the pane, the question's top is above
        // it while the view follows its end: Enter would confirm a call whose
        // name, and whose first answer, are not on screen.
        if !self.question_top_drawn {
            self.hint = Some(TOP_OFF_SCREEN_HINT.to_owned());
            return;
        }
        // The answer field takes words, and words may arrive together —
        // pasted, expanded, dictated. Only what would send the answer is
        // held to the guard against keys that were not meant for it.
        if self.ask_focus == AskFocus::Writing && key.code != KeyCode::Enter {
            self.on_writing_key(key.code);
            return;
        }
        if self.too_soon_to_answer() {
            return;
        }
        match self.ask_focus {
            AskFocus::Writing => self.on_writing_key(key.code),
            AskFocus::Choosing | AskFocus::Deferred => self.on_choosing_key(key.code),
        }
    }

    /// One key, on the numbered answers.
    fn on_choosing_key(&mut self, code: ratatui::crossterm::event::KeyCode) {
        use ratatui::crossterm::event::KeyCode;

        match code {
            KeyCode::Enter => self.answer(self.ask_selected()),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(true),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(false),
            KeyCode::Char(c) => {
                if let Some(number) = c.to_digit(10) {
                    self.select_option(number);
                }
            }
            KeyCode::Tab => self.ask_focus = AskFocus::Writing,
            KeyCode::Esc => self.ask_focus = AskFocus::Deferred,
            _ => {}
        }
    }

    /// One key, on an answer being written.
    fn on_writing_key(&mut self, code: ratatui::crossterm::event::KeyCode) {
        use ratatui::crossterm::event::KeyCode;

        match code {
            KeyCode::Enter => {
                let words = self.ask_draft.clone();
                self.answer_saying(&words);
            }
            KeyCode::Char(c) => self.ask_draft.push(c),
            KeyCode::Backspace => {
                let kept = self
                    .ask_draft
                    .grapheme_indices(true)
                    .next_back()
                    .map_or(0, |(at, _)| at);
                self.ask_draft.truncate(kept);
            }
            KeyCode::Esc => self.ask_focus = AskFocus::Choosing,
            _ => {}
        }
    }

    /// Hands in the clock, once per pass of the event loop.
    ///
    /// The running turn's elapsed time is measured from the first tick that
    /// saw it running, so a turn is timed from when the shell knew of it, and
    /// an idle session's from the first tick that saw the turn stop.
    ///
    /// `at` is the wall clock, read here rather than in the draw: the draw
    /// reads no clock, so a test draws the same frame every time. It is also
    /// the stamp every event folded in before the next tick is given, so one
    /// read of the clock serves the menu row and the whole tick's events —
    /// a hundred milliseconds of slack against a figure drawn to the minute.
    pub fn tick(&mut self, now: Instant, at: Option<Stamp>) {
        self.now = Some(now);
        self.at = at;
        // The trust question is up from the first tick, which draws it, and
        // waits from there for a quiet keyboard as a permission question does.
        if self.trusting.is_some() && self.ask_quiet_since.is_none() {
            self.ask_quiet_since = Some(now);
        }
        match self.working() {
            true => {
                self.working_since = self.working_since.or(Some(now));
                self.idle_since = None;
            }
            false => {
                self.working_since = None;
                self.idle_since = self.idle_since.or(Some(now));
            }
        }
    }

    /// How long the agent has worked in this session: every turn from its
    /// first prompt to its end, the idle between turns left out.
    ///
    /// A turn still running counts to now while it is being worked on from
    /// here, and to the last thing it did where it was read back unfinished,
    /// which nothing is running now. `None` where any turn's span was not
    /// measured, or before the first turn: what the session spent cannot be
    /// divided by a time that leaves part of the spending out.
    pub fn worked(&self) -> Option<Duration> {
        let ended = self.turns_took?;
        if !self.session.turn_running() {
            return (!self.session.turns().is_empty()).then_some(ended);
        }
        let reached = match self.working() {
            true => self.at,
            false => self.last_folded_at,
        };
        let running = reached?.since(self.turn_began_at?)?;
        Some(ended.saturating_add(running))
    }

    /// The time of day, as the event loop last read it.
    pub fn clock(&self) -> Option<LocalTime> {
        self.at.and_then(Stamp::local)
    }

    /// The clock the next event folded in will be stamped with.
    pub fn stamp(&self) -> Option<Stamp> {
        self.at
    }

    /// Whether a turn is running, and for how long it or the idle before it
    /// has been.
    ///
    /// The duration is `None` until the loop has ticked at least once with the
    /// session in the state it is in, so a turn that started since the last
    /// tick says it is working without yet claiming a time for it.
    pub fn pulse(&self) -> Pulse {
        let working = self.working();
        let since = match working {
            true => self.working_since,
            false => self.idle_since,
        };
        Pulse {
            working,
            since: match (self.now, since) {
                (Some(now), Some(since)) => Some(now.saturating_duration_since(since)),
                _ => None,
            },
        }
    }

    /// What the running turn is doing, while one sent from here is running.
    pub fn activity(&self) -> Option<Activity> {
        if !self.working() {
            return None;
        }
        let elapsed = match (self.now, self.working_since) {
            (Some(now), Some(since)) => now.saturating_duration_since(since),
            _ => Duration::ZERO,
        };
        Some(Activity {
            elapsed,
            doing: self.doing(),
        })
    }

    fn working(&self) -> bool {
        self.attached && self.sent_here && self.session.turn_running()
    }

    /// The operator first, then the newest call still running, then the reply
    /// being written: the first of them there is, is what the turn is waiting
    /// on.
    fn doing(&self) -> String {
        if !self.session.pending_permissions().is_empty() {
            return "waiting on you".to_owned();
        }
        let running = self.tool_entries.values().max().and_then(|&(at, index)| {
            let entry = self.entries.get(at)?;
            Some((&entry.head, &entry.calls.get(index)?.what))
        });
        match (running, self.entries.last()) {
            (Some((head, what)), _) => format!("running {head}  {what}"),
            (None, Some(last)) if last.kind == EntryKind::Agent && last.streaming => {
                "writing".to_owned()
            }
            _ => "thinking".to_owned(),
        }
    }

    /// Sends what is in the composer.
    ///
    /// The prompt is folded in and queued: the event loop takes it from
    /// [`App::take_produced`] and hands it to the backend and to the journal.
    /// With nothing attached, the shell says so plainly rather than leaving a
    /// prompt on screen that looks sent.
    pub fn submit(&mut self) {
        let text = self.composed();
        if text.trim().is_empty() {
            return;
        }

        self.composer.clear();
        self.offer_closed = None;
        self.recall = None;
        if let Some(model) = crate::slash::model_named(&text, self.session.commands()) {
            self.images.discard();
            self.produce(Event::ModelSelected { model });
            self.scroll_to_tail();
            return;
        }
        self.images.send(&text);
        self.produce(Event::UserMessage { text });
        self.sent_here = self.attached;
        if !self.attached {
            self.push(Entry {
                kind: EntryKind::Notice,
                head: "no backend".to_owned(),
                meta: "not sent".to_owned(),
                body: "This session is not attached to a backend, so the prompt above was \
                       not sent. `niobe profiles` shows which profiles are defined and which \
                       backend each one runs."
                    .to_owned(),
                streaming: false,
                at: self.at,
                calls: Vec::new(),
                agent: None,
            });
        }
        self.scroll_to_tail();
    }

    /// Says in the transcript that a turn never reached the backend, so that a
    /// prompt with no reply is not read as a backend thinking about it.
    pub fn not_sent(&mut self, error: &str) {
        self.sent_here = false;
        if self.session.user_messages() == 1 {
            self.first_prompt_unsent = true;
        }
        self.push(Entry {
            kind: EntryKind::Failure,
            head: "not sent".to_owned(),
            meta: String::new(),
            body: format!(
                "What you just typed did not reach the backend, so nothing is working on \
                 it: {error}"
            ),
            streaming: false,
            at: self.at,
            calls: Vec::new(),
            agent: None,
        });
        self.scroll_to_tail();
    }

    /// What Cost & usage says: the plan's windows and when they come back,
    /// where a backend has reported them, and otherwise that the breakdown
    /// behind the item is not implemented yet.
    ///
    /// `now` is seconds since the Unix epoch, taken by the caller so that the
    /// wording can be asserted against a fixed clock.
    pub fn cost_hint(&self, now: u64) -> String {
        let Some(windows) = self.session.usage_windows() else {
            return "Cost & usage — the usage breakdown is not implemented yet".to_owned();
        };
        let mut parts = Vec::new();
        if let Some(window) = windows.five_hour {
            parts.push(window_hint("5h", window, now));
        }
        if let Some(window) = windows.seven_day {
            parts.push(window_hint("7d", window, now));
        }
        if windows.using_overage {
            parts.push("spending beyond the plan".to_owned());
        }
        format!("Cost & usage — {}", parts.join(" · "))
    }

    /// The moment the shell is reading the session at, in seconds since the
    /// Unix epoch.
    ///
    /// The clock the event loop last handed in, so that what a key reads out
    /// about a window and what the Usage pane draws about the same window are
    /// measured from one moment. Before the loop has ticked there is no such
    /// moment and the machine's own clock stands in.
    fn read_at(&self) -> u64 {
        self.at
            .and_then(|at| at.at().duration_since(std::time::UNIX_EPOCH).ok())
            .map_or_else(now_secs, |since| since.as_secs())
    }

    /// Feeds a key straight to the composer, for tests and for a paste.
    pub fn type_into_composer(&mut self, input: impl Into<Input>) {
        self.composer.input(input);
    }
}

/// The prompts and sessions the operator can go back to: Up and Down in the
/// composer, and the history dialog.
impl App {
    /// The same shell, handed a record of the repository's sessions: the
    /// history dialog can open one of them in place of this one.
    #[must_use]
    pub fn remembers(mut self) -> Self {
        self.remembers = true;
        self
    }

    /// Whether the shell has asked for the repository's earlier sessions to
    /// be read since this was last asked: once as it opens, and again each
    /// time the history dialog does, so a session another shell ended since
    /// is in it.
    pub fn take_history_request(&mut self) -> bool {
        std::mem::take(&mut self.history_wanted)
    }

    /// What a load of the repository's earlier sessions found.
    ///
    /// A walk through the prompts already under way keeps the ones it started
    /// with, so the prompt on screen does not change under the operator.
    pub fn set_past(&mut self, past: crate::history::Past) {
        self.past = Some(past);
        self.keep_browser_cursor();
    }

    /// Whether a load has landed, so that an empty list is one that is empty
    /// rather than one that has not been read yet.
    pub fn history_read(&self) -> bool {
        self.past.is_some()
    }

    /// What the last load could not read, in words the operator reads.
    pub fn history_unread(&self) -> Option<&str> {
        self.past.as_ref()?.unread.as_deref()
    }

    /// The number this session is recorded under, once it is: its record is
    /// this session, which the dialog lists as itself rather than as another.
    pub fn set_recorded_as(&mut self, recorded: Option<String>) {
        if self.recorded_as != recorded {
            self.recorded_as = recorded;
        }
    }

    /// The session the operator chose to open in place of this one, once.
    pub fn take_opening(&mut self) -> Option<Target> {
        self.opening.take()
    }

    /// The history dialog, while it is open.
    pub fn browser(&self) -> Option<&Browser> {
        self.browser.as_ref()
    }

    /// Opens the history dialog on `view`, and asks for what it lists to be
    /// read again.
    fn open_history(&mut self, view: View) {
        self.menu = None;
        self.browser = Some(Browser::new(view));
        self.history_wanted = true;
    }

    /// Whether `target` is the record of this very session: the number it is
    /// recorded under, or the conversation its backend is carrying on.
    fn is_this_session(&self, target: &Target) -> bool {
        match target {
            Target::Recorded(id) => self.recorded_as.as_deref() == Some(id.as_str()),
            Target::Claude(id) => {
                self.session
                    .meta()
                    .and_then(|meta| meta.backend_session.as_deref())
                    == Some(id.as_str())
            }
        }
    }

    /// This session's prompts, oldest first, with when each was sent.
    fn own_prompts(&self) -> impl DoubleEndedIterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::User)
    }

    /// Every prompt there is to go back to, newest first: this session's, then
    /// the earlier sessions'. A prompt sent more than once is listed where it
    /// was sent last.
    fn every_prompt(&self) -> Vec<PromptRow> {
        let own = self.own_prompts().rev().map(|entry| PromptRow {
            text: entry.body.clone(),
            at: entry.at,
            session: None,
        });
        let earlier = self
            .past
            .iter()
            .flat_map(|past| &past.prompts)
            .filter(|prompt| !self.is_this_session(&prompt.session))
            .map(|prompt| PromptRow {
                text: prompt.text.clone(),
                at: prompt.at,
                session: Some(prompt.session.clone()),
            });
        let mut seen = std::collections::HashSet::new();
        own.chain(earlier)
            .filter(|row| !row.text.trim().is_empty() && seen.insert(row.text.clone()))
            .collect()
    }

    /// The prompts the history dialog lists, newest first, as its filter
    /// leaves them.
    pub fn prompt_rows(&self) -> Vec<PromptRow> {
        let query = self.browser.as_ref().map_or("", |browser| &browser.query);
        self.every_prompt()
            .into_iter()
            .filter(|row| crate::history::matches(query, &row.text))
            .collect()
    }

    /// The sessions the history dialog lists, as its filter leaves them: this
    /// one first, then the others newest first, each with its prompts where
    /// they are known.
    pub fn session_rows(&self) -> Vec<SessionRow> {
        let query = self.browser.as_ref().map_or("", |browser| &browser.query);
        let this = SessionRow {
            target: None,
            last: self.entries.iter().rev().find_map(|entry| entry.at),
            prompts: self.own_prompts().map(|entry| entry.body.clone()).collect(),
            first_prompt: None,
        };
        let mut prompts: std::collections::HashMap<&Target, Vec<String>> =
            std::collections::HashMap::new();
        for prompt in self.past.iter().flat_map(|past| past.prompts.iter().rev()) {
            prompts
                .entry(&prompt.session)
                .or_default()
                .push(prompt.text.clone());
        }
        let others = self
            .past
            .iter()
            .flat_map(|past| &past.sessions)
            .filter(|session| !self.is_this_session(&session.target))
            .map(|session| SessionRow {
                target: Some(session.target.clone()),
                last: session.last,
                prompts: prompts.remove(&session.target).unwrap_or_default(),
                first_prompt: session.first_prompt.clone(),
            });
        std::iter::once(this)
            .chain(others)
            .filter(|row| row.matches(query))
            .collect()
    }

    /// How many rows the open dialog lists.
    fn browser_rows(&self) -> usize {
        match self.browser.as_ref().map(|browser| browser.view) {
            Some(View::Prompts) => self.prompt_rows().len(),
            Some(View::Sessions) => self.session_rows().len(),
            None => 0,
        }
    }

    /// Keeps the dialog's cursor on a row it lists, after what it lists
    /// changed under it.
    fn keep_browser_cursor(&mut self) {
        let last = self.browser_rows().saturating_sub(1);
        if let Some(browser) = self.browser.as_mut() {
            browser.at = browser.at.min(last);
        }
    }

    /// One key, while the history dialog is open. It takes the keyboard
    /// whole: what is typed filters it, and nothing reaches the composer
    /// behind it until a prompt is chosen.
    fn on_browser_key(&mut self, key: ratatui::crossterm::event::KeyEvent) {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};

        let Some(browser) = self.browser.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => self.browser = None,
            (KeyCode::Tab, KeyModifiers::NONE) | (KeyCode::BackTab, _) => {
                browser.view = browser.view.other();
                browser.at = 0;
            }
            (KeyCode::Up, _) => browser.at = browser.at.saturating_sub(1),
            (KeyCode::PageUp, _) => browser.at = browser.at.saturating_sub(BROWSER_PAGE),
            // Ctrl+R again steps to the next older match, as a shell's
            // reverse search does.
            (KeyCode::Down, _) | (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                browser.at = browser.at.saturating_add(1);
            }
            (KeyCode::PageDown, _) => browser.at = browser.at.saturating_add(BROWSER_PAGE),
            (KeyCode::Enter, _) => {
                self.choose_from_history();
                return;
            }
            (KeyCode::Backspace, _) => {
                browser.query.pop();
                browser.at = 0;
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                browser.query.clear();
                browser.at = 0;
            }
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                browser.query.push(c);
                browser.at = 0;
            }
            _ => {}
        }
        self.keep_browser_cursor();
    }

    /// Text pasted while the history dialog is open: more of its filter, on
    /// one line.
    fn paste_into_browser(&mut self, pasted: &str) {
        if let Some(browser) = self.browser.as_mut() {
            browser
                .query
                .push_str(&pasted.split_whitespace().collect::<Vec<_>>().join(" "));
            browser.at = 0;
        }
        self.keep_browser_cursor();
    }

    /// Does what Enter on the dialog's row does: puts a prompt in the
    /// composer, or opens a session in place of this one.
    fn choose_from_history(&mut self) {
        let Some(browser) = self.browser.as_ref() else {
            return;
        };
        let at = browser.at;
        match browser.view {
            View::Prompts => {
                let Some(row) = self.prompt_rows().into_iter().nth(at) else {
                    return;
                };
                self.browser = None;
                self.recall = None;
                self.focus = Focus::Session;
                self.show_in_composer(&row.text, false);
            }
            View::Sessions => {
                let Some(row) = self.session_rows().into_iter().nth(at) else {
                    return;
                };
                self.open_session(row.target);
            }
        }
    }

    /// Ends this session and opens `target` in its place, where that is a
    /// session that can be opened now.
    fn open_session(&mut self, target: Option<Target>) {
        let Some(target) = target else {
            self.hint = Some("This is the session open now".to_owned());
            return;
        };
        if !self.remembers {
            self.hint = Some(NO_RECORD_HINT.to_owned());
            return;
        }
        // The turn would be left half done, and the session it was part of
        // reopened later would read it as one the process was killed under.
        if self.working() {
            self.hint = Some(
                "A turn is running: stop it first with Esc, then open another session".to_owned(),
            );
            return;
        }
        self.browser = None;
        self.opening = Some(target);
        self.quit();
    }

    /// Up or Down where the composer's cursor cannot move that way: the
    /// prompt before or after the one shown. Returns whether it was that.
    fn recall_key(&mut self, older: bool) -> bool {
        use ratatui_textarea::CursorMove;

        let before = self.composer.cursor();
        self.composer.move_cursor(match older {
            true => CursorMove::Up,
            false => CursorMove::Down,
        });
        if self.composer.cursor() != before {
            return false;
        }
        match older {
            true => self.recall_older(),
            false => self.recall_newer(),
        }
        true
    }

    /// The prompt before the one shown, starting a walk on the newest with
    /// what was being written set aside.
    fn recall_older(&mut self) {
        let shown = match self.recall.as_mut() {
            Some(recall) => recall.older().map(str::to_owned),
            None => {
                let prompts: Vec<String> = self
                    .every_prompt()
                    .into_iter()
                    .map(|row| row.text)
                    .collect();
                let recall = Recall::start(prompts, self.composed());
                let shown = recall.as_ref().map(|recall| recall.current().to_owned());
                self.recall = recall;
                shown
            }
        };
        if let Some(shown) = shown {
            self.show_in_composer(&shown, true);
        }
    }

    /// The prompt after the one shown, or past the newest the draft the walk
    /// set aside, which ends it.
    fn recall_newer(&mut self) {
        let Some(recall) = self.recall.as_mut() else {
            return;
        };
        let shown = match recall.newer() {
            Newer::Prompt(prompt) => prompt.to_owned(),
            Newer::Draft(draft) => {
                self.recall = None;
                draft
            }
        };
        self.show_in_composer(&shown, false);
    }

    /// Replaces what the composer holds with `text`, the cursor at the end of
    /// its first row or of its last: where the next Up, or the next Down,
    /// steps on rather than moving inside it.
    ///
    /// A `/` or `@` it opens with is not offered a list: the arrows would go
    /// to the list, and the walk would stop on the first such prompt.
    fn show_in_composer(&mut self, text: &str, at_top: bool) {
        use ratatui_textarea::CursorMove;

        self.composer.clear();
        self.composer.insert_str(text);
        match at_top {
            true => self.composer.move_cursor(CursorMove::Top),
            false => self.composer.move_cursor(CursorMove::Bottom),
        }
        self.composer.move_cursor(CursorMove::End);
        self.offer_selected = 0;
        self.offer_closed = None;
        if let Some(mention) = self.mention() {
            self.offer_closed = Some((mention.row, mention.at));
        } else if self.slash().is_some() {
            self.offer_closed = Some((0, 0));
        }
    }
}

/// What opening another session says where there is no record to open one
/// from: a log being read back.
const NO_RECORD_HINT: &str =
    "This shell was opened on a log, with no record of sessions to open another from";

/// Rows PageUp and PageDown move the history dialog's cursor by.
const BROWSER_PAGE: usize = 10;

/// Backspace or Delete on the composer, taking what the operator sees as one
/// character — a letter and the accents on it, an emoji and the ones joined
/// to it — rather than the one code point the editor would. `None` where the
/// editor's own handling is already that: a selection, a line break, a
/// character that is one code point, or any other key.
///
/// `Some` says whether the text changed, as the editor's own input does.
fn delete_whole(
    composer: &mut TextArea<'static>,
    key: ratatui::crossterm::event::KeyEvent,
) -> Option<bool> {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use ratatui_textarea::CursorMove;

    if key.modifiers != KeyModifiers::NONE
        || !matches!(key.code, KeyCode::Backspace | KeyCode::Delete)
        || composer.selection_range().is_some()
    {
        return None;
    }
    let ratatui_textarea::DataCursor(row, col) = composer.cursor();
    let line = composer.lines().get(row)?;
    let split = line
        .char_indices()
        .nth(col)
        .map_or(line.len(), |(at, _)| at);
    let (before, after) = line.split_at(split);
    match key.code {
        KeyCode::Backspace => {
            let chars = before.graphemes(true).next_back()?.chars().count();
            if chars < 2 {
                return None;
            }
            for _ in 0..chars {
                composer.move_cursor(CursorMove::Back);
            }
            Some(composer.delete_str(chars))
        }
        KeyCode::Delete => {
            let chars = after.graphemes(true).next()?.chars().count();
            (chars >= 2).then(|| composer.delete_str(chars))
        }
        _ => None,
    }
}

/// A paste with every line break in it written as `\n`, which is what the
/// composer splits lines at. A terminal sends the lines of a paste apart with
/// a carriage return, as the Enter key is sent; a file copied from elsewhere
/// may hold `\r\n` or `\n`.
fn pasted_lines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// A paste for somewhere that holds one line: its lines joined by spaces.
fn joined_lines(text: &str) -> String {
    text.replace('\n', " ")
}

/// Now, in seconds since the Unix epoch. Zero on a clock set before it, which
/// makes every reset read as still to come rather than panicking on a machine
/// whose clock is wrong.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// One window, as Cost & usage reads it out: `5h window 62%, resets in 2h 14m`.
///
/// The reset is shown as the time left rather than as a wall clock, because
/// what the operator is deciding is whether to wait, and a clock time would
/// have to pick a time zone to be read in. A window whose reset has passed
/// says so instead of counting down from nothing: a replayed session is read
/// long after the window it recorded came back.
fn window_hint(label: &str, window: UsageWindow, now: u64) -> String {
    let used = percent(window.utilization);
    match window.resets_at {
        None => format!("{label} window {used}%, no reset time reported"),
        Some(at) if at <= now => format!("{label} window {used}%, already reset"),
        Some(at) => format!("{label} window {used}%, resets in {}", left(at - now)),
    }
}

/// How long is left, in the two coarsest units that say anything: `4d 6h`,
/// `2h 14m`, `47m`. Seconds are left out — a window is hours wide, and a
/// countdown to the second in a pane is a number that redraws for
/// nothing.
fn left(seconds: u64) -> String {
    let minutes = seconds / 60;
    match (minutes / 1_440, (minutes / 60) % 24, minutes % 60) {
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

/// A share of a window as whole percent. Not clamped: a backend reporting more
/// than the whole window is reporting something the operator has to see.
pub fn percent(utilization: f64) -> u64 {
    (utilization * 100.0).round() as u64
}

/// What an empty composer says it is for, and after it what else it does,
/// each only once it works.
const PLACEHOLDER: [&str; 5] = [
    "Ask for a change",
    "/ command or skill",
    "@ file",
    "! shell",
    "Ctrl+F find",
];

/// The key that searches the transcript.
const FIND_KEY: &str = "Ctrl+F";

/// What an empty composer says while it holds a command: where it runs, and
/// the one thing about it an operator could not guess.
const SHELL_PLACEHOLDER: &str = "A command to run here; the agent does not see what it prints";

/// How many files the list under an `@` word offers at once.
const MENTION_ROWS: usize = 8;

/// The character `key` types, where it is one typed as itself: no Ctrl or
/// Alt, which make it a binding, and not a control character, which the
/// composer's editor would take for a line break.
fn typed_char(key: ratatui::crossterm::event::KeyEvent) -> Option<char> {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    match (key.code, key.modifiers) {
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) if !c.is_control() => Some(c),
        _ => None,
    }
}

/// Whether `key` is Enter with nothing held, which sends the prompt.
fn is_plain_enter(key: ratatui::crossterm::event::KeyEvent) -> bool {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    key.code == KeyCode::Enter && key.modifiers == KeyModifiers::NONE
}

/// Whether `key` opens a line in the composer rather than sending it.
///
/// Shift+Enter, on a terminal that can report it. Ctrl+J on every terminal:
/// it is the line feed, a byte of its own rather than a modifier the terminal
/// may not pass on. Alt+Enter as well, which reaches the shell only where the
/// terminal is set to send Option as Meta: out of the box, Terminal.app sends
/// Option+Enter as Enter.
fn opens_a_line(key: ratatui::crossterm::event::KeyEvent) -> bool {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    match key.code {
        KeyCode::Enter => {
            key.modifiers == KeyModifiers::SHIFT || key.modifiers == KeyModifiers::ALT
        }
        KeyCode::Char('j') => key.modifiers == KeyModifiers::CONTROL,
        _ => false,
    }
}

/// The placeholder in at most `columns` columns: as many of the things the
/// composer does as fit whole, and what it is for however narrow it is.
///
/// `@ file` is left out where there are no files to name — a session outside
/// a repository, or one whose repository has not been read yet — since it
/// would be advertising a list that cannot open; `! shell` where nothing runs
/// commands, as in a recorded log being looked at; `/ command or skill` where
/// the backend has listed no commands of its own.
fn placeholder(columns: usize, can: Offers) -> String {
    let mut said = PLACEHOLDER[0].to_owned();
    for more in PLACEHOLDER[1..].iter().filter(|more| match **more {
        "@ file" => can.files,
        "! shell" => can.shell,
        "/ command or skill" => can.commands,
        _ => true,
    }) {
        let longer = format!("{said} · {more}");
        if crate::text::width(&longer) > columns {
            break;
        }
        said = longer;
    }
    said
}

/// What the composer can do beyond taking a prompt, which is what its
/// placeholder may advertise.
#[derive(Debug, Clone, Copy)]
struct Offers {
    /// There are files to name with `@`.
    files: bool,
    /// Something runs the operator's `!` commands.
    shell: bool,
    /// The backend has listed commands to pick with `/`.
    commands: bool,
}

/// Gives the composer the theme's colours.
///
/// The composer is `ratatui-textarea`, which keeps the styles it draws with
/// rather than taking them per frame, so a theme reaches it by being written
/// into it. Everything else the shell draws reads the theme at draw time.
fn paint_composer(composer: &mut TextArea<'static>, theme: &Theme) {
    composer.set_placeholder_style(Style::new().fg(theme.dim).bg(theme.pane_bg));
    composer.set_style(Style::new().fg(theme.fg).bg(theme.pane_bg));
    composer.set_cursor_line_style(Style::new().fg(theme.fg).bg(theme.pane_bg));
    composer.set_cursor_style(Style::new().fg(theme.pane_bg).bg(theme.hot));
}

/// What a reply the session was writing when its process stopped says of
/// itself.
const CUT_OFF: &str = "cut off: the session stopped here";

/// What the bar says once a stop of the running turn has been asked for.
const STOPPING_HINT: &str = "Stopping the turn · Ctrl+C again quits";

/// What the shell says once Esc has made the next key an F-key or a menu.
const ESCAPED_HINT: &str = "Esc — a digit now presses its F-key (1 Help … 0 Quit), a letter \
                            opens its menu: N Niobe · S Session · C Context · M Model · V View · \
                            H Help";

/// Where this project is published, which the About sheet names and the Help
/// menu's links go to.
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// How many lines PgUp and PgDn move a sheet.
const SHEET_PAGE: usize = 10;

/// The F-key a digit stands for after Esc: `1` to `9` are F1 to F9 and `0`
/// is F10, in the order the bar draws them.
fn function_key_of(digit: char) -> Option<u8> {
    match digit {
        '0' => Some(10),
        _ => digit
            .to_digit(10)
            .and_then(|n| u8::try_from(n).ok())
            .filter(|n| *n > 0),
    }
}

/// The hint a running `!` command leaves, which only holds while one does:
/// the last one ending takes it down rather than leave the bar naming a key
/// with nothing to stop.
fn stop_hint() -> String {
    format!("{} stops it", crate::shell::STOP_KEY)
}

/// A count of things in the transcript, as a figure.
fn count(things: usize) -> u64 {
    u64::try_from(things).unwrap_or(u64::MAX)
}

/// A tool's name as a person reads it.
///
/// An MCP tool arrives as `mcp__<server>__<tool>`, and the server as the
/// client registered it (`claude_ai_Notion`). It reads as the server's last
/// word and the tool, with the server's name taken off the tool's front where
/// it repeats it: `Notion·search`. Every other name is the backend's own.
pub fn tool_label(name: &str) -> String {
    let Some((server, tool)) = name
        .strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
    else {
        return name.to_owned();
    };
    let server = server.rsplit('_').next().unwrap_or(server);
    let prefix = format!("{}-", server.to_lowercase());
    let tool = match tool.to_lowercase().starts_with(&prefix) {
        true => tool.get(prefix.len()..).unwrap_or(tool),
        false => tool,
    };
    format!("{server}·{tool}")
}

/// The backend's one-line reading of a call, or its arguments where it had
/// none.
fn what_it_does(summary: Option<&str>, input: &str) -> String {
    match summary {
        Some(summary) => one_line(summary),
        None => one_line(input),
    }
}

/// Collapses whitespace so a tool's arguments stay on the one line beside its
/// name.
fn one_line(input: &str) -> String {
    let mut words = input.split_whitespace();
    let Some(first) = words.next() else {
        return String::new();
    };
    let mut result = String::with_capacity(input.len());
    result.push_str(first);
    for word in words {
        result.push(' ');
        result.push_str(word);
    }
    result
}

/// Bytes, short enough for a meta line.
pub fn human_bytes(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.1} kB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::LocalMoment;
    use niobe_core::event::{Backend, SessionMeta, Usage, UsageWindows};
    use niobe_core::permission::Rule;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;
    use ratatui_textarea::Key;
    use std::time::{Duration, Instant};

    fn app() -> App {
        App::new(Repo {
            name: "niobe".to_owned(),
            branch: Some("main".to_owned()),
            ..Default::default()
        })
    }

    #[test]
    fn a_section_folds_and_opens_again() {
        let mut app = app();

        assert!(Section::ALL.iter().all(|s| !app.folded(*s)));

        app.fold(Section::SubAgents);
        assert!(app.folded(Section::SubAgents));
        assert!(!app.folded(Section::WorkingTree), "one section, not all");

        app.fold(Section::SubAgents);
        assert!(!app.folded(Section::SubAgents));
    }

    #[test]
    fn folding_a_section_puts_the_pane_back_at_its_top() {
        let mut app = app();
        app.measured_pane(Pane::Changes, Rect::new(0, 0, 40, 10), 60, 10);
        app.scroll_pane(Pane::Changes, 20);
        assert_eq!(app.pane_scroll(Pane::Changes), 20);

        app.fold(Section::WorkingTree);

        assert_eq!(
            app.pane_scroll(Pane::Changes),
            0,
            "an offset measured against the old height scrolls past the new end"
        );
    }

    #[test]
    fn the_changes_pane_never_scrolls_past_either_end() {
        let mut app = app();
        app.measured_pane(Pane::Changes, Rect::new(0, 0, 40, 10), 25, 10);

        app.scroll_pane(Pane::Changes, 100);
        assert_eq!(
            app.pane_scroll(Pane::Changes),
            15,
            "content 25 in a viewport of 10"
        );

        app.scroll_pane(Pane::Changes, -100);
        assert_eq!(app.pane_scroll(Pane::Changes), 0);
    }

    #[test]
    fn a_pane_taller_than_what_it_holds_does_not_scroll_at_all() {
        let mut app = app();
        app.measured_pane(Pane::Changes, Rect::new(0, 0, 40, 30), 12, 30);

        app.scroll_pane(Pane::Changes, 5);

        assert_eq!(app.pane_scroll(Pane::Changes), 0);
    }

    #[test]
    fn a_pane_that_shrank_under_the_scroll_comes_back_to_what_it_holds() {
        let mut app = app();
        app.measured_pane(Pane::Changes, Rect::new(0, 0, 40, 10), 60, 10);
        app.scroll_pane(Pane::Changes, 50);
        assert_eq!(app.pane_scroll(Pane::Changes), 50);

        // The next frame is a smaller session, or a folded section.
        app.measured_pane(Pane::Changes, Rect::new(0, 0, 40, 10), 20, 10);

        assert_eq!(app.pane_scroll(Pane::Changes), 10);
    }

    /// The shell as a wide frame lays it out: the session pane on the left and
    /// the two scrolling panes stacked on the right, each holding more than it
    /// shows.
    fn laid_out() -> App {
        let mut app = app();
        app.measured_session(Rect::new(0, 1, 60, 28));
        app.measured(100, 20);
        app.measured_pane(Pane::Changes, Rect::new(62, 8, 30, 10), 40, 10);
        app.measured_pane(Pane::Activity, Rect::new(62, 20, 30, 8), 30, 8);
        app
    }

    fn click_at(column: u16, row: u16) -> MouseEvent {
        use ratatui::crossterm::event::MouseButton;
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn the_session_has_the_keyboard_and_tab_walks_the_panes_round_to_it() {
        let mut app = laid_out();
        assert_eq!(app.focus(), Focus::Session);

        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus(), Focus::Pane(Pane::Changes));
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus(), Focus::Pane(Pane::Activity));
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus(), Focus::Session, "the cycle wraps");
    }

    /// `a` groups the transcript by agent where a pane beside it has the
    /// keyboard; in the composer it is a letter of the prompt.
    #[test]
    fn a_groups_by_agent_from_a_pane_and_is_typed_in_the_composer() {
        let mut app = laid_out();
        app.on_key(key(KeyCode::Char('a')));
        assert!(!app.grouped_by_agent());
        assert_eq!(app.composed(), "a");

        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Char('a')));
        assert!(app.grouped_by_agent());
        assert_eq!(app.composed(), "a", "nothing was typed");
        app.on_key(key(KeyCode::Char('a')));
        assert!(!app.grouped_by_agent(), "the key groups and ungroups");
    }

    #[test]
    fn a_pane_that_is_not_on_screen_is_never_given_the_keyboard() {
        let mut app = app();
        app.measured(100, 20);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus(), Focus::Session, "nothing else is drawn");

        let mut app = laid_out();
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus(), Focus::Pane(Pane::Changes));
        // The terminal narrowed and the right-hand stack went with it.
        app.right_stack_hidden();
        assert_eq!(app.focus(), Focus::Session);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus(), Focus::Session);
    }

    #[test]
    fn the_scroll_keys_move_the_focused_pane_and_nothing_else() {
        let mut app = laid_out();
        app.on_key(key(KeyCode::Tab));

        app.on_key(key(KeyCode::PageDown));
        assert_eq!(
            app.pane_scroll(Pane::Changes),
            10,
            "a page is the pane's height"
        );
        app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(app.pane_scroll(Pane::Changes), 9);
        app.on_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
        assert_eq!(app.pane_scroll(Pane::Changes), 30);
        app.on_key(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL));
        assert_eq!(app.pane_scroll(Pane::Changes), 0);

        assert!(
            app.follows_tail(),
            "the transcript was not the focused pane"
        );
        assert_eq!(app.pane_scroll(Pane::Activity), 0);

        // And with the keyboard back, the transcript pages as it always has.
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::PageUp));
        assert_eq!(app.scroll(), 60);
        assert_eq!(app.pane_scroll(Pane::Changes), 0);
    }

    #[test]
    fn a_wheel_notch_gives_the_pane_it_scrolls_the_keyboard() {
        let mut app = laid_out();

        app.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 70,
            row: 22,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(app.focus(), Focus::Pane(Pane::Activity));
        assert_eq!(app.pane_scroll(Pane::Activity), 3);
        assert!(app.follows_tail());
    }

    #[test]
    fn a_click_on_a_pane_gives_it_the_keyboard() {
        let mut app = laid_out();

        app.on_mouse(click_at(70, 10));
        assert_eq!(app.focus(), Focus::Pane(Pane::Changes));

        app.on_mouse(click_at(10, 10));
        assert_eq!(app.focus(), Focus::Session);
    }

    #[test]
    fn typing_needs_no_focus_moved_and_takes_the_keyboard_back_with_it() {
        let mut app = laid_out();
        app.on_key(key(KeyCode::Tab));

        app.on_key(key(KeyCode::Char('h')));
        app.on_key(key(KeyCode::Char('i')));

        assert_eq!(app.composed(), "hi");
        assert_eq!(
            app.focus(),
            Focus::Session,
            "Enter must send what was typed, not fold a section"
        );
    }

    #[test]
    fn the_arrows_walk_a_focused_panes_sections_and_enter_folds_the_one_under_the_cursor() {
        let mut app = laid_out();
        app.measured_sections(
            Pane::Activity,
            vec![(Section::SubAgents, 1), (Section::Tools, 19)],
        );
        assert_eq!(app.section_cursor(Pane::Activity), None, "not focused");

        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.section_cursor(Pane::Activity), Some(Section::SubAgents));

        app.on_key(key(KeyCode::Down));
        assert_eq!(app.section_cursor(Pane::Activity), Some(Section::Tools));
        assert_eq!(
            app.pane_scroll(Pane::Activity),
            12,
            "the section under the cursor is scrolled into view"
        );
        app.on_key(key(KeyCode::Down));
        assert_eq!(
            app.section_cursor(Pane::Activity),
            Some(Section::Tools),
            "the last section is as far as it goes"
        );

        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Enter));
        assert!(app.folded(Section::SubAgents));
        assert!(app.entries().is_empty(), "Enter folded rather than sent");
        assert!(
            app.pane_scroll(Pane::Activity) <= 1,
            "the folded header stays in view"
        );
    }

    #[test]
    fn a_question_holding_the_keyboard_is_where_the_focus_is() {
        let mut app = laid_out();
        app.on_key(key(KeyCode::Tab));
        app.apply(&prompt(Some("rm -rf build")));

        assert_eq!(app.focus(), Focus::Session);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(
            app.focus(),
            Focus::Pane(Pane::Changes),
            "put off, the question gives the keyboard back where it was"
        );
    }

    #[test]
    fn paging_up_a_transcript_that_fits_leaves_it_on_the_newest_line() {
        let mut app = app();
        app.measured(4, 20);

        app.scroll_up(20);
        assert!(app.follows_tail(), "there was nothing to scroll back to");
        app.scroll_to_head();
        assert!(app.follows_tail());
    }

    #[test]
    fn clicking_the_way_back_down_returns_to_the_newest_line() {
        let mut app = laid_out();
        app.scroll_to_head();
        app.drew_jump(Some(Rect::new(40, 18, 18, 1)));

        app.on_mouse(click_at(45, 18));

        assert!(app.follows_tail());
        assert_eq!(app.scroll(), 80);
    }

    #[test]
    fn deltas_accumulate_into_one_entry_and_the_message_replaces_them() {
        let mut app = app();
        app.apply(&Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "default".to_owned(),
            model: "opus-5".to_owned(),
            backend_session: None,
        }));
        app.apply(&Event::AssistantDelta {
            text: "Reading ".to_owned(),
        });
        app.apply(&Event::AssistantDelta {
            text: "fetch.ts".to_owned(),
        });

        assert_eq!(app.entries().len(), 1);
        assert_eq!(app.entries()[0].body, "Reading fetch.ts");
        assert_eq!(app.entries()[0].head, "claude");
        assert!(app.entries()[0].streaming);

        app.apply(&Event::AssistantMessage {
            text: "Reading fetch.ts and its callers.".to_owned(),
            agent: None,
        });
        assert_eq!(app.entries().len(), 1);
        assert_eq!(app.entries()[0].body, "Reading fetch.ts and its callers.");
        assert!(!app.entries()[0].streaming);
    }

    /// A sub-agent's words are its own: drawn under its name, and never
    /// the end of the reply the session is streaming, which keeps its place
    /// and its deltas.
    #[test]
    fn a_sub_agents_words_are_its_own_and_leave_the_sessions_reply_streaming() {
        let mut app = app();
        app.apply(&Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "default".to_owned(),
            model: "opus-5".to_owned(),
            backend_session: None,
        }));
        app.apply(&Event::AgentSpawn {
            id: AgentId::new("toolu_a"),
            parent: None,
            kind: None,
            label: "deep-reasoner: Review cache".to_owned(),
        });
        app.apply(&Event::AssistantDelta {
            text: "While they ".to_owned(),
        });
        app.apply(&Event::AssistantMessage {
            text: "Let me check how eviction is triggered.".to_owned(),
            agent: Some(AgentId::new("toolu_a")),
        });
        app.apply(&Event::AssistantDelta {
            text: "work".to_owned(),
        });
        app.apply(&Event::AssistantMessage {
            text: "While they work, I will wait.".to_owned(),
            agent: None,
        });

        let entries: Vec<(EntryKind, &str, &str, bool)> = app
            .entries()
            .iter()
            .map(|entry| {
                (
                    entry.kind,
                    entry.head.as_str(),
                    entry.body.as_str(),
                    entry.streaming,
                )
            })
            .collect();
        assert_eq!(
            entries,
            [
                (
                    EntryKind::Agent,
                    "claude",
                    "While they work, I will wait.",
                    false
                ),
                (
                    EntryKind::SubAgent,
                    "deep-reasoner: Review cache",
                    "Let me check how eviction is triggered.",
                    false
                ),
            ]
        );
    }

    /// An agent tagged by its first word while it was the only one is
    /// renamed on the rows it has already made once a second agent shares
    /// that word.
    #[test]
    fn a_second_agent_renames_the_first_agents_rows() {
        let mut app = app();
        let spawn = |id: &str, label: &str| Event::AgentSpawn {
            id: AgentId::new(id),
            parent: None,
            kind: None,
            label: label.to_owned(),
        };
        app.apply(&spawn("toolu_a", "Review catalog/fetch.py"));
        app.apply(&Event::ToolCallStart {
            id: ToolCallId::new("r1"),
            name: "Read".to_owned(),
            input: String::new(),
            summary: None,
            agent: Some(AgentId::new("toolu_a")),
        });
        assert_eq!(app.entries()[0].agent.as_deref(), Some("review"));

        app.apply(&spawn("toolu_b", "Review catalog/cache.py"));
        assert_eq!(app.entries()[0].agent.as_deref(), Some("fetch"));
    }

    #[test]
    fn a_tool_call_fills_in_its_own_entry_when_it_ends() {
        let mut app = app();
        app.apply(&Event::ToolCallStart {
            id: "t1".into(),
            name: "Read".to_owned(),
            input: "catalog/fetch.ts".to_owned(),
            summary: None,
            agent: None,
        });
        assert!(app.entries()[0].streaming);

        app.apply(&Event::ToolCallEnd {
            id: "t1".into(),
            name: "Read".to_owned(),
            input: "catalog/fetch.ts".to_owned(),
            output: "212 lines".to_owned(),
            bytes: 4_096,
            outcome: ToolOutcome::Ok,
            summary: None,
            exit_code: None,
            error: None,
        });

        assert_eq!(app.entries().len(), 1, "the end opened a second entry");
        let [call] = app.entries()[0].calls.as_slice() else {
            panic!("one call: {:?}", app.entries()[0]);
        };
        assert_eq!(call.what, "catalog/fetch.ts");
        assert_eq!(
            (call.outcome, call.bytes),
            (Some(ToolOutcome::Ok), Some(4_096))
        );
        assert!(!app.entries()[0].streaming);
    }

    #[test]
    fn a_failed_call_is_recoloured_and_an_unmatched_end_still_shows() {
        let mut app = app();
        app.apply(&Event::ToolCallStart {
            id: "t1".into(),
            name: "Bash".to_owned(),
            input: "npm test".to_owned(),
            summary: None,
            agent: None,
        });
        app.apply(&Event::ToolCallEnd {
            id: "t1".into(),
            name: "Bash".to_owned(),
            input: "npm test".to_owned(),
            output: "1 failing".to_owned(),
            bytes: 128,
            outcome: ToolOutcome::Failed,
            summary: None,
            exit_code: None,
            error: None,
        });
        app.apply(&Event::ToolCallEnd {
            id: "t9".into(),
            name: "Edit".to_owned(),
            input: "cache.ts".to_owned(),
            output: String::new(),
            bytes: 0,
            outcome: ToolOutcome::Denied,
            summary: None,
            exit_code: None,
            error: None,
        });

        assert_eq!(app.entries()[0].kind, EntryKind::Failure);
        assert_eq!(app.entries().len(), 2);
        assert_eq!(app.entries()[1].head, "Edit");
        assert_eq!(app.entries()[1].kind, EntryKind::Failure);
        let [call] = app.entries()[1].calls.as_slice() else {
            panic!("one call: {:?}", app.entries()[1]);
        };
        assert_eq!(
            (call.what.as_str(), call.outcome),
            ("cache.ts", Some(ToolOutcome::Denied))
        );
    }

    #[test]
    fn numbers_go_to_the_panes_and_not_to_the_transcript() {
        let mut app = app();
        app.apply(&Event::AgentSpawn {
            id: "a1".into(),
            parent: None,
            kind: None,
            label: "test-writer".to_owned(),
        });

        assert!(app.entries().is_empty());
        assert_eq!(app.session().agents_spawned(), 1);
    }

    fn png(tag: &[u8]) -> niobe_core::image::Image {
        let mut data = b"\x89PNG\r\n\x1a\n".to_vec();
        data.extend_from_slice(tag);
        niobe_core::image::Image::from_bytes(data).expect("a PNG signature")
    }

    #[test]
    fn a_pasted_image_path_asks_for_the_file_rather_than_typing_the_path() {
        let mut app = app();
        app.on_paste("'/Users/me/Desktop/Screen Shot.png'");

        assert_eq!(
            app.take_image_requests(),
            [Source::File("/Users/me/Desktop/Screen Shot.png".to_owned())]
        );
        assert_eq!(app.composed(), "");
    }

    #[test]
    fn an_empty_paste_asks_for_the_image_on_the_clipboard() {
        let mut app = app();
        app.on_paste("");
        assert_eq!(app.take_image_requests(), [Source::Clipboard]);
        assert_eq!(app.hint(), Some("Reading the image on the clipboard…"));
    }

    #[test]
    fn a_pasted_path_that_is_no_image_goes_into_the_prompt_as_it_would_have() {
        let mut app = app();
        app.on_paste("/tmp/notes.png");
        app.fetched(Fetched {
            source: Source::File("/tmp/notes.png".to_owned()),
            image: Err("it is not a PNG, JPEG, GIF or WebP image".to_owned()),
        });

        assert_eq!(app.composed(), "/tmp/notes.png");
        assert_eq!(
            app.hint(),
            Some("/tmp/notes.png is not attached: it is not a PNG, JPEG, GIF or WebP image")
        );
    }

    #[test]
    fn an_image_lands_where_the_cursor_is() {
        let mut app = app();
        app.on_paste("before  after");
        for _ in 0.."after".len() + 1 {
            app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }
        app.fetched(Fetched {
            source: Source::Clipboard,
            image: Ok(png(b"a")),
        });

        assert_eq!(app.composed(), "before [Image #1]  after");
    }

    #[test]
    fn images_attached_after_a_resume_are_numbered_past_the_ones_it_holds() {
        let mut app = app();
        app.apply(&Event::UserMessage {
            text: "earlier: [Image #2]".to_owned(),
        });
        app.fetched(Fetched {
            source: Source::Clipboard,
            image: Ok(png(b"a")),
        });

        assert_eq!(app.composed(), "[Image #3] ");
    }

    #[test]
    fn submitting_records_the_prompt_and_says_nothing_is_listening() {
        let mut app = app();
        app.type_into_composer(Input {
            key: Key::Char('h'),
            ..Default::default()
        });
        app.type_into_composer(Input {
            key: Key::Char('i'),
            ..Default::default()
        });
        assert_eq!(app.composed(), "hi");

        app.submit();
        assert_eq!(app.composed(), "");
        assert_eq!(app.session().user_messages(), 1);
        assert_eq!(app.entries()[0].kind, EntryKind::User);
        assert_eq!(app.entries()[1].kind, EntryKind::Notice);
    }

    /// `app` with `text` typed into its composer, then `enter` with `modifiers`.
    fn typed_then_enter(
        app: &mut App,
        text: &str,
        modifiers: ratatui::crossterm::event::KeyModifiers,
    ) {
        for c in text.chars() {
            app.type_into_composer(Input {
                key: Key::Char(c),
                ..Default::default()
            });
        }
        app.on_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Enter,
            modifiers,
        ));
    }

    /// `app`, told the backend offers `/model`.
    fn offering_model(mut app: App) -> App {
        app.apply(&Event::Commands {
            commands: vec![SlashCommand {
                name: "model".to_owned(),
                description: String::new(),
                argument_hint: None,
            }],
        });
        app
    }

    fn submitted(app: &mut App, text: &str) -> Vec<Event> {
        for c in text.chars() {
            app.type_into_composer(Input {
                key: Key::Char(c),
                ..Default::default()
            });
        }
        app.submit();
        app.take_produced()
    }

    /// Typed as a prompt, the backend would move the model only when the next
    /// turn starts and say nothing of it until then; asked for the way the
    /// picker asks, the menu row names it at once.
    #[test]
    fn a_model_command_with_a_name_moves_the_model_as_the_picker_does() {
        let mut app = offering_model(app());
        assert_eq!(
            submitted(&mut app, "/model  haiku "),
            [Event::ModelSelected {
                model: "haiku".to_owned()
            }]
        );
        assert_eq!(app.session().model(), Some("haiku"));
        assert_eq!(app.composed(), "");
        assert_eq!(app.session().user_messages(), 0);
    }

    #[test]
    fn a_model_command_the_picker_cannot_stand_for_goes_to_the_backend_as_typed() {
        for text in ["/model", "/model haiku please", "/models haiku"] {
            let mut app = offering_model(app());
            assert_eq!(
                submitted(&mut app, text),
                [Event::UserMessage {
                    text: text.to_owned()
                }]
            );
        }

        let mut app = app();
        assert_eq!(
            submitted(&mut app, "/model haiku"),
            [Event::UserMessage {
                text: "/model haiku".to_owned()
            }],
            "a backend that lists no `/model` is sent what was typed"
        );
    }

    #[test]
    fn a_cleared_conversation_is_a_line_in_the_transcript() {
        let mut app = app();
        app.apply(&Event::Cleared);
        let last = app.entries().last().expect("an entry");
        assert_eq!(last.kind, EntryKind::Notice);
        assert_eq!(last.head, "cleared");
    }

    #[test]
    fn shift_enter_opens_a_line_and_enter_still_sends() {
        use ratatui::crossterm::event::KeyModifiers;

        let mut app = app().reports_shift_enter();
        typed_then_enter(&mut app, "one", KeyModifiers::SHIFT);
        typed_then_enter(&mut app, "two", KeyModifiers::NONE);

        assert_eq!(
            app.take_produced(),
            [Event::UserMessage {
                text: "one\ntwo".to_owned()
            }]
        );
    }

    #[test]
    fn alt_enter_opens_a_line_on_any_terminal() {
        use ratatui::crossterm::event::KeyModifiers;

        for mut app in [app(), app().reports_shift_enter()] {
            typed_then_enter(&mut app, "one", KeyModifiers::ALT);
            typed_then_enter(&mut app, "two", KeyModifiers::NONE);

            assert_eq!(
                app.take_produced(),
                [Event::UserMessage {
                    text: "one\ntwo".to_owned()
                }]
            );
        }
    }

    #[test]
    fn ctrl_j_opens_a_line_on_any_terminal() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        for mut app in [app(), app().reports_shift_enter()] {
            for c in "one".chars() {
                app.type_into_composer(Input {
                    key: Key::Char(c),
                    ..Default::default()
                });
            }
            app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
            typed_then_enter(&mut app, "two", KeyModifiers::NONE);

            assert_eq!(
                app.take_produced(),
                [Event::UserMessage {
                    text: "one\ntwo".to_owned()
                }]
            );
        }
    }

    #[test]
    fn the_key_named_for_a_new_line_is_the_one_the_terminal_can_report() {
        assert_eq!(app().newline_key(), "Ctrl+J");
        assert_eq!(app().reports_shift_enter().newline_key(), "Shift+Enter");
    }

    #[test]
    fn a_shift_enter_that_arrives_is_the_key_named_from_then_on() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = app();
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(app.newline_key(), "Ctrl+J");

        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

        assert_eq!(app.newline_key(), "Shift+Enter");
        assert!(!app.sends_enter_for_shift_enter());
    }

    #[test]
    fn a_submitted_prompt_is_handed_out_once_to_be_kept() {
        let mut app = app();
        app.type_into_composer(Input {
            key: Key::Char('h'),
            ..Default::default()
        });
        app.submit();

        assert_eq!(
            app.take_produced(),
            [Event::UserMessage {
                text: "h".to_owned()
            }]
        );
        assert!(app.take_produced().is_empty());
    }

    #[test]
    fn events_folded_in_from_elsewhere_are_not_handed_out_again() {
        let mut app = app();
        app.apply(&Event::UserMessage {
            text: "from the store".to_owned(),
        });
        assert!(app.take_produced().is_empty());
    }

    #[test]
    fn an_event_that_could_not_be_kept_says_so_in_the_transcript() {
        let mut app = app();
        app.not_kept("disk full");

        let entry = app.entries().last().expect("an entry was pushed");
        assert_eq!(entry.kind, EntryKind::Failure);
        assert_eq!(entry.head, "not saved");
        assert!(entry.body.ends_with("disk full"), "{}", entry.body);
    }

    /// A usage record carrying a cost the backend reported.
    fn priced(cost_usd: f64) -> Event {
        Event::Usage(Usage {
            input: 10,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 0,
            model: "opus-5".to_owned(),
            cost_usd: Some(cost_usd),
            cost_basis: None,
            settles_model: false,
            fast: false,
        })
    }

    /// A prompt as the bridge produces one.
    fn prompt(target: Option<&str>) -> Event {
        Event::PermissionRequest {
            id: "t1".into(),
            tool: "Bash".to_owned(),
            input: r#"{"command":"rm -rf build"}"#.to_owned(),
            target: target.map(str::to_owned),
            agent: None,
        }
    }

    fn key(code: ratatui::crossterm::event::KeyCode) -> ratatui::crossterm::event::KeyEvent {
        ratatui::crossterm::event::KeyEvent::new(
            code,
            ratatui::crossterm::event::KeyModifiers::NONE,
        )
    }

    #[test]
    fn esc_then_a_digit_presses_the_function_key_of_that_number() {
        use ratatui::crossterm::event::KeyCode;

        let mut app = app();
        app.on_key(key(KeyCode::Esc));
        assert!(
            app.hint().is_some_and(|hint| hint.contains("0 Quit")),
            "Esc says what the digit after it does: {:?}",
            app.hint()
        );
        app.on_key(key(KeyCode::Char('5')));
        assert!(app.diffs_open(), "Esc then 5 is F5 Diff");
        assert_eq!(app.composer().lines(), [""]);

        // It is one key after Esc, not every key after it.
        app.on_key(key(KeyCode::Char('5')));
        assert!(app.diffs_open());
        assert_eq!(app.composer().lines(), ["5"]);

        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::Char('0')));
        assert!(app.should_quit(), "Esc then 0 is F10");
    }

    #[test]
    fn alt_and_a_digit_is_the_same_function_key() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        // Esc and a digit that reach the terminal together, or Option sent
        // as Meta, arrive as the digit with Alt held.
        let mut app = app();
        app.on_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::ALT));
        assert!(app.diffs_open());
        assert_eq!(app.composer().lines(), [""]);
        app.on_key(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::ALT));
        assert!(app.should_quit(), "Alt+0 is F10");
    }

    #[test]
    fn a_key_after_esc_that_is_neither_a_digit_nor_a_menus_letter_is_typed() {
        use crate::theme::CYBER;
        use ratatui::crossterm::event::KeyCode;

        let mut app = app();
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::Char('x')));
        app.on_key(key(KeyCode::Char('9')));
        assert_eq!(app.composer().lines(), ["x9"]);
        assert_eq!(*app.theme(), CYBER);
    }

    #[test]
    fn a_digit_after_the_esc_that_closed_a_search_is_typed() {
        use crate::theme::CYBER;
        use ratatui::crossterm::event::KeyCode;

        // That Esc already did something; the digit after it is the start of
        // what the operator types next.
        let mut app = app();
        ctrl_f(&mut app);
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::Char('9')));
        assert_eq!(app.composer().lines(), ["9"]);
        assert_eq!(*app.theme(), CYBER);
    }

    #[test]
    fn a_shell_on_a_deep_terminal_draws_every_theme_at_that_depth() {
        use crate::theme::{CLASSIC, CYBER_TRUE, Depth, MODERN_TRUE, NEO, NEO_TRUE};

        // In either order: the depth is the terminal's, the theme the
        // operator's, and neither is allowed to undo the other.
        let depth_first = app().with_depth(Depth::TrueColour).with_theme(NEO);
        assert_eq!(*depth_first.theme(), NEO_TRUE);
        let mut app = app().with_theme(NEO).with_depth(Depth::TrueColour);
        assert_eq!(*app.theme(), NEO_TRUE);
        assert_eq!(
            app.composer().style(),
            Style::new().fg(NEO_TRUE.fg).bg(NEO_TRUE.pane_bg)
        );

        for (name, drawn) in [
            (MODERN_TRUE.name, MODERN_TRUE),
            (CYBER_TRUE.name, CYBER_TRUE),
            (CLASSIC.name, CLASSIC),
        ] {
            app.choose(Purpose::Theme, name.to_owned());
            assert_eq!(*app.theme(), drawn);
        }
    }

    #[test]
    fn a_shell_opened_on_a_theme_draws_its_composer_in_it() {
        use crate::theme::NEO;

        let app = app().with_theme(NEO);

        assert_eq!(*app.theme(), NEO);
        assert_eq!(
            app.composer().placeholder_style(),
            Some(Style::new().fg(NEO.dim).bg(NEO.pane_bg))
        );
    }

    #[test]
    fn a_session_that_ended_leaves_no_prompt_to_answer() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = app();
        app.apply(&Event::UserMessage {
            text: "clean up".to_owned(),
        });
        app.apply(&prompt(Some("rm -rf build")));
        app.apply(&Event::PermissionRequest {
            id: "t2".into(),
            tool: "Bash".to_owned(),
            input: r#"{"command":"ls"}"#.to_owned(),
            target: Some("ls".to_owned()),
            agent: None,
        });
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.ask_focus(), AskFocus::Deferred);

        app.apply(&Event::Error {
            message: "the `claude` session ended with exit status: 3: crashed".to_owned(),
            fatal: true,
        });

        assert!(app.asking().is_none(), "{:?}", app.asking());
        assert_eq!(app.asks_waiting(), 0);
        assert_eq!(app.ask_focus(), AskFocus::Choosing);
        assert!(app.session().pending_permissions().is_empty());
        // Nothing is left for a key to answer, so nothing goes to a backend
        // that could not take it.
        app.on_key(key(KeyCode::Enter));
        assert!(app.take_produced().is_empty());
    }

    #[test]
    fn a_session_that_ended_leaves_no_call_or_agent_running_but_the_operators() {
        let mut app = app();
        app.apply(&Event::UserMessage {
            text: "review it".to_owned(),
        });
        app.apply(&Event::AgentSpawn {
            id: "a1".into(),
            parent: None,
            kind: None,
            label: "Review fetch".to_owned(),
        });
        app.apply(&start("t1", "Bash", "cargo test", None));
        app.apply(&start("t2", "Bash", "cargo build", None));
        app.apply(&start("op1", crate::shell::OPERATOR_SHELL, "git log", None));

        app.apply(&Event::Error {
            message: "the `claude` session ended with exit status: 3: crashed".to_owned(),
            fatal: true,
        });

        let calls: Vec<(&str, bool, bool)> = app
            .entries()
            .iter()
            .flat_map(|entry| &entry.calls)
            .map(|call| (call.what.as_str(), call.running(), call.interrupted))
            .collect();
        assert_eq!(
            calls,
            [
                ("cargo test", false, true),
                ("cargo build", false, true),
                ("git log", true, false),
            ]
        );
        let bash = &app.entries()[1];
        assert!(
            !bash.streaming,
            "the calls the session cut short still spin"
        );
        assert!(!bash.calls.iter().any(Call::failed));
        assert_eq!(
            app.agents()
                .iter()
                .map(|agent| (agent.outcome, agent.interrupted))
                .collect::<Vec<_>>(),
            [(None, true)]
        );
        assert!(app.activity().is_none(), "{:?}", app.activity());
    }

    #[test]
    fn a_prompt_waits_on_the_operator_and_is_answered_once() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = app();
        app.apply(&prompt(Some("rm -rf build")));

        let ask = app.asking().expect("the transcript has a question");
        assert_eq!(ask.tool, "Bash");
        assert_eq!(ask.target.as_deref(), Some("rm -rf build"));
        assert_eq!(app.asks_waiting(), 0);
        assert_eq!(app.ask_selected(), Answer::Once);

        app.on_key(key(KeyCode::Enter));

        assert!(app.asking().is_none());
        assert_eq!(
            app.take_produced(),
            [Event::PermissionResponse {
                id: "t1".into(),
                decision: PermissionDecision::Allow,
                message: None,
            }]
        );
        assert!(
            app.allowed().is_empty(),
            "allowing once left a standing rule behind"
        );
    }

    #[test]
    fn the_options_are_numbered_in_the_order_they_are_offered() {
        let mut targeted = app();
        targeted.apply(&prompt(Some("rm -rf build")));
        assert_eq!(
            targeted.ask_options(),
            [
                Answer::Once,
                Answer::AlwaysTool,
                Answer::AlwaysTarget,
                Answer::No
            ]
        );

        // A prompt with no target has nothing to pin, and the numbers close up
        // rather than leaving a gap the operator would have to count past.
        let mut untargeted = app();
        untargeted.apply(&prompt(None));
        assert_eq!(
            untargeted.ask_options(),
            [Answer::Once, Answer::AlwaysTool, Answer::No]
        );
    }

    #[test]
    fn a_refusal_is_visible_in_the_transcript() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = app();
        app.apply(&prompt(Some("rm -rf build")));

        app.on_key(key(KeyCode::Char('4')));
        app.on_key(key(KeyCode::Enter));

        let entry = app.entries().last().expect("an entry was pushed");
        assert_eq!(entry.kind, EntryKind::Failure);
        assert_eq!(entry.head, "denied");
        assert_eq!(entry.meta, "Bash · rm -rf build");
        assert_eq!(app.session().permissions_denied(), 1);
    }

    /// A rule about an empty target is written `Bash()`, which the next start
    /// refuses to read, so the answer that would write it is not offered.
    #[test]
    fn a_prompt_with_an_empty_target_offers_no_standing_answer_about_it() {
        let mut app = app();
        app.apply(&prompt(Some("")));

        let ask = app.asking().expect("the prompt is up");
        assert!(ask.target_rule().is_none());
        assert!(!ask.options().contains(&Answer::AlwaysTarget));
        assert!(ask.options().contains(&Answer::AlwaysTool));
    }

    #[test]
    fn always_this_target_on_a_command_ending_in_a_glob_allows_only_that_command() {
        use ratatui::crossterm::event::KeyCode;

        let mut app = app();
        app.apply(&prompt(Some("rm -rf build/*")));
        let rule = app
            .asking()
            .and_then(Ask::target_rule)
            .expect("the prompt has a target");
        assert!(!rule.covers("Bash", Some("rm -rf build/ ~")));

        app.on_key(key(KeyCode::Char('3')));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.take_rules(), [Rule::targeted("Bash", "rm -rf build/*")]);
        assert!(app.allowed().allows("Bash", Some("rm -rf build/*")));
        assert!(!app.allowed().allows("Bash", Some("rm -rf build/ ~")));
    }

    #[test]
    fn always_this_tool_and_always_this_target_store_the_rule_they_name() {
        use ratatui::crossterm::event::KeyCode;

        let mut by_target = app();
        by_target.apply(&prompt(Some("rm -rf build")));
        by_target.on_key(key(KeyCode::Char('3')));
        by_target.on_key(key(KeyCode::Enter));
        assert_eq!(
            by_target.take_rules(),
            [Rule::targeted("Bash", "rm -rf build")]
        );
        assert!(!by_target.allowed().allows("Bash", Some("rm -rf dist")));

        let mut by_tool = app();
        by_tool.apply(&prompt(Some("rm -rf build")));
        by_tool.on_key(key(KeyCode::Char('2')));
        by_tool.on_key(key(KeyCode::Enter));
        assert_eq!(by_tool.take_rules(), [Rule::tool("Bash")]);
        assert!(by_tool.allowed().allows("Bash", Some("anything at all")));

        // Both are allowed as well as remembered, and the stream says the
        // answer was a standing one.
        for mut app in [by_target, by_tool] {
            assert_eq!(
                app.take_produced(),
                [Event::PermissionResponse {
                    id: "t1".into(),
                    decision: PermissionDecision::AllowAlways,
                    message: None,
                }]
            );
        }
    }

    #[test]
    fn a_number_past_the_last_option_selects_nothing() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = app();
        app.apply(&prompt(None));

        app.on_key(key(KeyCode::Char('4')));
        assert_eq!(app.ask_selected(), Answer::Once);
        app.on_key(key(KeyCode::Char('3')));
        assert_eq!(app.ask_selected(), Answer::No);

        assert!(app.take_rules().is_empty());
        assert!(
            app.take_produced().is_empty(),
            "a number answered by itself"
        );
    }

    #[test]
    fn a_prompt_takes_the_keyboard_from_the_composer() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = app();
        app.apply(&prompt(Some("rm -rf build")));

        // `x` is neither an answer nor a quit: under a question it is nothing.
        app.on_key(key(KeyCode::Char('x')));

        assert_eq!(app.composed(), "");
        assert!(app.asking().is_some());
    }

    #[test]
    fn quitting_works_with_a_prompt_on_screen() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = app();
        app.apply(&prompt(Some("rm -rf build")));

        app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));

        assert!(app.should_quit());
    }

    #[test]
    fn ctrl_z_asks_to_suspend_once_even_with_a_prompt_on_screen() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = app();
        app.apply(&prompt(Some("rm -rf build")));

        app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));

        assert!(app.take_suspend());
        assert!(!app.take_suspend(), "one Ctrl+Z is one stop");
        assert!(!app.should_quit());
        assert!(app.asking().is_some(), "the question is still waiting");
        assert_eq!(app.composed(), "");
    }

    #[test]
    fn prompts_are_answered_in_the_order_they_arrived() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = app();
        app.apply(&prompt(Some("rm -rf build")));
        app.apply(&Event::PermissionRequest {
            id: "t2".into(),
            tool: "Write".to_owned(),
            input: r#"{"file_path":"/repo/a.rs"}"#.to_owned(),
            target: Some("/repo/a.rs".to_owned()),
            agent: None,
        });

        assert_eq!(app.asks_waiting(), 1);
        app.on_key(key(KeyCode::Enter));

        assert_eq!(
            app.asking().map(|ask| ask.tool.clone()),
            Some("Write".to_owned())
        );
        assert_eq!(app.asks_waiting(), 0);
    }

    #[test]
    fn a_question_names_the_turn_it_is_blocking() {
        let mut app = sent(sent(app().attached(), "first"), "second");
        app.apply(&prompt(Some("rm -rf build")));
        assert_eq!(app.asking().map(|ask| ask.turn), Some(2));
    }

    /// What the operator sees as one character can be several code points: a
    /// family emoji joined by zero-width joiners, a letter and the accent
    /// that combines with it. Backspace takes it whole, or what is sent to
    /// the model is half of it.
    #[test]
    fn backspace_deletes_what_is_seen_as_one_character() {
        use ratatui::crossterm::event::KeyCode;
        for (typed, left) in [
            ("hi \u{1f469}\u{200d}\u{1f469}\u{200d}\u{1f467}", "hi "),
            ("a\u{301}", ""),
            ("ab", "a"),
        ] {
            let mut app = app();
            for c in typed.chars() {
                app.on_key(key(KeyCode::Char(c)));
            }
            app.on_key(key(KeyCode::Backspace));
            assert_eq!(app.composed(), left, "{typed:?}");
        }
    }

    #[test]
    fn delete_deletes_what_is_seen_as_one_character() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = app();
        for c in "a\u{301}b".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Home));
        app.on_key(key(KeyCode::Delete));
        assert_eq!(app.composed(), "b");
    }

    #[test]
    fn backspace_in_a_written_answer_deletes_what_is_seen_as_one_character() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = app();
        app.apply(&prompt(Some("rm -rf build")));
        app.on_key(key(KeyCode::Tab));
        for c in "no e\u{301}".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.ask_draft(), "no ");
    }

    #[test]
    fn tab_writes_an_answer_that_reaches_the_backend_as_the_answer() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = app();
        app.type_into_composer(Input {
            key: Key::Char('d'),
            ..Default::default()
        });
        app.apply(&prompt(Some("rm -rf build")));

        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.ask_focus(), AskFocus::Writing);
        for c in "keep buildx".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.ask_draft(), "keep build");
        app.on_key(key(KeyCode::Enter));

        assert_eq!(
            app.take_produced(),
            [Event::PermissionResponse {
                id: "t1".into(),
                decision: PermissionDecision::Deny,
                message: Some("keep build".to_owned()),
            }],
            "the words went somewhere other than the answer"
        );
        assert_eq!(app.session().user_messages(), 0, "the answer became a turn");
        assert_eq!(app.composed(), "d", "the draft in the composer was touched");
        let entry = app
            .entries()
            .last()
            .expect("the answer is in the transcript");
        assert_eq!(entry.head, "denied");
        assert!(entry.body.ends_with("keep build"), "{entry:?}");
    }

    #[test]
    fn an_empty_written_answer_sends_nothing_and_esc_goes_back_to_the_options() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = app();
        app.apply(&prompt(Some("rm -rf build")));

        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Enter));
        assert!(app.take_produced().is_empty());

        app.on_key(key(KeyCode::Char('n')));
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.ask_focus(), AskFocus::Choosing);
        // Written in the box, not on the options: `n` selected nothing.
        assert_eq!(app.ask_selected(), Answer::Once);
        assert!(app.asking().is_some());
    }

    #[test]
    fn a_question_left_open_by_a_turn_that_ended_is_not_waited_on() {
        let mut app = sent(app().attached(), "go");
        app.apply(&prompt(Some("rm -rf build")));
        app.apply(&Event::TurnEnded);
        let mut app = sent(app, "again");
        app.take_produced();

        assert!(app.asking().is_none(), "the question is still up");
        assert_ne!(
            app.activity().map(|a| a.doing).as_deref(),
            Some("waiting on you")
        );
    }

    #[test]
    fn a_question_the_backend_withdrew_is_taken_down() {
        let mut app = sent(app().attached(), "go");
        app.apply(&prompt(Some("rm -rf build")));

        app.apply(&Event::PermissionWithdrawn { id: "t1".into() });

        assert!(app.asking().is_none());
        assert_ne!(
            app.activity().map(|a| a.doing).as_deref(),
            Some("waiting on you")
        );
    }

    #[test]
    fn esc_puts_the_question_off_and_leaves_it_waiting_in_the_transcript() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = sent(app().attached(), "go");
        app.take_produced();
        app.apply(&prompt(Some("rm -rf build")));

        app.on_key(key(KeyCode::Esc));

        assert_eq!(app.ask_focus(), AskFocus::Deferred);
        assert!(app.asking().is_some(), "putting it off answered it");
        assert!(app.take_produced().is_empty());
        assert_eq!(
            app.activity().map(|a| a.doing).as_deref(),
            Some("waiting on you"),
            "the session stopped saying it is waiting"
        );

        // The keyboard is the shell's again: a draft can be typed …
        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(app.composed(), "x");
        // … but not sent into a turn that is waiting on the answer.
        app.on_key(key(KeyCode::Enter));
        assert!(
            app.take_produced().is_empty(),
            "a prompt went out past the question"
        );
        assert_eq!(app.composed(), "x");
        let hint = app.hint().unwrap_or_default();
        assert!(hint.contains("waiting"), "{hint}");

        // Esc takes the operator back to it, on the option they had.
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.ask_focus(), AskFocus::Choosing);
        app.on_key(key(KeyCode::Enter));
        assert!(app.asking().is_none());
    }

    #[test]
    fn a_key_meant_for_a_question_out_of_view_brings_it_into_view_first() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = app();
        app.measured(100, 20);
        app.scroll_up(30);
        app.apply(&prompt(Some("rm -rf build")));
        assert!(!app.follows_tail(), "the question yanked the view");

        // Scrolling still reads the work that led to the question.
        app.on_key(key(KeyCode::PageUp));
        assert_eq!(app.scroll(), 30);

        app.on_key(key(KeyCode::Enter));
        assert!(app.follows_tail());
        assert!(
            app.take_produced().is_empty(),
            "a question the operator could not see was answered"
        );
        app.on_key(key(KeyCode::Enter));
        assert!(app.asking().is_none());
    }

    /// `prompt`'s call as call `id`, made by `agent`.
    fn asked_by(id: &str, agent: Option<&str>) -> Event {
        Event::PermissionRequest {
            id: id.into(),
            tool: "Bash".to_owned(),
            input: r#"{"command":"rm -rf build"}"#.to_owned(),
            target: Some("rm -rf build".to_owned()),
            agent: agent.map(AgentId::new),
        }
    }

    /// A rule is about the tool and what it acts on: the one kept from a
    /// sub-agent's question is the one the session's would have kept, and it
    /// answers the session's next question too.
    #[test]
    fn a_standing_answer_to_a_sub_agents_question_is_the_rule_the_sessions_would_be() {
        use ratatui::crossterm::event::KeyCode;

        let from_an_agent = |answer: char| {
            let mut app = app();
            app.apply(&Event::AgentSpawn {
                id: AgentId::new("toolu_a"),
                parent: None,
                kind: None,
                label: "deep-reasoner: Review fetch.py".to_owned(),
            });
            app.apply(&asked_by("t1", Some("toolu_a")));
            assert_eq!(
                app.asking().map(|ask| app.asker(ask)),
                Some("deep".to_owned())
            );
            app.on_key(key(KeyCode::Char(answer)));
            app.on_key(key(KeyCode::Enter));
            app
        };

        let mut by_target = from_an_agent('3');
        assert_eq!(
            by_target.take_rules(),
            [Rule::targeted("Bash", "rm -rf build")]
        );
        by_target.apply(&asked_by("t2", None));
        by_target.settle_rules();
        assert!(by_target.asking().is_none(), "the session's own question");

        assert_eq!(from_an_agent('2').take_rules(), [Rule::tool("Bash")]);
    }

    #[test]
    fn a_sub_agents_refusal_names_the_agent_and_one_it_never_asked_does_not() {
        let mut app = app();
        app.apply(&Event::AgentSpawn {
            id: AgentId::new("toolu_a"),
            parent: None,
            kind: None,
            label: "deep-reasoner: Review fetch.py".to_owned(),
        });
        app.apply(&asked_by("t1", Some("toolu_a")));
        let deny = |id: &str| Event::PermissionResponse {
            id: id.into(),
            decision: PermissionDecision::Deny,
            message: None,
        };
        app.apply(&deny("t1"));
        app.apply(&asked_by("t2", None));
        app.apply(&deny("t2"));

        let refused: Vec<(&str, Option<&str>)> = app
            .entries()
            .iter()
            .filter(|entry| entry.head == "denied")
            .map(|entry| (entry.meta.as_str(), entry.agent.as_deref()))
            .collect();
        assert_eq!(
            refused,
            [
                ("Bash · rm -rf build", Some("deep")),
                ("Bash · rm -rf build", None),
            ]
        );
    }

    #[test]
    fn a_standing_rule_answers_a_prompt_without_showing_it() {
        let mut app = app().with_rules([Rule::tool("Read")].into_iter().collect());
        app.apply(&Event::PermissionRequest {
            id: "t1".into(),
            tool: "Read".to_owned(),
            input: r#"{"file_path":"/repo/a.rs"}"#.to_owned(),
            target: Some("/repo/a.rs".to_owned()),
            agent: None,
        });

        app.settle_rules();

        assert!(app.asking().is_none());
        assert_eq!(
            app.take_produced(),
            [Event::PermissionResponse {
                id: "t1".into(),
                decision: PermissionDecision::AllowByRule,
                message: None,
            }],
            "the call was let through without the backend being told"
        );
    }

    #[test]
    fn a_prefix_rule_does_not_answer_a_command_chained_after_the_one_it_allows() {
        let rule = Rule::parse("Bash(cargo *)").expect("the rule is valid");
        let mut app = app().with_rules([rule].into_iter().collect());
        app.apply(&Event::PermissionRequest {
            id: "t1".into(),
            tool: "Bash".to_owned(),
            input: r#"{"command":"cargo test && rm -rf ~"}"#.to_owned(),
            target: Some("cargo test && rm -rf ~".to_owned()),
            agent: None,
        });

        app.settle_rules();

        assert_eq!(
            app.asking().map(|ask| ask.target.as_deref()),
            Some(Some("cargo test && rm -rf ~")),
            "the chained command was let through by a rule about cargo"
        );
        assert!(app.take_produced().is_empty());
    }

    fn under_a_profile(models: &[&str]) -> App {
        app().with_profile(SelectedProfile {
            name: "max".to_owned(),
            backend: Backend::Claude,
            models: models.iter().map(|m| (*m).to_owned()).collect(),
        })
    }

    fn back_tab() -> ratatui::crossterm::event::KeyEvent {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)
    }

    #[test]
    fn shift_tab_cycles_the_mode_and_hands_the_change_out_to_be_kept() {
        let mut app = app();
        app.apply(&Event::ModeSelected { mode: Mode::Ask });

        app.on_key(back_tab());

        assert_eq!(app.session().mode(), Some(Mode::Auto));
        assert_eq!(
            app.take_produced(),
            [Event::ModeSelected { mode: Mode::Auto }]
        );

        app.on_key(back_tab());
        assert_eq!(app.session().mode(), Some(Mode::Plan));
    }

    #[test]
    fn a_session_no_backend_has_reported_a_mode_for_cycles_from_asking() {
        let mut app = app();
        assert_eq!(app.session().mode(), None);

        app.on_key(back_tab());

        assert_eq!(
            app.session().mode(),
            Some(Mode::Auto),
            "cycling from nowhere landed somewhere other than one step past `ask`"
        );
    }

    #[test]
    fn f4_offers_the_models_the_profile_names_and_picking_one_asks_for_it() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = under_a_profile(&["opus", "sonnet", "haiku"]);

        app.on_key(key(KeyCode::F(4)));
        let picker = app.picking().expect("the model list is on screen");
        assert_eq!(picker.options, ["opus", "sonnet", "haiku"]);
        assert_eq!(picker.at, 0);

        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));

        assert!(app.picking().is_none(), "the list stayed on screen");
        assert_eq!(
            app.take_produced(),
            [Event::ModelSelected {
                model: "sonnet".to_owned()
            }]
        );
        assert_eq!(
            app.session().model(),
            Some("sonnet"),
            "the menu row would still name the model the session moved off"
        );
    }

    #[test]
    fn a_model_list_the_operator_left_asks_for_nothing() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = under_a_profile(&["opus", "sonnet"]);
        app.on_key(key(KeyCode::F(4)));

        app.on_key(key(KeyCode::Esc));

        assert!(app.picking().is_none());
        assert!(app.take_produced().is_empty());
    }

    #[test]
    fn f4_under_a_profile_that_names_no_model_says_so_rather_than_opening_an_empty_list() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = under_a_profile(&[]);

        app.on_key(key(KeyCode::F(4)));

        assert!(app.picking().is_none());
        let hint = app.hint().unwrap_or_default();
        assert!(hint.contains("models"), "{hint}");
    }

    #[test]
    fn a_prompt_keeps_the_keyboard_from_the_model_list() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = under_a_profile(&["opus"]);
        app.apply(&prompt(Some("rm -rf build")));

        app.on_key(key(KeyCode::F(4)));

        assert!(
            app.picking().is_none(),
            "a list opened over the question the turn is waiting on"
        );
        assert!(app.asking().is_some());
    }

    #[test]
    fn four_fifths_of_a_budget_is_warned_about_once_and_says_the_stop_can_overshoot() {
        let mut app = app().with_budget(1.0);

        app.apply(&priced(0.79));
        app.settle_budget();
        assert!(
            !app.entries().iter().any(|entry| entry.head == "budget"),
            "a session under four fifths of its budget was warned"
        );

        app.apply(&priced(0.02));
        app.settle_budget();
        app.settle_budget();

        let warnings: Vec<&Entry> = app
            .entries()
            .iter()
            .filter(|entry| entry.head == "budget")
            .collect();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].meta, "$0.81 of $1.00");
        assert!(
            warnings[0].body.contains("between turns"),
            "the warning let the budget read as a hard ceiling: {}",
            warnings[0].body
        );
    }

    #[test]
    fn a_floor_past_four_fifths_of_the_budget_is_warned_about_as_a_floor() {
        let mut app = app().with_budget(1.0);
        let mut unreported = priced(0.0);
        if let Event::Usage(usage) = &mut unreported {
            usage.cost_usd = None;
        }

        app.apply(&unreported);
        app.settle_budget();
        assert!(
            app.entries().iter().all(|entry| entry.head != "budget"),
            "a cost nobody reported was warned about"
        );

        app.apply(&priced(0.85));
        app.settle_budget();
        let warning = app
            .entries()
            .iter()
            .find(|entry| entry.head == "budget")
            .expect("a floor past the line is warned about");
        assert_eq!(warning.meta, "≥$0.85 of $1.00");
    }

    #[test]
    fn a_session_with_no_budget_is_never_warned_about_one() {
        let mut app = app();
        app.apply(&priced(99.0));
        app.settle_budget();

        assert!(app.entries().iter().all(|entry| entry.head != "budget"));
    }

    #[test]
    fn a_carriage_return_writes_over_the_line_as_a_terminal_does() {
        assert_eq!(terminal_line("abcdef\rXY"), "XYcdef");
        assert_eq!(terminal_line("[#####     ] 50%\r"), "[#####     ] 50%");
        assert_eq!(terminal_line("10%\r20%\r100%"), "100%");
        assert_eq!(terminal_line("plain"), "plain");
    }

    #[test]
    fn a_commands_colours_and_title_are_taken_out_whole_and_not_drawn_as_text() {
        let printed = Printed::of("\x1b[31mred\x1b[0m \x1b]0;TITLE\x07 x\ty\n");
        assert_eq!(printed.tail, ["red  x  y"]);

        let linked = Printed::of("\x1b]8;;https://example.test\x1b\\link\x1b]8;;\x1b\\ done\n");
        assert_eq!(linked.tail, ["link done"]);
    }

    #[test]
    fn an_empty_composer_sends_nothing() {
        let mut app = app();
        app.submit();
        assert!(app.entries().is_empty());
    }

    #[test]
    fn paging_up_unpins_the_tail_and_paging_back_down_repins_it() {
        let mut app = app();
        app.measured(100, 20);
        assert!(app.follows_tail());
        assert_eq!(app.scroll(), 80);

        app.scroll_up(20);
        assert!(!app.follows_tail());
        assert_eq!(app.scroll(), 60);

        app.scroll_down(20);
        assert!(app.follows_tail());
        assert_eq!(app.scroll(), 80);

        app.scroll_to_head();
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn a_transcript_shorter_than_the_pane_never_scrolls() {
        let mut app = app();
        app.measured(4, 20);
        assert_eq!(app.scroll(), 0);
        app.scroll_down(20);
        assert_eq!(app.scroll(), 0);
    }

    /// A five-hour window a third gone with an hour and a half to run, and a
    /// seven-day window a fifth gone with four days left.
    fn windows(now: u64) -> UsageWindows {
        UsageWindows {
            five_hour: Some(UsageWindow {
                utilization: 0.33,
                resets_at: Some(now + 5_400),
            }),
            seven_day: Some(UsageWindow {
                utilization: 0.23,
                resets_at: Some(now + 367_200),
            }),
            using_overage: false,
        }
    }

    #[test]
    fn cost_and_usage_reads_out_both_windows_and_when_each_comes_back() {
        const NOW: u64 = 1_789_000_000;

        let mut app = app();
        app.apply(&Event::UsageWindows(windows(NOW)));

        assert_eq!(
            app.cost_hint(NOW),
            "Cost & usage — 5h window 33%, resets in 1h 30m · 7d window 23%, resets in 4d 6h"
        );
    }

    #[test]
    fn cost_and_usage_on_a_session_with_no_windows_says_what_it_does_not_do_yet() {
        let mut app = app();
        assert!(
            app.cost_hint(0)
                .contains("the usage breakdown is not implemented yet")
        );

        // Choosing it is what puts the line under the transcript.
        app.apply(&Event::UsageWindows(windows(1_789_000_000)));
        app.perform(crate::menu::Action::Usage);
        assert!(
            app.hint()
                .is_some_and(|hint| hint.contains("5h window 33%")),
            "{:?}",
            app.hint()
        );
    }

    #[test]
    fn a_window_that_has_since_come_back_is_not_counted_down_from_nothing() {
        const NOW: u64 = 1_789_000_000;

        let mut app = app();
        app.apply(&Event::UsageWindows(UsageWindows {
            five_hour: Some(UsageWindow {
                utilization: 0.9,
                resets_at: Some(NOW - 1),
            }),
            seven_day: Some(UsageWindow {
                utilization: 0.4,
                resets_at: None,
            }),
            using_overage: true,
        }));

        assert_eq!(
            app.cost_hint(NOW),
            "Cost & usage — 5h window 90%, already reset · 7d window 40%, no reset time \
             reported · spending beyond the plan"
        );
    }

    #[test]
    fn a_window_reads_as_the_share_the_backend_reported_and_no_other() {
        assert_eq!(percent(0.0), 0);
        assert_eq!(percent(0.334), 33);
        assert_eq!(percent(0.336), 34);
        // Over the window is a thing the operator has to be able to see.
        assert_eq!(percent(1.04), 104);
    }

    #[test]
    fn a_moment_an_event_named_is_read_in_the_clock_the_loop_handed_over() {
        let app = App::new(Repo::default())
            .with_clock(Clock::fixed(3_600).expect("an hour east is an offset"));
        // Half past eleven at night on the first day of 1970, an hour east of
        // UTC, is half past midnight on the second.
        let moment = app
            .moment(23 * 3_600 + 30 * 60)
            .and_then(|stamp| stamp.moment())
            .expect("a fixed clock always names a zone");

        assert_eq!(moment.day(), 1);
        assert_eq!(moment.time().to_string(), "00:30");
    }

    #[test]
    fn a_moment_read_without_a_clock_has_no_time_of_day_at_all() {
        let app = App::new(Repo::default());
        assert_eq!(
            app.moment(23 * 3_600).and_then(|stamp| stamp.moment()),
            None
        );
    }

    #[test]
    fn a_time_past_what_the_clock_holds_is_no_moment() {
        let app = App::new(Repo::default());
        assert_eq!(app.moment(u64::MAX), None);
    }

    #[test]
    fn time_left_reads_in_the_two_units_that_say_anything() {
        assert_eq!(left(0), "0m");
        assert_eq!(left(59), "0m");
        assert_eq!(left(2 * 3_600 + 14 * 60), "2h 14m");
        assert_eq!(left(4 * 86_400 + 6 * 3_600 + 30 * 60), "4d 6h");
    }

    #[test]
    fn bytes_read_short() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(4_096), "4.0 kB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MB");
    }

    fn ctrl_c() -> ratatui::crossterm::event::KeyEvent {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
    }

    #[test]
    fn esc_during_a_running_turn_asks_the_backend_to_stop_it() {
        let mut app = sent(app().attached(), "go");
        app.take_produced();

        app.on_key(key(ratatui::crossterm::event::KeyCode::Esc));

        assert!(app.take_interrupt(), "nothing asked the turn to stop");
        assert!(!app.take_interrupt(), "the stop is asked for once");
        assert!(!app.should_quit());
        assert!(
            !app.escaped,
            "the next digit would press an F-key rather than type"
        );
    }

    #[test]
    fn esc_with_no_turn_running_is_still_the_way_to_the_f_keys() {
        let mut app = app().attached();

        app.on_key(key(ratatui::crossterm::event::KeyCode::Esc));

        assert!(!app.take_interrupt());
        assert!(app.escaped);
    }

    #[test]
    fn ctrl_c_stops_a_running_turn_first_and_quits_on_the_second_press() {
        let mut app = sent(app().attached(), "go");
        app.take_produced();

        app.on_key(ctrl_c());
        assert!(app.take_interrupt());
        assert!(!app.should_quit(), "the first Ctrl+C ended the session");

        app.on_key(ctrl_c());
        assert!(app.should_quit());
    }

    #[test]
    fn ctrl_c_with_no_turn_running_quits_at_once() {
        let mut app = app().attached();

        app.on_key(ctrl_c());

        assert!(app.should_quit());
        assert!(!app.take_interrupt());
    }

    #[test]
    fn a_turn_that_ended_after_a_stop_lets_the_next_turn_be_stopped_too() {
        let mut app = sent(app().attached(), "go");
        app.on_key(ctrl_c());
        app.take_interrupt();
        app.apply(&Event::TurnEnded);
        let mut app = sent(app, "again");
        app.take_produced();

        app.on_key(ctrl_c());

        assert!(app.take_interrupt());
        assert!(!app.should_quit());
    }

    #[test]
    fn a_stop_the_backend_would_not_take_is_said_in_the_transcript() {
        let mut app = sent(app().attached(), "go");

        app.not_stopped("the pipe is closed");

        let last = app.entries().last().expect("an entry");
        assert_eq!(last.kind, EntryKind::Failure);
        assert!(last.body.contains("the pipe is closed"), "{}", last.body);
    }

    fn sent(mut app: App, prompt: &str) -> App {
        for c in prompt.chars() {
            app.type_into_composer(Input {
                key: Key::Char(c),
                ..Default::default()
            });
        }
        app.submit();
        app
    }

    fn start(id: &str, name: &str, input: &str, summary: Option<&str>) -> Event {
        Event::ToolCallStart {
            id: id.into(),
            name: name.to_owned(),
            input: input.to_owned(),
            summary: summary.map(str::to_owned),
            agent: None,
        }
    }

    #[test]
    fn a_tool_is_named_the_way_a_person_would_name_it() {
        assert_eq!(tool_label("Bash"), "Bash");
        assert_eq!(
            tool_label("mcp__claude_ai_Notion__notion-search"),
            "Notion·search"
        );
        assert_eq!(
            tool_label("mcp__code-review-graph__query_graph_tool"),
            "code-review-graph·query_graph_tool"
        );
        assert_eq!(
            tool_label("mcp__github__create_issue"),
            "github·create_issue"
        );
    }

    #[test]
    fn a_tool_line_reads_as_what_the_call_does_rather_than_its_arguments() {
        let mut app = app();
        app.apply(&start(
            "t1",
            "mcp__claude_ai_Notion__notion-search",
            r#"{"query":"Niobe"}"#,
            Some("query: Niobe"),
        ));
        app.apply(&start("t2", "Glob", r#"{"pattern":"*.rs"}"#, None));

        assert_eq!(app.entries()[0].head, "Notion·search");
        assert_eq!(app.entries()[0].calls[0].what, "query: Niobe");
        assert_eq!(
            app.entries()[1].calls[0].what,
            r#"{"pattern":"*.rs"}"#,
            "with no summary the arguments are all there is to show"
        );
    }

    #[test]
    fn a_prompt_sent_from_here_shows_the_session_working_until_the_turn_ends() {
        let mut app = sent(app().attached(), "fix it");
        let t0 = Instant::now();
        app.tick(t0, None);
        let working = app.activity().expect("a prompt was just sent");
        assert_eq!(working.doing, "thinking");
        assert_eq!(working.elapsed, Duration::ZERO);

        app.tick(t0 + Duration::from_secs(12), None);
        app.apply(&start(
            "t1",
            "Bash",
            r#"{"command":"cargo test"}"#,
            Some("cargo test"),
        ));
        let working = app.activity().expect("the turn is still running");
        assert_eq!(working.doing, "running Bash  cargo test");
        assert_eq!(working.elapsed, Duration::from_secs(12));

        app.apply(&Event::AssistantDelta {
            text: "The test".to_owned(),
        });
        assert_eq!(
            app.activity().map(|a| a.doing).as_deref(),
            Some("running Bash  cargo test"),
            "a call still running is what the session is doing"
        );

        app.apply(&Event::TurnEnded);
        app.tick(t0 + Duration::from_secs(13), None);
        assert_eq!(app.activity(), None);
    }

    #[test]
    fn a_turn_waiting_on_a_prompt_says_it_is_waiting_on_the_operator() {
        let mut app = sent(app().attached(), "go");
        app.apply(&Event::PermissionRequest {
            id: "t1".into(),
            tool: "Bash".to_owned(),
            input: "{}".to_owned(),
            target: None,
            agent: None,
        });
        assert_eq!(
            app.activity().map(|a| a.doing).as_deref(),
            Some("waiting on you")
        );
    }

    #[test]
    fn a_turn_read_back_from_a_record_is_not_shown_as_working() {
        let mut app = app().attached();
        app.apply(&Event::UserMessage {
            text: "from an earlier run".to_owned(),
        });
        app.tick(Instant::now(), None);
        assert_eq!(app.activity(), None);
    }

    #[test]
    fn a_prompt_that_never_reached_the_backend_is_not_shown_as_working() {
        let mut app = sent(app().attached(), "go");
        app.not_sent("the pipe is closed");
        assert_eq!(app.activity(), None);
    }

    fn asked(target: Option<&str>) -> App {
        let mut app = app();
        for id in ["t1", "t2"] {
            app.apply(&Event::PermissionRequest {
                id: id.into(),
                tool: "Bash".to_owned(),
                input: r#"{"command":"ls"}"#.to_owned(),
                target: target.map(str::to_owned),
                agent: None,
            });
        }
        app
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn arrows_and_vi_keys_move_the_selection_and_wrap_at_either_end() {
        let mut app = asked(Some("ls"));
        assert_eq!(app.ask_selected(), Answer::Once);

        let mut walked = Vec::new();
        for code in [
            KeyCode::Down,
            KeyCode::Char('j'),
            KeyCode::Down,
            KeyCode::Down,
        ] {
            press(&mut app, code);
            walked.push(app.ask_selected());
        }
        assert_eq!(
            walked,
            [
                Answer::AlwaysTool,
                Answer::AlwaysTarget,
                Answer::No,
                Answer::Once
            ]
        );

        press(&mut app, KeyCode::Up);
        assert_eq!(app.ask_selected(), Answer::No);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.ask_selected(), Answer::AlwaysTarget);
    }

    #[test]
    fn a_prompt_with_no_target_has_no_target_option_to_select() {
        let mut app = asked(None);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.ask_selected(), Answer::No);
    }

    #[test]
    fn enter_confirms_the_selection_and_the_next_prompt_starts_on_the_first() {
        let mut app = asked(Some("ls"));
        press(&mut app, KeyCode::Char('4'));
        press(&mut app, KeyCode::Enter);

        assert_eq!(
            app.take_produced(),
            [Event::PermissionResponse {
                id: "t1".into(),
                decision: PermissionDecision::Deny,
                message: None,
            }]
        );
        assert_eq!(app.asking().map(|ask| ask.id.as_str()), Some("t2"));
        assert_eq!(app.ask_selected(), Answer::Once);
    }

    fn wheel(app: &mut App, kind: MouseEventKind) {
        app.on_mouse(MouseEvent {
            kind,
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
    }

    #[test]
    fn the_wheel_scrolls_the_transcript_and_back_down_to_the_tail() {
        let mut app = app();
        app.measured(100, 20);
        assert_eq!(app.scroll(), 80);

        wheel(&mut app, MouseEventKind::ScrollUp);
        assert_eq!(app.scroll(), 77);
        assert!(!app.follows_tail());

        wheel(&mut app, MouseEventKind::ScrollDown);
        wheel(&mut app, MouseEventKind::ScrollDown);
        assert_eq!(app.scroll(), 80);
        assert!(app.follows_tail(), "the bottom sticks to the tail again");

        wheel(&mut app, MouseEventKind::Moved);
        assert_eq!(app.scroll(), 80, "only the wheel scrolls");
    }

    /// A clock reading, as the event loop hands one in.
    fn at(seconds: u64, hour: u8, minute: u8) -> Stamp {
        Stamp::new(
            std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(seconds),
            LocalMoment::at(0, hour, minute),
        )
    }

    fn tokens(input: u64) -> Event {
        Event::Usage(niobe_core::Usage {
            input,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 0,
            model: "opus-5".to_owned(),
            cost_usd: None,
            cost_basis: None,
            settles_model: false,
            fast: false,
        })
    }

    fn rules(app: &App) -> Vec<(usize, TurnRule)> {
        app.entries()
            .iter()
            .enumerate()
            .filter_map(|(at, entry)| {
                let EntryKind::Turn(rule) = entry.kind else {
                    return None;
                };
                Some((at, rule))
            })
            .collect()
    }

    #[test]
    fn a_turn_is_ruled_off_where_it_ended_with_what_it_spent_and_took() {
        let mut app = app();
        app.apply_at(
            &Event::UserMessage {
                text: "go on".to_owned(),
            },
            at(50_662, 14, 4),
        );
        app.apply_at(&tokens(4_100), at(50_680, 14, 4));
        assert!(rules(&app).is_empty(), "a running turn was ruled off");

        app.apply_at(&Event::TurnEnded, at(50_700, 14, 5));
        let [(place, rule)] = rules(&app)[..] else {
            panic!("one turn ended: {:?}", app.entries());
        };
        assert_eq!(
            place + 1,
            app.entries().len(),
            "the rule is not under the turn"
        );
        assert_eq!(
            rule,
            TurnRule {
                number: 1,
                ended: LocalTime::new(14, 5),
                tokens: Some(4_100),
                five_hour_points: None,
                took: Some(Duration::from_secs(38)),
                agents: 0,
                calls: 0,
                cut: false,
            }
        );
    }

    #[test]
    fn a_turns_rule_counts_the_agents_it_spawned_and_the_calls_it_made() {
        let mut app = app();
        let start = |id: &str, agent: Option<&str>| Event::ToolCallStart {
            id: id.into(),
            name: "Read".to_owned(),
            input: String::new(),
            summary: None,
            agent: agent.map(AgentId::new),
        };
        app.apply(&Event::UserMessage {
            text: "first".to_owned(),
        });
        app.apply(&start("t0", None));
        app.apply(&Event::TurnEnded);

        app.apply(&Event::UserMessage {
            text: "second".to_owned(),
        });
        app.apply(&Event::AgentSpawn {
            id: AgentId::new("toolu_a"),
            parent: None,
            kind: Some("Explore".to_owned()),
            label: "Find the loop".to_owned(),
        });
        app.apply(&start("t1", Some("toolu_a")));
        app.apply(&start("t2", Some("toolu_a")));
        app.apply(&start("t3", None));
        app.apply(&Event::TurnEnded);

        let counted: Vec<(u64, u64)> = rules(&app)
            .into_iter()
            .map(|(_, rule)| (rule.agents, rule.calls))
            .collect();
        assert_eq!(counted, [(0, 1), (1, 3)]);
    }

    #[test]
    fn a_turn_an_error_ended_is_ruled_off_under_the_error_with_its_numbers() {
        let mut app = app();
        app.apply_at(
            &Event::UserMessage {
                text: "go on".to_owned(),
            },
            at(50_000, 13, 53),
        );
        app.apply_at(&tokens(900), at(50_010, 13, 53));
        app.apply_at(
            &Event::Error {
                message: "the CLI exited".to_owned(),
                fatal: true,
            },
            at(50_012, 13, 53),
        );

        let [(place, rule)] = rules(&app)[..] else {
            panic!("the failed turn was not ruled off: {:?}", app.entries());
        };
        assert_eq!(
            app.entries().get(place - 1).map(|entry| entry.kind),
            Some(EntryKind::Failure)
        );
        assert_eq!(
            (rule.tokens, rule.took),
            (Some(900), Some(Duration::from_secs(12)))
        );
    }

    /// A log that kept no times has turns that ended at no time and took no
    /// time anyone measured; what they spent is still in the log.
    #[test]
    fn a_turn_from_a_log_with_no_times_says_what_it_spent_and_nothing_else() {
        let mut app = app();
        app.extend(&[
            Event::UserMessage {
                text: "go on".to_owned(),
            },
            tokens(300),
            Event::TurnEnded,
        ]);
        let [(_, rule)] = rules(&app)[..] else {
            panic!("the turn was not ruled off");
        };
        assert_eq!(
            (rule.ended, rule.took, rule.tokens),
            (None, None, Some(300))
        );
    }

    /// A turn cut short before the backend reported any usage spent what
    /// nobody measured, so its rule carries no token figure rather than a
    /// zero.
    #[test]
    fn a_turn_ended_before_any_usage_was_reported_is_ruled_off_with_no_tokens() {
        let mut app = app();
        app.extend(&[
            Event::UserMessage {
                text: "go on".to_owned(),
            },
            Event::Error {
                message: "the CLI exited".to_owned(),
                fatal: true,
            },
        ]);
        let [(_, rule)] = rules(&app)[..] else {
            panic!("the turn was not ruled off");
        };
        assert_eq!(rule.tokens, None);
    }

    #[test]
    fn an_event_is_stamped_with_the_clock_the_tick_that_took_it_read() {
        let mut app = app();
        app.tick(Instant::now(), Some(at(50_700, 14, 5)));
        app.apply(&Event::UserMessage {
            text: "go on".to_owned(),
        });

        assert_eq!(
            app.entries().last().and_then(|entry| entry.at),
            Some(at(50_700, 14, 5)),
            "a live entry carries no time the shell could draw"
        );
    }

    #[test]
    fn a_fold_with_no_clock_behind_it_leaves_no_times_rather_than_invented_ones() {
        // A JSON Lines log records no times. It is folded in before the event
        // loop has ticked, and what it produces has to say so.
        let mut app = app();
        app.extend(&[
            Event::UserMessage {
                text: "go on".to_owned(),
            },
            Event::AssistantMessage {
                text: "done".to_owned(),
                agent: None,
            },
        ]);

        assert!(!app.entries().is_empty());
        assert!(
            app.entries().iter().all(|entry| entry.at.is_none()),
            "a log that kept no times was stamped with the moment it was parsed"
        );
    }

    #[test]
    fn a_recorded_event_keeps_the_moment_it_was_recorded_at() {
        let mut app = app();
        app.tick(Instant::now(), Some(at(50_700, 14, 5)));
        app.apply_at(
            &Event::UserMessage {
                text: "yesterday".to_owned(),
            },
            at(1_000, 9, 30),
        );

        assert_eq!(
            app.entries().last().and_then(|entry| entry.at),
            Some(at(1_000, 9, 30)),
            "a session read back off disk was stamped with the time it was read"
        );
        assert_eq!(
            app.stamp(),
            Some(at(50_700, 14, 5)),
            "the live clock did not come back after the recorded fold"
        );
    }

    #[test]
    fn a_whole_recorded_stream_is_folded_at_the_times_it_was_recorded_at() {
        let first = Event::UserMessage {
            text: "one".to_owned(),
        };
        let second = Event::UserMessage {
            text: "two".to_owned(),
        };
        let mut app = app();
        app.extend_at([(&first, at(1_000, 9, 30)), (&second, at(1_060, 9, 31))]);

        let times: Vec<_> = app.entries().iter().map(|entry| entry.at).collect();
        assert_eq!(times, [Some(at(1_000, 9, 30)), Some(at(1_060, 9, 31))]);
        assert!(
            app.stamp().is_none(),
            "a fold left a clock the loop never read"
        );
    }

    #[test]
    fn a_running_sub_agent_carries_the_moment_it_was_spawned() {
        let mut app = app();
        app.tick(Instant::now(), Some(at(50_700, 14, 5)));
        app.apply(&Event::AgentSpawn {
            id: AgentId::new("a1"),
            parent: None,
            kind: None,
            label: "review the diff".to_owned(),
        });

        let spawned = app
            .agent_spawned_at(&AgentId::new("a1"))
            .expect("the shell had a clock when it took the spawn");
        // Which is what a timer beside it is measured from.
        assert_eq!(
            at(50_802, 14, 6).since(spawned),
            Some(Duration::from_secs(102))
        );
        assert_eq!(
            app.agent_label(&AgentId::new("a1")).as_deref(),
            Some("review the diff")
        );
    }

    #[test]
    fn a_sub_agents_report_replaces_only_what_it_speaks_to() {
        let mut app = app();
        let id = AgentId::new("a1");
        app.apply(&Event::AgentSpawn {
            id: id.clone(),
            parent: None,
            kind: None,
            label: "review the diff".to_owned(),
        });
        app.apply(&Event::AgentProgress {
            id: id.clone(),
            model: Some("claude-opus-5".to_owned()),
            context_tokens: None,
            latest: None,
        });
        app.apply(&Event::AgentProgress {
            id: id.clone(),
            model: None,
            context_tokens: Some(12_938),
            latest: Some("Reading catalog/cache.py".to_owned()),
        });
        app.apply(&Event::AgentProgress {
            id,
            model: None,
            context_tokens: Some(13_009),
            latest: None,
        });

        let [agent] = app.agents() else {
            panic!("one agent was spawned: {:?}", app.agents());
        };
        assert_eq!(agent.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(agent.context_tokens, Some(13_009));
        assert_eq!(agent.latest.as_deref(), Some("Reading catalog/cache.py"));
    }

    #[test]
    fn a_report_about_an_agent_never_spawned_adds_no_row() {
        let mut app = app();
        app.apply(&Event::AgentProgress {
            id: AgentId::new("a9"),
            model: Some("claude-opus-5".to_owned()),
            context_tokens: Some(1),
            latest: None,
        });

        assert!(app.agents().is_empty());
    }

    /// An edit the backend reported the lines of, as a bridge produces it:
    /// the call's end, and the change directly after it.
    fn edited(app: &mut App, id: &str, hunks: Vec<Hunk>) {
        app.apply(&start(
            id,
            "Edit",
            r#"{"file_path":"/repo/a.rs"}"#,
            Some("a.rs"),
        ));
        app.apply(&Event::ToolCallEnd {
            id: id.into(),
            name: "Edit".to_owned(),
            input: r#"{"file_path":"/repo/a.rs"}"#.to_owned(),
            output: "updated".to_owned(),
            bytes: 7,
            outcome: ToolOutcome::Ok,
            summary: Some("a.rs".to_owned()),
            exit_code: None,
            error: None,
        });
        app.apply(&Event::FileChange {
            path: "a.rs".to_owned(),
            added: Some(1),
            removed: Some(1),
            hunks,
        });
    }

    fn one_hunk() -> Vec<Hunk> {
        vec![
            Hunk::checked(
                3,
                1,
                3,
                1,
                vec![
                    niobe_core::diff::Line::Removed("old".to_owned()),
                    niobe_core::diff::Line::Added("new".to_owned()),
                ],
            )
            .expect("one line each side"),
        ]
    }

    fn change_of(app: &App) -> Option<&Change> {
        app.entries()
            .iter()
            .rev()
            .find(|entry| entry.kind == EntryKind::Tool)
            .and_then(|entry| entry.calls.last()?.change.as_ref())
    }

    #[test]
    fn a_change_the_backend_reported_the_lines_of_is_drawn_under_the_call_that_made_it() {
        let mut app = app();
        edited(&mut app, "t1", one_hunk());

        assert_eq!(
            change_of(&app).map(Change::hunks),
            Some(one_hunk().as_slice())
        );
        assert_eq!(
            app.entries().len(),
            1,
            "the change became an entry of its own"
        );
    }

    #[test]
    fn a_change_reported_only_as_counts_leaves_the_call_as_its_summary_line() {
        let mut app = app();
        edited(&mut app, "t1", Vec::new());

        assert_eq!(change_of(&app), None);
    }

    /// A change is drawn under the call only when it follows that call's end,
    /// which is the order a backend reports the two in. Anything else is a
    /// change this shell cannot place.
    #[test]
    fn a_change_that_does_not_follow_the_end_of_its_call_is_not_put_under_another_call() {
        let mut app = app();
        app.apply(&start("t1", "Bash", r#"{"command":"ls"}"#, Some("ls")));
        app.apply(&Event::ToolCallEnd {
            id: "t1".into(),
            name: "Bash".to_owned(),
            input: String::new(),
            output: String::new(),
            bytes: 0,
            outcome: ToolOutcome::Ok,
            summary: None,
            exit_code: None,
            error: None,
        });
        app.apply(&Event::AssistantMessage {
            text: "and then".to_owned(),
            agent: None,
        });
        app.apply(&Event::FileChange {
            path: "a.rs".to_owned(),
            added: Some(1),
            removed: Some(1),
            hunks: one_hunk(),
        });

        let calls: Vec<_> = app
            .entries()
            .iter()
            .flat_map(|entry| &entry.calls)
            .collect();
        assert!(!calls.is_empty());
        assert!(
            calls
                .iter()
                .all(|call| call.change.is_none() && call.lines.is_none())
        );
    }

    fn answered(app: &mut App, decision: PermissionDecision) {
        app.apply(&Event::PermissionRequest {
            id: "t1".into(),
            tool: "Edit".to_owned(),
            input: r#"{"file_path":"/repo/a.rs"}"#.to_owned(),
            target: Some("/repo/a.rs".to_owned()),
            agent: None,
        });
        app.apply(&Event::PermissionResponse {
            id: "t1".into(),
            decision,
            message: None,
        });
    }

    #[test]
    fn the_diff_says_whether_the_operator_or_a_standing_rule_let_the_call_through() {
        for (decision, gate) in [
            (PermissionDecision::Allow, Gate::Operator),
            (PermissionDecision::AllowAlways, Gate::OperatorAlways),
            (PermissionDecision::AllowByRule, Gate::Rule),
        ] {
            let mut app = app();
            answered(&mut app, decision);
            edited(&mut app, "t1", one_hunk());

            assert_eq!(
                change_of(&app).and_then(Change::gate),
                Some(gate),
                "{decision:?}"
            );
        }
    }

    #[test]
    fn a_call_sent_from_here_that_nobody_was_asked_about_ran_in_the_mode() {
        let mut app = sent(app().attached(), "go");
        app.apply(&Event::ModeSelected { mode: Mode::Auto });
        edited(&mut app, "t1", one_hunk());

        assert_eq!(
            change_of(&app).and_then(Change::gate),
            Some(Gate::Unasked(Mode::Auto))
        );
    }

    /// A call read back from a record, with no answer beside it, may have
    /// been asked about somewhere this shell was not; saying it was not asked
    /// would be a guess.
    #[test]
    fn a_call_read_back_without_an_answer_makes_no_claim_about_how_it_ran() {
        let mut app = app();
        app.apply(&Event::ModeSelected { mode: Mode::Auto });
        edited(&mut app, "t1", one_hunk());

        assert_eq!(change_of(&app).map(Change::gate), Some(None));
    }

    /// A call's end, with the figures a row reads.
    fn ended(id: &str, name: &str, outcome: ToolOutcome, bytes: u64) -> Event {
        Event::ToolCallEnd {
            id: id.into(),
            name: name.to_owned(),
            input: String::new(),
            output: String::new(),
            bytes,
            outcome,
            summary: None,
            exit_code: None,
            error: None,
        }
    }

    fn called(app: &mut App, id: &str, name: &str) {
        app.apply(&start(id, name, "{}", Some(id)));
        app.apply(&ended(id, name, ToolOutcome::Ok, 10));
    }

    fn heads(app: &App) -> Vec<(String, usize)> {
        app.entries()
            .iter()
            .map(|entry| (entry.head.clone(), entry.calls.len()))
            .collect()
    }

    #[test]
    fn calls_to_the_same_tool_one_after_another_are_one_group() {
        let mut app = app();
        called(&mut app, "t1", "Read");
        called(&mut app, "t2", "Read");
        called(&mut app, "t3", "Read");

        assert_eq!(heads(&app), vec![("Read".to_owned(), 3)]);
        let what: Vec<&str> = app.entries()[0]
            .calls
            .iter()
            .map(|call| call.what.as_str())
            .collect();
        assert_eq!(what, ["t1", "t2", "t3"]);
    }

    #[test]
    fn calls_started_together_are_grouped_and_each_end_finds_its_own_call() {
        let mut app = app();
        for id in ["t1", "t2"] {
            app.apply(&start(id, "Read", "{}", Some(id)));
        }
        app.apply(&ended("t2", "Read", ToolOutcome::Failed, 5));
        assert!(
            app.entries()[0].streaming,
            "one of the two is still running"
        );
        app.apply(&ended("t1", "Read", ToolOutcome::Ok, 7));

        let calls = &app.entries()[0].calls;
        assert_eq!(
            calls
                .iter()
                .map(|call| (call.outcome, call.bytes))
                .collect::<Vec<_>>(),
            [
                (Some(ToolOutcome::Ok), Some(7)),
                (Some(ToolOutcome::Failed), Some(5))
            ]
        );
        assert!(!app.entries()[0].streaming);
        assert_eq!(app.entries()[0].kind, EntryKind::Failure);
    }

    #[test]
    fn another_tool_words_between_or_the_end_of_a_turn_break_a_run() {
        let mut app = app();
        called(&mut app, "t1", "Read");
        called(&mut app, "t2", "Bash");
        called(&mut app, "t3", "Read");
        app.apply(&Event::AssistantMessage {
            text: "and then".to_owned(),
            agent: None,
        });
        called(&mut app, "t4", "Read");
        app.apply(&Event::TurnEnded);
        called(&mut app, "t5", "Read");

        assert_eq!(
            heads(&app),
            vec![
                ("Read".to_owned(), 1),
                ("Bash".to_owned(), 1),
                ("Read".to_owned(), 1),
                ("agent".to_owned(), 0),
                ("Read".to_owned(), 1),
                // The rule the turn's end is drawn as, which has no head.
                (String::new(), 0),
                ("Read".to_owned(), 1),
            ]
        );
    }

    #[test]
    fn a_refusal_between_two_calls_breaks_their_run() {
        let mut app = app();
        called(&mut app, "t1", "Edit");
        app.apply(&start("t2", "Edit", "{}", None));
        app.apply(&Event::PermissionResponse {
            id: "t2".into(),
            decision: PermissionDecision::Deny,
            message: None,
        });
        app.apply(&ended("t2", "Edit", ToolOutcome::Denied, 0));
        called(&mut app, "t3", "Edit");

        assert_eq!(
            heads(&app),
            vec![
                ("Edit".to_owned(), 2),
                ("denied".to_owned(), 0),
                ("Edit".to_owned(), 1),
            ]
        );
    }

    fn millis(ms: u64) -> Stamp {
        Stamp::new(
            std::time::SystemTime::UNIX_EPOCH + Duration::from_millis(ms),
            None,
        )
    }

    fn said(text: &str) -> Event {
        Event::UserMessage {
            text: text.to_owned(),
        }
    }

    #[test]
    fn the_time_worked_is_every_turn_from_its_prompt_to_its_end() {
        let mut app = app();
        app.apply_at(&said("one"), millis(1_000));
        app.apply_at(&Event::TurnEnded, millis(61_000));
        // The idle between turns is not work.
        app.apply_at(&said("two"), millis(600_000));
        app.apply_at(&Event::TurnEnded, millis(630_000));

        assert_eq!(app.worked(), Some(Duration::from_secs(90)));
    }

    #[test]
    fn a_turn_still_running_counts_to_now_while_it_is_worked_on_here() {
        let mut app = app().attached();
        app.tick(Instant::now(), Some(millis(1_000)));
        typed_then_enter(
            &mut app,
            "go",
            ratatui::crossterm::event::KeyModifiers::NONE,
        );
        app.tick(Instant::now(), Some(millis(46_000)));

        assert_eq!(app.worked(), Some(Duration::from_secs(45)));
    }

    #[test]
    fn a_turn_read_back_unfinished_counts_to_the_last_thing_it_did() {
        let mut app = app();
        app.apply_at(&said("go"), millis(1_000));
        app.apply_at(&start("t1", "Bash", "{}", None), millis(21_000));
        // Read back three days later: the turn was not running all that time.
        app.tick(Instant::now(), Some(millis(259_200_000)));

        assert_eq!(app.worked(), Some(Duration::from_secs(20)));
    }

    /// A record that stops in the middle of a turn — the session was quit
    /// while it ran — taken up again forty-seven minutes later.
    fn resumed_after_leaving_mid_turn() -> App {
        let mut app = app();
        app.apply_at(&said("slow"), millis(1_000));
        app.apply_at(
            &Event::AssistantDelta {
                text: "tick 0 tick 1".to_owned(),
            },
            millis(6_000),
        );
        app.apply_at(&Event::SessionLeft, millis(2_826_000));
        app.apply_at(&said("again"), millis(2_826_000));
        app.apply_at(&Event::TurnEnded, millis(2_826_100));
        app
    }

    #[test]
    fn a_turn_a_quit_left_open_is_closed_where_its_record_stops() {
        let app = resumed_after_leaving_mid_turn();

        let rules: Vec<(bool, Option<Duration>)> = app
            .entries()
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::Turn(rule) => Some((rule.cut, rule.took)),
                _ => None,
            })
            .collect();
        assert_eq!(
            rules,
            [
                (true, Some(Duration::from_secs(5))),
                (false, Some(Duration::from_millis(100)))
            ],
            "the first turn is timed to its last recorded event, and cut"
        );
        assert_eq!(
            app.worked(),
            Some(Duration::from_millis(5_100)),
            "the time niobe was not running was counted as work"
        );
    }

    #[test]
    fn a_reply_the_session_left_half_written_says_it_was_cut_off() {
        let app = resumed_after_leaving_mid_turn();

        let reply = app
            .entries()
            .iter()
            .find(|entry| entry.kind == EntryKind::Agent)
            .expect("the reply is drawn");
        assert!(!reply.streaming);
        assert!(reply.meta.contains("cut off"), "{reply:?}");
    }

    #[test]
    fn leaving_records_the_turn_it_cuts_and_nothing_when_none_runs() {
        let mut idle = app();
        idle.leave();
        assert!(idle.take_produced().is_empty());

        let mut app = sent(app().attached(), "go");
        app.take_produced();
        app.leave();
        assert_eq!(app.take_produced(), [Event::SessionLeft]);
        assert!(!app.session().turn_running());
    }

    #[test]
    fn a_session_with_a_turn_nobody_timed_has_worked_for_no_known_time() {
        let mut app = app();
        // Folded with no clock, as an imported transcript is.
        app.apply(&said("one"));
        app.apply(&Event::TurnEnded);
        app.apply_at(&said("two"), millis(1_000));
        app.apply_at(&Event::TurnEnded, millis(61_000));

        assert_eq!(app.worked(), None);
    }

    #[test]
    fn a_session_with_no_turn_has_worked_for_no_known_time() {
        assert_eq!(app().worked(), None);
    }

    #[test]
    fn a_call_is_timed_from_its_start_to_its_end_by_the_clock_it_was_folded_at() {
        let mut app = app();
        app.apply_at(&start("t1", "Bash", "{}", None), millis(1_000));
        app.apply_at(&ended("t1", "Bash", ToolOutcome::Ok, 3), millis(1_300));

        assert_eq!(
            app.entries()[0].calls[0].took,
            Some(Duration::from_millis(300))
        );
    }

    #[test]
    fn a_call_whose_ends_the_clock_cannot_tell_apart_was_not_timed() {
        let mut app = app();
        // Inside one tick of the event loop, and as close together as a
        // session read in from another record is written down.
        for (id, ended_at) in [("t1", 1_000), ("t2", 1_003), ("t3", 1_099)] {
            app.apply(&Event::TurnEnded);
            app.apply_at(&start(id, "Bash", "{}", None), millis(1_000));
            app.apply_at(&ended(id, "Bash", ToolOutcome::Ok, 3), millis(ended_at));
        }
        app.apply(&Event::TurnEnded);
        app.apply_at(&start("t4", "Bash", "{}", None), millis(1_000));
        app.apply_at(&ended("t4", "Bash", ToolOutcome::Ok, 3), millis(1_100));

        let took: Vec<Option<Duration>> = app
            .entries()
            .iter()
            .flat_map(|entry| &entry.calls)
            .map(|call| call.took)
            .collect();
        assert_eq!(took, [None, None, None, Some(Duration::from_millis(100))]);
    }

    #[test]
    fn a_call_folded_in_with_no_clock_has_no_duration_rather_than_none_at_all() {
        let mut app = app();
        called(&mut app, "t1", "Bash");

        assert_eq!(app.entries()[0].calls[0].took, None);
        assert_eq!(app.entries()[0].calls[0].bytes, Some(10));
    }

    #[test]
    fn the_time_a_call_waited_on_the_operator_is_not_counted_as_the_tools() {
        let mut app = app();
        app.apply_at(&start("t1", "Bash", "{}", None), millis(1_000));
        app.apply_at(
            &Event::PermissionRequest {
                id: "t1".into(),
                tool: "Bash".to_owned(),
                input: "{}".to_owned(),
                target: None,
                agent: None,
            },
            millis(1_100),
        );
        app.apply_at(
            &Event::PermissionResponse {
                id: "t1".into(),
                decision: PermissionDecision::Allow,
                message: None,
            },
            millis(61_100),
        );
        app.apply_at(&ended("t1", "Bash", ToolOutcome::Ok, 3), millis(61_500));

        assert_eq!(
            app.entries()[0].calls[0].took,
            Some(Duration::from_millis(400))
        );
    }

    #[test]
    fn every_call_keeps_its_own_bytes_and_they_add_up_to_the_sessions() {
        let mut app = app();
        for (id, name, bytes) in [("t1", "Read", 100), ("t2", "Read", 250), ("t3", "Bash", 7)] {
            app.apply(&start(id, name, "{}", None));
            app.apply(&ended(id, name, ToolOutcome::Ok, bytes));
        }

        let per_call: u64 = app
            .entries()
            .iter()
            .flat_map(|entry| &entry.calls)
            .filter_map(|call| call.bytes)
            .sum();
        assert_eq!(per_call, 357);
        assert_eq!(app.session().tools().output_bytes, per_call);
    }

    #[test]
    fn a_calls_exit_status_reason_and_line_counts_reach_its_row() {
        let mut app = app();
        app.apply(&start("t1", "Bash", "{}", None));
        app.apply(&Event::ToolCallEnd {
            id: "t1".into(),
            name: "Bash".to_owned(),
            input: String::new(),
            output: "Exit code 2\nno such file".to_owned(),
            bytes: 24,
            outcome: ToolOutcome::Failed,
            summary: None,
            exit_code: Some(2),
            error: Some("no such file".to_owned()),
        });
        app.apply(&Event::AssistantMessage {
            text: "then".to_owned(),
            agent: None,
        });
        app.apply(&start("t2", "Write", "{}", None));
        app.apply(&ended("t2", "Write", ToolOutcome::Ok, 20));
        app.apply(&Event::FileChange {
            path: "a.rs".to_owned(),
            added: Some(4),
            removed: None,
            hunks: Vec::new(),
        });

        let shell = &app.entries()[0].calls[0];
        assert_eq!(
            (shell.exit_code, shell.error.as_deref()),
            (Some(2), Some("no such file"))
        );
        let write = &app.entries()[2].calls[0];
        assert_eq!(write.lines, Some((Some(4), None)));
        assert_eq!(write.change, None, "counts alone draw no diff");
    }

    #[test]
    fn ctrl_o_folds_every_run_of_calls_and_opens_them_again() {
        let mut app = app();
        assert!(!app.calls_folded(), "a run starts open");

        app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(app.calls_folded());
        assert_eq!(app.composed(), "", "the key reached the composer");

        app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(!app.calls_folded());
    }

    fn ctrl_t(app: &mut App) {
        app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    }

    fn ctrl_f(app: &mut App) {
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    }

    #[test]
    fn ctrl_t_opens_every_cut_diff_and_cuts_them_again() {
        let mut app = app();
        assert!(!app.diffs_open(), "a diff starts cut");

        ctrl_t(&mut app);
        assert!(app.diffs_open());
        assert_eq!(app.composed(), "", "the key reached the composer");

        ctrl_t(&mut app);
        assert!(!app.diffs_open());
    }

    #[test]
    fn ctrl_t_opens_the_diffs_while_a_search_is_open_and_leaves_the_query_alone() {
        let mut app = app();
        ctrl_f(&mut app);
        press(&mut app, KeyCode::Char('x'));

        ctrl_t(&mut app);

        assert!(app.diffs_open());
        assert_eq!(app.find_query().as_deref(), Some("x"));
    }

    /// The change a question is about may be the one that was cut, so the
    /// key reads the diff rather than being swallowed with the others.
    #[test]
    fn ctrl_t_opens_the_diffs_while_a_question_waits_and_answers_nothing() {
        let mut app = asked(Some("ls"));

        ctrl_t(&mut app);

        assert!(app.diffs_open());
        assert!(app.asking().is_some(), "the key answered the question");
        assert!(app.take_produced().is_empty(), "{:?}", app.take_produced());
    }

    /// Hands `bytes` to the shell the way the event loop does: decoded as one
    /// read of the terminal, every key in it arriving at `at`.
    fn read(app: &mut App, bytes: &[u8], at: Instant) {
        let mut events = Vec::new();
        crate::keys::Decoder::default().feed(bytes, false, &mut events);
        let keys: Vec<KeyEvent> = events
            .into_iter()
            .filter_map(|event| match event {
                ratatui::crossterm::event::Event::Key(key) => Some(key),
                _ => None,
            })
            .collect();
        let alone = keys.len() == 1;
        app.on_keys_read(&keys, Arrival { at, alone });
    }

    /// The answer field is for words, so words that arrive in one read — a
    /// text expander, dictation — go into it; an Enter among them still does
    /// not send.
    #[test]
    fn keys_read_together_go_into_the_answer_being_written() {
        let mut app = asked(Some("ls"));
        let at = Instant::now();
        read(&mut app, b"\t", at + ASK_QUIET * 2);
        assert_eq!(app.ask_focus(), AskFocus::Writing);

        read(&mut app, b"use rg instead\r", at + ASK_QUIET * 4);

        assert_eq!(app.ask_draft(), "use rg instead");
        assert!(
            app.asking().is_some(),
            "an Enter in a burst sent the answer"
        );
    }

    #[test]
    fn keys_read_together_go_to_an_open_menu_and_not_into_the_prompt() {
        let mut menu = app();
        let mut sheet = app();
        read(&mut menu, b"\x1bv", Instant::now());
        assert!(menu.menu().is_some());
        read(&mut menu, b"t\r", Instant::now());
        assert_eq!(
            menu.picking().map(|picker| picker.purpose),
            Some(Purpose::Theme)
        );
        assert_eq!(menu.composer().lines(), [""]);

        sheet.on_key(key(KeyCode::F(1)));
        read(&mut sheet, b"xy\r", Instant::now());
        assert_eq!(sheet.composer().lines(), [""], "nor past an open sheet");
        assert_eq!(sheet.sheet(), None, "whose Enter closes it");
    }

    #[test]
    fn a_completed_path_with_a_space_goes_in_quoted() {
        let mut app = App::new(Repo {
            name: "niobe".to_owned(),
            files: vec!["dir with space/file name.txt".to_owned()],
            ..Default::default()
        });
        for c in "see @file".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }

        app.on_key(key(KeyCode::Tab));

        assert_eq!(
            app.composer().lines(),
            [r#"see @"dir with space/file name.txt" "#]
        );
    }

    /// What typing `text` a key at a time leaves in the composer.
    fn typed_alone(text: &str) -> Vec<String> {
        let mut app = app();
        let at = Instant::now();
        for byte in text.bytes() {
            read(&mut app, &[byte], at);
        }
        app.composer().lines().to_vec()
    }

    /// A terminal that does not bracket pastes sends a pasted line break as
    /// Enter, in the same read as the text around it.
    #[test]
    fn an_enter_pasted_after_a_bang_runs_no_command() {
        let mut app = app().runs_commands();

        read(&mut app, b"!echo pwned\r", Instant::now());

        assert!(app.take_commands().is_empty(), "a pasted line ran");
        assert_eq!(app.composer().lines(), ["echo pwned", ""]);
    }

    #[test]
    fn an_enter_pasted_into_a_prompt_sends_nothing() {
        let mut app = app();

        read(&mut app, b"hello\rworld\r", Instant::now());

        let sent: Vec<Event> = app
            .take_produced()
            .into_iter()
            .filter(|event| matches!(event, Event::UserMessage { .. }))
            .collect();
        assert!(sent.is_empty(), "{sent:?}");
        assert_eq!(app.composer().lines(), ["hello", "world", ""]);
    }

    #[test]
    fn an_enter_pressed_alone_still_sends() {
        let mut app = app();
        read(&mut app, b"hello", Instant::now());

        read(&mut app, b"\r", Instant::now());

        assert!(app.composer().is_empty());
    }

    #[test]
    fn keys_read_together_type_what_they_type_one_at_a_time() {
        let text = "look at @src/ma, /tmp and ops@example.com! 2 then @x";
        let mut app = app();

        read(&mut app, text.as_bytes(), Instant::now());

        assert_eq!(app.composer().lines(), typed_alone(text));
        assert_eq!(app.composer().lines(), [text]);
        assert!(app.finding().is_none(), "a slash inside the text searched");
    }

    #[test]
    fn a_slash_that_opens_a_read_starts_a_command_with_the_rest_of_it() {
        let mut app = app();

        read(&mut app, b"/build", Instant::now());

        assert!(app.finding().is_none(), "the slash opened the search");
        assert_eq!(app.composer().lines(), ["/build"]);
    }

    #[test]
    fn ctrl_f_searches_with_a_prompt_half_written_and_esc_gives_it_back() {
        let mut app = app();
        read(&mut app, b"half a", Instant::now());

        ctrl_f(&mut app);
        press(&mut app, KeyCode::Char('x'));

        let query = app.finding().expect("Ctrl+F opened the search");
        assert_eq!(query.lines(), ["x"]);
        assert_eq!(app.composer().lines(), ["half a"]);
        press(&mut app, KeyCode::Esc);
        assert!(app.finding().is_none());
        assert_eq!(app.composer().lines(), ["half a"]);
    }

    /// Typed quickly while the shell was busy, `run it` and its Enter can
    /// come in one read; so does a paste of the same bytes from a terminal
    /// that does not bracket pastes, and nothing tells the two apart. The
    /// Enter is taken as the paste's line break: a prompt left unsent costs
    /// one more press, a pasted line sent or run costs whatever it said.
    #[test]
    fn an_enter_read_with_the_keys_before_it_is_a_line_break() {
        let mut app = app();

        read(&mut app, b"run it\r", Instant::now());

        assert!(app.take_produced().is_empty());
        assert_eq!(app.composer().lines(), ["run it", ""]);
    }

    /// A prompt for `rm -rf build`, put on screen by the tick at `shown`.
    fn shown_at(shown: Instant) -> App {
        let mut app = app();
        app.tick(shown, None);
        app.apply(&prompt(Some("rm -rf build")));
        assert_eq!(
            app.ask_options(),
            [
                Answer::Once,
                Answer::AlwaysTool,
                Answer::AlwaysTarget,
                Answer::No
            ]
        );
        app
    }

    #[test]
    fn a_paste_answers_no_prompt_and_stores_no_rule() {
        let shown = Instant::now();
        let mut app = shown_at(shown);

        read(
            &mut app,
            b"step 2 failed\rsee log",
            shown + Duration::from_secs(5),
        );

        assert!(app.asking().is_some(), "the paste answered the question");
        assert!(app.take_produced().is_empty());
        assert!(app.take_rules().is_empty());
        assert_eq!(
            app.ask_selected(),
            Answer::Once,
            "the paste chose an answer"
        );
    }

    #[test]
    fn a_three_line_paste_is_one_three_line_prompt_and_sends_nothing() {
        let mut app = app();

        app.on_paste("here is the log:\rerror: boom\rat foo.rs:3");

        assert_eq!(
            app.composer().lines(),
            ["here is the log:", "error: boom", "at foo.rs:3"]
        );
        assert!(app.take_produced().is_empty(), "the paste sent a turn");

        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.take_produced(),
            [Event::UserMessage {
                text: "here is the log:\nerror: boom\nat foo.rs:3".to_owned()
            }]
        );
    }

    #[test]
    fn a_pasted_line_break_is_one_line_break_whatever_it_was_written_as() {
        let mut app = app();

        app.on_paste("crlf\r\nlf\ncr\rend");

        assert_eq!(app.composer().lines(), ["crlf", "lf", "cr", "end"]);
    }

    #[test]
    fn a_paste_goes_in_at_the_cursor_without_opening_search_or_a_command() {
        let mut app = app();

        app.on_paste("/tmp holds it");
        app.on_paste("!");

        assert!(app.finding().is_none(), "a pasted slash opened the search");
        assert_eq!(app.composer().lines(), ["/tmp holds it!"]);
    }

    #[test]
    fn a_paste_into_the_search_is_one_line_of_query() {
        let mut app = app();
        ctrl_f(&mut app);

        app.on_paste("two\rwords");

        let query = app.finding().expect("Ctrl+F opened the search");
        assert_eq!(query.lines(), ["two words"]);
        assert!(
            app.composer().is_empty(),
            "the paste landed behind the search"
        );
    }

    #[test]
    fn a_bracketed_paste_answers_no_prompt_and_types_nothing_under_it() {
        let shown = Instant::now();
        let mut app = shown_at(shown);

        app.on_paste("2\r");

        assert!(app.asking().is_some(), "the paste answered the question");
        assert!(app.take_rules().is_empty());
        assert_eq!(
            app.ask_selected(),
            Answer::Once,
            "the paste chose an answer"
        );
        assert!(
            app.composer().is_empty(),
            "the paste went under the question"
        );
    }

    #[test]
    fn a_paste_into_a_written_answer_is_written_and_sends_nothing() {
        let mut app = shown_at(Instant::now());
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

        app.on_paste("keep\rbuild");

        assert_eq!(app.ask_draft(), "keep build");
        assert!(app.asking().is_some(), "the paste sent the answer");
    }

    #[test]
    fn keys_typed_as_a_prompt_appears_do_not_answer_it() {
        let shown = Instant::now();
        let mut app = shown_at(shown);

        // "retry 2⏎", a key every 200 ms, the prompt up after "retry ".
        read(&mut app, b"2", shown + Duration::from_millis(150));
        read(&mut app, b"\r", shown + Duration::from_millis(350));
        read(&mut app, b"\r", shown + Duration::from_millis(550));

        assert!(app.asking().is_some(), "typing ahead answered the question");
        assert!(app.take_produced().is_empty());
        assert!(app.take_rules().is_empty());
        assert_eq!(app.ask_selected(), Answer::Once);
        assert!(app.hint().is_some(), "nothing said the keys were not taken");
    }

    #[test]
    fn a_deliberate_key_after_the_pause_still_answers() {
        let shown = Instant::now();
        let mut app = shown_at(shown);

        read(&mut app, b"2", shown + Duration::from_secs(2));
        read(&mut app, b"\r", shown + Duration::from_secs(3));

        assert!(app.asking().is_none());
        assert_eq!(
            app.take_produced(),
            [Event::PermissionResponse {
                id: "t1".into(),
                decision: PermissionDecision::AllowAlways,
                message: None,
            }]
        );
        assert_eq!(app.take_rules(), [Rule::tool("Bash")]);
    }

    #[test]
    fn typing_on_after_a_prompt_appears_holds_it_until_the_keyboard_is_quiet() {
        let shown = Instant::now();
        let mut app = shown_at(shown);

        read(&mut app, b"2", shown + Duration::from_millis(400));
        read(&mut app, b"\r", shown + Duration::from_millis(800));
        assert!(app.asking().is_some(), "typing on answered the question");

        read(&mut app, b"\r", shown + Duration::from_millis(1800));
        assert_eq!(
            app.take_produced(),
            [Event::PermissionResponse {
                id: "t1".into(),
                decision: PermissionDecision::Allow,
                message: None,
            }]
        );
    }

    #[test]
    fn a_second_enter_does_not_answer_the_prompt_the_first_one_brought_up() {
        let mut app = asked(Some("ls"));
        let first = Instant::now();
        app.tick(first, None);

        read(&mut app, b"\r", first + Duration::from_secs(2));
        read(&mut app, b"\r", first + Duration::from_millis(2100));

        assert_eq!(app.asking().map(|ask| ask.id.as_str()), Some("t2"));
        assert_eq!(app.take_produced().len(), 1);
    }

    /// Types `text` a key at a time, `every` apart from `from`, and gives
    /// back when the last key was read.
    fn typed_apart(app: &mut App, text: &str, from: Instant, every: Duration) -> Instant {
        let mut at = from;
        for c in text.chars() {
            at += every;
            read(app, c.to_string().as_bytes(), at);
        }
        at
    }

    #[test]
    fn a_question_brought_up_by_withdrawing_the_one_being_answered_waits_for_a_quiet_keyboard() {
        let shown = Instant::now();
        let mut app = asked(Some("ls"));
        app.tick(shown, None);
        let every = Duration::from_millis(150);
        read(&mut app, b"\t", shown + ASK_QUIET * 2);
        let written = typed_apart(&mut app, "use rg", shown + ASK_QUIET * 2, every);

        let withdrawn = written + every;
        app.tick(withdrawn, None);
        app.apply(&Event::PermissionWithdrawn { id: "t1".into() });
        assert_eq!(app.hint(), Some(QUESTION_WITHDRAWN_HINT));
        let last = typed_apart(&mut app, " instead", withdrawn, every);
        read(&mut app, b"\r", last + every);

        assert_eq!(app.take_produced(), []);
        assert_eq!(app.hint(), Some(TOO_SOON_HINT));
        assert_eq!(app.asking().map(|ask| ask.id.as_str()), Some("t2"));
        assert_eq!(app.ask_focus(), AskFocus::Choosing);
        read(&mut app, b"\r", last + every + ASK_QUIET * 2);
        assert_eq!(
            app.take_produced(),
            [Event::PermissionResponse {
                id: "t2".into(),
                decision: PermissionDecision::Allow,
                message: None,
            }]
        );
    }

    /// A shell whose only question was being answered when `ended` took it
    /// away, the operator typing on through it to an Enter; with when the
    /// Enter was read.
    fn written_through_a_withdrawal(ended: &Event) -> (App, Instant) {
        let shown = Instant::now();
        let mut app = sent(app().attached(), "go");
        app.take_produced();
        app.tick(shown, None);
        app.apply(&prompt(Some("rm -rf build")));
        let every = Duration::from_millis(150);
        read(&mut app, b"\t", shown + ASK_QUIET * 2);
        let written = typed_apart(&mut app, "no, use", shown + ASK_QUIET * 2, every);

        let withdrawn = written + every;
        app.tick(withdrawn, None);
        app.apply(ended);
        assert!(app.asking().is_none());
        assert_eq!(app.hint(), Some(QUESTION_WITHDRAWN_HINT));
        let last = typed_apart(&mut app, " rg", withdrawn, every);
        read(&mut app, b"\r", last + every);
        (app, last + every)
    }

    #[test]
    fn an_enter_typed_through_a_withdrawn_question_sends_nothing() {
        let (mut app, pressed) =
            written_through_a_withdrawal(&Event::PermissionWithdrawn { id: "t1".into() });

        assert_eq!(app.take_produced(), []);
        assert_eq!(app.hint(), Some(WITHDRAWN_NOT_SENT_HINT));
        assert_eq!(app.composed(), "no, use rg");
        read(&mut app, b"\r", pressed + ASK_QUIET * 2);
        assert_eq!(
            app.take_produced(),
            [Event::UserMessage {
                text: "no, use rg".to_owned()
            }]
        );
    }

    #[test]
    fn an_enter_typed_through_a_question_its_turn_ended_on_sends_nothing() {
        let (mut app, _) = written_through_a_withdrawal(&Event::TurnEnded);

        assert_eq!(app.take_produced(), []);
        assert_eq!(app.hint(), Some(WITHDRAWN_NOT_SENT_HINT));
        assert_eq!(app.composed(), "no, use rg");
    }

    #[test]
    fn one_line_collapses_whitespace_into_single_spaces_and_handles_edge_cases() {
        assert_eq!(one_line(""), "");
        assert_eq!(one_line("   \t\n\r  "), "");
        assert_eq!(one_line("hello"), "hello");
        assert_eq!(one_line("  hello   world  "), "hello world");
        assert_eq!(
            one_line(
                "{\n  \"command\": \"cargo test\",\n  \"args\": [\n    \"--workspace\"\n  ]\n}"
            ),
            "{\n  \"command\": \"cargo test\",\n  \"args\": [\n    \"--workspace\"\n  ]\n}"
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        );
    }

    /// A shell asking whether to trust the repository's config, drawn first
    /// by the tick at `shown`.
    fn trusting_at(shown: Instant) -> App {
        let mut app = app().asking_trust(crate::trust::Question {
            path: "/r/.niobe/config.toml".to_owned(),
            grants: vec![("permissions".to_owned(), "allow Bash".to_owned())],
        });
        app.tick(shown, None);
        app
    }

    #[test]
    fn answering_the_trust_question_ends_the_shell_with_the_answer() {
        let shown = Instant::now();
        let mut app = trusting_at(shown);
        assert!(app.trusting().is_some());

        read(&mut app, b"\r", shown + ASK_QUIET * 2);

        assert_eq!(app.trust_answer(), Some(crate::trust::Answer::Trust));
        assert!(app.trusting().is_none());
        assert!(app.should_quit());
    }

    #[test]
    fn an_enter_typed_as_the_trust_question_came_up_trusts_nothing() {
        let shown = Instant::now();
        let mut app = trusting_at(shown);

        read(&mut app, b"\r", shown + ASK_QUIET / 5);
        assert_eq!(app.trust_answer(), None);
        assert_eq!(app.hint(), Some(TOO_SOON_HINT));
        // Each key held back starts the wait again.
        read(&mut app, b"\r", shown + ASK_QUIET);
        assert_eq!(app.trust_answer(), None);

        read(&mut app, b"\r", shown + ASK_QUIET * 3);
        assert_eq!(app.trust_answer(), Some(crate::trust::Answer::Trust));
    }

    #[test]
    fn a_key_read_before_the_trust_question_was_drawn_trusts_nothing() {
        let mut app = app().asking_trust(crate::trust::Question {
            path: "/r/.niobe/config.toml".to_owned(),
            grants: Vec::new(),
        });

        read(&mut app, b"\r", Instant::now());

        assert_eq!(app.trust_answer(), None);
        assert!(!app.should_quit());
    }

    #[test]
    fn neither_a_paste_nor_typing_reaches_a_prompt_behind_the_trust_question() {
        let shown = Instant::now();
        let mut app = trusting_at(shown);

        app.on_paste("fix the build");
        read(&mut app, b"x", shown + ASK_QUIET * 2);

        assert_eq!(app.composer().lines(), [""]);
        assert_eq!(app.trust_answer(), None);
        assert!(app.trusting().is_some());
    }

    #[test]
    fn quitting_from_the_trust_question_leaves_it_unanswered() {
        let shown = Instant::now();
        let mut app = trusting_at(shown);

        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert!(app.should_quit());
        assert_eq!(app.trust_answer(), None);
    }

    /// `app`, attached to a backend that lists `names` as its commands.
    fn offering(names: &[&str]) -> App {
        let mut app = app().attached();
        app.apply(&Event::Commands {
            commands: names
                .iter()
                .map(|name| SlashCommand {
                    name: (*name).to_owned(),
                    description: String::new(),
                    argument_hint: None,
                })
                .collect(),
        });
        app
    }

    fn alt(c: char) -> ratatui::crossterm::event::KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    /// The action the cursor of the open menu is on.
    fn under_cursor(app: &App) -> Option<crate::menu::Action> {
        app.menu()
            .and_then(|open| open.item())
            .map(|item| item.action)
    }

    #[test]
    fn esc_then_a_menus_letter_opens_that_menu_on_its_first_item() {
        let mut app = app();
        app.on_key(key(KeyCode::Esc));
        assert!(
            app.hint().is_some_and(|hint| hint.contains("V View")),
            "Esc says the letters open menus: {:?}",
            app.hint()
        );
        app.on_key(key(KeyCode::Char('v')));
        assert_eq!(app.menu().map(|open| open.menu().name), Some("View"));
        assert_eq!(under_cursor(&app), Some(crate::menu::Action::Diff));
        assert_eq!(app.composer().lines(), [""]);
    }

    #[test]
    fn alt_and_a_menus_letter_opens_it_too() {
        let mut app = app();
        app.on_key(alt('s'));
        assert_eq!(app.menu().map(|open| open.menu().name), Some("Session"));
    }

    #[test]
    fn a_letter_typed_without_esc_is_typed() {
        let mut app = app();
        app.on_key(key(KeyCode::Char('v')));
        assert_eq!(app.menu(), None);
        assert_eq!(app.composer().lines(), ["v"]);
    }

    #[test]
    fn the_arrows_walk_an_open_menu_and_enter_runs_the_item() {
        let mut app = app();
        app.on_key(alt('v'));
        app.on_key(key(KeyCode::Down));
        assert_eq!(under_cursor(&app), Some(crate::menu::Action::GroupByAgent));
        app.on_key(key(KeyCode::Enter));
        assert!(app.grouped_by_agent());
        assert_eq!(app.menu(), None, "running an item closes its menu");

        app.on_key(alt('v'));
        app.on_key(key(KeyCode::Right));
        assert_eq!(app.menu().map(|open| open.menu().name), Some("Help"));
        app.on_key(key(KeyCode::Left));
        app.on_key(key(KeyCode::Left));
        assert_eq!(app.menu().map(|open| open.menu().name), Some("Model"));
    }

    #[test]
    fn a_letter_in_an_open_menu_moves_the_cursor_and_runs_nothing() {
        let mut app = offering(&["clear"]);
        app.on_key(alt('s'));
        app.on_key(key(KeyCode::Char('n')));
        assert_eq!(under_cursor(&app), Some(crate::menu::Action::NewSession));
        assert_eq!(app.take_produced(), [], "nothing is sent until Enter");
        assert_eq!(app.composer().lines(), [""], "and nothing is typed");
    }

    #[test]
    fn esc_closes_a_menu_and_leaves_the_prompt_as_it_was() {
        let mut app = app();
        app.on_key(key(KeyCode::Char('x')));
        app.on_key(alt('h'));
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.menu(), None);
        assert_eq!(app.composer().lines(), ["x"]);
        app.on_key(key(KeyCode::Char('5')));
        assert_eq!(
            app.composer().lines(),
            ["x5"],
            "the Esc that closed it arms no F-key"
        );
    }

    #[test]
    fn a_click_on_a_menus_name_opens_it_and_a_click_on_an_item_runs_it() {
        let mut app = app();
        app.drew_bars(Rect::new(0, 0, 120, 1), Rect::new(0, 29, 120, 1));
        let (view, _) = crate::menu::title_columns()[4];
        app.on_mouse(click_at(view + 1, 0));
        assert_eq!(app.menu().map(|open| open.menu().name), Some("View"));

        app.drew_menu_list(Some(Rect::new(view, 1, 30, 8)));
        app.on_mouse(click_at(view + 2, 3));
        assert!(app.grouped_by_agent(), "the second item, under the border");
        assert_eq!(app.menu(), None);

        app.on_mouse(click_at(view + 1, 0));
        app.on_mouse(click_at(60, 15));
        assert_eq!(app.menu(), None, "a click anywhere else closes it");
        assert!(app.grouped_by_agent(), "and runs nothing");
    }

    #[test]
    fn a_click_on_the_f_key_bar_presses_that_key() {
        let mut app = app();
        app.drew_bars(Rect::new(0, 0, 120, 1), Rect::new(0, 29, 120, 1));
        let widths = crate::menu::fkey_widths(120);
        let start = |n: usize| crate::menu::stop_columns() + widths[..n].iter().sum::<u16>();
        app.on_mouse(click_at(start(4) + 2, 29));
        assert!(app.diffs_open(), "5 Diff");
        app.on_mouse(click_at(start(9) + 2, 29));
        assert!(app.should_quit(), "0 Quit");
    }

    /// Whether anything that takes keys of its own, or answers for the
    /// operator, is open over the shell.
    fn opened_over(app: &App) -> Vec<&'static str> {
        [
            (app.picking().is_some(), "a list"),
            (app.finding().is_some(), "the search"),
            (app.menu().is_some(), "a menu"),
            (app.sheet().is_some(), "a sheet"),
            (app.browser().is_some(), "the history"),
            (app.diffs_open(), "the cut diffs"),
        ]
        .into_iter()
        .filter_map(|(open, what)| open.then_some(what))
        .collect()
    }

    #[test]
    fn no_click_on_the_bars_opens_anything_while_a_question_waits() {
        for focus in [AskFocus::Choosing, AskFocus::Deferred] {
            for (row, columns) in [(0, 0..60), (29, 0..120)] {
                for column in columns {
                    let mut app = under_a_profile(&["opus", "sonnet"]);
                    app.drew_bars(Rect::new(0, 0, 120, 1), Rect::new(0, 29, 120, 1));
                    app.apply(&prompt(Some("rm -rf build")));
                    if focus == AskFocus::Deferred {
                        app.on_key(key(KeyCode::Esc));
                    }

                    app.on_mouse(click_at(column, row));

                    assert_eq!(
                        opened_over(&app),
                        [] as [&str; 0],
                        "{focus:?} {column},{row}"
                    );
                    assert_eq!(app.ask_focus(), focus, "{column},{row}");
                    assert!(app.asking().is_some(), "{column},{row}");
                    assert_eq!(app.take_produced(), [], "{column},{row}");
                    assert!(app.take_handoffs().is_empty(), "{column},{row}");
                }
            }
        }
    }

    #[test]
    fn a_click_on_the_bars_while_a_question_waits_says_to_answer_it_first() {
        let mut app = under_a_profile(&["opus", "sonnet"]);
        app.drew_bars(Rect::new(0, 0, 120, 1), Rect::new(0, 29, 120, 1));
        app.apply(&prompt(Some("rm -rf build")));
        let widths = crate::menu::fkey_widths(120);
        let model = crate::menu::stop_columns() + widths[..3].iter().sum::<u16>() + 2;

        app.on_mouse(click_at(model, 29));

        assert_eq!(app.hint(), Some(QUESTION_FIRST_HINT));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(
            app.take_produced(),
            [Event::PermissionResponse {
                id: "t1".into(),
                decision: PermissionDecision::AllowAlways,
                message: None,
            }],
            "the keys went to the question the operator was looking at"
        );
    }

    #[test]
    fn quitting_from_the_f_key_bar_still_works_while_a_question_waits() {
        let mut app = app();
        app.drew_bars(Rect::new(0, 0, 120, 1), Rect::new(0, 29, 120, 1));
        app.apply(&prompt(Some("rm -rf build")));
        let widths = crate::menu::fkey_widths(120);
        let quit = crate::menu::stop_columns() + widths[..9].iter().sum::<u16>() + 2;

        app.on_mouse(click_at(quit, 29));

        assert!(app.should_quit());
    }

    #[test]
    fn no_item_of_a_menu_open_when_a_question_comes_runs_from_a_click_but_quit() {
        use crate::menu::{Action, MENUS};
        for (at, menu) in MENUS.iter().enumerate() {
            for (row, item) in (2..).zip(menu.items) {
                let mut app = under_a_profile(&["opus", "sonnet"]);
                app.drew_bars(Rect::new(0, 0, 120, 1), Rect::new(0, 29, 120, 1));
                let (title, _) = crate::menu::title_columns()[at];
                app.on_mouse(click_at(title + 1, 0));
                app.apply(&prompt(Some("rm -rf build")));
                let rows = u16::try_from(menu.items.len()).expect("a menu holds a few items");
                app.drew_menu_list(Some(Rect::new(title, 1, 30, rows + 2)));

                app.on_mouse(click_at(title + 2, row));

                assert_eq!(opened_over(&app), [] as [&str; 0], "{}", item.label);
                assert!(app.asking().is_some(), "{}", item.label);
                assert_eq!(app.take_produced(), [], "{}", item.label);
                assert!(app.take_handoffs().is_empty(), "{}", item.label);
                assert_eq!(
                    app.should_quit(),
                    item.action == Action::Quit,
                    "{}",
                    item.label
                );
            }
        }
    }

    #[test]
    fn no_click_on_the_bars_opens_anything_under_the_trust_question() {
        for (row, columns) in [(0, 0..60), (29, 0..120)] {
            for column in columns {
                let shown = Instant::now();
                let mut app = trusting_at(shown);
                app.drew_bars(Rect::new(0, 0, 120, 1), Rect::new(0, 29, 120, 1));

                app.on_mouse(click_at(column, row));
                read(&mut app, b"\x1b[B", shown + ASK_QUIET * 2);

                assert_eq!(opened_over(&app), [] as [&str; 0], "{column},{row}");
                assert_eq!(app.trust_answer(), None, "{column},{row}");
                assert!(app.take_handoffs().is_empty(), "{column},{row}");
            }
        }
    }

    #[test]
    fn a_list_open_when_a_question_comes_takes_its_keys_and_answers_nothing() {
        let mut app = under_a_profile(&["opus", "sonnet"]);
        app.on_key(key(KeyCode::F(4)));
        app.apply(&prompt(Some("rm -rf build")));

        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));

        assert_eq!(
            app.take_produced(),
            [Event::ModelSelected {
                model: "sonnet".to_owned()
            }]
        );
        assert!(app.picking().is_none());
        assert!(app.asking().is_some());
        assert_eq!(
            app.ask_selected(),
            Answer::Once,
            "Down moved the question's answer"
        );
    }

    #[test]
    fn the_search_open_when_a_question_comes_takes_typing_and_enter_and_answers_nothing() {
        let mut app = app();
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        app.apply(&prompt(Some("rm -rf build")));

        app.on_key(key(KeyCode::Char('2')));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));

        assert_eq!(
            app.finding().map(|query| query.lines().to_vec()),
            Some(vec!["2".to_owned()])
        );
        assert_eq!(app.take_produced(), []);
        assert!(app.asking().is_some());
        assert_eq!(
            app.ask_selected(),
            Answer::Once,
            "Down moved the question's answer"
        );
    }

    #[test]
    fn a_paste_into_the_search_open_over_a_question_goes_into_the_search() {
        let mut app = app();
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        app.apply(&prompt(Some("rm -rf build")));

        app.on_paste("error");

        assert_eq!(
            app.finding().map(|query| query.lines().to_vec()),
            Some(vec!["error".to_owned()])
        );
        assert!(app.asking().is_some());
    }

    #[test]
    fn esc_closes_a_list_opened_under_a_question_put_aside_and_leaves_it_aside() {
        let mut app = under_a_profile(&["opus", "sonnet"]);
        app.apply(&prompt(Some("rm -rf build")));
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::F(4)));
        assert!(app.picking().is_some());

        app.on_key(key(KeyCode::Esc));

        assert!(app.picking().is_none());
        assert_eq!(app.ask_focus(), AskFocus::Deferred);
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.take_produced(), []);
        assert!(app.asking().is_some());
    }

    #[test]
    fn esc_closes_a_search_opened_under_a_question_put_aside_and_leaves_it_aside() {
        let mut app = app();
        app.apply(&prompt(Some("rm -rf build")));
        app.on_key(key(KeyCode::Esc));
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(app.finding().is_some());
        app.on_key(key(KeyCode::Char('e')));

        app.on_key(key(KeyCode::Esc));

        assert!(app.finding().is_none());
        assert_eq!(app.ask_focus(), AskFocus::Deferred);
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.take_produced(), []);
        assert!(app.asking().is_some());
    }

    #[test]
    fn a_question_uncovered_by_closing_a_list_waits_for_a_quiet_keyboard_again() {
        let shown = Instant::now();
        let mut app = under_a_profile(&["opus", "sonnet"]);
        app.tick(shown, None);
        app.on_key(key(KeyCode::F(4)));
        app.apply(&prompt(Some("rm -rf build")));
        let alone = |at| Arrival { at, alone: true };

        app.on_key_read(key(KeyCode::Esc), alone(shown + ASK_QUIET * 2));
        app.on_key_read(
            key(KeyCode::Enter),
            alone(shown + ASK_QUIET * 2 + ASK_QUIET / 5),
        );

        assert!(app.picking().is_none());
        assert_eq!(app.take_produced(), []);
        assert_eq!(app.hint(), Some(TOO_SOON_HINT));
        app.on_key_read(key(KeyCode::Enter), alone(shown + ASK_QUIET * 5));
        assert_eq!(
            app.take_produced(),
            [Event::PermissionResponse {
                id: "t1".into(),
                decision: PermissionDecision::Allow,
                message: None,
            }]
        );
    }

    #[test]
    fn f2_and_f3_send_the_backends_own_commands() {
        let mut app = offering(&["compact", "clear"]);
        app.on_key(key(KeyCode::F(2)));
        assert_eq!(
            app.take_produced(),
            [Event::UserMessage {
                text: "/compact".to_owned()
            }]
        );
        app.apply(&Event::TurnEnded);
        app.on_key(key(KeyCode::F(3)));
        assert_eq!(
            app.take_produced(),
            [Event::UserMessage {
                text: "/clear".to_owned()
            }]
        );
    }

    #[test]
    fn a_command_the_backend_does_not_list_is_not_sent_and_says_why() {
        let mut app = offering(&["compact"]);
        app.on_key(key(KeyCode::F(7)));
        assert_eq!(app.take_produced(), []);
        assert!(
            app.hint().is_some_and(|hint| hint.contains("/mcp")),
            "{:?}",
            app.hint()
        );
    }

    #[test]
    fn a_backend_command_waits_for_the_running_turn() {
        let mut app = offering(&["compact", "clear"]);
        typed_then_enter(&mut app, "go", KeyModifiers::NONE);
        app.take_produced();
        app.on_key(key(KeyCode::F(3)));
        assert_eq!(app.take_produced(), [], "a /clear mid-turn is not sent");
        assert!(app.hint().is_some_and(|hint| hint.contains("Esc")));
    }

    #[test]
    fn a_backend_command_leaves_the_images_of_the_prompt_being_written() {
        let mut app = offering(&["compact"]);
        app.on_key(key(KeyCode::F(2)));
        app.take_produced();
        assert_eq!(app.take_turn_images(), Vec::new());
    }

    #[test]
    fn f9_shows_the_settings_and_the_files_they_come_from() {
        let mut app = app().with_places(Places {
            config_files: vec![
                ConfigFile {
                    path: "/home/me/.config/niobe/config.toml".to_owned(),
                    exists: true,
                    openable: true,
                },
                ConfigFile {
                    path: "/work/repo/.niobe/config.toml".to_owned(),
                    exists: false,
                    openable: false,
                },
            ],
            memory: None,
        });
        app.on_key(key(KeyCode::F(9)));
        let sheet = app.sheet().expect("F9 opens the settings");
        assert_eq!(sheet.title, "Settings");
        let text = sheet.rows.join("\n");
        assert!(
            text.contains("/home/me/.config/niobe/config.toml"),
            "{text}"
        );
        assert!(text.contains("not there"), "{text}");
        assert!(text.contains("cyber"), "the theme in force: {text}");

        app.on_key(key(KeyCode::Char('o')));
        assert_eq!(
            app.take_handoffs(),
            [crate::desktop::Handoff::Open(
                "/home/me/.config/niobe/config.toml".to_owned()
            )],
            "o opens the file that is there"
        );
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.sheet(), None);
        assert_eq!(app.composer().lines(), [""]);
    }

    #[test]
    fn the_permissions_sheet_lists_the_standing_answers() {
        let mut allowed = Allowlist::new();
        allowed.insert(Rule::prefixed("Bash", "cargo test"));
        let mut app = app().with_rules(allowed);
        app.on_key(alt('n'));
        app.on_key(key(KeyCode::Char('p')));
        app.on_key(key(KeyCode::Enter));
        let sheet = app.sheet().expect("the permissions");
        assert!(
            sheet.rows.iter().any(|row| row.contains("Bash(cargo test")),
            "{:?}",
            sheet.rows
        );
    }

    #[test]
    fn f8_opens_the_repositorys_memory_file_or_says_there_is_none() {
        let mut without = app();
        let mut with = app().with_places(Places {
            config_files: Vec::new(),
            memory: Some("/work/repo/CLAUDE.md".to_owned()),
        });
        with.on_key(key(KeyCode::F(8)));
        assert_eq!(
            with.take_handoffs(),
            [crate::desktop::Handoff::Open(
                "/work/repo/CLAUDE.md".to_owned()
            )]
        );

        without.on_key(key(KeyCode::F(8)));
        assert_eq!(without.take_handoffs(), []);
        assert!(
            without
                .hint()
                .is_some_and(|hint| hint.contains("CLAUDE.md"))
        );
    }

    #[test]
    fn the_theme_list_switches_to_the_theme_chosen() {
        use crate::theme::{CYBER, NEO};

        let mut app = app();
        app.on_key(alt('v'));
        app.on_key(key(KeyCode::Char('t')));
        app.on_key(key(KeyCode::Enter));
        let picker = app.picking().expect("the theme list");
        assert_eq!(picker.purpose, Purpose::Theme);
        assert_eq!(
            picker.options[picker.at], CYBER.name,
            "on the theme in force"
        );
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.theme().name, NEO.name);
        assert_eq!(
            app.composer().style(),
            Style::new().fg(NEO.fg).bg(NEO.pane_bg),
            "the composer is repainted in it"
        );
        assert_eq!(app.picking(), None);
    }

    #[test]
    fn the_effort_list_sends_the_level_chosen() {
        let mut app = offering(&["effort"]);
        app.on_key(alt('m'));
        app.on_key(key(KeyCode::Char('e')));
        app.on_key(key(KeyCode::Enter));
        let picker = app.picking().expect("the effort list");
        assert_eq!(picker.purpose, Purpose::Effort);
        assert_eq!(picker.options, EFFORTS);
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(
            app.take_produced(),
            [Event::UserMessage {
                text: "/effort medium".to_owned()
            }]
        );
    }

    #[test]
    fn a_pane_hidden_from_the_view_menu_is_not_drawn_or_focused() {
        use crate::menu::SidePane;

        let mut app = app();
        assert!(app.shows(SidePane::Changes));
        app.on_key(alt('v'));
        app.on_key(key(KeyCode::Char('c')));
        app.on_key(key(KeyCode::Enter));
        assert!(!app.shows(SidePane::Changes));
        assert!(app.shows(SidePane::Activity));

        app.on_key(alt('v'));
        app.on_key(key(KeyCode::Char('c')));
        app.on_key(key(KeyCode::Enter));
        assert!(app.shows(SidePane::Changes), "and shown again");
    }

    #[test]
    fn f1_lists_the_keys_and_esc_closes_the_list() {
        let mut app = app();
        app.on_key(key(KeyCode::F(1)));
        let sheet = app.sheet().expect("the shortcuts");
        assert!(sheet.rows.iter().any(|row| row.contains("Ctrl+O")));
        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(app.composer().lines(), [""], "the list has the keyboard");
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.sheet(), None);
    }

    #[test]
    fn what_the_shell_cannot_do_says_why_rather_than_doing_nothing() {
        use crate::menu::Action;

        for action in [
            Action::SignIn,
            Action::Resume,
            Action::Rewind,
            Action::Hooks,
        ] {
            let mut app = app();
            app.perform(action);
            assert!(
                app.hint().is_some_and(|hint| hint.len() > 20),
                "{action:?}: {:?}",
                app.hint()
            );
            assert_eq!(app.take_produced(), [], "{action:?}");
        }
    }

    #[test]
    fn add_file_starts_an_at_word_and_commands_a_slash() {
        let mut naming = app();
        naming.perform(crate::menu::Action::AddFile);
        assert_eq!(naming.composer().lines(), ["@"]);

        let mut commanding = app();
        commanding.perform(crate::menu::Action::Commands);
        assert_eq!(commanding.composer().lines(), ["/"]);
    }

    #[test]
    fn the_links_open_this_projects_pages() {
        let mut app = app();
        app.perform(crate::menu::Action::ReportBug);
        app.perform(crate::menu::Action::ReleaseNotes);
        let opened: Vec<String> = app
            .take_handoffs()
            .into_iter()
            .map(|handoff| match handoff {
                crate::desktop::Handoff::Open(link) => link,
                crate::desktop::Handoff::Copy(text) => panic!("copied {text}"),
            })
            .collect();
        assert_eq!(opened.len(), 2);
        assert!(
            opened.iter().all(|link| link.starts_with("https://")),
            "{opened:?}"
        );
    }

    #[test]
    fn stop_from_the_menu_stops_a_running_turn_and_says_so_when_none_is() {
        let mut app = app().attached();
        app.perform(crate::menu::Action::Stop);
        assert!(app.hint().is_some_and(|hint| hint.contains("Nothing")));

        typed_then_enter(&mut app, "go", KeyModifiers::NONE);
        app.perform(crate::menu::Action::Stop);
        assert!(app.take_interrupt());
    }

    /// A `cargo test` call `id` that starts at `from` seconds and, where
    /// `counted`, ends at `to` with a run whose counts were read.
    fn test_call(app: &mut App, id: &str, command: &str, from: u64, to: u64, counted: bool) {
        let input = format!(r#"{{"command":"{command}"}}"#);
        app.apply_at(&start(id, "Bash", &input, Some(command)), at(from, 9, 0));
        app.apply_at(&ended(id, "Bash", ToolOutcome::Ok, 10), at(to, 9, 1));
        app.apply_at(
            &Event::TestRun {
                id: id.into(),
                counts: counted.then_some(niobe_core::TestCounts {
                    passed: 3,
                    failed: 0,
                    ignored: 0,
                    suites: 1,
                }),
                exit_code: Some(0),
                failed: false,
                failures: Vec::new(),
            },
            at(to, 9, 1),
        );
    }

    fn last_call(app: &App) -> &Call {
        app.entries()
            .iter()
            .rev()
            .find_map(|entry| entry.calls.last())
            .expect("a call was made")
    }

    #[test]
    fn a_call_that_runs_the_tests_is_known_as_one_from_its_start() {
        let mut app = app();
        app.apply_at(
            &start(
                "t1",
                "Bash",
                r#"{"command":"cargo test"}"#,
                Some("cargo test"),
            ),
            at(100, 9, 0),
        );
        assert!(last_call(&app).testing);
        assert_eq!(last_call(&app).began(), Some(at(100, 9, 0)));

        app.apply(&start("l1", "Bash", r#"{"command":"ls"}"#, Some("ls")));
        assert!(!last_call(&app).testing, "a listing runs no tests");
    }

    #[test]
    fn a_test_run_carries_how_long_the_last_counted_run_of_its_command_took() {
        let mut app = app();
        test_call(&mut app, "t1", "cargo test", 100, 142, true);
        assert_eq!(last_call(&app).last_run, None, "nothing ran before it");

        // A run whose counts were not read may have stopped anywhere, and is
        // nothing to measure the next one against.
        test_call(&mut app, "t2", "cargo test", 200, 205, false);
        test_call(&mut app, "t3", "cargo test -p niobe-tui", 300, 310, true);
        app.apply_at(
            &start(
                "t4",
                "Bash",
                r#"{"command":"cargo test"}"#,
                Some("cargo test"),
            ),
            at(400, 9, 0),
        );

        assert_eq!(
            last_call(&app).last_run,
            Some(Duration::from_secs(42)),
            "the last counted run of the same command, not of another one"
        );
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    fn send_prompt(app: &mut App, text: &str) {
        typed(app, text);
        app.on_key(key(KeyCode::Enter));
        app.take_produced();
    }

    fn earlier(text: &str, session: &str) -> crate::history::PastPrompt {
        crate::history::PastPrompt {
            text: text.to_owned(),
            at: None,
            session: Target::Recorded(session.to_owned()),
        }
    }

    fn remembering(prompts: &[(&str, &str)], sessions: &[&str]) -> App {
        let mut app = app().remembers();
        assert!(
            app.take_history_request(),
            "the shell asks for its history as it opens"
        );
        app.set_past(crate::history::Past {
            prompts: prompts
                .iter()
                .map(|(text, session)| earlier(text, session))
                .collect(),
            sessions: sessions
                .iter()
                .map(|id| crate::history::PastSession {
                    target: Target::Recorded((*id).to_owned()),
                    last: None,
                    first_prompt: None,
                })
                .collect(),
            unread: None,
        });
        app
    }

    #[test]
    fn up_on_the_first_row_walks_back_through_the_prompts_and_down_brings_the_draft_back() {
        let mut app = remembering(&[("from yesterday", "3")], &["3"]);
        send_prompt(&mut app, "first");
        send_prompt(&mut app, "second");
        typed(&mut app, "half");

        app.on_key(key(KeyCode::Up));
        assert_eq!(app.composed(), "second");
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.composed(), "first");
        app.on_key(key(KeyCode::Up));
        assert_eq!(
            app.composed(),
            "from yesterday",
            "earlier sessions come after this one"
        );
        app.on_key(key(KeyCode::Up));
        assert_eq!(
            app.composed(),
            "from yesterday",
            "the oldest is where the walk stops"
        );

        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.composed(), "second");
        app.on_key(key(KeyCode::Down));
        assert_eq!(
            app.composed(),
            "half",
            "past the newest is the draft, as it was"
        );
        assert!(app.take_produced().is_empty(), "recalling sends nothing");
    }

    #[test]
    fn a_prompt_sent_in_this_session_and_an_earlier_one_is_recalled_once() {
        let mut app = remembering(&[("run the tests", "3"), ("fix it", "3")], &["3"]);
        send_prompt(&mut app, "run the tests");

        app.on_key(key(KeyCode::Up));
        assert_eq!(app.composed(), "run the tests");
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.composed(), "fix it");
    }

    #[test]
    fn up_below_the_first_row_of_the_composer_moves_the_cursor_and_recalls_nothing() {
        let mut app = remembering(&[("earlier", "3")], &["3"]);
        typed(&mut app, "one");
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        typed(&mut app, "two");

        app.on_key(key(KeyCode::Up));
        assert_eq!(app.composed(), "one\ntwo");
        assert_eq!(app.composer().cursor().0, 0);
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.composed(), "earlier", "the first row's Up recalls");
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.composed(), "one\ntwo");
    }

    #[test]
    fn a_recalled_multiline_prompt_walks_on_with_the_next_arrow_either_way() {
        let mut app = remembering(
            &[("newest", "3"), ("two\nlines", "3"), ("oldest", "3")],
            &["3"],
        );
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.composed(), "two\nlines");
        app.on_key(key(KeyCode::Up));
        assert_eq!(
            app.composed(),
            "oldest",
            "the cursor was left on the first row"
        );
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.composed(), "two\nlines");
        app.on_key(key(KeyCode::Down));
        assert_eq!(
            app.composed(),
            "newest",
            "and on the last row going the other way"
        );
    }

    #[test]
    fn ctrl_r_finds_a_prompt_by_its_words_and_puts_it_in_the_composer_unsent() {
        let mut app = remembering(
            &[("fix the resume race", "4"), ("add etag support", "3")],
            &["4", "3"],
        );
        typed(&mut app, "draft");

        app.on_key(ctrl('r'));
        let browser = app.browser().expect("Ctrl+R opens the history");
        assert_eq!(browser.view, View::Prompts);
        typed(&mut app, "ETAG");
        let rows = app.prompt_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "add etag support");
        assert_eq!(rows[0].session, Some(Target::Recorded("3".to_owned())));

        app.on_key(key(KeyCode::Enter));
        assert!(app.browser().is_none());
        assert_eq!(app.composed(), "add etag support");
        assert!(
            app.take_produced().is_empty(),
            "choosing a prompt sends nothing"
        );
    }

    #[test]
    fn esc_closes_the_history_and_leaves_the_composer_as_it_was() {
        let mut app = remembering(&[("earlier", "3")], &["3"]);
        typed(&mut app, "draft");
        app.on_key(ctrl('r'));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Esc));

        assert!(app.browser().is_none());
        assert_eq!(app.composed(), "draft");
    }

    #[test]
    fn the_history_lists_this_sessions_prompts_first_newest_first() {
        let mut app = remembering(&[("earlier", "3")], &["3"]);
        send_prompt(&mut app, "one");
        send_prompt(&mut app, "two");
        app.on_key(ctrl('r'));

        let rows = app.prompt_rows();
        let texts: Vec<&str> = rows.iter().map(|row| row.text.as_str()).collect();
        assert_eq!(texts, ["two", "one", "earlier"]);
        assert_eq!(rows[0].session, None, "this session's own");
    }

    #[test]
    fn the_history_says_it_is_reading_until_the_load_lands() {
        let mut app = app().remembers();
        app.on_key(ctrl('r'));
        assert!(!app.history_read(), "nothing has arrived yet");
        app.set_past(crate::history::Past::default());
        assert!(app.history_read());
    }

    #[test]
    fn opening_the_history_asks_for_it_to_be_read_again() {
        let mut app = remembering(&[], &[]);
        assert!(!app.take_history_request());
        app.on_key(ctrl('r'));
        assert!(
            app.take_history_request(),
            "a session another shell ended is listed"
        );
    }

    #[test]
    fn tab_turns_to_the_sessions_and_enter_opens_the_chosen_one_in_place_of_this() {
        let mut app = remembering(&[("add etag support", "3")], &["3"]);
        app.on_key(ctrl('r'));
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.browser().map(|b| b.view), Some(View::Sessions));

        let rows = app.session_rows();
        assert_eq!(rows[0].target, None, "this session heads the list");
        assert_eq!(rows[1].target, Some(Target::Recorded("3".to_owned())));
        assert_eq!(rows[1].prompts, ["add etag support"]);

        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        assert!(app.should_quit(), "this session ends");
        assert_eq!(app.take_opening(), Some(Target::Recorded("3".to_owned())));
    }

    #[test]
    fn the_session_open_now_is_not_opened_again() {
        let mut app = remembering(&[("one", "5")], &["5"]);
        app.set_recorded_as(Some("5".to_owned()));
        app.perform(crate::menu::Action::Resume);
        assert_eq!(app.browser().map(|b| b.view), Some(View::Sessions));

        let rows = app.session_rows();
        assert_eq!(rows.len(), 1, "its record is this session, not another");
        app.on_key(key(KeyCode::Enter));
        assert!(!app.should_quit());
        assert!(app.hint().is_some_and(|hint| hint.contains("open now")));
    }

    #[test]
    fn another_session_is_not_opened_while_a_turn_runs() {
        let mut app = remembering(&[], &["3"]).attached();
        send_prompt(&mut app, "go");
        assert!(app.working());
        app.perform(crate::menu::Action::Resume);
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));

        assert!(!app.should_quit());
        assert_eq!(app.take_opening(), None);
        assert!(app.hint().is_some_and(|hint| hint.contains("stop")));
    }

    #[test]
    fn a_shell_with_no_record_cannot_resume() {
        let app = app();
        assert!(!app.can(crate::menu::Action::Resume));
        assert!(remembering(&[], &[]).can(crate::menu::Action::Resume));
    }

    #[test]
    fn keys_read_together_while_the_history_is_open_go_to_it_not_the_composer() {
        let mut app = remembering(&[("add etag support", "3"), ("fix it", "3")], &["3"]);
        typed(&mut app, "draft");
        app.on_key(ctrl('r'));

        let keys: Vec<KeyEvent> = "etag\t\r"
            .chars()
            .map(|c| match c {
                '\t' => key(KeyCode::Tab),
                '\r' => key(KeyCode::Enter),
                c => key(KeyCode::Char(c)),
            })
            .collect();
        app.on_keys_read(
            &keys[..4],
            Arrival {
                at: Instant::now(),
                alone: false,
            },
        );
        assert_eq!(app.browser().map(|b| b.query.as_str()), Some("etag"));
        assert_eq!(app.composed(), "draft");
        app.on_keys_read(
            &keys[4..],
            Arrival {
                at: Instant::now(),
                alone: false,
            },
        );
        assert_eq!(
            app.browser().map(|b| b.view),
            None,
            "the Enter chose a session"
        );
        assert_eq!(app.take_opening(), Some(Target::Recorded("3".to_owned())));
    }
}
