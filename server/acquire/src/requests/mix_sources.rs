//! [`MixSources`] over the one ListenBrainz client and the per-user link
//! store. Popularity and similar-artist reads are public, so they go out
//! without the user's token, and share the rows Discover keeps; playlist
//! reads lend it.
//!
//! Every read runs at background priority: the weekly sweep is scheduled,
//! and a manual refresh runs in a spawned task whose result reaches the
//! user as an event, so no request waits on it.

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::mix::MixSources;
use super::sql;
use droppedneedle_catalog::reads::discover::listening::shared;
use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_media::playback::listening::ListeningStore;
use droppedneedle_providers::listenbrainz::playlists::{
    RecommendationPlaylist, RecommendationTrack,
};
use droppedneedle_providers::listenbrainz::{
    ListenBrainzClient, ListenBrainzCredentials, SimilarArtist, TopRecording, TopReleaseGroup,
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
}

impl LiveMixSources {
    /// Sources over the ListenBrainz client and answer cache, the link
    /// store and the database holding `user_connections`.
    pub fn new(
        client: ListenBrainzClient,
        cache: ProviderCacheStore,
        links: ListeningStore,
        db: Db,
    ) -> Self {
        Self {
            client,
            cache,
            links,
            db,
        }
    }
}

/// A failed read becomes an error carrying ListenBrainz's reason.
fn settle<T>(result: Result<T, ProviderError>) -> Result<T, String> {
    result.map_err(|error| error.to_string())
}

fn anonymous() -> ListenBrainzCredentials {
    ListenBrainzCredentials::default()
}

impl MixSources for LiveMixSources {
    fn identity<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Option<ListenBrainzCredentials>> {
        Box::pin(async move {
            let link = self.links.listenbrainz_link(user_id).await?;
            let token = self.links.listenbrainz_token(user_id).await;
            Some(ListenBrainzCredentials {
                username: Some(link.username),
                user_token: token,
            })
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
        artist_mbid: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<SimilarArtist>, String>> {
        Box::pin(async move {
            let rows = settle(
                shared::similar_artists(&self.cache, &self.client, artist_mbid, limit, PRIORITY)
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

    fn top_release_groups<'a>(
        &'a self,
        artist_mbid: &'a str,
        count: usize,
    ) -> BoxFuture<'a, Result<Vec<TopReleaseGroup>, String>> {
        Box::pin(async move {
            let rows = settle(
                shared::artist_albums(&self.cache, &self.client, artist_mbid, count, PRIORITY)
                    .await,
            )?;
            Ok(rows
                .into_iter()
                .map(|row| TopReleaseGroup {
                    release_group_mbid: row.album.release_group_mbid,
                    name: row.album.title,
                    artist_name: row.album.artist_name,
                    listen_count: row.listen_count,
                })
                .collect())
        })
    }

    fn top_recording<'a>(
        &'a self,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<TopRecording>, String>> {
        Box::pin(async move {
            let key = CacheKey::new(
                Source::ListenBrainz,
                &["top_recording", &artist_mbid.to_lowercase()],
            );
            let (client, mbid) = (self.client.clone(), artist_mbid.to_owned());
            let found = self
                .cache
                .get_or_fetch(
                    &key,
                    Policy::LISTENBRAINZ_ARTIST,
                    PRIORITY,
                    move |priority| async move {
                        let recordings = client
                            .artist_top_recordings(&mbid, 1, &anonymous(), priority)
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
/// ListenBrainz client.
pub fn live_builder(
    requests: &super::RequestsState,
    providers: &droppedneedle_providers::Providers,
    listening: ListeningStore,
    playlists: droppedneedle_catalog::reads::collections::store::playlists::PlaylistStore,
    events: droppedneedle_kernel::events::EventHub,
) -> Arc<super::mix::PersonalMixBuilder> {
    let sources = Arc::new(LiveMixSources::new(
        providers.listenbrainz.clone(),
        providers.cache.clone(),
        listening.clone(),
        requests.library.db().clone(),
    ));
    Arc::new(super::mix::PersonalMixBuilder::new(
        sources,
        listening,
        playlists,
        requests.clone(),
        events,
    ))
}
