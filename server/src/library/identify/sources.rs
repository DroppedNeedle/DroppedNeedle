//! Where identification gets releases and fingerprints.
//!
//! [`ReleaseSource`] is the MusicBrainz side: release search, a release
//! with its full tracklist, and the canonical id of a merged recording.
//! Production binds it to the shared MusicBrainz client (which honors the
//! source setting, official or BrainzMash, and its rate limits); every
//! call runs identity-critical, so a dead provider defers the job rather
//! than letting it guess. [`FingerprintSource`] is the AcoustID side:
//! print the files on the blocking pool and look the prints up in
//! batches, with the API key read from the settings on every call.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::models::LocalTrackFacts;
use super::stores::FingerprintStore;
use crate::library::matching::model::credit_text;
use crate::library::matching::{CreditedArtist, Release, ReleaseMedium, ReleaseTrack};
use crate::library::scan::pool::BlockingPool;
use crate::library::scan::roots::RootRegistry;
use crate::providers::acoustid::{AcoustIdClient, BatchQuery};
use crate::providers::degradation::DegradationSink;
use crate::providers::limiter::Pacer;
use crate::providers::musicbrainz::{
    ArtistCreditName, Criticality, MbRelease, MbTransport, MusicBrainzClient, ReleaseSearchHit,
    hit_score,
};
use crate::runtime_config::ConfigStore;
use crate::runtime_config::secret_sections::TypedLibrary;

/// The provider could not answer; the job waits and retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceError(pub String);

/// One release search hit, before its tracklist is fetched.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseHit {
    pub id: String,
    pub release_group_id: String,
    pub score: i64,
    /// Total tracks over all media, when the index knows every count.
    pub track_count: Option<u32>,
}

/// A curator's release search: one page of editions for a title and
/// (unless blank) an artist, or every release of one release group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditionQuery {
    pub title: String,
    pub artist: String,
    /// List this release group's releases instead of searching by name.
    pub release_group_mbid: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

/// One MusicBrainz release as the edition finder lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Edition {
    pub release_mbid: String,
    pub release_group_mbid: String,
    pub artist_name: String,
    pub title: String,
    pub date: Option<String>,
    pub country: Option<String>,
    pub status: Option<String>,
    pub packaging: Option<String>,
    pub media_formats: Vec<String>,
    pub disc_count: u32,
    pub track_count: u32,
    pub label: Option<String>,
    pub catalogue_number: Option<String>,
    pub barcode: Option<String>,
    pub disambiguation: Option<String>,
    pub score: i64,
}

/// One page of editions plus the index's total.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditionPage {
    pub items: Vec<Edition>,
    pub total: u64,
    pub offset: u64,
}

pub trait ReleaseSource: Send + Sync {
    /// Releases matching an album title and (unless blank) an artist.
    fn search<'a>(
        &'a self,
        title: &'a str,
        artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>>;

    /// One release with its tracklist; `None` when MusicBrainz has no
    /// such release. A redirected (merged) id comes back as the release
    /// it now names, with the requested id in [`Release::old_ids`].
    fn release<'a>(&'a self, mbid: &'a str) -> BoxFuture<'a, Result<Option<Release>, SourceError>>;

    /// The id a recording MBID resolves to after merges.
    fn canonical_recording<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>>;

    /// One page of releases for a curator choosing an exact edition. A
    /// source without a search index answers with an empty page.
    fn search_editions<'a>(
        &'a self,
        _query: &'a EditionQuery,
    ) -> BoxFuture<'a, Result<EditionPage, SourceError>> {
        Box::pin(async { Ok(EditionPage::default()) })
    }
}

/// Search hits per query.
const SEARCH_LIMIT: u32 = 25;
/// Includes for a release fetched for matching and tagging.
const RELEASE_INCLUDES: [&str; 4] = ["artist-credits", "labels", "recordings", "release-groups"];

