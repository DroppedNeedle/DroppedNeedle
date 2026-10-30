//! The OpenID Connect provider an admin configured: its discovery
//! document, its signing keys, the token endpoint and userinfo.
//!
//! Every call goes through the executor at the `oidc` rate row. The
//! addresses come from the OIDC settings and the discovery document, so
//! the caller hands over each one already checked against its transport
//! rule, and the request goes to it exactly as written. Redirects are
//! refused: one could downgrade a call to plain http after that check.
//! The wire shapes and what a failure means for a sign-in stay with the
//! caller, which decodes each answer into its own types.

use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::RequestPriority;
use crate::client::decode;
use crate::error::ProviderError;
use crate::executor::{
    Backoff, Executor, Request, Spec, USER_COOLDOWN_WAIT, Url, standard_classify,
};

/// The provider name errors carry, and its rate row.
pub const SOURCE: &str = "oidc";
/// Largest answer read.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Discovery, signing keys and userinfo: the standard rules with a ten
/// second deadline. No provider publishes a rate-limit answer beyond a
/// 429; one that names no wait rests the row five seconds.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(10),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(5)),
    retry_after_cap: Duration::from_secs(60),
    classify: standard_classify,
};

/// The token endpoint: as [`SPEC`], with fifteen seconds to answer.
pub static TOKEN_SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(5)),
    retry_after_cap: Duration::from_secs(60),
    classify: standard_classify,
};

/// The OIDC client. Cheap to clone; clones share the row.
#[derive(Debug, Clone)]
pub struct OidcClient {
    reads: Executor,
    token: Executor,
}

impl OidcClient {
    /// Reads through `reads` and code exchanges through `token`, both on
    /// the `oidc` row.
    #[must_use]
    pub fn new(reads: Executor, token: Executor) -> Self {
        Self { reads, token }
    }

    /// Whether the row can cover `calls` requests a person is waiting on,
    /// as [`crate::plextv::PlexTvClient::room_for`] judges it. A step that
    /// spends something it cannot get back (a login state, a one-time
    /// code) asks first, so pacing never turns it down halfway.
    ///
    /// # Errors
    ///
    /// [`ProviderError::RateLimited`] with the wait for all of them.
    pub fn room_for(&self, calls: u32) -> Result<(), ProviderError> {
        let wait = self.reads.limiter().retry_after(calls);
        if wait <= USER_COOLDOWN_WAIT {
            return Ok(());
        }
        Err(ProviderError::RateLimited {
            provider: SOURCE,
            retry_after: Some(wait),
        })
    }

    /// GET the JSON document at `url`, with `bearer` as the access token
    /// when given.
    ///
    /// # Errors
    ///
    /// The executor's typed failure (a redirect is
    /// [`ProviderError::Redirect`]), or a payload error for an answer
    /// that does not decode as `T`.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        url: &str,
        bearer: Option<&str>,
        priority: RequestPriority,
    ) -> Result<T, ProviderError> {
        let mut request = Request::get_url(parse(url)?).header("accept", "application/json");
        if let Some(token) = bearer {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        decode(&self.reads.send(request, priority).await?)
    }

    /// POST `form` to the token endpoint at `url`. Sent once, never
    /// retried: an authorization code is good for one try.
    ///
    /// # Errors
    ///
    /// As [`OidcClient::get_json`].
    pub async fn exchange<T: DeserializeOwned>(
        &self,
        url: &str,
        form: Vec<(String, String)>,
        priority: RequestPriority,
    ) -> Result<T, ProviderError> {
        let request = Request::post_form_url(parse(url)?, form)
            .header("accept", "application/json")
            .idempotent(false);
        decode(&self.token.send(request, priority).await?)
    }
}

/// `raw` as an http or https URL.
fn parse(raw: &str) -> Result<Url, ProviderError> {
    Url::parse(raw)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .ok_or(ProviderError::Invalid {
            provider: SOURCE,
            reason: "the provider address is not usable",
        })
}
