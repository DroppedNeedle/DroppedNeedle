//! The follow poll's side of [`Follows`]: cursors and auto followers.
//!
//! Every followed artist has one cursor in `acquire_follow_cursors`, with
//! its known release groups in `artist_known_releases`; a poll records the
//! cursor and the new releases it saw in one transaction.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{Follows, auto_download_live, sql};
use crate::flows::stores::{StoreFailure, lane_failure, read};
use crate::requests::ledger::ApprovalState;
use droppedneedle_auth::Role;

/// A follower whose auto-download is live, with the role on their account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Follower {
    /// User id.
    pub user_id: String,
    /// Account role, for the quota check.
    pub role: Role,
    /// Display name, shown on the request a new release starts.
    pub name: Option<String>,
}

/// One followed artist's poll cursor. Every artist someone follows has
/// one; an artist polled for the first time starts with no baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowCursor {
    /// Artist MBID as first followed (the store keys on its lowercase).
    pub artist_mbid: String,
    /// Artist name snapshot from the follow row.
    pub artist_name: String,
    /// True once the baseline poll has run.
    pub baselined: bool,
    /// Prior successful UTC cursor date (`YYYY-MM-DD`).
    pub cursor_date: Option<String>,
    /// Release groups already seen for this artist, lowercase.
    pub known: HashSet<String>,
    /// Unix seconds when the next poll is due (stored as milliseconds).
    pub next_poll_at: i64,
    /// Releases held for auto-download until their date (dispatch-pending).
    pub pending: Vec<PendingRelease>,
}

/// One future-dated release held until its date arrives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRelease {
    /// Release-group MBID.
    pub rg_mbid: String,
    /// Release title.
    pub title: String,
    /// First release date (`YYYY-MM-DD`).
    pub date: String,
}

/// One new release for the followers' feed (`new_release_feed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedRelease {
    /// Release-group MBID as MusicBrainz spells it.
    pub rg_mbid: String,
    /// Release title.
    pub title: String,
    /// Primary type, when known.
    pub primary_type: Option<String>,
    /// Secondary types, comma separated, when any.
    pub secondary_types: Option<String>,
    /// First release date, when known.
    pub first_release_date: Option<String>,
}

/// One cursor as `CURSORS_DUE` and `CURSOR` select it: lowercase key, MBID
/// as followed, name, then the cursor columns (all null for a never-polled
/// artist), with its known set read beside it. Pending releases that do
/// not decode fail the read.
fn cursor_from_row(conn: &Connection, row: &rusqlite::Row<'_>) -> rusqlite::Result<FollowCursor> {
    let lower: String = row.get(0)?;
    let baselined: Option<i64> = row.get(3)?;
    let next_poll_at: Option<i64> = row.get(5)?;
    let pending = match row.get::<_, Option<String>>(6)? {
        Some(text) => serde_json::from_str(&text).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        None => Vec::new(),
    };
    let mut known = conn.prepare_cached(sql::KNOWN_RELEASES.text())?;
    let known = known
        .query_map(params![lower], |row| row.get(0))?
        .collect::<rusqlite::Result<HashSet<String>>>()?;
    Ok(FollowCursor {
        artist_mbid: row.get(1)?,
        artist_name: row.get(2)?,
        baselined: baselined.unwrap_or(0) != 0,
        cursor_date: row.get(4)?,
        known,
        next_poll_at: next_poll_at.unwrap_or(0) / 1000,
        pending,
    })
}

impl Follows {
    /// Followed artists whose poll is due at `now`, never-polled first,
    /// then oldest due, capped at `limit`. Artists nobody follows any more
    /// drop out even when a cursor remains.
    pub async fn list_due(
        &self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<FollowCursor>, StoreFailure> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        read(self.db.db(), "follows.list_due", move |conn| {
            let mut statement = conn.prepare_cached(sql::CURSORS_DUE.text())?;
            let rows = statement.query_map(params![now.saturating_mul(1000), limit], |row| {
                cursor_from_row(conn, row)
            })?;
            rows.collect()
        })
        .await
    }

    /// Users whose auto-download is live for this artist, ordered by user
    /// id so the owner pick is stable: intent on, and either a role that
    /// approves itself or an approved grant.
    pub async fn auto_followers(&self, artist_mbid: &str) -> Result<Vec<Follower>, StoreFailure> {
        let artist = artist_mbid.to_lowercase();
        let rows: Vec<(String, String, Option<ApprovalState>, Option<String>)> =
            read(self.db.db(), "follows.auto_followers", move |conn| {
                let mut statement = conn.prepare_cached(sql::AUTO_FOLLOWERS.text())?;
                let rows = statement.query_map(params![artist], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?;
                rows.collect()
            })
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(user_id, role, verdict, name)| {
                let role = Role::from_stored(&role);
                auto_download_live(role, verdict).then_some(Follower {
                    user_id,
                    role,
                    name,
                })
            })
            .collect())
    }

    /// Store one cursor after a poll, with its known set, and add the new
    /// releases to the followers' feed in the same transaction. A release
    /// already in the feed keeps its first discovery time.
    pub async fn record_poll(
        &self,
        cursor: &FollowCursor,
        feed: &[FeedRelease],
    ) -> Result<(), StoreFailure> {
        let pending = serde_json::to_string(&cursor.pending).map_err(|error| StoreFailure {
            operation: "follows.record_poll",
            cause: error.to_string(),
        })?;
        let cursor = cursor.clone();
        let feed = feed.to_vec();
        let clock = self.db.clock().clone();
        self.db
            .write_background("follows.record_poll", move |tx| {
                let artist = cursor.artist_mbid.to_lowercase();
                tx.execute(
                    sql::PUT_CURSOR.text(),
                    params![
                        artist,
                        cursor.baselined,
                        cursor.cursor_date,
                        cursor.next_poll_at.saturating_mul(1000),
                        pending
                    ],
                )?;
                for rg in &cursor.known {
                    tx.execute(
                        sql::ADD_KNOWN_RELEASE.text(),
                        params![artist, rg.to_lowercase()],
                    )?;
                }
                let now = crate::db::unix_secs_f64(clock.as_ref());
                for row in &feed {
                    tx.execute(
                        sql::ADD_FEED_RELEASE.text(),
                        params![
                            row.rg_mbid.to_lowercase(),
                            row.rg_mbid,
                            artist,
                            cursor.artist_name,
                            row.title,
                            row.primary_type,
                            row.secondary_types,
                            row.first_release_date,
                            now
                        ],
                    )?;
                }
                Ok(())
            })
            .await
            .map_err(|error| lane_failure("follows.record_poll", error))
    }

    /// One artist's cursor, if it has been polled.
    pub async fn cursor(&self, artist_mbid: &str) -> Result<Option<FollowCursor>, StoreFailure> {
        let artist = artist_mbid.to_lowercase();
        read(self.db.db(), "follows.get", move |conn| {
            conn.query_row(sql::CURSOR.text(), params![artist], |row| {
                cursor_from_row(conn, row)
            })
            .optional()
        })
        .await
    }
}
