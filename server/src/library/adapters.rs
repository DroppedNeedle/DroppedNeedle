//! Seam implementations between the library modules.
//!
//! Each adapter is narrow by design: it translates at a module boundary
//! and owns no domain logic. The contribution adapters read live
//! MusicBrainz and Discogs, decide attachments with the library matcher,
//! and follow a link up through identification.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;

use super::contrib::error::ContribError;
use super::contrib::models::{
    ContributionRecord, DiscogsArtistCredit, DiscogsFormat, DiscogsIdentifier, DiscogsLabel,
    DiscogsMedium, DiscogsRelease, DiscogsReleaseCandidate, DiscogsTrack, MusicBrainzUrlResolution,
    MusicBrainzVerifiedRelease, MusicBrainzVerifiedTrack, ReleaseTrackDraft,
};
use super::contrib::seams::{
    AttachmentCandidate, AttachmentDecision, AttachmentEvidence, AttachmentOutcome,
    ContributionCatalog, DiscogsContrib, DuplicateSearchFacts, MusicBrainzContrib, ProviderFailure,
    TrackEvidence, UrlRelation,
};
use super::matching::decide::{ACCEPT_ALBUM, ACCEPT_TRACK};
use super::matching::{
    CreditedArtist, LocalAlbum, LocalTrack, Release, ReleaseMedium, ReleaseTrack, match_release,
};
use super::publish::PublishError;
use super::publish::planner::SpaceProbe;
use super::scan::seams::{ScannedTags, TagReadError, TagReader};
use crate::providers::adapters::{HealthSink, ReqwestGet};
use crate::providers::discogs;
use crate::providers::musicbrainz::models::{
    ArtistCreditName, LabelInfo, Medium as MbMedium, ReleaseGroupRef,
};
use crate::providers::musicbrainz::{Criticality, MbError, MusicBrainzClient, ReqwestMbTransport};
use crate::providers::slots::RequestPriority;

// ---------------------------------------------------------------------------
// Scan -> tags: read-only tag access over the tags module.
// ---------------------------------------------------------------------------

/// Read-only tag reader over [`crate::library::tags`]. Never writes:
/// tags and stream properties come from the container headers (no audio
/// decode), and every failure maps onto the scan seam's two errors. A
/// parser panic on a malformed file is caught and reads as a fatal tag
/// failure for that file instead of taking the scan worker down.
pub struct LoftyTagReader;

impl TagReader for LoftyTagReader {
    fn read_tags(&self, path: &Path) -> Result<ScannedTags, TagReadError> {
        let format = super::tags::format_for_path(path).map_err(|_| TagReadError::Fatal)?;
        let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::tags::read::read_scan_metadata(path, format)
        }));
        match read {
            Ok(Ok((tag, header))) => Ok(ScannedTags { tag, header }),
            Ok(Err(error)) => Err(map_tag_read_error(error)),
            Err(_) => {
                tracing::error!(path = %path.display(), "tag parser panicked; file skipped");
                Err(TagReadError::Fatal)
            }
        }
    }
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
// Contrib ports.
// ---------------------------------------------------------------------------

/// Contribution catalog port over identification. Reads serve straight
/// from SQLite, so there is no identity-bearing cache to invalidate (both
/// invalidations are traced no-ops). A link asks identification to take
/// the album again now, so its credits and release document catch up with
/// the new manual identity (identify never overwrites a manual decision).
///
/// Library Management (WI-21) hooks auto-management of a newly linked
/// album here, at `after_identified`, the same place v2's `on_identified`
/// callback fired.
pub struct IdentifyFollowUp {
    identify: Arc<super::identify::sqlite::SqliteIdentifyStore>,
}

impl IdentifyFollowUp {
    /// Follow links up through `identify`.
    pub fn new(identify: Arc<super::identify::sqlite::SqliteIdentifyStore>) -> Self {
        Self { identify }
    }
}

impl ContributionCatalog for IdentifyFollowUp {
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
        let identify = self.identify.clone();
        Box::pin(async move {
            let offered = {
                let album = album.clone();
                tokio::task::spawn_blocking(move || identify.offer(&album)).await
            };
            match offered {
                Ok(Some(queued)) => {
                    tracing::info!(album, policy, queued, "linked album offered to identify")
                }
                Ok(None) => tracing::warn!(
                    album,
                    "linked album could not be offered to identify; the next scan will"
                ),
                Err(error) => tracing::warn!(%error, album, "identify follow-up did not run"),
            }
        })
    }
}

