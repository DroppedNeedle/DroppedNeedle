//! [`QueueSources`] over the live providers: ListenBrainz, Last.fm,
//! MusicBrainz, Wikipedia and the user's Jellyfin account.
//!
//! Clients come from the catalog's [`Upstream`], built per call from the
//! settings as they are now; ListenBrainz is the one shared client. Each
//! ListenBrainz read takes the priority of the build that asked for it.
//! Repeated provider reads go through the shared provider cache, so a
//! rebuild minutes later does not pay the 1/s ListenBrainz pacing again.
//! ListenBrainz switching a popularity endpoint off (its "disabled due to high load"
//! answer and its anti-scraper refusal) marks popularity as down for a few
//! minutes, which lets Last.fm stand in (v2 `lb_popularity_degraded`). So
//! does a popularity read turned down for its rate, for the wait it names:
//! the build stops asking instead of spending every shelf's time bound on
//! retries.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Serialize, de::DeserializeOwned};

use super::sources::{
    AlbumRow, ArtistFacts, ArtistRow, JellyfinList, LastFmAlbum, LastFmFacts, MusicSource,
    QueueSources, ReleaseGroupFacts, SourceResult, StatsRange, UserMusic,
};
use crate::reads::catalog::mapping::should_include_release;
use crate::reads::catalog::upstream::{CatalogLastFm, UNREADABLE_NAMESPACE, Upstream, user_lastfm};
use crate::reads::discover::ports::{BoxFuture, ProviderFailure};
use droppedneedle_media::playback::listening::{ListenBrainzLinkStore, ListeningStore};
use droppedneedle_media::remotes::adapter::RemoteHandle;
use droppedneedle_media::remotes::connections::{ConnectionResolver, Reads, ResolveError};
use droppedneedle_media::remotes::models::SourceName;
use droppedneedle_providers::lastfm::LastFmCredentials;
use droppedneedle_providers::listenbrainz::{ListenBrainzClient, ListenBrainzCredentials};
use droppedneedle_providers::musicbrainz::models::{ArtistCreditName, Relation};
use droppedneedle_providers::musicbrainz::{Criticality, is_valid_mbid};
use droppedneedle_providers::{CacheKey, ProviderError, Providers, RequestPriority, Source};

/// How long a popularity refusal, or a rate limit that names no wait,
/// keeps Last.fm standing in.
const POPULARITY_DOWN_FOR: Duration = Duration::from_secs(10 * 60);
/// Cache lifetime of per-user listening statistics.
pub(in crate::reads::discover::adapters) const USER_STATS_TTL: Duration =
    Duration::from_secs(5 * 60);
/// Cache lifetime of artist-level and chart reads.
pub(in crate::reads::discover::adapters) const ARTIST_TTL: Duration = Duration::from_secs(60 * 60);

/// The live provider reads.
pub struct LiveSources {
    pub(in crate::reads::discover::adapters) upstream: Upstream,
    listenbrainz: ListenBrainzClient,
    links: Arc<dyn ListenBrainzLinkStore>,
    pub(in crate::reads::discover::adapters) jellyfin: Arc<ConnectionResolver>,
    pub(in crate::reads::discover::adapters) http: reqwest::Client,
    listening: ListeningStore,
    popularity_down_until: Mutex<Option<Instant>>,
}

impl LiveSources {
    /// Build over the catalog upstream.
    pub fn new(
        upstream: Upstream,
        providers: Arc<Providers>,
        http: reqwest::Client,
        links: Arc<dyn ListenBrainzLinkStore>,
        jellyfin: Arc<ConnectionResolver>,
        listening: ListeningStore,
    ) -> Self {
        Self {
            listenbrainz: providers.listenbrainz.clone(),
            upstream,
            links,
            jellyfin,
            http,
            listening,
            popularity_down_until: Mutex::new(None),
        }
    }

