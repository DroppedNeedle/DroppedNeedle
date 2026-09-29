//! The common provider-client surface and its contract test.
//!
//! Every provider client (MusicBrainz, ListenBrainz, AudioDB, AcoustID,
//! Cover Art Archive, Last.fm) implements [`ProviderClient`]: a lowercase
//! source key, the verified [`RatePolicy`], the cache prefixes it writes,
//! and status classification with the shared default. The trait is
//! deliberately small — call shapes differ per provider, but pacing,
//! caching, degradation, and error semantics stay uniform.
//!
//! [`check_client_contract`] is the conformance harness: each slice's test
//! suite calls it against its client and fails on any violation. It returns
//! a [`ContractViolation`] listing every breach instead of asserting, so the
//! harness itself stays free of test-only panics and slices choose how to
//! surface the report.
//!
//! Copy-paste surface for other slices:
//!
//! ```rust,no_run
//! use droppedneedle::providers::client::ProviderClient;
//! use droppedneedle::providers::limiter::{RatePolicy, policy_for};
//!
//! pub struct MusicBrainzClient {
//!     // ... http client, cache handle, singleflight, deps ...
//! }
//!
//! impl ProviderClient for MusicBrainzClient {
//!     fn source(&self) -> &'static str {
//!         "musicbrainz"
//!     }
//!
//!     fn rate_policy(&self) -> RatePolicy {
//!         policy_for("musicbrainz").expect("verified row exists")
//!     }
//!
//!     fn cache_prefixes(&self) -> &'static [&'static str] {
//!         &["mb:rg:detail:", "mb:release:detail:"]
//!     }
//!
//!     // `classify` keeps the shared default unless the provider speaks in
//!     // body codes (Last.fm's error 29 inside a 200 is the known case).
//! }
//! ```

use std::time::Duration;

use thiserror::Error;

use super::{
    cache::{prefix_is_registered, prefixes_for},
    error::{ProviderError, classify_status},
    limiter::{RatePolicy, policy_for},
};

/// One scripted or live HTTP answer on the catalog GET port.
#[derive(Debug, Clone)]
pub struct HttpReply {
    /// HTTP status code.
    pub status: u16,
    /// Raw response body.
    pub body: Vec<u8>,
    /// Parsed `Retry-After` header value, when the transport saw one.
    pub retry_after: Option<String>,
}

/// The transport broke down before producing a status code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpFault;

/// Smallest HTTP surface the catalog clients need: GET with query pairs.
/// The six catalog slices landed with identical local copies of this trait
/// (plus identical reply/fault types); the integrator unified them here so
/// one fake serves every catalog client and the production
/// [`ReqwestGet`](super::adapters::ReqwestGet) implements it once. Anything
/// before a status line (bad URL, DNS, connect, TLS, timeout, reset,
/// truncated body) is an [`HttpFault`]; every answered status maps into the
/// reply, `Retry-After` included, and the client decides what it means.
pub trait HttpPort {
    /// GET `url` with `query` pairs.
    fn get(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> impl std::future::Future<Output = Result<HttpReply, HttpFault>> + Send;
}

/// The surface every provider client implements.
///
/// `source` names the provider, `rate_policy` paces it, `cache_prefixes`
/// scopes its invalidation, and `classify` types its failures. Everything
/// else (typed call methods, models, endpoint paths) lives on the concrete
/// client.
pub trait ProviderClient: Send + Sync {
    /// Lowercase provider key, e.g. `"musicbrainz"`.
    fn source(&self) -> &'static str;

    /// The verified rate row, always `policy_for(Self::source)`.
    fn rate_policy(&self) -> RatePolicy;

    /// Cache prefixes this client writes. Each must sit under one of the
    /// source's registered invalidation roots, so sweeping the roots covers
    /// everything the client cached.
    fn cache_prefixes(&self) -> &'static [&'static str];

    /// Type one HTTP status, or `None` for success. The default is the
    /// shared [`classify_status`]; providers whose errors hide in 200
    /// bodies (Last.fm) override this to decode them.
    fn classify(&self, status: u16, retry_after: Option<&str>) -> Option<ProviderError> {
        classify_status(self.source(), status, retry_after)
    }
}

/// Every contract breach found by [`check_client_contract`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("provider client contract violated:\n{}", .failures.join("\n"))]
pub struct ContractViolation {
    /// One human-readable line per breach.
    pub failures: Vec<String>,
}

/// Check one client against the shared contract. `Ok(())` means full
/// conformance; `Err` lists every breach at once so slices fix them in one
/// pass. The checks:
///
/// - `source` is non-empty lowercase ASCII (the key other tables join on).
/// - `rate_policy` equals the verified row for `source`.
/// - `cache_prefixes` is non-empty, each prefix non-empty, each under a
///   registered root for `source`.
/// - `classify` keeps distinct 401/403/404/400 semantics (never a blanket
///   mapping), honors `Retry-After` on 429/503, types 5xx as retriable
///   server failures, and returns `None` for 2xx.
pub fn check_client_contract<C: ProviderClient>(client: &C) -> Result<(), ContractViolation> {
    let mut failures = Vec::new();
    let source = client.source();

    if source.is_empty()
        || !source
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        failures.push(format!(
            "source {source:?} must be non-empty lowercase alphanumeric"
        ));
    }

    match policy_for(source) {
        Some(expected) if expected == client.rate_policy() => {}
        Some(expected) => failures.push(format!(
            "rate_policy {:?} does not match the verified row {expected:?} for {source:?}",
            client.rate_policy()
        )),
        None => failures.push(format!("source {source:?} has no verified rate row")),
    }

    let prefixes = client.cache_prefixes();
    if prefixes.is_empty() {
        failures.push(format!("cache_prefixes for {source:?} must not be empty"));
    }
    for prefix in prefixes {
        if prefix.is_empty() {
            failures.push(format!(
                "cache_prefixes for {source:?} holds an empty prefix"
            ));
        } else if !prefix_is_registered(source, prefix) {
            failures.push(format!(
                "prefix {prefix:?} is not under a registered root for {source:?} \
                 (roots: {:?}); sweeping would miss it",
                prefixes_for(source)
            ));
        }
    }

    check_classification(client, &mut failures);
    if failures.is_empty() {
        Ok(())
    } else {
        Err(ContractViolation { failures })
    }
}

