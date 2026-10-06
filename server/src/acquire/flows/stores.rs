//! State behind the acquisition flows.
//!
//! Follow-poll cursors, the upgrade worklist and drop-import quarantine
//! entries live in SQLite (migration 0007 plus `artist_known_releases`),
//! read through the pool and written through the writer lane. The request
//! ledger and the wanted watches are the requests module's durable stores,
//! shared with the loops. Library presence and the admin directory are
//! derived views refreshed from their owners.

use std::collections::HashSet;
use std::sync::Mutex;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::acquire::db::AcquireDb;
use crate::db::{DbError, map_sqlx_busy};

/// A flows store failure, already logged by the caller with its context.
#[derive(Debug, thiserror::Error)]
#[error("flows store {operation} failed: {cause}")]
pub struct StoreFailure {
    /// Operation name.
    pub operation: &'static str,
    /// Cause text.
    pub cause: String,
}

fn lane_failure(operation: &'static str, error: DbError) -> StoreFailure {
    StoreFailure {
        operation,
        cause: error.to_string(),
    }
}

fn read_failure(operation: &'static str, error: sqlx::Error) -> StoreFailure {
    lane_failure(operation, map_sqlx_busy(operation, error))
}

/// One followed artist's poll cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowCursor {
    /// Artist MBID (lowercase key).
    pub artist_mbid: String,
    /// True once the baseline poll has run.
    pub baselined: bool,
    /// Prior successful UTC cursor date (`YYYY-MM-DD`).
    pub cursor_date: Option<String>,
    /// Known release-group MBIDs (baseline inventory).
    pub known: HashSet<String>,
    /// Unix seconds when the next poll is due.
    pub next_poll_at: i64,
    /// Approved followers auto-enqueued on new releases.
    pub followers: Vec<String>,
    /// Releases held for a future date (dispatch-pending).
    pub pending: Vec<PendingRelease>,
}

/// One future-dated release held until its date arrives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRelease {
    /// Release-group MBID.
    pub rg_mbid: String,
    /// Release title.
    pub title: String,
    /// First release date (`YYYY-MM-DD`).
    pub date: String,
}

/// Durable follow-poll cursors.
#[derive(Clone)]
pub struct FollowStore {
    db: AcquireDb,
}

/// One cursor row as the pool reads it.
type CursorRow = (String, i64, Option<String>, i64, String, String);

impl FollowStore {
    /// Cursors over one database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// Insert or replace one cursor, its known set included.
    pub async fn upsert(&self, cursor: FollowCursor) -> Result<(), StoreFailure> {
        let followers = serde_json::to_string(&cursor.followers).map_err(|error| StoreFailure {
            operation: "follows.upsert",
            cause: error.to_string(),
        })?;
        let pending = serde_json::to_string(&cursor.pending).map_err(|error| StoreFailure {
            operation: "follows.upsert",
            cause: error.to_string(),
        })?;
        self.db
            .write_background("follows.upsert", move |tx| {
                let artist = cursor.artist_mbid.to_lowercase();
                tx.execute(
                    "INSERT OR REPLACE INTO acquire_follow_cursors (artist_mbid_lower, \
                     baselined, cursor_date, next_poll_at, followers, pending) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        artist,
                        cursor.baselined,
                        cursor.cursor_date,
                        cursor.next_poll_at,
                        followers,
                        pending
                    ],
                )?;
                tx.execute(
                    "DELETE FROM artist_known_releases WHERE artist_mbid_lower = ?1",
                    params![artist],
                )?;
                for rg in &cursor.known {
                    tx.execute(
                        "INSERT OR IGNORE INTO artist_known_releases \
                         (artist_mbid_lower, rg_mbid_lower) VALUES (?1, ?2)",
                        params![artist, rg.to_lowercase()],
                    )?;
                }
                Ok(())
            })
            .await
            .map_err(|error| lane_failure("follows.upsert", error))
    }

    /// One cursor, if present.
    pub async fn get(&self, artist_mbid: &str) -> Result<Option<FollowCursor>, StoreFailure> {
        let row: Option<CursorRow> = sqlx::query_as(
            "SELECT artist_mbid_lower, baselined, cursor_date, next_poll_at, followers, pending \
             FROM acquire_follow_cursors WHERE artist_mbid_lower = ?1",
        )
        .bind(artist_mbid.to_lowercase())
        .fetch_optional(self.db.pool())
        .await
        .map_err(|error| read_failure("follows.get", error))?;
        match row {
            Some(row) => Ok(Some(self.hydrate(row).await?)),
            None => Ok(None),
        }
    }

    /// Artists due at `now`, oldest first, capped at `limit`.
    pub async fn list_due(
        &self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<FollowCursor>, StoreFailure> {
        let rows: Vec<CursorRow> = sqlx::query_as(
            "SELECT artist_mbid_lower, baselined, cursor_date, next_poll_at, followers, pending \
             FROM acquire_follow_cursors WHERE next_poll_at <= ?1 \
             ORDER BY next_poll_at, artist_mbid_lower LIMIT ?2",
        )
        .bind(now)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| read_failure("follows.list_due", error))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(self.hydrate(row).await?);
        }
        Ok(out)
    }

    /// Replace one cursor after a poll.
    pub async fn record_poll(&self, cursor: &FollowCursor) -> Result<(), StoreFailure> {
        self.upsert(cursor.clone()).await
    }

    /// Decode one row and load its known set.
    async fn hydrate(&self, row: CursorRow) -> Result<FollowCursor, StoreFailure> {
        let (artist, baselined, cursor_date, next_poll_at, followers, pending) = row;
        let known: Vec<String> = sqlx::query_scalar(
            "SELECT rg_mbid_lower FROM artist_known_releases WHERE artist_mbid_lower = ?1",
        )
        .bind(&artist)
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| read_failure("follows.known", error))?;
        let decode = |error: serde_json::Error| StoreFailure {
            operation: "follows.decode",
            cause: error.to_string(),
        };
        Ok(FollowCursor {
            artist_mbid: artist,
            baselined: baselined != 0,
            cursor_date,
            known: known.into_iter().collect(),
            next_poll_at,
            followers: serde_json::from_str(&followers).map_err(decode)?,
            pending: serde_json::from_str(&pending).map_err(decode)?,
        })
    }
}

