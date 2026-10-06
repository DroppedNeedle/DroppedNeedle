//! Library management: sealed previews, apply, undo, baseline restore.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use super::adapters::FsSpaceProbe;
use super::clock::today_day;
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
fn policy_revision_u64(registry: &RootRegistry) -> u64 {
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

impl PublishCell {
    /// An unopened cell over the application database at `db_path`.
    pub(crate) fn new(db_path: &Path) -> Self {
        Self {
            db_path: db_path.to_owned(),
            cell: None,
        }
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
        let mut plan_items = Vec::with_capacity(items.len());
        let mut docs = BTreeMap::new();
        let mut files = Vec::with_capacity(items.len());
        for item in &items {
            if item.root_id.is_empty() || item.rel_path.is_empty() {
                return Err(ServiceError::InvalidInput {
                    message: "Preview items need a root and a relative path".to_owned(),
                });
            }
            super::publish::staging::check_managed_updates(&item.managed_updates)
                .map_err(publish_error)?;
            let track_id = self.scan_track(&item.root_id, &item.rel_path)?;
            let fingerprint = Self::live_fingerprint(&sandbox, &item.root_id, &item.rel_path)?;
            let format = Self::live_format(&item.rel_path)?;
            let identity = self.resolve_identity(album_id, &track_id)?;
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
            if !item.managed_updates.is_empty() {
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
                managed_updates: item.managed_updates.clone(),
                sidecars: Vec::new(),
                staged_bytes_estimate: size + STAGED_HEADROOM_BYTES,
            });
            docs.insert(
                track_id.clone(),
                Self::live_doc(&sandbox, &item.root_id, &item.rel_path)?,
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
        let mut fingerprints = BTreeMap::new();
        let mut identities = BTreeMap::new();
        let mut overrides = BTreeMap::new();
        for item in &bundle.items {
            fingerprints.insert(
                item.track_id.clone(),
                Self::live_fingerprint(sandbox, &item.source_root, &item.source_rel).map_err(
                    |_| ServiceError::Conflict {
                        message: format!("Track {} changed under the preview", item.track_id),
                    },
                )?,
            );
            let album_id = self.track_album(&item.track_id)?;
            identities.insert(
                item.track_id.clone(),
                self.resolve_identity(&album_id, &item.track_id)
                    .map_err(|_| ServiceError::Conflict {
                        message: format!("Identity for track {} changed", item.track_id),
                    })?,
            );
            overrides.insert(item.track_id.clone(), PINNED_OVERRIDE);
        }
        Ok(SealRecheck {
            fingerprints,
            identities,
            overrides,
            profile_revision: PINNED_REVISION,
            naming_revision: PINNED_REVISION,
            policy_revision: policy_revision_u64(&self.live_registry()),
            catalog_revision: bundle.catalog_revision,
            settings_revision: PINNED_REVISION,
            today_day: today_day(),
            token_hash: token_hash.to_owned(),
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
        use super::publish::publisher::Catalog as _;
        use super::publish::undo::{UndoInput, UndoLive, plan_undo};

        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.live_registry();
        let _guards = self.publish_guards(&registry);
        cell.refresh(&registry, &self.root_dirs)
            .map_err(publish_error)?;
        let open = cell.open().map_err(publish_error)?;
        let conn = open.publisher.connection();
        let source = super::publish::operations::load_operation(conn, bundle_id)
            .map_err(publish_error)?
            .ok_or(ServiceError::NotFound)?;
        let snapshots = SnapshotStore::new(conn)
            .bundle(bundle_id)
            .map_err(publish_error)?;
        if snapshots.is_empty() {
            return Err(ServiceError::Conflict {
                message: format!("Bundle {bundle_id} holds no undoable snapshots"),
            });
        }
        let journals = super::publish::journal::JournalStore::new(conn)
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
        if inputs.is_empty() {
            return Err(ServiceError::Conflict {
                message: format!("Bundle {bundle_id} holds no undoable snapshots"),
            });
        }
        let mut fingerprints = BTreeMap::new();
        let mut locations = BTreeMap::new();
        let mut identities = BTreeMap::new();
        let mut overrides = BTreeMap::new();
        for input in &inputs {
            let (root_id, rel_path, _, _) = SqliteCatalog
                .locate(conn, &input.track_id)
                .map_err(publish_error)?
                .ok_or_else(|| ServiceError::Conflict {
                    message: format!("Track {} is no longer managed", input.track_id),
                })?;
            fingerprints.insert(
                input.track_id.clone(),
                Self::live_fingerprint(&open.sandbox, &root_id, &rel_path)?,
            );
            locations.insert(input.track_id.clone(), (root_id, rel_path));
            let album_id = self.track_album(&input.track_id)?;
            identities.insert(
                input.track_id.clone(),
                self.resolve_identity(&album_id, &input.track_id)?,
            );
            overrides.insert(input.track_id.clone(), PINNED_OVERRIDE);
        }
        let sandbox = open.sandbox.clone();
        let live = UndoLive {
            fingerprints,
            locations,
            identities,
            overrides,
            today_day: today_day(),
        };
        let plan = plan_undo(bundle_id, &inputs, &live, &|root_id, rel_path| {
            match sandbox.resolve_no_symlink(root_id, rel_path) {
                Ok(path) => path.symlink_metadata().is_ok(),
                // An unresolvable restore target fails closed as
                // occupied: undo never writes through a symlink.
                Err(_) => true,
            }
        });
        if !plan.writable() {
            return Err(ServiceError::Conflict {
                message: format!("Undo blocked: {}", undo_blocks(&plan)),
            });
        }
        // Restore bundle: each eligible file republishes its exact
        // before document onto its prior path.
        let mut plan_items = Vec::new();
        let mut docs = BTreeMap::new();
        for item in &plan.eligible {
            let live_loc =
                live.locations
                    .get(&item.track_id)
                    .ok_or_else(|| ServiceError::Conflict {
                        message: format!("Track {} moved during undo", item.track_id),
                    })?;
            let fingerprint = Self::live_fingerprint(&sandbox, &live_loc.0, &live_loc.1)?;
            let format = Self::live_format(&live_loc.1)?;
            let album_id = self.track_album(&item.track_id)?;
            let identity = self.resolve_identity(&album_id, &item.track_id)?;
            let same_path = live_loc.0 == item.restore_root && live_loc.1 == item.restore_rel;
            let item_kind = if same_path {
                PlanKind::SamePath
            } else {
                PlanKind::Move
            };
            let mut capabilities = Vec::new();
            if !item.doc.managed.is_empty() {
                capabilities.push(Capability::Metadata);
            }
            if item_kind == PlanKind::Move {
                if live_loc.0 == item.restore_root {
                    capabilities.push(Capability::SameRootMove);
                } else {
                    capabilities.push(Capability::CrossRootMove);
                }
            }
            plan_items.push(PlanItem {
                track_id: item.track_id.clone(),
                source_root: live_loc.0.clone(),
                source_rel: live_loc.1.clone(),
                dest_root: item.restore_root.clone(),
                dest_rel: item.restore_rel.clone(),
                kind: item_kind,
                fingerprint,
                identity,
                override_revision: PINNED_OVERRIDE,
                capabilities,
                format,
                managed_updates: item.doc.managed.clone(),
                sidecars: Vec::new(),
                staged_bytes_estimate: 0,
            });
            let size = sandbox
                .resolve_no_symlink(&live_loc.0, &live_loc.1)
                .ok()
                .and_then(|path| std::fs::metadata(&path).ok())
                .map(|meta| meta.len())
                .unwrap_or(0);
            if let Some(last) = plan_items.last_mut() {
                last.staged_bytes_estimate = size + STAGED_HEADROOM_BYTES;
            }
            docs.insert(
                item.track_id.clone(),
                Self::live_doc(&sandbox, &live_loc.0, &live_loc.1)?,
            );
        }
        self.publish_restoration(&mut cell, plan_items, docs)
    }

    /// Restore tracks to their immutable first-management baselines.
    /// Missing baselines, missing roots, changed files, and occupied
    /// originals 409 with per-file reasons. The write guards span
    /// the refresh plus the restoration publish. Blocking: handlers
    /// run this off the async runtime.
    pub fn baseline_restore(&self, track_ids: &[String]) -> Result<AppliedBundle, ServiceError> {
        use super::publish::publisher::Catalog as _;
        use super::publish::undo::{BaselineInput, plan_baseline_restore};

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
        let conn = open.publisher.connection();
        let blobs = BlobStore::new(conn);
        let baselines = BaselineStore::new(conn);
        let mut inputs = Vec::new();
        let mut current_locs: HashMap<String, (String, String)> = HashMap::new();
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
            let (root_id, rel_path, _, _) = SqliteCatalog
                .locate(conn, track_id)
                .map_err(publish_error)?
                .ok_or_else(|| ServiceError::Conflict {
                    message: format!("Track {track_id} is not managed"),
                })?;
            let current = Self::live_fingerprint(&open.sandbox, &root_id, &rel_path)?;
            let format = Self::live_format(&rel_path)?;
            let album_id = self.track_album(track_id)?;
            let identity = self.resolve_identity(&album_id, track_id)?;
            current_locs.insert(track_id.clone(), (root_id, rel_path));
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
        // The original counts as occupied only when another file
        // holds it: a same-path restore whose subject still sits at
        // the original path is restoring tags in place, mirroring
        // undo's self-occupancy exemption.
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
        if !plan.writable() {
            return Err(ServiceError::Conflict {
                message: format!("Baseline restore blocked: {}", baseline_blocks(&plan)),
            });
        }
        let mut plan_items = Vec::new();
        let mut docs = BTreeMap::new();
        for (track_id, before) in &plan.eligible {
            let live_loc = current_locs
                .get(track_id)
                .ok_or_else(|| ServiceError::Conflict {
                    message: format!("Track {track_id} moved during restore"),
                })?;
            let fingerprint = Self::live_fingerprint(&sandbox, &live_loc.0, &live_loc.1)?;
            let format = Self::live_format(&live_loc.1)?;
            let album_id = self.track_album(track_id)?;
            let identity = self.resolve_identity(&album_id, track_id)?;
            let same_path = live_loc.0 == before.source_root && live_loc.1 == before.source_rel;
            let item_kind = if same_path {
                PlanKind::SamePath
            } else {
                PlanKind::Move
            };
            let mut capabilities = Vec::new();
            if !before.doc.managed.is_empty() {
                capabilities.push(Capability::Metadata);
            }
            if item_kind == PlanKind::Move {
                if live_loc.0 == before.source_root {
                    capabilities.push(Capability::SameRootMove);
                } else {
                    capabilities.push(Capability::CrossRootMove);
                }
            }
            let size = sandbox
                .resolve_no_symlink(&live_loc.0, &live_loc.1)
                .ok()
                .and_then(|path| std::fs::metadata(&path).ok())
                .map(|meta| meta.len())
                .unwrap_or(0);
            plan_items.push(PlanItem {
                track_id: track_id.clone(),
                source_root: live_loc.0.clone(),
                source_rel: live_loc.1.clone(),
                dest_root: before.source_root.clone(),
                dest_rel: before.source_rel.clone(),
                kind: item_kind,
                fingerprint,
                identity,
                override_revision: PINNED_OVERRIDE,
                capabilities,
                format,
                managed_updates: before.doc.managed.clone(),
                sidecars: Vec::new(),
                staged_bytes_estimate: size + STAGED_HEADROOM_BYTES,
            });
            docs.insert(
                track_id.clone(),
                Self::live_doc(&sandbox, &live_loc.0, &live_loc.1)?,
            );
        }
        self.publish_restoration(&mut cell, plan_items, docs)
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
fn undo_blocks(plan: &super::publish::undo::UndoPlan) -> String {
    plan.blocked
        .iter()
        .map(|(track_id, block)| format!("{track_id} {}", undo_block_label(block)))
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
fn baseline_blocks(plan: &super::publish::undo::BaselineRestorePlan) -> String {
    plan.blocked
        .iter()
        .map(|(track_id, block)| format!("{track_id} {}", baseline_block_label(block)))
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
