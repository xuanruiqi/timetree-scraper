//! Sync scraped events into a Google Calendar.

mod auth;
mod client;
mod event;

use std::collections::HashMap;

use chrono::{DateTime, NaiveTime, TimeDelta, Utc};
use futures::{StreamExt, stream};
use serde_json::Value;

use crate::error::GcalError;
use crate::output::EventPayload;
use crate::output::event::{CalendarEvent, EventTimes, parse_events};
use client::{CalendarListEntry, Client, Upsert};
use event::{event_id, is_unchanged, to_gcal_event};

/// How many events to write at once. Google allows each user 600 requests
/// a minute, so this mainly hides the latency of each request, and rate
/// limited requests are retried.
const CONCURRENT_WRITES: usize = 8;

/// A Google Calendar that the user has signed in to and can add events to.
pub struct GoogleCalendar {
    client: Client,
    id: String,
}

/// How many events a sync created, updated, found already up to date and
/// failed to write.
#[derive(Debug, Default)]
pub struct SyncSummary {
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub failed: usize,
}

impl GoogleCalendar {
    /// Sign in to Google, prompting the user if needed, and find the
    /// calendar named `name` among those the user can edit, creating it if
    /// there is none.
    pub async fn connect(name: &str) -> Result<Self, GcalError> {
        let client = Client::new(auth::authenticate().await?);
        let id = match find_calendar(client.writable_calendars().await?, name)? {
            Some(id) => id,
            None => {
                let id = client.create_calendar(name).await?;
                println!("Created Google Calendar \"{name}\"");
                id
            }
        };
        Ok(Self { client, id })
    }

    /// Write the events to the calendar.
    ///
    /// Each event's ID is derived from its TimeTree ID, so syncing again
    /// updates the events instead of duplicating them, and events that are
    /// already up to date are left alone. Events with no TimeTree ID are
    /// created each time. Events with no parseable start time are skipped,
    /// and a message is printed for them and for each event that fails to
    /// sync.
    pub async fn sync(&self, events: &[EventPayload]) -> SyncSummary {
        let mut summary = SyncSummary::default();
        let events = parse_events(events);
        let Some(time_min) = earliest_time(&events) else {
            return summary;
        };
        // Listing the existing events up front finds those that need no
        // write, and whether the rest need creating or updating. If it
        // fails, every event is written, which is slower but still correct.
        let existing = self
            .client
            .list_events(&self.id, time_min)
            .await
            .unwrap_or_else(|e| {
                let e = anyhow::Error::new(e);
                println!("Failed to list the calendar's events, so all will be written: {e:#}");
                HashMap::new()
            });
        let existing = &existing;

        let mut results = stream::iter(&events)
            .map(|event| async move { (event, self.sync_event(event, existing).await) })
            .buffer_unordered(CONCURRENT_WRITES);
        while let Some((event, result)) = results.next().await {
            match result {
                Ok(Some(Upsert::Created)) => summary.created += 1,
                Ok(Some(Upsert::Updated)) => summary.updated += 1,
                Ok(None) => summary.unchanged += 1,
                Err(e) => {
                    summary.failed += 1;
                    // Go through `anyhow` to print the whole chain of causes.
                    let e = anyhow::Error::new(e);
                    println!("Failed to sync event \"{}\": {e:#}", event.title);
                }
            }
        }
        summary
    }

    /// Write one event, given the calendar's `existing` events by ID.
    /// Returns `None` if the event was already up to date.
    async fn sync_event(
        &self,
        event: &CalendarEvent,
        existing: &HashMap<String, Value>,
    ) -> Result<Option<Upsert>, GcalError> {
        let resource = to_gcal_event(event);
        let Some(event_id) = event.uid.as_deref().and_then(event_id) else {
            self.client.insert_event(&self.id, &resource).await?;
            return Ok(Some(Upsert::Created));
        };
        match existing.get(&event_id) {
            Some(current) if is_unchanged(current, &resource) => Ok(None),
            current => self
                .client
                .write_event(&self.id, &event_id, &resource, current.is_some())
                .await
                .map(Some),
        }
    }
}

/// The earliest start among `events`, less a day so that no all-day event
/// is missed whatever the calendar's time zone, or `None` if there are no
/// events.
///
/// Listing only events that end after this keeps the list short in a
/// calendar with a long history. An event whose old version in Google
/// Calendar ended earlier is missed, but is still updated correctly, just
/// with an extra request.
fn earliest_time(events: &[CalendarEvent]) -> Option<DateTime<Utc>> {
    let start = |event: &CalendarEvent| match event.times {
        EventTimes::Timed { start, .. } => start,
        EventTimes::AllDay { start, .. } => start.and_time(NaiveTime::MIN).and_utc(),
    };
    let earliest = events.iter().map(start).min()?;
    Some(earliest - TimeDelta::days(1))
}

/// The ID of the calendar named `name`, or `None` if there is none, matching
/// the name the user sees: their own name for the calendar if they gave it
/// one, otherwise its title.
fn find_calendar(
    calendars: Vec<CalendarListEntry>,
    name: &str,
) -> Result<Option<String>, GcalError> {
    let display_name = |calendar: &CalendarListEntry| {
        calendar
            .summary_override
            .clone()
            .unwrap_or_else(|| calendar.summary.clone())
    };
    let mut matches: Vec<_> = calendars
        .iter()
        .filter(|calendar| display_name(calendar) == name)
        .collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches.remove(0).id.clone())),
        count => Err(GcalError::AmbiguousCalendar {
            name: name.to_owned(),
            count,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calendar(id: &str, summary: &str, summary_override: Option<&str>) -> CalendarListEntry {
        CalendarListEntry {
            id: id.to_owned(),
            summary: summary.to_owned(),
            summary_override: summary_override.map(str::to_owned),
        }
    }

    fn calendars() -> Vec<CalendarListEntry> {
        vec![
            calendar("a", "Work", None),
            calendar("b", "Shared title", Some("Family")),
            calendar("c", "Club", None),
            calendar("d", "Club", None),
        ]
    }

    #[test]
    fn earliest_time_covers_all_events() {
        use crate::output::event::parse_event;
        use crate::output::payload;
        use serde_json::json;

        let events: Vec<_> = [
            json!({"start_at": "2026-03-05T10:00:00+09:00"}),
            json!({"start_at": "2026-03-03"}),
        ]
        .into_iter()
        .map(|value| parse_event(&payload(value)).unwrap())
        .collect();
        let expected = DateTime::parse_from_rfc3339("2026-03-02T00:00:00Z").unwrap();
        assert_eq!(earliest_time(&events), Some(expected.to_utc()));
        assert_eq!(earliest_time(&[]), None);
    }

    #[test]
    fn finds_calendar_by_display_name() {
        assert_eq!(
            find_calendar(calendars(), "Work").unwrap().as_deref(),
            Some("a")
        );
        assert_eq!(
            find_calendar(calendars(), "Family").unwrap().as_deref(),
            Some("b")
        );
    }

    #[test]
    fn missing_and_ambiguous_names() {
        // A title hidden behind the user's own name for the calendar does
        // not match.
        assert_eq!(find_calendar(calendars(), "Shared title").unwrap(), None);
        assert!(matches!(
            find_calendar(calendars(), "Club"),
            Err(GcalError::AmbiguousCalendar { count: 2, .. })
        ));
    }
}
