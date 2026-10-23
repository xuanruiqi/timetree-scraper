//! Helpers for reading values out of TimeTree's JSON event payloads.

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, Utc};
use serde_json::Value;

use super::EventPayload;

/// Numeric timestamps larger than this are in milliseconds, not seconds.
/// It is the last second of the year 9999 UTC.
const MAX_TIMESTAMP_SECONDS: i64 = 253_402_300_799;

/// A parsed start or end value: a calendar date, or a date-time that keeps
/// its original UTC offset so that its date part is the one the source meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DateValue {
    Date(NaiveDate),
    DateTime(DateTime<FixedOffset>),
}

impl DateValue {
    /// The calendar date, in the value's own offset for a date-time.
    pub(super) fn date_part(self) -> NaiveDate {
        match self {
            Self::Date(date) => date,
            Self::DateTime(datetime) => datetime.date_naive(),
        }
    }

    /// The value as a UTC date-time, taking a date as midnight UTC.
    pub(super) fn as_datetime(self) -> DateTime<Utc> {
        match self {
            Self::Date(date) => date.and_time(Default::default()).and_utc(),
            Self::DateTime(datetime) => datetime.to_utc(),
        }
    }
}

/// Return the value of the first key in `keys` that is present and non-empty.
///
/// Keys may be dotted paths (e.g. `"place.name"`) to look inside nested
/// objects. A key counts as missing if any part of its path is absent or its
/// value is `null` or `""`. Returns `None` if no key matches.
///
/// This lets callers list several possible field names, since the TimeTree
/// payload's field names are not guaranteed.
pub(super) fn first_value<'a>(mapping: &'a EventPayload, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| {
        let mut parts = key.split('.');
        let first = mapping.get(parts.next()?)?;
        let value = parts.try_fold(first, |value, part| value.as_object()?.get(part))?;
        match value {
            Value::Null => None,
            Value::String(s) if s.is_empty() => None,
            _ => Some(value),
        }
    })
}

/// Convert an event's start or end value to a [`DateValue`].
///
/// ISO 8601 strings containing `T` become a date-time (UTC if they have no
/// offset), and other strings are parsed as a date. Numbers are Unix
/// timestamps in seconds or milliseconds and become a UTC date-time. Returns
/// `None` for empty or zero values, values that fail to parse or fall outside
/// the years 1 to 9999, and any other type.
pub(super) fn parse_datetime_value(value: Option<&Value>) -> Option<DateValue> {
    let parsed = match value? {
        Value::String(s) if s.contains('T') => parse_iso_datetime(s).map(DateValue::DateTime),
        Value::String(s) if !s.is_empty() => parse_iso_date(s).map(DateValue::Date),
        Value::Number(n) => parse_timestamp(n).map(|dt| DateValue::DateTime(dt.fixed_offset())),
        _ => None,
    }?;
    let year = match parsed {
        DateValue::Date(date) => date.year_ce(),
        DateValue::DateTime(datetime) => datetime.to_utc().date_naive().year_ce(),
    };
    (year.0 && (1..=9999).contains(&year.1)).then_some(parsed)
}

fn parse_iso_datetime(s: &str) -> Option<DateTime<FixedOffset>> {
    const WITH_OFFSET: &[&str] = &[
        "%Y-%m-%dT%H:%M:%S%.f%:z",
        "%Y-%m-%dT%H:%M%:z",
        "%Y-%m-%dT%H:%M:%S%.f%z",
    ];
    const WITHOUT_OFFSET: &[&str] = &["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"];

    let s = s.replace('Z', "+00:00");
    WITH_OFFSET
        .iter()
        .find_map(|format| DateTime::parse_from_str(&s, format).ok())
        .or_else(|| {
            WITHOUT_OFFSET
                .iter()
                .find_map(|format| NaiveDateTime::parse_from_str(&s, format).ok())
                .map(|naive| naive.and_utc().fixed_offset())
        })
}

fn parse_iso_date(s: &str) -> Option<NaiveDate> {
    ["%Y-%m-%d", "%Y%m%d"]
        .iter()
        .find_map(|format| NaiveDate::parse_from_str(s, format).ok())
}

