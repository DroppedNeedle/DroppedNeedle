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
use crate::library::matching::{CreditedArtist, Release, ReleaseMedium, ReleaseTrack};
use crate::library::scan::pool::BlockingPool;
use crate::library::scan::roots::RootRegistry;
use crate::providers::acoustid::{AcoustIdClient, BatchQuery};
use crate::providers::degradation::DegradationSink;
use crate::providers::limiter::Pacer;
use crate::providers::musicbrainz::{
    ArtistCreditName, Criticality, MbRelease, MbTransport, MusicBrainzClient, hit_score,
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

/// Chromaprint on the blocking pool plus batched AcoustID lookups.
pub struct AcoustIdFingerprints<P, S> {
    client: AcoustIdClient<P, S>,
    config: Arc<ConfigStore>,
    roots: Roots,
    pool: BlockingPool,
}

impl<P: Pacer, S: DegradationSink> AcoustIdFingerprints<P, S> {
    pub fn new(
        client: AcoustIdClient<P, S>,
        config: Arc<ConfigStore>,
        roots: Roots,
        pool: BlockingPool,
    ) -> Self {
        Self {
            client,
            config,
            roots,
            pool,
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
        let registry = (self.roots)();
        let jobs: Vec<(String, PathBuf)> = tracks
            .iter()
            .filter_map(|track| {
                let root = registry.resolve(&track.root_id)?;
                Some((
                    track.local_track_id.clone(),
                    root.path.join(&track.relative_path),
                ))
            })
            .collect();
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
                Ok(Ok(print)) if print.duration_seconds > 0 => prints.push((id, print)),
                Ok(Ok(_)) => tracing::debug!(track = id, "zero-length audio not fingerprinted"),
                Ok(Err(error)) => tracing::info!(track = id, %error, "fingerprint failed"),
                Err(error) => tracing::warn!(track = id, %error, "fingerprint job failed"),
            }
        }
        let queries: Vec<BatchQuery<'_>> = prints
            .iter()
            .map(|(_, print)| BatchQuery {
                fingerprint: &print.fingerprint,
                duration_secs: u64::from(print.duration_seconds),
            })
            .collect();
        let found = self.client.lookup_batch(&key, &queries).await;
        let mut matches = AudioMatches::default();
        for (index, heard) in found {
            let Some((id, _)) = prints.get(index) else {
                continue;
            };
            if !heard.recording_ids.is_empty() {
                matches.recordings.insert(id.clone(), heard.recording_ids);
            }
            if !heard.release_ids.is_empty() {
                matches.releases.insert(id.clone(), heard.release_ids);
            }
        }
        matches
    }
}

impl<P: Pacer, S: DegradationSink> FingerprintSource for AcoustIdFingerprints<P, S> {
    fn identify<'a>(&'a self, tracks: &'a [LocalTrackFacts]) -> BoxFuture<'a, AudioMatches> {
        Box::pin(self.identify_tracks(tracks))
    }
}
