//! Write scraped events as JSON, iCalendar or Google Calendar CSV.

mod csv;
pub mod event;
mod ics;
mod json;
mod util;

use std::fs;
use std::path::Path;

use chrono_tz::Tz;
use serde_json::{Map, Value};

use crate::OutputFormat;
use crate::error::WriteError;

pub type EventPayload = Map<String, Value>;

/// Write `events` to `output_file` in the given format.
///
/// `timezone` is used only for CSV, whose times are wall-clock times; `None`
/// means the system time zone.
pub fn write_events(
    output_file: &Path,
    events: &[EventPayload],
    format: OutputFormat,
    timezone: Option<Tz>,
) -> Result<(), WriteError> {
    let contents = match format {
        OutputFormat::Json => json::to_json(events)?,
        OutputFormat::Ics => ics::to_ics(events),
        OutputFormat::Csv => csv::to_csv(events, timezone),
    };
    fs::write(output_file, contents).map_err(|source| WriteError::Io {
        path: output_file.to_owned(),
        source,
    })
}

/// Build an event payload from a JSON object literal.
#[cfg(test)]
pub fn payload(value: Value) -> EventPayload {
    value.as_object().unwrap().clone()
}
