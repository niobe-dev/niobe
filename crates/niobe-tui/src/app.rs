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

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant, SystemTime};

use niobe_core::diff::Hunk;
use niobe_core::event::{
    AgentId, AgentOutcome, Backend, Event, Mode, PermissionDecision, ToolCallId, ToolOutcome,
    UsageWindow,
};
use niobe_core::permission::{Allowlist, Rule};
use niobe_core::session::{DecisionRecord, SessionState, TestRunRecord};
use ratatui_textarea::{Input, TextArea, WrapMode};

use ratatui::style::Style;

use crate::clock::{Clock, LocalTime, Stamp};
use crate::prices::Prices;
use crate::theme::{Depth, Theme};

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
    /// Until one has, the working tree and the commits below are empty because
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
    /// The commits made since the session started, newest first.
    pub commits: Vec<Commit>,
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
}

/// A commit made while the session has been running.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Commit {
    /// The hash, shortened the way the repository shortens it.
    pub hash: String,
    /// The first line of the message.
    pub subject: String,
    /// When it was committed, where the repository dated it. The pane draws
    /// how long ago that was; a commit with no date carries no age rather than
    /// one counted from the moment it was read.
    pub at: Option<SystemTime>,
    /// Whether the branch's upstream already has it. A branch with no upstream
    /// has nowhere to have pushed it, so nothing there is pushed; `None` is a
    /// branch that names an upstream the repository cannot find, where the
    /// answer is not known rather than no.
    pub pushed: Option<bool>,
}

/// A pane of the right-hand stack that scrolls and holds folding sections.
///
/// Which pane a section belongs to decides which scroll offset folding it
/// resets and which pane a wheel notch moves, so it is written down once here
/// rather than inferred at each call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// What the session changed: the working tree, the commits, the files.
    Changes,
    /// What the session is doing: its sub-agents, decisions and tools.
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
///
/// Each names one claim about the work, and two of them are claims about the
/// same files made by different parties — which is why they are never one
/// section: [`Section::WorkingTree`] is what the repository measured, and
/// [`Section::Edited`] is what this session's own edit tools reported, a floor
/// where a tool did not say how much it changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    /// What the repository says the working tree has changed.
    WorkingTree,
    /// The commits made since the session started.
    Commits,
    /// What this session's edit tools said they changed.
    Edited,
    /// What the session's latest test run reported.
    Tests,
    /// The sub-agents the session spawned, running and finished.
    SubAgents,
    /// The decisions the session recorded.
    Decisions,
    /// The tools it called, and how often.
    Tools,
}

impl Section {
    /// Every section, in the order the panes draw them.
    pub const ALL: [Section; 7] = [
        Section::WorkingTree,
        Section::Commits,
        Section::Edited,
        Section::Tests,
        Section::SubAgents,
        Section::Decisions,
        Section::Tools,
    ];

