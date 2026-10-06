//! [`QueueSources`] over the live providers: ListenBrainz, Last.fm,
//! MusicBrainz, Wikipedia and the user's Jellyfin account.
//!
//! Clients come from the catalog's [`Upstream`], built per call from the
//! settings as they are now. Repeated provider reads go through the shared
//! provider cache, so a rebuild minutes later does not pay the 1/s
//! ListenBrainz pacing again. Popularity refusals from ListenBrainz (its
//! "disabled due to high load" answer) mark popularity as down for a few
//! minutes, which lets Last.fm stand in (v2 `lb_popularity_degraded`).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Serialize, de::DeserializeOwned};
use sqlx::SqlitePool;

use super::sources::{
    AlbumRow, ArtistFacts, ArtistRow, JellyfinList, LastFmAlbum, LastFmFacts, MusicSource,
    QueueSources, ReleaseGroupFacts, SourceResult, StatsRange, UserMusic,
};
use crate::plugins::scrobble::ListenBrainzLinkStore;
use crate::providers::adapters::{CorePacer, CoreSink};
use crate::providers::cache::cache_aside_json;
use crate::providers::lastfm::{self, LastFmCredentials};
use crate::providers::listenbrainz::{self, ListenBrainzClient, ListenBrainzCredentials};
use crate::providers::musicbrainz::models::{ArtistCreditName, Relation};
use crate::providers::musicbrainz::{Criticality, MbError, is_valid_mbid};
use crate::providers::wikidata::WikidataClient;
use crate::providers::{Providers, RequestPriority};
use crate::reads::catalog::mapping::should_include_release;
use crate::reads::catalog::upstream::{CatalogLastFm, Upstream, user_lastfm};
use crate::reads::discover::ports::BoxFuture;
use crate::remotes::connections::{ConnectionResolver, ResolveError};
use crate::remotes::jellyfin::JellyfinAdapter;
use crate::remotes::models::SourceName;

/// How long a popularity refusal keeps Last.fm standing in.
const POPULARITY_DOWN_FOR: Duration = Duration::from_secs(10 * 60);
/// Cache lifetime of per-user listening statistics.
const USER_STATS_TTL: Duration = Duration::from_secs(5 * 60);
/// Cache lifetime of artist-level and chart reads.
const ARTIST_TTL: Duration = Duration::from_secs(60 * 60);

/// The live provider reads.
pub struct LiveSources {
    upstream: Upstream,
    listenbrainz: Option<ListenBrainzClient<CorePacer, CoreSink>>,
    links: Arc<dyn ListenBrainzLinkStore>,
    jellyfin: Arc<ConnectionResolver>,
    http: reqwest::Client,
    pool: SqlitePool,
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
        pool: SqlitePool,
    ) -> Self {
        let base = upstream.endpoints().listenbrainz.clone();
        let listenbrainz = match CorePacer::for_source(providers, listenbrainz::SOURCE) {
            Some(pacer) => Some(ListenBrainzClient::new(
                http.clone(),
                &base,
                pacer,
                CoreSink,
            )),
            None => {
                tracing::error!("no listenbrainz rate limit row; the queue skips ListenBrainz");
                None
            }
        };
        Self {
            listenbrainz,
            upstream,
            links,
            jellyfin,
            http,
            pool,
            popularity_down_until: Mutex::new(None),
        }
    }

    fn lb(&self) -> SourceResult<&ListenBrainzClient<CorePacer, CoreSink>> {
        self.listenbrainz
            .as_ref()
            .ok_or_else(|| "no listenbrainz limiter".to_owned())
    }

    async fn cached<T, Fut>(&self, key: String, ttl: Duration, fetch: Fut) -> SourceResult<T>
    where
        T: Serialize + DeserializeOwned,
        Fut: Future<Output = SourceResult<T>>,
    {
        cache_aside_json(self.upstream.cache(), &key, ttl, || fetch).await
    }

    /// The user's ListenBrainz identity, lending the token to reads that
    /// ListenBrainz gates for anonymous callers.
    async fn credentials(&self, user_id: &str) -> ListenBrainzCredentials {
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

    fn mark_popularity_down(&self, message: &str) {
        if message.contains("popularity endpoint unavailable upstream")
            && let Ok(mut until) = self.popularity_down_until.lock()
        {
            *until = Some(Instant::now() + POPULARITY_DOWN_FOR);
        }
    }

    fn listenbrainz_result<T: Default>(
        &self,
        outcome: listenbrainz::Outcome<T>,
    ) -> SourceResult<T> {
        match outcome {
            listenbrainz::Outcome::Found(value) => Ok(value),
            listenbrainz::Outcome::Missing => Ok(T::default()),
            listenbrainz::Outcome::Unavailable { message, .. } => {
                self.mark_popularity_down(&message);
                Err(message)
            }
        }
    }

    async fn lastfm(&self, user_id: &str) -> Option<(CatalogLastFm, LastFmCredentials)> {
        self.upstream.lastfm(user_id).await
    }

    async fn primary_source(&self, user_id: &str) -> MusicSource {
        let stored: Option<(String,)> = sqlx::query_as(
            "SELECT primary_music_source FROM user_listening_prefs WHERE user_id = ?1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "listening prefs unreadable; discovery follows ListenBrainz");
            None
        });
        match stored.as_ref().map(|(source,)| source.as_str()) {
            Some("lastfm") => MusicSource::LastFm,
            _ => MusicSource::ListenBrainz,
        }
    }

    fn musicbrainz(
        &self,
        priority: RequestPriority,
    ) -> crate::reads::catalog::upstream::CatalogMusicBrainz {
        self.upstream.musicbrainz(priority).0
    }
}

