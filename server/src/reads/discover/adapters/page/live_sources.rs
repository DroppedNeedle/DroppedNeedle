//! [`PageSources`] over the queue's live provider reads.
//!
//! The page shares the queue's [`LiveSources`]: the same clients, the same
//! shared provider cache and the same ListenBrainz popularity health
//! signal, so a shelf and a queue card never pay for the same read twice.

use std::collections::HashMap;

use super::sources::{
    LastFmChartAlbum, PageSources, PlayedArtist, PlaylistTrack, RankedAlbum, ScoredArtist,
    SourceContext, WeeklyPlaylist,
};
use crate::providers::RequestPriority;
use crate::providers::musicbrainz::Criticality;
use crate::reads::discover::adapters::queue::live_sources::{
    ARTIST_TTL, LiveSources, USER_STATS_TTL, lastfm_result,
};
use crate::reads::discover::adapters::queue::sources::{
    AlbumRow, ArtistRow, QueueSources, SourceResult, StatsRange,
};
use crate::reads::discover::ports::BoxFuture;
use crate::remotes::connections::ResolveError;
use crate::remotes::jellyfin::JellyfinAdapter;
use crate::remotes::models::SourceName;

/// The algorithm ListenBrainz names its weekly exploration playlist after.
const WEEKLY_EXPLORATION_PATCH: &str = "weekly-exploration";

fn ranked(row: crate::providers::listenbrainz::stats::ReleaseGroupStat) -> Option<RankedAlbum> {
    Some(RankedAlbum {
        album: AlbumRow {
            release_group_mbid: row.release_group_mbid?,
            title: row.release_group_name,
            artist_name: row.artist_name,
            artist_mbid: row.artist_mbids.into_iter().next(),
        },
        listen_count: row.listen_count,
    })
}

fn chart_album(row: crate::providers::lastfm::TopItem) -> LastFmChartAlbum {
    LastFmChartAlbum {
        name: row.name,
        artist_name: row.artist_name,
        mbid: row.mbid,
        playcount: row.playcount,
        image_url: row.image_url,
    }
}

