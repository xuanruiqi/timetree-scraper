//! Signing in to Google with OAuth, caching the credentials on disk.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use yup_oauth2::authenticator::DefaultAuthenticator;
use yup_oauth2::authenticator_delegate::InstalledFlowDelegate;
use yup_oauth2::{InstalledFlowAuthenticator, InstalledFlowReturnMethod};

use crate::error::GcalError;

/// Access needed to find a calendar by name and write its events.
pub(super) const SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/calendar.calendarlist.readonly",
    "https://www.googleapis.com/auth/calendar.events",
];

/// The OAuth client, downloaded from the Google Cloud console.
const CLIENT_SECRET_FILE: &str = "client_secret.json";
/// The user's cached access and refresh tokens.
const TOKEN_FILE: &str = "token.json";

/// `~/.config/timetree-scraper`, which holds the client secret and the
/// cached tokens.
fn config_dir() -> Result<PathBuf, GcalError> {
    let home = std::env::home_dir().ok_or(GcalError::NoHomeDir)?;
    Ok(home.join(".config").join("timetree-scraper"))
}

/// Sign in to Google, prompting the user in their browser only if there is
/// no cached token or it can no longer be refreshed.
///
/// The returned authenticator refreshes the access token as it expires and
/// writes new tokens back to the cache file (readable only by the user on
/// Unix).
pub(super) async fn authenticate() -> Result<DefaultAuthenticator, GcalError> {
    let config_dir = config_dir()?;
    let secret_path = config_dir.join(CLIENT_SECRET_FILE);
    let secret = yup_oauth2::read_application_secret(&secret_path)
        .await
        .map_err(|source| GcalError::ClientSecret {
            path: secret_path,
            source,
        })?;
    let auth = InstalledFlowAuthenticator::builder(secret, InstalledFlowReturnMethod::HTTPRedirect)
        .persist_tokens_to_disk(config_dir.join(TOKEN_FILE))
        .flow_delegate(Box::new(BrowserDelegate))
        .build()
        .await
        .map_err(yup_oauth2::Error::from)?;
    // Sign in now rather than at the first request, so that the user is
    // prompted before scraping starts.
    auth.token(SCOPES).await?;
    Ok(auth)
}

/// Opens the Google sign-in page in the user's browser, and prints its URL
/// in case that fails.
struct BrowserDelegate;

impl InstalledFlowDelegate for BrowserDelegate {
    fn present_user_url<'a>(
        &'a self,
        url: &'a str,
        _need_code: bool,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        // With `HTTPRedirect`, Google sends the code to a local server, so
        // there is never a code for the user to paste.
        println!("Sign in to Google in your browser to allow access to your calendars:\n{url}");
        let _ = open::that_detached(url);
        Box::pin(async { Ok(String::new()) })
    }
}