/// Attachment evidence over the library matcher: the album's files, as
/// the contribution describes them (draft titles where the curator edited,
/// snapshot positions and lengths, the files' recording MBIDs), are scored
/// against the verified release's tracklist with the same pairing,
/// distances and title, artist and length gates identification uses.
///
/// v2 decided attachments with its evidence engine minus the lone-release
/// quorum (the curator named this exact release), so this does the same:
/// accept only a clean fit, and send everything else to review with a
/// catalogued reason. An accepted decision carries the file-to-track
/// pairing, which the link writes as track identities.
pub struct MatchingAttachmentEvidence;

impl AttachmentEvidence for MatchingAttachmentEvidence {
    fn matcher_version(&self) -> String {
        "v3-matching-1".to_owned()
    }

    fn decide_attachment<'a>(
        &'a self,
        contribution: &'a ContributionRecord,
        verified: &'a MusicBrainzVerifiedRelease,
        recording_mbids: &'a HashMap<String, Option<String>>,
        _relative_paths: &'a HashMap<String, String>,
    ) -> BoxFuture<'a, AttachmentDecision> {
        let decision = attachment_decision(contribution, verified, recording_mbids);
        Box::pin(async move { decision })
    }
}

fn attachment_decision(
    contribution: &ContributionRecord,
    verified: &MusicBrainzVerifiedRelease,
    recording_mbids: &HashMap<String, Option<String>>,
) -> AttachmentDecision {
    let candidate = AttachmentCandidate {
        release_group_mbid: verified.release_group_mbid.clone(),
        release_mbid: Some(verified.release_mbid.clone()),
        artist_mbid: verified.artist_mbid.clone(),
    };
    let review = |code: &str, candidate: AttachmentCandidate| AttachmentDecision {
        outcome: AttachmentOutcome::NeedsReview,
        reason_code: Some(code.to_owned()),
        selected_candidate_key: None,
        candidates: vec![candidate],
        tracks: Vec::new(),
    };
    if verified.release_mbid.trim().is_empty() {
        return review("VERIFIED_MBID_MISSING", candidate);
    }
    let local = local_album(contribution, recording_mbids);
    let release = matching_release(verified);
    let matched = match_release(&local, &release, &HashMap::new());
    if !matched.conflicts.is_empty() {
        return review("ATTACHMENT_CONTRADICTION", candidate);
    }
    if !matched.names_agree {
        return review("ATTACHMENT_NAME_MISMATCH", candidate);
    }
    let unmatched_limit = if local.tracks.len() <= 20 { 1 } else { 2 };
    if matched.pairs.is_empty()
        || matched.library_distance() > ACCEPT_ALBUM
        || matched.worst_track() > ACCEPT_TRACK
        || matched.unmatched.len() > unmatched_limit
    {
        return review("ATTACHMENT_WEAK_FIT", candidate);
    }
    let tracks = matched
        .pairs
        .iter()
        .filter_map(|pair| {
            let file = local.tracks.get(pair.local)?;
            let track = release.tracks.get(pair.track)?;
            (!track.recording_id.is_empty()).then(|| TrackEvidence {
                local_track_id: file.id.clone(),
                recording_mbid: track.recording_id.clone(),
                release_track_mbid: Some(track.id.clone()).filter(|id| !id.is_empty()),
                medium_position: Some(i64::from(track.disc)),
                track_position: Some(i64::from(track.position)),
            })
        })
        .collect();
    AttachmentDecision {
        outcome: AttachmentOutcome::Identified,
        reason_code: None,
        selected_candidate_key: Some(candidate.key()),
        candidates: vec![candidate],
        tracks,
    }
}

