//! iCalendar output.

use icalendar::{Calendar, Component, Event, EventLike};

use super::EventPayload;
use super::event::{CalendarEvent, EventTimes, parse_events};

/// Convert the events to an iCalendar file.
///
/// Events with no parseable start time are skipped, and a message is printed.
pub(super) fn to_ics(events: &[EventPayload]) -> String {
    let mut calendar = Calendar::new();
    for event in parse_events(events) {
        calendar.push(to_ics_event(&event));
    }
    calendar.done().to_string()
}

/// Convert an event to an iCalendar event.
///
/// Description, location and UID are set only when present.
fn to_ics_event(source_event: &CalendarEvent) -> Event {
    let mut event = Event::new();
    event.summary(&source_event.title);
    if let Some(description) = &source_event.description {
        event.description(description);
    }
    if let Some(location) = &source_event.location {
        event.location(location);
    }
    if let Some(uid) = &source_event.uid {
        event.uid(uid);
    }
    match source_event.times {
        EventTimes::Timed { start, end } => event.starts(start).ends(end),
        EventTimes::AllDay { start, end } => event.starts(start).ends(end),
    };
    event.done()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::event::parse_event;
    use crate::output::payload;
    use serde_json::{Value, json};

    fn ics_lines(value: Value) -> Vec<String> {
        let event = to_ics_event(&parse_event(&payload(value)).unwrap());
        event.to_string().lines().map(str::to_owned).collect()
    }

    fn has(lines: &[String], line: &str) -> bool {
        lines.iter().any(|l| l == line)
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
        assert!(parse_event(&payload(json!({"id": "x", "title": "No start"}))).is_none());
        let ics = to_ics(&[
            payload(json!({"id": "x"})),
            payload(json!({"start_at": "2026-03-01"})),
        ]);
        assert_eq!(ics.matches("BEGIN:VEVENT").count(), 1);
    }
}
