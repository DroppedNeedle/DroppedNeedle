//! Durable identify state over the application database.
//!
//! One store implements every identify port. Accepted identities live in
//! the external-identity tables with their `decision_source`, so manual
//! and legacy decisions outlive rescans, moves, and restarts. The queue
//! and reviews use the 0011 tables; pins and aliases use their 0001
//! tables. Album facts come straight from the catalog the scan writes, so
//! a restart never strands an album without facts. Release documents go
//! in `library_management_metadata_snapshots` (immutable rows, one per
//! distinct payload); expired ones no identity names are pruned on each
//! save.
//!
//! Every call is one short synchronous read or write on the shared
//! database, run on the calling (blocking) thread. Every call answers a
//! `Result`: a read that fails is an error, never an absent row.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use droppedneedle_kernel::db::{Db, DbError, Lane};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use sha2::{Digest as _, Sha256};

use super::artist_links;
use super::merged_ids;
use super::models::{
    AlbumIdentity, Alias, AliasKind, AutomaticSeal, CandidateEvidence, DecisionSource, IdentifyJob,
    IdentifyKind, JobState, LocalAlbumFacts, LocalTrackFacts, MatchFlag, MatchFlagState,
    PriorAlbumIdentity, PriorTrackIdentity, ReviewItem, ReviewState, TrackIdentity,
};
use super::queue::{PRIORITY_NEW_OR_CHANGED, PRIORITY_REVIEW_RETRY};
use super::sources::ArtistAnswer;
use super::sql::{ALBUM_IDENTITY, TRACK_IDENTITY};
use super::stores::{Approval, AttemptLanding, RELEASE_FRESH_SECS, StoreError, land_job};
use crate::corrections::artist_convergence::{
    any_waiting, converge_ids, follow_moved, follow_waiting,
};
use crate::corrections::continuation::{DRAIN_BOUND, Scope, continue_tracks, drain_continuations};
use crate::corrections::release_join::join_replaced;
use crate::identity::{self, AutomaticOutcome, Inputs};
use crate::matching::Release;
use crate::operations::choice::RemapOrigin;

const PROVIDER: &str = "musicbrainz";
/// Snapshot kind for stored release documents.
const RELEASE_KIND: &str = "release";
/// Shape version of the stored document; a new shape is a new input hash.
/// v2 adds MusicBrainz genres: older documents are fetched again.
const RELEASE_DOCUMENT_VERSION: &str = "matching-release-v2";

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

fn now_ms() -> i64 {
    (now_secs() * 1000.0) as i64
}

fn kind_str(kind: IdentifyKind) -> &'static str {
    match kind {
        IdentifyKind::Automatic => "automatic",
        IdentifyKind::Manual => "manual",
        IdentifyKind::Historical => "historical",
    }
}

fn kind_from(raw: &str) -> IdentifyKind {
    match raw {
        "manual" => IdentifyKind::Manual,
        "historical" => IdentifyKind::Historical,
        _ => IdentifyKind::Automatic,
    }
}

fn state_str(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Deferred => "deferred",
        JobState::Succeeded => "succeeded",
        JobState::Failed => "failed",
        JobState::Attention => "attention",
    }
}

fn state_from(raw: &str) -> JobState {
    match raw {
        "running" => JobState::Running,
        "deferred" => JobState::Deferred,
        "succeeded" => JobState::Succeeded,
        "failed" => JobState::Failed,
        "attention" => JobState::Attention,
        _ => JobState::Queued,
    }
}

fn review_state_str(state: ReviewState) -> &'static str {
    match state {
        ReviewState::Pending => "pending",
        ReviewState::Approved => "approved",
        ReviewState::Rejected => "rejected",
        ReviewState::Dismissed => "dismissed",
        ReviewState::Excluded => "excluded",
    }
}

fn review_state_from(raw: &str) -> ReviewState {
    match raw {
        "approved" => ReviewState::Approved,
        "rejected" => ReviewState::Rejected,
        "dismissed" => ReviewState::Dismissed,
        "excluded" => ReviewState::Excluded,
        _ => ReviewState::Pending,
    }
}

fn idempotency_key(local_album_id: &str, input_revision: &str) -> String {
    format!("{local_album_id}:{input_revision}")
}

const JOB_COLUMNS: &str = "id, local_album_id, kind, priority, state, attempts, not_before_ms, \
     input_revision, requested_by_user_id, failure_code, lease_expires_ms";

/// Land a claimed job, only while the claim that ran it still holds it:
/// a lease another worker took over after this one lapsed lands nothing.
const LAND_JOB: &str = "UPDATE library_identify_jobs SET state = ?1, attempts = ?2, \
     not_before_ms = ?3, input_revision = ?4, idempotency_key = ?5, failure_code = ?6, \
     priority = ?7, updated_ms = ?8, lease_expires_ms = NULL \
     WHERE id = ?9 AND state = 'running' AND lease_expires_ms = ?10";

/// Store one identify job. A live job on the same album and input
/// revision (the partial unique index on its key) keeps its place.
const INSERT_JOB: &str = "INSERT INTO library_identify_jobs (id, local_album_id, kind, \
     priority, state, attempts, not_before_ms, input_revision, idempotency_key, \
     requested_by_user_id, failure_code, created_ms, updated_ms) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12) ON CONFLICT DO NOTHING";

fn map_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<IdentifyJob> {
    Ok(IdentifyJob {
        id: row.get(0)?,
        local_album_id: row.get(1)?,
        kind: kind_from(&row.get::<_, String>(2)?),
        priority: row.get::<_, i64>(3)? as u32,
        state: state_from(&row.get::<_, String>(4)?),
        attempts: row.get::<_, i64>(5)? as u32,
        not_before_ms: row.get::<_, i64>(6)?.max(0) as u64,
        input_revision: row.get(7)?,
        requested_by_user_id: row.get(8)?,
        failure_code: row.get(9)?,
        lease_expires_ms: row.get::<_, Option<i64>>(10)?.map(|at| at.max(0) as u64),
    })
}

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
            kind_str(IdentifyKind::Manual),
            PRIORITY_REVIEW_RETRY as i64,
            state_str(JobState::Queued),
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

/// SQLite identify store.
pub struct SqliteIdentifyStore {
    db: Db,
    /// Where the sweep of the app's artist links goes on from.
    link_cursor: Mutex<String>,
    /// When an album sweep last found nothing to do, and this store's
    /// write count then.
    swept: Mutex<Option<Swept>>,
    /// When the sweep last worked off continued tracks, and whether some
    /// were left.
    drained: Mutex<Option<Drained>>,
    /// Writes through this store that changed a row. A match changed
    /// through this store wakes the album sweep at once.
    writes: AtomicU64,
}

/// The last drain of continued tracks.
#[derive(Clone, Copy)]
struct Drained {
    more: bool,
    at: Instant,
}

/// Turns one sweep spends on continued tracks before it lets the queue
/// claim: a backlog is worked off over the following sweeps.
const DRAIN_TURNS: usize = 10;

/// After an album sweep finds nothing stale, the next one waits this long
/// unless this store changed something. Plays, sessions and scans write
/// all the time and seldom change a match; a match changed through this
/// store wakes the sweep at once.
const SWEEP_FLOOR: Duration = Duration::from_secs(60);

/// An album sweep that found nothing stale.
#[derive(Clone, Copy)]
struct Swept {
    /// This store's write count when the sweep read the stale albums.
    writes: u64,
    at: Instant,
}

/// Run `op` on `tx` and say whether it changed a row, so the album
/// sweep's quiet check counts only writes that did.
fn changing<T>(
    tx: &Transaction<'_>,
    op: impl FnOnce(&Transaction<'_>) -> Result<T, StoreError>,
) -> Result<(T, bool), StoreError> {
    let before = tx.total_changes();
    let value = op(tx)?;
    Ok((value, tx.total_changes() != before))
}

impl SqliteIdentifyStore {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            link_cursor: Mutex::new(String::new()),
            swept: Mutex::new(None),
            drained: Mutex::new(None),
            writes: AtomicU64::new(0),
        }
    }

    /// A committed write's value; one that changed a row is counted.
    fn landed<T>(&self, (value, changed): (T, bool)) -> T {
        if changed {
            self.writes.fetch_add(1, Ordering::Relaxed);
        }
        value
    }

    /// Queue an identification for one album the catalog holds, reusing a
    /// live job on the same revision. `true` when a job was queued.
    pub fn offer(&self, local_album_id: &str) -> Result<bool, StoreError> {
        self.db
            .write_blocking(Lane::Background, "identify.offer", |tx| {
                changing(tx, |tx| Ok(offer_album(tx, local_album_id, now_ms())?))
            })
            .map(|written| self.landed(written))
    }

    /// Current input revision of one album.
    pub fn input_revision(&self, local_album_id: &str) -> Result<String, StoreError> {
        self.db.read_blocking("identify.input_revision", |conn| {
            Ok(album_input_revision(conn, local_album_id)?)
        })
    }
}

/// One album's match, on `conn`.
pub(crate) fn read_album_identity(
    conn: &Connection,
    local_album_id: &str,
) -> rusqlite::Result<Option<AlbumIdentity>> {
    conn.prepare_cached(ALBUM_IDENTITY.text())?
        .query_row(params![local_album_id, PROVIDER], |row| {
            Ok(AlbumIdentity {
                local_album_id: local_album_id.to_owned(),
                provider: PROVIDER.to_owned(),
                release_group_mbid: Some(row.get(0)?),
                release_mbid: row.get(1)?,
                decision_source: row.get(2)?,
                row_revision: row.get::<_, i64>(3)? as u64,
            })
        })
        .optional()
}

