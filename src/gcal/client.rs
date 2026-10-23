//! A minimal Google Calendar API v3 client.

use std::time::Duration;

use reqwest::{Method, StatusCode, Url};
use serde::Deserialize;
use serde_json::Value;
use tokio::time::sleep;
use yup_oauth2::authenticator::DefaultAuthenticator;

use super::auth::SCOPES;
use crate::error::GcalError;

const API_BASE: &str = "https://www.googleapis.com/calendar/v3";
/// How many times to retry a request that was rate limited or hit a server
/// error, doubling the wait each time.
const MAX_RETRIES: u32 = 5;
const FIRST_RETRY_DELAY: Duration = Duration::from_secs(1);

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CalendarList {
    #[serde(default)]
    items: Vec<CalendarListEntry>,
    next_page_token: Option<String>,
}

/// Whether [`Client::upsert_event`] created or updated the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Upsert {
    Created,
    Updated,
}

pub(super) struct Client {
    http: reqwest::Client,
    auth: DefaultAuthenticator,
}

impl Client {
    pub(super) fn new(auth: DefaultAuthenticator) -> Self {
        Self {
            http: reqwest::Client::new(),
            auth,
        }
    }

    /// The calendars in the user's list that they can add events to.
    pub(super) async fn writable_calendars(&self) -> Result<Vec<CalendarListEntry>, GcalError> {
        let mut calendars = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut url = api_url(&["users", "me", "calendarList"]);
            url.query_pairs_mut().append_pair("minAccessRole", "writer");
            if let Some(token) = &page_token {
                url.query_pairs_mut().append_pair("pageToken", token);
            }
            let page: CalendarList =
                serde_json::from_value(self.call(Method::GET, url, None).await?).map_err(|e| {
                    api_error(StatusCode::OK, &format!("unexpected calendar list: {e}"))
                })?;
            calendars.extend(page.items);
            page_token = page.next_page_token;
            if page_token.is_none() {
                return Ok(calendars);
            }
        }
    }

    /// Replace the event with ID `event_id`, or create it with that ID if it
    /// does not exist.
    pub(super) async fn upsert_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        event: &Value,
    ) -> Result<Upsert, GcalError> {
        // Updating first also covers events deleted in Google Calendar, which
        // keep their ID, so inserting them again would conflict.
        let url = api_url(&["calendars", calendar_id, "events", event_id]);
        match self.send(Method::PUT, url, Some(event)).await? {
            (status, _) if status.is_success() => Ok(Upsert::Updated),
            (StatusCode::NOT_FOUND, _) => {
                let mut event = event.clone();
                event["id"] = event_id.into();
                self.insert_event(calendar_id, &event).await?;
                Ok(Upsert::Created)
            }
            (status, body) => Err(error_from_response(status, &body)),
        }
    }

    /// Create an event.
    pub(super) async fn insert_event(
        &self,
        calendar_id: &str,
        event: &Value,
    ) -> Result<(), GcalError> {
        let url = api_url(&["calendars", calendar_id, "events"]);
        self.call(Method::POST, url, Some(event)).await?;
        Ok(())
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
    async fn send(
        &self,
        method: Method,
        url: Url,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value), GcalError> {
        let mut delay = FIRST_RETRY_DELAY;
        let mut retries = 0;
        loop {
            let token = self.auth.token(SCOPES).await?;
            let token = token.token().ok_or(yup_oauth2::Error::MissingAccessToken)?;
            let mut request = self
                .http
                .request(method.clone(), url.clone())
                .bearer_auth(token);
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request.send().await?;
            let status = response.status();
            let body = response.json().await.unwrap_or(Value::Null);
            if retries == MAX_RETRIES || !is_retryable(status, &body) {
                return Ok((status, body));
            }
            sleep(delay).await;
            delay *= 2;
            retries += 1;
        }
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
    use serde_json::json;

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
}
