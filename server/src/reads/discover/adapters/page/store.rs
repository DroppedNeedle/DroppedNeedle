//! The discover page's durable state, in tables the baseline schema
//! already has (the same ones v2 wrote, so migrated rows keep working):
//!
//! - `discovery_snapshots`: the last good page per user and service mix
//!   (key `discover_response:<user>:<lb>:<lfm>`), tied to the library
//!   catalog revision it was built against, so a library change makes it
//!   read as missing;
//! - `discovery_activity`: which discover features a user used in the
//!   last day, so the warm cycle rebuilds only what people look at.
//!
//! Reads use the reader pool; writes go through the writer lane. A write
//! for a user who no longer exists does nothing, so a build that finishes
//! after the account was deleted leaves no rows behind.

use sqlx::SqlitePool;

use crate::db::{WriteLane, writer::Lane};

/// Activity older than this is forgotten.
pub const ACTIVITY_WINDOW_SECS: f64 = 86_400.0;
/// Repeated activity within this many seconds is not rewritten.
const ACTIVITY_DEBOUNCE_SECS: f64 = 300.0;
/// Most activity rows kept per user.
const ACTIVITY_PER_USER: i64 = 100;
/// Optional-progress rows older than a week are dropped.
const PROGRESS_WINDOW_SECS: f64 = 604_800.0;

/// The snapshot key v2 used: the service flags are part of it, so linking
/// or unlinking a service starts from a fresh page.
pub fn snapshot_key(user_id: &str, listenbrainz: bool, lastfm: bool) -> String {
    let flag = |on: bool| if on { "True" } else { "False" };
    format!(
        "discover_response:{user_id}:{}:{}",
        flag(listenbrainz),
        flag(lastfm)
    )
}

/// One saved page as stored.
#[derive(Debug, Clone)]
pub struct SavedPage {
    /// The stored JSON.
    pub payload: Vec<u8>,
    /// When it was saved (unix seconds).
    pub saved_at: f64,
    /// True when an invalidation marked it stale.
    pub stale: bool,
}

/// One feature a user used recently.
#[derive(Debug, Clone, PartialEq)]
pub struct ActivityRow {
    /// User id.
    pub user_id: String,
    /// `home`, `discover`, `queue` or `artist`.
    pub feature: String,
    /// Artist MBID for artist activity, else empty.
    pub artist_mbid: String,
    /// Artist-page section for artist activity, else empty.
    pub section: String,
    /// Provider for artist activity, else empty.
    pub provider: String,
    /// The MusicBrainz source key the activity was recorded under.
    pub source: String,
}

/// The page tables.
#[derive(Clone, Debug)]
pub struct PageDb {
    pool: SqlitePool,
    lane: WriteLane,
}

impl PageDb {
    /// Bind the tables over the reader pool and the writer lane.
    pub fn new(pool: SqlitePool, lane: WriteLane) -> Self {
        Self { pool, lane }
    }

    /// The reader pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// The saved page under `key`, when it was built against the current
    /// library catalog revision.
    pub async fn load(&self, key: &str) -> Result<Option<SavedPage>, String> {
        let row: Option<(Vec<u8>, f64, i64)> = sqlx::query_as(
            "SELECT snapshot.payload, snapshot.saved_at, snapshot.stale \
             FROM discovery_snapshots snapshot \
             JOIN library_catalog_revision revision ON revision.singleton = 1 \
             WHERE snapshot.snapshot_key = ?1 AND snapshot.catalog_revision = revision.value",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| format!("discover snapshot read: {error}"))?;
        Ok(row.map(|(payload, saved_at, stale)| SavedPage {
            payload,
            saved_at,
            stale: stale != 0,
        }))
    }