/// The album as the contribution describes it (v2 `_attachment_evidence`):
/// draft values where present, the snapshot otherwise.
fn local_album(
    contribution: &ContributionRecord,
    recording_mbids: &HashMap<String, Option<String>>,
) -> LocalAlbum {
    let draft_tracks: HashMap<&str, &ReleaseTrackDraft> = contribution
        .draft
        .media
        .iter()
        .flat_map(|medium| medium.tracks.iter())
        .map(|track| (track.local_track_id.as_str(), track))
        .collect();
    let text = |value: Option<&String>, fallback: &str| {
        value
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .unwrap_or(fallback)
            .to_owned()
    };
    let snapshot = &contribution.local_snapshot;
    let tracks = snapshot
        .media
        .iter()
        .flat_map(|medium| medium.tracks.iter())
        .map(|track| {
            let draft = draft_tracks.get(track.local_track_id.as_str());
            LocalTrack {
                id: track.local_track_id.clone(),
                title: text(draft.and_then(|d| d.title.value.as_ref()), &track.title),
                artist: text(
                    draft.and_then(|d| d.artist_name.value.as_ref()),
                    track.artist_name.as_deref().unwrap_or(""),
                ),
                track_number: u32::try_from(track.track_number).unwrap_or(0),
                disc_number: u32::try_from(track.disc_number.max(1)).unwrap_or(1),
                duration_secs: track.duration_seconds.filter(|_| track.duration_reliable),
                recording_mbid: recording_mbids
                    .get(&track.local_track_id)
                    .cloned()
                    .flatten(),
                ..LocalTrack::default()
            }
        })
        .collect();
    LocalAlbum {
        title: text(contribution.draft.title.value.as_ref(), &snapshot.title),
        artist: text(
            contribution.draft.artist_credit.value.as_ref(),
            &snapshot.album_artist_name,
        ),
        year: snapshot.year,
        is_compilation: snapshot.is_compilation,
        tracks,
    }
}

/// The verified release in the matcher's shape.
fn matching_release(verified: &MusicBrainzVerifiedRelease) -> Release {
    let mut media: Vec<ReleaseMedium> = Vec::new();
    for track in &verified.tracks {
        let disc = u32::try_from(track.disc_number.max(1)).unwrap_or(1);
        match media.iter_mut().find(|medium| medium.position == disc) {
            Some(medium) => medium.track_count += 1,
            None => media.push(ReleaseMedium {
                position: disc,
                track_count: 1,
                ..ReleaseMedium::default()
            }),
        }
    }
    Release {
        id: verified.release_mbid.clone(),
        release_group_id: verified.release_group_mbid.clone(),
        title: verified.title.clone(),
        artists: vec![CreditedArtist {
            id: verified.artist_mbid.clone().unwrap_or_default(),
            name: verified.artist_name.clone(),
            sort_name: None,
            join: String::new(),
        }],
        date: verified.date.clone(),
        country: verified.country.clone(),
        status: verified.status.clone(),
        barcode: verified.barcode.clone(),
        media,
        tracks: verified
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| ReleaseTrack {
                id: track.release_track_mbid.clone().unwrap_or_default(),
                recording_id: track.recording_mbid.clone().unwrap_or_default(),
                title: track.title.clone(),
                artists: Vec::new(),
                disc: u32::try_from(track.disc_number.max(1)).unwrap_or(1),
                position: u32::try_from(track.position.max(0)).unwrap_or(0),
                absolute_position: u32::try_from(index + 1).unwrap_or(u32::MAX),
                length_ms: track
                    .duration_seconds
                    .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
                    .map(|seconds| (seconds * 1000.0).round() as u64),
            })
            .collect(),
        ..Release::default()
    }
}

/// A small expiring map for provider answers. Bounded: when it fills, the
/// expired entries go first and, failing that, everything does.
struct TtlCache<V> {
    ttl: Duration,
    entries: Mutex<HashMap<String, (Instant, V)>>,
}

const TTL_CACHE_MAX: usize = 512;

impl<V: Clone> TtlCache<V> {
    fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (Instant, V)>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn get(&self, key: &str) -> Option<V> {
        self.lock()
            .get(key)
            .filter(|(stored, _)| stored.elapsed() < self.ttl)
            .map(|(_, value)| value.clone())
    }

    fn put(&self, key: String, value: V) {
        let mut entries = self.lock();
        if entries.len() >= TTL_CACHE_MAX {
            let ttl = self.ttl;
            entries.retain(|_, (stored, _)| stored.elapsed() < ttl);
            if entries.len() >= TTL_CACHE_MAX {
                entries.clear();
            }
        }
        entries.insert(key, (Instant::now(), value));
    }
}

