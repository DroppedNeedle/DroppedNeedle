//! Follow reads over `user_followed_artists`, plus the new-release feed.
//!
//! The follows module in acquisition alone writes the follow rows and
//! their approval verdicts in `auto_download_approvals`; this store only
//! reads them to show a follow's state. Rows key on the lowercased MBID.
//!
//! New releases come from `new_release_feed` joined to the user's follows;
//! albums already in the library drop out of the to-do list (v2 rule).

use rusqlite::params;
use sqlx::Row as _;

use super::super::db::{CollectionsDb, StoreError, epoch_from_real};
use crate::reads::library::definitions::owned_release_groups;

/// Release groups the library owns, lowercased.
const OWNED_RELEASE_GROUPS: &str = owned_release_groups!();

/// One follow row with its approval verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct FollowRow {
    /// Follower user id.
    pub user_id: String,
    /// Follower display name.
    pub user_name: String,
    /// Follower role (`user`, `trusted`, `admin`).
    pub user_role: String,
    /// Artist MBID as first followed.
    pub artist_mbid: String,
    /// Artist name snapshot.
    pub artist_name: String,
    /// Auto-download intent.
    pub auto_download: bool,
    /// Approval verdict, when an approval row exists.
    pub approval_state: Option<String>,
    /// When the ask was filed, epoch seconds, when an approval row exists.
    pub requested_at: Option<u64>,
    /// When the follow started, epoch seconds.
    pub followed_at: u64,
    /// Last change, epoch seconds.
    pub updated_at: u64,
}

/// One new-release sighting.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseRow {
    /// Release-group MBID.
    pub release_group_mbid: String,
    /// Release title.
    pub title: String,
    /// Artist name.
    pub artist_name: String,
    /// Artist MBID as the user followed it.
    pub artist_mbid: String,
    /// Primary type, when known.
    pub primary_type: Option<String>,
    /// First release date, when known.
    pub first_release_date: Option<String>,
    /// True when the library already holds the album.
    pub in_library: bool,
}

/// Which release sightings to read.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseFilter {
    /// Only releases dated on or after this `YYYY-MM-DD`, or, undated,
    /// discovered at or after this epoch time.
    pub since: Option<(String, f64)>,
    /// Keep albums the library already holds (the log view).
    pub owned_too: bool,
    /// Page size.
    pub limit: usize,
    /// Rows skipped first.
    pub offset: usize,
}

/// The follow store.
#[derive(Clone, Debug)]
pub struct FollowStore {
    db: CollectionsDb,
}

impl FollowStore {
    /// Now, as fractional epoch seconds, by the store's clock.
    pub fn now_real(&self) -> f64 {
        self.db.now_real()
    }
}

const FOLLOW_SELECT: &str = "SELECT f.user_id AS user_id, \
    COALESCE(u.username_display, u.username, u.display_name, f.user_id) AS user_name, \
    COALESCE(u.role, 'user') AS user_role, f.artist_mbid AS artist_mbid, \
    f.artist_name AS artist_name, f.auto_download AS auto_download, \
    a.state AS approval_state, a.requested_at / 1000.0 AS requested_at, \
    f.followed_at AS followed_at, f.updated_at AS updated_at \
    FROM user_followed_artists f \
    LEFT JOIN auth_users u ON u.id = f.user_id \
    LEFT JOIN auto_download_approvals a \
      ON a.user_id = f.user_id AND a.artist_mbid_lower = f.artist_mbid_lower";

fn follow_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<FollowRow, sqlx::Error> {
    Ok(FollowRow {
        user_id: row.try_get("user_id")?,
        user_name: row.try_get("user_name")?,
        user_role: row.try_get("user_role")?,
        artist_mbid: row.try_get("artist_mbid")?,
        artist_name: row.try_get("artist_name")?,
        auto_download: row.try_get::<i64, _>("auto_download")? != 0,
        approval_state: row.try_get("approval_state")?,
        requested_at: row
            .try_get::<Option<f64>, _>("requested_at")?
            .map(epoch_from_real),
        followed_at: epoch_from_real(row.try_get("followed_at")?),
        updated_at: epoch_from_real(row.try_get("updated_at")?),
    })
}

fn release_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<ReleaseRow, sqlx::Error> {
    Ok(ReleaseRow {
        release_group_mbid: row.try_get("release_group_mbid")?,
        title: row.try_get("title")?,
        artist_name: row.try_get("artist_name")?,
        artist_mbid: row.try_get("artist_mbid")?,
        primary_type: row.try_get("primary_type")?,
        first_release_date: row.try_get("first_release_date")?,
        in_library: row.try_get::<i64, _>("in_library")? != 0,
    })
}

impl FollowStore {
    /// Store over one database.
    pub fn new(db: CollectionsDb) -> Self {
        Self { db }
    }

