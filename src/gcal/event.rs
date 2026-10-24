//! Google Calendar API event resources.

use chrono::{DateTime, SecondsFormat};
use serde_json::{Value, json};

use crate::output::event::{CalendarEvent, EventTimes};

/// Google limits event IDs to this many characters.
const MAX_EVENT_ID_LEN: usize = 1024;

/// The Google Calendar event ID for an event's UID, or `None` if the UID is
/// too long to make one.
///
/// Event IDs may contain only the base32hex digits `0-9` and `a-v`, so the
/// UID's bytes are written in lowercase hex. The same UID always gives the
/// same ID, which lets a sync find and update the events it created before.
pub(super) fn event_id(uid: &str) -> Option<String> {
    let id: String = uid.bytes().map(|byte| format!("{byte:02x}")).collect();
    (id.len() <= MAX_EVENT_ID_LEN).then_some(id)
}

/// Convert an event to a Google Calendar event resource.
///
/// Every field is written, absent ones as `null`, so that an update clears
/// fields removed in TimeTree. The status is always `confirmed`, so that an
/// update restores an event that was deleted in Google Calendar.
pub(super) fn to_gcal_event(event: &CalendarEvent) -> Value {
    let (start, end) = match event.times {
        EventTimes::Timed { start, end } => (
            json!({"dateTime": start.to_rfc3339_opts(SecondsFormat::AutoSi, true)}),
            json!({"dateTime": end.to_rfc3339_opts(SecondsFormat::AutoSi, true)}),
        ),
        EventTimes::AllDay { start, end } => (
            json!({"date": start.to_string()}),
            json!({"date": end.to_string()}),
        ),
    };
    json!({
        "summary": event.title,
        "description": event.description,
        "location": event.location,
        "start": start,
        "end": end,
        "status": "confirmed",
    })
}

/// Whether `existing`, an event as Google lists it, already matches
/// `event`, a resource from [`to_gcal_event`], so writing it would change
/// nothing.
///
/// Google leaves out empty text fields, and gives times in the calendar's
/// time zone, so text is compared treating missing and empty alike, and
/// times are compared as instants.
pub(super) fn is_unchanged(existing: &Value, event: &Value) -> bool {
    existing["status"] == event["status"]
        && ["summary", "description", "location"]
            .iter()
            .all(|field| text(&existing[field]) == text(&event[field]))
        && same_time(&existing["start"], &event["start"])
        && same_time(&existing["end"], &event["end"])
}

fn text(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| !text.is_empty())
}

/// Whether two event times are the same date, or the same instant.
fn same_time(a: &Value, b: &Value) -> bool {
    let instant = |time: &Value| {
        time["dateTime"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
    };
    match (a["date"].as_str(), b["date"].as_str()) {
        (Some(a), Some(b)) => a == b,
        (None, None) => instant(a).is_some() && instant(a) == instant(b),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::event::parse_event;
    use crate::output::payload;

    fn gcal_event(value: Value) -> Value {
        to_gcal_event(&parse_event(&payload(value)).unwrap())
    }

    #[test]
    fn timed_event() {
        let event = gcal_event(json!({
            "title": "Meeting",
            "start_at": 1_772_359_200_000_i64,
            "end_at": 1_772_366_400_000_i64,
            "description": "Notes",
            "location": "Tokyo",
        }));
        assert_eq!(
            event,
            json!({
                "summary": "Meeting",
                "description": "Notes",
                "location": "Tokyo",
                "start": {"dateTime": "2026-03-01T10:00:00Z"},
                "end": {"dateTime": "2026-03-01T12:00:00Z"},
                "status": "confirmed",
            })
        );
    }

    #[test]
    fn all_day_event_end_is_exclusive() {
        let event = gcal_event(json!({"start_at": "2026-03-01", "end_at": "2026-03-04"}));
        assert_eq!(event["start"], json!({"date": "2026-03-01"}));
        assert_eq!(event["end"], json!({"date": "2026-03-04"}));
        assert_eq!(event["description"], Value::Null);
        assert_eq!(event["location"], Value::Null);
    }

    #[test]
    fn unchanged_events_match_as_google_lists_them() {
        let event = gcal_event(json!({
            "title": "Meeting",
            "start_at": "2026-03-01T10:00:00Z",
            "end_at": "2026-03-01T12:00:00Z",
            "location": "Tokyo",
        }));
        let listed = json!({
            "id": "abc",
            "status": "confirmed",
            "summary": "Meeting",
            "location": "Tokyo",
            "start": {"dateTime": "2026-03-01T19:00:00+09:00", "timeZone": "Asia/Tokyo"},
            "end": {"dateTime": "2026-03-01T21:00:00+09:00", "timeZone": "Asia/Tokyo"},
        });
        assert!(is_unchanged(&listed, &event));

        let mut moved = listed.clone();
        moved["end"]["dateTime"] = json!("2026-03-01T22:00:00+09:00");
        assert!(!is_unchanged(&moved, &event));

        let mut described = listed.clone();
        described["description"] = json!("Notes");
        assert!(!is_unchanged(&described, &event));

        // Deleted events are listed with little more than their status.
        let deleted = json!({"id": "abc", "status": "cancelled"});
        assert!(!is_unchanged(&deleted, &event));
    }

    #[test]
    fn all_day_and_timed_events_differ() {
        let all_day = gcal_event(json!({"start_at": "2026-03-01"}));
        let listed = json!({
            "status": "confirmed",
            "summary": "Untitled event",
            "start": {"date": "2026-03-01"},
            "end": {"date": "2026-03-02"},
        });
        assert!(is_unchanged(&listed, &all_day));

        let timed = gcal_event(json!({"start_at": "2026-03-01T00:00:00Z"}));
        assert!(!is_unchanged(&listed, &timed));
    }

    #[test]
    fn event_ids_are_base32hex() {
        assert_eq!(
            event_id("timetree-Ab_9").unwrap(),
            "74696d65747265652d41625f39"
        );
        let id = event_id("timetree-予定").unwrap();
        assert!(id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='v')));
        assert_eq!(event_id(&"x".repeat(512)).unwrap().len(), 1024);
        assert_eq!(event_id(&"x".repeat(513)), None);
    }
}
