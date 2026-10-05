//! MusicBrainz provider client: search, lookups, and BrainzMash lifecycle.
//!
//! This module ports the v2 `musicbrainz_*` repositories plus the BrainzMash
//! transport. Every behavior first observed against the live service keeps
//! its citation inline. Tolerant where the wire is sparse (unknown fields
//! ignored, display fields optional), strict where identity is at stake
//! (missing entity ids fail decoding; a retired MBID counts as its target
//! only after a lookup proves the redirect).
//!
//! Seam notes (the shared infrastructure lives in the provider core):
//! - [`MbTransport`] stays a typed local port: its requests carry validated
//!   redirect hops the catalog GET port cannot express, and the reqwest
//!   adapter below serves production from the shared client.
//! - [`RateGate`] is a small interval gate on purpose (both MB policies
//!   run capacity 1). Unifying with the shared token buckets stays open:
//!   the official 1/s gate is limiter-shaped, but the BrainzMash cooldown
//!   scheduler is endpoint-specific behavior with no core counterpart.
//! - Degradation rides the shared [`DegradationSink`](super::degradation::DegradationSink)
//!   under the `musicbrainz` source key, with the operation folded into the
//!   message.
//! - Redirect hops are returned to the caller, never persisted here; the
//!   durable canonical map banks them exactly like v2's follow-and-bank.
//! - Retries live above this client (typed errors carry retry hints). The
//!   only exception is the BrainzMash cooldown, which paces attempts inside
//!   [`BrainzMashScheduler`] the way v2 did.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use super::degradation::{DegradationSink, NoopSink};
use serde::Deserialize;
use thiserror::Error;

/// Official MusicBrainz web-service root.
pub const MB_API_BASE: &str = "https://musicbrainz.org/ws/2";
/// The one server-owned BrainzMash origin (v2 `BRAINZMASH_ENDPOINT`).
pub const BRAINZMASH_API_BASE: &str = "https://api.brainzmash.cc/ws/2";
/// Official hard rule: at most one request per second.
/// <https://musicbrainz.org/doc/MusicBrainz_API/Rate_Limiting>
pub const MB_RATE_PER_SEC: f64 = 1.0;
/// BrainzMash sustained policy: 10 requests/second, no burst capacity.
pub const BRAINZMASH_RATE_PER_SEC: f64 = 10.0;
/// Redirect follows per logical lookup (v2 `_BRAINZMASH_MAX_REDIRECTS`).
pub const MAX_REDIRECT_HOPS: usize = 2;
/// BrainzMash cooldown ceiling (v2 `_BRAINZMASH_MAX_COOLDOWN_SECONDS`).
pub const BRAINZMASH_MAX_COOLDOWN_SECS: f64 = 60.0;

/// One outbound GET, transport-agnostic so fakes stay script-only.
#[derive(Debug, Clone)]
pub struct MbRequest {
    /// Full URL without the query string.
    pub url: String,
    /// Query pairs in send order (`fmt=json` is always appended).
    pub query: Vec<(String, String)>,
    /// Headers in send order; the client always sets User-Agent.
    pub headers: Vec<(String, String)>,
}

impl MbRequest {
    /// Render the URL with its encoded query string.
    pub fn full_url(&self) -> String {
        if self.query.is_empty() {
            return self.url.clone();
        }
        let pairs: Vec<String> = self
            .query
            .iter()
            .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
            .collect();
        format!("{}?{}", self.url, pairs.join("&"))
    }

    /// Fetch one header value, case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Minimal raw response: status, the headers this client reads, and bytes.
#[derive(Debug, Clone)]
pub struct RawResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers in arrival order.
    pub headers: Vec<(String, String)>,
    /// Raw body bytes.
    pub body: Vec<u8>,
}

impl RawResponse {
    /// Build a response from the parts fakes script.
    pub fn new(status: u16, headers: Vec<(&str, &str)>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: headers
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
            body: body.into(),
        }
    }

    /// Fetch one header value, case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Wire failure below HTTP semantics (DNS, connect, TLS, timeout, reset).
#[derive(Debug, Clone, Error)]
#[error("musicbrainz transport failure: {0}")]
pub struct TransportError(pub String);

/// Minimal local transport port. The shared core traits replace this seam;
/// until then the reqwest adapter below and the scripted test fake both
/// implement it, and the client never touches the network directly.
pub trait MbTransport: Send + Sync {
    /// Perform one GET, following no redirects (the client walks 3xx hops
    /// itself so every hop is validated and reported).
    fn get(
        &self,
        request: &MbRequest,
    ) -> impl Future<Output = Result<RawResponse, TransportError>> + Send;
}

/// Production adapter over the factory's no-redirect client, which carries
/// the shared timeouts and User-Agent.
pub struct ReqwestMbTransport {
    client: reqwest::Client,
}

impl ReqwestMbTransport {
    /// Wrap `HttpClientFactory::no_redirect`. Redirects must stay off
    /// because both MB clients in v2 set `follow_redirects=False` and
    /// validate each hop by hand.
    pub fn new(no_redirect: reqwest::Client) -> Self {
        Self {
            client: no_redirect,
        }
    }
}

impl MbTransport for ReqwestMbTransport {
    async fn get(&self, request: &MbRequest) -> Result<RawResponse, TransportError> {
        let mut outgoing = self.client.get(request.full_url());
        for (key, value) in &request.headers {
            outgoing = outgoing.header(key.as_str(), value.as_str());
        }
        let response = outgoing
            .send()
            .await
            .map_err(|error| TransportError(error.to_string()))?;
        let status = response.status().as_u16();
        let mut headers = Vec::new();
        for name in ["location", "retry-after"] {
            if let Some(value) = response.headers().get(name)
                && let Ok(text) = value.to_str()
            {
                headers.push((name.to_owned(), text.to_owned()));
            }
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| TransportError(error.to_string()))?
            .to_vec();
        Ok(RawResponse {
            status,
            headers,
            body,
        })
    }
}

/// Interval gate for the capacity-1 rate policies (MB 1/s, BrainzMash 10/s).
/// The mutex is never held across an await.
pub struct RateGate {
    interval: Duration,
    next_allowed: Mutex<Option<tokio::time::Instant>>,
}

impl RateGate {
    /// Gate allowing `rate_per_sec` acquisitions per second, one at a time.
    pub fn new(rate_per_sec: f64) -> Self {
        let interval = Duration::from_secs_f64(1.0 / rate_per_sec.max(f64::MIN_POSITIVE));
        Self {
            interval,
            next_allowed: Mutex::new(None),
        }
    }

    /// The official 1/s hard limiter.
    pub fn musicbrainz() -> Self {
        Self::new(MB_RATE_PER_SEC)
    }

    /// The BrainzMash 10/s limiter.
    pub fn brainzmash() -> Self {
        Self::new(BRAINZMASH_RATE_PER_SEC)
    }

    /// Configured rate, for the policy-table test.
    pub fn rate_per_sec(&self) -> f64 {
        1.0 / self.interval.as_secs_f64()
    }

    /// Wait until one acquisition is due, then take it.
    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut guard = self.next_allowed.lock().unwrap_or_else(|poison| {
                    tracing::warn!("musicbrainz rate-gate lock poisoned; resetting");
                    poison.into_inner()
                });
                let now = tokio::time::Instant::now();
                match *guard {
                    None => {
                        *guard = Some(now + self.interval);
                        None
                    }
                    Some(due) if now >= due => {
                        *guard = Some(now + self.interval);
                        None
                    }
                    Some(due) => Some(due.saturating_duration_since(now)),
                }
            };
            match wait {
                None => return,
                Some(delay) => tokio::time::sleep(delay).await,
            }
        }
    }
}

/// Which upstream answers: official MusicBrainz or a BrainzMash binding.
#[derive(Debug, Clone)]
pub enum MbSource {
    /// Official service (custom base allowed for dedicated test instances).
    Official {
        /// API root; production uses [`MB_API_BASE`].
        base_url: String,
    },
    /// Community mirror behind the pinned endpoint and lifecycle gate.
    BrainzMash {
        /// False until the active binding validates; requests fail closed.
        binding_valid: bool,
    },
}

