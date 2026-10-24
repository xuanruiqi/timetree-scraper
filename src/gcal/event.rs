//! Google Calendar API event resources.

use google_calendar3::api::{Event, EventDateTime};

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
/// An update replaces the whole event, so fields left out here, including
/// ones removed in TimeTree, are cleared. The status is always `confirmed`,
/// so that an update restores an event that was deleted in Google Calendar.
pub(super) fn to_gcal_event(event: &CalendarEvent) -> Event {
    let (start, end) = match event.times {
        EventTimes::Timed { start, end } => (
            EventDateTime {
                date_time: Some(start),
                ..Default::default()
            },
            EventDateTime {
                date_time: Some(end),
                ..Default::default()
            },
        ),
        EventTimes::AllDay { start, end } => (
            EventDateTime {
                date: Some(start),
                ..Default::default()
            },
            EventDateTime {
                date: Some(end),
                ..Default::default()
            },
        ),
    };
    Event {
        summary: Some(event.title.clone()),
        description: event.description.clone(),
        location: event.location.clone(),
        start: Some(start),
        end: Some(end),
        status: Some("confirmed".to_owned()),
        ..Default::default()
    }
}

/// Whether `existing`, an event as Google lists it, already matches
/// `event`, a resource from [`to_gcal_event`], so writing it would change
/// nothing.
///
/// Google leaves out empty text fields, so missing and empty text count as
/// the same. Times are compared as dates or instants, ignoring the time
/// zone Google lists them in.
pub(super) fn is_unchanged(existing: &Event, event: &Event) -> bool {
    let text = |value: &Option<String>| value.clone().filter(|text| !text.is_empty());
    let time = |time: &Option<EventDateTime>| time.as_ref().map(|time| (time.date, time.date_time));
    existing.status == event.status
        && text(&existing.summary) == text(&event.summary)
        && text(&existing.description) == text(&event.description)
        && text(&existing.location) == text(&event.location)
        && time(&existing.start) == time(&event.start)
        && time(&existing.end) == time(&event.end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::event::parse_event;
    use crate::output::payload;
    use google_calendar3::common::remove_json_null_values;
    use serde_json::{Value, json};

    fn gcal_event(value: Value) -> Event {
        to_gcal_event(&parse_event(&payload(value)).unwrap())
    }

    /// The JSON body sent for an event, from which nulls are removed.
    fn request_body(event: &Event) -> Value {
        let mut body = serde_json::to_value(event).unwrap();
        remove_json_null_values(&mut body);
        body
    }

    /// An event as Google lists it.
    fn listed(value: Value) -> Event {
        serde_json::from_value(value).unwrap()
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
            request_body(&event),
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
        assert_eq!(
            request_body(&event),
            json!({
                "summary": "Untitled event",
                "start": {"date": "2026-03-01"},
                "end": {"date": "2026-03-04"},
                "status": "confirmed",
            })
        );
    }

    #[test]
    fn unchanged_events_match_as_google_lists_them() {
        let event = gcal_event(json!({
            "title": "Meeting",
            "start_at": "2026-03-01T10:00:00Z",
            "end_at": "2026-03-01T12:00:00Z",
            "location": "Tokyo",
        }));
        let same = json!({
            "id": "abc",
            "status": "confirmed",
            "summary": "Meeting",
            "location": "Tokyo",
            "start": {"dateTime": "2026-03-01T19:00:00+09:00", "timeZone": "Asia/Tokyo"},
            "end": {"dateTime": "2026-03-01T21:00:00+09:00", "timeZone": "Asia/Tokyo"},
        });
        assert!(is_unchanged(&listed(same.clone()), &event));

        let mut moved = same.clone();
        moved["end"]["dateTime"] = json!("2026-03-01T22:00:00+09:00");
        assert!(!is_unchanged(&listed(moved), &event));

        let mut described = same.clone();
        described["description"] = json!("Notes");
        assert!(!is_unchanged(&listed(described), &event));

        // Deleted events are listed with little more than their status.
        let deleted = json!({"id": "abc", "status": "cancelled"});
        assert!(!is_unchanged(&listed(deleted), &event));
    }

    #[test]
    fn all_day_and_timed_events_differ() {
        let all_day = gcal_event(json!({"start_at": "2026-03-01"}));
        let same = listed(json!({
            "status": "confirmed",
            "summary": "Untitled event",
            "start": {"date": "2026-03-01"},
            "end": {"date": "2026-03-02"},
        }));
        assert!(is_unchanged(&same, &all_day));

        let timed = gcal_event(json!({"start_at": "2026-03-01T00:00:00Z"}));
        assert!(!is_unchanged(&same, &timed));
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
