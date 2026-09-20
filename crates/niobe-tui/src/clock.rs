// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The clock the shell draws by: the wall clock in the menu row, the moment an
//! entry happened, and how long ago that was.
//!
//! The draw never reads the time. The event loop reads it once per tick and
//! hands it in, so a test draws the same frame every time and the frame budget
//! is not spent on a system call — and so that a [`Stamp`] already carries the
//! time of day it resolves to, rather than resolving a timezone under the draw.
//! Where the machine has no timezone to read, there is no local time to show
//! and the shell shows none — a UTC clock presented as the operator's own would
//! be a figure nobody measured, which is the one thing the shell never draws.
//!
//! [`niobe_core::event::Event`] carries no time and this module does not give
//! it one. A live session is stamped as the shell takes an event off the
//! channel; a recorded one is stamped from what the store wrote beside each
//! row, so a session read back off disk shows the times it actually ran at. A
//! log that recorded no times — a JSON Lines log is one — leaves every stamp
//! absent, and the rows that would carry a clock carry none.
//!
//! The two renderings of a duration both live here so that no two panes can
//! disagree about the same figure: [`spent`] for how long something has been
//! running, [`ago`] for how long ago it happened.

use std::fmt;
use std::time::{Duration, SystemTime};

/// A local time of day, to the minute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalTime {
    hour: u8,
    minute: u8,
}

impl LocalTime {
    /// The time, or `None` when it is not a time of day.
    pub fn new(hour: u8, minute: u8) -> Option<Self> {
        (hour < 24 && minute < 60).then_some(Self { hour, minute })
    }

    /// The hour, on a twenty-four hour clock.
    pub fn hour(self) -> u8 {
        self.hour
    }

    /// The minute.
    pub fn minute(self) -> u8 {
        self.minute
    }
}

impl fmt::Display for LocalTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

/// The moment something happened, and the time of day that was.
///
/// The time of day is resolved once, when the stamp is made, because resolving
/// it needs the machine's timezone and the draw may not go looking for it. The
/// instant underneath is kept too: a time of day cannot say how long ago
/// something was, and `13:41` twice over is two different moments on two
/// different days.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stamp {
    at: SystemTime,
    local: Option<LocalTime>,
}

impl Stamp {
    /// A stamp for a moment whose time of day is already known.
    ///
    /// [`Clock`] is how one is normally made; this is for a caller that has
    /// both halves already, and for tests that need a fixed clock.
    pub fn new(at: SystemTime, local: Option<LocalTime>) -> Self {
        Self { at, local }
    }

    /// The time of day, where the machine said which timezone it is in.
    pub fn local(self) -> Option<LocalTime> {
        self.local
    }

    /// The moment itself.
    pub fn at(self) -> SystemTime {
        self.at
    }

    /// How long after `earlier` this stamp is, or `None` when it is not after
    /// it at all.
    ///
    /// A clock that went backwards between two stamps — the operator changed
    /// it, or the machine woke up and was corrected — leaves a duration nobody
    /// can defend, and the shell draws nothing rather than a negative age
    /// rendered as a positive one.
    pub fn since(self, earlier: Stamp) -> Option<Duration> {
        self.at.duration_since(earlier.at).ok()
    }
}

/// Reads the wall clock, in the timezone the machine says it is in.
///
/// The timezone is read once, when the clock is made, rather than on every
/// tick: it is a file on disk, and the event loop asks for the time ten times
/// a second.
#[derive(Debug, Clone)]
pub struct Clock {
    zone: Option<jiff::tz::TimeZone>,
}

impl Clock {
    /// A clock in the machine's own timezone, where it names one.
    pub fn system() -> Self {
        Self {
            zone: jiff::tz::TimeZone::try_system().ok(),
        }
    }

    /// Whether the machine named a timezone, and so whether stamps from this
    /// clock carry a time of day.
    pub fn knows_the_zone(&self) -> bool {
        self.zone.is_some()
    }

    /// A stamp for now.
    ///
    /// Called from the event loop and never from a draw.
    pub fn now(&self) -> Stamp {
        self.at(SystemTime::now())
    }

    /// A stamp for a moment something else recorded — the time the store wrote
    /// beside an event, read back.
    pub fn at(&self, at: SystemTime) -> Stamp {
        Stamp {
            at,
            local: self.local(at),
        }
    }

    fn local(&self, at: SystemTime) -> Option<LocalTime> {
        let zone = self.zone.clone()?;
        let millis =
            i64::try_from(at.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_millis()).ok()?;
        let zoned = jiff::Timestamp::from_millisecond(millis)
            .ok()?
            .to_zoned(zone);
        LocalTime::new(
            u8::try_from(zoned.hour()).ok()?,
            u8::try_from(zoned.minute()).ok()?,
        )
    }
}

/// `12s`, `1m 15s`, `2h 03m`: how long something has been running.
///
/// Seconds are kept for the first hour because this is what a figure that ticks
/// on screen is rendered with, and a counter that drops its seconds looks
/// stopped.
pub fn spent(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60),
    }
}