impl MbSource {
    /// The official production source.
    pub fn official() -> Self {
        Self::Official {
            base_url: MB_API_BASE.to_owned(),
        }
    }

    /// API root for request building.
    pub fn base_url(&self) -> &str {
        match self {
            Self::Official { base_url } => base_url,
            Self::BrainzMash { .. } => BRAINZMASH_API_BASE,
        }
    }

    fn is_brainzmash(&self) -> bool {
        matches!(self, Self::BrainzMash { .. })
    }
}

/// Operation class from the degradation matrix. A dead MusicBrainz fails
/// identity-critical work with a typed error and degrades everything else
/// to a recorded absence (a dead MusicBrainz fails identity-critical work
/// only; v2 grouped search kept the same bucket isolation by returning
/// empty results plus a failure record per dead bucket).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Criticality {
    /// Identity proof: provider death is a typed failure, never silence.
    IdentityCritical,
    /// Display, search acceleration, optional enrichment: provider death
    /// records a degradation and resolves to absence.
    BestEffort,
}

/// Typed MusicBrainz failures. `None`/empty still means absence; these
/// variants mean the provider could not answer or answered badly.
#[derive(Debug, Error, PartialEq)]
pub enum MbError {
    /// Transport dead or 5xx (other than the rate-limit 503): retriable.
    #[error("musicbrainz unavailable: {0}")]
    Unavailable(String),
    /// 429, or 503 which MusicBrainz also uses for rate limiting (v2
    /// `_mb_api_get_attempt` labels 503 "rate limited"). Carries the
    /// honored Retry-After delay when the response gave one.
    #[error("musicbrainz rate limited")]
    RateLimited {
        /// Seconds to wait before retrying, when the response said.
        retry_after_secs: Option<f64>,
    },
    /// Syntactically valid MBID the service rejects, e.g. the all-zero
    /// UUID answered 400 `{"error":"Invalid mbid."}` (live 2026-07-21,
    /// `musicbrainz_MANAGEMENT_API_NOTES.md`).
    #[error("musicbrainz rejected the request as invalid: {0}")]
    InvalidMbid(String),
    /// Other 4xx: the request is wrong, retrying will not help.
    #[error("musicbrainz rejected the request (HTTP {0})")]
    Rejected(u16),
    /// A redirect the client refuses to follow: foreign host, scheme or
    /// port shift, off-`/ws/2` path, or a non-lookup shape such as a
    /// browse URL (v2 `_lookup_redirect_pair` persists lookup-shaped hops
    /// only, since a 301 on `/release-group?artist=` names an artist).
    #[error("musicbrainz redirect rejected: {0}")]
    RedirectRejected(String),
    /// The wire answered 200 but the payload breaks the provider contract
    /// (unparseable body or a missing required identity field). Never
    /// masked as absence: a contract break is a signal, not a miss.
    #[error("musicbrainz contract break: {0}")]
    Contract(String),
    /// Local configuration refuses the request (BrainzMash binding not
    /// valid, unapproved endpoint, bad path). Fail fast, never send.
    #[error("musicbrainz misconfigured: {0}")]
    Misconfigured(String),
}

/// Parse Retry-After in either legal shape: delay seconds or an HTTP date.
/// Delegates to the core [`parse_retry_after`](super::error::parse_retry_after),
/// which covers all three HTTP date shapes where the old local parser read
/// IMF-fixdate only. Mirrors v2 `_parse_retry_after_seconds`: unparseable,
/// non-finite, and negative values yield `None`; valid values clamp to 60
/// seconds.
pub fn parse_retry_after_secs(value: Option<&str>) -> Option<f64> {
    super::error::parse_retry_after(value).map(|delay| delay.as_secs_f64())
}

