//! Discover features whose builders are not ported yet: discovery
//! batches, radio, and the presence feed on this router.
//!
//! None of them invent rows. Batch listings are empty and creating one
//! answers "not available" (a batch files real requests, which needs the
//! acquisition port); radio answers "not available". Live presence is
//! served by playback, so this router's copy is empty.

use std::collections::HashMap;

use crate::reads::discover::{
    models::{
        ChartRange, ChartSection, ChartSource, GenreDetailResponse, NowPlayingEntry,
        PlaylistSuggestionsResponse, PopularAlbumsPage, RadioPlanResponse, TrendingArtistsPage,
        YouTubeQuotaResponse,
    },
    ports::{
        BatchItemRow, BatchRow, BatchStore, BoxFuture, ChartsSource, NowPlayingStore,
        ProviderFailure, RadioPlanner, YouTubeSource,
    },
};

/// Discovery batches without the acquisition port behind them.
pub struct UnavailableBatches;

impl BatchStore for UnavailableBatches {
    fn create(
        &self,
        _owner_id: &str,
        _name: &str,
        _source_section: &str,
        _items: Vec<BatchItemRow>,
    ) -> Result<BatchRow, ProviderFailure> {
        Err(ProviderFailure::not_built(
            "Discovery batches are not built in this version yet.",
        ))
    }

    fn list_for_user(&self, _owner_id: &str) -> Vec<BatchRow> {
        Vec::new()
    }

    fn get_for_user(&self, _owner_id: &str, _batch_id: &str) -> Option<BatchRow> {
        None
    }

    fn remove(
        &self,
        _owner_id: &str,
        _batch_id: &str,
        _remove_albums: bool,
    ) -> Option<(i64, i64, i64)> {
        None
    }
}

/// Radio before its planner is ported.
pub struct UnavailableRadio;

impl RadioPlanner for UnavailableRadio {
    fn plan<'a>(
        &'a self,
        _user_id: &'a str,
        _seed_type: &'a str,
        _seed_id: Option<&'a str>,
        _mode: &'a str,
        _count: i64,
        _exclude: &'a [String],
    ) -> BoxFuture<'a, Result<RadioPlanResponse, ProviderFailure>> {
        Box::pin(async {
            Err(ProviderFailure::not_built(
                "Radio is not built in this version yet.",
            ))
        })
    }

    fn shelf<'a>(
        &'a self,
        _seed_type: &'a str,
        _seed_id: &'a str,
        _count: i64,
        _source: ChartSource,
    ) -> BoxFuture<'a, Result<ChartSection, ProviderFailure>> {
        Box::pin(async {
            Err(ProviderFailure::not_built(
                "Radio is not built in this version yet.",
            ))
        })
    }

    fn playlist_suggestions<'a>(
        &'a self,
        _user_id: &'a str,
        _playlist_id: &'a str,
        _count: i64,
        _source: ChartSource,
    ) -> BoxFuture<'a, Result<PlaylistSuggestionsResponse, ProviderFailure>> {
        Box::pin(async {
            Err(ProviderFailure::not_built(
                "Playlist suggestions are not built in this version yet.",
            ))
        })
    }
}

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
