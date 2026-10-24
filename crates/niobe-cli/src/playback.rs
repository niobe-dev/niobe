// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! `niobe replay <log> --speed <n>`: a recorded log handed to the shell as a
//! backend would hand it a live session, each event at the gap its log
//! recorded before it, divided by the speed.
//!
//! The events go through the same fold a live session's do, so the panes
//! fill as they did then. A gap is cut to [`LONGEST_GAP`] once divided,
//! because a log records the hours its operator was away as faithfully as
//! the seconds the agent took, and a replay that waits out an afternoon
//! shows nothing. An event with no time, or one dated before an event
//! ahead of it, arrives with the one before, and the next gap is counted
//! from the latest time read so far.

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime};

use niobe_core::event::Event;
use niobe_core::image::Image;
use niobe_tui::{Bridge, BridgeError};

/// The longest a replay waits between two events, after the speed divided
/// the recorded gap.
pub const LONGEST_GAP: Duration = Duration::from_secs(2);

/// How many times the recorded pace a replay plays at: positive and finite,
/// which [`Speed::parse`] is the only way to make one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Speed(f64);

// Never NaN, so equality is total.
impl Eq for Speed {}

impl Speed {
    /// `text` as a speed, where it is a positive, finite number.
    pub fn parse(text: &str) -> Option<Self> {
        text.parse::<f64>()
            .ok()
            .filter(|speed| speed.is_finite() && *speed > 0.0)
            .map(Self)
    }
}

/// A recorded log, handed over as its schedule comes due.
#[derive(Debug)]
pub struct Playback {
    /// The events still to come, each with when it is due, counted from the
    /// start of the replay.
    pending: VecDeque<(Event, Duration)>,
    started: Instant,
}

impl Playback {
    /// `events` as `niobe-store` reads a dated log, played at `speed` times
    /// the pace they were recorded at.
    pub fn new(events: Vec<(Event, Option<SystemTime>)>, speed: Speed) -> Self {
        Self {
            pending: schedule(events, speed),
            started: Instant::now(),
        }
    }

    /// The events due by `elapsed` into the replay, oldest first.
    fn due(&mut self, elapsed: Duration) -> Vec<Event> {
        let mut due = Vec::new();
        while let Some((_, at)) = self.pending.front() {
            if *at > elapsed {
                break;
            }
            if let Some((event, _)) = self.pending.pop_front() {
                due.push(event);
            }
        }
        due
    }
}

/// When each of `events` is due, counted from the first, at `speed`.
fn schedule(
    events: Vec<(Event, Option<SystemTime>)>,
    Speed(speed): Speed,
) -> VecDeque<(Event, Duration)> {
    let mut due = Duration::ZERO;
    let mut last: Option<SystemTime> = None;
    events
        .into_iter()
        .map(|(event, at)| {
            if let (Some(before), Some(now)) = (last, at) {
                let gap = now.duration_since(before).unwrap_or_default();
                due = due.saturating_add(gap.div_f64(speed).min(LONGEST_GAP));
            }
            last = last.max(at);
            (event, due)
        })
        .collect()
}

impl Bridge for Playback {
    fn send(&mut self, _prompt: &str, _images: &[Image]) -> Result<(), BridgeError> {
        Err("this is a recorded log being replayed, not a session to continue".into())
    }

    fn drain(&mut self) -> Vec<Event> {
        self.due(self.started.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn said(text: &str) -> Event {
        Event::UserMessage {
            text: text.to_owned(),
        }
    }

    fn at(secs: u64) -> Option<SystemTime> {
        Some(UNIX_EPOCH + Duration::from_secs(secs))
    }

    fn texts(events: &[Event]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::UserMessage { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn each_event_is_due_at_its_recorded_gap_divided_by_the_speed() {
        let mut playback = Playback::new(
            vec![
                (said("a"), at(100)),
                (said("b"), at(101)),
                (said("c"), at(103)),
            ],
            Speed(2.0),
        );

        assert_eq!(texts(&playback.due(Duration::ZERO)), ["a"]);
        assert_eq!(texts(&playback.due(Duration::from_millis(499))), [""; 0]);
        assert_eq!(texts(&playback.due(Duration::from_millis(500))), ["b"]);
        assert_eq!(texts(&playback.due(Duration::from_millis(1_499))), [""; 0]);
        assert_eq!(texts(&playback.due(Duration::from_millis(1_500))), ["c"]);
        assert!(playback.due(Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn a_long_gap_is_cut_and_an_undated_or_earlier_event_comes_with_the_one_before() {
        let due: Vec<Duration> = schedule(
            vec![
                (said("a"), at(0)),
                (said("b"), at(3_600)),
                (said("c"), None),
                (said("d"), at(3_000)),
                (said("e"), at(3_601)),
            ],
            Speed(1.0),
        )
        .into_iter()
        .map(|(_, due)| due)
        .collect();

        assert_eq!(
            due,
            [
                Duration::ZERO,
                LONGEST_GAP,
                LONGEST_GAP,
                LONGEST_GAP,
                LONGEST_GAP + Duration::from_secs(1),
            ]
        );
    }

    #[test]
    fn a_replay_takes_no_prompt() {
        let mut playback = Playback::new(Vec::new(), Speed(1.0));

        assert!(playback.send("go on", &[]).is_err());
        assert!(playback.drain().is_empty());
    }
}
