//! The rows of `catalog_discover_activity`: which features each user used
//! in the last day, and when each is due to be warmed again. Reads use
//! the reader pool; writes go through the writer lane. Activity of a user
//! who no longer exists is not recorded, and goes with the account.

use super::Feature;
use crate::reads::discover::{read_all, sql};
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
                    sql::RECORD_ACTIVITY.text(),
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
                tx.execute(sql::PRUNE_ACTIVITY_BEFORE.text(), rusqlite::params![before])?;
                tx.execute(
                    sql::PRUNE_ACTIVITY_PER_USER.text(),
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
        let rows = read_all(
            &self.db,
            "discover.activity.due",
            sql::DUE_ACTIVITY.text(),
            (source.to_owned(), since, now, user_id.map(str::to_owned)),
            sql::ActivityColumns::read,
        )
        .await
        .map_err(|error| format!("discover activity read: {error}"))?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                Some(Activity {
                    user_id: row.user_id,
                    feature: Feature::parse(&row.feature)?,
                    artist_mbid: row.artist_mbid,
                    section: row.section,
                    provider: row.provider,
                    source: row.source,
                })
            })
            .collect())
    }

    /// Mark one activity row serviced at `now` and due again at `retry_at`.
    pub async fn finish(&self, row: &Activity, now: i64, retry_at: i64) -> Result<(), DbError> {
        let row = row.clone();
        self.db
            .write(Lane::Background, "discover.activity.finish", move |tx| {
                tx.execute(
                    sql::FINISH_ACTIVITY.text(),
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