fn now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

/// The production MusicBrainz client the contribution adapter drives.
pub type ContribMbClient = MusicBrainzClient<ReqwestMbTransport, HealthSink>;

/// Live MusicBrainz reads for contributions. Two clients over the same
/// transport and limiter: one waits at user priority (the curator is on
/// the page), one at background priority (the verification worker), so
/// verification never jumps the user queue. Every call is identity
/// critical: an outage is a typed failure, never an empty answer that
/// reads as "no duplicates". Answers are cached like v2 did (URL lookups
/// and verified releases for an hour, duplicate searches for 15 minutes);
/// `bypass_cache` reads fresh.
pub struct LiveMusicBrainzContrib {
    user: ContribMbClient,
    background: ContribMbClient,
    urls: TtlCache<MusicBrainzUrlResolution>,
    releases: TtlCache<Option<MusicBrainzVerifiedRelease>>,
    searches: TtlCache<Vec<MusicBrainzVerifiedRelease>>,
}

impl LiveMusicBrainzContrib {
    /// Wrap a user-priority and a background-priority client.
    pub fn new(user: ContribMbClient, background: ContribMbClient) -> Self {
        Self {
            user,
            background,
            urls: TtlCache::new(Duration::from_secs(3600)),
            releases: TtlCache::new(Duration::from_secs(3600)),
            searches: TtlCache::new(Duration::from_secs(900)),
        }
    }

    fn client(&self, priority: RequestPriority) -> &ContribMbClient {
        if priority == RequestPriority::UserInitiated {
            &self.user
        } else {
            &self.background
        }
    }
}

fn mb_contrib_error(error: MbError) -> ContribError {
    match error {
        MbError::Unavailable(_) | MbError::RateLimited { .. } | MbError::Misconfigured(_) => {
            tracing::warn!(%error, "MusicBrainz contribution read failed");
            ContribError::ProviderUnavailable
        }
        other => {
            tracing::warn!(error = %other, "MusicBrainz contribution payload unusable");
            ContribError::ProviderUnmappable
        }
    }
}

/// v2 `_verified_release`: a release with an id, a release group and a
/// title, credited as one string; the artist MBID only when the credit
/// names exactly one artist.
fn verified_release(
    id: &str,
    title: Option<&str>,
    group: Option<&ReleaseGroupRef>,
    credit: &[ArtistCreditName],
    facts: VerifiedFacts<'_>,
    media: &[MbMedium],
) -> Option<MusicBrainzVerifiedRelease> {
    let title = title.filter(|t| !t.is_empty())?;
    let group = group.filter(|g| !g.id.is_empty())?;
    if id.is_empty() {
        return None;
    }
    let artist_name = credit
        .iter()
        .map(|c| {
            let name = if c.name.is_empty() {
                c.artist.name.as_str()
            } else {
                c.name.as_str()
            };
            format!("{name}{}", c.joinphrase)
        })
        .collect::<String>()
        .trim()
        .to_owned();
    let mut artist_ids: Vec<&str> = credit
        .iter()
        .map(|c| c.artist.id.as_str())
        .filter(|id| !id.is_empty())
        .collect();
    artist_ids.sort_unstable();
    artist_ids.dedup();
    let first_label = facts.labels.first();
    let non_empty = |value: Option<&str>| value.filter(|v| !v.is_empty()).map(str::to_owned);
    Some(MusicBrainzVerifiedRelease {
        release_mbid: id.to_owned(),
        release_group_mbid: group.id.clone(),
        title: title.to_owned(),
        artist_name,
        artist_mbid: (artist_ids.len() == 1).then(|| artist_ids[0].to_owned()),
        date: non_empty(facts.date),
        country: non_empty(facts.country),
        status: non_empty(facts.status),
        packaging: non_empty(facts.packaging),
        barcode: non_empty(facts.barcode),
        label: first_label
            .and_then(|l| l.label.as_ref())
            .and_then(|l| non_empty(l.name.as_deref())),
        catalogue_number: first_label.and_then(|l| non_empty(l.catalog_number.as_deref())),
        tracks: media
            .iter()
            .flat_map(|medium| {
                medium.tracks.iter().filter_map(move |track| {
                    let title = track
                        .title
                        .clone()
                        .filter(|t| !t.is_empty())
                        .or_else(|| track.recording.as_ref().and_then(|r| r.title.clone()))
                        .filter(|t| !t.is_empty())?;
                    Some(MusicBrainzVerifiedTrack {
                        title,
                        position: i64::from(track.position.unwrap_or(0)),
                        disc_number: i64::from(medium.position.unwrap_or(1)),
                        duration_seconds: track.length.map(|ms| ms as f64 / 1000.0),
                        recording_mbid: track.recording.as_ref().map(|r| r.id.clone()),
                        release_track_mbid: Some(track.id.clone()).filter(|id| !id.is_empty()),
                    })
                })
            })
            .collect(),
    })
}