/// One track's match, on `conn`.
pub(crate) fn read_track_identity(
    conn: &Connection,
    local_track_id: &str,
) -> rusqlite::Result<Option<TrackIdentity>> {
    conn.prepare_cached(TRACK_IDENTITY.text())?
        .query_row(params![local_track_id, PROVIDER], |row| {
            Ok(TrackIdentity {
                local_track_id: local_track_id.to_owned(),
                provider: PROVIDER.to_owned(),
                recording_mbid: Some(row.get(0)?),
                release_track_mbid: row.get(1)?,
                decision_source: row.get(2)?,
                row_revision: row.get::<_, i64>(3)? as u64,
            })
        })
        .optional()
}

/// Durable identity rows plus the facts the matcher reads.
impl SqliteIdentifyStore {
    pub fn album_identity(
        &self,
        local_album_id: &str,
    ) -> Result<Option<AlbumIdentity>, StoreError> {
        self.db.read_blocking("identify.album_identity", |conn| {
            Ok(read_album_identity(conn, local_album_id)?)
        })
    }

    /// Save an album's match, never an automatic or embedded row over a
    /// person's.
    pub fn save_album_identity(&self, identity: AlbumIdentity) -> Result<(), StoreError> {
        self.db
            .write_blocking(Lane::Background, "identify.save_album_identity", |tx| {
                changing(tx, |tx| {
                    crate::identity::save_album(tx, &identity, now_secs())?;
                    Ok(())
                })
            })
            .map(|written| self.landed(written))
    }

    pub fn track_identity(
        &self,
        local_track_id: &str,
    ) -> Result<Option<TrackIdentity>, StoreError> {
        self.db.read_blocking("identify.track_identity", |conn| {
            Ok(read_track_identity(conn, local_track_id)?)
        })
    }

    /// Save a track's match, guarded like the album's, and carry what
    /// people made of the album's tracks over to the new mapping.
    pub fn save_track_identity(&self, identity: TrackIdentity) -> Result<(), StoreError> {
        self.db
            .write_blocking(Lane::Background, "identify.save_track_identity", |tx| {
                changing(tx, |tx| {
                    if crate::identity::save_track(tx, &identity, now_secs())? == 1 {
                        let album: String = tx.query_row(
                            "SELECT local_album_id FROM local_tracks WHERE id = ?1",
                            params![identity.local_track_id],
                            |row| row.get(0),
                        )?;
                        continue_tracks(tx, Scope::Album(&album), now_secs())?;
                    }
                    Ok(())
                })
            })
            .map(|written| self.landed(written))
    }

    /// Seal an automatic match on the album's files as they are now, as
    /// [`identity::seal_automatic`] does. `false` when a person's match
    /// holds the album (nothing is written).
    pub fn seal_automatic(&self, seal: &AutomaticSeal) -> Result<bool, StoreError> {
        self.db
            .write_blocking(Lane::Background, "identify.seal_automatic", |tx| {
                changing(tx, |tx| {
                    let revision = album_input_revision(tx, &seal.local_album_id)?;
                    let inputs = Inputs {
                        input_revision: &revision,
                        now: now_secs(),
                    };
                    Ok(identity::seal_automatic(tx, seal, inputs)? == AutomaticOutcome::Done)
                })
            })
            .map(|sealed| self.landed(sealed))
    }

    /// Seal identification's match for a claimed job and land the job, in
    /// one transaction. `input_revision` is the one the job's facts were
    /// read at: when the files moved since, nothing is sealed, the album is
    /// offered again and the job lands as `INPUT_MOVED`. `None` when the
    /// job's lease was lost: nothing is written.
    pub fn seal_and_land(
        &self,
        seal: &AutomaticSeal,
        input_revision: &str,
        job: &IdentifyJob,
    ) -> Result<Option<(AutomaticOutcome, IdentifyJob)>, StoreError> {
        self.land_with("identify.seal_automatic", job, |tx| {
            identity::seal_automatic(
                tx,
                seal,
                Inputs {
                    input_revision,
                    now: now_secs(),
                },
            )
        })
    }

    /// Settle an album identification matched to nothing and land the job,
    /// in one transaction ([`identity::retract`]); as
    /// [`seal_and_land`](Self::seal_and_land) for moved files and a lost
    /// lease.
    pub fn retract_and_land(
        &self,
        local_album_id: &str,
        contradicted: bool,
        flag: &MatchFlag,
        input_revision: &str,
        job: &IdentifyJob,
    ) -> Result<Option<(AutomaticOutcome, IdentifyJob)>, StoreError> {
        self.land_with("identify.retract", job, |tx| {
            identity::retract(
                tx,
                local_album_id,
                contradicted,
                flag,
                Inputs {
                    input_revision,
                    now: now_secs(),
                },
            )
        })
    }

    /// Run `work` and land `job` after it on one transaction. Files that
    /// moved land the job as `INPUT_MOVED`, a terminal failure: the job
    /// the move queued takes over. A lost lease rolls `work` back too.
    fn land_with(
        &self,
        label: &'static str,
        job: &IdentifyJob,
        work: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<AutomaticOutcome>,
    ) -> Result<Option<(AutomaticOutcome, IdentifyJob)>, StoreError> {
        let landed = self.db.write_blocking(Lane::Background, label, |tx| {
            let before = tx.total_changes();
            let outcome = work(tx)?;
            let mut landed = job.clone();
            if outcome == AutomaticOutcome::InputMoved {
                land_job(
                    &mut landed,
                    AttemptLanding::Failed,
                    now_ms().max(0) as u64,
                    Some("INPUT_MOVED"),
                );
            }
            land_tx(tx, &landed)?;
            Ok(((outcome, landed), tx.total_changes() != before))
        });
        match landed {
            Ok(written) => Ok(Some(self.landed(written))),
            Err(Landing::LeaseLost) => Ok(None),
            Err(Landing::Store(error)) => Err(error),
        }
    }

    /// The album's match flag, if identification is unsure of it.
    pub fn match_flag(&self, local_album_id: &str) -> Result<Option<MatchFlag>, StoreError> {
        self.db.read_blocking("identify.match_flag", |conn| {
            Ok(conn
                .query_row(
                    "SELECT state, reason_code, release_mbid, candidates_json \
                     FROM library_album_match_state WHERE local_album_id = ?1",
                    params![local_album_id],
                    |row| {
                        let state: String = row.get(0)?;
                        let candidates: String = row.get(3)?;
                        Ok(MatchFlag {
                            state: if state == "unmatched" {
                                MatchFlagState::Unmatched
                            } else {
                                MatchFlagState::Unconfirmed
                            },
                            reason_code: row.get(1)?,
                            release_mbid: row.get(2)?,
                            candidates: serde_json::from_str(&candidates).map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    3,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?,
                        })
                    },
                )
                .optional()?)
        })
    }
}

/// Why a landing wrote nothing.
enum Landing {
    Store(StoreError),
    /// Another worker holds the job now.
    LeaseLost,
}

impl std::fmt::Display for Landing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => error.fmt(f),
            Self::LeaseLost => write!(f, "identify job's lease was taken over"),
        }
    }
}

impl From<DbError> for Landing {
    fn from(error: DbError) -> Self {
        Self::Store(error.into())
    }
}

impl From<rusqlite::Error> for Landing {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.into())
    }
}

/// Land a claimed job on `tx`, under the lease it was claimed with.
fn land_tx(tx: &Transaction<'_>, job: &IdentifyJob) -> Result<(), Landing> {
    let key = idempotency_key(&job.local_album_id, &job.input_revision);
    let landed = tx.prepare_cached(LAND_JOB)?.execute(params![
        state_str(job.state),
        job.attempts as i64,
        job.not_before_ms as i64,
        job.input_revision,
        key,
        job.failure_code,
        job.priority as i64,
        now_ms(),
        job.id,
        job.lease_expires_ms.map(|at| at as i64),
    ])?;
    if landed != 1 {
        tracing::warn!(
            job_id = job.id,
            album_id = job.local_album_id,
            "identify job's lease was taken over; this attempt lands nothing"
        );
        return Err(Landing::LeaseLost);
    }
    bump_identification_stream(tx)?;
    Ok(())
}

/// The raw album identity row, every column the undo puts back.
pub fn album_identity_row(
    conn: &Connection,
    local_album_id: &str,
) -> rusqlite::Result<Option<PriorAlbumIdentity>> {
    conn.query_row(
        "SELECT release_group_mbid, release_mbid, decision_source, matcher_version, \
         attempt_id, selected_by_user_id, selected_at, row_revision \
         FROM local_album_external_identities WHERE local_album_id = ?1 AND provider = ?2",
        params![local_album_id, PROVIDER],
        |row| {
            Ok(PriorAlbumIdentity {
                release_group_mbid: row.get(0)?,
                release_mbid: row.get(1)?,
                decision_source: row.get(2)?,
                matcher_version: row.get(3)?,
                attempt_id: row.get(4)?,
                selected_by_user_id: row.get(5)?,
                selected_at: row.get(6)?,
                row_revision: row.get(7)?,
            })
        },
    )
    .optional()
}