/// True for the `8-4-4-4-12` hex MBID shape (v2 `is_valid_mbid` rejects
/// `unknown_*` sentinels the same way: not this shape, not valid).
pub fn is_valid_mbid(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.len() != 36 {
        return false;
    }
    for (index, byte) in trimmed.bytes().enumerate() {
        let is_dash_slot = matches!(index, 8 | 13 | 18 | 23);
        if is_dash_slot != (byte == b'-') {
            return false;
        }
        if !is_dash_slot && !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// Lowercase-and-trim identity key for MBIDs (v2 `normalize_mb_id`).
pub fn normalize_mb_id(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

/// Escape user text before placing it inside a Lucene field phrase (v2
/// `escape_lucene_phrase`).
pub fn escape_lucene_phrase(value: &str) -> String {
    const RESERVED: [char; 19] = [
        '+', '-', '&', '|', '!', '(', ')', '{', '}', '[', ']', '^', '"', '~', '*', '?', ':', '\\',
        '/',
    ];
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if RESERVED.contains(&character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Release query, live-verified against MusicBrainz WS/2 on 2026-08-13 (v2
/// `build_release_search_query`).
pub fn build_release_search_query(title: &str, artist: &str) -> String {
    let mut query = format!(r#"release:"{}""#, escape_lucene_phrase(title));
    if !artist.is_empty() {
        query.push_str(&format!(
            r#" AND artist:"{}""#,
            escape_lucene_phrase(artist)
        ));
    }
    query
}

/// Release-group query, live-verified against MusicBrainz WS/2 on 2026-08-13
/// (v2 `build_release_group_search_query`).
pub fn build_release_group_search_query(title: &str, artist: &str) -> String {
    let escaped = escape_lucene_phrase(title);
    let mut query = format!(r#"(releasegroup:"{escaped}" OR release:"{escaped}")"#);
    if !artist.is_empty() {
        query.push_str(&format!(
            r#" AND artist:"{}""#,
            escape_lucene_phrase(artist)
        ));
    }
    query
}

/// Recording query using the same verified Lucene field escaping (v2
/// `build_recording_search_query`).
pub fn build_recording_search_query(title: &str, artist: &str) -> String {
    format!(
        r#"recording:"{}" AND artist:"{}""#,
        escape_lucene_phrase(title),
        escape_lucene_phrase(artist)
    )
}

/// Search-result score from either observed key (v2 `get_score`):
/// `score` first, then `ext:score`, defaulting to zero.
pub fn hit_score(score: Option<i64>, ext_score: Option<i64>) -> i64 {
    score.or(ext_score).unwrap_or(0)
}

/// First artist-credit display name: credited name, then the artist's
/// canonical name (v2 `extract_artist_name`).
pub fn credit_display_name(credit: &[ArtistCreditName]) -> Option<&str> {
    credit.first().and_then(|entry| {
        if !entry.name.is_empty() {
            Some(entry.name.as_str())
        } else if !entry.artist.name.is_empty() {
            Some(entry.artist.name.as_str())
        } else {
            None
        }
    })
}

/// Leading year of an MB date, tolerating partial dates (v2 `parse_year`).
pub fn parse_year(date: Option<&str>) -> Option<i32> {
    let year = date?.split('-').next()?;
    if !year.is_empty() && year.bytes().all(|byte| byte.is_ascii_digit()) {
        year.parse().ok()
    } else {
        None
    }
}

/// One followed redirect hop: same entity on both ends, both MBIDs valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectHop {
    /// Entity kind (`artist`, `release`, `release-group`, `recording`).
    pub entity: String,
    /// Retired MBID the client requested.
    pub from_mbid: String,
    /// Canonical MBID the provider pointed at.
    pub to_mbid: String,
}

/// Entities whose lookup redirects persist (v2 `_MB_REDIRECT_ENTITY_KINDS`).
const REDIRECT_ENTITY_KINDS: [&str; 4] = ["artist", "release", "release-group", "recording"];

/// Validate one followed hop as an entity/from/to triple. Only
/// lookup-shaped paths persist: exactly two segments, an allowlisted entity
/// on both ends, the same entity on each end, both MBIDs valid. A 301 on a
/// browse path such as `/release-group?artist=` is never persisted, since
/// the only MBID in play there belongs to another entity (v2
/// `_lookup_redirect_pair`).
pub fn lookup_redirect_pair(from_path: &str, hop_path: &str) -> Option<RedirectHop> {
    let from_segments: Vec<&str> = from_path
        .split('?')
        .next()?
        .trim_matches('/')
        .split('/')
        .collect();
    let hop_segments: Vec<&str> = hop_path
        .split('?')
        .next()?
        .trim_matches('/')
        .split('/')
        .collect();
    if from_segments.len() != 2 || hop_segments.len() != 2 {
        return None;
    }
    let (entity, from_mbid) = (from_segments[0], from_segments[1]);
    let (hop_entity, to_mbid) = (hop_segments[0], hop_segments[1]);
    if entity != hop_entity || !REDIRECT_ENTITY_KINDS.contains(&entity) {
        return None;
    }
    if !is_valid_mbid(from_mbid) || !is_valid_mbid(to_mbid) {
        return None;
    }
    Some(RedirectHop {
        entity: entity.to_owned(),
        from_mbid: from_mbid.to_owned(),
        to_mbid: to_mbid.to_owned(),
    })
}

/// Split a URL into (scheme, host, port, path) for origin checks. Small and
/// strict: only http/https URLs with a host parse.
fn split_origin(url: &str) -> Option<(String, String, Option<u16>, String)> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    if authority.contains('@') || authority.is_empty() {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port_text)) if !port_text.contains(']') => {
            let port: u16 = port_text.parse().ok()?;
            (host, Some(port))
        }
        _ => (authority, None),
    };
    if host.is_empty() {
        return None;
    }
    Some((
        scheme.to_owned(),
        host.to_ascii_lowercase(),
        port,
        path.to_owned(),
    ))
}

/// Resolve a Location against the request URL (absolute form, or a path on
/// the same origin). Anything else is unresolvable and the 3xx is rejected.
fn resolve_location(request_url: &str, location: &str) -> Option<String> {
    let trimmed = location.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return Some(trimmed.to_owned());
    }
    if !trimmed.starts_with('/') {
        return None;
    }
    let (scheme, host, port, _) = split_origin(request_url)?;
    let authority = match port {
        Some(port) => format!("{host}:{port}"),
        None => host,
    };
    Some(format!("{scheme}://{authority}{trimmed}"))
}

/// Best-effort parse of an official-mode 3xx Location into a hop triple:
/// resolve against the request URL, same-origin check against the
/// attempt's source, then entity/UUID validation (v2
/// `_official_redirect_pair`). Unparseable input returns `None` and the
/// 3xx raises as usual.
pub fn official_redirect_hop(
    request_url: &str,
    source_base: &str,
    request_path: &str,
    location: &str,
) -> Option<RedirectHop> {
    let target = resolve_location(request_url, location)?;
    let (target_scheme, target_host, target_port, target_path) = split_origin(&target)?;
    let normalized_base = source_base.trim_end_matches('/');
    let (base_scheme, base_host, base_port, base_path) = split_origin(normalized_base)?;
    if target_scheme != base_scheme || target_host != base_host || target_port != base_port {
        return None;
    }
    let suffix = target_path.strip_prefix(&format!("{base_path}/"))?;
    lookup_redirect_pair(request_path, &format!("/{suffix}"))
}

/// BrainzMash host allowlist: exactly this hostname, nothing else.
pub const BRAINZMASH_HOST: &str = "api.brainzmash.cc";

/// Entity paths the application uses on BrainzMash (v2
/// `_BRAINZMASH_ENTITY_PATHS`).
const BRAINZMASH_ENTITY_PATHS: [&str; 6] = [
    "artist",
    "release-group",
    "release",
    "recording",
    "isrc",
    "url",
];

/// Validate the one server-owned BrainzMash origin and return its base URL
/// (v2 `validate_brainzmash_url`): https, exact host, no port, no
/// credentials, no query or fragment, exactly `/ws/2`.
pub fn validate_brainzmash_url(url: &str) -> Result<&str, MbError> {
    let rejected = || {
        MbError::Misconfigured(
            "brainzmash endpoint must be the approved HTTPS /ws/2 origin".to_owned(),
        )
    };
    let (scheme, host, port, path) = split_origin(url).ok_or_else(rejected)?;
    if scheme != "https" || host != BRAINZMASH_HOST || port.is_some() {
        return Err(rejected());
    }
    if path.contains(['?', '#', '%']) || path.trim_end_matches('/') != "/ws/2" {
        return Err(rejected());
    }
    Ok(BRAINZMASH_API_BASE)
}

/// Allow only the MusicBrainz WS/2 entity paths this application uses (v2
/// `validate_brainzmash_path`): one or two segments, allowlisted entity,
/// and an ASCII-alphanumeric-and-dash second segment (MBIDs qualify).
pub fn validate_brainzmash_path(path: &str) -> Result<String, MbError> {
    let rejected = || MbError::Misconfigured(format!("invalid brainzmash API path: {path}"));
    if !path.starts_with('/') || path.contains(['\\', '%', '?', '#']) || path.contains("//") {
        return Err(rejected());
    }
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    if segments.len() > 2 || !BRAINZMASH_ENTITY_PATHS.contains(&segments[0]) {
        return Err(rejected());
    }
    if segments.len() == 2 {
        let leaf = segments[1];
        if leaf.is_empty()
            || leaf == "."
            || leaf == ".."
            || !leaf
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(rejected());
        }
    }
    Ok(format!("/{}", segments.join("/")))
}

/// Reject request authority/path changes before a BrainzMash wire attempt
/// (v2 `validate_brainzmash_request_url`): the approved origin, no port or
/// credentials, no fragment, and a `/ws/2/` path. The query string (v2
/// validates `urlsplit(...).path`, so `?fmt=json` Locations pass) is
/// ignored for path validation.
pub fn validate_brainzmash_request_url(url: &str) -> Result<(), MbError> {
    let rejected =
        || MbError::Misconfigured("brainzmash request authority is not approved".to_owned());
    let (scheme, host, port, path) = split_origin(url).ok_or_else(rejected)?;
    if scheme != "https" || host != BRAINZMASH_HOST || port.is_some() {
        return Err(rejected());
    }
    if path.contains('#') {
        return Err(rejected());
    }
    let path_only = path.split('?').next().unwrap_or("");
    if !path_only.starts_with("/ws/2/") {
        return Err(rejected());
    }
    validate_brainzmash_path(&path_only["/ws/2".len()..])?;
    Ok(())
}

/// Validated `/ws/2` path for one same-origin BrainzMash redirect hop.
/// Probed live 2026-09-12 against api.brainzmash.cc: fetching merged
/// release `77a698a8-...` answers 301 with a same-origin
/// `/ws/2/release/<survivor>?fmt=json` Location, and that hop answers 200
/// with the surviving release (v2 `_validated_brainzmash_redirect_path`).
/// The Location resolves against the approved origin before validation, so
/// a foreign host, scheme downgrade, port shift, or off-`/ws/2` path still
/// returns `None` and the 3xx falls through to rejection.
pub fn brainzmash_redirect_path(
    request_url: &str,
    status: u16,
    location: Option<&str>,
) -> Option<String> {
    if !(300..400).contains(&status) {
        return None;
    }
    let resolved = resolve_location(request_url, location?)?;
    validate_brainzmash_request_url(&resolved).ok()?;
    let (_, _, _, path) = split_origin(&resolved)?;
    let path_only = path.split('?').next().unwrap_or("");
    validate_brainzmash_path(&path_only["/ws/2".len()..]).ok()
}

/// Serializes, paces, and cools down every BrainzMash attempt in this
/// process (v2 `_BrainzMashScheduler`): one in flight at a time, 10/s
/// pacing, and a cooldown after each 429 that honors Retry-After or backs
/// off exponentially with jitter (1s base, 60s ceiling). Mutexes are never
/// held across an await.
pub struct BrainzMashScheduler {
    gate: RateGate,
    state: Mutex<SchedulerState>,
}

struct SchedulerState {
    cooldown_until: Option<tokio::time::Instant>,
    consecutive_no_retry_after: u32,
}

impl BrainzMashScheduler {
    /// Scheduler with the production 10/s pacing gate.
    pub fn new() -> Self {
        Self::with_gate(RateGate::brainzmash())
    }

    /// Scheduler with a caller-supplied gate (tests pace faster).
    pub fn with_gate(gate: RateGate) -> Self {
        Self {
            gate,
            state: Mutex::new(SchedulerState {
                cooldown_until: None,
                consecutive_no_retry_after: 0,
            }),
        }
    }

    /// Record one 429 and return the bounded delay selected for it.
    pub fn note_cooldown(&self, retry_after_secs: Option<f64>) -> f64 {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let delay = match retry_after_secs {
            Some(seconds) if seconds.is_finite() && seconds >= 0.0 => {
                state.consecutive_no_retry_after = 0;
                seconds.min(BRAINZMASH_MAX_COOLDOWN_SECS)
            }
            _ => {
                state.consecutive_no_retry_after =
                    state.consecutive_no_retry_after.saturating_add(1);
                let exponent = state.consecutive_no_retry_after.saturating_sub(1).min(10);
                let base = (BRAINZMASH_COOLDOWN_BASE_SECS * f64::from(1u32 << exponent))
                    .min(BRAINZMASH_MAX_COOLDOWN_SECS);
                base * (0.5 + 0.5 * scheduler_jitter())
            }
        };
        state.cooldown_until = Some(tokio::time::Instant::now() + Duration::from_secs_f64(delay));
        delay
    }

    /// Record one success, clearing any cooldown.
    pub fn note_success(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.cooldown_until = None;
        state.consecutive_no_retry_after = 0;
    }

    /// Remaining cooldown, or zero when attempts may proceed.
    pub fn cooldown_remaining(&self) -> Duration {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match state.cooldown_until {
            Some(until) => until.saturating_duration_since(tokio::time::Instant::now()),
            None => Duration::ZERO,
        }
    }

    /// Wait out the cooldown, then take one paced slot.
    pub async fn acquire(&self) {
        let wait = self.cooldown_remaining();
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        self.gate.acquire().await;
    }
}

impl Default for BrainzMashScheduler {
    fn default() -> Self {
        Self::new()
    }
}

/// BrainzMash cooldown base (v2 `_BRAINZMASH_COOLDOWN_BASE_SECONDS`).
const BRAINZMASH_COOLDOWN_BASE_SECS: f64 = 1.0;

/// Jitter in [0, 1] from nanotime entropy (v2 uses `random.random`; no rand
/// crate is wired, and only the distribution shape matters for backoff).
fn scheduler_jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(500_000_000);
    f64::from(nanos % 1_000_000) / 1_000_000.0
}

/// Minimal percent-encoding for query pairs (letters, digits, and the
/// unreserved marks pass through; everything else becomes %XX).
fn percent_encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

// ---------------------------------------------------------------------------
// Wire models: tolerant of unknown fields (serde skips them), optional where
// the service is sparse, and strict only on identity. Every struct with an
// `id` requires it: a missing entity id fails decoding with a contract
// error, never a defaulted empty value that looks real.
// ---------------------------------------------------------------------------

/// One artist-credit entry: credited name, join phrase, and artist.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ArtistCreditName {
    /// Name as credited on this release (differs from canonical at times).
    #[serde(default)]
    pub name: String,
    /// Exact join phrase (`"; "`, `" & "`, `""`); provider evidence, never
    /// reconstructed (live 2026-07-31, management notes).
    #[serde(default)]
    pub joinphrase: String,
    /// The credited artist.
    pub artist: ArtistRef,
}

