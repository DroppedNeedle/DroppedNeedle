//! Played and unplayed marks over `library_user_play_marks`.
//!
//! One row per (user, track). A Jellyfin app sets them; marking an album or
//! an artist marks its tracks. A mark never touches the play history: the
//! player reads weigh the marks against the plays.

use rusqlite::params;

use super::super::db::{CollectionsDb, StoreError};

/// What a mark covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkScope<'a> {
    /// One track row.
    Track(&'a str),
    /// Every track row of an album, apart from the missing rows that
    /// continued into another track: those answer for the track they
    /// continued into.
    Album(&'a str),
    /// The streamable tracks on the albums an artist leads, the set a
    /// Jellyfin app's `AlbumArtistIds` filter lists.
    AlbumArtist(&'a str),
}

/// The marks store.
#[derive(Clone, Debug)]
pub struct PlayMarkStore {
    db: CollectionsDb,
}

impl PlayMarkStore {
    /// Store over one database.
    pub fn new(db: CollectionsDb) -> Self {
        Self { db }
    }

    /// Mark the tracks `scope` covers played or unplayed for one user, all
    /// in one transaction, at the server's time. Played keeps an earlier
    /// unplayed mark; unplayed clears the played one.
    pub async fn mark(
        &self,
        user_id: &str,
        scope: MarkScope<'_>,
        played: bool,
    ) -> Result<(), StoreError> {
        let user_id = user_id.to_owned();
        let (tracks, id) = match scope {
            MarkScope::Track(id) => ("SELECT id FROM local_tracks WHERE id = ?3", id),
            MarkScope::Album(id) => (
                "SELECT id FROM local_tracks WHERE local_album_id = ?3 \
                 AND id NOT IN (SELECT alias FROM local_track_aliases)",
                id,
            ),
            MarkScope::AlbumArtist(id) => (
                "SELECT t.id FROM local_tracks t JOIN local_albums a ON a.id = t.local_album_id \
                 WHERE t.availability = 'indexed' AND a.album_artist_id = ?3",
                id,
            ),
        };
        let (values, update) = if played {
            ("?2, NULL", "marked_played_at = excluded.marked_played_at")
        } else {
            (
                "NULL, ?2",
                "marked_unplayed_at = excluded.marked_unplayed_at, marked_played_at = NULL",
            )
        };
        let sql = format!(
            "INSERT INTO library_user_play_marks \
             (user_id, local_track_id, marked_played_at, marked_unplayed_at) \
             SELECT ?1, id, {values} FROM ({tracks}) WHERE true \
             ON CONFLICT (user_id, local_track_id) DO UPDATE SET {update}"
        );
        let id = id.to_owned();
        let now_ms = self.db.now_ms();
        self.db
            .write("play_marks.mark", move |tx| {
                tx.execute(&sql, params![user_id, now_ms, id])?;
                Ok(())
            })
            .await
    }
}