fn parse_timestamp(n: &serde_json::Number) -> Option<DateTime<Utc>> {
    if let Some(i) = n.as_i64() {
        return match i {
            0 => None,
            i if i > MAX_TIMESTAMP_SECONDS => DateTime::from_timestamp_millis(i),
            i => DateTime::from_timestamp(i, 0),
        };
    }
    let f = n.as_f64()?;
    if f == 0.0 {
        return None;
    }
    let seconds = if f > MAX_TIMESTAMP_SECONDS as f64 {
        f / 1000.0
    } else {
        f
    };
    let micros = (seconds * 1_000_000.0).round();
    if !micros.is_finite() || micros.abs() >= i64::MAX as f64 {
        return None;
    }
    DateTime::from_timestamp_micros(micros as i64)
}

/// Interpret a JSON value as a boolean flag.
///
/// Strings are true only if they are `"1"`, `"true"`, `"yes"` or `"y"`
/// (case-insensitive, surrounding whitespace ignored). Other values use
/// Python-style truthiness, as the original scraper did.
pub(super) fn parse_bool_value(value: &Value) -> bool {
    match value {
        Value::String(s) => matches!(s.trim().to_lowercase().as_str(), "1" | "true" | "yes" | "y"),
        other => is_truthy(other),
    }
}

/// Python-style truthiness: `null`, `false`, zero and empty strings, arrays
/// and objects are false.
pub(super) fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Render a JSON value as text: strings as-is, anything else as JSON.
///
/// Line breaks are normalized to `\n`, since icalendar escapes `\n` but
/// writes `\r` raw, which breaks the content line it appears in.
pub(super) fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.replace("\r\n", "\n").replace('\r', "\n"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::payload;
    use serde_json::json;

    #[test]
    fn first_value_skips_empty_and_follows_paths() {
        let event =
            payload(json!({"title": "", "name": null, "summary": "S", "place": {"name": "P"}}));
        assert_eq!(
            first_value(&event, &["title", "name", "summary"]),
            Some(&json!("S"))
        );
        assert_eq!(first_value(&event, &["place.name"]), Some(&json!("P")));
        assert_eq!(first_value(&event, &["missing", "place.missing"]), None);
        // `false` and `0` are present values, not missing ones.
        let event = payload(json!({"all_day": false}));
        assert_eq!(first_value(&event, &["all_day"]), Some(&json!(false)));
    }

    #[test]
    fn parses_strings_and_timestamps() {
        let utc = |s: &str| DateValue::DateTime(DateTime::parse_from_rfc3339(s).unwrap());
        let parse = |v: Value| parse_datetime_value(Some(&v));

        assert_eq!(
            parse(json!("2026-03-01T10:00:00Z")),
            Some(utc("2026-03-01T10:00:00+00:00"))
        );
        assert_eq!(
            parse(json!("2026-03-01T10:00:00+09:00")),
            Some(utc("2026-03-01T10:00:00+09:00"))
        );
        assert_eq!(
            parse(json!("2026-03-01T10:00")),
            Some(utc("2026-03-01T10:00:00+00:00"))
        );
        assert_eq!(
            parse(json!("2026-03-01")),
            Some(DateValue::Date(
                NaiveDate::from_ymd_opt(2026, 3, 1).unwrap()
            ))
        );
        assert_eq!(
            parse(json!(1_772_359_200)),
            Some(utc("2026-03-01T10:00:00+00:00"))
        );
        assert_eq!(
            parse(json!(1_772_359_200_000_i64)),
            Some(utc("2026-03-01T10:00:00+00:00"))
        );
        assert_eq!(
            parse(json!(1_772_359_200.5)),
            Some(utc("2026-03-01T10:00:00.5+00:00"))
        );

        for invalid in [
            json!(0),
            json!(""),
            json!("nope"),
            json!(true),
            json!(null),
            json!([1]),
        ] {
            assert_eq!(parse(invalid.clone()), None, "{invalid}");
        }
    }

    #[test]
    fn parses_bool_flags() {
        for truthy in [
            json!(true),
            json!(1),
            json!(" Yes "),
            json!("TRUE"),
            json!("y"),
        ] {
            assert!(parse_bool_value(&truthy), "{truthy}");
        }
        for falsy in [
            json!(false),
            json!(0),
            json!("no"),
            json!(""),
            json!(null),
            json!([]),
        ] {
            assert!(!parse_bool_value(&falsy), "{falsy}");
        }
    }
}
