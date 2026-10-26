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
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, Lane};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use sha2::{Digest as _, Sha256};

use super::artist_links;
use super::merged_ids;
use super::models::{
    AlbumIdentity, Alias, AliasKind, AutomaticSeal, CandidateEvidence, IdentifyJob, IdentifyKind,
    JobState, LocalAlbumFacts, LocalTrackFacts, MatchFlag, MatchFlagState, ReviewItem, ReviewState,
    TrackIdentity,
};
pub(crate) use super::queue::bump_identification_stream;
use super::queue::{INSERT_JOB, idempotency_key};
pub use super::queue::{album_input_revision, offer_album};
use super::sources::ArtistAnswer;
use super::stores::{Approval, AttemptLanding, RELEASE_FRESH_SECS, StoreError, land_job};
use crate::clock::LibraryClock as _;
use crate::corrections::artist_convergence::{
    any_waiting, converge_ids, follow_moved, follow_waiting,
};
use crate::corrections::continuation::{DRAIN_BOUND, Scope, continue_tracks, drain_continuations};
pub(crate) use crate::identity::follow::{
    chosen_matches_differ, match_chosen, merge_moves_match, release_chosen, settle_followed_match,
    settle_merged_match,
};
use crate::identity::reads::{RELEASE_KIND, release_input_hash};
pub use crate::identity::reads::{album_identity_row, track_identity_rows};
pub(crate) use crate::identity::reads::{read_album_identity, read_track_identity, stored_release};
use crate::identity::{self, AutomaticOutcome, Inputs};
use crate::matching::Release;

const PROVIDER: &str = "musicbrainz";

pub(crate) fn kind_str(kind: IdentifyKind) -> &'static str {
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

const JOB_COLUMNS: &str = "id, local_album_id, kind, priority, state, attempts, not_before_ms, \
     input_revision, requested_by_user_id, failure_code, lease_expires_ms";

/// Land a claimed job, only while the claim that ran it still holds it:
/// a lease another worker took over after this one lapsed lands nothing.
const LAND_JOB: &str = "UPDATE library_identify_jobs SET state = ?1, attempts = ?2, \
     not_before_ms = ?3, input_revision = ?4, idempotency_key = ?5, failure_code = ?6, \
     priority = ?7, updated_ms = ?8, lease_expires_ms = NULL \
     WHERE id = ?9 AND state = 'running' AND lease_expires_ms = ?10";

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

/// SQLite identify store.
pub struct SqliteIdentifyStore {
    db: Db,
    /// Wall time for job, review and identity stamps.
    clock: Arc<dyn Clock>,
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
    pub fn new(db: Db, clock: Arc<dyn Clock>) -> Self {
        Self {
            db,
            clock,
            link_cursor: Mutex::new(String::new()),
            swept: Mutex::new(None),
            drained: Mutex::new(None),
            writes: AtomicU64::new(0),
        }
    }

