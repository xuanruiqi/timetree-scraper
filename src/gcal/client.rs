//! The Google Calendar API calls a sync makes, through `google-calendar3`.

use std::collections::HashMap;
use std::error::Error as StdError;
use std::future::Future;
use std::hash::{BuildHasher, RandomState};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use google_calendar3::api::{Calendar, CalendarListEntry, Event};
use google_calendar3::common::{GetToken, Response, Retry};
use google_calendar3::hyper::StatusCode;
use google_calendar3::hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use google_calendar3::hyper_util::client::legacy::Client as HyperClient;
use google_calendar3::hyper_util::client::legacy::connect::HttpConnector;
use google_calendar3::hyper_util::rt::TokioExecutor;
use google_calendar3::{CalendarHub, Delegate};
use serde_json::Value;
use tokio::sync::Mutex;
use yup_oauth2::authenticator::DefaultAuthenticator;

use super::auth::SCOPES;
use crate::error::GcalError;

/// How many times to retry a request that was rate limited or hit a server
/// error, doubling the wait each time. The waits add up to over a minute,
/// since Google measures its per-user rate limit over a sliding minute.
const MAX_RETRIES: u32 = 6;
const FIRST_RETRY_DELAY: Duration = Duration::from_secs(1);
/// The most events a page of an events list can hold.
const MAX_EVENTS_PER_PAGE: i32 = 2500;
/// The event fields that [`super::event::is_unchanged`] compares.
const EVENT_LIST_FIELDS: &str =
    "items(id,status,summary,description,location,start,end),nextPageToken";

/// Whether [`Client::write_event`] created or updated the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Upsert {
    Created,
    Updated,
}

pub(super) struct Client {
    hub: CalendarHub<HttpsConnector<HttpConnector>>,
    tokens: TokenSource,
}

impl Client {
    pub(super) fn new(auth: DefaultAuthenticator) -> Result<Self, GcalError> {
        let connector = HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(|e| GcalError::Request(Box::new(e.into())))?
            .https_only()
            .enable_http1()
            .enable_http2()
            .build();
        let client = HyperClient::builder(TokioExecutor::new()).build(connector);
        let tokens = TokenSource {
            auth,
            lock: Arc::default(),
        };
        Ok(Self {
            hub: CalendarHub::new(client, tokens.clone()),
            tokens,
        })
    }

    /// Get a new access token, for when Google rejects the cached one
    /// although it has not expired, for example because the user revoked
    /// access. Signs the user in again if the token cannot be refreshed.
    pub(super) async fn refresh_token(&self) -> Result<(), GcalError> {
        let _guard = self.tokens.lock.lock().await;
        self.tokens.auth.force_refreshed_token(SCOPES).await?;
        Ok(())
    }

