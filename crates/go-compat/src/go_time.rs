//! Stored time text: RFC 3339 in UTC, with up to nine fraction digits.

// Rust guideline compliant 2026-06-26

use std::ops::RangeInclusive;

use serde::{Deserialize, Deserializer, Serializer};
use time::error::ParseFromDescription;
use time::{Date, Duration, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

use crate::Error;

/// Largest year that the time JSON form accepts.
///
/// The form rejects years outside `0..=GO_MAX_JSON_YEAR`, and only
/// `serialize` applies the bound.
const GO_MAX_JSON_YEAR: i32 = 9999;

/// Largest year that the parser reads: the stored form has exactly four year digits.
const MAX_YEAR: u32 = 9999;
/// Largest month of a calendar date.
const MAX_MONTH: u32 = 12;
/// Largest day of a calendar date; `Date` rejects the days a month lacks.
const MAX_DAY: u32 = 31;
/// Largest hour of a clock time.
const MAX_HOUR: u32 = 23;
/// Largest minute or second of a clock time; leap seconds are refused.
const MAX_MINUTE_OR_SECOND: u32 = 59;
/// Digits in the year field.
const YEAR_DIGITS: usize = 4;
/// Digits in a two-digit field.
const TWO_DIGITS: usize = 2;
/// Fraction digits kept; longer fractions are truncated to nanoseconds.
const NANOSECOND_DIGITS: usize = 9;
/// Largest hour of a numeric offset that the parser accepts.
const GO_MAX_OFFSET_HOUR: u32 = 24;
/// Largest minute of a numeric offset that the parser accepts.
const GO_MAX_OFFSET_MINUTE: u32 = 60;
/// Seconds in a minute, for offset arithmetic.
const SECONDS_PER_MINUTE: i64 = 60;
/// Minutes in an hour, for offset arithmetic.
const MINUTES_PER_HOUR: i64 = 60;

/// Formats `value` in UTC with a fraction of up to nine digits, trailing zeros dropped.
pub fn format_rfc3339_nano(value: OffsetDateTime) -> String {
    let utc = value.to_offset(UtcOffset::UTC);
    let base = format_date_time(&utc);
    let nanos = utc.nanosecond();
    if nanos == 0 {
        format!("{base}Z")
    } else {
        // Nine digits, then strip trailing zeros.
        let fraction = format!("{nanos:09}");
        let trimmed = fraction.trim_end_matches('0');
        format!("{base}.{trimmed}Z")
    }
}

/// Formats `value` in UTC as whole-second RFC 3339 text, dropping the fraction.
pub fn format_rfc3339_seconds(value: OffsetDateTime) -> String {
    let utc = value.to_offset(UtcOffset::UTC);
    let base = format_date_time(&utc);
    format!("{base}Z")
}

/// Parses RFC 3339 text with an offset and returns the instant in UTC.
///
/// The accepted texts use the layout `YYYY-MM-DDThh:mm:ss`, then an optional
/// fraction and a zone. A one-digit hour and a comma fraction are valid, while a lower-case
/// `t` or `z`, a space separator and a leap second are refused.
///
/// # Errors
///
/// Returns `Error::Parse` when `text` is not in that form, or when its instant
/// lies outside the years that `OffsetDateTime` represents.
pub fn parse_rfc3339(text: &str) -> Result<OffsetDateTime, Error> {
    match parse_go_layout(text.as_bytes()) {
        Ok(value) => Ok(value),
        Err(source) => Err(Error::Parse {
            input: text.to_owned(),
            source,
        }),
    }
}

/// Parses `text` in the layout `YYYY-MM-DDThh:mm:ss` with an optional fraction and a zone.
fn parse_go_layout(text: &[u8]) -> Result<OffsetDateTime, time::error::Parse> {
    let mut cursor = Cursor { rest: text };
    let year = cursor.digits(YEAR_DIGITS, 0..=MAX_YEAR, "year")?;
    cursor.expect(b'-', "year")?;
    let month = cursor.digits(TWO_DIGITS, 1..=MAX_MONTH, "month")?;
    cursor.expect(b'-', "month")?;
    let day = cursor.digits(TWO_DIGITS, 1..=MAX_DAY, "day")?;
    cursor.expect(b'T', "separator")?;
    let hour = cursor.hour()?;
    cursor.expect(b':', "hour")?;
    let minute = cursor.digits(TWO_DIGITS, 0..=MAX_MINUTE_OR_SECOND, "minute")?;
    cursor.expect(b':', "minute")?;
    let second = cursor.digits(TWO_DIGITS, 0..=MAX_MINUTE_OR_SECOND, "second")?;
    let nanosecond = cursor.fraction()?;
    let offset_seconds = zone(cursor.rest)?;
    let year_int = i32::try_from(year).map_err(|_| invalid("year"))?;
    let month_u8 = u8::try_from(month).map_err(|_| invalid("month"))?;
    let month_val = Month::try_from(month_u8).map_err(|_| invalid("month"))?;
    let day_u8 = u8::try_from(day).map_err(|_| invalid("day"))?;
    let date = Date::from_calendar_date(year_int, month_val, day_u8).map_err(|_| invalid("day"))?;
    let hour_u8 = u8::try_from(hour).map_err(|_| invalid("time"))?;
    let minute_u8 = u8::try_from(minute).map_err(|_| invalid("time"))?;
    let second_u8 = u8::try_from(second).map_err(|_| invalid("time"))?;
    let time = Time::from_hms_nano(hour_u8, minute_u8, second_u8, nanosecond)
        .map_err(|_| invalid("time"))?;
    let primitive = PrimitiveDateTime::new(date, time);
    let utc_primitive = primitive
        .checked_sub(Duration::seconds(offset_seconds))
        .ok_or_else(|| invalid("offset"))?;
    Ok(utc_primitive.assume_utc())
}

/// Builds the error for a field that the layout parser refuses.
fn invalid(component: &'static str) -> time::error::Parse {
    time::error::Parse::ParseFromDescription(ParseFromDescription::InvalidComponent(component))
}

/// Parses ASCII digits into a number, or returns `None` for any other byte.
fn parse_digits(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0u32, |acc, &byte| {
        let digit = byte.checked_sub(b'0').filter(|d| *d <= 9)?;
        let acc = acc.checked_mul(10)?;
        let digit_val = u32::from(digit);
        acc.checked_add(digit_val)
    })
}