/// The raw identity rows of the album's indexed tracks, by track id.
pub fn track_identity_rows(
    conn: &Connection,
    local_album_id: &str,
) -> rusqlite::Result<Vec<PriorTrackIdentity>> {
    let mut stmt = conn.prepare(
        "SELECT i.local_track_id, i.recording_mbid, i.release_mbid, i.release_track_mbid, \
         i.medium_position, i.release_track_position, i.decision_source, i.attempt_id, \
         i.selected_at FROM local_track_external_identities i \
         JOIN local_tracks t ON t.id = i.local_track_id \
         WHERE t.local_album_id = ?1 AND t.availability = 'indexed' AND i.provider = ?2 \
         ORDER BY i.local_track_id",
    )?;
    stmt.query_map(params![local_album_id, PROVIDER], |row| {
        Ok(PriorTrackIdentity {
            local_track_id: row.get(0)?,
            recording_mbid: row.get(1)?,
            release_mbid: row.get(2)?,
            release_track_mbid: row.get(3)?,
            medium_position: row.get(4)?,
            release_track_position: row.get(5)?,
            decision_source: row.get(6)?,
            attempt_id: row.get(7)?,
            selected_at: row.get(8)?,
        })
    })?
    .collect()
}

/// Local album facts the matcher reads, from the catalog the scan
/// writes.
impl SqliteIdentifyStore {
    /// `None` means the album does not exist.
    pub fn album_facts(&self, local_album_id: &str) -> Result<Option<LocalAlbumFacts>, StoreError> {
        self.db.read_blocking("identify.album_facts", |conn| {
            let album = conn
                .query_row(
                    "SELECT title, COALESCE(album_artist_name, ''), is_compilation, year \
                     FROM local_albums WHERE id = ?1",
                    params![local_album_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)? != 0,
                            row.get::<_, Option<i64>>(3)?,
                        ))
                    },
                )
                .optional()?;
            let Some((title, album_artist_name, is_compilation, year)) = album else {
                return Ok(None);
            };
            // In the same read as the facts, so a seal can tell whether the
            // files moved after they were read.
            let input_revision = album_input_revision(conn, local_album_id)?;
            let mut stmt = conn.prepare(
                "SELECT t.id, t.title, COALESCE(t.artist_name, ''), t.track_number, \
                 t.disc_number, t.duration_seconds, t.embedded_recording_mbid, \
                 t.embedded_release_track_mbid, t.embedded_release_mbid, \
                 t.embedded_release_group_mbid, t.membership_locked, t.root_id, \
                 t.relative_path, t.stat_revision, h.media, h.barcode, h.catalog_number, \
                 h.release_country, h.total_discs \
                 FROM local_tracks t LEFT JOIN local_track_edition_tags h \
                 ON h.local_track_id = t.id \
                 WHERE t.local_album_id = ?1 AND t.availability = 'indexed' \
                 ORDER BY t.disc_number, t.track_number, t.relative_path",
            )?;
            let mut tracks = Vec::new();
            let mut locked = Vec::new();
            let rows = stmt.query_map(params![local_album_id], |row| {
                Ok((
                    LocalTrackFacts {
                        local_track_id: row.get(0)?,
                        title: row.get(1)?,
                        artist_name: row.get(2)?,
                        track_number: row.get::<_, i64>(3)?.max(0) as u32,
                        disc_number: row.get::<_, i64>(4)?.max(0) as u32,
                        duration_secs: row
                            .get::<_, Option<f64>>(5)?
                            .map(|seconds| seconds.max(0.0) as u64),
                        recording_mbid: row.get(6)?,
                        release_track_mbid: row.get(7)?,
                        release_mbid: row.get(8)?,
                        release_group_mbid: row.get(9)?,
                        root_id: row.get(11)?,
                        relative_path: row.get(12)?,
                        stat_revision: row.get(13)?,
                        duration_exact: row
                            .get::<_, Option<f64>>(5)?
                            .filter(|seconds| *seconds > 0.0),
                        media: row.get(14)?,
                        barcode: row.get(15)?,
                        catalog_number: row.get(16)?,
                        release_country: row.get(17)?,
                        total_discs: row
                            .get::<_, Option<i64>>(18)?
                            .and_then(|total| u32::try_from(total).ok()),
                    },
                    row.get::<_, i64>(10)? != 0,
                ))
            })?;
            for row in rows {
                let (track, membership_locked) = row?;
                if membership_locked {
                    locked.push(track.local_track_id.clone());
                }
                tracks.push(track);
            }
            Ok(Some(LocalAlbumFacts {
                local_album_id: local_album_id.to_owned(),
                title,
                album_artist_name,
                year: year.and_then(|year| i32::try_from(year).ok()),
                tracks,
                locked_track_ids: locked,
                is_compilation,
                input_revision,
            }))
        })
    }
}

/// Version tag for stored prints: the Chromaprint pipeline in
/// `tags::fingerprint` (test2 preset, 120 s window).
///
/// `audio_fingerprint_outcomes` references `local_tracks` with
/// `ON DELETE RESTRICT`: a future purge of missing tracks must delete a
/// track's rows here before the track itself.
const FINGERPRINTER_VERSION: &str = "chromaprint-test2-120s";

/// Fingerprints already taken, keyed by track and its file's stat
/// revision, so an unchanged file is never decoded twice.
impl SqliteIdentifyStore {
    /// The stored print and its duration in seconds.
    pub fn fingerprint(
        &self,
        local_track_id: &str,
        stat_revision: &str,
    ) -> Result<Option<(String, u32)>, StoreError> {
        self.db.read_blocking("identify.fingerprint", |conn| {
            Ok(conn
                .query_row(
                    "SELECT fingerprint, duration_seconds FROM audio_fingerprint_outcomes \
                     WHERE local_track_id = ?1 AND stat_revision = ?2 \
                     AND fingerprinter_version = ?3 AND fingerprint IS NOT NULL",
                    params![local_track_id, stat_revision, FINGERPRINTER_VERSION],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<f64>>(1)?.unwrap_or(0.0).max(0.0) as u32,
                        ))
                    },
                )
                .optional()?)
        })
    }

    /// Keep a print; `matched` says whether AcoustID knew it.
    pub fn save_fingerprint(
        &self,
        local_track_id: &str,
        stat_revision: &str,
        fingerprint: &str,
        duration_seconds: u32,
        matched: bool,
    ) -> Result<(), StoreError> {
        let now = now_secs();
        self.db
            .write_blocking(Lane::Background, "identify.save_fingerprint", |tx| {
                changing(tx, |tx| {
                    // One print per track: older file revisions are stale.
                    tx.execute(
                        "DELETE FROM audio_fingerprint_outcomes WHERE local_track_id = ?1 \
                         AND (stat_revision != ?2 OR fingerprinter_version != ?3)",
                        params![local_track_id, stat_revision, FINGERPRINTER_VERSION],
                    )?;
                    tx.execute(
                        "INSERT INTO audio_fingerprint_outcomes (id, local_track_id, \
                         stat_revision, fingerprinter_version, state, fingerprint, \
                         duration_seconds, first_attempt_at, last_attempt_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8) \
                         ON CONFLICT (local_track_id, stat_revision, fingerprinter_version) \
                         DO UPDATE SET state = excluded.state, \
                         fingerprint = excluded.fingerprint, \
                         duration_seconds = excluded.duration_seconds, \
                         last_attempt_at = excluded.last_attempt_at, \
                         attempt_count = attempt_count + 1",
                        params![
                            hex_sha256(
                                format!(
                                    "{local_track_id}\0{stat_revision}\0{FINGERPRINTER_VERSION}"
                                )
                                .as_bytes()
                            ),
                            local_track_id,
                            stat_revision,
                            FINGERPRINTER_VERSION,
                            if matched { "matched" } else { "no_match" },
                            fingerprint,
                            f64::from(duration_seconds),
                            now,
                        ],
                    )?;
                    Ok(())
                })
            })
            .map(|written| self.landed(written))
    }
}

/// Work out what album `album`'s match proves about its artists and settle
/// their links; with `converge`, also merge what the links prove to be one
/// artist. Either way, a row the pass left empty follows its credits.
fn artist_pass(tx: &Transaction<'_>, album: &str, converge: bool) -> Result<(), StoreError> {
    let now = now_ms();
    let linked = artist_links::link_album(tx, album, now)?;
    let now_secs = now as f64 / 1000.0;
    if converge && !linked.ids.is_empty() {
        converge_ids(tx, &linked.ids, now_secs)?;
    }
    follow_moved(tx, &linked.moved, now_secs)?;
    Ok(())
}

/// Artists linked to MusicBrainz from their album's accepted match
/// (`artist_links`).
impl SqliteIdentifyStore {
    /// Work out what the album's match proves about its artists and settle
    /// their links, with no merge of rows the links prove to be one
    /// artist; a row the pass left empty still follows its credits.
    /// Library Management runs this before it plans an album.
    pub fn link_album_artists(&self, local_album_id: &str) -> Result<(), StoreError> {
        self.db
            .write_blocking(Lane::Background, "identify.link_album_artists", |tx| {
                changing(tx, |tx| artist_pass(tx, local_album_id, false))
            })
            .map(|written| self.landed(written))
    }

    /// [`Self::link_album_artists`], then merge what the links it made
    /// prove to be one artist. Identification runs this after each
    /// attempt and for each album a sweep finds stale.
    pub fn link_and_merge_album_artists(&self, local_album_id: &str) -> Result<(), StoreError> {
        self.db
            .write_blocking(
                Lane::Background,
                "identify.link_and_merge_album_artists",
                |tx| changing(tx, |tx| artist_pass(tx, local_album_id, true)),
            )
            .map(|written| self.landed(written))
    }

