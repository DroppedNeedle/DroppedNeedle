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

use super::dispatch::Journal;
use super::downloads::quarantine::QUARANTINE_TTL_SECONDS;
use super::downloads::sources::{
    DownloadSource, Materialization, SourceError, SourceHandle, TransferProgress,
};
use super::usenet::policy::{QualityTier, UsenetPolicy};
use crate::plugins::capabilities::acquisition::{
    PluginEnqueue, PluginSearchResult, PluginTaskHandle,
};
use crate::plugins::host::PluginHost;
use crate::plugins::runtime::CallError;

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
    journal: Arc<Journal>,
    policy: ReleasePolicy,
}

impl PluginDownloadSource {
    /// Adapter for one `plugin:<name>` source key.
    pub fn new(
        host: Arc<PluginHost>,
        key: String,
        journal: Arc<Journal>,
        policy: ReleasePolicy,
    ) -> Self {
        Self {
            host,
            key,
            journal,
            policy,
        }
    }

    /// The source key (`plugin:<name>`).
    pub fn key(&self) -> &str {
        &self.key
    }

    fn plugin_handle(handle: &SourceHandle) -> PluginTaskHandle {
        PluginTaskHandle {
            source: handle.source.clone(),
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
    async fn enqueue(
        &self,
        task_id: &str,
        candidate_index: i64,
    ) -> Result<SourceHandle, SourceError> {
        let task = self
            .journal
            .read_task(task_id)
            .await
            .map_err(SourceError::LocalFault)?
            .ok_or_else(|| SourceError::Rejected(format!("unknown download task {task_id}")))?;
        let results = if task.download_type == "track" {
            self.host
                .search_track(&self.key, &task.artist_name, &task.album_title, None)
                .await
        } else {
            self.host
                .search_album(&self.key, &task.artist_name, &task.album_title, None, None)
                .await
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs_f64())
            .unwrap_or(0.0);
        let quarantined: Vec<String> = self
            .journal
            .read_quarantine_set(now, QUARANTINE_TTL_SECONDS)
            .await
            .map_err(SourceError::LocalFault)?
            .into_iter()
            .filter(|(source, _)| *source == self.key)
            .map(|(_, identity)| identity)
            .collect();
        let index = candidate_index.max(0) as usize;
        let ranked = rank_plugin_releases(results, &self.policy, &quarantined);
        let pick = ranked
            .into_iter()
            .filter(|release| release.band == Band::Auto)
            .nth(index)
            .ok_or_else(|| {
                SourceError::Rejected(format!(
                    "{} has no automatic candidate {candidate_index} for {task_id}",
                    self.key
                ))
            })?;
        let handle = self
            .host
            .enqueue_download(&PluginEnqueue {
                task_id: task_id.to_owned(),
                source: self.key.clone(),
                files: pick.release.files.clone(),
                payload: pick.release.payload.clone(),
                job_name: format!("droppedneedle-{task_id}-{index}"),
                download_type: task.download_type.clone(),
            })
            .await
            .map_err(source_error)?;
        Ok(SourceHandle {
            source: self.key.clone(),
            username: handle.username,
            filenames: handle.filenames,
            job_name: handle.job_name,
            nzo_id: handle.nzo_id,
            plugin_token: handle.plugin_token,
        })
    }

    async fn poll(&self, handle: &SourceHandle) -> Result<TransferProgress, SourceError> {
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
        })
    }

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

    async fn discard(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        self.host
            .download_discard(&Self::plugin_handle(handle))
            .await
            .map_err(source_error)
    }

    async fn abort(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
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
