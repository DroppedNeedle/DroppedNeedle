//! Wrapped from ListenBrainz: each user's year in review, as v2 built it.
//!
//! Ports v2's `WrappedService`. A user's stats come from their linked
//! ListenBrainz account (`this_year` top artists, recordings and release
//! groups, ten of each, genre activity, and a loved-recordings sample);
//! the server summary adds up the users who have data and takes the
//! sitewide top artist and album. A failed leg is logged and left empty
//! rather than failing the whole payload, as in v2. Users without a link
//! get the empty payload with their display name.

use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::wrapped::{
    ServerWrappedResponse, UserWrappedResponse, WrappedAlbum, WrappedArtist, WrappedData,
    WrappedGenre, WrappedLeaderboardEntry, WrappedTrack, WrappedUserSummary,
};
use crate::auth::users::stores::UserStore;
use crate::plugins::scrobble::ListenBrainzLinkStore;
use crate::providers::adapters::{CorePacer, CoreSink};
use crate::providers::listenbrainz::{ListenBrainzClient, Outcome};

/// Rows per top list (v2 `_TOP_N`).
const TOP_N: u32 = 10;
/// Stats range (v2 `_RANGE`).
const RANGE: &str = "this_year";
/// Users read per page (v2 `_LIST_USERS_PAGE_SIZE`).
const USERS_PAGE: u64 = 500;
/// Loved-recordings sample size; ListenBrainz caps the page at 100.
const LOVED_SAMPLE: u32 = 100;

/// Wrapped over the user store, the ListenBrainz links, and one paced
/// client.
pub struct ListenBrainzWrapped {
    users: Arc<dyn UserStore>,
    links: Arc<dyn ListenBrainzLinkStore>,
    client: ListenBrainzClient<CorePacer, CoreSink>,
}

impl ListenBrainzWrapped {
    /// Wire the data source.
    pub fn new(
        users: Arc<dyn UserStore>,
        links: Arc<dyn ListenBrainzLinkStore>,
        client: ListenBrainzClient<CorePacer, CoreSink>,
    ) -> Self {
        Self {
            users,
            links,
            client,
        }
    }

    async fn all_users(&self) -> Vec<crate::auth::users::models::UserRecord> {
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
        let Some(link) = self.links.status(user_id).await else {
            return Some(payload);
        };
        let username = link.username.as_str();
        let (artists, recordings, groups, genres, loved) = tokio::join!(
            self.client.user_top_artists(username, RANGE, TOP_N, 0),
            self.client.user_top_recordings(username, RANGE, TOP_N, 0),
            self.client
                .user_top_release_groups(username, RANGE, TOP_N, 0),
            self.client.user_genre_activity(username),
            self.client.user_loved_sample(username, LOVED_SAMPLE),
        );
        payload.top_artists = leg(artists, "top_artists", user_id)
            .into_iter()
            .map(|row| WrappedArtist {
                name: row.artist_name,
                listen_count: row.listen_count,
                artist_mbid: row.artist_mbids.into_iter().next(),
            })
            .collect();
        payload.top_tracks = leg(recordings, "top_recordings", user_id)
            .into_iter()
            .map(|row| WrappedTrack {
                name: row.track_name,
                artist_name: row.artist_name,
                listen_count: row.listen_count,
            })
            .collect();
        payload.top_albums = leg(groups, "top_release_groups", user_id)
            .into_iter()
            .map(|row| WrappedAlbum {
                name: row.release_group_name,
                artist_name: row.artist_name,
                listen_count: row.listen_count,
                mbid: row.release_group_mbid,
            })
            .collect();
        payload.top_genres = leg(genres, "genre_activity", user_id)
            .into_iter()
            .take(TOP_N as usize)
            .map(|row| WrappedGenre {
                genre: row.genre,
                listen_count: row.listen_count,
            })
            .collect();
        payload.loved_tracks_count = match loved {
            Outcome::Found(count) => i64::try_from(count).unwrap_or(i64::MAX),
            Outcome::Missing => 0,
            Outcome::Unavailable { message, .. } => {
                tracing::warn!(user_id, %message, "wrapped loved_recordings fetch failed");
                0
            }
        };
        payload.has_data = !(payload.top_artists.is_empty()
            && payload.top_tracks.is_empty()
            && payload.top_albums.is_empty());
        payload.total_listens_estimated = payload.top_artists.iter().map(|a| a.listen_count).sum();
        Some(payload)
    }
}

/// One leg's rows; a failed leg is logged and reads as empty (v2).
fn leg<T>(outcome: Outcome<Vec<T>>, label: &str, user_id: &str) -> Vec<T> {
    match outcome {
        Outcome::Found(rows) => rows,
        Outcome::Missing => Vec::new(),
        Outcome::Unavailable { message, .. } => {
            tracing::warn!(user_id, %message, "wrapped {label} fetch failed");
            Vec::new()
        }
    }
}

impl WrappedData for ListenBrainzWrapped {
    fn current_year(&self) -> i32 {
        time::OffsetDateTime::now_utc().year()
    }

    fn list_users(&self) -> BoxFuture<'_, Vec<WrappedUserSummary>> {
        Box::pin(async move {
            let mut summaries = Vec::new();
            for user in self.all_users().await {
                let linked = self.links.status(&user.id).await.is_some();
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
            leaderboard.sort_by(|a, b| b.listen_count.cmp(&a.listen_count));
            // v2 read these through the charts' one-row pages, which skip
            // artists without an MBID.
            let (artists, groups) = tokio::join!(
                self.client.sitewide_top_artists(RANGE, 2, 0),
                self.client.sitewide_top_release_groups(RANGE, 2, 0),
            );
            let top_artist_sitewide = leg(artists, "sitewide top artist", "server")
                .into_iter()
                .find_map(|row| {
                    let mbid = row.artist_mbids.into_iter().next()?;
                    Some(WrappedArtist {
                        name: row.artist_name,
                        listen_count: row.listen_count,
                        artist_mbid: Some(mbid),
                    })
                });
            let top_album_sitewide = leg(groups, "sitewide top album", "server")
                .into_iter()
                .next()
                .map(|row| WrappedAlbum {
                    name: row.release_group_name,
                    artist_name: row.artist_name,
                    listen_count: row.listen_count,
                    mbid: row.release_group_mbid,
                });
            ServerWrappedResponse {
                year: self.current_year(),
                total_users_tracked: i32::try_from(with_data.len()).unwrap_or(i32::MAX),
                total_listens_estimated: with_data
                    .iter()
                    .map(|payload| payload.total_listens_estimated)
                    .sum(),
                leaderboard,
                top_artist_sitewide,
                top_album_sitewide,
            }
        })
    }
}
