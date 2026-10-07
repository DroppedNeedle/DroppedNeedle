//! State behind the acquisition flows.
//!
//! Follow-poll cursors (keyed on the artists in `user_followed_artists`)
//! and drop-import quarantine entries live in SQLite (migration 0007 plus
//! `artist_known_releases`), read through the pool and written through the
//! writer lane. The request ledger and the wanted watches are the requests
//! module's durable stores, shared with the loops. Library presence, the
//! upgrade worklist and the admin directory are read from their owners'
//! tables (the catalog, the accounts) on every call.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use futures_util::future::BoxFuture;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::acquire::db::AcquireDb;
use crate::db::{DbError, map_sqlx_busy};
use crate::runtime_config::sections::tier_rank;

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

/// One followed artist's poll cursor. Every artist someone follows has
/// one; an artist polled for the first time starts with no baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowCursor {
    /// Artist MBID as first followed (the store keys on its lowercase).
    pub artist_mbid: String,
    /// Artist name snapshot from the follow row.
    pub artist_name: String,
    /// True once the baseline poll has run.
    pub baselined: bool,
    /// Prior successful UTC cursor date (`YYYY-MM-DD`).
    pub cursor_date: Option<String>,
    /// Release groups already seen for this artist, lowercase.
    pub known: HashSet<String>,
    /// Unix seconds when the next poll is due.
    pub next_poll_at: i64,
    /// Releases held for auto-download until their date (dispatch-pending).
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

/// One new release for the followers' feed (`new_release_feed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedRelease {
    /// Release-group MBID as MusicBrainz spells it.
    pub rg_mbid: String,
    /// Release title.
    pub title: String,
    /// Primary type, when known.
    pub primary_type: Option<String>,
    /// Secondary types, comma separated (v2 shape), when any.
    pub secondary_types: Option<String>,
    /// First release date, when known.
    pub first_release_date: Option<String>,
}

/// Durable follow-poll cursors over `user_followed_artists`.
#[derive(Clone)]
pub struct FollowStore {
    db: AcquireDb,
}

/// One cursor as the pool reads it: lowercase key, MBID as followed,
/// name, then the cursor columns (all null for a never-polled artist).
type CursorRow = (
    String,
    String,
    String,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<String>,
);

