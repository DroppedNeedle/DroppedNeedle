//! Plugin acquisition sources behind the downloads fetch seam, plus the
//! plugin release scorer.
//!
//! A plugin source is `plugin:<name>`: its own `download_client`, fed by
//! every `indexer` plugin that targets it. Enqueue searches those indexers,
//! ranks the results with [`rank_plugin_releases`], and hands the
//! `candidate_index`-th automatic pick to the client; failover walks to the
//! next pick, as with Usenet releases.
//!
//! "You rank your source; the app enforces policy": the plugin's own score
//! (clamped to 0..1) is the match confidence, and the same gates as the
//! built-in sources apply on top: quarantine, ignored and required terms,
//! the size cap and the quality band. Scores of 0.70 and up download
//! automatically; 0.50 to 0.70 would need a person to pick them; anything
//! lower is dropped. Port of v2's `PluginReleaseScorer`.

use std::path::PathBuf;
use std::sync::Arc;

use super::downloads::journal::Journal;
use super::downloads::quarantine::QUARANTINE_TTL_SECONDS;
use super::downloads::source_tag::SourceTag;
use super::downloads::sources::{
    DownloadSource, Materialization, SourceError, SourceHandle, TransferProgress,
};
use super::downloads::store::TaskRow;
use super::search_jobs::candidates::{Candidate, MAX_PER_SOURCE, Pin};
use super::search_jobs::pinned;
use super::target::reasons::TrackReason;
use super::target::{SearchTarget, Targets};
use super::usenet::policy::{QualityTier, UsenetPolicy};
use droppedneedle_plugins::capabilities::acquisition::{
    PluginEnqueue, PluginSearchResult, PluginTaskHandle,
};
use droppedneedle_plugins::host::PluginHost;
use droppedneedle_plugins::runtime::CallError;
use futures_util::future::BoxFuture;

/// Score at or above which a release downloads without a person.
pub const AUTO_ACCEPT: f64 = 0.70;
/// Score at or above which a release is offered for a manual pick.
pub const MANUAL_ACCEPT: f64 = 0.50;
/// Releases kept after ranking.
const RANKED_MAX: usize = 50;

/// How sure the scorer is about one release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Band {
    /// Below [`MANUAL_ACCEPT`]: never offered.
    Rejected,
    /// Offered for a manual pick only.
    Manual,
    /// Downloads automatically.
    Auto,
}

/// One ranked plugin release.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredRelease {
    /// The plugin's result.
    pub release: PluginSearchResult,
    /// Clamped score.
    pub score: f64,
    /// Automatic or manual.
    pub band: Band,
    /// Parsed quality tier, when the plugin gave one.
    pub tier: Option<QualityTier>,
}

/// The policy gates a plugin release must pass.
#[derive(Debug, Clone)]
pub struct ReleasePolicy {
    /// Quality band and size cap.
    pub usenet: UsenetPolicy,
    /// A release whose title contains any of these is dropped.
    pub ignored_terms: Vec<String>,
    /// A release must contain all of these.
    pub required_terms: Vec<String>,
}

/// Quarantine identity for one plugin result: its payload, or the
/// normalised title plus size in MB when it has none (v2's key).
pub fn release_identity(release: &PluginSearchResult) -> String {
    let payload = release.payload.trim();
    if !payload.is_empty() {
        return payload.to_owned();
    }
    let title = release
        .title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    format!("{title}\u{1f}{}", release.size_bytes.max(0) / (1024 * 1024))
}

