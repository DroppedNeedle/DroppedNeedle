//! State behind the acquisition flows.
//!
//! The follow-poll cursors live with the follows
//! ([`crate::follows::cursors`]). The request ledger and the wanted watches
//! are the requests module's durable stores, shared with the loops. Library presence, the
//! upgrade worklist and the admin directory are read from their owners'
//! tables (the catalog, the accounts) on every call.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use futures_util::future::BoxFuture;
use rusqlite::{Connection, OptionalExtension, Params, params_from_iter};

use super::sql;
use crate::db::AcquireDb;
use droppedneedle_kernel::db::statement::ListLen;
use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_kernel::runtime_config::sections::{DownloadPolicy, tier_rank};

/// A flows store failure, already logged by the caller with its context.
#[derive(Debug, thiserror::Error)]
#[error("flows store {operation} failed: {cause}")]
pub struct StoreFailure {
    /// Operation name.
    pub operation: &'static str,
    /// Cause text.
    pub cause: String,
}

pub(crate) fn lane_failure(operation: &'static str, error: DbError) -> StoreFailure {
    StoreFailure {
        operation,
        cause: error.to_string(),
    }
}

/// One pooled read, its failure (a row that does not decode included)
/// mapped as [`lane_failure`] maps a write's.
pub(crate) async fn read<R, F>(db: &Db, operation: &'static str, op: F) -> Result<R, StoreFailure>
where
    F: FnOnce(&Connection) -> rusqlite::Result<R> + Send + 'static,
    R: Send + 'static,
{
    db.read(operation, move |conn| op(conn).map_err(DbError::from))
        .await
        .map_err(|error| lane_failure(operation, error))
}

/// Every row one statement selects, mapped by `map`.
fn read_rows<T>(
    conn: &Connection,
    text: &str,
    values: impl Params,
    map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> rusqlite::Result<Vec<T>> {
    let mut statement = conn.prepare_cached(text)?;
    let rows = statement.query_map(values, map)?;
    rows.collect()
}

/// A track's quality facts: format, bitrate, bit depth.
fn quality_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(String, Option<i64>, Option<i64>)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
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

    /// Release groups the sweep leaves alone at `now` (unix seconds): an
    /// open held entry awaits a decision, or the last upgrade finished less
    /// than its back-off ago.
    fn resting(&self, now: i64) -> BoxFuture<'_, Result<HashSet<String>, StoreFailure>>;

    /// When the newest upgrade task was made (unix seconds), the sweep's
    /// last run across a restart.
    fn last_upgrade_at(&self) -> BoxFuture<'_, Result<Option<i64>, StoreFailure>>;
}

const DAY: i64 = 86_400;
/// Five doublings of a day pass the 30-day cap.
const BACKOFF_MAX_DOUBLINGS: u32 = 5;
const BACKOFF_CAP: i64 = 30 * DAY;

/// The wait after an album's newest finished upgrade: a day, doubled for
/// each failed or cancelled upgrade directly before it, at most 30 days.
/// `history` is the album's finished upgrades, newest first.
fn upgrade_backoff(history: &[&str]) -> i64 {
    let streak = history
        .iter()
        .take_while(|status| matches!(**status, "failed" | "cancelled"))
        .count();
    let doublings = u32::try_from(streak.saturating_sub(1)).unwrap_or(u32::MAX);
    (DAY << doublings.min(BACKOFF_MAX_DOUBLINGS)).min(BACKOFF_CAP)
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
    crate::landing::quality::tier_for(
        format,
        bitrate.and_then(|rate| u32::try_from(rate).ok()),
        depth.and_then(|depth| u8::try_from(depth).ok()),
    )
}

/// The upgrade worklist, read from the library catalog on every call:
/// an album is as good as its worst file.
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
        let rows: Vec<CatalogTrack> = read(self.db.db(), "upgrades.cutoff_unmet", |conn| {
            read_rows(conn, sql::IDENTIFIED_TRACKS.text(), [], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            })
        })
        .await?;
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
        let group = rg_mbid.to_ascii_lowercase();
        let rows = read(self.db.db(), "upgrades.album_tier", move |conn| {
            read_rows(
                conn,
                sql::GROUP_TRACK_QUALITY.text(),
                [group],
                quality_from_row,
            )
        })
        .await?;
        let worst = rows
            .iter()
            .filter_map(|(format, bitrate, depth)| tier_rank(tier_of(format, *bitrate, *depth)))
            .min();
        Ok(meets(worst, cutoff))
    }

    /// Whether the library's best copy of a recording already meets
    /// `cutoff`. A recording it lacks does not.
    pub async fn recording_meets_cutoff(
        &self,
        recording_mbid: &str,
        cutoff: &str,
    ) -> Result<bool, StoreFailure> {
        let recording = recording_mbid.to_ascii_lowercase();
        let rows = read(self.db.db(), "upgrades.recording_tier", move |conn| {
            read_rows(
                conn,
                sql::RECORDING_TRACK_QUALITY.text(),
                [recording],
                quality_from_row,
            )
        })
        .await?;
        let best = rows
            .iter()
            .filter_map(|(format, bitrate, depth)| tier_rank(tier_of(format, *bitrate, *depth)))
            .max();
        Ok(meets(best, cutoff))
    }

    /// How many of these library tracks sit below `cutoff`: the edition's
    /// owned tracks an upgrade would replace.
    pub async fn tracks_below_cutoff(
        &self,
        track_ids: &[String],
        cutoff: &str,
    ) -> Result<Vec<String>, StoreFailure> {
        let mut below = Vec::new();
        for chunk in track_ids.chunks(400) {
            let ids = chunk.to_vec();
            let rows: Vec<(String, String, Option<i64>, Option<i64>)> =
                read(self.db.db(), "upgrades.track_tiers", move |conn| {
                    let mut statement =
                        conn.prepare(&sql::track_quality_in(ListLen::new(ids.len())))?;
                    let rows = statement.query_map(params_from_iter(&ids), |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })?;
                    rows.collect()
                })
                .await?;
            below.extend(
                rows.into_iter()
                    .filter(|(_, format, bitrate, depth)| {
                        !meets(tier_rank(tier_of(format, *bitrate, *depth)), cutoff)
                    })
                    .map(|(id, ..)| id),
            );
        }
        Ok(below)
    }
}