impl FollowStore {
    /// Cursors over one database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// Followed artists whose poll is due at `now`, never-polled first,
    /// then oldest due, capped at `limit`. Artists nobody follows any more
    /// drop out even when a cursor remains.
    pub async fn list_due(
        &self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<FollowCursor>, StoreFailure> {
        let rows: Vec<CursorRow> = sqlx::query_as(
            "SELECT f.artist_mbid_lower, MIN(f.artist_mbid), MIN(f.artist_name), \
             MAX(c.baselined), MAX(c.cursor_date), MAX(c.next_poll_at), MAX(c.pending) \
             FROM user_followed_artists f \
             LEFT JOIN acquire_follow_cursors c ON c.artist_mbid_lower = f.artist_mbid_lower \
             GROUP BY f.artist_mbid_lower \
             HAVING MAX(c.next_poll_at) IS NULL OR MAX(c.next_poll_at) <= ?1 \
             ORDER BY COALESCE(MAX(c.next_poll_at), 0), f.artist_mbid_lower LIMIT ?2",
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

    /// Users whose auto-download is live for this artist, ordered by user
    /// id so the owner pick is stable: intent on, and either a curator
    /// role (they approve themselves) or an approved grant.
    pub async fn auto_followers(&self, artist_mbid: &str) -> Result<Vec<String>, StoreFailure> {
        sqlx::query_scalar(
            "SELECT f.user_id FROM user_followed_artists f \
             JOIN auth_users u ON u.id = f.user_id \
             LEFT JOIN auto_download_approvals a \
               ON a.user_id = f.user_id AND a.artist_mbid_lower = f.artist_mbid_lower \
             WHERE f.artist_mbid_lower = ?1 AND f.auto_download = 1 \
               AND (u.role IN ('admin', 'trusted') OR a.state = 'approved') \
             ORDER BY f.user_id",
        )
        .bind(artist_mbid.to_lowercase())
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| read_failure("follows.auto_followers", error))
    }

    /// Store one cursor after a poll, with its known set, and add the new
    /// releases to the followers' feed in the same transaction. A release
    /// already in the feed keeps its first discovery time.
    pub async fn record_poll(
        &self,
        cursor: &FollowCursor,
        feed: &[FeedRelease],
    ) -> Result<(), StoreFailure> {
        let pending = serde_json::to_string(&cursor.pending).map_err(|error| StoreFailure {
            operation: "follows.record_poll",
            cause: error.to_string(),
        })?;
        let cursor = cursor.clone();
        let feed = feed.to_vec();
        self.db
            .write_background("follows.record_poll", move |tx| {
                let artist = cursor.artist_mbid.to_lowercase();
                tx.execute(
                    "INSERT OR REPLACE INTO acquire_follow_cursors (artist_mbid_lower, \
                     baselined, cursor_date, next_poll_at, followers, pending) \
                     VALUES (?1, ?2, ?3, ?4, '[]', ?5)",
                    params![
                        artist,
                        cursor.baselined,
                        cursor.cursor_date,
                        cursor.next_poll_at,
                        pending
                    ],
                )?;
                for rg in &cursor.known {
                    tx.execute(
                        "INSERT OR IGNORE INTO artist_known_releases \
                         (artist_mbid_lower, rg_mbid_lower) VALUES (?1, ?2)",
                        params![artist, rg.to_lowercase()],
                    )?;
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|span| span.as_secs_f64())
                    .unwrap_or(0.0);
                for row in &feed {
                    tx.execute(
                        "INSERT OR IGNORE INTO new_release_feed (release_group_mbid_lower, \
                         release_group_mbid, artist_mbid_lower, artist_name, title, \
                         primary_type, secondary_types, first_release_date, discovered_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            row.rg_mbid.to_lowercase(),
                            row.rg_mbid,
                            artist,
                            cursor.artist_name,
                            row.title,
                            row.primary_type,
                            row.secondary_types,
                            row.first_release_date,
                            now
                        ],
                    )?;
                }
                Ok(())
            })
            .await
            .map_err(|error| lane_failure("follows.record_poll", error))
    }

    /// One artist's cursor, if it has been polled.
    pub async fn get(&self, artist_mbid: &str) -> Result<Option<FollowCursor>, StoreFailure> {
        let row: Option<CursorRow> = sqlx::query_as(
            "SELECT c.artist_mbid_lower, c.artist_mbid_lower, '', c.baselined, c.cursor_date, \
             c.next_poll_at, c.pending FROM acquire_follow_cursors c \
             WHERE c.artist_mbid_lower = ?1",
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

    /// Decode one row and load its known set.
    async fn hydrate(&self, row: CursorRow) -> Result<FollowCursor, StoreFailure> {
        let (lower, mbid, name, baselined, cursor_date, next_poll_at, pending) = row;
        let known: Vec<String> = sqlx::query_scalar(
            "SELECT rg_mbid_lower FROM artist_known_releases WHERE artist_mbid_lower = ?1",
        )
        .bind(&lower)
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| read_failure("follows.known", error))?;
        let pending = match pending {
            Some(text) => serde_json::from_str(&text).map_err(|error| StoreFailure {
                operation: "follows.decode",
                cause: error.to_string(),
            })?,
            None => Vec::new(),
        };
        Ok(FollowCursor {
            artist_mbid: mbid,
            artist_name: name,
            baselined: baselined.unwrap_or(0) != 0,
            cursor_date,
            known: known.into_iter().collect(),
            next_poll_at: next_poll_at.unwrap_or(0),
            pending,
        })
    }
}

/// One album whose files sit below the quality cutoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeItem {
    /// Release-group MBID.
    pub rg_mbid: String,
    /// Artist name for the task row.
    pub artist: String,
    /// Album title for the task row.
    pub title: String,
    /// The album's worst file tier (`low` .. `mp3_320`).
    pub current_tier: &'static str,
    /// Indexed tracks of the album.
    pub track_count: i64,
    pub year: Option<i64>,
    /// The album artist's MusicBrainz id, when identified.
    pub artist_mbid: Option<String>,
}

/// Where the upgrade sweep reads its worklist.
pub trait CutoffList: Send + Sync {
    /// Albums whose worst file sits below `cutoff`.
    fn cutoff_unmet<'a>(
        &'a self,
        cutoff: &'a str,
    ) -> BoxFuture<'a, Result<Vec<UpgradeItem>, StoreFailure>>;
}

