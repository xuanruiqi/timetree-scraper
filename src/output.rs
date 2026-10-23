//! Write scraped events as JSON or iCalendar.

use std::fs;
use std::path::Path;

use chrono::{DateTime, Datelike, Days, FixedOffset, NaiveDate, NaiveDateTime, TimeDelta, Utc};
use icalendar::{Calendar, Component, Event, EventLike};
use serde_json::{Map, Value, json};

use crate::OutputFormat;
use crate::error::WriteError;

pub type EventPayload = Map<String, Value>;

/// Numeric timestamps larger than this are in milliseconds, not seconds.
/// It is the last second of the year 9999 UTC.
const MAX_TIMESTAMP_SECONDS: i64 = 253_402_300_799;

// Candidate field names for each event property, in order of preference,
// since the TimeTree payload's field names are not guaranteed.
const START_KEYS: &[&str] = &[
    "start_at",
    "starts_at",
    "start",
    "started_at",
    "start_time",
    "starts_on",
    "start_date",
];
const END_KEYS: &[&str] = &[
    "end_at", "ends_at", "end", "ended_at", "end_time", "ends_on", "end_date",
];
const ALL_DAY_KEYS: &[&str] = &["all_day", "is_all_day", "allDay", "full_day"];
const TITLE_KEYS: &[&str] = &["title", "name", "summary"];
const DESCRIPTION_KEYS: &[&str] = &["description", "note", "memo", "details", "body"];
const LOCATION_KEYS: &[&str] = &["location", "place", "place.name", "venue", "venue.name"];
const UID_KEYS: &[&str] = &["id", "uuid", "uid"];

/// A parsed start or end value: a calendar date, or a date-time that keeps
/// its original UTC offset so that its date part is the one the source meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DateValue {
    Date(NaiveDate),
    DateTime(DateTime<FixedOffset>),
}

impl DateValue {
    /// The calendar date, in the value's own offset for a date-time.
    fn date_part(self) -> NaiveDate {
        match self {
            Self::Date(date) => date,
            Self::DateTime(datetime) => datetime.date_naive(),
        }
    }

    /// The value as a UTC date-time, taking a date as midnight UTC.
    fn as_datetime(self) -> DateTime<Utc> {
        match self {
            Self::Date(date) => date.and_time(Default::default()).and_utc(),
            Self::DateTime(datetime) => datetime.to_utc(),
        }
    }
}

/// Write `events` to `output_file` in the given format.
pub fn write_events(
    output_file: &Path,
    events: &[EventPayload],
    format: OutputFormat,
) -> Result<(), WriteError> {
    let contents = match format {
        OutputFormat::Json => to_json(events)?,
        OutputFormat::Ics => to_ics(events),
    };
    fs::write(output_file, contents).map_err(|source| WriteError::Io {
        path: output_file.to_owned(),
        source,
    })
}

/// Serialize the raw events as `{"public_events": [...]}`.
fn to_json(events: &[EventPayload]) -> serde_json::Result<String> {
    serde_json::to_string_pretty(&json!({ "public_events": events }))
}

/// Convert the events to an iCalendar file.
///
/// Events with no parseable start time are skipped, and a message is printed.
fn to_ics(events: &[EventPayload]) -> String {
    let mut calendar = Calendar::new();
    for source_event in events {
        match to_ics_event(source_event) {
            Some(event) => {
                calendar.push(event);
            }
            None => {
                let id = source_event.get("id").map_or("<unknown>".into(), py_str);
                println!("Skipping event with no start time: {id}");
            }
        }
    }
    calendar.done().to_string()
}

/// Convert a TimeTree event to an iCalendar event.
///
/// Each field is read from the first matching name among several candidates
/// (see [`first_value`]). Returns `None` if the event has no parseable start
/// time.
///
/// - An event is all-day if it has an all-day flag or a date-only start.
/// - The title defaults to `"Untitled event"`. Description and location are
///   set only when present.
/// - The UID is `timetree-<id>`, so re-imports update existing entries
///   instead of duplicating them.
fn to_ics_event(source_event: &EventPayload) -> Option<Event> {
    let start = parse_datetime_value(first_value(source_event, START_KEYS))?;

    let mut location = first_value(source_event, LOCATION_KEYS);
    if let Some(Value::Object(place)) = location {
        location = first_value(place, &["name", "title", "address"]);
    }
    let title = first_value(source_event, TITLE_KEYS)
        .filter(|v| is_truthy(v))
        .map_or("Untitled event".into(), py_str);

    let mut event = Event::new();
    event.summary(&title);
    if let Some(description) = first_value(source_event, DESCRIPTION_KEYS).filter(|v| is_truthy(v))
    {
        event.description(&py_str(description));
    }
    if let Some(location) = location.filter(|v| is_truthy(v)) {
        event.location(&py_str(location));
    }
    if let Some(uid) = first_value(source_event, UID_KEYS).filter(|v| is_truthy(v)) {
        event.uid(&format!("timetree-{}", py_str(uid)));
    }
    set_event_times(
        &mut event,
        start,
        parse_datetime_value(first_value(source_event, END_KEYS)),
        first_value(source_event, ALL_DAY_KEYS).is_some_and(parse_bool_value),
    );
    Some(event.done())
}

