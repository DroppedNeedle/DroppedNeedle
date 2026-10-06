//! The live OIDC client over the factory's no-redirect HTTP client.
//!
//! Discovery documents are cached per issuer for a day and signing keys
//! per `jwks_uri` for an hour (a token naming an unknown key forces one
//! refetch). Every provider URL passes the transport rule in
//! [`super::transport`] before it is used, redirects are never followed
//! (one could downgrade a call to plain http after the check), and bodies
//! are capped at 1 MiB. Tokens, secrets and codes never reach a log line.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::FederatedError;
use super::oidc::{
    DISCOVERY_SUFFIX, DiscoveryDoc, OidcIdp, OidcTokens, RawClaims, TokenRequest, form_encode,
    raw_claims,
};
use super::oidc_models::{DiscoveryWire, Jwk, JwksWire, TokenWire};
use super::transport::check_provider_url;

/// Discovery fetch timeout (v2 parity).
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
/// Token exchange timeout (v2 parity).
const TOKEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Signing-key and userinfo fetch timeout.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a discovery document stays cached (v2 `_DISCOVERY_TTL`).
const DISCOVERY_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// How long a key set stays cached.
const JWKS_TTL: Duration = Duration::from_secs(60 * 60);
/// Largest provider answer we read.
const MAX_BODY: usize = 1024 * 1024;

#[derive(Default)]
struct Cache {
    discovery: HashMap<String, (Instant, DiscoveryDoc)>,
    keys: HashMap<String, (Instant, Vec<Jwk>)>,
}

/// Live [`OidcIdp`]. Cheap to clone; clones share the cache.
#[derive(Clone)]
pub struct OidcHttp {
    http: reqwest::Client,
    cache: Arc<Mutex<Cache>>,
}

impl OidcHttp {
    /// Build over the factory's no-redirect client.
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            cache: Arc::new(Mutex::new(Cache::default())),
        }
    }

    fn cached<T: Clone>(
        &self,
        pick: impl FnOnce(&Cache) -> Option<&(Instant, T)>,
        ttl: Duration,
    ) -> Option<T> {
        let cache = self.cache.lock().ok()?;
        pick(&cache)
            .filter(|(at, _)| at.elapsed() < ttl)
            .map(|(_, value)| value.clone())
    }

    fn remember(&self, store: impl FnOnce(&mut Cache)) {
        if let Ok(mut cache) = self.cache.lock() {
            store(&mut cache);
        }
    }
}

fn unreachable(cause: reqwest::Error) -> FederatedError {
    tracing::debug!(cause = %cause.without_url(), "OIDC provider request failed");
    FederatedError::ProviderUnavailable("Could not reach OIDC provider".to_owned())
}

/// Refuse a redirect: the client follows none, and the admin should enter
/// the final URL.
fn refuse_redirect(response: &reqwest::Response) -> Result<(), FederatedError> {
    if response.status().is_redirection() {
        tracing::warn!(status = %response.status(), "OIDC provider answered with a redirect");
        return Err(FederatedError::ProviderUnavailable(
            "OIDC provider answered with a redirect; configure its final URL".to_owned(),
        ));
    }
    Ok(())
}

