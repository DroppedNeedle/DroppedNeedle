//! Wrapped from ListenBrainz: each user's year in review.
//!
//! A user's stats come from their linked
//! ListenBrainz account (`this_year` top artists, recordings and release
//! groups, ten of each, genre activity, and a loved-recordings sample);
//! the server summary adds up the users who have data and takes the
//! sitewide top artist and album. A failed leg is logged and left empty
//! rather than failing the whole payload. Users without a link, and
//! everyone while an admin has ListenBrainz reads switched off, get the
//! empty payload with their display name.

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

use super::{
    ServerWrappedResponse, UserWrappedResponse, WrappedAlbum, WrappedArtist, WrappedData,
    WrappedGenre, WrappedLeaderboardEntry, WrappedTrack, WrappedUserSummary,
};
use droppedneedle_auth::sqlite::Users;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::{ListenBrainzSettings, listenbrainz_reads_on};
use droppedneedle_media::playback::listening::ListeningStore;
use droppedneedle_providers::listenbrainz::ListenBrainzClient;
use droppedneedle_providers::{
    CacheKey, Fetched, Policy, ProviderCacheStore, ProviderError, RequestPriority, Source,
};

/// Rows per top list.
const TOP_N: u32 = 10;
/// Stats range.
const RANGE: &str = "this_year";
/// Users read per page.
const USERS_PAGE: u64 = 500;
/// Loved-recordings sample size; ListenBrainz caps the page at 100.
const LOVED_SAMPLE: u32 = 100;
/// Wrapped stats cache policy: 6h fresh, 30m missing (with singleflight).
const WRAPPED_POLICY: Policy = Policy {
    fresh: Duration::from_secs(6 * 3_600),
    miss: Duration::from_secs(30 * 60),
};