/// Reads the text one field at a time; reads never index past the end.
struct Cursor<'a> {
    rest: &'a [u8],
}

impl Cursor<'_> {
    /// Reads exactly `count` digits whose value lies in `range`.
    fn digits(
        &mut self,
        count: usize,
        range: RangeInclusive<u32>,
        component: &'static str,
    ) -> Result<u32, time::error::Parse> {
        let (head, tail) = self
            .rest
            .split_at_checked(count)
            .ok_or_else(|| invalid(component))?;
        let value = parse_digits(head).ok_or_else(|| invalid(component))?;
        if !range.contains(&value) {
            return Err(invalid(component));
        }
        self.rest = tail;
        Ok(value)
    }

    /// Reads the hour, which the layout accepts as one digit or two.
    fn hour(&mut self) -> Result<u32, time::error::Parse> {
        let width = match self.rest.get(1) {
            Some(b) if b.is_ascii_digit() => 2,
            _ => 1,
        };
        self.digits(width, 0..=MAX_HOUR, "hour")
    }

    /// Consumes the byte `expected`, or refuses with `component`.
    fn expect(&mut self, expected: u8, component: &'static str) -> Result<(), time::error::Parse> {
        let (first, tail) = self.rest.split_first().ok_or_else(|| invalid(component))?;
        if *first != expected {
            return Err(invalid(component));
        }
        self.rest = tail;
        Ok(())
    }

    /// Reads an optional fraction: a comma or period followed by a digit.
    ///
    /// A separator without a digit is left in place, so the zone check refuses it.
    fn fraction(&mut self) -> Result<u32, time::error::Parse> {
        if self.rest.is_empty() {
            return Ok(0);
        }
        let (separator, after) = match self.rest.split_first() {
            Some((first, tail)) => (*first, tail),
            None => return Ok(0),
        };
        let digit_count = after.iter().take_while(|b| b.is_ascii_digit()).count();
        if (separator != b'.' && separator != b',') || digit_count == 0 {
            return Ok(0);
        }
        let (digits, tail) = after
            .split_at_checked(digit_count)
            .ok_or_else(|| invalid("fraction"))?;
        self.rest = tail;
        let mut value = 0u32;
        for index in 0..NANOSECOND_DIGITS {
            let byte = match digits.get(index) {
                Some(&b) => b,
                None => b'0',
            };
            let digit_val = match byte.checked_sub(b'0') {
                Some(d) if d <= 9 => u32::from(d),
                _ => 0,
            };
            value = value * 10 + digit_val;
        }
        Ok(value)
    }
}

/// Reads the zone, which must be the whole remaining text: `Z` or `±hh:mm`.
fn zone(rest: &[u8]) -> Result<i64, time::error::Parse> {
    match rest {
        [b'Z'] => Ok(0),
        [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] => {
            let hours = parse_digits(&[*h1, *h2]).ok_or_else(|| invalid("offset"))?;
            if hours > GO_MAX_OFFSET_HOUR {
                return Err(invalid("offset"));
            }
            let minutes = parse_digits(&[*m1, *m2]).ok_or_else(|| invalid("offset"))?;
            if minutes > GO_MAX_OFFSET_MINUTE {
                return Err(invalid("offset"));
            }
            let total_minutes = i64::from(hours) * MINUTES_PER_HOUR + i64::from(minutes);
            let seconds = total_minutes * SECONDS_PER_MINUTE;
            if *sign == b'-' {
                Ok(-seconds)
            } else {
                Ok(seconds)
            }
        }
        _ => Err(invalid("offset")),
    }
}

/// Serializes `value` as [`format_rfc3339_nano`] text.
///
/// # Errors
///
/// Returns an error when the year in UTC is outside `0..=GO_MAX_JSON_YEAR`.
pub fn serialize<S: Serializer>(value: &OffsetDateTime, serializer: S) -> Result<S::Ok, S::Error> {
    let year = value.to_offset(UtcOffset::UTC).year();
    if !(0..=GO_MAX_JSON_YEAR).contains(&year) {
        let err = Error::YearOutOfRange { year };
        return Err(serde::ser::Error::custom(err.to_string()));
    }
    let text = format_rfc3339_nano(*value);
    serializer.serialize_str(&text)
}

/// Deserializes RFC 3339 text with [`parse_rfc3339`].
///
/// # Errors
///
/// Returns an error when the input is not RFC 3339 text with an offset.
pub fn deserialize<'de, D>(deserializer: D) -> Result<OffsetDateTime, D::Error>
where
    D: Deserializer<'de>,
{
    let text = String::deserialize(deserializer)?;
    parse_rfc3339(&text).map_err(|err| serde::de::Error::custom(err.to_string()))
}

fn format_date_time(value: &OffsetDateTime) -> String {
    let year = value.year();
    let year_str = if year >= 0 {
        format!("{year:04}")
    } else {
        let abs = -year;
        format!("-{abs:04}")
    };
    // Month, day and time components are always two digits.
    let month: u8 = value.month().into();
    let day = value.day();
    let hour = value.hour();
    let minute = value.minute();
    let second = value.second();
    format!("{year_str}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}")
}
