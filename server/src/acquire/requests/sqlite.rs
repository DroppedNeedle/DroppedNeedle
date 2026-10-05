//! SQLite stores behind the requests surface.
//!
//! [`RequestStore`] keeps the request ledger in `request_history`, its
//! listeners in `request_history_requesters` and per-user hides in
//! `request_history_dismissals`. [`WantedStore`] keeps `wanted_watches`,
//! shared by the wanted view and the watcher loop. [`FollowApprovalStore`],
//! [`PersonalMixStore`] and [`EditionStore`] keep the approval queues and
//! the in-flight edition acquires. Reads use the reader pool; every
//! mutation is one writer-lane transaction, so each compare-and-swap reads
//! and writes the row under the write lock.
//!
//! Album rows key on the lowercased release-group MBID and track rows on
//! `track:` plus the lowercased recording MBID (the v2 key scheme). Times
//! are epoch seconds.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use rusqlite::{OptionalExtension, Transaction, params};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::error::RequestsError;
use super::ledger::{
    ApprovalBatch, BeginOutcome, CANCELLABLE_STATUSES, CancelDecision, EditionMark, FollowApproval,
    MixApproval, QuotaGate, QuotaRefusal, RETRYABLE_STATUSES, RequestRecord, Requester, RetryClaim,
    STATUS_AWAITING_APPROVAL, STATUS_CANCELLED, STATUS_CANCELLING, STATUS_PENDING, STATUS_REJECTED,
    WATCH_DORMANT, WATCH_FULFILLED, WATCH_STOPPED, WATCH_WATCHING, WantedWatch, is_active,
};
use super::models::RequestKind;
use crate::acquire::db::{AcquireDb, to_i64, to_u64};
use crate::db::{DbError, map_sqlx_busy};

/// Map a writer-lane failure: busy stays retryable, the rest is a fault.
fn lane_error(operation: &'static str, error: DbError) -> RequestsError {
    match error {
        DbError::Busy { .. } | DbError::LaneClosed => RequestsError::Busy {
            operation: operation.to_owned(),
        },
        other => RequestsError::internal(&format_args!("{operation}: {other}")),
    }
}

/// Map a reader-pool failure the same way.
fn read_error(operation: &'static str, error: sqlx::Error) -> RequestsError {
    lane_error(operation, map_sqlx_busy(operation, error))
}

/// Ledger key for one row (v2 `_request_key`).
fn row_key(kind: RequestKind, key: &str) -> String {
    let lower = key.trim().to_lowercase();
    match kind {
        RequestKind::Album => lower,
        RequestKind::Track => format!("track:{lower}"),
    }
}

/// Columns every record read selects, in decode order. Times are stored as
/// epoch seconds in v2's TEXT columns, so reads cast them back.
const RECORD_SELECT: &str = "SELECT rh.musicbrainz_id_lower, rh.request_kind, rh.status, \
     rh.artist_name, rh.album_title, rh.artist_mbid, rh.year, rh.release_mbid, \
     rh.track_title, rh.duration_seconds, rh.track_release_group_mbid, rh.user_id, \
     rh.requested_by_name, CAST(rh.requested_at AS INTEGER) AS requested_at, \
     CAST(rh.completed_at AS INTEGER) AS completed_at, rh.download_task_id, rh.generation, \
     rh.dispatch_authorized, rh.monitor_artist, rh.auto_download_artist, \
     rh.reviewed_by_name, CAST(rh.reviewed_at AS INTEGER) AS reviewed_at \
     FROM request_history rh";

/// Per-user filter: with `?1` NULL every row passes; otherwise the row's
/// owner or a listener is `?1` and that user has not hidden it.
const MEMBER_FILTER: &str = "(?1 IS NULL OR ((rh.user_id = ?1 OR EXISTS (SELECT 1 FROM \
     request_history_requesters rr WHERE rr.musicbrainz_id_lower = rh.musicbrainz_id_lower \
     AND rr.user_id = ?1)) AND NOT EXISTS (SELECT 1 FROM request_history_dismissals d \
     WHERE d.musicbrainz_id_lower = rh.musicbrainz_id_lower AND d.user_id = ?1)))";

/// Split a ledger key into kind and bare MBID.
fn split_key(stored: &str, kind: &str) -> (RequestKind, String) {
    let kind = RequestKind::parse(kind).unwrap_or(RequestKind::Album);
    let key = stored.strip_prefix("track:").unwrap_or(stored).to_owned();
    (kind, key)
}

/// Decode one record from the reader pool (requesters attach later).
fn record_from_sqlx(row: &SqliteRow) -> Result<RequestRecord, sqlx::Error> {
    let stored: String = row.try_get(0)?;
    let kind: String = row.try_get(1)?;
    let (kind, key) = split_key(&stored, &kind);
    Ok(RequestRecord {
        key,
        kind,
        status: row.try_get(2)?,
        artist_name: row.try_get(3)?,
        album_title: row.try_get(4)?,
        artist_mbid: row.try_get(5)?,
        year: row
            .try_get::<Option<i64>, _>(6)?
            .and_then(|year| i32::try_from(year).ok()),
        release_mbid: row.try_get(7)?,
        track_title: row.try_get(8)?,
        duration_seconds: row.try_get(9)?,
        track_release_group_mbid: row.try_get(10)?,
        user_id: row.try_get(11)?,
        requested_by_name: row.try_get(12)?,
        requesters: Vec::new(),
        requested_at: row.try_get::<Option<i64>, _>(13)?.map_or(0, to_u64),
        completed_at: row.try_get::<Option<i64>, _>(14)?.map(to_u64),
        task_id: row.try_get(15)?,
        generation: to_u64(row.try_get(16)?),
        dispatch_authorized: row.try_get::<i64, _>(17)? != 0,
        monitor_artist: row.try_get::<i64, _>(18)? != 0,
        auto_download_artist: row.try_get::<i64, _>(19)? != 0,
        reviewed_by_name: row.try_get(20)?,
        reviewed_at: row.try_get::<Option<i64>, _>(21)?.map(to_u64),
    })
}

/// Decode one record inside a write transaction (requesters attach later).
fn record_from_rusqlite(row: &rusqlite::Row<'_>) -> rusqlite::Result<RequestRecord> {
    let stored: String = row.get(0)?;
    let kind: String = row.get(1)?;
    let (kind, key) = split_key(&stored, &kind);
    Ok(RequestRecord {
        key,
        kind,
        status: row.get(2)?,
        artist_name: row.get(3)?,
        album_title: row.get(4)?,
        artist_mbid: row.get(5)?,
        year: row
            .get::<_, Option<i64>>(6)?
            .and_then(|year| i32::try_from(year).ok()),
        release_mbid: row.get(7)?,
        track_title: row.get(8)?,
        duration_seconds: row.get(9)?,
        track_release_group_mbid: row.get(10)?,
        user_id: row.get(11)?,
        requested_by_name: row.get(12)?,
        requesters: Vec::new(),
        requested_at: row.get::<_, Option<i64>>(13)?.map_or(0, to_u64),
        completed_at: row.get::<_, Option<i64>>(14)?.map(to_u64),
        task_id: row.get(15)?,
        generation: to_u64(row.get(16)?),
        dispatch_authorized: row.get::<_, i64>(17)? != 0,
        monitor_artist: row.get::<_, i64>(18)? != 0,
        auto_download_artist: row.get::<_, i64>(19)? != 0,
        reviewed_by_name: row.get(20)?,
        reviewed_at: row.get::<_, Option<i64>>(21)?.map(to_u64),
    })
}

