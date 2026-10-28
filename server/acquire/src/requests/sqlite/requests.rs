//! The request ledger: `request_history`, its requesters and dismissals.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, Params, Transaction, params, params_from_iter};

use super::super::error::RequestsError;
use super::super::ledger::{
    BeginOutcome, CancelDecision, QuotaGate, QuotaRefusal, RequestRecord, RequestStatus, Requester,
    RetryClaim,
};
use super::super::models::RequestKind;
use super::super::settle::{self, Fulfilled};
use super::super::sql;
use super::{lane_error, ms_from_secs, read, secs_from_ms, unknown_value};
use crate::db::{AcquireDb, to_i64, to_u64};
use crate::dispatch::DispatchOrigin;
use crate::follows::FollowAsk;
use droppedneedle_auth::Role;
use droppedneedle_kernel::db::statement::ListLen;

droppedneedle_kernel::sql_choice! {
    /// The history list's sort.
    pub(crate) enum HistoryOrder {
        /// Newest ask first.
        Newest,
        /// Oldest ask first.
        Oldest,
        /// By status, newest first within one.
        Status,
    }
}

impl HistoryOrder {
    /// The sort a history request names; anything else is newest first.
    fn from_param(sort: &str) -> Self {
        match sort {
            "oldest" => Self::Oldest,
            "status" => Self::Status,
            _ => Self::Newest,
        }
    }
}

/// Ledger key for one row.
fn row_key(kind: RequestKind, key: &str) -> String {
    let lower = key.trim().to_lowercase();
    match kind {
        RequestKind::Album => lower,
        RequestKind::Track => format!("track:{lower}"),
    }
}

/// The bare MBID of a ledger key.
fn bare_key(stored: &str) -> String {
    stored.strip_prefix("track:").unwrap_or(stored).to_owned()
}

/// Decode one record selected by `RECORD_SELECT` (requesters attach
/// later). A kind, status or origin outside its set fails the read.
fn record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RequestRecord> {
    let stored: String = row.get(0)?;
    let origin: String = row.get(22)?;
    let origin = DispatchOrigin::parse(&origin)
        .ok_or_else(|| unknown_value(22, "request_history.origin", &origin))?;
    Ok(RequestRecord {
        origin,
        key: bare_key(&stored),
        kind: row.get(1)?,
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
        requested_at: secs_from_ms(row.get(13)?),
        completed_at: row.get::<_, Option<i64>>(14)?.map(secs_from_ms),
        task_id: row.get(15)?,
        generation: to_u64(row.get(16)?),
        dispatch_authorized: row.get::<_, i64>(17)? != 0,
        monitor_artist: row.get::<_, i64>(18)? != 0,
        auto_download_artist: row.get::<_, i64>(19)? != 0,
        reviewed_by_name: row.get(20)?,
        reviewed_at: row.get::<_, Option<i64>>(21)?.map(secs_from_ms),
    })
}