    /// Save a page under `key` against the current catalog revision.
    pub async fn save(
        &self,
        key: &str,
        user_id: &str,
        payload: Vec<u8>,
        saved_at: f64,
    ) -> Result<(), String> {
        let key = key.to_owned();
        let user_id = user_id.to_owned();
        self.lane
            .write(Lane::Background, "discover.page.save", move |tx| {
                tx.execute(
                    "INSERT INTO discovery_snapshots \
                         (snapshot_key, user_id, payload, saved_at, stale, catalog_revision) \
                     SELECT ?1, ?2, ?3, ?4, 0, \
                         COALESCE((SELECT value FROM library_catalog_revision \
                                   WHERE singleton = 1), 0) \
                     WHERE EXISTS (SELECT 1 FROM auth_users WHERE id = ?2) \
                     ON CONFLICT(snapshot_key) DO UPDATE SET \
                         user_id = excluded.user_id, payload = excluded.payload, \
                         saved_at = excluded.saved_at, stale = 0, \
                         catalog_revision = excluded.catalog_revision",
                    rusqlite::params![key, user_id, payload, saved_at],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| format!("discover snapshot write: {error}"))
    }

    /// Record that the user used a feature (v2 `record_activity`): prune
    /// what fell out of the window, upsert the row unless it was touched
    /// in the last five minutes under the same source, and keep the
    /// newest hundred rows per user. A source switch resets the row's
    /// schedule so the new source is warmed right away.
    pub async fn record_activity(&self, row: ActivityRow, now: f64) -> Result<(), String> {
        self.lane
            .write(Lane::Background, "discover.activity.record", move |tx| {
                tx.execute(
                    "DELETE FROM discovery_activity WHERE last_used < ?1",
                    rusqlite::params![now - ACTIVITY_WINDOW_SECS],
                )?;
                tx.execute(
                    "DELETE FROM discovery_optional_progress WHERE updated_at < ?1",
                    rusqlite::params![now - PROGRESS_WINDOW_SECS],
                )?;
                tx.execute(
                    "INSERT INTO discovery_activity \
                         (user_id, feature, artist_mbid, section, provider, source, last_used) \
                     SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7 \
                     WHERE EXISTS (SELECT 1 FROM auth_users WHERE id = ?1) \
                     ON CONFLICT(user_id, feature, artist_mbid, section, provider) DO UPDATE SET \
                         last_used = excluded.last_used, source = excluded.source, \
                         retry_at = CASE WHEN discovery_activity.source != excluded.source \
                             THEN 0 ELSE discovery_activity.retry_at END, \
                         last_success = CASE WHEN discovery_activity.source != excluded.source \
                             THEN 0 ELSE discovery_activity.last_success END \
                     WHERE discovery_activity.last_used <= excluded.last_used - ?8 \
                        OR discovery_activity.source != excluded.source",
                    rusqlite::params![
                        row.user_id,
                        row.feature,
                        row.artist_mbid,
                        row.section,
                        row.provider,
                        row.source,
                        now,
                        ACTIVITY_DEBOUNCE_SECS
                    ],
                )?;
                tx.execute(
                    "DELETE FROM discovery_activity WHERE rowid IN (\
                         SELECT rowid FROM discovery_activity WHERE user_id = ?1 \
                         ORDER BY last_used DESC, rowid DESC LIMIT -1 OFFSET ?2)",
                    rusqlite::params![row.user_id, ACTIVITY_PER_USER],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| format!("discover activity write: {error}"))
    }

    /// Activity due for warming under `source`, least recently serviced
    /// first (v2 `get_due_activity`), optionally for one user. Stale rows
    /// are pruned first.
    pub async fn due_activity(
        &self,
        source: &str,
        now: f64,
        user_id: Option<&str>,
    ) -> Result<Vec<ActivityRow>, String> {
        let prune = self
            .lane
            .write(Lane::Background, "discover.activity.prune", move |tx| {
                tx.execute(
                    "DELETE FROM discovery_activity WHERE last_used <= ?1",
                    rusqlite::params![now - ACTIVITY_WINDOW_SECS],
                )?;
                tx.execute(
                    "DELETE FROM discovery_optional_progress WHERE updated_at <= ?1",
                    rusqlite::params![now - PROGRESS_WINDOW_SECS],
                )?;
                Ok(())
            })
            .await;
        if let Err(error) = prune {
            tracing::warn!(%error, "discover activity prune failed; reading anyway");
        }
        let rows: Vec<(String, String, String, String, String, String)> = sqlx::query_as(
            "SELECT user_id, feature, artist_mbid, section, provider, source \
             FROM discovery_activity \
             WHERE source = ?1 AND last_used > ?2 AND retry_at <= ?3 \
               AND (?4 IS NULL OR user_id = ?4) \
             ORDER BY serviced_at, user_id, feature, artist_mbid LIMIT 100",
        )
        .bind(source)
        .bind(now - ACTIVITY_WINDOW_SECS)
        .bind(now)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| format!("discover activity read: {error}"))?;
        Ok(rows
            .into_iter()
            .map(
                |(user_id, feature, artist_mbid, section, provider, source)| ActivityRow {
                    user_id,
                    feature,
                    artist_mbid,
                    section,
                    provider,
                    source,
                },
            )
            .collect())
    }

    /// Mark one activity row serviced and schedule its next warm.
    pub async fn finish_activity(
        &self,
        row: &ActivityRow,
        now: f64,
        success: bool,
        retry_secs: f64,
    ) -> Result<(), String> {
        let row = row.clone();
        self.lane
            .write(Lane::Background, "discover.activity.finish", move |tx| {
                tx.execute(
                    "UPDATE discovery_activity SET serviced_at = ?1, retry_at = ?2, \
                         last_success = CASE WHEN ?3 THEN ?1 ELSE last_success END \
                     WHERE user_id = ?4 AND feature = ?5 AND artist_mbid = ?6 \
                       AND section = ?7 AND provider = ?8 AND source = ?9",
                    rusqlite::params![
                        now,
                        now + retry_secs,
                        success,
                        row.user_id,
                        row.feature,
                        row.artist_mbid,
                        row.section,
                        row.provider,
                        row.source
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| format!("discover activity finish: {error}"))
    }

    /// Whether the user still exists.
    pub async fn user_exists(&self, user_id: &str) -> Result<bool, String> {
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM auth_users WHERE id = ?1)")
            .bind(user_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| format!("user check: {error}"))
    }

    /// Forget a deleted user's activity.
    pub async fn forget_user(&self, user_id: &str) -> Result<(), String> {
        let user_id = user_id.to_owned();
        self.lane
            .write(Lane::Background, "discover.activity.forget", move |tx| {
                for table in [
                    "discovery_activity",
                    "discovery_optional_progress",
                    "discovery_snapshots",
                ] {
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE user_id = ?1"),
                        rusqlite::params![user_id],
                    )?;
                }
                Ok(())
            })
            .await
            .map_err(|error| format!("discover user cleanup: {error}"))
    }
}
