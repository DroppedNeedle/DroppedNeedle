//! The jobs table and its one module: enqueue, claim under a lease,
//! heartbeat, complete, fail with backoff, and recover.
//!
//! A queue or outbox is a job kind with a handler, never new lease code.
//! An area declares a [`JobKind`], enqueues on its own transaction, and
//! registers a [`JobHandler`] through the builder; each registered kind
//! gets one supervised runner (see [`runner`]).
//!
//! The rules:
//!
//! 1. A job is at-least-once, never exactly-once. Restoring a backup
//!    re-runs every job that finished after the backup was taken.
//! 2. A handler doing destructive work records its progress in its own
//!    domain transaction and checks it on entry. A handler whose last step
//!    is a write calls [`JobCtx::complete`] in that same transaction, so a
//!    worker that lost its lease has its write rolled back. Work the
//!    handler spawns and does not await outlives the job and is not
//!    covered. A joined enqueue's change is always visible to the run that
//!    serves it: the handler reads after its claim commits.
//! 3. An enqueue in a transaction that rolls back never existed.
//! 4. A restart neither forgets a queued or running job nor runs one twice
//!    at once. At start every running row is from a stopped process,
//!    because the server lock keeps a second server off the data
//!    directory.
//!
//! A key has at most one queued row. An enqueue while its row runs adds
//! one queued row; claim skips a queued row whose key has a running one. A
//! payload is a function of its key: a joined enqueue keeps the queued
//! row's payload.
//!
//! Every put-back and recovery folds: if the key already has a queued row,
//! the running row is deleted and the queued one takes the higher attempt
//! count, the later due time and the code. A row at its attempt limit is
//! failed first. A folded row's id reads as gone.
//!
//! A busy database never spends an attempt or fails a job: a handler that
//! hears busy answers [`Outcome::Busy`] and its job goes back with the
//! claim's attempt returned. Claim counts one attempt; a process that dies
//! holding a claim has spent it, so a job that kills the process every
//! time ends failed.
//!
//! The module logs kind, id, attempt and code, never a payload or a key.

pub mod lease;
pub mod runner;
pub(crate) mod sql;

use std::sync::Arc;
use std::time::Duration;

use rusqlite::{OptionalExtension, Transaction, named_params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::clock::Clock;
use crate::db::{Db, DbError, Lane};
use lease::{LeaseStatements, Owner};

pub use runner::{Handlers, start_runners};

/// Requeued at start: the process that held it stopped.
pub const INTERRUPTED: &str = "INTERRUPTED";
/// Requeued after its lease ran out.
pub const LEASE_EXPIRED: &str = "LEASE_EXPIRED";
/// The handler ran past its kind's run timeout.
pub const TIMED_OUT: &str = "TIMED_OUT";
/// The handler panicked.
pub const HANDLER_PANICKED: &str = "HANDLER_PANICKED";
/// The payload did not decode; for handlers to answer with.
pub const PAYLOAD_INVALID: &str = "PAYLOAD_INVALID";
/// A retry asked for a time further out than the module waits.
pub const RETRY_TOO_FAR: &str = "RETRY_TOO_FAR";
/// Put back because the database was busy.
pub const BUSY: &str = "BUSY";

/// Longest wait a retry may ask for before the job fails instead.
const RETRY_CEILING: Duration = Duration::from_secs(24 * 60 * 60);
/// Longest kind name, key and payload the table holds.
const MAX_KIND: usize = 64;
const MAX_KEY: usize = 200;
const MAX_PAYLOAD: usize = 16_384;
/// Longest code `last_error` holds.
const MAX_CODE_CHARS: usize = 64;

/// The jobs table's lease statements.
pub const STATEMENTS: LeaseStatements = LeaseStatements {
    claim: sql::CLAIM,
    heartbeat: sql::HEARTBEAT,
    release: sql::RELEASE,
    recover_expired: sql::RECOVER_EXPIRED,
    recover_at_start: sql::RECOVER_AT_START,
};

/// A job kind, declared once as a constant by the area that enqueues it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JobKind(&'static str);

impl JobKind {
    /// A kind named `name`, such as `scrobble.lastfm`.
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// The name stored on each row.
    pub const fn name(&self) -> &'static str {
        self.0
    }
}

