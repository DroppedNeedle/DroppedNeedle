//! Production source adapters: slskd and SABnzbd behind the downloads
//! [`DownloadSource`](super::downloads::sources::DownloadSource) seam.
//!
//! Each adapter reads the task row from the shared journal, searches its
//! own side, and enqueues the `candidate_index`-th pick: the index-th peer
//! group for slskd, the index-th release for Usenet. The worker journals
//! the returned handle and polls it; on failover it re-enqueues the same
//! task at the next index, which walks to the next peer or release.
//!
//! Pick quality is simple on purpose (free slots and file counts for
//! slskd; retention, size, and password gates for Usenet). The full v2
//! candidate matcher is not ported yet; the adapters never blocklist on
//! local faults either way.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::dispatch::Journal;
use super::downloads::quarantine::{QUARANTINE_TTL_SECONDS, soulseek_hit_quarantined};
use super::downloads::sources::{
    DownloadSource, Materialization, OrphanOwnership, SourceError, SourceHandle, TransferProgress,
};
use super::downloads::store::TaskRow;
use super::slskd::{EnqueueFile, ReqwestSlskdHttp, SlskdError, SlskdRepository};
use super::usenet::newznab::{IndexerResult, NewznabIndexer};
use super::usenet::policy::UsenetPolicy;
use super::usenet::prowlarr::ProwlarrIndexer;
use super::usenet::sabnzbd::{SabnzbdError, SabnzbdQueue};
use crate::runtime_config::sections::{UsenetBackend, UsenetBackendSetting};

/// Current unix time as float seconds for retention math.
fn now_unix_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

fn slskd_error(error: SlskdError) -> SourceError {
    match &error {
        SlskdError::Auth { .. } => SourceError::Unavailable(error.to_string()),
        SlskdError::Transport(_) | SlskdError::RateLimited | SlskdError::Api { .. } => {
            SourceError::Unavailable(error.to_string())
        }
        SlskdError::Decode(_) => SourceError::Rejected(error.to_string()),
    }
}

fn sab_error(error: SabnzbdError) -> SourceError {
    match &error {
        SabnzbdError::RejectedNzb | SabnzbdError::RejectedNzbUrl | SabnzbdError::MissingNzbUrl => {
            SourceError::Rejected(error.to_string())
        }
        _ => SourceError::Unavailable(error.to_string()),
    }
}

/// Read one task row or fail the fetch as rejected (a fetch for a task
/// that no longer exists must not look like a client outage).
async fn task_row(journal: &Arc<Journal>, task_id: &str) -> Result<TaskRow, SourceError> {
    let owned = task_id.to_owned();
    let lookup = owned.clone();
    Journal::with_store_async(journal, move |store| store.get_task(&lookup))
        .await
        .map_err(SourceError::LocalFault)?
        .ok_or_else(|| SourceError::Rejected(format!("unknown download task {owned}")))
}

/// slskd behind the fetch seam.
pub struct SlskdSource {
    repo: Arc<SlskdRepository<ReqwestSlskdHttp>>,
    journal: Arc<Journal>,
}

impl SlskdSource {
    /// Adapter over a configured repository and the shared journal.
    pub fn new(repo: Arc<SlskdRepository<ReqwestSlskdHttp>>, journal: Arc<Journal>) -> Self {
        Self { repo, journal }
    }

    /// Search hits grouped by peer, best group first: a free upload slot
    /// outranks file count, ties break on username for determinism.
    fn rank_groups(
        hits: &[super::slskd::SearchResult],
    ) -> Vec<(String, Vec<super::slskd::SearchResult>)> {
        let mut groups: HashMap<String, Vec<super::slskd::SearchResult>> = HashMap::new();
        for hit in hits {
            groups
                .entry(hit.username.clone())
                .or_default()
                .push(hit.clone());
        }
        let mut groups: Vec<_> = groups.into_iter().collect();
        groups.sort_by(|a, b| {
            let free_a = a.1.iter().any(|hit| hit.has_free_slot);
            let free_b = b.1.iter().any(|hit| hit.has_free_slot);
            free_b
                .cmp(&free_a)
                .then(b.1.len().cmp(&a.1.len()))
                .then(a.0.cmp(&b.0))
        });
        groups
    }
}