impl<T: MbTransport, S: DegradationSink> ReleaseSource for MusicBrainzClient<T, S> {
    fn search<'a>(
        &'a self,
        title: &'a str,
        artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        Box::pin(async move {
            let page = self
                .search_releases(title, artist, SEARCH_LIMIT, Criticality::IdentityCritical)
                .await
                .map_err(|error| SourceError(error.to_string()))?;
            Ok(page
                .items
                .into_iter()
                .filter_map(|hit| {
                    let group = hit.release_group.as_ref()?.id.clone();
                    let counts: Option<Vec<u32>> =
                        hit.media.iter().map(|medium| medium.track_count).collect();
                    Some(ReleaseHit {
                        score: hit_score(hit.score, hit.ext_score),
                        track_count: counts
                            .filter(|counts| !counts.is_empty())
                            .map(|counts| counts.iter().sum()),
                        id: hit.id.to_ascii_lowercase(),
                        release_group_id: group.to_ascii_lowercase(),
                    })
                })
                .collect())
        })
    }

    fn release<'a>(&'a self, mbid: &'a str) -> BoxFuture<'a, Result<Option<Release>, SourceError>> {
        Box::pin(async move {
            let found = self
                .lookup_release(mbid, &RELEASE_INCLUDES, Criticality::IdentityCritical)
                .await
                .map_err(|error| SourceError(error.to_string()))?;
            let Some(lookup) = found else {
                return Ok(None);
            };
            let mut old_ids: Vec<String> = lookup
                .redirects
                .iter()
                .filter(|hop| hop.entity == "release")
                .map(|hop| hop.from_mbid.to_ascii_lowercase())
                .collect();
            let requested = mbid.trim().to_ascii_lowercase();
            if !requested.eq_ignore_ascii_case(&lookup.entity.id) && !old_ids.contains(&requested) {
                old_ids.push(requested);
            }
            release_from_wire(lookup.entity, old_ids).map(Some)
        })
    }

    fn canonical_recording<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>> {
        Box::pin(async move {
            self.resolve_recording_mbid(mbid, Criticality::IdentityCritical)
                .await
                .map(|found| found.map(|id| id.to_ascii_lowercase()))
                .map_err(|error| SourceError(error.to_string()))
        })
    }

    fn search_editions<'a>(
        &'a self,
        query: &'a EditionQuery,
    ) -> BoxFuture<'a, Result<EditionPage, SourceError>> {
        Box::pin(async move {
            // A dead index is an error the curator sees, not an empty list.
            let page = match query.release_group_mbid.as_deref() {
                Some(group) => {
                    self.search_release_group_editions(
                        group,
                        query.limit,
                        query.offset,
                        Criticality::IdentityCritical,
                    )
                    .await
                }
                None => {
                    self.search_release_editions(
                        &query.title,
                        &query.artist,
                        query.limit,
                        query.offset,
                        Criticality::IdentityCritical,
                    )
                    .await
                }
            }
            .map_err(|error| SourceError(error.to_string()))?;
            Ok(EditionPage {
                total: page.count,
                offset: page.offset,
                items: page
                    .items
                    .into_iter()
                    .filter_map(edition_from_hit)
                    .collect(),
            })
        })
    }
}

/// An edition-finder row from a release search hit; hits without a
/// release group are skipped, as v2 did.
fn edition_from_hit(hit: ReleaseSearchHit) -> Option<Edition> {
    let release_group_mbid = hit.release_group.as_ref()?.id.to_ascii_lowercase();
    let label = hit.label_info.first();
    let mut media_formats: Vec<String> = Vec::new();
    for format in hit.media.iter().filter_map(|medium| medium.format.clone()) {
        if !media_formats.contains(&format) {
            media_formats.push(format);
        }
    }
    let blank_none = |value: Option<String>| value.filter(|value| !value.trim().is_empty());
    Some(Edition {
        score: hit_score(hit.score, hit.ext_score),
        artist_name: credit_text(&credits(&hit.artist_credit)),
        title: hit.title.clone().unwrap_or_default(),
        date: blank_none(hit.date.clone()),
        country: blank_none(hit.country.clone()),
        status: blank_none(hit.status.clone()),
        packaging: blank_none(hit.packaging.clone()),
        disc_count: hit.media.len() as u32,
        track_count: hit
            .media
            .iter()
            .filter_map(|medium| medium.track_count)
            .sum(),
        media_formats,
        label: blank_none(label.and_then(|info| info.label.as_ref()?.name.clone())),
        catalogue_number: blank_none(label.and_then(|info| info.catalog_number.clone())),
        barcode: blank_none(hit.barcode.clone()),
        disambiguation: blank_none(hit.disambiguation.clone()),
        release_mbid: hit.id.to_ascii_lowercase(),
        release_group_mbid,
    })
}