/// One full record inside a write transaction, listeners included.
fn load_record_tx(tx: &Transaction, stored: &str) -> rusqlite::Result<Option<RequestRecord>> {
    let sql = format!("{RECORD_SELECT} WHERE rh.musicbrainz_id_lower = ?1");
    let Some(mut record) = tx
        .query_row(&sql, params![stored], record_from_rusqlite)
        .optional()?
    else {
        return Ok(None);
    };
    let mut statement = tx.prepare(
        "SELECT user_id, requested_by_name FROM request_history_requesters \
         WHERE musicbrainz_id_lower = ?1 ORDER BY requested_at, user_id",
    )?;
    let rows = statement.query_map(params![stored], |row| {
        Ok(Requester {
            user_id: row.get(0)?,
            name: row.get(1)?,
        })
    })?;
    for requester in rows {
        let requester = requester?;
        if record.user_id.as_deref() != Some(requester.user_id.as_str()) {
            record.requesters.push(requester);
        }
    }
    Ok(Some(record))
}

/// Asks one listener made since `since` (v2 `async_count_user_requests_since`).
fn count_asks_tx(tx: &Transaction, user_id: &str, since: u64) -> rusqlite::Result<u32> {
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM request_history_requesters rr \
         JOIN request_history rh ON rh.musicbrainz_id_lower = rr.musicbrainz_id_lower \
         WHERE rr.user_id = ?1 AND CAST(rr.requested_at AS INTEGER) >= ?2",
        params![user_id, to_i64(since)],
        |row| row.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Whether one user owns or listens to a row, inside a write transaction.
fn is_member_tx(tx: &Transaction, stored: &str, user_id: &str) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM request_history WHERE musicbrainz_id_lower = ?1 \
         AND user_id = ?2) OR EXISTS (SELECT 1 FROM request_history_requesters \
         WHERE musicbrainz_id_lower = ?1 AND user_id = ?2)",
        params![stored, user_id],
        |row| row.get(0),
    )
}

