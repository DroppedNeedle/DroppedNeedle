//! [`ListeningNetwork`] over the live providers: ListenBrainz, Last.fm,
//! MusicBrainz, Wikipedia and the user's Jellyfin account.
//!
//! Clients come from the catalog's [`Upstream`], built per call from the
//! settings as they are now; ListenBrainz is the one shared client. Each
//! read goes out at the priority it is given. Nothing here caches: the
//! cache sits in front, in [`ListeningData`](super::ListeningData).

use std::collections::HashMap;
use std::sync::Arc;

use super::network::{ListeningNetwork, Read};
use super::rows::{
    AlbumRow, ArtistFacts, ArtistRow, JellyfinList, LastFmAlbum, LastFmChartAlbum, LastFmFacts,
    ListenSource, ListenedAlbum, LovedTrack, PlayedArtist, PlaylistTrack, RankedAlbum,
    ReleaseGroupFacts, ScoredArtist, SourceContext, StatsRange, TagAlbum, UserMusic,
    WeeklyPlaylist,
};
use super::{ListeningError, ListeningResult};
use crate::reads::catalog::mapping::should_include_release;
use crate::reads::catalog::upstream::{CatalogLastFm, Upstream, user_lastfm};
use crate::reads::discover::ports::BoxFuture;
use droppedneedle_media::playback::listening::{ListenBrainzLinkStore, ListeningStore};
use droppedneedle_media::remotes::adapter::RemoteHandle;
use droppedneedle_media::remotes::connections::{ConnectionResolver, Reads, ResolveError};
use droppedneedle_media::remotes::models::SourceName;
use droppedneedle_providers::lastfm::{LastFmCredentials, TopItem};
use droppedneedle_providers::listenbrainz::stats::{ArtistStat, ReleaseGroupStat};
use droppedneedle_providers::listenbrainz::{ListenBrainzClient, ListenBrainzCredentials};
use droppedneedle_providers::musicbrainz::models::{ArtistCreditName, Relation};
use droppedneedle_providers::musicbrainz::{Criticality, is_valid_mbid};
use droppedneedle_providers::{ProviderError, Providers, RequestPriority};

/// The algorithm ListenBrainz names its weekly exploration playlist after.
const WEEKLY_EXPLORATION_PATCH: &str = "weekly-exploration";

/// The live provider reads.
pub struct LiveNetwork {
    upstream: Upstream,
    listenbrainz: ListenBrainzClient,
    links: Arc<dyn ListenBrainzLinkStore>,
    jellyfin: Arc<ConnectionResolver>,
    http: reqwest::Client,
    listening: ListeningStore,
}

impl LiveNetwork {
    /// Build over the catalog upstream.
    pub fn new(
        upstream: Upstream,
        providers: &Providers,
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
        }
    }

    /// The user's Jellyfin client, `None` when Jellyfin is not set up for
    /// them. Discover reads it in its background warm cycle as well as for
    /// a page, so its reads are not counted against the user read bound.
    async fn jellyfin_client(&self, user_id: &str) -> ListeningResult<Option<RemoteHandle>> {
        match self.jellyfin.resolve(user_id, SourceName::Jellyfin).await {
            Ok(resolved) => Ok(Some(self.jellyfin.client(
                &self.http,
                resolved,
                Reads::Background,
            ))),
            Err(ResolveError::NotConfigured) => Ok(None),
            Err(error) => Err(ListeningError::MediaServer(error.to_string())),
        }
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

    async fn lastfm(&self, user_id: &str) -> Option<(CatalogLastFm, LastFmCredentials)> {
        self.upstream.lastfm(user_id).await
    }

    /// Where the user's listening is read from. Preferences that do not
    /// read follow ListenBrainz, the default.
    async fn primary_source(&self, user_id: &str) -> ListenSource {
        match self
            .listening
            .prefs(user_id)
            .await
            .primary_music_source
            .as_str()
        {
            "lastfm" => ListenSource::Lastfm,
            _ => ListenSource::Listenbrainz,
        }
    }

    fn musicbrainz(
        &self,
        priority: RequestPriority,
    ) -> crate::reads::catalog::upstream::CatalogMusicBrainz {
        self.upstream.musicbrainz(priority).0
    }
}

fn provider<T>(result: Result<T, ProviderError>) -> ListeningResult<T> {
    result.map_err(ListeningError::Provider)
}