/// Minimal artist reference inside credits.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ArtistRef {
    /// Artist MBID: required identity.
    pub id: String,
    /// Canonical artist name.
    #[serde(default)]
    pub name: String,
    /// Sort name (`"Beatles, The"` style), when the service sends one.
    #[serde(rename = "sort-name", default)]
    pub sort_name: Option<String>,
}

/// Release-group reference shared by search hits and release lookups.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ReleaseGroupRef {
    /// Release-group MBID: required identity.
    pub id: String,
    /// Group title.
    #[serde(default)]
    pub title: Option<String>,
    /// First release date (`YYYY`, `YYYY-MM`, or `YYYY-MM-DD`).
    #[serde(rename = "first-release-date", default)]
    pub first_release_date: Option<String>,
    /// Primary type; explicitly null on some groups (live 2026-08-15:
    /// "Haunt Me" returned null `primary-type` and `primary-type-id`).
    #[serde(rename = "primary-type", default)]
    pub primary_type: Option<String>,
    /// Primary-type MBID; nullable for the same reason (the identifier was
    /// once modelled required and poisoned the shared breaker).
    #[serde(rename = "primary-type-id", default)]
    pub primary_type_id: Option<String>,
    /// Secondary types (`Compilation`, `Live`, `Remix`, ...).
    #[serde(rename = "secondary-types", default)]
    pub secondary_types: Vec<String>,
    /// Artist credit, when the include set carries it.
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCreditName>,
}

/// Label reference inside label-info entries.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LabelRef {
    /// Label MBID: required identity.
    pub id: String,
    /// Label name.
    #[serde(default)]
    pub name: Option<String>,
}

/// One label-info entry: catalogue number plus a nullable label object.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LabelInfo {
    /// Catalogue number; explicitly null on some entries even when the
    /// label object is present (live 2026-07-28, Anthony Green _Avalon_).
    #[serde(rename = "catalog-number", default)]
    pub catalog_number: Option<String>,
    /// The label, or null.
    #[serde(default)]
    pub label: Option<LabelRef>,
}

/// Medium summary inside releases.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Medium {
    /// Medium position within the release.
    #[serde(default)]
    pub position: Option<u32>,
    /// Medium title, when set.
    #[serde(default)]
    pub title: Option<String>,
    /// Format (`CD`, `Vinyl`, ...); sparse on search summaries.
    #[serde(default)]
    pub format: Option<String>,
    /// Track count; sparse on search summaries.
    #[serde(rename = "track-count", default)]
    pub track_count: Option<u32>,
    /// Tracks, present only with the `recordings` include.
    #[serde(default)]
    pub tracks: Vec<Track>,
}

/// Recording reference inside release tracks.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RecordingRef {
    /// Recording MBID: required identity, distinct from the track id.
    pub id: String,
    /// Recording title: fallback only; edition surfaces prefer the
    /// release-track title (live 2026-07-29: Avalon track 14 differs from
    /// its recording by one word).
    #[serde(default)]
    pub title: Option<String>,
    /// Recording length in milliseconds.
    #[serde(default)]
    pub length: Option<u64>,
    /// Recording artist credit: fallback only; a present release-track
    /// credit wins (live 2026-07-31: Bach on the track, Gould on the
    /// recording of the same Goldberg track).
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCreditName>,
}

/// One release track. The release-track `id` is not the recording MBID:
/// management retains both and never derives one from the other (live
/// 2026-07-21, management notes).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Track {
    /// Release-track MBID: required identity.
    pub id: String,
    /// Numeric position within the medium.
    #[serde(default)]
    pub position: Option<u32>,
    /// Display number (`"A1"`, `"14"`).
    #[serde(default)]
    pub number: Option<String>,
    /// Release-track title: preferred over the recording title.
    #[serde(default)]
    pub title: Option<String>,
    /// Track length in milliseconds.
    #[serde(default)]
    pub length: Option<u64>,
    /// Release-track credit, when the release carries its own.
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Option<Vec<ArtistCreditName>>,
    /// Linked recording, with the `recordings` include.
    #[serde(default)]
    pub recording: Option<RecordingRef>,
}

impl Track {
    /// Edition title: release-track title first, recording title fallback.
    pub fn display_title(&self) -> Option<&str> {
        self.title
            .as_deref()
            .or_else(|| self.recording.as_ref()?.title.as_deref())
    }

    /// Edition credit: release-track credit first, recording fallback.
    pub fn credit(&self) -> &[ArtistCreditName] {
        if let Some(credit) = self.artist_credit.as_ref() {
            return credit;
        }
        self.recording
            .as_ref()
            .map_or(&[], |recording| &recording.artist_credit)
    }

    /// Length in milliseconds, track first then recording.
    pub fn length_ms(&self) -> Option<u64> {
        self.length.or_else(|| self.recording.as_ref()?.length)
    }
}

