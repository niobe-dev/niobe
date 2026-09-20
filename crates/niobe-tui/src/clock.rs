// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The wall clock the menu row shows.
//!
//! The draw never reads the time: the event loop reads it once per tick and
//! hands it in, so a test draws the same frame every time and the frame budget
//! is not spent on a system call. Where the machine has no timezone to read,
//! there is no local time to show and the shell shows none — a UTC clock
//! presented as the operator's own would be a figure nobody measured, which is
//! the one thing the shell never draws.

use std::fmt;

/// A local time of day, to the minute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

/// The time it is now where the operator is, or `None` where the machine does
/// not say which timezone that is.
///
/// Called from the event loop and never from a draw.
pub fn now_local() -> Option<LocalTime> {
    let zone = jiff::tz::TimeZone::try_system().ok()?;
    let now = jiff::Timestamp::now().to_zoned(zone);
    LocalTime::new(
        u8::try_from(now.hour()).ok()?,
        u8::try_from(now.minute()).ok()?,
    )
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
}
