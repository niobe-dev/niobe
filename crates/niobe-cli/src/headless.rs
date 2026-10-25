// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! `niobe -p`: one turn run with no terminal, for a script or a CI job.
//!
//! The prompt is recorded and sent as the shell would send it, and the turn's
//! events are recorded as they arrive, so the session is in the store like any
//! other and `niobe --resume` carries it on. Nobody is at the terminal to
//! answer a permission prompt, so the repository's and the user's
//! `[permissions]` rules answer what they allow and every other call is
//! refused, with words that tell the agent why.
//!
//! What a caller needs is whether to trust the run, so the outcome is one of
//! three, each with its own exit status: the turn finished, it stopped on the
//! budget, or it failed. A turn failed where it ended on an error; a warning
//! the turn went on from, such as a request the backend retried, does not
//! fail it. A budget stop is read off the figures — the turn ended in an
//! error with as much spent as the budget allowed — rather than off the
//! backend's wording of it.
//!
//! A run asked to stop while the turn is under way — a CI job's timeout, or
//! Ctrl+C — asks the backend to stop the turn and gives it [`STOP_GRACE`] to
//! close it, so the turn's last figures are recorded; such a turn failed.

use std::time::{Duration, Instant, SystemTime};

use niobe_core::event::{Event, PermissionDecision};
use niobe_core::permission::Allowlist;
use niobe_core::session::SessionState;
use niobe_tui::Billed;
use niobe_tui::bridge::Bridge;
use niobe_tui::journal::Journal;

/// What `niobe -p` exits with when the turn stopped because it had spent the
/// budget `--budget` gave it. Apart from the failure status, so that a job can
/// tell "this cost too much" from "this did not work".
pub const BUDGET_STOPPED: u8 = 3;

/// How long a backend asked to stop the turn is given to close it before
/// the run ends without its close. Short of the grace a job runner gives
/// between the signal that asks and the kill — seven and a half seconds on
/// GitHub Actions, ten under `docker stop` — and long past the moment the CLI
/// was recorded taking to close a turn it was asked to stop.
pub const STOP_GRACE: Duration = Duration::from_secs(3);

/// What a prompt no rule allows is refused with. The backend hands it to the
/// agent, which can then say what it could not do rather than retry it.
pub const NO_OPERATOR: &str = "Refused: niobe -p runs with no one at the terminal, and no \
                               [permissions] rule allows this call. Say what you needed it for \
                               instead of trying it again.";

/// How a headless turn ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The turn finished without an error.
    Finished,
    /// The turn stopped on the budget, in the backend's words.
    Budget(String),
    /// The turn, or the backend running it, failed, in the backend's words.
    Failed(String),
    /// The run was asked to stop before the turn ended, and why.
    Stopped(String),
}

impl Outcome {
    /// The word `--json` names the outcome with.
    pub fn name(&self) -> &'static str {
        match self {
            Outcome::Finished => "finished",
            Outcome::Budget(_) => "budget_stopped",
            // The published shape has no word of its own for a run stopped
            // from outside, and a turn that did not end did not finish: it
            // failed, and the exit status says it was stopped.
            Outcome::Failed(_) | Outcome::Stopped(_) => "failed",
        }
    }

    /// Why the turn did not finish, where it did not.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Outcome::Finished => None,
            Outcome::Budget(why) | Outcome::Failed(why) | Outcome::Stopped(why) => Some(why),
        }
    }
}

/// One headless turn, run.
#[derive(Debug)]
pub struct Turn {
    /// How it ended.
    pub outcome: Outcome,
    /// The session's last reply of its own, as the agent wrote it; empty where
    /// it wrote none.
    pub reply: String,
    /// When the prompt was sent.
    pub started_at: SystemTime,
    /// Every event of the turn, folded.
    pub state: SessionState,
}

