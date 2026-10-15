//! Library management: sealed previews, apply, undo, baseline restore.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use super::adapters::FsSpaceProbe;
use super::clock::today_day;
use super::publish::journal::JournalStore;
use super::publish::planner::{
    Capability, CollisionGate, DiskPreflight, FileFingerprint, PlanBundle, PlanItem, PlanKind,
    ReleaseIdentity, SealRecheck, SealedPreview,
};
use super::publish::publisher::{PublishOutcome, Publisher, SqliteCatalog};
use super::publish::recovery::LeftoverReport;
use super::publish::snapshots::{BaselineStore, BlobStore, SnapshotStore, sha256_hex};
use super::publish::tags_seam::TagDocument;
use super::publish::{PlacementMode, PublishError, Sandbox};
use super::scan::fs::WriteGuard;
use super::scan::models::EffectivePolicy;
use super::scan::roots::RootRegistry;
use super::service::ServiceError;
use super::wiring::{LibrarySetup, RootDirs};

/// Preview seals live until the next unix day.
const PREVIEW_TTL_DAYS: i64 = 1;

/// Staged-bytes headroom over the source size for disk preflight.
const STAGED_HEADROOM_BYTES: u64 = 65_536;

/// Profile/naming/settings revisions while those settings have no store.
/// All pinned at 1 so seal rechecks compare against constants.
const PINNED_REVISION: u64 = 1;

/// Override revision while overrides have no store.
const PINNED_OVERRIDE: u64 = 0;

/// Policy revision as the seal's u64: the registry fingerprint is
/// 16 hex chars by construction.
pub(super) fn policy_revision_u64(registry: &RootRegistry) -> u64 {
    u64::from_str_radix(registry.policy_revision(), 16).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Publish cell: one publisher over the live sandbox, reopened on change.
// ---------------------------------------------------------------------------

/// One sealed preview awaiting apply.
pub struct PreviewEntry {
    sealed: SealedPreview,
    docs: BTreeMap<String, TagDocument>,
    plain_retag: bool,
}

/// Open publisher plus the sandbox it runs under.
pub(crate) struct OpenPublish {
    pub(crate) sandbox: Sandbox,
    pub(crate) publisher: Publisher<SqliteCatalog, FsSpaceProbe>,
    root_ids: Vec<String>,
}

/// Publish state. Empty until the first usable root is configured;
/// reopened (with reconciliation) whenever the usable root set
/// changes. One mutex serializes publishes against reopens.
pub struct PublishCell {
    db_path: PathBuf,
    pub(crate) cell: Option<OpenPublish>,
    placement: RootPlacement,
}

/// How files can be placed under each root of the open publisher session,
/// as its probes found. Kept beside the cell so a read never waits on a
/// publish holding it, and never probes: only a publish or an
/// administrator's "Check again" does.
#[derive(Clone, Default)]
pub struct RootPlacement {
    open: std::sync::Arc<std::sync::Mutex<Option<Sandbox>>>,
}

impl RootPlacement {
    /// The mode of one root as last probed. `Unknown` for a root the open
    /// session does not hold, and while no session is open.
    pub fn mode(&self, root_id: &str) -> PlacementMode {
        self.open
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map_or(PlacementMode::Unknown, |sandbox| {
                sandbox.placement_mode(root_id)
            })
    }

    /// Fix a root's mode without probing. Tests only.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_mode(&self, root_id: &str, mode: PlacementMode) {
        let open = self
            .open
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(sandbox) = open.as_ref() {
            sandbox.set_placement_mode(root_id, mode);
        }
    }

    fn follow(&self, session: Option<Sandbox>) {
        *self
            .open
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = session;
    }
}

/// What a placement check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementCheck {
    /// The root's mode now.
    pub mode: PlacementMode,
    /// False when the probe could not tell (a full or read-only disk, a
    /// share that did not answer). `mode` is then what was known before.
    pub answered: bool,
}

/// Unsettled publish bundles as the journal records them: anything not
/// cleaned, compensated, or flagged for an administrator. Reads its own
/// connection, so a status read never waits on a publish holding the cell.
#[derive(Clone)]
pub struct HeldBundles {
    db_path: PathBuf,
}

impl HeldBundles {
    pub(crate) fn new(db_path: &Path) -> Self {
        Self {
            db_path: db_path.to_owned(),
        }
    }

