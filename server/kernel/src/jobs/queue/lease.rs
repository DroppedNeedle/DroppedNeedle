//! The lease core: claim a row under a fresh owner, keep it alive, put it
//! back, and recover rows whose owner stopped.
//!
//! A table that leases work (the jobs table, or an area's own job table)
//! hands these functions five statements. Each binds its values by name,
//! so a statement that forgets one fails when it is bound:
//!
//! - `claim(:now, :owner, :lease_until, :scope) RETURNING id`: take one due
//!   row of `scope`, owned by `owner` until `lease_until`.
//! - `heartbeat(:id, :owner, :lease_until, :now)`: extend the lease, for its
//!   owner only.
//! - `release(:id, :owner, :now)`: back to waiting without spending an
//!   attempt, for its owner only.
//! - `recover_expired(:now, :scope)`: requeue rows whose lease ran out.
//! - `recover_at_start(:now, :scope)`: requeue every row a stopped process
//!   held. Only safe because the server lock keeps a second server off the
//!   data directory.
//!
//! Which states are claimable, failing a row at its attempt limit, and any
//! folding of rows are the table's own SQL. An owner is unique per claim,
//! so a stale worker from this same process never matches after a reclaim.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{Transaction, named_params};

use super::JobError;
use crate::db::statement::Statement;

/// The five statements a leased table supplies.
#[derive(Debug, Clone, Copy)]
pub struct LeaseStatements {
    /// Take one due row.
    pub claim: Statement,
    /// Extend a lease.
    pub heartbeat: Statement,
    /// Put a row back without spending an attempt.
    pub release: Statement,
    /// Requeue rows whose lease ran out.
    pub recover_expired: Statement,
    /// Requeue every row held at the last stop.
    pub recover_at_start: Statement,
}

/// Mints lease owners: a random nonce for this process plus a counter.
#[derive(Debug, Clone)]
pub struct Owner {
    nonce: Arc<str>,
    next: Arc<AtomicU64>,
}

impl Default for Owner {
    fn default() -> Self {
        Self::new()
    }
}

impl Owner {
    /// A minter with a fresh random nonce.
    pub fn new() -> Self {
        Self {
            nonce: Arc::from(uuid::Uuid::new_v4().simple().to_string()),
            next: Arc::new(AtomicU64::new(1)),
        }
    }

    /// A new owner, never handed out before by any process.
    pub fn mint(&self) -> Arc<str> {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        Arc::from(format!("{}-{n}", self.nonce))
    }
}

/// Claim one due row of `scope` for `owner` until `lease_until`.
pub fn claim(
    tx: &Transaction,
    statements: &LeaseStatements,
    scope: &str,
    owner: &str,
    now: i64,
    lease_until: i64,
) -> Result<Option<i64>, JobError> {
    let mut statement = tx.prepare_cached(statements.claim.text())?;
    let mut rows = statement.query(named_params! {
        ":now": now, ":owner": owner, ":lease_until": lease_until, ":scope": scope,
    })?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// Extend `owner`'s lease on row `id`. False when `owner` no longer holds it.
pub fn heartbeat(
    tx: &Transaction,
    statements: &LeaseStatements,
    id: i64,
    owner: &str,
    now: i64,
    lease_until: i64,
) -> Result<bool, JobError> {
    let changed = tx.prepare_cached(statements.heartbeat.text())?.execute(
        named_params! { ":id": id, ":owner": owner, ":lease_until": lease_until, ":now": now },
    )?;
    Ok(changed == 1)
}

/// Put row `id` back without spending an attempt. False when `owner` no
/// longer holds it.
pub fn release(
    tx: &Transaction,
    statements: &LeaseStatements,
    id: i64,
    owner: &str,
    now: i64,
) -> Result<bool, JobError> {
    let changed = tx
        .prepare_cached(statements.release.text())?
        .execute(named_params! { ":id": id, ":owner": owner, ":now": now })?;
    Ok(changed == 1)
}

/// Requeue the rows of `scope` whose lease ran out by `now`.
pub fn recover_expired(
    tx: &Transaction,
    statements: &LeaseStatements,
    scope: &str,
    now: i64,
) -> Result<u64, JobError> {
    let changed = tx
        .prepare_cached(statements.recover_expired.text())?
        .execute(named_params! { ":now": now, ":scope": scope })?;
    Ok(u64::try_from(changed).unwrap_or(u64::MAX))
}

/// Requeue every held row of `scope`, whatever its lease says.
pub fn recover_at_start(
    tx: &Transaction,
    statements: &LeaseStatements,
    scope: &str,
    now: i64,
) -> Result<u64, JobError> {
    let changed = tx
        .prepare_cached(statements.recover_at_start.text())?
        .execute(named_params! { ":now": now, ":scope": scope })?;
    Ok(u64::try_from(changed).unwrap_or(u64::MAX))
}