    /// The user's Jellyfin client, `None` when Jellyfin is not set up for
    /// them. Discover reads it in its background warm cycle as well as for
    /// a page, so its reads are not counted against the user read bound.
    pub(in crate::reads::discover::adapters) async fn jellyfin_client(
        &self,
        user_id: &str,
    ) -> SourceResult<Option<RemoteHandle>> {
        match self.jellyfin.resolve(user_id, SourceName::Jellyfin).await {
            Ok(resolved) => Ok(Some(self.jellyfin.client(
                &self.http,
                resolved,
                Reads::Background,
            ))),
            Err(ResolveError::NotConfigured) => Ok(None),
            Err(error) => Err(ProviderFailure::failed(error.to_string())),
        }
    }

    pub(in crate::reads::discover::adapters) fn lb(&self) -> &ListenBrainzClient {
        &self.listenbrainz
    }

    pub(in crate::reads::discover::adapters) async fn cached<T, Fut>(
        &self,
        key: CacheKey,
        ttl: Duration,
        fetch: Fut,
    ) -> SourceResult<T>
    where
        T: Serialize + DeserializeOwned,
        Fut: Future<Output = SourceResult<T>>,
    {
        let cache = self.upstream.cache();
        if let Some(hit) = cache.get_bytes(&key).await
            && let Ok(decoded) = serde_json::from_slice::<T>(&hit)
        {
            return Ok(decoded);
        }
        let fresh = fetch.await?;
        if let Ok(bytes) = serde_json::to_vec(&fresh) {
            cache.set_bytes(&key, bytes, ttl);
        }
        Ok(fresh)
    }

    /// The user's ListenBrainz identity, lending the token to reads that
    /// ListenBrainz gates for anonymous callers.
    pub(in crate::reads::discover::adapters) async fn credentials(
        &self,
        user_id: &str,
    ) -> ListenBrainzCredentials {
        let username = self.links.status(user_id).await.map(|link| link.username);
        let user_token = match username {
            Some(_) => self.links.token_for(user_id).await,
            None => None,
        };
        ListenBrainzCredentials {
            username,
            user_token,
        }
    }

    /// Mark popularity down for `wait`; a later end already set stays.
    fn mark_popularity_down(&self, wait: Duration) {
        if let Ok(mut until) = self.popularity_down_until.lock() {
            let end = Instant::now() + wait;
            *until = Some(until.map_or(end, |current| current.max(end)));
        }
    }

    pub(in crate::reads::discover::adapters) fn listenbrainz_result<T>(
        &self,
        result: Result<T, ProviderError>,
    ) -> SourceResult<T> {
        result.map_err(ProviderFailure::Provider)
    }

    /// [`listenbrainz_result`](Self::listenbrainz_result) for a popularity
    /// read: ListenBrainz switching it off, or turning it down for its
    /// rate, marks popularity as down (see [`popularity_down_for`]).
    pub(in crate::reads::discover::adapters) fn popularity_result<T>(
        &self,
        result: Result<T, ProviderError>,
    ) -> SourceResult<T> {
        if let Err(error) = &result
            && let Some(wait) = popularity_down_for(error)
        {
            self.mark_popularity_down(wait);
        }
        self.listenbrainz_result(result)
    }

    pub(in crate::reads::discover::adapters) async fn lastfm(
        &self,
        user_id: &str,
    ) -> Option<(CatalogLastFm, LastFmCredentials)> {
        self.upstream.lastfm(user_id).await
    }

    /// Where the user's listening is read from. Preferences that do not
    /// read follow ListenBrainz, the default.
    async fn primary_source(&self, user_id: &str) -> MusicSource {
        match self
            .listening
            .prefs(user_id)
            .await
            .primary_music_source
            .as_str()
        {
            "lastfm" => MusicSource::LastFm,
            _ => MusicSource::ListenBrainz,
        }
    }

    pub(in crate::reads::discover::adapters) fn musicbrainz(
        &self,
        priority: RequestPriority,
    ) -> crate::reads::catalog::upstream::CatalogMusicBrainz {
        self.upstream.musicbrainz(priority).0
    }
}

