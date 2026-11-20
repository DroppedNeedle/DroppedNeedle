//! The MusicBrainz half of unified search: artist, release-group and
//! recording buckets, cached for an hour, with the kept answer standing in
//! when MusicBrainz is down.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use droppedneedle_providers::listenbrainz::ListenBrainzCredentials;
use droppedneedle_providers::musicbrainz::{
    Criticality, OnEmpty, RecordingSearchHit, RecordingSearchRelease, ReleaseGroupRef, SearchTerm,
    forgiving_term, hit_score, recording_release_group_rank,
};
use droppedneedle_providers::{
    CacheKey, Fetched, IntegrationStatus, Policy, ProviderError, RequestPriority, Source,
    record_current, scoped,
};

use super::error::CatalogError;
use super::mapping;
use super::upstream::CatalogListenBrainz;
use super::{Catalog, mb_search_error};
use crate::discover::listening::shared::{self, Lender};

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
    /// Artist, release-group or recording MBID.
    pub mbid: String,
    /// Name or title.
    pub title: String,
    /// Credited artist, for albums and songs.
    pub artist: Option<String>,
    /// First release year for albums; for songs, the year of the release
    /// they are shown under.
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

/// The release a recording search hit is shown and requested under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteTrack {
    /// The release's group.
    pub release_group_mbid: String,
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

