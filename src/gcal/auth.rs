//! Signing in to Google with OAuth, caching the credentials on disk.

use std::fs::{self, DirBuilder};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use yup_oauth2::authenticator::DefaultAuthenticator;
use yup_oauth2::authenticator_delegate::InstalledFlowDelegate;
use yup_oauth2::{ApplicationSecret, InstalledFlowAuthenticator, InstalledFlowReturnMethod};

use crate::error::GcalError;

/// Access needed to find a calendar by name and write its events.
pub(super) const SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/calendar.calendarlist.readonly",
    "https://www.googleapis.com/auth/calendar.events",
];

/// Environment variables holding the OAuth client's ID and secret.
const CLIENT_ID_VAR: &str = "TIMETREE_GOOGLE_CLIENT_ID";
const CLIENT_SECRET_VAR: &str = "TIMETREE_GOOGLE_CLIENT_SECRET";
/// The user's cached access and refresh tokens.
const TOKEN_FILE: &str = "token.json";

/// `~/.config/timetree-scraper`, which holds the cached tokens.
fn config_dir() -> Result<PathBuf, GcalError> {
    let home = std::env::home_dir().ok_or(GcalError::NoHomeDir)?;
    Ok(home.join(".config").join("timetree-scraper"))
}

/// Sign in to Google, prompting the user in their browser only if there is
/// no usable cached token or it can no longer be refreshed.
///
/// The OAuth client is read from the environment (see [`client_secret`]).
/// An unreadable token cache is discarded. The returned authenticator
/// refreshes the access token as it expires and writes new tokens back to
/// the cache file (readable only by the user on Unix).
pub(super) async fn authenticate() -> Result<DefaultAuthenticator, GcalError> {
    let secret = client_secret(|name| std::env::var(name).ok())?;
    let config_dir = config_dir()?;
    create_private_dir(&config_dir).map_err(|source| GcalError::ConfigDir {
        path: config_dir.clone(),
        source,
    })?;
    let token_path = config_dir.join(TOKEN_FILE);
    // Building the authenticator reads the token cache, and fails with
    // `InvalidData` if it is malformed.
    let auth = match build_authenticator(secret.clone(), &token_path).await {
        Err(e) if e.kind() == io::ErrorKind::InvalidData && token_path.exists() => {
            println!(
                "Ignoring unreadable Google credentials in {} ({e}); you will need to sign in again.",
                token_path.display()
            );
            fs::remove_file(&token_path).map_err(yup_oauth2::Error::from)?;
            build_authenticator(secret, &token_path).await
        }
        result => result,
    }
    .map_err(yup_oauth2::Error::from)?;
    // Sign in now rather than at the first request, so that the user is
    // prompted before scraping starts.
    auth.token(SCOPES).await?;
    Ok(auth)
}

async fn build_authenticator(
    secret: ApplicationSecret,
    token_path: &Path,
) -> io::Result<DefaultAuthenticator> {
    InstalledFlowAuthenticator::builder(secret, InstalledFlowReturnMethod::HTTPRedirect)
        .persist_tokens_to_disk(token_path)
        .flow_delegate(Box::new(BrowserDelegate))
        .build()
        .await
}

/// The OAuth client whose ID and secret are in the [`CLIENT_ID_VAR`] and
/// [`CLIENT_SECRET_VAR`] variables, as read by `var`. Empty values count as
/// unset.
fn client_secret(var: impl Fn(&str) -> Option<String>) -> Result<ApplicationSecret, GcalError> {
    let var = |name| var(name).filter(|value| !value.trim().is_empty());
    match (var(CLIENT_ID_VAR), var(CLIENT_SECRET_VAR)) {
        (Some(client_id), Some(client_secret)) => Ok(ApplicationSecret {
            client_id: client_id.trim().to_owned(),
            client_secret: client_secret.trim().to_owned(),
            auth_uri: "https://accounts.google.com/o/oauth2/auth".to_owned(),
            token_uri: "https://oauth2.googleapis.com/token".to_owned(),
            redirect_uris: vec!["http://localhost".to_owned()],
            ..Default::default()
        }),
        (client_id, client_secret) => Err(GcalError::MissingClient {
            missing: [
                (CLIENT_ID_VAR, client_id),
                (CLIENT_SECRET_VAR, client_secret),
            ]
            .into_iter()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name)
            .collect(),
        }),
    }
}

/// Create `dir` and its parents if needed, making `dir` accessible only by
/// the user on Unix.
fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn secret_from(vars: &[(&str, &str)]) -> Result<ApplicationSecret, GcalError> {
        client_secret(|name| {
            vars.iter()
                .find(|(var, _)| *var == name)
                .map(|(_, value)| value.to_string())
        })
    }

    #[test]
    fn reads_client_from_variables() {
        let secret = secret_from(&[
            (CLIENT_ID_VAR, " id.apps.googleusercontent.com\n"),
            (CLIENT_SECRET_VAR, "secret"),
        ])
        .unwrap();
        assert_eq!(secret.client_id, "id.apps.googleusercontent.com");
        assert_eq!(secret.client_secret, "secret");
    }

    #[test]
    fn reports_missing_and_empty_variables() {
        match secret_from(&[(CLIENT_ID_VAR, "id"), (CLIENT_SECRET_VAR, " ")]) {
            Err(GcalError::MissingClient { missing }) => assert_eq!(missing, [CLIENT_SECRET_VAR]),
            other => panic!("unexpected result: {other:?}"),
        }
        match secret_from(&[]) {
            Err(GcalError::MissingClient { missing }) => {
                assert_eq!(missing, [CLIENT_ID_VAR, CLIENT_SECRET_VAR]);
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }
}
