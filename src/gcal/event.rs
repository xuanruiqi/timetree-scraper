//! Google Calendar API event resources.

use chrono::SecondsFormat;
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
