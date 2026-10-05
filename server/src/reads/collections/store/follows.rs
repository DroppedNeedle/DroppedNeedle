//! Follows over `user_followed_artists`, plus the new-release feed.
//!
//! A follow row carries the auto-download intent; the approval verdict
//! lives in `auto_download_approvals`, which the acquisition approval
//! store owns. This store reads the verdict to show a follow's state and
//! writes it only when a verdict arrives for an ask it never filed (see
//! [`FollowStore::arm`]). Rows key on the lowercased MBID, as in v2, so
//! imported follows and follows made here are one row.
//!
//! New releases come from `new_release_feed` joined to the user's follows;
//! albums already in the library drop out of the to-do list (v2 rule).

use std::collections::HashSet;

use rusqlite::params;
use sqlx::Row as _;

use super::super::db::{CollectionsDb, StoreError, epoch_from_real, now_real, placeholders};

/// Name used when a follow arrives without one (v2 placeholder).
pub const UNKNOWN_ARTIST: &str = "Unknown Artist";

/// Release groups the library already holds, lowercased.
const OWNED_RELEASE_GROUPS: &str = "SELECT lower(release_group_mbid) \
    FROM local_album_external_identities";

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
}

/// The follow store.
#[derive(Clone, Debug)]
pub struct FollowStore {
    db: CollectionsDb,
}

