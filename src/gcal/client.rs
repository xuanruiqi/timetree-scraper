//! A minimal Google Calendar API v3 client.

use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::{Method, StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::time::sleep;
use yup_oauth2::authenticator::DefaultAuthenticator;

use super::auth::SCOPES;
use crate::error::GcalError;

const API_BASE: &str = "https://www.googleapis.com/calendar/v3";
/// How many times to retry a request that was rate limited or hit a server
/// error, doubling the wait each time. The waits add up to over a minute,
/// since Google measures its per-user rate limit over a sliding minute.
const MAX_RETRIES: u32 = 6;
const FIRST_RETRY_DELAY: Duration = Duration::from_secs(1);
/// The most events a page of an events list can hold.
const MAX_EVENTS_PER_PAGE: &str = "2500";
/// The event fields that [`super::event::is_unchanged`] compares.
const EVENT_LIST_FIELDS: &str =
    "items(id,status,summary,description,location,start,end),nextPageToken";

/// An entry in the user's calendar list.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CalendarListEntry {
    pub(super) id: String,
    #[serde(default)]
    pub(super) summary: String,
    /// The name the user gave the calendar in their own list, if any.
    pub(super) summary_override: Option<String>,
}

/// Whether [`Client::write_event`] created or updated the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Upsert {
    Created,
    Updated,
}

pub(super) struct Client {
    http: reqwest::Client,
    auth: DefaultAuthenticator,
    /// Held while getting an access token, so that when it must be
    /// refreshed, or the user must sign in again, concurrent requests wait
    /// for that to happen once instead of each doing it.
    token_lock: Mutex<()>,
}

impl Client {
    pub(super) fn new(auth: DefaultAuthenticator) -> Self {
        Self {
            http: reqwest::Client::new(),
            auth,
            token_lock: Mutex::new(()),
        }
    }

    /// The calendars in the user's list that they can add events to.
    pub(super) async fn writable_calendars(&self) -> Result<Vec<CalendarListEntry>, GcalError> {
        let mut url = api_url(&["users", "me", "calendarList"]);
        url.query_pairs_mut().append_pair("minAccessRole", "writer");
        self.list_all(url)
            .await?
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<_, _>>()
            .map_err(|e| api_error(StatusCode::OK, &format!("unexpected calendar list: {e}")))
    }

    /// Create a calendar named `name` and return its ID.
    pub(super) async fn create_calendar(&self, name: &str) -> Result<String, GcalError> {
        let url = api_url(&["calendars"]);
        let calendar = self
            .call(Method::POST, url, Some(&json!({"summary": name})))
            .await?;
        calendar["id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| api_error(StatusCode::OK, "created calendar has no ID"))
    }

    /// The calendar's events, including deleted ones, by ID, with only the
    /// fields in [`EVENT_LIST_FIELDS`]. Only events that end after
    /// `time_min` are listed.
    pub(super) async fn list_events(
        &self,
        calendar_id: &str,
        time_min: DateTime<Utc>,
    ) -> Result<HashMap<String, Value>, GcalError> {
        let mut url = api_url(&["calendars", calendar_id, "events"]);
        url.query_pairs_mut()
            .append_pair("showDeleted", "true")
            .append_pair("maxResults", MAX_EVENTS_PER_PAGE)
            .append_pair(
                "timeMin",
                &time_min.to_rfc3339_opts(SecondsFormat::Secs, true),
            )
            .append_pair("fields", EVENT_LIST_FIELDS);
        Ok(self
            .list_all(url)
            .await?
            .into_iter()
            .filter_map(|event| Some((event["id"].as_str()?.to_owned(), event)))
            .collect())
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
        event: &Value,
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
        written.ok_or_else(|| {
            api_error(
                StatusCode::CONFLICT,
                "the event could be neither created nor updated",
            )
        })
    }

    /// Create an event with a Google-assigned ID.
    pub(super) async fn insert_event(
        &self,
        calendar_id: &str,
        event: &Value,
    ) -> Result<(), GcalError> {
        let url = api_url(&["calendars", calendar_id, "events"]);
        self.call(Method::POST, url, Some(event)).await?;
        Ok(())
    }

    /// Replace the event with ID `event_id`. Returns `false` if there is no
    /// such event.
    async fn replace_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        event: &Value,
    ) -> Result<bool, GcalError> {
        let url = api_url(&["calendars", calendar_id, "events", event_id]);
        match self.send(Method::PUT, url, Some(event)).await? {
            (status, _) if status.is_success() => Ok(true),
            (StatusCode::NOT_FOUND, _) => Ok(false),
            (status, body) => Err(error_from_response(status, &body)),
        }
    }

