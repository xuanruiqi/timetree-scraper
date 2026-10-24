//! Scrape public TimeTree events into JSON, ICS or Google Calendar CSV, or
//! sync them into a Google Calendar.

mod error;
mod gcal;
mod output;
mod scrape;

use std::path::PathBuf;

use anyhow::{Result, bail};
use chrono_tz::Tz;
use clap::{Parser, ValueEnum};

/// Scrape public TimeTree events into JSON, ICS or Google Calendar CSV, or
/// sync them into a Google Calendar.
#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    /// Public TimeTree calendar URL to scrape.
    url: String,
    /// Output file path. May be omitted when syncing with `--gcal`.
    #[arg(required_unless_present = "gcal")]
    output_file: Option<PathBuf>,
    /// Output format. Defaults to the output file extension, then JSON.
    #[arg(long, value_enum, requires = "output_file")]
    format: Option<OutputFormat>,
    /// Time zone for the times in CSV output, as an IANA name such as
    /// `Asia/Tokyo`. Defaults to the system time zone.
    #[arg(long, value_parser = parse_timezone)]
    timezone: Option<Tz>,
    /// Sync the events into the Google Calendar with this name, signing in
    /// to Google first if needed.
    #[arg(long, value_name = "CALENDAR_NAME")]
    gcal: Option<String>,
}

fn parse_timezone(name: &str) -> Result<Tz, String> {
    name.parse()
        .map_err(|_| "unknown time zone; use an IANA name such as Asia/Tokyo".to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Json,
    Ics,
    /// Google Calendar's CSV import format.
    Csv,
}

impl OutputFormat {
    fn name(self) -> &'static str {
        match self {
            Self::Json => "JSON",
            Self::Ics => "ICS",
            Self::Csv => "CSV",
        }
    }
}

/// Decide which format to write.
///
/// An explicitly requested format wins. Otherwise a `.ics` or `.csv`
/// extension (case-insensitive) selects ICS or CSV, and anything else falls
/// back to JSON.
fn output_format(output_file: &std::path::Path, requested: Option<OutputFormat>) -> OutputFormat {
    if let Some(format) = requested {
        return format;
    }
    let extension = output_file.extension().unwrap_or_default();
    if extension.eq_ignore_ascii_case("ics") {
        OutputFormat::Ics
    } else if extension.eq_ignore_ascii_case("csv") {
        OutputFormat::Csv
    } else {
        OutputFormat::Json
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    // Sign in and find the calendar before scraping, which takes a while,
    // so that a problem with either is reported straight away.
    let calendar = match &args.gcal {
        Some(name) => Some(gcal::GoogleCalendar::connect(name).await?),
        None => None,
    };

    let events = scrape::scrape(&args.url).await?;
    println!("Total events scraped: {}", events.len());

    if let Some(output_file) = &args.output_file {
        let file_format = output_format(output_file, args.format);
        output::write_events(output_file, &events, file_format, args.timezone)?;
        println!(
            "Wrote {} output to {}",
            file_format.name(),
            output_file.display()
        );
    }

    if let (Some(calendar), Some(name)) = (calendar, &args.gcal) {
        let summary = calendar.sync(&events).await;
        println!(
            "Synced to Google Calendar \"{name}\": {} created, {} updated, {} unchanged",
            summary.created, summary.updated, summary.unchanged
        );
        if summary.failed > 0 {
            bail!(
                "{} events failed to sync to Google Calendar",
                summary.failed
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn format_from_extension() {
        assert_eq!(output_format(Path::new("a.ics"), None), OutputFormat::Ics);
        assert_eq!(output_format(Path::new("a.ICS"), None), OutputFormat::Ics);
        assert_eq!(output_format(Path::new("a.Csv"), None), OutputFormat::Csv);
        assert_eq!(output_format(Path::new("a.json"), None), OutputFormat::Json);
        assert_eq!(output_format(Path::new("a"), None), OutputFormat::Json);
    }

    #[test]
    fn requested_format_wins() {
        assert_eq!(
            output_format(Path::new("a.ics"), Some(OutputFormat::Json)),
            OutputFormat::Json
        );
    }
}