/// Sends `prompt` to `bridge` and waits for its turn to end, handing every
/// event to `journal` in the order it happened and answering each permission
/// prompt by `allowed`. `pause` is called whenever the backend has nothing
/// new, so that waiting does not spin. `stopping` says why the run has been
/// asked to stop, once it has: the backend is then asked to stop the turn
/// and given [`STOP_GRACE`] to close it.
pub fn run(
    bridge: &mut dyn Bridge,
    journal: &mut dyn Journal,
    allowed: &Allowlist,
    prompt: &str,
    budget: Option<f64>,
    pause: &mut dyn FnMut(),
    stopping: &mut dyn FnMut() -> Option<String>,
) -> Turn {
    let started_at = SystemTime::now();
    let mut state = SessionState::new();
    let mut reply = String::new();
    let mut failure: Option<String> = None;

    let asked = Event::UserMessage {
        text: prompt.to_owned(),
    };
    keep(journal, &mut state, &asked);
    if let Err(error) = bridge.send(prompt, &[]) {
        return Turn {
            outcome: Outcome::Failed(format!("the prompt did not reach the backend: {error}")),
            reply,
            started_at,
            state,
        };
    }

    // Every event of a batch is recorded, those after the one that ends the
    // turn included, so the store holds all the backend said; the first
    // ending is the outcome.
    let mut outcome: Option<Outcome> = None;
    let mut stopped: Option<(String, Instant)> = None;
    while outcome.is_none() {
        if stopped.is_none()
            && let Some(why) = stopping()
        {
            stopped = Some(asked_to_stop(bridge, journal, &mut state, why));
        }
        if let Some((why, by)) = &stopped
            && Instant::now() >= *by
        {
            outcome = Some(Outcome::Stopped(why.clone()));
            break;
        }
        let events = bridge.drain();
        if events.is_empty() {
            pause();
            continue;
        }
        for (at, event) in events.iter().enumerate() {
            keep(journal, &mut state, event);
            let ending = match event {
                Event::AssistantMessage { text, agent: None } => {
                    reply.clone_from(text);
                    None
                }
                Event::PermissionRequest {
                    id, tool, target, ..
                } if outcome.is_none() && !refused_by_backend(&events[at + 1..], id) => answered(
                    bridge,
                    journal,
                    &mut state,
                    allowed,
                    id,
                    tool,
                    target.as_deref(),
                ),
                Event::Error {
                    message,
                    fatal: true,
                } => Some(Outcome::Failed(message.clone())),
                Event::Error {
                    message,
                    fatal: false,
                } => {
                    failure = Some(message.clone());
                    None
                }
                Event::SessionLeft => Some(Outcome::Failed(
                    "the backend left before the turn ended".to_owned(),
                )),
                Event::TurnEnded => Some(ended(failure.take(), &state, budget)),
                _ => None,
            };
            // A warning is the turn's failure only where the turn ends on it:
            // the backend reports a turn that failed with the warning just
            // before its end, while one it went on from — a request retried,
            // a CLI release nobody recorded, a write slow to be read — is
            // followed by the rest of the turn.
            if !matches!(event, Event::Error { fatal: false, .. }) {
                failure = None;
            }
            if outcome.is_none() {
                outcome = ending;
            }
        }
    }
    let outcome = match (stopped, outcome) {
        (Some((why, _)), _) => Outcome::Stopped(why),
        (None, outcome) => outcome.unwrap_or(Outcome::Finished),
    };

    Turn {
        outcome,
        reply,
        started_at,
        state,
    }
}

/// Records why the run is stopping and asks the backend to stop the turn,
/// returning why and the moment the run ends whether the turn has closed or
/// not. A backend that cannot be asked is not waited on.
fn asked_to_stop(
    bridge: &mut dyn Bridge,
    journal: &mut dyn Journal,
    state: &mut SessionState,
    why: String,
) -> (String, Instant) {
    keep(
        journal,
        state,
        &Event::Notice {
            message: format!("niobe -p was stopped: {why}"),
        },
    );
    let grace = match bridge.interrupt() {
        Ok(()) => STOP_GRACE,
        Err(_) => Duration::ZERO,
    };
    (why, Instant::now() + grace)
}

