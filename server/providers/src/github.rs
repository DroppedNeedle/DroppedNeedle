//! GitHub releases client (the update check).
//!
//! Port of v2's GitHub repository and its `GitHubRelease` schema. This is
//! the quiet client: every failure (an error answer, an undecodable body,
//! no answer at all) degrades to an empty release list with a log line,
//! never an error. An update check must not break the app it checks for
//! updates. Requests go through the executor at the `github` rate row.
//!
//! Kept behaviors:
//!
//! - The `Accept: application/vnd.github+json` header on every call.
//! - Draft releases are filtered out; prereleases stay in the list but
//!   [GitHubClient::fetch_latest_release] skips them (the API returns
//!   newest first, so the first stable release wins).
//! - Missing `name` falls back to the tag, missing `body` to `""`.
//! - One hourly memo of the last good answer. A failure is never
//!   remembered, so the next check asks again.
//!
//! Rate limits, from GitHub's REST rate-limit page (read 2026-10-18): a 403
//! or 429 with `x-ratelimit-remaining: 0` waits until `x-ratelimit-reset`
//! (unix seconds); any other 403 or 429 with `retry-after` waits that long;
//! with neither, wait at least a minute.

use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::RequestPriority;
use crate::client::{decode, parse_base, unusable_base};
use crate::error::ProviderError;
use crate::executor::{Answer, Backoff, Executor, Request, Spec, Url, standard_classify};

/// The provider name errors carry, and its rate row.
pub const SOURCE: &str = "github";
/// Production endpoint.
pub const GITHUB_RELEASES_URL: &str =
    "https://api.github.com/repos/DroppedNeedle/DroppedNeedle/releases";
/// How long a fetched release list stays fresh, kept from v2's 1h TTL.
pub const RELEASES_CACHE_TTL: Duration = Duration::from_secs(3600);
/// Largest releases answer read.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// GitHub on the wire: the standard rules plus its rate-limit answers.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(10),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(60)),
    retry_after_cap: Duration::from_secs(3600),
    classify,
};

/// A 403 or 429 that says the allowance is spent, or names a wait, is a
/// rate limit; any other 403 is a refusal.
fn classify(answer: &Answer<'_>) -> Result<(), ProviderError> {
    let reply = answer.reply;
    if matches!(reply.status(), 403 | 429) {
        let spent = reply
            .header("x-ratelimit-remaining")
            .is_some_and(|remaining| remaining.trim() == "0");
        if spent || reply.header("retry-after").is_some() {
            return Err(ProviderError::RateLimited {
                provider: SOURCE,
                retry_after: answer.retry_after(),
            });
        }
    }
    standard_classify(answer)
}

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

/// The hourly memo: when the list was fetched plus the list itself.
type ReleaseMemo = std::sync::Arc<tokio::sync::Mutex<Option<(Instant, Vec<GitHubRelease>)>>>;

/// The releases client, built once with the providers; clones share the
/// hourly memo.
#[derive(Debug, Clone)]
pub struct GitHubClient {
    executor: Executor,
    url: Option<Url>,
    memo: ReleaseMemo,
}

impl GitHubClient {
    /// The client sending through `executor` to the releases list at
    /// `releases_url`.
    #[must_use]
    pub fn new(executor: Executor, releases_url: &str) -> Self {
        Self {
            executor,
            url: parse_base(releases_url, SOURCE),
            memo: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// All non-draft releases, newest first, with the hourly memo. Every
    /// failure degrades to `[]` with a log line and is not remembered. A
    /// person's check that the executor turns down because the row has no
    /// request free soon answers `[]` at once without sending.
    pub async fn fetch_releases(&self, priority: RequestPriority) -> Vec<GitHubRelease> {
        if let Some(cached) = self.memo_get().await {
            return cached;
        }
        match self.fetch(priority).await {
            Ok(releases) => {
                self.memo_put(releases.clone()).await;
                releases
            }
            Err(error @ ProviderError::RateLimited { .. }) => {
                tracing::debug!(%error, "github releases skipped");
                Vec::new()
            }
            Err(error) => {
                tracing::warn!(%error, "github releases unavailable");
                Vec::new()
            }
        }
    }

    /// The latest non-prerelease release, if any.
    pub async fn fetch_latest_release(&self, priority: RequestPriority) -> Option<GitHubRelease> {
        self.fetch_releases(priority)
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

    /// One uncached fetch.
    async fn fetch(&self, priority: RequestPriority) -> Result<Vec<GitHubRelease>, ProviderError> {
        let url = self.url.as_ref().ok_or_else(|| unusable_base(SOURCE))?;
        let request = Request::get(url, &[], &[])
            .map_err(|_| unusable_base(SOURCE))?
            .header("accept", "application/vnd.github+json");
        let reply = self.executor.send(request, priority).await?;
        let raw: Vec<RawRelease> = decode(&reply)?;
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