    /// The pane this section is drawn in.
    pub fn pane(self) -> Pane {
        match self {
            Section::WorkingTree | Section::Commits | Section::Edited | Section::Tests => {
                Pane::Changes
            }
            Section::SubAgents | Section::Decisions | Section::Tools => Pane::Activity,
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
}

impl Ask {
    /// Whether this prompt can be given `answer`. A standing answer about the
    /// target needs a target to be about.
    pub fn offers(&self, answer: Answer) -> bool {
        answer != Answer::AlwaysTarget || self.target.is_some()
    }

    /// The answers this prompt offers, in the order they are numbered.
    pub fn options(&self) -> Vec<Answer> {
        Answer::ALL
            .into_iter()
            .filter(|answer| self.offers(*answer))
            .collect()
    }

    /// The standing rule "always this tool".
    pub fn tool_rule(&self) -> Rule {
        Rule::tool(self.tool.clone())
    }

    /// The standing rule "always this tool, on this target", where the prompt
    /// has a target to write one about.
    pub fn target_rule(&self) -> Option<Rule> {
        self.target
            .as_ref()
            .map(|target| Rule::targeted(self.tool.clone(), target.clone()))
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

/// The models the operator is choosing between.
///
/// The list is the profile's, in the order it names them: the shell knows no
/// backend and so knows no models of its own, and offering an id the backend
/// would refuse is worse than offering nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    /// The models offered.
    pub models: Vec<String>,
    /// Which one the cursor is on.
    pub at: usize,
}

/// What a transcript entry is, which decides its glyph and its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// The operator.
    User,
    /// The assistant.
    Agent,
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
    /// Every token it spent, cache traffic included.
    pub tokens: u64,
    /// How far the five-hour window moved across it, in whole points of the
    /// window as the Usage pane rounds them. `Some(0)` is a move under one
    /// point, which is not the same as no move at all and is drawn as such.
    pub five_hour_points: Option<u64>,
    /// How long it ran, from the prompt that opened it to its end.
    pub took: Option<Duration>,
}

impl EntryKind {
    /// The glyph in the transcript gutter.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::User => ">",
            Self::Agent => "◆",
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
            Self::Agent => theme.agent,
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
    /// group even where no words came between them.
    pub calls: Vec<Call>,
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
    /// When it started running: its start, or the moment it was allowed
    /// where it waited on a question first, so that the time the operator
    /// took to answer is not read as the time the tool took.
    started: Option<Stamp>,
}

impl Call {
    /// A call that has just started, at `at`.
    fn started(what: String, at: Option<Stamp>) -> Self {
        Self {
            what,
            outcome: None,
            bytes: None,
            exit_code: None,
            error: None,
            lines: None,
            took: None,
            change: None,
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

    /// Whether the call is still running.
    pub fn running(&self) -> bool {
        self.outcome.is_none()
    }

    /// Whether the call ran and did not succeed, or was not allowed to run.
    pub fn failed(&self) -> bool {
        matches!(
            self.outcome,
            Some(ToolOutcome::Failed | ToolOutcome::Denied)
        )
    }
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
}

impl Change {
    /// A change of `hunks`, let through as `gate` says.
    pub fn new(hunks: Vec<Hunk>, gate: Option<Gate>) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        hunks.hash(&mut hasher);
        Self {
            hunks,
            gate,
            fingerprint: hasher.finish(),
        }
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
/// A finished agent is kept rather than dropped. What a session spawned and
/// how it went is the record the operator reads the pane for; a list of only
/// what is running now would erase the failure the moment it mattered most.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubAgent {
    /// The agent, as the backend named it.
    pub id: AgentId,
    /// What it was spawned to do.
    pub label: String,
    /// When it was spawned, where the shell had a clock at the time.
    pub at: Option<Stamp>,
    /// How it finished, or `None` while it is still running.
    pub outcome: Option<AgentOutcome>,
    /// The model its own messages were answered by, where its backend said.
    pub model: Option<String>,
    /// The tokens in its conversation at its latest message, where its
    /// backend counted them. A size, not what it spent.
    pub context_tokens: Option<u64>,
    /// The last thing it was observed doing, in its backend's words.
    pub latest: Option<String>,
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
    profile: Option<SelectedProfile>,
    theme: Theme,
    /// How many colours the terminal draws, which every theme is drawn at.
    depth: Depth,
    /// Whether the desktop moves while a turn is running.
    effects: bool,
    session: SessionState,
    entries: Vec<Entry>,
    /// Where each running call is drawn: its entry, and its place among the
    /// entry's calls.
    tool_entries: BTreeMap<ToolCallId, (usize, usize)>,
    /// The entry a call to the same tool would join, while nothing has come
    /// between it and the next call: see [`Entry::calls`] for what breaks a
    /// run.
    run: Option<usize>,
    /// Whether a run of calls is drawn as its group row alone, rather than
    /// with a row for each call under it.
    calls_folded: bool,
    /// How each call the operator or a rule let through was allowed, until
    /// the call ends and its entry takes the answer.
    answered: BTreeMap<ToolCallId, PermissionDecision>,
    /// The call that ended with the event just folded — its entry and its
    /// place in it — and how it came to run. A backend reports a file change
    /// directly after the end of the call that made it, so this is where the
    /// change is drawn; any other event in between clears it.
    just_ended: Option<(usize, usize, Option<Gate>)>,
    /// Every sub-agent the session spawned, in the order it spawned them. The
    /// session fold keeps the counts and which ids are running; the label, the
    /// moment and the outcome as this pane reads them are this shell's
    /// business, so they are kept here rather than widening the shared state —
    /// and the event model, which carries no time — for a pane.
    agents: Vec<SubAgent>,
    /// When each decision in [`SessionState::decisions`] was recorded, in the
    /// same order, so a pane reads the two together and cannot pair a decision
    /// with another one's time.
    decided_at: Vec<Option<Stamp>>,
    /// When the call that ran [`SessionState::test_run`] finished, which is
    /// what the run's age is counted from. Kept beside it for the reason
    /// [`App::decided_at`] is.
    tested_at: Option<Stamp>,
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
    /// The search through the transcript, while it is open.
    find: Option<Find>,
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
    /// The standing answers this session starts with, plus the ones made in
    /// it.
    allowed: Allowlist,
    /// Rules made here that have not been handed out to be kept.
    learned: Vec<Rule>,
    /// Events the operator produced that have not been handed out to be kept.
    produced: Vec<Event>,
    /// The model list the operator opened, while it is open.
    picking: Option<Picker>,
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
    /// When the running turn was first seen running by that clock.
    working_since: Option<Instant>,
    /// When the session was first seen not running by that clock. The mirror
    /// of `working_since`: exactly one of the two is set once the loop has
    /// ticked, so the state segment always has a duration to show.
    idle_since: Option<Instant>,
    /// Each entry as last drawn, so a redraw re-renders only what changed.
    drawn: crate::ui::DrawnEntries,
    should_quit: bool,
}

/// A search through the transcript, from `/` on an empty composer to Esc.
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

impl App {
    /// An empty session in the given repo.
    pub fn new(repo: Repo) -> Self {
        let theme = Theme::default();
        let mut composer = TextArea::default();
        // A prompt is prose, so it wraps rather than scrolling sideways, and a
        // path or a URL longer than the pane falls back to breaking mid-word.
        composer.set_wrap_mode(WrapMode::WordOrGlyph);
        composer.set_placeholder_text(placeholder(usize::MAX));
        paint_composer(&mut composer, &theme);

        Self {
            repo,
            folded: std::collections::BTreeSet::new(),
            changes: Scroller::default(),
            activity: Scroller::default(),
            focus: Focus::Session,
            session_area: None,
            jump: None,
            profile: None,
            theme,
            depth: Depth::default(),
            effects: true,
            session: SessionState::new(),
            entries: Vec::new(),
            tool_entries: BTreeMap::new(),
            run: None,
            calls_folded: false,
            answered: BTreeMap::new(),
            just_ended: None,
            agents: Vec::new(),
            decided_at: Vec::new(),
            tested_at: None,
            composer,
            scroll: 0,
            follow: true,
            transcript_lines: 0,
            viewport_lines: 0,
            hint: None,
            find: None,
            asks: VecDeque::new(),
            ask_selected: 0,
            ask_focus: AskFocus::Choosing,
            ask_draft: String::new(),
            allowed: Allowlist::new(),
            learned: Vec::new(),
            produced: Vec::new(),
            picking: None,
            budget_usd: None,
            prices: None,
            budget_warned: false,
            attached: false,
            sent_here: false,
            clock: None,
            now: None,
            at: None,
            turn_began_at: None,
            working_since: None,
            idle_since: None,
            drawn: crate::ui::DrawnEntries::default(),
            should_quit: false,
        }
    }

    /// Folds one event in: the session totals the panes read, and the
    /// transcript entry it produces, if it produces one.
    pub fn apply(&mut self, event: &Event) {
        let turns = self.session.turns().len();
        if matches!(event, Event::UserMessage { .. }) && !self.session.turn_running() {
            self.turn_began_at = self.at;
        }
        self.session.apply(event);
        let ended = self.just_ended.take();
        self.fold_into_transcript(event, ended);
        if self.session.turns().len() > turns {
            self.rule_turn();
        }
    }

    /// Draws the rule under the turn the fold has just ended, below whatever
    /// the event that ended it put in the transcript.
    fn rule_turn(&mut self) {
        let began = self.turn_began_at.take();
        let Some(turn) = self.session.turns().last() else {
            return;
        };
        let took = match (began, self.at) {
            (Some(began), Some(ended)) => ended.since(began).filter(|took| *took >= TIMED),
            _ => None,
        };
        let rule = TurnRule {
            number: turn.number,
            ended: self.at.and_then(Stamp::local),
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
        });
    }

    /// Puts what `event` says in the transcript, where it says anything there.
    /// `ended` is the call that ended with the event before it, which a file
    /// change is drawn under.
    fn fold_into_transcript(&mut self, event: &Event, ended: Option<(usize, usize, Option<Gate>)>) {
        match event {
            Event::UserMessage { text } => self.push(Entry {
                kind: EntryKind::User,
                head: "you".to_owned(),
                meta: String::new(),
                body: text.clone(),
                streaming: false,
                at: self.at,
                calls: Vec::new(),
            }),

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
                    });
                }
            },

            Event::AssistantMessage { text } => match self.streaming_agent_entry() {
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
                    });
                }
            },

            Event::ToolCallStart {
                id,
                name,
                input,
                summary,
            } => {
                let head = tool_label(name);
                let call = Call::started(what_it_does(summary.as_deref(), input), self.at);
                match self.open_run(&head) {
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
                        self.push(Entry {
                            kind: EntryKind::Tool,
                            head,
                            meta: String::new(),
                            body: String::new(),
                            streaming: true,
                            at: self.at,
                            calls: vec![call],
                        });
                        self.run = Some(at);
                    }
                }
            }

            Event::ToolCallEnd {
                id,
                name,
                input,
                bytes,
                outcome,
                summary,
                exit_code,
                error,
                ..
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
                        });
                        Some((self.entries.len().saturating_sub(1), 0))
                    }
                };
                if *outcome == ToolOutcome::Ok
                    && let Some((at, index)) = ended
                {
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
            Event::TurnEnded => self.run = None,

            Event::Error { message, fatal } => self.push(Entry {
                kind: EntryKind::Failure,
                head: if *fatal { "fatal" } else { "error" }.to_owned(),
                meta: String::new(),
                body: message.clone(),
                streaming: false,
                at: self.at,
                calls: Vec::new(),
            }),

            Event::Notice { message } => self.push(Entry {
                kind: EntryKind::Notice,
                head: self.agent_name(),
                meta: String::new(),
                body: message.clone(),
                streaming: false,
                at: self.at,
                calls: Vec::new(),
            }),

            Event::PermissionRequest {
                id,
                tool,
                input,
                target,
            } => self.asks.push_back(Ask {
                id: id.clone(),
                tool: tool.clone(),
                input: input.clone(),
                target: target.clone(),
                turn: self.session.user_messages(),
            }),

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
                    let (tool, what) = match &asked {
                        Some(ask) => (
                            tool_label(&ask.tool),
                            ask.target.clone().unwrap_or_else(|| one_line(&ask.input)),
                        ),
                        None => (id.to_string(), String::new()),
                    };
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
            | Event::Checkpoint { .. } => {}

            // The decision itself is in the session fold, which carries no
            // times; when it was made is kept here, beside it.
            Event::Decision { .. } => self.decided_at.push(self.at),

            // Straight after its call's end, so in the same tick and at the
            // same clock: the moment the call finished.
            Event::TestRun { .. } => self.tested_at = self.at,

            Event::AgentSpawn { id, label, .. } => {
                let spawned = SubAgent {
                    id: id.clone(),
                    label: label.clone(),
                    at: self.at,
                    outcome: None,
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

    /// The entry a call to `head` joins, where the transcript's last entry is
    /// a run of calls to it that nothing has broken.
    fn open_run(&self, head: &str) -> Option<usize> {
        self.run.filter(|&at| {
            at + 1 == self.entries.len()
                && self
                    .entries
                    .get(at)
                    .is_some_and(|entry| entry.head == head && !entry.calls.is_empty())
        })
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
        self.calls_folded = !self.calls_folded;
    }

    /// Folds in an event the operator produced here and queues it to be kept.
    fn produce(&mut self, event: Event) {
        self.apply(&event);
        self.produced.push(event);
    }

    /// The events the operator produced since the last call, oldest first.
    ///
    /// Events folded in through [`App::apply`] came from somewhere that already
    /// has them, and are never handed out.
    pub fn take_produced(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.produced)
    }

    /// Drops a prompt from the queue, and gives it back.
    ///
    /// The next prompt starts afresh — on its first option, with the keyboard
    /// on it — whatever the operator was doing with the one before, so a
    /// question put off is not mistaken for the one behind it.
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

    fn forget_ask(&mut self, id: &ToolCallId) -> Option<Ask> {
        let at = self.asks.iter().position(|ask| &ask.id == id)?;
        if at == 0 {
            self.ask_selected = 0;
            self.ask_focus = AskFocus::Choosing;
            self.ask_draft.clear();
        }
        self.asks.remove(at)
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
            Answer::AlwaysTool => Some(ask.tool_rule()),
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
        });
        self.scroll_to_tail();
    }

    /// Says in the transcript that an event could not be kept, so the operator
    /// does not find out from a resumed session that is missing it.
    pub fn not_kept(&mut self, error: &str) {
        self.push(Entry {
            kind: EntryKind::Failure,
            head: "not saved".to_owned(),
            meta: String::new(),
            body: format!(
                "What you just did is on screen but not in the session store, so a \
                 resumed session will not show it: {error}"
            ),
            streaming: false,
            at: self.at,
            calls: Vec::new(),
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
    pub fn moment(&self, seconds: u64) -> Stamp {
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
        match &self.clock {
            Some(clock) => clock.at(at),
            None => Stamp::new(at, None),
        }
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
        let spent = self.session.totals().reported_cost_usd;
        if spent < budget * BUDGET_WARNING {
            return;
        }

        self.budget_warned = true;
        self.push(Entry {
            kind: EntryKind::Notice,
            head: "budget".to_owned(),
            meta: format!("${spent:.2} of ${budget:.2}"),
            body: "Most of this session's budget is spent. The backend stops the session \
                   when the budget is reached, and it checks between turns rather than \
                   inside one, so the session can finish above the figure by what the turn \
                   that crosses the line costs."
                .to_owned(),
            streaming: false,
            at: self.at,
            calls: Vec::new(),
        });
        self.scroll_to_tail();
    }

    /// The model list the operator opened, while it is open.
    pub fn picking(&self) -> Option<&Picker> {
        self.picking.as_ref()
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
                "F8 Model — this profile names no models; add `models = [\"…\"]` to it in \
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
        self.picking = Some(Picker { models, at });
    }

    /// One key, while the model list is up.
    ///
    /// Anything that is not a move, a choice or a way out is swallowed: the
    /// list is a question, and a key that typed into the composer behind it
    /// would be a key nobody meant.
    fn on_pick_key(&mut self, key: ratatui::crossterm::event::KeyEvent) {
        use ratatui::crossterm::event::KeyCode;

        let Some(picker) = self.picking.as_mut() else {
            return;
        };
        let last = picker.models.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => picker.at = picker.at.saturating_sub(1),
            KeyCode::Down => picker.at = picker.at.saturating_add(1).min(last),
            KeyCode::Esc | KeyCode::F(8) => self.picking = None,
            KeyCode::Enter => {
                let model = picker.models.get(picker.at).cloned();
                self.picking = None;
                if let Some(model) = model {
                    self.produce(Event::ModelSelected { model });
                }
            }
            _ => {}
        }
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
        match self.entries.last_mut() {
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
    /// the one in force and each `F9` moves to, is drawn at it.
    #[must_use]
    pub fn with_depth(mut self, depth: Depth) -> Self {
        self.depth = depth;
        self.set_theme(self.theme);
        self
    }

    /// The same shell with the desktop's motion on or off.
    ///
    /// Off, the desktop is left empty while a turn runs and no frame of the
    /// motion is worked out at all; the spinner under the transcript still
    /// says the turn is going.
    #[must_use]
    pub fn with_effects(mut self, on: bool) -> Self {
        self.effects = on;
        self
    }

    /// Moves to the next theme, which is what `F9` does.
    fn cycle_theme(&mut self) {
        self.set_theme(self.theme.next());
    }

    fn set_theme(&mut self, theme: Theme) {
        let theme = theme.at(self.depth);
        self.theme = theme;
        paint_composer(&mut self.composer, &theme);
        if let Some(find) = self.find.as_mut() {
            paint_composer(&mut find.query, &theme);
        }
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

    /// Whether the desktop moves while a turn is running.
    pub fn effects(&self) -> bool {
        self.effects
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

    /// The transcript, oldest first.
    /// Each decision the session recorded, with the moment it was recorded at.
    ///
    /// The two are handed out together because they are kept apart: the
    /// decision is in the session fold, which has no times, and the time is
    /// here. A pane that paired them by index could pair the wrong two.
    pub fn decisions(&self) -> impl Iterator<Item = (&DecisionRecord, Option<Stamp>)> {
        // Padded rather than zipped short: the two grow together in the same
        // fold and cannot fall out of step, and if they ever did, a decision
        // that lost its time should still be on screen without one.
        let times = self
            .decided_at
            .iter()
            .copied()
            .chain(std::iter::repeat(None));
        self.session.decisions().iter().zip(times)
    }

    /// The session's latest test run, with the moment its call finished where
    /// the shell had a clock at the time. `None` where the session has run no
    /// tests.
    pub fn test_run(&self) -> Option<(TestRunRecord, Option<Stamp>)> {
        self.session.test_run().map(|run| (run, self.tested_at))
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

    /// The transient message under the transcript, if there is one.
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    /// Fits the empty composer's placeholder into `columns`, dropping what it
    /// advertises before what the bar says beside it: the mode a session is in
    /// matters more than a reminder of a key.
    pub(crate) fn fit_placeholder(&mut self, columns: usize) {
        let said = placeholder(columns);
        if self.composer.placeholder_text() != said {
            self.composer.set_placeholder_text(said);
        }
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

    /// One key, while a search is open. Returns whether the search took it.
    ///
    /// The F-keys, Shift+Tab and Ctrl+O still do what they do everywhere;
    /// every other key is the search's, so nothing typed at it lands in the
    /// composer behind it. The key that opened it, pressed again on an empty
    /// query, closes it and types itself: that is how a prompt starts with a
    /// literal `/`.
    fn on_find_key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> bool {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};

        let Some(find) = self.find.as_mut() else {
            return false;
        };
        let empty = find.query.is_empty();
        match (key.code, key.modifiers) {
            (KeyCode::F(_) | KeyCode::BackTab, _) | (KeyCode::Char('o'), KeyModifiers::CONTROL) => {
                return false;
            }
            (KeyCode::Esc, _) => self.close_find(),
            (KeyCode::Backspace, _) if empty => self.close_find(),
            (KeyCode::Char('/'), KeyModifiers::NONE | KeyModifiers::SHIFT) if empty => {
                self.close_find();
                self.composer.insert_char('/');
            }
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
    /// the way back down returns the transcript to its newest line. Nothing
    /// else the mouse does means anything to the shell yet.
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
            MouseEventKind::Down(MouseButton::Left) => {
                let jump = self
                    .jump
                    .is_some_and(|at| at.contains((mouse.column, mouse.row).into()));
                if jump {
                    self.focus = Focus::Session;
                    self.scroll_to_tail();
                } else if let Some(focus) = over {
                    self.focus = focus;
                }
            }
            _ => {}
        }
    }

    /// Handles one key.
    ///
    /// The shell's own bindings are taken first and everything left over goes
    /// to the composer, so typing an `f` is typing an `f` even though `F1` is a
    /// menu.
    pub fn on_key(&mut self, key: ratatui::crossterm::event::KeyEvent) {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};

        self.hint = None;

        // Quitting is always available: a session with a prompt up is still a
        // session the operator may need to leave, and the backend is told the
        // same way it is told about any other way out.
        if let (KeyCode::F(10), _) | (KeyCode::Char('q' | 'c'), KeyModifiers::CONTROL) =
            (key.code, key.modifiers)
        {
            self.quit();
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
                AskFocus::Deferred
                    if key.code == KeyCode::Enter && key.modifiers != KeyModifiers::ALT =>
                {
                    self.hint = Some(
                        "The turn is still waiting on the question above; Esc to answer it first"
                            .to_owned(),
                    );
                    return;
                }
                AskFocus::Deferred => {}
            }
        }
        // The model list takes the keyboard the same way, and for the same
        // reason: an arrow key that scrolled the transcript behind an open
        // list would move something the operator was not looking at.
        if self.picking().is_some() {
            self.on_pick_key(key);
            return;
        }

        if self.scroll_key(key) {
            return;
        }
        if self.on_find_key(key) {
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
            // Alt+Enter opens a line; Enter sends. The other way round would
            // make the common action the awkward one.
            (KeyCode::Enter, KeyModifiers::ALT) => {
                self.focus = Focus::Session;
                self.composer.insert_newline();
            }
            (KeyCode::Enter, _) => self.submit(),
            (KeyCode::Char('o'), KeyModifiers::CONTROL) => self.fold_calls(),

            // Shift+Tab reaches crossterm as its own code rather than as Tab
            // with a modifier, which is why it is matched on the code alone.
            (KeyCode::BackTab, _) => self.cycle_mode(),
            // F5 has no cost breakdown behind it, but on a flat-rate plan the
            // question it is pressed for is when the windows come back, and
            // the session fold knows that.
            (KeyCode::F(5), _) => self.hint = Some(self.cost_hint(self.read_at())),
            // The two sections the F-key bar already names fold from anywhere;
            // every section, these two included, also folds under the cursor
            // of the pane that has the keyboard.
            (KeyCode::F(6), _) => self.fold(Section::WorkingTree),
            (KeyCode::F(7), _) => self.fold(Section::Tools),
            (KeyCode::F(8), _) => self.pick_model(),
            (KeyCode::F(9), _) => self.cycle_theme(),
            (KeyCode::F(n), _) => self.hint = Some(fkey_hint(n).to_owned()),

            // `/` searches the transcript where it would start a prompt: in
            // a composer with anything in it, it is a slash.
            (KeyCode::Char('/'), KeyModifiers::NONE | KeyModifiers::SHIFT)
                if self.composer.is_empty() =>
            {
                self.focus = Focus::Session;
                self.open_find();
            }
            // What was typed goes where typing always goes, and the keyboard
            // follows it back: the Enter after it has to send it.
            _ => {
                self.focus = Focus::Session;
                self.composer.input(Input::from(key));
            }
        }
    }

    /// One key, while a prompt has the keyboard.
    ///
    /// Scrolling still scrolls, because the work that led to the question is
    /// what the operator reads to answer it. Anything else that is not about
    /// the question is swallowed rather than passed on: a key that did
    /// something else while a question was being asked would be a key nobody
    /// meant.
    fn on_ask_key(&mut self, key: ratatui::crossterm::event::KeyEvent) {
        if self.scroll_key(key) {
            return;
        }
        // The question is the last thing in the transcript. Scrolled back,
        // the operator cannot see it, and a key that answered it then would
        // answer something they were not reading; the first such key brings
        // it into view and does nothing else.
        if !self.follow {
            self.scroll_to_tail();
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
                self.ask_draft.pop();
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
            });
        }
        self.scroll_to_tail();
    }

    /// Says in the transcript that a turn never reached the backend, so that a
    /// prompt with no reply is not read as a backend thinking about it.
    pub fn not_sent(&mut self, error: &str) {
        self.sent_here = false;
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
        });
        self.scroll_to_tail();
    }

    /// What F5 says: the plan's windows and when they come back, where a
    /// backend has reported them, and otherwise that the breakdown behind the
    /// key is not implemented yet.
    ///
    /// `now` is seconds since the Unix epoch, taken by the caller so that the
    /// wording can be asserted against a fixed clock.
    pub fn cost_hint(&self, now: u64) -> String {
        let Some(windows) = self.session.usage_windows() else {
            return "F5 Usage — the usage breakdown is not implemented yet".to_owned();
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
        format!("F5 Usage — {}", parts.join(" · "))
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

/// Now, in seconds since the Unix epoch. Zero on a clock set before it, which
/// makes every reset read as still to come rather than panicking on a machine
/// whose clock is wrong.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// One window, as F5 reads it out: `5h window 62%, resets in 2h 14m`.
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
const PLACEHOLDER: [&str; 2] = ["Ask for a change", "/ search transcript"];

/// The placeholder in at most `columns` columns: as many of the things the
/// composer does as fit whole, and what it is for however narrow it is.
fn placeholder(columns: usize) -> String {
    let mut said = PLACEHOLDER[0].to_owned();
    for more in &PLACEHOLDER[1..] {
        let longer = format!("{said} · {more}");
        if crate::text::width(&longer) > columns {
            break;
        }
        said = longer;
    }
    said
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

/// What an F-key does, for the ones that do nothing yet.
///
/// F5, F8, F9 and F10 are handled before this is reached, so nothing here
/// names them.
fn fkey_hint(n: u8) -> &'static str {
    match n {
        1 => {
            "F1 Help — the help browser is not implemented yet. Tab or a click moves the \
             keyboard between the panes; the wheel and PgUp/PgDn scroll the one that has it, \
             and in the right-hand panes ↑↓ and Enter fold a section; Ctrl+End returns to the \
             newest line; Ctrl+O folds runs of tool calls; Shift- or Option-drag selects text"
        }
        2 => {
            "F2 Plan — the plan view is not implemented yet; Shift+Tab puts the \
              session in plan mode"
        }
        3 => "F3 Diff — the diff viewer is not implemented yet",
        4 => "F4 Undo — checkpoints and rewind are not implemented yet",
        // F8 opens the model list and F9 changes the theme rather than saying
        // anything, so nothing here names them.
        _ => "F10 Quit",
    }
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
    input.split_whitespace().collect::<Vec<_>>().join(" ")
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

        app.fold(Section::Commits);
        assert!(app.folded(Section::Commits));
        assert!(!app.folded(Section::WorkingTree), "one section, not all");

        app.fold(Section::Commits);
        assert!(!app.folded(Section::Commits));
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
            Pane::Changes,
            vec![
                (Section::WorkingTree, 1),
                (Section::Commits, 12),
                (Section::Edited, 25),
            ],
        );
        assert_eq!(app.section_cursor(Pane::Changes), None, "not focused");

        app.on_key(key(KeyCode::Tab));
        assert_eq!(
            app.section_cursor(Pane::Changes),
            Some(Section::WorkingTree)
        );

        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.section_cursor(Pane::Changes), Some(Section::Edited));
        assert_eq!(
            app.pane_scroll(Pane::Changes),
            16,
            "the section under the cursor is scrolled into view"
        );
        app.on_key(key(KeyCode::Down));
        assert_eq!(
            app.section_cursor(Pane::Changes),
            Some(Section::Edited),
            "the last section is as far as it goes"
        );

        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Enter));
        assert!(app.folded(Section::Commits));
        assert!(app.entries().is_empty(), "Enter folded rather than sent");
        assert!(
            app.pane_scroll(Pane::Changes) <= 12,
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
        });
        assert_eq!(app.entries().len(), 1);
        assert_eq!(app.entries()[0].body, "Reading fetch.ts and its callers.");
        assert!(!app.entries()[0].streaming);
    }

    #[test]
    fn a_tool_call_fills_in_its_own_entry_when_it_ends() {
        let mut app = app();
        app.apply(&Event::ToolCallStart {
            id: "t1".into(),
            name: "Read".to_owned(),
            input: "catalog/fetch.ts".to_owned(),
            summary: None,
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
        app.apply(&Event::Decision {
            summary: "Reuse the existing LRU".to_owned(),
            rationale: None,
            rejected: vec!["A second Map".to_owned()],
        });
        app.apply(&Event::AgentSpawn {
            id: "a1".into(),
            parent: None,
            label: "test-writer".to_owned(),
        });

        assert!(app.entries().is_empty());
        assert_eq!(app.session().decisions().len(), 1);
        assert_eq!(app.session().agents_spawned(), 1);
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
        })
    }

    /// A prompt as the bridge produces one.
    fn prompt(target: Option<&str>) -> Event {
        Event::PermissionRequest {
            id: "t1".into(),
            tool: "Bash".to_owned(),
            input: r#"{"command":"rm -rf build"}"#.to_owned(),
            target: target.map(str::to_owned),
        }
    }

    fn key(code: ratatui::crossterm::event::KeyCode) -> ratatui::crossterm::event::KeyEvent {
        ratatui::crossterm::event::KeyEvent::new(
            code,
            ratatui::crossterm::event::KeyModifiers::NONE,
        )
    }

    #[test]
    fn f9_moves_to_the_next_theme_and_takes_the_composer_with_it() {
        use crate::theme::{CLASSIC, CYBER, THEMES};
        use ratatui::crossterm::event::KeyCode;

        let mut app = app();
        assert_eq!(*app.theme(), CYBER);

        app.on_key(key(KeyCode::F(9)));

        assert_eq!(*app.theme(), CLASSIC);
        // The composer keeps the styles it was given rather than being handed
        // them per frame, so a theme that did not reach it would leave the
        // prompt drawn in the one before.
        assert_eq!(
            app.composer().style(),
            Style::new().fg(CLASSIC.fg).bg(CLASSIC.pane_bg)
        );
        // And it says nothing: the menu bar already names the theme in force.
        assert_eq!(app.hint(), None);

        for _ in 1..THEMES.len() {
            app.on_key(key(KeyCode::F(9)));
        }
        assert_eq!(*app.theme(), CYBER);
    }

    #[test]
    fn a_shell_on_a_deep_terminal_draws_every_theme_at_that_depth() {
        use crate::theme::{CLASSIC, CYBER_TRUE, Depth, MODERN_TRUE, NEO, NEO_TRUE};
        use ratatui::crossterm::event::KeyCode;

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

        app.on_key(key(KeyCode::F(9)));
        assert_eq!(*app.theme(), MODERN_TRUE);
        app.on_key(key(KeyCode::F(9)));
        assert_eq!(*app.theme(), CYBER_TRUE);
        app.on_key(key(KeyCode::F(9)));
        assert_eq!(*app.theme(), CLASSIC);
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
    fn prompts_are_answered_in_the_order_they_arrived() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = app();
        app.apply(&prompt(Some("rm -rf build")));
        app.apply(&Event::PermissionRequest {
            id: "t2".into(),
            tool: "Write".to_owned(),
            input: r#"{"file_path":"/repo/a.rs"}"#.to_owned(),
            target: Some("/repo/a.rs".to_owned()),
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

    #[test]
    fn a_standing_rule_answers_a_prompt_without_showing_it() {
        let mut app = app().with_rules([Rule::tool("Read")].into_iter().collect());
        app.apply(&Event::PermissionRequest {
            id: "t1".into(),
            tool: "Read".to_owned(),
            input: r#"{"file_path":"/repo/a.rs"}"#.to_owned(),
            target: Some("/repo/a.rs".to_owned()),
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
    fn f8_offers_the_models_the_profile_names_and_picking_one_asks_for_it() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = under_a_profile(&["opus", "sonnet", "haiku"]);

        app.on_key(key(KeyCode::F(8)));
        let picker = app.picking().expect("the model list is on screen");
        assert_eq!(picker.models, ["opus", "sonnet", "haiku"]);
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
        app.on_key(key(KeyCode::F(8)));

        app.on_key(key(KeyCode::Esc));

        assert!(app.picking().is_none());
        assert!(app.take_produced().is_empty());
    }

    #[test]
    fn f8_under_a_profile_that_names_no_model_says_so_rather_than_opening_an_empty_list() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = under_a_profile(&[]);

        app.on_key(key(KeyCode::F(8)));

        assert!(app.picking().is_none());
        let hint = app.hint().unwrap_or_default();
        assert!(hint.contains("models"), "{hint}");
    }

    #[test]
    fn a_prompt_keeps_the_keyboard_from_the_model_list() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = under_a_profile(&["opus"]);
        app.apply(&prompt(Some("rm -rf build")));

        app.on_key(key(KeyCode::F(8)));

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
    fn a_session_with_no_budget_is_never_warned_about_one() {
        let mut app = app();
        app.apply(&priced(99.0));
        app.settle_budget();

        assert!(app.entries().iter().all(|entry| entry.head != "budget"));
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
    fn f5_reads_out_both_windows_and_when_each_comes_back() {
        const NOW: u64 = 1_789_000_000;

        let mut app = app();
        app.apply(&Event::UsageWindows(windows(NOW)));

        assert_eq!(
            app.cost_hint(NOW),
            "F5 Usage — 5h window 33%, resets in 1h 30m · 7d window 23%, resets in 4d 6h"
        );
    }

    #[test]
    fn f5_on_a_session_with_no_windows_says_what_the_key_does_not_do_yet() {
        let mut app = app();
        assert!(
            app.cost_hint(0)
                .contains("the usage breakdown is not implemented yet")
        );

        // Pressing it is what puts the line under the transcript.
        app.apply(&Event::UsageWindows(windows(1_789_000_000)));
        app.on_key(key(KeyCode::F(5)));
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
            "F5 Usage — 5h window 90%, already reset · 7d window 40%, no reset time \
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
            .moment()
            .expect("a fixed clock always names a zone");

        assert_eq!(moment.day(), 1);
        assert_eq!(moment.time().to_string(), "00:30");
    }

    #[test]
    fn a_moment_read_without_a_clock_has_no_time_of_day_at_all() {
        let app = App::new(Repo::default());
        assert_eq!(app.moment(23 * 3_600).moment(), None);
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
                tokens: 4_100,
                five_hour_points: None,
                took: Some(Duration::from_secs(38)),
            }
        );
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
            (900, Some(Duration::from_secs(12)))
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
        assert_eq!((rule.ended, rule.took, rule.tokens), (None, None, 300));
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
    fn the_latest_test_run_is_kept_with_the_moment_its_call_finished() {
        let run = |id: &str, passed: u64| Event::TestRun {
            id: ToolCallId::new(id),
            counts: Some(niobe_core::TestCounts {
                passed,
                failed: 0,
                ignored: 0,
                suites: 1,
            }),
            exit_code: Some(0),
            failed: false,
        };
        let mut app = app();
        assert_eq!(app.test_run(), None, "no run is not a run of nothing");

        app.apply_at(&run("t1", 3), at(1_000, 9, 30));
        app.apply_at(&run("t2", 5), at(1_060, 9, 31));

        let (latest, finished) = app.test_run().expect("the session ran its tests");
        assert_eq!(latest.counts.map(|counts| counts.passed), Some(5));
        assert_eq!(finished, Some(at(1_060, 9, 31)));
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
            },
        ]);

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
    fn a_decision_is_handed_out_with_the_time_it_was_made() {
        let mut app = app();
        app.tick(Instant::now(), Some(at(49_260, 13, 41)));
        app.apply(&Event::Decision {
            summary: "Reuse the existing cache".to_owned(),
            rationale: None,
            rejected: Vec::new(),
        });
        app.tick(Instant::now(), Some(at(50_580, 14, 3)));
        app.apply(&Event::Decision {
            summary: "Do not widen the layering table".to_owned(),
            rationale: None,
            rejected: Vec::new(),
        });

        let made: Vec<_> = app
            .decisions()
            .map(|(decision, at)| (decision.summary.as_str(), at.and_then(Stamp::local)))
            .collect();
        assert_eq!(
            made,
            [
                ("Reuse the existing cache", LocalTime::new(13, 41)),
                ("Do not widen the layering table", LocalTime::new(14, 3)),
            ]
        );
    }

    #[test]
    fn a_decision_folded_in_without_a_clock_is_still_paired_with_its_own_row() {
        let mut app = app();
        app.apply(&Event::Decision {
            summary: "made before the loop ticked".to_owned(),
            rationale: None,
            rejected: Vec::new(),
        });

        let made: Vec<_> = app.decisions().collect();
        assert_eq!(made.len(), 1);
        assert_eq!(made[0].0.summary, "made before the loop ticked");
        assert_eq!(made[0].1, None);
    }

    #[test]
    fn a_running_sub_agent_carries_the_moment_it_was_spawned() {
        let mut app = app();
        app.tick(Instant::now(), Some(at(50_700, 14, 5)));
        app.apply(&Event::AgentSpawn {
            id: AgentId::new("a1"),
            parent: None,
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
        });
        app.apply(&Event::FileChange {
            path: "a.rs".to_owned(),
            added: Some(1),
            removed: Some(1),
            hunks: one_hunk(),
        });

        assert!(
            app.entries()
                .iter()
                .flat_map(|entry| &entry.calls)
                .all(|call| call.change.is_none() && call.lines.is_none())
        );
    }

    fn answered(app: &mut App, decision: PermissionDecision) {
        app.apply(&Event::PermissionRequest {
            id: "t1".into(),
            tool: "Edit".to_owned(),
            input: r#"{"file_path":"/repo/a.rs"}"#.to_owned(),
            target: Some("/repo/a.rs".to_owned()),
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
        Stamp::new(SystemTime::UNIX_EPOCH + Duration::from_millis(ms), None)
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
}
