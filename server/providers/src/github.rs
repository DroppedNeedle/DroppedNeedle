//! GitHub: the releases client (the update check) and the repository reads
//! plugin installs make.
//!
//! Both send on the one `github` rate row, so they share its hourly
//! allowance and any wait GitHub names. Plugin installs read through
//! [GitHubRepos], which answers typed errors for the installer to judge.
//!
//! The releases client is a port of v2's GitHub repository and its
//! `GitHubRelease` schema. It is the quiet client: every failure (an error
//! answer, an undecodable body, no answer at all) degrades to an empty
//! release list with a log line, never an error. An update check must not
//! break the app it checks for updates. The wire shapes, and the dates they
//! were verified on, are in `github/models.rs`.
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
//! with neither, wait at least a minute. A 403 with no rate-limit headers at
//! all is a secondary limit and rests that minute; one that reports
//! allowance left is a refusal.
//!
//! A renamed or transferred repository answers its old address with a
//! redirect to the new one. It is followed on the same host or on
//! `api.github.com`, so the update check keeps working after a rename.

use std::time::{Duration, Instant};

use crate::RequestPriority;
use crate::client::{decode, parse_base, unusable_base};
use crate::error::ProviderError;
use crate::executor::{
    Answer, Backoff, Executor, RedirectRule, Redirects, Reply, Request, Spec, USER_COOLDOWN_WAIT,
    Url, never_foreign, standard_classify,
};
use crate::web_address;

mod models;

use models::{Commit, Release, ReleaseList, Repository};

/// The provider name errors carry, and its rate row.
pub const SOURCE: &str = "github";
/// Production endpoint.
pub const GITHUB_RELEASES_URL: &str =
    "https://api.github.com/repos/DroppedNeedle/DroppedNeedle/releases";
/// How long a fetched release list stays fresh, kept from v2's 1h TTL.
pub const RELEASES_CACHE_TTL: Duration = Duration::from_secs(3600);
/// GitHub API origin, which plugin installs read.
pub const API_BASE: &str = "https://api.github.com";
/// GitHub archive download origin, which plugin installs download from.
pub const CODELOAD_BASE: &str = "https://codeload.github.com";
/// Largest releases answer read.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Largest plugin archive downloaded (32 MiB).
pub const MAX_ARCHIVE_BYTES: usize = 32 * 1024 * 1024;

/// GitHub on the wire: the standard rules plus its rate-limit answers.
pub static SPEC: Spec = API;

const API: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(10),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(60)),
    retry_after_cap: Duration::from_secs(3600),
    classify,
};

/// Archive downloads from codeload: GitHub's rules with the archive's size
/// cap and the shared HTTP client's 30 second deadline. Built on the
/// `github` row's limiter, so they pace and cool down with every other
/// GitHub call.
pub static ARCHIVE_SPEC: Spec = Spec {
    timeout: Duration::from_secs(30),
    max_body: MAX_ARCHIVE_BYTES,
    ..API
};

/// A 403 or 429 that says the allowance is spent, names a wait, or carries
/// no rate-limit header at all is a rate limit; a 403 that reports
/// allowance left is a refusal.
fn classify(answer: &Answer<'_>) -> Result<(), ProviderError> {
    let reply = answer.reply;
    if matches!(reply.status(), 403 | 429) {
        let remaining = reply.header("x-ratelimit-remaining");
        let spent = remaining.is_some_and(|remaining| remaining.trim() == "0");
        let named_wait = reply.header("retry-after").is_some();
        if spent || named_wait || remaining.is_none() {
            return Err(ProviderError::RateLimited {
                provider: SOURCE,
                retry_after: answer.retry_after(),
            });
        }
    }
    standard_classify(answer)
}

/// Redirects stay on the host that sent them, or go to the API host.
static HOPS: RedirectRule = RedirectRule {
    max_hops: 3,
    next: next_hop,
    foreign: never_foreign,
};

/// The hop from `from` to `to`, when it stays on `from`'s origin or goes to
/// `https://api.github.com`. Credentials and fragments are never followed.
fn next_hop(from: &Url, to: &Url) -> Option<Url> {
    if !to.username().is_empty() || to.password().is_some() {
        return None;
    }
    let same_origin = to.origin() == from.origin();
    let api_host = to.scheme() == "https"
        && to.port().is_none()
        && to
            .domain()
            .is_some_and(|host| host.eq_ignore_ascii_case("api.github.com"));
    if !(same_origin || api_host) {
        return None;
    }
    let mut next = to.clone();
    next.set_fragment(None);
    Some(next)
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
            // The executor has already logged an outage once; only what it
            // passed over quietly is worth a warning here.
            Err(error) if error.trips_breaker() => {
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
            .header("accept", "application/vnd.github+json")
            .redirects(Redirects::Follow(&HOPS));
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
        html_url: web_address::or_empty(entry.html_url),
        prerelease: entry.prerelease.unwrap_or(true),
        tag_name: entry.tag_name,
    }
}

/// The repository reads a plugin install makes: its id, its latest release,
/// the commit a reference names and the archive of one commit. "Nothing
/// there" is `Ok(None)`; every other failure is the executor's typed error.
#[derive(Debug, Clone)]
pub struct GitHubRepos {
    api: Executor,
    archives: Executor,
    api_base: Option<Url>,
    codeload: Option<Url>,
}

