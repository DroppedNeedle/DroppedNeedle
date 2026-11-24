//! The override for the compilation rule: a person keeps a song the sync
//! moved to the artist's own album on the album Spotify lists it on.
//!
//! The entry goes back to Spotify's album, the release group that album is
//! by proof (through the cached resolver), or none, and the song's row
//! records that a person kept it there, so the rule never moves it again.
//! The request the sync filed for the song on the moved album is withdrawn,
//! as the owner's own ask, while it still waits (for approval, or in the
//! queue) and nobody else asked for it; one that has started, one someone
//! else waits for, or an album request that also covers another of the
//! playlist's songs is left alone. The song is opened again so the next
//! pass asks for it on Spotify's album.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use droppedneedle_auth::Principal;
use droppedneedle_catalog::collections::models::FiledKind;
use droppedneedle_providers::RequestPriority;

use super::{PlaylistSyncError, SpotifySync};
use crate::imports::spotify_albums::SpotifyAlbum;
use crate::requests::models::RequestKind;
use crate::requests::service::RequestsService;

/// A song put back on the album Spotify lists it on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SpotifyAlbumKept {
    /// The release group Spotify's album is, when something proves one;
    /// none shows "No album found yet" until a look finds it.
    pub album_id: Option<String>,
    /// The sync's request for the moved album was cancelled.
    pub cancelled: bool,
}

impl SpotifySync {
    /// Keep the song of `entry_id` on `playlist_id` on Spotify's album, for
    /// the playlist's owner.
    pub async fn keep_spotify_album(
        &self,
        caller: &Principal,
        playlist_id: &str,
        entry_id: &str,
    ) -> Result<SpotifyAlbumKept, PlaylistSyncError> {
        self.playlist_for(caller, playlist_id, false).await?;
        let moved = self
            .playlists
            .moved_song(playlist_id, entry_id)
            .await?
            .ok_or(PlaylistSyncError::NotMoved)?;
        let mut album = SpotifyAlbum {
            title: moved.album.title.clone(),
            artist: moved.album.artist.clone(),
            kind: moved.album.kind.map(|kind| kind.as_str().to_owned()),
            total_tracks: moved.album.tracks,
            songs: Default::default(),
            isrcs: Default::default(),
        };
        if let Some(source) = &moved.album.source_id {
            for (title, isrc) in self
                .playlists
                .source_album_songs(playlist_id, source)
                .await?
            {
                if !title.trim().is_empty() {
                    album.songs.insert(title);
                }
                if let Some(isrc) = isrc {
                    album.add_isrc(&isrc);
                }
            }
        }
        // MusicBrainz down leaves the album for the walk's next look.
        let group = match self
            .resolver
            .resolve(&album, RequestPriority::UserInitiated)
            .await
        {
            Ok(group) => group,
            Err(error) => {
                tracing::warn!(%error, "spotify album not looked up for a kept song");
                None
            }
        };
        if !self
            .playlists
            .keep_source_album(playlist_id, &moved, entry_id, group.as_deref())
            .await?
        {
            return Err(PlaylistSyncError::EntryChanged);
        }
        let filed = moved.filed.clone().filter(|_| !moved.filed_shared);
        let cancelled = match filed {
            Some((FiledKind::Album, key)) => {
                self.cancel_waiting(caller, RequestKind::Album, &key).await
            }
            Some((FiledKind::Track, key)) => {
                self.cancel_waiting(caller, RequestKind::Track, &key).await
            }
            None => false,
        };
        if let Err(error) = self.store.wake(playlist_id).await {
            tracing::warn!(%error, "spotify sync not woken after a kept album");
        }
        Ok(SpotifyAlbumKept {
            album_id: group,
            cancelled,
        })
    }

    /// Withdraw the owner's ask of `kind` for `key` while the request still
    /// waits for approval or in the queue and nobody else asked for it; one
    /// that has started is left alone. Answers whether the ask was
    /// withdrawn.
    async fn cancel_waiting(&self, owner: &Principal, kind: RequestKind, key: &str) -> bool {
        match RequestsService::new(&self.requests)
            .withdraw_ask(owner, key, kind)
            .await
        {
            Ok(decided) => decided.applied(),
            Err(error) => {
                tracing::warn!(?error, "the moved album's request was not cancelled");
                false
            }
        }
    }
}
