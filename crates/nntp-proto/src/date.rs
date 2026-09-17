//! Parsing the `Date` header.
//!
//! RFC 5322 §3.3 defines the format; Usenet does not always follow it. Two-digit years,
//! missing seconds, missing time zones, obsolete zone abbreviations, parenthesised
//! comments and full weekday names all appear in live traffic. A reader that rejects them
//! shows articles with no date at all, so this parser accepts the obsolete forms listed in
//! RFC 5322 §4.3 plus the ones observed in practice, and only gives up when the day, month
//! or year cannot be recovered.
//!
//! Note that [`crate::overview::OverviewRecord`] keeps the raw date string as well as the
//! parsed value, so a date this parser cannot handle is still displayable.

use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone, Utc};

use crate::{ProtoError, Result};

/// Parses a `Date` header value into a date-time with its original UTC offset.
///
/// The offset is preserved rather than normalised to UTC because it carries information:
/// it is a hint about where the author was, and displaying an article with the sender's
/// own clock reading is often what a reader wants.
///
/// # Errors
///
/// Returns [`ProtoError::InvalidDate`] if the day, month, year or time cannot be
/// recovered, or if the fields do not form a real instant (30 February, hour 25).
pub fn parse_date(value: &str) -> Result<DateTime<FixedOffset>> {
    let invalid = || ProtoError::InvalidDate(value.trim().to_owned());

    let cleaned = strip_comments(value);
    let mut tokens = cleaned
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();

    // An optional day-of-week comes first. Any leading alphabetic token is dropped: it is
    // either a weekday name or something we cannot use, and in both cases the date is in
    // the tokens that follow.
    if tokens
        .first()
        .is_some_and(|token| token.starts_with(|c: char| c.is_ascii_alphabetic()))
    {
        tokens.remove(0);
    }

    let day: u32 = tokens
        .first()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let month = parse_month(tokens.get(1).ok_or_else(invalid)?).ok_or_else(invalid)?;
    let year = parse_year(tokens.get(2).ok_or_else(invalid)?).ok_or_else(invalid)?;

    let (hour, minute, second) = match tokens.get(3) {
        Some(time) => parse_time(time).ok_or_else(invalid)?,
        // RFC 5322 requires a time; an article carrying only a date is still worth
        // showing, so midnight is assumed.
        None => (0, 0, 0),
    };

    let offset = match tokens.get(4) {
        Some(zone) => parse_zone(zone).ok_or_else(invalid)?,
        // A missing zone means "local time, unknown offset" (RFC 5322 §4.3 treats -0000
        // the same way). UTC is the only defensible assumption.
        None => 0,
    };

    let naive = NaiveDate::from_ymd_opt(year, month, day)
        .ok_or_else(invalid)?
        .and_hms_opt(hour, minute, second)
        .ok_or_else(invalid)?;
    let offset = FixedOffset::east_opt(offset).ok_or_else(invalid)?;

    offset
        .from_local_datetime(&naive)
        .single()
        .ok_or_else(invalid)
}

/// Parses a `Date` header value and converts it to UTC.
///
/// # Errors
///
/// As [`parse_date`].
pub fn parse_date_utc(value: &str) -> Result<DateTime<Utc>> {
    Ok(parse_date(value)?.with_timezone(&Utc))
}

/// Parses the `111` response to `DATE`: `yyyymmddhhmmss` in UTC (RFC 3977 §7.1).
///
/// # Errors
///
/// Returns [`ProtoError::InvalidDate`] if the value is not exactly fourteen digits
/// describing a real instant.
pub fn parse_server_date(value: &str) -> Result<DateTime<Utc>> {
    let invalid = || ProtoError::InvalidDate(value.trim().to_owned());
    let digits = value.trim();

    if digits.len() != 14 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }

    let number = |from: usize, to: usize| -> Result<u32> {
        digits
            .get(from..to)
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())
    };

    let year = i32::try_from(number(0, 4)?).map_err(|_| invalid())?;
    let naive = NaiveDate::from_ymd_opt(year, number(4, 6)?, number(6, 8)?)
        .ok_or_else(invalid)?
        .and_hms_opt(number(8, 10)?, number(10, 12)?, number(12, 14)?)
        .ok_or_else(invalid)?;

    Ok(Utc.from_utc_datetime(&naive))
}

/// Removes RFC 5322 comments, which may nest: `(a (b) c)`.
fn strip_comments(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut depth = 0usize;

    for ch in value.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }

    out
}

fn parse_month(token: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];

    // Full month names ("September") appear occasionally; the first three letters decide.
    let prefix = token.get(..3)?.to_ascii_lowercase();
    let index = MONTHS.iter().position(|month| *month == prefix)?;
    u32::try_from(index + 1).ok()
}