/// Rank one source's releases: gates first, then band, quality tier and
/// score, best first. Ties keep title order.
pub fn rank_plugin_releases(
    releases: Vec<PluginSearchResult>,
    policy: &ReleasePolicy,
    quarantined: &[String],
) -> Vec<ScoredRelease> {
    let lower_terms = |terms: &[String]| -> Vec<String> {
        terms
            .iter()
            .map(|term| term.trim().to_lowercase())
            .filter(|term| !term.is_empty())
            .collect()
    };
    let ignored = lower_terms(&policy.ignored_terms);
    let required = lower_terms(&policy.required_terms);
    let mut scored: Vec<ScoredRelease> = releases
        .into_iter()
        .filter_map(|release| {
            let title = release.title.to_lowercase();
            if quarantined.contains(&release_identity(&release))
                || ignored.iter().any(|term| title.contains(term))
                || !required.iter().all(|term| title.contains(term))
                || !policy
                    .usenet
                    .within_size_cap(release.size_bytes.max(0) as u64)
            {
                return None;
            }
            let tier = QualityTier::parse(&release.quality_tier);
            if tier.is_some_and(|tier| !policy.usenet.accepts_tier(tier)) {
                return None;
            }
            let score = if release.score.is_finite() {
                release.score.clamp(0.0, 1.0)
            } else {
                0.0
            };
            let band = if score >= AUTO_ACCEPT {
                Band::Auto
            } else if score >= MANUAL_ACCEPT {
                Band::Manual
            } else {
                Band::Rejected
            };
            (band != Band::Rejected).then_some(ScoredRelease {
                release,
                score,
                band,
                tier,
            })
        })
        .collect();
    scored.sort_by(|left, right| left.release.title.cmp(&right.release.title));
    scored.sort_by(|left, right| {
        right
            .band
            .cmp(&left.band)
            .then(
                right
                    .tier
                    .map(QualityTier::rank)
                    .cmp(&left.tier.map(QualityTier::rank)),
            )
            .then(right.score.total_cmp(&left.score))
    });
    scored.truncate(RANKED_MAX);
    scored
}

/// One plugin source behind the downloads fetch seam.
pub struct PluginDownloadSource {
    host: Arc<PluginHost>,
    key: String,
    tag: SourceTag,
    journal: Arc<Journal>,
    policy: ReleasePolicy,
    targets: Option<Arc<Targets>>,
}

impl PluginDownloadSource {
    /// Adapter for one `plugin:<name>` source key.
    pub fn new(
        host: Arc<PluginHost>,
        key: String,
        journal: Arc<Journal>,
        policy: ReleasePolicy,
    ) -> Self {
        let tag = SourceTag::plugin(&key);
        Self {
            host,
            key,
            tag,
            journal,
            policy,
            targets: None,
        }
    }

    /// Search each task's album: a single track is fetched as part of
    /// its album's release, and a lone-track release is the last resort.
    pub fn with_targets(mut self, targets: Arc<Targets>) -> Self {
        self.targets = Some(targets);
        self
    }

    /// Blocklisted release identities for this source.
    async fn quarantined(&self) -> Result<Vec<String>, SourceError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs_f64())
            .unwrap_or(0.0);
        Ok(self
            .journal
            .read_quarantine_set(now, QUARANTINE_TTL_SECONDS)
            .await
            .map_err(|error| SourceError::LocalFault(error.to_string()))?
            .into_iter()
            .filter(|(source, _)| self.tag == *source)
            .map(|(_, identity)| identity)
            .collect())
    }

    /// The album's releases from this source's indexers.
    async fn album_releases(&self, target: &SearchTarget) -> Vec<PluginSearchResult> {
        self.host
            .search_album(
                &self.key,
                &target.artist,
                &target.album_title,
                target.year.map(i64::from),
                i64::try_from(target.tracklist.len())
                    .ok()
                    .filter(|count| *count > 0),
            )
            .await
    }

    /// Hand one release to the plugin's download client.
    async fn enqueue_release(
        &self,
        task: &TaskRow,
        release: &PluginSearchResult,
        index: usize,
    ) -> Result<SourceHandle, SourceError> {
        let handle = self
            .host
            .enqueue_download(&PluginEnqueue {
                task_id: task.id.clone(),
                source: self.key.clone(),
                files: release.files.clone(),
                payload: release.payload.clone(),
                job_name: format!("droppedneedle-{}-{index}", task.id),
                download_type: task.download_type.clone(),
            })
            .await
            .map_err(source_error)?;
        Ok(SourceHandle {
            source: self.tag.clone(),
            username: handle.username,
            filenames: handle.filenames,
            job_name: handle.job_name,
            nzo_id: handle.nzo_id,
            plugin_token: handle.plugin_token,
            sizes: Vec::new(),
        })
    }

    /// A manual search: the album's releases, ranked and gated, with the
    /// ones a person must judge (the plugin's weaker matches) included.
    async fn search_album(&self, target: &SearchTarget) -> Result<Vec<Candidate>, SourceError> {
        if !target.has_album() {
            return Ok(Vec::new());
        }
        let quarantined = self.quarantined().await?;
        Ok(rank_plugin_releases(
            self.album_releases(target).await,
            &self.policy,
            &quarantined,
        )
        .into_iter()
        .take(MAX_PER_SOURCE)
        .map(|scored| Candidate::from_plugin(&self.tag, scored))
        .collect())
    }

    fn plugin_handle(handle: &SourceHandle) -> PluginTaskHandle {
        PluginTaskHandle {
            source: handle.source.to_string(),
            username: handle.username.clone(),
            filenames: handle.filenames.clone(),
            job_name: handle.job_name.clone(),
            nzo_id: handle.nzo_id.clone(),
            plugin_token: handle.plugin_token.clone(),
        }
    }
}