    /// One sweep: up to [`artist_links::SWEEP_BATCH`] albums whose match
    /// or track mappings changed since their last pass, each in its own
    /// transaction, then up to as many of the app's links, in id order
    /// from where the last sweep stopped, each settled again in its own
    /// transaction. Continued tracks are worked off first. An album or a
    /// link that fails is logged and the sweep goes on; a busy or closing
    /// database stops it, and the next sweep picks up from there.
    pub fn sweep_artist_links(&self) -> Result<(), StoreError> {
        match self.drain_continuations() {
            Ok(()) => {}
            Err(error @ (StoreError::Busy(_) | StoreError::Closed)) => return Err(error),
            Err(error) => tracing::warn!(
                %error,
                "continued tracks not worked off; a later sweep tries again"
            ),
        }
        // Read before the stale albums, so a write that lands meanwhile
        // wakes the next sweep.
        let writes = self.writes.load(Ordering::Relaxed);
        let quiet = self
            .swept_guard()
            .is_some_and(|last| last.writes == writes && last.at.elapsed() < SWEEP_FLOOR);
        if !quiet {
            let stale = self.db.read_blocking("identify.stale_albums", |conn| {
                Ok::<_, StoreError>(artist_links::stale_albums(conn, artist_links::SWEEP_BATCH)?)
            })?;
            for album in &stale {
                match self.link_and_merge_album_artists(album) {
                    Ok(()) => {}
                    Err(error @ (StoreError::Busy(_) | StoreError::Closed)) => return Err(error),
                    Err(error) => tracing::warn!(
                        %error,
                        album_id = album,
                        "the album's artist links could not be worked out; a later sweep tries again"
                    ),
                }
            }
            let any_waiting = self.db.read_blocking("identify.any_waiting", |conn| {
                Ok::<_, StoreError>(any_waiting(conn)?)
            })?;
            let waiting = if any_waiting {
                self.db
                    .write_blocking(Lane::Background, "identify.follow_waiting", |tx| {
                        changing(tx, |tx| {
                            follow_waiting(tx, now_ms() as f64 / 1000.0)?;
                            Ok(())
                        })
                    })
                    .map(|written| self.landed(written))
            } else {
                Ok(())
            };
            match waiting {
                Ok(()) => {}
                Err(error @ (StoreError::Busy(_) | StoreError::Closed)) => return Err(error),
                Err(error) => tracing::warn!(
                    %error,
                    "rows waiting to follow their artist credits not looked at; a later sweep tries again"
                ),
            }
            *self.swept_guard() = stale.is_empty().then(|| Swept {
                writes,
                at: Instant::now(),
            });
        }
        let cursor = self.cursor_guard().clone();
        let (rows, due) = self.db.read_blocking("identify.linked_artists", |conn| {
            let rows =
                artist_links::automatic_links_after(conn, &cursor, artist_links::SWEEP_BATCH)?;
            let mut due = Vec::new();
            for artist in &rows {
                if !artist_links::plan(conn, std::slice::from_ref(artist))?.is_empty() {
                    due.push(artist.clone());
                }
            }
            Ok::<_, StoreError>((rows, due))
        })?;
        *self.cursor_guard() = match rows.last() {
            Some(last) if rows.len() == artist_links::SWEEP_BATCH => last.clone(),
            _ => String::new(),
        };
        let mut settled = cursor;
        for artist in rows {
            if due.contains(&artist) {
                let one = std::slice::from_ref(&artist);
                let outcome = self
                    .db
                    .write_blocking(Lane::Background, "identify.settle_artist_link", |tx| {
                        changing(tx, |tx| {
                            artist_links::settle(tx, one, now_ms())?;
                            Ok(())
                        })
                    })
                    .map(|written| self.landed(written));
                match outcome {
                    Ok(()) => {}
                    Err(error @ (StoreError::Busy(_) | StoreError::Closed)) => {
                        // The rest of this batch goes first next time.
                        *self.cursor_guard() = settled;
                        return Err(error);
                    }
                    Err(error) => tracing::warn!(
                        %error,
                        artist_id = artist,
                        "an artist link could not be settled; a later sweep tries again"
                    ),
                }
            }
            settled = artist;
        }
        Ok(())
    }

    /// Up to `limit` artist ids waiting for MusicBrainz's answer, newest
    /// first.
    pub fn pending_artist_ids(&self, limit: usize) -> Result<Vec<String>, StoreError> {
        self.db
            .read_blocking("identify.pending_artist_ids", |conn| {
                Ok(merged_ids::pending(conn, limit)?)
            })
    }

    /// Write MusicBrainz's answer about artist id `mbid` in one
    /// transaction, then merge the records it proves to be one artist.
    pub fn answer_artist_id(&self, mbid: &str, answer: &ArtistAnswer) -> Result<(), StoreError> {
        self.db
            .write_blocking(Lane::Background, "identify.answer_artist_id", |tx| {
                changing(tx, |tx| {
                    let now = now_ms();
                    let ids = merged_ids::answer(tx, mbid, answer, now)?;
                    if !ids.is_empty() {
                        converge_ids(tx, &ids, now as f64 / 1000.0)?;
                    }
                    Ok(())
                })
            })
            .map(|written| self.landed(written))
    }

    fn swept_guard(&self) -> MutexGuard<'_, Option<Swept>> {
        self.swept
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn cursor_guard(&self) -> MutexGuard<'_, String> {
        self.link_cursor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Work off what continued tracks still hold, in turns of
    /// [`DRAIN_BOUND`] aliases each in its own transaction: writes move
    /// only a bounded number themselves, and a v2 import can leave many.
    /// While some are left every sweep goes on; once none are, it looks
    /// again after [`SWEEP_FLOOR`].
    fn drain_continuations(&self) -> Result<(), StoreError> {
        let due = self
            .drained_guard()
            .is_none_or(|last| last.more || last.at.elapsed() >= SWEEP_FLOOR);
        if !due {
            return Ok(());
        }
        let mut more = true;
        let mut outcome = Ok(());
        for _ in 0..DRAIN_TURNS {
            let turn = self
                .db
                .write_blocking(Lane::Background, "identify.drain_continuations", |tx| {
                    changing(tx, |tx| {
                        Ok(drain_continuations(tx, DRAIN_BOUND, now_secs())?)
                    })
                })
                .map(|written| self.landed(written));
            match turn {
                Ok(left) => more = left,
                Err(error) => {
                    outcome = Err(error);
                    break;
                }
            }
            if !more {
                break;
            }
        }
        *self.drained_guard() = Some(Drained {
            more,
            at: Instant::now(),
        });
        outcome
    }

    fn drained_guard(&self) -> MutexGuard<'_, Option<Drained>> {
        self.drained
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Release documents identification fetched or sealed against. Recall
/// reuses fresh ones instead of asking MusicBrainz again, and the
/// publisher tags files from the one an album's identity names.
impl SqliteIdentifyStore {
    /// The stored document for one release, if it was saved no more than
    /// `max_age_secs` ago (`None` accepts any age).
    pub fn release(
        &self,
        release_mbid: &str,
        max_age_secs: Option<u64>,
    ) -> Result<Option<Release>, StoreError> {
        let oldest = max_age_secs.map_or(0.0, |age| now_secs() - age as f64);
        self.db.read_blocking("identify.release", |conn| {
            Ok(stored_release(conn, release_mbid, oldest)?)
        })
    }

    /// Keep a release document, on the caller's lane.
    pub fn save_release(&self, release: &Release, lane: Lane) -> Result<(), StoreError> {
        let canonical = release.id.trim().to_ascii_lowercase();
        self.db
            .write_blocking(lane, "identify.save_release", |tx| {
                changing(tx, |tx| {
                    let stored = stored_release(tx, &canonical, 0.0)?;
                    store_release(tx, release, stored, now_secs())
                })
            })
            .map(|written| self.landed(written))
    }
}

/// Write `release`'s document under its own id and every merged id that
/// redirects to it, then drop expired documents nothing names. `stored`
/// is the document on file for its id, whose merged ids are kept: a
/// refetch by the canonical id carries no redirect, so it would forget
/// the merged ids an earlier lookup learned.
fn store_release(
    tx: &Transaction<'_>,
    release: &Release,
    stored: Option<Release>,
    now: f64,
) -> Result<(), StoreError> {
    let canonical = release.id.trim().to_ascii_lowercase();
    let mut release = release.clone();
    if let Some(stored) = stored {
        for old in stored.old_ids {
            if !release
                .old_ids
                .iter()
                .any(|known| known.eq_ignore_ascii_case(&old))
            {
                release.old_ids.push(old);
            }
        }
    }
    let payload = serde_json::to_string(&release).map_err(|error| {
        StoreError::Failed(format!(
            "release document {} did not encode: {error}",
            release.id
        ))
    })?;
    let payload_sha = hex_sha256(payload.as_bytes());
    // The release's own id plus every merged id that redirects to it:
    // each gets a row with the same payload, so a lookup by any of them
    // stays a plain indexed match on entity_id.
    let mut entities = vec![canonical];
    for old in &release.old_ids {
        let old = old.trim().to_ascii_lowercase();
        if !old.is_empty() && !entities.contains(&old) {
            entities.push(old);
        }
    }
    for entity in &entities {
        // Rows are immutable, so a refetch replaces the old row.
        tx.execute(
            "DELETE FROM library_management_metadata_snapshots \
             WHERE provider = ?1 AND entity_kind = ?2 AND entity_id = ?3",
            params![PROVIDER, RELEASE_KIND, entity],
        )?;
        tx.execute(
            "INSERT INTO library_management_metadata_snapshots (id, provider, \
             entity_kind, entity_id, input_hash, canonical_payload_json, payload_sha256, \
             fetched_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                hex_sha256(format!("{entity}\0{payload_sha}").as_bytes()),
                PROVIDER,
                RELEASE_KIND,
                entity,
                release_input_hash(),
                payload,
                payload_sha,
                now,
                now + RELEASE_FRESH_SECS as f64,
            ],
        )?;
    }
    // Expired documents stay while an identity names them or a pending
    // review offers them to a curator. Merged-id rows share their
    // document's payload hash, so they go and stay with it.
    tx.execute(
        "DELETE FROM library_management_metadata_snapshots \
         WHERE provider = ?1 AND entity_kind = ?2 AND expires_at < ?3 \
         AND payload_sha256 NOT IN (SELECT kept.payload_sha256 \
         FROM library_management_metadata_snapshots AS kept \
         WHERE kept.provider = ?1 AND kept.entity_kind = ?2 AND kept.entity_id IN ( \
         SELECT lower(release_mbid) FROM local_album_external_identities \
         WHERE release_mbid IS NOT NULL \
         UNION SELECT lower(json_extract(candidate.value, '$.release_mbid')) \
         FROM library_identify_reviews, json_each(candidates_json) AS candidate \
         WHERE state = ?4 AND json_extract(candidate.value, '$.release_mbid') IS NOT NULL))",
        params![
            PROVIDER,
            RELEASE_KIND,
            now,
            review_state_str(ReviewState::Pending)
        ],
    )?;
    Ok(())
}

