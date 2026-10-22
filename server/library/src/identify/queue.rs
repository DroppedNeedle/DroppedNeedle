//! Queue policy: priorities, leases, and the bounded backoff ladder, and
//! the writes that offer an album to the queue inside the caller's
//! transaction.

use rusqlite::{Connection, OptionalExtension as _, params};
use sha2::{Digest as _, Sha256};

use super::models::IdentifyKind;
use super::sqlite::kind_str;

/// New or changed albums identify first.
pub const PRIORITY_NEW_OR_CHANGED: u32 = 20;
/// A curator asked for another look.
pub const PRIORITY_REVIEW_RETRY: u32 = 30;
/// Historical backlog fills in behind live work.
pub const PRIORITY_HISTORICAL_BACKLOG: u32 = 40;
/// Supporting maintenance runs last.
pub const PRIORITY_SUPPORTING_MAINTENANCE: u32 = 50;

/// How long one worker holds a claimed job before it can be reclaimed.
pub const LEASE_SECONDS: u64 = 60;

/// The ladder is 30, 60, 120 ... doubling per
/// deferral, capped so it always equals the largest delay the formula can
/// schedule under the ten-attempt terminal bound: 30 through 7,680 s,
/// 15,330 s cumulative before attempt ten terminalizes. A bounded-retry
/// window, never a provider-health timeout.
pub const MAX_DEFERRAL_ATTEMPTS: u32 = 10;
pub const MAX_BACKOFF_SECONDS: u64 = 30 * 2_u64.pow(MAX_DEFERRAL_ATTEMPTS - 2);

/// Backoff before the next attempt after `deferrals` consecutive deferrals
/// (1-based: the first deferral waits 30 s).
pub fn backoff_secs(deferrals: u32) -> u64 {
    if deferrals == 0 {
        return 0;
    }
    let shift = deferrals.saturating_sub(1).min(30);
    (30_u64.saturating_mul(2_u64.pow(shift))).min(MAX_BACKOFF_SECONDS)
}

/// True once the job has used its whole bounded window.
pub fn terminally_deferred(attempts: u32) -> bool {
    attempts >= MAX_DEFERRAL_ATTEMPTS
}

// ---------------------------------------------------------------------------
// Offering albums to identification, inside the caller's transaction.
// ---------------------------------------------------------------------------

pub(crate) fn idempotency_key(local_album_id: &str, input_revision: &str) -> String {
    format!("{local_album_id}:{input_revision}")
}

/// Store one identify job. A live job on the same album and input
/// revision (the partial unique index on its key) keeps its place.
pub(crate) const INSERT_JOB: &str = "INSERT INTO library_identify_jobs (id, local_album_id, kind, \
     priority, state, attempts, not_before_ms, input_revision, idempotency_key, \
     requested_by_user_id, failure_code, created_ms, updated_ms) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12) ON CONFLICT DO NOTHING";