    /// The wall clock this store stamps its rows by.
    pub(crate) fn clock(&self) -> &dyn Clock {
        &*self.clock
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
                changing(tx, |tx| {
                    Ok(offer_album(tx, local_album_id, self.clock.now_ms())?)
                })
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
                    crate::identity::save_album(tx, &identity, self.clock.now_secs())?;
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
                    if crate::identity::save_track(tx, &identity, self.clock.now_secs())? == 1 {
                        let album: String = tx.query_row(
                            "SELECT local_album_id FROM local_tracks WHERE id = ?1",
                            params![identity.local_track_id],
                            |row| row.get(0),
                        )?;
                        continue_tracks(tx, Scope::Album(&album), self.clock.now_secs())?;
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
                        now: self.clock.now_secs(),
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
                    now: self.clock.now_secs(),
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
                    now: self.clock.now_secs(),
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
                    self.clock.now_ms().max(0) as u64,
                    Some("INPUT_MOVED"),
                );
            }
            land_tx(tx, &*self.clock, &landed)?;
            Ok(((outcome, landed), tx.total_changes() != before))
        });
        match landed {
            Ok(written) => Ok(Some(self.landed(written))),
            Err(Landing::LeaseLost) => Ok(None),
            Err(Landing::Store(error)) => Err(error),
        }
    }

    /// The album's match flag, if identification is unsure of it.
    /// True when an administrator undid an automatic edition of
    /// `release_group` on the album and got the earlier edition back: the
    /// app does not move it to another edition of that group by itself
    /// again.
    pub fn edition_change_undone(
        &self,
        local_album_id: &str,
        release_group: &str,
    ) -> Result<bool, StoreError> {
        self.db
            .read_blocking("identify.edition_change_undone", |conn| {
                Ok(conn.query_row(
                    "SELECT EXISTS (SELECT 1 FROM library_catalog_actions \
                 WHERE local_album_id = ?1 AND action_kind = 'undo_automatic_edition' \
                 AND json_extract(after_json, '$.outcome') = 'restored' \
                 AND lower(json_extract(before_json, '$.identity.release_group_mbid')) \
                     = lower(?2))",
                    params![local_album_id, release_group],
                    |row| row.get(0),
                )?)
            })
    }

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
fn land_tx(tx: &Transaction<'_>, clock: &dyn Clock, job: &IdentifyJob) -> Result<(), Landing> {
    let key = idempotency_key(&job.local_album_id, &job.input_revision);
    let landed = tx.prepare_cached(LAND_JOB)?.execute(params![
        state_str(job.state),
        job.attempts as i64,
        job.not_before_ms as i64,
        job.input_revision,
        key,
        job.failure_code,
        job.priority as i64,
        clock.now_ms(),
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
        let now = self.clock.now_secs();
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
fn artist_pass(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    album: &str,
    converge: bool,
) -> Result<(), StoreError> {
    let now = clock.now_ms();
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
                changing(tx, |tx| {
                    artist_pass(tx, &*self.clock, local_album_id, false)
                })
            })
            .map(|written| self.landed(written))
    }

    /// [`Self::link_album_artists`], then merge the rows proven to be one
    /// artist by the ids the album's proofs name and the ids the pass
    /// newly linked. Identification runs this after each attempt and
    /// for each album a sweep finds stale; a person's identity command
    /// runs it once its own write has committed ([`Self::link_after_command`]).
    pub fn link_and_merge_album_artists(&self, local_album_id: &str) -> Result<(), StoreError> {
        self.db
            .write_blocking(
                Lane::Background,
                "identify.link_and_merge_album_artists",
                |tx| changing(tx, |tx| artist_pass(tx, &*self.clock, local_album_id, true)),
            )
            .map(|written| self.landed(written))
    }

    /// [`Self::link_and_merge_album_artists`] right after a person's
    /// identity command committed, so the album's artist proofs and links
    /// are there when the command answers. The command has already moved
    /// the album's identity, so a pass that fails is logged and left to the
    /// sweep; the command's answer stands.
    pub fn link_after_command(&self, local_album_id: &str) {
        if let Err(error) = self.link_and_merge_album_artists(local_album_id) {
            tracing::warn!(
                %error,
                album_id = local_album_id,
                "the album's artist links were not worked out after the change; the sweep tries again"
            );
        }
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
                            follow_waiting(tx, self.clock.now_ms() as f64 / 1000.0)?;
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
                            artist_links::settle(tx, one, self.clock.now_ms())?;
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

    /// Up to `limit` releases whose document an album with an accepted
    /// match waits for, the longest wait first.
    pub fn releases_waiting_for_documents(&self, limit: usize) -> Result<Vec<String>, StoreError> {
        self.db.read_blocking("identify.releases_waiting", |conn| {
            Ok(artist_links::releases_waiting(
                conn,
                &release_input_hash(),
                limit,
            )?)
        })
    }

    /// Record that MusicBrainz has no release `release_mbid`: the albums
    /// waiting for its document stop asking for it and are offered for
    /// identification again, so a new match can end the wait. Returns how
    /// many albums.
    pub fn release_not_found(&self, release_mbid: &str) -> Result<usize, StoreError> {
        let release = release_mbid.trim().to_ascii_lowercase();
        self.db
            .write_blocking(Lane::Background, "identify.release_not_found", |tx| {
                changing(tx, |tx| {
                    let now = self.clock.now_ms();
                    let albums = artist_links::release_settled(
                        tx,
                        &release,
                        artist_links::RELEASE_NOT_FOUND,
                        now,
                    )?;
                    for album in &albums {
                        offer_album(tx, album, now)?;
                    }
                    Ok(albums.len())
                })
            })
            .map(|written| self.landed(written))
    }

    /// Record that MusicBrainz refused release `release_mbid`'s document in
    /// a way asking again will not change, so the albums waiting for it
    /// stop asking for it. Returns how many albums.
    pub fn release_refused(&self, release_mbid: &str) -> Result<usize, StoreError> {
        let release = release_mbid.trim().to_ascii_lowercase();
        self.db
            .write_blocking(Lane::Background, "identify.release_refused", |tx| {
                changing(tx, |tx| {
                    Ok(artist_links::release_settled(
                        tx,
                        &release,
                        artist_links::RELEASE_DOCUMENT_REFUSED,
                        self.clock.now_ms(),
                    )?
                    .len())
                })
            })
            .map(|written| self.landed(written))
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
                    let now = self.clock.now_ms();
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
                        Ok(drain_continuations(tx, DRAIN_BOUND, self.clock.now_secs())?)
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
        let oldest = max_age_secs.map_or(0.0, |age| self.clock.now_secs() - age as f64);
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
                    store_release(tx, release, stored, self.clock.now_secs())
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
        // An album matched to it may have been waiting for it.
        artist_links::mark_release_stale(tx, entity)?;
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
        // An album matched to it may have been waiting for it, or its
        // proofs may read what changed.
        artist_links::mark_release_stale(tx, &entity)?;
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
                    let now = self.clock.now_ms();
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
                land_tx(tx, &*self.clock, job).map(|()| ((), true))
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
                        params![self.clock.now_ms()],
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
                    let now = self.clock.now_ms();
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
                            self.clock.now_ms(),
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

    /// Settle a pending review as approved and make its candidate the
    /// album's match ([`identity::approve`]), all or nothing. `Ok(false)`
    /// when the review is no longer pending; nothing is written then.
    pub fn approve(&self, approval: &Approval) -> Result<bool, StoreError> {
        let approved = self
            .db
            .write_blocking(APPROVE_LANE, "identify.approve_review", |tx| {
                changing(tx, |tx| approve_tx(tx, &*self.clock, approval))
            })
            .map(|written| self.landed(written))?;
        if approved {
            self.link_after_command(&approval.album.local_album_id);
        }
        Ok(approved)
    }
}

/// The lane a review approval writes on: a request's.
const APPROVE_LANE: Lane = Lane::Foreground;

/// [`SqliteIdentifyStore::approve`] on its transaction.
fn approve_tx(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    approval: &Approval,
) -> Result<bool, StoreError> {
    let album = &approval.album.local_album_id;
    let Some(group) = approval.album.release_group_mbid.as_deref() else {
        return Err(StoreError::Failed(format!(
            "approve review {}: a candidate without a release group",
            approval.review_id
        )));
    };
    // Only this review settles, and only while it is still pending.
    let settled = tx.execute(
        "UPDATE library_identify_reviews SET state = 'approved', \
         resolved_by_user_id = ?1, selected_candidate_key = ?2, updated_ms = ?3 \
         WHERE id = ?4 AND state = 'pending'",
        params![
            approval.by_user_id,
            approval.candidate_key,
            clock.now_ms(),
            approval.review_id
        ],
    )?;
    if settled != 1 {
        return Ok(false);
    }
    identity::approve(
        tx,
        album,
        &identity::Approved {
            release_group_mbid: group,
            release_mbid: approval.album.release_mbid.as_deref(),
            by_user_id: &approval.by_user_id,
            now: clock.now_secs(),
        },
    )?;
    bump_identification_stream(tx)?;
    Ok(true)
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