/// Map a plugin failure onto the fetch seam: a plugin that is down is an
/// outage (retry later), a plugin that refused is a rejected candidate.
fn source_error(error: CallError) -> SourceError {
    match error {
        CallError::Failed(reason) | CallError::Malformed(reason) => SourceError::Rejected(reason),
        other => SourceError::Unavailable(other.to_string()),
    }
}

impl DownloadSource for PluginDownloadSource {
    fn tag(&self) -> SourceTag {
        self.tag.clone()
    }

    fn search<'a>(
        &'a self,
        target: &'a SearchTarget,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, SourceError>> {
        Box::pin(self.search_album(target))
    }

    fn enqueue<'a>(
        &'a self,
        task_id: &'a str,
        candidate_index: i64,
    ) -> BoxFuture<'a, Result<SourceHandle, SourceError>> {
        Box::pin(self.enqueue_pick(task_id, candidate_index))
    }

    fn poll<'a>(
        &'a self,
        handle: &'a SourceHandle,
    ) -> BoxFuture<'a, Result<TransferProgress, SourceError>> {
        Box::pin(self.progress(handle))
    }

    fn landed<'a>(
        &'a self,
        handle: &'a SourceHandle,
    ) -> BoxFuture<'a, Result<crate::landing::Landed, SourceError>> {
        Box::pin(self.landed_paths(handle))
    }

    fn discard<'a>(&'a self, handle: &'a SourceHandle) -> BoxFuture<'a, Result<bool, SourceError>> {
        Box::pin(self.discard_records(handle))
    }

    fn abort<'a>(&'a self, handle: &'a SourceHandle) -> BoxFuture<'a, Result<bool, SourceError>> {
        Box::pin(self.abort_job(handle))
    }

    fn mount_healthy<'a>(&'a self, handle: &'a SourceHandle) -> BoxFuture<'a, Option<bool>> {
        Box::pin(async move {
            self.inspect(handle)
                .await
                .map(|seen| seen.mount_healthy)
                .ok()
        })
    }
}

