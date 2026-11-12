//! Version reads: current version, update check, release history.
//!
//! The check logic is pure over the [`ReleaseSource`] port, and production
//! runs [`GitHubReleases`], the build tag plus the GitHub releases client
//! (cached for an hour, quiet on failure).
//!
//! Comparison rules: trim a leading `v`, parse both tags as semver and
//! compare by precedence (build metadata ignored). Anything else fails
//! closed as `comparison_failed`. A `dev` build reports an update as
//! available when comparison fails so the full UI stays testable.
//! `latest_release` ships only when an update is available; a missing
//! latest release answers with the current version alone.
//!
//! The running version is the image's `COMMIT_TAG`, which `AppConfig`
//! reads at start, else `dev`.

use std::sync::Arc;

use axum::{Json, extract::State};
use droppedneedle_auth::User;
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_providers::RequestPriority;
use futures_util::future::BoxFuture;
use serde::Serialize;
use utoipa::ToSchema;

/// Current-version value for a dev checkout.
const DEV_VERSION: &str = "dev";

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

/// Production release data: the build tag plus GitHub releases.
#[derive(Debug, Clone)]
pub struct GitHubReleases {
    client: droppedneedle_providers::github::GitHubClient,
    current: VersionInfo,
}

impl GitHubReleases {
    /// Wrap the releases client for the running build.
    pub fn new(
        client: droppedneedle_providers::github::GitHubClient,
        current: VersionInfo,
    ) -> Self {
        Self { client, current }
    }
}

fn from_github(release: droppedneedle_providers::github::GitHubRelease) -> GitHubRelease {
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

    // Both are asked for by a page someone has open, so they go at user
    // priority: while GitHub rests, or no request is free soon, the answer
    // is nothing at once instead of a held page.
    fn latest_release(&self) -> BoxFuture<'_, Option<GitHubRelease>> {
        Box::pin(async move {
            self.client
                .fetch_latest_release(RequestPriority::UserInitiated)
                .await
                .map(from_github)
        })
    }

    fn release_history(&self) -> BoxFuture<'_, Vec<GitHubRelease>> {
        Box::pin(async move {
            self.client
                .fetch_releases(RequestPriority::UserInitiated)
                .await
                .into_iter()
                .map(from_github)
                .collect()
        })
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

/// Version routes, for any signed-in user.
pub fn routes() -> Routes<VersionState> {
    Routes::new()
        .user(routes!(get_version))
        .user(routes!(check_update))
        .user(routes!(get_releases))
}