/// The release facts the verified shape copies through.
struct VerifiedFacts<'a> {
    date: Option<&'a str>,
    country: Option<&'a str>,
    status: Option<&'a str>,
    packaging: Option<&'a str>,
    barcode: Option<&'a str>,
    labels: &'a [LabelInfo],
}

impl MusicBrainzContrib for LiveMusicBrainzContrib {
    fn resolve_url<'a>(
        &'a self,
        url: &'a str,
        relation: UrlRelation,
        priority: RequestPriority,
        bypass_cache: bool,
    ) -> BoxFuture<'a, Result<MusicBrainzUrlResolution, ContribError>> {
        Box::pin(async move {
            let include = match relation {
                UrlRelation::Release => "release-rels",
                UrlRelation::ReleaseGroup => "release-group-rels",
            };
            let key = format!("{include}:{url}");
            if !bypass_cache && let Some(hit) = self.urls.get(&key) {
                return Ok(hit);
            }
            let resolution = self
                .client(priority)
                .resolve_url(url, &[include], Criticality::IdentityCritical)
                .await
                .map_err(mb_contrib_error)?;
            let mut release_mbids: Vec<String> = Vec::new();
            let mut release_group_mbids: Vec<String> = Vec::new();
            for relation in &resolution.relations {
                if let Some(release) = relation.release.as_ref()
                    && !release.id.is_empty()
                    && !release_mbids.contains(&release.id)
                {
                    release_mbids.push(release.id.clone());
                }
                if let Some(group) = relation.release_group.as_ref()
                    && !group.id.is_empty()
                    && !release_group_mbids.contains(&group.id)
                {
                    release_group_mbids.push(group.id.clone());
                }
            }
            let resolved = MusicBrainzUrlResolution {
                resource_url: url.to_owned(),
                release_mbids,
                release_group_mbids,
            };
            self.urls.put(key, resolved.clone());
            Ok(resolved)
        })
    }

    fn get_release_for_verification<'a>(
        &'a self,
        release_mbid: &'a str,
        priority: RequestPriority,
        bypass_cache: bool,
    ) -> BoxFuture<'a, Result<Option<MusicBrainzVerifiedRelease>, ProviderFailure>> {
        Box::pin(async move {
            let mbid = crate::providers::musicbrainz::normalize_mb_id(release_mbid);
            if !crate::providers::musicbrainz::is_valid_mbid(&mbid) {
                return Ok(None);
            }
            if !bypass_cache && let Some(hit) = self.releases.get(&mbid) {
                return Ok(hit);
            }
            let found = self
                .client(priority)
                .lookup_release(
                    &mbid,
                    &[
                        "artist-credits",
                        "labels",
                        "recordings",
                        "release-groups",
                        "url-rels",
                    ],
                    Criticality::IdentityCritical,
                )
                .await;
            let found = match found {
                Ok(found) => found,
                Err(MbError::InvalidMbid(_)) => return Ok(None),
                Err(MbError::RateLimited { retry_after_secs }) => {
                    return Err(ProviderFailure::Unavailable {
                        retry_after_seconds: retry_after_secs,
                    });
                }
                Err(error @ (MbError::Unavailable(_) | MbError::Misconfigured(_))) => {
                    tracing::warn!(%error, "MusicBrainz release verification unavailable");
                    return Err(ProviderFailure::Unavailable {
                        retry_after_seconds: None,
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "MusicBrainz release payload unusable");
                    return Err(ProviderFailure::Unmappable);
                }
            };
            let verified = found.and_then(|lookup| {
                let release = lookup.entity;
                verified_release(
                    &release.id,
                    release.title.as_deref(),
                    release.release_group.as_ref(),
                    &release.artist_credit,
                    VerifiedFacts {
                        date: release.date.as_deref(),
                        country: release.country.as_deref(),
                        status: release.status.as_deref(),
                        packaging: release.packaging.as_deref(),
                        barcode: release.barcode.as_deref(),
                        labels: &release.label_info,
                    },
                    &release.media,
                )
            });
            // Absence is not cached: a release saved a moment ago may
            // propagate on the next try.
            if verified.is_some() {
                self.releases.put(mbid, verified.clone());
            }
            Ok(verified)
        })
    }

    fn search_duplicate_releases<'a>(
        &'a self,
        facts: &'a DuplicateSearchFacts,
        limit: usize,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<MusicBrainzVerifiedRelease>, ContribError>> {
        Box::pin(async move {
            use crate::providers::musicbrainz::escape_lucene_phrase;
            let bounded = limit.clamp(1, 10) as u32;
            let mut query = format!(
                r#"release:"{}" AND artist:"{}""#,
                escape_lucene_phrase(&facts.title),
                escape_lucene_phrase(&facts.artist_name)
            );
            if let Some(barcode) = facts.barcode.as_deref().filter(|b| !b.is_empty()) {
                query = format!(
                    r#"({query}) OR barcode:"{}""#,
                    escape_lucene_phrase(barcode)
                );
            }
            let key = format!("{bounded}:{query}");
            if let Some(hit) = self.searches.get(&key) {
                return Ok(hit);
            }
            let page = self
                .client(priority)
                .search_releases_query(&query, bounded, Criticality::IdentityCritical)
                .await
                .map_err(mb_contrib_error)?;
            let releases: Vec<MusicBrainzVerifiedRelease> = page
                .items
                .iter()
                .filter_map(|hit| {
                    verified_release(
                        &hit.id,
                        hit.title.as_deref(),
                        hit.release_group.as_ref(),
                        &hit.artist_credit,
                        VerifiedFacts {
                            date: hit.date.as_deref(),
                            country: hit.country.as_deref(),
                            status: hit.status.as_deref(),
                            packaging: hit.packaging.as_deref(),
                            barcode: hit.barcode.as_deref(),
                            labels: &hit.label_info,
                        },
                        &hit.media,
                    )
                })
                .collect();
            self.searches.put(key, releases.clone());
            Ok(releases)
        })
    }
}

