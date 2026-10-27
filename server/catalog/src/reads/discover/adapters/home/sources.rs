//! What the home page and the genre page read from outside, as plain rows.
//!
//! Home sees only [`HomeSources`], so its builders run the same way over
//! the live providers and over a scripted test double. Every read is
//! fallible with a log-only cause: a failed read empties its shelf, it
//! never fails the page. The live implementation shares the discover
//! page's [`LiveSources`]: the same clients and the same provider cache.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::reads::catalog::mapping::should_include_release;
use crate::reads::discover::adapters::page::build::weekly_exploration;
use crate::reads::discover::adapters::page::live_sources::BUILD;
use crate::reads::discover::adapters::page::sources::{LastFmChartAlbum, PageSources, RankedAlbum};
use crate::reads::discover::adapters::queue::live_sources::{
    ARTIST_TTL, LiveSources, USER_ACTIVITY_TTL, USER_STATS_TTL, credit_name, lastfm_result,
};
use crate::reads::discover::adapters::queue::sources::{
    ArtistRow, QueueSources, SourceResult, StatsRange, UserMusic,
};
use crate::reads::discover::models::WeeklyExploration;
use crate::reads::discover::ports::{BoxFuture, ProviderFailure};
use droppedneedle_providers::musicbrainz::Criticality;
use droppedneedle_providers::{CacheKey, RequestPriority, Source};

/// One loved track (ListenBrainz feedback or Last.fm loved tracks).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LovedTrack {
    /// Track title.
    pub name: String,
    /// Credited artist name.
    pub artist_name: String,
    /// First credited artist MBID, when known.
    pub artist_mbid: Option<String>,
    /// Album title, when known.
    pub album_name: Option<String>,
    /// Recording MBID, when known.
    pub recording_mbid: Option<String>,
    /// Cover URL, when known.
    pub image_url: Option<String>,
}

/// One release group carrying a tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TagAlbum {
    /// Release-group MBID.
    pub release_group_mbid: String,
    /// Album title.
    pub title: String,
    /// Credited artist name.
    pub artist_name: String,
    /// First release year, when known.
    pub year: Option<i64>,
}

/// Every outside read home and the genre page make.
pub trait HomeSources: Send + Sync {
    /// The MusicBrainz source in use; a build that sees it change drops
    /// its result.
    fn source_key(&self) -> String;
    /// The user's listening services.
    fn user_music<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, UserMusic>;

    /// This week's most listened artists on ListenBrainz.
    fn listenbrainz_trending_artists(
        &self,
        count: u32,
    ) -> BoxFuture<'_, SourceResult<Vec<ArtistRow>>>;
    /// This week's most listened release groups on ListenBrainz.
    fn listenbrainz_popular_albums(
        &self,
        count: u32,
    ) -> BoxFuture<'_, SourceResult<Vec<RankedAlbum>>>;
    /// A ListenBrainz user's top release groups this month.
    fn listenbrainz_user_albums<'a>(
        &'a self,
        username: &'a str,
        count: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<RankedAlbum>>>;
    /// A ListenBrainz user's loved recordings.
    fn listenbrainz_loved<'a>(
        &'a self,
        username: &'a str,
        count: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<LovedTrack>>>;
    /// A ListenBrainz user's genres with their listens.
    fn listenbrainz_genres<'a>(
        &'a self,
        username: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<(String, i64)>>>;
    /// The user's weekly exploration playlist, when there is one.
    fn listenbrainz_weekly<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
    ) -> BoxFuture<'a, Option<WeeklyExploration>>;

    /// The sitewide Last.fm artist chart.
    fn lastfm_trending_artists<'a>(
        &'a self,
        user_id: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>>;
    /// A Last.fm user's top albums this month.
    fn lastfm_user_albums<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<LastFmChartAlbum>>>;
    /// A Last.fm user's loved tracks.
    fn lastfm_loved<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<LovedTrack>>>;

    /// One page of artists carrying a MusicBrainz tag.
    fn musicbrainz_tag_artists<'a>(
        &'a self,
        tag: &'a str,
        limit: u32,
        offset: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>>;
    /// One page of release groups carrying a MusicBrainz tag, with the
    /// release types the user excludes left out.
    fn musicbrainz_tag_albums<'a>(
        &'a self,
        tag: &'a str,
        limit: u32,
        offset: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<TagAlbum>>>;
}

/// How long a tag search is reused: twice the search lifetime.
fn tag_ttl() -> Duration {
    crate::reads::catalog::SEARCH_TTL * 2
}

fn loved_from_lastfm(row: droppedneedle_providers::lastfm::TopItem) -> LovedTrack {
    LovedTrack {
        name: row.name,
        artist_name: row.artist_name,
        artist_mbid: None,
        album_name: None,
        recording_mbid: None,
        image_url: Some(row.image_url).filter(|url| !url.is_empty()),
    }
}

impl HomeSources for LiveSources {
    fn source_key(&self) -> String {
        QueueSources::source_key(self)
    }

    fn user_music<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, UserMusic> {
        QueueSources::user_music(self, user_id)
    }