impl PluginDownloadSource {
    async fn enqueue_pick(
        &self,
        task_id: &str,
        candidate_index: i64,
    ) -> Result<SourceHandle, SourceError> {
        let task = self
            .journal
            .read_task(task_id)
            .await
            .map_err(|error| SourceError::LocalFault(error.to_string()))?
            .ok_or_else(|| SourceError::Rejected(format!("unknown download task {task_id}")))?;
        let target = super::sources::resolve_target(self.targets.as_ref(), &task).await?;
        let quarantined = self.quarantined().await?;
        let index = candidate_index.max(0) as usize;
        if let Some(Pin::Release { identity }) =
            pinned(&self.journal, &task, &self.tag, candidate_index).await
        {
            // The release a person picked from a manual search, found
            // again by its identity (any band: they judged it).
            let release = rank_plugin_releases(
                self.album_releases(&target).await,
                &self.policy,
                &quarantined,
            )
            .into_iter()
            .find(|scored| release_identity(&scored.release) == identity)
            .ok_or_else(|| {
                SourceError::Rejected(format!(
                    "The release you picked is no longer offered by {}.",
                    self.key
                ))
            })?;
            return self.enqueue_release(&task, &release.release, index).await;
        }
        let automatic = |results: Vec<PluginSearchResult>| -> Vec<ScoredRelease> {
            rank_plugin_releases(results, &self.policy, &quarantined)
                .into_iter()
                .filter(|release| release.band == Band::Auto)
                .collect()
        };
        let albums = if target.has_album() {
            automatic(self.album_releases(&target).await)
        } else {
            Vec::new()
        };
        let album_count = albums.len();
        let mut pick = albums.into_iter().nth(index);
        if pick.is_none()
            && let Some(track) = target.track.as_ref()
        {
            // Every album release is used up: a release of the lone track
            // is the last resort, and the reason is recorded.
            pick = automatic(
                self.host
                    .search_track(&self.key, &track.artist, &track.title, None)
                    .await,
            )
            .into_iter()
            .nth(index - album_count);
            if pick.is_some()
                && let Some(targets) = self.targets.as_ref()
            {
                let reason = match (track.no_album, album_count) {
                    (Some(reason), _) => reason,
                    (None, 0) => TrackReason::NoAlbumRelease,
                    (None, _) => TrackReason::AlbumReleasesExhausted,
                };
                targets.record_lone_track(&task, reason).await;
            }
        }
        let pick = pick.ok_or_else(|| {
            SourceError::Rejected(format!(
                "{} has no automatic candidate {candidate_index} for {task_id}",
                self.key
            ))
        })?;
        self.enqueue_release(&task, &pick.release, index).await
    }

    async fn progress(&self, handle: &SourceHandle) -> Result<TransferProgress, SourceError> {
        let status = self
            .host
            .download_status(&Self::plugin_handle(handle))
            .await
            .map_err(source_error)?;
        let state = status.status.to_ascii_lowercase();
        Ok(TransferProgress {
            all_terminal: matches!(state.as_str(), "completed" | "failed"),
            all_succeeded: state == "completed",
            has_active_transfer: status.has_active_transfer || state == "downloading",
            downloaded_bytes: status.bytes_downloaded.max(0) as u64,
            succeeded_filenames: status.succeeded_filenames,
            queue_position_start: status.queue_position_start,
            queue_position_end: status.queue_position_end,
            files_total: u32::try_from(status.files_total.max(0)).unwrap_or(u32::MAX),
            files_completed: u32::try_from(status.files_completed.max(0)).unwrap_or(u32::MAX),
            ..Default::default()
        })
    }

    /// Current client state and exact local evidence.
    async fn inspect(&self, handle: &SourceHandle) -> Result<Materialization, SourceError> {
        let seen = self
            .host
            .download_inspect(&Self::plugin_handle(handle))
            .await
            .map_err(source_error)?;
        let mut paths: Vec<PathBuf> = seen.file_paths.iter().map(PathBuf::from).collect();
        if paths.is_empty() && !seen.workspace_path.is_empty() {
            paths.push(PathBuf::from(&seen.workspace_path));
        }
        Ok(Materialization {
            mount_healthy: seen.mount_healthy.unwrap_or(true),
            state: seen.state,
            paths,
        })
    }

    /// The files a finished plugin download produced, for import, with
    /// the folders the plugin may download into. Plugins are trusted code,
    /// but what they report is still confined: the landing refuses a path
    /// outside those folders, whatever workspace the plugin names.
    async fn landed_paths(
        &self,
        handle: &SourceHandle,
    ) -> Result<crate::landing::Landed, SourceError> {
        use crate::landing::{Landed, Reported};
        let seen = self
            .host
            .download_inspect(&Self::plugin_handle(handle))
            .await
            .map_err(source_error)?;
        let mut files: Vec<Reported> = seen
            .file_paths
            .iter()
            .map(|path| Reported::path(PathBuf::from(path)))
            .collect();
        if files.is_empty() && !seen.workspace_path.is_empty() {
            files.push(Reported::path(PathBuf::from(&seen.workspace_path)));
        }
        if files.is_empty() {
            tracing::warn!(
                plugin = %self.key,
                "plugin download names no files and no workspace; nothing imported"
            );
        }
        Ok(Landed {
            mounts: self.download_folders(),
            files,
        })
    }