/// Discogs allows 25 unauthenticated requests a minute with no burst
/// (`x-discogs-ratelimit: 25`, v2 API notes), so calls are spaced by this.
const DISCOGS_SPACING: Duration = Duration::from_millis(2_400);
/// v2 kept Discogs answers for six hours, the display limit its terms set.
const DISCOGS_CACHE: Duration = Duration::from_secs(6 * 60 * 60);
/// v2 retried a failed Discogs call up to three times, waiting 1-4s.
const DISCOGS_ATTEMPTS: u32 = 3;
/// The longest Retry-After a user call waits out in place; a longer one
/// fails the call and holds every call until it passes.
const DISCOGS_MAX_INLINE_WAIT: Duration = Duration::from_secs(4);

/// Live Discogs reads for contributions over the shared catalog GET port:
/// paced at 25 a minute, retried briefly, cached six hours. Only
/// contribution metadata crosses this boundary (see `providers::discogs`).
/// An outage surfaces as "Discogs is unavailable" with a retry hint
/// rather than v2's silent "not found".
///
/// A 429's Retry-After is honoured: nothing goes to Discogs before it
/// passes. Display reads (the `PrefetchVisible` lane) make one attempt and
/// never wait, so a slow Discogs cannot hold up the contribution page.
pub struct LiveDiscogsContrib {
    http: ReqwestGet,
    next_slot: tokio::sync::Mutex<Instant>,
    blocked_until: Mutex<Option<Instant>>,
    releases: TtlCache<Option<DiscogsRelease>>,
    searches: TtlCache<Vec<DiscogsReleaseCandidate>>,
}