fn parse_year(token: &str) -> Option<i32> {
    let year: i32 = token.parse().ok()?;
    match token.len() {
        // RFC 5322 §4.3: two digits below 50 mean the 2000s, otherwise the 1900s.
        1 | 2 if year < 50 => Some(2000 + year),
        1 | 2 => Some(1900 + year),
        // Three-digit years are an obsolete form meaning 1900 + yyy.
        3 => Some(1900 + year),
        _ => Some(year),
    }
}

fn parse_time(token: &str) -> Option<(u32, u32, u32)> {
    let mut parts = token.split(':');
    let hour: u32 = parts.next()?.parse().ok()?;
    let minute: u32 = parts.next()?.parse().ok()?;
    let second: u32 = match parts.next() {
        // Seconds are optional in the obsolete syntax.
        None => 0,
        Some(raw) => raw.parse().ok()?,
    };

    if parts.next().is_some() {
        return None;
    }

    // A leap second is a real value that no calendar type accepts; clamping it loses one
    // second and keeps the article readable.
    Some((hour, minute, second.min(59)))
}

/// Parses a time zone into an offset in seconds east of UTC.
fn parse_zone(token: &str) -> Option<i32> {
    if let Some(rest) = token.strip_prefix('+') {
        return parse_numeric_zone(rest, 1);
    }
    if let Some(rest) = token.strip_prefix('-') {
        return parse_numeric_zone(rest, -1);
    }

    // RFC 5322 §4.3 obsolete zones. Anything else — including the single-letter military
    // zones, which the RFC says to treat as -0000 because they were widely mis-signed —
    // resolves to UTC.
    let hours = match token.to_ascii_uppercase().as_str() {
        "UT" | "UTC" | "GMT" | "Z" => 0,
        "EST" => -5,
        "EDT" => -4,
        "CST" => -6,
        "CDT" => -5,
        "MST" => -7,
        "MDT" => -6,
        "PST" => -8,
        "PDT" => -7,
        other if other.len() == 1 && other.is_ascii() => 0,
        _ => return None,
    };

    Some(hours * 3600)
}

