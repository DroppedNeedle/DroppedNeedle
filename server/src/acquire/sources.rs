//! Production source adapters: slskd and SABnzbd behind the downloads
//! [`DownloadSource`](super::downloads::sources::DownloadSource) seam.
//!
//! Each adapter reads the task row from the shared journal, asks
//! [`Targets`] what the task fetches (its edition's tracklist; for a single
//! track, the album it is on and the wanted position), searches its own
//! side for the album, and enqueues one pick. slskd takes the best folder
//! against the tracklist (see [`super::slskd::folders`]) that is neither
//! blocklisted nor from a peer already tried for this task, and for a
//! single track enqueues only that track's file. Usenet takes the
//! `candidate_index`-th album release in tracklist order, where the index
//! counts this source's own earlier attempts. The worker journals the
//! returned handle and polls it; on failover it re-enqueues the task,
//! which walks to the next folder or release. The adapters never
//! blocklist on local faults.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::dispatch::Journal;
use super::downloads::TrackPosition;
use super::downloads::quarantine::{QUARANTINE_TTL_SECONDS, soulseek_hit_quarantined};
use super::downloads::sources::{
    DownloadSource, Materialization, OrphanOwnership, SourceError, SourceHandle, TransferProgress,
};
use super::downloads::store::TaskRow;
use super::slskd::{EnqueueFile, ReqwestSlskdHttp, SlskdError, SlskdRepository};
use super::target::reasons::TrackReason;
use super::target::releases::order_releases;
use super::target::soulseek::{self, SoulseekMiss};
use super::target::{SearchTarget, TargetError, Targets};
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
    journal
        .read_task(&lookup)
        .await
        .map_err(SourceError::LocalFault)?
        .ok_or_else(|| SourceError::Rejected(format!("unknown download task {owned}")))
}

/// The task's search target, or why it cannot be worked out yet. Without
/// a target service an album still searches by its names; a single track
/// waits, since searching for it alone is what this design avoids.
pub(super) async fn resolve_target(
    targets: Option<&Arc<Targets>>,
    task: &TaskRow,
) -> Result<SearchTarget, SourceError> {
    match targets {
        Some(targets) => targets.target(task).await.map_err(|error| match error {
            TargetError::Unavailable(reason) => SourceError::Unavailable(reason.to_string()),
            TargetError::LocalFault(detail) => SourceError::LocalFault(detail),
        }),
        None if task.download_type == "track" => Err(SourceError::Unavailable(
            TrackReason::AlbumLookupUnavailable.to_string(),
        )),
        None => Ok(SearchTarget {
            artist: task.artist_name.clone(),
            album_title: task.album_title.clone(),
            year: None,
            tracklist: Vec::new(),
            wanted: Vec::new(),
            track: None,
        }),
    }
}

/// slskd behind the fetch seam.
pub struct SlskdSource {
    repo: Arc<SlskdRepository<ReqwestSlskdHttp>>,
    journal: Arc<Journal>,
    targets: Option<Arc<Targets>>,
}

impl SlskdSource {
    /// Adapter over a configured repository and the shared journal.
    pub fn new(repo: Arc<SlskdRepository<ReqwestSlskdHttp>>, journal: Arc<Journal>) -> Self {
        Self {
            repo,
            journal,
            targets: None,
        }
    }

    /// Where the handle's files sit on the downloads mount. Located from
    /// the mount alone (no transfer records needed), so a reimport after
    /// the client forgot the transfers still finds them. Files that cannot
    /// be located are left out; the landing reports them missing.
    pub async fn locate_files(
        &self,
        handle: &SourceHandle,
    ) -> Result<Vec<crate::acquire::landing::Reported>, SourceError> {
        let repo_handle =
            super::slskd::repository::TaskHandle::new(&handle.username, handle.filenames.clone());
        let mut paths = Vec::new();
        for (index, filename) in handle.filenames.iter().enumerate() {
            let size = handle.sizes.get(index).copied().filter(|size| *size > 0);
            if let Some(path) = self
                .repo
                .get_file_path(&repo_handle, filename, size)
                .await
                .map_err(slskd_error)?
            {
                // Each located copy keeps the size advertised for it.
                paths.push(crate::acquire::landing::Reported {
                    path,
                    size: size.and_then(|size| u64::try_from(size).ok()),
                });
            }
        }
        Ok(paths)
    }

