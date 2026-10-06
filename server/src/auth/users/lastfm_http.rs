//! The live Last.fm client for linking a user's account.
//!
//! Both calls are signed with the user's own shared secret (v2 signed them
//! too). Keys, secrets, tokens and session keys never reach a log line;
//! the request URL carries them, so transport errors drop it.

use std::time::Duration;

use super::lastfm_models::{ErrorWire, SessionWire, TokenWire};
use super::stores::{BoxFuture, LastFmAuthClient, LastFmError};
use crate::auth::federated::oidc::form_encode;
use crate::providers::lastfm::{DEFAULT_BASE_URL, api_sig};

/// Where the user approves a token.
pub const AUTH_PAGE: &str = "https://www.last.fm/api/auth/";
/// Request timeout (v2 parity).
const TIMEOUT: Duration = Duration::from_secs(15);

/// Live [`LastFmAuthClient`] over the shared outbound client.
#[derive(Clone)]
pub struct LastFmAuthHttp {
    http: reqwest::Client,
    base: String,
}

impl LastFmAuthHttp {
    /// Build against the real Last.fm API.
    pub fn new(http: reqwest::Client) -> Self {
        Self::with_base(http, DEFAULT_BASE_URL)
    }

    /// Build against another API root (tests point this at a mock).
    pub fn with_base(http: reqwest::Client, base: &str) -> Self {
        Self {
            http,
            base: base.to_owned(),
        }
    }

    /// One signed GET; the decoded JSON body, or the mapped Last.fm error.
    async fn signed_call(
        &self,
        method: &str,
        api_key: &str,
        shared_secret: &str,
        extra: &[(&str, &str)],
    ) -> Result<serde_json::Value, LastFmError> {
        let mut params: Vec<(String, String)> = vec![
            ("method".to_owned(), method.to_owned()),
            ("api_key".to_owned(), api_key.to_owned()),
        ];
        params.extend(
            extra
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
        );
        let signature = api_sig(&params, shared_secret);
        params.push(("api_sig".to_owned(), signature));
        params.push(("format".to_owned(), "json".to_owned()));
        let response = self
            .http
            .get(&self.base)
            .query(&params)
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|cause| {
                tracing::warn!(cause = %cause.without_url(), method, "last.fm request failed");
                LastFmError::Transport
            })?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|cause| {
            tracing::warn!(cause = %cause.without_url(), method, "last.fm body read failed");
            LastFmError::Transport
        })?;
        if let Ok(error) = serde_json::from_slice::<ErrorWire>(&bytes) {
            tracing::info!(code = error.error, message = %error.message, method, "last.fm refused the call");
            return Err(map_error_code(error.error));
        }
        if !status.is_success() {
            tracing::warn!(%status, method, "last.fm answered an error");
            return Err(LastFmError::Transport);
        }
        serde_json::from_slice(&bytes).map_err(|_| {
            tracing::warn!(method, "last.fm answered unreadable JSON");
            LastFmError::Transport
        })
    }
}

/// Last.fm error codes: 14 is an unapproved token; bad keys, signatures,
/// tokens and suspended keys are the caller's settings; everything else
/// (service offline, rate limit, server errors) is an outage.
fn map_error_code(code: i64) -> LastFmError {
    match code {
        14 => LastFmError::TokenNotAuthorized,
        4 | 6 | 9 | 10 | 13 | 15 | 26 => LastFmError::Configuration,
        _ => LastFmError::Transport,
    }
}

impl LastFmAuthClient for LastFmAuthHttp {
    fn request_token<'a>(
        &'a self,
        api_key: &'a str,
        shared_secret: &'a str,
    ) -> BoxFuture<'a, Result<(String, String), LastFmError>> {
        Box::pin(async move {
            let body = self
                .signed_call("auth.getToken", api_key, shared_secret, &[])
                .await?;
            let token = serde_json::from_value::<TokenWire>(body)
                .map_err(|_| LastFmError::Transport)?
                .token;
            if token.is_empty() {
                return Err(LastFmError::Transport);
            }
            let auth_url = format!(
                "{AUTH_PAGE}?api_key={}&token={}",
                form_encode(api_key),
                form_encode(&token)
            );
            Ok((token, auth_url))
        })
    }

    fn exchange_session<'a>(
        &'a self,
        api_key: &'a str,
        shared_secret: &'a str,
        token: &'a str,
    ) -> BoxFuture<'a, Result<(String, String), LastFmError>> {
        Box::pin(async move {
            let body = self
                .signed_call(
                    "auth.getSession",
                    api_key,
                    shared_secret,
                    &[("token", token)],
                )
                .await?;
            let session = serde_json::from_value::<SessionWire>(body)
                .map_err(|_| LastFmError::Transport)?
                .session;
            if session.name.is_empty() || session.key.is_empty() {
                return Err(LastFmError::Transport);
            }
            Ok((session.name, session.key))
        })
    }
}
