//! Queue policy: priorities, leases, and the bounded backoff ladder, and
//! the writes that offer an album to the queue inside the caller's
//! transaction.

use rusqlite::{Connection, OptionalExtension as _, params};
use sha2::{Digest as _, Sha256};

use super::models::IdentifyKind;
use super::providers::{ACOUSTID_UNAVAILABLE, MUSICBRAINZ_UNAVAILABLE, RELEASES_UNREADABLE};
pub(crate) use crate::identify::sql::INSERT_JOB;

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

/// How long a job that ran out of deferrals on an outage waits in
/// Attention before it is tried again, by its failure code. Any other code
/// is a real failure and stays in Attention for a person. A release
/// MusicBrainz refuses may be refused for good, so it waits a day.
pub const OUTAGE_WAITS: [(&str, u64); 3] = [
    (MUSICBRAINZ_UNAVAILABLE, MAX_BACKOFF_SECONDS),
    (ACOUSTID_UNAVAILABLE, MAX_BACKOFF_SECONDS),
    (RELEASES_UNREADABLE, 24 * 60 * 60),
];

/// The outside services an attempt showed answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answering {
    pub musicbrainz: bool,
    pub acoustid: bool,
}

impl Answering {
    /// What a probe shows: the job waited out `was` and landed with
    /// `landed`. AcoustID counts only for a probe that needed it.
    pub fn after_probe(was: &str, landed: Option<&str>) -> Self {
        let musicbrainz = landed != Some(MUSICBRAINZ_UNAVAILABLE);
        Self {
            musicbrainz,
            acoustid: musicbrainz
                && was == ACOUSTID_UNAVAILABLE
                && landed != Some(ACOUSTID_UNAVAILABLE),
        }
    }

    fn covers(self, code: &str) -> bool {
        if code == ACOUSTID_UNAVAILABLE {
            self.acoustid
        } else {
            self.musicbrainz
        }
    }
}

/// Each outage code with the latest landing time at which a job waiting
/// it out is due again at `now_ms`. A code `answering` does not cover gets
/// a cutoff no job meets.
pub fn outage_cutoffs(now_ms: u64, answering: Answering) -> [(&'static str, i64); 3] {
    OUTAGE_WAITS.map(|(code, wait_secs)| {
        let cutoff = if answering.covers(code) {
            now_ms as i64 - (wait_secs * 1000) as i64
        } else {
            -1
        };
        (code, cutoff)
    })
}

// ---------------------------------------------------------------------------
// Offering albums to identification, inside the caller's transaction.
// ---------------------------------------------------------------------------

pub(crate) fn idempotency_key(local_album_id: &str, input_revision: &str) -> String {
    format!("{local_album_id}:{input_revision}")
}

/// Content revision of one album: a hash over its indexed members and
/// their stat revisions. Any added, removed, or changed file moves it.
pub fn album_input_revision(conn: &Connection, local_album_id: &str) -> rusqlite::Result<String> {
    let mut stmt = conn.prepare_cached(crate::identify::sql::ALBUM_INPUT_REVISION.text())?;
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
            crate::identify::sql::OLDEST_WAITING_JOB.text(),
            params![local_album_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(job_id) = waiting {
        conn.execute(
            crate::identify::sql::REVISE_WAITING_JOB.text(),
            params![revision, key, now_ms, job_id],
        )?;
        return Ok(false);
    }
    let settled: bool = conn.query_row(
        crate::identify::sql::INPUT_ALREADY_HANDLED.text(),
        params![key],
        |row| row.get(0),
    )?;
    if settled {
        return Ok(false);
    }
    let inserted = conn.execute(
        crate::identify::sql::ADD_OFFERED_JOB.text(),
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
        INSERT_JOB.text(),
        params![
            uuid::Uuid::new_v4().to_string(),
            local_album_id,
            IdentifyKind::Historical,
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
/// A job already waiting for the album takes the album's current files,
/// becomes the person's own and runs now, even when it was waiting out a
/// backoff; a live job on those files does the work; otherwise a manual
/// job is queued. Runs inside the caller's transaction, so it lands with
/// the write that asked.
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
            crate::identify::sql::WAITING_JOB_FOR_RETRY.text(),
            params![local_album_id, revision],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(job_id) = waiting {
        let woken = conn.execute(
            crate::identify::sql::BRING_RETRY_FORWARD.text(),
            params![
                revision,
                key,
                now_ms,
                job_id,
                PRIORITY_REVIEW_RETRY as i64,
                requested_by
            ],
        )?;
        if woken == 1 {
            return bump_identification_stream(conn);
        }
    }
    let inserted = conn.execute(
        INSERT_JOB.text(),
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
    conn.execute(crate::identify::sql::BUMP_IDENTIFICATION_STREAM.text(), [])?;
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
        // No deferral yet waits nothing; attempt ten terminalizes.
        assert_eq!(backoff_secs(0), 0);
        assert!(!terminally_deferred(9));
        assert!(terminally_deferred(10));
    }

    #[test]
    fn a_probe_shows_only_the_services_it_reached() {
        let still_down =
            Answering::after_probe(MUSICBRAINZ_UNAVAILABLE, Some(MUSICBRAINZ_UNAVAILABLE));
        assert!(!still_down.musicbrainz && !still_down.acoustid);
        let musicbrainz = Answering::after_probe(MUSICBRAINZ_UNAVAILABLE, None);
        assert!(musicbrainz.musicbrainz && !musicbrainz.acoustid);
        let acoustid_down =
            Answering::after_probe(ACOUSTID_UNAVAILABLE, Some(ACOUSTID_UNAVAILABLE));
        assert!(acoustid_down.musicbrainz && !acoustid_down.acoustid);
        let both = Answering::after_probe(ACOUSTID_UNAVAILABLE, None);
        assert!(both.musicbrainz && both.acoustid);
        let cutoffs = outage_cutoffs(100_000_000, musicbrainz);
        assert_eq!(
            cutoffs[0],
            (MUSICBRAINZ_UNAVAILABLE, 100_000_000 - 7_680_000)
        );
        assert_eq!(cutoffs[1], (ACOUSTID_UNAVAILABLE, -1));
        assert_eq!(cutoffs[2], (RELEASES_UNREADABLE, 100_000_000 - 86_400_000));
    }
}