impl DownloadSource for SlskdSource {
    async fn enqueue(
        &self,
        task_id: &str,
        candidate_index: i64,
    ) -> Result<SourceHandle, SourceError> {
        if !self.repo.is_configured() {
            return Err(SourceError::Unavailable("slskd not configured".to_owned()));
        }
        let task = task_row(&self.journal, task_id).await?;
        let hits = if task.download_type == "track" {
            self.repo
                .search_track(&task.artist_name, &task.album_title, None)
                .await
        } else {
            self.repo
                .search_album(&task.artist_name, &task.album_title, None)
                .await
        }
        .map_err(slskd_error)?;
        // Blocklisted peer/file pairs never re-enqueue: a failover that
        // quarantined them walks to the next candidate instead. A journal
        // hiccup reads as an empty set (fail open here; the release simply
        // retries like before).
        let live = Journal::with_store_async(&self.journal, |store| {
            store.load_quarantine_set(now_unix_f64(), QUARANTINE_TTL_SECONDS)
        })
        .await
        .unwrap_or_default();
        let hits: Vec<_> = hits
            .into_iter()
            .filter(|hit| !soulseek_hit_quarantined(&hit.username, &hit.filename, &live))
            .collect();
        let groups = Self::rank_groups(&hits);
        let (_, files) = groups.get(candidate_index.max(0) as usize).ok_or_else(|| {
            SourceError::Rejected(format!(
                "slskd has no candidate {candidate_index} for {task_id}"
            ))
        })?;
        let payload: Vec<EnqueueFile> = files
            .iter()
            .map(|hit| EnqueueFile {
                username: hit.username.clone(),
                filename: hit.filename.clone(),
                size: hit.size,
            })
            .collect();
        let handle = self.repo.enqueue(&payload).await.map_err(slskd_error)?;
        Ok(SourceHandle {
            source: "soulseek".to_owned(),
            username: handle.username,
            filenames: handle.filenames,
            job_name: String::new(),
        })
    }

    async fn poll(&self, handle: &SourceHandle) -> Result<TransferProgress, SourceError> {
        let repo_handle =
            super::slskd::repository::TaskHandle::new(&handle.username, handle.filenames.clone());
        let status = self
            .repo
            .get_status(&repo_handle)
            .await
            .map_err(slskd_error)?;
        let all_terminal = matches!(status.status.as_str(), "completed" | "partial" | "failed");
        Ok(TransferProgress {
            all_terminal,
            all_succeeded: status.status == "completed",
            has_active_transfer: status.has_active_transfer,
            downloaded_bytes: status.bytes_downloaded.max(0) as u64,
            succeeded_filenames: status.succeeded_filenames,
            queue_position_start: status.queue_position_start,
            queue_position_end: status.queue_position_end,
        })
    }

    async fn inspect(&self, handle: &SourceHandle) -> Result<Materialization, SourceError> {
        let repo_handle =
            super::slskd::repository::TaskHandle::new(&handle.username, handle.filenames.clone());
        let status = self
            .repo
            .get_status(&repo_handle)
            .await
            .map_err(slskd_error)?;
        let mut paths = Vec::new();
        for filename in &status.succeeded_filenames {
            match self.repo.get_file_path(&repo_handle, filename, None).await {
                Ok(Some(path)) => paths.push(path),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%error, "slskd inspect path failed");
                }
            }
        }
        Ok(Materialization {
            mount_healthy: true,
            state: if status.status == "completed" {
                "completed".to_owned()
            } else {
                "active".to_owned()
            },
            paths,
        })
    }

    async fn discard(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        let repo_handle =
            super::slskd::repository::TaskHandle::new(&handle.username, handle.filenames.clone());
        self.repo
            .discard_client_artifacts(&repo_handle)
            .await
            .map_err(slskd_error)
    }

    async fn abort(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        let repo_handle =
            super::slskd::repository::TaskHandle::new(&handle.username, handle.filenames.clone());
        self.repo.abort(&repo_handle).await.map_err(slskd_error)
    }
}

/// SABnzbd plus its indexer side behind the fetch seam.
pub struct SabnzbdSource {
    queue: Arc<SabnzbdQueue>,
    newznab: Arc<NewznabIndexer>,
    prowlarr: Arc<ProwlarrIndexer>,
    backend: UsenetBackend,
    policy: UsenetPolicy,
    journal: Arc<Journal>,
    category: Option<String>,
    timeout: Duration,
}

