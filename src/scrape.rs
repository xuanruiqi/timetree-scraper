//! Drive a headless browser over a TimeTree calendar and capture its events.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use base64::Engine;
use chromiumoxide::Page;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::{
    EventLoadingFailed, EventLoadingFinished, EventRequestWillBeSent, GetResponseBodyParams,
    RequestId,
};
use chromiumoxide::error::CdpError;
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};

use crate::error::{ReadError, ResponseError, ScrapeError};
use crate::output::EventPayload;

const NEXT_MONTH_SELECTOR: &str = r#"button[aria-label="Next month"]"#;
const INITIAL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const NEXT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const DOM_SETTLE_DELAY: Duration = Duration::from_secs(1);

/// The outcome of one `public_events` request: its events, or why they could
/// not be read.
type Response = Result<Vec<EventPayload>, ResponseError>;

#[derive(Default)]
struct CaptureState {
    events: Vec<EventPayload>,
    last_response_empty: bool,
}

impl CaptureState {
    fn record(&mut self, response: Response) {
        match response {
            Ok(events) if events.is_empty() => self.last_response_empty = true,
            Ok(events) => self.events.extend(events),
            // Go through `anyhow` to print the whole chain of causes.
            Err(e) => println!("{:#}", anyhow::Error::new(e)),
        }
    }

    /// Record the responses already received without waiting for more.
    fn record_received(&mut self, responses: &mut mpsc::UnboundedReceiver<Response>) {
        while let Ok(response) = responses.try_recv() {
            self.record(response);
        }
    }
}

/// Scrape a public TimeTree calendar and return its events.
///
/// Opens the calendar URL in headless Chromium and collects events from the
/// `public_events` API responses the page makes. It then clicks "Next
/// month" until one of these happens:
///
/// - a month returns no events
/// - the button disappears
/// - no response arrives within 5 seconds
///
/// Scraping starts at the month the page opens on, so earlier months are not
/// collected.
pub async fn scrape(url: &str) -> Result<Vec<EventPayload>, ScrapeError> {
    let config = BrowserConfig::builder()
        .build()
        .map_err(ScrapeError::Config)?;
    let (mut browser, mut handler) = Browser::launch(config).await.map_err(ScrapeError::Launch)?;
    // Drive the CDP connection until it closes. Errors are not fatal: the
    // handler also reports messages it cannot decode, and the connection
    // keeps working after them.
    let handler = tokio::spawn(async move { while handler.next().await.is_some() {} });

    // Close the browser even if scraping fails.
    let events = scrape_page(&browser, url).await;
    let closed = async {
        browser.close().await.map_err(ScrapeError::Close)?;
        browser.wait().await.map_err(ScrapeError::Exit)?;
        handler.await.map_err(ScrapeError::Handler)
    }
    .await;

    let mut events = events?;
    closed?;
    dedup_by_id(&mut events);
    Ok(events)
}

/// Open `url` in a new page of `browser` and capture its events while
/// clicking through the months.
async fn scrape_page(browser: &Browser, url: &str) -> Result<Vec<EventPayload>, ScrapeError> {
    let page = browser
        .new_page("about:blank")
        .await
        .map_err(ScrapeError::OpenPage)?;
    let (responses_tx, responses) = mpsc::unbounded_channel();
    let capture = capture_responses(&page, responses_tx)
        .await
        .map_err(ScrapeError::Listen)?;
    tokio::select! {
        events = click_through_months(&page, url, responses) => events,
        () = capture => Err(ScrapeError::CaptureStopped),
    }
}

/// Load `url` and click "Next month" until the calendar runs out, recording
/// each month's response. Responses are produced by [`capture_responses`],
/// which must be polled alongside this.
async fn click_through_months(
    page: &Page,
    url: &str,
    mut responses: mpsc::UnboundedReceiver<Response>,
) -> Result<Vec<EventPayload>, ScrapeError> {
    let mut state = CaptureState::default();

    // Initial load
    page.goto(url).await.map_err(|source| ScrapeError::Load {
        url: url.to_owned(),
        source,
    })?;
    // Wait for the API request to finish
    if let Ok(Some(response)) = timeout(INITIAL_RESPONSE_TIMEOUT, responses.recv()).await {
        state.record(response);
    }

    loop {
        // Take in responses that arrived after we stopped waiting, so the
        // wait below is for this click's response.
        state.record_received(&mut responses);
        if state.last_response_empty {
            break;
        }

        let Some(next_btn) = page
            .find_elements(NEXT_MONTH_SELECTOR)
            .await
            .map_err(ScrapeError::NextMonth)?
            .into_iter()
            .next()
        else {
            println!("No next button found, stopping.");
            break;
        };

        println!("Clicking Next month...");
        next_btn.click().await.map_err(ScrapeError::NextMonth)?;

        let Ok(Some(response)) = timeout(NEXT_RESPONSE_TIMEOUT, responses.recv()).await else {
            println!("Timeout waiting for events response, assuming end.");
            break;
        };
        state.record(response);

        sleep(DOM_SETTLE_DELAY).await; // Small pause to let DOM update if needed
    }

    state.record_received(&mut responses);
    Ok(state.events)
}

