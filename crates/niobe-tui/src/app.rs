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

use std::collections::BTreeMap;

use niobe_core::event::{AgentId, Backend, Event, ToolCallId, ToolOutcome};
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
    /// Events the operator produced that have not been handed out to be kept.
    produced: Vec<Event>,
    should_quit: bool,
}

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
            produced: Vec::new(),
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

            // Everything else is a number or a list a pane reads off the
            // session fold, not a line in the transcript.
            Event::SessionMeta(_)
            | Event::Usage(_)
            | Event::PermissionRequest { .. }
            | Event::PermissionResponse { .. }
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

        match (key.code, key.modifiers) {
            (KeyCode::F(10), _) | (KeyCode::Char('q' | 'c'), KeyModifiers::CONTROL) => self.quit(),

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

            (KeyCode::F(n), _) => self.hint = Some(fkey_hint(n).to_owned()),

            _ => {
                self.composer.input(Input::from(key));
            }
        }
    }

    /// Sends what is in the composer.
    ///
    /// No backend is attached yet, so the prompt goes into the transcript and
    /// the shell says plainly that nothing is listening — rather than showing a
    /// reply nobody produced.
    pub fn submit(&mut self) {
        let text = self.composed();
        if text.trim().is_empty() {
            return;
        }

        self.composer.clear();

        self.produce(Event::UserMessage { text });
        self.push(Entry {
            kind: EntryKind::Notice,
            head: "no backend".to_owned(),
            meta: "not sent".to_owned(),
            body: "Nothing is attached to this session yet: the claude and codex bridges \
                   are not implemented. The prompt above was not sent."
                .to_owned(),
            streaming: false,
        });
        self.scroll_to_tail();
    }

    /// Feeds a key straight to the composer, for tests and for a paste.
    pub fn type_into_composer(&mut self, input: impl Into<Input>) {
        self.composer.input(input);
    }
}

/// What an F-key does. Only F10 does anything so far; the others say so.
fn fkey_hint(n: u8) -> &'static str {
    match n {
        1 => "F1 Help — the help browser is not implemented yet",
        2 => "F2 Plan — plan mode is not implemented yet",
        3 => "F3 Diff — the diff viewer is not implemented yet",
        4 => "F4 Undo — checkpoints and rewind are not implemented yet",
        5 => "F5 Cost — the cost breakdown is not implemented yet",
        6 => "F6 Files — file attribution is not implemented yet",
        7 => "F7 Tools — tool detail is not implemented yet",
        8 => "F8 Model — model switching is not implemented yet",
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
    use niobe_core::event::{Backend, SessionMeta};
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

    #[test]
    fn bytes_read_short() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(4_096), "4.0 kB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MB");
    }
}
