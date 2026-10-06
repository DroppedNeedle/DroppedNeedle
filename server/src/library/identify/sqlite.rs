//! Durable identify state over the application database.
//!
//! One store implements every identify port. Accepted identities live in
//! the external-identity tables with their `decision_source`, so manual
//! and legacy decisions outlive rescans, moves, and restarts. The queue,
//! reviews, proofs, and provider credits use the 0011 tables; pins and
//! aliases use their 0001 tables. Album facts come straight from the
//! catalog the scan writes, so a restart never strands an album without
//! facts. Release documents go in `library_management_metadata_snapshots`
//! (immutable rows, one per distinct payload); expired ones no identity
//! names are pruned on each save.
//!
//! Every call is a short synchronous transaction on one connection behind
//! a mutex. A store failure logs and reads as absence (or `false`), never
//! a panic; the queue keeps its failures visible through `Option`.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};
use sha2::{Digest as _, Sha256};

use super::models::{
    AlbumIdentity, Alias, AliasKind, ArtistCredit, ArtistIdentity, CandidateEvidence, CreditProof,
    DecisionSource, IdentifyJob, IdentifyKind, JobState, LocalAlbumFacts, LocalTrackFacts,
    ReleasePin, ReviewItem, ReviewState, TrackIdentity,
};
use super::queue::PRIORITY_NEW_OR_CHANGED;
use super::stores::{
    AliasStore, Approval, FactsSource, IdentityStore, PinStore, ProofStore, QueueStore,
    ReleaseStore, ReviewStore, StoreError,
};
use crate::library::matching::Release;

const PROVIDER: &str = "musicbrainz";
/// Snapshot kind for stored release documents.
const RELEASE_KIND: &str = "release";
/// Shape version of the stored document; a new shape is a new input hash.
const RELEASE_DOCUMENT_VERSION: &str = "matching-release-v1";
/// Documents count as fresh for recall this long.
pub const RELEASE_FRESH_SECS: u64 = 7 * 24 * 3600;

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
    }
}

fn review_state_from(raw: &str) -> ReviewState {
    match raw {
        "approved" => ReviewState::Approved,
        "rejected" => ReviewState::Rejected,
        _ => ReviewState::Pending,
    }
}

/// `embedded` rows (tags carried the ids) are revisable like automatic ones.
fn decision_from(raw: &str) -> DecisionSource {
    match raw {
        "manual" => DecisionSource::Manual,
        "legacy_import" => DecisionSource::LegacyImport,
        _ => DecisionSource::Automatic,
    }
}

fn idempotency_key(local_album_id: &str, input_revision: &str) -> String {
    format!("{local_album_id}:{input_revision}")
}

const JOB_COLUMNS: &str = "id, local_album_id, kind, priority, state, attempts, not_before_ms, \
     input_revision, requested_by_user_id, failure_code";

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
    Ok(inserted == 1)
}

/// SQLite identify store.
pub struct SqliteIdentifyStore {
    conn: Mutex<Connection>,
}

impl SqliteIdentifyStore {
    /// Open against the migrated application database through the
    /// database factory.
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = crate::db::open_connection(path).map_err(|error| error.to_string())?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Run one read; a failure logs and reads as `None`.
    fn read<T>(
        &self,
        what: &str,
        op: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Option<T> {
        match op(&self.lock()) {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::error!(%error, what, "identify store read failed");
                None
            }
        }
    }

    /// Run one write in an immediate transaction; a failure logs and
    /// reads as `None`.
    fn write<T>(
        &self,
        what: &str,
        op: impl FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<T>,
    ) -> Option<T> {
        let mut conn = self.lock();
        let outcome = (|| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let value = op(&tx)?;
            tx.commit()?;
            Ok(value)
        })();
        match outcome {
            Ok(value) => Some(value),
            Err(error) => {
                let error: rusqlite::Error = error;
                tracing::error!(%error, what, "identify store write failed");
                None
            }
        }
    }

    /// Queue an identification for one album the catalog holds, reusing a
    /// live job on the same revision. `None` when the album is unknown or
    /// the store failed.
    pub fn offer(&self, local_album_id: &str) -> Option<bool> {
        self.write("offer album", |tx| {
            offer_album(tx, local_album_id, now_ms())
        })
    }

    /// Current input revision of one album.
    pub fn input_revision(&self, local_album_id: &str) -> Option<String> {
        self.read("album revision", |conn| {
            album_input_revision(conn, local_album_id)
        })
    }
}

