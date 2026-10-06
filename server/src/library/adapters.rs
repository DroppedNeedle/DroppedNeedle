//! Seam implementations between the library modules.
//!
//! Each adapter is narrow by design: it translates at a module boundary
//! and owns no domain logic. The contribution provider adapters read live
//! MusicBrainz and Discogs; the catalog port stays a traced no-op while
//! reads serve straight from SQLite.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;

use super::contrib::error::ContribError;
use super::contrib::models::{
    ContributionRecord, DiscogsArtistCredit, DiscogsFormat, DiscogsIdentifier, DiscogsLabel,
    DiscogsMedium, DiscogsRelease, DiscogsReleaseCandidate, DiscogsTrack, MusicBrainzUrlResolution,
    MusicBrainzVerifiedRelease, MusicBrainzVerifiedTrack,
};
use super::contrib::seams::{
    AttachmentCandidate, AttachmentDecision, AttachmentEvidence, AttachmentOutcome,
    ContributionCatalog, DiscogsContrib, DuplicateSearchFacts, MusicBrainzContrib, ProviderFailure,
    UrlRelation,
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

/// Contribution catalog port. Reads serve straight from SQLite with
/// no identity-bearing cache to invalidate and no reindex hook to
/// fire, so all three calls are no-ops (traced, so a future
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

/// Live Discogs reads for contributions over the shared catalog GET port:
/// paced at 25 a minute, retried briefly, cached six hours. Only
/// contribution metadata crosses this boundary (see `providers::discogs`).
/// An outage surfaces as "Discogs is unavailable" with a retry hint
/// rather than v2's silent "not found".
pub struct LiveDiscogsContrib {
    http: ReqwestGet,
    next_slot: tokio::sync::Mutex<Instant>,
    releases: TtlCache<Option<DiscogsRelease>>,
    searches: TtlCache<Vec<DiscogsReleaseCandidate>>,
}

impl LiveDiscogsContrib {
    /// Read Discogs through `http`.
    pub fn new(http: ReqwestGet) -> Self {
        Self {
            http,
            next_slot: tokio::sync::Mutex::new(Instant::now()),
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

    /// Run one paced call with v2's short retry on outages and 429s.
    async fn with_retry<T, F, Fut>(&self, call: F) -> Result<T, ContribError>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T, discogs::FetchError>>,
    {
        let mut attempt = 0;
        loop {
            self.pace().await;
            let wait = match call().await {
                Ok(value) => return Ok(value),
                Err(discogs::FetchError::Unusable) => {
                    tracing::warn!("Discogs answered with data it could not use");
                    return Err(ContribError::Missing(
                        "Discogs did not return usable data for that release.".into(),
                    ));
                }
                Err(discogs::FetchError::RateLimited { retry_after_secs }) => {
                    Duration::from_secs_f64(retry_after_secs.clamp(1.0, 4.0))
                }
                Err(discogs::FetchError::Transport) => Duration::from_secs(1 << attempt.min(2)),
            };
            attempt += 1;
            if attempt >= DISCOGS_ATTEMPTS {
                tracing::warn!("Discogs unavailable after {DISCOGS_ATTEMPTS} attempts");
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
        _priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<DiscogsReleaseCandidate>, ContribError>> {
        Box::pin(async move {
            let bounded = limit.clamp(1, 10) as u32;
            let key = format!("{bounded}:{}", discogs::normalize_query(query));
            if let Some(hit) = self.searches.get(&key) {
                return Ok(hit);
            }
            let candidates = self
                .with_retry(|| async {
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
        _priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<DiscogsRelease>, ContribError>> {
        Box::pin(async move {
            if let Some(hit) = self.releases.get(release_id) {
                return Ok(hit);
            }
            let release = self
                .with_retry(|| async {
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
