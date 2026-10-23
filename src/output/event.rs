//! The calendar fields of a TimeTree event, shared by the ICS and CSV writers
//! and the Google Calendar sync.

use chrono::{DateTime, Days, NaiveDate, TimeDelta, Utc};
use serde_json::Value;

use super::EventPayload;
use super::util::{
    DateValue, first_value, is_truthy, parse_bool_value, parse_datetime_value, py_str,
};

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

/// When an event happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventTimes {
    Timed {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },
    /// Whole days from `start` up to, but not including, `end`.
    AllDay { start: NaiveDate, end: NaiveDate },
}

/// The parts of a TimeTree event that calendar files keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub uid: Option<String>,
    pub times: EventTimes,
}

/// Parse the events, skipping those with no parseable start time and
/// printing a message for each.
pub fn parse_events(events: &[EventPayload]) -> Vec<CalendarEvent> {
    events
        .iter()
        .filter_map(|source_event| {
            let event = parse_event(source_event);
            if event.is_none() {
                let id = source_event.get("id").map_or("<unknown>".into(), py_str);
                println!("Skipping event with no start time: {id}");
            }
            event
        })
        .collect()
}

/// Read the calendar fields of a TimeTree event.
///
/// Each field is read from the first matching name among several candidates
/// (see [`first_value`]). Returns `None` if the event has no parseable start
/// time.
///
/// - An event is all-day if it has an all-day flag or a date-only start.
/// - The title defaults to `"Untitled event"`.
/// - The UID is `timetree-<id>`, so re-imports update existing entries
///   instead of duplicating them.
pub fn parse_event(source_event: &EventPayload) -> Option<CalendarEvent> {
    let start = parse_datetime_value(first_value(source_event, START_KEYS))?;

    let mut location = first_value(source_event, LOCATION_KEYS);
    if let Some(Value::Object(place)) = location {
        location = first_value(place, &["name", "title", "address"]);
    }
    let text = |value: Option<&Value>| value.filter(|v| is_truthy(v)).map(py_str);

    Some(CalendarEvent {
        title: text(first_value(source_event, TITLE_KEYS)).unwrap_or("Untitled event".into()),
        description: text(first_value(source_event, DESCRIPTION_KEYS)),
        location: text(location),
        uid: text(first_value(source_event, UID_KEYS)).map(|id| format!("timetree-{id}")),
        times: event_times(
            start,
            parse_datetime_value(first_value(source_event, END_KEYS)),
            first_value(source_event, ALL_DAY_KEYS).is_some_and(parse_bool_value),
        ),
    })
}

/// Work out when an event happens.
///
/// An all-day event spans whole days up to its end date when that is later
/// than the start date. Otherwise it lasts a single day. A timed event with no
/// end, or with an end before its start, is given a one-hour duration.
fn event_times(start: DateValue, end: Option<DateValue>, all_day: bool) -> EventTimes {
    match start {
        DateValue::DateTime(_) if !all_day => {
            let start = start.as_datetime();
            let end = end
                .map(DateValue::as_datetime)
                .filter(|end| *end >= start)
                .unwrap_or(start + TimeDelta::hours(1));
            EventTimes::Timed { start, end }
        }
        _ => {
            let start = start.date_part();
            let end = end
                .map(DateValue::date_part)
                .filter(|end| *end > start)
                .unwrap_or(start + Days::new(1));
            EventTimes::AllDay { start, end }
        }
    }
}
