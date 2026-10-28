//! Production source adapters: slskd and SABnzbd behind the downloads
//! [`DownloadSource`](super::downloads::sources::DownloadSource) seam.
//!
//! Each adapter reads the task row from the shared journal, asks
//! [`Targets`] what the task fetches (its edition's tracklist; for a single
//! track, the album it is on and the wanted position), searches its own
//! side for the album, and enqueues one pick. slskd takes the best folder
//! against the tracklist (see [`super::slskd::folders`]) that is neither
//! blocklisted nor from a peer already tried for this task, and for a
//! single track enqueues only that track's file. Usenet takes the first
//! album release in tracklist order that is neither blocklisted nor
//! already tried by this task, and records its identity on the handle.
//! The worker journals the returned handle and polls it; on failover it
//! blocklists what the handle fetched and re-enqueues the task, which
//! walks to the next folder or release. Local faults are never
//! blocklisted.
//!
//! A task started from a manual search fetches the candidate the person
//! picked on its first attempt (see [`super::search_jobs`]), even one
//! outside the download settings, and marks that attempt's handle
//! `pinned`; failover then searches as usual. Each adapter also answers a
//! manual search with its ranked candidates, those outside the settings
//! last.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::client_error::ClientError;
use super::downloads::TrackPosition;
use super::downloads::journal::Journal;
use super::downloads::quarantine::Blocklist;
use super::downloads::source_tag::SourceTag;
use super::downloads::sources::{
    DownloadSource, Materialization, OrphanOwnership, SourceError, SourceHandle, TransferProgress,
};
use super::downloads::store::TaskRow;
use super::landing::ports::LandingLibrary;
use super::reason::Reason;
use super::search_jobs::candidates::{Candidate, CandidateRefusal, MAX_PER_SOURCE, Pin};
use super::search_jobs::pinned;
use super::slskd::folders::{RankRequest, rank_listing};
use super::slskd::{EnqueueFile, ReqwestSlskdHttp, SlskdRepository};
use super::target::reasons::TrackReason;
use super::target::releases::order_releases;
use super::target::soulseek::{self, SoulseekMiss};
use super::target::{SearchTarget, TargetError, Targets};
use super::usenet::indexer::{Indexer, IndexerQuery};
use super::usenet::newznab::{IndexerResult, usenet_identity};
use super::usenet::policy::UsenetPolicy;
use super::usenet::sabnzbd::{SabnzbdError, SabnzbdQueue};

/// A refused request or an answer of the wrong shape is the client
/// rejecting this request; a refused key, a missing item, a rate limit or
/// an outage leaves the client unavailable for now.
fn client_error(error: ClientError) -> SourceError {
    match &error {
        ClientError::BadRequest { .. } | ClientError::Decode { .. } => {
            SourceError::rejected(Reason::DownloadFailed, error.to_string())
        }
        ClientError::Auth { .. }
        | ClientError::NotFound { .. }
        | ClientError::RateLimited { .. }
        | ClientError::Unavailable { .. } => SourceError::Unavailable(error.to_string()),
    }
}

fn sab_error(error: SabnzbdError) -> SourceError {
    match error {
        SabnzbdError::Client(error) => client_error(error),
        SabnzbdError::RejectedNzb
        | SabnzbdError::RejectedNzbUrl
        | SabnzbdError::MissingNzbUrl
        | SabnzbdError::NzbTooLarge => {
            SourceError::rejected(Reason::DownloadFailed, error.to_string())
        }
        SabnzbdError::Api(_)
        | SabnzbdError::NzbRefused { .. }
        | SabnzbdError::AmbiguousIdentity => SourceError::Unavailable(error.to_string()),
    }
}

/// One source's live blocklist; a journal that cannot be read is a local
/// fault.
pub(super) async fn blocklist(
    journal: &Journal,
    source: &SourceTag,
) -> Result<Blocklist, SourceError> {
    journal
        .read_blocklist(source, journal.db().now_unix_f64())
        .await
        .map_err(|error| SourceError::LocalFault(error.to_string()))
}

/// The releases a task's attempts on one source already fetched.
pub(super) async fn tried_releases(
    journal: &Journal,
    task_id: &str,
    source: &SourceTag,
) -> Result<HashSet<String>, SourceError> {
    journal
        .read_tried_releases(task_id, source)
        .await
        .map_err(|error| SourceError::LocalFault(error.to_string()))
}