/// Claim one generation for a record inside a write transaction (v2
/// `async_record_request`). A live or cancelling row wins over the
/// newcomer; a terminal row is reused with its listeners and dismissals
/// cleared.
fn begin_tx(tx: &Transaction, mut record: RequestRecord) -> rusqlite::Result<BeginOutcome> {
    let stored = row_key(record.kind, &record.key);
    let existing: Option<(String, i64)> = tx
        .query_row(
            "SELECT status, generation FROM request_history WHERE musicbrainz_id_lower = ?1",
            params![stored],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((status, _)) = &existing
        && (is_active(status) || status == STATUS_CANCELLING)
    {
        let current = load_record_tx(tx, &stored)?;
        return Ok(match current {
            Some(current) => BeginOutcome::Existing(current),
            None => BeginOutcome::Existing(record),
        });
    }
    let generation = existing
        .as_ref()
        .map_or(1, |(_, generation)| generation.saturating_add(1));
    record.generation = to_u64(generation);
    record.requesters.clear();
    if existing.is_some() {
        tx.execute(
            "DELETE FROM request_history_requesters WHERE musicbrainz_id_lower = ?1",
            params![stored],
        )?;
        tx.execute(
            "DELETE FROM request_history_dismissals WHERE musicbrainz_id_lower = ?1",
            params![stored],
        )?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO request_history (musicbrainz_id_lower, musicbrainz_id, \
         artist_name, album_title, artist_mbid, year, cover_url, requested_at, completed_at, \
         status, monitor_artist, auto_download_artist, user_id, requested_by_name, \
         reviewed_by_id, reviewed_by_name, reviewed_at, download_task_id, release_mbid, \
         request_kind, track_title, duration_seconds, track_release_group_mbid, \
         dispatch_authorized, generation) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, \
         ?10, ?11, ?12, ?13, NULL, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
        params![
            stored,
            record.key,
            record.artist_name,
            record.album_title,
            record.artist_mbid,
            record.year,
            to_i64(record.requested_at),
            record.completed_at.map(to_i64),
            record.status,
            record.monitor_artist,
            record.auto_download_artist,
            record.user_id,
            record.requested_by_name,
            record.reviewed_by_name,
            record.reviewed_at.map(to_i64),
            record.task_id,
            record.release_mbid,
            record.kind.as_str(),
            record.track_title,
            record.duration_seconds,
            record.track_release_group_mbid,
            record.dispatch_authorized,
            generation,
        ],
    )?;
    if let Some(owner) = &record.user_id {
        tx.execute(
            "INSERT OR REPLACE INTO request_history_requesters (user_id, musicbrainz_id_lower, \
             requested_at, requested_by_name) VALUES (?1, ?2, ?3, ?4)",
            params![
                owner,
                stored,
                to_i64(record.requested_at),
                record.requested_by_name
            ],
        )?;
    }
    Ok(BeginOutcome::Won(record))
}

/// Optional generation guard as SQL: NULL matches any generation.
fn guard(expected: Option<u64>) -> Option<i64> {
    expected.map(to_i64)
}

/// Durable request ledger.
#[derive(Clone)]
pub struct RequestStore {
    db: AcquireDb,
}

impl RequestStore {
    /// Ledger over one database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// Read one row with its listeners.
    pub async fn get(
        &self,
        kind: RequestKind,
        key: &str,
    ) -> Result<Option<RequestRecord>, RequestsError> {
        let sql = format!("{RECORD_SELECT} WHERE rh.musicbrainz_id_lower = ?1");
        let row = sqlx::query(&sql)
            .bind(row_key(kind, key))
            .fetch_optional(self.db.pool())
            .await
            .map_err(|error| read_error("requests.get", error))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let record = record_from_sqlx(&row).map_err(|error| read_error("requests.get", error))?;
        let mut records = vec![record];
        self.attach_listeners(&mut records).await?;
        Ok(records.pop())
    }

    /// Lowercased MBIDs with a live row of one kind (v2
    /// `async_get_requested_mbids`, the batch dedupe read).
    pub async fn active_mbids(&self, kind: RequestKind) -> Result<HashSet<String>, RequestsError> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT musicbrainz_id_lower FROM request_history WHERE request_kind = ?1 \
             AND status IN ('pending','downloading','queued','awaiting_approval')",
        )
        .bind(kind.as_str())
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| read_error("requests.active_mbids", error))?;
        Ok(rows
            .into_iter()
            .map(|stored| stored.strip_prefix("track:").unwrap_or(&stored).to_owned())
            .collect())
    }

    /// Claim one row, enforcing the quota gate inside the same write.
    pub async fn begin(
        &self,
        record: RequestRecord,
        owner: &str,
        gate: Option<QuotaGate>,
    ) -> Result<Result<BeginOutcome, QuotaRefusal>, RequestsError> {
        match self.begin_batch(vec![record], owner, gate).await? {
            Err(refusal) => Ok(Err(refusal)),
            Ok(outcomes) => outcomes
                .into_iter()
                .next()
                .map(Ok)
                .ok_or_else(|| RequestsError::internal(&"request begin returned no outcome")),
        }
    }

    /// Claim several rows in one write. The quota gate counts the asker's
    /// window once, against `gate.new_requests`, before any row is written;
    /// a refusal writes nothing.
    pub async fn begin_batch(
        &self,
        records: Vec<RequestRecord>,
        owner: &str,
        gate: Option<QuotaGate>,
    ) -> Result<Result<Vec<BeginOutcome>, QuotaRefusal>, RequestsError> {
        let owner = owner.to_owned();
        let now = records.first().map_or(0, |record| record.requested_at);
        self.db
            .write("requests.begin", move |tx| {
                if let Some(gate) = gate.filter(|gate| gate.limit > 0) {
                    let since = now.saturating_sub(u64::from(gate.window_days.max(1)) * 86_400);
                    let used = count_asks_tx(tx, &owner, since)?;
                    if used.saturating_add(gate.new_requests) > gate.limit {
                        return Ok(Err(QuotaRefusal {
                            used,
                            limit: gate.limit,
                            window_days: gate.window_days,
                        }));
                    }
                }
                let mut outcomes = Vec::with_capacity(records.len());
                for record in records {
                    outcomes.push(begin_tx(tx, record)?);
                }
                Ok(Ok(outcomes))
            })
            .await
            .map_err(|error| lane_error("requests.begin", error))
    }

    /// Attach a listener to a live row (v2 `async_add_requester`). Terminal
    /// and cancelling rows refuse; the generation is untouched.
    pub async fn attach_requester(
        &self,
        kind: RequestKind,
        key: &str,
        user_id: &str,
        name: Option<String>,
        now: u64,
    ) -> Result<bool, RequestsError> {
        let stored = row_key(kind, key);
        let user_id = user_id.to_owned();
        self.db
            .write("requests.attach", move |tx| {
                let status: Option<String> = tx
                    .query_row(
                        "SELECT status FROM request_history WHERE musicbrainz_id_lower = ?1",
                        params![stored],
                        |row| row.get(0),
                    )
                    .optional()?;
                if !status.as_deref().is_some_and(is_active) {
                    return Ok(false);
                }
                tx.execute(
                    "INSERT INTO request_history_requesters (user_id, musicbrainz_id_lower, \
                     requested_at, requested_by_name) VALUES (?1, ?2, ?3, ?4) \
                     ON CONFLICT (user_id, musicbrainz_id_lower) DO UPDATE SET \
                     requested_by_name = COALESCE(excluded.requested_by_name, \
                     request_history_requesters.requested_by_name)",
                    params![user_id, stored, to_i64(now), name],
                )?;
                Ok(true)
            })
            .await
            .map_err(|error| lane_error("requests.attach", error))
    }

    /// Widen monitoring flags; never narrows them (v2 batch/single quirk).
    pub async fn widen_monitoring(
        &self,
        kind: RequestKind,
        key: &str,
        auto_download_artist: bool,
    ) -> Result<(), RequestsError> {
        let stored = row_key(kind, key);
        self.db
            .write("requests.widen_monitoring", move |tx| {
                tx.execute(
                    "UPDATE request_history SET monitor_artist = 1, auto_download_artist = ?2 \
                     WHERE musicbrainz_id_lower = ?1 AND monitor_artist = 0",
                    params![stored, auto_download_artist],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| lane_error("requests.widen_monitoring", error))
    }

    /// Move one row's status behind the generation CAS (`None` skips the
    /// guard). Answers whether the row moved.
    pub async fn update_status(
        &self,
        kind: RequestKind,
        key: &str,
        status: &str,
        completed_at: Option<u64>,
        expected_generation: Option<u64>,
    ) -> Result<bool, RequestsError> {
        let stored = row_key(kind, key);
        let status = status.to_owned();
        let expected = guard(expected_generation);
        self.db
            .write("requests.update_status", move |tx| {
                let changed = tx.execute(
                    "UPDATE request_history SET status = ?2, completed_at = ?3, \
                     generation = generation + 1 WHERE musicbrainz_id_lower = ?1 \
                     AND (?4 IS NULL OR generation = ?4)",
                    params![stored, status, completed_at.map(to_i64), expected],
                )?;
                Ok(changed > 0)
            })
            .await
            .map_err(|error| lane_error("requests.update_status", error))
    }

    /// Link one row to its download task behind the generation CAS (`None`
    /// skips the guard).
    pub async fn link_task(
        &self,
        kind: RequestKind,
        key: &str,
        task_id: &str,
        expected_generation: Option<u64>,
    ) -> Result<bool, RequestsError> {
        let stored = row_key(kind, key);
        let task_id = task_id.to_owned();
        let expected = guard(expected_generation);
        self.db
            .write("requests.link_task", move |tx| {
                let changed = tx.execute(
                    "UPDATE request_history SET download_task_id = ?2, \
                     generation = generation + 1 WHERE musicbrainz_id_lower = ?1 \
                     AND (?3 IS NULL OR generation = ?3)",
                    params![stored, task_id, expected],
                )?;
                Ok(changed > 0)
            })
            .await
            .map_err(|error| lane_error("requests.link_task", error))
    }

    /// Claim an approval: `awaiting_approval` becomes `pending` with the
    /// reviewer stamped and dispatch authorized (v2 `async_claim_approval`).
    pub async fn claim_approval(
        &self,
        kind: RequestKind,
        key: &str,
        reviewer_name: Option<String>,
        reviewed_at: u64,
        expected_generation: u64,
    ) -> Result<Option<RequestRecord>, RequestsError> {
        let stored = row_key(kind, key);
        self.db
            .write("requests.claim_approval", move |tx| {
                let changed = tx.execute(
                    "UPDATE request_history SET status = ?2, dispatch_authorized = 1, \
                     reviewed_by_name = ?3, reviewed_at = ?4, generation = generation + 1 \
                     WHERE musicbrainz_id_lower = ?1 AND status = ?5 AND generation = ?6",
                    params![
                        stored,
                        STATUS_PENDING,
                        reviewer_name,
                        to_i64(reviewed_at),
                        STATUS_AWAITING_APPROVAL,
                        to_i64(expected_generation)
                    ],
                )?;
                if changed == 0 {
                    return Ok(None);
                }
                Ok(load_record_tx(tx, &stored)?)
            })
            .await
            .map_err(|error| lane_error("requests.claim_approval", error))
    }

    /// Claim a rejection: `awaiting_approval` becomes `rejected`.
    pub async fn claim_rejection(
        &self,
        kind: RequestKind,
        key: &str,
        reviewer_name: Option<String>,
        now: u64,
        expected_generation: u64,
    ) -> Result<bool, RequestsError> {
        let stored = row_key(kind, key);
        self.db
            .write("requests.claim_rejection", move |tx| {
                let changed = tx.execute(
                    "UPDATE request_history SET status = ?2, dispatch_authorized = 0, \
                     reviewed_by_name = ?3, reviewed_at = ?4, completed_at = ?4, \
                     generation = generation + 1 WHERE musicbrainz_id_lower = ?1 \
                     AND status = ?5 AND generation = ?6",
                    params![
                        stored,
                        STATUS_REJECTED,
                        reviewer_name,
                        to_i64(now),
                        STATUS_AWAITING_APPROVAL,
                        to_i64(expected_generation)
                    ],
                )?;
                Ok(changed > 0)
            })
            .await
            .map_err(|error| lane_error("requests.claim_rejection", error))
    }

    /// Claim a retry generation: only retryable rows move, and a non-admin
    /// must already be a listener (v2 `async_claim_retry`).
    #[allow(clippy::too_many_arguments)]
    pub async fn claim_retry(
        &self,
        kind: RequestKind,
        key: &str,
        user_id: &str,
        target_status: &str,
        dispatch_authorized: bool,
        require_membership: bool,
        requested_at: u64,
        expected_generation: u64,
    ) -> Result<RetryClaim, RequestsError> {
        let stored = row_key(kind, key);
        let user_id = user_id.to_owned();
        let target = target_status.to_owned();
        self.db
            .write("requests.claim_retry", move |tx| {
                let row: Option<(String, i64)> = tx
                    .query_row(
                        "SELECT status, generation FROM request_history \
                         WHERE musicbrainz_id_lower = ?1",
                        params![stored],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                let Some((status, generation)) = row else {
                    return Ok(RetryClaim::Lost);
                };
                if !RETRYABLE_STATUSES.contains(&status.as_str())
                    || to_u64(generation) != expected_generation
                    || (require_membership && !is_member_tx(tx, &stored, &user_id)?)
                {
                    return Ok(RetryClaim::Lost);
                }
                tx.execute(
                    "UPDATE request_history SET status = ?2, dispatch_authorized = ?3, \
                     requested_at = ?4, completed_at = NULL, download_task_id = NULL, \
                     generation = generation + 1 WHERE musicbrainz_id_lower = ?1",
                    params![stored, target, dispatch_authorized, to_i64(requested_at)],
                )?;
                Ok(RetryClaim::Claimed {
                    generation: to_u64(generation).saturating_add(1),
                    target_status: target,
                })
            })
            .await
            .map_err(|error| lane_error("requests.claim_retry", error))
    }

    /// Whether one user owns or listens to a row.
    pub async fn is_requester(
        &self,
        kind: RequestKind,
        key: &str,
        user_id: &str,
    ) -> Result<bool, RequestsError> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM request_history WHERE musicbrainz_id_lower = ?1 \
             AND user_id = ?2) OR EXISTS (SELECT 1 FROM request_history_requesters \
             WHERE musicbrainz_id_lower = ?1 AND user_id = ?2)",
        )
        .bind(row_key(kind, key))
        .bind(user_id)
        .fetch_one(self.db.pool())
        .await
        .map_err(|error| read_error("requests.is_requester", error))
    }

    /// The atomic requester-cancel decision (v2
    /// `async_prepare_requester_cancel`): strangers are denied, listeners
    /// detach, waiting rows cancel outright, and owners of task rows move
    /// through `cancelling` so a failed task-cancel can restore the prior
    /// status instead of stranding the row.
    pub async fn prepare_requester_cancel(
        &self,
        kind: RequestKind,
        key: &str,
        user_id: &str,
        now: u64,
    ) -> Result<Option<CancelDecision>, RequestsError> {
        let stored = row_key(kind, key);
        let user_id = user_id.to_owned();
        self.db
            .write("requests.prepare_cancel", move |tx| {
                let Some(record) = load_record_tx(tx, &stored)? else {
                    return Ok(None);
                };
                let prior = record.status.clone();
                if !is_member_tx(tx, &stored, &user_id)? {
                    return Ok(Some(CancelDecision::Denied {
                        prior_status: prior,
                    }));
                }
                if record.user_id.as_deref() != Some(user_id.as_str()) {
                    tx.execute(
                        "DELETE FROM request_history_requesters \
                         WHERE musicbrainz_id_lower = ?1 AND user_id = ?2",
                        params![stored, user_id],
                    )?;
                    return Ok(Some(CancelDecision::Detached));
                }
                if record.status == STATUS_AWAITING_APPROVAL {
                    tx.execute(
                        "UPDATE request_history SET status = ?2, completed_at = ?3, \
                         dispatch_authorized = 0, generation = generation + 1 \
                         WHERE musicbrainz_id_lower = ?1",
                        params![stored, STATUS_CANCELLED, to_i64(now)],
                    )?;
                    return Ok(Some(CancelDecision::CancelledDirect));
                }
                if !CANCELLABLE_STATUSES.contains(&record.status.as_str()) {
                    return Ok(Some(CancelDecision::Denied {
                        prior_status: prior,
                    }));
                }
                tx.execute(
                    "UPDATE request_history SET status = ?2, generation = generation + 1 \
                     WHERE musicbrainz_id_lower = ?1",
                    params![stored, STATUS_CANCELLING],
                )?;
                Ok(Some(CancelDecision::CancelTask {
                    prior_status: prior,
                    task_id: record.task_id.clone(),
                    task_owner: record.user_id.clone().unwrap_or(user_id),
                    generation: record.generation.saturating_add(1),
                }))
            })
            .await
            .map_err(|error| lane_error("requests.prepare_cancel", error))
    }

    /// Restore a row's status after a failed cancel, but only when the row
    /// still sits in the expected status and generation.
    pub async fn restore_status(
        &self,
        kind: RequestKind,
        key: &str,
        status: &str,
        expected_status: &str,
        expected_generation: u64,
    ) -> Result<bool, RequestsError> {
        let stored = row_key(kind, key);
        let (status, expected_status) = (status.to_owned(), expected_status.to_owned());
        self.db
            .write("requests.restore_status", move |tx| {
                let changed = tx.execute(
                    "UPDATE request_history SET status = ?2, completed_at = NULL, \
                     generation = generation + 1 WHERE musicbrainz_id_lower = ?1 \
                     AND status = ?3 AND generation = ?4",
                    params![stored, status, expected_status, to_i64(expected_generation)],
                )?;
                Ok(changed > 0)
            })
            .await
            .map_err(|error| lane_error("requests.restore_status", error))
    }

    /// Revoke or restore the persisted dispatch capability.
    pub async fn set_dispatch_authorized(
        &self,
        kind: RequestKind,
        key: &str,
        value: bool,
    ) -> Result<(), RequestsError> {
        let stored = row_key(kind, key);
        self.db
            .write("requests.set_dispatch_authorized", move |tx| {
                tx.execute(
                    "UPDATE request_history SET dispatch_authorized = ?2, \
                     generation = generation + 1 WHERE musicbrainz_id_lower = ?1",
                    params![stored, value],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| lane_error("requests.set_dispatch_authorized", error))
    }

    /// Drop a row outright with its listeners and hides (admin history-clear).
    pub async fn delete(&self, kind: RequestKind, key: &str) -> Result<bool, RequestsError> {
        let stored = row_key(kind, key);
        self.db
            .write("requests.delete", move |tx| {
                tx.execute(
                    "DELETE FROM request_history_requesters WHERE musicbrainz_id_lower = ?1",
                    params![stored],
                )?;
                tx.execute(
                    "DELETE FROM request_history_dismissals WHERE musicbrainz_id_lower = ?1",
                    params![stored],
                )?;
                let changed = tx.execute(
                    "DELETE FROM request_history WHERE musicbrainz_id_lower = ?1",
                    params![stored],
                )?;
                Ok(changed > 0)
            })
            .await
            .map_err(|error| lane_error("requests.delete", error))
    }

    /// Hide a row from one user's own history (non-admin history-clear).
    pub async fn dismiss(
        &self,
        kind: RequestKind,
        key: &str,
        user_id: &str,
    ) -> Result<bool, RequestsError> {
        let stored = row_key(kind, key);
        let user_id = user_id.to_owned();
        self.db
            .write("requests.dismiss", move |tx| {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM request_history WHERE musicbrainz_id_lower = ?1)",
                    params![stored],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Ok(false);
                }
                tx.execute(
                    "INSERT OR IGNORE INTO request_history_dismissals \
                     (user_id, musicbrainz_id_lower) VALUES (?1, ?2)",
                    params![user_id, stored],
                )?;
                Ok(true)
            })
            .await
            .map_err(|error| lane_error("requests.dismiss", error))
    }

    /// Live rows, newest first, optionally for one member and one kind.
    pub async fn active(
        &self,
        user_id: Option<&str>,
        kind: Option<RequestKind>,
    ) -> Result<Vec<RequestRecord>, RequestsError> {
        let sql = format!(
            "{RECORD_SELECT} WHERE rh.status IN ('pending','downloading','queued',\
             'awaiting_approval') AND (?2 IS NULL OR rh.request_kind = ?2) AND {MEMBER_FILTER} \
             ORDER BY CAST(rh.requested_at AS INTEGER) DESC, rh.musicbrainz_id_lower"
        );
        self.fetch_records(
            "requests.active",
            sqlx::query(&sql)
                .bind(user_id.map(str::to_owned))
                .bind(kind.map(|kind| kind.as_str())),
        )
        .await
    }

    /// One page of history rows with the status filter and sort, plus the
    /// total row count (v2 history shape).
    pub async fn history(
        &self,
        user_id: Option<&str>,
        kind: Option<RequestKind>,
        status: Option<&str>,
        sort: &str,
        offset: u32,
        limit: u32,
    ) -> Result<(Vec<RequestRecord>, u32), RequestsError> {
        let filter = format!(
            "WHERE (?2 IS NULL OR rh.request_kind = ?2) AND (?3 IS NULL OR rh.status = ?3) \
             AND {MEMBER_FILTER}"
        );
        let order = match sort {
            "oldest" => "CAST(rh.requested_at AS INTEGER) ASC",
            "status" => "rh.status ASC, CAST(rh.requested_at AS INTEGER) DESC",
            _ => "CAST(rh.requested_at AS INTEGER) DESC",
        };
        let user = user_id.map(str::to_owned);
        let kind = kind.map(|kind| kind.as_str());
        let total: i64 =
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM request_history rh {filter}"))
                .bind(&user)
                .bind(kind)
                .bind(status)
                .fetch_one(self.db.pool())
                .await
                .map_err(|error| read_error("requests.history", error))?;
        let sql = format!(
            "{RECORD_SELECT} {filter} ORDER BY {order}, rh.musicbrainz_id_lower LIMIT ?4 OFFSET ?5"
        );
        let rows = self
            .fetch_records(
                "requests.history",
                sqlx::query(&sql)
                    .bind(&user)
                    .bind(kind)
                    .bind(status)
                    .bind(i64::from(limit))
                    .bind(i64::from(offset)),
            )
            .await?;
        Ok((rows, u32::try_from(total).unwrap_or(u32::MAX)))
    }

    /// Rows waiting for review, oldest first.
    pub async fn pending_approvals(&self) -> Result<Vec<RequestRecord>, RequestsError> {
        let sql = format!(
            "{RECORD_SELECT} WHERE rh.status = ?1 \
             ORDER BY CAST(rh.requested_at AS INTEGER) ASC, rh.musicbrainz_id_lower"
        );
        self.fetch_records(
            "requests.pending_approvals",
            sqlx::query(&sql).bind(STATUS_AWAITING_APPROVAL),
        )
        .await
    }

    /// Rows of one kind in one status, oldest first (wanted enrolment).
    pub async fn with_status(
        &self,
        kind: RequestKind,
        status: &str,
    ) -> Result<Vec<RequestRecord>, RequestsError> {
        let sql = format!(
            "{RECORD_SELECT} WHERE rh.status = ?1 AND rh.request_kind = ?2 \
             ORDER BY CAST(rh.requested_at AS INTEGER) ASC, rh.musicbrainz_id_lower"
        );
        self.fetch_records(
            "requests.with_status",
            sqlx::query(&sql)
                .bind(status.to_owned())
                .bind(kind.as_str()),
        )
        .await
    }

    /// Asks one listener made since `since` (quota usage).
    pub async fn count_asks_since(&self, user_id: &str, since: u64) -> Result<u32, RequestsError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM request_history_requesters rr \
             JOIN request_history rh ON rh.musicbrainz_id_lower = rr.musicbrainz_id_lower \
             WHERE rr.user_id = ?1 AND CAST(rr.requested_at AS INTEGER) >= ?2",
        )
        .bind(user_id)
        .bind(to_i64(since))
        .fetch_one(self.db.pool())
        .await
        .map_err(|error| read_error("requests.count_asks", error))?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    /// Run one record query and attach listeners to every row.
    async fn fetch_records<'q>(
        &self,
        operation: &'static str,
        query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    ) -> Result<Vec<RequestRecord>, RequestsError> {
        let rows = query
            .fetch_all(self.db.pool())
            .await
            .map_err(|error| read_error(operation, error))?;
        let mut records = rows
            .iter()
            .map(record_from_sqlx)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| read_error(operation, error))?;
        self.attach_listeners(&mut records).await?;
        Ok(records)
    }

    /// Fill each record's listeners (owner excluded) in one query.
    async fn attach_listeners(&self, records: &mut [RequestRecord]) -> Result<(), RequestsError> {
        if records.is_empty() {
            return Ok(());
        }
        let keys: Vec<String> = records
            .iter()
            .map(|record| row_key(record.kind, &record.key))
            .collect();
        let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
            "SELECT musicbrainz_id_lower, user_id, requested_by_name \
             FROM request_history_requesters WHERE musicbrainz_id_lower IN (",
        );
        let mut separated = builder.separated(", ");
        for key in &keys {
            separated.push_bind(key.clone());
        }
        builder.push(") ORDER BY requested_at, user_id");
        let rows = builder
            .build()
            .fetch_all(self.db.pool())
            .await
            .map_err(|error| read_error("requests.listeners", error))?;
        let mut by_key: HashMap<String, Vec<Requester>> = HashMap::new();
        for row in rows {
            let stored: String = row
                .try_get(0)
                .map_err(|error| read_error("requests.listeners", error))?;
            let requester = Requester {
                user_id: row
                    .try_get(1)
                    .map_err(|error| read_error("requests.listeners", error))?,
                name: row
                    .try_get(2)
                    .map_err(|error| read_error("requests.listeners", error))?,
            };
            by_key.entry(stored).or_default().push(requester);
        }
        for (record, key) in records.iter_mut().zip(keys) {
            if let Some(listeners) = by_key.remove(&key) {
                record.requesters = listeners
                    .into_iter()
                    .filter(|listener| record.user_id.as_deref() != Some(listener.user_id.as_str()))
                    .collect();
            }
        }
        Ok(())
    }
}

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