fn check_classification<C: ProviderClient>(client: &C, failures: &mut Vec<String>) {
    let source = client.source();
    // 2xx is success.
    for status in [200, 201, 204] {
        if client.classify(status, None).is_some() {
            failures.push(format!("classify({status}) for {source:?} must be None"));
        }
    }
    // Each client-fault family keeps its own meaning; a blanket mapping
    // collapses these and fails here.
    if !matches!(
        client.classify(401, None),
        Some(ProviderError::Unauthorized { .. })
    ) {
        failures.push(format!(
            "classify(401) for {source:?} must be Unauthorized, keeping credentials \
             failures distinct from outages"
        ));
    }
    if !matches!(
        client.classify(403, None),
        Some(ProviderError::Forbidden { .. })
    ) {
        failures.push(format!("classify(403) for {source:?} must be Forbidden"));
    }
    if !matches!(
        client.classify(404, None),
        Some(ProviderError::NotFound { .. })
    ) {
        failures.push(format!(
            "classify(404) for {source:?} must be NotFound (absence, never failure)"
        ));
    }
    match client.classify(400, None) {
        Some(error) if !error.is_retriable() => {}
        other => failures.push(format!(
            "classify(400) for {source:?} must be a non-retriable rejection, got {other:?}"
        )),
    }
    // 429/503 are retriable and honor Retry-After.
    match client.classify(429, Some("7")) {
        Some(error)
            if error.is_retriable() && error.retry_after() == Some(Duration::from_secs(7)) => {}
        other => failures.push(format!(
            "classify(429) for {source:?} must be retriable with the 7s Retry-After honored, \
             got {other:?}"
        )),
    }
    match client.classify(503, None) {
        Some(error) if error.is_retriable() => {}
        other => failures.push(format!(
            "classify(503) for {source:?} must be retriable, got {other:?}"
        )),
    }
    // Other 5xx stay retriable server failures, distinct from client faults.
    match client.classify(500, None) {
        Some(error) if error.is_retriable() => {}
        other => failures.push(format!(
            "classify(500) for {source:?} must be a retriable server failure, got {other:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct GoodClient;

    impl ProviderClient for GoodClient {
        fn source(&self) -> &'static str {
            "lastfm"
        }

        fn rate_policy(&self) -> RatePolicy {
            policy_for("lastfm").expect("verified row exists in tests")
        }

        fn cache_prefixes(&self) -> &'static [&'static str] {
            &["lfm_", "lfm_management:album-genres:"]
        }
    }

    struct BlanketClient;

    impl ProviderClient for BlanketClient {
        fn source(&self) -> &'static str {
            "audiodb"
        }

        fn rate_policy(&self) -> RatePolicy {
            policy_for("audiodb").expect("verified row exists in tests")
        }

        fn cache_prefixes(&self) -> &'static [&'static str] {
            &["audiodb_"]
        }

        fn classify(&self, status: u16, retry_after: Option<&str>) -> Option<ProviderError> {
            // The banned shape: every non-2xx collapses into one variant.
            if (200..300).contains(&status) {
                None
            } else {
                classify_status(self.source(), 503, retry_after)
            }
        }
    }

    struct WrongRateClient;

    impl ProviderClient for WrongRateClient {
        fn source(&self) -> &'static str {
            "musicbrainz"
        }

        fn rate_policy(&self) -> RatePolicy {
            RatePolicy::new(100.0, 100)
        }

        fn cache_prefixes(&self) -> &'static [&'static str] {
            &["mb:rg:detail:"]
        }
    }

    struct StrayPrefixClient;

    impl ProviderClient for StrayPrefixClient {
        fn source(&self) -> &'static str {
            "lastfm"
        }

        fn rate_policy(&self) -> RatePolicy {
            policy_for("lastfm").expect("verified row exists in tests")
        }

        fn cache_prefixes(&self) -> &'static [&'static str] {
            &["lfm_", "stray:"]
        }
    }

    #[test]
    fn conforming_client_passes() {
        check_client_contract(&GoodClient).expect("good client conforms");
    }

    #[test]
    fn blanket_mapping_fails_with_every_collapsed_family() {
        let violation =
            check_client_contract(&BlanketClient).expect_err("blanket mapping must fail");
        let report = violation.to_string();
        for needle in ["401", "403", "404", "400"] {
            assert!(
                report.contains(needle),
                "report names the collapsed {needle} family:\n{report}"
            );
        }
    }

    #[test]
    fn wrong_rate_and_stray_prefix_fail() {
        let violation = check_client_contract(&WrongRateClient).expect_err("wrong rate must fail");
        assert!(
            violation.to_string().contains("verified row"),
            "report names the row mismatch: {violation}"
        );
        let violation =
            check_client_contract(&StrayPrefixClient).expect_err("stray prefix must fail");
        assert!(
            violation.to_string().contains("stray:"),
            "report names the stray prefix: {violation}"
        );
    }
}