impl SabnzbdSource {
    /// Adapter over a configured queue, the indexer sides, and the journal.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        queue: Arc<SabnzbdQueue>,
        newznab: Arc<NewznabIndexer>,
        prowlarr: Arc<ProwlarrIndexer>,
        backend: UsenetBackendSetting,
        policy: UsenetPolicy,
        journal: Arc<Journal>,
        category: Option<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            queue,
            newznab,
            prowlarr,
            backend: backend.0,
            policy,
            journal,
            category,
            timeout,
        }
    }

    /// Search the active side, then gate: an NZB URL is required, a
    /// positive password flag rejects (aggregators use negative for
    /// unknown, which never rejects), and retention plus size caps apply.
    /// Usenet quarantine consult waits on release identity riding the
    /// handle (failover records job-name rows for audit until then);
    /// soulseek consults the live set at enqueue.
    async fn candidates(&self, task: &TaskRow) -> Vec<IndexerResult> {
        let hits = match self.backend {
            UsenetBackend::Indexers => {
                if task.download_type == "track" {
                    self.newznab
                        .search_track(&task.artist_name, &task.album_title, self.timeout)
                        .await
                } else {
                    self.newznab
                        .search_album(&task.artist_name, &task.album_title, None, self.timeout)
                        .await
                }
            }
            UsenetBackend::Prowlarr => {
                if task.download_type == "track" {
                    self.prowlarr
                        .search_track(&task.artist_name, &task.album_title, self.timeout)
                        .await
                } else {
                    self.prowlarr
                        .search_album(&task.artist_name, &task.album_title, self.timeout)
                        .await
                }
            }
        };
        let now = now_unix_f64();
        hits.into_iter()
            .filter(|hit| !hit.usenet.nzb_url.is_empty())
            .filter(|hit| hit.usenet.password <= 0)
            .filter(|hit| self.policy.within_retention(hit.usenet.usenet_date, now))
            .filter(|hit| self.policy.within_size_cap(hit.usenet.size_bytes))
            .collect()
    }
}

impl DownloadSource for SabnzbdSource {
    async fn enqueue(
        &self,
        task_id: &str,
        candidate_index: i64,
    ) -> Result<SourceHandle, SourceError> {
        if !self.queue.is_configured() {
            return Err(SourceError::Unavailable(
                "sabnzbd not configured".to_owned(),
            ));
        }
        let task = task_row(&self.journal, task_id).await?;
        let index = candidate_index.max(0) as usize;
        // Crash-retry dedup: the job name is deterministic per
        // (task, index), so a repeat enqueue after a crash between the
        // client add and the journal write re-attaches to the existing
        // job instead of adding a twin.
        let job_name = format!("droppedneedle-{task_id}-{index}");
        let probe = super::usenet::sabnzbd::TaskHandle {
            source: "usenet".to_owned(),
            job_name: job_name.clone(),
            nzo_id: String::new(),
        };
        match self.queue.get_status(&probe).await {
            Ok(status) if status.matched_transfers > 0 => {
                return Ok(SourceHandle {
                    source: "usenet".to_owned(),
                    username: String::new(),
                    filenames: Vec::new(),
                    job_name,
                });
            }
            Ok(_) => {}
            Err(SabnzbdError::AmbiguousIdentity) => {
                return Err(SourceError::Rejected(format!(
                    "usenet job {job_name} is ambiguous; refusing a duplicate add"
                )));
            }
            Err(error) => return Err(sab_error(error)),
        }
        let picks = self.candidates(&task).await;
        let pick = picks.get(index).ok_or_else(|| {
            SourceError::Rejected(format!(
                "usenet has no candidate {candidate_index} for {task_id}"
            ))
        })?;
        let category = self.category.as_deref().filter(|cat| *cat != "*");
        // Worker-built job name (v2 strategy): the counter keeps failover
        // attempts distinct on the client and matches the orphan shape.
        let handle = if task.download_type == "track" {
            self.queue
                .enqueue_track(
                    task_id,
                    &job_name,
                    Some(pick.usenet.nzb_url.as_str()),
                    category,
                    None,
                    None,
                )
                .await
        } else {
            self.queue
                .enqueue_album_as(
                    task_id,
                    &job_name,
                    Some(pick.usenet.nzb_url.as_str()),
                    category,
                    None,
                    None,
                )
                .await
        }
        .map_err(sab_error)?;
        Ok(SourceHandle {
            source: "usenet".to_owned(),
            username: String::new(),
            filenames: Vec::new(),
            job_name: handle.job_name,
        })
    }