/// v2 stores watch times as REAL epoch seconds.
fn epoch_from_real(value: f64) -> u64 {
    if value.is_finite() && value > 0.0 {
        value as u64
    } else {
        0
    }
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

/// Approval states.
const APPROVAL_PENDING: &str = "pending";
const APPROVAL_APPROVED: &str = "approved";
const APPROVAL_REVOKED: &str = "revoked";

/// Durable auto-download approvals plus import batches over
/// `auto_download_approvals` (v2 `follow_store` approval half). Batch
/// members are approval rows sharing a `batch_id`.
#[derive(Clone)]
pub struct FollowApprovalStore {
    db: AcquireDb,
}

/// One pending approval or batch row as the queries read it.
type ApprovalRow = (
    String,
    Option<String>,
    String,
    String,
    String,
    f64,
    Option<String>,
);

impl FollowApprovalStore {
    /// Approvals over one database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// File (or refresh) one pending approval (v2 `upsert_approval`).
    pub async fn file_pending(
        &self,
        user_id: &str,
        artist_mbid: &str,
        artist_name: &str,
        now: u64,
    ) -> Result<(), RequestsError> {
        let (user_id, artist_mbid, artist_name) = (
            user_id.to_owned(),
            artist_mbid.to_owned(),
            artist_name.to_owned(),
        );
        self.db
            .write("approvals.file", move |tx| {
                tx.execute(
                    "INSERT INTO auto_download_approvals (user_id, artist_mbid, \
                     artist_mbid_lower, artist_name, state, requested_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                     ON CONFLICT (user_id, artist_mbid_lower) DO UPDATE SET \
                     artist_name = excluded.artist_name, state = excluded.state, \
                     requested_at = excluded.requested_at, reviewed_by_id = NULL, \
                     reviewed_by_name = NULL, reviewed_at = NULL, batch_id = NULL",
                    params![
                        user_id,
                        artist_mbid,
                        artist_mbid.to_lowercase(),
                        artist_name,
                        APPROVAL_PENDING,
                        now as f64
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| lane_error("approvals.file", error))
    }

    /// Open one import batch over several artists (v2
    /// `create_import_approval_batch`). Never downgrades an approved row.
    pub async fn create_batch(
        &self,
        batch_id: &str,
        user_id: &str,
        artists: &[(String, String)],
        source: &str,
        now: u64,
    ) -> Result<(), RequestsError> {
        let (batch_id, user_id, source) =
            (batch_id.to_owned(), user_id.to_owned(), source.to_owned());
        let artists = artists.to_vec();
        self.db
            .write("approvals.batch", move |tx| {
                for (mbid, name) in &artists {
                    tx.execute(
                        "INSERT INTO auto_download_approvals (user_id, artist_mbid, \
                         artist_mbid_lower, artist_name, state, requested_at, batch_id, source) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                         ON CONFLICT (user_id, artist_mbid_lower) DO UPDATE SET \
                         artist_name = excluded.artist_name, state = excluded.state, \
                         requested_at = excluded.requested_at, reviewed_by_id = NULL, \
                         reviewed_by_name = NULL, reviewed_at = NULL, \
                         batch_id = excluded.batch_id, source = excluded.source \
                         WHERE auto_download_approvals.state != ?9",
                        params![
                            user_id,
                            mbid,
                            mbid.to_lowercase(),
                            name,
                            APPROVAL_PENDING,
                            now as f64,
                            batch_id,
                            source,
                            APPROVAL_APPROVED
                        ],
                    )?;
                }
                Ok(())
            })
            .await
            .map_err(|error| lane_error("approvals.batch", error))
    }

    /// Pending single approvals (not batch members), oldest first.
    pub async fn pending(&self) -> Result<Vec<FollowApproval>, RequestsError> {
        let rows = self.pending_rows(false).await?;
        Ok(rows
            .into_iter()
            .map(
                |(user_id, user_name, artist_mbid, artist_name, state, requested_at, _)| {
                    FollowApproval {
                        user_name: user_name.unwrap_or_else(|| user_id.clone()),
                        user_id,
                        artist_mbid,
                        artist_name,
                        state,
                        requested_at: epoch_from_real(requested_at),
                    }
                },
            )
            .collect())
    }

    /// Pending batches grouped by batch and user, oldest first (v2
    /// `list_pending_approval_batches`).
    pub async fn pending_batches(&self) -> Result<Vec<ApprovalBatch>, RequestsError> {
        let rows = self.pending_rows(true).await?;
        let mut batches: Vec<ApprovalBatch> = Vec::new();
        for (user_id, user_name, artist_mbid, artist_name, state, requested_at, batch_id) in rows {
            let batch_id = batch_id.unwrap_or_default();
            let requested_at = epoch_from_real(requested_at);
            if let Some(batch) = batches
                .iter_mut()
                .find(|batch| batch.batch_id == batch_id && batch.user_id == user_id)
            {
                batch.artists.push((artist_mbid, artist_name));
                batch.requested_at = batch.requested_at.min(requested_at);
                continue;
            }
            batches.push(ApprovalBatch {
                batch_id,
                user_name: user_name.unwrap_or_else(|| user_id.clone()),
                user_id,
                artists: vec![(artist_mbid, artist_name)],
                state,
                requested_at,
            });
        }
        batches.sort_by(|a, b| a.requested_at.cmp(&b.requested_at));
        Ok(batches)
    }

    /// Pending units for the badge: single approvals plus one unit per
    /// pending batch (v2 `count_pending_approval_units`).
    pub async fn pending_units(&self) -> Result<u32, RequestsError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM auto_download_approvals \
             WHERE state = 'pending' AND batch_id IS NULL) + \
             (SELECT COUNT(*) FROM (SELECT batch_id, user_id FROM auto_download_approvals \
             WHERE state = 'pending' AND batch_id IS NOT NULL GROUP BY batch_id, user_id))",
        )
        .fetch_one(self.db.pool())
        .await
        .map_err(|error| read_error("approvals.units", error))?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    /// Move one pending approval to `state`, stamping the reviewer. Only a
    /// pending row moves (v2 `set_approval_state` behind the pending read).
    pub async fn decide(
        &self,
        user_id: &str,
        artist_mbid: &str,
        state: &str,
        reviewer: (&str, Option<String>),
        now: u64,
    ) -> Result<Option<FollowApproval>, RequestsError> {
        self.transition(
            user_id,
            artist_mbid,
            APPROVAL_PENDING,
            state,
            reviewer,
            now,
            "approvals.decide",
        )
        .await
    }

    /// Revoke a prior grant. Only approved rows revoke.
    pub async fn revoke(
        &self,
        user_id: &str,
        artist_mbid: &str,
        reviewer: (&str, Option<String>),
        now: u64,
    ) -> Result<Option<FollowApproval>, RequestsError> {
        self.transition(
            user_id,
            artist_mbid,
            APPROVAL_APPROVED,
            APPROVAL_REVOKED,
            reviewer,
            now,
            "approvals.revoke",
        )
        .await
    }

    /// Withdraw a pending ask (the user turned auto-download back off).
    pub async fn withdraw(&self, user_id: &str, artist_mbid: &str) -> Result<bool, RequestsError> {
        let (user_id, mbid_lower) = (user_id.to_owned(), artist_mbid.to_lowercase());
        self.db
            .write("approvals.withdraw", move |tx| {
                let changed = tx.execute(
                    "DELETE FROM auto_download_approvals WHERE user_id = ?1 \
                     AND artist_mbid_lower = ?2 AND state = ?3",
                    params![user_id, mbid_lower, APPROVAL_PENDING],
                )?;
                Ok(changed > 0)
            })
            .await
            .map_err(|error| lane_error("approvals.withdraw", error))
    }

    /// Decide every still-pending row of one batch; returns the moved rows
    /// as `(user_id, artist_mbid, artist_name)` (v2 `set_batch_approval_state`).
    pub async fn decide_batch(
        &self,
        batch_id: &str,
        state: &str,
        reviewer: (&str, Option<String>),
        now: u64,
    ) -> Result<Vec<(String, String, String)>, RequestsError> {
        let (batch_id, state) = (batch_id.to_owned(), state.to_owned());
        let (reviewer_id, reviewer_name) = (reviewer.0.to_owned(), reviewer.1);
        self.db
            .write("approvals.decide_batch", move |tx| {
                let mut statement = tx.prepare(
                    "SELECT user_id, artist_mbid, artist_name FROM auto_download_approvals \
                     WHERE batch_id = ?1 AND state = ?2 ORDER BY artist_name",
                )?;
                let rows = statement
                    .query_map(params![batch_id, APPROVAL_PENDING], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })?
                    .collect::<rusqlite::Result<Vec<(String, String, String)>>>()?;
                drop(statement);
                tx.execute(
                    "UPDATE auto_download_approvals SET state = ?2, reviewed_by_id = ?3, \
                     reviewed_by_name = ?4, reviewed_at = ?5 WHERE batch_id = ?1 AND state = ?6",
                    params![
                        batch_id,
                        state,
                        reviewer_id,
                        reviewer_name,
                        now as f64,
                        APPROVAL_PENDING
                    ],
                )?;
                Ok(rows)
            })
            .await
            .map_err(|error| lane_error("approvals.decide_batch", error))
    }

    /// Guarded state move for one row.
    #[allow(clippy::too_many_arguments)]
    async fn transition(
        &self,
        user_id: &str,
        artist_mbid: &str,
        from: &'static str,
        to: &str,
        reviewer: (&str, Option<String>),
        now: u64,
        operation: &'static str,
    ) -> Result<Option<FollowApproval>, RequestsError> {
        let (user_id, mbid_lower, to) = (
            user_id.to_owned(),
            artist_mbid.to_lowercase(),
            to.to_owned(),
        );
        let (reviewer_id, reviewer_name) = (reviewer.0.to_owned(), reviewer.1);
        self.db
            .write(operation, move |tx| {
                let row: Option<(String, String, Option<String>, f64)> = tx
                    .query_row(
                        "SELECT a.artist_mbid, a.artist_name, u.display_name, a.requested_at \
                         FROM auto_download_approvals a LEFT JOIN auth_users u ON u.id = a.user_id \
                         WHERE a.user_id = ?1 AND a.artist_mbid_lower = ?2 AND a.state = ?3",
                        params![user_id, mbid_lower, from],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .optional()?;
                let Some((artist_mbid, artist_name, user_name, requested_at)) = row else {
                    return Ok(None);
                };
                tx.execute(
                    "UPDATE auto_download_approvals SET state = ?3, reviewed_by_id = ?4, \
                     reviewed_by_name = ?5, reviewed_at = ?6 \
                     WHERE user_id = ?1 AND artist_mbid_lower = ?2",
                    params![
                        user_id,
                        mbid_lower,
                        to,
                        reviewer_id,
                        reviewer_name,
                        now as f64
                    ],
                )?;
                Ok(Some(FollowApproval {
                    user_name: user_name.unwrap_or_else(|| user_id.clone()),
                    user_id,
                    artist_mbid,
                    artist_name,
                    state: to,
                    requested_at: epoch_from_real(requested_at),
                }))
            })
            .await
            .map_err(|error| lane_error(operation, error))
    }

    /// Pending rows, singles or batch members, oldest first.
    async fn pending_rows(&self, batched: bool) -> Result<Vec<ApprovalRow>, RequestsError> {
        sqlx::query_as(
            "SELECT a.user_id, u.display_name, a.artist_mbid, a.artist_name, a.state, \
             a.requested_at, a.batch_id FROM auto_download_approvals a \
             LEFT JOIN auth_users u ON u.id = a.user_id \
             WHERE a.state = 'pending' AND (a.batch_id IS NOT NULL) = ?1 \
             ORDER BY a.requested_at ASC, a.artist_name ASC",
        )
        .bind(batched)
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| read_error("approvals.pending", error))
    }
}

/// Durable personal-mix approvals over `personal_mix_approvals`, plus the
/// in-process refresh guard (a build running in this process).
pub struct PersonalMixStore {
    db: AcquireDb,
    refresh_running: Mutex<HashSet<String>>,
    #[cfg(any(test, feature = "test-support"))]
    unlinked: std::sync::RwLock<HashSet<String>>,
}

impl PersonalMixStore {
    /// Mix approvals over one database.
    pub fn new(db: AcquireDb) -> Self {
        Self {
            db,
            refresh_running: Mutex::new(HashSet::new()),
            #[cfg(any(test, feature = "test-support"))]
            unlinked: std::sync::RwLock::new(HashSet::new()),
        }
    }

    /// File (or refresh) one pending approval.
    pub async fn file_pending(&self, user_id: &str, now: u64) -> Result<(), RequestsError> {
        let user_id = user_id.to_owned();
        self.db
            .write("mix.file", move |tx| {
                tx.execute(
                    "INSERT INTO personal_mix_approvals (user_id, state, requested_at) \
                     VALUES (?1, ?2, ?3) ON CONFLICT (user_id) DO UPDATE SET \
                     state = excluded.state, requested_at = excluded.requested_at, \
                     reviewed_by_id = NULL, reviewed_by_name = NULL, reviewed_at = NULL",
                    params![user_id, APPROVAL_PENDING, now as f64],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| lane_error("mix.file", error))
    }

    /// Mark one user unlinked (fixtures only).
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_unlinked(&self, user_id: &str) {
        if let Ok(mut unlinked) = self.unlinked.write() {
            unlinked.insert(user_id.to_owned());
        }
    }

    /// Whether one user may build a mix. Linking lives with the
    /// ListenBrainz connection; every user reads as linked until that port
    /// arrives.
    pub fn is_linked(&self, user_id: &str) -> bool {
        #[cfg(any(test, feature = "test-support"))]
        if let Ok(unlinked) = self.unlinked.read()
            && unlinked.contains(user_id)
        {
            return false;
        }
        let _ = user_id;
        true
    }

    /// Pending approvals, oldest first.
    pub async fn pending(&self) -> Result<Vec<MixApproval>, RequestsError> {
        let rows: Vec<(String, Option<String>, String, f64)> = sqlx::query_as(
            "SELECT m.user_id, u.display_name, m.state, m.requested_at \
             FROM personal_mix_approvals m LEFT JOIN auth_users u ON u.id = m.user_id \
             WHERE m.state = 'pending' ORDER BY m.requested_at ASC, m.user_id",
        )
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| read_error("mix.pending", error))?;
        Ok(rows
            .into_iter()
            .map(|(user_id, user_name, state, requested_at)| MixApproval {
                user_name: user_name.unwrap_or_else(|| user_id.clone()),
                user_id,
                state,
                requested_at: epoch_from_real(requested_at),
            })
            .collect())
    }

    /// Pending approval count for the badge.
    pub async fn pending_count(&self) -> Result<u32, RequestsError> {
        Ok(u32::try_from(self.pending().await?.len()).unwrap_or(u32::MAX))
    }

    /// Move one pending approval to `state`.
    pub async fn decide(
        &self,
        user_id: &str,
        state: &str,
        reviewer: (&str, Option<String>),
        now: u64,
    ) -> Result<bool, RequestsError> {
        self.transition(user_id, APPROVAL_PENDING, state, reviewer, now)
            .await
    }

    /// Revoke a prior grant. Only approved rows revoke.
    pub async fn revoke(
        &self,
        user_id: &str,
        reviewer: (&str, Option<String>),
        now: u64,
    ) -> Result<bool, RequestsError> {
        self.transition(user_id, APPROVAL_APPROVED, APPROVAL_REVOKED, reviewer, now)
            .await
    }

    /// Claim the refresh key. False means a build already holds it.
    pub fn refresh_start(&self, user_id: &str) -> Result<bool, RequestsError> {
        let mut running = self.refresh_running.lock().map_err(|cause| {
            RequestsError::internal(&format_args!("mix refresh lock failed: {cause}"))
        })?;
        Ok(running.insert(user_id.to_owned()))
    }

    /// Release the refresh key once the build lands.
    pub fn refresh_finish(&self, user_id: &str) -> Result<(), RequestsError> {
        let mut running = self.refresh_running.lock().map_err(|cause| {
            RequestsError::internal(&format_args!("mix refresh lock failed: {cause}"))
        })?;
        running.remove(user_id);
        Ok(())
    }

    async fn transition(
        &self,
        user_id: &str,
        from: &'static str,
        to: &str,
        reviewer: (&str, Option<String>),
        now: u64,
    ) -> Result<bool, RequestsError> {
        let (user_id, to) = (user_id.to_owned(), to.to_owned());
        let (reviewer_id, reviewer_name) = (reviewer.0.to_owned(), reviewer.1);
        self.db
            .write("mix.transition", move |tx| {
                let changed = tx.execute(
                    "UPDATE personal_mix_approvals SET state = ?2, reviewed_by_id = ?3, \
                     reviewed_by_name = ?4, reviewed_at = ?5 WHERE user_id = ?1 AND state = ?6",
                    params![user_id, to, reviewer_id, reviewer_name, now as f64, from],
                )?;
                Ok(changed > 0)
            })
            .await
            .map_err(|error| lane_error("mix.transition", error))
    }
}

/// Durable in-flight edition acquires.
#[derive(Clone)]
pub struct EditionStore {
    db: AcquireDb,
}

impl EditionStore {
    /// Marks over one database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// Read one mark.
    pub async fn get(&self, key: &str) -> Result<Option<EditionMark>, RequestsError> {
        let row: Option<(String, String, i64)> = sqlx::query_as(
            "SELECT release_group_mbid_lower, task_id, started_at \
             FROM acquire_edition_acquires WHERE release_group_mbid_lower = ?1",
        )
        .bind(key.to_lowercase())
        .fetch_optional(self.db.pool())
        .await
        .map_err(|error| read_error("editions.get", error))?;
        Ok(row.map(|(key, task_id, started_at)| EditionMark {
            key,
            task_id,
            started_at: to_u64(started_at),
        }))
    }

    /// Set one mark.
    pub async fn set(&self, mark: EditionMark) -> Result<(), RequestsError> {
        self.db
            .write("editions.set", move |tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO acquire_edition_acquires \
                     (release_group_mbid_lower, task_id, started_at) VALUES (?1, ?2, ?3)",
                    params![
                        mark.key.to_lowercase(),
                        mark.task_id,
                        to_i64(mark.started_at)
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| lane_error("editions.set", error))
    }

    /// Clear one mark.
    pub async fn clear(&self, key: &str) -> Result<(), RequestsError> {
        let key = key.to_lowercase();
        self.db
            .write("editions.clear", move |tx| {
                tx.execute(
                    "DELETE FROM acquire_edition_acquires WHERE release_group_mbid_lower = ?1",
                    params![key],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| lane_error("editions.clear", error))
    }
}
