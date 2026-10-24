//! Errors raised while scraping, writing and syncing events.

use std::io;
use std::path::PathBuf;

use chromiumoxide::error::CdpError;
use tokio::task::JoinError;

/// Why scraping failed.
#[derive(Debug, thiserror::Error)]
pub enum ScrapeError {
    #[error("invalid browser configuration: {0}")]
    Config(String),
    #[error("failed to launch Chromium (set CHROME to its path if it is not found)")]
    Launch(#[source] CdpError),
    #[error("failed to open a browser page")]
    OpenPage(#[source] CdpError),
    #[error("failed to listen for the page's network events")]
    Listen(#[source] CdpError),
    #[error("failed to load {url}")]
    Load {
        url: String,
        #[source]
        source: CdpError,
    },
    #[error("failed to click \"Next month\"")]
    NextMonth(#[source] CdpError),
    #[error("the page stopped reporting network events")]
    CaptureStopped,
    #[error("failed to close Chromium")]
    Close(#[source] CdpError),
    #[error("failed to wait for Chromium to exit")]
    Exit(#[source] io::Error),
    #[error("the browser connection task failed")]
    Handler(#[source] JoinError),
}

/// Why one `public_events` request yielded no events. These are reported
/// and skipped rather than ending the scrape.
#[derive(Debug, thiserror::Error)]
pub enum ResponseError {
    #[error("request to {url} failed: {reason}")]
    RequestFailed { url: String, reason: String },
    #[error("failed to read the response from {url}")]
    Read {
        url: String,
        #[source]
        source: ReadError,
    },
}

/// Why a finished response's events could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("failed to fetch the response body")]
    Body(#[from] CdpError),
    #[error("response body is not valid base64")]
    Base64(#[from] base64::DecodeError),
    #[error("response is not JSON")]
    NotJson(#[from] serde_json::Error),
    #[error("response is not a JSON object")]
    NotObject,
}

/// Why signing in to Google, or a Google Calendar request, failed.
#[derive(Debug, thiserror::Error)]
pub enum GcalError {
    #[error("could not find your home directory")]
    NoHomeDir,
    #[error(
        "set {} to the client ID and secret of a \"Desktop app\" Google OAuth client \
         (see \"Google Calendar sync\" in the README)",
        missing.join(" and ")
    )]
    MissingClient { missing: Vec<&'static str> },
    #[error("failed to create {}", path.display())]
    ConfigDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to sign in to Google")]
    Auth(#[from] yup_oauth2::Error),
    #[error("failed to reach Google Calendar")]
    Http(#[from] reqwest::Error),
    #[error("Google Calendar returned {status}: {message}")]
    Api {
        status: reqwest::StatusCode,
        message: String,
    },
    #[error(
        "you have no calendar named \"{name}\" that you can edit; \
         your editable calendars are: {}",
        available.join(", ")
    )]
    CalendarNotFound {
        name: String,
        available: Vec<String>,
    },
    #[error("{count} of your calendars are named \"{name}\"; rename one of them")]
    AmbiguousCalendar { name: String, count: usize },
}

/// Why the events could not be written.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("failed to serialize events as JSON")]
    Json(#[from] serde_json::Error),
    #[error("failed to write {}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}