fn lastfm_result<T: Default>(outcome: lastfm::Outcome<T>) -> SourceResult<T> {
    match outcome {
        lastfm::Outcome::Found(value) => Ok(value),
        lastfm::Outcome::Missing => Ok(T::default()),
        lastfm::Outcome::Unavailable { message, .. } => Err(message),
    }
}

fn lastfm_option<T>(outcome: lastfm::Outcome<T>) -> SourceResult<Option<T>> {
    match outcome {
        lastfm::Outcome::Found(value) => Ok(Some(value)),
        lastfm::Outcome::Missing => Ok(None),
        lastfm::Outcome::Unavailable { message, .. } => Err(message),
    }
}

/// A MusicBrainz lookup answer: a malformed or refused id is a definite
/// miss, anything else that failed stays an error.
fn musicbrainz_option<T>(result: Result<Option<T>, MbError>) -> SourceResult<Option<T>> {
    match result {
        Ok(found) => Ok(found),
        Err(MbError::InvalidMbid(_) | MbError::Rejected(400 | 404)) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn first_youtube(relations: &[Relation]) -> Option<String> {
    relations
        .iter()
        .filter_map(|relation| relation.url.as_ref())
        .map(|url| url.resource.clone())
        .find(|url| url.contains("youtube.com") || url.contains("youtu.be"))
}

fn credit_name(credit: &[ArtistCreditName]) -> String {
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
        let settings = self.upstream.settings().musicbrainz();
        format!("{}:g{}", settings.source_id, settings.generation)
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
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        Box::pin(async move {
            let key = format!(
                "lb_queue:top_artists:{}:{}:{count}",
                username.to_lowercase(),
                range.as_str()
            );
            self.cached(key, USER_STATS_TTL, async {
                let rows = self.listenbrainz_result(
                    self.lb()?
                        .user_top_artists(username, range.as_str(), count, 0)
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
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>> {
        Box::pin(async move {
            let key = format!(
                "lb_queue:top_albums:{}:{}:{count}",
                username.to_lowercase(),
                range.as_str()
            );
            self.cached(key, USER_STATS_TTL, async {
                let rows = self.listenbrainz_result(
                    self.lb()?
                        .user_top_release_groups(username, range.as_str(), count, 0)
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
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        Box::pin(async move {
            let key = format!("lb_queue:similar:{}:{limit}", artist_mbid.to_lowercase());
            self.cached(key, ARTIST_TTL, async {
                let creds = self.credentials(user_id).await;
                let rows = self.listenbrainz_result(
                    self.lb()?.similar_artists(artist_mbid, limit, &creds).await,
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
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>> {
        Box::pin(async move {
            if self.listenbrainz_popularity_down() {
                return Ok(Vec::new());
            }
            let key = format!(
                "lb_queue:artist_albums:{}:{count}",
                artist_mbid.to_lowercase()
            );
            self.cached(key, ARTIST_TTL, async {
                let creds = self.credentials(user_id).await;
                let rows = self.listenbrainz_result(
                    self.lb()?
                        .artist_top_release_groups(artist_mbid, count, &creds)
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
    ) -> BoxFuture<'a, SourceResult<Vec<String>>> {
        Box::pin(async move {
            let key = format!("lb_queue:genres:{}", username.to_lowercase());
            self.cached(key, ARTIST_TTL, async {
                let rows =
                    self.listenbrainz_result(self.lb()?.user_genre_activity(username).await)?;
                Ok(rows.into_iter().map(|row| row.genre).collect())
            })
            .await
        })
    }

    fn listenbrainz_fresh_releases<'a>(
        &'a self,
        username: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>> {
        Box::pin(async move {
            let key = format!("lb_queue:fresh:{}", username.to_lowercase());
            self.cached(key, ARTIST_TTL, async {
                let rows =
                    self.listenbrainz_result(self.lb()?.user_fresh_releases(username).await)?;
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
    ) -> BoxFuture<'a, SourceResult<Vec<String>>> {
        Box::pin(async move {
            let key = format!("lb_queue:loved:{}:{count}", username.to_lowercase());
            self.cached(key, USER_STATS_TTL, async {
                self.listenbrainz_result(self.lb()?.user_loved_artist_mbids(username, count).await)
            })
            .await
        })
    }

    fn listenbrainz_trending(&self, count: u32) -> BoxFuture<'_, SourceResult<Vec<AlbumRow>>> {
        Box::pin(async move {
            let key = format!("lb_queue:trending:{count}");
            self.cached(key, ARTIST_TTL, async {
                let rows = self.listenbrainz_result(
                    self.lb()?
                        .sitewide_top_release_groups("this_week", count, 0)
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
    ) -> BoxFuture<'a, SourceResult<HashMap<String, i64>>> {
        Box::pin(async move {
            let cache = self.upstream.cache();
            let key_of = |mbid: &str| format!("lb_rg_popularity:{}", mbid.to_lowercase());
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
            let fetched = self
                .listenbrainz_result(self.lb()?.release_group_popularity(&missing, &creds).await)?;
            for (mbid, count) in fetched {
                if let Ok(bytes) = serde_json::to_vec(&count) {
                    cache.set_bytes(&key_of(&mbid), bytes, ARTIST_TTL).await;
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
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let key = format!("lfm_queue:top_artists:{}:{limit}", username.to_lowercase());
            self.cached(key, USER_STATS_TTL, async {
                let rows = lastfm_result(
                    client
                        .user_top_artists(&creds, username, "3month", limit)
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
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let key = format!("lfm_queue:similar:{}:{limit}", lastfm_artist_key(artist));
            self.cached(key, ARTIST_TTL, async {
                let rows = lastfm_result(
                    client
                        .similar_artists(&creds, &artist.name, artist.mbid.as_deref(), limit)
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
    ) -> BoxFuture<'a, SourceResult<Vec<LastFmAlbum>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let key = format!("lfm_queue:albums:{}:{limit}", lastfm_artist_key(artist));
            self.cached(key, ARTIST_TTL, async {
                let rows = lastfm_result(
                    client
                        .artist_top_albums(&creds, &artist.name, artist.mbid.as_deref(), limit)
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
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let key = format!("lfm_queue:chart:{limit}");
            self.cached(key, ARTIST_TTL, async {
                let rows = lastfm_result(client.chart_top_artists(&creds, limit).await)?;
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
    ) -> BoxFuture<'a, SourceResult<Option<LastFmFacts>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(None);
            };
            let info = lastfm_option(client.album_info(&creds, artist, album, None).await)?;
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
    ) -> BoxFuture<'a, SourceResult<Option<LastFmFacts>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(None);
            };
            let info = lastfm_option(client.artist_info(&creds, artist, mbid).await)?;
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
            let resolved = match self.jellyfin.resolve(user_id, SourceName::Jellyfin).await {
                Ok(resolved) => resolved,
                Err(ResolveError::NotConfigured) => return Ok(Vec::new()),
                Err(error) => return Err(error.to_string()),
            };
            let adapter = JellyfinAdapter::new(
                self.http.clone(),
                resolved.base_url,
                resolved.credential,
                resolved.user_id,
            );
            let artists = match list {
                JellyfinList::MostPlayed => adapter.most_played_artists(i64::from(limit)).await,
                JellyfinList::Favorites => adapter
                    .favorites(i64::from(limit))
                    .await
                    .map(|favorites| favorites.artists),
            }
            .map_err(|error| error.to_string())?;
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
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>> {
        Box::pin(async move {
            let ttl = Duration::from_secs(
                u64::try_from(self.upstream.settings().advanced().cache_ttl_search)
                    .unwrap_or(3600)
                    .saturating_mul(2),
            );
            let key = format!(
                "mb_rg_by_tag:{}:{}:{limit}",
                self.source_key(),
                tag.trim().to_lowercase()
            );
            self.cached(key, ttl, async {
                // Ask for extra rows: the type exclusions drop some.
                let asked = ((limit as f64 * 1.5) as u32).clamp(25, 100);
                let page = self
                    .musicbrainz(RequestPriority::BackgroundSync)
                    .search_release_groups_by_tag(tag, asked, 0, Criticality::BestEffort)
                    .await
                    .map_err(|error| error.to_string())?;
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
    ) -> BoxFuture<'a, SourceResult<Option<String>>> {
        Box::pin(async move {
            if !is_valid_mbid(release_mbid) {
                return Ok(None);
            }
            musicbrainz_option(
                self.musicbrainz(RequestPriority::BackgroundSync)
                    .resolve_release_to_release_group(release_mbid, Criticality::IdentityCritical)
                    .await,
            )
        })
    }

    fn musicbrainz_release_group<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<ReleaseGroupFacts>>> {
        Box::pin(async move {
            if !is_valid_mbid(mbid) {
                return Ok(None);
            }
            let found = musicbrainz_option(
                self.musicbrainz(RequestPriority::BackgroundSync)
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
    ) -> BoxFuture<'a, SourceResult<Option<ArtistFacts>>> {
        Box::pin(async move {
            if !is_valid_mbid(mbid) {
                return Ok(None);
            }
            let found = musicbrainz_option(
                self.musicbrainz(RequestPriority::BackgroundSync)
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
        Box::pin(async move {
            let http = self.upstream.http_get();
            let endpoints = self.upstream.endpoints();
            WikidataClient::with_bases(
                &http,
                &endpoints.wikidata,
                &endpoints.wikipedia,
                &endpoints.commons,
            )
            .get_bio_extract(url, "en")
            .await
            .map_err(|error| format!("wikipedia: {error:?}"))
        })
    }
}