/// Linked work reference inside relationships.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct WorkRef {
    /// Work MBID: required identity.
    pub id: String,
    /// Work title.
    #[serde(default)]
    pub title: Option<String>,
    /// Work type display string; explicitly null on some linked works.
    #[serde(rename = "type", default)]
    pub work_type: Option<String>,
    /// Work type MBID; explicitly null alongside it (live 2026-08-03) and
    /// must not fail decoding of an otherwise valid release.
    #[serde(rename = "type-id", default)]
    pub type_id: Option<String>,
}

/// Generic entity pointer for relationship targets.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct EntityRef {
    /// Target MBID: required identity.
    pub id: String,
}

/// One relationship. Relation `type-id` fields stay required: a
/// relationship-rich probe on 2026-08-15 returned null only for relation
/// `begin`/`end`, so identifiers stay strict until a live payload proves
/// otherwise (management notes).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Relation {
    /// Relationship type name.
    #[serde(rename = "type")]
    pub rel_type: String,
    /// Relationship type MBID: required until proven otherwise.
    #[serde(rename = "type-id")]
    pub type_id: String,
    /// Linked work, for work relationships.
    #[serde(default)]
    pub work: Option<WorkRef>,
    /// Linked artist, for artist relationships.
    #[serde(default)]
    pub artist: Option<EntityRef>,
    /// Linked release, for release relationships.
    #[serde(default)]
    pub release: Option<EntityRef>,
    /// Linked release group, for release-group relationships.
    #[serde(rename = "release-group", default)]
    pub release_group: Option<EntityRef>,
}

/// Full release lookup document (identity-readiness surface).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MbRelease {
    /// Release MBID: required identity.
    pub id: String,
    /// Release title.
    #[serde(default)]
    pub title: Option<String>,
    /// Status; explicitly null on some releases (live 2026-08-15: "I
    /// Fought the Law" returned null `status` and `status-id`).
    #[serde(default)]
    pub status: Option<String>,
    /// Status MBID; nullable for the same reason.
    #[serde(rename = "status-id", default)]
    pub status_id: Option<String>,
    /// Release date.
    #[serde(default)]
    pub date: Option<String>,
    /// Release country.
    #[serde(default)]
    pub country: Option<String>,
    /// Barcode.
    #[serde(default)]
    pub barcode: Option<String>,
    /// Amazon identifier, when present.
    #[serde(default)]
    pub asin: Option<String>,
    /// Packaging display string; explicitly null on several releases.
    #[serde(default)]
    pub packaging: Option<String>,
    /// Packaging MBID; explicitly null alongside it (live 2026-07-28).
    #[serde(rename = "packaging-id", default)]
    pub packaging_id: Option<String>,
    /// Release artist credit.
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCreditName>,
    /// Label info entries.
    #[serde(rename = "label-info", default)]
    pub label_info: Vec<LabelInfo>,
    /// Media, with track detail under the `recordings` include.
    #[serde(default)]
    pub media: Vec<Medium>,
    /// Release group: present only with the `release-groups` include (live
    /// 2026-08-10: Clairo _Immunity_ omits it without that include).
    #[serde(rename = "release-group", default)]
    pub release_group: Option<ReleaseGroupRef>,
    /// Relationships, under the relation includes.
    #[serde(default)]
    pub relations: Vec<Relation>,
}

/// Release-group lookup document.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MbReleaseGroup {
    /// Release-group MBID: required identity.
    pub id: String,
    /// Group title.
    #[serde(default)]
    pub title: Option<String>,
    /// First release date.
    #[serde(rename = "first-release-date", default)]
    pub first_release_date: Option<String>,
    /// Primary type; nullable (live 2026-08-15).
    #[serde(rename = "primary-type", default)]
    pub primary_type: Option<String>,
    /// Primary-type MBID; nullable (live 2026-08-15).
    #[serde(rename = "primary-type-id", default)]
    pub primary_type_id: Option<String>,
    /// Secondary types.
    #[serde(rename = "secondary-types", default)]
    pub secondary_types: Vec<String>,
    /// Artist credit.
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCreditName>,
    /// Sibling releases, under the `releases` include.
    #[serde(default)]
    pub releases: Vec<MbRelease>,
}

/// Area reference on artists.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AreaRef {
    /// Area MBID: required identity.
    pub id: String,
    /// Area name.
    #[serde(default)]
    pub name: Option<String>,
}

/// Artist life span.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LifeSpan {
    /// Begin date, partial shapes allowed.
    #[serde(default)]
    pub begin: Option<String>,
    /// End date, partial shapes allowed.
    #[serde(default)]
    pub end: Option<String>,
}

/// Artist lookup document.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MbArtist {
    /// Artist MBID: required identity.
    pub id: String,
    /// Canonical name.
    #[serde(default)]
    pub name: Option<String>,
    /// Sort name.
    #[serde(rename = "sort-name", default)]
    pub sort_name: Option<String>,
    /// Disambiguation comment.
    #[serde(default)]
    pub disambiguation: Option<String>,
    /// Artist type (`Person`, `Group`, ...).
    #[serde(rename = "type", default)]
    pub artist_type: Option<String>,
    /// Gender, for persons.
    #[serde(default)]
    pub gender: Option<String>,
    /// Home area.
    #[serde(default)]
    pub area: Option<AreaRef>,
    /// Life span.
    #[serde(rename = "life-span", default)]
    pub life_span: Option<LifeSpan>,
}

/// Release attached to a recording lookup.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RecordingRelease {
    /// Release MBID: required identity.
    pub id: String,
    /// Release status (`Official`, `Bootleg`, ...).
    #[serde(default)]
    pub status: Option<String>,
    /// Release date.
    #[serde(default)]
    pub date: Option<String>,
    /// Release group carrying the ranking fields (live 2026-07-20,
    /// `musicbrainz_API_NOTES.md`).
    #[serde(rename = "release-group", default)]
    pub release_group: Option<ReleaseGroupRef>,
}

/// Recording lookup document.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MbRecording {
    /// Recording MBID: required identity (canonical after redirects).
    pub id: String,
    /// Recording title.
    #[serde(default)]
    pub title: Option<String>,
    /// Length in milliseconds.
    #[serde(default)]
    pub length: Option<u64>,
    /// ISRCs.
    #[serde(default)]
    pub isrcs: Vec<String>,
    /// Recording artist credit.
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCreditName>,
    /// Releases carrying this recording, under `inc=releases`.
    #[serde(default)]
    pub releases: Vec<RecordingRelease>,
    /// Relationships, under the relation includes.
    #[serde(default)]
    pub relations: Vec<Relation>,
}

/// Release search hit. The search index carries release-group,
/// artist-credit, label-info, and medium facets without any `inc`
/// parameter; optional fields stay absent on many releases (live 2026-08-11,
/// `musicbrainz_release_search_models.py`).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ReleaseSearchHit {
    /// Release MBID: required identity.
    pub id: String,
    /// Plain score key.
    #[serde(default)]
    pub score: Option<i64>,
    /// Namespaced score key.
    #[serde(rename = "ext:score", default)]
    pub ext_score: Option<i64>,
    /// Release title.
    #[serde(default)]
    pub title: Option<String>,
    /// Artist credit facet.
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCreditName>,
    /// Release-group facet.
    #[serde(rename = "release-group", default)]
    pub release_group: Option<ReleaseGroupRef>,
    /// Release date.
    #[serde(default)]
    pub date: Option<String>,
    /// Release country.
    #[serde(default)]
    pub country: Option<String>,
    /// Release status.
    #[serde(default)]
    pub status: Option<String>,
    /// Packaging.
    #[serde(default)]
    pub packaging: Option<String>,
    /// Medium facet.
    #[serde(default)]
    pub media: Vec<Medium>,
    /// Label-info facet.
    #[serde(rename = "label-info", default)]
    pub label_info: Vec<LabelInfo>,
    /// Barcode.
    #[serde(default)]
    pub barcode: Option<String>,
    /// Disambiguation comment.
    #[serde(default)]
    pub disambiguation: Option<String>,
}