fn meets(rank: Option<usize>, cutoff: &str) -> bool {
    match (rank, tier_rank(cutoff)) {
        (Some(rank), Some(cutoff)) => rank >= cutoff,
        _ => false,
    }
}

impl UpgradeWorklist {
    /// Release groups the upgrade sweep skips at `now`: those with an open
    /// held entry, and those whose newest finished upgrade task is younger
    /// than its back-off.
    pub async fn list_resting(&self, now: i64) -> Result<HashSet<String>, StoreFailure> {
        let mut resting: HashSet<String> = read(self.db.db(), "upgrades.held_groups", |conn| {
            read_rows(conn, sql::HELD_GROUPS.text(), [], |row| row.get(0))
        })
        .await?
        .into_iter()
        .collect();
        let rows: Vec<(String, String, i64)> = read(self.db.db(), "upgrades.history", |conn| {
            read_rows(conn, sql::UPGRADE_HISTORY.text(), [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
        })
        .await?;
        for album in rows.chunk_by(|a, b| a.0 == b.0) {
            let Some((rg_mbid, _, finished_at)) = album.first() else {
                continue;
            };
            let history: Vec<&str> = album.iter().map(|row| row.1.as_str()).collect();
            if now - finished_at < upgrade_backoff(&history) {
                resting.insert(rg_mbid.clone());
            }
        }
        Ok(resting)
    }

    /// When the newest upgrade task was made, in unix seconds.
    pub async fn newest_upgrade_at(&self) -> Result<Option<i64>, StoreFailure> {
        read(self.db.db(), "upgrades.last_run", |conn| {
            conn.query_row(sql::NEWEST_UPGRADE.text(), [], |row| row.get(0))
        })
        .await
    }
}

impl CutoffList for UpgradeWorklist {
    fn cutoff_unmet<'a>(
        &'a self,
        cutoff: &'a str,
    ) -> BoxFuture<'a, Result<Vec<UpgradeItem>, StoreFailure>> {
        Box::pin(self.list_cutoff_unmet(cutoff))
    }

    fn resting(&self, now: i64) -> BoxFuture<'_, Result<HashSet<String>, StoreFailure>> {
        Box::pin(self.list_resting(now))
    }

    fn last_upgrade_at(&self) -> BoxFuture<'_, Result<Option<i64>, StoreFailure>> {
        Box::pin(self.newest_upgrade_at())
    }
}

/// Background-upgrade policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradePolicy {
    /// Upgrades allowed at all.
    pub upgrade_allowed: bool,
    /// The tier an album must reach before it stops being upgraded.
    pub cutoff: String,
    /// The background scan enabled.
    pub scan_enabled: bool,
    /// Grabs per sweep, at most.
    pub max_per_run: usize,
    /// Sweep cadence in hours.
    pub interval_hours: u64,
}

impl UpgradePolicy {
    /// The upgrade half of the download-policy section.
    pub fn from_policy(policy: &DownloadPolicy) -> Self {
        use crate::settings::{clamp_u64, clamp_usize};
        Self {
            upgrade_allowed: policy.upgrade_allowed,
            cutoff: policy.quality_cutoff.clone(),
            scan_enabled: policy.background_upgrade_scan_enabled,
            max_per_run: clamp_usize(policy.background_upgrade_max_per_run).max(1),
            interval_hours: clamp_u64(policy.background_upgrade_scan_interval_hours).max(1),
        }
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
    catalog: Option<Db>,
}

impl LibraryPresence {
    /// Empty presence.
    pub fn new() -> Self {
        Self::default()
    }

    /// Presence read from the library catalog over the reader pool.
    pub fn over_catalog(catalog: Db) -> Self {
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
        match crate::edition::library_holds(catalog, mbid.trim()).await {
            Ok(owned) => owned,
            Err(error) => {
                tracing::warn!(mbid, %error, "library presence unreadable");
                false
            }
        }
    }
}

/// Admin directory: upgrades are a curator action owned by the oldest
/// admin. Read from the accounts on every call, so an admin created after boot
/// owns the next sweep.
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
        let admin = read(self.db.db(), "upgrades.oldest_admin", |conn| {
            conn.query_row(sql::OLDEST_ADMIN.text(), [], |row| row.get(0))
                .optional()
        })
        .await;
        match admin {
            Ok(admin) => admin,
            Err(error) => {
                tracing::warn!(%error, "admin directory unreadable; the upgrade sweep idles");
                None
            }
        }
    }
}