/// Drop repeats of an event already seen, keeping the first copy. Adjacent
/// months' responses overlap, since a month view also shows the edges of
/// the weeks around it. Events without an `id` are always kept.
fn dedup_by_id(events: &mut Vec<EventPayload>) {
    let mut seen = HashSet::new();
    events.retain(|event| event.get("id").is_none_or(|id| seen.insert(id.to_string())));
}

/// Return a future that collects events from `public_events` GET responses.
///
/// While polled, the future sends one [`Response`] to `responses` per
/// request, including when the request fails or reading the response fails,
/// so the main loop always hears back about each month. It finishes only if
/// the page stops reporting network events.
///
/// The listeners are registered before this returns, so no response made
/// after it is missed.
async fn capture_responses(
    page: &Page,
    responses: mpsc::UnboundedSender<Response>,
) -> Result<impl Future<Output = ()>, CdpError> {
    let mut requests = page.event_listener::<EventRequestWillBeSent>().await?;
    let mut finished = page.event_listener::<EventLoadingFinished>().await?;
    let mut failed = page.event_listener::<EventLoadingFailed>().await?;

    Ok(async move {
        // URLs of the `public_events` requests still in flight.
        let mut pending: HashMap<RequestId, String> = HashMap::new();
        loop {
            tokio::select! {
                // A request is always announced before it finishes, so poll
                // announcements first to never see a finish we cannot match.
                biased;
                Some(event) = requests.next() => {
                    let request = &event.request;
                    if request.url.contains("public_events") && request.method == "GET" {
                        pending.insert(event.request_id.clone(), request.url.clone());
                    }
                }
                Some(event) = finished.next() => {
                    if let Some(url) = pending.remove(&event.request_id) {
                        let response = match read_events(page, event.request_id.clone()).await {
                            Ok(events) => {
                                println!("Captured {} events from {url}", events.len());
                                Ok(events)
                            }
                            Err(source) => Err(ResponseError::Read { url, source }),
                        };
                        let _ = responses.send(response);
                    }
                }
                Some(event) = failed.next() => {
                    if let Some(url) = pending.remove(&event.request_id) {
                        let reason = event.error_text.clone();
                        let _ = responses.send(Err(ResponseError::RequestFailed { url, reason }));
                    }
                }
                else => break,
            }
        }
    })
}

/// Fetch a finished response's body and return the event objects in its
/// `public_events` list. A missing or non-list `public_events` counts as no
/// events.
async fn read_events(page: &Page, request_id: RequestId) -> Result<Vec<EventPayload>, ReadError> {
    let response = page.execute(GetResponseBodyParams::new(request_id)).await?;
    let body = if response.base64_encoded {
        base64::engine::general_purpose::STANDARD.decode(&response.body)?
    } else {
        response.body.clone().into_bytes()
    };
    let data: Value = serde_json::from_slice(&body)?;
    let data = data.as_object().ok_or(ReadError::NotObject)?;
    let events = match data.get("public_events") {
        Some(Value::Array(events)) => events
            .iter()
            .filter_map(|event| event.as_object().cloned())
            .collect(),
        _ => Vec::new(),
    };
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dedup_keeps_first_copy_and_events_without_id() {
        let mut events: Vec<EventPayload> = [
            json!({"id": "a", "n": 1}),
            json!({"n": 2}),
            json!({"id": "a", "n": 3}),
            json!({"n": 4}),
        ]
        .into_iter()
        .map(|v| v.as_object().unwrap().clone())
        .collect();
        dedup_by_id(&mut events);
        let ns: Vec<_> = events.iter().map(|e| e["n"].clone()).collect();
        assert_eq!(ns, [json!(1), json!(2), json!(4)]);
    }
}
