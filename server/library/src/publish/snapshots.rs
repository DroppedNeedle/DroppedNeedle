//! Content-addressed snapshots and immutable baselines.
//!
//! Snapshot bytes live in the undo folder beside the database
//! ([`UndoDir`]), one file per content hash, and reference rows in the
//! database own them: per-operation snapshots expire by
//! `undo_retention_days`, while first-management baselines are immutable
//! and indefinite and are removed only by explicit administrator purge.
//!
//! A file is written and synced before the row that refers to it
//! commits, so a crash leaves at worst a file nothing names, which the
//! sweep removes; never a row naming a file that is not there.
//!
//! Older databases kept the bytes in `library_publish_blobs`. That table
//! stays as the way in for bytes that arrive in a database (a v2 import, a
//! backup from before the folder): the move at start takes them out into
//! the folder, and reads fall back to it until then.

use std::collections::HashSet;

use droppedneedle_kernel::db::undo_dir::{Stored, UndoDir, shard_names};
use droppedneedle_kernel::db::{CancelFlag, Db, DurableLane, Lane};
use rusqlite::{Connection, OptionalExtension, Transaction};

use super::{PublishError, sql};

/// SHA-256 hex of one byte string.
pub fn sha256_hex(bytes: &[u8]) -> String {
    droppedneedle_kernel::db::undo_dir::sha256_hex(bytes)
}

/// Lowercase hex of a finished digest.
pub(crate) fn hex_digest(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(char::from_digit(u32::from(*byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(*byte & 0x0f), 16).unwrap_or('0'));
    }
    out
}

/// About how many bytes one step of the move reads out of the database.
const MOVE_CHUNK_BYTES: usize = 64 * 1024 * 1024;

/// The undo copies: files in the undo folder, with the legacy table read
/// as a fallback.
#[derive(Debug, Clone)]
pub struct BlobStore {
    dir: UndoDir,
}

impl BlobStore {
    /// The store over the undo folder at `dir`.
    pub fn new(dir: UndoDir) -> Self {
        Self { dir }
    }

    /// The folder.
    pub fn dir(&self) -> &UndoDir {
        &self.dir
    }

    /// Write `blobs` to the folder, then run `refer` in one write that
    /// records the rows naming them; `refer` gets their hashes in order.
    /// The folder is held shared from the first file until that write has
    /// committed, so no sweep removes a file before its row lands.
    pub fn keep<R>(
        &self,
        db: &Db,
        lane: Lane,
        name: &'static str,
        blobs: &[&[u8]],
        refer: impl FnOnce(&Transaction, &[String]) -> Result<R, PublishError>,
    ) -> Result<R, PublishError> {
        let hold = self.dir.hold_shared()?;
        let mut hashes = Vec::with_capacity(blobs.len());
        for bytes in blobs {
            hashes.push(self.dir.put(&hold, bytes)?);
        }
        let kept = db.write_blocking(lane, name, |tx| refer(tx, &hashes));
        drop(hold);
        kept
    }

    /// Read and hash-verify one blob. A missing or damaged one fails
    /// closed instead of being trusted.
    pub fn get(&self, conn: &Connection, sha256: &str) -> Result<Vec<u8>, PublishError> {
        self.fetch(conn, sha256)?.ok_or_else(|| {
            PublishError::Snapshot(format!("blob {sha256} is missing or damaged").into())
        })
    }

    /// Read and hash-verify one blob; `None` when it is missing or its
    /// bytes do not match its name (logged, never used). The folder is
    /// read again after the table, so a move between the two reads is
    /// not taken for a loss.
    pub fn fetch(&self, conn: &Connection, sha256: &str) -> Result<Option<Vec<u8>>, PublishError> {
        let first = self.dir.read(sha256)?;
        if let Stored::Found(bytes) = first {
            return Ok(Some(bytes));
        }
        let row: Option<Vec<u8>> = conn
            .prepare_cached(sql::LEGACY_BLOB.text())?
            .query_row(rusqlite::params![sha256], |row| row.get(0))
            .optional()?;
        let row_damaged = match row {
            Some(bytes) if sha256_hex(&bytes) == sha256 => return Ok(Some(bytes)),
            Some(_) => true,
            None => false,
        };
        let second = self.dir.read(sha256)?;
        if let Stored::Found(bytes) = second {
            return Ok(Some(bytes));
        }
        if row_damaged || first == Stored::Corrupt || second == Stored::Corrupt {
            tracing::error!(blob = %sha256, "an undo copy does not match its name and is not used");
        } else {
            tracing::warn!(blob = %sha256, "an undo copy is missing");
        }
        Ok(None)
    }

