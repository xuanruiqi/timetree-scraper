//! Sync scraped events into a Google Calendar.

mod auth;
mod client;
mod event;

use crate::error::GcalError;
use crate::output::EventPayload;
use crate::output::event::parse_events;
use client::{CalendarListEntry, Client, Upsert};
use event::{event_id, to_gcal_event};

/// A Google Calendar that the user has signed in to and can add events to.
pub struct GoogleCalendar {
    client: Client,
    id: String,
}

/// How many events a sync created, updated and failed to write.
#[derive(Debug, Default)]
pub struct SyncSummary {
    pub created: usize,
    pub updated: usize,
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
    /// updates the events instead of duplicating them. Events with no
    /// TimeTree ID are created each time. Events with no parseable start time
    /// are skipped, and a message is printed for them and for each event that
    /// fails to sync.
    pub async fn sync(&self, events: &[EventPayload]) -> SyncSummary {
        let mut summary = SyncSummary::default();
        for event in parse_events(events) {
            let resource = to_gcal_event(&event);
            let result = match event.uid.as_deref().and_then(event_id) {
                Some(event_id) => {
                    self.client
                        .upsert_event(&self.id, &event_id, &resource)
                        .await
                }
                None => self
                    .client
                    .insert_event(&self.id, &resource)
                    .await
                    .map(|()| Upsert::Created),
            };
            match result {
                Ok(Upsert::Created) => summary.created += 1,
                Ok(Upsert::Updated) => summary.updated += 1,
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