    /// True when the publisher committed `bundle_id`: its files changed
    /// even if whoever asked never heard back. Blocking.
    pub fn committed(&self, bundle_id: &str) -> Result<bool, PublishError> {
        use super::publish::JournalState;
        use super::publish::publisher::Catalog as _;
        let conn = droppedneedle_kernel::db::open_connection(&self.db_path)
            .map_err(|error| PublishError::Store(error.to_string()))?;
        let journals = JournalStore::new(&conn).bundle(bundle_id)?;
        if journals.iter().any(|journal| {
            matches!(
                journal.state,
                JournalState::Committed | JournalState::Cleaned | JournalState::CleanupPending
            )
        }) {
            return Ok(true);
        }
        // A journal flagged for an administrator after the commit: the
        // catalog already points at the bytes this bundle published.
        for journal in journals
            .iter()
            .filter(|journal| journal.state == JournalState::NeedsAttention)
        {
            let Some(track_id) = journal.track_id.as_deref() else {
                continue;
            };
            if let Some((root, rel, published, _)) = SqliteCatalog.locate(&conn, track_id)?
                && root == journal.dest_root
                && rel == journal.dest_rel
                && published == journal.staged_sha256
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// True while `bundle_id` has journals neither committed nor settled:
    /// recovery has not decided yet whether it finishes or unwinds.
    /// Blocking.
    pub fn unsettled(&self, bundle_id: &str) -> Result<bool, PublishError> {
        use super::publish::JournalState;
        let conn = droppedneedle_kernel::db::open_connection(&self.db_path)
            .map_err(|error| PublishError::Store(error.to_string()))?;
        let journals = JournalStore::new(&conn).bundle(bundle_id)?;
        Ok(journals.iter().any(|journal| {
            !journal.state.is_terminal()
                && !matches!(
                    journal.state,
                    JournalState::Committed | JournalState::CleanupPending
                )
        }))
    }

    /// Bundle ids with unsettled journals. Blocking.
    pub fn list(&self) -> Result<Vec<String>, PublishError> {
        let conn = droppedneedle_kernel::db::open_connection(&self.db_path)
            .map_err(|error| PublishError::Store(error.to_string()))?;
        JournalStore::new(&conn).active_bundles()
    }
}

/// Hidden files under the library roots that no journal row names, as the
/// last pass found them. Nothing here removes or adopts one.
#[derive(Clone)]
pub struct LeftoverFiles {
    db_path: PathBuf,
    report: std::sync::Arc<std::sync::Mutex<Option<LeftoverReport>>>,
}

impl LeftoverFiles {
    pub(crate) fn new(db_path: &Path) -> Self {
        Self {
            db_path: db_path.to_owned(),
            report: Default::default(),
        }
    }

    /// Walk the usable roots once and keep what was found. `None`, with
    /// the earlier report kept, when `cancelled` turned true. Blocking.
    pub fn refresh(
        &self,
        registry: &RootRegistry,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<LeftoverReport>, PublishError> {
        let conn = droppedneedle_kernel::db::open_connection(&self.db_path)
            .map_err(|error| PublishError::Store(error.to_string()))?;
        let found = super::publish::recovery::find_leftovers(
            &conn,
            &PublishCell::root_dirs(registry),
            std::time::SystemTime::now(),
            cancelled,
        )?;
        if let Some(report) = &found {
            *self
                .report
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(report.clone());
        }
        Ok(found)
    }

    /// What the last finished pass found. `None` before the first one.
    pub fn latest(&self) -> Option<LeftoverReport> {
        self.report
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl PublishCell {
    /// An unopened cell over the application database at `db_path`.
    pub(crate) fn new(db_path: &Path) -> Self {
        Self {
            db_path: db_path.to_owned(),
            cell: None,
            placement: RootPlacement::default(),
        }
    }

    /// The placement modes of whatever session this cell has open.
    pub(crate) fn placement(&self) -> RootPlacement {
        self.placement.clone()
    }

    /// Probe one root of the open session now. `None` for a root the
    /// session does not hold. The caller holds the cell, so no publish is
    /// placing files under the root meanwhile.
    fn recheck_placement(&self, root_id: &str) -> Result<Option<PlacementCheck>, PublishError> {
        let Some(open) = self.cell.as_ref() else {
            return Ok(None);
        };
        if !open.root_ids.iter().any(|id| id == root_id) {
            return Ok(None);
        }
        let probed = open.sandbox.recheck_placement(root_id)?;
        Ok(Some(PlacementCheck {
            mode: open.sandbox.placement_mode(root_id),
            answered: probed != PlacementMode::Unknown,
        }))
    }

    /// True while the journal holds an unsettled bundle (cleanup still
    /// pending, a deferred recovery, a compensation that did not finish).
    /// The cell lock keeps any publish out, so none of them is in flight.
    pub(crate) fn has_held(&self) -> bool {
        let Some(open) = self.cell.as_ref() else {
            return false;
        };
        match JournalStore::new(open.publisher.connection()).active_bundles() {
            Ok(bundles) => !bundles.is_empty(),
            Err(error) => {
                tracing::warn!(%error, "publish journal unreadable; no retry this tick");
                false
            }
        }
    }

    /// Reconcile again under the open sandbox: the maintenance tick's
    /// retry for held bundles. No-op while no usable root exists.
    pub(crate) fn retry_held(
        &mut self,
    ) -> Result<Vec<super::publish::recovery::BundleRecovery>, PublishError> {
        let Some(open) = self.cell.as_ref() else {
            return Ok(Vec::new());
        };
        let mut conn = super::publish::recovery::open_recovery_connection(&self.db_path)?;
        super::publish::recovery::reconcile(&mut conn, &open.sandbox, &SqliteCatalog)
    }

    fn root_dirs(registry: &RootRegistry) -> Vec<super::publish::paths::Root> {
        registry
            .roots()
            .iter()
            .filter(|root| root.policy != EffectivePolicy::Excluded)
            .map(|root| super::publish::paths::Root {
                id: root.id.clone(),
                dir: root.path.clone(),
            })
            .collect()
    }

    /// Reconcile and (re)open when the usable root set changed.
    /// Returns the bundle recoveries from the reconcile, if any.
    pub(crate) fn refresh(
        &mut self,
        registry: &RootRegistry,
        space_roots: &RootDirs,
    ) -> Result<Vec<super::publish::recovery::BundleRecovery>, PublishError> {
        let roots = Self::root_dirs(registry);
        let ids: Vec<String> = roots.iter().map(|root| root.id.clone()).collect();
        if roots.is_empty() {
            self.cell = None;
            self.placement.follow(None);
            return Ok(Vec::new());
        }
        if let Some(open) = self.cell.as_ref()
            && open.root_ids == ids
        {
            return Ok(Vec::new());
        }
        let sandbox = Sandbox::new(roots)?;
        // Reconcile through a short-lived connection first: the
        // publisher owns its connection privately, so recovery runs
        // before it opens. The journal lives in the application
        // database, so root order never decides where it is.
        let recoveries = {
            let mut conn = super::publish::recovery::open_recovery_connection(&self.db_path)?;
            super::publish::recovery::reconcile(&mut conn, &sandbox, &SqliteCatalog)?
        };
        let publisher = Publisher::open(
            sandbox.clone(),
            &self.db_path,
            SqliteCatalog,
            FsSpaceProbe::new(space_roots.clone()),
            today_day,
        )?;
        self.placement.follow(Some(sandbox.clone()));
        self.cell = Some(OpenPublish {
            sandbox,
            publisher,
            root_ids: ids,
        });
        Ok(recoveries)
    }

    pub(crate) fn open(&mut self) -> Result<&mut OpenPublish, PublishError> {
        self.cell
            .as_mut()
            .ok_or_else(|| PublishError::Journal("no usable library root is configured".into()))
    }

    /// Root ids a refresh would open under.
    pub(crate) fn pending_ids(registry: &RootRegistry) -> Vec<String> {
        Self::root_dirs(registry)
            .iter()
            .map(|root| root.id.clone())
            .collect()
    }

    /// True when a refresh would reopen (and reconcile) under
    /// `registry`: exactly the case where reconcile can write.
    pub(crate) fn needs_refresh(&self, registry: &RootRegistry) -> bool {
        let ids = Self::pending_ids(registry);
        if ids.is_empty() {
            return false;
        }
        self.cell.as_ref().map(|open| &open.root_ids) != Some(&ids)
    }
}

/// Revisions a publish is sealed with: the job's pins, which the live
/// side must repeat for the seal to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SealPins {
    pub profile: u64,
    pub naming: u64,
    pub settings: u64,
}

impl SealPins {
    /// The per-file preview flow, which has no stored settings to pin.
    fn constant() -> Self {
        Self {
            profile: PINNED_REVISION,
            naming: PINNED_REVISION,
            settings: PINNED_REVISION,
        }
    }
}

/// Why undo leaves one file of a bundle alone.
#[derive(Debug, Clone)]
pub(crate) enum UndoSkip {
    Plan(super::publish::undo::UndoBlock),
    /// The file is no longer in the library, unreadable, or unmatched.
    Gone,
}

/// One file undo would restore, with its tags as they are now.
#[derive(Debug, Clone)]
pub(crate) struct UndoFile {
    pub plan: PlanItem,
    pub current: BTreeMap<String, Vec<String>>,
    /// Sidecars that changed or whose old path is taken: they stay put.
    pub sidecars_left: Vec<String>,
}

/// What undoing one bundle would do.
#[derive(Debug, Clone, Default)]
pub(crate) struct UndoFiles {
    pub eligible: Vec<UndoFile>,
    pub blocked: Vec<(String, UndoSkip)>,
}

/// Why a baseline restore leaves one file alone.
#[derive(Debug, Clone)]
pub(crate) enum BaselineSkip {
    Plan(super::publish::undo::BaselineBlock),
    /// The file is no longer in the library, unreadable, or unmatched.
    Gone,
}

/// What restoring files to their originals would do.
#[derive(Debug, Clone, Default)]
pub(crate) struct BaselineFiles {
    pub eligible: Vec<UndoFile>,
    pub blocked: Vec<(String, BaselineSkip)>,
}

/// One file inside a management preview request (handler-mapped).
pub struct PreviewItemInput {
    /// Source root id.
    pub root_id: String,
    /// Source path relative to the root.
    pub rel_path: String,
    /// Destination rel path (organize only).
    pub dest_rel: Option<String>,
    /// Managed-field updates.
    pub managed_updates: BTreeMap<String, Vec<String>>,
}

/// Whether a preview request is a plain retag: the album's edition tags
/// written in place, with no destination and no tag values from the
/// caller. Anything else moves a file or writes what the caller chose.
pub fn plain_retag(kind: PlanKind, items: &[PreviewItemInput]) -> bool {
    kind == PlanKind::SamePath
        && items
            .iter()
            .all(|item| item.dest_rel.is_none() && item.managed_updates.is_empty())
}

/// One planned file inside a sealed preview.
pub struct PreviewFile {
    /// Stable local track id.
    pub track_id: String,
    /// Source `root/rel`.
    pub source: String,
    /// Destination `root/rel`.
    pub dest: String,
    /// `same_path` or `move`.
    pub kind: String,
    /// The tags a retag changes, in display order; empty for a move.
    pub changes: Vec<TagChange>,
}

/// One tag a retag would change. Values are text; several values are
/// joined with "; ". `None` means the tag is absent.
pub struct TagChange {
    /// Tag field name, or `musicbrainz_ids` for the ids summarised.
    pub field: String,
    /// Value the file holds now.
    pub from: Option<String>,
    /// Value the write leaves.
    pub to: Option<String>,
}

/// The fields a retag preview names one by one, in display order.
const SHOWN_CHANGES: [super::tags::TagField; 10] = {
    use super::tags::TagField;
    [
        TagField::Album,
        TagField::AlbumArtist,
        TagField::Title,
        TagField::Artist,
        TagField::TrackNumber,
        TagField::DiscNumber,
        TagField::Date,
        TagField::Label,
        TagField::CatalogNumber,
        TagField::Barcode,
    ]
};

/// The shown fields `updates` changes against `before`, then one
/// `musicbrainz_ids` entry counting the MusicBrainz id fields that change
/// and stay, when any changes. Fields the write leaves alone are skipped.
fn tag_changes(
    before: &BTreeMap<String, Vec<String>>,
    updates: &BTreeMap<String, Vec<String>>,
) -> Vec<TagChange> {
    let text = |values: Option<&Vec<String>>| {
        values
            .filter(|values| !values.is_empty())
            .map(|values| values.join("; "))
    };
    let differs = |name: &str, to: &Vec<String>| {
        before.get(name).map(Vec::as_slice).unwrap_or_default() != to.as_slice()
    };
    let mut changes: Vec<TagChange> = SHOWN_CHANGES
        .iter()
        .filter_map(|field| {
            let name = field.name();
            let to = updates.get(name)?;
            differs(name, to).then(|| TagChange {
                field: name.to_owned(),
                from: text(before.get(name)),
                to: text(Some(to)),
            })
        })
        .collect();
    let (changed, kept) = updates
        .iter()
        .filter(|(name, _)| name.starts_with("musicbrainz_"))
        .fold((0, 0), |(changed, kept), (name, to)| {
            if differs(name, to) {
                (changed + 1, kept)
            } else {
                (changed, kept + 1)
            }
        });
    if changed > 0 {
        changes.push(TagChange {
            field: "musicbrainz_ids".to_owned(),
            from: None,
            to: Some(if kept > 0 {
                format!("{changed} changed, {kept} kept")
            } else {
                format!("{changed} changed")
            }),
        });
    }
    changes
}

/// Sealed preview awaiting apply.
pub struct PreviewSealed {
    /// Single-use confirmation token.
    pub token: String,
    /// Expiry day (unix day, exclusive).
    pub expires_day: i64,
    /// Bundle id the apply will publish under.
    pub bundle_id: String,
    /// Planned files.
    pub files: Vec<PreviewFile>,
}

/// One applied file.
pub struct AppliedFile {
    /// Stable local track id.
    pub track_id: String,
    /// Adopted root id.
    pub root_id: String,
    /// Adopted path relative to the root.
    pub rel_path: String,
}

/// Published bundle answer.
pub struct AppliedBundle {
    /// Published bundle id.
    pub bundle_id: String,
    /// `committed` or `cleanup_pending`.
    pub outcome: String,
    /// Applied files.
    pub files: Vec<AppliedFile>,
}

/// Map a publisher failure onto HTTP. Collisions, stale state, and
/// capability blocks are 409s (valid input against the wrong
/// state); unsafe paths are 400s; store and I/O faults are 5xx.
pub(crate) fn publish_error(error: PublishError) -> ServiceError {
    match error {
        PublishError::Collision(message)
        | PublishError::Validation(message)
        | PublishError::Snapshot(message)
        | PublishError::Journal(message)
        | PublishError::Catalog(message)
        | PublishError::Space(message)
        | PublishError::Capability(message)
        | PublishError::NoSafePlacement(message)
        | PublishError::NotLinkable(message)
        | PublishError::Cleanup(message) => ServiceError::Conflict { message },
        PublishError::UnsafePath(message) | PublishError::Archive(message) => {
            ServiceError::InvalidInput { message }
        }
        PublishError::Store(message)
        | PublishError::Io(message)
        | PublishError::InjectedCrash(message) => ServiceError::internal(&message),
    }
}

impl LibrarySetup {
    /// Blocking write guards over every usable root in sorted order.
    /// The caller holds them across its publish-cell refresh plus
    /// its publish so scans never interleave with managed writes.
    /// Blocking: callers run off the async runtime.
    pub(crate) fn publish_guards(&self, registry: &RootRegistry) -> Vec<WriteGuard> {
        let mut ids = PublishCell::pending_ids(registry);
        ids.sort();
        ids.into_iter()
            .map(|id| self.fs.blocking_write(&id))
            .collect()
    }

    /// Async form of [`publish_guards`](Self::publish_guards) for
    /// callers already on the runtime.
    pub(crate) async fn publish_guards_async(&self, registry: &RootRegistry) -> Vec<WriteGuard> {
        let mut ids = PublishCell::pending_ids(registry);
        ids.sort();
        let mut guards = Vec::with_capacity(ids.len());
        for id in ids {
            guards.push(self.fs.write(&id).await);
        }
        guards
    }

    /// Drop expired preview seals.
    pub(crate) fn sweep_previews(&self) {
        let today = today_day();
        let mut previews = self
            .previews
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        previews.retain(|_, entry| entry.sealed.expires_day > today);
    }

    /// Album id for one track through the scan-fed join.
    fn track_album(&self, track_id: &str) -> Result<String, ServiceError> {
        self.scan_store
            .album_for_track(track_id)
            .ok_or_else(|| ServiceError::Conflict {
                message: format!("Track {track_id} is not in the catalog"),
            })
    }

    /// File management waits for an album whose match is a best guess
    /// (or matches nothing) until someone confirms it, unless the edition
    /// preferences let it manage unconfirmed matches.
    fn unconfirmed_gate(&self, album_id: &str) -> Result<(), ServiceError> {
        let allowed = super::management::planner::manage_unconfirmed(self);
        let reason = self
            .identify_store
            .with_connection(|conn| super::management::planner::match_gate(conn, album_id, allowed))
            .map_err(|error| {
                tracing::error!(%error, album = album_id, "match state unreadable; management refused");
                ServiceError::internal(&error)
            })?;
        match reason {
            Some(reason) => Err(ServiceError::Conflict {
                message: format!("{}: {} {}", reason.code, reason.message, reason.action),
            }),
            None => Ok(()),
        }
    }

    /// An album whose chosen edition is still being placed has files that
    /// are about to move, so no preview of them is made until that is done.
    fn remap_gate(&self, album_id: &str) -> Result<(), ServiceError> {
        let reason = self
            .identify_store
            .with_connection(|conn| super::management::planner::remap_gate(conn, album_id))
            .map_err(|error| {
                tracing::error!(%error, album = album_id, "edition remap state unreadable; management refused");
                ServiceError::internal(&error)
            })?;
        match reason {
            Some(reason) => Err(ServiceError::Conflict {
                message: format!("{} {}", reason.message, reason.action),
            }),
            None => Ok(()),
        }
    }

    /// Accepted exact identity for one track (management needs an
    /// accepted exact MusicBrainz release plus a full track mapping).
    fn resolve_identity(
        &self,
        album_id: &str,
        track_id: &str,
    ) -> Result<ReleaseIdentity, ServiceError> {
        let missing = |message: String| ServiceError::Conflict { message };
        let album = self
            .identify_store
            .album_identity(album_id)
            .ok_or_else(|| {
                missing(format!(
                    "Album {album_id} has no accepted identity; identify it first"
                ))
            })?;
        if album.release_mbid.is_none()
            && let Some(release_group_mbid) = album.release_group_mbid.clone()
        {
            // A custom edition: the identity Library Management planned with.
            let custom = self
                .identify_store
                .with_connection(|conn| super::management::custom::active(conn, album_id))
                .map_err(|error| ServiceError::internal(&error.to_string()))?
                .ok_or_else(|| {
                    missing(format!(
                        "Album {album_id} has no accepted exact release; approve an exact edition"
                    ))
                })?;
            let track = self.identify_store.track_identity(track_id);
            return Ok(ReleaseIdentity {
                release_mbid: custom.id,
                release_group_mbid,
                recording_mbid: track
                    .as_ref()
                    .and_then(|track| track.recording_mbid.clone())
                    .unwrap_or_default(),
                release_track_mbid: String::new(),
                album_identity_revision: album.row_revision,
                mapping_revision: track.map_or(0, |track| track.row_revision),
            });
        }
        let release_mbid = album.release_mbid.clone().ok_or_else(|| {
            missing(format!(
                "Album {album_id} has no accepted exact release; approve an exact edition"
            ))
        })?;
        let release_group_mbid = album
            .release_group_mbid
            .clone()
            .ok_or_else(|| missing(format!("Album {album_id} has no accepted release group")))?;
        let track = self
            .identify_store
            .track_identity(track_id)
            .ok_or_else(|| missing(format!("Track {track_id} has no accepted mapping")))?;
        let recording_mbid = track
            .recording_mbid
            .clone()
            .ok_or_else(|| missing(format!("Track {track_id} has an incomplete mapping")))?;
        let release_track_mbid = track
            .release_track_mbid
            .clone()
            .ok_or_else(|| missing(format!("Track {track_id} has an incomplete mapping")))?;
        Ok(ReleaseIdentity {
            release_mbid,
            release_group_mbid,
            recording_mbid,
            release_track_mbid,
            album_identity_revision: album.row_revision,
            mapping_revision: track.row_revision,
        })
    }

    /// Scan-assigned track id for one catalog file.
    fn scan_track(&self, root_id: &str, rel_path: &str) -> Result<String, ServiceError> {
        self.scan_store
            .track_at(root_id, rel_path)
            .ok_or_else(|| ServiceError::Conflict {
                message: format!("{root_id}/{rel_path} is not indexed; scan the root first"),
            })
    }

    /// Live fingerprint for one sandbox file. Unreadable files are
    /// 409s: valid input against a moved file.
    fn live_fingerprint(
        sandbox: &Sandbox,
        root_id: &str,
        rel_path: &str,
    ) -> Result<FileFingerprint, ServiceError> {
        let path = sandbox
            .resolve_no_symlink(root_id, rel_path)
            .map_err(publish_error)?;
        let bytes =
            super::publish::paths::read_regular_file(&path).map_err(|error| match error {
                PublishError::Validation(message) => ServiceError::Conflict { message },
                other => publish_error(other),
            })?;
        Ok(FileFingerprint {
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
        })
    }

    /// Current semantic tag document for one sandbox file.
    fn live_doc(
        sandbox: &Sandbox,
        root_id: &str,
        rel_path: &str,
    ) -> Result<TagDocument, ServiceError> {
        let path = sandbox
            .resolve_no_symlink(root_id, rel_path)
            .map_err(publish_error)?;
        super::publish::staging::document_from_file(&path).map_err(publish_error)
    }

    /// The before-state document for a write: the file's fields plus an
    /// empty entry per field the write adds, so undo removes those.
    fn live_doc_for_write(
        sandbox: &Sandbox,
        root_id: &str,
        rel_path: &str,
        managed_updates: &BTreeMap<String, Vec<String>>,
    ) -> Result<TagDocument, ServiceError> {
        let path = sandbox
            .resolve_no_symlink(root_id, rel_path)
            .map_err(publish_error)?;
        super::publish::staging::document_for_write(&path, managed_updates).map_err(publish_error)
    }

    /// The tags a retag writes: the identified release's full tag set
    /// (when its document is on file and the format is writable), with
    /// the caller's explicit updates on top.
    fn with_release_tags(
        &self,
        identity: &ReleaseIdentity,
        rel_path: &str,
        explicit: &BTreeMap<String, Vec<String>>,
    ) -> BTreeMap<String, Vec<String>> {
        let writable = super::tags::format_for_path(PathBuf::from(rel_path).as_path())
            .is_ok_and(super::tags::save::writable);
        if !writable {
            return explicit.clone();
        }
        let Some(release) = self.identify_store.release(&identity.release_mbid, None) else {
            tracing::info!(
                release = identity.release_mbid,
                "no stored release document; retag writes only the requested fields"
            );
            return explicit.clone();
        };
        let Some(track) = release
            .tracks
            .iter()
            .find(|track| track.id.eq_ignore_ascii_case(&identity.release_track_mbid))
        else {
            tracing::info!(
                release = identity.release_mbid,
                track = identity.release_track_mbid,
                "release document lacks the mapped track; retag writes only the requested fields"
            );
            return explicit.clone();
        };
        let mut tags = super::tags::picard::release_tags(&release, track);
        // A value the writer would refuse is dropped here, at preview,
        // rather than failing the publish.
        tags.retain(|name, values| {
            super::tags::TagField::from_name(name).is_some_and(|field| {
                super::tags::save::accepts(&super::tags::TagEdit::new(field, values.clone()))
            })
        });
        tags.extend(
            explicit
                .iter()
                .map(|(name, values)| (name.clone(), values.clone())),
        );
        tags
    }

    /// Lowercase container format for one rel path.
    fn live_format(rel_path: &str) -> Result<String, ServiceError> {
        super::tags::format_for_path(PathBuf::from(rel_path).as_path())
            .map(|format| format.as_str().to_owned())
            .map_err(|_| ServiceError::InvalidInput {
                message: format!("Unsupported audio format for {rel_path}"),
            })
    }

    /// Build and seal a management preview: retag writes in place,
    /// organize moves within the root. The preview dry-runs every
    /// apply gate (capability, collision, disk) so apply only fails
    /// on state that moved underneath. Blocking file and database
    /// work: handlers run this off the async runtime.
    pub fn plan_preview(
        &self,
        kind: PlanKind,
        album_id: &str,
        items: Vec<PreviewItemInput>,
    ) -> Result<PreviewSealed, ServiceError> {
        use super::publish::publisher::Catalog as _;

        if items.is_empty() {
            return Err(ServiceError::InvalidInput {
                message: "Preview needs at least one file".to_owned(),
            });
        }
        if items.len() > super::publish::MAX_PLAN_SUBJECTS {
            return Err(ServiceError::InvalidInput {
                message: "Preview exceeds the per-bundle file limit".to_owned(),
            });
        }
        let registry = self.live_registry();
        let (sandbox, catalog_revision) = {
            let mut cell = self
                .publish
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let _guards = if cell.needs_refresh(&registry) {
                self.publish_guards(&registry)
            } else {
                Vec::new()
            };
            cell.refresh(&registry, &self.root_dirs)
                .map_err(publish_error)?;
            let open = cell.open().map_err(publish_error)?;
            let revision = SqliteCatalog
                .revision(open.publisher.connection())
                .map_err(publish_error)?;
            (open.sandbox.clone(), revision)
        };
        // Accepted album identity first: without an exact release the
        // whole bundle blocks before any file is touched.
        let album = self
            .identify_store
            .album_identity(album_id)
            .ok_or_else(|| ServiceError::Conflict {
                message: format!("Album {album_id} has no accepted identity; identify it first"),
            })?;
        if album.release_mbid.is_none() {
            return Err(ServiceError::Conflict {
                message: format!(
                    "Album {album_id} has no accepted exact release; approve an exact edition"
                ),
            });
        }
        self.unconfirmed_gate(album_id)?;
        self.remap_gate(album_id)?;
        let mut plan_items = Vec::with_capacity(items.len());
        let mut docs = BTreeMap::new();
        let mut files = Vec::with_capacity(items.len());
        for item in &items {
            if item.root_id.is_empty() || item.rel_path.is_empty() {
                return Err(ServiceError::InvalidInput {
                    message: "Preview items need a root and a relative path".to_owned(),
                });
            }
            super::publish::staging::check_new_values(&item.managed_updates)
                .map_err(publish_error)?;
            let track_id = self.scan_track(&item.root_id, &item.rel_path)?;
            if self.track_album(&track_id)? != album_id {
                return Err(ServiceError::Conflict {
                    message: format!(
                        "{}/{} is not part of album {album_id}",
                        item.root_id, item.rel_path
                    ),
                });
            }
            let fingerprint = Self::live_fingerprint(&sandbox, &item.root_id, &item.rel_path)?;
            let format = Self::live_format(&item.rel_path)?;
            let identity = self.resolve_identity(album_id, &track_id)?;
            // A retag writes the release's full tag set; explicit
            // updates win over it. Organize moves keep their tags.
            let managed_updates = match kind {
                PlanKind::SamePath => {
                    self.with_release_tags(&identity, &item.rel_path, &item.managed_updates)
                }
                PlanKind::Move => item.managed_updates.clone(),
            };
            let (dest_root, dest_rel, item_kind) = match kind {
                PlanKind::SamePath => (
                    item.root_id.clone(),
                    item.rel_path.clone(),
                    PlanKind::SamePath,
                ),
                PlanKind::Move => {
                    let Some(dest_rel) = item.dest_rel.clone() else {
                        return Err(ServiceError::InvalidInput {
                            message: "Organize items need a destination path".to_owned(),
                        });
                    };
                    if dest_rel.is_empty() {
                        return Err(ServiceError::InvalidInput {
                            message: "Organize items need a destination path".to_owned(),
                        });
                    }
                    (item.root_id.clone(), dest_rel, PlanKind::Move)
                }
            };
            let mut capabilities = Vec::new();
            if !managed_updates.is_empty() {
                capabilities.push(Capability::Metadata);
            }
            if item_kind == PlanKind::Move {
                capabilities.push(Capability::SameRootMove);
            }
            let source = sandbox
                .resolve_no_symlink(&item.root_id, &item.rel_path)
                .map_err(publish_error)?;
            let size = std::fs::metadata(&source)
                .map(|meta| meta.len())
                .unwrap_or(fingerprint.size);
            plan_items.push(PlanItem {
                track_id: track_id.clone(),
                source_root: item.root_id.clone(),
                source_rel: item.rel_path.clone(),
                dest_root: dest_root.clone(),
                dest_rel: dest_rel.clone(),
                kind: item_kind,
                fingerprint,
                identity,
                override_revision: PINNED_OVERRIDE,
                capabilities,
                format,
                managed_updates: managed_updates.clone(),
                sidecars: Vec::new(),
                staged_bytes_estimate: size + STAGED_HEADROOM_BYTES,
                adopt: None,
                replaces: None,
                outputs: Vec::new(),
                removals: Vec::new(),
            });
            let doc = Self::live_doc_for_write(
                &sandbox,
                &item.root_id,
                &item.rel_path,
                &managed_updates,
            )?;
            // The before-state the seal keeps is the file's tags now, so
            // the changes come from it with no extra read.
            let changes = match item_kind {
                PlanKind::SamePath => tag_changes(&doc.managed, &managed_updates),
                PlanKind::Move => Vec::new(),
            };
            docs.insert(track_id.clone(), doc);
            files.push(PreviewFile {
                track_id,
                source: format!("{}/{}", item.root_id, item.rel_path),
                dest: format!("{dest_root}/{dest_rel}"),
                kind: match item_kind {
                    PlanKind::SamePath => "same_path".to_owned(),
                    PlanKind::Move => "move".to_owned(),
                },
                changes,
            });
        }
        let bundle = PlanBundle {
            id: self.ids.new_id(),
            items: plan_items,
            profile_revision: PINNED_REVISION,
            naming_revision: PINNED_REVISION,
            policy_revision: policy_revision_u64(&registry),
            catalog_revision,
        };
        CollisionGate::check_bundle(&sandbox, &bundle).map_err(publish_error)?;
        DiskPreflight::check(&bundle, &FsSpaceProbe::new(self.root_dirs.clone()))
            .map_err(publish_error)?;
        let token = self.ids.new_id();
        let token_hash = sha256_hex(token.as_bytes());
        let expires_day = today_day() + PREVIEW_TTL_DAYS;
        let sealed = SealedPreview::seal(
            bundle.clone(),
            token_hash.clone(),
            PINNED_REVISION,
            expires_day,
        );
        {
            let mut previews = self
                .previews
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            previews.insert(
                token_hash,
                PreviewEntry {
                    sealed,
                    docs,
                    plain_retag: plain_retag(kind, &items),
                },
            );
        }
        Ok(PreviewSealed {
            token,
            expires_day,
            bundle_id: bundle.id,
            files,
        })
    }

    /// Whether the preview behind `token` is a plain retag; `None` when
    /// no such preview waits. Looking does not spend the token.
    pub fn preview_is_plain_retag(&self, token: &str) -> Option<bool> {
        let previews = self
            .previews
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        previews
            .get(&sha256_hex(token.as_bytes()))
            .map(|entry| entry.plain_retag)
    }

    /// Apply a sealed preview exactly once. The write guards span
    /// the refresh plus the publish, so no scan interleaves with
    /// the commit. `applied_by` is recorded so that account may undo
    /// it. Blocking: handlers run this off the async runtime.
    pub fn apply_preview(
        &self,
        token: &str,
        applied_by: Option<&str>,
    ) -> Result<AppliedBundle, ServiceError> {
        use super::publish::publisher::Catalog as _;

        let token_hash = sha256_hex(token.as_bytes());
        let entry = {
            let mut previews = self
                .previews
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            previews.remove(&token_hash).ok_or(ServiceError::NotFound)?
        };
        if today_day() >= entry.sealed.expires_day {
            return Err(ServiceError::Conflict {
                message: "Preview expired; plan it again".to_owned(),
            });
        }
        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.live_registry();
        let _guards = self.publish_guards(&registry);
        cell.refresh(&registry, &self.root_dirs)
            .map_err(publish_error)?;
        let open = cell.open().map_err(publish_error)?;
        let live = self.seal_live(&open.sandbox, &entry.sealed.bundle, &token_hash)?;
        let catalog_revision = SqliteCatalog
            .revision(open.publisher.connection())
            .map_err(publish_error)?;
        let mut live = live;
        live.catalog_revision = catalog_revision;
        open.publisher
            .set_undo_retention_days(self.undo_retention_days());
        let outcome = open
            .publisher
            .publish_as(&entry.sealed, &live, &entry.docs, applied_by)
            .map_err(publish_error)?;
        Ok(AppliedBundle {
            bundle_id: entry.sealed.bundle.id.clone(),
            outcome: match outcome {
                PublishOutcome::Committed => "committed".to_owned(),
                PublishOutcome::CleanupPending => "cleanup_pending".to_owned(),
            },
            files: entry
                .sealed
                .bundle
                .items
                .iter()
                .map(|item| AppliedFile {
                    track_id: item.track_id.clone(),
                    root_id: item.dest_root.clone(),
                    rel_path: item.dest_rel.clone(),
                })
                .collect(),
        })
    }

    /// Live seal recheck for one bundle: fresh fingerprints,
    /// identities, and revisions under the publish lock.
    fn seal_live(
        &self,
        sandbox: &Sandbox,
        bundle: &PlanBundle,
        token_hash: &str,
    ) -> Result<SealRecheck, ServiceError> {
        self.live_seal(sandbox, bundle, token_hash, SealPins::constant(), &|_| {
            PINNED_OVERRIDE
        })
        .map_err(|error| match error {
            PublishError::Validation(message) => ServiceError::Conflict { message },
            other => publish_error(other),
        })
    }

    /// The live state a seal is checked against: fresh fingerprints and
    /// identities, the caller's current revisions and per-file override
    /// revisions.
    fn live_seal(
        &self,
        sandbox: &Sandbox,
        bundle: &PlanBundle,
        token_hash: &str,
        pins: SealPins,
        override_of: &dyn Fn(&str) -> u64,
    ) -> Result<SealRecheck, PublishError> {
        let mut fingerprints = BTreeMap::new();
        let mut identities = BTreeMap::new();
        let mut overrides = BTreeMap::new();
        for item in &bundle.items {
            let changed = || {
                PublishError::Validation(format!(
                    "Track {} changed under the preview",
                    item.track_id
                ))
            };
            fingerprints.insert(
                item.track_id.clone(),
                Self::live_fingerprint(sandbox, &item.source_root, &item.source_rel)
                    .map_err(|_| changed())?,
            );
            let album_id = self.track_album(&item.track_id).map_err(|_| changed())?;
            identities.insert(
                item.track_id.clone(),
                self.resolve_identity(&album_id, &item.track_id)
                    .map_err(|_| {
                        PublishError::Validation(format!(
                            "Identity for track {} changed",
                            item.track_id
                        ))
                    })?,
            );
            overrides.insert(item.track_id.clone(), override_of(&item.track_id));
        }
        Ok(SealRecheck {
            fingerprints,
            identities,
            overrides,
            profile_revision: pins.profile,
            naming_revision: pins.naming,
            policy_revision: policy_revision_u64(&self.live_registry()),
            catalog_revision: bundle.catalog_revision,
            settings_revision: pins.settings,
            today_day: today_day(),
            token_hash: token_hash.to_owned(),
        })
    }

    /// How long undo keeps before-states, read from the settings per call.
    fn undo_retention_days(&self) -> i64 {
        self.config
            .get::<droppedneedle_kernel::runtime_config::sections::LibraryManagement>()
            .map(|settings| settings.undo_retention_days)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "management settings unreadable; default undo retention");
                super::publish::DEFAULT_UNDO_RETENTION_DAYS as i64
            })
    }

    /// Run publish recovery now: reopen (and reconcile) when the roots
    /// changed, and retry any held bundle, so a bundle a crash left half
    /// way is finished or unwound before anyone asks how it ended.
    /// Blocking.
    /// Arm (or clear) a crash point on the open publisher, so a test can
    /// stop a publish part way as a killed process would. Tests only.
    #[cfg(any(test, feature = "test-support"))]
    pub fn arm_publish_crash(&self, point: Option<super::publish::CrashPoint>) {
        let registry = self.live_registry();
        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cell.refresh(&registry, &self.root_dirs).is_ok()
            && let Ok(open) = cell.open()
        {
            open.publisher.set_crash_point(point);
        }
    }

    pub(crate) fn settle_publishes(&self) -> Result<(), PublishError> {
        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.live_registry();
        let reopen = cell.needs_refresh(&registry);
        let retry = !reopen && cell.has_held();
        let _guards = if reopen || retry {
            self.publish_guards(&registry)
        } else {
            Vec::new()
        };
        cell.refresh(&registry, &self.root_dirs)?;
        if retry {
            cell.retry_held()?;
        }
        Ok(())
    }

    /// The sandbox over the usable roots, opening the publish cell when
    /// the root set changed. Blocking.
    pub(crate) fn publish_sandbox(&self) -> Result<Sandbox, PublishError> {
        let registry = self.live_registry();
        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guards = if cell.needs_refresh(&registry) {
            self.publish_guards(&registry)
        } else {
            Vec::new()
        };
        cell.refresh(&registry, &self.root_dirs)?;
        Ok(cell.open()?.sandbox.clone())
    }

    /// Probe one root now, under the session the saved library folders
    /// call for: a folder excluded or removed since the session opened is
    /// no longer in it and answers `None`, and nothing is written there.
    /// Waits for a write in flight. Blocking.
    pub(crate) fn check_placement(
        &self,
        root_id: &str,
    ) -> Result<Option<PlacementCheck>, PublishError> {
        let registry = self.live_registry();
        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guards = if cell.needs_refresh(&registry) {
            self.publish_guards(&registry)
        } else {
            Vec::new()
        };
        cell.refresh(&registry, &self.root_dirs)?;
        cell.recheck_placement(root_id)
    }

    /// Publish one album a management job planned. The tags to write and
    /// the destinations were fixed at planning; the before-state is read
    /// now, and the seal refuses any file, identity, override or revision
    /// that moved since. Blocking: the job worker runs it.
    pub(crate) fn publish_plans(
        &self,
        bundle_id: &str,
        plan_items: Vec<PlanItem>,
        pins: SealPins,
        override_of: &dyn Fn(&str) -> u64,
    ) -> Result<AppliedBundle, PublishError> {
        use super::publish::publisher::Catalog as _;

        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.live_registry();
        let _guards = self.publish_guards(&registry);
        cell.refresh(&registry, &self.root_dirs)?;
        let open = cell.open()?;
        let mut docs = BTreeMap::new();
        for item in &plan_items {
            let path = open
                .sandbox
                .resolve_no_symlink(&item.source_root, &item.source_rel)?;
            docs.insert(
                item.track_id.clone(),
                super::publish::staging::document_for_write(&path, &item.managed_updates)?,
            );
        }
        let catalog_revision = SqliteCatalog.revision(open.publisher.connection())?;
        open.publisher
            .set_undo_retention_days(self.undo_retention_days());
        let bundle = PlanBundle {
            id: bundle_id.to_owned(),
            items: plan_items,
            profile_revision: pins.profile,
            naming_revision: pins.naming,
            policy_revision: policy_revision_u64(&registry),
            catalog_revision,
        };
        let token_hash = sha256_hex(self.ids.new_id().as_bytes());
        let sealed = SealedPreview::seal(
            bundle,
            token_hash.clone(),
            pins.settings,
            today_day() + PREVIEW_TTL_DAYS,
        );
        let live = self.live_seal(
            &open.sandbox,
            &sealed.bundle,
            &token_hash,
            pins,
            override_of,
        )?;
        let outcome = open.publisher.publish(&sealed, &live, &docs)?;
        Ok(AppliedBundle {
            bundle_id: sealed.bundle.id.clone(),
            outcome: match outcome {
                PublishOutcome::Committed => "committed".to_owned(),
                PublishOutcome::CleanupPending => "cleanup_pending".to_owned(),
            },
            files: sealed
                .bundle
                .items
                .iter()
                .map(|item| AppliedFile {
                    track_id: item.track_id.clone(),
                    root_id: item.dest_root.clone(),
                    rel_path: item.dest_rel.clone(),
                })
                .collect(),
        })
    }

    /// Undo one published bundle as a new operation over the exact
    /// immediate before state. Blocked files (external edits, moves,
    /// identity drift, expired snapshots, occupied restores) 409 with
    /// per-file reasons; the bundle only writes when every sibling is
    /// eligible. The write guards span the refresh plus the
    /// restoration publish. With `only_by`, a bundle that account did
    /// not apply is not found, so its existence does not leak.
    /// Blocking: handlers run this off the async runtime.
    pub fn undo_bundle(
        &self,
        bundle_id: &str,
        only_by: Option<&str>,
    ) -> Result<AppliedBundle, ServiceError> {
        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.live_registry();
        let _guards = self.publish_guards(&registry);
        cell.refresh(&registry, &self.root_dirs)
            .map_err(publish_error)?;
        let open = cell.open().map_err(publish_error)?;
        if let Some(caller) = only_by {
            let applied_by =
                super::publish::operations::applied_by(open.publisher.connection(), bundle_id)
                    .map_err(publish_error)?;
            if applied_by.flatten().as_deref() != Some(caller) {
                return Err(ServiceError::NotFound);
            }
        }
        let files = self.undo_files_in(open, bundle_id, &|_| PINNED_OVERRIDE)?;
        if !files.blocked.is_empty() || files.eligible.is_empty() {
            return Err(ServiceError::Conflict {
                message: format!("Undo blocked: {}", undo_blocks(&files.blocked)),
            });
        }
        let mut plan_items = Vec::new();
        let mut docs = BTreeMap::new();
        for file in files.eligible {
            let path = open
                .sandbox
                .resolve_no_symlink(&file.plan.source_root, &file.plan.source_rel)
                .map_err(publish_error)?;
            docs.insert(
                file.plan.track_id.clone(),
                super::publish::staging::document_for_write(&path, &file.plan.managed_updates)
                    .map_err(publish_error)?,
            );
            plan_items.push(file.plan);
        }
        let applied = self.publish_restoration(&mut cell, plan_items, docs)?;
        drop(cell);
        // An undone run of the profile is not run again by itself. The
        // undo stands either way.
        if let Err(error) =
            super::management::service::Management::new(self).bundle_undone(bundle_id)
        {
            tracing::warn!(%error, bundle_id, "undone bundle not recorded for automatic management");
        }
        Ok(applied)
    }

    /// What undoing one published bundle would do, file by file. Blocking.
    pub(crate) fn undo_files(
        &self,
        bundle_id: &str,
        override_of: &dyn Fn(&str) -> u64,
    ) -> Result<UndoFiles, ServiceError> {
        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.live_registry();
        let _guards = if cell.needs_refresh(&registry) {
            self.publish_guards(&registry)
        } else {
            Vec::new()
        };
        cell.refresh(&registry, &self.root_dirs)
            .map_err(publish_error)?;
        let open = cell.open().map_err(publish_error)?;
        self.undo_files_in(open, bundle_id, override_of)
    }

    /// Sidecars an apply moved, moved back: each one still holding the
    /// bytes the apply put there, going to an old path nothing took since.
    /// The others stay where they are and are named in the second list.
    fn undo_sidecars(
        sandbox: &Sandbox,
        moved: &[super::publish::planner::SidecarPlan],
    ) -> (Vec<super::publish::planner::SidecarPlan>, Vec<String>) {
        let mut back = Vec::new();
        let mut left = Vec::new();
        for sidecar in moved {
            let unchanged = sandbox
                .resolve_no_symlink(&sidecar.dest_root, &sidecar.dest_rel)
                .ok()
                .and_then(|path| super::publish::paths::read_regular_file(&path).ok())
                .is_some_and(|bytes| sha256_hex(&bytes) == sidecar.fingerprint.sha256);
            let free = sandbox
                .resolve_no_symlink(&sidecar.source_root, &sidecar.source_rel)
                .is_ok_and(|path| path.symlink_metadata().is_err());
            if unchanged && free {
                back.push(super::publish::planner::SidecarPlan {
                    source_root: sidecar.dest_root.clone(),
                    source_rel: sidecar.dest_rel.clone(),
                    dest_root: sidecar.source_root.clone(),
                    dest_rel: sidecar.source_rel.clone(),
                    fingerprint: sidecar.fingerprint.clone(),
                });
            } else {
                left.push(sidecar.dest_rel.clone());
            }
        }
        (back, left)
    }

    /// [`undo_files`](Self::undo_files) under a publish cell the caller holds.
    fn undo_files_in(
        &self,
        open: &OpenPublish,
        bundle_id: &str,
        override_of: &dyn Fn(&str) -> u64,
    ) -> Result<UndoFiles, ServiceError> {
        use super::publish::publisher::Catalog as _;
        use super::publish::undo::{UndoInput, UndoLive, plan_undo};

        let conn = open.publisher.connection();
        let source = super::publish::operations::load_operation(conn, bundle_id)
            .map_err(publish_error)?
            .ok_or(ServiceError::NotFound)?;
        let snapshots = SnapshotStore::new(conn)
            .bundle(bundle_id)
            .map_err(publish_error)?;
        let journals = JournalStore::new(conn)
            .bundle(bundle_id)
            .map_err(publish_error)?;
        let blobs = BlobStore::new(conn);
        let by_track: HashMap<&str, &PlanItem> = source
            .items
            .iter()
            .map(|item| (item.track_id.as_str(), item))
            .collect();
        let mut inputs = Vec::new();
        for (_, track_id, blob_sha, expires_day) in &snapshots {
            let Some(item) = by_track.get(track_id.as_str()) else {
                continue;
            };
            let Some(journal) = journals
                .iter()
                .find(|journal| journal.track_id.as_deref() == Some(track_id.as_str()))
            else {
                continue;
            };
            let bytes = blobs.get(blob_sha).map_err(publish_error)?;
            let before =
                super::publish::undo::BeforeState::from_bytes(&bytes).map_err(publish_error)?;
            let staged_size = super::publish::paths::read_regular_file(
                &open
                    .sandbox
                    .resolve_no_symlink(&journal.dest_root, &journal.dest_rel)
                    .unwrap_or_else(|_| PathBuf::from(&journal.staged)),
            )
            .map(|bytes| bytes.len() as u64)
            .unwrap_or(item.fingerprint.size);
            inputs.push(UndoInput {
                track_id: track_id.clone(),
                before,
                published: FileFingerprint {
                    size: staged_size,
                    sha256: journal.staged_sha256.clone(),
                },
                published_root: journal.dest_root.clone(),
                published_rel: journal.dest_rel.clone(),
                identity: item.identity.clone(),
                override_revision: item.override_revision,
                expires_day: *expires_day,
            });
        }
        let mut files = UndoFiles::default();
        let mut fingerprints = BTreeMap::new();
        let mut locations = BTreeMap::new();
        let mut identities = BTreeMap::new();
        let mut overrides = BTreeMap::new();
        let mut live_inputs = Vec::new();
        for input in inputs {
            let located = SqliteCatalog
                .locate(conn, &input.track_id)
                .map_err(publish_error)?;
            let live = located.and_then(|(root_id, rel_path, _, _)| {
                let fingerprint =
                    Self::live_fingerprint(&open.sandbox, &root_id, &rel_path).ok()?;
                let album_id = self.track_album(&input.track_id).ok()?;
                let identity = self.resolve_identity(&album_id, &input.track_id).ok()?;
                Some((root_id, rel_path, fingerprint, identity))
            });
            let Some((root_id, rel_path, fingerprint, identity)) = live else {
                files.blocked.push((input.track_id.clone(), UndoSkip::Gone));
                continue;
            };
            fingerprints.insert(input.track_id.clone(), fingerprint);
            locations.insert(input.track_id.clone(), (root_id, rel_path));
            identities.insert(input.track_id.clone(), identity);
            overrides.insert(input.track_id.clone(), override_of(&input.track_id));
            live_inputs.push(input);
        }
        let live = UndoLive {
            fingerprints,
            locations,
            identities,
            overrides,
            today_day: today_day(),
        };
        let sandbox = open.sandbox.clone();
        let live_paths: Vec<PathBuf> = live
            .locations
            .values()
            .filter_map(|(root, rel)| sandbox.resolve_no_symlink(root, rel).ok())
            .collect();
        let plan = plan_undo(bundle_id, &live_inputs, &live, &|root_id, rel_path| {
            match sandbox.resolve_no_symlink(root_id, rel_path) {
                // Undoing a case-only rename on a case-folding filesystem
                // reaches the file itself: that is not an occupier.
                Ok(path) => {
                    path.symlink_metadata().is_ok()
                        && !live_paths
                            .iter()
                            .any(|live| super::publish::paths::same_file(live, &path))
                }
                // An unresolvable restore target fails closed as
                // occupied: undo never writes through a symlink.
                Err(_) => true,
            }
        });
        files.blocked.extend(
            plan.blocked
                .into_iter()
                .map(|(track, block)| (track, UndoSkip::Plan(block))),
        );
        for item in plan.eligible {
            let (Some(live_loc), Some(fingerprint), Some(identity)) = (
                live.locations.get(&item.track_id),
                live.fingerprints.get(&item.track_id),
                live.identities.get(&item.track_id),
            ) else {
                files.blocked.push((item.track_id.clone(), UndoSkip::Gone));
                continue;
            };
            let format = Self::live_format(&live_loc.1)?;
            let same_path = live_loc.0 == item.restore_root && live_loc.1 == item.restore_rel;
            let mut capabilities = Vec::new();
            if !item.doc.managed.is_empty() {
                capabilities.push(Capability::Metadata);
            }
            if !same_path {
                capabilities.push(if live_loc.0 == item.restore_root {
                    Capability::SameRootMove
                } else {
                    Capability::CrossRootMove
                });
            }
            let current = Self::live_doc(&open.sandbox, &live_loc.0, &live_loc.1)
                .map(|doc| doc.managed)
                .unwrap_or_default();
            let (sidecars, mut left) = by_track
                .get(item.track_id.as_str())
                .map(|source| Self::undo_sidecars(&open.sandbox, &source.sidecars))
                .unwrap_or_default();
            if !sidecars.is_empty() {
                capabilities.push(Capability::Sidecars);
            }
            // Files the run generated go away, while they are still what it wrote.
            let mut removals = Vec::new();
            for output in by_track
                .get(item.track_id.as_str())
                .map(|source| source.outputs.as_slice())
                .unwrap_or_default()
            {
                let unchanged = open
                    .sandbox
                    .resolve_no_symlink(&output.dest_root, &output.dest_rel)
                    .ok()
                    .and_then(|path| super::publish::paths::read_regular_file(&path).ok())
                    .is_some_and(|bytes| sha256_hex(&bytes) == output.sha256);
                if unchanged {
                    removals.push(output.clone());
                } else {
                    left.push(output.dest_rel.clone());
                }
            }
            files.eligible.push(UndoFile {
                current,
                sidecars_left: left,
                plan: PlanItem {
                    track_id: item.track_id.clone(),
                    source_root: live_loc.0.clone(),
                    source_rel: live_loc.1.clone(),
                    dest_root: item.restore_root.clone(),
                    dest_rel: item.restore_rel.clone(),
                    kind: if same_path {
                        PlanKind::SamePath
                    } else {
                        PlanKind::Move
                    },
                    fingerprint: fingerprint.clone(),
                    identity: identity.clone(),
                    override_revision: override_of(&item.track_id),
                    capabilities,
                    format,
                    managed_updates: item.doc.managed.clone(),
                    sidecars,
                    staged_bytes_estimate: fingerprint.size + STAGED_HEADROOM_BYTES,
                    adopt: None,
                    replaces: None,
                    outputs: Vec::new(),
                    removals,
                },
            });
        }
        Ok(files)
    }

    /// Restore tracks to their immutable first-management baselines.
    /// Missing baselines, missing roots, changed files, and occupied
    /// originals 409 with per-file reasons. The write guards span
    /// the refresh plus the restoration publish. Blocking: handlers
    /// run this off the async runtime.
    pub fn baseline_restore(&self, track_ids: &[String]) -> Result<AppliedBundle, ServiceError> {
        if track_ids.is_empty() {
            return Err(ServiceError::InvalidInput {
                message: "Restore needs at least one track".to_owned(),
            });
        }
        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.live_registry();
        let _guards = self.publish_guards(&registry);
        cell.refresh(&registry, &self.root_dirs)
            .map_err(publish_error)?;
        let open = cell.open().map_err(publish_error)?;
        let files = self.baseline_files_in(open, track_ids, &|_| PINNED_OVERRIDE)?;
        if !files.blocked.is_empty() || files.eligible.is_empty() {
            return Err(ServiceError::Conflict {
                message: format!(
                    "Baseline restore blocked: {}",
                    baseline_blocks(&files.blocked)
                ),
            });
        }
        let mut plan_items = Vec::new();
        let mut docs = BTreeMap::new();
        for file in files.eligible {
            docs.insert(
                file.plan.track_id.clone(),
                Self::live_doc_for_write(
                    &open.sandbox,
                    &file.plan.source_root,
                    &file.plan.source_rel,
                    &file.plan.managed_updates,
                )?,
            );
            plan_items.push(file.plan);
        }
        self.publish_restoration(&mut cell, plan_items, docs)
    }

    /// What restoring these tracks to their first-management baselines
    /// would do, file by file. Blocking.
    pub(crate) fn baseline_files(
        &self,
        track_ids: &[String],
        override_of: &dyn Fn(&str) -> u64,
    ) -> Result<BaselineFiles, ServiceError> {
        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.live_registry();
        let _guards = if cell.needs_refresh(&registry) {
            self.publish_guards(&registry)
        } else {
            Vec::new()
        };
        cell.refresh(&registry, &self.root_dirs)
            .map_err(publish_error)?;
        let open = cell.open().map_err(publish_error)?;
        self.baseline_files_in(open, track_ids, override_of)
    }

    /// [`baseline_files`](Self::baseline_files) under a publish cell the
    /// caller holds. A file that is gone or unreadable is blocked rather
    /// than failing the others.
    fn baseline_files_in(
        &self,
        open: &OpenPublish,
        track_ids: &[String],
        override_of: &dyn Fn(&str) -> u64,
    ) -> Result<BaselineFiles, ServiceError> {
        use super::publish::publisher::Catalog as _;
        use super::publish::undo::{BaselineInput, plan_baseline_restore};

        let conn = open.publisher.connection();
        let blobs = BlobStore::new(conn);
        let baselines = BaselineStore::new(conn);
        let mut files = BaselineFiles::default();
        let mut inputs = Vec::new();
        let mut current_locs: HashMap<String, (String, String)> = HashMap::new();
        let mut live: HashMap<String, (FileFingerprint, String, ReleaseIdentity)> = HashMap::new();
        for track_id in track_ids {
            let baseline = match baselines.get(track_id).map_err(publish_error)? {
                Some((blob_sha, _, _)) => {
                    let bytes = blobs.get(&blob_sha).map_err(publish_error)?;
                    Some(
                        super::publish::undo::BeforeState::from_bytes(&bytes)
                            .map_err(publish_error)?,
                    )
                }
                None => None,
            };
            let located = SqliteCatalog
                .locate(conn, track_id)
                .map_err(publish_error)?;
            let state = located.and_then(|(root_id, rel_path, _, _)| {
                let current = Self::live_fingerprint(&open.sandbox, &root_id, &rel_path).ok()?;
                let format = Self::live_format(&rel_path).ok()?;
                let album_id = self.track_album(track_id).ok()?;
                let identity = self.resolve_identity(&album_id, track_id).ok()?;
                Some((root_id, rel_path, current, format, identity))
            });
            let Some((root_id, rel_path, current, format, identity)) = state else {
                files.blocked.push((track_id.clone(), BaselineSkip::Gone));
                continue;
            };
            current_locs.insert(track_id.clone(), (root_id, rel_path));
            live.insert(
                track_id.clone(),
                (current.clone(), format.clone(), identity.clone()),
            );
            inputs.push(BaselineInput {
                track_id: track_id.clone(),
                baseline,
                current: current.clone(),
                pinned_current: current,
                identity: identity.clone(),
                pinned_identity: identity,
                format: format.clone(),
                pinned_format: format,
            });
        }
        let sandbox = open.sandbox.clone();
        let registry = self.live_registry();
        // The original counts as occupied only when another file holds
        // it: a same-path restore whose subject still sits at the
        // original path is restoring tags in place, mirroring undo's
        // self-occupancy exemption.
        let plan = plan_baseline_restore(&inputs, &|root_id, rel_path| {
            registry.resolve(root_id)?;
            let occupied = match sandbox.resolve_no_symlink(root_id, rel_path) {
                Ok(path) => path.symlink_metadata().is_ok(),
                // An unresolvable original fails closed as occupied:
                // restore never writes through a symlink.
                Err(_) => true,
            };
            if !occupied {
                return Some(false);
            }
            let self_held = current_locs
                .values()
                .any(|(root, rel)| root == root_id && rel == rel_path);
            Some(!self_held)
        });
        files.blocked.extend(
            plan.blocked
                .into_iter()
                .map(|(track, block)| (track, BaselineSkip::Plan(block))),
        );
        for (track_id, before) in plan.eligible {
            let (Some(live_loc), Some((fingerprint, format, identity))) =
                (current_locs.get(&track_id), live.get(&track_id))
            else {
                files.blocked.push((track_id, BaselineSkip::Gone));
                continue;
            };
            let Ok(current) = Self::live_doc(&sandbox, &live_loc.0, &live_loc.1) else {
                files.blocked.push((track_id, BaselineSkip::Gone));
                continue;
            };
            let same_path = live_loc.0 == before.source_root && live_loc.1 == before.source_rel;
            let updates =
                super::publish::staging::restore_updates(&before.doc, &current, &live_loc.1);
            // Files management generated for the track (a cover, an
            // `.lrc`) go too, while they still hold the bytes it wrote.
            let removals: Vec<_> = super::publish::operations::generated_for_track(conn, &track_id)
                .map_err(publish_error)?
                .into_iter()
                .filter(|output| {
                    sandbox
                        .resolve_no_symlink(&output.dest_root, &output.dest_rel)
                        .ok()
                        .and_then(|path| super::publish::paths::read_regular_file(&path).ok())
                        .is_some_and(|bytes| sha256_hex(&bytes) == output.sha256)
                })
                .collect();
            let mut capabilities = Vec::new();
            if !updates.is_empty() {
                capabilities.push(Capability::Metadata);
            }
            if !same_path {
                capabilities.push(if live_loc.0 == before.source_root {
                    Capability::SameRootMove
                } else {
                    Capability::CrossRootMove
                });
            }
            files.eligible.push(UndoFile {
                current: current.managed,
                sidecars_left: Vec::new(),
                plan: PlanItem {
                    track_id: track_id.clone(),
                    source_root: live_loc.0.clone(),
                    source_rel: live_loc.1.clone(),
                    dest_root: before.source_root.clone(),
                    dest_rel: before.source_rel.clone(),
                    kind: if same_path {
                        PlanKind::SamePath
                    } else {
                        PlanKind::Move
                    },
                    fingerprint: fingerprint.clone(),
                    identity: identity.clone(),
                    override_revision: override_of(&track_id),
                    capabilities,
                    format: format.clone(),
                    managed_updates: updates,
                    sidecars: Vec::new(),
                    staged_bytes_estimate: fingerprint.size + STAGED_HEADROOM_BYTES,
                    adopt: None,
                    replaces: None,
                    outputs: Vec::new(),
                    removals,
                },
            });
        }
        Ok(files)
    }
    /// Seal and publish an internally planned restoration bundle
    /// (undo or baseline restore). The caller holds the publish
    /// lock plus the write guards; the seal and the commit run
    /// under both.
    fn publish_restoration(
        &self,
        cell: &mut PublishCell,
        plan_items: Vec<PlanItem>,
        docs: BTreeMap<String, TagDocument>,
    ) -> Result<AppliedBundle, ServiceError> {
        use super::publish::publisher::Catalog as _;

        let open = cell.open().map_err(publish_error)?;
        let catalog_revision = SqliteCatalog
            .revision(open.publisher.connection())
            .map_err(publish_error)?;
        let bundle = PlanBundle {
            id: self.ids.new_id(),
            items: plan_items,
            profile_revision: PINNED_REVISION,
            naming_revision: PINNED_REVISION,
            policy_revision: policy_revision_u64(&self.live_registry()),
            catalog_revision,
        };
        // Restoration bundles re-run the dry-run gates: a restore
        // onto an occupied path blocks instead of overwriting.
        CollisionGate::check_bundle(&open.sandbox, &bundle).map_err(publish_error)?;
        DiskPreflight::check(&bundle, &FsSpaceProbe::new(self.root_dirs.clone()))
            .map_err(publish_error)?;
        let token = self.ids.new_id();
        let token_hash = sha256_hex(token.as_bytes());
        let sealed = SealedPreview::seal(
            bundle,
            token_hash.clone(),
            PINNED_REVISION,
            today_day() + PREVIEW_TTL_DAYS,
        );
        let live = self.seal_live(&open.sandbox, &sealed.bundle, &token_hash)?;
        open.publisher
            .set_undo_retention_days(self.undo_retention_days());
        let outcome = open
            .publisher
            .publish(&sealed, &live, &docs)
            .map_err(publish_error)?;
        Ok(AppliedBundle {
            bundle_id: sealed.bundle.id.clone(),
            outcome: match outcome {
                PublishOutcome::Committed => "committed".to_owned(),
                PublishOutcome::CleanupPending => "cleanup_pending".to_owned(),
            },
            files: sealed
                .bundle
                .items
                .iter()
                .map(|item| AppliedFile {
                    track_id: item.track_id.clone(),
                    root_id: item.dest_root.clone(),
                    rel_path: item.dest_rel.clone(),
                })
                .collect(),
        })
    }
}

/// Per-file undo blocks as a compact message.
fn undo_blocks(blocked: &[(String, UndoSkip)]) -> String {
    if blocked.is_empty() {
        return "the bundle holds no undoable snapshots".to_owned();
    }
    blocked
        .iter()
        .map(|(track_id, skip)| match skip {
            UndoSkip::Plan(block) => format!("{track_id} {}", undo_block_label(block)),
            UndoSkip::Gone => format!("{track_id} not_managed"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn undo_block_label(block: &super::publish::undo::UndoBlock) -> &'static str {
    match block {
        super::publish::undo::UndoBlock::ExternallyChanged => "externally_changed",
        super::publish::undo::UndoBlock::Moved => "moved",
        super::publish::undo::UndoBlock::IdentityChanged => "identity_changed",
        super::publish::undo::UndoBlock::OverrideChanged => "override_changed",
        super::publish::undo::UndoBlock::SnapshotExpired => "snapshot_expired",
        super::publish::undo::UndoBlock::RestoreOccupied => "restore_occupied",
    }
}

/// Per-file baseline blocks as a compact message.
fn baseline_blocks(blocked: &[(String, BaselineSkip)]) -> String {
    if blocked.is_empty() {
        return "no track has a saved original".to_owned();
    }
    blocked
        .iter()
        .map(|(track_id, skip)| match skip {
            BaselineSkip::Plan(block) => format!("{track_id} {}", baseline_block_label(block)),
            BaselineSkip::Gone => format!("{track_id} not_managed"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn baseline_block_label(block: &super::publish::undo::BaselineBlock) -> &'static str {
    match block {
        super::publish::undo::BaselineBlock::MissingBaseline => "missing_baseline",
        super::publish::undo::BaselineBlock::MissingRoot => "missing_root",
        super::publish::undo::BaselineBlock::FormatMismatch => "format_mismatch",
        super::publish::undo::BaselineBlock::CurrentChanged => "current_changed",
        super::publish::undo::BaselineBlock::IdentityChanged => "identity_changed",
        super::publish::undo::BaselineBlock::OriginalOccupied => "original_occupied",
    }
}