/// One indexed track of an identified album, with its quality facts.
type CatalogTrack = (
    String,
    String,
    Option<String>,
    Option<i64>,
    Option<String>,
    String,
    Option<i64>,
    Option<i64>,
);

fn tier_of(format: &str, bitrate: Option<i64>, depth: Option<i64>) -> &'static str {
    crate::acquire::landing::quality::tier_for(
        format,
        bitrate.and_then(|rate| u32::try_from(rate).ok()),
        depth.and_then(|depth| u8::try_from(depth).ok()),
    )
}

/// The upgrade worklist, read from the library catalog on every call
/// (v2 `list_cutoff_unmet`): an album is as good as its worst file.
#[derive(Clone)]
pub struct UpgradeWorklist {
    db: AcquireDb,
}

impl UpgradeWorklist {
    /// Worklist over the application database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// Albums whose worst indexed file sits below `cutoff`, worst first,
    /// then by artist and title.
    pub async fn list_cutoff_unmet(&self, cutoff: &str) -> Result<Vec<UpgradeItem>, StoreFailure> {
        let Some(cutoff_rank) = tier_rank(cutoff) else {
            return Ok(Vec::new());
        };
        let rows: Vec<CatalogTrack> = sqlx::query_as(
            "SELECT lower(COALESCE(ai.release_group_mbid, t.embedded_release_group_mbid)), \
                    b.title, b.album_artist_name, b.year, ae.provider_artist_id, \
                    t.file_format, t.bit_rate, t.bit_depth \
             FROM local_tracks t \
             JOIN local_albums b ON b.id = t.local_album_id AND b.retired_into_album_id IS NULL \
             LEFT JOIN local_album_external_identities ai ON ai.local_album_id = b.id \
             LEFT JOIN local_artist_external_identities ae \
               ON ae.local_artist_id = b.album_artist_id \
             WHERE t.availability = 'indexed' \
               AND COALESCE(ai.release_group_mbid, t.embedded_release_group_mbid, '') <> ''",
        )
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| read_failure("upgrades.cutoff_unmet", error))?;
        let mut albums: HashMap<String, UpgradeItem> = HashMap::new();
        for (rg_mbid, title, artist, year, artist_mbid, format, bitrate, depth) in rows {
            let tier = tier_of(&format, bitrate, depth);
            let album = albums
                .entry(rg_mbid.clone())
                .or_insert_with(|| UpgradeItem {
                    rg_mbid,
                    artist: artist.unwrap_or_default(),
                    title,
                    current_tier: tier,
                    track_count: 0,
                    year,
                    artist_mbid,
                });
            album.track_count += 1;
            if tier_rank(tier) < tier_rank(album.current_tier) {
                album.current_tier = tier;
            }
        }
        let mut items: Vec<UpgradeItem> = albums
            .into_values()
            .filter(|album| tier_rank(album.current_tier).is_some_and(|rank| rank < cutoff_rank))
            .collect();
        items.sort_by(|a, b| {
            tier_rank(a.current_tier)
                .cmp(&tier_rank(b.current_tier))
                .then_with(|| a.artist.to_lowercase().cmp(&b.artist.to_lowercase()))
                .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
                .then_with(|| a.rg_mbid.cmp(&b.rg_mbid))
        });
        Ok(items)
    }

    /// Whether the library's copy of an album already meets `cutoff`
    /// (its worst file). An album the library does not hold does not.
    pub async fn album_meets_cutoff(
        &self,
        rg_mbid: &str,
        cutoff: &str,
    ) -> Result<bool, StoreFailure> {
        let sql = format!(
            "SELECT t.file_format, t.bit_rate, t.bit_depth {}",
            crate::acquire::landing::library::OWNED_TRACKS_FROM
        );
        let rows: Vec<(String, Option<i64>, Option<i64>)> = sqlx::query_as(&sql)
            .bind(rg_mbid.to_ascii_lowercase())
            .fetch_all(self.db.pool())
            .await
            .map_err(|error| read_failure("upgrades.album_tier", error))?;
        let worst = rows
            .iter()
            .filter_map(|(format, bitrate, depth)| tier_rank(tier_of(format, *bitrate, *depth)))
            .min();
        Ok(meets(worst, cutoff))
    }

    /// Whether the library's best copy of a recording already meets
    /// `cutoff` (v2 per-recording floor). A recording it lacks does not.
    pub async fn recording_meets_cutoff(
        &self,
        recording_mbid: &str,
        cutoff: &str,
    ) -> Result<bool, StoreFailure> {
        let rows: Vec<(String, Option<i64>, Option<i64>)> = sqlx::query_as(
            "SELECT t.file_format, t.bit_rate, t.bit_depth FROM local_tracks t \
             JOIN local_albums b ON b.id = t.local_album_id AND b.retired_into_album_id IS NULL \
             LEFT JOIN local_track_external_identities ti ON ti.local_track_id = t.id \
             WHERE t.availability = 'indexed' \
               AND lower(COALESCE(ti.recording_mbid, t.embedded_recording_mbid, '')) = ?1",
        )
        .bind(recording_mbid.to_ascii_lowercase())
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| read_failure("upgrades.recording_tier", error))?;
        let best = rows
            .iter()
            .filter_map(|(format, bitrate, depth)| tier_rank(tier_of(format, *bitrate, *depth)))
            .max();
        Ok(meets(best, cutoff))
    }
}

