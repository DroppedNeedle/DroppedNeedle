//! The live Last.fm client for linking a user's account.
//!
//! Both calls are signed with the user's own shared secret (v2 signed them
//! too) and go through the one Last.fm client, on the same bucket as every
//! other Last.fm call, at user priority: a person is waiting. Keys,
//! secrets, tokens and session keys never reach a log line or an error.

use droppedneedle_providers::lastfm::{LastFmClient, LastFmCredentials};
use droppedneedle_providers::{ProviderError, RequestPriority};

use super::stores::{BoxFuture, LastFmAuthClient, LastFmError};
use crate::federated::oidc::form_encode;

/// Where the user approves a token.
pub const AUTH_PAGE: &str = "https://www.last.fm/api/auth/";

/// A person waits on both calls.
const PRIORITY: RequestPriority = RequestPriority::UserInitiated;

/// Live [`LastFmAuthClient`] over the one Last.fm client.
#[derive(Clone)]
pub struct LastFmAuthHttp {
    client: LastFmClient,
}

impl LastFmAuthHttp {
    /// Link accounts through `client`.
    pub fn new(client: LastFmClient) -> Self {
        Self { client }
    }
}

fn credentials(api_key: &str, shared_secret: &str) -> LastFmCredentials {
    LastFmCredentials {
        api_key: api_key.to_owned(),
        shared_secret: shared_secret.to_owned(),
        username: None,
        session_key: None,
    }
}

/// Map a failed call. A key, signature or expired token Last.fm refused is
/// the caller's settings; an unapproved token (error 14 and the other
/// authorization codes) on the session exchange is the user's step still
/// to take; anything else (offline, rate limit, server errors) is an
/// outage.
fn link_error(error: &ProviderError, exchanging: bool) -> LastFmError {
    tracing::info!(%error, "last.fm refused the link call");
    match error {
        ProviderError::Forbidden { .. } if exchanging => LastFmError::TokenNotAuthorized,
        ProviderError::Unauthorized { .. }
        | ProviderError::Forbidden { .. }
        | ProviderError::NotFound { .. }
        | ProviderError::NotConfigured { .. } => LastFmError::Configuration,
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
            let token = self
                .client
                .request_token(&credentials(api_key, shared_secret), PRIORITY)
                .await
                .map_err(|error| link_error(&error, false))?
                .token;
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
            let session = self
                .client
                .exchange_session(&credentials(api_key, shared_secret), token, PRIORITY)
                .await
                .map_err(|error| link_error(&error, true))?;
            Ok((session.name, session.key))
        })
    }
}
