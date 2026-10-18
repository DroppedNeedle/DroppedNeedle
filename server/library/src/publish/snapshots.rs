//! Content-addressed snapshots and immutable baselines.
//!
//! Snapshot bytes live in the application database, deduplicated by
//! content hash, and reference rows own them: per-operation snapshots expire by
//! `undo_retention_days`, while first-management baselines are
//! immutable and indefinite and are removed only by explicit
//! administrator purge.

use droppedneedle_kernel::db::{CancelFlag, Db, Lane};
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use super::{PublishError, sql};

/// SHA-256 hex of one byte string.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(*byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(*byte & 0x0f), 16).unwrap_or('0'));
    }
    out
}

/// Content-addressed blob store in the application database.
pub struct BlobStore<'a> {
    conn: &'a Connection,
}

impl<'a> BlobStore<'a> {
    /// Borrow a connection as a blob store.
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Store bytes, returning their content hash. Identical bytes
    /// deduplicate to one row; a stored blob whose bytes disagree with
    /// its hash fails closed instead of being trusted.
    pub fn put(&self, bytes: &[u8]) -> Result<String, PublishError> {
        let hash = sha256_hex(bytes);
        self.conn.execute(
            "INSERT OR IGNORE INTO library_publish_blobs (sha256, bytes) VALUES (?1, ?2)",
            rusqlite::params![hash, bytes],
        )?;
        self.get(&hash)?;
        Ok(hash)
    }

    /// Delete the bytes no snapshot or baseline refers to any more (undo
    /// periods that ran out, purged baselines). Callers hold the publish
    /// lock, so no publish is between storing a blob and referring to it.
    pub fn sweep_unreferenced(&self) -> Result<usize, PublishError> {
        Ok(self.conn.execute(
            "DELETE FROM library_publish_blobs WHERE NOT EXISTS \
             (SELECT 1 FROM library_publish_blob_refs r WHERE r.sha256 = library_publish_blobs.sha256)",
            [],
        )?)
    }

    /// Read and hash-verify one blob.
    pub fn get(&self, sha256: &str) -> Result<Vec<u8>, PublishError> {
        let bytes: Vec<u8> = self
            .conn
            .query_row(
                "SELECT bytes FROM library_publish_blobs WHERE sha256 = ?1",
                rusqlite::params![sha256],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| PublishError::Snapshot(format!("blob {sha256} is missing")))?;
        if sha256_hex(&bytes) != sha256 {
            return Err(PublishError::Snapshot(format!(
                "blob {sha256} failed content verification"
            )));
        }
        Ok(bytes)
    }
}

/// Per-operation before-state snapshots. Each successful
/// publisher call captures the semantic audio snapshot plus the exact
/// sidecar/external-art manifest; rows expire by retention day.
pub struct SnapshotStore<'a> {
    conn: &'a Connection,
}

