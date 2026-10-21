//! The follow poll's side of [`Follows`]: cursors and auto followers.
//!
//! Every followed artist has one cursor in `acquire_follow_cursors`, with
//! its known release groups in `artist_known_releases`; a poll records the
//! cursor and the new releases it saw in one transaction.

use std::collections::HashSet;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::{Follows, auto_download_live};
use crate::flows::stores::{StoreFailure, lane_failure, read_failure};
use crate::requests::ledger::ApprovalState;
use droppedneedle_auth::Role;

/// A follower whose auto-download is live, with the role on their account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Follower {
    /// User id.
    pub user_id: String,
    /// Account role, for the quota check.
    pub role: Role,
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
    /// Unix seconds when the next poll is due.
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
    /// Secondary types, comma separated (v2 shape), when any.
    pub secondary_types: Option<String>,
    /// First release date, when known.
    pub first_release_date: Option<String>,
}

/// One cursor as the pool reads it: lowercase key, MBID as followed,
/// name, then the cursor columns (all null for a never-polled artist).
type CursorRow = (
    String,
    String,
    String,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<String>,
);

impl Follows {
    /// Followed artists whose poll is due at `now`, never-polled first,
    /// then oldest due, capped at `limit`. Artists nobody follows any more
    /// drop out even when a cursor remains.
    pub async fn list_due(
        &self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<FollowCursor>, StoreFailure> {
        let rows: Vec<CursorRow> = sqlx::query_as(
            "SELECT f.artist_mbid_lower, MIN(f.artist_mbid), MIN(f.artist_name), \
             MAX(c.baselined), MAX(c.cursor_date), MAX(c.next_poll_at), MAX(c.pending) \
             FROM user_followed_artists f \
             LEFT JOIN acquire_follow_cursors c ON c.artist_mbid_lower = f.artist_mbid_lower \
             GROUP BY f.artist_mbid_lower \
             HAVING MAX(c.next_poll_at) IS NULL OR MAX(c.next_poll_at) <= ?1 \
             ORDER BY COALESCE(MAX(c.next_poll_at), 0), f.artist_mbid_lower LIMIT ?2",
        )
        .bind(now)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| read_failure("follows.list_due", error))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(self.hydrate(row).await?);
        }
        Ok(out)
    }

    /// Users whose auto-download is live for this artist, ordered by user
    /// id so the owner pick is stable: intent on, and either a role that
    /// approves itself or an approved grant.
    pub async fn auto_followers(&self, artist_mbid: &str) -> Result<Vec<Follower>, StoreFailure> {
        let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT f.user_id, u.role, a.state FROM user_followed_artists f \
             JOIN auth_users u ON u.id = f.user_id \
             LEFT JOIN auto_download_approvals a \
               ON a.user_id = f.user_id AND a.artist_mbid_lower = f.artist_mbid_lower \
             WHERE f.artist_mbid_lower = ?1 AND f.auto_download = 1 \
             ORDER BY f.user_id",
        )
        .bind(artist_mbid.to_lowercase())
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| read_failure("follows.auto_followers", error))?;
        Ok(rows
            .into_iter()
            .filter_map(|(user_id, role, verdict)| {
                let role = Role::from_stored(&role);
                let verdict = verdict.as_deref().and_then(ApprovalState::parse);
                auto_download_live(role, verdict).then_some(Follower { user_id, role })
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
        self.db
            .write_background("follows.record_poll", move |tx| {
                let artist = cursor.artist_mbid.to_lowercase();
                tx.execute(
                    "INSERT OR REPLACE INTO acquire_follow_cursors (artist_mbid_lower, \
                     baselined, cursor_date, next_poll_at, followers, pending) \
                     VALUES (?1, ?2, ?3, ?4, '[]', ?5)",
                    params![
                        artist,
                        cursor.baselined,
                        cursor.cursor_date,
                        cursor.next_poll_at,
                        pending
                    ],
                )?;
                for rg in &cursor.known {
                    tx.execute(
                        "INSERT OR IGNORE INTO artist_known_releases \
                         (artist_mbid_lower, rg_mbid_lower) VALUES (?1, ?2)",
                        params![artist, rg.to_lowercase()],
                    )?;
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|span| span.as_secs_f64())
                    .unwrap_or(0.0);
                for row in &feed {
                    tx.execute(
                        "INSERT OR IGNORE INTO new_release_feed (release_group_mbid_lower, \
                         release_group_mbid, artist_mbid_lower, artist_name, title, \
                         primary_type, secondary_types, first_release_date, discovered_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
        let row: Option<CursorRow> = sqlx::query_as(
            "SELECT c.artist_mbid_lower, c.artist_mbid_lower, '', c.baselined, c.cursor_date, \
             c.next_poll_at, c.pending FROM acquire_follow_cursors c \
             WHERE c.artist_mbid_lower = ?1",
        )
        .bind(artist_mbid.to_lowercase())
        .fetch_optional(self.db.pool())
        .await
        .map_err(|error| read_failure("follows.get", error))?;
        match row {
            Some(row) => Ok(Some(self.hydrate(row).await?)),
            None => Ok(None),
        }
    }

    /// Decode one row and load its known set.
    async fn hydrate(&self, row: CursorRow) -> Result<FollowCursor, StoreFailure> {
        let (lower, mbid, name, baselined, cursor_date, next_poll_at, pending) = row;
        let known: Vec<String> = sqlx::query_scalar(
            "SELECT rg_mbid_lower FROM artist_known_releases WHERE artist_mbid_lower = ?1",
        )
        .bind(&lower)
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| read_failure("follows.known", error))?;
        let pending = match pending {
            Some(text) => serde_json::from_str(&text).map_err(|error| StoreFailure {
                operation: "follows.decode",
                cause: error.to_string(),
            })?,
            None => Vec::new(),
        };
        Ok(FollowCursor {
            artist_mbid: mbid,
            artist_name: name,
            baselined: baselined.unwrap_or(0) != 0,
            cursor_date,
            known: known.into_iter().collect(),
            next_poll_at: next_poll_at.unwrap_or(0),
            pending,
        })
    }
}