/// Whether the backend refused call `id` by itself, in the events that
/// follow its prompt: a call the backend's own rules, hooks or mode refused
/// arrives as a prompt with its refusal behind it, in the same batch, and
/// nothing is waiting on an answer to it. Answering it anyway would be
/// refused as a prompt nobody asked, and record a second decision.
fn refused_by_backend(after: &[Event], id: &niobe_core::event::ToolCallId) -> bool {
    after.iter().any(
        |event| matches!(event, Event::PermissionResponse { id: answered, .. } if answered == id),
    )
}

/// Answers the prompt about call `id` by `allowed`, and records the answer.
/// `Some` where the answer could not be given, which fails the turn: the
/// call would wait on it for ever.
fn answered(
    bridge: &mut dyn Bridge,
    journal: &mut dyn Journal,
    state: &mut SessionState,
    allowed: &Allowlist,
    id: &niobe_core::event::ToolCallId,
    tool: &str,
    target: Option<&str>,
) -> Option<Outcome> {
    let answer = answer(allowed, tool, target);
    let message = (answer == PermissionDecision::Deny).then_some(NO_OPERATOR);
    if let Err(error) = bridge.answer(id, answer, message) {
        return Some(Outcome::Failed(format!(
            "a permission prompt could not be answered: {error}"
        )));
    }
    let answer = Event::PermissionResponse {
        id: id.clone(),
        decision: answer,
        message: message.map(str::to_owned),
    };
    keep(journal, state, &answer);
    None
}

fn keep(journal: &mut dyn Journal, state: &mut SessionState, event: &Event) {
    journal.append(event);
    state.apply(event);
}

/// What a standing rule answers a prompt about `tool` on `target` with:
/// allowed where one covers it, refused where none does.
fn answer(allowed: &Allowlist, tool: &str, target: Option<&str>) -> PermissionDecision {
    match allowed.allows(tool, target) {
        true => PermissionDecision::AllowByRule,
        false => PermissionDecision::Deny,
    }
}

