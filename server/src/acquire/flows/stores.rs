//! Memory state behind the acquisition flows.
//!
//! Wanted watches, request rows, follow cursors, the upgrade worklist, and
//! quarantine entries live here on mutex-guarded maps. Durable rows are a
//! later persistence tier (the stage-6 precedent); the registry already
//! persists job liveness, and every state transition emits a durable tick
//! (see [`TickSink`](super::seams::TickSink)), so no outcome is silent.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// One wanted watch: a user waiting on an unavailable album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    /// Release-group MBID under watch.
    pub rg_mbid: String,
    /// Watching user id.
    pub user_id: String,
    /// Artist name for searches.
    pub artist: String,
    /// Album title for searches.
    pub title: String,
    /// First release date (`YYYY-MM-DD`, possibly partial), when known.
    pub first_release_date: Option<String>,
    /// Consecutive quiet checks; long streaks back off to 28 days.
    pub quiet_streak: u32,
    /// Unix seconds when the next check is due.
    pub next_check_at: i64,
}

/// Wanted-watch registry.
#[derive(Debug, Default)]
pub struct WantedStore {
    watches: Mutex<HashMap<String, Watch>>,
}

impl WantedStore {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enrol a watch; answers false when one already covers the MBID.
    pub fn enrol(&self, watch: Watch) -> bool {
        self.watches
            .lock()
            .map(|mut watches| {
                if watches.contains_key(&watch.rg_mbid) {
                    return false;
                }
                watches.insert(watch.rg_mbid.clone(), watch);
                true
            })
            .unwrap_or(false)
    }

    /// Watches due at `now`, oldest first, capped at `limit`.
    pub fn list_due(&self, now: i64, limit: usize) -> Vec<Watch> {
        self.watches
            .lock()
            .map(|watches| {
                let mut due: Vec<Watch> = watches
                    .values()
                    .filter(|watch| watch.next_check_at <= now)
                    .cloned()
                    .collect();
                due.sort_by(|a, b| {
                    a.next_check_at
                        .cmp(&b.next_check_at)
                        .then_with(|| a.rg_mbid.cmp(&b.rg_mbid))
                });
                due.truncate(limit);
                due
            })
            .unwrap_or_default()
    }

    /// Replace one watch's check outcome (streak + next due).
    pub fn record_check(&self, rg_mbid: &str, quiet_streak: u32, next_check_at: i64) {
        if let Ok(mut watches) = self.watches.lock()
            && let Some(watch) = watches.get_mut(rg_mbid)
        {
            watch.quiet_streak = quiet_streak;
            watch.next_check_at = next_check_at;
        }
    }

    /// Drop a watch (Stop, or satisfied).
    pub fn remove(&self, rg_mbid: &str) {
        if let Ok(mut watches) = self.watches.lock() {
            watches.remove(rg_mbid);
        }
    }

    /// One watch, if present.
    pub fn get(&self, rg_mbid: &str) -> Option<Watch> {
        self.watches
            .lock()
            .ok()
            .and_then(|watches| watches.get(rg_mbid).cloned())
    }

    /// Watch count, for briefs.
    pub fn len(&self) -> usize {
        self.watches
            .lock()
            .map(|watches| watches.len())
            .unwrap_or(0)
    }

    /// True when no watches are enrolled.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One request row as the status sync sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRow {
    /// Album release-group MBID or track recording MBID.
    pub mbid: String,
    /// `album` or `track`.
    pub kind: String,
    /// Requesting user id.
    pub user_id: String,
    /// Artist name for searches and task rows.
    pub artist: String,
    /// Album or track title for searches and task rows.
    pub title: String,
    /// Current request status.
    pub status: String,
    /// Linked download task id, when dispatched.
    pub task_id: Option<String>,
    /// Optimistic generation for status writes.
    pub generation: u64,
    /// Completion instant (unix seconds), once terminal.
    pub completed_at: Option<i64>,
}

