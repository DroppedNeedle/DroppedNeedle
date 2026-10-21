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

use crate::db::AcquireDb;
use droppedneedle_kernel::db::{DbError, map_sqlx_busy};
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

pub(crate) fn read_failure(operation: &'static str, error: sqlx::Error) -> StoreFailure {
    lane_failure(operation, map_sqlx_busy(operation, error))
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
    crate::landing::quality::tier_for(
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
            crate::landing::library::OWNED_TRACKS_FROM
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

    /// How many of these library tracks sit below `cutoff` (v2 counted the
    /// edition's owned tracks it would upgrade).
    pub async fn tracks_below_cutoff(
        &self,
        track_ids: &[String],
        cutoff: &str,
    ) -> Result<Vec<String>, StoreFailure> {
        let mut below = Vec::new();
        for chunk in track_ids.chunks(400) {
            let marks = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT id, file_format, bit_rate, bit_depth FROM local_tracks \
                 WHERE id IN ({marks})"
            );
            let mut query = sqlx::query_as::<_, (String, String, Option<i64>, Option<i64>)>(&sql);
            for id in chunk {
                query = query.bind(id);
            }
            let rows = query
                .fetch_all(self.db.pool())
                .await
                .map_err(|error| read_failure("upgrades.track_tiers", error))?;
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

impl CutoffList for UpgradeWorklist {
    fn cutoff_unmet<'a>(
        &'a self,
        cutoff: &'a str,
    ) -> BoxFuture<'a, Result<Vec<UpgradeItem>, StoreFailure>> {
        Box::pin(self.list_cutoff_unmet(cutoff))
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
            crate::landing::library::OWNED_TRACKS_FROM
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