/// `4.2s`, `38s`, `12m`, `1h`, `2d`: how long ago something happened.
///
/// One unit, and coarser the older it gets: an age sits in a narrow column
/// beside the thing it dates, and `12m` there says everything `12m 04s` would.
/// Under ten seconds it keeps its tenths, because that is the range where the
/// operator is asking whether something *just* happened.
pub fn ago(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    match seconds {
        ..10.0 => format!("{seconds:.1}s"),
        ..60.0 => format!("{}s", duration.as_secs()),
        ..3600.0 => format!("{}m", duration.as_secs() / 60),
        ..86_400.0 => format!("{}h", duration.as_secs() / 3600),
        _ => format!("{}d", duration.as_secs() / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_of_day_is_two_digits_and_two_digits() {
        assert_eq!(
            LocalTime::new(9, 5).map(|t| t.to_string()).as_deref(),
            Some("09:05")
        );
        assert_eq!(
            LocalTime::new(14, 7).map(|t| t.to_string()).as_deref(),
            Some("14:07")
        );
        assert_eq!(
            LocalTime::new(0, 0).map(|t| t.to_string()).as_deref(),
            Some("00:00")
        );
    }

    #[test]
    fn an_hour_or_a_minute_off_the_clock_is_no_time_at_all() {
        assert_eq!(LocalTime::new(24, 0), None);
        assert_eq!(LocalTime::new(0, 60), None);
    }

    /// A moment in the middle of a minute, so that a minute added to it does
    /// not land on a boundary, and far from any daylight-saving change.
    const A_MOMENT: Duration = Duration::from_secs(1_789_000_020);

    #[test]
    fn a_stamp_has_a_time_of_day_exactly_when_the_machine_named_a_timezone() {
        let clock = Clock::system();
        let stamp = clock.at(SystemTime::UNIX_EPOCH + A_MOMENT);

        assert_eq!(stamp.local().is_some(), clock.knows_the_zone());
    }

    #[test]
    fn a_stamp_a_minute_later_reads_a_minute_later_on_the_clock() {
        let clock = Clock::system();
        let at = SystemTime::UNIX_EPOCH + A_MOMENT;
        let (Some(first), Some(later)) = (
            clock.at(at).local(),
            clock.at(at + Duration::from_secs(60)).local(),
        ) else {
            assert!(
                !clock.knows_the_zone(),
                "a zone resolved one time and not the other"
            );
            return;
        };

        // Minutes into the day, so that the assertion holds in a timezone
        // whose offset is not a whole hour, and across midnight.
        let minutes = |t: LocalTime| u32::from(t.hour()) * 60 + u32::from(t.minute());
        assert_eq!((minutes(later) + 1440 - minutes(first)) % 1440, 1);
    }

    #[test]
    fn how_long_apart_two_stamps_are_is_how_long_the_later_one_waited() {
        let clock = Clock::system();
        let first = clock.at(SystemTime::UNIX_EPOCH + Duration::from_secs(100));
        let later = clock.at(SystemTime::UNIX_EPOCH + Duration::from_secs(142));
        assert_eq!(later.since(first), Some(Duration::from_secs(42)));
    }

    #[test]
    fn a_stamp_earlier_than_the_one_it_is_measured_from_is_no_duration_at_all() {
        let clock = Clock::system();
        let first = clock.at(SystemTime::UNIX_EPOCH + Duration::from_secs(100));
        let later = clock.at(SystemTime::UNIX_EPOCH + Duration::from_secs(142));
        assert_eq!(first.since(later), None);
    }

    #[test]
    fn a_duration_reads_as_seconds_then_minutes_then_hours() {
        assert_eq!(spent(Duration::from_secs(0)), "0s");
        assert_eq!(spent(Duration::from_secs(38)), "38s");
        assert_eq!(spent(Duration::from_secs(59)), "59s");
        assert_eq!(spent(Duration::from_secs(72)), "1m 12s");
        assert_eq!(spent(Duration::from_secs(3599)), "59m 59s");
        assert_eq!(spent(Duration::from_secs(7380)), "2h 03m");
    }

    #[test]
    fn an_age_is_one_unit_and_gets_coarser_as_it_gets_older() {
        assert_eq!(ago(Duration::from_millis(4200)), "4.2s");
        assert_eq!(ago(Duration::from_secs(38)), "38s");
        assert_eq!(ago(Duration::from_secs(720)), "12m");
        assert_eq!(ago(Duration::from_secs(3600)), "1h");
        assert_eq!(ago(Duration::from_secs(172_800)), "2d");
    }

    #[test]
    fn an_age_under_ten_seconds_keeps_its_tenths_and_one_over_it_does_not() {
        assert_eq!(ago(Duration::from_millis(9900)), "9.9s");
        assert_eq!(ago(Duration::from_millis(10_100)), "10s");
    }
}