fn meets(rank: Option<usize>, cutoff: &str) -> bool {
    match (rank, tier_rank(cutoff)) {
        (Some(rank), Some(cutoff)) => rank >= cutoff,
        _ => false,
    }
}

impl CutoffList for UpgradeWorklist {
    fn cutoff_unmet<'a>(
        &'a self,
        cutoff: &'a str,
    ) -> BoxFuture<'a, Result<Vec<UpgradeItem>, StoreFailure>> {
        Box::pin(self.list_cutoff_unmet(cutoff))
    }
}

/// A fixed worklist for tests.
#[cfg(any(test, feature = "test-support"))]
pub struct MemoryWorklist(pub Vec<UpgradeItem>);

#[cfg(any(test, feature = "test-support"))]
impl CutoffList for MemoryWorklist {
    fn cutoff_unmet<'a>(
        &'a self,
        _cutoff: &'a str,
    ) -> BoxFuture<'a, Result<Vec<UpgradeItem>, StoreFailure>> {
        let items = self.0.clone();
        Box::pin(async move { Ok(items) })
    }
}

/// Background-upgrade policy (v2 `download_policy` upgrade half).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradePolicy {
    /// Upgrades allowed at all.
    pub upgrade_allowed: bool,
    /// The tier an album must reach before it stops being upgraded.
    pub cutoff: String,
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
            cutoff: "lossless".to_owned(),
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
/// (v2 `run_background_upgrade_sweep`). Read from the accounts on every
/// call, so an admin created after boot owns the next sweep.
#[derive(Clone)]
pub struct AdminDirectory {
    db: AcquireDb,
}

impl AdminDirectory {
    /// Directory over the accounts table.
    pub fn over_users(db: AcquireDb) -> Self {
        Self { db }
    }

    /// Oldest admin id, or `None` when nobody can own the sweep.
    pub async fn oldest_admin(&self) -> Option<String> {
        match sqlx::query_scalar(
            "SELECT id FROM auth_users WHERE role = 'admin' ORDER BY created_at, id LIMIT 1",
        )
        .fetch_optional(self.db.pool())
        .await
        {
            Ok(admin) => admin,
            Err(error) => {
                tracing::warn!(%error, "admin directory unreadable; the upgrade sweep idles");
                None
            }
        }
    }
}