impl SqliteIdentifyStore {
    /// Graft fetched parts (credits, artist names) onto the stored
    /// document for a release, in one transaction ([`rewrite_document`]).
    /// Returns the grafted document, or `None` when none is stored.
    pub fn graft_extras(
        &self,
        release_mbid: &str,
        fetched: &crate::matching::model::ReleaseExtras,
    ) -> Result<Option<Release>, StoreError> {
        self.db
            .write_blocking(Lane::Background, "identify.graft_extras", |tx| {
                changing(tx, |tx| {
                    Ok(rewrite_document(tx, release_mbid, |release| {
                        release.graft_extras(fetched)
                    })?)
                })
            })
            .map(|written| self.landed(written))
    }

    /// Add the merged ids `fetched` answers to that the stored document of
    /// its release lacks, so a lookup by a merged id finds it. The stored
    /// document is otherwise kept as it is ([`rewrite_document`]). `false`
    /// when none is stored.
    pub fn keep_old_ids(&self, fetched: &Release, lane: Lane) -> Result<bool, StoreError> {
        self.db
            .write_blocking(lane, "identify.keep_old_ids", |tx| {
                changing(tx, |tx| {
                    Ok(rewrite_document(tx, &fetched.id, |stored| {
                        for old in &fetched.old_ids {
                            let old = old.trim().to_ascii_lowercase();
                            if !old.is_empty() && !stored.answers_to(&old) {
                                stored.old_ids.push(old);
                            }
                        }
                    })?
                    .is_some())
                })
            })
            .map(|written| self.landed(written))
    }
}

/// Change the stored document for release `release_mbid` with `edit` and
/// write it back under every id that shared its payload and every id it
/// answers to (merged ids too), keeping when it was fetched and when it
/// expires. `None` when no document is stored or it does not code.
fn rewrite_document(
    tx: &rusqlite::Transaction<'_>,
    release_mbid: &str,
    edit: impl FnOnce(&mut Release),
) -> rusqlite::Result<Option<Release>> {
    let entity = release_mbid.trim().to_ascii_lowercase();
    let current: Option<(String, String, f64, Option<f64>)> = tx
        .query_row(
            "SELECT canonical_payload_json, payload_sha256, fetched_at, expires_at \
             FROM library_management_metadata_snapshots \
             WHERE provider = ?1 AND entity_kind = ?2 AND entity_id = ?3 \
             AND input_hash = ?4 ORDER BY fetched_at DESC LIMIT 1",
            params![PROVIDER, RELEASE_KIND, entity, release_input_hash()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((payload, old_sha, fetched_at, expires_at)) = current else {
        return Ok(None);
    };
    let mut release: Release = match serde_json::from_str(&payload) {
        Ok(release) => release,
        Err(error) => {
            tracing::warn!(%error, release_mbid, "stored release document did not decode");
            return Ok(None);
        }
    };
    edit(&mut release);
    let payload = match serde_json::to_string(&release) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, release_mbid, "release document did not encode");
            return Ok(None);
        }
    };
    let payload_sha = hex_sha256(payload.as_bytes());
    let mut rows: Vec<(String, String, f64, Option<f64>)> = {
        let mut statement = tx.prepare(
            "SELECT entity_id, input_hash, fetched_at, expires_at \
             FROM library_management_metadata_snapshots \
             WHERE provider = ?1 AND entity_kind = ?2 AND payload_sha256 = ?3",
        )?;
        statement
            .query_map(params![PROVIDER, RELEASE_KIND, old_sha], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?
    };
    let answers: Vec<String> = std::iter::once(&release.id)
        .chain(&release.old_ids)
        .map(|id| id.trim().to_ascii_lowercase())
        .filter(|id| !id.is_empty())
        .collect();
    for id in answers {
        if !rows.iter().any(|(known, ..)| *known == id) {
            rows.push((id, release_input_hash(), fetched_at, expires_at));
        }
    }
    // Rows are immutable, so each is replaced, along with any other
    // document an id it now answers to had.
    tx.execute(
        "DELETE FROM library_management_metadata_snapshots \
         WHERE provider = ?1 AND entity_kind = ?2 AND payload_sha256 = ?3",
        params![PROVIDER, RELEASE_KIND, old_sha],
    )?;
    for (entity, input_hash, fetched_at, expires_at) in rows {
        tx.execute(
            "DELETE FROM library_management_metadata_snapshots \
             WHERE provider = ?1 AND entity_kind = ?2 AND entity_id = ?3",
            params![PROVIDER, RELEASE_KIND, entity],
        )?;
        tx.execute(
            "INSERT INTO library_management_metadata_snapshots (id, provider, \
             entity_kind, entity_id, input_hash, canonical_payload_json, payload_sha256, \
             fetched_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                hex_sha256(format!("{entity}\0{payload_sha}").as_bytes()),
                PROVIDER,
                RELEASE_KIND,
                entity,
                input_hash,
                payload,
                payload_sha,
                fetched_at,
                expires_at,
            ],
        )?;
    }
    Ok(Some(release))
}

/// Point what people made of a merged album, artist or track (favorites,
/// ratings, play history) at the record it merged into.
/// A person who already favorited or rated the survivor keeps one row.
pub(crate) fn retarget_references(
    tx: &Connection,
    item_kind: &str,
    retired_id: &str,
    surviving_id: &str,
) -> rusqlite::Result<()> {
    let column = match item_kind {
        "album" => "local_album_id",
        "artist" => "local_artist_id",
        _ => "local_track_id",
    };
    // Copy, then delete: favorite names reference the favorite's key
    // without ON UPDATE, so the key cannot change in place. Deleting the
    // old favorite cascades to its name.
    tx.execute(
        "INSERT OR IGNORE INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         SELECT user_id, item_kind, ?1, created_at FROM library_user_favorites \
         WHERE item_kind = ?2 AND item_id = ?3",
        params![surviving_id, item_kind, retired_id],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO library_user_favorite_names (user_id, item_kind, item_id, \
         display_name) SELECT user_id, item_kind, ?1, display_name \
         FROM library_user_favorite_names WHERE item_kind = ?2 AND item_id = ?3",
        params![surviving_id, item_kind, retired_id],
    )?;
    tx.execute(
        "DELETE FROM library_user_favorites WHERE item_kind = ?1 AND item_id = ?2",
        params![item_kind, retired_id],
    )?;
    // A person who rated both keeps the survivor's rating.
    tx.execute(
        "INSERT OR IGNORE INTO library_user_ratings (user_id, item_kind, item_id, rating, \
         rated_at) SELECT user_id, item_kind, ?1, rating, rated_at FROM library_user_ratings \
         WHERE item_kind = ?2 AND item_id = ?3",
        params![surviving_id, item_kind, retired_id],
    )?;
    tx.execute(
        "DELETE FROM library_user_ratings WHERE item_kind = ?1 AND item_id = ?2",
        params![item_kind, retired_id],
    )?;
    tx.execute(
        &format!("UPDATE library_play_history SET {column} = ?1 WHERE {column} = ?2"),
        params![surviving_id, retired_id],
    )?;
    Ok(())
}

/// An album's match, as far as settling two rows of one album reads it.
struct AlbumMatch {
    release_group: String,
    /// `None` for a match that names only the release group.
    release: Option<String>,
    source: DecisionSource,
}