/// Parse a tag as semver after trimming a leading `v`. Anything that is
/// not a full semver version is unparseable and comparison fails closed.
fn parse_tag(tag: &str) -> Option<semver::Version> {
    semver::Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

/// Compare two tags. Returns `(update_available, comparison_failed)`.
/// Unparseable tags fail closed.
fn is_newer(latest_tag: &str, current_tag: &str) -> (bool, bool) {
    match (parse_tag(latest_tag), parse_tag(current_tag)) {
        (Some(latest), Some(current)) => (latest.cmp_precedence(&current).is_gt(), false),
        _ => (false, true),
    }
}

/// Pure update-check over one current version and one latest release,
/// including the dev-build carve-out.
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
    let is_dev = current.version == DEV_VERSION;
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
    tag = "platform",
    path = "/api/v3/version",
    responses(
        (status = 200, description = "Running build identity", body = VersionInfo),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_version(State(state): State<VersionState>, _user: User) -> Json<VersionInfo> {
    Json(state.releases.current_version())
}

/// Update check against the latest known release.
#[utoipa::path(
    get,
    tag = "platform",
    path = "/api/v3/version/check-update",
    responses(
        (status = 200, description = "Update check result", body = UpdateCheckResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn check_update(
    State(state): State<VersionState>,
    _user: User,
) -> Json<UpdateCheckResponse> {
    let current = state.releases.current_version();
    let latest = state.releases.latest_release().await;
    Json(build_update_response(&current, latest.as_ref()))
}

/// Release history, newest first.
#[utoipa::path(
    get,
    tag = "platform",
    path = "/api/v3/version/releases",
    responses(
        (status = 200, description = "Release history", body = Vec<GitHubRelease>),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_releases(
    State(state): State<VersionState>,
    _user: User,
) -> Json<Vec<GitHubRelease>> {
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

    /// The update check compares the running version with the latest tag: a
    /// newer tag is an update with its release attached, the same or an
    /// older one is not, no latest answers the current version only, an
    /// unreadable tag fails closed, and a dev build reports one.
    #[test]
    fn update_check_compares_tags() {
        let answer = build_update_response(&current("1.2.0"), Some(&release("v1.3.0")));
        assert!(answer.update_available, "newer");
        assert!(!answer.comparison_failed, "newer");
        assert_eq!(answer.latest_version.as_deref(), Some("v1.3.0"), "newer");
        assert!(answer.latest_release.is_some(), "newer");

        for tag in ["v1.2.0", "1.1.9"] {
            let answer = build_update_response(&current("1.2.0"), Some(&release(tag)));
            assert!(!answer.update_available, "tag {tag}");
            assert!(!answer.comparison_failed, "tag {tag}");
            assert!(answer.latest_release.is_none(), "tag {tag}");
        }

        let answer = build_update_response(&current("1.2.0"), None);
        assert_eq!(answer.current_version, "1.2.0", "no latest");
        assert_eq!(answer.latest_version, None, "no latest");
        assert!(!answer.update_available, "no latest");
        assert!(!answer.comparison_failed, "no latest");
        assert!(answer.latest_release.is_none(), "no latest");

        let answer = build_update_response(&current("1.2.0"), Some(&release("nightly")));
        assert!(!answer.update_available, "unreadable tag");
        assert!(answer.comparison_failed, "unreadable tag");
        assert!(answer.latest_release.is_none(), "unreadable tag");

        let answer = build_update_response(&current("dev"), Some(&release("nightly")));
        assert!(answer.update_available, "dev build");
        assert!(answer.comparison_failed, "dev build");
        assert!(answer.latest_release.is_some(), "dev build");
    }

    /// A v3 prerelease compares instead of failing: a later beta or the
    /// final release is an update, an equal or older one is not; the live
    /// beta.3 answer (latest `v2.15.0`) is up to date.
    #[test]
    fn update_check_compares_v3_prereleases() {
        let beta = current("3.0.0-beta.3");

        for tag in ["v3.0.0-beta.4", "v3.0.0-beta.10", "v3.0.0"] {
            let answer = build_update_response(&beta, Some(&release(tag)));
            assert!(answer.update_available, "tag {tag}");
            assert!(!answer.comparison_failed, "tag {tag}");
            assert_eq!(answer.latest_version.as_deref(), Some(tag), "tag {tag}");
            assert!(answer.latest_release.is_some(), "tag {tag}");
        }

        for tag in ["v3.0.0-beta.3", "v3.0.0-beta.2", "v3.0.0-alpha.9"] {
            let answer = build_update_response(&beta, Some(&release(tag)));
            assert!(!answer.update_available, "tag {tag}");
            assert!(!answer.comparison_failed, "tag {tag}");
            assert!(answer.latest_release.is_none(), "tag {tag}");
        }

        // GitHub's latest release is the v2 line; a v3 beta is ahead of it.
        let answer = build_update_response(&beta, Some(&release("v2.15.0")));
        assert_eq!(answer.current_version, "3.0.0-beta.3", "v2 latest");
        assert_eq!(
            answer.latest_version.as_deref(),
            Some("v2.15.0"),
            "v2 latest"
        );
        assert!(!answer.update_available, "v2 latest");
        assert!(!answer.comparison_failed, "v2 latest");
        assert!(answer.latest_release.is_none(), "v2 latest");

        // A release outranks the beta before it; build metadata is ignored.
        let answer = build_update_response(&current("3.0.0"), Some(&release("v3.0.0-beta.3")));
        assert!(!answer.update_available, "release vs beta");
        assert!(!answer.comparison_failed, "release vs beta");

        let answer = build_update_response(
            &current("3.0.0-beta.3+sha.abc"),
            Some(&release("v3.0.0-beta.3")),
        );
        assert!(!answer.update_available, "build metadata");
        assert!(!answer.comparison_failed, "build metadata");
    }
}