impl LiveDiscogsContrib {
    /// Read Discogs through `http`.
    pub fn new(http: ReqwestGet) -> Self {
        Self {
            http,
            next_slot: tokio::sync::Mutex::new(Instant::now()),
            blocked_until: Mutex::new(None),
            releases: TtlCache::new(DISCOGS_CACHE),
            searches: TtlCache::new(DISCOGS_CACHE),
        }
    }

    /// Wait for this caller's turn under the 25-a-minute budget.
    async fn pace(&self) {
        let mut next = self.next_slot.lock().await;
        let now = Instant::now();
        if *next > now {
            tokio::time::sleep(*next - now).await;
        }
        *next = Instant::now() + DISCOGS_SPACING;
    }

    /// The time a Retry-After told us to wait until, while it holds.
    fn blocked(&self) -> Option<Instant> {
        let guard = self
            .blocked_until
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.filter(|until| *until > Instant::now())
    }

    fn block_for(&self, wait: Duration) {
        *self
            .blocked_until
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now() + wait);
    }

    /// Run one paced call with v2's short retry on outages and 429s. The
    /// display lane gets one attempt and no waiting.
    async fn with_retry<T, F, Fut>(
        &self,
        priority: RequestPriority,
        call: F,
    ) -> Result<T, ContribError>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T, discogs::FetchError>>,
    {
        let display = priority == RequestPriority::PrefetchVisible;
        let attempts = if display { 1 } else { DISCOGS_ATTEMPTS };
        let mut attempt = 0;
        loop {
            if let Some(until) = self.blocked() {
                let wait = until.saturating_duration_since(Instant::now());
                if display || wait > DISCOGS_MAX_INLINE_WAIT {
                    return Err(ContribError::DiscogsUnavailable);
                }
                tokio::time::sleep(wait).await;
            }
            if display {
                // Never queue behind the pacer on the display lane.
                match self.next_slot.try_lock() {
                    Ok(mut next) if *next <= Instant::now() => {
                        *next = Instant::now() + DISCOGS_SPACING;
                    }
                    _ => return Err(ContribError::DiscogsUnavailable),
                }
            } else {
                self.pace().await;
            }
            let wait = match call().await {
                Ok(value) => return Ok(value),
                Err(discogs::FetchError::Unusable) => {
                    tracing::warn!("Discogs answered with data it could not use");
                    return Err(ContribError::DiscogsUnusable);
                }
                Err(discogs::FetchError::RateLimited { retry_after_secs }) => {
                    let wait = Duration::from_secs_f64(retry_after_secs.max(1.0));
                    self.block_for(wait);
                    if wait > DISCOGS_MAX_INLINE_WAIT {
                        tracing::warn!(?wait, "Discogs asked us to wait; holding calls until then");
                        return Err(ContribError::DiscogsUnavailable);
                    }
                    wait
                }
                Err(discogs::FetchError::Transport) => Duration::from_secs(1 << attempt.min(2)),
            };
            attempt += 1;
            if attempt >= attempts {
                tracing::warn!(attempts, "Discogs unavailable");
                return Err(ContribError::DiscogsUnavailable);
            }
            tokio::time::sleep(wait).await;
        }
    }
}

fn contrib_release(release: discogs::Release) -> DiscogsRelease {
    let credit = |artist: discogs::ArtistCredit| DiscogsArtistCredit {
        name: artist.name,
        credited_name: artist.credited_name,
        join_phrase: artist.join_phrase,
        artist_id: artist.artist_id,
        canonical_url: artist.canonical_url,
    };
    DiscogsRelease {
        release_id: release.release_id,
        master_id: release.master_id,
        canonical_release_url: release.canonical_release_url,
        canonical_master_url: release.canonical_master_url,
        title: release.title,
        artist_name: release.artist_name,
        artists: release.artists.into_iter().map(credit).collect(),
        released_date: release.released_date,
        year: release.year.and_then(|year| i32::try_from(year).ok()),
        country: release.country,
        labels: release
            .labels
            .into_iter()
            .map(|label| DiscogsLabel {
                name: label.name,
                catalogue_number: label.catalogue_number,
                label_id: label.label_id,
                canonical_url: label.canonical_url,
            })
            .collect(),
        identifiers: release
            .identifiers
            .into_iter()
            .map(|identifier| DiscogsIdentifier {
                kind: identifier.kind,
                value: identifier.value,
                description: identifier.description,
            })
            .collect(),
        barcode: release.barcode,
        formats: release
            .formats
            .into_iter()
            .map(|format| DiscogsFormat {
                name: format.name,
                quantity: format.quantity,
                descriptions: format.descriptions,
                text: format.text,
            })
            .collect(),
        media: release
            .media
            .into_iter()
            .map(|medium| DiscogsMedium {
                position: i64::from(medium.position),
                title: None,
                format: medium.format,
                tracks: medium
                    .tracks
                    .into_iter()
                    .map(|track| DiscogsTrack {
                        source_position: track.source_position,
                        number: track.number.map(i64::from),
                        title: track.title,
                        duration_seconds: track.duration_seconds,
                        heading: track.heading,
                        artists: track.artists.into_iter().map(credit).collect(),
                    })
                    .collect(),
            })
            .collect(),
        source_fetched_at: release.source_fetched_at,
    }
}

