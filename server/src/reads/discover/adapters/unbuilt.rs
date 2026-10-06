//! Discover features whose builders are not ported yet: the queue deck,
//! discovery batches, radio, and the presence feed on this router.
//!
//! None of them invent rows. The queue reads as an empty, ready deck;
//! batch listings are empty and creating one answers "not available"
//! (a batch files real requests, which needs the acquisition port); radio
//! answers "not available". Live presence is served by playback, so this
//! router's copy is empty.

use std::collections::HashMap;

use crate::reads::discover::{
    models::{
        ChartRange, ChartSection, ChartSource, GenreDetailResponse, IgnoredRelease,
        NowPlayingEntry, PlaylistSuggestionsResponse, PopularAlbumsPage, QueueGenerateResponse,
        RadioPlanResponse, TrendingArtistsPage, YouTubeQuotaResponse,
    },
    ports::{
        BatchItemRow, BatchRow, BatchStore, BoxFuture, ChartsSource, Clock, NowPlayingStore,
        ProviderFailure, QueueBuild, QueueStore, RadioPlanner, YouTubeSource,
    },
};

/// Queue id of the empty deck.
const EMPTY_QUEUE_ID: &str = "empty";

/// An empty, ready queue deck.
pub struct EmptyQueue<C: Clock> {
    clock: C,
}

impl<C: Clock> EmptyQueue<C> {
    /// Stamp builds with `clock`.
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    fn empty(&self) -> QueueBuild {
        QueueBuild {
            status: "ready".to_owned(),
            queue_id: EMPTY_QUEUE_ID.to_owned(),
            items: Vec::new(),
            built_at: self.clock.now_unix(),
            error: None,
        }
    }
}

impl<C: Clock> QueueStore for EmptyQueue<C> {
    fn consume(&self, _user_id: &str) -> Option<QueueBuild> {
        None
    }

    fn build_lightweight(
        &self,
        _user_id: &str,
        _count: Option<i64>,
    ) -> Result<QueueBuild, ProviderFailure> {
        Ok(self.empty())
    }

    fn ensure_loaded(&self, _user_id: &str) {}

    fn status(&self, _user_id: &str) -> QueueBuild {
        self.empty()
    }

    fn start_build(&self, _user_id: &str, _force: bool) -> QueueGenerateResponse {
        let build = self.empty();
        QueueGenerateResponse {
            action: "built".to_owned(),
            status: build.status,
            queue_id: Some(build.queue_id),
            item_count: Some(0),
            built_at: Some(build.built_at),
            stale: Some(false),
            error: None,
        }
    }

    fn ignore_release(
        &self,
        _user_id: &str,
        release_group_mbid: &str,
        _artist_mbid: &str,
        _release_name: &str,
        _artist_name: &str,
    ) {
        // The deck is empty, so there is nothing to hide the release from.
        tracing::debug!(release_group_mbid, "queue ignore skipped: no queue builder");
    }

    fn ignored(&self, _user_id: &str) -> Vec<IgnoredRelease> {
        Vec::new()
    }
}

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
        Err(ProviderFailure::not_available("Discovery batches"))
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
        Box::pin(async { Err(ProviderFailure::not_available("Radio")) })
    }

    fn shelf<'a>(
        &'a self,
        _seed_type: &'a str,
        _seed_id: &'a str,
        _count: i64,
        _source: ChartSource,
    ) -> BoxFuture<'a, Result<ChartSection, ProviderFailure>> {
        Box::pin(async { Err(ProviderFailure::not_available("Radio")) })
    }

    fn playlist_suggestions<'a>(
        &'a self,
        _user_id: &'a str,
        _playlist_id: &'a str,
        _count: i64,
        _source: ChartSource,
    ) -> BoxFuture<'a, Result<PlaylistSuggestionsResponse, ProviderFailure>> {
        Box::pin(async { Err(ProviderFailure::not_available("Playlist suggestions")) })
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
        Box::pin(async { Err(ProviderFailure::not_available("Charts")) })
    }

    fn popular_albums(
        &self,
        _range: ChartRange,
        _limit: i64,
        _offset: i64,
        _source: ChartSource,
    ) -> BoxFuture<'_, Result<PopularAlbumsPage, ProviderFailure>> {
        Box::pin(async { Err(ProviderFailure::not_available("Charts")) })
    }

    fn your_top_albums<'a>(
        &'a self,
        _user_id: &'a str,
        _range: ChartRange,
        _limit: i64,
        _offset: i64,
        _source: ChartSource,
    ) -> BoxFuture<'a, Result<PopularAlbumsPage, ProviderFailure>> {
        Box::pin(async { Err(ProviderFailure::not_available("Charts")) })
    }

    fn genre_detail<'a>(
        &'a self,
        _genre: &'a str,
        _limit: i64,
        _artist_offset: i64,
        _album_offset: i64,
    ) -> BoxFuture<'a, Result<GenreDetailResponse, ProviderFailure>> {
        Box::pin(async { Err(ProviderFailure::not_available("Genre pages")) })
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
        Box::pin(async { Err(ProviderFailure::not_available("YouTube search")) })
    }

    fn search_track<'a>(
        &'a self,
        _artist: &'a str,
        _track: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderFailure>> {
        Box::pin(async { Err(ProviderFailure::not_available("YouTube search")) })
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