/// Release-group search hit.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ReleaseGroupSearchHit {
    /// Release-group MBID: required identity.
    pub id: String,
    /// Plain score key.
    #[serde(default)]
    pub score: Option<i64>,
    /// Namespaced score key.
    #[serde(rename = "ext:score", default)]
    pub ext_score: Option<i64>,
    /// Group title.
    #[serde(default)]
    pub title: Option<String>,
    /// Primary type.
    #[serde(rename = "primary-type", default)]
    pub primary_type: Option<String>,
    /// Secondary types.
    #[serde(rename = "secondary-types", default)]
    pub secondary_types: Vec<String>,
    /// First release date.
    #[serde(rename = "first-release-date", default)]
    pub first_release_date: Option<String>,
    /// Artist credit facet.
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCreditName>,
    /// Disambiguation comment.
    #[serde(default)]
    pub disambiguation: Option<String>,
}

/// Artist search hit.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ArtistSearchHit {
    /// Artist MBID: required identity.
    pub id: String,
    /// Plain score key.
    #[serde(default)]
    pub score: Option<i64>,
    /// Namespaced score key.
    #[serde(rename = "ext:score", default)]
    pub ext_score: Option<i64>,
    /// Canonical name.
    #[serde(default)]
    pub name: Option<String>,
    /// Sort name.
    #[serde(rename = "sort-name", default)]
    pub sort_name: Option<String>,
    /// Disambiguation comment.
    #[serde(default)]
    pub disambiguation: Option<String>,
    /// Artist type.
    #[serde(rename = "type", default)]
    pub artist_type: Option<String>,
    /// Home area.
    #[serde(default)]
    pub area: Option<AreaRef>,
}

/// Recording search hit: candidate plus the releases it appears on.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RecordingSearchHit {
    /// Recording MBID: required identity.
    pub id: String,
    /// Plain score key.
    #[serde(default)]
    pub score: Option<i64>,
    /// Namespaced score key.
    #[serde(rename = "ext:score", default)]
    pub ext_score: Option<i64>,
    /// Recording title.
    #[serde(default)]
    pub title: Option<String>,
    /// Recording length in milliseconds.
    #[serde(default)]
    pub length: Option<u64>,
    /// Releases carrying this recording.
    #[serde(default)]
    pub releases: Vec<RecordingRelease>,
}

/// Search page: items plus the envelope counters. The wire shape is
/// `{count, created, offset, <entities>}` (contribution probe 2026-07-21);
/// `created` is ignored and each search reads its own entity key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPage<T> {
    /// Total matches in the index.
    pub count: u64,
    /// Offset of this page.
    pub offset: u64,
    /// Decoded page items.
    pub items: Vec<T>,
}

/// Decode one search page from the entity array inside the envelope.
fn decode_search_page<T>(body: &[u8], array_key: &str) -> Result<SearchPage<T>, MbError>
where
    T: for<'de> Deserialize<'de>,
{
    let envelope: HashMap<String, serde_json::Value> = serde_json::from_slice(body)
        .map_err(|error| MbError::Contract(format!("unparseable search payload: {error}")))?;
    let missing_id = |error: serde_json::Error| {
        MbError::Contract(format!("search hit breaks the identity contract: {error}"))
    };
    let items = match envelope.get(array_key) {
        None => Vec::new(),
        Some(value) => serde_json::from_value(value.clone()).map_err(missing_id)?,
    };
    let count = envelope
        .get("count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let offset = envelope
        .get("offset")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Ok(SearchPage {
        count,
        offset,
        items,
    })
}

/// URL-resolution response: the full relation list, retained so a
/// multi-target response reads as ambiguity rather than silently selecting
/// its first item (live 2026-07-21, contribution notes).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct UrlResolution {
    /// Resource URL echoed back.
    #[serde(default)]
    pub resource: Option<String>,
    /// Every relation found; 404 resolves to an empty list, not an error.
    #[serde(default)]
    pub relations: Vec<Relation>,
}

