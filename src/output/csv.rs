//! Google Calendar CSV output.

use chrono::{DateTime, Days, Local, NaiveDate, Utc};
use chrono_tz::Tz;

use super::EventPayload;
use super::event::{CalendarEvent, EventTimes, parse_events};

/// Column headers for Google Calendar's CSV import, which must be in English.
const CSV_HEADERS: [&str; 8] = [
    "Subject",
    "Start Date",
    "Start Time",
    "End Date",
    "End Time",
    "All Day Event",
    "Description",
    "Location",
];

/// Convert the events to a CSV file for Google Calendar's import, following
/// <https://support.google.com/calendar/answer/37118>.
///
/// CSV has no time zone field, so timed events are written as wall-clock
/// times in `timezone` (the system time zone if `None`). Google Calendar reads
/// them in the time zone of the calendar they are imported into. Events with
/// no parseable start time are skipped, and a message is printed.
pub(super) fn to_csv(events: &[EventPayload], timezone: Option<Tz>) -> String {
    let mut csv = String::new();
    push_csv_record(&mut csv, &CSV_HEADERS);
    for event in parse_events(events) {
        push_csv_record(&mut csv, &to_csv_record(&event, timezone));
    }
    csv
}

/// Convert an event to a row of [`CSV_HEADERS`] values.
///
/// Dates are `MM/DD/YYYY` and times are 12-hour `H:MM AM`, as Google expects.
/// All-day events have no times, and their end date is the last day of the
/// event rather than the day after it.
fn to_csv_record(event: &CalendarEvent, timezone: Option<Tz>) -> [String; 8] {
    let date = |date: NaiveDate| date.format("%m/%d/%Y").to_string();
    let date_time = |datetime: DateTime<Utc>| {
        let local = match timezone {
            Some(timezone) => datetime.with_timezone(&timezone).naive_local(),
            None => datetime.with_timezone(&Local).naive_local(),
        };
        (date(local.date()), local.format("%-I:%M %p").to_string())
    };
    let ((start_date, start_time), (end_date, end_time), all_day) = match event.times {
        EventTimes::Timed { start, end } => (date_time(start), date_time(end), "False"),
        EventTimes::AllDay { start, end } => (
            (date(start), String::new()),
            (date(end - Days::new(1)), String::new()),
            "True",
        ),
    };
    [
        event.title.clone(),
        start_date,
        start_time,
        end_date,
        end_time,
        all_day.to_owned(),
        event.description.clone().unwrap_or_default(),
        event.location.clone().unwrap_or_default(),
    ]
}

/// Append a CSV record, quoting fields that contain a comma, a double quote
/// or a line break, and ending it with CRLF (RFC 4180).
fn push_csv_record(csv: &mut String, fields: &[impl AsRef<str>]) {
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            csv.push(',');
        }
        let field = field.as_ref();
        if field.contains([',', '"', '\n', '\r']) {
            csv.push('"');
            csv.push_str(&field.replace('"', "\"\""));
            csv.push('"');
        } else {
            csv.push_str(field);
        }
    }
    csv.push_str("\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::event::parse_event;
    use crate::output::payload;
    use serde_json::{Value, json};

    fn csv_record(value: Value) -> [String; 8] {
        to_csv_record(
            &parse_event(&payload(value)).unwrap(),
            Some(Tz::Asia__Tokyo),
        )
    }

    #[test]
    fn timed_event_with_quoting() {
        let csv = to_csv(
            &[payload(json!({
                "title": "Meeting",
                "start_at": 1_772_359_200_000_i64,
                "end_at": 1_772_366_400_000_i64,
                "description": "Bring \"pens\", paper\r\nand snacks",
                "location": "Tokyo",
            }))],
            Some(Tz::Asia__Tokyo),
        );
        assert_eq!(
            csv,
            "Subject,Start Date,Start Time,End Date,End Time,All Day Event,Description,Location\r\n\
             Meeting,03/01/2026,7:00 PM,03/01/2026,9:00 PM,False,\
             \"Bring \"\"pens\"\", paper\nand snacks\",Tokyo\r\n"
        );
    }

    #[test]
    fn times_use_the_given_time_zone() {
        // 15:30 UTC is 00:30 the next day in Tokyo.
        let record = csv_record(json!({"start_at": "2026-03-01T15:30:00Z"}));
        assert_eq!(
            record[1..6],
            ["03/02/2026", "12:30 AM", "03/02/2026", "1:30 AM", "False"]
        );
    }

    #[test]
    fn all_day_end_date_is_inclusive() {
        let record = csv_record(json!({"start_at": "2026-03-01", "end_at": "2026-03-04"}));
        assert_eq!(record[1..6], ["03/01/2026", "", "03/03/2026", "", "True"]);

        let record = csv_record(json!({"start_at": "2026-03-01T00:00:00+09:00", "all_day": true}));
        assert_eq!(record[1..6], ["03/01/2026", "", "03/01/2026", "", "True"]);
    }

    #[test]
    fn skips_events_without_start() {
        let csv = to_csv(
            &[
                payload(json!({"id": "x"})),
                payload(json!({"start_at": "2026-03-01"})),
            ],
            None,
        );
        assert_eq!(csv.lines().count(), 2);
    }
}