impl IdentityStore for SqliteIdentifyStore {
    fn album_identity(&self, local_album_id: &str) -> Option<AlbumIdentity> {
        self.read("album identity", |conn| {
            conn.query_row(
                "SELECT release_group_mbid, release_mbid, decision_source, row_revision \
                 FROM local_album_external_identities \
                 WHERE local_album_id = ?1 AND provider = ?2",
                params![local_album_id, PROVIDER],
                |row| {
                    Ok(AlbumIdentity {
                        local_album_id: local_album_id.to_owned(),
                        provider: PROVIDER.to_owned(),
                        release_group_mbid: Some(row.get(0)?),
                        release_mbid: row.get(1)?,
                        decision_source: decision_from(&row.get::<_, String>(2)?),
                        row_revision: row.get::<_, i64>(3)? as u64,
                    })
                },
            )
            .optional()
        })
        .flatten()
    }

    fn save_album_identity(&self, identity: AlbumIdentity) {
        let Some(group) = identity.release_group_mbid.clone() else {
            tracing::warn!(
                album = identity.local_album_id,
                "album identity without a release group not stored"
            );
            return;
        };
        self.write("save album identity", |tx| {
            tx.execute(
                "INSERT INTO local_album_external_identities (local_album_id, provider, \
                 release_group_mbid, release_mbid, decision_source, selected_at, row_revision) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT (local_album_id, provider) DO UPDATE SET \
                 release_group_mbid = excluded.release_group_mbid, \
                 release_mbid = excluded.release_mbid, \
                 decision_source = excluded.decision_source, \
                 selected_at = excluded.selected_at, row_revision = excluded.row_revision",
                params![
                    identity.local_album_id,
                    PROVIDER,
                    group,
                    identity.release_mbid,
                    identity.decision_source.as_str(),
                    now_secs(),
                    identity.row_revision.max(1) as i64,
                ],
            )
        });
    }

    fn clear_album_identity(&self, local_album_id: &str) {
        self.write("clear album identity", |tx| {
            tx.execute(
                "DELETE FROM local_album_external_identities \
                 WHERE local_album_id = ?1 AND provider = ?2",
                params![local_album_id, PROVIDER],
            )
        });
    }

    fn track_identity(&self, local_track_id: &str) -> Option<TrackIdentity> {
        self.read("track identity", |conn| {
            conn.query_row(
                "SELECT recording_mbid, release_track_mbid, decision_source, row_revision \
                 FROM local_track_external_identities \
                 WHERE local_track_id = ?1 AND provider = ?2",
                params![local_track_id, PROVIDER],
                |row| {
                    Ok(TrackIdentity {
                        local_track_id: local_track_id.to_owned(),
                        provider: PROVIDER.to_owned(),
                        recording_mbid: Some(row.get(0)?),
                        release_track_mbid: row.get(1)?,
                        decision_source: decision_from(&row.get::<_, String>(2)?),
                        row_revision: row.get::<_, i64>(3)? as u64,
                    })
                },
            )
            .optional()
        })
        .flatten()
    }

    fn save_track_identity(&self, identity: TrackIdentity) {
        let Some(recording) = identity.recording_mbid.clone() else {
            tracing::warn!(
                track = identity.local_track_id,
                "track identity without a recording not stored"
            );
            return;
        };
        self.write("save track identity", |tx| {
            tx.execute(
                "INSERT INTO local_track_external_identities (local_track_id, provider, \
                 recording_mbid, release_track_mbid, decision_source, selected_at, row_revision) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT (local_track_id, provider) DO UPDATE SET \
                 recording_mbid = excluded.recording_mbid, \
                 release_track_mbid = excluded.release_track_mbid, \
                 decision_source = excluded.decision_source, \
                 selected_at = excluded.selected_at, row_revision = excluded.row_revision",
                params![
                    identity.local_track_id,
                    PROVIDER,
                    recording,
                    identity.release_track_mbid,
                    identity.decision_source.as_str(),
                    now_secs(),
                    identity.row_revision.max(1) as i64,
                ],
            )
        });
    }