/// The matcher's release from the MusicBrainz wire shape. A release
/// without its group, or a track without its recording, breaks the
/// provider contract for this include set.
pub fn release_from_wire(wire: MbRelease, old_ids: Vec<String>) -> Result<Release, SourceError> {
    let group = wire.release_group.ok_or_else(|| {
        SourceError(format!(
            "release {} arrived without its release group",
            wire.id
        ))
    })?;
    let mut media = wire.media;
    media.sort_by_key(|medium| medium.position.unwrap_or(u32::MAX));
    let mut tracks = Vec::new();
    let mut summary = Vec::with_capacity(media.len());
    let mut absolute = 0;
    for (medium_index, medium) in media.into_iter().enumerate() {
        let disc = medium.position.unwrap_or(medium_index as u32 + 1);
        summary.push(ReleaseMedium {
            position: disc,
            format: medium.format.clone(),
            title: medium.title.clone().filter(|title| !title.is_empty()),
            track_count: medium.track_count.unwrap_or(medium.tracks.len() as u32),
        });
        for (track_index, track) in medium.tracks.iter().enumerate() {
            let recording = track.recording.as_ref().ok_or_else(|| {
                SourceError(format!("track {} arrived without its recording", track.id))
            })?;
            absolute += 1;
            tracks.push(ReleaseTrack {
                id: track.id.to_ascii_lowercase(),
                recording_id: recording.id.to_ascii_lowercase(),
                title: track.display_title().unwrap_or_default().to_owned(),
                artists: credits(track.credit()),
                disc,
                position: track.position.unwrap_or(track_index as u32 + 1),
                absolute_position: absolute,
                length_ms: track.length_ms(),
            });
        }
    }
    let labels: Vec<String> = wire
        .label_info
        .iter()
        .filter_map(|info| info.label.as_ref()?.name.clone())
        .filter(|name| !name.is_empty())
        .collect();
    let catalog_numbers: Vec<String> = wire
        .label_info
        .iter()
        .filter_map(|info| info.catalog_number.clone())
        .filter(|number| !number.is_empty())
        .collect();
    Ok(Release {
        id: wire.id.to_ascii_lowercase(),
        release_group_id: group.id.to_ascii_lowercase(),
        title: wire.title.unwrap_or_default(),
        artists: credits(&wire.artist_credit),
        date: wire.date.filter(|date| !date.is_empty()),
        original_date: group.first_release_date.filter(|date| !date.is_empty()),
        country: wire.country.filter(|country| !country.is_empty()),
        status: wire.status,
        barcode: wire.barcode.filter(|barcode| !barcode.is_empty()),
        asin: wire.asin.filter(|asin| !asin.is_empty()),
        primary_type: group.primary_type,
        secondary_types: group.secondary_types,
        labels: dedup(labels),
        catalog_numbers: dedup(catalog_numbers),
        media: summary,
        tracks,
        old_ids,
    })
}

fn credits(credit: &[ArtistCreditName]) -> Vec<CreditedArtist> {
    credit
        .iter()
        .map(|entry| CreditedArtist {
            id: entry.artist.id.to_ascii_lowercase(),
            name: if entry.name.is_empty() {
                entry.artist.name.clone()
            } else {
                entry.name.clone()
            },
            sort_name: entry
                .artist
                .sort_name
                .clone()
                .filter(|name| !name.is_empty()),
            join: entry.joinphrase.clone(),
        })
        .collect()
}

fn dedup(values: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(values.len());
    for value in values {
        if !out.contains(&value) {
            out.push(value);
        }
    }
    out
}

