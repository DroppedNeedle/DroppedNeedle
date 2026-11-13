//! [`MixSources`] over the one ListenBrainz client and the per-user link
//! store, read only while an admin leaves ListenBrainz reads on. Every
//! read lends the user's own link, which ListenBrainz needs for an
//! artist's top songs. Popularity, similar-artist and album answers are
//! public, so their keys carry no token; similar artists and a
//! recording's album share the rows Discover keeps.
//!
//! Every read runs at background priority: the weekly sweep is scheduled,
//! and a manual refresh runs in a spawned task whose result reaches the
//! user as an event, so no request waits on it.

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::mix::MixSources;
use super::sql;
use droppedneedle_catalog::discover::listening::shared::{self, Lender};
use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::{ListenBrainzSettings, listenbrainz_reads_on};
use droppedneedle_media::playback::listening::ListeningStore;
use droppedneedle_providers::listenbrainz::playlists::{
    RecommendationPlaylist, RecommendationTrack,
};
use droppedneedle_providers::listenbrainz::{
    ListenBrainzClient, ListenBrainzCredentials, SimilarArtist, TopRecording,
};
use droppedneedle_providers::{
    CacheKey, Fetched, Policy, ProviderCacheStore, ProviderError, RequestPriority, Source,
};

/// The priority of every Weekly Mix read.
const PRIORITY: RequestPriority = RequestPriority::BackgroundSync;

/// Live ListenBrainz reads for the mix builder.
pub struct LiveMixSources {
    client: ListenBrainzClient,
    cache: ProviderCacheStore,
    links: ListeningStore,
    db: Db,
    switch: Live<ListenBrainzSettings>,
}

impl LiveMixSources {
    /// Sources over the ListenBrainz client and answer cache, the link
    /// store and the database holding `user_connections`, read while
    /// `switch` leaves ListenBrainz reads on.
    pub fn new(
        client: ListenBrainzClient,
        cache: ProviderCacheStore,
        links: ListeningStore,
        db: Db,
        switch: Live<ListenBrainzSettings>,
    ) -> Self {
        Self {
            client,
            cache,
            links,
            db,
            switch,
        }
    }

    /// The user's own link, read only when a fetch goes out.
    fn lender(&self, user_id: &str) -> Lender {
        Lender::Person(self.links.clone(), user_id.to_owned())
    }
}

/// A failed read becomes an error carrying ListenBrainz's reason.
fn settle<T>(result: Result<T, ProviderError>) -> Result<T, String> {
    result.map_err(|error| error.to_string())
}

impl MixSources for LiveMixSources {
    fn reads_on(&self) -> bool {
        listenbrainz_reads_on(&self.switch)
    }

    fn identity<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Option<ListenBrainzCredentials>> {
        Box::pin(async move {
            let creds = self.links.listenbrainz_credentials(user_id).await;
            creds.username.is_some().then_some(creds)
        })
    }

    fn linked_users(&self) -> BoxFuture<'_, Result<Vec<String>, String>> {
        Box::pin(async move {
            self.db
                .read("mix.linked_users", |conn| {
                    let mut statement = conn.prepare_cached(sql::LISTENBRAINZ_USERS.text())?;
                    let rows = statement.query_map([], |row| row.get(0))?;
                    Ok::<_, DbError>(rows.collect::<rusqlite::Result<Vec<String>>>()?)
                })
                .await
                .map_err(|error| format!("listenbrainz links read failed: {error}"))
        })
    }

    fn recommendation_playlists<'a>(
        &'a self,
        creds: &'a ListenBrainzCredentials,
    ) -> BoxFuture<'a, Result<Vec<RecommendationPlaylist>, String>> {
        Box::pin(async move {
            let username = creds.username.as_deref().unwrap_or_default();
            settle(
                self.client
                    .recommendation_playlists(username, creds, PRIORITY)
                    .await,
            )
        })
    }

    fn playlist_tracks<'a>(
        &'a self,
        playlist_id: &'a str,
        creds: &'a ListenBrainzCredentials,
    ) -> BoxFuture<'a, Result<Vec<RecommendationTrack>, String>> {
        Box::pin(async move {
            settle(
                self.client
                    .playlist_tracks(playlist_id, creds, PRIORITY)
                    .await,
            )
        })
    }

    fn release_groups<'a>(
        &'a self,
        recording_mbids: &'a [String],
        creds: &'a ListenBrainzCredentials,
    ) -> BoxFuture<'a, Result<HashMap<String, String>, String>> {
        Box::pin(async move {
            settle(
                self.client
                    .cached_recording_groups(&self.cache, recording_mbids, creds, PRIORITY)
                    .await,
            )
        })
    }

    fn similar_artists<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<SimilarArtist>, String>> {
        Box::pin(async move {
            let rows = settle(
                shared::similar_artists(
                    &self.cache,
                    &self.client,
                    artist_mbid,
                    limit,
                    &self.lender(user_id),
                    PRIORITY,
                )
                .await,
            )?;
            Ok(rows
                .into_iter()
                .filter_map(|row| {
                    Some(SimilarArtist {
                        artist_mbid: row.artist.mbid?,
                        artist_name: row.artist.name,
                        listen_count: row.artist.listen_count.unwrap_or_default(),
                    })
                })
                .collect())
        })
    }

    fn top_recording<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<TopRecording>, String>> {
        Box::pin(async move {
            let key = CacheKey::new(
                Source::ListenBrainz,
                &["top_recording", &artist_mbid.to_lowercase()],
            );
            let (client, mbid, lender) = (
                self.client.clone(),
                artist_mbid.to_owned(),
                self.lender(user_id),
            );
            let found = self
                .cache
                .get_or_fetch(
                    &key,
                    Policy::LISTENBRAINZ_ARTIST,
                    PRIORITY,
                    move |priority| async move {
                        let creds = lender.credentials().await;
                        let recordings = client
                            .artist_top_recordings(&mbid, 1, &creds, priority)
                            .await?;
                        Ok(recordings
                            .into_iter()
                            .next()
                            .map_or(Fetched::Missing, Fetched::Found))
                    },
                )
                .await;
            settle(found)
        })
    }
}

/// The production builder: live ListenBrainz reads through the one
/// ListenBrainz client, while `switch` leaves them on.
pub fn live_builder(
    requests: &super::RequestsState,
    providers: &droppedneedle_providers::Providers,
    listening: ListeningStore,
    playlists: droppedneedle_catalog::collections::store::playlists::PlaylistStore,
    events: droppedneedle_kernel::events::EventHub,
    switch: Live<ListenBrainzSettings>,
) -> Arc<super::mix::PersonalMixBuilder> {
    let sources = Arc::new(LiveMixSources::new(
        providers.listenbrainz.clone(),
        providers.cache.clone(),
        listening.clone(),
        requests.library.db().clone(),
        switch,
    ));
    Arc::new(super::mix::PersonalMixBuilder::new(
        sources,
        listening,
        playlists,
        requests.clone(),
        events,
    ))
}