    fn artist_identity(&self, local_artist_id: &str) -> Option<ArtistIdentity> {
        self.read("artist identity", |conn| {
            conn.query_row(
                "SELECT provider_artist_id, decision_source, row_revision \
                 FROM local_artist_external_identities \
                 WHERE local_artist_id = ?1 AND provider = ?2",
                params![local_artist_id, PROVIDER],
                |row| {
                    Ok(ArtistIdentity {
                        local_artist_id: local_artist_id.to_owned(),
                        provider: PROVIDER.to_owned(),
                        provider_artist_mbid: Some(row.get(0)?),
                        decision_source: decision_from(&row.get::<_, String>(1)?),
                        row_revision: row.get::<_, i64>(2)? as u64,
                    })
                },
            )
            .optional()
        })
        .flatten()
    }

    fn save_artist_identity(&self, identity: ArtistIdentity) {
        let Some(mbid) = identity.provider_artist_mbid.clone() else {
            tracing::warn!(
                artist = identity.local_artist_id,
                "artist identity without an MBID not stored"
            );
            return;
        };
        self.write("save artist identity", |tx| {
            tx.execute(
                "INSERT INTO local_artist_external_identities (local_artist_id, provider, \
                 provider_artist_id, decision_source, selected_at, row_revision) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT (local_artist_id, provider) DO UPDATE SET \
                 provider_artist_id = excluded.provider_artist_id, \
                 decision_source = excluded.decision_source, \
                 selected_at = excluded.selected_at, row_revision = excluded.row_revision",
                params![
                    identity.local_artist_id,
                    PROVIDER,
                    mbid,
                    identity.decision_source.as_str(),
                    now_secs(),
                    identity.row_revision.max(1) as i64,
                ],
            )
        });
    }

    fn owned_artist_by_mbid(&self, artist_mbid: &str) -> Option<String> {
        self.read("owned artist", |conn| {
            conn.query_row(
                "SELECT local_artist_id FROM local_artist_external_identities \
                 WHERE provider = ?1 AND lower(provider_artist_id) = lower(?2)",
                params![PROVIDER, artist_mbid],
                |row| row.get(0),
            )
            .optional()
        })
        .flatten()
    }

    fn save_owned_artist(&self, artist_mbid: &str, local_artist_id: &str) {
        self.save_artist_identity(ArtistIdentity {
            local_artist_id: local_artist_id.to_owned(),
            provider: PROVIDER.to_owned(),
            provider_artist_mbid: Some(artist_mbid.to_lowercase()),
            decision_source: DecisionSource::Automatic,
            row_revision: 1,
        });
    }

    fn track_credits(&self, local_track_id: &str) -> Vec<ArtistCredit> {
        self.read("track credits", |conn| {
            let mut stmt = conn.prepare(
                "SELECT position, artist_mbid, canonical_name, credited_name \
                 FROM library_identify_track_credits WHERE local_track_id = ?1 \
                 ORDER BY position",
            )?;
            let rows = stmt.query_map(params![local_track_id], |row| {
                Ok(ArtistCredit {
                    position: row.get::<_, i64>(0)? as u32,
                    artist_mbid: row.get(1)?,
                    canonical_name: row.get(2)?,
                    credited_name: row.get(3)?,
                })
            })?;
            rows.collect()
        })
        .unwrap_or_default()
    }

    fn save_track_credits(&self, local_track_id: &str, credits: Vec<ArtistCredit>) {
        self.write("save track credits", |tx| {
            tx.execute(
                "DELETE FROM library_identify_track_credits WHERE local_track_id = ?1",
                params![local_track_id],
            )?;
            for credit in &credits {
                tx.execute(
                    "INSERT INTO library_identify_track_credits (local_track_id, position, \
                     artist_mbid, canonical_name, credited_name) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        local_track_id,
                        credit.position as i64,
                        credit.artist_mbid,
                        credit.canonical_name,
                        credit.credited_name,
                    ],
                )?;
            }
            Ok(())
        });
    }

    fn accepted_release_mbid_for_artist(&self, source_local_artist_id: &str) -> Option<String> {
        self.read("accepted release for artist", |conn| {
            conn.query_row(
                "SELECT e.release_mbid FROM local_album_external_identities e \
                 JOIN local_album_artists a ON a.local_album_id = e.local_album_id \
                 WHERE a.local_artist_id = ?1 AND e.release_mbid IS NOT NULL \
                 ORDER BY e.selected_at DESC LIMIT 1",
                params![source_local_artist_id],
                |row| row.get(0),
            )
            .optional()
        })
        .flatten()
    }
}

