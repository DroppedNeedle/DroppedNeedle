//! Scripted release data.

use futures_util::future::BoxFuture;

use crate::version::*;

/// Scripted release data for tests.
#[derive(Debug, Clone)]
pub struct FakeReleases {
    current: VersionInfo,
    latest: Option<GitHubRelease>,
    history: Vec<GitHubRelease>,
}

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