    /// Rank against each task's edition and fetch single tracks as part
    /// of their album.
    pub fn with_targets(mut self, targets: Arc<Targets>) -> Self {
        self.targets = Some(targets);
        self
    }
}

impl DownloadSource for SlskdSource {
    async fn enqueue(
        &self,
        task_id: &str,
        _candidate_index: i64,
    ) -> Result<SourceHandle, SourceError> {
        if !self.repo.is_configured() {
            return Err(SourceError::Unavailable("slskd not configured".to_owned()));
        }
        let task = task_row(&self.journal, task_id).await?;
        let target = resolve_target(self.targets.as_ref(), &task).await?;
        // Blocklisted peer/file pairs never re-enqueue, and neither does a
        // peer this task already tried: a failover walks to the best
        // remaining folder, so the list shrinking under quarantine never
        // skips a candidate.
        let live = self
            .journal
            .read_quarantine_set(now_unix_f64(), QUARANTINE_TTL_SECONDS)
            .await
            .map_err(SourceError::LocalFault)?;
        let mut tried = Vec::new();
        for json in self
            .journal
            .read_source_handles(task_id, "soulseek")
            .await
            .map_err(SourceError::LocalFault)?
        {
            match serde_json::from_str::<SourceHandle>(&json) {
                Ok(handle) => tried.push(handle.username),
                Err(error) => {
                    tracing::warn!(task_id, %error, "stored slskd handle does not decode");
                }
            }
        }
        // After a short landing, the next peer is asked only for the tracks
        // still missing (v2 per-file failover). With the edition's tracklist
        // the folder ranker pairs files to those positions; without one,
        // files are kept by the position their name gives, or kept when it
        // gives none.
        let missing = self
            .journal
            .run("downloads.missing_positions", {
                let task_id = task_id.to_owned();
                move |store| store.missing_positions(&task_id)
            })
            .await
            .map_err(SourceError::LocalFault)?;
        let mut target = target;
        let refill = target.track.is_none() && !missing.is_empty();
        let by_name = refill && target.tracklist.is_empty();
        if refill && !by_name {
            target.wanted = missing
                .iter()
                .map(|&(disc, track)| TrackPosition { disc, track })
                .collect();
        }
        let keep = |hit: &super::slskd::SearchResult| {
            !soulseek_hit_quarantined(&hit.username, &hit.filename, &live)
                && !tried.contains(&hit.username)
        };
        let choice = soulseek::choose(self.repo.as_ref(), &target, keep)
            .await
            .map_err(|miss| match miss {
                SoulseekMiss::Search(error) => slskd_error(error),
                SoulseekMiss::Nothing(reason) => SourceError::Rejected(reason),
            })?;
        if let (Some(reason), Some(targets)) = (choice.lone_reason, self.targets.as_ref()) {
            targets.record_lone_track(&task, reason).await;
        }
        let payload: Vec<EnqueueFile> = choice
            .pick
            .files
            .iter()
            .filter(|hit| {
                !by_name
                    || crate::acquire::landing::matching::position_in_name(&hit.filename)
                        .is_none_or(|position| missing.contains(&position))
            })
            .map(|hit| EnqueueFile {
                username: hit.username.clone(),
                filename: hit.filename.clone(),
                size: hit.size,
            })
            .collect();
        if payload.is_empty() {
            return Err(SourceError::Rejected(
                "slskd's best remaining peer has none of the missing tracks".to_owned(),
            ));
        }
        let advertised: HashMap<&str, i64> = payload
            .iter()
            .map(|file| (file.filename.as_str(), file.size))
            .collect();
        let handle = self.repo.enqueue(&payload).await.map_err(slskd_error)?;
        let sizes = handle
            .filenames
            .iter()
            .map(|name| advertised.get(name.as_str()).copied().unwrap_or(0))
            .collect();
        Ok(SourceHandle {
            sizes,
            source: "soulseek".to_owned(),
            username: handle.username,
            filenames: handle.filenames,
            job_name: String::new(),
            nzo_id: String::new(),
            plugin_token: String::new(),
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

/// The queue's correlation handle for one journaled handle.
fn queue_handle(handle: &SourceHandle) -> super::usenet::sabnzbd::TaskHandle {
    super::usenet::sabnzbd::TaskHandle {
        source: "usenet".to_owned(),
        job_name: handle.job_name.clone(),
        nzo_id: handle.nzo_id.clone(),
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
    plugins: super::wiring::PluginSlot,
    targets: Option<Arc<Targets>>,
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
            plugins: Default::default(),
            targets: None,
        }
    }

    /// Search each task's album (a single track is fetched in its album's
    /// release) and order releases against the edition's tracklist.
    pub fn with_targets(mut self, targets: Arc<Targets>) -> Self {
        self.targets = Some(targets);
        self
    }

    /// Pool releases from plugin indexers that target `usenet`.
    pub fn with_plugins(mut self, plugins: super::wiring::PluginSlot) -> Self {
        self.plugins = plugins;
        self
    }

    /// Releases from plugin indexers that target `usenet`, as NZB hits.
    /// Results without an NZB URL cannot go to SABnzbd and are skipped.
    async fn plugin_hits(&self, query: &Query<'_>) -> Vec<IndexerResult> {
        let Some(host) = self.plugins.get() else {
            return Vec::new();
        };
        let found = match query {
            Query::Track { artist, title } => {
                host.search_track("usenet", artist, title, None).await
            }
            Query::Album(target) => {
                host.search_album(
                    "usenet",
                    &target.artist,
                    &target.album_title,
                    target.year.map(i64::from),
                    i64::try_from(target.tracklist.len())
                        .ok()
                        .filter(|count| *count > 0),
                )
                .await
            }
        };
        found
            .into_iter()
            .filter(|hit| hit.nzb_url.starts_with("https://") || hit.nzb_url.starts_with("http://"))
            .map(|hit| IndexerResult {
                source: "usenet".to_owned(),
                usenet: super::usenet::newznab::UsenetRelease {
                    indexer_id: "plugin".to_owned(),
                    indexer_name: "plugin".to_owned(),
                    guid: hit.payload,
                    title: hit.title,
                    nzb_url: hit.nzb_url,
                    size_bytes: hit.size_bytes.max(0) as u64,
                    category_ids: Vec::new(),
                    grabs: None,
                    files: None,
                    usenet_date: hit.usenet_date,
                    password: 0,
                },
            })
            .collect()
    }

    /// Search the active side, then gate: an NZB URL is required, a
    /// positive password flag rejects (aggregators use negative for
    /// unknown, which never rejects), and retention plus size caps apply.
    /// Album results are ordered against the target's tracklist.
    /// Usenet quarantine consult waits on release identity riding the
    /// handle (failover records job-name rows for audit until then);
    /// soulseek consults the live set at enqueue.
    async fn candidates(&self, query: &Query<'_>) -> Vec<IndexerResult> {
        let hits = match (self.backend, query) {
            (UsenetBackend::Indexers, Query::Track { artist, title }) => {
                self.newznab.search_track(artist, title, self.timeout).await
            }
            (UsenetBackend::Indexers, Query::Album(target)) => {
                self.newznab
                    .search_album(
                        &target.artist,
                        &target.album_title,
                        target.year,
                        self.timeout,
                    )
                    .await
            }
            (UsenetBackend::Prowlarr, Query::Track { artist, title }) => {
                self.prowlarr
                    .search_track(artist, title, self.timeout)
                    .await
            }
            (UsenetBackend::Prowlarr, Query::Album(target)) => {
                self.prowlarr
                    .search_album(&target.artist, &target.album_title, self.timeout)
                    .await
            }
        };
        // Pool plugin releases after the configured side; the first copy
        // of a release wins (v2's composite indexer rule).
        let mut seen = std::collections::HashSet::new();
        let hits: Vec<IndexerResult> = hits
            .into_iter()
            .chain(self.plugin_hits(query).await)
            .filter(|hit| {
                seen.insert(super::usenet::newznab::usenet_identity(
                    &hit.usenet.title,
                    hit.usenet.size_bytes,
                ))
            })
            .collect();
        let now = now_unix_f64();
        let hits = hits
            .into_iter()
            .filter(|hit| !hit.usenet.nzb_url.is_empty())
            .filter(|hit| hit.usenet.password <= 0)
            .filter(|hit| self.policy.within_retention(hit.usenet.usenet_date, now))
            .filter(|hit| self.policy.within_size_cap(hit.usenet.size_bytes))
            .collect();
        match query {
            Query::Album(target) => order_releases(hits, target),
            Query::Track { .. } => hits,
        }
    }

    /// The `index`-th release for a task. Usenet has no folder search, so
    /// a single track is fetched as part of an album release; only when
    /// every album release is used up does the walk go on to releases
    /// found for the track alone, with the reason recorded.
    async fn pick(
        &self,
        task: &TaskRow,
        target: &SearchTarget,
        index: usize,
    ) -> Option<IndexerResult> {
        let albums = if target.has_album() {
            self.candidates(&Query::Album(target)).await
        } else {
            Vec::new()
        };
        let album_count = albums.len();
        if let Some(pick) = albums.into_iter().nth(index) {
            return Some(pick);
        }
        let track = target.track.as_ref()?;
        let lone = self
            .candidates(&Query::Track {
                artist: &track.artist,
                title: &track.title,
            })
            .await
            .into_iter()
            .nth(index - album_count)?;
        let reason = match (track.no_album, album_count) {
            (Some(reason), _) => reason,
            (None, 0) => TrackReason::NoAlbumRelease,
            (None, _) => TrackReason::AlbumReleasesExhausted,
        };
        if let Some(targets) = self.targets.as_ref() {
            targets.record_lone_track(task, reason).await;
        }
        Some(lone)
    }
}

/// What one Usenet search asks for.
enum Query<'a> {
    /// The target's album.
    Album(&'a SearchTarget),
    /// A single track on its own (the last resort).
    Track { artist: &'a str, title: &'a str },
}

impl SabnzbdSource {
    /// Whether SABnzbd still lists the job in its queue or history (orphan
    /// evidence). A job in neither is gone.
    pub async fn job_present(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        let status = self
            .queue
            .get_status(&queue_handle(handle))
            .await
            .map_err(sab_error)?;
        Ok(status.matched_transfers > 0
            && status.status != "completed"
            && status.status != "failed")
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
                let nzo_id = self
                    .queue
                    .inspect_materialization(&probe)
                    .await
                    .map(|seen| seen.nzo_id)
                    .unwrap_or_default();
                return Ok(SourceHandle {
                    source: "usenet".to_owned(),
                    username: String::new(),
                    filenames: Vec::new(),
                    job_name,
                    nzo_id,
                    plugin_token: String::new(),
                    sizes: Vec::new(),
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
        let target = resolve_target(self.targets.as_ref(), &task).await?;
        let pick = self.pick(&task, &target, index).await.ok_or_else(|| {
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
            nzo_id: handle.nzo_id,
            plugin_token: String::new(),
            sizes: Vec::new(),
        })
    }

    async fn poll(&self, handle: &SourceHandle) -> Result<TransferProgress, SourceError> {
        let queue_handle = queue_handle(handle);
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
        let queue_handle = queue_handle(handle);
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
        let queue_handle = queue_handle(handle);
        self.queue
            .discard_client_artifacts(&queue_handle)
            .await
            .map_err(sab_error)
    }

    async fn abort(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        let queue_handle = queue_handle(handle);
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
        self.journal
            .read_cleanup_debt(&source, &task_id, &journal_name)
            .await
            .map_err(SourceError::LocalFault)
    }

    async fn task_status(&self, task_id: &str) -> Result<Option<String>, SourceError> {
        let task_id = task_id.to_owned();
        self.journal
            .read_task(&task_id)
            .await
            .map_err(SourceError::LocalFault)
            .map(|row| row.map(|task| task.status.as_str().to_owned()))
    }

    async fn bundles_settled(&self, _task_id: &str) -> Result<bool, SourceError> {
        Ok(true)
    }
}
