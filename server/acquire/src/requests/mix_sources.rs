//! [`MixSources`] over the one ListenBrainz client and the per-user link
//! store. Popularity and similar-artist reads are public, so they go out
//! without the user's token; playlist reads lend it.
//!
//! Every read runs at background priority: the weekly sweep is scheduled,
//! and a manual refresh runs in a spawned task whose result reaches the
//! user as an event, so no request waits on it.

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::mix::MixSources;
use super::sql;
use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_media::playback::listening::{ListenBrainzLinkStore, ListeningStore};
use droppedneedle_providers::listenbrainz::playlists::{
    RecommendationPlaylist, RecommendationTrack,
};
use droppedneedle_providers::listenbrainz::{
    ListenBrainzClient, ListenBrainzCredentials, SimilarArtist, TopRecording, TopReleaseGroup,
};
use droppedneedle_providers::{ProviderError, RequestPriority};

/// The priority of every Weekly Mix read.
const PRIORITY: RequestPriority = RequestPriority::BackgroundSync;

/// Live ListenBrainz reads for the mix builder.
pub struct LiveMixSources {
    client: ListenBrainzClient,
    links: Arc<dyn ListenBrainzLinkStore>,
    db: Db,
}

impl LiveMixSources {
    /// Sources over the ListenBrainz client, the link store and the
    /// database holding `user_connections`.
    pub fn new(client: ListenBrainzClient, links: Arc<dyn ListenBrainzLinkStore>, db: Db) -> Self {
        Self { client, links, db }
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
            let link = self.links.status(user_id).await?;
            let token = self.links.token_for(user_id).await;
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
                    .recording_release_groups(recording_mbids, creds, PRIORITY)
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
            settle(
                self.client
                    .similar_artists(artist_mbid, limit, &anonymous(), PRIORITY)
                    .await,
            )
        })
    }

    fn top_release_groups<'a>(
        &'a self,
        artist_mbid: &'a str,
        count: usize,
    ) -> BoxFuture<'a, Result<Vec<TopReleaseGroup>, String>> {
        Box::pin(async move {
            settle(
                self.client
                    .artist_top_release_groups(artist_mbid, count, &anonymous(), PRIORITY)
                    .await,
            )
        })
    }

    fn top_recording<'a>(
        &'a self,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<TopRecording>, String>> {
        Box::pin(async move {
            settle(
                self.client
                    .artist_top_recordings(artist_mbid, 1, &anonymous(), PRIORITY)
                    .await,
            )
            .map(|recordings| recordings.into_iter().next())
        })
    }
}

/// The production builder: live ListenBrainz reads through the one
/// ListenBrainz client.
pub fn live_builder(
    requests: &super::RequestsState,
    providers: &droppedneedle_providers::Providers,
    links: Arc<dyn ListenBrainzLinkStore>,
    prefs: ListeningStore,
    playlists: droppedneedle_catalog::reads::collections::store::playlists::PlaylistStore,
    events: droppedneedle_kernel::events::EventHub,
) -> Arc<super::mix::PersonalMixBuilder> {
    let sources = Arc::new(LiveMixSources::new(
        providers.listenbrainz.clone(),
        links,
        requests.library.db().clone(),
    ));
    Arc::new(super::mix::PersonalMixBuilder::new(
        sources,
        prefs,
        playlists,
        requests.clone(),
        events,
    ))
}