    /// Create an event with ID `event_id`. Returns `false` if an event with
    /// that ID already exists.
    async fn create_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        event: &Value,
    ) -> Result<bool, GcalError> {
        let url = api_url(&["calendars", calendar_id, "events"]);
        let mut event = event.clone();
        event["id"] = event_id.into();
        match self.send(Method::POST, url, Some(&event)).await? {
            (status, _) if status.is_success() => Ok(true),
            (StatusCode::CONFLICT, _) => Ok(false),
            (status, body) => Err(error_from_response(status, &body)),
        }
    }

    /// GET every page of a list and return the items on all of them.
    async fn list_all(&self, url: Url) -> Result<Vec<Value>, GcalError> {
        let mut items = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut page_url = url.clone();
            if let Some(token) = &page_token {
                page_url.query_pairs_mut().append_pair("pageToken", token);
            }
            let mut page = self.call(Method::GET, page_url, None).await?;
            if let Value::Array(page_items) = page["items"].take() {
                items.extend(page_items);
            }
            page_token = page["nextPageToken"].as_str().map(str::to_owned);
            if page_token.is_none() {
                return Ok(items);
            }
        }
    }

    /// Send a request and return its JSON response, or an error if it did
    /// not succeed.
    async fn call(
        &self,
        method: Method,
        url: Url,
        body: Option<&Value>,
    ) -> Result<Value, GcalError> {
        match self.send(method, url, body).await? {
            (status, body) if status.is_success() => Ok(body),
            (status, body) => Err(error_from_response(status, &body)),
        }
    }

    /// Send a request, retrying with exponential backoff while it is rate
    /// limited or hits a server error, and return its status and JSON body
    /// (`null` if the body is not JSON).
    ///
    /// If the access token is rejected, it is refreshed, or the user is asked
    /// to sign in again if that fails, and the request is retried once.
    async fn send(
        &self,
        method: Method,
        url: Url,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value), GcalError> {
        let mut delay = FIRST_RETRY_DELAY;
        let mut retries = 0;
        let mut rejected_token = None;
        loop {
            let token = self.access_token(rejected_token.as_deref()).await?;
            let mut request = self
                .http
                .request(method.clone(), url.clone())
                .bearer_auth(&token);
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request.send().await?;
            let status = response.status();
            let body = response.json().await.unwrap_or(Value::Null);
            // A cached token that has not expired can still be invalid, for
            // example if the user revoked access.
            if status == StatusCode::UNAUTHORIZED && rejected_token.is_none() {
                rejected_token = Some(token);
                continue;
            }
            if retries == MAX_RETRIES || !is_retryable(status, &body) {
                return Ok((status, body));
            }
            sleep(delay + jitter()).await;
            delay *= 2;
            retries += 1;
        }
    }

    /// A current access token. If `rejected` is the cached token, a new one
    /// is fetched, unless a concurrent request already replaced it.
    async fn access_token(&self, rejected: Option<&str>) -> Result<String, GcalError> {
        let _guard = self.token_lock.lock().await;
        let mut token = self.auth.token(SCOPES).await?;
        if rejected.is_some() && token.token() == rejected {
            token = self.auth.force_refreshed_token(SCOPES).await?;
        }
        Ok(token
            .token()
            .ok_or(yup_oauth2::Error::MissingAccessToken)?
            .to_owned())
    }
}

/// The URL of an API resource, percent-encoding each path segment.
fn api_url(segments: &[&str]) -> Url {
    let mut url = Url::parse(API_BASE).expect("API_BASE is a valid URL");
    url.path_segments_mut()
        .expect("API_BASE has a path")
        .extend(segments);
    url
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

/// Turn an error response into a [`GcalError::Api`], using the message
/// Google sends in its body when there is one.
fn error_from_response(status: StatusCode, body: &Value) -> GcalError {
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or("no error message");
    api_error(status, message)
}

fn api_error(status: StatusCode, message: &str) -> GcalError {
    GcalError::Api {
        status,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_url_encodes_segments() {
        let url = api_url(&["calendars", "a#b@group.calendar.google.com", "events"]);
        assert_eq!(
            url.as_str(),
            "https://www.googleapis.com/calendar/v3/calendars/a%23b@group.calendar.google.com/events"
        );
    }

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
}