/// One cutoff-unmet album awaiting an upgrade grab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeItem {
    /// Release-group MBID.
    pub rg_mbid: String,
    /// Artist name for the task row.
    pub artist: String,
    /// Album title for the task row.
    pub title: String,
}

/// Durable background-upgrade worklist: cutoff-unmet albums, oldest first.
#[derive(Clone)]
pub struct UpgradeWorklist {
    db: AcquireDb,
}

impl UpgradeWorklist {
    /// Worklist over one database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// Replace the worklist contents, keeping the given order.
    pub async fn set(&self, items: Vec<UpgradeItem>) -> Result<(), StoreFailure> {
        self.db
            .write_background("upgrades.set", move |tx| {
                tx.execute("DELETE FROM acquire_upgrade_worklist", [])?;
                for (position, item) in items.iter().enumerate() {
                    tx.execute(
                        "INSERT OR REPLACE INTO acquire_upgrade_worklist \
                         (release_group_mbid, artist_name, album_title, position) \
                         VALUES (?1, ?2, ?3, ?4)",
                        params![
                            item.rg_mbid,
                            item.artist,
                            item.title,
                            i64::try_from(position).unwrap_or(i64::MAX)
                        ],
                    )?;
                }
                Ok(())
            })
            .await
            .map_err(|error| lane_failure("upgrades.set", error))
    }

    /// Cutoff-unmet albums, oldest first.
    pub async fn list_cutoff_unmet(&self) -> Result<Vec<UpgradeItem>, StoreFailure> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT release_group_mbid, artist_name, album_title \
             FROM acquire_upgrade_worklist ORDER BY position, release_group_mbid",
        )
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| read_failure("upgrades.list", error))?;
        Ok(rows
            .into_iter()
            .map(|(rg_mbid, artist, title)| UpgradeItem {
                rg_mbid,
                artist,
                title,
            })
            .collect())
    }
}

/// Background-upgrade policy (v2 `download_policy` upgrade half).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradePolicy {
    /// Upgrades allowed at all.
    pub upgrade_allowed: bool,
    /// The background scan enabled (default off in v2).
    pub scan_enabled: bool,
    /// Grabs per sweep, at most.
    pub max_per_run: usize,
    /// Sweep cadence in hours (default 12 in v2).
    pub interval_hours: u64,
}

impl Default for UpgradePolicy {
    fn default() -> Self {
        Self {
            upgrade_allowed: false,
            scan_enabled: false,
            max_per_run: 5,
            interval_hours: 12,
        }
    }
}

/// One quarantine entry: a bad source the flows must not retry blindly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineEntry {
    /// Quarantined key (source path or release key).
    pub key: String,
    /// Album key for album-scoped clears, when known.
    pub album_key: Option<String>,
    /// Why the source was quarantined.
    pub reason: String,
    /// Unix seconds when the entry landed.
    pub at: i64,
}

/// Durable drop-import quarantine registry.
#[derive(Clone)]
pub struct QuarantineStore {
    db: AcquireDb,
}

impl QuarantineStore {
    /// Registry over one database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// Record one quarantine entry.
    pub async fn quarantine(&self, entry: QuarantineEntry) -> Result<(), StoreFailure> {
        self.db
            .write_background("quarantine.add", move |tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO acquire_flow_quarantine \
                     (key, album_key, reason, quarantined_at) VALUES (?1, ?2, ?3, ?4)",
                    params![entry.key, entry.album_key, entry.reason, entry.at],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| lane_failure("quarantine.add", error))
    }

