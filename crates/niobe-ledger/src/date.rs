// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Calendar dates, which is what a price is in force from.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// A day in the proleptic Gregorian calendar, in UTC.
///
/// Providers announce price changes by the day, so a day is the resolution a
/// price is looked up at. Ordered chronologically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    year: u16,
    month: u8,
    day: u8,
}

impl Date {
    /// The date, or `None` when the day does not exist in that month.
    pub fn new(year: u16, month: u8, day: u8) -> Option<Self> {
        let valid = (1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month);
        valid.then_some(Self { year, month, day })
    }

    /// The UTC day `time` falls on. A time before 1970 reads as the first of
    /// January 1970, and one past the year 9999 as its last day: neither can
    /// be the clock of a session.
    pub fn of(time: SystemTime) -> Self {
        const SECONDS_PER_DAY: u64 = 86_400;
        let days = time
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs() / SECONDS_PER_DAY);
        civil_from_days(days)
    }

    /// The year.
    pub fn year(self) -> u16 {
        self.year
    }

    /// The month, from 1.
    pub fn month(self) -> u8 {
        self.month
    }

    /// The day of the month, from 1.
    pub fn day(self) -> u8 {
        self.day
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn is_leap(year: u16) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// The date `days` days after 1970-01-01, by Howard Hinnant's `civil_from_days`
/// for days on or after the epoch.
fn civil_from_days(days: u64) -> Date {
    const LAST: Date = Date {
        year: 9999,
        month: 12,
        day: 31,
    };
    const DAYS_PER_ERA: u64 = 146_097;

    let Some(shifted) = days.checked_add(719_468) else {
        return LAST;
    };
    let era = shifted / DAYS_PER_ERA;
    let day_of_era = shifted % DAYS_PER_ERA;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);

    match (u16::try_from(year), u8::try_from(month), u8::try_from(day)) {
        (Ok(year), Ok(month), Ok(day)) if year <= LAST.year => Date { year, month, day },
        _ => LAST,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn at(seconds: u64) -> Date {
        Date::of(UNIX_EPOCH + Duration::from_secs(seconds))
    }

    #[test]
    fn only_days_that_exist_are_dates() {
        assert!(Date::new(2026, 2, 28).is_some());
        assert!(Date::new(2026, 2, 29).is_none());
        assert!(Date::new(2024, 2, 29).is_some());
        assert!(Date::new(1900, 2, 29).is_none());
        assert!(Date::new(2000, 2, 29).is_some());
        assert!(Date::new(2026, 4, 31).is_none());
        assert!(Date::new(2026, 13, 1).is_none());
        assert!(Date::new(2026, 0, 1).is_none());
        assert!(Date::new(2026, 1, 0).is_none());
    }

    #[test]
    fn a_time_is_read_as_its_utc_day() {
        assert_eq!(at(0).to_string(), "1970-01-01");
        // 2026-09-16T00:00:00Z and one second before it.
        assert_eq!(at(1_789_516_800).to_string(), "2026-09-16");
        assert_eq!(at(1_789_516_799).to_string(), "2026-09-15");
        // 2024-02-29T12:00:00Z.
        assert_eq!(at(1_709_208_000).to_string(), "2024-02-29");
        // 2000-03-01T00:00:00Z, the day after a century leap day.
        assert_eq!(at(951_868_800).to_string(), "2000-03-01");
    }

    #[test]
    fn a_time_outside_the_calendar_is_clamped_to_its_ends() {
        assert_eq!(
            Date::of(UNIX_EPOCH - Duration::from_secs(1)).to_string(),
            "1970-01-01"
        );
        // Year 10000 onwards, and a day count too large to shift.
        assert_eq!(at(253_402_300_800).to_string(), "9999-12-31");
        assert_eq!(civil_from_days(u64::MAX).to_string(), "9999-12-31");
        assert_eq!(civil_from_days(2_932_896).to_string(), "9999-12-31");
    }

    #[test]
    fn dates_order_chronologically() {
        let date = |y, m, d| Date::new(y, m, d).expect("a real date");
        assert!(date(2026, 7, 30) < date(2026, 8, 1));
        assert!(date(2025, 12, 31) < date(2026, 1, 1));
        assert!(date(2026, 3, 13) > date(2026, 3, 12));
    }
}
