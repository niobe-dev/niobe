// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The moment a transcript record's `timestamp` names.
//!
//! The CLI writes every stamp as `2026-09-19T11:14:47.000Z`: UTC, to the
//! millisecond. That one shape is read, with any number of fractional digits
//! up to nine; anything else — an offset other than `Z`, a date that is not
//! one, a time past `23:59:59` — is no moment at all rather than a guess at
//! one, so a record stamped in a shape nobody recorded shows no time.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The moment `stamp` names, where it is a UTC time in the shape the CLI
/// writes.
pub(crate) fn parse(stamp: &str) -> Option<SystemTime> {
    let (date, time) = stamp.strip_suffix('Z')?.split_once('T')?;
    let days = days(date)?;
    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (time, None),
    };
    let seconds = seconds(clock)?;
    let nanos = fraction.map_or(Some(0), nanos)?;
    let whole = u64::try_from(days.checked_mul(86_400)?.checked_add(seconds)?).ok()?;
    UNIX_EPOCH.checked_add(Duration::new(whole, nanos))
}

/// Days from 1970-01-01 to `date`, written `YYYY-MM-DD`.
fn days(date: &str) -> Option<i64> {
    let mut parts = date.split('-');
    let year = digits(parts.next()?, 4)?;
    let month = digits(parts.next()?, 2)?;
    let day = digits(parts.next()?, 2)?;
    if parts.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    if day < 1 || day > month_length(year, month) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// Seconds into the day of `clock`, written `HH:MM:SS`.
fn seconds(clock: &str) -> Option<i64> {
    let mut parts = clock.split(':');
    let hours = digits(parts.next()?, 2)?;
    let minutes = digits(parts.next()?, 2)?;
    let seconds = digits(parts.next()?, 2)?;
    if parts.next().is_some() || hours > 23 || minutes > 59 || seconds > 59 {
        return None;
    }
    Some(hours * 3_600 + minutes * 60 + seconds)
}

/// The nanoseconds a fraction of a second written as its digits stands for.
fn nanos(fraction: &str) -> Option<u32> {
    if fraction.is_empty() || fraction.len() > 9 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: u32 = fraction.parse().ok()?;
    let scale = 10_u32.checked_pow(u32::try_from(9 - fraction.len()).ok()?)?;
    value.checked_mul(scale)
}

/// `text` read as a number, where it is exactly `width` ASCII digits.
fn digits(text: &str, width: usize) -> Option<i64> {
    (text.len() == width && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

fn month_length(year: i64, month: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970-01-01 to a date of the proleptic Gregorian calendar, by
/// Howard Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64, millis: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs) + Duration::from_millis(millis)
    }

    #[test]
    fn a_stamp_in_the_clis_shape_reads_to_the_millisecond() {
        // `date -u -d 2026-09-18T09:00:05Z +%s` is 1789722005.
        assert_eq!(
            parse("2026-09-18T09:00:05.000Z"),
            Some(at(1_789_722_005, 0))
        );
        assert_eq!(
            parse("2026-09-18T09:00:05.250Z"),
            Some(at(1_789_722_005, 250))
        );
    }

    #[test]
    fn a_stamp_without_a_fraction_or_with_more_digits_still_reads() {
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some(UNIX_EPOCH));
        assert_eq!(
            parse("1970-01-01T00:00:01.000000001Z"),
            Some(UNIX_EPOCH + Duration::new(1, 1))
        );
    }

    #[test]
    fn a_leap_day_reads_and_a_day_that_does_not_exist_does_not() {
        // 2028-02-29 is 1,095 days after 2025-03-01, which is 1740787200.
        assert_eq!(
            parse("2028-02-29T00:00:00.000Z"),
            Some(at(1_740_787_200 + 1_095 * 86_400, 0))
        );
        assert_eq!(parse("2026-02-29T00:00:00.000Z"), None);
        assert_eq!(parse("2026-04-31T00:00:00.000Z"), None);
        assert_eq!(parse("2026-13-01T00:00:00.000Z"), None);
    }

    #[test]
    fn a_stamp_in_any_other_shape_is_no_moment() {
        for stamp in [
            "",
            "yesterday",
            "2026-09-18T09:00:05.000",
            "2026-09-18T09:00:05.000+02:00",
            "2026-09-18 09:00:05.000Z",
            "2026-09-18T24:00:00.000Z",
            "2026-09-18T09:60:00.000Z",
            "2026-09-18T09:00:05.Z",
            "2026-09-18T09:00:05.0000000001Z",
            "2026-9-18T09:00:05.000Z",
            "1969-12-31T23:59:59.000Z",
            "+2026-09-18T09:00:05.000Z",
        ] {
            assert_eq!(parse(stamp), None, "{stamp:?}");
        }
    }
}