impl DiscogsContrib for LiveDiscogsContrib {
    fn search_releases<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<DiscogsReleaseCandidate>, ContribError>> {
        Box::pin(async move {
            let bounded = limit.clamp(1, 10) as u32;
            let key = format!("{bounded}:{}", discogs::normalize_query(query));
            if let Some(hit) = self.searches.get(&key) {
                return Ok(hit);
            }
            let candidates = self
                .with_retry(priority, || async {
                    discogs::DiscogsClient::new(&self.http)
                        .search_releases(query, bounded, now_seconds())
                        .await
                })
                .await?;
            let candidates: Vec<DiscogsReleaseCandidate> = candidates
                .into_iter()
                .map(|candidate| DiscogsReleaseCandidate {
                    release_id: candidate.release_id,
                    title: candidate.title,
                    artist_name: candidate.artist_name,
                    canonical_url: candidate.canonical_url,
                    year: candidate.year.and_then(|year| i32::try_from(year).ok()),
                    country: candidate.country,
                    label: candidate.label,
                    catalogue_number: candidate.catalogue_number,
                    format_summary: candidate.format_summary,
                    track_count: None,
                    master_id: candidate.master_id,
                    fetched_at: candidate.fetched_at,
                })
                .collect();
            self.searches.put(key, candidates.clone());
            Ok(candidates)
        })
    }

    fn get_release<'a>(
        &'a self,
        release_id: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<DiscogsRelease>, ContribError>> {
        Box::pin(async move {
            if let Some(hit) = self.releases.get(release_id) {
                return Ok(hit);
            }
            let release = self
                .with_retry(priority, || async {
                    discogs::DiscogsClient::new(&self.http)
                        .get_release(release_id, now_seconds())
                        .await
                })
                .await?
                .map(contrib_release);
            self.releases.put(release_id.to_owned(), release.clone());
            Ok(release)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new("tests/fixtures/library").join(name)
    }

    /// The scan reads the same tags the full read does, and its header
    /// duration agrees with the decode-counted probe, without decoding.
    #[test]
    fn scan_read_matches_full_read() {
        let reader = LoftyTagReader;
        for name in [
            "flac_full_01.flac",
            "flac_no_tags.flac",
            "mp3_full_01.mp3",
            "m4a_full_01.m4a",
            "management_full.ogg",
            "management_full.opus",
            "management_full.wav",
            "management_full.aac",
        ] {
            let path = fixture(name);
            let format = super::super::tags::format_for_path(&path).expect("fixture format");
            let scanned = reader.read_tags(&path).expect("scan read");
            let full = super::super::tags::read::read_tag_only(&path, format).expect("full read");
            assert_eq!(scanned.tag, full, "fixture {name}");
            let probed = super::super::tags::probe(&path)
                .expect("probe")
                .duration_seconds;
            let header = scanned.header.duration_seconds.expect("header duration");
            assert!(
                (header - probed).abs() < 0.5,
                "fixture {name}: {header} vs {probed}"
            );
        }
        assert_eq!(
            reader.read_tags(&fixture("management_full.wma")),
            Err(TagReadError::Fatal)
        );
        // A vanished file is an I/O failure: deferred, offered again.
        assert_eq!(
            reader.read_tags(Path::new("/nonexistent-missing-file.mp3")),
            Err(TagReadError::Deferred)
        );
    }
}