impl FactsSource for SqliteIdentifyStore {
    fn album_facts(&self, local_album_id: &str) -> Option<LocalAlbumFacts> {
        self.read("album facts", |conn| {
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
            let mut stmt = conn.prepare(
                "SELECT id, title, COALESCE(artist_name, ''), track_number, disc_number, \
                 duration_seconds, embedded_recording_mbid, embedded_release_track_mbid, \
                 embedded_release_mbid, embedded_release_group_mbid, membership_locked, \
                 root_id, relative_path \
                 FROM local_tracks WHERE local_album_id = ?1 AND availability = 'indexed' \
                 ORDER BY disc_number, track_number, relative_path",
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
            }))
        })
        .flatten()
    }
}

impl ReleaseStore for SqliteIdentifyStore {
    fn release(&self, release_mbid: &str, max_age_secs: Option<u64>) -> Option<Release> {
        let oldest = max_age_secs.map_or(0.0, |age| now_secs() - age as f64);
        let payload: Option<String> = self
            .read("release document", |conn| {
                conn.query_row(
                    "SELECT canonical_payload_json FROM library_management_metadata_snapshots \
                     WHERE provider = ?1 AND entity_kind = ?2 AND entity_id = ?3 \
                     AND input_hash = ?4 AND fetched_at >= ?5 \
                     ORDER BY fetched_at DESC LIMIT 1",
                    params![
                        PROVIDER,
                        RELEASE_KIND,
                        release_mbid.trim().to_ascii_lowercase(),
                        release_input_hash(),
                        oldest,
                    ],
                    |row| row.get(0),
                )
                .optional()
            })
            .flatten();
        let payload = payload?;
        match serde_json::from_str(&payload) {
            Ok(release) => Some(release),
            Err(error) => {
                tracing::warn!(%error, release_mbid, "stored release document did not decode");
                None
            }
        }
    }

    fn save_release(&self, release: &Release) {
        let payload = match serde_json::to_string(release) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, release = release.id, "release document did not encode");
                return;
            }
        };
        let payload_sha = hex_sha256(payload.as_bytes());
        let entity = release.id.trim().to_ascii_lowercase();
        let now = now_secs();
        self.write("save release document", |tx| {
            // A refetch with the same payload refreshes nothing (rows are
            // immutable), so drop the old row first and keep one per payload.
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
            // Expired documents stay only while an identity names them.
            tx.execute(
                "DELETE FROM library_management_metadata_snapshots \
                 WHERE provider = ?1 AND entity_kind = ?2 AND expires_at < ?3 \
                 AND entity_id NOT IN (SELECT lower(release_mbid) \
                 FROM local_album_external_identities WHERE release_mbid IS NOT NULL)",
                params![PROVIDER, RELEASE_KIND, now],
            )?;
            Ok(())
        });
    }
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

impl ProofStore for SqliteIdentifyStore {
    fn proofs_for_artist(&self, source_local_artist_id: &str) -> Vec<CreditProof> {
        self.read("credit proofs", |conn| {
            let mut stmt = conn.prepare(
                "SELECT local_album_id, local_track_id, artist_mbid, release_mbid, \
                 album_identity_revision, track_identity_revision \
                 FROM library_identify_credit_proofs WHERE source_local_artist_id = ?1",
            )?;
            let rows = stmt.query_map(params![source_local_artist_id], |row| {
                Ok(CreditProof {
                    local_album_id: row.get(0)?,
                    local_track_id: row.get(1)?,
                    source_local_artist_id: source_local_artist_id.to_owned(),
                    artist_mbid: row.get(2)?,
                    release_mbid: row.get(3)?,
                    album_identity_revision: row.get::<_, i64>(4)? as u64,
                    track_identity_revision: row.get::<_, i64>(5)? as u64,
                })
            })?;
            rows.collect()
        })
        .unwrap_or_default()
    }