impl AlbumMatch {
    fn of(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<Self>> {
        conn.query_row(
            "SELECT lower(release_group_mbid), lower(NULLIF(trim(release_mbid), '')), \
             decision_source FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2",
            params![album_id, PROVIDER],
            |row| {
                Ok(Self {
                    release_group: row.get(0)?,
                    release: row.get(1)?,
                    source: row.get(2)?,
                })
            },
        )
        .optional()
    }

    /// Automatic and embedded matches stay revisable. A person's is one
    /// made by hand or one carried over from v2.
    fn revisable(&self) -> bool {
        self.source.is_revisable()
    }

    /// Whether this match and `other`, both a person's, are two different
    /// choices.
    ///
    /// A match carried over from v2 may name only the release group: a
    /// release of that group on the other row is the same album, said more
    /// exactly. A match a person made with no release is "leave unmanaged"
    /// or a custom edition, a decision of its own: a release on the other
    /// row is a different choice even within the group. Otherwise the two
    /// are the same when they name the same group and the same release.
    fn differs_from(&self, other: &Self) -> bool {
        if self.release_group != other.release_group {
            return true;
        }
        let imported = |chosen: &Self| chosen.source == DecisionSource::LegacyImport;
        match (&self.release, &other.release) {
            (None, Some(_)) => !imported(self),
            (Some(_), None) => !imported(other),
            (mine, theirs) => mine != theirs,
        }
    }
}

/// Whether two rows of one album each hold a match a person chose (or an
/// imported one) and the two are different choices
/// ([`AlbumMatch::differs_from`]). Nothing automatic picks between two
/// people's choices.
pub(crate) fn chosen_matches_differ(
    conn: &Connection,
    album_id: &str,
    other_album_id: &str,
) -> rusqlite::Result<bool> {
    let (Some(one), Some(other)) = (
        AlbumMatch::of(conn, album_id)?,
        AlbumMatch::of(conn, other_album_id)?,
    ) else {
        return Ok(false);
    };
    Ok(!one.revisable() && !other.revisable() && one.differs_from(&other))
}

/// The release document on file for `mbid`, fetched at or after `oldest`
/// (seconds). `None` when there is none, or it does not decode.
pub(crate) fn stored_release(
    conn: &Connection,
    mbid: &str,
    oldest: f64,
) -> rusqlite::Result<Option<Release>> {
    let payload: Option<String> = conn
        .prepare_cached(
            "SELECT canonical_payload_json FROM library_management_metadata_snapshots \
             WHERE provider = ?1 AND entity_kind = ?2 AND entity_id = ?3 \
             AND input_hash = ?4 AND fetched_at >= ?5 \
             ORDER BY fetched_at DESC LIMIT 1",
        )?
        .query_row(
            params![
                PROVIDER,
                RELEASE_KIND,
                mbid.trim().to_ascii_lowercase(),
                release_input_hash(),
                oldest,
            ],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    match serde_json::from_str(&payload) {
        Ok(release) => Ok(Some(release)),
        Err(error) => {
            tracing::warn!(%error, release_mbid = mbid, "stored release document did not decode");
            Ok(None)
        }
    }
}

/// Whether the album's match is a release someone chose: made by a person
/// or carried over from v2, and naming a release. An automatic match does
/// not count, though it names a release too.
pub(crate) fn release_chosen(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    Ok(AlbumMatch::of(conn, album_id)?
        .is_some_and(|chosen| chosen.source.is_persons_choice() && chosen.release.is_some()))
}

/// Whether the album's match is one a person chose or v2 carried, naming
/// a release or not ("leave unmanaged" and a custom edition name none).
pub(crate) fn match_chosen(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    Ok(AlbumMatch::of(conn, album_id)?.is_some_and(|chosen| !chosen.revisable()))
}

/// Settle the matches of two rows of one album before `gone` is deleted
/// and `kept` carries on: a scan made `gone` for the album while it was
/// being retagged, and a match or a review landed on it before the scan
/// had read the rest. When each row holds a match a person chose and the
/// two differ ([`chosen_matches_differ`]) it is refused with an error,
/// before anything is written: the caller keeps both rows.
///
/// - A person's match on `kept` stays, untouched. The match on `gone`
///   (automatic, or the same choice made again) goes with its row.
/// - Otherwise the match on `gone` becomes the match of `kept`, with its
///   source and its "unsure" flag. It replaces an automatic one, as the
///   later verdict on the files as they are now tagged or a person's. It
///   also replaces an imported one that names only the release group,
///   when a person chose a release of that group for `gone`.
/// - A review still waiting on `gone` goes with it. One a person closed
///   moves to `kept`.
///
/// Rows made for a match that is no longer there are dropped, never
/// pointed at another match: both kinds of edition undo name the
/// revisions of the match they were made for. The moved match takes a
/// revision neither row has had, so nothing kept from either row reads as
/// current against it. An artist proof names the album revision it was
/// made at, so after the move it stops counting until the album's next
/// pass replaces it. A chosen edition still waiting to be placed on the
/// files is queued again for `kept`, and automatic management looks at
/// `kept` afresh. When the match `kept` ends with is a person's release
/// and a file is not on it yet, the files are queued to be placed on it,
/// changing nothing else
/// ([`queue_placement`](crate::operations::choice::queue_placement)).
pub(crate) fn settle_followed_match(
    tx: &rusqlite::Transaction<'_>,
    gone: &str,
    kept: &str,
    now: f64,
) -> rusqlite::Result<()> {
    let arriving = AlbumMatch::of(tx, gone)?;
    let staying = AlbumMatch::of(tx, kept)?;
    if let (Some(arriving), Some(staying)) = (&arriving, &staying)
        && !arriving.revisable()
        && !staying.revisable()
        && arriving.differs_from(staying)
    {
        return Err(rusqlite::Error::InvalidParameterName(
            "the two rows hold different chosen matches".to_owned(),
        ));
    }
    // Of two equal choices the one that names the release is kept: the
    // one on `kept`, unless it names only the group and the other one
    // names the release.
    let arriving_names_release = arriving
        .as_ref()
        .is_some_and(|arriving| !arriving.revisable() && arriving.release.is_some());
    let kept_stays = staying.as_ref().is_some_and(|staying| {
        !staying.revisable() && (staying.release.is_some() || !arriving_names_release)
    });
    let managed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_management_automatic_queue \
         WHERE local_album_id = ?1)",
        params![gone],
        |row| row.get(0),
    )?;
    let mut reviews = tx.execute(
        "UPDATE library_identify_reviews SET local_album_id = ?2 \
         WHERE local_album_id = ?1 AND state <> 'pending'",
        params![gone, kept],
    )?;
    reviews += tx.execute(
        "DELETE FROM library_identify_reviews WHERE local_album_id = ?1",
        params![gone],
    )?;
    let moved = match arriving {
        None => false,
        Some(_) if kept_stays => {
            tx.execute(
                "DELETE FROM local_album_external_identities WHERE local_album_id = ?1",
                params![gone],
            )?;
            false
        }
        Some(arriving) => {
            carry_match(tx, gone, kept, !arriving.revisable(), now)?;
            true
        }
    };
    if managed || moved {
        crate::management::automatic::queue(tx, kept, now)?;
    }
    if reviews > 0 {
        bump_identification_stream(tx)?;
    }
    crate::operations::choice::queue_placement(tx, kept, now)?;
    Ok(())
}

/// Put the match of `from` on `to`, with its source, its chooser and its
/// "unsure" flag, at a revision neither row has had. The match on `to` and
/// what was made for it go. A person's match (`chosen`) keeps the remap
/// still waiting to place it on the files. What is left on `from` is the
/// caller's.
fn carry_match(
    tx: &rusqlite::Transaction<'_>,
    from: &str,
    to: &str,
    chosen: bool,
    now: f64,
) -> rusqlite::Result<()> {
    let remap: Option<(String, Option<String>, RemapOrigin)> = tx
        .query_row(
            "SELECT release_mbid, chosen_by_user_id, origin \
             FROM library_edition_remap_queue WHERE local_album_id = ?1",
            params![from],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .filter(|_| chosen);
    let revision: i64 = tx.query_row(
        "SELECT MAX(row_revision) + 1 FROM local_album_external_identities \
         WHERE local_album_id IN (?1, ?2)",
        params![from, to],
        |row| row.get(0),
    )?;
    for table in [
        "local_album_external_identities",
        "library_album_match_state",
        "library_edition_choice_undo",
        "library_automatic_edition_undo",
        "library_edition_remap_queue",
    ] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE local_album_id = ?1"),
            params![to],
        )?;
    }
    tx.execute(
        "UPDATE local_album_external_identities SET local_album_id = ?2, \
         row_revision = ?3 WHERE local_album_id = ?1",
        params![from, to, revision],
    )?;
    tx.execute(
        "UPDATE library_album_match_state SET local_album_id = ?2 WHERE local_album_id = ?1",
        params![from, to],
    )?;
    if let Some((release, chosen_by, origin)) = remap {
        crate::operations::choice::queue_remap(tx, to, &release, chosen_by.as_deref(), now)?;
        tx.execute(
            "UPDATE library_edition_remap_queue SET origin = ?2 WHERE local_album_id = ?1",
            params![to, origin.as_str()],
        )?;
    }
    Ok(())
}

/// Whether merging `old` into `new` puts the old row's match on `new`
/// ([`settle_merged_match`]): `old` has a match, and `new` has none, or an
/// automatic one where the old row's is a person's.
pub(crate) fn merge_moves_match(conn: &Connection, old: &str, new: &str) -> rusqlite::Result<bool> {
    let Some(arriving) = AlbumMatch::of(conn, old)? else {
        return Ok(false);
    };
    Ok(match AlbumMatch::of(conn, new)? {
        None => true,
        Some(staying) => !arriving.revisable() && staying.revisable(),
    })
}

