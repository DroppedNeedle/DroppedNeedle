//! Seam implementations between the library slices (integrator-owned).
//!
//! Each adapter is narrow by design: it translates at a slice
//! boundary and owns no domain logic. Gaps that need a future slice
//! (AcoustID key config, album projection for contributions) degrade
//! loudly or dormantly, never silently wrong; each carries a note.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Read as _;
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

/// Files at or under this size parse from one shared read; larger files
/// keep the streaming path so a huge file never spikes the scan heap.
const MAX_BUFFERED_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Files over this size leave an empty buffer behind instead of a big
/// retained one. Covers the corpus outright.
const RETAINED_READ_CAPACITY: usize = 256 * 1024;

thread_local! {
    /// One file buffer per scan worker, reused across files. Held behind
    /// `Arc` so the probe half shares the bytes without copying; the
    /// clones never escape one `read_tags` call, so the buffer is always
    /// exclusively owned here (a contended buffer falls back cleanly).
    static READ_BUFFER: RefCell<Arc<Vec<u8>>> = RefCell::new(Arc::new(Vec::new()));
}

impl TagReader for LoftyTagReader {
    fn read_tags(&self, path: &Path) -> Result<ScannedTags, TagReadError> {
        let format = super::tags::format_for_path(path).map_err(|_| TagReadError::Fatal)?;
        if let Some(scanned) = read_tags_buffered(path, format) {
            return scanned;
        }
        let tag = super::tags::read::read_tag_only(path, format).map_err(map_tag_read_error)?;
        let duration_secs = super::tags::probe(path)
            .ok()
            .map(|info| info.duration_seconds);
        Ok(scanned_tags(tag, duration_secs))
    }
}

/// Tag text plus duration from a single shared read of the file. Returns
/// `None` when the file should take the streaming path instead (over the
/// size cap, unreadable metadata, or a contended thread buffer); I/O and
/// parse failures inside the buffered path map exactly like the
/// streaming path, so verdicts never depend on which path ran.
fn read_tags_buffered(
    path: &Path,
    format: super::tags::AudioFormat,
) -> Option<Result<ScannedTags, TagReadError>> {
    let size = path.metadata().map(|meta| meta.len()).unwrap_or(0);
    if size > MAX_BUFFERED_FILE_BYTES {
        return None;
    }
    READ_BUFFER.with(|cell| {
        let mut shared = cell.borrow_mut();
        {
            let buffer = Arc::get_mut(&mut shared)?;
            buffer.clear();
            let mut file = match std::fs::File::open(path) {
                Ok(file) => file,
                Err(source) => return Some(Err(map_read_open_error(source, path, format))),
            };
            if let Err(source) = file.read_to_end(buffer) {
                return Some(Err(map_read_open_error(source, path, format)));
            }
        }
        let bytes = shared.clone();
        if bytes.len() > RETAINED_READ_CAPACITY {
            // A big file just passed through: leave an empty buffer
            // behind so the retained capacity stays small. This file's
            // backing lives on in `bytes` and frees when parsing ends.
            *shared = Arc::new(Vec::new());
        }
        let parsed = super::tags::read::read_tag_from_bytes(&bytes, path, format)
            .map_err(map_tag_read_error);
        let tag = match parsed {
            Ok(tag) => tag,
            Err(error) => return Some(Err(error)),
        };
        let duration_secs =
            super::tags::probe::probe_duration_from_shared(bytes, path, format).ok();
        Some(Ok(scanned_tags(tag, duration_secs)))
    })
}

/// Map a buffered-path open/read failure like the streaming path would:
/// AAC reads surface I/O (deferred re-offer), where every other format
/// surfaces a tag-read failure (lofty owns the read there, including
/// its I/O errors).
fn map_read_open_error(
    source: std::io::Error,
    path: &Path,
    format: super::tags::AudioFormat,
) -> TagReadError {
    if format == super::tags::AudioFormat::Aac {
        map_tag_read_error(super::tags::TagsError::Io {
            path: path.display().to_string(),
            source,
        })
    } else {
        map_tag_read_error(super::tags::TagsError::TagRead {
            path: path.display().to_string(),
            reason: source.to_string(),
        })
    }
}

