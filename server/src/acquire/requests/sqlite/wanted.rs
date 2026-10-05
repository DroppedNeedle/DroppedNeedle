//! Wanted watches over `wanted_watches`.

use rusqlite::{OptionalExtension, Transaction, params};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::super::error::RequestsError;
use super::super::ledger::{
    WATCH_DORMANT, WATCH_FULFILLED, WATCH_STOPPED, WATCH_WATCHING, WantedWatch,
};
use super::{epoch_from_real, lane_error, read_error};
use crate::acquire::db::AcquireDb;

/// Columns every watch read selects, in decode order.
const WATCH_SELECT: &str = "SELECT w.release_group_mbid_lower, w.user_id, u.display_name, \
     w.artist_name, w.album_title, w.artist_mbid, w.year, w.cover_url, w.kind, w.state, \
     w.created_at, w.first_release_date, w.check_count, w.quiet_streak, w.next_check_at, \
     w.new_candidate_count FROM wanted_watches w LEFT JOIN auth_users u ON u.id = w.user_id";

fn watch_from_sqlx(row: &SqliteRow) -> Result<WantedWatch, sqlx::Error> {
    Ok(WantedWatch {
        key: row.try_get(0)?,
        user_id: row.try_get(1)?,
        user_name: row.try_get(2)?,
        artist_name: row.try_get(3)?,
        album_title: row.try_get(4)?,
        artist_mbid: row.try_get(5)?,
        year: row
            .try_get::<Option<i64>, _>(6)?
            .and_then(|year| i32::try_from(year).ok()),
        cover_url: row.try_get(7)?,
        kind: row.try_get(8)?,
        state: row.try_get(9)?,
        created_at: epoch_from_real(row.try_get(10)?),
        first_release_date: row.try_get(11)?,
        check_count: u32::try_from(row.try_get::<i64, _>(12)?).unwrap_or(0),
        quiet_streak: u32::try_from(row.try_get::<i64, _>(13)?).unwrap_or(0),
        next_check_at: epoch_from_real(row.try_get(14)?),
        new_candidate_count: u32::try_from(row.try_get::<i64, _>(15)?).unwrap_or(0),
    })
}

fn watch_from_rusqlite(row: &rusqlite::Row<'_>) -> rusqlite::Result<WantedWatch> {
    Ok(WantedWatch {
        key: row.get(0)?,
        user_id: row.get(1)?,
        user_name: row.get(2)?,
        artist_name: row.get(3)?,
        album_title: row.get(4)?,
        artist_mbid: row.get(5)?,
        year: row
            .get::<_, Option<i64>>(6)?
            .and_then(|year| i32::try_from(year).ok()),
        cover_url: row.get(7)?,
        kind: row.get(8)?,
        state: row.get(9)?,
        created_at: epoch_from_real(row.get(10)?),
        first_release_date: row.get(11)?,
        check_count: u32::try_from(row.get::<_, i64>(12)?).unwrap_or(0),
        quiet_streak: u32::try_from(row.get::<_, i64>(13)?).unwrap_or(0),
        next_check_at: epoch_from_real(row.get(14)?),
        new_candidate_count: u32::try_from(row.get::<_, i64>(15)?).unwrap_or(0),
    })
}

/// One watch inside a write transaction.
fn load_watch_tx(tx: &Transaction, key: &str) -> rusqlite::Result<Option<WantedWatch>> {
    let sql = format!("{WATCH_SELECT} WHERE w.release_group_mbid_lower = ?1");
    tx.query_row(&sql, params![key], watch_from_rusqlite)
        .optional()
}

/// What a guarded watch mutation found.
#[derive(Debug, Clone, PartialEq)]
pub enum WatchChange {
    /// The watch moved (or already sat where asked) and reads like this.
    Done(Box<WantedWatch>),
    /// No watch, or one the caller may not touch.
    NotFound,
    /// The watch is fulfilled; re-request the album to watch it again.
    Fulfilled,
}

/// Durable wanted watches, shared by the wanted view and the watcher loop
/// (v2 `WantedStore`).
#[derive(Clone)]
pub struct WantedStore {
    db: AcquireDb,
}

impl WantedStore {
    /// Delete stopped and fulfilled watches last touched before `cutoff`
    /// (epoch seconds), then seen-candidate rows left without a watch.
    /// Returns `(watches, seen rows)` deleted (v2 `WantedStore.prune`).
    pub async fn prune(&self, cutoff: u64) -> Result<(u64, u64), RequestsError> {
        self.db
            .write_background("wanted.prune", move |tx| {
                let watches = tx.execute(
                    "DELETE FROM wanted_watches WHERE state IN ('stopped', 'fulfilled') \
                     AND COALESCE(last_checked_at, created_at) < ?1",
                    params![cutoff as f64],
                )?;
                let seen = tx.execute(
                    "DELETE FROM wanted_seen_candidates WHERE release_group_mbid_lower \
                     NOT IN (SELECT release_group_mbid_lower FROM wanted_watches)",
                    [],
                )?;
                Ok((
                    u64::try_from(watches).unwrap_or(0),
                    u64::try_from(seen).unwrap_or(0),
                ))
            })
            .await
            .map_err(|error| lane_error("wanted.prune", error))
    }

