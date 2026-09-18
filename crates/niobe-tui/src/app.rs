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

use niobe_core::event::{
    AgentId, Backend, Event, Mode, PermissionDecision, ToolCallId, ToolOutcome, UsageWindow,
    UsageWindows,
};
use niobe_core::permission::{Allowlist, Rule};
use niobe_core::session::SessionState;
use ratatui_textarea::{Input, TextArea, WrapMode};

use ratatui::style::Style;

use crate::theme::Theme;

/// Where the session is running, for the pane title and the status line.
///
/// Filled in by the caller: reading the working directory and the git branch is
/// the CLI's job, not the TUI's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Repo {
    /// The directory name the session was started in.
    pub name: String,
    /// The checked-out branch, where there is one.
    pub branch: Option<String>,
}

/// The profile the operator selected, for the status line to name until a
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// What each sub-agent was spawned to do. The session fold keeps the counts
    /// and the ids; the label is the parallel pane's business, so it is kept
    /// here rather than widening the shared state for one pane.
    agent_labels: BTreeMap<AgentId, String>,
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
    /// Whether the budget warning has already been given, so that it is said
    /// once rather than on every usage record after the line is crossed.
    budget_warned: bool,
    /// Whether a backend is listening. The shell holds no backend handle; this
    /// is the one bit of it the transcript needs, so that a prompt with nowhere
    /// to go says so instead of looking sent.
    attached: bool,
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
        composer.set_placeholder_style(Style::new().fg(theme.dim).bg(theme.pane_bg));
        composer.set_style(Style::new().fg(theme.fg).bg(theme.pane_bg));
        composer.set_cursor_line_style(Style::new().fg(theme.fg).bg(theme.pane_bg));
        composer.set_cursor_style(Style::new().fg(theme.pane_bg).bg(theme.hot));

        Self {
            repo,
            profile: None,
            theme,
            session: SessionState::new(),
            entries: Vec::new(),
            tool_entries: BTreeMap::new(),
            agent_labels: BTreeMap::new(),
            composer,
            scroll: 0,
            follow: true,
            transcript_lines: 0,
            viewport_lines: 0,
            hint: None,
            asks: VecDeque::new(),
            allowed: Allowlist::new(),
            learned: Vec::new(),
            produced: Vec::new(),
            picking: None,
            budget_usd: None,
            budget_warned: false,
            attached: false,
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
                    });
                }
            },

            Event::ToolCallStart { id, name, input } => {
                self.tool_entries.insert(id.clone(), self.entries.len());
                self.push(Entry {
                    kind: EntryKind::Tool,
                    head: name.clone(),
                    meta: one_line(input),
                    body: String::new(),
                    streaming: true,
                });
            }

            Event::ToolCallEnd {
                id,
                name,
                input,
                bytes,
                outcome,
                ..
            } => {
                let meta = format!("{} · {}", one_line(input), outcome_label(*outcome, *bytes));
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
                        head: name.clone(),
                        meta,
                        body: String::new(),
                        streaming: false,
                    }),
                }
            }

            Event::Error { message, fatal } => self.push(Entry {
                kind: EntryKind::Failure,
                head: if *fatal { "fatal" } else { "error" }.to_owned(),
                meta: String::new(),
                body: message.clone(),
                streaming: false,
            }),

            Event::Notice { message } => self.push(Entry {
                kind: EntryKind::Notice,
                head: self.agent_name(),
                meta: String::new(),
                body: message.clone(),
                streaming: false,
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
                            ask.tool.clone(),
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
                    });
                }
            }

            // Everything else is a number or a list a pane reads off the
            // session fold, not a line in the transcript. The mode and the
            // model are on the status line, which is where a session says what
            // it is running as; a file's counts are in the changes pane, and
            // repeating them under the call that made them would say the same
            // thing twice in the place with the least room for it.
            Event::SessionMeta(_)
            | Event::Usage(_)
            | Event::ModeSelected { .. }
            | Event::ModelSelected { .. }
            | Event::UsageWindows(_)
            | Event::FileChange { .. }
            | Event::Decision { .. }
            | Event::Checkpoint { .. } => {}

            Event::AgentSpawn { id, label, .. } => {
                self.agent_labels.insert(id.clone(), label.clone());
            }
            Event::AgentExit { id, .. } => {
                self.agent_labels.remove(id);
            }
        }
    }

    /// Folds a whole stream in.
    pub fn extend<'a>(&mut self, events: impl IntoIterator<Item = &'a Event>) {
        for event in events {
            self.apply(event);
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
        self.asks.remove(at)
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
        });
        self.scroll_to_tail();
    }

    /// The same shell, with a ceiling on what the session may spend.
    #[must_use]
    pub fn with_budget(mut self, budget_usd: f64) -> Self {
        self.budget_usd = Some(budget_usd);
        self
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
        self.agent_labels.get(id).cloned()
    }

    /// The transcript, oldest first.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The composer widget, for the draw.
    pub fn composer(&self) -> &TextArea<'static> {
        &self.composer
    }

    /// What the operator has typed and not sent.
    pub fn composed(&self) -> String {
        self.composer.lines().join("\n")
    }

    /// The transient message on the status line, if there is one.
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
            (KeyCode::F(5), _) => self.hint = Some(self.cost_hint(now_secs())),
            (KeyCode::F(8), _) => self.pick_model(),
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
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => self.answer(Answer::Once),
            KeyCode::Char('n' | 'N') | KeyCode::Esc => self.answer(Answer::No),
            KeyCode::Char('a' | 'A') => self.answer(Answer::AlwaysTool),
            KeyCode::Char('p' | 'P') if has_target => self.answer(Answer::AlwaysTarget),
            _ => {}
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
            });
        }
        self.scroll_to_tail();
    }

    /// Says in the transcript that a turn never reached the backend, so that a
    /// prompt with no reply is not read as a backend thinking about it.
    pub fn not_sent(&mut self, error: &str) {
        self.push(Entry {
            kind: EntryKind::Failure,
            head: "not sent".to_owned(),
            meta: String::new(),
            body: format!(
                "What you just typed did not reach the backend, so nothing is working on \
                 it: {error}"
            ),
            streaming: false,
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
/// countdown to the second on a status line is a number that redraws for
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
fn percent(utilization: f64) -> u64 {
    (utilization * 100.0).round() as u64
}

/// The plan's windows as the status line shows them — `62%/5h · 18%/7d` — and
/// `None` where no backend has reported any, so the segment is absent rather
/// than zeroed.
pub fn windows_label(windows: &UsageWindows) -> Option<String> {
    let parts: Vec<String> = [("5h", windows.five_hour), ("7d", windows.seven_day)]
        .into_iter()
        .filter_map(|(label, window)| {
            window.map(|window| format!("{}%/{label}", percent(window.utilization)))
        })
        .collect();
    match parts.is_empty() {
        true => None,
        false => Some(parts.join(" · ")),
    }
}

/// What an F-key does, for the ones that do nothing yet.
///
/// F5, F8 and F10 are handled before this is reached, so nothing here names
/// them.
fn fkey_hint(n: u8) -> &'static str {
    match n {
        1 => "F1 Help — the help browser is not implemented yet",
        2 => {
            "F2 Plan — the plan view is not implemented yet; Shift+Tab puts the \
              session in plan mode"
        }
        3 => "F3 Diff — the diff viewer is not implemented yet",
        4 => "F4 Undo — checkpoints and rewind are not implemented yet",
        6 => "F6 Files — file attribution is not implemented yet",
        7 => "F7 Tools — tool detail is not implemented yet",
        // F8 opens the model list rather than saying anything, so nothing here
        // names it.
        9 => "F9 Theme — classic is the only theme",
        _ => "F10 Quit",
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
    use niobe_core::event::{Backend, SessionMeta, Usage};
    use niobe_core::permission::Rule;
    use ratatui::crossterm::event::KeyCode;
    use ratatui_textarea::Key;

    fn app() -> App {
        App::new(Repo {
            name: "niobe".to_owned(),
            branch: Some("main".to_owned()),
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
        });
        assert!(app.entries()[0].streaming);

        app.apply(&Event::ToolCallEnd {
            id: "t1".into(),
            name: "Read".to_owned(),
            input: "catalog/fetch.ts".to_owned(),
            output: "212 lines".to_owned(),
            bytes: 4_096,
            outcome: ToolOutcome::Ok,
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
        });
        app.apply(&Event::ToolCallEnd {
            id: "t1".into(),
            name: "Bash".to_owned(),
            input: "npm test".to_owned(),
            output: "1 failing".to_owned(),
            bytes: 128,
            outcome: ToolOutcome::Failed,
        });
        app.apply(&Event::ToolCallEnd {
            id: "t9".into(),
            name: "Edit".to_owned(),
            input: "cache.ts".to_owned(),
            output: String::new(),
            bytes: 0,
            outcome: ToolOutcome::Denied,
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
            "the status line would still name the model the session moved off"
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

        // Pressing it is what puts the line on the status bar.
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
    fn the_status_label_names_only_the_windows_that_were_reported() {
        assert_eq!(
            windows_label(&windows(0)).as_deref(),
            Some("33%/5h · 23%/7d")
        );
        assert_eq!(
            windows_label(&UsageWindows {
                seven_day: None,
                ..windows(0)
            })
            .as_deref(),
            Some("33%/5h")
        );
        assert_eq!(
            windows_label(&UsageWindows::default()),
            None,
            "a plan with no window reported was given one"
        );
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
}