/// How one kind is run. Defaults suit a short outside call.
#[derive(Debug, Clone, Copy)]
pub struct KindSpec {
    kind: JobKind,
    max_attempts: u32,
    lease: Duration,
    run_timeout: Duration,
    backoff_first: Duration,
    backoff_cap: Duration,
    stop_grace: Duration,
    keep_done: Duration,
    keep_failed: Duration,
}

impl KindSpec {
    /// The defaults: 5 attempts, a 60 s lease, a 10 min run, backoff from
    /// 30 s doubling to 1 h, 5 s to finish at stop, done rows kept 7 days
    /// and failed rows 30.
    pub const fn new(kind: JobKind) -> Self {
        Self {
            kind,
            max_attempts: 5,
            lease: Duration::from_secs(60),
            run_timeout: Duration::from_secs(600),
            backoff_first: Duration::from_secs(30),
            backoff_cap: Duration::from_secs(3600),
            stop_grace: Duration::from_secs(5),
            keep_done: Duration::from_secs(7 * 24 * 3600),
            keep_failed: Duration::from_secs(30 * 24 * 3600),
        }
    }

    /// Claims before the job fails for good.
    pub const fn max_attempts(mut self, attempts: u32) -> Self {
        self.max_attempts = attempts;
        self
    }

    /// How long a claim holds without a heartbeat.
    pub const fn lease(mut self, lease: Duration) -> Self {
        self.lease = lease;
        self
    }

    /// Longest one run may take.
    pub const fn run_timeout(mut self, timeout: Duration) -> Self {
        self.run_timeout = timeout;
        self
    }

    /// First retry delay and its cap; each retry doubles it.
    pub const fn backoff(mut self, first: Duration, cap: Duration) -> Self {
        self.backoff_first = first;
        self.backoff_cap = cap;
        self
    }

    /// How long a running handler gets to finish at stop.
    pub const fn stop_grace(mut self, grace: Duration) -> Self {
        self.stop_grace = grace;
        self
    }

    /// How long done rows are kept.
    pub const fn keep_done(mut self, keep: Duration) -> Self {
        self.keep_done = keep;
        self
    }

    /// How long failed rows are kept.
    pub const fn keep_failed(mut self, keep: Duration) -> Self {
        self.keep_failed = keep;
        self
    }

    /// The kind.
    pub const fn kind(&self) -> JobKind {
        self.kind
    }
}

/// A row's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Waiting for its due time and a runner.
    Queued,
    /// Held under a lease.
    Running,
    /// Finished.
    Done,
    /// Failed for good.
    Failed,
}

impl State {
    /// The stored value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    /// The state a stored value names.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// What a job module call can fail with.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// Another worker holds the job now, or it is finished or gone.
    #[error("the job's lease is no longer held")]
    LeaseLost,
    /// The kind, key or payload is empty, too long or does not encode.
    #[error("the job's kind, key or payload is out of bounds")]
    Payload,
    /// The database failed.
    #[error(transparent)]
    Db(#[from] DbError),
    /// A lease statement broke its promise (the conformance check).
    #[cfg(any(test, feature = "test-support"))]
    #[error("{0}")]
    Conformance(String),
}

impl From<rusqlite::Error> for JobError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(DbError::from(error))
    }
}

impl JobError {
    /// True when the database was busy: retry later, spend nothing.
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Db(DbError::Busy { .. }))
    }
}

/// What an enqueue did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Enqueued {
    /// The queued row.
    pub id: i64,
    /// True when the key's queued row already stood and was left as it is.
    pub joined: bool,
}

/// A held claim: the row and its owner.
#[derive(Debug, Clone)]
pub struct Lease {
    id: i64,
    owner: Arc<str>,
}

impl Lease {
    /// The row.
    pub fn id(&self) -> i64 {
        self.id
    }
}

/// A claimed row.
#[derive(Debug, Clone)]
pub struct Claimed {
    /// The lease on it.
    pub lease: Lease,
    /// Its key.
    pub key: String,
    /// Which claim this is, from 1.
    pub attempt: u32,
    /// Its payload as stored.
    pub payload: Arc<str>,
}