impl<'a> SnapshotStore<'a> {
    /// Borrow a connection as a snapshot store.
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Record one operation snapshot and its blob reference.
    pub fn record(
        &self,
        id: &str,
        bundle_id: &str,
        track_id: &str,
        blob_sha256: &str,
        created_day: i64,
        expires_day: i64,
    ) -> Result<(), PublishError> {
        self.conn.execute(
            "INSERT INTO library_publish_snapshots
             (id, bundle_id, track_id, blob_sha256, created_day, expires_day)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                id,
                bundle_id,
                track_id,
                blob_sha256,
                created_day,
                expires_day
            ],
        )?;
        self.add_ref(blob_sha256, "operation", bundle_id)?;
        Ok(())
    }

    /// Add a blob reference row; shared hashes are reused even across
    /// owner kinds instead of duplicating blob-kind metadata.
    pub fn add_ref(
        &self,
        sha256: &str,
        owner_kind: &str,
        owner_id: &str,
    ) -> Result<(), PublishError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO library_publish_blob_refs (sha256, owner_kind, owner_id)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![sha256, owner_kind, owner_id],
        )?;
        Ok(())
    }

    /// Load one snapshot row with its expiry day.
    pub fn get(&self, id: &str) -> Result<Option<(String, String, i64)>, PublishError> {
        self.conn
            .query_row(
                "SELECT track_id, blob_sha256, expires_day
                 FROM library_publish_snapshots WHERE id = ?1",
                rusqlite::params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(PublishError::from)
    }

    /// Every snapshot in a bundle: (id, track id, blob hash, expiry day).
    pub fn bundle(
        &self,
        bundle_id: &str,
    ) -> Result<Vec<(String, String, String, i64)>, PublishError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, track_id, blob_sha256, expires_day
             FROM library_publish_snapshots WHERE bundle_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(rusqlite::params![bundle_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

/// Bundles whose expired snapshots one write deletes.
const PURGE_CHUNK_BUNDLES: usize = 10;

/// Blobs one write of the sweep deletes.
const SWEEP_CHUNK_BLOBS: usize = 500;

/// Delete the snapshots expired by `today_day` with their operation
/// references, a few bundles a write. Every snapshot of a bundle shares one
/// expiry day, so each write takes whole snapshots with their references.
/// Blob bytes with remaining owners survive; the sweep owns unreferenced
/// bytes later. Returns the snapshots deleted.
pub fn purge_expired(db: &Db, today_day: i64) -> Result<usize, PublishError> {
    let bundles: Vec<String> = db.read_long(
        "publish.expired_snapshot_bundles",
        &CancelFlag::never(),
        |conn| {
            let mut stmt = conn.prepare_cached(sql::EXPIRED_SNAPSHOT_BUNDLES.text())?;
            let rows = stmt.query_map(rusqlite::params![today_day], |row| row.get(0))?;
            Ok::<_, PublishError>(rows.collect::<Result<_, _>>()?)
        },
    )?;
    let mut purged = 0;
    for chunk in bundles.chunks(PURGE_CHUNK_BUNDLES) {
        purged += db.write_blocking(Lane::Background, "publish.purge_snapshots", |tx| {
            let mut deleted = 0;
            for bundle_id in chunk {
                tx.prepare_cached(sql::DELETE_OPERATION_REFS.text())?
                    .execute(rusqlite::params![bundle_id])?;
                deleted += tx
                    .prepare_cached(sql::DELETE_EXPIRED_SNAPSHOTS.text())?
                    .execute(rusqlite::params![bundle_id, today_day])?;
            }
            Ok::<_, PublishError>(deleted)
        })?;
    }
    Ok(purged)
}

/// Delete the bytes no snapshot, baseline or preview refers to any more,
/// from a list read first, a chunk a write. Each delete checks the
/// references again in its own transaction, so a reference written since
/// the list was read keeps its blob. Returns the blobs deleted.
pub fn sweep_unreferenced_blobs(db: &Db) -> Result<usize, PublishError> {
    let hashes: Vec<String> =
        db.read_long("publish.unreferenced_blobs", &CancelFlag::never(), |conn| {
            let mut stmt = conn.prepare_cached(sql::UNREFERENCED_BLOBS.text())?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            Ok::<_, PublishError>(rows.collect::<Result<_, _>>()?)
        })?;
    let mut freed = 0;
    for chunk in hashes.chunks(SWEEP_CHUNK_BLOBS) {
        freed += db.write_blocking(Lane::Background, "publish.sweep_blobs", |tx| {
            let mut stmt = tx.prepare_cached(sql::DELETE_UNREFERENCED_BLOB.text())?;
            let mut deleted = 0;
            for sha256 in chunk {
                deleted += stmt.execute(rusqlite::params![sha256])?;
            }
            Ok::<_, PublishError>(deleted)
        })?;
    }
    Ok(freed)
}

/// First-management baselines: immutable pre-DroppedNeedle semantic
/// state, kept until explicit administrator purge.
pub struct BaselineStore<'a> {
    conn: &'a Connection,
}

impl<'a> BaselineStore<'a> {
    /// Borrow a connection as a baseline store.
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Capture a baseline once. Recapturing an existing track baseline
    /// fails closed: baselines stay pre-DroppedNeedle forever.
    pub fn capture(
        &self,
        track_id: &str,
        blob_sha256: &str,
        original_root: &str,
        original_rel: &str,
        created_day: i64,
    ) -> Result<(), PublishError> {
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO library_publish_baselines
             (track_id, blob_sha256, original_root, original_rel, created_day)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                track_id,
                blob_sha256,
                original_root,
                original_rel,
                created_day
            ],
        )?;
        if changed == 0 {
            return Err(PublishError::Snapshot(format!(
                "baseline for track {track_id} already exists and is immutable"
            )));
        }
        self.conn.execute(
            "INSERT OR IGNORE INTO library_publish_blob_refs (sha256, owner_kind, owner_id)
             VALUES (?1, 'baseline', ?2)",
            rusqlite::params![blob_sha256, track_id],
        )?;
        Ok(())
    }

    /// Load one baseline: (blob hash, original root, original rel path).
    pub fn get(&self, track_id: &str) -> Result<Option<(String, String, String)>, PublishError> {
        self.conn
            .query_row(
                "SELECT blob_sha256, original_root, original_rel
                 FROM library_publish_baselines WHERE track_id = ?1",
                rusqlite::params![track_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(PublishError::from)
    }

    /// True when v2 recorded an original for this track (its carried
    /// management state names a v2 baseline, or v2's baseline row itself
    /// came across) but no baseline reached v3:
    /// the v2 import has not run that far, or could not translate it. A
    /// managed write would then record the file v2 already changed as its
    /// original, losing the real one, so the track waits.
    pub fn v2_original_missing(&self, track_id: &str) -> Result<bool, PublishError> {
        self.conn
            .query_row(
                "SELECT (EXISTS(SELECT 1 FROM library_track_management_state m \
                 WHERE m.local_track_id = ?1 AND m.baseline_id IS NOT NULL) \
                 OR EXISTS(SELECT 1 FROM library_management_baselines v \
                 WHERE v.local_track_id = ?1)) \
                 AND NOT EXISTS (SELECT 1 FROM library_publish_baselines b \
                 WHERE b.track_id = ?1)",
                rusqlite::params![track_id],
                |row| row.get(0),
            )
            .map_err(PublishError::from)
    }

    /// Baseline count plus distinct referenced blob count for the purge
    /// impact report.
    pub fn impact(&self) -> Result<(usize, usize), PublishError> {
        let baselines: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM library_publish_baselines",
            [],
            |row| row.get(0),
        )?;
        let blobs: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT sha256) FROM library_publish_blob_refs WHERE owner_kind = 'baseline'",
            [],
            |row| row.get(0),
        )?;
        Ok((baselines as usize, blobs as usize))
    }
}
