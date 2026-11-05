//! Go's `time.Time` text forms: RFC 3339 in UTC with Go's fraction rules.

// Rust guideline compliant 2026-06-26

use std::ops::RangeInclusive;

use serde::{Deserialize, Deserializer, Serializer};
use time::error::ParseFromDescription;
use time::{Date, Duration, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

use crate::Error;

/// Largest year that Go's `time.Time` JSON encoding accepts.
///
/// Go's JSON encoder rejects years outside `0..=GO_MAX_JSON_YEAR` and only
/// `serialize` applies the bound.
const GO_MAX_JSON_YEAR: i32 = 9999;

/// Largest year that the parser reads: Go's layout takes exactly four digits.
const MAX_YEAR: u32 = 9999;
/// Largest month of a calendar date.
const MAX_MONTH: u32 = 12;
/// Largest day of a calendar date; `Date` rejects the days a month lacks.
const MAX_DAY: u32 = 31;
/// Largest hour of a clock time.
const MAX_HOUR: u32 = 23;
/// Largest minute or second of a clock time; Go refuses leap seconds.
const MAX_MINUTE_OR_SECOND: u32 = 59;
/// Digits in the year field.
const YEAR_DIGITS: usize = 4;
/// Digits in a two-digit field.
const TWO_DIGITS: usize = 2;
/// Fraction digits kept; Go truncates longer fractions to nanoseconds.
const NANOSECOND_DIGITS: usize = 9;
/// Largest hour of a numeric offset; Go's `time.Parse` refuses only hours above it.
const GO_MAX_OFFSET_HOUR: u32 = 24;
/// Largest minute of a numeric offset; Go's `time.Parse` refuses only minutes above it.
const GO_MAX_OFFSET_MINUTE: u32 = 60;
/// Seconds in a minute, for offset arithmetic.
const SECONDS_PER_MINUTE: i64 = 60;
/// Minutes in an hour, for offset arithmetic.
const MINUTES_PER_HOUR: i64 = 60;

/// Formats `value` as Go's `time.RFC3339Nano` in UTC.
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

/// Formats `value` as Go's `time.RFC3339` in UTC, dropping the fraction.
pub fn format_rfc3339_seconds(value: OffsetDateTime) -> String {
    let utc = value.to_offset(UtcOffset::UTC);
    let base = format_date_time(&utc);
    format!("{base}Z")
}

/// Parses RFC 3339 text with an offset and returns the instant in UTC.
///
/// The accepted texts are the ones Go's `time.Time` JSON decoding accepts: it
/// reads the layout `2006-01-02T15:04:05Z07:00` with `time.Parse`, so a
/// one-digit hour and a comma fraction are valid, and a lower-case `t` or `z`,
/// a space separator and a leap second are refused.
///
/// # Errors
///
/// Returns `Error::Parse` when `text` is not accepted by Go, or when its instant
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

/// Parses `text` the way Go's `time.Parse` reads `2006-01-02T15:04:05Z07:00`.
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

/// Builds the error for a field that Go's layout parser refuses.
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

    /// Reads the hour, which Go's `15` field reads as one digit or two.
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

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use serde_json::Value;
    use time::{Date, Duration, Month, OffsetDateTime, Time};

    use super::{format_rfc3339_nano, format_rfc3339_seconds, parse_rfc3339};
    use crate::Error;

    /// Go-written timestamp cases, shared with the Go reference tests.
    const GOLDEN: &str = include_str!("../../../testdata/parity/timestamps.json");

    #[derive(Debug, Serialize, Deserialize)]
    struct Stamp {
        #[serde(with = "crate::go_time")]
        at: OffsetDateTime,
    }

    fn golden_instant(input: &Value) -> OffsetDateTime {
        if input["zero_time"].as_bool() == Some(true) {
            let date = Date::from_calendar_date(1, Month::January, 1).expect("valid date");
            return OffsetDateTime::new_utc(date, Time::MIDNIGHT);
        }
        let seconds = input["unix_seconds"]
            .as_i64()
            .expect("unix_seconds is an integer");
        let nanos = input["nanos"].as_i64().expect("nanos is an integer");
        OffsetDateTime::from_unix_timestamp(seconds).expect("valid timestamp")
            + Duration::nanoseconds(nanos)
    }

    #[test]
    fn parse_rfc3339_follows_go_json_decoding() {
        for refused in [
            "2026-01-02t03:04:05Z",
            "2026-01-02T03:04:05z",
            "2026-01-02 03:04:05Z",
        ] {
            assert!(
                matches!(parse_rfc3339(refused), Err(Error::Parse { .. })),
                "{refused} must be rejected"
            );
        }
        let comma = parse_rfc3339("2026-01-02T03:04:05,5Z").expect("comma fraction parses");
        assert_eq!(format_rfc3339_nano(comma), "2026-01-02T03:04:05.5Z");
        let whole = parse_rfc3339("2026-01-02T03:04:05Z").expect("RFC 3339 parses");
        assert_eq!(format_rfc3339_nano(whole), "2026-01-02T03:04:05Z");
    }

    #[test]
    fn parse_rfc3339_refuses_leap_second_and_unrepresentable_year() {
        for refused in ["2026-12-31T23:59:60Z", "9999-12-31T23:59:59-01:00"] {
            assert!(
                matches!(parse_rfc3339(refused), Err(Error::Parse { .. })),
                "{refused} must be rejected without panicking"
            );
        }
    }

    #[test]
    fn go_time_matches_go_golden() {
        let golden: Value = serde_json::from_str(GOLDEN).expect("golden is JSON");
        let cases = golden.as_array().expect("golden is an array");
        for case in cases {
            let name = case["name"].as_str().expect("case has a name");
            let instant = golden_instant(&case["input"]);
            let output = &case["output"];
            let published = output["published_record"]["published_at"]
                .as_str()
                .expect("published_at is a string");
            let store = output["store_timestamp"]
                .as_str()
                .expect("store_timestamp is a string");
            let reviewed = output["reviewed_at"]
                .as_str()
                .expect("reviewed_at is a string");
            assert_eq!(
                format_rfc3339_nano(instant),
                published,
                "{name}: published_at"
            );
            assert_eq!(
                format_rfc3339_nano(instant),
                store,
                "{name}: store_timestamp"
            );
            assert_eq!(
                format_rfc3339_seconds(instant),
                reviewed,
                "{name}: reviewed_at"
            );
            assert_eq!(
                parse_rfc3339(published).expect("golden parses"),
                instant,
                "{name}"
            );
            assert_eq!(
                parse_rfc3339(reviewed).expect("golden parses"),
                instant.replace_nanosecond(0).expect("whole seconds"),
                "{name}: reviewed_at"
            );
        }

        // Expected values below were produced by Go 1.27 time.Format, time.Parse and json.Marshal.
        let whole = OffsetDateTime::from_unix_timestamp(1_767_225_600).expect("valid timestamp");
        let truncated = whole + Duration::nanoseconds(999_999_999);
        assert_eq!(
            format_rfc3339_nano(truncated),
            "2026-01-01T00:00:00.999999999Z"
        );
        assert_eq!(format_rfc3339_seconds(truncated), "2026-01-01T00:00:00Z");

        let parsed_offset = parse_rfc3339("2026-01-01T02:00:00+02:00").expect("offset parses");
        assert_eq!(parsed_offset, whole);
        assert_eq!(format_rfc3339_nano(parsed_offset), "2026-01-01T00:00:00Z");
        let parsed_negative_zero =
            parse_rfc3339("2026-01-01T00:00:00-00:00").expect("-00:00 parses");
        assert_eq!(parsed_negative_zero, whole);

        let fraction = parse_rfc3339("2026-01-01T00:00:00.1234567899Z").expect("fraction parses");
        assert_eq!(
            format_rfc3339_nano(fraction),
            "2026-01-01T00:00:00.123456789Z"
        );

        let zero = parse_rfc3339("0001-01-01T00:00:00+00:00").expect("zero time parses");
        assert_eq!(format_rfc3339_nano(zero), "0001-01-01T00:00:00Z");

        let negative_year = Date::from_calendar_date(-1, Month::January, 1)
            .expect("valid date")
            .with_time(Time::MIDNIGHT)
            .assume_utc();
        assert_eq!(format_rfc3339_nano(negative_year), "-0001-01-01T00:00:00Z");
        let stamp = Stamp { at: negative_year };
        assert!(serde_json::to_string(&stamp).is_err());

        let stamp = Stamp {
            at: whole + Duration::nanoseconds(500_000_000),
        };
        assert_eq!(
            serde_json::to_string(&stamp).expect("serializes"),
            r#"{"at":"2026-01-01T00:00:00.5Z"}"#
        );
        let read: Stamp =
            serde_json::from_str(r#"{"at":"2026-01-01T02:00:00+02:00"}"#).expect("deserializes");
        assert_eq!(read.at, whole);

        for invalid in [
            "2026-01-01T00:00:00",
            "2026-02-30T00:00:00Z",
            "2026-01-01T23:59:60Z",
            "2026-01-01T00:00:00+0200",
        ] {
            assert!(
                matches!(parse_rfc3339(invalid), Err(Error::Parse { .. })),
                "{invalid} must be rejected"
            );
        }
    }
}
