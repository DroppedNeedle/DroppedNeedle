//! The contribution ports over the outside world: live MusicBrainz and
//! Discogs reads, and the catalog port that follows a link up through
//! identification.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;

use super::error::ContribError;
use super::models::{
    DiscogsArtistCredit, DiscogsFormat, DiscogsIdentifier, DiscogsLabel, DiscogsMedium,
    DiscogsRelease, DiscogsReleaseCandidate, DiscogsTrack, MusicBrainzUrlResolution,
    MusicBrainzVerifiedRelease, MusicBrainzVerifiedTrack,
};
use super::seams::{
    ContributionCatalog, DiscogsContrib, DuplicateSearchFacts, MusicBrainzContrib, ProviderFailure,
    UrlRelation,
};
use crate::clock::LibraryClock as _;
use droppedneedle_providers::ProviderError;
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::discogs;
use droppedneedle_providers::musicbrainz::models::{
    ArtistCreditName, LabelInfo, Medium as MbMedium, ReleaseGroupRef,
};
use droppedneedle_providers::musicbrainz::{Criticality, MusicBrainzView};

/// Contribution catalog port over identification. Reads serve straight
/// from SQLite, so there is no identity-bearing cache to invalidate (both
/// invalidations are traced no-ops). A link asks identification to take
/// the album again now, so its credits and release document catch up with
/// the new manual identity (identify never overwrites a manual decision).
///
/// Library Management hooks auto-management of a newly linked
/// album here, at `after_identified`, the same place v2's `on_identified`
/// callback fired.
pub struct IdentifyFollowUp {
    identify: Arc<crate::identify::sqlite::SqliteIdentifyStore>,
}

impl IdentifyFollowUp {
    /// Follow links up through `identify`.
    pub fn new(identify: Arc<crate::identify::sqlite::SqliteIdentifyStore>) -> Self {
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
                Ok(Ok(queued)) => {
                    tracing::info!(album, policy, queued, "linked album offered to identify")
                }
                Ok(Err(error)) => tracing::warn!(
                    %error,
                    album,
                    "linked album could not be offered to identify; the next scan will"
                ),
                Err(error) => tracing::warn!(%error, album, "identify follow-up did not run"),
            }
        })
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

/// The production MusicBrainz client the contribution adapter drives.
pub type ContribMbClient = MusicBrainzView;

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

/// Whether MusicBrainz could not answer, or the source cannot be used:
/// worth asking again later, unlike an answer that does not fit.
fn mb_unanswered(error: &ProviderError) -> bool {
    error.is_retriable()
        || matches!(
            error,
            ProviderError::NotConfigured { .. } | ProviderError::Invalid { .. }
        )
}

fn mb_contrib_error(error: ProviderError) -> ContribError {
    match error {
        error if mb_unanswered(&error) => {
            tracing::warn!(%error, "MusicBrainz contribution read failed");
            ContribError::ProviderUnavailable(error)
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
            let mbid = droppedneedle_providers::musicbrainz::normalize_mb_id(release_mbid);
            if !droppedneedle_providers::musicbrainz::is_valid_mbid(&mbid) {
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
                // MusicBrainz answers 400 "Invalid mbid." for an id it rejects.
                Err(ProviderError::BadRequest { status: 400, .. }) => return Ok(None),
                Err(
                    ProviderError::RateLimited { retry_after, .. }
                    | ProviderError::Unavailable {
                        retry_after: retry_after @ Some(_),
                        ..
                    },
                ) => {
                    return Err(ProviderFailure::Unavailable {
                        retry_after_seconds: retry_after.map(|wait| wait.as_secs_f64()),
                    });
                }
                Err(error) if mb_unanswered(&error) => {
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
            use droppedneedle_providers::musicbrainz::escape_lucene_phrase;
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

/// v2 kept Discogs answers for six hours, the display limit its terms set.
const DISCOGS_CACHE: Duration = Duration::from_secs(6 * 60 * 60);

/// Live Discogs reads for contributions over the one Discogs client, paced
/// at its rate row (25 a minute, no burst) and cached six hours. Only
/// contribution metadata crosses this boundary (see `providers::discogs`).
/// An outage surfaces as "Discogs is unavailable" rather than v2's silent
/// "not found".
///
/// Every caller is a person waiting, so calls go at user priority. A
/// display read (the `PrefetchVisible` lane) never waits for a turn: when
/// the bucket has no request free right now, or is resting after a 429, it
/// answers "unavailable" at once so a slow Discogs cannot hold up the
/// contribution page.
pub struct LiveDiscogsContrib {
    client: discogs::DiscogsClient,
    clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
    releases: TtlCache<Option<DiscogsRelease>>,
    searches: TtlCache<Vec<DiscogsReleaseCandidate>>,
}

impl LiveDiscogsContrib {
    /// Read Discogs through `client`, dating its answers by `clock`.
    pub fn new(
        client: discogs::DiscogsClient,
        clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
    ) -> Self {
        Self {
            client,
            clock,
            releases: TtlCache::new(DISCOGS_CACHE),
            searches: TtlCache::new(DISCOGS_CACHE),
        }
    }

    /// The priority to send at, or "unavailable" when a display read would
    /// have to wait.
    fn turn(&self, priority: RequestPriority) -> Result<RequestPriority, ContribError> {
        let wait = self.client.limiter().retry_after(1);
        if priority == RequestPriority::PrefetchVisible && !wait.is_zero() {
            return Err(ContribError::DiscogsUnavailable(
                ProviderError::RateLimited {
                    provider: "discogs",
                    retry_after: Some(wait),
                },
            ));
        }
        Ok(RequestPriority::UserInitiated)
    }
}

/// What a failed Discogs call means for a contribution: an outage or a
/// rate limit is "unavailable", anything else an answer that cannot be
/// used. A 404 never reaches here: the client reads it as absence.
fn discogs_error(error: ProviderError) -> ContribError {
    if error.is_retriable() {
        tracing::warn!(%error, "Discogs unavailable");
        ContribError::DiscogsUnavailable(error)
    } else {
        tracing::warn!(%error, "Discogs answered with data it could not use");
        ContribError::DiscogsUnusable
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
                .client
                .search_releases(query, bounded, self.clock.now_secs(), self.turn(priority)?)
                .await
                .map_err(discogs_error)?;
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
                .client
                .get_release(release_id, self.clock.now_secs(), self.turn(priority)?)
                .await
                .map_err(discogs_error)?
                .map(contrib_release);
            self.releases.put(release_id.to_owned(), release.clone());
            Ok(release)
        })
    }
}
