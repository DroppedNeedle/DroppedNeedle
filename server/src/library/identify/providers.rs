//! Recall: gather candidate releases, with their tracklists, for one album.
//!
//! The live recall follows beets and Lidarr:
//!
//! 1. When most files name one release MBID, fetch that release first. If
//!    it matches closely ([`STRONG`]) recall stops there: one request.
//! 2. Otherwise search by album title and album artist (no artist for
//!    compilations), retrying once without an edition suffix such as
//!    "(Deluxe Edition)" when nothing comes back.
//! 3. Fetch the tracklists of the best few hits, skipping releases too
//!    small to hold the files, at most [`PER_GROUP`] editions of one
//!    release group and [`FETCH_LIMIT`] in all. Fetched releases are
//!    kept in the release store and reused for a week.
//! 4. Resolve file recording MBIDs no candidate carries, in case
//!    MusicBrainz merged them (a few lookups at most).
//! 5. When the tags are weak, fingerprint the files and add the releases
//!    most of the prints point at.
//!
//! Every MusicBrainz call is identity-critical: an outage defers the job.

use std::collections::HashMap;
use std::sync::Arc;

use super::evidence::local_album;
use super::models::{LocalAlbumFacts, RecallResult};
use super::sources::{FingerprintSource, ReleaseHit, ReleaseSource, SourceError};
use super::stores::{RELEASE_FRESH_SECS, ReleaseStore};
use crate::library::matching::decide::STRONG;
use crate::library::matching::strings::fold;
use crate::library::matching::{LocalAlbum, Release, match_release, should_fingerprint};
use crate::providers::musicbrainz::Criticality;

/// Tracklists fetched per album after a search.
pub const FETCH_LIMIT: usize = 5;
/// Editions of one release group fetched per album.
pub const PER_GROUP: usize = 3;
/// Merged-recording lookups per album.
const ALIAS_LOOKUPS: usize = 4;
/// Releases nominated by fingerprints fetched per album.
const FINGERPRINT_RELEASES: usize = 2;
/// Share of printed tracks that must point at a release to nominate it
/// (beets' chroma plugin).
const COMMON_RELEASE_SHARE: f64 = 0.6;

/// What the identify service needs from providers. The live adapter owns
/// provider clients; the fake answers from scripts. No live network in tests.
pub trait IdentifyProviders: Send + Sync {
    /// Recall candidate releases for the album facts.
    fn recall_candidates(
        &self,
        facts: &LocalAlbumFacts,
        limit: u32,
    ) -> std::pin::Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>>;
}

/// Recall result with the criticality each call ran under.
#[derive(Debug, Clone, Default)]
pub struct RecallOutcome {
    pub result: RecallResult,
    /// Criticality used for the MusicBrainz recall calls.
    pub recall_criticality: Option<Criticality>,
}

/// Live recall over a release source, the release store, and a
/// fingerprint source.
pub struct LiveProviders<R, F> {
    releases: R,
    store: Arc<dyn ReleaseStore>,
    audio: F,
}

impl<R: ReleaseSource, F: FingerprintSource> LiveProviders<R, F> {
    pub fn new(releases: R, store: Arc<dyn ReleaseStore>, audio: F) -> Self {
        Self {
            releases,
            store,
            audio,
        }
    }

