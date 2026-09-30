//! Seam implementations between the library slices (integrator-owned).
//!
//! Each adapter is narrow by design: it translates at a slice
//! boundary and owns no domain logic. Gaps that need a future slice
//! (AcoustID key config, album projection for contributions) degrade
//! loudly or dormantly, never silently wrong; each carries a note.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::future::BoxFuture;

use super::contrib::error::ContribError;
use super::contrib::models::{
    ContributionRecord, MusicBrainzUrlResolution, MusicBrainzVerifiedRelease,
};
use super::contrib::seams::{
    AlbumIdentificationContext, AttachmentCandidate, AttachmentDecision, AttachmentEvidence,
    AttachmentOutcome, ContributionCatalog, ContributionIdentity, DuplicateSearchFacts,
    MusicBrainzContrib, ProviderFailure, UrlRelation,
};
use super::identify::models::IdentifyKind;
use super::identify::stores::QueueStore;
use super::publish::PublishError;
use super::publish::planner::SpaceProbe;
use super::scan::seams::{IdentifyQueue, ScannedTags, TagReadError, TagReader};
use crate::ids::IdGenerator;

// ---------------------------------------------------------------------------
// Scan -> tags: read-only tag access over the tags slice.
// ---------------------------------------------------------------------------

/// Read-only tag reader over [`crate::library::tags`]. Never writes:
/// tag text comes from the read half, duration from the probe half,
/// and every failure maps onto the scan seam's two errors.
pub struct LoftyTagReader;

impl TagReader for LoftyTagReader {
    fn read_tags(&self, path: &Path) -> Result<ScannedTags, TagReadError> {
        let format = super::tags::format_for_path(path).map_err(|_| TagReadError::Fatal)?;
        let tag = super::tags::read::read_tag_only(path, format).map_err(map_tag_read_error)?;
        let duration_secs = super::tags::probe(path)
            .ok()
            .map(|info| info.duration_seconds);
        let mut extra = HashMap::new();
        if !tag.genres.is_empty() {
            extra.insert("genre".to_owned(), tag.genres.join("\u{0}"));
        }
        if let Some(mbid) = tag.musicbrainz_recording_id.as_deref() {
            extra.insert("musicbrainz_recording_id".to_owned(), mbid.to_owned());
        }
        Ok(ScannedTags {
            artist: non_empty(tag.artist),
            album: non_empty(tag.album),
            title: non_empty(tag.title),
            duration_secs,
            extra,
        })
    }
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

/// Map a tag failure onto the scan seam. I/O races (a file vanishing
/// mid-read, contended disk) defer with re-offer; unrecognized and
/// unparseable files are fatal for this file.
fn map_tag_read_error(error: super::tags::TagsError) -> TagReadError {
    match error {
        super::tags::TagsError::Io { .. } => TagReadError::Deferred,
        _ => TagReadError::Fatal,
    }
}

// ---------------------------------------------------------------------------
// Scan -> identify: fire-and-forget enqueue plus the track/album index.
// ---------------------------------------------------------------------------

/// Track-to-album index fed by scan enqueue. The identify stores key
/// everything by album while management plans by track; this map is
/// the join until a durable catalog owns it.
#[derive(Debug, Default)]
pub struct TrackAlbumMap {
    inner: Mutex<HashMap<String, String>>,
}

impl TrackAlbumMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, album_key: &str, track_ids: &[String]) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for track_id in track_ids {
            inner.insert(track_id.clone(), album_key.to_owned());
        }
    }

    pub fn album_for_track(&self, track_id: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(track_id)
            .cloned()
    }
}

/// Fire-and-forget identify enqueue over an [`IdentifyService`]-shaped
/// queue. Scan offers album keys with fresh track ids; the adapter
/// records the track/album join and enqueues one automatic job per
/// album. Facts are filled later from the scan catalog plus disk tag
/// reads (see the identify loop), so enqueue stays synchronous.
pub struct IdentifyEnqueue {
    queue: Arc<dyn QueueStore>,
    map: Arc<TrackAlbumMap>,
    ids: Arc<dyn IdGenerator>,
}

impl IdentifyEnqueue {
    pub fn new(
        queue: Arc<dyn QueueStore>,
        map: Arc<TrackAlbumMap>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self { queue, map, ids }
    }

    pub fn map(&self) -> &Arc<TrackAlbumMap> {
        &self.map
    }
}