/// Recording search hits as songs to show: one per recording, not videos,
/// each under the release a track request tries first ([`best_release`]).
/// A recording on no release in a group is nothing to request, so it is
/// left out.
pub(super) fn song_hits(hits: Vec<RecordingSearchHit>) -> Vec<RemoteHit> {
    let mut seen = HashSet::new();
    hits.into_iter()
        .filter(|hit| seen.insert(hit.id.to_ascii_lowercase()))
        .filter(|hit| hit.video != Some(true))
        .filter_map(|hit| {
            let (release, group) = best_release(&hit.releases)?;
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
                    release_group_mbid: group.id.clone(),
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
        .collect()
}

/// A bucket's hits as the cache keeps them: none is "nothing there", which
/// is asked again sooner.
pub(super) fn found_hits(items: Vec<RemoteHit>) -> Fetched<RemoteHits> {
    if items.is_empty() {
        Fetched::Missing
    } else {
        Fetched::Found(RemoteHits { items })
    }
}

impl Catalog {
    /// Search MusicBrainz artists for a page of `limit`, placeholder
    /// artists filtered out, in MusicBrainz's order; the caller cuts. A
    /// first page is one answer whatever the limit up to [`ARTIST_WIDTH`],
    /// so typeahead, the All tab and "see all" share it.
    pub async fn search_artists_remote(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
        deadline: Duration,
        on_empty: OnEmpty,
    ) -> RemoteBucket {
        let started = tokio::time::Instant::now();
        let width = page_width(ARTIST_WIDTH, offset, (limit * 2).clamp(25, 100));
        let typed = self
            .artists_page(query, width, offset, SearchTerm::Typed, deadline)
            .await;
        if !asks_again(query, offset, on_empty, &typed) {
            return typed;
        }
        let left = deadline.saturating_sub(started.elapsed());
        self.artists_page(query, width, offset, SearchTerm::Forgiving, left)
            .await
    }

    /// One page of an artist search for `term`, cached under its own key.
    async fn artists_page(
        &self,
        query: &str,
        width: u32,
        offset: u32,
        term: SearchTerm,
        deadline: Duration,
    ) -> RemoteBucket {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        let width_part = width.to_string();
        let offset_part = offset.to_string();
        let mut parts = vec![
            namespace.as_str(),
            query.trim(),
            width_part.as_str(),
            offset_part.as_str(),
        ];
        parts.extend(term_key(term));
        let key = CacheKey::hashed(Source::MusicBrainz, "artist_search", &parts);
        let catalog = self.clone();
        let text = query.trim().to_owned();
        let fetch = move |priority| async move {
            let (client, _) = catalog.upstream().musicbrainz(priority);
            let page = client
                .search_artists_text(&text, width, offset, term, Criticality::IdentityCritical)
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
                .collect();
            Ok(found_hits(items))
        };
        self.remote_bucket(key, deadline, fetch).await
    }

    /// Search MusicBrainz release groups for a page of `limit`, keeping the
    /// preferred primary types. Every secondary type shows: a soundtrack or
    /// a compilation someone searches for is what they mean, whatever the
    /// discography and new-release feeds leave out. The page is asked wider
    /// than `limit` so the filter leaves enough, and every hit it keeps
    /// comes back: the caller cuts, after ranking or in MusicBrainz's order.
    /// A first page is one answer whatever the limit up to [`GROUP_WIDTH`].
    pub async fn search_albums_remote(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
        deadline: Duration,
        on_empty: OnEmpty,
    ) -> RemoteBucket {
        let started = tokio::time::Instant::now();
        let width = page_width(GROUP_WIDTH, offset, (limit + limit / 2).clamp(25, 100));
        let typed = self
            .albums_page(query, width, offset, SearchTerm::Typed, deadline)
            .await;
        if !asks_again(query, offset, on_empty, &typed) {
            return typed;
        }
        let left = deadline.saturating_sub(started.elapsed());
        self.albums_page(query, width, offset, SearchTerm::Forgiving, left)
            .await
    }

    /// One page of a release-group search for `term`, cached under its own
    /// key.
    async fn albums_page(
        &self,
        query: &str,
        width: u32,
        offset: u32,
        term: SearchTerm,
        deadline: Duration,
    ) -> RemoteBucket {
        let preferences = self.upstream().settings().preferences();
        let primary = mapping::type_set(&preferences.primary_types);
        let mut primary_key: Vec<&String> = primary.iter().collect();
        primary_key.sort();
        let filter_key = primary_key
            .iter()
            .map(|value| value.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        let width_part = width.to_string();
        let offset_part = offset.to_string();
        let mut parts = vec![
            namespace.as_str(),
            query.trim(),
            width_part.as_str(),
            offset_part.as_str(),
            filter_key.as_str(),
        ];
        parts.extend(term_key(term));
        let key = CacheKey::hashed(Source::MusicBrainz, "album_search", &parts);
        let catalog = self.clone();
        let text = query.trim().to_owned();
        let fetch = move |priority| async move {
            let (client, _) = catalog.upstream().musicbrainz(priority);
            let page = client
                .search_release_groups_text(
                    &text,
                    width,
                    offset,
                    term,
                    Criticality::IdentityCritical,
                )
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
                        None,
                        false,
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
    /// videos, each under the release a track request tries first
    /// ([`best_release`]). A recording on no release in a group is nothing
    /// to request, so it is left out. A first page is one answer whatever
    /// the limit up to [`GROUP_WIDTH`].
    pub async fn search_tracks_remote(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
        deadline: Duration,
        on_empty: OnEmpty,
    ) -> RemoteBucket {
        let started = tokio::time::Instant::now();
        let width = page_width(GROUP_WIDTH, offset, (limit + limit / 2).clamp(25, 100));
        let typed = self
            .tracks_page(query, width, offset, SearchTerm::Typed, deadline)
            .await;
        if !asks_again(query, offset, on_empty, &typed) {
            return typed;
        }
        let left = deadline.saturating_sub(started.elapsed());
        self.tracks_page(query, width, offset, SearchTerm::Forgiving, left)
            .await
    }

    /// One page of a recording search for `term`, cached under its own key.
    async fn tracks_page(
        &self,
        query: &str,
        width: u32,
        offset: u32,
        term: SearchTerm,
        deadline: Duration,
    ) -> RemoteBucket {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        let width_part = width.to_string();
        let offset_part = offset.to_string();
        let mut parts = vec![
            namespace.as_str(),
            query.trim(),
            width_part.as_str(),
            offset_part.as_str(),
        ];
        parts.extend(term_key(term));
        let key = CacheKey::hashed(Source::MusicBrainz, "track_search", &parts);
        let catalog = self.clone();
        let text = query.trim().to_owned();
        let fetch = move |priority| async move {
            let (client, _) = catalog.upstream().musicbrainz(priority);
            let page = client
                .search_recordings_text(&text, width, offset, term, Criticality::IdentityCritical)
                .await
                .map_err(mb_search_error)?;
            Ok(found_hits(song_hits(page.items)))
        };
        self.remote_bucket(key, deadline, fetch).await
    }

    /// Run one bucket under its deadline. A failed search falls back to
    /// the kept answer, as does a missed deadline; either reads as stale.
    pub(super) async fn remote_bucket<F, Fut>(
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
    /// requested albums, then requested songs (all lowercase MBIDs). A
    /// song the library holds is known by the library row search attaches
    /// to it, not by a flag.
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

/// How many artists a first search page asks MusicBrainz for, whatever
/// the caller shows, so every view of a query's first page shares one
/// answer.
const ARTIST_WIDTH: u32 = 48;
/// How many release groups or recordings a first search page asks for.
const GROUP_WIDTH: u32 = 36;

/// The page width to ask MusicBrainz for: a first page at least `first`,
/// a later one what the caller's page needs.
fn page_width(first: u32, offset: u32, needed: u32) -> u32 {
    if offset == 0 {
        needed.max(first)
    } else {
        needed
    }
}

/// The cache key part of a search term: none for the words as typed, so
/// every caller shares that answer, and its own for the forgiving retry.
fn term_key(term: SearchTerm) -> Option<&'static str> {
    match term {
        SearchTerm::Typed => None,
        SearchTerm::Forgiving => Some("forgiving"),
    }
}

/// Whether a first page whose words as typed matched nothing asks again
/// with the forgiving term: only when the caller may, MusicBrainz answered,
/// and there is a second guess to make.
fn asks_again(query: &str, offset: u32, on_empty: OnEmpty, typed: &RemoteBucket) -> bool {
    on_empty == OnEmpty::AskAgain
        && offset == 0
        && typed.state == RemoteState::Ok
        && typed.hits.is_empty()
        && forgiving_term(query.trim()).is_some()
}

/// The release a song search hit is shown and requested under, with its
/// group, in the order a track request tries a recording's releases:
/// official, not live or remixed, no secondary type, album before EP before
/// single, earliest, then by id. So the album a row names is the one its
/// request starts from. `None` when no release names its group.
fn best_release(
    releases: &[RecordingSearchRelease],
) -> Option<(&RecordingSearchRelease, &ReleaseGroupRef)> {
    releases
        .iter()
        .filter_map(|release| Some((release, release.release_group.as_ref()?)))
        .filter(|(_, group)| !group.id.trim().is_empty())
        .min_by_key(|(release, group)| {
            let (official, version_changed, primary, secondary_penalty, date, group) =
                recording_release_group_rank(
                    release.status.as_deref(),
                    &group.secondary_types,
                    group.primary_type.as_deref(),
                    release.date.as_deref(),
                    &group.id,
                );
            (
                official,
                version_changed,
                secondary_penalty,
                primary,
                date,
                group,
                release.id.clone(),
            )
        })
}

impl Catalog {
    /// ListenBrainz listen counts for release groups and recordings
    /// (lowercase MBIDs), from the cache only, fresh or kept: search never
    /// waits on ListenBrainz. With `fill`, the ids with nothing kept are
    /// fetched in the background ([`Self::fill_listen_counts`]), so the next
    /// search ranks by them and a hover's enrichment finds them kept. A
    /// search fills once: not while its songs are still on their way.
    /// Empty, and nothing fetched, while ListenBrainz reads are off.
    pub async fn cached_listen_counts(
        &self,
        release_groups: &[String],
        recordings: &[String],
        fill: bool,
    ) -> (HashMap<String, i64>, HashMap<String, i64>) {
        let Some(client) = self.upstream().listenbrainz() else {
            return (HashMap::new(), HashMap::new());
        };
        let (groups, missing_groups) = self
            .kept_counts(release_groups, shared::listen_count_key)
            .await;
        let (tracks, missing_tracks) = self
            .kept_counts(recordings, recording_listen_count_key)
            .await;
        if fill {
            self.fill_listen_counts(client, missing_groups, missing_tracks);
        }
        (groups, tracks)
    }

    /// The counts the cache keeps for `ids`, fresh or not, by lowercase id,
    /// read in one batch, and the ids it has no row for.
    async fn kept_counts(
        &self,
        ids: &[String],
        key_of: fn(&str) -> CacheKey,
    ) -> (HashMap<String, i64>, Vec<String>) {
        let keys: Vec<CacheKey> = ids.iter().map(|id| key_of(id)).collect();
        let rows = self.upstream().cache().peek_many::<i64>(&keys).await;
        let mut counts = HashMap::new();
        let mut missing = Vec::new();
        for (id, row) in ids.iter().zip(rows) {
            match row {
                Some(Some(count)) => {
                    counts.insert(id.to_ascii_lowercase(), count);
                }
                Some(None) => {}
                None => missing.push(id.clone()),
            }
        }
        (counts, missing)
    }

    /// Fetch and keep the listen counts of `release_groups` and
    /// `recordings`, anonymously and at background priority, on a task of
    /// its own that logs its own end. An id already being fetched is
    /// skipped. A fetch that fails stores nothing, so a later search asks
    /// again: while fetches fail, at most one starts every
    /// [`LISTENS_RETRY`], and the failure is logged once until one
    /// succeeds.
    fn fill_listen_counts(
        &self,
        client: CatalogListenBrainz,
        release_groups: Vec<String>,
        recordings: Vec<String>,
    ) {
        {
            let mut failing = self
                .inner
                .listens_failing
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if let Some(tried) = *failing {
                if tried.elapsed() < LISTENS_RETRY {
                    return;
                }
                *failing = Some(tokio::time::Instant::now());
            }
        }
        let claim = |ids: Vec<String>, kind: &str| -> Vec<String> {
            let mut warming = self
                .inner
                .warming
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let mut ids: Vec<String> = ids
                .into_iter()
                .map(|id| id.trim().to_ascii_lowercase())
                .filter(|id| !id.is_empty() && warming.insert(format!("listens:{kind}:{id}")))
                .collect();
            ids.sort();
            ids.dedup();
            ids
        };
        let groups = claim(release_groups, "group");
        let tracks = claim(recordings, "recording");
        if groups.is_empty() && tracks.is_empty() {
            return;
        }
        let catalog = self.clone();
        tokio::spawn(async move {
            let outcome = catalog.fetch_listen_counts(&client, &groups, &tracks).await;
            let was_failing = {
                let mut failing = catalog
                    .inner
                    .listens_failing
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                let was = failing.is_some();
                *failing = match outcome {
                    Ok(()) => None,
                    Err(_) => Some((*failing).unwrap_or_else(tokio::time::Instant::now)),
                };
                was
            };
            match outcome {
                Ok(()) if was_failing => {
                    tracing::info!("listenbrainz listen counts for search are back");
                }
                Err(error) if !was_failing => {
                    tracing::warn!(
                        %error,
                        "listenbrainz listen counts for search failed; search ranks by text"
                    );
                }
                _ => {}
            }
            let mut warming = catalog
                .inner
                .warming
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            for id in &groups {
                warming.remove(&format!("listens:group:{id}"));
            }
            for id in &tracks {
                warming.remove(&format!("listens:recording:{id}"));
            }
        });
    }

    /// One background fetch of the counts, groups then recordings, each
    /// kept row by row.
    async fn fetch_listen_counts(
        &self,
        client: &CatalogListenBrainz,
        release_groups: &[String],
        recordings: &[String],
    ) -> Result<(), ProviderError> {
        let cache = self.upstream().cache();
        let priority = RequestPriority::BackgroundSync;
        if !release_groups.is_empty() {
            shared::listen_counts(cache, client, release_groups, &Lender::Anonymous, priority)
                .await?;
        }
        if !recordings.is_empty() {
            cache
                .fetch_each(
                    recordings,
                    recording_listen_count_key,
                    Policy::LISTENBRAINZ_RECORDING,
                    |missing| async move {
                        client
                            .recording_popularity(
                                &missing,
                                &ListenBrainzCredentials::default(),
                                priority,
                            )
                            .await
                    },
                )
                .await?;
        }
        Ok(())
    }
}

/// How often a background fetch of search listen counts starts while they
/// keep failing.
const LISTENS_RETRY: Duration = Duration::from_secs(60);

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

/// What the library says about one search's rows: owned artists, owned and
/// requested albums, and requested songs.
#[derive(Debug, Clone, Default)]
pub struct SearchFlags {
    /// Owned artists.
    pub artists: HashSet<String>,
    /// Owned albums, requested albums.
    pub albums: (HashSet<String>, HashSet<String>),
    /// Requested songs.
    pub tracks: HashSet<String>,
}