    /// Watches over one database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// Watches visible to one caller: own rows, or every row for admins,
    /// newest first (v2 `list_watches`).
    pub async fn watches_for(
        &self,
        user_id: &str,
        is_admin: bool,
    ) -> Result<Vec<WantedWatch>, RequestsError> {
        let sql = format!(
            "{WATCH_SELECT} WHERE ?1 OR w.user_id = ?2 ORDER BY w.created_at DESC, \
             w.release_group_mbid_lower"
        );
        let rows = sqlx::query(&sql)
            .bind(is_admin)
            .bind(user_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(|error| read_error("wanted.list", error))?;
        rows.iter()
            .map(watch_from_sqlx)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| read_error("wanted.list", error))
    }

    /// One watch, if present.
    pub async fn get(&self, key: &str) -> Result<Option<WantedWatch>, RequestsError> {
        let sql = format!("{WATCH_SELECT} WHERE w.release_group_mbid_lower = ?1");
        let row = sqlx::query(&sql)
            .bind(key.to_lowercase())
            .fetch_optional(self.db.pool())
            .await
            .map_err(|error| read_error("wanted.get", error))?;
        row.as_ref()
            .map(watch_from_sqlx)
            .transpose()
            .map_err(|error| read_error("wanted.get", error))
    }

    /// Watching rows due at `now`, oldest due first, at most `limit`.
    pub async fn list_due(
        &self,
        now: u64,
        limit: usize,
    ) -> Result<Vec<WantedWatch>, RequestsError> {
        let sql = format!(
            "{WATCH_SELECT} WHERE w.state = ?1 AND w.next_check_at <= ?2 \
             ORDER BY w.next_check_at, w.release_group_mbid_lower LIMIT ?3"
        );
        let rows = sqlx::query(&sql)
            .bind(WATCH_WATCHING)
            .bind(now as f64)
            .bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .fetch_all(self.db.pool())
            .await
            .map_err(|error| read_error("wanted.list_due", error))?;
        rows.iter()
            .map(watch_from_sqlx)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| read_error("wanted.list_due", error))
    }

    /// Enrol a watch. A fulfilled watch re-arms with fresh counters (the
    /// album was re-requested and failed again); any other existing watch
    /// stays untouched, so enrolment never overrides a human stop (v2
    /// `create_watch` plus `rearm_watch`). Answers whether a watch armed.
    pub async fn enrol(&self, watch: WantedWatch) -> Result<bool, RequestsError> {
        self.db
            .write_background("wanted.enrol", move |tx| {
                let key = watch.key.to_lowercase();
                let inserted = tx.execute(
                    "INSERT INTO wanted_watches (release_group_mbid_lower, release_group_mbid, \
                     user_id, artist_name, album_title, artist_mbid, year, cover_url, kind, \
                     state, created_at, first_release_date, next_check_at) \
                     VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
                     ON CONFLICT (release_group_mbid_lower) DO NOTHING",
                    params![
                        key,
                        watch.user_id,
                        watch.artist_name,
                        watch.album_title,
                        watch.artist_mbid,
                        watch.year,
                        watch.cover_url,
                        watch.kind,
                        WATCH_WATCHING,
                        watch.created_at as f64,
                        watch.first_release_date,
                        watch.next_check_at as f64,
                    ],
                )?;
                if inserted > 0 {
                    return Ok(true);
                }
                let rearmed = tx.execute(
                    "UPDATE wanted_watches SET state = ?2, user_id = ?3, kind = ?4, \
                     created_at = ?5, check_count = 0, quiet_streak = 0, last_checked_at = NULL, \
                     next_check_at = ?6, last_outcome = NULL, new_candidate_count = 0 \
                     WHERE release_group_mbid_lower = ?1 AND state = ?7",
                    params![
                        key,
                        WATCH_WATCHING,
                        watch.user_id,
                        watch.kind,
                        watch.created_at as f64,
                        watch.next_check_at as f64,
                        WATCH_FULFILLED,
                    ],
                )?;
                Ok(rearmed > 0)
            })
            .await
            .map_err(|error| lane_error("wanted.enrol", error))
    }

    /// Record one check: the new quiet streak and next due time.
    pub async fn record_check(
        &self,
        key: &str,
        quiet_streak: u32,
        next_check_at: u64,
        now: u64,
        outcome: &str,
    ) -> Result<(), RequestsError> {
        let key = key.to_lowercase();
        let outcome = outcome.to_owned();
        self.db
            .write_background("wanted.record_check", move |tx| {
                tx.execute(
                    "UPDATE wanted_watches SET check_count = check_count + 1, \
                     quiet_streak = ?2, next_check_at = ?3, last_checked_at = ?4, \
                     last_outcome = ?5 WHERE release_group_mbid_lower = ?1",
                    params![key, quiet_streak, next_check_at as f64, now as f64, outcome],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| lane_error("wanted.record_check", error))
    }

    /// Mark a watch fulfilled (the album reached the library).
    pub async fn mark_fulfilled(
        &self,
        key: &str,
        outcome: &str,
        now: u64,
    ) -> Result<(), RequestsError> {
        let key = key.to_lowercase();
        let outcome = outcome.to_owned();
        self.db
            .write_background("wanted.fulfilled", move |tx| {
                tx.execute(
                    "UPDATE wanted_watches SET state = ?2, last_outcome = ?3, \
                     last_checked_at = ?4, new_candidate_count = 0 \
                     WHERE release_group_mbid_lower = ?1",
                    params![key, WATCH_FULFILLED, outcome, now as f64],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| lane_error("wanted.fulfilled", error))
    }

    /// Stop one watch (v2 `stop_watch`): watching or dormant becomes
    /// stopped. Owners and admins only.
    pub async fn stop(
        &self,
        key: &str,
        user_id: &str,
        is_admin: bool,
    ) -> Result<WatchChange, RequestsError> {
        self.change(
            key,
            user_id,
            is_admin,
            "wanted.stop",
            false,
            |tx, key, _| {
                tx.execute(
                    "UPDATE wanted_watches SET state = ?2 WHERE release_group_mbid_lower = ?1 \
                 AND state IN (?3, ?4)",
                    params![key, WATCH_STOPPED, WATCH_WATCHING, WATCH_DORMANT],
                )?;
                Ok(())
            },
        )
        .await
    }

    /// Resume one watch (v2 `resume_watch`): dormant or stopped becomes
    /// watching and due now with a fresh window; a watching row is simply
    /// due now; a fulfilled row refuses. Owners and admins only.
    pub async fn resume(
        &self,
        key: &str,
        user_id: &str,
        is_admin: bool,
        now: u64,
    ) -> Result<WatchChange, RequestsError> {
        self.change(key, user_id, is_admin, "wanted.resume", false, move |tx, key, state| {
            if state == WATCH_WATCHING {
                tx.execute(
                    "UPDATE wanted_watches SET next_check_at = ?2 WHERE release_group_mbid_lower = ?1",
                    params![key, now as f64],
                )?;
            } else {
                tx.execute(
                    "UPDATE wanted_watches SET state = ?2, next_check_at = ?3, created_at = ?3 \
                     WHERE release_group_mbid_lower = ?1 AND state IN (?4, ?5)",
                    params![key, WATCH_WATCHING, now as f64, WATCH_DORMANT, WATCH_STOPPED],
                )?;
            }
            Ok(())
        })
        .await
    }

    /// Clear one watch's unseen candidates. Owners and admins only.
    pub async fn mark_seen(
        &self,
        key: &str,
        user_id: &str,
        is_admin: bool,
    ) -> Result<WatchChange, RequestsError> {
        self.change(key, user_id, is_admin, "wanted.seen", true, |tx, key, _| {
            tx.execute(
                "UPDATE wanted_watches SET new_candidate_count = 0 \
                 WHERE release_group_mbid_lower = ?1",
                params![key],
            )?;
            Ok(())
        })
        .await
    }

    /// Ownership-checked mutation; a fulfilled watch refuses everything
    /// except marking seen.
    async fn change<F>(
        &self,
        key: &str,
        user_id: &str,
        is_admin: bool,
        operation: &'static str,
        allow_fulfilled: bool,
        apply: F,
    ) -> Result<WatchChange, RequestsError>
    where
        F: FnOnce(&Transaction, &str, &str) -> rusqlite::Result<()> + Send + 'static,
    {
        let key = key.to_lowercase();
        let user_id = user_id.to_owned();
        self.db
            .write(operation, move |tx| {
                let Some(watch) = load_watch_tx(tx, &key)? else {
                    return Ok(WatchChange::NotFound);
                };
                if !is_admin && watch.user_id != user_id {
                    return Ok(WatchChange::NotFound);
                }
                if watch.state == WATCH_FULFILLED && !allow_fulfilled {
                    return Ok(WatchChange::Fulfilled);
                }
                apply(tx, &key, &watch.state)?;
                Ok(match load_watch_tx(tx, &key)? {
                    Some(watch) => WatchChange::Done(Box::new(watch)),
                    None => WatchChange::NotFound,
                })
            })
            .await
            .map_err(|error| lane_error(operation, error))
    }
}
