//! The MusicBrainz half of unified search: artist, release-group and
//! recording buckets, cached for an hour, with the kept answer standing in
//! when MusicBrainz is down.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use droppedneedle_providers::listenbrainz::ListenBrainzCredentials;
use droppedneedle_providers::musicbrainz::{Criticality, hit_score};
use droppedneedle_providers::{
    CacheKey, Fetched, IntegrationStatus, Policy, RequestPriority, Source, record_current, scoped,
};

use super::error::CatalogError;
use super::mapping;
use super::{Catalog, mb_search_error};
use crate::discover::listening::shared::{self, Lender};

/// How long a search waits for ListenBrainz's listen counts, after
/// MusicBrainz has answered. Counts that miss it leave the text order.
pub const LISTENS_DEADLINE: Duration = Duration::from_secs(2);

/// Which MusicBrainz bucket a hit came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteKind {
    /// An artist.
    Artist,
    /// A release group.
    Album,
    /// A recording.
    Track,
}

/// One MusicBrainz search hit, before library flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteHit {
    /// Bucket.
    pub kind: RemoteKind,
    /// Artist or release-group MBID.
    pub mbid: String,
    /// Name or title.
    pub title: String,
    /// Credited artist, for albums.
    pub artist: Option<String>,
    /// First release year, for albums.
    pub year: Option<i32>,
    /// Disambiguation comment.
    pub disambiguation: Option<String>,
    /// Artist type, or `Album + Live` style type label.
    pub type_info: Option<String>,
    /// MusicBrainz relevance score, 0-100.
    pub score: i32,
    /// How many releases the group holds, for albums; `None` when
    /// MusicBrainz did not say. Answers kept from before it was read have
    /// none.
    #[serde(default)]
    pub releases: Option<u32>,
    /// What requesting the song needs, for recordings.
    #[serde(default)]
    pub track: Option<RemoteTrack>,
}

/// The release a recording search hit is shown and requested under: its
/// first official release, else its first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteTrack {
    /// The release's group, when MusicBrainz named one.
    pub release_group_mbid: Option<String>,
    /// The release.
    pub release_mbid: String,
    /// The release's title.
    pub album_title: Option<String>,
    /// Recording length, whole seconds.
    pub duration_seconds: Option<i64>,
    /// First credited artist.
    pub artist_mbid: Option<String>,
}

/// One bucket's hits as cached.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteHits {
    /// Hits, MusicBrainz order.
    pub items: Vec<RemoteHit>,
}

/// How a bucket's MusicBrainz leg went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteState {
    /// MusicBrainz answered.
    Ok,
    /// MusicBrainz failed; a stale copy stands in.
    Stale,
    /// MusicBrainz missed the deadline and no stale copy exists.
    Timeout,
    /// MusicBrainz failed and no stale copy exists.
    Error,
}

/// One bucket's answer.
#[derive(Debug, Clone)]
pub struct RemoteBucket {
    /// Hits (possibly stale), empty on failure.
    pub hits: Vec<RemoteHit>,
    /// How the MusicBrainz leg went.
    pub state: RemoteState,
}

/// A bucket's hits as the cache keeps them: none is "nothing there", which
/// is asked again sooner.
fn found_hits(items: Vec<RemoteHit>) -> Fetched<RemoteHits> {
    if items.is_empty() {
        Fetched::Missing
    } else {
        Fetched::Found(RemoteHits { items })
    }
}