impl GitHubRepos {
    /// Reads through `api` to the API at `api_base`, and archive downloads
    /// through `archives` from `codeload`. Both executors are on the
    /// `github` row.
    #[must_use]
    pub fn new(api: Executor, archives: Executor, api_base: &str, codeload: &str) -> Self {
        Self {
            api,
            archives,
            api_base: parse_base(api_base, SOURCE),
            codeload: parse_base(codeload, SOURCE),
        }
    }

    /// Whether the `github` row can cover `reads` requests a person is
    /// waiting on. Each request may wait [USER_COOLDOWN_WAIT] for its turn,
    /// so the row covers them when the last one is due within that. If it
    /// cannot, the answer is a rate limit naming the wait for all of them,
    /// and nothing is spent: an operation the row cannot finish never takes
    /// the first of its requests.
    ///
    /// # Errors
    ///
    /// [`ProviderError::RateLimited`] with that wait.
    pub fn room_for(&self, reads: u32) -> Result<(), ProviderError> {
        let wait = self.api.limiter().retry_after(reads);
        if wait <= USER_COOLDOWN_WAIT {
            return Ok(());
        }
        Err(ProviderError::RateLimited {
            provider: SOURCE,
            retry_after: Some(wait),
        })
    }

    /// GitHub's id for `owner/repo`, or `None` when there is no such public
    /// repository.
    ///
    /// # Errors
    ///
    /// The executor's typed failure, or a payload error for an answer that
    /// does not decode.
    pub async fn repository_id(
        &self,
        owner: &str,
        repo: &str,
        priority: RequestPriority,
    ) -> Result<Option<u64>, ProviderError> {
        let found: Option<Repository> = self
            .read(&["repos", owner, repo], priority)
            .await?
            .map(|reply| decode(&reply))
            .transpose()?;
        Ok(found.map(|repository| repository.id))
    }

    /// The tag of the newest release, or `None` when the repository has no
    /// release.
    ///
    /// # Errors
    ///
    /// As [GitHubRepos::repository_id].
    pub async fn latest_release_tag(
        &self,
        owner: &str,
        repo: &str,
        priority: RequestPriority,
    ) -> Result<Option<String>, ProviderError> {
        let found: Option<Release> = self
            .read(&["repos", owner, repo, "releases", "latest"], priority)
            .await?
            .map(|reply| decode(&reply))
            .transpose()?;
        Ok(found.map(|release| release.tag_name))
    }

    /// The SHA of the commit `reference` (a tag, branch or commit, which
    /// may hold slashes) names, or `None` when the repository has no such
    /// ref or no commits at all.
    ///
    /// # Errors
    ///
    /// As [GitHubRepos::repository_id].
    pub async fn commit_sha(
        &self,
        owner: &str,
        repo: &str,
        reference: &str,
        priority: RequestPriority,
    ) -> Result<Option<String>, ProviderError> {
        let mut path = vec!["repos", owner, repo, "commits"];
        path.extend(reference.split('/'));
        let found: Option<Commit> = match self.read(&path, priority).await {
            Ok(reply) => reply.map(|reply| decode(&reply)).transpose()?,
            // An unknown ref and an empty repository; see `Commit`.
            Err(ProviderError::BadRequest {
                status: 409 | 422, ..
            }) => None,
            Err(error) => return Err(error),
        };
        Ok(found.map(|commit| commit.sha))
    }

    /// The zip archive of `owner/repo` at `commit`, read under
    /// [MAX_ARCHIVE_BYTES], or `None` when GitHub has no such commit.
    ///
    /// # Errors
    ///
    /// The executor's typed failure; an archive over the cap is a
    /// too-large payload error.
    pub async fn archive(
        &self,
        owner: &str,
        repo: &str,
        commit: &str,
        priority: RequestPriority,
    ) -> Result<Option<Vec<u8>>, ProviderError> {
        let base = self
            .codeload
            .as_ref()
            .ok_or_else(|| unusable_base(SOURCE))?;
        let request = Request::get(base, &[owner, repo, "zip", commit], &[])
            .map_err(|_| unusable_base(SOURCE))?
            .redirects(Redirects::Follow(&HOPS));
        let reply = absent_on_404(self.archives.send(request, priority).await)?;
        Ok(reply.map(|reply| reply.bytes().to_vec()))
    }

    /// One API GET of `path`, `None` on a 404.
    async fn read(
        &self,
        path: &[&str],
        priority: RequestPriority,
    ) -> Result<Option<Reply>, ProviderError> {
        let base = self
            .api_base
            .as_ref()
            .ok_or_else(|| unusable_base(SOURCE))?;
        let request = Request::get(base, path, &[])
            .map_err(|_| unusable_base(SOURCE))?
            .header("accept", "application/vnd.github+json")
            .redirects(Redirects::Follow(&HOPS));
        absent_on_404(self.api.send(request, priority).await)
    }
}

/// A 404 is "nothing there", never a failure.
fn absent_on_404<T>(answer: Result<T, ProviderError>) -> Result<Option<T>, ProviderError> {
    match answer {
        Ok(found) => Ok(Some(found)),
        Err(ProviderError::NotFound { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}