    fn listenbrainz_trending_artists(
        &self,
        count: u32,
    ) -> BoxFuture<'_, SourceResult<Vec<ArtistRow>>> {
        self.listenbrainz_sitewide_artists(count)
    }

    fn listenbrainz_popular_albums(
        &self,
        count: u32,
    ) -> BoxFuture<'_, SourceResult<Vec<RankedAlbum>>> {
        self.listenbrainz_trending_ranked(count)
    }

    fn listenbrainz_user_albums<'a>(
        &'a self,
        username: &'a str,
        count: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<RankedAlbum>>> {
        self.listenbrainz_user_ranked(username, StatsRange::ThisMonth, count)
    }

    fn listenbrainz_loved<'a>(
        &'a self,
        username: &'a str,
        count: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<LovedTrack>>> {
        Box::pin(async move {
            let key = CacheKey::new(
                Source::ListenBrainz,
                &["home_loved", &username.to_lowercase(), &count.to_string()],
            )
            .personal();
            self.cached(key, USER_ACTIVITY_TTL, async {
                let rows = self.listenbrainz_result(
                    self.lb()
                        .user_loved_recordings(username, count, BUILD)
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
            .await
        })
    }

    fn listenbrainz_genres<'a>(
        &'a self,
        username: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<(String, i64)>>> {
        self.listenbrainz_genre_counts(username)
    }

    fn listenbrainz_weekly<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
    ) -> BoxFuture<'a, Option<WeeklyExploration>> {
        Box::pin(weekly_exploration(self, user_id, username))
    }

    fn lastfm_trending_artists<'a>(
        &'a self,
        user_id: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        self.lastfm_chart_artists(user_id, limit, BUILD)
    }

    fn lastfm_user_albums<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<LastFmChartAlbum>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let key = CacheKey::new(
                Source::LastFm,
                &[
                    "home_top_albums",
                    &username.to_lowercase(),
                    &limit.to_string(),
                ],
            )
            .personal();
            self.cached(key, USER_STATS_TTL, async {
                let rows = lastfm_result(
                    client
                        .user_top_albums(&creds, username, "1month", limit, BUILD)
                        .await,
                )?;
                Ok(rows
                    .into_iter()
                    .map(|row| LastFmChartAlbum {
                        name: row.name,
                        artist_name: row.artist_name,
                        mbid: row.mbid,
                        playcount: row.playcount,
                        image_url: row.image_url,
                    })
                    .collect())
            })
            .await
        })
    }

    fn lastfm_loved<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<LovedTrack>>> {
        Box::pin(async move {
            let Some((client, creds)) = self.lastfm(user_id).await else {
                return Ok(Vec::new());
            };
            let key = CacheKey::new(
                Source::LastFm,
                &["home_loved", &username.to_lowercase(), &limit.to_string()],
            )
            .personal();
            self.cached(key, USER_ACTIVITY_TTL, async {
                let rows = lastfm_result(
                    client
                        .user_loved_tracks(&creds, username, limit, BUILD)
                        .await,
                )?;
                Ok(rows.into_iter().map(loved_from_lastfm).collect())
            })
            .await
        })
    }

    fn musicbrainz_tag_artists<'a>(
        &'a self,
        tag: &'a str,
        limit: u32,
        offset: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        Box::pin(async move {
            let key = CacheKey::new(
                Source::MusicBrainz,
                &[
                    &QueueSources::source_key(self),
                    "genre_artists",
                    &tag.trim().to_lowercase(),
                    &limit.to_string(),
                    &offset.to_string(),
                ],
            );
            self.cached(key, tag_ttl().max(ARTIST_TTL), async {
                let page = self
                    .musicbrainz(RequestPriority::UserInitiated)
                    .search_artists_by_tag_from(tag, limit, offset, Criticality::BestEffort)
                    .await
                    .map_err(ProviderFailure::Provider)?;
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
            .await
        })
    }

    fn musicbrainz_tag_albums<'a>(
        &'a self,
        tag: &'a str,
        limit: u32,
        offset: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<TagAlbum>>> {
        Box::pin(async move {
            let key = CacheKey::new(
                Source::MusicBrainz,
                &[
                    &QueueSources::source_key(self),
                    "genre_albums",
                    &tag.trim().to_lowercase(),
                    &limit.to_string(),
                    &offset.to_string(),
                ],
            );
            self.cached(key, tag_ttl(), async {
                // Ask for extra rows: the type exclusions drop some (v2).
                let asked = ((limit as f64 * 1.5) as u32).clamp(25, 100);
                let page = self
                    .musicbrainz(RequestPriority::UserInitiated)
                    .search_release_groups_by_tag(tag, asked, offset, Criticality::BestEffort)
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
                        Some(TagAlbum {
                            title: hit.title.filter(|title| !title.is_empty())?,
                            artist_name: credit_name(&hit.artist_credit),
                            year: hit
                                .first_release_date
                                .as_deref()
                                .and_then(|date| date.get(..4))
                                .and_then(|year| year.parse().ok()),
                            release_group_mbid: hit.id,
                        })
                    })
                    .take(limit as usize)
                    .collect())
            })
            .await
        })
    }
}