    /// One follow, or None when the user does not follow the artist.
    pub async fn get(
        &self,
        user_id: &str,
        artist_mbid: &str,
    ) -> Result<Option<FollowRow>, StoreError> {
        let pool = self.db.pool()?;
        let row = sqlx::query(&format!(
            "{FOLLOW_SELECT} WHERE f.user_id = ? AND f.artist_mbid_lower = ?"
        ))
        .bind(user_id)
        .bind(artist_mbid.to_lowercase())
        .fetch_optional(pool)
        .await
        .map_err(|error| StoreError::read("follows.get", error))?;
        row.as_ref()
            .map(follow_from_row)
            .transpose()
            .map_err(|error| StoreError::read("follows.get", error))
    }

    /// A user's follows, newest first.
    pub async fn list(&self, user_id: &str) -> Result<Vec<FollowRow>, StoreError> {
        let pool = self.db.pool()?;
        let rows = sqlx::query(&format!(
            "{FOLLOW_SELECT} WHERE f.user_id = ? ORDER BY f.followed_at DESC, f.artist_mbid_lower"
        ))
        .bind(user_id)
        .fetch_all(pool)
        .await
        .map_err(|error| StoreError::read("follows.list", error))?;
        rows.iter()
            .map(follow_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| StoreError::read("follows.list", error))
    }

    /// One page of release sightings for the user's follows, newest
    /// first, with the total the filter matches.
    pub async fn releases(
        &self,
        user_id: &str,
        filter: &ReleaseFilter,
    ) -> Result<(Vec<ReleaseRow>, usize), StoreError> {
        let pool = self.db.pool()?;
        let mut from = "FROM new_release_feed n JOIN user_followed_artists f \
             ON f.artist_mbid_lower = n.artist_mbid_lower AND f.user_id = ? WHERE 1 = 1"
            .to_owned();
        if !filter.owned_too {
            from.push_str(&format!(
                " AND n.release_group_mbid_lower NOT IN ({OWNED_RELEASE_GROUPS})"
            ));
        }
        if filter.since.is_some() {
            from.push_str(
                " AND (n.first_release_date >= ? \
                 OR (n.first_release_date IS NULL AND n.discovered_at >= ?))",
            );
        }
        let count_sql = format!("SELECT COUNT(*) {from}");
        let mut count = sqlx::query_scalar::<_, i64>(&count_sql).bind(user_id);
        if let Some((date, discovered)) = &filter.since {
            count = count.bind(date).bind(discovered);
        }
        let total = count
            .fetch_one(pool)
            .await
            .map_err(|error| StoreError::read("follows.releases", error))?;
        let page_sql = format!(
            "SELECT n.release_group_mbid AS release_group_mbid, n.title AS title, \
             n.artist_name AS artist_name, f.artist_mbid AS artist_mbid, \
             n.primary_type AS primary_type, n.first_release_date AS first_release_date, \
             n.release_group_mbid_lower IN ({OWNED_RELEASE_GROUPS}) AS in_library \
             {from} ORDER BY n.first_release_date DESC, n.discovered_at DESC LIMIT ? OFFSET ?"
        );
        let mut query = sqlx::query(&page_sql).bind(user_id);
        if let Some((date, discovered)) = &filter.since {
            query = query.bind(date).bind(discovered);
        }
        let rows = query
            .bind(i64::try_from(filter.limit).unwrap_or(i64::MAX))
            .bind(i64::try_from(filter.offset).unwrap_or(i64::MAX))
            .fetch_all(pool)
            .await
            .map_err(|error| StoreError::read("follows.releases", error))?;
        let rows = rows
            .iter()
            .map(release_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| StoreError::read("follows.releases", error))?;
        Ok((rows, total.max(0) as usize))
    }

    /// Releases not yet owned and discovered after the user's seen marker.
    pub async fn unseen_count(&self, user_id: &str) -> Result<usize, StoreError> {
        let pool = self.db.pool()?;
        let count: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM new_release_feed n JOIN user_followed_artists f \
             ON f.artist_mbid_lower = n.artist_mbid_lower AND f.user_id = ? \
             WHERE n.release_group_mbid_lower NOT IN ({OWNED_RELEASE_GROUPS}) \
             AND n.discovered_at > COALESCE( \
               (SELECT seen_at FROM user_new_release_seen WHERE user_id = ?), 0)"
        ))
        .bind(user_id)
        .bind(user_id)
        .fetch_one(pool)
        .await
        .map_err(|error| StoreError::read("follows.unseen_count", error))?;
        Ok(count.max(0) as usize)
    }

    /// Move the user's seen marker to now.
    pub async fn mark_seen(&self, user_id: &str) -> Result<(), StoreError> {
        let user_id = user_id.to_owned();
        let now = self.db.now_real();
        self.db
            .write("follows.mark_seen", move |tx| {
                tx.execute(
                    "INSERT INTO user_new_release_seen (user_id, seen_at) VALUES (?1, ?2) \
                     ON CONFLICT (user_id) DO UPDATE SET seen_at = excluded.seen_at",
                    params![user_id, now],
                )?;
                Ok(())
            })
            .await
    }
}
