//! Wanted watches over `wanted_watches`.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, Params, Transaction, params};

use super::super::error::RequestsError;
use super::super::ledger::{WantedWatch, WatchState};
use super::super::sql;
use super::{lane_error, ms_from_secs, ms_from_secs_f64, read, secs_from_ms};
use crate::db::AcquireDb;

/// Decode one watch selected by `WATCH_SELECT`. A state outside its set
/// fails the read.
fn watch_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WantedWatch> {
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
        created_at: secs_from_ms(row.get(10)?),
        first_release_date: row.get(11)?,
        check_count: u32::try_from(row.get::<_, i64>(12)?).unwrap_or(0),
        quiet_streak: u32::try_from(row.get::<_, i64>(13)?).unwrap_or(0),
        next_check_at: secs_from_ms(row.get(14)?),
        new_candidate_count: u32::try_from(row.get::<_, i64>(15)?).unwrap_or(0),
    })
}

/// One watch inside a write transaction.
fn load_watch_tx(tx: &Transaction, key: &str) -> rusqlite::Result<Option<WantedWatch>> {
    tx.query_row(sql::WATCH_BY_KEY.text(), params![key], watch_from_row)
        .optional()
}

/// Every watch one statement selects.
fn read_watches(
    conn: &Connection,
    text: &str,
    values: impl Params,
) -> rusqlite::Result<Vec<WantedWatch>> {
    let mut statement = conn.prepare_cached(text)?;
    let rows = statement.query_map(values, watch_from_row)?;
    rows.collect()
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

/// One check of a watch, as [`WantedStore::record_check`] writes it.
#[derive(Debug, Clone, Default)]
pub struct WatchCheck {
    /// The quiet streak after this check.
    pub quiet_streak: u32,
    /// When the watch is next due (epoch seconds).
    pub next_check_at: u64,
    /// When the check ran (epoch seconds).
    pub now: u64,
    /// What the check found.
    pub outcome: &'static str,
    /// The release date, when this check found one.
    pub first_release_date: Option<String>,
    /// Every candidate the check saw, as `(source, identity)`.
    pub seen: Vec<(String, String)>,
    /// How many of them were new, when any were: the badge count.
    pub new_candidates: Option<u32>,
}

/// Durable wanted watches, shared by the wanted view and the watcher
/// loop.
#[derive(Clone)]
pub struct WantedStore {
    db: AcquireDb,
}

impl WantedStore {
    /// Delete stopped and fulfilled watches last touched before `cutoff`
    /// (epoch seconds); their seen candidates go with them. Returns the
    /// watches deleted.
    pub async fn prune(&self, cutoff: u64) -> Result<u64, RequestsError> {
        self.db
            .write_background("wanted.prune", move |tx| {
                let watches =
                    tx.execute(sql::PRUNE_WATCHES.text(), params![ms_from_secs(cutoff)])?;
                Ok(u64::try_from(watches).unwrap_or(0))
            })
            .await
            .map_err(|error| lane_error("wanted.prune", error))
    }

    /// Watches over one database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// Watches visible to one caller: own rows, or every row for admins,
    /// newest first.
    pub async fn watches_for(
        &self,
        user_id: &str,
        is_admin: bool,
    ) -> Result<Vec<WantedWatch>, RequestsError> {
        let user_id = user_id.to_owned();
        read(&self.db, "wanted.list", move |conn| {
            read_watches(conn, sql::WATCHES_FOR.text(), params![is_admin, user_id])
        })
        .await
    }

    /// One watch, if present.
    pub async fn get(&self, key: &str) -> Result<Option<WantedWatch>, RequestsError> {
        let key = key.to_lowercase();
        read(&self.db, "wanted.get", move |conn| {
            conn.query_row(sql::WATCH_BY_KEY.text(), params![key], watch_from_row)
                .optional()
        })
        .await
    }

    /// Watching rows due at `now`, oldest due first, at most `limit`.
    pub async fn list_due(
        &self,
        now: u64,
        limit: usize,
    ) -> Result<Vec<WantedWatch>, RequestsError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        read(&self.db, "wanted.list_due", move |conn| {
            read_watches(
                conn,
                sql::WATCHES_DUE.text(),
                params![WatchState::Watching, ms_from_secs(now), limit],
            )
        })
        .await
    }

    /// Enrol a watch. A fulfilled watch re-arms with fresh counters (the
    /// album was re-requested and failed again); any other existing watch
    /// stays untouched, so enrolment never overrides a human stop.
    /// Answers whether a watch armed.
    pub async fn enrol(&self, watch: WantedWatch) -> Result<bool, RequestsError> {
        self.db
            .write_background("wanted.enrol", move |tx| {
                let key = watch.key.to_lowercase();
                let inserted = tx.execute(
                    sql::ENROL_WATCH.text(),
                    params![
                        key,
                        watch.user_id,
                        watch.artist_name,
                        watch.album_title,
                        watch.artist_mbid,
                        watch.year,
                        watch.cover_url,
                        watch.kind,
                        WatchState::Watching,
                        ms_from_secs(watch.created_at),
                        watch.first_release_date,
                        ms_from_secs(watch.next_check_at),
                    ],
                )?;
                if inserted > 0 {
                    return Ok(true);
                }
                let rearmed = tx.execute(
                    sql::REARM_WATCH.text(),
                    params![
                        key,
                        WatchState::Watching,
                        watch.user_id,
                        watch.kind,
                        ms_from_secs(watch.created_at),
                        ms_from_secs(watch.next_check_at),
                        WatchState::Fulfilled,
                        watch.first_release_date,
                    ],
                )?;
                Ok(rearmed > 0)
            })
            .await
            .map_err(|error| lane_error("wanted.enrol", error))
    }

    /// Record one check: the new quiet streak and next due time, the
    /// release date when this check found one (a stored date is kept), the
    /// candidates it saw and, when some were new, the badge count. The
    /// check and its seen candidates land together.
    pub async fn record_check(&self, key: &str, check: WatchCheck) -> Result<(), RequestsError> {
        let key = key.to_lowercase();
        self.db
            .write_background("wanted.record_check", move |tx| {
                tx.execute(
                    sql::RECORD_CHECK.text(),
                    params![
                        key,
                        check.quiet_streak,
                        ms_from_secs(check.next_check_at),
                        ms_from_secs(check.now),
                        check.outcome,
                        check.first_release_date,
                        check.new_candidates
                    ],
                )?;
                for (source, identity) in &check.seen {
                    tx.execute(
                        sql::ADD_SEEN.text(),
                        params![key, source, identity, ms_from_secs(check.now)],
                    )?;
                }
                Ok(())
            })
            .await
            .map_err(|error| lane_error("wanted.record_check", error))
    }

    /// The candidates a watch has seen, as `(source, identity)`.
    pub async fn seen(&self, key: &str) -> Result<HashSet<(String, String)>, RequestsError> {
        let key = key.to_lowercase();
        read(&self.db, "wanted.seen", move |conn| {
            let mut statement = conn.prepare_cached(sql::SEEN_CANDIDATES.text())?;
            let rows = statement.query_map(params![key], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect()
        })
        .await
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
                    sql::MARK_FULFILLED.text(),
                    params![key, WatchState::Fulfilled, outcome, ms_from_secs(now)],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| lane_error("wanted.fulfilled", error))
    }

    /// Stop one watch: watching becomes stopped. Owners
    /// and admins only.
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
                    sql::MOVE_WATCH.text(),
                    params![key, WatchState::Stopped, WatchState::Watching],
                )?;
                Ok(())
            },
        )
        .await
    }

    /// Resume one watch: stopped becomes watching and
    /// due now with a fresh window; a watching row is simply due now; a
    /// fulfilled row refuses. Owners and admins only.
    pub async fn resume(
        &self,
        key: &str,
        user_id: &str,
        is_admin: bool,
        now: u64,
    ) -> Result<WatchChange, RequestsError> {
        self.change(
            key,
            user_id,
            is_admin,
            "wanted.resume",
            false,
            move |tx, key, state| {
                if state == WatchState::Watching {
                    tx.execute(sql::WATCH_DUE_AT.text(), params![key, ms_from_secs(now)])?;
                } else {
                    tx.execute(
                        sql::RESUME_WATCH.text(),
                        params![
                            key,
                            WatchState::Watching,
                            ms_from_secs(now),
                            WatchState::Stopped
                        ],
                    )?;
                }
                Ok(())
            },
        )
        .await
    }

    /// Remember candidates a person turned down for this album, so the
    /// watcher can tell them from copies that turn up later.
    /// Already-known ones keep their first-seen time.
    pub async fn add_seen(
        &self,
        key: &str,
        candidates: Vec<(String, String)>,
        now: u64,
    ) -> Result<(), RequestsError> {
        let key = key.to_lowercase();
        self.db
            .write("wanted.add_seen", move |tx| {
                for (source, identity) in &candidates {
                    tx.execute(
                        sql::ADD_SEEN.text(),
                        params![key, source, identity, ms_from_secs(now)],
                    )?;
                }
                Ok(())
            })
            .await
            .map_err(|error| lane_error("wanted.add_seen", error))
    }

    /// Clear one watch's unseen candidates. Owners and admins only.
    pub async fn mark_seen(
        &self,
        key: &str,
        user_id: &str,
        is_admin: bool,
    ) -> Result<WatchChange, RequestsError> {
        self.change(key, user_id, is_admin, "wanted.seen", true, |tx, key, _| {
            tx.execute(sql::CLEAR_UNSEEN.text(), params![key])?;
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
        F: FnOnce(&Transaction, &str, WatchState) -> rusqlite::Result<()> + Send + 'static,
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
                if watch.state == WatchState::Fulfilled && !allow_fulfilled {
                    return Ok(WatchChange::Fulfilled);
                }
                apply(tx, &key, watch.state)?;
                Ok(match load_watch_tx(tx, &key)? {
                    Some(watch) => WatchChange::Done(Box::new(watch)),
                    None => WatchChange::NotFound,
                })
            })
            .await
            .map_err(|error| lane_error(operation, error))
    }
}

