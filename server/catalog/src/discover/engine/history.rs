//! A person's history as Discover reads it: their plays in the app and the
//! listens imported from their ListenBrainz history, per artist, and the
//! release groups they played here. Imported listens name no release
//! group, so they are known at artist level only. An imported listen is
//! history for taste and never a play: nothing here counts it as one.

use std::collections::HashSet;

use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_media::history::HistoryImportState;
use rusqlite::params;

use super::sql;
use super::taste::RECENT_DAYS;
use crate::discover::listening::{ListenedAlbum, StatsRange};
use crate::discover::queue::seeds::seed_mbid;

/// A day, ms.
const DAY_MS: i64 = 86_400_000;

/// One artist the person played or listened to.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Played {
    /// Its MusicBrainz id, lowercased.
    pub mbid: String,
    /// Its name.
    pub name: String,
    /// Plays in the app.
    pub plays: i64,
    /// Imported listens.
    pub imported_listens: i64,
    /// Calendar months that hold either.
    pub months: i64,
    /// The first of either, unix ms.
    pub first_at: i64,
    /// The latest of either, unix ms.
    pub last_at: i64,
    /// How many of either fell in the last [`RECENT_DAYS`].
    pub recent: i64,
}

/// One release group the person played in the app.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlayedGroup {
    /// Its MusicBrainz id, lowercased.
    pub mbid: String,
    /// How many plays.
    pub plays: i64,
    /// The latest, unix ms.
    pub last_at: i64,
}

/// What a person's history holds, read in one snapshot.
#[derive(Debug, Clone, Default)]
pub(crate) struct History {
    /// Every artist with a usable MusicBrainz id they played or listened
    /// to.
    pub artists: Vec<Played>,
    /// The release groups they played in the app.
    pub groups: Vec<PlayedGroup>,
    /// Whether their imported ListenBrainz history is current.
    pub import_current: bool,
}

impl History {
    /// The plays and imported listens counted, and the span they cover.
    pub(crate) fn span(&self) -> (i64, i64, Option<i64>, Option<i64>) {
        let plays = self.artists.iter().map(|artist| artist.plays).sum();
        let listens = self
            .artists
            .iter()
            .map(|artist| artist.imported_listens)
            .sum();
        let from = self.artists.iter().map(|artist| artist.first_at).min();
        let to = self.artists.iter().map(|artist| artist.last_at).max();
        (plays, listens, from, to)
    }

    /// `user_id`'s history at `now_ms`. A row with no usable artist id, a
    /// placeholder or one of MusicBrainz's special-purpose ids is left out.
    ///
    /// # Errors
    ///
    /// The read's failure.
    pub(crate) async fn read(db: &Db, user_id: &str, now_ms: i64) -> Result<Self, DbError> {
        let user = user_id.to_owned();
        let recent_since = now_ms - RECENT_DAYS * DAY_MS;
        db.read("discover.taste.history", move |conn| {
            // A background read whose statements must agree: the plays per
            // artist, the albums played and whether the import is current
            // describe one moment of the person's history.
            let artists = conn
                .prepare_cached(sql::HISTORY_ARTISTS.text())?
                .query_map(params![user, recent_since], sql::HistoryArtist::read)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let groups = conn
                .prepare_cached(sql::HISTORY_GROUPS.text())?
                .query_map([&user], sql::HistoryGroup::read)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let import_current: bool = conn.query_row(
                sql::IMPORT_IN_STATE.text(),
                params![user, HistoryImportState::Current],
                |row| row.get(0),
            )?;
            Ok(Self {
                artists: artists
                    .into_iter()
                    .filter_map(|row| {
                        Some(Played {
                            mbid: seed_mbid(Some(&row.mbid))?,
                            name: row.name,
                            plays: row.plays,
                            imported_listens: row.imported_listens,
                            months: row.months,
                            first_at: row.first_played_at.0,
                            last_at: row.last_played_at.0,
                            recent: row.recent,
                        })
                    })
                    .collect(),
                groups: groups
                    .into_iter()
                    .filter_map(|row| {
                        Some(PlayedGroup {
                            mbid: seed_mbid(Some(&row.mbid))?,
                            plays: row.plays,
                            last_at: row.last_played_at.0,
                        })
                    })
                    .collect(),
                import_current,
            })
        })
        .await
    }
}

/// Whether `user_id`'s imported ListenBrainz history is current: their
/// listening statistics are then read from it, not from ListenBrainz.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn import_current(db: &Db, user_id: &str) -> Result<bool, DbError> {
    let user = user_id.to_owned();
    db.read("discover.taste.import_current", move |conn| {
        conn.query_row(
            sql::IMPORT_IN_STATE.text(),
            params![user, HistoryImportState::Current],
            |row| row.get(0),
        )
        .map_err(DbError::from)
    })
    .await
}

/// The start of `range` at `now_ms`, unix ms: the last 7, 30 or 365 days,
/// or all time.
pub(crate) fn range_start(range: StatsRange, now_ms: i64) -> i64 {
    let days = match range {
        StatsRange::ThisWeek => 7,
        StatsRange::ThisMonth => 30,
        StatsRange::ThisYear => 365,
        StatsRange::AllTime => return 0,
    };
    now_ms - days * DAY_MS
}

/// `user_id`'s most played release groups in the app since `since_ms`,
/// `count` from `offset`, with how many they played in that time.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn top_albums(
    db: &Db,
    user_id: &str,
    since_ms: i64,
    count: u32,
    offset: u32,
) -> Result<(Vec<ListenedAlbum>, i64), DbError> {
    let user = user_id.to_owned();
    db.read("discover.taste.top_albums", move |conn| {
        // The page and the count it is judged by agree.
        let rows = conn
            .prepare_cached(sql::TOP_PLAYED_GROUPS.text())?
            .query_map(
                params![user, since_ms, i64::from(count), i64::from(offset)],
                sql::TopPlayedGroup::read,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let albums: i64 = conn.query_row(
            sql::PLAYED_GROUP_COUNT.text(),
            params![user, since_ms],
            |row| row.get(0),
        )?;
        let rows = rows
            .into_iter()
            .map(|row| ListenedAlbum {
                release_group_mbid: Some(row.release_group_mbid),
                title: row.title,
                artist_name: row.artist_name,
                artist_mbid: row.artist_mbid,
                listen_count: row.plays,
            })
            .collect();
        Ok((rows, albums))
    })
    .await
}

/// The release groups `user_id` is known to have played, from their last
/// taste build.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn known_groups(db: &Db, user_id: &str) -> Result<HashSet<String>, DbError> {
    let user = user_id.to_owned();
    db.read("discover.taste.known_groups", move |conn| {
        conn.prepare_cached(sql::KNOWN_GROUPS.text())?
            .query_map([&user], |row| row.get(0))?
            .collect::<rusqlite::Result<HashSet<String>>>()
            .map_err(DbError::from)
    })
    .await
}