    /// Remove the files no row names any more, and files a write left
    /// half done, one shard folder at a time. A shard someone holds is
    /// skipped until the next pass. Before anything is removed one durable
    /// write syncs the database, so the deletes of their references are on
    /// disk first and a power cut cannot bring back a reference to a file
    /// that is gone. Returns the files removed.
    pub fn sweep(&self, db: &Db) -> Result<usize, PublishError> {
        let mut freed = 0;
        for shard in shard_names() {
            let Some(hold) = self.dir.try_hold_exclusive()? else {
                continue;
            };
            let entries = self.dir.shard(&shard)?;
            for temp in &entries.temps {
                self.dir.remove_temp(&hold, temp)?;
            }
            if entries.blobs.is_empty() {
                continue;
            }
            let names = serde_json::to_string(&entries.blobs)?;
            let referenced: HashSet<String> =
                db.read_long("publish.referenced_blobs", &CancelFlag::never(), |conn| {
                    let mut stmt = conn.prepare_cached(sql::REFERENCED_AMONG.text())?;
                    let rows = stmt.query_map(rusqlite::params![names], |row| row.get(0))?;
                    Ok::<_, PublishError>(rows.collect::<Result<_, _>>()?)
                })?;
            let unreferenced: Vec<&String> = entries
                .blobs
                .iter()
                .filter(|sha256| !referenced.contains(*sha256))
                .collect();
            if unreferenced.is_empty() {
                continue;
            }
            sync_database(db)?;
            for sha256 in unreferenced {
                if self.dir.remove(&hold, sha256)? {
                    freed += 1;
                }
            }
        }
        Ok(freed)
    }

    /// Move the bytes still kept in the database into the folder, a chunk
    /// at a time: write each file and read it back, then delete its row.
    /// A crash anywhere leaves the row, and the next start finishes the
    /// file and deletes it. A row whose bytes do not match its name stays
    /// (it is never used). `go_on` is asked between chunks. Returns the
    /// rows moved.
    pub fn move_out_of_database(
        &self,
        db: &Db,
        go_on: &dyn Fn() -> bool,
    ) -> Result<usize, PublishError> {
        let mut after = String::new();
        let mut moved = 0;
        while go_on() {
            let chunk: Vec<(String, Vec<u8>)> =
                db.read_long("publish.blobs_to_move", &CancelFlag::never(), |conn| {
                    let mut stmt = conn.prepare_cached(sql::LEGACY_BLOBS_AFTER.text())?;
                    let mut rows = stmt.query(rusqlite::params![after])?;
                    let (mut chunk, mut size) = (Vec::new(), 0);
                    while size < MOVE_CHUNK_BYTES {
                        let Some(row) = rows.next()? else { break };
                        let bytes: Vec<u8> = row.get(1)?;
                        size += bytes.len().max(1);
                        chunk.push((row.get(0)?, bytes));
                    }
                    Ok::<_, PublishError>(chunk)
                })?;
            let Some((last, _)) = chunk.last() else {
                break;
            };
            after = last.clone();
            let hold = self.dir.hold_shared()?;
            let mut done = Vec::with_capacity(chunk.len());
            for (sha256, bytes) in chunk {
                if sha256_hex(&bytes) != sha256 {
                    tracing::error!(
                        blob = %sha256,
                        "an undo copy in the database does not match its name; it stays there and is never used"
                    );
                    continue;
                }
                self.dir.put(&hold, &bytes)?;
                if self.dir.read(&sha256)? != Stored::Found(bytes) {
                    return Err(PublishError::Snapshot(
                        format!("blob {sha256} did not read back from the undo folder").into(),
                    ));
                }
                done.push(sha256);
            }
            db.write_blocking(Lane::Background, "publish.move_blobs", |tx| {
                let mut stmt = tx.prepare_cached(sql::DELETE_LEGACY_BLOB.text())?;
                for sha256 in &done {
                    stmt.execute(rusqlite::params![sha256])?;
                }
                Ok::<_, PublishError>(())
            })?;
            moved += done.len();
            drop(hold);
        }
        Ok(moved)
    }
}

/// One durable write, so every commit before it is on disk. A durable
/// write that changes no row syncs nothing, so this one writes a marker
/// row and deletes it again in the same transaction.
fn sync_database(db: &Db) -> Result<(), PublishError> {
    db.write_durable(DurableLane::Background, "publish.sync_before_sweep", |tx| {
        tx.prepare_cached(sql::SYNC_MARK_INSERT.text())?
            .execute([])?;
        tx.prepare_cached(sql::SYNC_MARK_DELETE.text())?
            .execute([])?;
        Ok::<_, PublishError>(())
    })
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

/// Delete the rows of bytes still in the database that no snapshot,
/// baseline or preview refers to any more,
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
            return Err(PublishError::Snapshot(
                format!("baseline for track {track_id} already exists and is immutable").into(),
            ));
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