    async fn poll(&self, handle: &SourceHandle) -> Result<TransferProgress, SourceError> {
        let queue_handle = super::usenet::sabnzbd::TaskHandle {
            source: "usenet".to_owned(),
            job_name: handle.job_name.clone(),
            nzo_id: String::new(),
        };
        let status = self
            .queue
            .get_status(&queue_handle)
            .await
            .map_err(sab_error)?;
        let all_terminal = matches!(status.status.as_str(), "completed" | "failed");
        Ok(TransferProgress {
            all_terminal,
            all_succeeded: status.status == "completed",
            has_active_transfer: status.has_active_transfer,
            downloaded_bytes: status.bytes_downloaded,
            succeeded_filenames: if status.status == "completed" {
                vec![handle.job_name.clone()]
            } else {
                Vec::new()
            },
            queue_position_start: None,
            queue_position_end: None,
        })
    }

    async fn inspect(&self, handle: &SourceHandle) -> Result<Materialization, SourceError> {
        let queue_handle = super::usenet::sabnzbd::TaskHandle {
            source: "usenet".to_owned(),
            job_name: handle.job_name.clone(),
            nzo_id: String::new(),
        };
        let seen = self
            .queue
            .inspect_materialization(&queue_handle)
            .await
            .map_err(sab_error)?;
        let paths = if seen.workspace_path.is_empty() {
            Vec::new()
        } else {
            vec![PathBuf::from(seen.workspace_path)]
        };
        Ok(Materialization {
            mount_healthy: seen.mount_healthy,
            state: seen.state,
            paths,
        })
    }

    async fn discard(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        let queue_handle = super::usenet::sabnzbd::TaskHandle {
            source: "usenet".to_owned(),
            job_name: handle.job_name.clone(),
            nzo_id: String::new(),
        };
        self.queue
            .discard_client_artifacts(&queue_handle)
            .await
            .map_err(sab_error)
    }

    async fn abort(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        let queue_handle = super::usenet::sabnzbd::TaskHandle {
            source: "usenet".to_owned(),
            job_name: handle.job_name.clone(),
            nzo_id: String::new(),
        };
        self.queue.abort(&queue_handle).await.map_err(sab_error)
    }
}

/// Orphan ownership over the journal. Publisher bundles do not exist in v3
/// yet (the library importer will own them), so "none exist" reads as settled
/// and the remaining evidence keeps the verdict fail-closed.
pub struct JournalOwnership {
    journal: Arc<Journal>,
}

impl JournalOwnership {
    /// Ownership lookups over the shared journal.
    pub fn new(journal: Arc<Journal>) -> Self {
        Self { journal }
    }
}

impl OrphanOwnership for JournalOwnership {
    async fn has_cleanup_debt(
        &self,
        source: &str,
        task_id: &str,
        job_name: &str,
    ) -> Result<bool, SourceError> {
        // SABnzbd renames colliding complete-dir entries `<job>.<N>`; the
        // journal keeps the unsuffixed name, so strip the suffix first.
        let journal_name = job_name
            .rsplit_once('.')
            .filter(|(_, suffix)| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
            .map(|(head, _)| head)
            .unwrap_or(job_name)
            .to_owned();
        let source = source.to_owned();
        let task_id = task_id.to_owned();
        Journal::with_store_async(&self.journal, move |store| {
            store.has_cleanup_debt(&source, &task_id, &journal_name)
        })
        .await
        .map_err(SourceError::LocalFault)
    }

    async fn task_status(&self, task_id: &str) -> Result<Option<String>, SourceError> {
        let task_id = task_id.to_owned();
        Journal::with_store_async(&self.journal, move |store| store.get_task(&task_id))
            .await
            .map_err(SourceError::LocalFault)
            .map(|row| row.map(|task| task.status.as_str().to_owned()))
    }

    async fn bundles_settled(&self, _task_id: &str) -> Result<bool, SourceError> {
        Ok(true)
    }
}