    fn save_proof(&self, proof: CreditProof) {
        self.write("save credit proof", |tx| {
            tx.execute(
                "INSERT OR REPLACE INTO library_identify_credit_proofs (local_album_id, \
                 local_track_id, source_local_artist_id, artist_mbid, release_mbid, \
                 album_identity_revision, track_identity_revision) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    proof.local_album_id,
                    proof.local_track_id,
                    proof.source_local_artist_id,
                    proof.artist_mbid,
                    proof.release_mbid,
                    proof.album_identity_revision as i64,
                    proof.track_identity_revision as i64,
                ],
            )
        });
    }

    fn album_revision(&self, local_album_id: &str) -> u64 {
        self.album_identity(local_album_id)
            .map(|identity| identity.row_revision)
            .unwrap_or(0)
    }

    fn track_revision(&self, local_track_id: &str) -> u64 {
        self.track_identity(local_track_id)
            .map(|identity| identity.row_revision)
            .unwrap_or(0)
    }
}

/// Alias table, owner column, and kind label for one alias kind.
fn alias_table(kind: AliasKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        AliasKind::MergedAlbum => ("local_album_aliases", "local_album_id", "merged_album"),
        AliasKind::MergedArtist => ("local_artist_aliases", "local_artist_id", "merged_artist"),
        AliasKind::MergedTrack => ("local_track_aliases", "local_track_id", "merged_track"),
    }
}

const ALIAS_KINDS: [AliasKind; 3] = [
    AliasKind::MergedAlbum,
    AliasKind::MergedArtist,
    AliasKind::MergedTrack,
];

impl AliasStore for SqliteIdentifyStore {
    fn save_alias(&self, alias: Alias) {
        let (table, owner, label) = alias_table(alias.kind);
        self.write("save alias", |tx| {
            tx.execute(
                &format!(
                    "INSERT OR REPLACE INTO {table} (alias, {owner}, kind, created_at) \
                     VALUES (?1, ?2, ?3, ?4)"
                ),
                params![alias.retired_id, alias.surviving_id, label, now_secs()],
            )
        });
    }

    fn resolve(&self, id: &str) -> String {
        for kind in ALIAS_KINDS {
            let (table, owner, _) = alias_table(kind);
            let found: Option<String> = self
                .read("resolve alias", |conn| {
                    conn.query_row(
                        &format!("SELECT {owner} FROM {table} WHERE alias = ?1"),
                        params![id],
                        |row| row.get(0),
                    )
                    .optional()
                })
                .flatten();
            if let Some(surviving) = found {
                return surviving;
            }
        }
        id.to_owned()
    }

    fn aliases_for(&self, surviving_id: &str) -> Vec<Alias> {
        let mut out = Vec::new();
        for kind in ALIAS_KINDS {
            let (table, owner, _) = alias_table(kind);
            let retired: Vec<String> = self
                .read("aliases", |conn| {
                    let mut stmt = conn.prepare(&format!(
                        "SELECT alias FROM {table} WHERE {owner} = ?1 ORDER BY alias"
                    ))?;
                    let rows = stmt.query_map(params![surviving_id], |row| row.get(0))?;
                    rows.collect()
                })
                .unwrap_or_default();
            out.extend(retired.into_iter().map(|retired_id| Alias {
                retired_id,
                surviving_id: surviving_id.to_owned(),
                kind,
            }));
        }
        out
    }

