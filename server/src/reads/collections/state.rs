//! Collections state: in-memory stores behind one handle.
//!
//! Each collection owns a small store; services take narrow references to the
//! stores they need (the pin service notably never sees the identity store).
//! Seed helpers exist for test fixtures only. Wiring swaps these stores for
//! SQLite-backed ports without touching handler signatures.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock, atomic::AtomicU64},
    time::{SystemTime, UNIX_EPOCH},
};

use super::error::CollectionsError;

/// Current time as epoch seconds. Falls back to zero when the clock is broken
/// rather than failing the request.
pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs())
        .unwrap_or(0)
}

/// Read a store or fail the request with a fixed 5xx. Lock poison details
/// never reach the wire.
pub fn read_store<'a, T>(
    store: &'a RwLock<T>,
    name: &str,
) -> Result<std::sync::RwLockReadGuard<'a, T>, CollectionsError> {
    store.read().map_err(|cause| {
        CollectionsError::internal(&format_args!("{name} store read failed: {cause}"))
    })
}

/// Write a store or fail the request with a fixed 5xx.
pub fn write_store<'a, T>(
    store: &'a RwLock<T>,
    name: &str,
) -> Result<std::sync::RwLockWriteGuard<'a, T>, CollectionsError> {
    store.write().map_err(|cause| {
        CollectionsError::internal(&format_args!("{name} store write failed: {cause}"))
    })
}

/// One stored playlist with its ordered tracks and optional cover bytes.
#[derive(Debug, Clone)]
pub struct StoredPlaylist {
    /// Playlist id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Owning user id.
    pub owner_id: String,
    /// Owner display name at creation.
    pub owner_name: String,
    /// Public playlists are visible to other users; private ones redact.
    pub is_public: bool,
    /// Tracks in position order.
    pub tracks: Vec<super::models::PlaylistTrack>,
    /// Uploaded cover bytes, if any.
    pub cover: Option<StoredCover>,
    /// Import provenance (`<source>:<id>`), when the playlist was imported.
    pub source_ref: Option<String>,
    /// Creation time, epoch seconds.
    pub created_at: u64,
    /// Last mutation time, epoch seconds.
    pub updated_at: u64,
}

/// Stored cover bytes with their content type.
#[derive(Debug, Clone)]
pub struct StoredCover {
    /// Mime type, from the validated allowlist.
    pub content_type: String,
    /// Raw image bytes.
    pub bytes: Vec<u8>,
}

/// Playlist store keyed by playlist id.
#[derive(Debug, Default)]
pub struct PlaylistStore {
    /// Rows by playlist id.
    pub playlists: RwLock<HashMap<String, StoredPlaylist>>,
}

/// One favorite row.
#[derive(Debug, Clone)]
pub struct FavoriteRow {
    /// Display name captured when favorited, if the caller sent one.
    pub name: Option<String>,
    /// When it was favorited, epoch seconds.
    pub favorited_at: u64,
}

/// Favorite store keyed by (user id, kind, item id).
#[derive(Debug, Default)]
pub struct FavoriteStore {
    /// Rows by (user id, kind, item id).
    pub favorites: RwLock<HashMap<(String, String, String), FavoriteRow>>,
}

/// Auto-download state for one follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoDownloadState {
    /// Auto-download off.
    Off,
    /// Requested, awaiting admin approval.
    Pending,
    /// Approved (or auto-approved for trusted/admin).
    Active,
}

impl AutoDownloadState {
    /// Wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Pending => "pending",
            Self::Active => "active",
        }
    }
}

/// One follow row: a user following an artist by MBID.
#[derive(Debug, Clone)]
pub struct FollowRow {
    /// Follower user id.
    pub user_id: String,
    /// Follower display name, for approval cards.
    pub user_name: String,
    /// Followed artist MBID.
    pub artist_mbid: String,
    /// Artist name as known when followed.
    pub artist_name: String,
    /// Whether auto-download is wanted.
    pub auto_download: bool,
    /// Approval state of that want.
    pub auto_download_state: AutoDownloadState,
    /// When the follow started, epoch seconds.
    pub followed_at: u64,
    /// When auto-download was requested, epoch seconds.
    pub requested_at: Option<u64>,
}

/// Follow store keyed by (user id, artist MBID).
#[derive(Debug, Default)]
pub struct FollowStore {
    /// Rows by (user id, artist MBID).
    pub follows: RwLock<HashMap<(String, String), FollowRow>>,
}

/// One new-release sighting (fixture-seeded; no provider feeds them yet).
#[derive(Debug, Clone)]
pub struct NewReleaseRow {
    /// Release-group MBID.
    pub release_group_mbid: String,
    /// Release title.
    pub title: String,
    /// Artist name.
    pub artist_name: String,
    /// Artist MBID.
    pub artist_mbid: String,
    /// Release-group primary type, if known.
    pub primary_type: Option<String>,
    /// First release date, if known.
    pub first_release_date: Option<String>,
    /// When this sighting was recorded, epoch seconds.
    pub detected_at: u64,
}

/// New-release catalog plus per-user seen watermarks.
#[derive(Debug, Default)]
pub struct NewReleaseStore {
    /// Sightings; fixtures append, reads filter.
    pub releases: RwLock<Vec<NewReleaseRow>>,
    /// Per-user seen watermark, epoch seconds.
    pub seen_at: RwLock<HashMap<String, u64>>,
}

/// One edition catalog entry (fixture-seeded; no provider feeds them yet).
#[derive(Debug, Clone)]
pub struct EditionCatalogRow {
    /// Known release MBIDs for the album.
    pub editions: Vec<String>,
    /// Default display pick when nothing is pinned.
    pub default_release_mbid: String,
}