/// One row as the screens read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobStatus {
    /// The row.
    pub id: i64,
    /// Its state.
    pub state: State,
    /// Claims counted so far.
    pub attempts: u32,
    /// The code of the last failure, cleared when done.
    pub last_error: Option<String>,
    /// When it may next run, unix ms.
    pub not_before_at: i64,
    /// When it was enqueued, unix ms.
    pub created_at: i64,
    /// When it finished, unix ms.
    pub finished_at: Option<i64>,
}

/// Live and failed rows of one kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindCounts {
    /// The kind.
    pub kind: String,
    /// Rows waiting.
    pub queued: u64,
    /// Rows held.
    pub running: u64,
    /// Rows failed for good and still kept.
    pub failed: u64,
}

/// A handler's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Finished. A handler that already completed in its own write may
    /// still answer this.
    Done,
    /// Try again later, at `at` (unix ms, an outside `Retry-After`) or
    /// after the kind's backoff. Spends the attempt.
    Retry {
        /// Fixed code for `last_error`.
        code: &'static str,
        /// The earliest next try.
        at: Option<i64>,
    },
    /// Failed for good.
    Fail {
        /// Fixed code for `last_error`.
        code: &'static str,
    },
    /// The database was busy: back in the queue, attempt returned.
    Busy,
}

/// Runs one kind's jobs.
pub trait JobHandler: Send + Sync + 'static {
    /// Run one job. A handler may be dropped at any await (stop, timeout,
    /// lost lease); rule 2 keeps that safe.
    fn run(&self, job: JobCtx) -> futures_util::future::BoxFuture<'static, Outcome>;
}

/// What a running handler is given.
#[derive(Clone)]
pub struct JobCtx {
    claimed: Claimed,
    spec: KindSpec,
    queue: JobQueue,
    token: CancellationToken,
}

impl std::fmt::Debug for JobCtx {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JobCtx")
            .field("id", &self.claimed.lease.id)
            .field("attempt", &self.claimed.attempt)
            .finish_non_exhaustive()
    }
}

impl JobCtx {
    /// The row.
    pub fn id(&self) -> i64 {
        self.claimed.lease.id
    }

    /// The idempotency key.
    pub fn key(&self) -> &str {
        &self.claimed.key
    }

    /// Which claim this is, from 1.
    pub fn attempt(&self) -> u32 {
        self.claimed.attempt
    }

    /// The payload, decoded.
    pub fn payload<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_str(&self.claimed.payload)
    }

    /// Cancelled when the job must stop: shutdown, timeout or a lost lease.
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// Extend the lease now. The runner also does this on its own.
    pub async fn heartbeat(&self) -> Result<(), JobError> {
        self.queue.heartbeat(&self.claimed.lease, &self.spec).await
    }

    /// Finish the job inside the handler's own write. `LeaseLost` means
    /// another worker holds it: return the error so the write rolls back.
    pub fn complete(&self, tx: &Transaction) -> Result<(), JobError> {
        self.queue.complete(tx, &self.claimed.lease)
    }
}

/// The jobs table. Cloneable; clones share the clock, the owner minter and
/// the wake channel.
#[derive(Clone)]
pub struct JobQueue {
    db: Db,
    clock: Arc<dyn Clock>,
    owners: Owner,
    wake: Arc<watch::Sender<u64>>,
}

impl std::fmt::Debug for JobQueue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("JobQueue").finish_non_exhaustive()
    }
}

impl JobQueue {
    /// The jobs table on `db`, stamped by `clock`.
    pub fn new(db: Db, clock: Arc<dyn Clock>) -> Self {
        Self {
            db,
            clock,
            owners: Owner::new(),
            wake: Arc::new(watch::channel(0).0),
        }
    }

    fn now(&self) -> i64 {
        self.clock.now_ms()
    }

    /// Enqueue a job due now, on the caller's transaction.
    pub fn enqueue(
        &self,
        tx: &Transaction,
        kind: JobKind,
        key: &str,
        payload: &impl Serialize,
    ) -> Result<Enqueued, JobError> {
        self.enqueue_at(tx, kind, key, payload, self.now())
    }