fn scanned_tags(tag: super::tags::read::AudioTag, duration_secs: Option<f64>) -> ScannedTags {
    let mut extra = HashMap::new();
    if !tag.genres.is_empty() {
        extra.insert("genre".to_owned(), tag.genres.join("\u{0}"));
    }
    if let Some(mbid) = tag.musicbrainz_recording_id.as_deref() {
        extra.insert("musicbrainz_recording_id".to_owned(), mbid.to_owned());
    }
    ScannedTags {
        artist: non_empty(tag.artist),
        album: non_empty(tag.album),
        title: non_empty(tag.title),
        duration_secs,
        extra,
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
    inner: Mutex<MapInner>,
}

#[derive(Debug, Default)]
struct MapInner {
    /// Track ids are UUIDs in production; the 128-bit form drops the
    /// 36-byte string plus its allocation at 100k scale.
    by_track: HashMap<u128, Arc<str>>,
    /// Non-UUID ids (tests, foreign callers) keep exact semantics here.
    overflow: HashMap<String, Arc<str>>,
    /// One shared copy of each album key: tracks outnumber albums ten
    /// to one, so sharing saves most of the value bytes at 100k scale.
    keys: HashMap<String, Arc<str>>,
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
        let shared = inner
            .keys
            .entry(album_key.to_owned())
            .or_insert_with(|| Arc::from(album_key))
            .clone();
        for track_id in track_ids {
            match uuid::Uuid::parse_str(track_id) {
                Ok(id) => {
                    inner.by_track.insert(id.as_u128(), shared.clone());
                }
                Err(_) => {
                    inner.overflow.insert(track_id.clone(), shared.clone());
                }
            }
        }
    }

    pub fn album_for_track(&self, track_id: &str) -> Option<String> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match uuid::Uuid::parse_str(track_id) {
            Ok(id) => inner.by_track.get(&id.as_u128()),
            Err(_) => inner.overflow.get(track_id),
        }
        .map(|shared| shared.to_string())
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
        _priority: crate::providers::slots::RequestPriority,
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
        _priority: crate::providers::slots::RequestPriority,
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
        _priority: crate::providers::slots::RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<MusicBrainzVerifiedRelease>, ContribError>> {
        Box::pin(async move {
            Err(ContribError::Data(
                "The MusicBrainz adapter is not available.".into(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new("tests/fixtures/library").join(name)
    }

    /// The buffered read-once path returns exactly what the streaming
    /// path would: same tag text, same duration, same error mapping.
    /// Runs over every committed audio fixture (WMA stays rejected).
    #[test]
    fn buffered_tag_read_matches_streaming() {
        let reader = LoftyTagReader;
        for name in [
            "flac_full_01.flac",
            "flac_full_02.flac",
            "flac_no_tags.flac",
            "flac_cjk_01.flac",
            "flac_compilation_01.flac",
            "flac_only_release_mbid.flac",
            "mp3_full_01.mp3",
            "m4a_full_01.m4a",
            "management_full.mp3",
            "management_full_v23.mp3",
            "management_full.flac",
            "management_full.m4a",
            "management_full.ogg",
            "management_full.opus",
            "management_full.wav",
            "management_full_riff.wav",
            "management_full.aac",
        ] {
            let path = fixture(name);
            let format = super::super::tags::format_for_path(&path).expect("fixture format");
            let streamed = (|| {
                let tag = super::super::tags::read::read_tag_only(&path, format)
                    .map_err(map_tag_read_error)?;
                let duration_secs = super::super::tags::probe(&path)
                    .ok()
                    .map(|info| info.duration_seconds);
                Ok::<_, TagReadError>(scanned_tags(tag, duration_secs))
            })();
            assert_eq!(reader.read_tags(&path), streamed, "fixture {name}");
        }
        // WMA never reaches either path.
        assert_eq!(
            reader.read_tags(&fixture("management_full.wma")),
            Err(TagReadError::Fatal)
        );
        // A vanishing file maps like the streaming path: I/O surfaces
        // for AAC (deferred re-offer), tag failure everywhere else.
        assert_eq!(
            reader.read_tags(Path::new("/nonexistent-missing-file.mp3")),
            Err(TagReadError::Fatal)
        );
        assert_eq!(
            reader.read_tags(Path::new("/nonexistent-missing-file.aac")),
            Err(TagReadError::Deferred)
        );
    }

    /// The track/album join records every offer and re-recording one
    /// album replaces its tracks' keys without disturbing the rest.
    /// UUID and non-UUID ids share the same semantics.
    #[test]
    fn track_album_map_records_and_replaces() {
        let map = TrackAlbumMap::new();
        let uuid_a = "123e4567-e89b-12d3-a456-426614174000";
        let uuid_b = "123e4567-e89b-12d3-a456-426614174001";
        map.record("r::a1", &[uuid_a.to_owned(), "t2".to_owned()]);
        map.record("r::a2", &[uuid_b.to_owned()]);
        assert_eq!(map.album_for_track(uuid_a).as_deref(), Some("r::a1"));
        assert_eq!(map.album_for_track("t2").as_deref(), Some("r::a1"));
        assert_eq!(map.album_for_track(uuid_b).as_deref(), Some("r::a2"));
        assert_eq!(map.album_for_track("missing"), None);
        assert_eq!(
            map.album_for_track("123e4567-e89b-12d3-a456-426614179999"),
            None
        );
        map.record("r::a1", &[uuid_a.to_owned(), "t4".to_owned()]);
        assert_eq!(map.album_for_track(uuid_a).as_deref(), Some("r::a1"));
        assert_eq!(map.album_for_track("t4").as_deref(), Some("r::a1"));
        assert_eq!(map.album_for_track(uuid_b).as_deref(), Some("r::a2"));
    }
}