/// How a turn that reached its end ended: finished where nothing failed, a
/// budget stop where it failed with the budget spent, and failed otherwise.
fn ended(failure: Option<String>, state: &SessionState, budget: Option<f64>) -> Outcome {
    let Some(why) = failure else {
        return Outcome::Finished;
    };
    let billed = Billed::of(state);
    let spent = billed
        .spent(state)
        .unwrap_or_else(|| state.totals())
        .reported_cost_usd;
    match budget {
        Some(budget) if spent >= budget => Outcome::Budget(why),
        Some(_) | None => Outcome::Failed(why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use niobe_core::event::{ToolCallId, Usage};
    use niobe_core::permission::Rule;
    use niobe_tui::bridge::BridgeError;
    use niobe_tui::journal::JournalError;
    use std::collections::VecDeque;

    /// A backend that hands over one batch of events per drain, and keeps
    /// what it was sent and answered.
    #[derive(Debug, Default)]
    struct Scripted {
        batches: VecDeque<Vec<Event>>,
        sent: Vec<String>,
        answered: Vec<(ToolCallId, PermissionDecision, Option<String>)>,
        /// Whether it can be asked to stop a turn, and how often it was.
        stoppable: bool,
        interrupted: usize,
    }

    impl Bridge for Scripted {
        fn send(
            &mut self,
            prompt: &str,
            _: &[niobe_core::image::Image],
        ) -> Result<(), BridgeError> {
            self.sent.push(prompt.to_owned());
            Ok(())
        }

        fn answer(
            &mut self,
            id: &ToolCallId,
            decision: PermissionDecision,
            message: Option<&str>,
        ) -> Result<(), BridgeError> {
            self.answered
                .push((id.clone(), decision, message.map(str::to_owned)));
            Ok(())
        }

        fn drain(&mut self) -> Vec<Event> {
            self.batches.pop_front().unwrap_or_default()
        }

        fn interrupt(&mut self) -> Result<(), BridgeError> {
            self.interrupted += 1;
            match self.stoppable {
                true => Ok(()),
                false => Err("cannot be asked".into()),
            }
        }
    }

    #[derive(Debug, Default)]
    struct Kept(Vec<Event>);

    impl Journal for Kept {
        fn append(&mut self, event: &Event) {
            self.0.push(event.clone());
        }

        fn settled(&mut self) -> Vec<Result<(), JournalError>> {
            Vec::new()
        }
    }

    fn cost(usd: f64) -> Event {
        Event::Usage(Usage {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: 0,
            model: "claude-sonnet-5".to_owned(),
            cost_usd: Some(usd),
            settles_model: Some(true),
            fast: false,
        })
    }

    fn metered() -> Event {
        Event::Billing {
            billing: niobe_core::Billing::Metered,
        }
    }

    fn ran(
        batches: Vec<Vec<Event>>,
        allowed: &Allowlist,
        budget: Option<f64>,
    ) -> (Turn, Scripted, Kept) {
        let mut bridge = Scripted {
            batches: batches.into(),
            ..Scripted::default()
        };
        let mut journal = Kept::default();
        let mut idle = 0;
        let turn = run(
            &mut bridge,
            &mut journal,
            allowed,
            "fix the test",
            budget,
            &mut || {
                idle += 1;
                assert!(idle < 100, "the turn never ended");
            },
            &mut || None,
        );
        (turn, bridge, journal)
    }

    /// Runs the batches against a backend that `stoppable` says can be
    /// asked to stop, with the run asked to stop before the first drain.
    fn stopped(batches: Vec<Vec<Event>>, stoppable: bool) -> (Turn, Scripted, Kept) {
        let mut bridge = Scripted {
            batches: batches.into(),
            stoppable,
            ..Scripted::default()
        };
        let mut journal = Kept::default();
        let turn = run(
            &mut bridge,
            &mut journal,
            &Allowlist::new(),
            "fix the test",
            None,
            &mut || std::thread::sleep(std::time::Duration::from_millis(1)),
            &mut || Some("SIGTERM asked the run to stop".to_owned()),
        );
        (turn, bridge, journal)
    }

    fn ended_turn(reply: &str) -> Vec<Event> {
        vec![
            Event::AssistantMessage {
                text: reply.to_owned(),
                agent: None,
            },
            Event::TurnEnded,
        ]
    }

    #[test]
    fn a_turn_that_ends_without_an_error_finished_with_its_last_reply() {
        let (turn, bridge, journal) = ran(
            vec![vec![], vec![metered(), cost(0.02)], ended_turn("done")],
            &Allowlist::new(),
            None,
        );
        assert_eq!(turn.outcome, Outcome::Finished);
        assert_eq!(turn.reply, "done");
        assert_eq!(bridge.sent, ["fix the test"]);
        assert_eq!(
            journal.0.first(),
            Some(&Event::UserMessage {
                text: "fix the test".to_owned()
            }),
            "the prompt is recorded before anything the backend said"
        );
        assert_eq!(journal.0.last(), Some(&Event::TurnEnded));
    }

    #[test]
    fn a_sub_agents_message_is_not_the_reply() {
        let (turn, ..) = ran(
            vec![vec![
                Event::AssistantMessage {
                    text: "mine".to_owned(),
                    agent: None,
                },
                Event::AssistantMessage {
                    text: "the agent's".to_owned(),
                    agent: Some(niobe_core::event::AgentId::from("a1")),
                },
                Event::TurnEnded,
            ]],
            &Allowlist::new(),
            None,
        );
        assert_eq!(turn.reply, "mine");
    }

    #[test]
    fn a_call_the_backend_refused_by_itself_is_left_with_the_backends_answer() {
        let mut allowed = Allowlist::new();
        allowed.insert(Rule::tool("Bash"));
        let id = ToolCallId::new("t1");
        let refused = vec![
            Event::PermissionRequest {
                id: id.clone(),
                tool: "Bash".to_owned(),
                input: "{}".to_owned(),
                target: Some("touch b.txt".to_owned()),
                proposal: None,
                reason: None,
                agent: None,
            },
            Event::PermissionResponse {
                id,
                decision: PermissionDecision::Deny,
                message: None,
            },
        ];
        let (turn, bridge, journal) = ran(vec![refused, ended_turn("could not")], &allowed, None);

        assert_eq!(turn.outcome, Outcome::Finished);
        assert!(bridge.answered.is_empty(), "{:?}", bridge.answered);
        let decisions: Vec<_> = journal
            .0
            .iter()
            .filter_map(|event| match event {
                Event::PermissionResponse { decision, .. } => Some(*decision),
                _ => None,
            })
            .collect();
        assert_eq!(decisions, [PermissionDecision::Deny]);
    }

    #[test]
    fn a_prompt_a_rule_allows_is_allowed_and_any_other_refused_with_why() {
        let mut allowed = Allowlist::new();
        allowed.insert(Rule::targeted("Bash", "cargo test"));
        let ask = |id: &str, command: &str| Event::PermissionRequest {
            id: ToolCallId::new(id),
            tool: "Bash".to_owned(),
            input: "{}".to_owned(),
            target: Some(command.to_owned()),
            proposal: None,
            reason: None,
            agent: None,
        };
        let (turn, bridge, journal) = ran(
            vec![
                vec![ask("t1", "cargo test"), ask("t2", "rm -rf target")],
                ended_turn("ran it"),
            ],
            &allowed,
            None,
        );

        assert_eq!(turn.outcome, Outcome::Finished);
        assert_eq!(
            bridge.answered,
            [
                (ToolCallId::new("t1"), PermissionDecision::AllowByRule, None),
                (
                    ToolCallId::new("t2"),
                    PermissionDecision::Deny,
                    Some(NO_OPERATOR.to_owned())
                ),
            ]
        );
        let recorded = journal
            .0
            .iter()
            .filter(|event| matches!(event, Event::PermissionResponse { .. }))
            .count();
        assert_eq!(recorded, 2, "each answer is in the session's record");
        assert!(turn.state.pending_permissions().is_empty());
    }

    #[test]
    fn a_turn_that_failed_with_the_budget_spent_stopped_on_the_budget() {
        let error = Event::Error {
            message: "the turn ended as `error_max_budget_usd`".to_owned(),
            fatal: false,
        };
        let batches = || vec![vec![metered(), cost(0.5), error.clone(), Event::TurnEnded]];

        let (stopped, ..) = ran(batches(), &Allowlist::new(), Some(0.5));
        assert_eq!(
            stopped.outcome,
            Outcome::Budget("the turn ended as `error_max_budget_usd`".to_owned())
        );

        let (failed, ..) = ran(batches(), &Allowlist::new(), Some(2.0));
        assert!(
            matches!(failed.outcome, Outcome::Failed(_)),
            "spent short of the budget, the error is the turn failing: {:?}",
            failed.outcome
        );
        let (unbudgeted, ..) = ran(batches(), &Allowlist::new(), None);
        assert!(matches!(unbudgeted.outcome, Outcome::Failed(_)));
    }

    fn warning(message: &str) -> Event {
        Event::Error {
            message: message.to_owned(),
            fatal: false,
        }
    }

    #[test]
    fn a_warning_the_turn_went_on_from_does_not_fail_a_turn_that_finished() {
        let (turn, ..) = ran(
            vec![
                vec![warning(
                    "this session is running Claude Code 9.9.9, and Niobe's reading of the \
                     protocol was recorded from 2.1.278",
                )],
                vec![warning(
                    "a request failed (overloaded), and the CLI is retrying it: attempt 1 of 10",
                )],
                vec![metered(), cost(0.02)],
                ended_turn("done"),
            ],
            &Allowlist::new(),
            Some(0.01),
        );
        assert_eq!(turn.outcome, Outcome::Finished);
        assert_eq!(turn.reply, "done");
    }

    #[test]
    fn a_turn_that_ends_on_an_error_after_earlier_warnings_failed_on_that_error() {
        let (turn, ..) = ran(
            vec![
                vec![warning(
                    "a request failed (overloaded), and the CLI is retrying it",
                )],
                vec![
                    cost(0.02),
                    warning("the turn ended as `error_during_execution`: overloaded"),
                    Event::TurnEnded,
                ],
            ],
            &Allowlist::new(),
            None,
        );
        assert_eq!(
            turn.outcome,
            Outcome::Failed("the turn ended as `error_during_execution`: overloaded".to_owned())
        );
    }

    #[test]
    fn a_backend_that_leaves_or_fails_outright_ends_the_turn_as_failed() {
        let (left, ..) = ran(vec![vec![Event::SessionLeft]], &Allowlist::new(), None);
        assert!(
            matches!(left.outcome, Outcome::Failed(_)),
            "{:?}",
            left.outcome
        );

        let (fatal, ..) = ran(
            vec![vec![Event::Error {
                message: "claude exited with status 1".to_owned(),
                fatal: true,
            }]],
            &Allowlist::new(),
            None,
        );
        assert_eq!(
            fatal.outcome,
            Outcome::Failed("claude exited with status 1".to_owned())
        );
    }

    #[test]
    fn what_the_backend_says_after_the_turn_ends_in_the_same_batch_is_recorded() {
        let (turn, _, journal) = ran(
            vec![vec![
                Event::Error {
                    message: "claude exited with status 1".to_owned(),
                    fatal: true,
                },
                Event::Notice {
                    message: "said while going".to_owned(),
                },
            ]],
            &Allowlist::new(),
            None,
        );
        assert!(matches!(turn.outcome, Outcome::Failed(_)));
        assert!(
            matches!(journal.0.last(), Some(Event::Notice { .. })),
            "{:?}",
            journal.0
        );
    }

    #[test]
    fn each_outcome_has_a_name_the_published_schema_allows() {
        let names = [
            Outcome::Finished.name(),
            Outcome::Budget(String::new()).name(),
            Outcome::Failed(String::new()).name(),
            Outcome::Stopped(String::new()).name(),
        ];
        assert_eq!(names, ["finished", "budget_stopped", "failed", "failed"]);
    }

    #[test]
    fn a_run_asked_to_stop_asks_the_backend_to_stop_and_records_the_turns_close() {
        let closed = vec![
            vec![],
            vec![
                metered(),
                cost(0.25),
                warning("the turn ended as `error_during_execution`"),
                Event::TurnEnded,
            ],
        ];
        let (turn, bridge, journal) = stopped(closed, true);

        assert_eq!(bridge.interrupted, 1, "asked once, however long it waits");
        assert_eq!(
            turn.outcome,
            Outcome::Stopped("SIGTERM asked the run to stop".to_owned())
        );
        assert_eq!(journal.0.last(), Some(&Event::TurnEnded));
        assert!(
            journal.0.iter().any(|event| matches!(
                event,
                Event::Notice { message } if message.contains("SIGTERM")
            )),
            "the session says why it stopped: {:?}",
            journal.0
        );
        assert_eq!(turn.state.totals().reported_cost_usd, 0.25);
    }

    #[test]
    fn a_backend_that_cannot_be_asked_to_stop_is_not_waited_on() {
        let started = std::time::Instant::now();
        let (turn, bridge, _) = stopped(Vec::new(), false);
        assert_eq!(bridge.interrupted, 1);
        assert!(matches!(turn.outcome, Outcome::Stopped(_)));
        assert!(
            started.elapsed() < STOP_GRACE,
            "waited {:?} on a close nobody asked for",
            started.elapsed()
        );
    }
}
