//! Version reads: current version, update check, release history.
//!
//! Carried over from v2's version route and `VersionService`, read-only:
//! the check logic is pure over the [`ReleaseSource`] port, and production
//! runs [`GitHubReleases`], the build tag plus the GitHub releases client
//! (cached for an hour, quiet on failure).
//!
//! Comparison rules, v2 kept: strip leading `v`s, compare numeric release
//! cores; anything else fails closed as `comparison_failed`. Dev builds
//! (`dev`, `hosting-local`) report an update as available when comparison
//! fails so the full UI stays testable. `latest_release` ships only when an
//! update is available; a missing latest release answers with the current
//! version alone.
//!
//! The running version is the `COMMIT_TAG` the image was built with (v2
//! read the same name from the container environment), else `dev`.

use std::sync::Arc;

use axum::{Json, Router, extract::State, routing::get};
use futures_util::future::BoxFuture;
use serde::Serialize;
use utoipa::ToSchema;

/// Current-version values for a dev checkout.
const DEV_VERSIONS: &[&str] = &["dev", "hosting-local"];

/// Running build identity.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VersionInfo {
    /// Release tag, or `dev` on a checkout build.
    pub version: String,
    /// Build timestamp when the image records one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_date: Option<String>,
}

/// One GitHub release row.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GitHubRelease {
    /// Release tag.
    pub tag_name: String,
    /// Publish timestamp.
    pub published_at: String,
    /// Release page URL.
    pub html_url: String,
    /// Release title when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Release notes when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// True for prereleases.
    pub prerelease: bool,
}

/// Update-check answer.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UpdateCheckResponse {
    /// Running version.
    pub current_version: String,
    /// Latest known tag, absent when the lookup found nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    /// True when a newer release exists (or a dev build failed comparison).
    pub update_available: bool,
    /// True when the two tags could not be compared.
    pub comparison_failed: bool,
    /// Latest release detail, present only when an update is available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_release: Option<GitHubRelease>,
}

/// Release-data port: [`GitHubReleases`] in production.
pub trait ReleaseSource: Send + Sync + 'static {
    /// Running build identity.
    fn current_version(&self) -> VersionInfo;
    /// Latest release, or `None` when the lookup found nothing.
    fn latest_release(&self) -> BoxFuture<'_, Option<GitHubRelease>>;
    /// Release history, newest first.
    fn release_history(&self) -> BoxFuture<'_, Vec<GitHubRelease>>;
}

/// The running build: `COMMIT_TAG` and `BUILD_DATE` as baked in at compile
/// time by the image build, else `dev` like v2.
pub fn build_version() -> VersionInfo {
    let version = option_env!("COMMIT_TAG")
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .unwrap_or("dev");
    VersionInfo {
        version: version.to_owned(),
        build_date: option_env!("BUILD_DATE")
            .map(str::trim)
            .filter(|date| !date.is_empty())
            .map(str::to_owned),
    }
}

/// Production release data: the build tag plus GitHub releases.
#[derive(Debug, Clone)]
pub struct GitHubReleases {
    client: crate::providers::github::GitHubClient,
    current: VersionInfo,
}

impl GitHubReleases {
    /// Wrap the releases client for the running build.
    pub fn new(client: crate::providers::github::GitHubClient) -> Self {
        Self {
            client,
            current: build_version(),
        }
    }
}

fn from_github(release: crate::providers::github::GitHubRelease) -> GitHubRelease {
    GitHubRelease {
        tag_name: release.tag_name,
        published_at: release.published_at,
        html_url: release.html_url,
        name: Some(release.name),
        body: Some(release.body),
        prerelease: release.prerelease,
    }
}

impl ReleaseSource for GitHubReleases {
    fn current_version(&self) -> VersionInfo {
        self.current.clone()
    }

    fn latest_release(&self) -> BoxFuture<'_, Option<GitHubRelease>> {
        Box::pin(async move { self.client.fetch_latest_release().await.map(from_github) })
    }

    fn release_history(&self) -> BoxFuture<'_, Vec<GitHubRelease>> {
        Box::pin(async move {
            self.client
                .fetch_releases()
                .await
                .into_iter()
                .map(from_github)
                .collect()
        })
    }
}

/// Scripted release data for tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone)]
pub struct FakeReleases {
    current: VersionInfo,
    latest: Option<GitHubRelease>,
    history: Vec<GitHubRelease>,
}

#[cfg(any(test, feature = "test-support"))]
impl FakeReleases {
    /// Build a fake from its parts.
    pub fn new(
        current: VersionInfo,
        latest: Option<GitHubRelease>,
        history: Vec<GitHubRelease>,
    ) -> Self {
        Self {
            current,
            latest,
            history,
        }
    }

    /// Fake for a tagged build with no known releases.
    pub fn tagged(version: &str) -> Self {
        Self::new(
            VersionInfo {
                version: version.to_owned(),
                build_date: None,
            },
            None,
            Vec::new(),
        )
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ReleaseSource for FakeReleases {
    fn current_version(&self) -> VersionInfo {
        self.current.clone()
    }

    fn latest_release(&self) -> BoxFuture<'_, Option<GitHubRelease>> {
        let latest = self.latest.clone();
        Box::pin(async move { latest })
    }

    fn release_history(&self) -> BoxFuture<'_, Vec<GitHubRelease>> {
        let history = self.history.clone();
        Box::pin(async move { history })
    }
}

/// Handler state: the release port behind an `Arc` so handlers stay
/// non-generic for utoipa.
#[derive(Clone)]
pub struct VersionState {
    /// Release data.
    pub releases: Arc<dyn ReleaseSource>,
}

