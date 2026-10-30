//! The live OIDC client over the providers crate's OIDC client.
//!
//! The issuer comes from the OIDC settings on each login; every other
//! address comes from its discovery document. Discovery documents are
//! cached per issuer for a day and signing keys per `jwks_uri` for an hour
//! (a token naming an unknown key forces one refetch). Every provider URL
//! passes the transport rule in [`super::transport`] before it is used.
//! Each call goes through the executor at the `oidc` rate row, at user
//! priority since a person is signing in: redirects are never followed
//! (one could downgrade a call to plain http after the check), bodies are
//! capped at 1 MiB, and tokens, secrets and codes never reach a log line.
//!
//! Rules (v2 parity where v2 had the step): discovery with a 10s timeout
//! (a refusal is `ProviderUnavailable`, a document missing an endpoint or
//! `jwks_uri`, or naming a browser address that is not http or https, is
//! `NotConfigured`); token POST with a 15s timeout, sent once (no answer
//! or a rate limit is `ProviderUnavailable`, a refusal is
//! `Authentication("OIDC token exchange failed")`, bad JSON is
//! `ProviderUnavailable`); userinfo answers `None` on any failure so the
//! verified `id_token` claims stand.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use droppedneedle_providers::oidc::OidcClient;
use droppedneedle_providers::{PayloadKind, ProviderError, RequestPriority};

use super::oidc::{
    DISCOVERY_SUFFIX, DiscoveryDoc, OidcTokens, RawClaims, TokenRequest, raw_claims,
};
use super::oidc_models::{DiscoveryWire, Jwk, JwksWire, TokenWire};
use super::transport::check_provider_url;
use super::{FederatedError, endpoint, web_address};

/// How long a discovery document stays cached (v2 `_DISCOVERY_TTL`).
const DISCOVERY_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// How long a key set stays cached.
const JWKS_TTL: Duration = Duration::from_secs(60 * 60);
/// Calls a callback may make after it spends its login state: discovery,
/// the code exchange, the signing keys and one refetch of them, userinfo.
pub const CALLBACK_CALLS: u32 = 5;

#[derive(Default)]
struct Cache {
    discovery: HashMap<String, (Instant, DiscoveryDoc)>,
    keys: HashMap<String, (Instant, Vec<Jwk>)>,
}

/// The OIDC client. Cheap to clone; clones share the cache.
#[derive(Clone)]
pub struct OidcHttp {
    idp: OidcClient,
    cache: Arc<Mutex<Cache>>,
}