/// Settle the matches of two rows of one album when `old` is merged into
/// `new` across two scans: the files left `old` in an earlier scan and sit
/// on `new`, which stays. When each row holds a match a person chose and
/// the two differ ([`chosen_matches_differ`]) it is refused with an error,
/// before anything is written.
///
/// - When [`merge_moves_match`]: the match on `old` becomes the match of
///   `new`, with its source, its chooser and its "unsure" flag, and a
///   person's keeps the remap still waiting to place it on the files. The
///   match on `new` and what was made for it go. A review waiting on `new`
///   is closed as a choice by hand closes one when a person's match
///   arrives (dismissed, by nobody), and goes when an automatic one does:
///   the offer after the merge judges the whole album.
/// - Otherwise the match on `new` stays as it is.
///
/// A review a person closed on `old` moves to `new`; one still waiting on
/// it goes. The moved match takes a revision neither row has had. When
/// the match `new` ends with is a person's release and a file on `new` is
/// not on it yet, the files are queued to be placed on it, changing
/// nothing else. What is left on `old` of its match is the caller's to
/// clear. Returns whether the match on `new` changed.
pub(crate) fn settle_merged_match(
    tx: &rusqlite::Transaction<'_>,
    old: &str,
    new: &str,
    now: f64,
) -> rusqlite::Result<bool> {
    if chosen_matches_differ(tx, old, new)? {
        return Err(rusqlite::Error::InvalidParameterName(
            "the two rows hold different chosen matches".to_owned(),
        ));
    }
    let moves = merge_moves_match(tx, old, new)?;
    let mut reviews = tx.execute(
        "UPDATE library_identify_reviews SET local_album_id = ?2 \
         WHERE local_album_id = ?1 AND state <> 'pending'",
        params![old, new],
    )?;
    reviews += tx.execute(
        "DELETE FROM library_identify_reviews WHERE local_album_id = ?1",
        params![old],
    )?;
    if let (true, Some(arriving)) = (moves, AlbumMatch::of(tx, old)?) {
        reviews += if arriving.revisable() {
            tx.execute(
                "DELETE FROM library_identify_reviews WHERE local_album_id = ?1 \
                 AND state = 'pending'",
                params![new],
            )?
        } else {
            tx.execute(
                "UPDATE library_identify_reviews SET state = 'dismissed', \
                 resolved_by_user_id = NULL, updated_ms = ?2 \
                 WHERE local_album_id = ?1 AND state = 'pending'",
                params![new, now_ms()],
            )?
        };
        carry_match(tx, old, new, !arriving.revisable(), now)?;
    }
    if reviews > 0 {
        bump_identification_stream(tx)?;
    }
    crate::operations::choice::queue_placement(tx, new, now)?;
    Ok(moves)
}

fn release_input_hash() -> String {
    hex_sha256(RELEASE_DOCUMENT_VERSION.as_bytes())
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Alias table and owner column for one alias kind.
fn alias_table(kind: AliasKind) -> (&'static str, &'static str) {
    match kind {
        AliasKind::MergedAlbum => ("local_album_aliases", "local_album_id"),
        AliasKind::MergedArtist => ("local_artist_aliases", "local_artist_id"),
    }
}

const ALIAS_KINDS: [AliasKind; 2] = [AliasKind::MergedAlbum, AliasKind::MergedArtist];

/// Retired-id aliases.
impl SqliteIdentifyStore {
    pub fn aliases_for(&self, surviving_id: &str) -> Result<Vec<Alias>, StoreError> {
        self.db.read_blocking("identify.aliases", |conn| {
            let mut out = Vec::new();
            for kind in ALIAS_KINDS {
                let (table, owner) = alias_table(kind);
                let mut stmt = conn.prepare(&format!(
                    "SELECT alias FROM {table} WHERE {owner} = ?1 ORDER BY alias"
                ))?;
                let retired = stmt
                    .query_map(params![surviving_id], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                out.extend(retired.into_iter().map(|retired_id| Alias {
                    retired_id,
                    surviving_id: surviving_id.to_owned(),
                    kind,
                }));
            }
            Ok(out)
        })
    }
}

/// Durable identification queue.
impl SqliteIdentifyStore {
    /// Store a job, on the caller's lane. When a live job already holds
    /// the same album and input revision, that job comes back instead.
    pub fn enqueue(&self, job: IdentifyJob, lane: Lane) -> Result<IdentifyJob, StoreError> {
        let key = idempotency_key(&job.local_album_id, &job.input_revision);
        self.db
            .write_blocking(lane, "identify.enqueue", |tx| {
                changing(tx, |tx| {
                    let now = now_ms();
                    let inserted = tx.execute(
                        INSERT_JOB,
                        params![
                            job.id,
                            job.local_album_id,
                            kind_str(job.kind),
                            job.priority as i64,
                            state_str(job.state),
                            job.attempts as i64,
                            job.not_before_ms as i64,
                            job.input_revision,
                            key,
                            job.requested_by_user_id,
                            job.failure_code,
                            now,
                        ],
                    )?;
                    if inserted == 1 {
                        bump_identification_stream(tx)?;
                        return Ok(job.clone());
                    }
                    Ok(tx.query_row(
                        &format!(
                            "SELECT {JOB_COLUMNS} FROM library_identify_jobs \
                             WHERE idempotency_key = ?1 AND state IN ('queued','running','deferred')"
                        ),
                        params![key],
                        map_job,
                    )?)
                })
            })
            .map(|written| self.landed(written))
    }

    /// The next due job, leased to the caller. `None` when nothing is due.
    pub fn claim(&self, now_ms: u64, lease_ms: u64) -> Result<Option<IdentifyJob>, StoreError> {
        self.db
            .write_blocking(Lane::Background, "identify.claim", |tx| {
                changing(tx, |tx| {
                    let now = now_ms as i64;
                    // While identification is paused nothing is claimed, lapsed
                    // leases included; the check sits in the same statement so a
                    // pause can never race a claim.
                    let job = tx
                        .query_row(
                            &format!(
                                "SELECT {JOB_COLUMNS} FROM library_identify_jobs \
                                 WHERE ((state IN ('queued','deferred') AND not_before_ms <= ?1) \
                                 OR (state = 'running' AND lease_expires_ms <= ?1)) \
                                 AND NOT EXISTS (SELECT 1 FROM library_work_control \
                                     WHERE queue_kind = 'identification' AND state = 'paused') \
                                 ORDER BY priority, created_ms, rowid LIMIT 1"
                            ),
                            params![now],
                            map_job,
                        )
                        .optional()?;
                    let Some(mut job) = job else {
                        return Ok(None);
                    };
                    tx.execute(
                        "UPDATE library_identify_jobs SET state = 'running', \
                         lease_expires_ms = ?1, updated_ms = ?2 WHERE id = ?3",
                        params![now + lease_ms as i64, now, job.id],
                    )?;
                    bump_identification_stream(tx)?;
                    job.state = JobState::Running;
                    job.lease_expires_ms = Some((now + lease_ms as i64).max(0) as u64);
                    Ok(Some(job))
                })
            })
            .map(|written| self.landed(written))
    }

    /// Land a claimed job (its result, a deferral, or a put-back) under
    /// the lease it was claimed with. `false` when another worker took the
    /// job over after its lease lapsed: nothing is written.
    pub fn land(&self, job: &IdentifyJob) -> Result<bool, StoreError> {
        let landed = self
            .db
            .write_blocking(Lane::Background, "identify.land", |tx| {
                land_tx(tx, job).map(|()| ((), true))
            });
        match landed {
            Ok(written) => {
                self.landed(written);
                Ok(true)
            }
            Err(Landing::LeaseLost) => Ok(false),
            Err(Landing::Store(error)) => Err(error),
        }
    }

    pub fn job(&self, job_id: &str) -> Result<Option<IdentifyJob>, StoreError> {
        self.db.read_blocking("identify.job", |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {JOB_COLUMNS} FROM library_identify_jobs WHERE id = ?1"),
                    params![job_id],
                    map_job,
                )
                .optional()?)
        })
    }

    pub fn jobs_for_album(&self, local_album_id: &str) -> Result<Vec<IdentifyJob>, StoreError> {
        self.db.read_blocking("identify.jobs_for_album", |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {JOB_COLUMNS} FROM library_identify_jobs \
                 WHERE local_album_id = ?1 ORDER BY created_ms, rowid"
            ))?;
            let rows = stmt.query_map(params![local_album_id], map_job)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// Boot recovery: jobs a previous process left running go back to
    /// the queue. Returns how many.
    pub fn recover(&self) -> Result<usize, StoreError> {
        self.db
            .write_blocking(Lane::Background, "identify.recover", |tx| {
                changing(tx, |tx| {
                    let requeued = tx.execute(
                        "UPDATE library_identify_jobs SET state = 'queued', \
                         lease_expires_ms = NULL, updated_ms = ?1 WHERE state = 'running'",
                        params![now_ms()],
                    )?;
                    if requeued > 0 {
                        bump_identification_stream(tx)?;
                    }
                    Ok(requeued)
                })
            })
            .map(|written| self.landed(written))
    }
}

