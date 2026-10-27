//! The rows of `catalog_discover_activity`: which features each user used
//! in the last day, and when each is due to be warmed again. Reads use
//! the reader pool; writes go through the writer lane. Activity of a user
//! who no longer exists is not recorded, and goes with the account.

use super::Feature;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};

/// Most activity rows kept per user.
const PER_USER: i64 = 100;

/// One feature a user used recently.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Activity {
    /// User id.
    pub user_id: String,
    /// The feature.
    pub feature: Feature,
    /// Artist MBID for artist activity, else empty.
    pub artist_mbid: String,
    /// Artist-page section for artist activity, else empty.
    pub section: String,
    /// Provider for artist activity, else empty.
    pub provider: String,
    /// The MusicBrainz source namespace it was recorded under.
    pub source: String,
}

/// The activity table.
#[derive(Clone, Debug)]
pub struct ActivityDb {
    db: Db,
}

impl ActivityDb {
    /// Bind the table over the database.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Record that the user used a feature at `now`. A source switch
    /// resets the row's schedule so the new source is warmed right away.
    pub async fn record(&self, row: Activity, now: i64) -> Result<(), DbError> {
        self.db
            .write(Lane::Background, "discover.activity.record", move |tx| {
                tx.execute(
                    "INSERT INTO catalog_discover_activity \
                         (user_id, feature, artist_mbid, section, provider, source, used_at) \
                     SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7 \
                     WHERE EXISTS (SELECT 1 FROM auth_users WHERE id = ?1) \
                     ON CONFLICT(user_id, feature, artist_mbid, section, provider) DO UPDATE SET \
                         used_at = excluded.used_at, source = excluded.source, \
                         retry_at = CASE WHEN catalog_discover_activity.source != excluded.source \
                             THEN 0 ELSE catalog_discover_activity.retry_at END",
                    rusqlite::params![
                        row.user_id,
                        row.feature.as_str(),
                        row.artist_mbid,
                        row.section,
                        row.provider,
                        row.source,
                        now
                    ],
                )?;
                Ok(())
            })
            .await
    }

    /// Forget activity last used at or before `before`, and all but each
    /// user's newest hundred rows.
    pub async fn prune(&self, before: i64) -> Result<(), DbError> {
        self.db
            .write(Lane::Background, "discover.activity.prune", move |tx| {
                tx.execute(
                    "DELETE FROM catalog_discover_activity WHERE used_at <= ?1",
                    rusqlite::params![before],
                )?;
                tx.execute(
                    "DELETE FROM catalog_discover_activity WHERE rowid IN (\
                         SELECT rowid FROM (SELECT rowid, ROW_NUMBER() OVER (\
                             PARTITION BY user_id ORDER BY used_at DESC, rowid DESC) AS n \
                         FROM catalog_discover_activity) WHERE n > ?1)",
                    rusqlite::params![PER_USER],
                )?;
                Ok(())
            })
            .await
    }

    /// Activity under `source` used after `since` and due by `now`, least
    /// recently serviced first, optionally for one user.
    pub async fn due(
        &self,
        source: &str,
        since: i64,
        now: i64,
        user_id: Option<&str>,
    ) -> Result<Vec<Activity>, String> {
        let rows: Vec<(String, String, String, String, String, String)> = sqlx::query_as(
            "SELECT user_id, feature, artist_mbid, section, provider, source \
             FROM catalog_discover_activity \
             WHERE source = ?1 AND used_at > ?2 AND retry_at <= ?3 \
               AND (?4 IS NULL OR user_id = ?4) \
             ORDER BY serviced_at, user_id, feature, artist_mbid LIMIT 100",
        )
        .bind(source)
        .bind(since)
        .bind(now)
        .bind(user_id)
        .fetch_all(self.db.sqlx())
        .await
        .map_err(|error| format!("discover activity read: {error}"))?;
        Ok(rows
            .into_iter()
            .filter_map(
                |(user_id, feature, artist_mbid, section, provider, source)| {
                    Some(Activity {
                        user_id,
                        feature: Feature::parse(&feature)?,
                        artist_mbid,
                        section,
                        provider,
                        source,
                    })
                },
            )
            .collect())
    }

    /// Mark one activity row serviced at `now` and due again at `retry_at`.
    pub async fn finish(&self, row: &Activity, now: i64, retry_at: i64) -> Result<(), DbError> {
        let row = row.clone();
        self.db
            .write(Lane::Background, "discover.activity.finish", move |tx| {
                tx.execute(
                    "UPDATE catalog_discover_activity SET serviced_at = ?1, retry_at = ?2 \
                     WHERE user_id = ?3 AND feature = ?4 AND artist_mbid = ?5 \
                       AND section = ?6 AND provider = ?7 AND source = ?8",
                    rusqlite::params![
                        now,
                        retry_at,
                        row.user_id,
                        row.feature.as_str(),
                        row.artist_mbid,
                        row.section,
                        row.provider,
                        row.source
                    ],
                )?;
                Ok(())
            })
            .await
    }
}
