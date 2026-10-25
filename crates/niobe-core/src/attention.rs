// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! How much an event asks of the operator: whether it waits on them, is
//! worth reading in a long session, or is the routine of the agent working.
//!
//! Decided from the event alone and never by a model, so two consumers that
//! fold the same events agree, and a session read back from its record reads
//! as it did live. What a consumer shows of an event decides nothing here:
//! a transcript entry several events built is as high as the highest of them.

use crate::event::Event;

/// How much an event asks of the operator, lowest first, so the higher of
/// two is their [`Ord::max`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Attention {
    /// The agent working: a read, a search, a step it reported, part of a
    /// reply, the backend saying what it runs as.
    #[default]
    Routine,
    /// What a long session is read for: what the operator and the agent
    /// said, a file of the project's written, a test run's result, a
    /// sub-agent finished, a turn ended, the conversation compacted.
    Notable,
    /// Nothing goes on until the operator does something, or something
    /// failed that they have to know of: a permission question, an error, a
    /// prompt that reached no backend, a model the backend refused.
    NeedsYou,
}

impl Attention {
    /// How much `event` asks of the operator.
    pub fn of(event: &Event) -> Self {
        match event {
            Event::PermissionRequest { .. }
            | Event::Error { .. }
            | Event::PromptNotSent { .. }
            | Event::ModelRefused { .. } => Self::NeedsYou,

            Event::UserMessage { .. }
            | Event::TurnEnded
            | Event::SessionLeft
            | Event::TestRun { .. }
            | Event::AgentExit { .. }
            | Event::Compacted { .. }
            | Event::Cleared
            | Event::TurnTotalDiffers { .. } => Self::Notable,
            // A sub-agent's words are its conversation with the session; what
            // the session made of them is the session's own reply.
            Event::AssistantMessage { agent, .. } => match agent {
                None => Self::Notable,
                Some(_) => Self::Routine,
            },
            // The agent's notes to itself are its own housekeeping.
            Event::FileChange { scope, .. } => match scope.is_project() {
                true => Self::Notable,
                false => Self::Routine,
            },

            Event::SessionMeta(_)
            | Event::Titled { .. }
            | Event::AssistantDelta { .. }
            | Event::ToolCallStart { .. }
            | Event::ToolCallEnd { .. }
            | Event::Usage(_)
            | Event::UsageWindows(_)
            | Event::Billing { .. }
            | Event::Context(_)
            | Event::CompactionStarted
            | Event::CompactionEnded
            | Event::PermissionResponse { .. }
            | Event::PermissionWithdrawn { .. }
            | Event::ModeSelected { .. }
            | Event::ModeUnknown
            | Event::ModelSelected { .. }
            | Event::AgentSpawn { .. }
            | Event::AgentProgress { .. }
            | Event::Commands { .. }
            | Event::McpServers { .. }
            | Event::Models { .. }
            | Event::Notice { .. }
            | Event::LimitReached { .. }
            | Event::Looked => Self::Routine,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{AgentId, ChangeScope, ToolCallId, ToolOutcome};

    fn change(scope: ChangeScope) -> Event {
        Event::FileChange {
            path: "src/lib.rs".to_owned(),
            added: Some(1),
            removed: Some(0),
            hunks: Vec::new(),
            scope,
        }
    }

    #[test]
    fn what_only_the_operator_can_move_on_needs_them() {
        let asked = Event::PermissionRequest {
            id: ToolCallId::from("call-1"),
            tool: "Bash".to_owned(),
            input: "{}".to_owned(),
            target: None,
            proposal: None,
            reason: None,
            agent: None,
        };
        assert_eq!(Attention::of(&asked), Attention::NeedsYou);
        let failed = Event::Error {
            message: "overloaded".to_owned(),
            fatal: false,
        };
        assert_eq!(Attention::of(&failed), Attention::NeedsYou);
        let unsent = Event::PromptNotSent {
            text: "go".to_owned(),
            why: None,
        };
        assert_eq!(Attention::of(&unsent), Attention::NeedsYou);
        assert_eq!(
            Attention::of(&Event::ModelRefused { back_to: None }),
            Attention::NeedsYou
        );
    }

    #[test]
    fn a_project_file_written_and_a_turn_ended_are_notable_and_the_agent_s_notes_are_not() {
        assert_eq!(
            Attention::of(&change(ChangeScope::Project)),
            Attention::Notable
        );
        assert_eq!(
            Attention::of(&change(ChangeScope::AgentMemory)),
            Attention::Routine
        );
        assert_eq!(Attention::of(&Event::TurnEnded), Attention::Notable);
        let exited = Event::AgentExit {
            id: AgentId::from("agent-1"),
            outcome: crate::event::AgentOutcome::Completed,
        };
        assert_eq!(Attention::of(&exited), Attention::Notable);
    }

    #[test]
    fn the_session_s_reply_is_notable_and_a_sub_agent_s_words_and_the_stream_are_routine() {
        let reply = Event::AssistantMessage {
            text: "done".to_owned(),
            agent: None,
        };
        assert_eq!(Attention::of(&reply), Attention::Notable);
        let agent_said = Event::AssistantMessage {
            text: "found it".to_owned(),
            agent: Some(AgentId::from("agent-1")),
        };
        assert_eq!(Attention::of(&agent_said), Attention::Routine);
        let part = Event::AssistantDelta {
            text: "do".to_owned(),
        };
        assert_eq!(Attention::of(&part), Attention::Routine);
    }

    #[test]
    fn a_read_a_search_and_a_backend_s_notice_are_routine_however_they_ended() {
        let read = Event::ToolCallEnd {
            id: ToolCallId::from("call-1"),
            name: "Read".to_owned(),
            input: "{}".to_owned(),
            output: String::new(),
            bytes: 0,
            outcome: ToolOutcome::Failed,
            summary: None,
            exit_code: None,
            error: None,
            command: None,
            untrimmed: None,
        };
        assert_eq!(Attention::of(&read), Attention::Routine);
        let notice = Event::Notice {
            message: "retrying".to_owned(),
        };
        assert_eq!(Attention::of(&notice), Attention::Routine);
    }

    #[test]
    fn the_higher_of_two_levels_is_their_max() {
        assert_eq!(
            Attention::Routine.max(Attention::NeedsYou),
            Attention::NeedsYou
        );
        assert_eq!(
            Attention::Notable.max(Attention::Routine),
            Attention::Notable
        );
    }
}