/// One full record inside a write transaction, listeners included.
fn load_record_tx(tx: &Transaction, stored: &str) -> rusqlite::Result<Option<RequestRecord>> {
    let Some(mut record) = tx
        .query_row(sql::RECORD_BY_KEY.text(), params![stored], record_from_row)
        .optional()?
    else {
        return Ok(None);
    };
    let mut statement = tx.prepare_cached(sql::ROW_LISTENERS.text())?;
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

/// Asks one listener made since `since`, on a pooled read or inside a write.
/// A row still waiting for quota room has used none yet.
fn count_asks_on(conn: &Connection, user_id: &str, since: u64) -> rusqlite::Result<u32> {
    let count: i64 = conn.query_row(
        sql::COUNT_ASKS.text(),
        params![user_id, ms_from_secs(since)],
        |row| row.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Asks one listener made since `since`, inside a write transaction.
fn count_asks_tx(tx: &Transaction, user_id: &str, since: u64) -> rusqlite::Result<u32> {
    count_asks_on(tx, user_id, since)
}

/// Whether `owner` may make one more ask at `now` under `gate`.
fn has_room(
    tx: &Transaction,
    owner: &str,
    gate: Option<&QuotaGate>,
    now: u64,
) -> rusqlite::Result<bool> {
    let Some(gate) = gate.filter(|gate| gate.limit > 0) else {
        return Ok(true);
    };
    let since = now.saturating_sub(u64::from(gate.window_days.max(1)) * 86_400);
    let used = count_asks_tx(tx, owner, since)?;
    Ok(used.saturating_add(gate.new_requests) <= gate.limit)
}

/// Whether one user owns or listens to a row, inside a write transaction.
fn is_member_tx(tx: &Transaction, stored: &str, user_id: &str) -> rusqlite::Result<bool> {
    tx.query_row(sql::IS_MEMBER.text(), params![stored, user_id], |row| {
        row.get(0)
    })
}

/// Claim one generation for a record inside a write transaction. A
/// live or cancelling row wins over the
/// newcomer; a terminal row is replaced, and its listeners and dismissals
/// go with it.
fn begin_tx(tx: &Transaction, mut record: RequestRecord) -> rusqlite::Result<BeginOutcome> {
    let stored = row_key(record.kind, &record.key);
    let existing: Option<(RequestStatus, i64)> = tx
        .query_row(sql::ROW_STATE.text(), params![stored], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    if let Some((status, _)) = &existing
        && (status.is_active() || *status == RequestStatus::Cancelling)
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
        tx.execute(sql::DELETE_ROW.text(), params![stored])?;
    }
    tx.execute(
        sql::INSERT_ROW.text(),
        params![
            stored,
            record.key,
            record.artist_name,
            record.album_title,
            record.artist_mbid,
            record.year,
            ms_from_secs(record.requested_at),
            record.completed_at.map(ms_from_secs),
            record.status,
            record.monitor_artist,
            record.auto_download_artist,
            record.user_id,
            record.requested_by_name,
            record.reviewed_by_name,
            record.reviewed_at.map(ms_from_secs),
            record.task_id,
            record.release_mbid,
            record.kind,
            record.track_title,
            record.duration_seconds,
            record.track_release_group_mbid,
            record.dispatch_authorized,
            generation,
            record.origin.as_str(),
        ],
    )?;
    if let Some(owner) = &record.user_id {
        tx.execute(
            sql::INSERT_OWNER.text(),
            params![
                owner,
                stored,
                ms_from_secs(record.requested_at),
                record.requested_by_name
            ],
        )?;
    }
    Ok(BeginOutcome::Won(record))
}

/// Attach a listener to a live row. Terminal
/// and cancelling rows refuse; the generation is untouched.
fn attach_tx(
    tx: &Transaction,
    stored: &str,
    user_id: &str,
    name: Option<&str>,
    now: u64,
) -> rusqlite::Result<bool> {
    let status: Option<RequestStatus> = tx
        .query_row(sql::ROW_STATUS.text(), params![stored], |row| row.get(0))
        .optional()?;
    if !status.is_some_and(RequestStatus::is_active) {
        return Ok(false);
    }
    tx.execute(
        sql::ATTACH_LISTENER.text(),
        params![user_id, stored, ms_from_secs(now), name],
    )?;
    Ok(true)
}

/// Optional generation guard as SQL: NULL matches any generation.
fn guard(expected: Option<u64>) -> Option<i64> {
    expected.map(to_i64)
}

/// Run one record statement and attach listeners to every row, on one
/// connection so the rows and their listeners agree.
fn read_records(
    conn: &Connection,
    text: &str,
    values: impl Params,
) -> rusqlite::Result<Vec<RequestRecord>> {
    let mut statement = conn.prepare_cached(text)?;
    let mut records = statement
        .query_map(values, record_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    attach_listeners(conn, &mut records)?;
    Ok(records)
}

/// Fill each record's listeners (owner excluded) in one query.
fn attach_listeners(conn: &Connection, records: &mut [RequestRecord]) -> rusqlite::Result<()> {
    if records.is_empty() {
        return Ok(());
    }
    let keys: Vec<String> = records
        .iter()
        .map(|record| row_key(record.kind, &record.key))
        .collect();
    let mut statement = conn.prepare(&sql::listeners_in(ListLen::new(keys.len())))?;
    let rows = statement.query_map(params_from_iter(&keys), |row| {
        Ok((
            row.get::<_, String>(0)?,
            Requester {
                user_id: row.get(1)?,
                name: row.get(2)?,
            },
        ))
    })?;
    let mut by_key: HashMap<String, Vec<Requester>> = HashMap::new();
    for row in rows {
        let (stored, requester) = row?;
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
        let stored = row_key(kind, key);
        read(&self.db, "requests.get", move |conn| {
            Ok(read_records(conn, sql::RECORD_BY_KEY.text(), params![stored])?.pop())
        })
        .await
    }

    /// Lowercased MBIDs with a live row of one kind (the
    /// batch dedupe read).
    pub async fn active_mbids(&self, kind: RequestKind) -> Result<HashSet<String>, RequestsError> {
        let rows: Vec<String> = read(&self.db, "requests.active_mbids", move |conn| {
            let mut statement = conn.prepare_cached(sql::ACTIVE_KEYS.text())?;
            let rows = statement.query_map(params![kind], |row| row.get(0))?;
            rows.collect()
        })
        .await?;
        Ok(rows
            .into_iter()
            .map(|stored| stored.strip_prefix("track:").unwrap_or(&stored).to_owned())
            .collect())
    }

    /// Claim one row, enforcing the quota gate inside the same write. A
    /// won row writes the follow it asks for in the same write.
    pub async fn begin(
        &self,
        record: RequestRecord,
        owner: &str,
        gate: Option<QuotaGate>,
        follow: Option<FollowAsk>,
    ) -> Result<Result<BeginOutcome, QuotaRefusal>, RequestsError> {
        match self
            .begin_following(vec![(record, follow)], owner, gate)
            .await?
        {
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
        let records = records.into_iter().map(|record| (record, None)).collect();
        self.begin_following(records, owner, gate).await
    }

    /// [`Self::begin_batch`] where each row may carry the follow its ask
    /// names: a won row follows for `owner` in the same write, so a
    /// refused or lost claim follows nobody.
    pub async fn begin_following(
        &self,
        records: Vec<(RequestRecord, Option<FollowAsk>)>,
        owner: &str,
        gate: Option<QuotaGate>,
    ) -> Result<Result<Vec<BeginOutcome>, QuotaRefusal>, RequestsError> {
        let owner = owner.to_owned();
        let now = records.first().map_or(0, |(record, _)| record.requested_at);
        let clock = self.db.clock().clone();
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
                for (record, follow) in records {
                    let outcome = begin_tx(tx, record)?;
                    if let (BeginOutcome::Won(_), Some(follow)) = (&outcome, follow) {
                        follow.apply(tx, &owner, crate::follows::now_ms(clock.as_ref()))?;
                    }
                    outcomes.push(outcome);
                }
                Ok(Ok(outcomes))
            })
            .await
            .map_err(|error| lane_error("requests.begin", error))
    }

    /// Claim one row that never refuses for quota: when `hold` is set or
    /// the gate has no room, the row is stored queued with no task and
    /// waits for room (see [`Self::admit_waiting`]).
    pub async fn begin_or_wait(
        &self,
        mut record: RequestRecord,
        owner: &str,
        gate: Option<QuotaGate>,
        hold: bool,
    ) -> Result<BeginOutcome, RequestsError> {
        let owner = owner.to_owned();
        self.db
            .write("requests.begin", move |tx| {
                if hold || !has_room(tx, &owner, gate.as_ref(), record.requested_at)? {
                    record.status = RequestStatus::Queued;
                }
                Ok(begin_tx(tx, record)?)
            })
            .await
            .map_err(|error| lane_error("requests.begin", error))
    }

    /// Start one row that waits for quota room when `asker`'s gate has
    /// room now: it moves to pending, and the asks of `asker` and of the
    /// row's owner count from `now`. Answers the started row, or `None`
    /// when it still waits or no longer waits for quota. Runs on the
    /// background lane: the follow poll calls it for waiting rows.
    pub async fn admit_waiting(
        &self,
        key: &str,
        asker: &str,
        gate: Option<QuotaGate>,
        now: u64,
    ) -> Result<Option<RequestRecord>, RequestsError> {
        let stored = row_key(RequestKind::Album, key);
        let asker = asker.to_owned();
        self.db
            .write_background("requests.admit_waiting", move |tx| {
                if !has_room(tx, &asker, gate.as_ref(), now)? {
                    return Ok(None);
                }
                let moved = tx.execute(sql::START_WAITING.text(), params![stored])?;
                if moved == 0 {
                    return Ok(None);
                }
                tx.execute(
                    sql::RESTAMP_ASKS.text(),
                    params![stored, asker, ms_from_secs(now)],
                )?;
                Ok(load_record_tx(tx, &stored)?)
            })
            .await
            .map_err(|error| lane_error("requests.admit_waiting", error))
    }

    /// Put a row started from waiting back to waiting when its download
    /// could not be queued, behind the generation guard. Answers whether
    /// it moved.
    pub async fn requeue_waiting(
        &self,
        key: &str,
        expected_generation: u64,
    ) -> Result<bool, RequestsError> {
        let stored = row_key(RequestKind::Album, key);
        let expected = to_i64(expected_generation);
        self.db
            .write_background("requests.requeue_waiting", move |tx| {
                let moved = tx.execute(sql::REQUEUE_WAITING.text(), params![stored, expected])?;
                Ok(moved > 0)
            })
            .await
            .map_err(|error| lane_error("requests.requeue_waiting", error))
    }

    /// Rows waiting for quota room, oldest first, with their owner's role.
    /// The role is `None` for a row whose owner no longer exists.
    pub async fn waiting_for_quota(
        &self,
    ) -> Result<Vec<(RequestRecord, Option<Role>)>, RequestsError> {
        let records = read(&self.db, "requests.waiting_for_quota", |conn| {
            read_records(conn, sql::WAITING_FOR_QUOTA.text(), [])
        })
        .await?;
        let mut out = Vec::with_capacity(records.len());
        for record in records {
            let role = match record.user_id.as_deref() {
                Some(owner) => self.role_of(owner).await?,
                None => None,
            };
            out.push((record, role));
        }
        Ok(out)
    }

    /// One account's role, or `None` when the account does not exist.
    pub async fn role_of(&self, user_id: &str) -> Result<Option<Role>, RequestsError> {
        let user_id = user_id.to_owned();
        let role: Option<String> = read(&self.db, "requests.role_of", move |conn| {
            conn.query_row(sql::ROLE_OF.text(), params![user_id], |row| row.get(0))
                .optional()
        })
        .await?;
        Ok(role.as_deref().map(Role::from_stored))
    }

    /// Attach a listener to a live row. Terminal
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
                Ok(attach_tx(tx, &stored, &user_id, name.as_deref(), now)?)
            })
            .await
            .map_err(|error| lane_error("requests.attach", error))
    }

    /// Attach a listener to a live album row with the follow its ask
    /// names, in one write: the row's monitoring flags widen (never
    /// narrow) and the listener follows the
    /// artist. A row that refuses the listener follows nobody.
    pub async fn attach_following(
        &self,
        key: &str,
        user_id: &str,
        name: Option<String>,
        now: u64,
        follow: Option<FollowAsk>,
    ) -> Result<bool, RequestsError> {
        let stored = row_key(RequestKind::Album, key);
        let user_id = user_id.to_owned();
        let clock = self.db.clock().clone();
        self.db
            .write("requests.attach", move |tx| {
                if !attach_tx(tx, &stored, &user_id, name.as_deref(), now)? {
                    return Ok(false);
                }
                if let Some(follow) = follow {
                    tx.execute(
                        sql::WIDEN_MONITORING.text(),
                        params![stored, follow.auto_download],
                    )?;
                    follow.apply(tx, &user_id, crate::follows::now_ms(clock.as_ref()))?;
                }
                Ok(true)
            })
            .await
            .map_err(|error| lane_error("requests.attach", error))
    }

    /// Switch a live album row to `release` while no download serves it,
    /// behind the generation CAS. Answers whether the row moved; a row
    /// whose download has started, or that moved on, keeps its release.
    pub async fn switch_release(
        &self,
        key: &str,
        release: &str,
        expected_generation: u64,
    ) -> Result<bool, RequestsError> {
        let stored = row_key(RequestKind::Album, key);
        let release = release.to_owned();
        self.db
            .write("requests.switch_release", move |tx| {
                let changed = tx.execute(
                    sql::SWITCH_RELEASE.text(),
                    params![stored, release, to_i64(expected_generation)],
                )?;
                Ok(changed > 0)
            })
            .await
            .map_err(|error| lane_error("requests.switch_release", error))
    }

    /// Move one row's status behind the generation CAS (`None` skips the
    /// guard). Answers whether the row moved.
    pub async fn update_status(
        &self,
        kind: RequestKind,
        key: &str,
        status: RequestStatus,
        completed_at: Option<u64>,
        expected_generation: Option<u64>,
    ) -> Result<bool, RequestsError> {
        let stored = row_key(kind, key);
        let expected = guard(expected_generation);
        self.db
            .write("requests.update_status", move |tx| {
                let changed = tx.execute(
                    sql::UPDATE_STATUS.text(),
                    params![stored, status, completed_at.map(ms_from_secs), expected],
                )?;
                Ok(changed > 0)
            })
            .await
            .map_err(|error| lane_error("requests.update_status", error))
    }

    /// Link one row to its download task behind the generation CAS (`None`
    /// skips the guard). The row reads downloading, and the task records
    /// which request it serves. A row waiting for approval, mid-cancel,
    /// rejected or imported is not linked. Answers whether the row moved.
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
                let changed =
                    tx.execute(sql::LINK_ROW.text(), params![stored, task_id, expected])?;
                if changed > 0 {
                    tx.execute(sql::MARK_TASK_REQUEST.text(), params![task_id, stored])?;
                }
                Ok(changed > 0)
            })
            .await
            .map_err(|error| lane_error("requests.link_task", error))
    }

    /// The library holds album `key`: settle its request and fulfil its
    /// wanted watch with `watch_outcome` in one write (see
    /// [`settle::on_album_in_library`]). Answers the requests that reached
    /// the library, for the caller to announce.
    pub async fn settle_in_library(
        &self,
        key: &str,
        watch_outcome: &'static str,
        at: u64,
    ) -> Result<Vec<Fulfilled>, RequestsError> {
        let key = key.to_owned();
        self.db
            .write_background("requests.settle_in_library", move |tx| {
                Ok(settle::on_album_in_library(
                    tx,
                    &key,
                    watch_outcome,
                    ms_from_secs(at),
                )?)
            })
            .await
            .map_err(|error| lane_error("requests.settle_in_library", error))
    }

    /// Finished tasks whose linked requests did not follow them, with the
    /// status each task settled as.
    pub async fn unsettled_tasks(&self) -> Result<Vec<(String, String)>, RequestsError> {
        self.db
            .db()
            .read("requests.unsettled_tasks", |conn| {
                let mut statement = conn.prepare_cached(sql::UNSETTLED_TASKS.text())?;
                let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
                Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
            .map_err(|error| lane_error("requests.unsettled_tasks", error))
    }

    /// Album requests only the library can settle: open ones with no task
    /// left to follow, and failed ones whose task is gone.
    pub async fn albums_without_task(&self) -> Result<Vec<String>, RequestsError> {
        self.db
            .db()
            .read("requests.albums_without_task", |conn| {
                let mut statement = conn.prepare_cached(sql::ALBUMS_WITHOUT_TASK.text())?;
                let rows = statement.query_map([], |row| row.get(0))?;
                Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
            })
            .await
            .map_err(|error| lane_error("requests.albums_without_task", error))
    }

    /// Claim an approval: `awaiting_approval` becomes `pending` with the
    /// reviewer stamped and dispatch authorized.
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
                    sql::CLAIM_APPROVAL.text(),
                    params![
                        stored,
                        RequestStatus::Pending,
                        reviewer_name,
                        ms_from_secs(reviewed_at),
                        RequestStatus::AwaitingApproval,
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
                    sql::CLAIM_REJECTION.text(),
                    params![
                        stored,
                        RequestStatus::Rejected,
                        reviewer_name,
                        ms_from_secs(now),
                        RequestStatus::AwaitingApproval,
                        to_i64(expected_generation)
                    ],
                )?;
                Ok(changed > 0)
            })
            .await
            .map_err(|error| lane_error("requests.claim_rejection", error))
    }

    /// Claim a retry generation: only retryable rows move, and a non-admin
    /// must already be a listener.
    #[allow(clippy::too_many_arguments)]
    pub async fn claim_retry(
        &self,
        kind: RequestKind,
        key: &str,
        user_id: &str,
        target: RequestStatus,
        dispatch_authorized: bool,
        require_membership: bool,
        requested_at: u64,
        expected_generation: u64,
    ) -> Result<RetryClaim, RequestsError> {
        let stored = row_key(kind, key);
        let user_id = user_id.to_owned();
        self.db
            .write("requests.claim_retry", move |tx| {
                let row: Option<(RequestStatus, i64)> = tx
                    .query_row(sql::ROW_STATE.text(), params![stored], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                    .optional()?;
                let Some((status, generation)) = row else {
                    return Ok(RetryClaim::Lost);
                };
                if !status.is_retryable()
                    || to_u64(generation) != expected_generation
                    || (require_membership && !is_member_tx(tx, &stored, &user_id)?)
                {
                    return Ok(RetryClaim::Lost);
                }
                tx.execute(
                    sql::CLAIM_RETRY.text(),
                    params![
                        stored,
                        target,
                        dispatch_authorized,
                        ms_from_secs(requested_at)
                    ],
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
        let stored = row_key(kind, key);
        let user_id = user_id.to_owned();
        read(&self.db, "requests.is_requester", move |conn| {
            conn.query_row(sql::IS_MEMBER.text(), params![stored, user_id], |row| {
                row.get(0)
            })
        })
        .await
    }

    /// The atomic requester-cancel decision:
    /// strangers are denied, listeners
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
                let prior = record.status;
                if !is_member_tx(tx, &stored, &user_id)? {
                    return Ok(Some(CancelDecision::Denied {
                        prior_status: prior,
                    }));
                }
                if record.user_id.as_deref() != Some(user_id.as_str()) {
                    tx.execute(sql::DETACH_LISTENER.text(), params![stored, user_id])?;
                    return Ok(Some(CancelDecision::Detached));
                }
                if record.status == RequestStatus::AwaitingApproval {
                    tx.execute(
                        sql::CANCEL_ROW.text(),
                        params![stored, RequestStatus::Cancelled, ms_from_secs(now)],
                    )?;
                    return Ok(Some(CancelDecision::CancelledDirect));
                }
                if !record.status.is_cancellable() {
                    return Ok(Some(CancelDecision::Denied {
                        prior_status: prior,
                    }));
                }
                tx.execute(
                    sql::SET_STATUS.text(),
                    params![stored, RequestStatus::Cancelling],
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
        status: RequestStatus,
        expected_status: RequestStatus,
        expected_generation: u64,
    ) -> Result<bool, RequestsError> {
        let stored = row_key(kind, key);
        self.db
            .write("requests.restore_status", move |tx| {
                let changed = tx.execute(
                    sql::RESTORE_STATUS.text(),
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
                tx.execute(sql::SET_DISPATCH_AUTHORIZED.text(), params![stored, value])?;
                Ok(())
            })
            .await
            .map_err(|error| lane_error("requests.set_dispatch_authorized", error))
    }

    /// Drop a row outright; its listeners and hides go with it (admin
    /// history-clear).
    pub async fn delete(&self, kind: RequestKind, key: &str) -> Result<bool, RequestsError> {
        let stored = row_key(kind, key);
        self.db
            .write("requests.delete", move |tx| {
                let changed = tx.execute(sql::DELETE_ROW.text(), params![stored])?;
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
        now: u64,
    ) -> Result<bool, RequestsError> {
        let stored = row_key(kind, key);
        let user_id = user_id.to_owned();
        self.db
            .write("requests.dismiss", move |tx| {
                let exists: bool =
                    tx.query_row(sql::ROW_EXISTS.text(), params![stored], |row| row.get(0))?;
                if !exists {
                    return Ok(false);
                }
                tx.execute(
                    sql::DISMISS_ROW.text(),
                    params![user_id, stored, ms_from_secs(now)],
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
        let user = user_id.map(str::to_owned);
        read(&self.db, "requests.active", move |conn| {
            read_records(conn, sql::ACTIVE_ROWS.text(), params![user, kind])
        })
        .await
    }

    /// One page of history rows with the status filter and sort, plus the
    /// total row count.
    pub async fn history(
        &self,
        user_id: Option<&str>,
        kind: Option<RequestKind>,
        status: Option<&str>,
        sort: &str,
        offset: u32,
        limit: u32,
    ) -> Result<(Vec<RequestRecord>, u32), RequestsError> {
        let page = sql::history_page(HistoryOrder::from_param(sort));
        let user = user_id.map(str::to_owned);
        let status = status.map(str::to_owned);
        let (rows, total) = read(&self.db, "requests.history", move |conn| {
            let total: i64 = conn.query_row(
                sql::HISTORY_COUNT.text(),
                params![user, kind, status],
                |row| row.get(0),
            )?;
            let rows = read_records(
                conn,
                &page,
                params![user, kind, status, i64::from(limit), i64::from(offset)],
            )?;
            Ok((rows, total))
        })
        .await?;
        Ok((rows, u32::try_from(total).unwrap_or(u32::MAX)))
    }

    /// Rows waiting for review, oldest first.
    pub async fn pending_approvals(&self) -> Result<Vec<RequestRecord>, RequestsError> {
        read(&self.db, "requests.pending_approvals", |conn| {
            read_records(
                conn,
                sql::ROWS_IN_STATUS.text(),
                params![RequestStatus::AwaitingApproval],
            )
        })
        .await
    }

    /// Rows of one kind in one status, oldest first (wanted enrolment).
    pub async fn with_status(
        &self,
        kind: RequestKind,
        status: RequestStatus,
    ) -> Result<Vec<RequestRecord>, RequestsError> {
        read(&self.db, "requests.with_status", move |conn| {
            read_records(
                conn,
                sql::ROWS_IN_STATUS_OF_KIND.text(),
                params![status, kind],
            )
        })
        .await
    }

    /// Asks one listener made since `since` (quota usage).
    pub async fn count_asks_since(&self, user_id: &str, since: u64) -> Result<u32, RequestsError> {
        let user_id = user_id.to_owned();
        read(&self.db, "requests.count_asks", move |conn| {
            count_asks_on(conn, &user_id, since)
        })
        .await
    }

    /// Delete terminal requests settled before `cutoff` (epoch seconds),
    /// keeping any a live wanted watch still points at; their requester
    /// and dismissal rows go with them. Returns the requests deleted.
    pub async fn prune_terminal(&self, cutoff: u64) -> Result<u64, RequestsError> {
        self.db
            .write_background("requests.prune", move |tx| {
                let deleted =
                    tx.execute(sql::PRUNE_TERMINAL.text(), params![ms_from_secs(cutoff)])?;
                Ok(u64::try_from(deleted).unwrap_or(0))
            })
            .await
            .map_err(|error| lane_error("requests.prune", error))
    }
}