impl PageSources for LiveSources {
    fn source_context(&self) -> SourceContext {
        let settings = self.upstream.settings().musicbrainz();
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

    fn listenbrainz_sitewide_artists(
        &self,
        count: u32,
    ) -> BoxFuture<'_, SourceResult<Vec<ArtistRow>>> {
        Box::pin(async move {
            let key = format!("lb_page:sitewide_artists:{count}");
            self.cached(key, ARTIST_TTL, async {
                let rows = self.listenbrainz_result(
                    self.lb()?.sitewide_top_artists("this_week", count, 0).await,
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

    fn listenbrainz_similar_scored<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, SourceResult<Vec<ScoredArtist>>> {
        Box::pin(async move {
            let rows = self
                .listenbrainz_similar_artists(user_id, artist_mbid, limit)
                .await?;
            Ok(rows
                .into_iter()
                .map(|artist| ScoredArtist {
                    score: artist.listen_count as f64,
                    artist,
                })
                .collect())
        })
    }

    fn listenbrainz_artist_ranked<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        count: usize,
    ) -> BoxFuture<'a, SourceResult<Vec<RankedAlbum>>> {
        Box::pin(async move {
            if self.listenbrainz_popularity_down() {
                return Ok(Vec::new());
            }
            let key = format!(
                "lb_page:artist_ranked:{}:{count}",
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
            .await
        })
    }

    fn listenbrainz_trending_ranked(
        &self,
        count: u32,
    ) -> BoxFuture<'_, SourceResult<Vec<RankedAlbum>>> {
        Box::pin(async move {
            let key = format!("lb_page:trending_ranked:{count}");
            self.cached(key, ARTIST_TTL, async {
                let rows = self.listenbrainz_result(
                    self.lb()?
                        .sitewide_top_release_groups("this_week", count, 0)
                        .await,
                )?;
                Ok(rows.into_iter().filter_map(ranked).collect())
            })
            .await
        })
    }

    fn listenbrainz_user_ranked<'a>(
        &'a self,
        username: &'a str,
        range: StatsRange,
        count: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<RankedAlbum>>> {
        Box::pin(async move {
            let key = format!(
                "lb_page:user_ranked:{}:{}:{count}",
                username.to_lowercase(),
                range.as_str()
            );
            self.cached(key, USER_STATS_TTL, async {
                let rows = self.listenbrainz_result(
                    self.lb()?
                        .user_top_release_groups(username, range.as_str(), count, 0)
                        .await,
                )?;
                Ok(rows.into_iter().filter_map(ranked).collect())
            })
            .await
        })
    }

    fn listenbrainz_genre_counts<'a>(
        &'a self,
        username: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<(String, i64)>>> {
        Box::pin(async move {
            let key = format!("lb_page:genre_counts:{}", username.to_lowercase());
            self.cached(key, ARTIST_TTL, async {
                let rows =
                    self.listenbrainz_result(self.lb()?.user_genre_activity(username).await)?;
                Ok(rows
                    .into_iter()
                    .map(|row| (row.genre, row.listen_count))
                    .collect())
            })
            .await
        })
    }

    fn listenbrainz_similar_users<'a>(
        &'a self,
        username: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>> {
        Box::pin(async move {
            let key = format!("lb_page:similar_users:{}", username.to_lowercase());
            self.cached(key, ARTIST_TTL, async {
                self.listenbrainz_result(self.lb()?.similar_users(username).await)
            })
            .await
        })
    }

    fn listenbrainz_weekly_playlist<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<WeeklyPlaylist>>> {
        Box::pin(async move {
            let key = format!("lb_page:weekly:{}", username.to_lowercase());
            self.cached(key, USER_STATS_TTL, async {
                let creds = self.credentials(user_id).await;
                let lb = self.lb()?;
                let playlists =
                    self.listenbrainz_result(lb.recommendation_playlists(username, &creds).await)?;
                let Some(newest) = playlists
                    .iter()
                    .find(|playlist| playlist.source_patch == WEEKLY_EXPLORATION_PATCH)
                    .or_else(|| playlists.first())
                else {
                    return Ok(None);
                };
                let page =
                    self.listenbrainz_result(lb.playlist(&newest.playlist_id, &creds).await)?;
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
            .await
        })
    }

    fn listenbrainz_recording_groups<'a>(
        &'a self,
        user_id: &'a str,
        recording_mbids: &'a [String],
    ) -> BoxFuture<'a, SourceResult<HashMap<String, String>>> {
        Box::pin(async move {
            let creds = self.credentials(user_id).await;
            let found = self.listenbrainz_result(
                self.lb()?
                    .recording_release_groups(recording_mbids, &creds)
                    .await,
            )?;
            Ok(found
                .into_iter()
                .map(|(recording, group)| (recording.to_ascii_lowercase(), group))
                .collect())
        })
    }

    fn lastfm_similar_scored<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a ArtistRow,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ScoredArtist>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let id = artist
                .mbid
                .clone()
                .unwrap_or_else(|| artist.name.trim().to_lowercase());
            let key = format!("lfm_page:similar_scored:{id}:{limit}");
            self.cached(key, ARTIST_TTL, async {
                let rows = lastfm_result(
                    client
                        .similar_artists(&creds, &artist.name, artist.mbid.as_deref(), limit)
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
            .await
        })
    }

    fn lastfm_weekly_artists<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let key = format!("lfm_page:weekly_artists:{}", username.to_lowercase());
            self.cached(key, USER_STATS_TTL, async {
                let rows = lastfm_result(client.user_weekly_artist_chart(&creds, username).await)?;
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

    fn lastfm_weekly_albums<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<LastFmChartAlbum>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let key = format!("lfm_page:weekly_albums:{}", username.to_lowercase());
            self.cached(key, USER_STATS_TTL, async {
                let rows = lastfm_result(client.user_weekly_album_chart(&creds, username).await)?;
                Ok(rows.into_iter().map(chart_album).collect())
            })
            .await
        })
    }

    fn lastfm_recent_albums<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<LastFmChartAlbum>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let key = format!("lfm_page:recent:{}:{limit}", username.to_lowercase());
            self.cached(key, USER_STATS_TTL, async {
                let rows = lastfm_result(client.user_recent_tracks(&creds, username, limit).await)?;
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
            .await
        })
    }

    fn lastfm_artist_tags<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a ArtistRow,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let id = artist
                .mbid
                .clone()
                .unwrap_or_else(|| artist.name.trim().to_lowercase());
            let key = format!("lfm_page:artist_tags:{id}");
            self.cached(key, ARTIST_TTL, async {
                let info = match client
                    .artist_info(&creds, &artist.name, artist.mbid.as_deref())
                    .await
                {
                    crate::providers::lastfm::Outcome::Found(info) => info,
                    crate::providers::lastfm::Outcome::Missing => return Ok(Vec::new()),
                    crate::providers::lastfm::Outcome::Unavailable { message, .. } => {
                        return Err(message);
                    }
                };
                Ok(info
                    .tags
                    .into_iter()
                    .map(|tag| tag.name)
                    .filter(|name| !name.trim().is_empty())
                    .collect())
            })
            .await
        })
    }

    fn lastfm_tag_artists<'a>(
        &'a self,
        user_id: &'a str,
        tag: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let key = format!("lfm_page:tag_artists:{}:{limit}", tag.to_lowercase());
            self.cached(key, ARTIST_TTL, async {
                let rows = lastfm_result(client.tag_top_artists(&creds, tag, limit).await)?;
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

    fn musicbrainz_tag_artists<'a>(
        &'a self,
        tag: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        Box::pin(async move {
            let key = format!(
                "mb_artists_by_tag:{}:{}:{limit}",
                self.source_key(),
                tag.trim().to_lowercase()
            );
            self.cached(key, ARTIST_TTL, async {
                let page = self
                    .musicbrainz(RequestPriority::BackgroundSync)
                    .search_artists_by_tag(tag, limit, Criticality::BestEffort)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(page
                    .items
                    .into_iter()
                    .filter_map(|hit| {
                        Some(ArtistRow {
                            name: hit.name.filter(|name| !name.is_empty())?,
                            mbid: Some(hit.id),
                            listen_count: 0,
                        })
                    })
                    .collect())
            })
            .await
        })
    }

    fn jellyfin_artist_plays<'a>(
        &'a self,
        user_id: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<PlayedArtist>>> {
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
            let rows = adapter
                .most_played_artist_plays(i64::from(limit))
                .await
                .map_err(|error| error.to_string())?;
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