/// Curator review queue.
impl SqliteIdentifyStore {
    pub fn file(&self, review: ReviewItem) -> Result<(), StoreError> {
        let candidates = serde_json::to_string(&review.candidates).map_err(|error| {
            StoreError::Failed(format!(
                "review {} candidates did not encode: {error}",
                review.id
            ))
        })?;
        self.db
            .write_blocking(Lane::Background, "identify.file_review", |tx| {
                changing(tx, |tx| {
                    let now = now_ms();
                    tx.execute(
                        "INSERT OR REPLACE INTO library_identify_reviews (id, local_album_id, \
                         reason_code, candidates_json, state, resolved_by_user_id, \
                         selected_candidate_key, created_ms, updated_ms) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                        params![
                            review.id,
                            review.local_album_id,
                            review.reason_code,
                            candidates,
                            review_state_str(review.state),
                            review.resolved_by_user_id,
                            review.selected_candidate_key,
                            now,
                        ],
                    )?;
                    Ok(bump_identification_stream(tx)?)
                })
            })
            .map(|written| self.landed(written))
    }

    /// One review. A review whose stored candidates no longer decode is
    /// logged and read as gone ([`map_review`]).
    pub fn get(&self, review_id: &str) -> Result<Option<ReviewItem>, StoreError> {
        self.db.read_blocking("identify.review", |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {REVIEW_COLUMNS} FROM library_identify_reviews WHERE id = ?1"),
                    params![review_id],
                    map_review,
                )
                .optional()?
                .flatten())
        })
    }

    pub fn pending_for_album(&self, local_album_id: &str) -> Result<Vec<ReviewItem>, StoreError> {
        self.db.read_blocking("identify.pending_reviews", |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {REVIEW_COLUMNS} FROM library_identify_reviews \
                 WHERE local_album_id = ?1 AND state = 'pending' ORDER BY created_ms, id"
            ))?;
            let rows = stmt.query_map(params![local_album_id], map_review)?;
            Ok(rows
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect())
        })
    }

    /// Settle a review from a request. `false` when no row changed.
    pub fn set_state(
        &self,
        review_id: &str,
        state: ReviewState,
        by_user_id: Option<&str>,
        selected_key: Option<&str>,
    ) -> Result<bool, StoreError> {
        self.db
            .write_blocking(Lane::Foreground, "identify.settle_review", |tx| {
                changing(tx, |tx| {
                    let changed = tx.execute(
                        "UPDATE library_identify_reviews SET state = ?1, \
                         resolved_by_user_id = ?2, selected_candidate_key = ?3, updated_ms = ?4 \
                         WHERE id = ?5",
                        params![
                            review_state_str(state),
                            by_user_id,
                            selected_key,
                            now_ms(),
                            review_id
                        ],
                    )?;
                    if changed == 1 {
                        bump_identification_stream(tx)?;
                    }
                    Ok(changed == 1)
                })
            })
            .map(|written| self.landed(written))
    }

    /// Settle a pending review as approved and seal the identities it
    /// chose, all or nothing. `Ok(false)` when the review is no longer
    /// pending. Each sealed row takes its stored revision plus one.
    pub fn approve(&self, approval: &Approval) -> Result<bool, StoreError> {
        self.db
            .write_blocking(APPROVE_LANE, "identify.approve_review", |tx| {
                changing(tx, |tx| approve_tx(tx, approval))
            })
            .map(|written| self.landed(written))
    }
}

/// The lane a review approval writes on: a request's.
const APPROVE_LANE: Lane = Lane::Foreground;

/// [`SqliteIdentifyStore::approve`] on its transaction.
fn approve_tx(tx: &Transaction<'_>, approval: &Approval) -> Result<bool, StoreError> {
    let album = &approval.album.local_album_id;
    let settled = tx.execute(
        "UPDATE library_identify_reviews SET state = 'approved', \
         resolved_by_user_id = ?1, selected_candidate_key = ?2, updated_ms = ?3 \
         WHERE id = ?4 AND state = 'pending'",
        params![
            approval.by_user_id,
            approval.candidate_key,
            now_ms(),
            approval.review_id
        ],
    )?;
    if settled != 1 {
        return Ok(false);
    }
    // A review approval can be taken back like any edition choice.
    let point = crate::operations::choice::undo_point(tx, album)?;
    seal_album_identity(tx, &approval.album)?;
    for track in &approval.tracks {
        seal_track_identity(tx, track)?;
    }
    // A curator's approval confirms the album: an unconfirmed flag left by
    // an earlier best guess must not keep holding its files back from
    // management (an edition choice clears it the same way).
    tx.execute(
        "DELETE FROM library_album_match_state WHERE local_album_id = ?1",
        params![album],
    )?;
    // A candidate without track evidence (an edition a catalog correction
    // dropped, restored from its review) names the release only: the remap
    // worker places the files on it.
    if approval.tracks.is_empty()
        && let Some(release) = approval.album.release_mbid.as_deref()
    {
        crate::operations::choice::queue_remap(
            tx,
            album,
            release,
            Some(&approval.by_user_id),
            crate::clock::now_unix(),
        )?;
    }
    crate::operations::choice::keep_undo_point(
        tx,
        album,
        &point,
        &approval.by_user_id,
        "review_approve",
        "REVIEW_APPROVED",
        crate::clock::now_unix(),
    )
    .map_err(|error| {
        StoreError::Failed(format!("approve review {}: {error}", approval.review_id))
    })?;
    bump_identification_stream(tx)?;
    let now = crate::clock::now_unix();
    join_replaced(tx, Scope::Album(album), now)?;
    continue_tracks(tx, Scope::Album(album), now)?;
    Ok(true)
}

/// Seal one album identity inside the caller's transaction. A new row
/// starts at revision 1; an existing one moves to its revision plus one.
fn seal_album_identity(tx: &Connection, identity: &AlbumIdentity) -> rusqlite::Result<()> {
    let Some(group) = identity.release_group_mbid.as_deref() else {
        return Err(rusqlite::Error::InvalidParameterName(
            "album identity without a release group".to_owned(),
        ));
    };
    tx.execute(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_at, row_revision) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1) \
         ON CONFLICT (local_album_id, provider) DO UPDATE SET \
         release_group_mbid = excluded.release_group_mbid, \
         release_mbid = excluded.release_mbid, \
         decision_source = excluded.decision_source, \
         selected_at = excluded.selected_at, \
         row_revision = local_album_external_identities.row_revision + 1",
        params![
            identity.local_album_id,
            PROVIDER,
            group,
            identity.release_mbid,
            identity.decision_source.as_str(),
            now_secs(),
        ],
    )?;
    Ok(())
}

/// Seal one track identity inside the caller's transaction, revisions as
/// for albums.
fn seal_track_identity(tx: &Connection, identity: &TrackIdentity) -> rusqlite::Result<()> {
    let Some(recording) = identity.recording_mbid.as_deref() else {
        return Err(rusqlite::Error::InvalidParameterName(
            "track identity without a recording".to_owned(),
        ));
    };
    tx.execute(
        "INSERT INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, release_track_mbid, decision_source, selected_at, row_revision) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1) \
         ON CONFLICT (local_track_id, provider) DO UPDATE SET \
         recording_mbid = excluded.recording_mbid, \
         release_track_mbid = excluded.release_track_mbid, \
         decision_source = excluded.decision_source, \
         selected_at = excluded.selected_at, \
         row_revision = local_track_external_identities.row_revision + 1",
        params![
            identity.local_track_id,
            PROVIDER,
            recording,
            identity.release_track_mbid,
            identity.decision_source.as_str(),
            now_secs(),
        ],
    )?;
    Ok(())
}

const REVIEW_COLUMNS: &str = "id, local_album_id, reason_code, candidates_json, state, \
     resolved_by_user_id, selected_candidate_key";

/// One review row. Candidates that no longer decode drop the review with
/// a log line instead of failing the whole listing.
pub(crate) fn map_review(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<ReviewItem>> {
    let id: String = row.get(0)?;
    let json: String = row.get(3)?;
    let candidates: Vec<CandidateEvidence> = match serde_json::from_str(&json) {
        Ok(candidates) => candidates,
        Err(error) => {
            tracing::error!(%error, review = id, "stored review candidates did not decode");
            return Ok(None);
        }
    };
    Ok(Some(ReviewItem {
        id,
        local_album_id: row.get(1)?,
        reason_code: row.get(2)?,
        candidates,
        state: review_state_from(&row.get::<_, String>(4)?),
        resolved_by_user_id: row.get(5)?,
        selected_candidate_key: row.get(6)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two rows of one album each hold a release a person chose, and the
    /// two differ. Settling them is refused before anything is written:
    /// neither match and no review changes.
    #[test]
    fn settling_two_different_chosen_matches_is_refused() {
        let dir = droppedneedle_kernel::db::scratch::ScratchDir::new("settle-match").unwrap();
        let path = dir.path().join("app.db");
        droppedneedle_kernel::schema::template::copy_to(&path).unwrap();
        let db = Db::open_for_tests(&path).unwrap();
        let seed = |tx: &Transaction<'_>| {
            tx.execute_batch(
                "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, grouping_source, created_at, updated_at) VALUES \
             ('old', 'r', 'old', 'T', 't', '00000000-0000-4000-8000-000000000002', \
              'automatic', 1, 1), \
             ('new', 'r', 'new', 'T', 't', '00000000-0000-4000-8000-000000000002', \
              'automatic', 1, 1); \
             INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_at) VALUES \
             ('old', 'musicbrainz', 'rg', 'rel-1', 'manual', 0), \
             ('new', 'musicbrainz', 'rg', 'rel-2', 'legacy_import', 0); \
             INSERT INTO library_identify_reviews (id, local_album_id, reason_code, \
             candidates_json, state, created_ms, updated_ms) VALUES \
             ('waiting', 'new', 'R', '[]', 'pending', 0, 0), \
             ('closed', 'new', 'R', '[]', 'dismissed', 0, 0);",
            )
        };
        db.write_blocking(Lane::Background, "test.seed", |tx| {
            Ok::<_, droppedneedle_kernel::db::DbError>(seed(tx)?)
        })
        .unwrap();
        let state = |conn: &Connection| -> Vec<(String, String)> {
            conn.prepare(
                "SELECT local_album_id, release_mbid || ' ' || decision_source || ' ' \
                 || row_revision FROM local_album_external_identities \
                 UNION ALL SELECT local_album_id, id FROM library_identify_reviews ORDER BY 1, 2",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
        };
        let before = db
            .read_blocking("test.before", |conn| {
                Ok::<_, droppedneedle_kernel::db::DbError>(state(conn))
            })
            .unwrap();
        assert_eq!(before.len(), 4);

        db.write_blocking(Lane::Background, "test.settle", |tx| {
            assert!(settle_followed_match(tx, "new", "old", 1.0).is_err());
            assert_eq!(state(tx), before);
            Ok::<_, droppedneedle_kernel::db::DbError>(())
        })
        .unwrap();
    }
}