impl UrlResolution {
    /// Empty resolution for a 404: the URL simply has no relations.
    pub fn empty(resource: &str) -> Self {
        Self {
            resource: Some(resource.to_owned()),
            relations: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Ranking: same-recording release-group choice. Ranking never substitutes
// one recording MBID for another; it only orders the release groups already
// attached to that recording (live 2026-07-20, `musicbrainz_API_NOTES.md`).
// ---------------------------------------------------------------------------

/// Secondary types that change the version enough to deprioritize (v2
/// `_VERSION_CHANGING_TYPES`).
const VERSION_CHANGING_TYPES: [&str; 5] = ["live", "remix", "dj-mix", "mixtape/street", "demo"];

/// Same-recording release-group rank, lower winning: Official flag,
/// version-change flag, primary-type order, secondary-type penalty, date,
/// then MBID for determinism.
pub type ReleaseGroupRank = (u8, u8, u8, u8, String, String);

/// Stable same-recording rank where lower values win (v2
/// `recording_release_group_rank`): Official first, then version-keeping,
/// then Album > EP > Single > other, then type-less, then earliest date,
/// then MBID for determinism.
pub fn recording_release_group_rank(
    release_status: Option<&str>,
    secondary_types: &[String],
    primary_type: Option<&str>,
    release_date: Option<&str>,
    release_group_mbid: &str,
) -> ReleaseGroupRank {
    let official =
        u8::from(!release_status.is_some_and(|status| status.eq_ignore_ascii_case("official")));
    let lowered: Vec<String> = secondary_types
        .iter()
        .map(|entry| entry.to_ascii_lowercase())
        .collect();
    let version_changed = u8::from(
        lowered
            .iter()
            .any(|entry| VERSION_CHANGING_TYPES.contains(&entry.as_str())),
    );
    let primary = match primary_type.unwrap_or("").to_ascii_lowercase().as_str() {
        "album" => 0,
        "ep" => 1,
        "single" => 2,
        _ => 3,
    };
    let secondary_penalty = u8::from(!secondary_types.is_empty());
    (
        official,
        version_changed,
        primary,
        secondary_penalty,
        release_date.unwrap_or("9999-99-99").to_owned(),
        release_group_mbid.to_owned(),
    )
}

/// Best release group attached to one recording, plus its rank. Returns
/// `None` when the recording carries no usable group. Never looks beyond
/// this recording's own releases: cross-recording substitution is the
/// caller's bug, and this signature makes it unwritable.
pub fn best_release_group_for_recording(
    releases: &[RecordingRelease],
) -> Option<(&ReleaseGroupRef, ReleaseGroupRank)> {
    releases
        .iter()
        .filter_map(|release| {
            let group = release.release_group.as_ref()?;
            if group.id.is_empty() {
                return None;
            }
            let rank = recording_release_group_rank(
                release.status.as_deref(),
                &group.secondary_types,
                group.primary_type.as_deref(),
                release.date.as_deref(),
                &group.id,
            );
            Some((group, rank))
        })
        .min_by(|left, right| left.1.cmp(&right.1))
}

// ---------------------------------------------------------------------------
// Client: one paced, identified request per call, with redirect proof and
// criticality routing. Every request carries `fmt=json`, the smallest
// sorted include set the caller asked for (never a broad default: the full
// verification include set decoded ~608 KB for one 34-track release, live
// 2026-07-21), and the descriptive DroppedNeedle User-Agent.
// ---------------------------------------------------------------------------

/// Lookup result: the entity plus the redirect proof behind it.
#[derive(Debug, Clone)]
pub struct Lookup<T> {
    /// Decoded entity (canonical id after any redirects).
    pub entity: T,
    /// Followed hops, oldest first, for the durable canonical map.
    pub redirects: Vec<RedirectHop>,
}

/// MusicBrainz client over any [`MbTransport`]. Owns pacing (1/s official,
/// BrainzMash scheduler), identification headers, status semantics, and
/// criticality routing; owns no cache and no persistence.
pub struct MusicBrainzClient<T: MbTransport, S: DegradationSink = NoopSink> {
    transport: T,
    sink: S,
    source: MbSource,
    official_gate: RateGate,
    brainzmash: BrainzMashScheduler,
}

impl<T: MbTransport> MusicBrainzClient<T, NoopSink> {
    /// Client against the official service with production pacing.
    pub fn official(transport: T) -> Self {
        Self {
            transport,
            sink: NoopSink,
            source: MbSource::official(),
            official_gate: RateGate::musicbrainz(),
            brainzmash: BrainzMashScheduler::new(),
        }
    }

    /// Client against BrainzMash. `binding_valid` must be true before any
    /// request is sent; otherwise calls fail closed without touching the
    /// wire (v2 raises "BrainzMash active binding is not valid").
    pub fn brainzmash(transport: T, binding_valid: bool) -> Self {
        Self {
            transport,
            sink: NoopSink,
            source: MbSource::BrainzMash { binding_valid },
            official_gate: RateGate::musicbrainz(),
            brainzmash: BrainzMashScheduler::new(),
        }
    }
}

impl<T: MbTransport, S: DegradationSink> MusicBrainzClient<T, S> {
    /// Swap the degradation sink (the enrichment aggregator mounts its own).
    pub fn with_sink<N: DegradationSink>(self, sink: N) -> MusicBrainzClient<T, N> {
        MusicBrainzClient {
            transport: self.transport,
            sink,
            source: self.source,
            official_gate: self.official_gate,
            brainzmash: self.brainzmash,
        }
    }

    /// Swap the pacing gates (tests pace faster than production).
    pub fn with_gates(mut self, official_gate: RateGate, brainzmash: BrainzMashScheduler) -> Self {
        self.official_gate = official_gate;
        self.brainzmash = brainzmash;
        self
    }

    /// Active source, for wiring assertions.
    pub fn source(&self) -> &MbSource {
        &self.source
    }

    /// Search releases by title and artist (verified Lucene shape).
    pub async fn search_releases(
        &self,
        title: &str,
        artist: &str,
        limit: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<ReleaseSearchHit>, MbError> {
        let query = build_release_search_query(title, artist);
        self.search(
            "/release",
            &query,
            limit,
            "releases",
            "search_releases",
            criticality,
        )
        .await
    }

    /// Search release groups by title and artist (verified Lucene shape).
    pub async fn search_release_groups(
        &self,
        title: &str,
        artist: &str,
        limit: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<ReleaseGroupSearchHit>, MbError> {
        let query = build_release_group_search_query(title, artist);
        self.search(
            "/release-group",
            &query,
            limit,
            "release-groups",
            "search_release_groups",
            criticality,
        )
        .await
    }

    /// Search artists by name.
    pub async fn search_artists(
        &self,
        name: &str,
        limit: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<ArtistSearchHit>, MbError> {
        let query = format!(r#"artist:"{}""#, escape_lucene_phrase(name));
        self.search(
            "/artist",
            &query,
            limit,
            "artists",
            "search_artists",
            criticality,
        )
        .await
    }

    /// Search recordings by track title and artist (verified Lucene shape).
    pub async fn search_recordings(
        &self,
        title: &str,
        artist: &str,
        limit: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<RecordingSearchHit>, MbError> {
        let query = build_recording_search_query(title, artist);
        self.search(
            "/recording",
            &query,
            limit,
            "recordings",
            "search_recordings",
            criticality,
        )
        .await
    }

    /// Look up one release with the caller's include set.
    pub async fn lookup_release(
        &self,
        mbid: &str,
        includes: &[&str],
        criticality: Criticality,
    ) -> Result<Option<Lookup<MbRelease>>, MbError> {
        let path = format!("/release/{}", normalize_mb_id(mbid));
        self.lookup(&path, includes, "lookup_release", criticality)
            .await
    }

    /// Look up one release with proof it belongs to a provider-returned
    /// group. Exact-release identification must include `release-groups`
    /// (Clairo _Immunity_ omits the member without it, live 2026-08-10);
    /// absence after that request is a fail-closed contract break, never
    /// an assumed group.
    pub async fn lookup_exact_release(
        &self,
        mbid: &str,
        criticality: Criticality,
    ) -> Result<Option<Lookup<MbRelease>>, MbError> {
        let found = self
            .lookup_release(
                mbid,
                &["artist-credits", "recordings", "release-groups"],
                criticality,
            )
            .await?;
        match found {
            Some(lookup) if lookup.entity.release_group.is_none() => Err(MbError::Contract(
                format!("exact release {mbid} arrived without its provider release group"),
            )),
            other => Ok(other),
        }
    }

    /// Resolve a release MBID to its release-group MBID (v2
    /// `MusicBrainzIdResolver`: tags carry the release id, the library
    /// keys on the group id).
    pub async fn resolve_release_to_release_group(
        &self,
        release_mbid: &str,
        criticality: Criticality,
    ) -> Result<Option<String>, MbError> {
        let found = self
            .lookup_release(release_mbid, &["release-groups"], criticality)
            .await?;
        Ok(found.and_then(|lookup| lookup.entity.release_group.map(|group| group.id)))
    }

    /// Look up one release group with the caller's include set.
    pub async fn lookup_release_group(
        &self,
        mbid: &str,
        includes: &[&str],
        criticality: Criticality,
    ) -> Result<Option<Lookup<MbReleaseGroup>>, MbError> {
        let path = format!("/release-group/{}", normalize_mb_id(mbid));
        self.lookup(&path, includes, "lookup_release_group", criticality)
            .await
    }

    /// Look up one artist. The detail surface uses
    /// `tags+aliases+url-rels` (v2 artist mixin).
    pub async fn lookup_artist(
        &self,
        mbid: &str,
        includes: &[&str],
        criticality: Criticality,
    ) -> Result<Option<Lookup<MbArtist>>, MbError> {
        let path = format!("/artist/{}", normalize_mb_id(mbid));
        self.lookup(&path, includes, "lookup_artist", criticality)
            .await
    }

    /// Look up one recording. Identity callers pass
    /// `inc=releases+release-groups`: each `releases` item carries
    /// `id`/`status`/`date` and each `release-group` carries
    /// `id`/`title`/`primary-type`/`secondary-types`/`first-release-date`
    /// (live 2026-07-20).
    pub async fn lookup_recording(
        &self,
        mbid: &str,
        includes: &[&str],
        criticality: Criticality,
    ) -> Result<Option<Lookup<MbRecording>>, MbError> {
        let path = format!("/recording/{}", normalize_mb_id(mbid));
        self.lookup(&path, includes, "lookup_recording", criticality)
            .await
    }

    /// Resolve a recording MBID through merge redirects to its canonical
    /// id. A retired MBID counts as equivalent only after this lookup
    /// proves the redirect target; any other outcome keeps the normal
    /// conflict gate (live 2026-08-10, merged recording identifiers).
    pub async fn resolve_recording_mbid(
        &self,
        recording_mbid: &str,
        criticality: Criticality,
    ) -> Result<Option<String>, MbError> {
        let normalized = normalize_mb_id(recording_mbid);
        if !is_valid_mbid(&normalized) {
            return Err(MbError::InvalidMbid(recording_mbid.to_owned()));
        }
        let found = self.lookup_recording(&normalized, &[], criticality).await?;
        Ok(found.map(|lookup| lookup.entity.id))
    }

    /// Resolve a resource URL to its relations. Callers pass the fixed
    /// numeric URL form, never a pasted or provider slug (a slugged Discogs
    /// URL 404s, live 2026-07-21). A missing URL resolves to an empty
    /// relation list, and multi-target responses stay ambiguous.
    pub async fn resolve_url(
        &self,
        resource: &str,
        includes: &[&str],
        criticality: Criticality,
    ) -> Result<UrlResolution, MbError> {
        let operation = "resolve_url";
        let mut params = vec![("resource".to_owned(), resource.to_owned())];
        let inc = sorted_includes(includes);
        if !inc.is_empty() {
            params.push(("inc".to_owned(), inc));
        }
        match self.get("/url", params, operation, criticality).await? {
            WireOutcome::Found(body) => serde_json::from_slice(&body).map_err(|error| {
                MbError::Contract(format!("url resolution broke the contract: {error}"))
            }),
            WireOutcome::Missing | WireOutcome::Degraded => Ok(UrlResolution::empty(resource)),
            WireOutcome::Redirect { .. } => Err(MbError::RedirectRejected(
                "url resolution never follows redirects".to_owned(),
            )),
        }
    }

    /// One entity search with bucket isolation: provider death on a
    /// non-critical search records a degradation and yields an empty page
    /// (v2 grouped search returns `[]` plus a failed bucket).
    async fn search<E>(
        &self,
        path: &str,
        query: &str,
        limit: u32,
        array_key: &str,
        operation: &'static str,
        criticality: Criticality,
    ) -> Result<SearchPage<E>, MbError>
    where
        E: for<'de> Deserialize<'de>,
    {
        let params = vec![
            ("query".to_owned(), query.to_owned()),
            ("limit".to_owned(), limit.to_string()),
        ];
        match self.get(path, params, operation, criticality).await? {
            WireOutcome::Found(body) => decode_search_page(&body, array_key),
            WireOutcome::Missing | WireOutcome::Degraded => Ok(SearchPage {
                count: 0,
                offset: 0,
                items: Vec::new(),
            }),
            WireOutcome::Redirect { .. } => Err(MbError::RedirectRejected(format!(
                "{operation} never follows redirects"
            ))),
        }
    }

    /// One entity lookup with redirect proof and criticality routing.
    async fn lookup<E>(
        &self,
        path: &str,
        includes: &[&str],
        operation: &'static str,
        criticality: Criticality,
    ) -> Result<Option<Lookup<E>>, MbError>
    where
        E: for<'de> Deserialize<'de>,
    {
        let mut params = Vec::new();
        let inc = sorted_includes(includes);
        if !inc.is_empty() {
            params.push(("inc".to_owned(), inc));
        }
        let mut current_path = path.to_owned();
        let mut redirects = Vec::new();
        for _ in 0..=MAX_REDIRECT_HOPS {
            match self
                .get(&current_path, params.clone(), operation, criticality)
                .await?
            {
                WireOutcome::Found(body) => {
                    let entity = serde_json::from_slice(&body).map_err(|error| {
                        MbError::Contract(format!(
                            "{operation} payload breaks the contract: {error}"
                        ))
                    })?;
                    return Ok(Some(Lookup { entity, redirects }));
                }
                WireOutcome::Missing | WireOutcome::Degraded => return Ok(None),
                WireOutcome::Redirect { hop, next_path } => {
                    redirects.push(hop);
                    current_path = next_path;
                }
            }
        }
        Err(MbError::RedirectRejected(format!(
            "{operation} exceeded {MAX_REDIRECT_HOPS} redirect hops"
        )))
    }

    /// One paced wire attempt with status semantics. Returns the redirect
    /// hop for the caller to follow (lookups) or reject (searches).
    async fn get(
        &self,
        path: &str,
        mut params: Vec<(String, String)>,
        operation: &'static str,
        criticality: Criticality,
    ) -> Result<WireOutcome, MbError> {
        if let MbSource::BrainzMash { binding_valid } = &self.source
            && !binding_valid
        {
            return Err(MbError::Misconfigured(
                "brainzmash active binding is not valid".to_owned(),
            ));
        }
        let brainzmash = self.source.is_brainzmash();
        let request_path = if brainzmash {
            validate_brainzmash_path(path)?
        } else {
            path.to_owned()
        };
        if brainzmash {
            self.brainzmash.acquire().await;
        } else {
            self.official_gate.acquire().await;
        }
        params.push(("fmt".to_owned(), "json".to_owned()));
        let url = format!(
            "{}{}",
            self.source.base_url().trim_end_matches('/'),
            request_path
        );
        let request = MbRequest {
            url: url.clone(),
            query: params,
            headers: Vec::new(),
        };
        let response = match self.transport.get(&request).await {
            Ok(response) => response,
            Err(error) => return self.provider_dead(operation, criticality, error.0),
        };
        if brainzmash {
            self.note_brainzmash_status(&response);
        }
        match response.status {
            200 => Ok(WireOutcome::Found(response.body)),
            404 => Ok(WireOutcome::Missing),
            429 | 503 => {
                let retry_after_secs = parse_retry_after_secs(response.header("Retry-After"));
                if brainzmash && response.status == 429 {
                    // Single note_cooldown call for this response: it is not
                    // idempotent, so the helper above skips 429 on purpose.
                    let selected = self.brainzmash.note_cooldown(retry_after_secs);
                    return self.provider_dead(
                        operation,
                        criticality,
                        format!("brainzmash rate limited; cooling down {selected:.1}s"),
                    );
                }
                if brainzmash && response.status == 503 {
                    // A BrainzMash 503 is a dead mirror, not a rate signal:
                    // v2 labels only the official 503 rate-limited.
                    return self.provider_dead(
                        operation,
                        criticality,
                        "brainzmash unavailable (HTTP 503)".to_owned(),
                    );
                }
                if criticality == Criticality::BestEffort {
                    self.sink.record(
                        "musicbrainz",
                        format!(
                            "{operation}: musicbrainz rate limited (HTTP {})",
                            response.status
                        ),
                    );
                    return Ok(WireOutcome::Degraded);
                }
                Err(MbError::RateLimited { retry_after_secs })
            }
            400 => Err(MbError::InvalidMbid(format!("{operation} {path}"))),
            300..=399 => {
                let hop = self.redirect_hop(&url, &request_path, &response, operation)?;
                Ok(WireOutcome::Redirect {
                    hop: hop.hop,
                    next_path: hop.next_path,
                })
            }
            401..=499 => Err(MbError::Rejected(response.status)),
            _ => self.provider_dead(
                operation,
                criticality,
                format!("HTTP {} from {}", response.status, self.source.base_url()),
            ),
        }
    }

    /// Validate one 3xx into a followable hop, or reject it.
    fn redirect_hop(
        &self,
        request_url: &str,
        request_path: &str,
        response: &RawResponse,
        operation: &'static str,
    ) -> Result<FollowHop, MbError> {
        let location = response.header("location").unwrap_or("");
        if self.source.is_brainzmash() {
            if let Some(hop_path) =
                brainzmash_redirect_path(request_url, response.status, response.header("location"))
                && let Some(hop) = lookup_redirect_pair(request_path, &hop_path)
            {
                return Ok(FollowHop {
                    hop,
                    next_path: hop_path,
                });
            }
        } else if let Some(hop) =
            official_redirect_hop(request_url, self.source.base_url(), request_path, location)
        {
            let next_path = format!("/{}/{}", hop.entity, hop.to_mbid);
            return Ok(FollowHop { hop, next_path });
        }
        Err(MbError::RedirectRejected(format!(
            "{operation} refused {request_path} -> {location}"
        )))
    }

    /// BrainzMash per-response bookkeeping: 200 clears the cooldown.
    /// The 429 arm below owns the single `note_cooldown` call for its
    /// response (it is not idempotent), so 429 is skipped here on purpose.
    fn note_brainzmash_status(&self, response: &RawResponse) {
        if response.status == 200 {
            self.brainzmash.note_success();
        }
    }

    /// Dead-provider routing: typed failure when identity-critical,
    /// recorded absence otherwise.
    fn provider_dead(
        &self,
        operation: &'static str,
        criticality: Criticality,
        cause: String,
    ) -> Result<WireOutcome, MbError> {
        if criticality == Criticality::IdentityCritical {
            return Err(MbError::Unavailable(cause));
        }
        self.sink.record(
            "musicbrainz",
            format!("{operation}: musicbrainz unavailable: {cause}"),
        );
        Ok(WireOutcome::Degraded)
    }
}

/// One wire attempt's classified result.
enum WireOutcome {
    /// 200 with a body to decode.
    Found(Vec<u8>),
    /// 404: definitive absence (a 404 today may still resolve after later
    /// edits, so misses are never cached as permanent elsewhere).
    Missing,
    /// Provider dead on a non-critical call: recorded, resolving to None.
    Degraded,
    /// Followable same-origin lookup redirect.
    Redirect {
        /// Validated hop for the durable map.
        hop: RedirectHop,
        /// Next lookup path.
        next_path: String,
    },
}

/// Validated redirect hop plus the path to request next.
struct FollowHop {
    hop: RedirectHop,
    next_path: String,
}

/// Smallest sorted include set for the request (v2 sorts and dedupes the
/// caller includes before sending).
fn sorted_includes(includes: &[&str]) -> String {
    let mut selected: Vec<&str> = includes.to_vec();
    selected.sort_unstable();
    selected.dedup();
    selected.join("+")
}