impl IdentifyQueue for IdentifyEnqueue {
    fn enqueue(&self, album_key: &str, track_ids: &[String]) -> usize {
        self.map.record(album_key, track_ids);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|span| span.as_millis() as u64)
            .unwrap_or(0);
        // Duplicate offers across runs are harmless: the queue holds
        // one job per album per input revision, and the scan revision
        // is the album key itself.
        let already = self
            .queue
            .jobs_for_album(album_key)
            .into_iter()
            .any(|job| job.input_revision == album_key);
        if already {
            return track_ids.len();
        }
        self.queue.enqueue(super::identify::models::IdentifyJob {
            id: self.ids.new_id(),
            local_album_id: album_key.to_owned(),
            kind: IdentifyKind::Automatic,
            priority: super::identify::queue::PRIORITY_NEW_OR_CHANGED,
            state: super::identify::models::JobState::Queued,
            attempts: 0,
            not_before_ms: now_ms,
            input_revision: album_key.to_owned(),
            requested_by_user_id: None,
            failure_code: None,
        });
        track_ids.len()
    }
}

// ---------------------------------------------------------------------------
// Publish -> filesystem: free-space probe over statvfs.
// ---------------------------------------------------------------------------

/// Free-space probe over `statvfs` on each root directory. The root
/// lookup re-reads the shared registry every call, never a captured
/// snapshot.
pub struct FsSpaceProbe {
    root_dirs: Arc<dyn Fn() -> HashMap<String, PathBuf> + Send + Sync>,
}

impl FsSpaceProbe {
    pub fn new(root_dirs: Arc<dyn Fn() -> HashMap<String, PathBuf> + Send + Sync>) -> Self {
        Self { root_dirs }
    }
}

impl SpaceProbe for FsSpaceProbe {
    fn free_bytes(&self, root_id: &str) -> Result<u64, PublishError> {
        let dirs = (self.root_dirs)();
        let dir = dirs
            .get(root_id)
            .ok_or_else(|| PublishError::UnsafePath(format!("unknown root {root_id}")))?;
        free_bytes_for(dir)
    }
}

fn free_bytes_for(dir: &Path) -> Result<u64, PublishError> {
    let text = dir.to_string_lossy().into_owned();
    let cstr = std::ffi::CString::new(text)
        .map_err(|_| PublishError::UnsafePath("root path holds a NUL byte".into()))?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // statvfs only reads filesystem metadata; the path is already sandbox-checked.
    let status = unsafe { libc::statvfs(cstr.as_ptr(), &mut stat) };
    if status != 0 {
        return Err(PublishError::Io(format!(
            "cannot stat free space under {}",
            dir.display()
        )));
    }
    Ok(stat.f_bavail * stat.f_frsize)
}

// ---------------------------------------------------------------------------
// Contrib ports: honest dormant implementations.
// ---------------------------------------------------------------------------

/// Contribution identity over the (not yet projected) album catalog.
/// Album rows with titles do not exist durably yet: scan commits
/// per-file catalog entries without titles, and no album-projection
/// slice has landed. Returning `None` keeps the service loud
/// (`AlbumNotFound`) instead of drafting from empty titles; the
/// verification worker idles honestly until the projection lands.
pub struct EmptyContributionIdentity;

impl ContributionIdentity for EmptyContributionIdentity {
    fn album_context<'a>(
        &'a self,
        _album_id: &'a str,
    ) -> BoxFuture<'a, Option<AlbumIdentificationContext>> {
        Box::pin(async move { None })
    }

    fn input_revisions(
        &self,
        _tracks: &[super::contrib::seams::IdentityTrack],
    ) -> (String, String, String) {
        (String::new(), String::new(), String::new())
    }
}

/// Contribution catalog port. Reads serve straight from SQLite with
/// no identity-bearing cache to invalidate and no reindex hook to
/// fire, so all three calls are honest no-ops (traced, so a future
/// cache lands its invalidation here).
pub struct NoopContributionCatalog;

impl ContributionCatalog for NoopContributionCatalog {
    fn invalidate_identity_scope<'a>(
        &'a self,
        album_mbids: &'a [String],
        artist_mbids: &'a [String],
    ) -> BoxFuture<'a, ()> {
        let albums = album_mbids.len();
        let artists = artist_mbids.len();
        Box::pin(async move {
            tracing::debug!(
                albums,
                artists,
                "contribution catalog invalidation: no cached scope to clear"
            );
        })
    }

    fn invalidate_identification<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            tracing::debug!("contribution catalog sweep: no cached scope to clear");
        })
    }

    fn after_identified<'a>(
        &'a self,
        local_album_id: &'a str,
        input_policy_revision: &'a str,
    ) -> BoxFuture<'a, ()> {
        let album = local_album_id.to_owned();
        let policy = input_policy_revision.to_owned();
        Box::pin(async move {
            tracing::debug!(
                album,
                policy,
                "contribution linked: no reindex hook registered"
            );
        })
    }
}