impl Catalog {
    /// Search MusicBrainz artists: placeholder
    /// artists filtered out, at most `limit` hits.
    pub async fn search_artists_remote(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
        deadline: Duration,
    ) -> RemoteBucket {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        let parts = [
            namespace.as_str(),
            query.trim(),
            &limit.to_string(),
            &offset.to_string(),
        ];
        let key = CacheKey::hashed(Source::MusicBrainz, "artist_search", &parts);
        let catalog = self.clone();
        let text = query.trim().to_owned();
        let fetch = move |priority| async move {
            let (client, _) = catalog.upstream().musicbrainz(priority);
            let wide = (limit * 2).clamp(25, 100);
            let page = client
                .search_artists_text(&text, wide, offset, Criticality::IdentityCritical)
                .await
                .map_err(mb_search_error)?;
            let mut seen = HashSet::new();
            let items: Vec<RemoteHit> = page
                .items
                .into_iter()
                .filter(|hit| seen.insert(hit.id.to_ascii_lowercase()))
                .filter_map(|hit| {
                    let name = hit
                        .name
                        .clone()
                        .unwrap_or_else(|| "Unknown Artist".to_owned());
                    if mapping::is_filtered_artist(&hit.id, &name) {
                        return None;
                    }
                    Some(RemoteHit {
                        kind: RemoteKind::Artist,
                        title: name,
                        artist: None,
                        year: None,
                        disambiguation: hit.disambiguation.clone().filter(|text| !text.is_empty()),
                        type_info: hit.artist_type.clone(),
                        score: i32::try_from(hit_score(hit.score, hit.ext_score)).unwrap_or(0),
                        releases: None,
                        track: None,
                        mbid: hit.id,
                    })
                })
                .take(limit as usize)
                .collect();
            Ok(found_hits(items))
        };
        self.remote_bucket(key, deadline, fetch).await
    }

    /// Search MusicBrainz release groups for a page of `limit`, filtered by
    /// the release-type preferences. The page is asked wider than `limit`
    /// so the filter leaves enough, and every hit it keeps comes back: the
    /// caller cuts, after ranking or in MusicBrainz's order.
    pub async fn search_albums_remote(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
        deadline: Duration,
    ) -> RemoteBucket {
        let preferences = self.upstream().settings().preferences();
        let primary = mapping::type_set(&preferences.primary_types);
        let secondary = mapping::type_set(&preferences.secondary_types);
        let mut primary_key: Vec<&String> = primary.iter().collect();
        primary_key.sort();
        let mut secondary_key: Vec<&String> = secondary.iter().collect();
        secondary_key.sort();
        let filter_key = format!(
            "{}|{}",
            primary_key
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(","),
            secondary_key
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(",")
        );
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        let parts = [
            namespace.as_str(),
            query.trim(),
            &limit.to_string(),
            &offset.to_string(),
            filter_key.as_str(),
        ];
        let key = CacheKey::hashed(Source::MusicBrainz, "album_search", &parts);
        let catalog = self.clone();
        let text = query.trim().to_owned();
        let fetch = move |priority| async move {
            let (client, _) = catalog.upstream().musicbrainz(priority);
            let wide = (limit + limit / 2).clamp(25, 100);
            let page = client
                .search_release_groups_text(&text, wide, offset, Criticality::IdentityCritical)
                .await
                .map_err(mb_search_error)?;
            let mut seen = HashSet::new();
            let items: Vec<RemoteHit> = page
                .items
                .into_iter()
                .filter(|hit| seen.insert(hit.id.to_ascii_lowercase()))
                .filter(|hit| {
                    mapping::should_include_release(
                        hit.primary_type.as_deref(),
                        &hit.secondary_types,
                        Some(&primary),
                        Some(&secondary),
                        true,
                    )
                })
                .map(|hit| RemoteHit {
                    kind: RemoteKind::Album,
                    title: hit
                        .title
                        .clone()
                        .unwrap_or_else(|| "Unknown Album".to_owned()),
                    artist: droppedneedle_providers::musicbrainz::credit_display_name(
                        &hit.artist_credit,
                    )
                    .map(str::to_owned),
                    year: mapping::year_of(hit.first_release_date.as_deref()),
                    disambiguation: hit.disambiguation.clone().filter(|text| !text.is_empty()),
                    type_info: mapping::type_info(
                        hit.primary_type.as_deref(),
                        &hit.secondary_types,
                    ),
                    score: i32::try_from(hit_score(hit.score, hit.ext_score)).unwrap_or(0),
                    releases: hit.count,
                    track: None,
                    mbid: hit.id,
                })
                .collect();
            Ok(found_hits(items))
        };
        self.remote_bucket(key, deadline, fetch).await
    }

