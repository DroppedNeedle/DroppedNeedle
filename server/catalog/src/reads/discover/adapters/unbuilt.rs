//! Discover stand-ins for when a live part cannot start: charts without
//! a ListenBrainz client, YouTube without its quota file, and the
//! presence feed on this router (live presence is served by playback).
//!
//! None of them invent rows: they answer empty or "not available".

use std::collections::HashMap;

use crate::reads::discover::{
    models::{
        ChartRange, ChartSource, GenreDetailResponse, NowPlayingEntry, PopularAlbumsPage,
        TrendingArtistsPage, YouTubeQuotaResponse,
    },
    ports::{BoxFuture, ChartsSource, NowPlayingStore, ProviderFailure, YouTubeSource},
};

/// Presence lives in playback; this router's feed is empty.
pub struct NoPresence;

impl NowPlayingStore for NoPresence {
    fn snapshot(&self) -> Vec<NowPlayingEntry> {
        Vec::new()
    }
}

/// Charts when no ListenBrainz client could be built.
pub struct UnavailableCharts;

impl ChartsSource for UnavailableCharts {
    fn trending_artists(
        &self,
        _range: ChartRange,
        _limit: i64,
        _offset: i64,
        _source: ChartSource,
    ) -> BoxFuture<'_, Result<TrendingArtistsPage, ProviderFailure>> {
        Box::pin(async {
            Err(ProviderFailure::not_built(
                "Charts are not available: the ListenBrainz client could not start.",
            ))
        })
    }

    fn popular_albums(
        &self,
        _range: ChartRange,
        _limit: i64,
        _offset: i64,
        _source: ChartSource,
    ) -> BoxFuture<'_, Result<PopularAlbumsPage, ProviderFailure>> {
        Box::pin(async {
            Err(ProviderFailure::not_built(
                "Charts are not available: the ListenBrainz client could not start.",
            ))
        })
    }

    fn your_top_albums<'a>(
        &'a self,
        _user_id: &'a str,
        _range: ChartRange,
        _limit: i64,
        _offset: i64,
        _source: ChartSource,
    ) -> BoxFuture<'a, Result<PopularAlbumsPage, ProviderFailure>> {
        Box::pin(async {
            Err(ProviderFailure::not_built(
                "Charts are not available: the ListenBrainz client could not start.",
            ))
        })
    }

    fn genre_detail<'a>(
        &'a self,
        _genre: &'a str,
        _limit: i64,
        _artist_offset: i64,
        _album_offset: i64,
    ) -> BoxFuture<'a, Result<GenreDetailResponse, ProviderFailure>> {
        Box::pin(async {
            Err(ProviderFailure::not_built(
                "Genre pages are not built in this version yet.",
            ))
        })
    }
}

/// YouTube when its quota file cannot be read: searches are off.
pub struct UnavailableYouTube;

impl YouTubeSource for UnavailableYouTube {
    fn is_configured(&self) -> bool {
        false
    }

    fn search_video<'a>(
        &'a self,
        _artist: &'a str,
        _album: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderFailure>> {
        Box::pin(async {
            Err(ProviderFailure::not_configured(
                "YouTube search is off: its quota file could not be read.",
            ))
        })
    }

    fn search_track<'a>(
        &'a self,
        _artist: &'a str,
        _track: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderFailure>> {
        Box::pin(async {
            Err(ProviderFailure::not_configured(
                "YouTube search is off: its quota file could not be read.",
            ))
        })
    }

    fn is_cached(&self, _artist: &str, _name: &str, _track: bool) -> bool {
        false
    }

    fn cached_tracks(&self, _pairs: &[(String, String)]) -> HashMap<String, bool> {
        HashMap::new()
    }

    fn quota(&self) -> BoxFuture<'_, Option<YouTubeQuotaResponse>> {
        Box::pin(async { None })
    }
}