    fn retarget(&self, retired_id: &str, surviving_id: &str) {
        let kind = ALIAS_KINDS.into_iter().find(|kind| {
            let (table, _, _) = alias_table(*kind);
            self.read("alias kind", |conn| {
                conn.query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE alias = ?1)"),
                    params![retired_id],
                    |row| row.get::<_, bool>(0),
                )
            })
            .unwrap_or(false)
        });
        let Some(kind) = kind else {
            tracing::warn!(retired_id, "retarget without a saved alias skipped");
            return;
        };
        let (item_kind, playlist_column) = match kind {
            AliasKind::MergedAlbum => ("album", "local_album_id"),
            AliasKind::MergedArtist => ("artist", "local_artist_id"),
            AliasKind::MergedTrack => ("track", "local_track_id"),
        };
        self.write("retarget references", |tx| {
            tx.execute(
                "UPDATE OR IGNORE library_user_favorites SET item_id = ?1 \
                 WHERE item_kind = ?2 AND item_id = ?3",
                params![surviving_id, item_kind, retired_id],
            )?;
            // A user who already favorited the survivor keeps one row.
            tx.execute(
                "DELETE FROM library_user_favorites WHERE item_kind = ?1 AND item_id = ?2",
                params![item_kind, retired_id],
            )?;
            tx.execute(
                &format!(
                    "UPDATE library_playlist_tracks SET {playlist_column} = ?1 \
                     WHERE {playlist_column} = ?2"
                ),
                params![surviving_id, retired_id],
            )?;
            if kind == AliasKind::MergedTrack {
                tx.execute(
                    "UPDATE library_play_history SET local_track_id = ?1 \
                     WHERE local_track_id = ?2",
                    params![surviving_id, retired_id],
                )?;
            }
            Ok(())
        });
    }

    fn favorite_holds(&self, user_id: &str, item_kind: &str, item_id: &str) -> bool {
        self.read("favorite", |conn| {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM library_user_favorites \
                 WHERE user_id = ?1 AND item_kind = ?2 AND item_id = ?3)",
                params![user_id, item_kind, item_id],
                |row| row.get(0),
            )
        })
        .unwrap_or(false)
    }

    fn add_favorite(&self, user_id: &str, item_kind: &str, item_id: &str) {
        self.write("add favorite", |tx| {
            tx.execute(
                "INSERT OR IGNORE INTO library_user_favorites (user_id, item_kind, item_id, \
                 created_at) VALUES (?1, ?2, ?3, ?4)",
                params![user_id, item_kind, item_id, now_secs()],
            )
        });
    }
}

impl PinStore for SqliteIdentifyStore {
    fn pin(&self, release_group_mbid: &str) -> Option<ReleasePin> {
        self.read("release pin", |conn| {
            conn.query_row(
                "SELECT release_group_mbid, release_mbid FROM album_release_pins \
                 WHERE release_group_mbid = ?1",
                params![release_group_mbid.to_lowercase()],
                |row| {
                    Ok(ReleasePin {
                        release_group_mbid: row.get(0)?,
                        release_mbid: row.get(1)?,
                    })
                },
            )
            .optional()
        })
        .flatten()
    }

    fn set_pin(&self, pin: ReleasePin) {
        self.write("set release pin", |tx| {
            tx.execute(
                "INSERT OR REPLACE INTO album_release_pins (release_group_mbid, release_mbid, \
                 set_at) VALUES (?1, ?2, ?3)",
                params![
                    pin.release_group_mbid.to_lowercase(),
                    pin.release_mbid,
                    now_secs().to_string(),
                ],
            )
        });
    }

    fn clear_pin(&self, release_group_mbid: &str) -> bool {
        self.write("clear release pin", |tx| {
            tx.execute(
                "DELETE FROM album_release_pins WHERE release_group_mbid = ?1",
                params![release_group_mbid.to_lowercase()],
            )
        })
        .is_some_and(|deleted| deleted > 0)
    }
}

impl QueueStore for SqliteIdentifyStore {
    fn enqueue(&self, job: IdentifyJob) -> Option<IdentifyJob> {
        let key = idempotency_key(&job.local_album_id, &job.input_revision);
        self.write("enqueue identify job", |tx| {
            let now = now_ms();
            let inserted = tx.execute(
                "INSERT INTO library_identify_jobs (id, local_album_id, kind, priority, state, \
                 attempts, not_before_ms, input_revision, idempotency_key, \
                 requested_by_user_id, failure_code, created_ms, updated_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12) \
                 ON CONFLICT DO NOTHING",
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
                return Ok(job.clone());
            }
            tx.query_row(
                &format!(
                    "SELECT {JOB_COLUMNS} FROM library_identify_jobs \
                     WHERE idempotency_key = ?1 AND state IN ('queued','running','deferred')"
                ),
                params![key],
                map_job,
            )
        })
    }