/// A MusicBrainz lookup answer: a malformed or refused id is a definite
/// miss, anything else that failed stays an error.
fn musicbrainz_option<T>(result: Result<Option<T>, ProviderError>) -> ListeningResult<Option<T>> {
    match result {
        Ok(found) => Ok(found),
        Err(
            ProviderError::BadRequest { status: 400, .. }
            | ProviderError::NotFound { .. }
            | ProviderError::Invalid { .. },
        ) => Ok(None),
        Err(error) => Err(ListeningError::Provider(error)),
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

fn stat_artist(row: ArtistStat) -> ArtistRow {
    ArtistRow {
        name: row.artist_name,
        mbid: row.artist_mbids.into_iter().next(),
        listen_count: row.listen_count,
    }
}

fn stat_album(row: ReleaseGroupStat) -> ListenedAlbum {
    ListenedAlbum {
        release_group_mbid: row.release_group_mbid,
        title: row.release_group_name,
        artist_name: row.artist_name,
        artist_mbid: row.artist_mbids.into_iter().next(),
        listen_count: row.listen_count,
    }
}

fn top_artist(row: TopItem) -> ArtistRow {
    ArtistRow {
        name: row.name,
        mbid: row.mbid,
        listen_count: row.playcount,
    }
}

fn chart_album(row: TopItem) -> LastFmChartAlbum {
    LastFmChartAlbum {
        name: row.name,
        artist_name: row.artist_name,
        mbid: row.mbid,
        playcount: row.playcount,
        image_url: row.image_url,
    }
}

fn lastfm_loved(row: TopItem) -> LovedTrack {
    LovedTrack {
        name: row.name,
        artist_name: row.artist_name,
        artist_mbid: None,
        album_name: None,
        recording_mbid: None,
        image_url: Some(row.image_url).filter(|url| !url.is_empty()),
    }
}

impl ListeningNetwork for LiveNetwork {
    fn source(&self) -> SourceContext {
        let Some(settings) = self.upstream.settings().musicbrainz() else {
            return SourceContext::unreadable();
        };
        let mode = serde_json::to_value(settings.source_mode)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "official".to_owned());
        SourceContext {
            mode,
            id: settings.source_id,
            generation: settings.generation,
        }
    }

    fn accounts<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, UserMusic> {
        Box::pin(async move {
            let listenbrainz = self.links.status(user_id).await.map(|link| link.username);
            let lastfm_link = user_lastfm(self.upstream.users(), user_id).await;
            let lastfm = self.lastfm(user_id).await.is_some();
            let jellyfin = match self.jellyfin.resolve(user_id, SourceName::Jellyfin).await {
                Ok(_) => true,
                Err(ResolveError::NotConfigured) => false,
                Err(error) => {
                    tracing::debug!(%error, "jellyfin unavailable for discover");
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

    fn lastfm_ready<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move { self.lastfm(user_id).await.is_some() })
    }

    fn sitewide_artists(
        &self,
        range: StatsRange,
        count: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Read<'_, Vec<ArtistRow>> {
        Box::pin(async move {
            let rows = provider(
                self.listenbrainz
                    .sitewide_top_artists(range.as_str(), count, offset, priority)
                    .await,
            )?;
            Ok(rows.into_iter().map(stat_artist).collect())
        })
    }

    fn sitewide_albums(
        &self,
        range: StatsRange,
        count: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Read<'_, Vec<ListenedAlbum>> {
        Box::pin(async move {
            let rows = provider(
                self.listenbrainz
                    .sitewide_top_release_groups(range.as_str(), count, offset, priority)
                    .await,
            )?;
            Ok(rows.into_iter().map(stat_album).collect())
        })
    }

    fn user_artists<'a>(
        &'a self,
        username: &'a str,
        range: StatsRange,
        count: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>> {
        Box::pin(async move {
            let rows = provider(
                self.listenbrainz
                    .user_top_artists(username, range.as_str(), count, offset, priority)
                    .await,
            )?;
            Ok(rows.into_iter().map(stat_artist).collect())
        })
    }

    fn user_albums<'a>(
        &'a self,
        username: &'a str,
        range: StatsRange,
        count: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ListenedAlbum>> {
        Box::pin(async move {
            let rows = provider(
                self.listenbrainz
                    .user_top_release_groups(username, range.as_str(), count, offset, priority)
                    .await,
            )?;
            Ok(rows.into_iter().map(stat_album).collect())
        })
    }

    fn similar_artists<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        limit: usize,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ScoredArtist>> {
        Box::pin(async move {
            let creds = self.credentials(user_id).await;
            let rows = provider(
                self.listenbrainz
                    .similar_artists(artist_mbid, limit, &creds, priority)
                    .await,
            )?;
            Ok(rows
                .into_iter()
                .map(|row| ScoredArtist {
                    score: row.listen_count as f64,
                    artist: ArtistRow {
                        name: row.artist_name,
                        mbid: Some(row.artist_mbid),
                        listen_count: row.listen_count,
                    },
                })
                .collect())
        })
    }

    fn artist_albums<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        count: usize,
        priority: RequestPriority,
    ) -> Read<'a, Vec<RankedAlbum>> {
        Box::pin(async move {
            let creds = self.credentials(user_id).await;
            let rows = provider(
                self.listenbrainz
                    .artist_top_release_groups(artist_mbid, count, &creds, priority)
                    .await,
            )?;
            Ok(rows
                .into_iter()
                .filter(|row| !row.name.is_empty())
                .map(|row| RankedAlbum {
                    album: AlbumRow {
                        release_group_mbid: row.release_group_mbid,
                        title: row.name,
                        artist_name: row.artist_name,
                        artist_mbid: Some(artist_mbid.to_lowercase()),
                    },
                    listen_count: row.listen_count,
                })
                .collect())
        })
    }

    fn genres<'a>(
        &'a self,
        username: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Vec<(String, i64)>> {
        Box::pin(async move {
            let rows = provider(
                self.listenbrainz
                    .user_genre_activity(username, priority)
                    .await,
            )?;
            Ok(rows
                .into_iter()
                .map(|row| (row.genre, row.listen_count))
                .collect())
        })
    }

    fn fresh_releases<'a>(
        &'a self,
        username: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Vec<AlbumRow>> {
        Box::pin(async move {
            let rows = provider(
                self.listenbrainz
                    .user_fresh_releases(username, priority)
                    .await,
            )?;
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
    }

    fn loved<'a>(
        &'a self,
        username: &'a str,
        count: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<LovedTrack>> {
        Box::pin(async move {
            let rows = provider(
                self.listenbrainz
                    .user_loved_recordings(username, count, priority)
                    .await,
            )?;
            Ok(rows
                .into_iter()
                .map(|row| LovedTrack {
                    image_url: row
                        .release_mbid
                        .as_deref()
                        .map(|release| format!("/api/v3/covers/release/{release}?size=250")),
                    name: row.track_name,
                    artist_name: row.artist_name,
                    artist_mbid: row.artist_mbid,
                    album_name: row.release_name,
                    recording_mbid: row.recording_mbid,
                })
                .collect())
        })
    }

    fn similar_users<'a>(
        &'a self,
        username: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Vec<String>> {
        Box::pin(async move { provider(self.listenbrainz.similar_users(username, priority).await) })
    }

    fn weekly_playlist<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<WeeklyPlaylist>> {
        Box::pin(async move {
            let creds = self.credentials(user_id).await;
            let lb = &self.listenbrainz;
            let playlists = provider(
                lb.recommendation_playlists(username, &creds, priority)
                    .await,
            )?;
            let Some(newest) = playlists
                .iter()
                .find(|playlist| playlist.source_patch == WEEKLY_EXPLORATION_PATCH)
                .or_else(|| playlists.first())
            else {
                return Ok(None);
            };
            let page = provider(lb.playlist(&newest.playlist_id, &creds, priority).await)?;
            if page.tracks.is_empty() {
                return Ok(None);
            }
            Ok(Some(WeeklyPlaylist {
                title: page.title,
                date: page.date,
                source_url: newest.identifier.clone(),
                tracks: page
                    .tracks
                    .into_iter()
                    .map(|track| PlaylistTrack {
                        title: track.title,
                        creator: track.creator,
                        album: track.album,
                        recording_mbid: track.recording_mbid,
                        artist_mbid: track.artist_mbids.into_iter().next(),
                        caa_release_mbid: track.caa_release_mbid,
                        duration_ms: track.duration_ms,
                    })
                    .collect(),
            }))
        })
    }

    fn recording_groups<'a>(
        &'a self,
        user_id: &'a str,
        recording_mbids: &'a [String],
        priority: RequestPriority,
    ) -> Read<'a, HashMap<String, String>> {
        Box::pin(async move {
            let creds = self.credentials(user_id).await;
            let found = provider(
                self.listenbrainz
                    .recording_release_groups(recording_mbids, &creds, priority)
                    .await,
            )?;
            Ok(found
                .into_iter()
                .map(|(recording, group)| (recording.to_ascii_lowercase(), group))
                .collect())
        })
    }

    fn listen_counts<'a>(
        &'a self,
        user_id: &'a str,
        release_group_mbids: &'a [String],
        priority: RequestPriority,
    ) -> Read<'a, HashMap<String, i64>> {
        Box::pin(async move {
            let creds = self.credentials(user_id).await;
            provider(
                self.listenbrainz
                    .release_group_popularity(release_group_mbids, &creds, priority)
                    .await,
            )
        })
    }

    fn lastfm_user_artists<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        period: &'a str,
        limit: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let rows = provider(
                client
                    .user_top_artists(&creds, username, period, limit, priority)
                    .await,
            )?;
            Ok(rows.into_iter().map(top_artist).collect())
        })
    }

    fn lastfm_user_albums<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        period: &'a str,
        limit: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<LastFmChartAlbum>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let rows = provider(
                client
                    .user_top_albums(&creds, username, period, limit, priority)
                    .await,
            )?;
            Ok(rows.into_iter().map(chart_album).collect())
        })
    }

    fn lastfm_similar<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a ArtistRow,
        limit: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ScoredArtist>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let rows = provider(
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
                .map(|row| ScoredArtist {
                    score: row.score,
                    artist: ArtistRow {
                        name: row.name,
                        mbid: row.mbid,
                        listen_count: 0,
                    },
                })
                .collect())
        })
    }

    fn lastfm_artist_albums<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a ArtistRow,
        limit: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<LastFmAlbum>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let rows = provider(
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
    }

    fn lastfm_chart_artists<'a>(
        &'a self,
        user_id: &'a str,
        limit: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let rows = provider(client.chart_top_artists(&creds, limit, priority).await)?;
            Ok(rows.into_iter().map(top_artist).collect())
        })
    }

    fn lastfm_album<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a str,
        album: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<LastFmFacts>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(None);
            };
            let info = provider(
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

    fn lastfm_artist<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a str,
        mbid: Option<&'a str>,
        priority: RequestPriority,
    ) -> Read<'a, Option<LastFmFacts>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(None);
            };
            let info = provider(client.artist_info(&creds, artist, mbid, priority).await)?;
            Ok(info.map(|info| LastFmFacts {
                mbid: info.mbid,
                tags: info.tags.into_iter().map(|tag| tag.name).collect(),
                summary: info.bio_summary,
            }))
        })
    }

    fn lastfm_weekly_artists<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let rows = provider(
                client
                    .user_weekly_artist_chart(&creds, username, priority)
                    .await,
            )?;
            Ok(rows.into_iter().map(top_artist).collect())
        })
    }

    fn lastfm_weekly_albums<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Vec<LastFmChartAlbum>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let rows = provider(
                client
                    .user_weekly_album_chart(&creds, username, priority)
                    .await,
            )?;
            Ok(rows.into_iter().map(chart_album).collect())
        })
    }

    fn lastfm_recent_albums<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        limit: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<LastFmChartAlbum>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let rows = provider(
                client
                    .user_recent_tracks(&creds, username, limit, priority)
                    .await,
            )?;
            Ok(rows
                .into_iter()
                .filter(|track| !track.album_name.is_empty())
                .map(|track| LastFmChartAlbum {
                    name: track.album_name,
                    artist_name: track.artist_name,
                    mbid: track.album_mbid,
                    playcount: 0,
                    image_url: track.image_url,
                })
                .collect())
        })
    }

    fn lastfm_tag_artists<'a>(
        &'a self,
        user_id: &'a str,
        tag: &'a str,
        limit: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let rows = provider(client.tag_top_artists(&creds, tag, limit, priority).await)?;
            Ok(rows.into_iter().map(top_artist).collect())
        })
    }

    fn lastfm_loved<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        limit: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<LovedTrack>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let rows = provider(
                client
                    .user_loved_tracks(&creds, username, limit, priority)
                    .await,
            )?;
            Ok(rows.into_iter().map(lastfm_loved).collect())
        })
    }

    fn tag_albums<'a>(
        &'a self,
        tag: &'a str,
        limit: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<TagAlbum>> {
        Box::pin(async move {
            let page = provider(
                self.musicbrainz(priority)
                    .search_release_groups_by_tag(tag, limit, offset, Criticality::BestEffort)
                    .await,
            )?;
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
                    Some(TagAlbum {
                        title: hit.title.filter(|title| !title.is_empty())?,
                        artist_name: credit_name(&hit.artist_credit),
                        artist_mbid: hit.artist_credit.first().map(|c| c.artist.id.clone()),
                        year: hit
                            .first_release_date
                            .as_deref()
                            .and_then(|date| date.get(..4))
                            .and_then(|year| year.parse().ok()),
                        release_group_mbid: hit.id,
                    })
                })
                .collect())
        })
    }

    fn tag_artists<'a>(
        &'a self,
        tag: &'a str,
        limit: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>> {
        Box::pin(async move {
            let page = provider(
                self.musicbrainz(priority)
                    .search_artists_by_tag_from(tag, limit, offset, Criticality::BestEffort)
                    .await,
            )?;
            let mut seen = std::collections::HashSet::new();
            Ok(page
                .items
                .into_iter()
                .filter(|hit| seen.insert(hit.id.to_lowercase()))
                .filter_map(|hit| {
                    Some(ArtistRow {
                        name: hit.name.filter(|name| !name.is_empty())?,
                        mbid: Some(hit.id),
                        listen_count: 0,
                    })
                })
                .collect())
        })
    }

    fn release_group_of<'a>(
        &'a self,
        release_mbid: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<String>> {
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

    fn release_group<'a>(
        &'a self,
        mbid: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<ReleaseGroupFacts>> {
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

    fn artist<'a>(
        &'a self,
        mbid: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<ArtistFacts>> {
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

    fn release_video<'a>(
        &'a self,
        release_mbid: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<String>> {
        Box::pin(async move {
            let found = musicbrainz_option(
                self.musicbrainz(priority)
                    .lookup_release(release_mbid, &["url-rels"], Criticality::BestEffort)
                    .await,
            )?;
            Ok(found.and_then(|lookup| first_youtube(&lookup.entity.relations)))
        })
    }

    fn release_recordings<'a>(
        &'a self,
        release_mbid: &'a str,
        limit: usize,
        priority: RequestPriority,
    ) -> Read<'a, Vec<String>> {
        Box::pin(async move {
            let found = musicbrainz_option(
                self.musicbrainz(priority)
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

    fn recording_video<'a>(
        &'a self,
        recording_mbid: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<String>> {
        Box::pin(async move {
            let found = musicbrainz_option(
                self.musicbrainz(priority)
                    .lookup_recording(recording_mbid, &["url-rels"], Criticality::BestEffort)
                    .await,
            )?;
            Ok(found.and_then(|lookup| first_youtube(&lookup.entity.relations)))
        })
    }

    fn wikipedia_extract<'a>(
        &'a self,
        url: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<String>> {
        Box::pin(async move {
            provider(
                self.upstream
                    .wikidata()
                    .get_bio_extract(url, "en", priority)
                    .await,
            )
        })
    }

    fn jellyfin_artists<'a>(
        &'a self,
        user_id: &'a str,
        list: JellyfinList,
        limit: u32,
    ) -> Read<'a, Vec<ArtistRow>> {
        Box::pin(async move {
            let Some(handle) = self.jellyfin_client(user_id).await? else {
                return Ok(Vec::new());
            };
            let adapter = handle
                .jellyfin("artist lists")
                .map_err(|error| ListeningError::MediaServer(error.to_string()))?;
            let artists = match list {
                JellyfinList::MostPlayed => adapter.most_played_artists(i64::from(limit)).await,
                JellyfinList::Favorites => adapter
                    .favorites(i64::from(limit))
                    .await
                    .map(|favorites| favorites.artists),
            }
            .map_err(|error| ListeningError::MediaServer(error.to_string()))?;
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

    fn jellyfin_plays<'a>(&'a self, user_id: &'a str, limit: u32) -> Read<'a, Vec<PlayedArtist>> {
        Box::pin(async move {
            let Some(handle) = self.jellyfin_client(user_id).await? else {
                return Ok(Vec::new());
            };
            let rows = handle
                .jellyfin("play counts")
                .map_err(|error| ListeningError::MediaServer(error.to_string()))?
                .most_played_artist_plays(i64::from(limit))
                .await
                .map_err(|error| ListeningError::MediaServer(error.to_string()))?;
            Ok(rows
                .into_iter()
                .map(|row| PlayedArtist {
                    name: row.artist.name,
                    mbid: row.artist.artist_mbid,
                    play_count: row.play_count,
                    last_played: row.last_played,
                    image_url: row.artist.image_url,
                })
                .collect())
        })
    }
}