/// Attachment evidence: contradiction-only check in the v2 shape
/// (curator-verified release, no lone quorum). An empty verified
/// MBID or a title that disagrees with the draft after casefolding
/// needs review; anything else attaches. The full evidence engine
/// port stays a follow-up; this never invents a match, it only
/// refuses to contradict the curator silently.
pub struct MinimalAttachmentEvidence;

impl AttachmentEvidence for MinimalAttachmentEvidence {
    fn matcher_version(&self) -> String {
        "v3-minimal-1".to_owned()
    }

    fn decide_attachment<'a>(
        &'a self,
        contribution: &'a ContributionRecord,
        verified: &'a MusicBrainzVerifiedRelease,
        recording_mbids: &'a HashMap<String, Option<String>>,
        _relative_paths: &'a HashMap<String, String>,
    ) -> BoxFuture<'a, AttachmentDecision> {
        let candidate = AttachmentCandidate {
            release_group_mbid: verified.release_group_mbid.clone(),
            release_mbid: Some(verified.release_mbid.clone()),
            artist_mbid: verified.artist_mbid.clone(),
        };
        let key = candidate.key();
        let decision = if verified.release_mbid.trim().is_empty() {
            AttachmentDecision {
                outcome: AttachmentOutcome::NeedsReview,
                reason_code: Some("VERIFIED_MBID_MISSING".to_owned()),
                selected_candidate_key: None,
                candidates: vec![candidate],
            }
        } else if !titles_agree(
            contribution.draft.title.value.as_deref().unwrap_or(""),
            &verified.title,
        ) || recording_mbids
            .values()
            .any(|mbid| mbid.as_deref().is_some_and(|mbid| mbid.trim().is_empty()))
        {
            AttachmentDecision {
                outcome: AttachmentOutcome::NeedsReview,
                reason_code: Some("ATTACHMENT_CONTRADICTION".to_owned()),
                selected_candidate_key: None,
                candidates: vec![candidate],
            }
        } else {
            AttachmentDecision {
                outcome: AttachmentOutcome::Identified,
                reason_code: None,
                selected_candidate_key: Some(key),
                candidates: vec![candidate],
            }
        };
        Box::pin(async move { decision })
    }
}

fn titles_agree(draft: &str, verified: &str) -> bool {
    let fold = |value: &str| caseless::default_case_fold_str(&value.trim().to_lowercase());
    draft.trim().is_empty() || verified.trim().is_empty() || fold(draft) == fold(verified)
}

/// MusicBrainz contribution reads before the stage-5 port lands. The
/// stage-5 clients speak different seams (no background-priority
/// lanes), so porting them is a follow-up; until then verification
/// lookups defer as unavailable (the worker retries inside its
/// bounded window) and duplicate checks fail loudly. No live provider
/// contact happens through this adapter, ever.
pub struct UnavailableMusicBrainz;

impl MusicBrainzContrib for UnavailableMusicBrainz {
    fn resolve_url<'a>(
        &'a self,
        _url: &'a str,
        _relation: UrlRelation,
        _priority: droppedneedle::providers::slots::RequestPriority,
        _bypass_cache: bool,
    ) -> BoxFuture<'a, Result<MusicBrainzUrlResolution, ContribError>> {
        Box::pin(async move {
            Err(ContribError::Data(
                "The MusicBrainz adapter is not available.".into(),
            ))
        })
    }

    fn get_release_for_verification<'a>(
        &'a self,
        _release_mbid: &'a str,
        _priority: droppedneedle::providers::slots::RequestPriority,
        _bypass_cache: bool,
    ) -> BoxFuture<'a, Result<Option<MusicBrainzVerifiedRelease>, ProviderFailure>> {
        Box::pin(async move {
            Err(ProviderFailure::Unavailable {
                retry_after_seconds: None,
            })
        })
    }

    fn search_duplicate_releases<'a>(
        &'a self,
        _facts: &'a DuplicateSearchFacts,
        _limit: usize,
        _priority: droppedneedle::providers::slots::RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<MusicBrainzVerifiedRelease>, ContribError>> {
        Box::pin(async move {
            Err(ContribError::Data(
                "The MusicBrainz adapter is not available.".into(),
            ))
        })
    }
}
