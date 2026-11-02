//! Played and unplayed marks over `library_user_play_marks`.
//!
//! One row per (user, track). A Jellyfin app sets them; marking an album or
//! an artist marks its tracks. A mark never touches the play history: the
//! player reads weigh the marks against the plays.

use rusqlite::params;

use super::super::db::{CollectionsDb, StoreError};
use super::super::sql::{self, MarkTracks};

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
            MarkScope::Track(id) => (MarkTracks::Track, id),
            MarkScope::Album(id) => (MarkTracks::Album, id),
            MarkScope::AlbumArtist(id) => (MarkTracks::AlbumArtist, id),
        };
        let text = sql::mark_play(tracks, played);
        let id = id.to_owned();
        let now_ms = self.db.now_ms();
        self.db
            .write("play_marks.mark", move |tx| {
                tx.execute(&text, params![user_id, now_ms, id])?;
                Ok(())
            })
            .await
    }
}