/// Set the start and end of `event`.
///
/// An all-day event spans whole days up to its end date when that is later
/// than the start date. Otherwise it lasts a single day. A timed event with no
/// end, or with an end before its start, is given a one-hour duration.
fn set_event_times(event: &mut Event, start: DateValue, end: Option<DateValue>, all_day: bool) {
    match start {
        DateValue::DateTime(_) if !all_day => {
            let start = start.as_datetime();
            let end = end
                .map(DateValue::as_datetime)
                .filter(|end| *end >= start)
                .unwrap_or(start + TimeDelta::hours(1));
            event.starts(start).ends(end);
        }
        _ => {
            let start = start.date_part();
            let end = end
                .map(DateValue::date_part)
                .filter(|end| *end > start)
                .unwrap_or(start + Days::new(1));
            event.starts(start).ends(end);
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
fn first_value<'a>(mapping: &'a EventPayload, keys: &[&str]) -> Option<&'a Value> {
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
fn parse_datetime_value(value: Option<&Value>) -> Option<DateValue> {
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
fn parse_bool_value(value: &Value) -> bool {
    match value {
        Value::String(s) => matches!(s.trim().to_lowercase().as_str(), "1" | "true" | "yes" | "y"),
        other => is_truthy(other),
    }
}

/// Python-style truthiness: `null`, `false`, zero and empty strings, arrays
/// and objects are false.
fn is_truthy(value: &Value) -> bool {
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
fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.replace("\r\n", "\n").replace('\r', "\n"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(value: Value) -> EventPayload {
        value.as_object().unwrap().clone()
    }

    fn ics_lines(value: Value) -> Vec<String> {
        let event = to_ics_event(&payload(value)).unwrap();
        event.to_string().lines().map(str::to_owned).collect()
    }

    fn has(lines: &[String], line: &str) -> bool {
        lines.iter().any(|l| l == line)
    }

    #[test]
    fn first_value_skips_empty_and_follows_paths() {
        let event =
            payload(json!({"title": "", "name": null, "summary": "S", "place": {"name": "P"}}));
        assert_eq!(first_value(&event, TITLE_KEYS), Some(&json!("S")));
        assert_eq!(first_value(&event, &["place.name"]), Some(&json!("P")));
        assert_eq!(first_value(&event, &["missing", "place.missing"]), None);
        // `false` and `0` are present values, not missing ones.
        let event = payload(json!({"all_day": false}));
        assert_eq!(first_value(&event, ALL_DAY_KEYS), Some(&json!(false)));
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

    #[test]
    fn timed_event() {
        let lines = ics_lines(json!({
            "id": "abc",
            "title": "Meeting",
            "start_at": 1_772_359_200_000_i64,
            "end_at": 1_772_366_400_000_i64,
            "all_day": false,
            "description": "Notes",
            "location": "Tokyo",
        }));
        assert!(has(&lines, "UID:timetree-abc"));
        assert!(has(&lines, "SUMMARY:Meeting"));
        assert!(has(&lines, "DESCRIPTION:Notes"));
        assert!(has(&lines, "LOCATION:Tokyo"));
        assert!(has(&lines, "DTSTART:20260301T100000Z"));
        assert!(has(&lines, "DTEND:20260301T120000Z"));
    }

    #[test]
    fn timed_event_without_end_lasts_one_hour() {
        let lines = ics_lines(json!({"start_at": "2026-03-01T10:00:00+09:00"}));
        assert!(has(&lines, "SUMMARY:Untitled event"));
        assert!(has(&lines, "DTSTART:20260301T010000Z"));
        assert!(has(&lines, "DTEND:20260301T020000Z"));
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("DESCRIPTION") || l.starts_with("LOCATION"))
        );
    }

    #[test]
    fn all_day_events() {
        // Single day, flagged all-day; the date is taken in the value's own offset.
        let lines = ics_lines(json!({"start_at": "2026-03-01T00:00:00+09:00", "all_day": true}));
        assert!(has(&lines, "DTSTART;VALUE=DATE:20260301"));
        assert!(has(&lines, "DTEND;VALUE=DATE:20260302"));

        // Multi-day, from a date-only start.
        let lines = ics_lines(json!({"start_at": "2026-03-01", "end_at": "2026-03-04"}));
        assert!(has(&lines, "DTSTART;VALUE=DATE:20260301"));
        assert!(has(&lines, "DTEND;VALUE=DATE:20260304"));
    }

    #[test]
    fn nested_location_and_numeric_id() {
        let lines = ics_lines(json!({
            "id": 42,
            "start_at": "2026-03-01",
            "place": {"title": "", "address": "1-1 Chiyoda"},
        }));
        assert!(has(&lines, "UID:timetree-42"));
        assert!(has(&lines, "LOCATION:1-1 Chiyoda"));
    }

    #[test]
    fn carriage_returns_become_escaped_newlines() {
        let lines = ics_lines(json!({"start_at": "2026-03-01", "note": "a\r\nb\rc"}));
        assert!(has(&lines, r"DESCRIPTION:a\nb\nc"));
    }

    #[test]
    fn skips_events_without_start() {
        assert!(to_ics_event(&payload(json!({"id": "x", "title": "No start"}))).is_none());
        let ics = to_ics(&[
            payload(json!({"id": "x"})),
            payload(json!({"start_at": "2026-03-01"})),
        ]);
        assert_eq!(ics.matches("BEGIN:VEVENT").count(), 1);
    }

    #[test]
    fn json_output_keeps_raw_events() {
        let events = [payload(json!({"z": 1, "a": "予定"}))];
        let out: Value = serde_json::from_str(&to_json(&events).unwrap()).unwrap();
        assert_eq!(out, json!({"public_events": [{"z": 1, "a": "予定"}]}));
    }
}