/// Read one task row or fail the fetch as rejected (a fetch for a task
/// that no longer exists must not look like a client outage).
async fn task_row(journal: &Arc<Journal>, task_id: &str) -> Result<TaskRow, SourceError> {
    let owned = task_id.to_owned();
    let lookup = owned.clone();
    journal
        .read_task(&lookup)
        .await
        .map_err(|error| SourceError::LocalFault(error.to_string()))?
        .ok_or_else(|| {
            SourceError::rejected(Reason::NoSource, format!("unknown download task {owned}"))
        })
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

    /// The handle's files, with the mount they must sit in.
    async fn landed_on_mount(
        &self,
        handle: &SourceHandle,
    ) -> Result<crate::landing::Landed, SourceError> {
        Ok(crate::landing::Landed {
            mounts: vec![self.repo.downloads_folder().clone()],
            files: self.locate_files(handle).await?,
        })
    }

    /// Where the handle's files sit on the downloads mount. Located from
    /// the mount alone (no transfer records needed), so a reimport after
    /// the client forgot the transfers still finds them. Files that cannot
    /// be located are left out; the landing reports them missing.
    pub async fn locate_files(
        &self,
        handle: &SourceHandle,
    ) -> Result<Vec<crate::landing::Reported>, SourceError> {
        let repo_handle =
            super::slskd::repository::TaskHandle::new(&handle.username, handle.filenames.clone());
        let mut paths = Vec::new();
        for (index, filename) in handle.filenames.iter().enumerate() {
            let size = handle.sizes.get(index).copied().filter(|size| *size > 0);
            if let Some(path) = self
                .repo
                .get_file_path(&repo_handle, filename, size)
                .await
                .map_err(client_error)?
            {
                // Each located copy keeps the size advertised for it.
                paths.push(crate::landing::Reported {
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

    /// A manual search: the album's folders, ranked against the edition's
    /// tracklist the same way the automatic path ranks them, with folders
    /// outside the quality band last. Blocklisted files are left out.
    async fn search_album(&self, target: &SearchTarget) -> Result<Vec<Candidate>, SourceError> {
        if !self.repo.is_configured() {
            return Ok(Vec::new());
        }
        let blocked = blocklist(&self.journal, &SourceTag::Soulseek).await?;
        let hits: Vec<_> = self
            .repo
            .search_album(&target.artist, &target.album_title, target.year)
            .await
            .map_err(client_error)?
            .into_iter()
            .filter(|hit| !blocked.blocks_file(&hit.username, &hit.filename))
            .collect();
        let request = RankRequest {
            tracklist: &target.tracklist,
            wanted: &[],
            single_track: false,
        };
        Ok(rank_listing(&hits, request, self.repo.policy())
            .into_iter()
            .take(MAX_PER_SOURCE)
            .map(Candidate::from_folder)
            .collect())
    }
}

impl SlskdSource {
    async fn enqueue_pick(
        &self,
        task_id: &str,
        candidate_index: i64,
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
        let blocked = blocklist(&self.journal, &SourceTag::Soulseek).await?;
        let mut tried = Vec::new();
        for json in self
            .journal
            .read_source_handles(task_id, &SourceTag::Soulseek)
            .await
            .map_err(|error| SourceError::LocalFault(error.to_string()))?
        {
            match serde_json::from_str::<SourceHandle>(&json) {
                Ok(handle) => tried.push(handle.username),
                Err(error) => {
                    tracing::warn!(task_id, %error, "stored slskd handle does not decode");
                }
            }
        }
        // After a short landing, the next peer is asked only for the tracks
        // still missing. With the edition's tracklist
        // the folder ranker pairs files to those positions; without one,
        // files are kept by the position their name gives, or kept when it
        // gives none.
        let missing = self
            .journal
            .read("downloads.missing_positions", {
                let task_id = task_id.to_owned();
                move |store| store.missing_positions(&task_id)
            })
            .await
            .map_err(|error| SourceError::LocalFault(error.to_string()))?;
        let mut target = target;
        let refill = target.track.is_none() && !missing.is_empty();
        let missing = if refill {
            missing
        } else if target.track.is_none() && target.tracklist.is_empty() {
            // "Get missing tracks" while MusicBrainz is down: the wanted
            // positions are matched by file name, as a refill does.
            std::mem::take(&mut target.wanted)
                .into_iter()
                .map(|at| (at.disc, at.track))
                .collect()
        } else {
            missing
        };
        let by_name = target.track.is_none() && target.tracklist.is_empty() && !missing.is_empty();
        if refill && !by_name {
            target.wanted = missing
                .iter()
                .map(|&(disc, track)| TrackPosition { disc, track })
                .collect();
        }
        // The folder a person picked from a manual search goes first,
        // minus any file blocklisted since.
        let picked = match pinned(&self.journal, &task, &SourceTag::Soulseek, candidate_index).await
        {
            Some(Pin::Soulseek {
                username, files, ..
            }) => files
                .into_iter()
                .filter(|file| !blocked.blocks_file(&username, &file.filename))
                .map(|file| EnqueueFile {
                    username: username.clone(),
                    filename: file.filename,
                    size: file.size,
                })
                .collect(),
            _ => Vec::new(),
        };
        let pinned_pick = !picked.is_empty();
        let payload: Vec<EnqueueFile> = if picked.is_empty() {
            let keep = |hit: &super::slskd::SearchResult| {
                !blocked.blocks_file(&hit.username, &hit.filename) && !tried.contains(&hit.username)
            };
            let choice = soulseek::choose(self.repo.as_ref(), &target, keep)
                .await
                .map_err(|miss| match miss {
                    SoulseekMiss::Search(error) => client_error(error),
                    SoulseekMiss::Nothing { reason, detail } => {
                        SourceError::rejected(reason, detail)
                    }
                })?;
            if let (Some(reason), Some(targets)) = (choice.lone_reason, self.targets.as_ref()) {
                targets.record_lone_track(&task, reason);
            }
            choice
                .pick
                .files
                .iter()
                .filter(|hit| {
                    !by_name
                        || crate::landing::matching::position_in_name(&hit.filename)
                            .is_none_or(|position| missing.contains(&position))
                })
                .map(|hit| EnqueueFile {
                    username: hit.username.clone(),
                    filename: hit.filename.clone(),
                    size: hit.size,
                })
                .collect()
        } else {
            picked
        };
        if payload.is_empty() {
            return Err(SourceError::rejected(
                Reason::NoSource,
                "slskd's best remaining peer has none of the missing tracks",
            ));
        }
        let advertised: HashMap<&str, i64> = payload
            .iter()
            .map(|file| (file.filename.as_str(), file.size))
            .collect();
        let handle = self.repo.enqueue(&payload).await.map_err(client_error)?;
        let sizes = handle
            .filenames
            .iter()
            .map(|name| advertised.get(name.as_str()).copied().unwrap_or(0))
            .collect();
        Ok(SourceHandle {
            sizes,
            username: handle.username,
            filenames: handle.filenames,
            pinned: pinned_pick,
            ..SourceHandle::new(SourceTag::Soulseek)
        })
    }

    async fn progress(&self, handle: &SourceHandle) -> Result<TransferProgress, SourceError> {
        let repo_handle =
            super::slskd::repository::TaskHandle::new(&handle.username, handle.filenames.clone());
        let status = self
            .repo
            .get_status(&repo_handle)
            .await
            .map_err(client_error)?;
        let all_terminal = matches!(status.status.as_str(), "completed" | "partial" | "failed");
        Ok(TransferProgress {
            all_terminal,
            all_succeeded: status.status == "completed",
            has_active_transfer: status.has_active_transfer,
            downloaded_bytes: status.bytes_downloaded.max(0) as u64,
            succeeded_filenames: status.succeeded_filenames,
            queue_position_start: status.queue_position_start,
            queue_position_end: status.queue_position_end,
            total_bytes: (status.bytes_total > 0).then_some(status.bytes_total as u64),
            files_total: u32::try_from(status.files_total).unwrap_or(u32::MAX),
            files_completed: u32::try_from(status.files_completed).unwrap_or(u32::MAX),
            files_failed: u32::try_from(status.files_failed).unwrap_or(u32::MAX),
        })
    }

    async fn discard_records(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        let repo_handle =
            super::slskd::repository::TaskHandle::new(&handle.username, handle.filenames.clone());
        self.repo
            .discard_client_artifacts(&repo_handle)
            .await
            .map_err(client_error)
    }

    async fn abort_job(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        let repo_handle =
            super::slskd::repository::TaskHandle::new(&handle.username, handle.filenames.clone());
        self.repo.abort(&repo_handle).await.map_err(client_error)
    }
}

impl DownloadSource for SlskdSource {
    fn tag(&self) -> SourceTag {
        SourceTag::Soulseek
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
        Box::pin(self.landed_on_mount(handle))
    }

    fn discard<'a>(&'a self, handle: &'a SourceHandle) -> BoxFuture<'a, Result<bool, SourceError>> {
        Box::pin(self.discard_records(handle))
    }

    fn abort<'a>(&'a self, handle: &'a SourceHandle) -> BoxFuture<'a, Result<bool, SourceError>> {
        Box::pin(self.abort_job(handle))
    }
}

/// The queue's correlation handle for one journaled handle.
fn queue_handle(handle: &SourceHandle) -> super::usenet::sabnzbd::TaskHandle {
    super::usenet::sabnzbd::TaskHandle {
        source: SourceTag::Usenet.to_string(),
        job_name: handle.job_name.clone(),
        nzo_id: handle.nzo_id.clone(),
    }
}

/// SABnzbd plus the indexers it fetches from, behind the fetch seam.
pub struct SabnzbdSource {
    queue: Arc<SabnzbdQueue>,
    indexers: Vec<Arc<dyn Indexer>>,
    policy: UsenetPolicy,
    journal: Arc<Journal>,
    category: Option<String>,
    targets: Option<Arc<Targets>>,
}

impl SabnzbdSource {
    /// Adapter over a configured queue, the indexers it searches (in
    /// order: the first copy of a release wins), and the journal.
    pub fn new(
        queue: Arc<SabnzbdQueue>,
        indexers: Vec<Arc<dyn Indexer>>,
        policy: UsenetPolicy,
        journal: Arc<Journal>,
        category: Option<String>,
    ) -> Self {
        Self {
            queue,
            indexers,
            policy,
            journal,
            category,
            targets: None,
        }
    }

    /// Search each task's album (a single track is fetched in its album's
    /// release) and order releases against the edition's tracklist.
    pub fn with_targets(mut self, targets: Arc<Targets>) -> Self {
        self.targets = Some(targets);
        self
    }

    /// The releases an automatic pick may take: [`Self::listed`] minus
    /// those outside the download settings.
    async fn candidates(&self, query: &IndexerQuery<'_>) -> Vec<IndexerResult> {
        self.listed(query)
            .await
            .into_iter()
            .filter(|hit| self.refusals(hit).is_empty())
            .collect()
    }

    /// Search every indexer and drop what cannot be fetched: an NZB URL is
    /// required, a positive password flag drops (aggregators use negative
    /// for unknown, which never drops), and so does a release past the
    /// provider's retention. Album results are ordered against the
    /// target's tracklist. Blocklisted releases are left to the caller,
    /// which needs the count found before them.
    async fn listed(&self, query: &IndexerQuery<'_>) -> Vec<IndexerResult> {
        let found = futures_util::future::join_all(
            self.indexers.iter().map(|indexer| indexer.search(query)),
        )
        .await;
        // The first copy of a release wins.
        let mut seen = std::collections::HashSet::new();
        let now = self.journal.db().now_unix_f64();
        let hits = found
            .into_iter()
            .flatten()
            .filter(|hit| seen.insert(usenet_identity(&hit.usenet.title, hit.usenet.size_bytes)))
            .filter(|hit| !hit.usenet.nzb_url.is_empty())
            .filter(|hit| hit.usenet.password <= 0)
            .filter(|hit| self.policy.within_retention(hit.usenet.usenet_date, now))
            .collect();
        match query {
            IndexerQuery::Album(target) => order_releases(hits, target),
            IndexerQuery::Track { .. } => hits,
        }
    }

    /// How a release is outside the download settings: a category that
    /// names a tier outside the band, or a size over the cap.
    fn refusals(&self, hit: &IndexerResult) -> Vec<CandidateRefusal> {
        let mut refusal = Vec::new();
        if !self.policy.accepts_categories(&hit.usenet.category_ids) {
            refusal.push(CandidateRefusal::QualityOutsideRange);
        }
        if !self.policy.within_size_cap(hit.usenet.size_bytes) {
            refusal.push(CandidateRefusal::OverSizeCap);
        }
        refusal
    }

    /// The best release for a task that is neither blocklisted nor already
    /// tried by it. Usenet has no folder search, so a single track is
    /// fetched as part of an album release; only when every album release
    /// is used up does the walk go on to releases found for the track
    /// alone, with the reason recorded.
    async fn pick(
        &self,
        task: &TaskRow,
        target: &SearchTarget,
        blocked: &Blocklist,
        tried: &HashSet<String>,
    ) -> Option<IndexerResult> {
        let usable = |hit: &IndexerResult| {
            let identity = usenet_identity(&hit.usenet.title, hit.usenet.size_bytes);
            !blocked.blocks(&identity) && !tried.contains(&identity)
        };
        let albums = if target.has_album() {
            self.candidates(&IndexerQuery::Album(target)).await
        } else {
            Vec::new()
        };
        let album_count = albums.len();
        if let Some(pick) = albums.into_iter().find(usable) {
            return Some(pick);
        }
        let track = target.track.as_ref()?;
        let lone = self
            .candidates(&IndexerQuery::Track {
                artist: &track.artist,
                title: &track.title,
            })
            .await
            .into_iter()
            .find(usable)?;
        let reason = match (track.no_album, album_count) {
            (Some(reason), _) => reason,
            (None, 0) => TrackReason::NoAlbumRelease,
            (None, _) => TrackReason::AlbumReleasesExhausted,
        };
        if let Some(targets) = self.targets.as_ref() {
            targets.record_lone_track(task, reason);
        }
        Some(lone)
    }
}

impl SabnzbdSource {
    /// The finished job's folder, with the mount it must sit in.
    async fn landed_in_mount(
        &self,
        handle: &SourceHandle,
    ) -> Result<crate::landing::Landed, SourceError> {
        let seen = self.inspect(handle).await?;
        Ok(crate::landing::Landed {
            mounts: vec![self.queue.downloads_mount().to_path_buf().into()],
            files: seen
                .paths
                .into_iter()
                .map(crate::landing::Reported::path)
                .collect(),
        })
    }

    /// A manual search: the album's releases from every indexer, ordered
    /// as the automatic path orders them, with those outside the download
    /// settings after the rest. Blocklisted releases are left out.
    async fn search_album(&self, target: &SearchTarget) -> Result<Vec<Candidate>, SourceError> {
        if !self.queue.is_configured() || !target.has_album() {
            return Ok(Vec::new());
        }
        let blocked = blocklist(&self.journal, &SourceTag::Usenet).await?;
        let (mut listing, refused): (Vec<Candidate>, Vec<Candidate>) = self
            .unblocked_albums(target, &blocked)
            .await
            .iter()
            .map(|hit| Candidate::from_release(hit, target, self.refusals(hit)))
            .partition(|candidate| !candidate.is_refused());
        listing.extend(refused);
        listing.truncate(MAX_PER_SOURCE);
        Ok(listing)
    }

    /// The album's releases minus the blocklisted ones, including those
    /// outside the download settings (a person may pick one).
    async fn unblocked_albums(
        &self,
        target: &SearchTarget,
        blocked: &Blocklist,
    ) -> Vec<IndexerResult> {
        self.listed(&IndexerQuery::Album(target))
            .await
            .into_iter()
            .filter(|hit| {
                !blocked.blocks(&usenet_identity(&hit.usenet.title, hit.usenet.size_bytes))
            })
            .collect()
    }

    /// Whether SABnzbd still lists the job in its queue or history (orphan
    /// evidence). A job in neither is gone.
    async fn job_present(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
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

impl SabnzbdSource {
    async fn enqueue_pick(
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
        let pin = pinned(&self.journal, &task, &SourceTag::Usenet, candidate_index).await;
        // Crash-retry dedup: the job name is deterministic per
        // (task, index), so a repeat enqueue after a crash between the
        // client add and the journal write re-attaches to the existing
        // job instead of adding a twin. With a pick, that job can only
        // be the picked release: a pick that is not found adds nothing.
        let job_name = format!("droppedneedle-{task_id}-{index}");
        let probe = super::usenet::sabnzbd::TaskHandle {
            source: SourceTag::Usenet.to_string(),
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
                    job_name,
                    nzo_id,
                    pinned: matches!(pin, Some(Pin::Release { .. })),
                    ..SourceHandle::new(SourceTag::Usenet)
                });
            }
            Ok(_) => {}
            Err(SabnzbdError::AmbiguousIdentity) => {
                return Err(SourceError::rejected(
                    Reason::DownloadFailed,
                    format!("usenet job {job_name} is ambiguous; refusing a duplicate add"),
                ));
            }
            Err(error) => return Err(sab_error(error)),
        }
        let target = resolve_target(self.targets.as_ref(), &task).await?;
        let blocked = blocklist(&self.journal, &SourceTag::Usenet).await?;
        let pinned_pick = matches!(pin, Some(Pin::Release { .. }));
        let pick = match pin {
            // The release a person picked, found again by its identity (even
            // outside the download settings: they judged it): the NZB link
            // is never stored.
            Some(Pin::Release { identity }) => self
                .unblocked_albums(&target, &blocked)
                .await
                .into_iter()
                .find(|hit| usenet_identity(&hit.usenet.title, hit.usenet.size_bytes) == identity)
                .ok_or_else(|| {
                    SourceError::rejected(
                        Reason::NoSource,
                        "The Usenet release you picked is no longer listed by your indexers.",
                    )
                })?,
            _ => {
                let tried = tried_releases(&self.journal, task_id, &SourceTag::Usenet).await?;
                self.pick(&task, &target, &blocked, &tried)
                    .await
                    .ok_or_else(|| {
                        SourceError::rejected(
                            Reason::NoSource,
                            format!("usenet has no candidate {candidate_index} for {task_id}"),
                        )
                    })?
            }
        };
        let category = self.category.as_deref().filter(|cat| *cat != "*");
        // Worker-built job name: the counter keeps failover
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
            job_name: handle.job_name,
            nzo_id: handle.nzo_id,
            release_identity: usenet_identity(&pick.usenet.title, pick.usenet.size_bytes),
            pinned: pinned_pick,
            ..SourceHandle::new(SourceTag::Usenet)
        })
    }

    async fn progress(&self, handle: &SourceHandle) -> Result<TransferProgress, SourceError> {
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
            total_bytes: (status.bytes_total > 0).then_some(status.bytes_total),
            files_total: status.files_total,
            files_completed: status.files_completed,
            files_failed: u32::from(status.status == "failed"),
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

    async fn discard_records(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        let queue_handle = queue_handle(handle);
        self.queue
            .discard_client_artifacts(&queue_handle)
            .await
            .map_err(sab_error)
    }

    async fn abort_job(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        let queue_handle = queue_handle(handle);
        self.queue.abort(&queue_handle).await.map_err(sab_error)
    }
}

impl DownloadSource for SabnzbdSource {
    fn tag(&self) -> SourceTag {
        SourceTag::Usenet
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
        Box::pin(self.landed_in_mount(handle))
    }

    fn discard<'a>(&'a self, handle: &'a SourceHandle) -> BoxFuture<'a, Result<bool, SourceError>> {
        Box::pin(self.discard_records(handle))
    }

    fn abort<'a>(&'a self, handle: &'a SourceHandle) -> BoxFuture<'a, Result<bool, SourceError>> {
        Box::pin(self.abort_job(handle))
    }

    fn job_active<'a>(&'a self, handle: &'a SourceHandle) -> BoxFuture<'a, Option<bool>> {
        Box::pin(async move { self.job_present(handle).await.ok() })
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

/// Orphan ownership over the download journal, plus the library for the
/// state of the writes that put a task's files in place.
pub struct JournalOwnership {
    journal: Arc<Journal>,
    library: Option<Arc<dyn LandingLibrary>>,
}

impl JournalOwnership {
    /// Ownership lookups over the shared journal, asking `library` about
    /// the writes behind a task. Without a library no task's writes count
    /// as settled.
    pub fn new(journal: Arc<Journal>, library: Option<Arc<dyn LandingLibrary>>) -> Self {
        Self { journal, library }
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
            .map_err(|error| SourceError::LocalFault(error.to_string()))
    }

    async fn task_status(&self, task_id: &str) -> Result<Option<String>, SourceError> {
        let task_id = task_id.to_owned();
        self.journal
            .read_task(&task_id)
            .await
            .map_err(|error| SourceError::LocalFault(error.to_string()))
            .map(|row| row.map(|task| task.status.as_str().to_owned()))
    }

    /// A journal that cannot be asked is not settled: the folder stays.
    async fn bundles_settled(&self, task_id: &str) -> Result<bool, SourceError> {
        let Some(library) = &self.library else {
            tracing::warn!(
                task_id,
                "no library to ask about this download's writes; its folder is kept"
            );
            return Ok(false);
        };
        match library.publish_settled(task_id).await {
            Ok(settled) => Ok(settled),
            Err(error) => {
                tracing::warn!(
                    task_id,
                    %error,
                    "publish journal unreadable; the download folder is kept"
                );
                Ok(false)
            }
        }
    }
}