    fn claim(&self, now_ms: u64, lease_ms: u64) -> Option<IdentifyJob> {
        self.write("claim identify job", |tx| {
            let now = now_ms as i64;
            let job = tx
                .query_row(
                    &format!(
                        "SELECT {JOB_COLUMNS} FROM library_identify_jobs \
                         WHERE (state IN ('queued','deferred') AND not_before_ms <= ?1) \
                         OR (state = 'running' AND lease_expires_ms <= ?1) \
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
                "UPDATE library_identify_jobs SET state = 'running', lease_expires_ms = ?1, \
                 updated_ms = ?2 WHERE id = ?3",
                params![now + lease_ms as i64, now, job.id],
            )?;
            job.state = JobState::Running;
            Ok(Some(job))
        })
        .flatten()
    }

    fn update(&self, job: IdentifyJob) {
        let key = idempotency_key(&job.local_album_id, &job.input_revision);
        self.write("update identify job", |tx| {
            tx.execute(
                "UPDATE library_identify_jobs SET state = ?1, attempts = ?2, \
                 not_before_ms = ?3, input_revision = ?4, idempotency_key = ?5, \
                 failure_code = ?6, priority = ?7, updated_ms = ?8, \
                 lease_expires_ms = CASE WHEN ?1 = 'running' THEN lease_expires_ms END \
                 WHERE id = ?9",
                params![
                    state_str(job.state),
                    job.attempts as i64,
                    job.not_before_ms as i64,
                    job.input_revision,
                    key,
                    job.failure_code,
                    job.priority as i64,
                    now_ms(),
                    job.id,
                ],
            )
        });
    }

    fn job(&self, job_id: &str) -> Option<IdentifyJob> {
        self.read("identify job", |conn| {
            conn.query_row(
                &format!("SELECT {JOB_COLUMNS} FROM library_identify_jobs WHERE id = ?1"),
                params![job_id],
                map_job,
            )
            .optional()
        })
        .flatten()
    }

    fn jobs_for_album(&self, local_album_id: &str) -> Vec<IdentifyJob> {
        self.read("album jobs", |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {JOB_COLUMNS} FROM library_identify_jobs \
                 WHERE local_album_id = ?1 ORDER BY created_ms, rowid"
            ))?;
            let rows = stmt.query_map(params![local_album_id], map_job)?;
            rows.collect()
        })
        .unwrap_or_default()
    }

    fn recover(&self) -> usize {
        self.write("recover identify jobs", |tx| {
            tx.execute(
                "UPDATE library_identify_jobs SET state = 'queued', lease_expires_ms = NULL, \
                 updated_ms = ?1 WHERE state = 'running'",
                params![now_ms()],
            )
        })
        .unwrap_or(0)
    }
}

impl ReviewStore for SqliteIdentifyStore {
    fn file(&self, review: ReviewItem) {
        let candidates = match serde_json::to_string(&review.candidates) {
            Ok(json) => json,
            Err(error) => {
                tracing::error!(%error, review = review.id, "review candidates did not encode");
                return;
            }
        };
        self.write("file review", |tx| {
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
            )
        });
    }

    fn get(&self, review_id: &str) -> Option<ReviewItem> {
        self.read("review", |conn| {
            conn.query_row(
                &format!("SELECT {REVIEW_COLUMNS} FROM library_identify_reviews WHERE id = ?1"),
                params![review_id],
                map_review,
            )
            .optional()
        })
        .flatten()
        .flatten()
    }

    fn pending_for_album(&self, local_album_id: &str) -> Vec<ReviewItem> {
        self.read("pending reviews", |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {REVIEW_COLUMNS} FROM library_identify_reviews \
                 WHERE local_album_id = ?1 AND state = 'pending' ORDER BY created_ms, id"
            ))?;
            let rows = stmt.query_map(params![local_album_id], map_review)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default()
        .into_iter()
        .flatten()
        .collect()
    }

    fn set_state(
        &self,
        review_id: &str,
        state: ReviewState,
        by_user_id: Option<&str>,
        selected_key: Option<&str>,
    ) -> bool {
        self.write("settle review", |tx| {
            tx.execute(
                "UPDATE library_identify_reviews SET state = ?1, resolved_by_user_id = ?2, \
                 selected_candidate_key = ?3, updated_ms = ?4 WHERE id = ?5",
                params![
                    review_state_str(state),
                    by_user_id,
                    selected_key,
                    now_ms(),
                    review_id
                ],
            )
        })
        .is_some_and(|changed| changed == 1)
    }

    fn approve(&self, approval: &Approval) -> Result<bool, StoreError> {
        let mut conn = self.lock();
        let outcome = (|| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
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
            seal_album_identity(&tx, &approval.album)?;
            for track in &approval.tracks {
                seal_track_identity(&tx, track)?;
            }
            tx.commit()?;
            Ok(true)
        })();
        outcome.map_err(|error: rusqlite::Error| StoreError {
            cause: format!("approve review {}: {error}", approval.review_id),
        })
    }
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
fn map_review(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<ReviewItem>> {
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