const FOLLOW_SELECT: &str = "SELECT f.user_id AS user_id, \
    COALESCE(u.username_display, u.username, u.display_name, f.user_id) AS user_name, \
    COALESCE(u.role, 'user') AS user_role, f.artist_mbid AS artist_mbid, \
    f.artist_name AS artist_name, f.auto_download AS auto_download, \
    a.state AS approval_state, a.requested_at AS requested_at, \
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

    /// Every follow with auto-download on and no approved verdict, oldest
    /// ask first. Callers drop the roles that approve themselves.
    pub async fn awaiting_verdict(&self) -> Result<Vec<FollowRow>, StoreError> {
        let pool = self.db.pool()?;
        let rows = sqlx::query(&format!(
            "{FOLLOW_SELECT} WHERE f.auto_download = 1 \
             AND (a.state IS NULL OR a.state = 'pending') \
             ORDER BY COALESCE(a.requested_at, f.updated_at), f.user_id, f.artist_mbid_lower"
        ))
        .fetch_all(pool)
        .await
        .map_err(|error| StoreError::read("follows.awaiting_verdict", error))?;
        rows.iter()
            .map(follow_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| StoreError::read("follows.awaiting_verdict", error))
    }

    /// Lowercased MBIDs among `candidates` the user already follows.
    pub async fn followed_among(
        &self,
        user_id: &str,
        candidates: &[String],
    ) -> Result<HashSet<String>, StoreError> {
        let mut out = HashSet::new();
        if candidates.is_empty() {
            return Ok(out);
        }
        let pool = self.db.pool()?;
        for chunk in candidates.chunks(500) {
            let sql = format!(
                "SELECT artist_mbid_lower FROM user_followed_artists \
                 WHERE user_id = ? AND artist_mbid_lower IN ({})",
                placeholders(chunk.len())
            );
            let mut query = sqlx::query_scalar::<_, String>(&sql).bind(user_id);
            for mbid in chunk {
                query = query.bind(mbid.to_lowercase());
            }
            out.extend(
                query
                    .fetch_all(pool)
                    .await
                    .map_err(|error| StoreError::read("follows.followed_among", error))?,
            );
        }
        Ok(out)
    }

    /// Follow artists. Re-following keeps the intent and the start date and
    /// refreshes the name snapshot when one is given (v2 upsert).
    pub async fn follow(
        &self,
        user_id: &str,
        artists: &[(String, Option<String>)],
    ) -> Result<(), StoreError> {
        if artists.is_empty() {
            return Ok(());
        }
        let user_id = user_id.to_owned();
        let artists = artists.to_vec();
        self.db
            .write("follows.follow", move |tx| {
                let now = now_real();
                let mut stmt = tx.prepare(
                    "INSERT INTO user_followed_artists (user_id, artist_mbid, \
                     artist_mbid_lower, artist_name, auto_download, followed_at, updated_at) \
                     VALUES (?1, ?2, ?3, COALESCE(?4, ?5), 0, ?6, ?6) \
                     ON CONFLICT (user_id, artist_mbid_lower) DO UPDATE SET \
                     artist_name = COALESCE(?4, artist_name), updated_at = ?6",
                )?;
                for (mbid, name) in &artists {
                    stmt.execute(params![
                        user_id,
                        mbid,
                        mbid.to_lowercase(),
                        name,
                        UNKNOWN_ARTIST,
                        now
                    ])?;
                }
                Ok(())
            })
            .await
    }

    /// Unfollow. Any approval row stays, so re-enabling keeps a grant (v2).
    pub async fn unfollow(&self, user_id: &str, artist_mbid: &str) -> Result<bool, StoreError> {
        let (user_id, lower) = (user_id.to_owned(), artist_mbid.to_lowercase());
        self.db
            .write("follows.unfollow", move |tx| {
                let changed = tx.execute(
                    "DELETE FROM user_followed_artists \
                     WHERE user_id = ?1 AND artist_mbid_lower = ?2",
                    params![user_id, lower],
                )?;
                Ok(changed > 0)
            })
            .await
    }

    /// Set the auto-download intent on followed rows. Unknown rows are
    /// skipped; returns how many rows changed.
    pub async fn set_intent(
        &self,
        user_id: &str,
        artist_mbids: &[String],
        enabled: bool,
    ) -> Result<usize, StoreError> {
        if artist_mbids.is_empty() {
            return Ok(0);
        }
        let user_id = user_id.to_owned();
        let lowers = artist_mbids
            .iter()
            .map(|mbid| mbid.to_lowercase())
            .collect::<Vec<_>>();
        self.db
            .write("follows.set_intent", move |tx| {
                let now = now_real();
                let mut stmt = tx.prepare(
                    "UPDATE user_followed_artists SET auto_download = ?1, updated_at = ?2 \
                     WHERE user_id = ?3 AND artist_mbid_lower = ?4",
                )?;
                let mut changed = 0;
                for lower in &lowers {
                    changed += stmt.execute(params![i64::from(enabled), now, user_id, lower])?;
                }
                Ok(changed)
            })
            .await
    }

    /// An approval verdict arrived: turn auto-download on, following the
    /// artist first when the ask came from a request rather than a follow
    /// toggle, and record the approved verdict. The approval store records
    /// it too when it filed the ask; the upsert keeps both writes one truth.
    pub async fn arm(
        &self,
        user_id: &str,
        artist_mbid: &str,
        artist_name: &str,
    ) -> Result<(), StoreError> {
        let (user_id, mbid, name) = (
            user_id.to_owned(),
            artist_mbid.to_owned(),
            artist_name.to_owned(),
        );
        self.db
            .write("follows.arm", move |tx| {
                let now = now_real();
                let lower = mbid.to_lowercase();
                tx.execute(
                    "INSERT INTO user_followed_artists (user_id, artist_mbid, \
                     artist_mbid_lower, artist_name, auto_download, followed_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5) \
                     ON CONFLICT (user_id, artist_mbid_lower) DO UPDATE SET \
                     auto_download = 1, updated_at = ?5",
                    params![user_id, mbid, lower, name, now],
                )?;
                tx.execute(
                    "INSERT INTO auto_download_approvals (user_id, artist_mbid, \
                     artist_mbid_lower, artist_name, state, requested_at, reviewed_at) \
                     VALUES (?1, ?2, ?3, ?4, 'approved', ?5, ?5) \
                     ON CONFLICT (user_id, artist_mbid_lower) DO UPDATE SET \
                     state = 'approved', reviewed_at = COALESCE(reviewed_at, ?5) \
                     WHERE state != 'approved'",
                    params![user_id, mbid, lower, name, now],
                )?;
                Ok(())
            })
            .await
    }

    /// Release sightings for the user's follows. `owned_too` keeps albums
    /// already in the library (the log view); `since` limits to releases
    /// dated (or, undated, discovered) inside the window.
    pub async fn releases(
        &self,
        user_id: &str,
        since: Option<(String, f64)>,
        owned_too: bool,
    ) -> Result<Vec<ReleaseRow>, StoreError> {
        let pool = self.db.pool()?;
        let mut sql = "SELECT n.release_group_mbid AS release_group_mbid, n.title AS title, \
             n.artist_name AS artist_name, f.artist_mbid AS artist_mbid, \
             n.primary_type AS primary_type, n.first_release_date AS first_release_date \
             FROM new_release_feed n JOIN user_followed_artists f \
             ON f.artist_mbid_lower = n.artist_mbid_lower AND f.user_id = ? WHERE 1 = 1"
            .to_owned();
        if !owned_too {
            sql.push_str(&format!(
                " AND n.release_group_mbid_lower NOT IN ({OWNED_RELEASE_GROUPS})"
            ));
        }
        if since.is_some() {
            sql.push_str(
                " AND (n.first_release_date >= ? \
                 OR (n.first_release_date IS NULL AND n.discovered_at >= ?))",
            );
        }
        sql.push_str(" ORDER BY n.first_release_date DESC, n.discovered_at DESC");
        let mut query = sqlx::query(&sql).bind(user_id);
        if let Some((date, discovered)) = &since {
            query = query.bind(date).bind(discovered);
        }
        let rows = query
            .fetch_all(pool)
            .await
            .map_err(|error| StoreError::read("follows.releases", error))?;
        rows.iter()
            .map(release_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| StoreError::read("follows.releases", error))
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
        self.db
            .write("follows.mark_seen", move |tx| {
                tx.execute(
                    "INSERT INTO user_new_release_seen (user_id, seen_at) VALUES (?1, ?2) \
                     ON CONFLICT (user_id) DO UPDATE SET seen_at = excluded.seen_at",
                    params![user_id, now_real()],
                )?;
                Ok(())
            })
            .await
    }
}