/// What AcoustID heard, per local track id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioMatches {
    pub recordings: HashMap<String, Vec<String>>,
    pub releases: HashMap<String, Vec<String>>,
}

pub trait FingerprintSource: Send + Sync {
    /// Print and look up the given tracks. Fails soft: tracks it could
    /// not print or look up are simply absent.
    fn identify<'a>(&'a self, tracks: &'a [LocalTrackFacts]) -> BoxFuture<'a, AudioMatches>;

    /// Print and look up files outside the library (a download being
    /// imported), keyed by the caller's ids: the recordings heard in each.
    /// Nothing is kept. Fails soft like [`Self::identify`].
    fn identify_files<'a>(
        &'a self,
        _files: &'a [(String, PathBuf)],
    ) -> BoxFuture<'a, HashMap<String, Vec<String>>> {
        Box::pin(async { HashMap::new() })
    }
}

/// No fingerprinting (tests, and wiring without a pool).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoFingerprints;

impl FingerprintSource for NoFingerprints {
    fn identify<'a>(&'a self, _tracks: &'a [LocalTrackFacts]) -> BoxFuture<'a, AudioMatches> {
        Box::pin(async { AudioMatches::default() })
    }
}

/// Live roots, read when a job needs them.
pub type Roots = Arc<dyn Fn() -> RootRegistry + Send + Sync>;

/// Chromaprint on the blocking pool plus batched AcoustID lookups. Prints
/// are kept per track and file revision, so a file is decoded once.
pub struct AcoustIdFingerprints<P, S> {
    client: AcoustIdClient<P, S>,
    config: Arc<ConfigStore>,
    roots: Roots,
    pool: BlockingPool,
    store: Arc<dyn FingerprintStore>,
}

impl<P: Pacer, S: DegradationSink> AcoustIdFingerprints<P, S> {
    pub fn new(
        client: AcoustIdClient<P, S>,
        config: Arc<ConfigStore>,
        roots: Roots,
        pool: BlockingPool,
        store: Arc<dyn FingerprintStore>,
    ) -> Self {
        Self {
            client,
            config,
            roots,
            pool,
            store,
        }
    }

    /// The AcoustID key from the settings, read per job so a change
    /// applies without a restart. Blank means fingerprinting is off.
    fn api_key(&self) -> String {
        match self.config.get_raw::<TypedLibrary>() {
            Ok(settings) => settings.acoustid_api_key.expose().trim().to_owned(),
            Err(error) => {
                tracing::warn!(%error, "cannot read the AcoustID key; fingerprints skipped");
                String::new()
            }
        }
    }

