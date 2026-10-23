//! Scrape public TimeTree events into JSON or ICS.

mod error;
mod output;
mod scrape;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, ValueEnum};

/// Scrape public TimeTree events into JSON or ICS.
#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    /// Public TimeTree calendar URL to scrape.
    url: String,
    /// Output file path.
    output_file: PathBuf,
    /// Output format. Defaults to the output file extension, then JSON.
    #[arg(long, value_enum)]
    format: Option<OutputFormat>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Json,
    Ics,
}

impl OutputFormat {
    fn name(self) -> &'static str {
        match self {
            Self::Json => "JSON",
            Self::Ics => "ICS",
        }
    }
}

/// Decide which format to write.
///
/// An explicitly requested format wins. Otherwise a `.ics` extension
/// (case-insensitive) selects ICS, and anything else falls back to JSON.
fn output_format(output_file: &std::path::Path, requested: Option<OutputFormat>) -> OutputFormat {
    if let Some(format) = requested {
        return format;
    }
    let is_ics = output_file
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ics"));
    if is_ics {
        OutputFormat::Ics
    } else {
        OutputFormat::Json
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let file_format = output_format(&args.output_file, args.format);

    let events = scrape::scrape(&args.url).await?;
    output::write_events(&args.output_file, &events, file_format)?;

    println!("Total events scraped: {}", events.len());
    println!(
        "Wrote {} output to {}",
        file_format.name(),
        args.output_file.display()
    );
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