/// Request ledger: active rows plus terminal history.
#[derive(Debug, Default)]
pub struct RequestLedger {
    rows: Mutex<HashMap<String, RequestRow>>,
}

impl RequestLedger {
    /// Empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace one row, keyed by MBID.
    pub fn upsert(&self, row: RequestRow) {
        if let Ok(mut rows) = self.rows.lock() {
            rows.insert(row.mbid.clone(), row);
        }
    }

    /// One row, if present.
    pub fn get(&self, mbid: &str) -> Option<RequestRow> {
        self.rows
            .lock()
            .ok()
            .and_then(|rows| rows.get(mbid).cloned())
    }

    /// Rows still needing reconciliation: anything outside the terminal set
    /// (`imported`, `incomplete`, `failed`, `cancelled`, `ignored`).
    pub fn active(&self) -> Vec<RequestRow> {
        self.rows
            .lock()
            .map(|rows| {
                rows.values()
                    .filter(|row| !is_terminal(&row.status))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Rows in one status, for enrolment scans.
    pub fn with_status(&self, status: &str) -> Vec<RequestRow> {
        self.rows
            .lock()
            .map(|rows| {
                rows.values()
                    .filter(|row| row.status == status)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Move one row to a new status, stamping `completed_at` for terminal
    /// states and bumping the generation. Answers false when the row or the
    /// expected generation is gone (lost race, skip quietly).
    pub fn update_status(
        &self,
        mbid: &str,
        status: &str,
        completed_at: Option<i64>,
        expected_generation: Option<u64>,
    ) -> bool {
        self.rows
            .lock()
            .map(|mut rows| {
                let Some(row) = rows.get_mut(mbid) else {
                    return false;
                };
                if expected_generation.is_some_and(|generation| generation != row.generation) {
                    return false;
                }
                row.status = status.to_owned();
                row.completed_at = completed_at;
                row.generation += 1;
                true
            })
            .unwrap_or(false)
    }

    /// Link a dispatch task id to a row.
    pub fn link_task(&self, mbid: &str, task_id: &str) {
        if let Ok(mut rows) = self.rows.lock()
            && let Some(row) = rows.get_mut(mbid)
        {
            row.task_id = Some(task_id.to_owned());
        }
    }
}

/// Terminal request statuses: nothing left to reconcile.
pub fn is_terminal(status: &str) -> bool {
    matches!(
        status,
        "imported" | "incomplete" | "failed" | "cancelled" | "ignored"
    )
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRelease {
    /// Release-group MBID.
    pub rg_mbid: String,
    /// Release title.
    pub title: String,
    /// First release date (`YYYY-MM-DD`).
    pub date: String,
}

/// Follow-poll cursor store.
#[derive(Debug, Default)]
pub struct FollowStore {
    cursors: Mutex<HashMap<String, FollowCursor>>,
}

impl FollowStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace one cursor.
    pub fn upsert(&self, cursor: FollowCursor) {
        if let Ok(mut cursors) = self.cursors.lock() {
            cursors.insert(cursor.artist_mbid.clone(), cursor);
        }
    }

    /// One cursor, if present.
    pub fn get(&self, artist_mbid: &str) -> Option<FollowCursor> {
        self.cursors
            .lock()
            .ok()
            .and_then(|cursors| cursors.get(artist_mbid).cloned())
    }

    /// Artists due at `now`, oldest first, capped at `limit`.
    pub fn list_due(&self, now: i64, limit: usize) -> Vec<FollowCursor> {
        self.cursors
            .lock()
            .map(|cursors| {
                let mut due: Vec<FollowCursor> = cursors
                    .values()
                    .filter(|cursor| cursor.next_poll_at <= now)
                    .cloned()
                    .collect();
                due.sort_by(|a, b| {
                    a.next_poll_at
                        .cmp(&b.next_poll_at)
                        .then_with(|| a.artist_mbid.cmp(&b.artist_mbid))
                });
                due.truncate(limit);
                due
            })
            .unwrap_or_default()
    }

    /// Replace one cursor after a poll.
    pub fn record_poll(&self, cursor: &FollowCursor) {
        if let Ok(mut cursors) = self.cursors.lock() {
            cursors.insert(cursor.artist_mbid.clone(), cursor.clone());
        }
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

/// Background-upgrade worklist: cutoff-unmet albums, oldest first.
#[derive(Debug, Default)]
pub struct UpgradeWorklist {
    items: Mutex<Vec<UpgradeItem>>,
}

impl UpgradeWorklist {
    /// Empty worklist.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the worklist contents.
    pub fn set(&self, items: Vec<UpgradeItem>) {
        if let Ok(mut current) = self.items.lock() {
            *current = items;
        }
    }

    /// Cutoff-unmet albums, oldest first.
    pub fn list_cutoff_unmet(&self) -> Vec<UpgradeItem> {
        self.items
            .lock()
            .map(|items| items.clone())
            .unwrap_or_default()
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

/// Quarantine registry (v2 `download_store` quarantine half).
#[derive(Debug, Default)]
pub struct QuarantineStore {
    entries: Mutex<HashMap<String, QuarantineEntry>>,
}

impl QuarantineStore {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one quarantine entry.
    pub fn quarantine(&self, entry: QuarantineEntry) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(entry.key.clone(), entry);
        }
    }

    /// True when the key is quarantined.
    pub fn is_quarantined(&self, key: &str) -> bool {
        self.entries
            .lock()
            .map(|entries| entries.contains_key(key))
            .unwrap_or(false)
    }

    /// One entry, if present.
    pub fn get(&self, key: &str) -> Option<QuarantineEntry> {
        self.entries
            .lock()
            .ok()
            .and_then(|entries| entries.get(key).cloned())
    }

    /// Drop one entry (manual resolve).
    pub fn clear(&self, key: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(key);
        }
    }

    /// Drop every entry scoped to one album (retry reconsiders quarantined
    /// sources for album downloads only, v2 orchestrator rule).
    pub fn clear_for_album(&self, album_key: &str) -> usize {
        self.entries
            .lock()
            .map(|mut entries| {
                let keys: Vec<String> = entries
                    .values()
                    .filter(|entry| entry.album_key.as_deref() == Some(album_key))
                    .map(|entry| entry.key.clone())
                    .collect();
                let cleared = keys.len();
                for key in keys {
                    entries.remove(&key);
                }
                cleared
            })
            .unwrap_or(0)
    }

    /// Every entry, for briefs.
    pub fn list(&self) -> Vec<QuarantineEntry> {
        self.entries
            .lock()
            .map(|entries| entries.values().cloned().collect())
            .unwrap_or_default()
    }
}

/// Library presence: the MBIDs the library already holds.
#[derive(Debug, Default)]
pub struct LibraryPresence {
    mbids: Mutex<HashSet<String>>,
}

impl LibraryPresence {
    /// Empty presence.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark MBIDs as owned.
    pub fn add(&self, mbids: &[&str]) {
        if let Ok(mut owned) = self.mbids.lock() {
            for mbid in mbids {
                owned.insert((*mbid).to_owned());
            }
        }
    }

    /// True when the library holds the MBID.
    pub fn contains(&self, mbid: &str) -> bool {
        self.mbids
            .lock()
            .map(|owned| owned.contains(mbid))
            .unwrap_or(false)
    }

    /// Every owned MBID, for briefs.
    pub fn all(&self) -> HashSet<String> {
        self.mbids
            .lock()
            .map(|owned| owned.clone())
            .unwrap_or_default()
    }
}

/// Admin directory: upgrades are a curator action owned by the oldest admin
/// (v2 `run_background_upgrade_sweep`, D18).
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
