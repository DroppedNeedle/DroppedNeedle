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
use super::publish::snapshots::{BaselineStore, BlobStore, SnapshotStore, sha256_hex};
use super::publish::tags_seam::TagDocument;
use super::publish::{PublishError, Sandbox};
use super::scan::fs::WriteGuard;
use super::scan::models::EffectivePolicy;
use super::scan::roots::RootRegistry;
use super::scan::store::CatalogStore;
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
        let conn = crate::db::open_connection(&self.db_path)
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
        let conn = crate::db::open_connection(&self.db_path)
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
        let conn = crate::db::open_connection(&self.db_path)
            .map_err(|error| PublishError::Store(error.to_string()))?;
        JournalStore::new(&conn).active_bundles()
    }
}

impl PublishCell {
    /// An unopened cell over the application database at `db_path`.
    pub(crate) fn new(db_path: &Path) -> Self {
        Self {
            db_path: db_path.to_owned(),
            cell: None,
        }
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
        let mut conn = crate::db::open_connection(&self.db_path)
            .map_err(|error| PublishError::Store(error.to_string()))?;
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
            let mut conn = crate::db::open_connection(&self.db_path)
                .map_err(|error| PublishError::Store(error.to_string()))?;
            super::publish::recovery::reconcile(&mut conn, &sandbox, &SqliteCatalog)?
        };
        let publisher = Publisher::open(
            sandbox.clone(),
            &self.db_path,
            SqliteCatalog,
            FsSpaceProbe::new(space_roots.clone()),
            today_day(),
        )?;
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

    /// Accepted exact identity for one track (management needs an
    /// accepted exact MusicBrainz release plus a full track mapping).
    fn resolve_identity(
        &self,
        album_id: &str,
        track_id: &str,
    ) -> Result<ReleaseIdentity, ServiceError> {
        use super::identify::stores::IdentityStore;

        let missing = |message: String| ServiceError::Conflict { message };
        let album = self
            .identify_store
            .album_identity(album_id)
            .ok_or_else(|| {
                missing(format!(
                    "Album {album_id} has no accepted identity; identify it first"
                ))
            })?;
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
        use super::identify::stores::ReleaseStore as _;

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
        use super::identify::stores::IdentityStore as _;
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
            });
            docs.insert(
                track_id.clone(),
                Self::live_doc_for_write(
                    &sandbox,
                    &item.root_id,
                    &item.rel_path,
                    &managed_updates,
                )?,
            );
            files.push(PreviewFile {
                track_id,
                source: format!("{}/{}", item.root_id, item.rel_path),
                dest: format!("{dest_root}/{dest_rel}"),
                kind: match item_kind {
                    PlanKind::SamePath => "same_path".to_owned(),
                    PlanKind::Move => "move".to_owned(),
                },
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
            previews.insert(token_hash, PreviewEntry { sealed, docs });
        }
        Ok(PreviewSealed {
            token,
            expires_day,
            bundle_id: bundle.id,
            files,
        })
    }

    /// Apply a sealed preview exactly once. The write guards span
    /// the refresh plus the publish, so no scan interleaves with
    /// the commit. Blocking: handlers run this off the async runtime.
    pub fn apply_preview(&self, token: &str) -> Result<AppliedBundle, ServiceError> {
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
            .publish(&entry.sealed, &live, &entry.docs)
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
            .get::<crate::runtime_config::sections::LibraryManagement>()
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
    /// restoration publish. Blocking: handlers run this off the
    /// async runtime.
    pub fn undo_bundle(&self, bundle_id: &str) -> Result<AppliedBundle, ServiceError> {
        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.live_registry();
        let _guards = self.publish_guards(&registry);
        cell.refresh(&registry, &self.root_dirs)
            .map_err(publish_error)?;
        let open = cell.open().map_err(publish_error)?;
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
        self.publish_restoration(&mut cell, plan_items, docs)
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
            let (sidecars, left) = by_track
                .get(item.track_id.as_str())
                .map(|source| Self::undo_sidecars(&open.sandbox, &source.sidecars))
                .unwrap_or_default();
            if !sidecars.is_empty() {
                capabilities.push(Capability::Sidecars);
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