/// After an admin removed the album at `removed_at` (unix seconds): stop a
/// watching watch armed at or before then. A watch armed since, by a new
/// request, is left alone. Answers whether it changed.
pub(crate) fn stop_after_removal(
    tx: &Transaction<'_>,
    key: &str,
    removed_at: f64,
) -> rusqlite::Result<bool> {
    let changed = tx.execute(
        sql::STOP_AFTER_REMOVAL.text(),
        params![
            key.to_lowercase(),
            WatchState::Stopped,
            WatchState::Watching,
            ms_from_secs_f64(removed_at)
        ],
    )?;
    Ok(changed > 0)
}

/// After an admin removed the album at `removed_at` but wants a
/// replacement: a watch fulfilled before then goes back to watching, due
/// `now`, with fresh counters.
/// Stopped watches keep the user's choice, and a watch armed
/// since the removal is left alone; rearming stamps `created_at`, so a
/// second run never rearms its own rearm.
pub(crate) fn rearm_after_removal(
    tx: &Transaction<'_>,
    key: &str,
    removed_at: f64,
    now: f64,
) -> rusqlite::Result<bool> {
    let changed = tx.execute(
        sql::REARM_AFTER_REMOVAL.text(),
        params![
            key.to_lowercase(),
            WatchState::Watching,
            ms_from_secs_f64(now),
            WatchState::Fulfilled,
            ms_from_secs_f64(removed_at)
        ],
    )?;
    Ok(changed > 0)
}