    /// The calendars in the user's list that they can add events to.
    pub(super) async fn writable_calendars(&self) -> Result<Vec<CalendarListEntry>, GcalError> {
        let mut calendars = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut call = self.hub.calendar_list().list().min_access_role("writer");
            if let Some(token) = &page_token {
                call = call.page_token(token);
            }
            let (_, page) = call
                .add_scopes(SCOPES)
                .delegate(&mut Backoff::default())
                .doit()
                .await
                .map_err(api_error)?;
            calendars.extend(page.items.unwrap_or_default());
            page_token = page.next_page_token;
            if page_token.is_none() {
                return Ok(calendars);
            }
        }
    }

    /// Create a calendar named `name` and return its ID.
    pub(super) async fn create_calendar(&self, name: &str) -> Result<String, GcalError> {
        let calendar = Calendar {
            summary: Some(name.to_owned()),
            ..Default::default()
        };
        let (_, calendar) = self
            .hub
            .calendars()
            .insert(calendar)
            .add_scopes(SCOPES)
            .delegate(&mut Backoff::default())
            .doit()
            .await
            .map_err(api_error)?;
        calendar.id.ok_or_else(|| GcalError::Api {
            status: StatusCode::OK,
            message: "created calendar has no ID".to_owned(),
        })
    }

    /// The calendar's events, including deleted ones, by ID, with only the
    /// fields in [`EVENT_LIST_FIELDS`]. Only events that end after
    /// `time_min` are listed.
    pub(super) async fn list_events(
        &self,
        calendar_id: &str,
        time_min: DateTime<Utc>,
    ) -> Result<HashMap<String, Event>, GcalError> {
        let mut events = HashMap::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut call = self
                .hub
                .events()
                .list(calendar_id)
                .show_deleted(true)
                .max_results(MAX_EVENTS_PER_PAGE)
                .time_min(time_min)
                .param("fields", EVENT_LIST_FIELDS);
            if let Some(token) = &page_token {
                call = call.page_token(token);
            }
            let (_, page) = call
                .add_scopes(SCOPES)
                .delegate(&mut Backoff::default())
                .doit()
                .await
                .map_err(api_error)?;
            for event in page.items.unwrap_or_default() {
                if let Some(id) = event.id.clone() {
                    events.insert(id, event);
                }
            }
            page_token = page.next_page_token;
            if page_token.is_none() {
                return Ok(events);
            }
        }
    }

    /// Write the event with ID `event_id`, replacing it if `exists` and
    /// creating it otherwise.
    ///
    /// If `exists` turns out to be wrong, the other write is tried instead.
    /// Replacing also restores an event deleted in Google Calendar, which
    /// keeps its ID, so creating it again would conflict.
    pub(super) async fn write_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        event: &Event,
        exists: bool,
    ) -> Result<Upsert, GcalError> {
        let written = if exists {
            if self.replace_event(calendar_id, event_id, event).await? {
                return Ok(Upsert::Updated);
            }
            self.create_event(calendar_id, event_id, event)
                .await?
                .then_some(Upsert::Created)
        } else {
            if self.create_event(calendar_id, event_id, event).await? {
                return Ok(Upsert::Created);
            }
            self.replace_event(calendar_id, event_id, event)
                .await?
                .then_some(Upsert::Updated)
        };
        written.ok_or_else(|| GcalError::Api {
            status: StatusCode::CONFLICT,
            message: "the event could be neither created nor updated".to_owned(),
        })
    }

    /// Create an event with a Google-assigned ID.
    pub(super) async fn insert_event(
        &self,
        calendar_id: &str,
        event: &Event,
    ) -> Result<(), GcalError> {
        self.hub
            .events()
            .insert(event.clone(), calendar_id)
            .add_scopes(SCOPES)
            .delegate(&mut Backoff::default())
            .doit()
            .await
            .map_err(api_error)?;
        Ok(())
    }

    /// Replace the event with ID `event_id`. Returns `false` if there is no
    /// such event.
    async fn replace_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        event: &Event,
    ) -> Result<bool, GcalError> {
        let result = self
            .hub
            .events()
            .update(event.clone(), calendar_id, event_id)
            .add_scopes(SCOPES)
            .delegate(&mut Backoff::default())
            .doit()
            .await;
        succeeded_unless(result, StatusCode::NOT_FOUND)
    }

    /// Create an event with ID `event_id`. Returns `false` if an event with
    /// that ID already exists.
    async fn create_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        event: &Event,
    ) -> Result<bool, GcalError> {
        let event = Event {
            id: Some(event_id.to_owned()),
            ..event.clone()
        };
        let result = self
            .hub
            .events()
            .insert(event, calendar_id)
            .add_scopes(SCOPES)
            .delegate(&mut Backoff::default())
            .doit()
            .await;
        succeeded_unless(result, StatusCode::CONFLICT)
    }
}

/// Hands the hub its access tokens one request at a time, so that when the
/// token must be refreshed, or the user must sign in again, concurrent
/// requests wait for that to happen once instead of each doing it.
#[derive(Clone)]
struct TokenSource {
    auth: DefaultAuthenticator,
    lock: Arc<Mutex<()>>,
}

type TokenResult = Result<Option<String>, Box<dyn StdError + Send + Sync>>;

impl GetToken for TokenSource {
    fn get_token<'a>(
        &'a self,
        scopes: &'a [&str],
    ) -> Pin<Box<dyn Future<Output = TokenResult> + Send + 'a>> {
        Box::pin(async move {
            let _guard = self.lock.lock().await;
            let token = self.auth.token(scopes).await?;
            Ok(token.token().map(str::to_owned))
        })
    }
}

/// Retries requests that were rate limited or hit a server error, with
/// exponential backoff. Each request needs its own.
#[derive(Default)]
struct Backoff {
    retries: u32,
}

