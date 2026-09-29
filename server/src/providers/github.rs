//! GitHub releases client (the update check).
//!
//! Port of `backend/repositories/github_repository.py` and the
//! `GitHubRelease` schema from `backend/api/v1/schemas/version.py`. This is
//! the slice's quiet client: every failure mode (non-200 answers,
//! undecodable bodies, transport errors) degrades to an empty release list
//! with a log line, never an error. An update check must not break the app
//! it checks for updates.
//!
//! Kept behaviors:
//!
//! - The `Accept: application/vnd.github+json` header on every call.
//! - Draft releases are filtered out; prereleases stay in the list but
//!   [GitHubClient::fetch_latest_release] skips them (the API returns
//!   newest first, so the first stable release wins).
//! - Missing `name` falls back to the tag, missing `body` to `""`.
//! - One hourly memo: repeated checks within the hour reuse the last good
//!   answer instead of calling GitHub again.
//!
//! Seam for s5-core: v2's hourly cache is an injected shared cache; here it
//! is a small in-client TTL memo so the port stays self-contained. Wiring
//! may later replace it with the shared cache without changing this surface.

use std::time::{Duration, Instant};

use serde::Deserialize;
use thiserror::Error;

/// Production endpoint. Tests point elsewhere via [GitHubClient::with_base_url].
pub const GITHUB_RELEASES_URL: &str =
    "https://api.github.com/repos/DroppedNeedle/DroppedNeedle/releases";
/// How long a fetched release list stays fresh, kept from v2's 1h TTL.
pub const RELEASES_CACHE_TTL: Duration = Duration::from_secs(3600);
/// Per-request timeout, kept from v2's 10s client call.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// One published release. `tag_name`, `published_at`, and `html_url` are the
/// required identity fields: an entry without one fails the whole decode,
/// which degrades to an empty list like any other fetch failure.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GitHubRelease {
    /// Release tag such as "v2.4.1".
    pub tag_name: String,
    /// Display name, falling back to the tag when the API omits it.
    #[serde(default)]
    pub name: String,
    /// Release notes, `""` when the API omits them.
    #[serde(default)]
    pub body: String,
    /// Publish timestamp.
    pub published_at: String,
    /// Release page URL.
    pub html_url: String,
    /// True for prereleases (kept in the list, skipped by latest).
    #[serde(default)]
    pub prerelease: bool,
}

/// Raw API entry: drafts are filtered before mapping, and the name/body
/// fallbacks apply here.
#[derive(Debug, Clone, Deserialize)]
struct RawRelease {
    tag_name: String,
    published_at: String,
    html_url: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

/// Transport failure inside the fetch. Never escapes the client: it only
/// exists so the fetch can log the kind before degrading to `[]`.
#[derive(Debug, Error)]
enum FetchError {
    /// The request never completed (DNS, TLS, connect, timeout).
    #[error("github releases request failed: {0}")]
    Transport(String),
}

/// The hourly memo: when the list was fetched plus the list itself.
type ReleaseMemo = std::sync::Arc<tokio::sync::Mutex<Option<(Instant, Vec<GitHubRelease>)>>>;

/// The releases client. Clone the shared `reqwest::Client` from the HTTP
/// factory at wiring time; clones share the hourly memo.
#[derive(Debug, Clone)]
pub struct GitHubClient {
    http: reqwest::Client,
    base_url: String,
    memo: ReleaseMemo,
}

impl GitHubClient {
    /// Client against the production endpoint.
    pub fn new(http: reqwest::Client) -> Self {
        Self::with_base_url(http, GITHUB_RELEASES_URL)
    }

    /// Client against an override endpoint (scripted fakes in tests).
    pub fn with_base_url(http: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            http,
            base_url: base_url.into(),
            memo: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// All non-draft releases, newest first, with the hourly memo. Every
    /// failure degrades to `[]` with a log line.
    pub async fn fetch_releases(&self) -> Vec<GitHubRelease> {
        if let Some(cached) = self.memo_get().await {
            return cached;
        }
        let releases = match self.fetch().await {
            Ok(releases) => releases,
            Err(error) => {
                tracing::error!(%error, "failed to fetch github releases");
                return Vec::new();
            }
        };
        self.memo_put(releases.clone()).await;
        releases
    }

    /// The latest non-prerelease release, if any.
    pub async fn fetch_latest_release(&self) -> Option<GitHubRelease> {
        self.fetch_releases()
            .await
            .into_iter()
            .find(|release| !release.prerelease)
    }

    /// Fresh memoized list when still inside the TTL.
    async fn memo_get(&self) -> Option<Vec<GitHubRelease>> {
        let memo = self.memo.lock().await;
        match memo.as_ref() {
            Some((fetched_at, releases)) if fetched_at.elapsed() < RELEASES_CACHE_TTL => {
                Some(releases.clone())
            }
            _ => None,
        }
    }

    /// Remember a freshly fetched list.
    async fn memo_put(&self, releases: Vec<GitHubRelease>) {
        let mut memo = self.memo.lock().await;
        *memo = Some((Instant::now(), releases));
    }

    /// One uncached fetch. Non-200 answers warn and degrade; transport and
    /// decode failures error and degrade.
    async fn fetch(&self) -> Result<Vec<GitHubRelease>, FetchError> {
        let response = self
            .http
            .get(self.base_url.as_str())
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| FetchError::Transport(transport_kind(&error).to_owned()))?;
        if response.status() != reqwest::StatusCode::OK {
            tracing::warn!(
                status = response.status().as_u16(),
                "github releases API returned a non-200 status"
            );
            return Ok(Vec::new());
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| FetchError::Transport(transport_kind(&error).to_owned()))?;
        let raw: Vec<RawRelease> = match serde_json::from_slice(&body) {
            Ok(raw) => raw,
            Err(error) => {
                tracing::error!(%error, "github releases payload did not decode");
                return Ok(Vec::new());
            }
        };
        Ok(raw
            .into_iter()
            .filter(|entry| !entry.draft)
            .map(|entry| GitHubRelease {
                tag_name: entry.tag_name.clone(),
                name: entry.name.unwrap_or_else(|| entry.tag_name.clone()),
                body: entry.body.unwrap_or_default(),
                published_at: entry.published_at,
                html_url: entry.html_url,
                prerelease: entry.prerelease,
            })
            .collect())
    }
}

/// Short transport failure kind. Only the kind is kept, never the URL, the
/// same shape as v2's logged exception.
fn transport_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "connection failed"
    } else {
        "request failed"
    }
}