/// Loading wrapped is a page a person waits on.
const PRIORITY: RequestPriority = RequestPriority::UserInitiated;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CachedUserWrapped {
    has_data: bool,
    top_artists: Vec<WrappedArtist>,
    top_tracks: Vec<WrappedTrack>,
    top_albums: Vec<WrappedAlbum>,
    top_genres: Vec<WrappedGenre>,
    loved_tracks_count: i64,
    total_listens_estimated: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CachedSitewide {
    top_artist: Option<WrappedArtist>,
    top_album: Option<WrappedAlbum>,
}

/// Wrapped over the user store, the ListenBrainz links, and the
/// ListenBrainz client.
pub struct ListenBrainzWrapped {
    users: Users,
    links: ListeningStore,
    client: ListenBrainzClient,
    cache: ProviderCacheStore,
    switch: Live<ListenBrainzSettings>,
    clock: Arc<dyn Clock>,
}

impl ListenBrainzWrapped {
    /// Wire the data source; `switch` says whether ListenBrainz may be
    /// read and `clock` which year is current.
    pub fn new(
        users: Users,
        links: ListeningStore,
        client: ListenBrainzClient,
        cache: ProviderCacheStore,
        switch: Live<ListenBrainzSettings>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            users,
            links,
            client,
            cache,
            switch,
            clock,
        }
    }

    async fn all_users(&self) -> Vec<droppedneedle_auth::users::models::UserRecord> {
        let mut all = Vec::new();
        let mut offset = 0;
        loop {
            match self.users.list(USERS_PAGE, offset).await {
                Ok((page, _)) => {
                    let short = (page.len() as u64) < USERS_PAGE;
                    offset += page.len() as u64;
                    all.extend(page);
                    if short {
                        break;
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "wrapped could not list users; listing what it has");
                    break;
                }
            }
        }
        all
    }

    async fn build_user(&self, user_id: &str) -> Option<UserWrappedResponse> {
        let display_name = match self.users.get_by_id(user_id).await {
            Ok(Some(user)) => user.display_name,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(%error, "wrapped could not read the user");
                user_id.to_owned()
            }
        };
        let year = self.current_year();
        let mut payload = UserWrappedResponse {
            user_id: user_id.to_owned(),
            display_name,
            year,
            has_data: false,
            top_artists: Vec::new(),
            top_tracks: Vec::new(),
            top_albums: Vec::new(),
            top_genres: Vec::new(),
            loved_tracks_count: 0,
            total_listens_estimated: 0,
        };
        if !listenbrainz_reads_on(&self.switch) {
            return Some(payload);
        }
        let Some(link) = self.links.listenbrainz_link(user_id).await else {
            return Some(payload);
        };
        let key = CacheKey::new(
            Source::ListenBrainz,
            &[
                "wrapped_user",
                &link.username.to_ascii_lowercase(),
                &year.to_string(),
            ],
        )
        .personal();
        let client = self.client.clone();
        let username = link.username;
        let uid = user_id.to_owned();
        let cached = self
            .cache
            .get_or_fetch(&key, WRAPPED_POLICY, PRIORITY, move |priority| async move {
                let (artists, recordings, groups, genres, loved) = tokio::join!(
                    client.user_top_artists(&username, RANGE, TOP_N, 0, priority),
                    client.user_top_recordings(&username, RANGE, TOP_N, 0, priority),
                    client.user_top_release_groups(&username, RANGE, TOP_N, 0, priority),
                    client.user_genre_activity(&username, priority),
                    client.user_loved_sample(&username, LOVED_SAMPLE, priority),
                );
                let any_failed = artists.is_err()
                    || recordings.is_err()
                    || groups.is_err()
                    || genres.is_err()
                    || loved.is_err();
                let top_artists: Vec<WrappedArtist> = leg(artists, "top_artists", &uid)
                    .into_iter()
                    .map(|row| WrappedArtist {
                        name: row.artist_name,
                        listen_count: row.listen_count,
                        artist_mbid: row.artist_mbids.into_iter().next(),
                    })
                    .collect();
                let top_tracks: Vec<WrappedTrack> = leg(recordings, "top_recordings", &uid)
                    .into_iter()
                    .map(|row| WrappedTrack {
                        name: row.track_name,
                        artist_name: row.artist_name,
                        listen_count: row.listen_count,
                    })
                    .collect();
                let top_albums: Vec<WrappedAlbum> = leg(groups, "top_release_groups", &uid)
                    .into_iter()
                    .map(|row| WrappedAlbum {
                        name: row.release_group_name,
                        artist_name: row.artist_name,
                        listen_count: row.listen_count,
                        mbid: row.release_group_mbid,
                    })
                    .collect();
                let top_genres: Vec<WrappedGenre> = leg(genres, "genre_activity", &uid)
                    .into_iter()
                    .take(TOP_N as usize)
                    .map(|row| WrappedGenre {
                        genre: row.genre,
                        listen_count: row.listen_count,
                    })
                    .collect();
                let loved_tracks_count = match loved {
                    Ok(count) => i64::try_from(count).unwrap_or(i64::MAX),
                    Err(error) => {
                        tracing::warn!(user_id = %uid, %error, "wrapped loved_recordings fetch failed");
                        0
                    }
                };
                let has_data =
                    !(top_artists.is_empty() && top_tracks.is_empty() && top_albums.is_empty());
                let total_listens_estimated =
                    top_artists.iter().map(|a| a.listen_count).sum();
                let stats = CachedUserWrapped {
                    has_data,
                    top_artists,
                    top_tracks,
                    top_albums,
                    top_genres,
                    loved_tracks_count,
                    total_listens_estimated,
                };
                Ok::<_, ProviderError>(if any_failed {
                    Fetched::Partial(stats)
                } else {
                    Fetched::Found(stats)
                })
            })
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        payload.top_artists = cached.top_artists;
        payload.top_tracks = cached.top_tracks;
        payload.top_albums = cached.top_albums;
        payload.top_genres = cached.top_genres;
        payload.loved_tracks_count = cached.loved_tracks_count;
        payload.has_data = cached.has_data;
        payload.total_listens_estimated = cached.total_listens_estimated;
        Some(payload)
    }
}

/// One leg's rows; a failed leg is logged and reads as empty.
fn leg<T>(result: Result<Vec<T>, ProviderError>, label: &str, user_id: &str) -> Vec<T> {
    result.unwrap_or_else(|error| {
        tracing::warn!(user_id, %error, "wrapped {label} fetch failed");
        Vec::new()
    })
}