impl OidcHttp {
    /// Build over the providers crate's OIDC client.
    pub fn new(idp: OidcClient) -> Self {
        Self {
            idp,
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

    /// Whether the `oidc` row has room for a whole callback. Asked before
    /// the login state is spent, so pacing answers "try again" while the
    /// state and the provider's code are both still good.
    pub fn room_for_callback(&self) -> Result<(), FederatedError> {
        self.idp.room_for(CALLBACK_CALLS).map_err(|error| {
            tracing::debug!(%error, "OIDC callback held for pace");
            busy()
        })
    }
}

fn busy() -> FederatedError {
    FederatedError::ProviderUnavailable("OIDC provider is busy; try again".to_owned())
}

/// The sign-in failure for a call that got no usable answer: a redirect,
/// an answer over the cap, a rate limit, or no answer at all. `None` for
/// a refusal or an answer that does not decode, which each step judges.
fn transport_failure(error: &ProviderError) -> Option<FederatedError> {
    Some(match error {
        ProviderError::Redirect { status, .. } => {
            tracing::warn!(%status, "OIDC provider answered with a redirect");
            FederatedError::ProviderUnavailable(
                "OIDC provider answered with a redirect; configure its final URL".to_owned(),
            )
        }
        ProviderError::Payload {
            kind: PayloadKind::TooLarge,
            ..
        } => FederatedError::ProviderUnavailable("OIDC provider answer is too large".to_owned()),
        ProviderError::RateLimited { .. } => busy(),
        ProviderError::Transport { .. } | ProviderError::Invalid { .. } => {
            tracing::debug!(%error, "OIDC provider request failed");
            FederatedError::ProviderUnavailable("Could not reach OIDC provider".to_owned())
        }
        _ => return None,
    })
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

impl OidcHttp {
    /// Fetch (and cache) the discovery document for `issuer`.
    pub async fn discover(&self, issuer: &str) -> Result<DiscoveryDoc, FederatedError> {
        let key = issuer.trim_end_matches('/').to_owned();
        if let Some(doc) = self.cached(|cache| cache.discovery.get(&key), DISCOVERY_TTL) {
            return Ok(doc);
        }
        require_allowed(&key).await?;
        let Some(base) = web_address(&key) else {
            return Err(FederatedError::NotConfigured(
                "OIDC issuer is not a web address".to_owned(),
            ));
        };
        let segments: Vec<&str> = DISCOVERY_SUFFIX.split('/').collect();
        let wire: DiscoveryWire = self
            .idp
            .get_json(
                endpoint(&base, &segments).as_str(),
                None,
                RequestPriority::UserInitiated,
            )
            .await
            .map_err(|error| {
                transport_failure(&error).unwrap_or_else(|| match error {
                    ProviderError::Payload { message, .. } => {
                        tracing::warn!(%message, "OIDC discovery document refused");
                        FederatedError::NotConfigured(
                            "OIDC discovery document is incomplete or names an invalid address"
                                .to_owned(),
                        )
                    }
                    error => {
                        tracing::debug!(%error, "OIDC discovery refused");
                        FederatedError::ProviderUnavailable(
                            "Failed to fetch OIDC discovery document".to_owned(),
                        )
                    }
                })
            })?;
        if !same_issuer(&wire.issuer, issuer) {
            return Err(FederatedError::NotConfigured(
                "OIDC discovery document names a different issuer".to_owned(),
            ));
        }
        let doc = DiscoveryDoc {
            issuer: wire.issuer,
            authorization_endpoint: wire.authorization_endpoint.into(),
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

    /// Exchange an authorization code for tokens.
    pub async fn exchange_code(
        &self,
        request: &TokenRequest,
    ) -> Result<OidcTokens, FederatedError> {
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
        let form = form
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        let wire: TokenWire = self
            .idp
            .exchange(
                &request.token_endpoint,
                form,
                RequestPriority::UserInitiated,
            )
            .await
            .map_err(|error| {
                transport_failure(&error).unwrap_or_else(|| match error {
                    ProviderError::Payload { .. } => FederatedError::ProviderUnavailable(
                        "OIDC provider returned an unexpected response".to_owned(),
                    ),
                    error => {
                        tracing::debug!(%error, "OIDC token endpoint refused the code");
                        FederatedError::Authentication("OIDC token exchange failed".to_owned())
                    }
                })
            })?;
        Ok(OidcTokens {
            access_token: wire.access_token,
            refresh_token: wire.refresh_token.unwrap_or_default(),
            id_token: wire.id_token.unwrap_or_default(),
        })
    }

    /// The provider's published signing keys. `refresh` bypasses the cache
    /// (used once when a token names a key the cached set lacks).
    pub async fn signing_keys(
        &self,
        jwks_uri: &str,
        refresh: bool,
    ) -> Result<Vec<Jwk>, FederatedError> {
        if !refresh && let Some(keys) = self.cached(|cache| cache.keys.get(jwks_uri), JWKS_TTL) {
            return Ok(keys);
        }
        let keys = self
            .idp
            .get_json::<JwksWire>(jwks_uri, None, RequestPriority::UserInitiated)
            .await
            .map_err(|error| {
                transport_failure(&error).unwrap_or_else(|| {
                    tracing::debug!(%error, "OIDC key set refused");
                    FederatedError::ProviderUnavailable(
                        "Could not fetch the OIDC signing keys".to_owned(),
                    )
                })
            })?
            .into_keys();
        self.remember(|cache| {
            cache
                .keys
                .insert(jwks_uri.to_owned(), (Instant::now(), keys.clone()));
        });
        Ok(keys)
    }

    /// Userinfo claims, or `None` when the endpoint fails.
    pub async fn userinfo(&self, userinfo_endpoint: &str, access_token: &str) -> Option<RawClaims> {
        match self
            .idp
            .get_json::<serde_json::Map<String, serde_json::Value>>(
                userinfo_endpoint,
                Some(access_token),
                RequestPriority::UserInitiated,
            )
            .await
        {
            Ok(claims) => Some(raw_claims(&claims)),
            Err(error) => {
                tracing::debug!(%error, "OIDC userinfo failed; using id_token claims");
                None
            }
        }
    }
}