/// A Last.fm answer; an unknown entity is already an empty answer.
pub(in crate::reads::discover::adapters) fn lastfm_result<T>(
    result: Result<T, ProviderError>,
) -> SourceResult<T> {
    result.map_err(ProviderFailure::Provider)
}

/// A MusicBrainz lookup answer: a malformed or refused id is a definite
/// miss, anything else that failed stays an error.
fn musicbrainz_option<T>(result: Result<Option<T>, ProviderError>) -> SourceResult<Option<T>> {
    match result {
        Ok(found) => Ok(found),
        Err(
            ProviderError::BadRequest { status: 400, .. }
            | ProviderError::NotFound { .. }
            | ProviderError::Invalid { .. },
        ) => Ok(None),
        Err(error) => Err(ProviderFailure::Provider(error)),
    }
}

fn first_youtube(relations: &[Relation]) -> Option<String> {
    relations
        .iter()
        .filter_map(|relation| relation.url.as_ref())
        .map(|url| url.resource.clone())
        .find(|url| url.contains("youtube.com") || url.contains("youtu.be"))
}

pub(in crate::reads::discover::adapters) fn credit_name(credit: &[ArtistCreditName]) -> String {
    credit
        .iter()
        .map(|entry| {
            let name = if entry.name.is_empty() {
                entry.artist.name.as_str()
            } else {
                entry.name.as_str()
            };
            format!("{name}{}", entry.joinphrase)
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

fn lastfm_artist_key(artist: &ArtistRow) -> String {
    artist
        .mbid
        .clone()
        .unwrap_or_else(|| artist.name.trim().to_lowercase())
}

impl QueueSources for LiveSources {
    fn source_key(&self) -> String {
        self.upstream.settings().musicbrainz().map_or_else(
            || UNREADABLE_NAMESPACE.to_owned(),
            |settings| format!("{}:g{}", settings.source_id, settings.generation),
        )
    }

    fn user_music<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, UserMusic> {
        Box::pin(async move {
            let listenbrainz = self.links.status(user_id).await.map(|link| link.username);
            let lastfm_link = user_lastfm(self.upstream.users(), user_id).await;
            let lastfm = self.lastfm(user_id).await.is_some();
            let jellyfin = match self.jellyfin.resolve(user_id, SourceName::Jellyfin).await {
                Ok(_) => true,
                Err(ResolveError::NotConfigured) => false,
                Err(error) => {
                    tracing::debug!(%error, "jellyfin unavailable for queue seeds");
                    false
                }
            };
            UserMusic {
                listenbrainz,
                lastfm_username: lastfm_link.username,
                lastfm,
                jellyfin,
                primary: self.primary_source(user_id).await,
            }
        })
    }

    fn listenbrainz_top_artists<'a>(
        &'a self,
        username: &'a str,
        range: StatsRange,
        count: u32,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        Box::pin(async move {
            let key = CacheKey::new(
                Source::ListenBrainz,
                &[
                    "queue_top_artists",
                    &username.to_lowercase(),
                    range.as_str(),
                    &count.to_string(),
                ],
            )
            .personal();
            self.cached(key, USER_STATS_TTL, async {
                let rows = self.listenbrainz_result(
                    self.lb()
                        .user_top_artists(username, range.as_str(), count, 0, priority)
                        .await,
                )?;
                Ok(rows
                    .into_iter()
                    .map(|row| ArtistRow {
                        name: row.artist_name,
                        mbid: row.artist_mbids.into_iter().next(),
                        listen_count: row.listen_count,
                    })
                    .collect())
            })
            .await
        })
    }

    fn listenbrainz_top_albums<'a>(
        &'a self,
        username: &'a str,
        range: StatsRange,
        count: u32,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>> {
        Box::pin(async move {
            let key = CacheKey::new(
                Source::ListenBrainz,
                &[
                    "queue_top_albums",
                    &username.to_lowercase(),
                    range.as_str(),
                    &count.to_string(),
                ],
            )
            .personal();
            self.cached(key, USER_STATS_TTL, async {
                let rows = self.listenbrainz_result(
                    self.lb()
                        .user_top_release_groups(username, range.as_str(), count, 0, priority)
                        .await,
                )?;
                Ok(rows
                    .into_iter()
                    .filter_map(|row| {
                        Some(AlbumRow {
                            release_group_mbid: row.release_group_mbid?,
                            title: row.release_group_name,
                            artist_name: row.artist_name,
                            artist_mbid: row.artist_mbids.into_iter().next(),
                        })
                    })
                    .collect())
            })
            .await
        })
    }

    fn listenbrainz_similar_artists<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        limit: usize,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        Box::pin(async move {
            let key = CacheKey::new(
                Source::ListenBrainz,
                &[
                    "queue_similar",
                    &artist_mbid.to_lowercase(),
                    &limit.to_string(),
                ],
            );
            self.cached(key, ARTIST_TTL, async {
                let creds = self.credentials(user_id).await;
                let rows = self.listenbrainz_result(
                    self.lb()
                        .similar_artists(artist_mbid, limit, &creds, priority)
                        .await,
                )?;
                Ok(rows
                    .into_iter()
                    .map(|row| ArtistRow {
                        name: row.artist_name,
                        mbid: Some(row.artist_mbid),
                        listen_count: row.listen_count,
                    })
                    .collect())
            })
            .await
        })
    }

    fn listenbrainz_artist_albums<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        count: usize,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>> {
        Box::pin(async move {
            if self.listenbrainz_popularity_down() {
                return Ok(Vec::new());
            }
            let key = CacheKey::new(
                Source::ListenBrainz,
                &[
                    "queue_artist_albums",
                    &artist_mbid.to_lowercase(),
                    &count.to_string(),
                ],
            );
            self.cached(key, ARTIST_TTL, async {
                let creds = self.credentials(user_id).await;
                let rows = self.popularity_result(
                    self.lb()
                        .artist_top_release_groups(artist_mbid, count, &creds, priority)
                        .await,
                )?;
                Ok(rows
                    .into_iter()
                    .filter(|row| !row.name.is_empty())
                    .map(|row| AlbumRow {
                        release_group_mbid: row.release_group_mbid,
                        title: row.name,
                        artist_name: row.artist_name,
                        artist_mbid: Some(artist_mbid.to_lowercase()),
                    })
                    .collect())
            })
            .await
        })
    }

    fn listenbrainz_genres<'a>(
        &'a self,
        username: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>> {
        Box::pin(async move {
            let key = CacheKey::new(
                Source::ListenBrainz,
                &["queue_genres", &username.to_lowercase()],
            )
            .personal();
            self.cached(key, ARTIST_TTL, async {
                let rows = self
                    .listenbrainz_result(self.lb().user_genre_activity(username, priority).await)?;
                Ok(rows.into_iter().map(|row| row.genre).collect())
            })
            .await
        })
    }

    fn listenbrainz_fresh_releases<'a>(
        &'a self,
        username: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>> {
        Box::pin(async move {
            let key = CacheKey::new(
                Source::ListenBrainz,
                &["queue_fresh", &username.to_lowercase()],
            )
            .personal();
            self.cached(key, ARTIST_TTL, async {
                let rows = self
                    .listenbrainz_result(self.lb().user_fresh_releases(username, priority).await)?;
                Ok(rows
                    .into_iter()
                    .filter_map(|row| {
                        let artist_name = row.artist_credit_name.filter(|name| !name.is_empty())?;
                        Some(AlbumRow {
                            release_group_mbid: row.release_group_mbid,
                            title: row.release_name,
                            artist_name,
                            artist_mbid: row.artist_mbids.into_iter().next(),
                        })
                    })
                    .collect())
            })
            .await
        })
    }

    fn listenbrainz_loved_artists<'a>(
        &'a self,
        username: &'a str,
        count: u32,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>> {
        Box::pin(async move {
            let key = CacheKey::new(
                Source::ListenBrainz,
                &["queue_loved", &username.to_lowercase(), &count.to_string()],
            )
            .personal();
            self.cached(key, USER_STATS_TTL, async {
                self.listenbrainz_result(
                    self.lb()
                        .user_loved_artist_mbids(username, count, priority)
                        .await,
                )
            })
            .await
        })
    }

    fn listenbrainz_trending(
        &self,
        count: u32,
        priority: RequestPriority,
    ) -> BoxFuture<'_, SourceResult<Vec<AlbumRow>>> {
        Box::pin(async move {
            let key = CacheKey::new(
                Source::ListenBrainz,
                &["queue_trending", &count.to_string()],
            );
            self.cached(key, ARTIST_TTL, async {
                let rows = self.listenbrainz_result(
                    self.lb()
                        .sitewide_top_release_groups("this_week", count, 0, priority)
                        .await,
                )?;
                Ok(rows
                    .into_iter()
                    .filter_map(|row| {
                        Some(AlbumRow {
                            release_group_mbid: row.release_group_mbid?,
                            title: row.release_group_name,
                            artist_name: row.artist_name,
                            artist_mbid: row.artist_mbids.into_iter().next(),
                        })
                    })
                    .collect())
            })
            .await
        })
    }

    fn listenbrainz_listen_counts<'a>(
        &'a self,
        user_id: &'a str,
        release_group_mbids: &'a [String],
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<HashMap<String, i64>>> {
        Box::pin(async move {
            let cache = self.upstream.cache();
            let key_of = |mbid: &str| {
                CacheKey::new(
                    Source::ListenBrainz,
                    &["rg_popularity", &mbid.to_lowercase()],
                )
            };
            let mut counts = HashMap::new();
            let mut missing = Vec::new();
            for mbid in release_group_mbids {
                let hit = cache
                    .get_bytes(&key_of(mbid))
                    .await
                    .and_then(|bytes| serde_json::from_slice::<i64>(&bytes).ok());
                match hit {
                    Some(count) => {
                        counts.insert(mbid.clone(), count);
                    }
                    None => missing.push(mbid.clone()),
                }
            }
            if missing.is_empty() || self.listenbrainz_popularity_down() {
                return Ok(counts);
            }
            let creds = self.credentials(user_id).await;
            let fetched = self.popularity_result(
                self.lb()
                    .release_group_popularity(&missing, &creds, priority)
                    .await,
            )?;
            for (mbid, count) in fetched {
                if let Ok(bytes) = serde_json::to_vec(&count) {
                    cache.set_bytes(&key_of(&mbid), bytes, ARTIST_TTL);
                }
                if let Some(asked) = missing
                    .iter()
                    .find(|asked| asked.eq_ignore_ascii_case(&mbid))
                {
                    counts.insert(asked.clone(), count);
                }
            }
            Ok(counts)
        })
    }

    fn listenbrainz_popularity_down(&self) -> bool {
        self.popularity_down_until
            .lock()
            .map(|until| until.is_some_and(|until| Instant::now() < until))
            .unwrap_or(false)
    }

    fn lastfm_top_artists<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        limit: u32,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let key = CacheKey::new(
                Source::LastFm,
                &[
                    "queue_top_artists",
                    &username.to_lowercase(),
                    &limit.to_string(),
                ],
            )
            .personal();
            self.cached(key, USER_STATS_TTL, async {
                let rows = lastfm_result(
                    client
                        .user_top_artists(&creds, username, "3month", limit, priority)
                        .await,
                )?;
                Ok(rows
                    .into_iter()
                    .map(|row| ArtistRow {
                        name: row.name,
                        mbid: row.mbid,
                        listen_count: row.playcount,
                    })
                    .collect())
            })
            .await
        })
    }

    fn lastfm_similar_artists<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a ArtistRow,
        limit: u32,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let key = CacheKey::new(
                Source::LastFm,
                &[
                    "queue_similar",
                    &lastfm_artist_key(artist),
                    &limit.to_string(),
                ],
            );
            self.cached(key, ARTIST_TTL, async {
                let rows = lastfm_result(
                    client
                        .similar_artists(
                            &creds,
                            &artist.name,
                            artist.mbid.as_deref(),
                            limit,
                            priority,
                        )
                        .await,
                )?;
                Ok(rows
                    .into_iter()
                    .map(|row| ArtistRow {
                        name: row.name,
                        mbid: row.mbid,
                        listen_count: 0,
                    })
                    .collect())
            })
            .await
        })
    }

    fn lastfm_artist_albums<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a ArtistRow,
        limit: u32,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<LastFmAlbum>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let key = CacheKey::new(
                Source::LastFm,
                &[
                    "queue_albums",
                    &lastfm_artist_key(artist),
                    &limit.to_string(),
                ],
            );
            self.cached(key, ARTIST_TTL, async {
                let rows = lastfm_result(
                    client
                        .artist_top_albums(
                            &creds,
                            &artist.name,
                            artist.mbid.as_deref(),
                            limit,
                            priority,
                        )
                        .await,
                )?;
                Ok(rows
                    .into_iter()
                    .map(|row| LastFmAlbum {
                        name: row.name,
                        artist_name: row.artist_name,
                        mbid: row.mbid,
                    })
                    .collect())
            })
            .await
        })
    }

    fn lastfm_chart_artists<'a>(
        &'a self,
        user_id: &'a str,
        limit: u32,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let key = CacheKey::new(Source::LastFm, &["queue_chart", &limit.to_string()]);
            self.cached(key, ARTIST_TTL, async {
                let rows = lastfm_result(client.chart_top_artists(&creds, limit, priority).await)?;
                Ok(rows
                    .into_iter()
                    .map(|row| ArtistRow {
                        name: row.name,
                        mbid: row.mbid,
                        listen_count: row.playcount,
                    })
                    .collect())
            })
            .await
        })
    }

    fn lastfm_album_facts<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a str,
        album: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Option<LastFmFacts>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(None);
            };
            let info = lastfm_result(
                client
                    .album_info(&creds, artist, album, None, priority)
                    .await,
            )?;
            Ok(info.map(|info| LastFmFacts {
                mbid: info.mbid,
                tags: info.tags.into_iter().map(|tag| tag.name).collect(),
                summary: info.summary,
            }))
        })
    }

    fn lastfm_artist_facts<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a str,
        mbid: Option<&'a str>,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Option<LastFmFacts>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(None);
            };
            let info = lastfm_result(client.artist_info(&creds, artist, mbid, priority).await)?;
            Ok(info.map(|info| LastFmFacts {
                mbid: info.mbid,
                tags: info.tags.into_iter().map(|tag| tag.name).collect(),
                summary: info.bio_summary,
            }))
        })
    }

    fn jellyfin_artists<'a>(
        &'a self,
        user_id: &'a str,
        list: JellyfinList,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        Box::pin(async move {
            let Some(handle) = self.jellyfin_client(user_id).await? else {
                return Ok(Vec::new());
            };
            let adapter = handle
                .jellyfin("artist lists")
                .map_err(|error| ProviderFailure::failed(error.to_string()))?;
            let artists = match list {
                JellyfinList::MostPlayed => adapter.most_played_artists(i64::from(limit)).await,
                JellyfinList::Favorites => adapter
                    .favorites(i64::from(limit))
                    .await
                    .map(|favorites| favorites.artists),
            }
            .map_err(|error| ProviderFailure::failed(error.to_string()))?;
            Ok(artists
                .into_iter()
                .map(|artist| ArtistRow {
                    name: artist.name,
                    mbid: artist.artist_mbid,
                    listen_count: 0,
                })
                .collect())
        })
    }

    fn musicbrainz_tag_albums<'a>(
        &'a self,
        tag: &'a str,
        limit: u32,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>> {
        Box::pin(async move {
            let ttl = crate::reads::catalog::SEARCH_TTL * 2;
            let key = CacheKey::new(
                Source::MusicBrainz,
                &[
                    &self.source_key(),
                    "rg_by_tag",
                    &tag.trim().to_lowercase(),
                    &limit.to_string(),
                ],
            );
            self.cached(key, ttl, async {
                // Ask for extra rows: the type exclusions drop some.
                let asked = ((limit as f64 * 1.5) as u32).clamp(25, 100);
                let page = self
                    .musicbrainz(priority)
                    .search_release_groups_by_tag(tag, asked, 0, Criticality::BestEffort)
                    .await
                    .map_err(ProviderFailure::Provider)?;
                let mut seen = std::collections::HashSet::new();
                Ok(page
                    .items
                    .into_iter()
                    .filter(|hit| {
                        should_include_release(
                            hit.primary_type.as_deref(),
                            &hit.secondary_types,
                            None,
                            None,
                            true,
                        )
                    })
                    .filter(|hit| seen.insert(hit.id.to_lowercase()))
                    .filter_map(|hit| {
                        Some(AlbumRow {
                            title: hit.title.filter(|title| !title.is_empty())?,
                            artist_name: credit_name(&hit.artist_credit),
                            artist_mbid: hit.artist_credit.first().map(|c| c.artist.id.clone()),
                            release_group_mbid: hit.id,
                        })
                    })
                    .take(limit as usize)
                    .collect())
            })
            .await
        })
    }

    fn musicbrainz_release_group_of<'a>(
        &'a self,
        release_mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Option<String>>> {
        Box::pin(async move {
            if !is_valid_mbid(release_mbid) {
                return Ok(None);
            }
            musicbrainz_option(
                self.musicbrainz(priority)
                    .resolve_release_to_release_group(release_mbid, Criticality::IdentityCritical)
                    .await,
            )
        })
    }

    fn musicbrainz_release_group<'a>(
        &'a self,
        mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Option<ReleaseGroupFacts>>> {
        Box::pin(async move {
            if !is_valid_mbid(mbid) {
                return Ok(None);
            }
            let found = musicbrainz_option(
                self.musicbrainz(priority)
                    .lookup_release_group(
                        mbid,
                        &["artist-credits", "releases", "tags", "url-rels"],
                        Criticality::IdentityCritical,
                    )
                    .await,
            )?;
            Ok(found.map(|lookup| {
                let group = lookup.entity;
                let mut tags = group.tags.clone();
                tags.sort_by(|a, b| b.count.unwrap_or(0).cmp(&a.count.unwrap_or(0)));
                let first_credit = group.artist_credit.first();
                let first_release = group.releases.first();
                ReleaseGroupFacts {
                    title: group.title.clone().unwrap_or_default(),
                    artist_mbid: first_credit.map(|credit| credit.artist.id.clone()),
                    artist_name: first_credit
                        .map(|credit| credit.artist.name.clone())
                        .filter(|name| !name.is_empty()),
                    tags: tags
                        .into_iter()
                        .map(|tag| tag.name)
                        .filter(|name| !name.is_empty())
                        .collect(),
                    youtube_url: first_youtube(&group.relations),
                    first_release_id: first_release.map(|release| release.id.clone()),
                    first_release_date: first_release.and_then(|release| release.date.clone()),
                }
            }))
        })
    }

    fn musicbrainz_artist<'a>(
        &'a self,
        mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Option<ArtistFacts>>> {
        Box::pin(async move {
            if !is_valid_mbid(mbid) {
                return Ok(None);
            }
            let found = musicbrainz_option(
                self.musicbrainz(priority)
                    .lookup_artist(mbid, &["url-rels"], Criticality::BestEffort)
                    .await,
            )?;
            Ok(found.map(|lookup| {
                let artist = lookup.entity;
                let country = artist
                    .country
                    .clone()
                    .filter(|code| !code.is_empty())
                    .or_else(|| artist.area.as_ref().and_then(|area| area.name.clone()));
                let wiki_url = artist
                    .relations
                    .iter()
                    .find(|relation| matches!(relation.rel_type.as_str(), "wikipedia" | "wikidata"))
                    .and_then(|relation| relation.url.as_ref())
                    .map(|url| url.resource.clone());
                ArtistFacts { country, wiki_url }
            }))
        })
    }

    fn musicbrainz_release_video<'a>(
        &'a self,
        release_mbid: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<String>>> {
        Box::pin(async move {
            let found = musicbrainz_option(
                self.musicbrainz(RequestPriority::UserInitiated)
                    .lookup_release(release_mbid, &["url-rels"], Criticality::BestEffort)
                    .await,
            )?;
            Ok(found.and_then(|lookup| first_youtube(&lookup.entity.relations)))
        })
    }

    fn musicbrainz_release_recordings<'a>(
        &'a self,
        release_mbid: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>> {
        Box::pin(async move {
            let found = musicbrainz_option(
                self.musicbrainz(RequestPriority::UserInitiated)
                    .lookup_release(release_mbid, &["recordings"], Criticality::BestEffort)
                    .await,
            )?;
            let mut recordings: Vec<String> = Vec::new();
            if let Some(lookup) = found {
                for track in lookup
                    .entity
                    .media
                    .iter()
                    .flat_map(|medium| medium.tracks.iter())
                {
                    if recordings.len() >= limit {
                        break;
                    }
                    if let Some(recording) = &track.recording
                        && !recordings.contains(&recording.id)
                    {
                        recordings.push(recording.id.clone());
                    }
                }
            }
            Ok(recordings)
        })
    }

    fn musicbrainz_recording_video<'a>(
        &'a self,
        recording_mbid: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<String>>> {
        Box::pin(async move {
            let found = musicbrainz_option(
                self.musicbrainz(RequestPriority::UserInitiated)
                    .lookup_recording(recording_mbid, &["url-rels"], Criticality::BestEffort)
                    .await,
            )?;
            Ok(found.and_then(|lookup| first_youtube(&lookup.entity.relations)))
        })
    }

    fn wikipedia_extract<'a>(
        &'a self,
        url: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<String>>> {
        // Queue cards are opened by a person, like the MusicBrainz reads
        // beside this one.
        Box::pin(async move {
            self.upstream
                .wikidata()
                .get_bio_extract(url, "en", RequestPriority::UserInitiated)
                .await
                .map_err(ProviderFailure::Provider)
        })
    }
}