    /// Enqueue a job due at `due_at` (unix ms), on the caller's
    /// transaction. A key whose queued row stands joins it unchanged.
    pub fn enqueue_at(
        &self,
        tx: &Transaction,
        kind: JobKind,
        key: &str,
        payload: &impl Serialize,
        due_at: i64,
    ) -> Result<Enqueued, JobError> {
        let payload = serde_json::to_string(payload).map_err(|_| JobError::Payload)?;
        let name = kind.name();
        if name.is_empty()
            || name.chars().count() > MAX_KIND
            || key.is_empty()
            || key.chars().count() > MAX_KEY
            || payload.len() > MAX_PAYLOAD
        {
            return Err(JobError::Payload);
        }
        let now = self.now();
        let added: Option<i64> = tx
            .prepare_cached(sql::ENQUEUE.text())?
            .query_row(
                named_params! {
                    ":kind": name, ":key": key, ":due": due_at, ":payload": payload, ":now": now,
                },
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = added {
            return Ok(Enqueued { id, joined: false });
        }
        let id = tx
            .prepare_cached(sql::QUEUED_ID.text())?
            .query_row(named_params! { ":kind": name, ":key": key }, |row| {
                row.get(0)
            })?;
        Ok(Enqueued { id, joined: true })
    }

    /// Finish a held job on the caller's transaction. Done by this same
    /// owner already is fine; anything else is `LeaseLost`.
    pub fn complete(&self, tx: &Transaction, lease: &Lease) -> Result<(), JobError> {
        let changed = tx
            .prepare_cached(sql::COMPLETE.text())?
            .execute(named_params! {
                ":id": lease.id, ":owner": &*lease.owner, ":now": self.now(),
            })?;
        if changed == 1 || done_by(tx, lease)? {
            Ok(())
        } else {
            Err(JobError::LeaseLost)
        }
    }

    /// End every queued and running row of a key, on the caller's
    /// transaction. A worker still running it finds its lease lost.
    pub fn cancel(&self, tx: &Transaction, kind: JobKind, key: &str) -> Result<u64, JobError> {
        let removed = tx
            .prepare_cached(sql::CANCEL.text())?
            .execute(named_params! { ":kind": kind.name(), ":key": key })?;
        Ok(u64::try_from(removed).unwrap_or(u64::MAX))
    }

    /// One row by id. `None` when it was pruned, folded into a newer
    /// request, or cancelled.
    pub async fn get(&self, id: i64) -> Result<Option<JobStatus>, DbError> {
        self.db
            .read("jobs-get", move |conn| {
                Ok(conn
                    .prepare_cached(sql::STATUS_BY_ID.text())?
                    .query_row(named_params! { ":id": id }, status_row)
                    .optional()?)
            })
            .await
    }

    /// The newest row of a key.
    pub async fn latest(&self, kind: JobKind, key: &str) -> Result<Option<JobStatus>, DbError> {
        let key = key.to_owned();
        self.db
            .read("jobs-latest", move |conn| {
                Ok(conn
                    .prepare_cached(sql::LATEST_BY_KEY.text())?
                    .query_row(
                        named_params! { ":kind": kind.name(), ":key": key },
                        status_row,
                    )
                    .optional()?)
            })
            .await
    }

    /// Wake every runner now: something a handler reads has changed.
    pub fn wake(&self) {
        self.wake
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// Extend a held lease. A row done by this owner is fine.
    pub async fn heartbeat(&self, lease: &Lease, spec: &KindSpec) -> Result<(), JobError> {
        let lease = lease.clone();
        let now = self.now();
        let until = now.saturating_add(ms(spec.lease));
        self.db
            .write(Lane::Background, "jobs-heartbeat", move |tx| {
                if lease::heartbeat(tx, &STATEMENTS, lease.id, &lease.owner, now, until)?
                    || done_by(tx, &lease)?
                {
                    Ok(())
                } else {
                    Err(JobError::LeaseLost)
                }
            })
            .await
    }

    /// Recover expired leases and claim one due job, on the caller's
    /// transaction, for a test that stands in for a worker.
    #[cfg(any(test, feature = "test-support"))]
    pub fn claim_in(&self, tx: &Transaction, spec: &KindSpec) -> Result<Option<Claimed>, JobError> {
        claim_on(tx, spec, &self.owners.mint(), self.now())
    }

    /// Requeue rows of a kind whose lease ran out, as a runner pass does.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn recover_expired(&self, spec: &KindSpec) -> Result<u64, JobError> {
        let spec = *spec;
        let now = self.now();
        self.db
            .write(Lane::Background, "jobs-recover", move |tx| {
                recover(tx, &spec, false, now)
            })
            .await
    }

    async fn claim(&self, spec: &KindSpec) -> Result<Option<Claimed>, JobError> {
        let spec = *spec;
        let owner = self.owners.mint();
        let now = self.now();
        self.db
            .write(Lane::Background, "jobs-claim", move |tx| {
                claim_on(tx, &spec, &owner, now)
            })
            .await
    }

    async fn recover_at_start(&self, spec: &KindSpec) -> Result<u64, JobError> {
        let spec = *spec;
        let now = self.now();
        self.db
            .write(Lane::Background, "jobs-recover-start", move |tx| {
                recover(tx, &spec, true, now)
            })
            .await
    }

    /// When this kind next has work, unix ms.
    async fn next_due(&self, kind: JobKind) -> Result<Option<i64>, DbError> {
        self.db
            .read("jobs-next-due", move |conn| {
                Ok(conn
                    .prepare_cached(sql::NEXT_DUE.text())?
                    .query_row(named_params! { ":kind": kind.name() }, |row| row.get(0))?)
            })
            .await
    }

    /// Write a finished run's answer. `LeaseLost` is final.
    async fn finish(
        &self,
        lease: &Lease,
        spec: &KindSpec,
        outcome: Outcome,
        spread: u32,
    ) -> Result<(), JobError> {
        let lease = lease.clone();
        let spec = *spec;
        let now = self.now();
        self.db
            .write(Lane::Background, "jobs-finish", move |tx| match outcome {
                Outcome::Done => {
                    let changed = tx.prepare_cached(sql::COMPLETE.text())?.execute(
                        named_params! { ":id": lease.id, ":owner": &*lease.owner, ":now": now },
                    )?;
                    if changed == 1 || done_by(tx, &lease)? {
                        Ok(())
                    } else {
                        Err(JobError::LeaseLost)
                    }
                }
                Outcome::Fail { code } => {
                    held(tx, &lease)?;
                    fail_row(tx, lease.id, code, now)
                }
                Outcome::Retry { code, at } => {
                    let (_, _, attempts) = held(tx, &lease)?;
                    let due = match at {
                        Some(at) if at > now.saturating_add(ms(RETRY_CEILING)) => {
                            return fail_row(tx, lease.id, RETRY_TOO_FAR, now);
                        }
                        Some(at) => at.max(now),
                        None => now.saturating_add(backoff_ms(&spec, attempts, spread)),
                    };
                    put_back(tx, &lease, &spec, 0, due, Some(code), true, now)
                }
                Outcome::Busy => {
                    let due = now.saturating_add(busy_delay_ms(spread));
                    put_back(tx, &lease, &spec, -1, due, Some(BUSY), false, now)
                }
            })
            .await
    }

    /// Put a job back at stop, without spending its attempt.
    async fn release(&self, lease: &Lease) -> Result<(), JobError> {
        let lease = lease.clone();
        let now = self.now();
        self.db
            .write(Lane::Background, "jobs-release", move |tx| {
                let (kind, key, attempts) = held(tx, &lease)?;
                if fold(
                    tx,
                    lease.id,
                    &kind,
                    &key,
                    attempts.saturating_sub(1),
                    now,
                    None,
                    now,
                )? {
                    return Ok(());
                }
                if lease::release(tx, &STATEMENTS, lease.id, &lease.owner, now)? {
                    Ok(())
                } else {
                    Err(JobError::LeaseLost)
                }
            })
            .await
    }

    /// Delete finished rows past their keep time.
    async fn prune(&self, spec: &KindSpec) -> Result<(), JobError> {
        let spec = *spec;
        let now = self.now();
        self.db
            .write(Lane::Background, "jobs-prune", move |tx| {
                let mut statement = tx.prepare_cached(sql::PRUNE.text())?;
                for (state, keep) in [
                    (State::Done, spec.keep_done),
                    (State::Failed, spec.keep_failed),
                ] {
                    statement.execute(named_params! {
                        ":kind": spec.kind.name(),
                        ":state": state.as_str(),
                        ":before": now.saturating_sub(ms(keep)),
                    })?;
                }
                Ok(())
            })
            .await
    }
}

/// Queued, running and kept failed rows per kind, for the admin view.
pub async fn counts(db: &Db) -> Result<Vec<KindCounts>, DbError> {
    db.read("jobs-counts", |conn| {
        let mut statement = conn.prepare_cached(sql::COUNTS.text())?;
        let rows = statement.query_map([], |row| {
            Ok(KindCounts {
                kind: row.get(0)?,
                queued: row.get(1)?,
                running: row.get(2)?,
                failed: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .await
}

/// Kinds with queued or running rows.
pub(crate) async fn live_kinds(db: &Db) -> Result<Vec<String>, DbError> {
    db.read("jobs-live-kinds", |conn| {
        let mut statement = conn.prepare_cached(sql::LIVE_KINDS.text())?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .await
}

fn status_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobStatus> {
    let state: String = row.get(1)?;
    let state = State::parse(&state).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            format!("unknown job state {state:?}").into(),
        )
    })?;
    Ok(JobStatus {
        id: row.get(0)?,
        state,
        attempts: row.get(2)?,
        last_error: row.get(3)?,
        not_before_at: row.get(4)?,
        created_at: row.get(5)?,
        finished_at: row.get(6)?,
    })
}

/// Recover expired rows (or, at start, every running row), then claim one.
fn claim_on(
    tx: &Transaction,
    spec: &KindSpec,
    owner: &Arc<str>,
    now: i64,
) -> Result<Option<Claimed>, JobError> {
    recover(tx, spec, false, now)?;
    let until = now.saturating_add(ms(spec.lease));
    let Some(id) = lease::claim(tx, &STATEMENTS, spec.kind.name(), owner, now, until)? else {
        return Ok(None);
    };
    let (key, attempt, payload): (String, u32, String) = tx
        .prepare_cached(sql::CLAIMED.text())?
        .query_row(named_params! { ":id": id }, |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    Ok(Some(Claimed {
        lease: Lease {
            id,
            owner: Arc::clone(owner),
        },
        key,
        attempt,
        payload: Arc::from(payload),
    }))
}

/// Requeue running rows of a kind: every one at start, else those whose
/// lease ran out. A row at its limit fails; a row whose key has a queued
/// row folds into it; the rest go back through the lease statements.
fn recover(tx: &Transaction, spec: &KindSpec, all: bool, now: i64) -> Result<u64, JobError> {
    let code = if all { INTERRUPTED } else { LEASE_EXPIRED };
    let kind = spec.kind.name();
    let rows: Vec<(i64, String, u32)> = {
        let mut statement = tx.prepare_cached(sql::RECOVERABLE.text())?;
        let rows = statement.query_map(
            named_params! { ":kind": kind, ":all": all, ":now": now },
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        rows.collect::<Result<_, _>>()?
    };
    let mut handled = 0_u64;
    for (id, key, attempts) in &rows {
        if *attempts >= spec.max_attempts {
            fail_row(tx, *id, code, now)?;
            handled += 1;
        } else if fold(tx, *id, kind, key, *attempts, now, Some(code), now)? {
            handled += 1;
        }
    }
    let requeued = if all {
        lease::recover_at_start(tx, &STATEMENTS, kind, now)?
    } else {
        lease::recover_expired(tx, &STATEMENTS, kind, now)?
    };
    if !rows.is_empty() {
        tracing::info!(kind, rows = rows.len(), code, "recovered held jobs");
    }
    Ok(handled + requeued)
}

/// The held row's kind, key and attempts, or `LeaseLost`.
fn held(tx: &Transaction, lease: &Lease) -> Result<(String, String, u32), JobError> {
    tx.prepare_cached(sql::HELD.text())?
        .query_row(
            named_params! { ":id": lease.id, ":owner": &*lease.owner },
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or(JobError::LeaseLost)
}

fn done_by(tx: &Transaction, lease: &Lease) -> Result<bool, JobError> {
    Ok(tx.prepare_cached(sql::DONE_BY.text())?.query_row(
        named_params! { ":id": lease.id, ":owner": &*lease.owner },
        |row| row.get(0),
    )?)
}

/// Put a held row back: fail it at the limit (when `may_fail`), fold it
/// into its key's queued row, or requeue it.
#[allow(clippy::too_many_arguments)]
fn put_back(
    tx: &Transaction,
    lease: &Lease,
    spec: &KindSpec,
    attempts_delta: i64,
    due: i64,
    code: Option<&'static str>,
    may_fail: bool,
    now: i64,
) -> Result<(), JobError> {
    let (kind, key, attempts) = held(tx, lease)?;
    let attempts = u32::try_from((i64::from(attempts) + attempts_delta).max(0)).unwrap_or(0);
    if may_fail && attempts >= spec.max_attempts {
        return fail_row(tx, lease.id, code.unwrap_or(LEASE_EXPIRED), now);
    }
    if fold(tx, lease.id, &kind, &key, attempts, due, code, now)? {
        return Ok(());
    }
    tx.prepare_cached(sql::REQUEUE.text())?.execute(named_params! {
        ":id": lease.id, ":attempts": attempts, ":due": due, ":code": code.map(clamp), ":now": now,
    })?;
    Ok(())
}

/// Fold running row `id` into its key's queued row, if one stands. True
/// when it did.
#[allow(clippy::too_many_arguments)]
fn fold(
    tx: &Transaction,
    id: i64,
    kind: &str,
    key: &str,
    attempts: u32,
    due: i64,
    code: Option<&'static str>,
    now: i64,
) -> Result<bool, JobError> {
    let sibling: Option<i64> = tx
        .prepare_cached(sql::SIBLING.text())?
        .query_row(named_params! { ":kind": kind, ":key": key }, |row| {
            row.get(0)
        })
        .optional()?;
    let Some(sibling) = sibling else {
        return Ok(false);
    };
    tx.prepare_cached(sql::FOLD_INTO.text())?.execute(named_params! {
        ":id": sibling, ":attempts": attempts, ":due": due, ":code": code.map(clamp), ":now": now,
    })?;
    tx.prepare_cached(sql::DELETE_ROW.text())?
        .execute(named_params! { ":id": id })?;
    Ok(true)
}

fn fail_row(tx: &Transaction, id: i64, code: &'static str, now: i64) -> Result<(), JobError> {
    tx.prepare_cached(sql::FAIL.text())?
        .execute(named_params! { ":id": id, ":code": clamp(code), ":now": now })?;
    Ok(())
}

/// Parts per thousand of jitter a runner draws for each finish.
pub(crate) const SPREAD_SCALE: u32 = 1000;

/// The retry delay after `attempts` claims: the first delay doubled per
/// claim past the first, capped, plus up to a quarter of it as jitter
/// (`spread` of [`SPREAD_SCALE`]).
fn backoff_ms(spec: &KindSpec, attempts: u32, spread: u32) -> i64 {
    let doublings = attempts.saturating_sub(1).min(30);
    let delay = ms(spec
        .backoff_first
        .saturating_mul(1_u32 << doublings)
        .min(spec.backoff_cap));
    delay.saturating_add(delay / 4 * i64::from(spread.min(SPREAD_SCALE)) / i64::from(SPREAD_SCALE))
}

/// How long a busy put-back waits: 2 s plus up to 2 s of jitter.
fn busy_delay_ms(spread: u32) -> i64 {
    2_000 + 2_000 * i64::from(spread.min(SPREAD_SCALE)) / i64::from(SPREAD_SCALE)
}

/// A code as `last_error` holds it.
fn clamp(code: &'static str) -> &'static str {
    match code.char_indices().nth(MAX_CODE_CHARS) {
        Some((end, _)) => &code[..end],
        None => code,
    }
}

fn ms(span: Duration) -> i64 {
    i64::try_from(span.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests;