impl VersionState {
    /// Wire the state from any port implementation.
    pub fn new(releases: Arc<dyn ReleaseSource>) -> Self {
        Self { releases }
    }
}

/// Version routes, relative paths for nesting under `/api/v3`.
pub fn routes(state: VersionState) -> Router {
    Router::new()
        .route("/version", get(get_version))
        .route("/version/check-update", get(check_update))
        .route("/version/releases", get(get_releases))
        .with_state(state)
}

/// Parse a tag into numeric release segments. Leading `v`s are stripped (v2
/// `lstrip` kept); the rest must be pure dotted numbers or the tag is
/// unparseable and comparison fails closed.
fn release_core(tag: &str) -> Option<Vec<u64>> {
    let stripped = tag.trim_start_matches('v');
    if stripped.is_empty() {
        return None;
    }
    let mut segments = Vec::new();
    for part in stripped.split('.') {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        segments.push(part.parse::<u64>().ok()?);
    }
    Some(segments)
}

/// Compare two tags. Returns `(update_available, comparison_failed)`, v2
/// `_is_newer` semantics kept: unparseable tags fail closed.
fn is_newer(latest_tag: &str, current_tag: &str) -> (bool, bool) {
    match (release_core(latest_tag), release_core(current_tag)) {
        (Some(latest), Some(current)) => (latest > current, false),
        _ => (false, true),
    }
}

/// Pure update-check over one current version and one latest release. V2
/// `check_for_updates` rules kept, including the dev-build carve-out.
fn build_update_response(
    current: &VersionInfo,
    latest: Option<&GitHubRelease>,
) -> UpdateCheckResponse {
    let Some(latest) = latest else {
        return UpdateCheckResponse {
            current_version: current.version.clone(),
            latest_version: None,
            update_available: false,
            comparison_failed: false,
            latest_release: None,
        };
    };
    let (mut update_available, comparison_failed) = is_newer(&latest.tag_name, &current.version);
    let is_dev = DEV_VERSIONS.contains(&current.version.as_str());
    if comparison_failed && is_dev {
        update_available = true;
    }
    UpdateCheckResponse {
        current_version: current.version.clone(),
        latest_version: Some(latest.tag_name.clone()),
        update_available,
        comparison_failed,
        latest_release: if update_available {
            Some(latest.clone())
        } else {
            None
        },
    }
}

/// Running version.
#[utoipa::path(
    get,
    path = "/api/v3/version",
    responses(
        (status = 200, description = "Running build identity", body = VersionInfo),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_version(State(state): State<VersionState>) -> Json<VersionInfo> {
    Json(state.releases.current_version())
}

/// Update check against the latest known release.
#[utoipa::path(
    get,
    path = "/api/v3/version/check-update",
    responses(
        (status = 200, description = "Update check result", body = UpdateCheckResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn check_update(State(state): State<VersionState>) -> Json<UpdateCheckResponse> {
    let current = state.releases.current_version();
    let latest = state.releases.latest_release().await;
    Json(build_update_response(&current, latest.as_ref()))
}

/// Release history, newest first.
#[utoipa::path(
    get,
    path = "/api/v3/version/releases",
    responses(
        (status = 200, description = "Release history", body = Vec<GitHubRelease>),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_releases(State(state): State<VersionState>) -> Json<Vec<GitHubRelease>> {
    Json(state.releases.release_history().await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag.to_owned(),
            published_at: "2026-01-01T00:00:00Z".to_owned(),
            html_url: "https://example.invalid/r".to_owned(),
            name: None,
            body: None,
            prerelease: false,
        }
    }

    fn current(version: &str) -> VersionInfo {
        VersionInfo {
            version: version.to_owned(),
            build_date: None,
        }
    }

    #[test]
    fn newer_tag_marks_update_and_attaches_release() {
        let answer = build_update_response(&current("1.2.0"), Some(&release("v1.3.0")));
        assert!(answer.update_available);
        assert!(!answer.comparison_failed);
        assert_eq!(answer.latest_version.as_deref(), Some("v1.3.0"));
        assert!(answer.latest_release.is_some());
    }

    #[test]
    fn same_or_older_tag_marks_no_update_without_release() {
        for tag in ["v1.2.0", "1.1.9"] {
            let answer = build_update_response(&current("1.2.0"), Some(&release(tag)));
            assert!(!answer.update_available, "tag {tag}");
            assert!(!answer.comparison_failed, "tag {tag}");
            assert!(answer.latest_release.is_none(), "tag {tag}");
        }
    }

    #[test]
    fn missing_latest_answers_current_only() {
        let answer = build_update_response(&current("1.2.0"), None);
        assert_eq!(answer.current_version, "1.2.0");
        assert_eq!(answer.latest_version, None);
        assert!(!answer.update_available);
        assert!(!answer.comparison_failed);
        assert!(answer.latest_release.is_none());
    }

    #[test]
    fn unparseable_tags_fail_closed() {
        let answer = build_update_response(&current("1.2.0"), Some(&release("nightly")));
        assert!(!answer.update_available);
        assert!(answer.comparison_failed);
        assert!(answer.latest_release.is_none());
    }

    #[test]
    fn dev_build_with_failed_comparison_reports_update() {
        for dev in ["dev", "hosting-local"] {
            let answer = build_update_response(&current(dev), Some(&release("nightly")));
            assert!(answer.update_available, "build {dev}");
            assert!(answer.comparison_failed, "build {dev}");
            assert!(answer.latest_release.is_some(), "build {dev}");
        }
    }
}