/// How long a failed popularity read keeps popularity down: a refusal for
/// [`POPULARITY_DOWN_FOR`]; a rate limit for the wait it names, else as
/// long as a refusal (a 429 without ListenBrainz's rate headers comes from
/// in front of the service and does not clear in seconds). Other failures
/// leave it up.
fn popularity_down_for(error: &ProviderError) -> Option<Duration> {
    match error {
        ProviderError::Disabled { .. } => Some(POPULARITY_DOWN_FOR),
        ProviderError::RateLimited { retry_after, .. } => {
            Some(retry_after.unwrap_or(POPULARITY_DOWN_FOR))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_keeps_popularity_down_for_the_window() {
        let refused = ProviderError::Disabled {
            provider: "listenbrainz",
            message: "ListenBrainz switched this endpoint off upstream (401)".to_owned(),
        };
        assert_eq!(popularity_down_for(&refused), Some(POPULARITY_DOWN_FOR));
    }

    #[test]
    fn a_rate_limit_keeps_popularity_down_for_the_wait_it_names() {
        let limited = ProviderError::RateLimited {
            provider: "listenbrainz",
            retry_after: Some(Duration::from_secs(7)),
        };
        assert_eq!(popularity_down_for(&limited), Some(Duration::from_secs(7)));
    }

    #[test]
    fn a_rate_limit_naming_no_wait_keeps_popularity_down_for_the_window() {
        let limited = ProviderError::RateLimited {
            provider: "listenbrainz",
            retry_after: None,
        };
        assert_eq!(popularity_down_for(&limited), Some(POPULARITY_DOWN_FOR));
    }

    #[test]
    fn an_outage_leaves_popularity_up() {
        let unavailable = ProviderError::Unavailable {
            provider: "listenbrainz",
            retry_after: None,
        };
        assert_eq!(popularity_down_for(&unavailable), None);
    }
}
