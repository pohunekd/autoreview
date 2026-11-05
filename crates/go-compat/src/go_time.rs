//! Go's `time.Time` text forms: RFC 3339 in UTC with Go's fraction rules.

// Rust guideline compliant 2026-06-26

use serde::{Deserialize, Deserializer, Serializer};
use time::{OffsetDateTime, UtcOffset};

use crate::Error;

/// Go's JSON encoder rejects years outside `0..=GO_MAX_JSON_YEAR` and only `serialize` applies the bound.
const GO_MAX_JSON_YEAR: i32 = 9999;

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
/// # Errors
///
/// Returns `Error::Parse` when `text` is not RFC 3339 with an offset.
pub fn parse_rfc3339(text: &str) -> Result<OffsetDateTime, Error> {
    match OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339) {
        Ok(parsed) => Ok(parsed.to_offset(UtcOffset::UTC)),
        Err(source) => Err(Error::Parse {
            input: text.to_owned(),
            source,
        }),
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
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<OffsetDateTime, D::Error> {
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