    async fn recall(&self, facts: &LocalAlbumFacts) -> Result<RecallResult, SourceError> {
        let local = local_album(facts, &HashMap::new());
        let mut releases: Vec<Release> = Vec::new();
        if let Some(tagged) = local.tagged_release()
            && let Some(release) = self.release(&tagged).await?
        {
            let matched = match_release(&local, &release, &HashMap::new());
            let strong = matched.conflicts.is_empty() && matched.library_distance() <= STRONG;
            releases.push(release);
            if strong {
                return Ok(RecallResult {
                    releases,
                    ..RecallResult::default()
                });
            }
        }

        let artist = if facts.is_compilation || is_various(&facts.album_artist_name) {
            ""
        } else {
            facts.album_artist_name.as_str()
        };
        let mut hits = Vec::new();
        if !facts.title.trim().is_empty() {
            hits = self.releases.search(&facts.title, artist).await?;
            if hits.is_empty()
                && let Some(stripped) = strip_edition_suffix(&facts.title)
            {
                hits = self.releases.search(stripped, artist).await?;
            }
        }
        for id in pick_hits(&hits, local.tracks.len(), &releases) {
            if let Some(release) = self.release(&id).await? {
                push_new(&mut releases, release);
            }
        }

        let recording_aliases = self.resolve_aliases(&local, &releases).await?;
        let matches: Vec<_> = releases
            .iter()
            .map(|release| match_release(&local, release, &recording_aliases))
            .collect();
        let mut fingerprint_support = HashMap::new();
        if should_fingerprint(&local, &matches) {
            let heard = self.audio.identify(&facts.tracks).await;
            for id in common_releases(&heard.releases, heard.recordings.len().max(1)) {
                if releases.iter().any(|release| release.answers_to(&id)) {
                    continue;
                }
                if let Some(release) = self.release(&id).await? {
                    push_new(&mut releases, release);
                }
            }
            fingerprint_support = heard.recordings;
        }
        Ok(RecallResult {
            releases,
            fingerprint_support,
            recording_aliases,
            ..RecallResult::default()
        })
    }

    /// A release from the store when fresh, else from the source.
    async fn release(&self, mbid: &str) -> Result<Option<Release>, SourceError> {
        if let Some(stored) = self.store.release(mbid, Some(RELEASE_FRESH_SECS)) {
            return Ok(Some(stored));
        }
        let fetched = self.releases.release(mbid).await?;
        if let Some(release) = &fetched {
            self.store.save_release(release);
        }
        Ok(fetched)
    }

    /// Map file recording MBIDs that no candidate carries to the ids
    /// MusicBrainz merged them into, when it did.
    async fn resolve_aliases(
        &self,
        local: &LocalAlbum,
        releases: &[Release],
    ) -> Result<HashMap<String, String>, SourceError> {
        let mut aliases = HashMap::new();
        if releases.is_empty() {
            return Ok(aliases);
        }
        let unknown: Vec<String> = local
            .tracks
            .iter()
            .filter_map(|track| track.recording_mbid.as_deref())
            .map(str::to_ascii_lowercase)
            .filter(|mbid| {
                !releases.iter().any(|release| {
                    release
                        .tracks
                        .iter()
                        .any(|track| track.recording_id.eq_ignore_ascii_case(mbid))
                })
            })
            .collect();
        for mbid in unknown.into_iter().take(ALIAS_LOOKUPS) {
            if let Some(canonical) = self.releases.canonical_recording(&mbid).await?
                && canonical != mbid
            {
                aliases.insert(mbid, canonical);
            }
        }
        Ok(aliases)
    }
}

impl<R: ReleaseSource, F: FingerprintSource> IdentifyProviders for LiveProviders<R, F> {
    fn recall_candidates(
        &self,
        facts: &LocalAlbumFacts,
        _limit: u32,
    ) -> std::pin::Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        let facts = facts.clone();
        Box::pin(async move {
            let result = match self.recall(&facts).await {
                Ok(result) => result,
                Err(SourceError(error)) => {
                    tracing::info!(album = facts.local_album_id, %error, "identify recall deferred");
                    RecallResult {
                        provider_deferred: true,
                        failure_code: Some("musicbrainz_unavailable".to_owned()),
                        ..RecallResult::default()
                    }
                }
            };
            RecallOutcome {
                result,
                recall_criticality: Some(Criticality::IdentityCritical),
            }
        })
    }
}

fn push_new(releases: &mut Vec<Release>, release: Release) {
    if !releases.iter().any(|known| known.id == release.id) {
        releases.push(release);
    }
}