/// Read-only edition catalog. Reads take `&self`; only fixtures mutate.
#[derive(Debug, Default)]
pub struct EditionCatalog {
    /// Rows by album id.
    pub albums: RwLock<HashMap<String, EditionCatalogRow>>,
}

/// One pin-hint row: a display preference, never catalog identity.
#[derive(Debug, Clone)]
pub struct PinHintRow {
    /// Pinned release MBID.
    pub pinned_release_mbid: String,
    /// Who pinned it.
    pub pinned_by: String,
    /// When it was pinned, epoch seconds.
    pub pinned_at: u64,
}

/// Pin-hint store keyed by album id. Display lane only.
#[derive(Debug, Default)]
pub struct PinHintStore {
    /// Hints by album id.
    pub pins: RwLock<HashMap<String, PinHintRow>>,
}

/// One accepted external identity. Owned by the library engine; this stub
/// exists so the pin-hint test can assert pins never write it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalIdentityRow {
    /// Album id.
    pub album_id: String,
    /// Accepted release MBID.
    pub release_mbid: String,
    /// How the identity was decided: automatic, manual, or legacy_import.
    pub decision_source: String,
}

/// Identity-table stub with a write counter. Every identity write bumps the
/// counter; the pin-hint test asserts it stays at zero across pin ops.
#[derive(Debug, Default)]
pub struct IdentityStore {
    /// Rows by album id.
    pub identities: RwLock<HashMap<String, ExternalIdentityRow>>,
    /// Count of identity writes. Fixture seeding bypasses this on purpose.
    pub writes: AtomicU64,
}

impl IdentityStore {
    /// Record an accepted identity. Only the library engine calls this;
    /// collections code must never reach it.
    pub fn save_identity(&self, row: ExternalIdentityRow) -> Result<(), CollectionsError> {
        write_store(&self.identities, "identity")?.insert(row.album_id.clone(), row);
        self.writes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Seed an identity row for tests. Bypasses the write counter so a test
    /// can assert zero product writes against a seeded table.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_identity(&self, row: ExternalIdentityRow) -> Result<(), CollectionsError> {
        write_store(&self.identities, "identity")?.insert(row.album_id.clone(), row);
        Ok(())
    }
}

/// One pending auto-download approval, plain data for the acquire bridge.
#[derive(Debug, Clone)]
pub struct PendingApproval {
    /// Requesting user id.
    pub user_id: String,
    /// Requesting display name.
    pub user_name: String,
    /// Artist MBID.
    pub artist_mbid: String,
    /// Artist name.
    pub artist_name: String,
    /// Epoch seconds when asked.
    pub requested_at: u64,
}

/// One pending bulk batch, plain data for the acquire bridge.
#[derive(Debug, Clone)]
pub struct PendingBatch {
    /// Batch id.
    pub batch_id: String,
    /// Requesting user id.
    pub user_id: String,
    /// Requesting display name.
    pub user_name: String,
    /// Covered `(artist MBID, artist name)` pairs.
    pub artists: Vec<(String, String)>,
    /// Epoch seconds when asked.
    pub requested_at: u64,
}

/// Pending approvals owned by `acquire::requests`. Wiring connects this so
/// the approval reads share one store with the approval mutations; tests
/// leave it empty and read the follow rows.
pub trait PendingApprovalsSource: Send + Sync {
    /// Pending approvals, oldest first.
    fn pending_approvals(
        &self,
    ) -> futures_util::future::BoxFuture<'_, Result<Vec<PendingApproval>, String>>;
    /// Pending batches, oldest first.
    fn pending_batches(
        &self,
    ) -> futures_util::future::BoxFuture<'_, Result<Vec<PendingBatch>, String>>;
}

/// Seed sink into the requests approval store. Wiring connects this so
/// follow toggles that land Pending also file the approval the admin
/// mutations decide; tests leave it empty.
pub trait ApprovalSeedSink: Send + Sync {
    /// File one pending approval.
    fn seed_approval<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        artist_name: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<(), String>>;
    /// Withdraw a pending ask (the user toggled back off).
    fn withdraw_approval<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<(), String>>;
}

/// All collections state. Handlers receive this via the `State` extractor and pass
/// narrow store references into services.
#[derive(Clone, Default)]
pub struct CollectionsState {
    /// Playlists and their covers.
    pub playlists: Arc<PlaylistStore>,
    /// Favorites.
    pub favorites: Arc<FavoriteStore>,
    /// Follows.
    pub follows: Arc<FollowStore>,
    /// New-release sightings and seen watermarks.
    pub new_releases: Arc<NewReleaseStore>,
    /// Read-only edition catalog for the pin display lane.
    pub edition_catalog: Arc<EditionCatalog>,
    /// Pin hints. Display lane only, never identity.
    pub pins: Arc<PinHintStore>,
    /// Identity-table stub. Pins must never write here.
    pub identities: Arc<IdentityStore>,
    /// Failure injection for the leak tests. While set, services fail.
    #[cfg(any(test, feature = "test-support"))]
    pub fail_stores: Arc<std::sync::atomic::AtomicBool>,
    /// Acquire-owned pending approvals, when wired. Reads prefer this over
    /// the follow rows so reads and mutations share one store.
    pub acquire_approvals: Option<Arc<dyn PendingApprovalsSource>>,
    /// Seed sink into the acquire approval store, when wired.
    pub approval_seeds: Option<Arc<dyn ApprovalSeedSink>>,
}

impl CollectionsState {
    /// Fresh empty state for tests and wiring.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fail the request when failure injection is armed.
    pub fn check_injection(&self) -> Result<(), CollectionsError> {
        #[cfg(any(test, feature = "test-support"))]
        if self.fail_stores.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(CollectionsError::internal(&"injected store failure"));
        }
        Ok(())
    }
}