    async fn identify_tracks(&self, tracks: &[LocalTrackFacts]) -> AudioMatches {
        let key = self.api_key();
        if key.is_empty() {
            return AudioMatches::default();
        }
        // Prints already taken for these exact files come from the store.
        let wanted: Vec<(String, String)> = tracks
            .iter()
            .map(|track| (track.local_track_id.clone(), track.stat_revision.clone()))
            .collect();
        let store = self.store.clone();
        let stored: HashMap<String, (String, u32)> = tokio::task::spawn_blocking(move || {
            wanted
                .into_iter()
                .filter_map(|(id, revision)| {
                    let print = store.fingerprint(&id, &revision)?;
                    Some((id, print))
                })
                .collect()
        })
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "reading stored fingerprints failed");
            HashMap::new()
        });
        let registry = (self.roots)();
        let jobs: Vec<(String, PathBuf)> = tracks
            .iter()
            .filter(|track| !stored.contains_key(&track.local_track_id))
            .filter_map(|track| {
                let root = registry.resolve(&track.root_id)?;
                Some((
                    track.local_track_id.clone(),
                    root.path.join(&track.relative_path),
                ))
            })
            .collect();
        let mut prints: Vec<(String, String, u32)> = stored
            .into_iter()
            .map(|(id, (print, seconds))| (id, print, seconds))
            .collect();
        prints.extend(self.print(jobs).await);
        let queries: Vec<BatchQuery<'_>> = prints
            .iter()
            .map(|(_, print, seconds)| BatchQuery {
                fingerprint: print,
                duration_secs: u64::from(*seconds),
            })
            .collect();
        let found = self.client.lookup_batch(&key, &queries).await;
        let mut matches = AudioMatches::default();
        for (index, heard) in &found {
            let Some((id, _, _)) = prints.get(*index) else {
                continue;
            };
            if !heard.recording_ids.is_empty() {
                matches
                    .recordings
                    .insert(id.clone(), heard.recording_ids.clone());
            }
            if !heard.release_ids.is_empty() {
                matches
                    .releases
                    .insert(id.clone(), heard.release_ids.clone());
            }
        }
        // Keep every print the lookup answered for, matched or not.
        let revisions: HashMap<String, String> = tracks
            .iter()
            .map(|track| (track.local_track_id.clone(), track.stat_revision.clone()))
            .collect();
        let keep: Vec<(String, String, String, u32, bool)> = found
            .keys()
            .filter_map(|index| {
                let (id, print, seconds) = prints.get(*index)?;
                let revision = revisions.get(id)?.clone();
                let matched = matches.recordings.contains_key(id);
                Some((id.clone(), revision, print.clone(), *seconds, matched))
            })
            .collect();
        let store = self.store.clone();
        let kept = tokio::task::spawn_blocking(move || {
            for (id, revision, print, seconds, matched) in keep {
                store.save_fingerprint(&id, &revision, &print, seconds, matched);
            }
        })
        .await;
        if let Err(error) = kept {
            tracing::warn!(%error, "keeping fingerprints failed");
        }
        matches
    }
}

impl<P: Pacer, S: DegradationSink> AcoustIdFingerprints<P, S> {
    /// Chromaprint each file on the blocking pool: (id, print, seconds).
    async fn print(&self, jobs: Vec<(String, PathBuf)>) -> Vec<(String, String, u32)> {
        let printed = futures_util::future::join_all(jobs.into_iter().map(|(id, path)| {
            let pool = self.pool.clone();
            async move {
                let result = pool
                    .run(move || crate::library::tags::generate_fingerprint(&path))
                    .await;
                (id, result)
            }
        }))
        .await;
        let mut prints = Vec::new();
        for (id, result) in printed {
            match result {
                Ok(Ok(print)) if print.duration_seconds > 0 => {
                    prints.push((id, print.fingerprint, print.duration_seconds));
                }
                Ok(Ok(_)) => tracing::debug!(track = id, "zero-length audio not fingerprinted"),
                Ok(Err(error)) => tracing::info!(track = id, %error, "fingerprint failed"),
                Err(error) => tracing::warn!(track = id, %error, "fingerprint job failed"),
            }
        }
        prints
    }

    async fn identify_paths(&self, files: &[(String, PathBuf)]) -> HashMap<String, Vec<String>> {
        let key = self.api_key();
        if key.is_empty() || files.is_empty() {
            return HashMap::new();
        }
        let prints = self.print(files.to_vec()).await;
        let queries: Vec<BatchQuery<'_>> = prints
            .iter()
            .map(|(_, print, seconds)| BatchQuery {
                fingerprint: print,
                duration_secs: u64::from(*seconds),
            })
            .collect();
        let found = self.client.lookup_batch(&key, &queries).await;
        found
            .into_iter()
            .filter_map(|(index, heard)| {
                let (id, _, _) = prints.get(index)?;
                (!heard.recording_ids.is_empty()).then(|| (id.clone(), heard.recording_ids))
            })
            .collect()
    }
}

impl<P: Pacer, S: DegradationSink> FingerprintSource for AcoustIdFingerprints<P, S> {
    fn identify<'a>(&'a self, tracks: &'a [LocalTrackFacts]) -> BoxFuture<'a, AudioMatches> {
        Box::pin(self.identify_tracks(tracks))
    }

    fn identify_files<'a>(
        &'a self,
        files: &'a [(String, PathBuf)],
    ) -> BoxFuture<'a, HashMap<String, Vec<String>>> {
        Box::pin(self.identify_paths(files))
    }
}