    /// One entry, if present.
    pub async fn get(&self, key: &str) -> Result<Option<QuarantineEntry>, StoreFailure> {
        let row: Option<(String, Option<String>, String, i64)> = sqlx::query_as(
            "SELECT key, album_key, reason, quarantined_at FROM acquire_flow_quarantine \
             WHERE key = ?1",
        )
        .bind(key)
        .fetch_optional(self.db.pool())
        .await
        .map_err(|error| read_failure("quarantine.get", error))?;
        Ok(row.map(|(key, album_key, reason, at)| QuarantineEntry {
            key,
            album_key,
            reason,
            at,
        }))
    }

    /// True when the key is quarantined.
    pub async fn is_quarantined(&self, key: &str) -> Result<bool, StoreFailure> {
        Ok(self.get(key).await?.is_some())
    }

    /// Drop one entry (manual resolve).
    pub async fn clear(&self, key: &str) -> Result<(), StoreFailure> {
        let key = key.to_owned();
        self.db
            .write_background("quarantine.clear", move |tx| {
                tx.execute(
                    "DELETE FROM acquire_flow_quarantine WHERE key = ?1",
                    params![key],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| lane_failure("quarantine.clear", error))
    }

    /// Drop every entry scoped to one album (retry reconsiders quarantined
    /// sources for album downloads only, v2 orchestrator rule).
    pub async fn clear_for_album(&self, album_key: &str) -> Result<usize, StoreFailure> {
        let album_key = album_key.to_owned();
        self.db
            .write_background("quarantine.clear_album", move |tx| {
                Ok(tx.execute(
                    "DELETE FROM acquire_flow_quarantine WHERE album_key = ?1",
                    params![album_key],
                )?)
            })
            .await
            .map_err(|error| lane_failure("quarantine.clear_album", error))
    }

    /// Every entry, oldest first.
    pub async fn list(&self) -> Result<Vec<QuarantineEntry>, StoreFailure> {
        let rows: Vec<(String, Option<String>, String, i64)> = sqlx::query_as(
            "SELECT key, album_key, reason, quarantined_at FROM acquire_flow_quarantine \
             ORDER BY quarantined_at, key",
        )
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| read_failure("quarantine.list", error))?;
        Ok(rows
            .into_iter()
            .map(|(key, album_key, reason, at)| QuarantineEntry {
                key,
                album_key,
                reason,
                at,
            })
            .collect())
    }
}

/// Library presence: the release groups the library holds, so the wanted
/// watcher and status sync never re-want an owned album. Production reads
/// the catalog the same way the download landing does (an indexed file on
/// an album identified as the group, or whose own tags name the group);
/// tests mark MBIDs by hand.
#[derive(Debug, Default)]
pub struct LibraryPresence {
    mbids: Mutex<HashSet<String>>,
    catalog: Option<sqlx::SqlitePool>,
}

impl LibraryPresence {
    /// Empty presence.
    pub fn new() -> Self {
        Self::default()
    }

    /// Presence read from the library catalog over the reader pool.
    pub fn over_catalog(catalog: sqlx::SqlitePool) -> Self {
        Self {
            mbids: Mutex::new(HashSet::new()),
            catalog: Some(catalog),
        }
    }

    /// Mark MBIDs as owned.
    pub fn add(&self, mbids: &[&str]) {
        if let Ok(mut owned) = self.mbids.lock() {
            for mbid in mbids {
                owned.insert((*mbid).to_owned());
            }
        }
    }

    /// True when the library holds the MBID. A catalog read failure is
    /// logged and reads as not held: the watcher then searches once more
    /// rather than dropping a request.
    pub async fn contains(&self, mbid: &str) -> bool {
        let marked = self
            .mbids
            .lock()
            .map(|owned| owned.contains(mbid))
            .unwrap_or(false);
        if marked {
            return true;
        }
        let Some(catalog) = &self.catalog else {
            return false;
        };
        let sql = format!(
            "SELECT EXISTS (SELECT 1 {})",
            crate::acquire::landing::library::OWNED_TRACKS_FROM
        );
        match sqlx::query_scalar::<_, bool>(&sql)
            .bind(mbid.trim().to_ascii_lowercase())
            .fetch_one(catalog)
            .await
        {
            Ok(owned) => owned,
            Err(error) => {
                tracing::warn!(mbid, %error, "library presence unreadable");
                false
            }
        }
    }
}

/// Admin directory: upgrades are a curator action owned by the oldest admin
/// (v2 `run_background_upgrade_sweep`).
#[derive(Debug, Default)]
pub struct AdminDirectory {
    admins: Mutex<Vec<String>>,
}

impl AdminDirectory {
    /// Empty directory.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the admin list, oldest first.
    pub fn set(&self, admins: Vec<String>) {
        if let Ok(mut current) = self.admins.lock() {
            *current = admins;
        }
    }

    /// Oldest admin id, or `None` when nobody can own the sweep.
    pub fn oldest_admin(&self) -> Option<String> {
        self.admins
            .lock()
            .ok()
            .and_then(|admins| admins.first().cloned())
    }
}