impl WrappedData for ListenBrainzWrapped {
    fn current_year(&self) -> i32 {
        time::OffsetDateTime::from_unix_timestamp(self.clock.now_unix())
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
            .year()
    }

    fn list_users(&self) -> BoxFuture<'_, Vec<WrappedUserSummary>> {
        Box::pin(async move {
            let mut summaries = Vec::new();
            for user in self.all_users().await {
                let linked = self.links.listenbrainz_link(&user.id).await.is_some();
                summaries.push(WrappedUserSummary {
                    id: user.id,
                    display_name: user.display_name,
                    has_listenbrainz: linked,
                    email: user.email,
                });
            }
            summaries
        })
    }

    fn user_wrapped(&self, user_id: &str) -> BoxFuture<'_, Option<UserWrappedResponse>> {
        let user_id = user_id.to_owned();
        Box::pin(async move { self.build_user(&user_id).await })
    }

    fn server_wrapped(&self) -> BoxFuture<'_, ServerWrappedResponse> {
        Box::pin(async move {
            if !listenbrainz_reads_on(&self.switch) {
                return ServerWrappedResponse {
                    year: self.current_year(),
                    total_users_tracked: 0,
                    total_listens_estimated: 0,
                    leaderboard: Vec::new(),
                    top_artist_sitewide: None,
                    top_album_sitewide: None,
                };
            }
            let mut with_data = Vec::new();
            for user in self.list_users().await {
                if !user.has_listenbrainz {
                    continue;
                }
                if let Some(payload) = self.build_user(&user.id).await
                    && payload.has_data
                {
                    with_data.push(payload);
                }
            }
            let mut leaderboard: Vec<WrappedLeaderboardEntry> = with_data
                .iter()
                .map(|payload| WrappedLeaderboardEntry {
                    display_name: payload.display_name.clone(),
                    listen_count: payload.total_listens_estimated,
                })
                .collect();
            leaderboard.sort_by_key(|a| std::cmp::Reverse(a.listen_count));
            let year = self.current_year();
            let sitewide_key = CacheKey::new(
                Source::ListenBrainz,
                &["wrapped_sitewide", &year.to_string()],
            );
            let client = self.client.clone();
            let sitewide = self
                .cache
                .get_or_fetch(
                    &sitewide_key,
                    WRAPPED_POLICY,
                    PRIORITY,
                    move |priority| async move {
                        // Two rows, so a first row without an MBID still leaves a top artist.
                        let (artists, groups) = tokio::join!(
                            client.sitewide_top_artists(RANGE, 2, 0, priority),
                            client.sitewide_top_release_groups(RANGE, 2, 0, priority),
                        );
                        let any_failed = artists.is_err() || groups.is_err();
                        let top_artist = leg(artists, "sitewide top artist", "server")
                            .into_iter()
                            .find_map(|row| {
                                let mbid = row.artist_mbids.into_iter().next()?;
                                Some(WrappedArtist {
                                    name: row.artist_name,
                                    listen_count: row.listen_count,
                                    artist_mbid: Some(mbid),
                                })
                            });
                        let top_album = leg(groups, "sitewide top album", "server")
                            .into_iter()
                            .next()
                            .map(|row| WrappedAlbum {
                                name: row.release_group_name,
                                artist_name: row.artist_name,
                                listen_count: row.listen_count,
                                mbid: row.release_group_mbid,
                            });
                        let stats = CachedSitewide {
                            top_artist,
                            top_album,
                        };
                        Ok::<_, ProviderError>(if any_failed {
                            Fetched::Partial(stats)
                        } else {
                            Fetched::Found(stats)
                        })
                    },
                )
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            ServerWrappedResponse {
                year,
                total_users_tracked: i32::try_from(with_data.len()).unwrap_or(i32::MAX),
                total_listens_estimated: with_data
                    .iter()
                    .map(|payload| payload.total_listens_estimated)
                    .sum(),
                leaderboard,
                top_artist_sitewide: sitewide.top_artist,
                top_album_sitewide: sitewide.top_album,
            }
        })
    }
}