fn is_various(name: &str) -> bool {
    matches!(fold(name).as_str(), "variousartists" | "various" | "va")
}

/// Drop a trailing "(Deluxe Edition)"-style suffix, if there is one.
fn strip_edition_suffix(title: &str) -> Option<&str> {
    const WORDS: [&str; 9] = [
        "edition",
        "deluxe",
        "remaster",
        "expanded",
        "anniversary",
        "bonus",
        "special",
        "version",
        "reissue",
    ];
    let trimmed = title.trim_end();
    let open = match trimmed.chars().last()? {
        ')' => '(',
        ']' => '[',
        _ => return None,
    };
    let start = trimmed.rfind(open)?;
    let inside = trimmed[start..].to_lowercase();
    let head = trimmed[..start].trim_end();
    (WORDS.iter().any(|word| inside.contains(word)) && !head.is_empty()).then_some(head)
}

/// The hits worth a tracklist fetch: big enough to hold the files, best
/// search score first, then the closest track count.
fn pick_hits(hits: &[ReleaseHit], local_tracks: usize, known: &[Release]) -> Vec<String> {
    let mut ranked: Vec<&ReleaseHit> = hits
        .iter()
        .filter(|hit| {
            hit.track_count
                .is_none_or(|count| count as usize + 2 >= local_tracks)
        })
        .filter(|hit| !known.iter().any(|release| release.answers_to(&hit.id)))
        .collect();
    ranked.sort_by(|a, b| {
        b.score.cmp(&a.score).then_with(|| {
            let gap = |hit: &ReleaseHit| {
                hit.track_count
                    .map_or(usize::MAX, |count| (count as usize).abs_diff(local_tracks))
            };
            gap(a).cmp(&gap(b))
        })
    });
    let mut per_group: HashMap<&str, usize> = HashMap::new();
    for release in known {
        *per_group
            .entry(release.release_group_id.as_str())
            .or_default() += 1;
    }
    let mut picked = Vec::new();
    for hit in ranked {
        if picked.len() >= FETCH_LIMIT {
            break;
        }
        let taken = per_group.entry(hit.release_group_id.as_str()).or_default();
        if *taken >= PER_GROUP {
            continue;
        }
        *taken += 1;
        picked.push(hit.id.clone());
    }
    picked
}

/// Releases most printed tracks point at, most common first.
fn common_releases(heard: &HashMap<String, Vec<String>>, printed: usize) -> Vec<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for releases in heard.values() {
        for release in releases {
            *counts.entry(release.as_str()).or_default() += 1;
        }
    }
    let mut common: Vec<(&str, usize)> = counts
        .into_iter()
        .filter(|(_, count)| *count as f64 > COMMON_RELEASE_SHARE * printed as f64)
        .collect();
    common.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    common
        .into_iter()
        .take(FINGERPRINT_RELEASES)
        .map(|(id, _)| id.to_owned())
        .collect()
}

/// Scripted providers for tests: no network, fully deterministic.
#[derive(Debug, Default)]
pub struct FakeProviders {
    pub recall: std::sync::Mutex<Option<RecallResult>>,
}

impl FakeProviders {
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_recall(result: RecallResult) -> Self {
        Self {
            recall: std::sync::Mutex::new(Some(result)),
        }
    }

    /// Replace the scripted recall. Test wiring uses this to
    /// seed case-specific recall after scan assigns real track ids.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_recall(&self, result: RecallResult) {
        if let Ok(mut slot) = self.recall.lock() {
            *slot = Some(result);
        }
    }
}

impl IdentifyProviders for FakeProviders {
    fn recall_candidates(
        &self,
        _facts: &LocalAlbumFacts,
        _limit: u32,
    ) -> std::pin::Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        let result = self
            .recall
            .lock()
            .map(|slot| slot.clone().unwrap_or_default())
            .unwrap_or_default();
        Box::pin(async move {
            RecallOutcome {
                result,
                recall_criticality: Some(Criticality::IdentityCritical),
            }
        })
    }
}