    /// Search MusicBrainz recordings for a page of `limit`: songs, not
    /// videos, each under one of its releases. A recording on no release
    /// is nothing to request, so it is left out.
    pub async fn search_tracks_remote(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
        deadline: Duration,
    ) -> RemoteBucket {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        let parts = [
            namespace.as_str(),
            query.trim(),
            &limit.to_string(),
            &offset.to_string(),
        ];
        let key = CacheKey::hashed(Source::MusicBrainz, "track_search", &parts);
        let catalog = self.clone();
        let text = query.trim().to_owned();
        let fetch = move |priority| async move {
            let (client, _) = catalog.upstream().musicbrainz(priority);
            let wide = (limit + limit / 2).clamp(25, 100);
            let page = client
                .search_recordings_text(&text, wide, offset, Criticality::IdentityCritical)
                .await
                .map_err(mb_search_error)?;
            let mut seen = HashSet::new();
            let items: Vec<RemoteHit> = page
                .items
                .into_iter()
                .filter(|hit| seen.insert(hit.id.to_ascii_lowercase()))
                .filter(|hit| hit.video != Some(true))
                .filter_map(|hit| {
                    let release = hit
                        .releases
                        .iter()
                        .find(|release| release.status.as_deref() == Some("Official"))
                        .or(hit.releases.first())?;
                    let group = release.release_group.as_ref();
                    Some(RemoteHit {
                        kind: RemoteKind::Track,
                        title: hit
                            .title
                            .clone()
                            .unwrap_or_else(|| "Unknown Track".to_owned()),
                        artist: droppedneedle_providers::musicbrainz::credit_display_name(
                            &hit.artist_credit,
                        )
                        .map(str::to_owned),
                        year: mapping::year_of(release.date.as_deref()),
                        disambiguation: None,
                        type_info: None,
                        score: i32::try_from(hit_score(hit.score, hit.ext_score)).unwrap_or(0),
                        releases: None,
                        track: Some(RemoteTrack {
                            release_group_mbid: group.map(|group| group.id.clone()),
                            release_mbid: release.id.clone(),
                            album_title: release.title.clone(),
                            duration_seconds: hit
                                .length
                                .and_then(|length| i64::try_from(length / 1000).ok()),
                            artist_mbid: hit
                                .artist_credit
                                .first()
                                .map(|credit| credit.artist.id.clone()),
                        }),
                        mbid: hit.id,
                    })
                })
                .collect();
            Ok(found_hits(items))
        };
        self.remote_bucket(key, deadline, fetch).await
    }

    /// Run one bucket under its deadline. A failed search falls back to
    /// the kept answer, as does a missed deadline; either reads as stale.
    async fn remote_bucket<F, Fut>(
        &self,
        key: CacheKey,
        deadline: Duration,
        fetch: F,
    ) -> RemoteBucket
    where
        F: FnOnce(RequestPriority) -> Fut + Clone + Send + 'static,
        Fut: std::future::Future<Output = Result<Fetched<RemoteHits>, CatalogError>>
            + Send
            + 'static,
    {
        let (outcome, context) = scoped(tokio::time::timeout(
            deadline,
            self.cached(&key, Policy::SEARCH, RequestPriority::UserInitiated, fetch),
        ))
        .await;
        // The cache notes a kept answer served in place of a failed search
        // as a degraded MusicBrainz.
        let summary = context.summary();
        let deterministic = context.deterministic_sources();
        for (source, status) in &summary {
            record_current(source, *status, deterministic.contains(source));
        }
        let source = key.source().as_str();
        let state = match outcome {
            Ok(Ok(hits)) => {
                let stale = summary.get(source) == Some(&IntegrationStatus::Degraded);
                return RemoteBucket {
                    hits: hits.unwrap_or_default().items,
                    state: if stale {
                        RemoteState::Stale
                    } else {
                        RemoteState::Ok
                    },
                };
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "musicbrainz search failed");
                RemoteState::Error
            }
            Err(_) => {
                tracing::warn!("musicbrainz search missed its deadline");
                if let Some(Some(hits)) = self.upstream().cache().peek::<RemoteHits>(&key).await {
                    record_current(source, IntegrationStatus::Degraded, false);
                    return RemoteBucket {
                        hits: hits.items,
                        state: RemoteState::Stale,
                    };
                }
                RemoteState::Timeout
            }
        };
        record_current(source, IntegrationStatus::Error, false);
        RemoteBucket {
            hits: Vec::new(),
            state,
        }
    }

    /// Library flags for search rows: owned artists, then owned and
    /// requested albums, then owned and requested songs (all lowercase
    /// MBIDs).
    pub async fn search_flags(&self, ids: SearchIds<'_>) -> SearchFlags {
        let (artists, albums, tracks) = tokio::join!(
            self.artist_flags(ids.artists),
            self.album_flags(ids.albums),
            self.track_flags(ids.tracks)
        );
        SearchFlags {
            artists,
            albums,
            tracks,
        }
    }
}