/// Content revision of one album: a hash over its indexed members and
/// their stat revisions. Any added, removed, or changed file moves it.
pub fn album_input_revision(conn: &Connection, local_album_id: &str) -> rusqlite::Result<String> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, stat_revision FROM local_tracks \
         WHERE local_album_id = ?1 AND availability = 'indexed' ORDER BY id",
    )?;
    let mut hasher = Sha256::new();
    let mut rows = stmt.query(params![local_album_id])?;
    while let Some(row) = rows.next()? {
        hasher.update(row.get::<_, String>(0)?.as_bytes());
        hasher.update(b"\0");
        hasher.update(row.get::<_, String>(1)?.as_bytes());
        hasher.update(b"\n");
    }
    Ok(hasher
        .finalize()
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Offer one album to identification after the scan changed it. A job
/// already waiting for the album takes the new revision (its attempt reads
/// facts fresh from the catalog anyway); otherwise a job is queued unless
/// one is running on, or already finished, this exact revision. Returns
/// true when a new job was queued. Runs inside the caller's transaction so
/// the offer lands with the catalog rows that caused it.
pub fn offer_album(conn: &Connection, local_album_id: &str, now_ms: i64) -> rusqlite::Result<bool> {
    let revision = album_input_revision(conn, local_album_id)?;
    let key = idempotency_key(local_album_id, &revision);
    let waiting: Option<String> = conn
        .query_row(
            "SELECT id FROM library_identify_jobs \
             WHERE local_album_id = ?1 AND state IN ('queued','deferred') \
             ORDER BY created_ms LIMIT 1",
            params![local_album_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(job_id) = waiting {
        conn.execute(
            "UPDATE OR IGNORE library_identify_jobs SET input_revision = ?1, \
             idempotency_key = ?2, updated_ms = ?3 WHERE id = ?4",
            params![revision, key, now_ms, job_id],
        )?;
        return Ok(false);
    }
    let settled: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_identify_jobs \
             WHERE idempotency_key = ?1 AND state IN ('running','succeeded'))",
        params![key],
        |row| row.get(0),
    )?;
    if settled {
        return Ok(false);
    }
    let inserted = conn.execute(
        "INSERT INTO library_identify_jobs (id, local_album_id, kind, priority, state, \
         attempts, not_before_ms, input_revision, idempotency_key, created_ms, updated_ms) \
         VALUES (?1, ?2, 'automatic', ?3, 'queued', 0, ?4, ?5, ?6, ?4, ?4) \
         ON CONFLICT DO NOTHING",
        params![
            uuid::Uuid::new_v4().to_string(),
            local_album_id,
            PRIORITY_NEW_OR_CHANGED as i64,
            now_ms,
            revision,
            key,
        ],
    )?;
    if inserted == 1 {
        bump_identification_stream(conn)?;
    }
    Ok(inserted == 1)
}

/// Queue identification of one album again, at backlog priority, because
/// catalog rows it reads changed while its files did not. A live job on
/// the album's files does the work instead. Runs inside the caller's
/// transaction.
pub(crate) fn offer_again(
    conn: &Connection,
    local_album_id: &str,
    now_ms: i64,
) -> rusqlite::Result<bool> {
    let revision = album_input_revision(conn, local_album_id)?;
    let inserted = conn.execute(
        INSERT_JOB,
        params![
            uuid::Uuid::new_v4().to_string(),
            local_album_id,
            kind_str(IdentifyKind::Historical),
            PRIORITY_HISTORICAL_BACKLOG as i64,
            "queued",
            0_i64,
            now_ms,
            revision,
            idempotency_key(local_album_id, &revision),
            None::<String>,
            None::<String>,
            now_ms,
        ],
    )?;
    if inserted == 1 {
        bump_identification_stream(conn)?;
    }
    Ok(inserted == 1)
}

/// Queue identification of one album because a person asked for it again.
/// A job already waiting for the album takes the album's current files
/// and runs now, even when it was waiting out a backoff; a live job on
/// those files does the work; otherwise a manual job is queued. Runs
/// inside the caller's transaction, so it lands with the write that asked.
pub(crate) fn queue_retry(
    conn: &Connection,
    local_album_id: &str,
    requested_by: Option<&str>,
    now_ms: i64,
) -> rusqlite::Result<()> {
    let revision = album_input_revision(conn, local_album_id)?;
    let key = idempotency_key(local_album_id, &revision);
    let waiting: Option<String> = conn
        .query_row(
            "SELECT id FROM library_identify_jobs \
             WHERE local_album_id = ?1 AND state IN ('queued','deferred') \
             ORDER BY input_revision <> ?2, created_ms LIMIT 1",
            params![local_album_id, revision],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(job_id) = waiting {
        let woken = conn.execute(
            "UPDATE OR IGNORE library_identify_jobs SET input_revision = ?1, \
             idempotency_key = ?2, not_before_ms = MIN(not_before_ms, ?3), \
             priority = MIN(priority, ?5), updated_ms = ?3 WHERE id = ?4",
            params![revision, key, now_ms, job_id, PRIORITY_REVIEW_RETRY as i64],
        )?;
        if woken == 1 {
            return bump_identification_stream(conn);
        }
    }
    let inserted = conn.execute(
        INSERT_JOB,
        params![
            uuid::Uuid::new_v4().to_string(),
            local_album_id,
            "manual",
            PRIORITY_REVIEW_RETRY as i64,
            "queued",
            0_i64,
            now_ms,
            revision,
            key,
            requested_by,
            None::<String>,
            now_ms,
        ],
    )?;
    if inserted == 1 {
        bump_identification_stream(conn)?;
    }
    Ok(())
}

/// Move the `identification` activity revision. Every change to the queue
/// or the review list calls this inside its own transaction, so the
/// revision poller sends `activity.changed` and open tabs refetch.
pub(crate) fn bump_identification_stream(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO library_event_stream_revisions (stream_kind, value) \
         VALUES ('identification', 1) \
         ON CONFLICT (stream_kind) DO UPDATE SET value = value + 1",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_matches_owner_signed_sequence() {
        let ladder: Vec<u64> = (1..=9).map(backoff_secs).collect();
        assert_eq!(ladder, vec![30, 60, 120, 240, 480, 960, 1920, 3840, 7680]);
        assert_eq!(MAX_BACKOFF_SECONDS, 7680);
    }
}