impl Delegate for Backoff {
    fn http_failure(&mut self, response: &Response, body: Option<&Value>) -> Retry {
        if self.retries == MAX_RETRIES
            || !is_retryable(response.status(), body.unwrap_or(&Value::Null))
        {
            return Retry::Abort;
        }
        let delay = FIRST_RETRY_DELAY * 2u32.pow(self.retries) + jitter();
        self.retries += 1;
        Retry::After(delay)
    }
}

/// Whether a failed request may succeed if sent again: it was rate limited
/// (429, or 403 with a rate limit reason) or hit a server error.
fn is_retryable(status: StatusCode, body: &Value) -> bool {
    let rate_limited = || {
        body["error"]["errors"].as_array().is_some_and(|errors| {
            errors.iter().any(|error| {
                matches!(
                    error["reason"].as_str(),
                    Some("rateLimitExceeded" | "userRateLimitExceeded")
                )
            })
        })
    };
    status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
        || (status == StatusCode::FORBIDDEN && rate_limited())
}

/// A random delay of up to a second, so that concurrent requests that were
/// rate limited together do not all retry together.
fn jitter() -> Duration {
    // Each `RandomState` is randomly seeded, which is random enough here.
    Duration::from_millis(RandomState::new().hash_one(()) % 1000)
}

/// `Ok(true)` if the request succeeded, or `Ok(false)` if it failed with
/// status `expected_failure`.
fn succeeded_unless<T>(
    result: google_calendar3::Result<T>,
    expected_failure: StatusCode,
) -> Result<bool, GcalError> {
    match result.map_err(api_error) {
        Ok(_) => Ok(true),
        Err(GcalError::Api { status, .. }) if status == expected_failure => Ok(false),
        Err(e) => Err(e),
    }
}

/// Turn a failed request into a [`GcalError`]: [`GcalError::Api`], with
/// the status and message from Google's error body, if Google responded.
fn api_error(error: google_calendar3::Error) -> GcalError {
    match error {
        google_calendar3::Error::BadRequest(body) => {
            let error = &body["error"];
            GcalError::Api {
                status: error["code"]
                    .as_u64()
                    .and_then(|code| StatusCode::from_u16(code.try_into().ok()?).ok())
                    .unwrap_or(StatusCode::BAD_REQUEST),
                message: error["message"]
                    .as_str()
                    .unwrap_or("no error message")
                    .to_owned(),
            }
        }
        google_calendar3::Error::Failure(response) => GcalError::Api {
            status: response.status(),
            message: "no error message".to_owned(),
        },
        error => GcalError::Request(Box::new(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retries_rate_limits_and_server_errors() {
        let rate_limited = json!({"error": {"errors": [{"reason": "rateLimitExceeded"}]}});
        let forbidden = json!({"error": {"errors": [{"reason": "forbidden"}]}});
        assert!(is_retryable(StatusCode::TOO_MANY_REQUESTS, &Value::Null));
        assert!(is_retryable(StatusCode::SERVICE_UNAVAILABLE, &Value::Null));
        assert!(is_retryable(StatusCode::FORBIDDEN, &rate_limited));
        assert!(!is_retryable(StatusCode::FORBIDDEN, &forbidden));
        assert!(!is_retryable(StatusCode::NOT_FOUND, &Value::Null));
        assert!(!is_retryable(StatusCode::OK, &Value::Null));
    }

    #[test]
    fn retry_waits_outlast_the_rate_limit_window() {
        let total: Duration = (0..MAX_RETRIES)
            .map(|n| FIRST_RETRY_DELAY * 2u32.pow(n))
            .sum();
        assert!(total > Duration::from_secs(60), "{total:?}");
    }

    #[test]
    fn errors_take_status_and_message_from_google() {
        let body =
            json!({"error": {"code": 409, "message": "The requested identifier already exists."}});
        match api_error(google_calendar3::Error::BadRequest(body)) {
            GcalError::Api { status, message } => {
                assert_eq!(status, StatusCode::CONFLICT);
                assert_eq!(message, "The requested identifier already exists.");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        let result: google_calendar3::Result<()> = Err(google_calendar3::Error::BadRequest(
            json!({"error": {"code": 404, "message": "Not Found"}}),
        ));
        assert!(!succeeded_unless(result, StatusCode::NOT_FOUND).unwrap());
    }
}
