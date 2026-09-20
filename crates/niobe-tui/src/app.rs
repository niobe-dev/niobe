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
use std::time::{Duration, Instant};

use niobe_core::event::{
    AgentId, Backend, Event, Mode, PermissionDecision, ToolCallId, ToolOutcome, UsageWindow,
};
use niobe_core::permission::{Allowlist, Rule};
use niobe_core::session::{DecisionRecord, SessionState};
use ratatui_textarea::{Input, TextArea, WrapMode};

use ratatui::style::Style;

use crate::clock::{Clock, LocalTime, Stamp};
use crate::prices::Prices;
use crate::theme::Theme;

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
    /// Whether the branch's upstream already has it. A branch with no upstream
    /// has nowhere to have pushed it, so nothing there is pushed; `None` is a
    /// branch that names an upstream the repository cannot find, where the
    /// answer is not known rather than no.
    pub pushed: Option<bool>,
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
/// What the backend said, and nothing derived: the modal shows the tool, what
/// it would run on and the whole of its arguments, because approving a call
/// whose arguments were summarised away is approving something else.
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
}

impl Ask {
    /// Whether this prompt can be given `answer`. A standing answer about the
    /// target needs a target to be about.
    pub fn offers(&self, answer: Answer) -> bool {
        answer != Answer::AlwaysTarget || self.target.is_some()
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

/// What the operator chose in the permission modal.
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
    /// Every answer, in the order the dialog's buttons are laid out and Tab
    /// walks them.
    pub const ALL: [Answer; 4] = [
        Answer::Once,
        Answer::AlwaysTool,
        Answer::AlwaysTarget,
        Answer::No,
    ];
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
        }
    }

    /// The colour the head and glyph are drawn in.
    pub fn colour(self, theme: &Theme) -> ratatui::style::Color {
        match self {
            Self::User => theme.user,
            Self::Agent => theme.agent,
            Self::Tool => theme.tool,
            Self::Failure => theme.del,
            Self::Notice => theme.dim,
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
    /// The dim detail beside the head: a file, a byte count, an outcome.
    pub meta: String,
    /// The body, wrapped at draw time.
    pub body: String,
    /// Whether more of this entry is still arriving.
    pub streaming: bool,
    /// When the event behind this entry happened: read off the clock as the
    /// shell took it off the channel, or off what the store recorded beside
    /// it. Absent where the entry came from a log that kept no times.
    pub at: Option<Stamp>,
}

/// A sub-agent as this shell saw it start: what it was spawned to do, and
/// when.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Spawned {
    label: String,
    at: Option<Stamp>,
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
    profile: Option<SelectedProfile>,
    theme: Theme,
    session: SessionState,
    entries: Vec<Entry>,
    tool_entries: BTreeMap<ToolCallId, usize>,
    /// What each sub-agent was spawned to do, and when it was spawned. The
    /// session fold keeps the counts and the ids; the label and the moment are
    /// this shell's business, so they are kept here rather than widening the
    /// shared state — and the event model, which carries no time — for a pane.
    agents: BTreeMap<AgentId, Spawned>,
    /// When each decision in [`SessionState::decisions`] was recorded, in the
    /// same order, so a pane reads the two together and cannot pair a decision
    /// with another one's time.
    decided_at: Vec<Option<Stamp>>,
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
    /// Prompts waiting on the operator, oldest first. The modal shows the
    /// front one; the rest wait behind it, because a backend can gate two
    /// calls of the same turn and answering them out of order would put the
    /// wrong arguments in front of the operator.
    asks: VecDeque<Ask>,
    /// Which of [`Answer::ALL`] the front prompt's Enter would give. Every
    /// prompt starts on the first, so Enter means the same thing whichever
    /// prompt it lands on.
    ask_focus: usize,
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
        composer.set_placeholder_text("Ask for a change, or press F1 for help");
        paint_composer(&mut composer, &theme);

        Self {
            repo,
            profile: None,
            theme,
            session: SessionState::new(),
            entries: Vec::new(),
            tool_entries: BTreeMap::new(),
            agents: BTreeMap::new(),
            decided_at: Vec::new(),
            composer,
            scroll: 0,
            follow: true,
            transcript_lines: 0,
            viewport_lines: 0,
            hint: None,
            asks: VecDeque::new(),
            ask_focus: 0,
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
            working_since: None,
            idle_since: None,
            drawn: crate::ui::DrawnEntries::default(),
            should_quit: false,
        }
    }

    /// Folds one event in: the session totals the panes read, and the
    /// transcript entry it produces, if it produces one.
    pub fn apply(&mut self, event: &Event) {
        self.session.apply(event);

        match event {
            Event::UserMessage { text } => self.push(Entry {
                kind: EntryKind::User,
                head: "you".to_owned(),
                meta: String::new(),
                body: text.clone(),
                streaming: false,
                at: self.at,
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
                    });
                }
            },

            Event::ToolCallStart {
                id,
                name,
                input,
                summary,
            } => {
                self.tool_entries.insert(id.clone(), self.entries.len());
                self.push(Entry {
                    kind: EntryKind::Tool,
                    head: tool_label(name),
                    meta: what_it_does(summary.as_deref(), input),
                    body: String::new(),
                    streaming: true,
                    at: self.at,
                });
            }

            Event::ToolCallEnd {
                id,
                name,
                input,
                bytes,
                outcome,
                summary,
                ..
            } => {
                let meta = format!(
                    "{} · {}",
                    what_it_does(summary.as_deref(), input),
                    outcome_label(*outcome, *bytes)
                );
                match self
                    .tool_entries
                    .remove(id)
                    .and_then(|i| self.entries.get_mut(i))
                {
                    Some(entry) => {
                        entry.meta = meta;
                        entry.streaming = false;
                        if *outcome != ToolOutcome::Ok {
                            entry.kind = EntryKind::Failure;
                        }
                    }
                    // An end whose start never arrived is shown, not dropped:
                    // the session fold counts it too, and a gap the operator
                    // cannot see is a gap nobody reports.
                    None => self.push(Entry {
                        kind: if *outcome == ToolOutcome::Ok {
                            EntryKind::Tool
                        } else {
                            EntryKind::Failure
                        },
                        head: tool_label(name),
                        meta,
                        body: String::new(),
                        streaming: false,
                        at: self.at,
                    }),
                }
            }

            // The working line reads the end off the fold; the transcript has
            // already shown everything the turn said.
            Event::TurnEnded => {}

            Event::Error { message, fatal } => self.push(Entry {
                kind: EntryKind::Failure,
                head: if *fatal { "fatal" } else { "error" }.to_owned(),
                meta: String::new(),
                body: message.clone(),
                streaming: false,
                at: self.at,
            }),

            Event::Notice { message } => self.push(Entry {
                kind: EntryKind::Notice,
                head: self.agent_name(),
                meta: String::new(),
                body: message.clone(),
                streaming: false,
                at: self.at,
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
            }),

            // A refusal is shown as its own entry rather than left to the tool
            // result that carries it back: a backend that reports nothing
            // further about a call it was not allowed to make would leave a
            // denial indistinguishable from the agent deciding not to act.
            Event::PermissionResponse { id, decision } => {
                let asked = self.forget_ask(id);
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
                        body: String::new(),
                        streaming: false,
                        at: self.at,
                    });
                }
            }

            // Everything else is a number or a list a pane reads off the
            // session fold, not a line in the transcript. The mode and the
            // model are in the menu row, which is where a session says what
            // it is running as; a file's counts are in the changes pane, and
            // repeating them under the call that made them would say the same
            // thing twice in the place with the least room for it.
            Event::SessionMeta(_)
            | Event::Usage(_)
            | Event::ModeSelected { .. }
            | Event::ModelSelected { .. }
            | Event::UsageWindows(_)
            | Event::FileChange { .. }
            | Event::Checkpoint { .. } => {}

            // The decision itself is in the session fold, which carries no
            // times; when it was made is kept here, beside it.
            Event::Decision { .. } => self.decided_at.push(self.at),

            Event::AgentSpawn { id, label, .. } => {
                self.agents.insert(
                    id.clone(),
                    Spawned {
                        label: label.clone(),
                        at: self.at,
                    },
                );
            }
            Event::AgentExit { id, .. } => {
                self.agents.remove(id);
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
    fn forget_ask(&mut self, id: &ToolCallId) -> Option<Ask> {
        let at = self.asks.iter().position(|ask| &ask.id == id)?;
        if at == 0 {
            self.ask_focus = 0;
        }
        self.asks.remove(at)
    }

    /// The answer Enter would give the prompt on screen.
    pub fn ask_focus(&self) -> Answer {
        Answer::ALL
            .get(self.ask_focus)
            .copied()
            .unwrap_or(Answer::Once)
    }

    /// Moves the focus one button along, `forward` or back, over the buttons
    /// the prompt offers, wrapping at either end.
    fn move_ask_focus(&mut self, forward: bool) {
        let Some(ask) = self.asks.front() else {
            return;
        };
        let count = Answer::ALL.len();
        let step = if forward { 1 } else { count - 1 };
        let mut at = self.ask_focus;
        for _ in 0..count {
            at = (at + step) % count;
            if Answer::ALL
                .get(at)
                .is_some_and(|answer| ask.offers(*answer))
            {
                self.ask_focus = at;
                return;
            }
        }
    }

    /// The prompt the modal is showing, if any.
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
                decision: PermissionDecision::Allow,
            });
        }
    }

    /// Answers the prompt the modal is showing.
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

    fn agent_name(&self) -> String {
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

    /// The same shell, drawn in `theme`.
    ///
    /// The composer is a widget that holds its own styles rather than being
    /// handed them at draw time, so it is repainted here; everything else
    /// reads [`App::theme`] as it draws.
    #[must_use]
    pub fn with_theme(mut self, theme: Theme) -> Self {
        self.set_theme(theme);
        self
    }

    /// Moves to the next theme, which is what `F9` does.
    fn cycle_theme(&mut self) {
        self.set_theme(self.theme.next());
    }

    fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        paint_composer(&mut self.composer, &theme);
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

    /// The fold every pane reads its numbers from.
    pub fn session(&self) -> &SessionState {
        &self.session
    }

    /// What a running sub-agent was spawned to do.
    pub fn agent_label(&self, id: &AgentId) -> Option<String> {
        self.agents.get(id).map(|spawned| spawned.label.clone())
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

    /// When a sub-agent still running was spawned, where the shell had a clock
    /// at the time.
    pub fn agent_spawned_at(&self, id: &AgentId) -> Option<Stamp> {
        self.agents.get(id).and_then(|spawned| spawned.at)
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

    /// Moves the transcript up by `lines`, which unpins it from the tail.
    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll = self.scroll().saturating_sub(lines);
        self.follow = false;
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
        self.follow = false;
    }

    /// Jumps to the newest line and stays there as the session grows.
    pub fn scroll_to_tail(&mut self) {
        self.scroll = self.max_scroll();
        self.follow = true;
    }

    /// Lines one notch of the mouse wheel scrolls, as most terminals scroll
    /// their own scrollback.
    const WHEEL_LINES: usize = 3;

    /// Handles one mouse report: the wheel scrolls the transcript. Nothing
    /// else the mouse does means anything to the shell yet.
    pub fn on_mouse(&mut self, mouse: ratatui::crossterm::event::MouseEvent) {
        use ratatui::crossterm::event::MouseEventKind;

        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_up(Self::WHEEL_LINES),
            MouseEventKind::ScrollDown => self.scroll_down(Self::WHEEL_LINES),
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
        let page = self.viewport_lines.max(1);

        // Quitting is always available: a session with a prompt up is still a
        // session the operator may need to leave, and the backend is told the
        // same way it is told about any other way out.
        if let (KeyCode::F(10), _) | (KeyCode::Char('q' | 'c'), KeyModifiers::CONTROL) =
            (key.code, key.modifiers)
        {
            self.quit();
            return;
        }
        // A prompt takes the keyboard whole. Typing into the composer behind a
        // modal would put the answer to a question the operator is still being
        // asked into the next turn.
        if self.asking().is_some() {
            self.on_ask_key(key);
            return;
        }
        // The model list takes the keyboard the same way, and for the same
        // reason: an arrow key that scrolled the transcript behind an open
        // list would move something the operator was not looking at.
        if self.picking().is_some() {
            self.on_pick_key(key);
            return;
        }

        match (key.code, key.modifiers) {
            // Alt+Enter opens a line; Enter sends. The other way round would
            // make the common action the awkward one.
            (KeyCode::Enter, KeyModifiers::ALT) => self.composer.insert_newline(),
            (KeyCode::Enter, _) => self.submit(),

            (KeyCode::PageUp, _) => self.scroll_up(page),
            (KeyCode::PageDown, _) => self.scroll_down(page),
            (KeyCode::Up, KeyModifiers::SHIFT) => self.scroll_up(1),
            (KeyCode::Down, KeyModifiers::SHIFT) => self.scroll_down(1),
            (KeyCode::Home, KeyModifiers::CONTROL) => self.scroll_to_head(),
            (KeyCode::End, KeyModifiers::CONTROL) => self.scroll_to_tail(),

            // Shift+Tab reaches crossterm as its own code rather than as Tab
            // with a modifier, which is why it is matched on the code alone.
            (KeyCode::BackTab, _) => self.cycle_mode(),
            // F5 has no cost breakdown behind it, but on a flat-rate plan the
            // question it is pressed for is when the windows come back, and
            // the session fold knows that.
            (KeyCode::F(5), _) => self.hint = Some(self.cost_hint(self.read_at())),
            (KeyCode::F(8), _) => self.pick_model(),
            (KeyCode::F(9), _) => self.cycle_theme(),
            (KeyCode::F(n), _) => self.hint = Some(fkey_hint(n).to_owned()),

            _ => {
                self.composer.input(Input::from(key));
            }
        }
    }

    /// One key, while the permission modal is up.
    ///
    /// Anything that is not an answer is swallowed rather than passed on: the
    /// operator is being asked a question, and a key that did something else
    /// under a modal would be a key nobody meant.
    fn on_ask_key(&mut self, key: ratatui::crossterm::event::KeyEvent) {
        use ratatui::crossterm::event::KeyCode;

        let has_target = self.asking().is_some_and(|ask| ask.target.is_some());
        match key.code {
            KeyCode::Enter => self.answer(self.ask_focus()),
            KeyCode::Tab | KeyCode::Right => self.move_ask_focus(true),
            KeyCode::BackTab | KeyCode::Left => self.move_ask_focus(false),
            KeyCode::Char('y' | 'Y') => self.answer(Answer::Once),
            KeyCode::Char('n' | 'N') | KeyCode::Esc => self.answer(Answer::No),
            KeyCode::Char('a' | 'A') => self.answer(Answer::AlwaysTool),
            KeyCode::Char('p' | 'P') if has_target => self.answer(Answer::AlwaysTarget),
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
        let running = self
            .tool_entries
            .values()
            .max()
            .and_then(|i| self.entries.get(*i));
        match (running, self.entries.last()) {
            (Some(call), _) => format!("running {}  {}", call.head, call.meta),
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
            return "F5 Cost — the cost breakdown is not implemented yet".to_owned();
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
        format!("F5 Cost — {}", parts.join(" · "))
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
            "F1 Help — the help browser is not implemented yet. The wheel and PgUp/PgDn \
             scroll; Shift- or Option-drag selects text"
        }
        2 => {
            "F2 Plan — the plan view is not implemented yet; Shift+Tab puts the \
              session in plan mode"
        }
        3 => "F3 Diff — the diff viewer is not implemented yet",
        4 => "F4 Undo — checkpoints and rewind are not implemented yet",
        6 => "F6 Files — file attribution is not implemented yet",
        7 => "F7 Tools — tool detail is not implemented yet",
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

/// How a finished call reads in the transcript.
fn outcome_label(outcome: ToolOutcome, bytes: u64) -> String {
    match outcome {
        ToolOutcome::Ok => format!("{} out", human_bytes(bytes)),
        ToolOutcome::Failed => format!("failed · {} out", human_bytes(bytes)),
        ToolOutcome::Denied => "denied".to_owned(),
    }
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
        });

        assert_eq!(app.entries().len(), 1, "the end opened a second entry");
        assert_eq!(app.entries()[0].meta, "catalog/fetch.ts · 4.0 kB out");
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
        });
        app.apply(&Event::ToolCallEnd {
            id: "t9".into(),
            name: "Edit".to_owned(),
            input: "cache.ts".to_owned(),
            output: String::new(),
            bytes: 0,
            outcome: ToolOutcome::Denied,
            summary: None,
        });

        assert_eq!(app.entries()[0].kind, EntryKind::Failure);
        assert_eq!(app.entries().len(), 2);
        assert_eq!(app.entries()[1].meta, "cache.ts · denied");
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

        let ask = app.asking().expect("the modal has a prompt");
        assert_eq!(ask.tool, "Bash");
        assert_eq!(ask.target.as_deref(), Some("rm -rf build"));
        assert_eq!(app.asks_waiting(), 0);

        app.on_key(key(KeyCode::Char('y')));

        assert!(app.asking().is_none());
        assert_eq!(
            app.take_produced(),
            [Event::PermissionResponse {
                id: "t1".into(),
                decision: PermissionDecision::Allow,
            }]
        );
        assert!(
            app.allowed().is_empty(),
            "allowing once left a standing rule behind"
        );
    }

    #[test]
    fn a_refusal_is_visible_in_the_transcript() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = app();
        app.apply(&prompt(Some("rm -rf build")));

        app.on_key(key(KeyCode::Char('n')));

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
        by_target.on_key(key(KeyCode::Char('p')));
        assert_eq!(
            by_target.take_rules(),
            [Rule::targeted("Bash", "rm -rf build")]
        );
        assert!(!by_target.allowed().allows("Bash", Some("rm -rf dist")));

        let mut by_tool = app();
        by_tool.apply(&prompt(Some("rm -rf build")));
        by_tool.on_key(key(KeyCode::Char('a')));
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
                }]
            );
        }
    }

    #[test]
    fn a_prompt_with_no_target_cannot_be_answered_for_one() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = app();
        app.apply(&prompt(None));

        app.on_key(key(KeyCode::Char('p')));

        assert!(
            app.asking().is_some(),
            "the prompt was answered by a key it does not offer"
        );
        assert!(app.take_rules().is_empty());
        assert!(app.take_produced().is_empty());
    }

    #[test]
    fn a_prompt_takes_the_keyboard_from_the_composer() {
        use ratatui::crossterm::event::KeyCode;
        let mut app = app();
        app.apply(&prompt(Some("rm -rf build")));

        // `x` is neither an answer nor a quit: under a modal it is nothing.
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
        app.on_key(key(KeyCode::Char('y')));

        assert_eq!(
            app.asking().map(|ask| ask.tool.clone()),
            Some("Write".to_owned())
        );
        assert_eq!(app.asks_waiting(), 0);
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
                decision: PermissionDecision::Allow,
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
            "F5 Cost — 5h window 33%, resets in 1h 30m · 7d window 23%, resets in 4d 6h"
        );
    }

    #[test]
    fn f5_on_a_session_with_no_windows_says_what_the_key_does_not_do_yet() {
        let mut app = app();
        assert!(
            app.cost_hint(0)
                .contains("the cost breakdown is not implemented yet")
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
            "F5 Cost — 5h window 90%, already reset · 7d window 40%, no reset time \
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
        assert_eq!(app.entries()[0].meta, "query: Niobe");
        assert_eq!(
            app.entries()[1].meta,
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
    fn the_first_button_has_focus_and_tab_walks_the_rest_in_order() {
        let mut app = asked(Some("ls"));
        assert_eq!(app.ask_focus(), Answer::Once);

        let mut walked = Vec::new();
        for _ in 0..4 {
            press(&mut app, KeyCode::Tab);
            walked.push(app.ask_focus());
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

        press(&mut app, KeyCode::Left);
        assert_eq!(app.ask_focus(), Answer::No);
        press(&mut app, KeyCode::Right);
        assert_eq!(app.ask_focus(), Answer::Once);
        app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.ask_focus(), Answer::No);
    }

    #[test]
    fn a_prompt_with_no_target_has_no_target_button_to_focus() {
        let mut app = asked(None);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.ask_focus(), Answer::No);
    }

    #[test]
    fn enter_presses_the_focused_button_and_the_next_prompt_starts_on_the_first() {
        let mut app = asked(Some("ls"));
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Enter);

        assert_eq!(
            app.take_produced(),
            [Event::PermissionResponse {
                id: "t1".into(),
                decision: PermissionDecision::Deny,
            }]
        );
        assert_eq!(app.asking().map(|ask| ask.id.as_str()), Some("t2"));
        assert_eq!(app.ask_focus(), Answer::Once);
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
}
