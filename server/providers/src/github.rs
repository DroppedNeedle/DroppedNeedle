//! GitHub releases client (the update check).
//!
//! Port of v2's GitHub repository and its `GitHubRelease` schema. This is
//! the quiet client: every failure (an error answer, an undecodable body,
//! no answer at all) degrades to an empty release list with a log line,
//! never an error. An update check must not break the app it checks for
//! updates. Requests go through the executor at the `github` rate row. The
//! wire shapes, and the dates they were verified on, are in
//! `github/models.rs`.
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
//! Rate limits, seen on 2026-10-18 per GitHub's REST rate-limit page: a 403
//! or 429 with `x-ratelimit-remaining: 0` waits until `x-ratelimit-reset`
//! (unix seconds); any other 403 or 429 with `retry-after` waits that long;
//! with neither, wait at least a minute.

use std::time::{Duration, Instant};

use crate::RequestPriority;
use crate::client::{decode, parse_base, unusable_base};
use crate::error::ProviderError;
use crate::executor::{Answer, Backoff, Executor, Request, Spec, Url, standard_classify};

mod models;

use models::{Release, ReleaseList};

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

/// One published release, as the update check reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct GitHubRelease {
    /// Release tag such as "v2.4.1".
    pub tag_name: String,
    /// Display name, falling back to the tag when the API omits it.
    pub name: String,
    /// Release notes, `""` when the API omits them.
    pub body: String,
    /// Publish timestamp, `""` when the API omits it.
    pub published_at: String,
    /// Release page URL, `""` when the API omits it.
    pub html_url: String,
    /// True for prereleases (kept in the list, skipped by latest).
    pub prerelease: bool,
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

    /// One uncached fetch of the releases list. Every failure is typed here;
    /// [GitHubClient::fetch_releases] turns it into an empty list.
    pub async fn fetch(
        &self,
        priority: RequestPriority,
    ) -> Result<Vec<GitHubRelease>, ProviderError> {
        let url = self.url.as_ref().ok_or_else(|| unusable_base(SOURCE))?;
        let request = Request::get(url, &[], &[])
            .map_err(|_| unusable_base(SOURCE))?
            .header("accept", "application/vnd.github+json");
        let reply = self.executor.send(request, priority).await?;
        let list: ReleaseList = decode(&reply)?;
        Ok(list
            .into_iter()
            .filter(|entry| !entry.draft.unwrap_or(true))
            .map(from_wire)
            .collect())
    }
}

/// Maps one wire release to the shape the callers read. A release with no
/// prerelease flag reads as a prerelease, so it is never the latest.
fn from_wire(entry: Release) -> GitHubRelease {
    GitHubRelease {
        name: entry.name.unwrap_or_else(|| entry.tag_name.clone()),
        body: entry.body.unwrap_or_default(),
        published_at: entry.published_at.unwrap_or_default(),
        html_url: entry.html_url.unwrap_or_default(),
        prerelease: entry.prerelease.unwrap_or(true),
        tag_name: entry.tag_name,
    }
}
