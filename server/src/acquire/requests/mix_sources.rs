//! [`MixSources`] over the live ListenBrainz client and the per-user link
//! store. Popularity and similar-artist reads are public, so they go out
//! without the user's token; playlist reads lend it.

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use sqlx::SqlitePool;

use super::mix::MixSources;
use crate::plugins::scrobble::ListenBrainzLinkStore;
use crate::providers::adapters::{CorePacer, CoreSink};
use crate::providers::listenbrainz::playlists::{RecommendationPlaylist, RecommendationTrack};
use crate::providers::listenbrainz::{
    ListenBrainzClient, ListenBrainzCredentials, Outcome, SimilarArtist, TopRecording,
    TopReleaseGroup,
};

/// Live ListenBrainz reads for the mix builder.
pub struct LiveMixSources {
    client: ListenBrainzClient<CorePacer, CoreSink>,
    links: Arc<dyn ListenBrainzLinkStore>,
    pool: SqlitePool,
}

impl LiveMixSources {
    /// Sources over one paced client, the link store and the database
    /// holding `user_connections`.
    pub fn new(
        client: ListenBrainzClient<CorePacer, CoreSink>,
        links: Arc<dyn ListenBrainzLinkStore>,
        pool: SqlitePool,
    ) -> Self {
        Self {
            client,
            links,
            pool,
        }
    }
}

/// Collapse an outcome: `Missing` is an empty answer, `Unavailable` an
/// error carrying ListenBrainz's reason.
fn settle<T: Default>(outcome: Outcome<T>) -> Result<T, String> {
    match outcome {
        Outcome::Found(value) => Ok(value),
        Outcome::Missing => Ok(T::default()),
        Outcome::Unavailable { message, .. } => Err(message),
    }
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
            sqlx::query_scalar(
                "SELECT user_id FROM user_connections \
                 WHERE service = 'listenbrainz' AND enabled = 1 ORDER BY user_id",
            )
            .fetch_all(&self.pool)
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
            settle(self.client.recommendation_playlists(username, creds).await)
        })
    }

    fn playlist_tracks<'a>(
        &'a self,
        playlist_id: &'a str,
        creds: &'a ListenBrainzCredentials,
    ) -> BoxFuture<'a, Result<Vec<RecommendationTrack>, String>> {
        Box::pin(async move { settle(self.client.playlist_tracks(playlist_id, creds).await) })
    }

    fn release_groups<'a>(
        &'a self,
        recording_mbids: &'a [String],
        creds: &'a ListenBrainzCredentials,
    ) -> BoxFuture<'a, Result<HashMap<String, String>, String>> {
        Box::pin(async move {
            settle(
                self.client
                    .recording_release_groups(recording_mbids, creds)
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
                    .similar_artists(artist_mbid, limit, &anonymous())
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
                    .artist_top_release_groups(artist_mbid, count, &anonymous())
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
                    .artist_top_recordings(artist_mbid, 1, &anonymous())
                    .await,
            )
            .map(|recordings| recordings.into_iter().next())
        })
    }
}

/// The production builder: live ListenBrainz reads paced on the shared
/// ListenBrainz limiter. `None` (logged) when no limiter row exists, which
/// leaves the refresh route answering "not available".
#[allow(clippy::too_many_arguments)]
pub fn live_builder(
    requests: &super::RequestsState,
    providers: Arc<crate::providers::Providers>,
    http: reqwest::Client,
    links: Arc<dyn ListenBrainzLinkStore>,
    prefs: Arc<dyn crate::plugins::scrobble::ScrobblePrefsStore>,
    playlists: crate::reads::collections::store::playlists::PlaylistStore,
    events: crate::events::EventSink,
) -> Option<Arc<super::mix::PersonalMixBuilder>> {
    use crate::providers::listenbrainz::{DEFAULT_BASE_URL, SOURCE};

    let Some(pacer) = CorePacer::for_source(providers, SOURCE) else {
        tracing::error!("no listenbrainz rate limit row; Weekly Mix is off");
        return None;
    };
    let client = ListenBrainzClient::new(http, DEFAULT_BASE_URL, pacer, CoreSink);
    let sources = Arc::new(LiveMixSources::new(
        client,
        links,
        requests.library.pool().clone(),
    ));
    Some(Arc::new(super::mix::PersonalMixBuilder::new(
        sources,
        prefs,
        playlists,
        requests.clone(),
        events,
    )))
}