fn parse_numeric_zone(digits: &str, sign: i32) -> Option<i32> {
    // `+HH:MM` is not legal but is sent anyway.
    let digits: String = digits.chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }

    let hours: i32 = digits.get(..2)?.parse().ok()?;
    let minutes: i32 = digits.get(2..)?.parse().ok()?;
    if minutes > 59 {
        return None;
    }

    Some(sign * (hours * 3600 + minutes * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(value: &str) -> String {
        parse_date(value).unwrap().to_rfc3339()
    }

    #[test]
    fn parses_the_canonical_form() {
        assert_eq!(
            parsed("Wed, 17 Sep 2026 08:09:10 +0000"),
            "2026-09-17T08:09:10+00:00"
        );
    }

    #[test]
    fn preserves_the_original_offset() {
        let date = parse_date("Wed, 17 Sep 2026 10:09:10 +0200").unwrap();
        assert_eq!(date.to_rfc3339(), "2026-09-17T10:09:10+02:00");
        assert_eq!(
            date.with_timezone(&Utc).to_rfc3339(),
            "2026-09-17T08:09:10+00:00"
        );
    }

    #[test]
    fn accepts_a_missing_day_of_week() {
        assert_eq!(
            parsed("17 Sep 2026 08:09:10 +0000"),
            "2026-09-17T08:09:10+00:00"
        );
    }

    #[test]
    fn accepts_a_full_weekday_and_month_name() {
        assert_eq!(
            parsed("Wednesday, 17 September 2026 08:09:10 +0000"),
            "2026-09-17T08:09:10+00:00"
        );
    }

    #[test]
    fn accepts_a_weekday_without_a_comma() {
        assert_eq!(
            parsed("Wed 17 Sep 2026 08:09:10 +0000"),
            "2026-09-17T08:09:10+00:00"
        );
    }

    #[test]
    fn accepts_missing_seconds() {
        assert_eq!(
            parsed("Wed, 17 Sep 2026 08:09 +0000"),
            "2026-09-17T08:09:00+00:00"
        );
    }

    #[test]
    fn expands_two_and_three_digit_years() {
        // RFC 5322 §4.3.
        assert!(parsed("17 Sep 26 08:09:10 +0000").starts_with("2026-"));
        assert!(parsed("17 Sep 49 08:09:10 +0000").starts_with("2049-"));
        assert!(parsed("17 Sep 50 08:09:10 +0000").starts_with("1950-"));
        assert!(parsed("17 Sep 99 08:09:10 +0000").starts_with("1999-"));
        assert!(parsed("17 Sep 101 08:09:10 +0000").starts_with("2001-"));
    }

    #[test]
    fn parses_obsolete_zone_names() {
        assert_eq!(
            parsed("17 Sep 2026 08:09:10 GMT"),
            "2026-09-17T08:09:10+00:00"
        );
        assert_eq!(
            parsed("17 Sep 2026 08:09:10 UT"),
            "2026-09-17T08:09:10+00:00"
        );
        assert_eq!(
            parsed("17 Sep 2026 08:09:10 PST"),
            "2026-09-17T08:09:10-08:00"
        );
        assert_eq!(
            parsed("17 Sep 2026 08:09:10 EDT"),
            "2026-09-17T08:09:10-04:00"
        );
        // Military single-letter zones resolve to UTC, as the RFC advises.
        assert_eq!(
            parsed("17 Sep 2026 08:09:10 A"),
            "2026-09-17T08:09:10+00:00"
        );
    }

    #[test]
    fn strips_parenthesised_comments_including_nested_ones() {
        assert_eq!(
            parsed("Wed, 17 Sep 2026 08:09:10 -0700 (PDT)"),
            "2026-09-17T08:09:10-07:00"
        );
        assert_eq!(
            parsed("Wed, 17 Sep 2026 08:09:10 +0000 (UTC (really))"),
            "2026-09-17T08:09:10+00:00"
        );
    }

    #[test]
    fn tolerates_a_colon_in_the_numeric_offset() {
        assert_eq!(
            parsed("17 Sep 2026 08:09:10 +02:00"),
            "2026-09-17T08:09:10+02:00"
        );
    }

    #[test]
    fn assumes_utc_when_the_zone_is_absent() {
        assert_eq!(parsed("17 Sep 2026 08:09:10"), "2026-09-17T08:09:10+00:00");
    }

    #[test]
    fn treats_minus_zero_zero_zero_zero_as_utc() {
        assert_eq!(
            parsed("17 Sep 2026 08:09:10 -0000"),
            "2026-09-17T08:09:10+00:00"
        );
    }

    #[test]
    fn clamps_a_leap_second() {
        assert_eq!(
            parsed("31 Dec 2016 23:59:60 +0000"),
            "2016-12-31T23:59:59+00:00"
        );
    }

    #[test]
    fn collapses_excess_whitespace() {
        assert_eq!(
            parsed("Wed,   17    Sep   2026   08:09:10   +0000"),
            "2026-09-17T08:09:10+00:00"
        );
        assert_eq!(
            parsed("\tWed, 17 Sep 2026 08:09:10 +0000  "),
            "2026-09-17T08:09:10+00:00"
        );
    }

    #[test]
    fn rejects_dates_it_cannot_recover() {
        for bad in [
            "",
            "not a date",
            "Wed, 17 Sep",
            "17 Foo 2026 08:09:10 +0000",
            "32 Sep 2026 08:09:10 +0000",
            "30 Feb 2026 08:09:10 +0000",
            "17 Sep 2026 25:09:10 +0000",
            "17 Sep 2026 08:99:10 +0000",
            "17 Sep 2026 08:09:10 +9999",
            "17 Sep 2026 08:09:10 NONSENSE",
            "17 Sep 2026 08:09:10:11 +0000",
        ] {
            assert!(
                parse_date(bad).is_err(),
                "expected {bad:?} to be rejected, got {:?}",
                parse_date(bad)
            );
        }
    }

    #[test]
    fn error_carries_the_offending_value() {
        assert_eq!(
            parse_date("  nonsense  "),
            Err(ProtoError::InvalidDate("nonsense".to_owned()))
        );
    }

    #[test]
    fn converts_to_utc() {
        assert_eq!(
            parse_date_utc("17 Sep 2026 10:00:00 +0200")
                .unwrap()
                .to_rfc3339(),
            "2026-09-17T08:00:00+00:00"
        );
    }

    #[test]
    fn parses_the_server_date_response() {
        assert_eq!(
            parse_server_date("20260917080910").unwrap().to_rfc3339(),
            "2026-09-17T08:09:10+00:00"
        );
        assert_eq!(
            parse_server_date(" 20260917080910 ").unwrap().to_rfc3339(),
            "2026-09-17T08:09:10+00:00"
        );
    }

    #[test]
    fn rejects_a_malformed_server_date() {
        for bad in [
            "2026091708091",
            "202609170809100",
            "2026091708091x",
            "20261317080910",
            "20260917250910",
            "",
        ] {
            assert!(parse_server_date(bad).is_err(), "expected {bad:?} rejected");
        }
    }
}