    /// Where this plugin may put finished downloads: its data folder, and
    /// the folder in its `downloads_dir` setting when its manifest
    /// declares one. Read on every landing, so a settings save applies to
    /// the next one. Empty when the plugin is gone.
    fn download_folders(&self) -> Vec<PathBuf> {
        let Some(plugin) = self.host.download_client(&self.key) else {
            return Vec::new();
        };
        let name = &plugin.manifest.name;
        let mut folders = vec![self.host.data_dir(name)];
        if plugin
            .manifest
            .settings
            .iter()
            .any(|field| field.key == "downloads_dir")
        {
            // A value sealed at rest (an import from v2 seals them all) is
            // opened; the folder is never a secret.
            match self
                .host
                .config()
                .get_plugin_masked(name, &plugin.manifest.secret_keys())
            {
                Ok(stored) => {
                    if let Some(dir) = stored
                        .settings
                        .get("downloads_dir")
                        .map(|dir| dir.trim())
                        .filter(|dir| !dir.is_empty())
                    {
                        folders.push(PathBuf::from(dir));
                    }
                }
                Err(error) => {
                    tracing::warn!(plugin = %self.key, %error, "plugin settings unreadable");
                }
            }
        }
        folders
    }

    async fn discard_records(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        self.host
            .download_discard(&Self::plugin_handle(handle))
            .await
            .map_err(source_error)
    }

    async fn abort_job(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        self.host
            .download_abort(&Self::plugin_handle(handle))
            .await
            .map_err(source_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(title: &str, score: f64, tier: &str) -> PluginSearchResult {
        PluginSearchResult {
            title: title.to_owned(),
            size_bytes: 300 * 1024 * 1024,
            score,
            quality_tier: tier.to_owned(),
            ..PluginSearchResult::default()
        }
    }

    #[test]
    fn plugin_scores_rank_inside_the_policy_gates() {
        let mut usenet = UsenetPolicy::v2_defaults();
        usenet.max_size_mb = 500;
        let policy = ReleasePolicy {
            usenet,
            ignored_terms: vec!["Live".to_owned()],
            required_terms: Vec::new(),
        };
        let mut quarantined = release("Artist - Album [quarantined]", 1.0, "lossless");
        quarantined.payload = "bad-one".to_owned();
        let mut huge = release("Artist - Album [huge]", 1.0, "lossless");
        huge.size_bytes = 900 * 1024 * 1024;
        let ranked = rank_plugin_releases(
            vec![
                release("Artist - Album [320]", 0.95, "mp3_320"),
                release("Artist - Album [flac]", 0.80, "lossless"),
                release("Artist - Album (Live)", 1.0, "lossless"),
                release("Artist - Album [maybe]", 0.6, ""),
                release("Artist - Album [nope]", 0.3, "lossless"),
                release("Artist - Album [192]", 0.99, "mp3_192"),
                release("Artist - Album [overscored]", 7.0, ""),
                quarantined,
                huge,
            ],
            &policy,
            &["bad-one".to_owned()],
        );
        let titles: Vec<(&str, Band)> = ranked
            .iter()
            .map(|scored| (scored.release.title.as_str(), scored.band))
            .collect();
        assert_eq!(
            titles,
            [
                ("Artist - Album [flac]", Band::Auto),
                ("Artist - Album [320]", Band::Auto),
                ("Artist - Album [overscored]", Band::Auto),
                ("Artist - Album [maybe]", Band::Manual),
            ]
        );
        assert_eq!(ranked[2].score, 1.0);
    }
}