impl Catalog {
    /// ListenBrainz listen counts for release groups and recordings
    /// (lowercase MBIDs), read anonymously under [`LISTENS_DEADLINE`] from
    /// the rows discover keeps, asking only for what no kept row holds.
    /// Empty while ListenBrainz reads are off, and for whatever failed or
    /// missed the deadline: counts rank, they never fail a search.
    pub async fn listen_counts(
        &self,
        release_groups: &[String],
        recordings: &[String],
    ) -> (HashMap<String, i64>, HashMap<String, i64>) {
        let Some(client) = self.upstream().listenbrainz() else {
            return (HashMap::new(), HashMap::new());
        };
        let cache = self.upstream().cache();
        let priority = RequestPriority::UserInitiated;
        let groups =
            shared::listen_counts(cache, &client, release_groups, &Lender::Anonymous, priority);
        let client = client.clone();
        let tracks = cache.fetch_each(
            recordings,
            recording_listen_count_key,
            Policy::LISTENBRAINZ_RECORDING,
            |missing| async move {
                client
                    .recording_popularity(&missing, &ListenBrainzCredentials::default(), priority)
                    .await
            },
        );
        let counts = tokio::time::timeout(LISTENS_DEADLINE, async { tokio::join!(groups, tracks) });
        let lower = |counts: HashMap<String, i64>| {
            counts
                .into_iter()
                .map(|(mbid, count)| (mbid.to_ascii_lowercase(), count))
                .collect()
        };
        match counts.await {
            Ok((Ok(groups), Ok(tracks))) => (lower(groups), lower(tracks)),
            Ok((groups, tracks)) => {
                for error in [groups.err(), tracks.err()].into_iter().flatten() {
                    tracing::warn!(%error, "listenbrainz counts failed; ranking by text");
                }
                (HashMap::new(), HashMap::new())
            }
            Err(_) => {
                tracing::warn!("listenbrainz counts missed the deadline; ranking by text");
                (HashMap::new(), HashMap::new())
            }
        }
    }
}

/// The cache key of one recording's listen count.
fn recording_listen_count_key(recording_mbid: &str) -> CacheKey {
    CacheKey::new(
        Source::ListenBrainz,
        &["rec_popularity", &recording_mbid.trim().to_lowercase()],
    )
}

/// The MusicBrainz ids of one search's rows, per bucket.
#[derive(Debug, Clone, Copy)]
pub struct SearchIds<'a> {
    /// Artist MBIDs.
    pub artists: &'a [String],
    /// Release-group MBIDs.
    pub albums: &'a [String],
    /// Recording MBIDs.
    pub tracks: &'a [String],
}

/// What the library says about one search's rows: owned artists, and for
/// albums and songs the owned ids and the ids with an open request.
#[derive(Debug, Clone, Default)]
pub struct SearchFlags {
    /// Owned artists.
    pub artists: HashSet<String>,
    /// Owned albums, requested albums.
    pub albums: (HashSet<String>, HashSet<String>),
    /// Owned songs, requested songs.
    pub tracks: (HashSet<String>, HashSet<String>),
}