/// Read a body of at most [`MAX_BODY`] bytes.
async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>, FederatedError> {
    let too_large =
        || FederatedError::ProviderUnavailable("OIDC provider answer is too large".to_owned());
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY as u64)
    {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(unreachable)? {
        if body.len() + chunk.len() > MAX_BODY {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Apply the transport rule to one provider URL.
async fn require_allowed(url: &str) -> Result<(), FederatedError> {
    check_provider_url(url).await.map_err(|reason| {
        tracing::warn!(%reason, "OIDC provider URL refused");
        FederatedError::NotConfigured(reason)
    })
}

/// Issuers compare without a trailing slash; admins type both forms.
fn same_issuer(left: &str, right: &str) -> bool {
    left.trim_end_matches('/') == right.trim_end_matches('/')
}

impl OidcIdp for OidcHttp {
    async fn discover(&self, issuer: &str) -> Result<DiscoveryDoc, FederatedError> {
        let key = issuer.trim_end_matches('/').to_owned();
        if let Some(doc) = self.cached(|cache| cache.discovery.get(&key), DISCOVERY_TTL) {
            return Ok(doc);
        }
        require_allowed(&key).await?;
        let response = self
            .http
            .get(format!("{key}{DISCOVERY_SUFFIX}"))
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(DISCOVERY_TIMEOUT)
            .send()
            .await
            .map_err(unreachable)?;
        refuse_redirect(&response)?;
        if response.status() != reqwest::StatusCode::OK {
            tracing::debug!(status = %response.status(), "OIDC discovery answered non-200");
            return Err(FederatedError::ProviderUnavailable(
                "Failed to fetch OIDC discovery document".to_owned(),
            ));
        }
        let bytes = read_capped(response).await?;
        let wire: DiscoveryWire = serde_json::from_slice(&bytes).map_err(|_| {
            FederatedError::NotConfigured(
                "OIDC discovery document is missing a required field".to_owned(),
            )
        })?;
        if !same_issuer(&wire.issuer, issuer) {
            return Err(FederatedError::NotConfigured(
                "OIDC discovery document names a different issuer".to_owned(),
            ));
        }
        let doc = DiscoveryDoc {
            issuer: wire.issuer,
            authorization_endpoint: wire.authorization_endpoint,
            token_endpoint: wire.token_endpoint,
            userinfo_endpoint: wire.userinfo_endpoint.filter(|url| !url.is_empty()),
            jwks_uri: wire.jwks_uri,
            id_token_algs: wire.id_token_signing_alg_values_supported,
        };
        // Every endpoint the document names follows the same rule as the
        // issuer: a public https issuer cannot point at a public http one.
        for url in [
            Some(doc.authorization_endpoint.as_str()),
            Some(doc.token_endpoint.as_str()),
            Some(doc.jwks_uri.as_str()),
            doc.userinfo_endpoint.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            require_allowed(url).await?;
        }
        tracing::info!(issuer = %key, "OIDC discovery cached");
        self.remember(|cache| {
            cache.discovery.insert(key, (Instant::now(), doc.clone()));
        });
        Ok(doc)
    }

    async fn exchange_code(&self, request: &TokenRequest) -> Result<OidcTokens, FederatedError> {
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", request.code.as_str()),
            ("redirect_uri", request.redirect_uri.as_str()),
            ("client_id", request.client_id.as_str()),
        ];
        // A confidential client sends its secret; a public client has none
        // and the PKCE verifier alone proves it started this login.
        if let Some(secret) = request.client_secret.as_deref() {
            form.push(("client_secret", secret));
        }
        if let Some(verifier) = request.code_verifier.as_deref() {
            form.push(("code_verifier", verifier));
        }
        let body = form
            .iter()
            .map(|(key, value)| format!("{key}={}", form_encode(value)))
            .collect::<Vec<_>>()
            .join("&");
        let response = self
            .http
            .post(&request.token_endpoint)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .timeout(TOKEN_TIMEOUT)
            .send()
            .await
            .map_err(unreachable)?;
        refuse_redirect(&response)?;
        let status = response.status();
        if status != reqwest::StatusCode::OK && status != reqwest::StatusCode::CREATED {
            tracing::debug!(%status, "OIDC token endpoint refused the code");
            return Err(FederatedError::Authentication(
                "OIDC token exchange failed".to_owned(),
            ));
        }
        let bytes = read_capped(response).await?;
        let wire: TokenWire = serde_json::from_slice(&bytes).map_err(|_| {
            FederatedError::ProviderUnavailable(
                "OIDC provider returned an unexpected response".to_owned(),
            )
        })?;
        Ok(OidcTokens {
            access_token: wire.access_token,
            refresh_token: wire.refresh_token.unwrap_or_default(),
            id_token: wire.id_token.unwrap_or_default(),
        })
    }

    async fn signing_keys(
        &self,
        jwks_uri: &str,
        refresh: bool,
    ) -> Result<Vec<Jwk>, FederatedError> {
        if !refresh && let Some(keys) = self.cached(|cache| cache.keys.get(jwks_uri), JWKS_TTL) {
            return Ok(keys);
        }
        let failed = || {
            FederatedError::ProviderUnavailable("Could not fetch the OIDC signing keys".to_owned())
        };
        let response = self
            .http
            .get(jwks_uri)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(FETCH_TIMEOUT)
            .send()
            .await
            .map_err(unreachable)?;
        refuse_redirect(&response)?;
        if !response.status().is_success() {
            tracing::debug!(status = %response.status(), "OIDC key set answered non-2xx");
            return Err(failed());
        }
        let bytes = read_capped(response).await?;
        let keys = serde_json::from_slice::<JwksWire>(&bytes)
            .map_err(|_| failed())?
            .into_keys();
        self.remember(|cache| {
            cache
                .keys
                .insert(jwks_uri.to_owned(), (Instant::now(), keys.clone()));
        });
        Ok(keys)
    }

    async fn userinfo(&self, userinfo_endpoint: &str, access_token: &str) -> Option<RawClaims> {
        let response = self
            .http
            .get(userinfo_endpoint)
            .bearer_auth(access_token)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(FETCH_TIMEOUT)
            .send()
            .await;
        let response = match response {
            Ok(response) if response.status() == reqwest::StatusCode::OK => response,
            Ok(response) => {
                tracing::debug!(status = %response.status(), "OIDC userinfo answered non-200; using id_token claims");
                return None;
            }
            Err(cause) => {
                tracing::debug!(cause = %cause.without_url(), "OIDC userinfo failed; using id_token claims");
                return None;
            }
        };
        let bytes = read_capped(response).await.ok()?;
        match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(serde_json::Value::Object(claims)) => Some(raw_claims(&claims)),
            _ => {
                tracing::debug!("OIDC userinfo was not a JSON object; using id_token claims");
                None
            }
        }
    }
}
