//! Library management: sealed previews, apply, undo, baseline restore.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use super::adapters::FsSpaceProbe;
use super::clock::LibraryClock as _;
use super::publish::bin::BinSource;
use super::publish::journal::JournalStore;
use super::publish::paths::{ResolvedRoot, Root, left_out};
use super::publish::planner::{
    Capability, CollisionGate, DiskPreflight, FileFingerprint, PlanBundle, PlanItem, PlanKind,
    ReleaseIdentity, STAGED_HEADROOM_BYTES, SealRecheck, SealedPreview,
};
use super::publish::publisher::{PublishOutcome, Publisher, locate};
use super::publish::recovery::LeftoverReport;
use super::publish::snapshots::{BaselineStore, BlobStore, SnapshotStore, sha256_hex};
use super::publish::tags_seam::{TagDocument, TagUpdates};
use super::publish::{PlacementMode, PublishError, Sandbox};
use super::scan::models::EffectivePolicy;
use super::scan::roots::RootRegistry;
use super::service::ServiceError;
use super::tags::AudioFormat;
use super::wiring::RootDirs;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{CancelFlag, Db, DbError};
use droppedneedle_kernel::jobs::queue::JobQueue;

/// Preview seals live until the next unix day.
const PREVIEW_TTL_DAYS: i64 = 1;

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
    pub(crate) publisher: Publisher<FsSpaceProbe>,
    session: SessionRoots,
}

/// What a publish session is open over: the usable library roots and the
/// recycle bins, each by id and folder (`Err` with the cause while the
/// bins are unknown), and the real folder each root resolves to now. A
/// change to any of them reopens the session, so a library folder whose
/// link is pointed somewhere else is probed and given its bins afresh. A
/// caller works it out once and refreshes to that one answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionRoots {
    library: Vec<Root>,
    /// Excluded library folders: files in their included subfolders are
    /// in the catalog, and only a removal may take them out.
    remove_only: Vec<Root>,
    /// Each root's real folder by id; `None` while it does not resolve.
    resolved: Vec<(String, Option<PathBuf>)>,
    bins: Result<Vec<Root>, &'static str>,
}

impl SessionRoots {
    fn is_empty(&self) -> bool {
        self.library.is_empty() && self.remove_only.is_empty()
    }
}

/// Publish state. Empty until the first usable root is configured;
/// reopened (with reconciliation) whenever the usable roots or the
/// recycle bins change. One mutex serializes publishes against reopens.
/// No read or write closure takes that mutex, so holding it across a
/// database call cannot deadlock the lane.
pub struct PublishCell {
    db: Db,
    /// Wall time for the publisher and its recovery.
    clock: Arc<dyn Clock>,
    bins: BinSource,
    /// The jobs table a commit queues its follow-up work on.
    jobs: JobQueue,
    /// The undo folder each publisher keeps before-states in.
    blobs: BlobStore,
    pub(crate) cell: Option<OpenPublish>,
    placement: RootPlacement,
    /// The roots last left out of the session, so each change is logged
    /// once.
    left_out: std::sync::Mutex<Vec<String>>,
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
/// cleaned, compensated, or flagged for an administrator. Its reads never
/// wait on a publish holding the cell. `committed` and `unsettled` decide
/// a job's next step, so they wait for a connection instead of answering
/// busy; `list` is a status read on the request pool.
#[derive(Clone)]
pub struct HeldBundles {
    db: Db,
}

impl HeldBundles {
    pub(crate) fn new(db: Db) -> Self {
        Self { db }
    }

    /// True when the publisher committed `bundle_id`: its files changed
    /// even if whoever asked never heard back. Blocking.
    pub fn committed(&self, bundle_id: &str) -> Result<bool, PublishError> {
        use super::publish::JournalState;
        self.db
            .read_long("publish.held_committed", &CancelFlag::never(), |conn| {
                let journals = JournalStore::new(conn).bundle(bundle_id)?;
                if journals.iter().any(|journal| {
                    matches!(
                        journal.state,
                        JournalState::Committed
                            | JournalState::Cleaned
                            | JournalState::CleanupPending
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
                    if let Some(located) = locate(conn, track_id)?
                        && located.root_id == journal.dest_root
                        && located.rel_path == journal.dest_rel
                        && located.published_sha256 == journal.staged_sha256
                    {
                        return Ok(true);
                    }
                }
                Ok(false)
            })
    }

    /// True while `bundle_id` has journals neither committed nor settled:
    /// recovery has not decided yet whether it finishes or unwinds.
    /// Blocking.
    pub fn unsettled(&self, bundle_id: &str) -> Result<bool, PublishError> {
        use super::publish::JournalState;
        let journals =
            self.db
                .read_long("publish.held_unsettled", &CancelFlag::never(), |conn| {
                    JournalStore::new(conn).bundle(bundle_id)
                })?;
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
        self.db.read_blocking("publish.held_list", |conn| {
            JournalStore::new(conn).active_bundles()
        })
    }
}

/// Hidden files under the library roots that no journal row names, as the
/// last pass found them. Nothing here removes or adopts one.
#[derive(Clone)]
pub struct LeftoverFiles {
    db: Db,
    clock: Arc<dyn Clock>,
    report: std::sync::Arc<std::sync::Mutex<Option<LeftoverReport>>>,
}

impl LeftoverFiles {
    pub(crate) fn new(db: Db, clock: Arc<dyn Clock>) -> Self {
        Self {
            db,
            clock,
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
        let found = super::publish::recovery::find_leftovers(
            &self.db,
            &PublishCell::root_dirs(registry),
            self.clock.now_system(),
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
    /// An unopened cell over the application database, taking its
    /// recycle bins from `bins` and queueing a commit's follow-up work on
    /// `jobs`, with before-states kept in `blobs`.
    pub(crate) fn new(
        db: Db,
        bins: BinSource,
        jobs: JobQueue,
        clock: Arc<dyn Clock>,
        blobs: BlobStore,
    ) -> Self {
        Self {
            db,
            clock,
            bins,
            jobs,
            blobs,
            cell: None,
            placement: RootPlacement::default(),
            left_out: std::sync::Mutex::default(),
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
        if !open.session.library.iter().any(|root| root.id == root_id) {
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
    pub(crate) fn has_held(&self) -> Result<bool, PublishError> {
        if self.cell.is_none() {
            return Ok(false);
        }
        self.db
            .read_long("publish.held", &CancelFlag::never(), |conn| {
                Ok(conn
                    .prepare_cached(super::publish::sql::ANY_ACTIVE.text())?
                    .query_row([], |row| row.get(0))?)
            })
    }

    /// Reconcile again under the open sandbox: the maintenance tick's
    /// retry for held bundles. No-op while no usable root exists.
    pub(crate) fn retry_held(
        &mut self,
    ) -> Result<Vec<super::publish::recovery::BundleRecovery>, PublishError> {
        let Some(open) = self.cell.as_ref() else {
            return Ok(Vec::new());
        };
        super::publish::recovery::reconcile(&self.db, &open.sandbox, &self.jobs, &*self.clock)
    }

    fn root_dirs(registry: &RootRegistry) -> Vec<Root> {
        registry
            .roots()
            .iter()
            .filter(|root| root.policy != EffectivePolicy::Excluded)
            .map(|root| Root::library(root.id.clone(), root.path.clone()))
            .collect()
    }

    fn remove_only_dirs(registry: &RootRegistry) -> Vec<Root> {
        registry
            .roots()
            .iter()
            .filter(|root| root.policy == EffectivePolicy::Excluded)
            .map(|root| Root::remove_only(root.id.clone(), root.path.clone()))
            .collect()
    }

    /// The roots and bins a session would open over now. The roots and
    /// bins are resolved here, once per call. A root [`left_out`] names
    /// takes no managed writes.
    pub(crate) fn session(&self, registry: &RootRegistry) -> SessionRoots {
        let resolved: Vec<ResolvedRoot> = registry
            .roots()
            .iter()
            .map(|root| ResolvedRoot::of(&root.id, &root.path))
            .collect();
        let left_out = left_out(&resolved);
        self.log_left_out(&left_out);
        let kept = |root: &Root| !left_out.contains(&root.id);
        SessionRoots {
            library: Self::root_dirs(registry).into_iter().filter(kept).collect(),
            remove_only: Self::remove_only_dirs(registry)
                .into_iter()
                .filter(kept)
                .collect(),
            resolved: resolved
                .into_iter()
                .filter(|root| !left_out.contains(&root.id))
                .map(|root| (root.id, root.real))
                .collect(),
            bins: self.bins.current(registry).map(|choice| choice.roots),
        }
    }

    fn log_left_out(&self, ids: &[String]) {
        let mut logged = self
            .left_out
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if logged.as_slice() == ids {
            return;
        }
        if !ids.is_empty() {
            tracing::warn!(
                roots = ids.join(", "),
                "library folders reached through a link into another library folder take no file changes"
            );
        }
        *logged = ids.to_vec();
    }

    /// [`Self::refresh_to`] the session `registry` calls for now.
    pub(crate) fn refresh(
        &mut self,
        registry: &RootRegistry,
        space_roots: &RootDirs,
    ) -> Result<Vec<super::publish::recovery::BundleRecovery>, PublishError> {
        let session = self.session(registry);
        self.refresh_to(session, space_roots)
    }

    /// Reconcile and (re)open when `session` differs from the open one:
    /// the usable roots or the bins changed, by id or by folder. Returns
    /// the bundle recoveries from the reconcile, if any.
    pub(crate) fn refresh_to(
        &mut self,
        session: SessionRoots,
        space_roots: &RootDirs,
    ) -> Result<Vec<super::publish::recovery::BundleRecovery>, PublishError> {
        if session.is_empty() {
            self.cell = None;
            self.placement.follow(None);
            return Ok(Vec::new());
        }
        if let Some(open) = self.cell.as_ref()
            && open.session == session
        {
            return Ok(Vec::new());
        }
        let mut roots = session.library.clone();
        roots.extend(session.remove_only.iter().cloned());
        roots.extend(session.bins.iter().flatten().cloned());
        let mut sandbox = Sandbox::new(roots)?;
        if let Err(cause) = session.bins {
            sandbox = sandbox.with_bins_unknown(cause);
        }
        // Recovery runs before the new session takes publishes. The
        // journal lives in the application database, so root order never
        // decides where it is.
        let recoveries =
            super::publish::recovery::reconcile(&self.db, &sandbox, &self.jobs, &*self.clock)?;
        let publisher = Publisher::new(
            sandbox.clone(),
            self.db.clone(),
            FsSpaceProbe::new(space_roots.clone()),
            self.jobs.clone(),
            Arc::clone(&self.clock),
            self.blobs.clone(),
        );
        self.placement.follow(Some(sandbox.clone()));
        self.cell = Some(OpenPublish {
            sandbox,
            publisher,
            session,
        });
        Ok(recoveries)
    }

    pub(crate) fn open(&mut self) -> Result<&mut OpenPublish, PublishError> {
        self.cell
            .as_mut()
            .ok_or_else(|| PublishError::Journal("no usable library root is configured".into()))
    }

    /// Library root ids a refresh would open under.
    pub(crate) fn pending_ids(registry: &RootRegistry) -> Vec<String> {
        Self::root_dirs(registry)
            .iter()
            .map(|root| root.id.clone())
            .collect()
    }

    /// True when refreshing to `session` would reopen (and reconcile):
    /// exactly the case where reconcile can write.
    pub(crate) fn needs_refresh(&self, session: &SessionRoots) -> bool {
        if session.is_empty() {
            return false;
        }
        self.cell.as_ref().map(|open| &open.session) != Some(session)
    }
}

/// A file a plugin added: where it is, and the bundle that added it.
#[derive(Debug, Clone)]
pub struct AddedPluginFile {
    pub path: PathBuf,
    pub bundle_id: String,
}

/// Why a plugin's file was not added.
#[derive(Debug)]
pub enum PluginFileRefusal {
    /// The library stayed busy until the deadline. Nothing was added.
    Busy,
    /// Whoever asked stopped waiting. Nothing was added.
    Cancelled,
    /// No library folder is set up.
    NoFolder,
    /// The first library folder takes no plugin files; the sentence says
    /// why.
    FolderUnusable(&'static str),
    /// Another file has the name.
    Taken,
    /// The write stopped partway; it finishes or is undone by itself.
    Pending,
    /// The publish refused or failed. Nothing was added.
    Publish(PublishError),
}

/// How long a plugin write pauses between tries for a busy library.
const PLUGIN_RETRY_PAUSE: std::time::Duration = std::time::Duration::from_millis(100);

impl super::wiring::LibraryCore {
    /// Add `bytes` as the new file `relative_path` under the first library
    /// folder for plugin `plugin`, as one publisher bundle with one add
    /// row. It waits for the publish lock and then the folder until
    /// `deadline`, never waiting on a folder while it holds the lock, and
    /// stops once `cancelled` is set; nothing is recorded until both are
    /// held and the write is still wanted, and the file is never placed
    /// past the deadline or once `cancelled` is set. `on_record` runs in
    /// the transaction that records the change. Blocking.
    pub fn add_plugin_file(
        &self,
        plugin: &str,
        relative_path: &str,
        bytes: Vec<u8>,
        deadline: std::time::Instant,
        cancelled: &std::sync::Arc<std::sync::atomic::AtomicBool>,
        on_record: super::publish::publisher::OnRecord<'_>,
    ) -> Result<AddedPluginFile, PluginFileRefusal> {
        use std::sync::atomic::Ordering;
        let library = self
            .library
            .get()
            .map_err(|_| PluginFileRefusal::FolderUnusable("the library settings can't be read"))?;
        let first = library
            .library_roots
            .iter()
            .find(|root| !root.id.is_empty() && !root.path.trim().is_empty())
            .map(|root| root.id.clone())
            .ok_or(PluginFileRefusal::NoFolder)?;
        let mut bytes = Some(bytes);
        loop {
            if cancelled.load(Ordering::SeqCst) {
                return Err(PluginFileRefusal::Cancelled);
            }
            let terms = || super::publish::publisher::AddTerms {
                deadline,
                cancelled: cancelled.clone(),
                on_record,
            };
            if let Some(done) =
                self.try_add_plugin_file(plugin, relative_path, &first, &mut bytes, terms)
            {
                return done;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(PluginFileRefusal::Busy);
            }
            std::thread::sleep(PLUGIN_RETRY_PAUSE.min(deadline - now));
        }
    }

    /// One try of [`Self::add_plugin_file`]: `None` while the lock is
    /// taken.
    fn try_add_plugin_file<'a>(
        &self,
        plugin: &str,
        relative_path: &str,
        first: &str,
        bytes: &mut Option<Vec<u8>>,
        terms: impl FnOnce() -> super::publish::publisher::AddTerms<'a>,
    ) -> Option<Result<AddedPluginFile, PluginFileRefusal>> {
        let mut cell = match self.publish.try_lock() {
            Ok(cell) => cell,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        let registry = self.refresh_registry();
        // The registry leaves out a root it cannot use; the plugin's file
        // never goes to the one after it instead.
        match registry.roots().first() {
            Some(root) if root.id == first => {
                if root.policy == EffectivePolicy::Excluded {
                    return Some(Err(PluginFileRefusal::FolderUnusable(
                        "the first library folder is excluded from the library",
                    )));
                }
            }
            _ => {
                return Some(Err(PluginFileRefusal::FolderUnusable(
                    "the first library folder isn't usable; check it in the library settings",
                )));
            }
        }
        let session = cell.session(&registry);
        let terms = terms();
        if terms.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            return Some(Err(PluginFileRefusal::Cancelled));
        }
        let bytes = bytes.take()?;
        Some(self.publish_plugin_file(
            &mut cell,
            session,
            &registry,
            (plugin, relative_path, first),
            bytes,
            terms,
        ))
    }

    /// Publish the plugin's file under the cell the caller holds. `file` is the plugin, the path and the root id.
    fn publish_plugin_file(
        &self,
        cell: &mut PublishCell,
        session: SessionRoots,
        registry: &RootRegistry,
        (plugin, relative_path, first): (&str, &str, &str),
        bytes: Vec<u8>,
        terms: super::publish::publisher::AddTerms<'_>,
    ) -> Result<AddedPluginFile, PluginFileRefusal> {
        use super::publish::add::{Added, AddedFile, Origin};
        cell.refresh_to(session, &self.root_dirs)
            .map_err(PluginFileRefusal::Publish)?;
        let unusable = PluginFileRefusal::FolderUnusable(
            "the library folder is reached through a link into another library folder",
        );
        let Ok(open) = cell.open() else {
            return Err(unusable);
        };
        if open.sandbox.root_dir(first).is_err() {
            return Err(unusable);
        }
        let path = open
            .sandbox
            .resolve(first, relative_path)
            .map_err(PluginFileRefusal::Publish)?;
        let policy_revision = policy_revision_u64(registry);
        let bundle = PlanBundle {
            id: self.ids.new_id(),
            items: Vec::new(),
            profile_revision: PINNED_REVISION,
            naming_revision: PINNED_REVISION,
            policy_revision,
            removal: None,
            put_back: None,
            added: Some(Added {
                origin: Origin::Plugin(plugin.to_owned()),
                file: AddedFile {
                    root_id: first.to_owned(),
                    relative_path: relative_path.to_owned(),
                    sha256: sha256_hex(&bytes),
                    size: bytes.len() as u64,
                },
                undoes: None,
            }),
        };
        let (sealed, live) = seal_pinned(
            bundle,
            &self.ids.new_id(),
            policy_revision,
            self.clock.today_day(),
        );
        let bundle_id = sealed.bundle.id.clone();
        let (deadline, cancelled) = (terms.deadline, terms.cancelled.clone());
        let Err(error) = open
            .publisher
            .publish_add_within(&sealed, &live, bytes, terms)
        else {
            self.rescan(first);
            return Ok(AddedPluginFile { path, bundle_id });
        };
        let committed = self
            .held_bundles
            .committed(&bundle_id)
            .unwrap_or_else(|read| {
                tracing::warn!(%read, bundle_id, "could not tell whether a failed plugin write committed");
                false
            });
        if committed {
            tracing::warn!(%error, bundle_id, "plugin write committed; recovery finishes the rest");
            self.rescan(first);
            return Ok(AddedPluginFile { path, bundle_id });
        }
        let unsettled = self.held_bundles.unsettled(&bundle_id).unwrap_or(true);
        let out_of_time = matches!(error, PublishError::TimedOut(_));
        Err(if unsettled {
            tracing::warn!(%error, bundle_id, "plugin write stopped partway; recovery settles it");
            PluginFileRefusal::Pending
        } else if out_of_time && cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            PluginFileRefusal::Cancelled
        } else if out_of_time && std::time::Instant::now() >= deadline {
            PluginFileRefusal::Busy
        } else if matches!(error, PublishError::Collision(_)) {
            PluginFileRefusal::Taken
        } else {
            PluginFileRefusal::Publish(error)
        })
    }

    /// Have the scan walk library folder `root_id` again soon, so a file
    /// added or taken away there shows in the library.
    fn rescan(&self, root_id: &str) {
        self.dirty.mark(root_id);
        self.wakeups.notify("scan");
    }
}

/// A bundle sealed with the pinned revisions, for a publish no person
/// previewed, with the recheck that matches it.
fn seal_pinned(
    bundle: PlanBundle,
    token: &str,
    policy_revision: u64,
    today_day: i64,
) -> (SealedPreview, SealRecheck) {
    let token_hash = sha256_hex(token.as_bytes());
    let sealed = SealedPreview::seal(
        bundle,
        token_hash.clone(),
        PINNED_REVISION,
        today_day + PREVIEW_TTL_DAYS,
    );
    let live = SealRecheck {
        fingerprints: BTreeMap::new(),
        identities: BTreeMap::new(),
        overrides: BTreeMap::new(),
        profile_revision: PINNED_REVISION,
        naming_revision: PINNED_REVISION,
        policy_revision,
        settings_revision: PINNED_REVISION,
        today_day,
        token_hash,
    };
    (sealed, live)
}

/// How a removal takes the publish lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemovalEntry {
    /// A person is waiting: a lock another write holds answers busy at
    /// once.
    Now,
    /// Background work: it waits its turn, as other background publishes
    /// do, so busy never fails it.
    Wait,
}

/// What a removal took out of the library.
#[derive(Debug, Clone, Default)]
pub(crate) struct RemovalDone {
    pub track_ids: Vec<String>,
    /// Each file as (where it was, where it is now): its bin entry, or its
    /// hidden name while its move into the bin waits.
    pub recycled: Vec<(PathBuf, PathBuf)>,
    /// The removal's id, when it wrote an undo record Put back names.
    pub removal_id: Option<String>,
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
    /// The saved copy of its earlier tags is missing or damaged.
    CopyMissing,
}

/// One file undo would restore, with its tags as they are now.
#[derive(Debug, Clone)]
pub(crate) struct UndoFile {
    pub plan: PlanItem,
    /// The file's tags now, by stored name.
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
    /// The saved copy of its original tags is missing or damaged.
    CopyMissing,
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
    /// Managed-field updates, as the request names them.
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
    /// Write in place or move.
    pub kind: PlanKind,
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
    /// Whether cleanup finished.
    pub outcome: PublishOutcome,
    /// Applied files.
    pub files: Vec<AppliedFile>,
}

/// Map a publisher failure onto HTTP. Collisions, stale state, and
/// capability blocks are 409s (valid input against the wrong
/// state); unsafe paths are 400s; store and I/O faults are 5xx.
pub(crate) fn publish_error(error: PublishError) -> ServiceError {
    match error {
        PublishError::Collision(detail)
        | PublishError::Validation(detail)
        | PublishError::Snapshot(detail)
        | PublishError::Journal(detail)
        | PublishError::Catalog(detail)
        | PublishError::Capability(detail)
        | PublishError::NoSafePlacement(detail)
        | PublishError::NotLinkable(detail)
        | PublishError::Cleanup(detail) => ServiceError::Conflict {
            message: detail.message,
        },
        PublishError::Space {
            root_id,
            needed,
            free,
        } => ServiceError::Conflict {
            message: format!("root {root_id} needs {needed} bytes but has {free} free"),
        },
        PublishError::UnsafePath(detail) | PublishError::Archive(detail) => {
            ServiceError::InvalidInput {
                message: detail.message,
            }
        }
        PublishError::FolderMissing => ServiceError::Conflict {
            message: "The library folder isn't there. Check that its drive or share is mounted."
                .to_owned(),
        },
        PublishError::Interrupted(cause) => ServiceError::Busy { cause },
        PublishError::Store(message) | PublishError::TimedOut(message) => {
            ServiceError::internal(&message)
        }
        PublishError::Io(detail) => ServiceError::internal(&detail.message),
        PublishError::InjectedCrash(point) => ServiceError::internal(&point),
    }
}

/// Managed writes: sealed previews and their apply, undo, baseline
/// restore, removals and put-backs, and the publish session they all run
/// under, plus its upkeep (snapshot purge, recycle-bin expiry, startup
/// recovery).
#[derive(Clone)]
pub struct Publishing {
    pub(crate) db: Db,
    /// The publish cell, the roots and the recycle bins.
    pub core: super::wiring::LibraryCore,
    pub(crate) identify_store: Arc<super::identify::sqlite::SqliteIdentifyStore>,
    pub(crate) scan_store: Arc<super::scan::sqlite_store::SqliteScanStore>,
    pub(crate) settings: super::settings::LibrarySettings,
    /// Sealed previews awaiting apply, keyed by token hash.
    pub(crate) previews: Arc<std::sync::Mutex<HashMap<String, PreviewEntry>>>,
    /// What recycle-bin expiry remembers between passes.
    pub(crate) expiry: Arc<super::publish::expiry::ExpiryMemory>,
    /// The jobs table: a removed album's clean-up is queued here in the
    /// removal's own commit, and emptying the recycle bin is a job.
    pub(crate) jobs: JobQueue,
}

impl Publishing {
    /// Reconcile the publish journal at startup: resume or compensate
    /// every unsettled bundle, never half-apply, and sweep what an
    /// interrupted import left once nothing waits on it. Answers each
    /// bundle's recovery. Never fails: a journal that cannot be
    /// reconciled is logged and managed writes stay closed until it
    /// clears.
    pub(crate) async fn recover(&self) -> Vec<(String, String)> {
        // Recovery waits on the database and holds the cell's mutex, so it
        // runs off the async runtime.
        let core = self.core.clone();
        let recovered = tokio::task::spawn_blocking(move || {
            let registry = core.live_registry();
            let mut cell = core
                .publish
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let recoveries = cell
                .refresh(&registry, &core.root_dirs)
                .unwrap_or_else(|error| {
                    tracing::error!(%error, "publish recovery did not run; managed writes stay closed");
                    Vec::new()
                });
            // Copies an interrupted download import left in the hidden
            // import folders, once no bundle still waits on them.
            let roots: Vec<PathBuf> = registry
                .roots()
                .iter()
                .map(|root| root.path.clone())
                .collect();
            let held = cell.has_held().unwrap_or_else(|error| {
                tracing::warn!(%error, "publish journal unreadable; import leftovers kept");
                true
            });
            let settled = cell.cell.is_some() && !held;
            super::publish::import_staging::sweep_import_leftovers(&roots, settled);
            recoveries
        })
        .await;
        recovered
            .unwrap_or_else(|error| {
                tracing::error!(%error, "publish recovery panicked; managed writes stay closed");
                Vec::new()
            })
            .into_iter()
            .map(|recovery| (recovery.bundle_id, format!("{:?}", recovery.action)))
            .collect()
    }

    /// One page of the files plugin `plugin` added, newest first. Blocking.
    pub fn plugin_files(
        &self,
        plugin: &str,
        before: Option<(i64, String)>,
        limit: u32,
    ) -> Result<Vec<super::publish::add::PluginFile>, ServiceError> {
        self.db
            .read_blocking("publish.plugin_files", |conn| {
                super::publish::add::plugin_files(
                    conn,
                    plugin,
                    before.as_ref().map(|(at, id)| (*at, id.as_str())),
                    limit,
                )
            })
            .map_err(publish_error)
    }

    /// Unsettled publish bundles in the journal. Blocking.
    pub fn held_publish_bundles(&self) -> Result<Vec<String>, ServiceError> {
        self.core.held_bundles.list().map_err(publish_error)
    }

    /// Drop expired preview seals.
    pub(crate) fn sweep_previews(&self) {
        let today = self.core.clock.today_day();
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
            .map_err(crate::scans::store_error)?
            .ok_or_else(|| ServiceError::Conflict {
                message: format!("Track {track_id} is not in the catalog"),
            })
    }

    /// File management waits for an album whose match is a best guess
    /// (or matches nothing) until someone confirms it, unless the edition
    /// preferences let it manage unconfirmed matches.
    fn unconfirmed_gate(&self, album_id: &str) -> Result<(), ServiceError> {
        let allowed = self.settings.editions.manage_unconfirmed();
        let reason = self
            .db
            .read_blocking("management.match_gate", |conn| {
                Ok::<_, DbError>(super::management::planner::match_gate(
                    conn, album_id, allowed,
                )?)
            })
            .map_err(|error| {
                tracing::error!(%error, album = album_id, "match state unreadable; management refused");
                gate_error(error)
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
            .db
            .read_blocking("management.remap_gate", |conn| {
                Ok::<_, DbError>(super::management::planner::remap_gate(conn, album_id)?)
            })
            .map_err(|error| {
                tracing::error!(%error, album = album_id, "edition remap state unreadable; management refused");
                gate_error(error)
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
            .album_identity(album_id)?
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
                .db
                .read_blocking("management.custom_edition", |conn| {
                    Ok::<_, DbError>(super::management::custom::active(conn, album_id)?)
                })
                .map_err(gate_error)?
                .ok_or_else(|| {
                    missing(format!(
                        "Album {album_id} has no accepted exact release; approve an exact edition"
                    ))
                })?;
            let track = self.identify_store.track_identity(track_id)?;
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
            .track_identity(track_id)?
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
            .map_err(crate::scans::store_error)?
            .ok_or_else(|| ServiceError::Conflict {
                message: format!("{root_id}/{rel_path} is not indexed; scan the root first"),
            })
    }

    /// The accepted identity a track's album holds now, or `None` when the
    /// track has left the catalog or its album has no exact identity. A
    /// store that could not be read is an error, never "gone".
    fn live_identity(&self, track_id: &str) -> Result<Option<ReleaseIdentity>, ServiceError> {
        let gone = |error: &ServiceError| {
            matches!(
                error,
                ServiceError::NotFound
                    | ServiceError::Conflict { .. }
                    | ServiceError::InvalidInput { .. }
            )
        };
        let album_id = match self.track_album(track_id) {
            Ok(album_id) => album_id,
            Err(error) if gone(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        match self.resolve_identity(&album_id, track_id) {
            Ok(identity) => Ok(Some(identity)),
            Err(error) if gone(&error) => Ok(None),
            Err(error) => Err(error),
        }
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
        let hashed =
            super::publish::paths::hash_regular_file(&path).map_err(|error| match error {
                PublishError::Validation(detail) => ServiceError::Conflict {
                    message: detail.message,
                },
                other => publish_error(other),
            })?;
        Ok(FileFingerprint {
            size: hashed.size,
            sha256: hashed.sha256,
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
        managed_updates: &TagUpdates,
    ) -> Result<TagDocument, ServiceError> {
        let path = sandbox
            .resolve_no_symlink(root_id, rel_path)
            .map_err(publish_error)?;
        super::publish::staging::document_for_write(&path, managed_updates).map_err(publish_error)
    }

    /// The tags a retag writes: the identified release's full tag set
    /// (when its document is on file and the format is writable), with
    /// the caller's explicit updates on top. A document that cannot be
    /// read fails the plan rather than writing fewer tags.
    fn with_release_tags(
        &self,
        identity: &ReleaseIdentity,
        rel_path: &str,
        explicit: &TagUpdates,
    ) -> Result<TagUpdates, ServiceError> {
        let writable = super::tags::format_for_path(PathBuf::from(rel_path).as_path())
            .is_ok_and(super::tags::save::writable);
        if !writable {
            return Ok(explicit.clone());
        }
        let Some(release) = self.identify_store.release(&identity.release_mbid, None)? else {
            tracing::info!(
                release = identity.release_mbid,
                "no stored release document; retag writes only the requested fields"
            );
            return Ok(explicit.clone());
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
            return Ok(explicit.clone());
        };
        let mut tags = super::tags::picard::release_tags(&release, track);
        // A value the writer would refuse is dropped here, at preview,
        // rather than failing the publish.
        tags.retain(|name, values| {
            super::tags::TagField::from_name(name).is_some_and(|field| {
                super::tags::save::accepts(&super::tags::TagEdit::new(field, values.clone()))
            })
        });
        let mut tags = TagUpdates::from_fields(tags).map_err(publish_error)?;
        tags.extend(explicit.clone());
        Ok(tags)
    }

    /// Lowercase container format for one rel path.
    fn live_format(rel_path: &str) -> Result<AudioFormat, ServiceError> {
        super::tags::format_for_path(PathBuf::from(rel_path).as_path()).map_err(|_| {
            ServiceError::InvalidInput {
                message: format!("Unsupported audio format for {rel_path}"),
            }
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
        let registry = self.core.live_registry();
        let sandbox = {
            let mut cell = self
                .core
                .publish
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let session = cell.session(&registry);
            cell.refresh_to(session, &self.core.root_dirs)
                .map_err(publish_error)?;
            cell.open().map_err(publish_error)?.sandbox.clone()
        };
        // Accepted album identity first: without an exact release the
        // whole bundle blocks before any file is touched.
        let album = self
            .identify_store
            .album_identity(album_id)?
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
            let requested = super::publish::staging::from_request(&item.managed_updates)
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
                    self.with_release_tags(&identity, &item.rel_path, &requested)?
                }
                PlanKind::Move => requested,
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
                sealed: None,
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
                PlanKind::SamePath => tag_changes(&doc.managed.named(), &managed_updates.named()),
                PlanKind::Move => Vec::new(),
            };
            docs.insert(track_id.clone(), doc);
            files.push(PreviewFile {
                track_id,
                source: format!("{}/{}", item.root_id, item.rel_path),
                dest: format!("{dest_root}/{dest_rel}"),
                kind: item_kind,
                changes,
            });
        }
        let mut bundle = PlanBundle {
            id: self.core.ids.new_id(),
            items: plan_items,
            profile_revision: PINNED_REVISION,
            naming_revision: PINNED_REVISION,
            policy_revision: policy_revision_u64(&registry),
            put_back: None,
            added: None,
            removal: None,
        };
        // The tracks' rows as previewed: Apply is refused if they move.
        super::publish::planner::seal_rows(&self.db, &mut bundle).map_err(publish_error)?;
        CollisionGate::check_bundle(&sandbox, &bundle).map_err(publish_error)?;
        DiskPreflight::check(&bundle, &FsSpaceProbe::new(self.core.root_dirs.clone()))
            .map_err(publish_error)?;
        let token = self.core.ids.new_id();
        let token_hash = sha256_hex(token.as_bytes());
        let expires_day = self.core.clock.today_day() + PREVIEW_TTL_DAYS;
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

    /// Apply a sealed preview exactly once, under the publish lock.
    /// `applied_by` is recorded so that account may undo
    /// it. Blocking: handlers run this off the async runtime.
    pub fn apply_preview(
        &self,
        token: &str,
        applied_by: Option<&str>,
    ) -> Result<AppliedBundle, ServiceError> {
        let retention = self.undo_retention_days().map_err(publish_error)?;
        let token_hash = sha256_hex(token.as_bytes());
        let entry = {
            let mut previews = self
                .previews
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            previews.remove(&token_hash).ok_or(ServiceError::NotFound)?
        };
        if self.core.clock.today_day() >= entry.sealed.expires_day {
            return Err(ServiceError::Conflict {
                message: "Preview expired; plan it again".to_owned(),
            });
        }
        let mut cell = self
            .core
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.core.live_registry();
        cell.refresh(&registry, &self.core.root_dirs)
            .map_err(publish_error)?;
        let open = cell.open().map_err(publish_error)?;
        let live = self.seal_live(&open.sandbox, &entry.sealed.bundle, &token_hash)?;
        open.publisher.set_undo_retention_days(retention);
        let outcome = open
            .publisher
            .publish_as(&entry.sealed, &live, &entry.docs, applied_by)
            .map_err(publish_error)?;
        Ok(AppliedBundle {
            bundle_id: entry.sealed.bundle.id.clone(),
            outcome,
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
            PublishError::Validation(detail) => ServiceError::Conflict {
                message: detail.message,
            },
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
                PublishError::Validation(
                    format!("Track {} changed under the preview", item.track_id).into(),
                )
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
                        PublishError::Validation(
                            format!("Identity for track {} changed", item.track_id).into(),
                        )
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
            policy_revision: policy_revision_u64(&self.core.live_registry()),
            settings_revision: pins.settings,
            today_day: self.core.clock.today_day(),
            token_hash: token_hash.to_owned(),
        })
    }

    /// How long undo keeps before-states, read from the settings per call.
    /// A section that does not read refuses the publish: a default window
    /// could expire before-states the admin chose to keep.
    fn undo_retention_days(&self) -> Result<i64, PublishError> {
        self.settings
            .management
            .read()
            .map(|settings| settings.undo_retention_days)
            .ok_or_else(|| {
                PublishError::Store("library management settings cannot be read".to_owned())
            })
    }

    /// Arm (or clear) a crash point on the open publisher, so a test can
    /// stop a publish part way as a killed process would. Tests only.
    #[cfg(any(test, feature = "test-support"))]
    pub fn arm_publish_crash(&self, point: Option<super::publish::CrashPoint>) {
        let registry = self.core.live_registry();
        let mut cell = self
            .core
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cell.refresh(&registry, &self.core.root_dirs).is_ok()
            && let Ok(open) = cell.open()
        {
            open.publisher.set_crash_point(point);
        }
    }

    /// Make placement on the open publisher behave like a filesystem
    /// without one of its steps. Tests only.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_publish_seams(&self, seams: super::publish::place::Seams) {
        let registry = self.core.live_registry();
        let mut cell = self
            .core
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cell.refresh(&registry, &self.core.root_dirs).is_ok()
            && let Ok(open) = cell.open()
        {
            open.publisher.set_place_seams(seams);
        }
    }

    /// Run `before_place` on the open publisher just before each file is
    /// placed, and `before_set_aside` just before each file is set aside,
    /// so a test can put something in the way. Tests only.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_publish_hooks(
        &self,
        before_place: Option<super::publish::publisher::BeforePlace>,
        before_set_aside: Option<super::publish::publisher::BeforePlace>,
    ) {
        let registry = self.core.live_registry();
        let mut cell = self
            .core
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cell.refresh(&registry, &self.core.root_dirs).is_ok()
            && let Ok(open) = cell.open()
        {
            open.publisher.set_before_place(before_place);
            open.publisher.set_before_set_aside(before_set_aside);
        }
    }

    /// Run publish recovery now: reopen (and reconcile) when the roots
    /// changed, and retry any held bundle, so a bundle a crash left half
    /// way is finished or unwound before anyone asks how it ended.
    /// Blocking.
    pub(crate) fn settle_publishes(&self) -> Result<(), PublishError> {
        let mut cell = self
            .core
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.core.live_registry();
        let session = cell.session(&registry);
        let reopen = cell.needs_refresh(&session);
        let retry = !reopen && cell.has_held()?;
        cell.refresh_to(session, &self.core.root_dirs)?;
        if retry {
            cell.retry_held()?;
        }
        Ok(())
    }

    /// The sandbox over the usable roots, opening the publish cell when
    /// the root set changed. Blocking.
    pub(crate) fn publish_sandbox(&self) -> Result<Sandbox, PublishError> {
        let registry = self.core.live_registry();
        let mut cell = self
            .core
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = cell.session(&registry);
        cell.refresh_to(session, &self.core.root_dirs)?;
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
        // The roots are read under the lock, so a probe waiting out a
        // write runs against the folders saved when it gets its turn.
        let mut cell = self
            .core
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.core.live_registry();
        let session = cell.session(&registry);
        cell.refresh_to(session, &self.core.root_dirs)?;
        cell.recheck_placement(root_id)
    }

    /// Take `targets` out of the library and their files to the recycle
    /// bin, as one removal bundle: planned, sealed and published under the
    /// publish lock. With [`RemovalEntry::Now`] a lock another write holds
    /// answers busy at once; with
    /// [`RemovalEntry::Wait`] the removal waits its turn. A removal whose
    /// commit landed answers what it removed even when a later step failed:
    /// recovery finishes it. `album_removed` queues the album's clean-up in
    /// the removal's commit. Blocking.
    pub(crate) fn publish_removal(
        &self,
        targets: &[super::publish::remove::Target],
        reason_code: &str,
        actor: Option<&str>,
        undo: Option<super::publish::planner::UndoRecord>,
        album_removed: Option<super::publish::planner::AlbumRemoved>,
        entry: RemovalEntry,
    ) -> Result<RemovalDone, super::publish::remove::Refusal> {
        use super::publish::JournalState;
        use super::publish::remove::{self, Refusal};
        let mut cell = match entry {
            RemovalEntry::Wait => self
                .core
                .publish
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            RemovalEntry::Now => match self.core.publish.try_lock() {
                Ok(cell) => cell,
                Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return Err(Refusal::Busy),
            },
        };
        let registry = self.core.live_registry();
        let session = cell.session(&registry);
        cell.refresh_to(session, &self.core.root_dirs)?;
        let open = cell.open().map_err(|_| Refusal::OutsideRoot)?;
        let bundle_id = uuid::Uuid::new_v4().to_string();
        let mut removal = remove::plan(
            &self.db,
            &open.sandbox,
            &bundle_id,
            targets,
            reason_code,
            actor,
            undo,
            self.core.clock.now_system(),
        )?;
        removal.album_removed = album_removed;
        let track_ids: Vec<String> = removal
            .items
            .iter()
            .map(|item| item.track_id.clone())
            .collect();
        let policy_revision = policy_revision_u64(&registry);
        let bundle = PlanBundle {
            id: bundle_id,
            items: Vec::new(),
            profile_revision: PINNED_REVISION,
            naming_revision: PINNED_REVISION,
            policy_revision,
            put_back: None,
            added: None,
            removal: Some(removal),
        };
        let token_hash = sha256_hex(self.core.ids.new_id().as_bytes());
        let sealed = SealedPreview::seal(
            bundle,
            token_hash.clone(),
            PINNED_REVISION,
            self.core.clock.today_day() + PREVIEW_TTL_DAYS,
        );
        let live = SealRecheck {
            fingerprints: BTreeMap::new(),
            identities: BTreeMap::new(),
            overrides: BTreeMap::new(),
            profile_revision: PINNED_REVISION,
            naming_revision: PINNED_REVISION,
            policy_revision,
            settings_revision: PINNED_REVISION,
            today_day: self.core.clock.today_day(),
            token_hash,
        };
        let published = open
            .publisher
            .publish_as(&sealed, &live, &BTreeMap::new(), None);
        let bundle_id = sealed.bundle.id.as_str();
        let rows = || {
            self.db.read_blocking("publish.removal_rows", |conn| {
                JournalStore::new(conn).bundle(bundle_id)
            })
        };
        if let Err(error) = published {
            let committed = self
                .core.held_bundles
                .committed(bundle_id)
                .unwrap_or_else(|read| {
                    tracing::warn!(%read, bundle_id, "could not tell whether a failed removal committed");
                    false
                });
            if !committed {
                let kept = rows().is_ok_and(|rows| {
                    rows.iter()
                        .any(|row| row.state == JournalState::NeedsAttention)
                });
                return Err(Refusal::Publish { error, kept });
            }
            tracing::warn!(%error, bundle_id, "removal committed; recovery finishes the rest");
        }
        let mut recycled = Vec::new();
        if let Some(removal) = sealed.bundle.removal.as_ref() {
            for row in rows().unwrap_or_default() {
                let (Some(file), Some(replaced)) =
                    (remove::file_of(removal, &row), row.replaces.as_ref())
                else {
                    continue;
                };
                let now_at = if row.state == JournalState::Cleaned {
                    &replaced.recycle_to
                } else {
                    &row.staged
                };
                recycled.push((PathBuf::from(&file.path), PathBuf::from(now_at)));
            }
        }
        let removal_id = sealed
            .bundle
            .removal
            .as_ref()
            .is_some_and(|removal| {
                removal.undo.is_some() && removal.items.iter().any(|item| item.file.is_some())
            })
            .then(|| bundle_id.to_owned());
        Ok(RemovalDone {
            track_ids,
            recycled,
            removal_id,
        })
    }

    /// Put back what the removal `removal_id` took from album `album_id`,
    /// as one put-back bundle planned, sealed and published under the
    /// publish lock. Each copy is hashed before the lock is taken. A
    /// person is waiting: a lock another write holds answers busy at once. A
    /// put-back whose commit landed answers what did not come back even
    /// when a later step failed: recovery finishes it. Answers the
    /// root-relative paths of the files that did not come back. Blocking.
    pub(crate) fn publish_put_back(
        &self,
        removal_id: &str,
        album_id: &str,
        actor: &str,
    ) -> Result<Vec<String>, super::publish::put_back::Refusal> {
        use super::publish::put_back::{self, Refusal};
        let (roots, prehashed) = put_back::prepare(&self.db, removal_id, album_id)?;
        let mut cell = match self.core.publish.try_lock() {
            Ok(cell) => cell,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return Err(Refusal::Busy),
        };
        let registry = self.core.live_registry();
        let session = cell.session(&registry);
        cell.refresh_to(session, &self.core.root_dirs)?;
        let open = cell.open().map_err(|_| Refusal::OutsideRoot)?;
        let bundle_id = uuid::Uuid::new_v4().to_string();
        let plan = put_back::plan(
            &self.db,
            &open.sandbox,
            &registry,
            &bundle_id,
            removal_id,
            album_id,
            Some(actor),
            &roots,
            &prehashed,
        )?;
        let missing = plan.missing.clone();
        let policy_revision = policy_revision_u64(&registry);
        let bundle = PlanBundle {
            id: bundle_id,
            items: Vec::new(),
            profile_revision: PINNED_REVISION,
            naming_revision: PINNED_REVISION,
            policy_revision,
            removal: None,
            put_back: Some(plan),
            added: None,
        };
        let token_hash = sha256_hex(self.core.ids.new_id().as_bytes());
        let sealed = SealedPreview::seal(
            bundle,
            token_hash.clone(),
            PINNED_REVISION,
            self.core.clock.today_day() + PREVIEW_TTL_DAYS,
        );
        let live = SealRecheck {
            fingerprints: BTreeMap::new(),
            identities: BTreeMap::new(),
            overrides: BTreeMap::new(),
            profile_revision: PINNED_REVISION,
            naming_revision: PINNED_REVISION,
            policy_revision,
            settings_revision: PINNED_REVISION,
            today_day: self.core.clock.today_day(),
            token_hash,
        };
        if let Err(error) = open
            .publisher
            .publish_as(&sealed, &live, &BTreeMap::new(), None)
        {
            let bundle_id = sealed.bundle.id.as_str();
            let committed = self
                .core.held_bundles
                .committed(bundle_id)
                .unwrap_or_else(|read| {
                    tracing::warn!(%read, bundle_id, "could not tell whether a failed put-back committed");
                    false
                });
            if !committed {
                return Err(Refusal::Publish(error));
            }
            tracing::warn!(%error, bundle_id, "put-back committed; recovery finishes the rest");
        }
        Ok(missing)
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
        let retention = self.undo_retention_days()?;
        let mut cell = self
            .core
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.core.live_registry();
        cell.refresh(&registry, &self.core.root_dirs)?;
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
        open.publisher.set_undo_retention_days(retention);
        let mut bundle = PlanBundle {
            id: bundle_id.to_owned(),
            items: plan_items,
            profile_revision: pins.profile,
            naming_revision: pins.naming,
            policy_revision: policy_revision_u64(&registry),
            put_back: None,
            added: None,
            removal: None,
        };
        // What moved since planning is caught by the seal recheck below;
        // the rows are sealed now, so the commit refuses only what moves
        // while the album is being saved.
        super::publish::planner::seal_rows(&self.db, &mut bundle)?;
        let token_hash = sha256_hex(self.core.ids.new_id().as_bytes());
        let sealed = SealedPreview::seal(
            bundle,
            token_hash.clone(),
            pins.settings,
            self.core.clock.today_day() + PREVIEW_TTL_DAYS,
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
            outcome,
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
            .core
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.core.live_registry();
        cell.refresh(&registry, &self.core.root_dirs)
            .map_err(publish_error)?;
        let open = cell.open().map_err(publish_error)?;
        if let Some(caller) = only_by {
            let applied_by = self
                .db
                .read_blocking("publish.applied_by", |conn| {
                    super::publish::operations::applied_by(conn, bundle_id)
                })
                .map_err(publish_error)?;
            if applied_by.flatten().as_deref() != Some(caller) {
                return Err(ServiceError::NotFound);
            }
        }
        // A bundle that added a file (a plugin's) is undone by taking the
        // file away; it has no before-states.
        let added = self
            .db
            .read_blocking("publish.undo_added", |conn| {
                super::publish::add::plan_take_back(conn, bundle_id)
            })
            .map_err(publish_error)?;
        if !matches!(added, Err(super::publish::add::TakeBackRefusal::NotAnAdd)) {
            return self.take_back(open, &registry, added);
        }
        let files = self.undo_files_in(open, bundle_id, &|_| PINNED_OVERRIDE)?;
        // A file whose saved copy is gone can never be undone, so it does
        // not hold the rest of the bundle back.
        let held = files
            .blocked
            .iter()
            .any(|(_, skip)| !matches!(skip, UndoSkip::CopyMissing));
        if held || files.eligible.is_empty() {
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
        Ok(applied)
    }

    /// Undo a bundle that added a file: take the file away while it still
    /// holds the bytes written, in a new bundle. The caller holds the cell
    /// and the guards.
    fn take_back(
        &self,
        open: &mut OpenPublish,
        registry: &RootRegistry,
        planned: Result<super::publish::add::Added, super::publish::add::TakeBackRefusal>,
    ) -> Result<AppliedBundle, ServiceError> {
        use super::publish::add::TakeBackRefusal;
        let blocked = |why: String| ServiceError::Conflict {
            message: format!("Undo blocked: {why}"),
        };
        let refused = |refusal: TakeBackRefusal| match refusal {
            TakeBackRefusal::NotAnAdd => ServiceError::NotFound,
            TakeBackRefusal::IsATakeBack => {
                blocked("this change took a plugin's file away and can't be undone".to_owned())
            }
            TakeBackRefusal::NotLanded => blocked("the plugin's file was never added".to_owned()),
            TakeBackRefusal::AlreadyTakenBack => {
                blocked("the plugin's file was already taken away".to_owned())
            }
            TakeBackRefusal::FolderUnusable(path) => blocked(format!(
                "the library folder of {path} takes no file changes now"
            )),
            TakeBackRefusal::Gone(path) => blocked(format!("{path} is no longer there")),
            TakeBackRefusal::Changed(path) => {
                blocked(format!("{path} changed since the plugin wrote it"))
            }
        };
        let added = planned.map_err(refused)?;
        super::publish::add::check_take_back(&open.sandbox, &added.file).map_err(refused)?;
        let root_id = added.file.root_id.clone();
        let policy_revision = policy_revision_u64(registry);
        let bundle = PlanBundle {
            id: self.core.ids.new_id(),
            items: Vec::new(),
            profile_revision: PINNED_REVISION,
            naming_revision: PINNED_REVISION,
            policy_revision,
            removal: None,
            put_back: None,
            added: Some(added),
        };
        let (sealed, live) = seal_pinned(
            bundle,
            &self.core.ids.new_id(),
            policy_revision,
            self.core.clock.today_day(),
        );
        let bundle_id = sealed.bundle.id.clone();
        let outcome = match open
            .publisher
            .publish_as(&sealed, &live, &BTreeMap::new(), None)
        {
            Ok(outcome) => outcome,
            Err(error) => {
                if !self
                    .core
                    .held_bundles
                    .committed(&bundle_id)
                    .unwrap_or(false)
                {
                    return Err(publish_error(error));
                }
                tracing::warn!(%error, bundle_id, "plugin file taken away; recovery finishes the rest");
                PublishOutcome::CleanupPending
            }
        };
        // A track the scan made from the file goes missing at the next scan.
        self.core.dirty.mark(&root_id);
        self.core.wakeups.notify("scan");
        Ok(AppliedBundle {
            bundle_id,
            outcome,
            files: Vec::new(),
        })
    }

    /// What undoing one published bundle would do, file by file. Blocking.
    pub(crate) fn undo_files(
        &self,
        bundle_id: &str,
        override_of: &dyn Fn(&str) -> u64,
    ) -> Result<UndoFiles, ServiceError> {
        let mut cell = self
            .core
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.core.live_registry();
        let session = cell.session(&registry);
        cell.refresh_to(session, &self.core.root_dirs)
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
                .is_ok_and(|path| super::publish::paths::holds(&path, &sidecar.fingerprint.sha256));
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
        use super::publish::undo::{UndoInput, UndoLive, plan_undo};

        // Everything the undo reads from the database, in one read that
        // ends before any file is looked at.
        let (sources, outputs) = self
            .db
            .read_blocking("publish.undo_sources", |conn| {
                let Some(source) = super::publish::operations::load_operation(conn, bundle_id)?
                else {
                    return Ok(None);
                };
                let snapshots = SnapshotStore::new(conn).bundle(bundle_id)?;
                let journals = JournalStore::new(conn).bundle(bundle_id)?;
                let blobs = &self.core.blobs;
                let mut rows = Vec::new();
                for (_, track_id, blob_sha, expires_day) in snapshots {
                    let Some(item) = source.items.iter().find(|item| item.track_id == track_id)
                    else {
                        continue;
                    };
                    let Some(journal) = journals
                        .iter()
                        .find(|journal| journal.track_id.as_deref() == Some(track_id.as_str()))
                    else {
                        continue;
                    };
                    let bytes = blobs.fetch(conn, &blob_sha)?;
                    let located = locate(conn, &track_id)?;
                    rows.push((
                        track_id,
                        item.clone(),
                        journal.clone(),
                        bytes,
                        expires_day,
                        located,
                    ));
                }
                let outputs = super::publish::operations::outputs_of(conn, bundle_id)?;
                Ok::<_, PublishError>(Some((rows, outputs)))
            })
            .map_err(publish_error)?
            .ok_or(ServiceError::NotFound)?;
        let mut inputs = Vec::new();
        let mut locations_read = HashMap::new();
        let mut by_track: HashMap<String, PlanItem> = HashMap::new();
        let mut copies_missing = Vec::new();
        for (track_id, item, journal, bytes, expires_day, located) in sources {
            let Some(bytes) = bytes else {
                copies_missing.push(track_id);
                continue;
            };
            locations_read.insert(track_id.clone(), located);
            by_track.insert(track_id.clone(), item.clone());
            let before =
                super::publish::undo::BeforeState::from_bytes(&bytes).map_err(publish_error)?;
            let staged_size = super::publish::paths::FileStat::read(
                &open
                    .sandbox
                    .resolve_no_symlink(&journal.dest_root, &journal.dest_rel)
                    .unwrap_or_else(|_| PathBuf::from(&journal.staged)),
            )
            .map(|stat| stat.size)
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
                expires_day,
            });
        }
        let mut files = UndoFiles::default();
        files.blocked.extend(
            copies_missing
                .into_iter()
                .map(|track_id| (track_id, UndoSkip::CopyMissing)),
        );
        let mut fingerprints = BTreeMap::new();
        let mut locations = BTreeMap::new();
        let mut identities = BTreeMap::new();
        let mut overrides = BTreeMap::new();
        let mut live_inputs = Vec::new();
        for input in inputs {
            let located = locations_read.remove(&input.track_id).flatten();
            let live = match located {
                Some(super::publish::publisher::Located {
                    root_id, rel_path, ..
                }) => match Self::live_fingerprint(&open.sandbox, &root_id, &rel_path) {
                    Ok(fingerprint) => self
                        .live_identity(&input.track_id)?
                        .map(|identity| (root_id, rel_path, fingerprint, identity)),
                    Err(_) => None,
                },
                None => None,
            };
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
            today_day: self.core.clock.today_day(),
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
                .map(|doc| doc.managed.named())
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
            for output in outputs
                .get(item.track_id.as_str())
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                let unchanged = open
                    .sandbox
                    .resolve_no_symlink(&output.dest_root, &output.dest_rel)
                    .is_ok_and(|path| super::publish::paths::holds(&path, &output.sha256));
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
                    sealed: None,
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
            .core
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.core.live_registry();
        cell.refresh(&registry, &self.core.root_dirs)
            .map_err(publish_error)?;
        let open = cell.open().map_err(publish_error)?;
        let files = self.baseline_files_in(open, track_ids, &|_| PINNED_OVERRIDE)?;
        let held = files
            .blocked
            .iter()
            .any(|(_, skip)| !matches!(skip, BaselineSkip::CopyMissing));
        if held || files.eligible.is_empty() {
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
            .core
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.core.live_registry();
        let session = cell.session(&registry);
        cell.refresh_to(session, &self.core.root_dirs)
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
        use super::publish::undo::{BaselineInput, plan_baseline_restore};

        // Each track's baseline bytes and catalog place, in one read that
        // ends before any file is looked at.
        let sources = self
            .db
            .read_blocking("publish.baseline_sources", |conn| {
                let blobs = &self.core.blobs;
                let baselines = BaselineStore::new(conn);
                track_ids
                    .iter()
                    .map(|track_id| {
                        let bytes = match baselines.get(track_id)? {
                            Some((blob_sha, _, _)) => Some(blobs.fetch(conn, &blob_sha)?),
                            None => None,
                        };
                        Ok((bytes, locate(conn, track_id)?))
                    })
                    .collect::<Result<Vec<_>, PublishError>>()
            })
            .map_err(publish_error)?;
        let mut files = BaselineFiles::default();
        let mut inputs = Vec::new();
        let mut current_locs: HashMap<String, (String, String)> = HashMap::new();
        let mut live: HashMap<String, (FileFingerprint, AudioFormat, ReleaseIdentity)> =
            HashMap::new();
        for (track_id, (bytes, located)) in track_ids.iter().zip(sources) {
            let baseline = match bytes {
                Some(Some(bytes)) => Some(
                    super::publish::undo::BeforeState::from_bytes(&bytes).map_err(publish_error)?,
                ),
                Some(None) => {
                    files
                        .blocked
                        .push((track_id.clone(), BaselineSkip::CopyMissing));
                    continue;
                }
                None => None,
            };
            let state = match located {
                Some(super::publish::publisher::Located {
                    root_id, rel_path, ..
                }) => {
                    match (
                        Self::live_fingerprint(&open.sandbox, &root_id, &rel_path),
                        Self::live_format(&rel_path),
                    ) {
                        (Ok(current), Ok(format)) => self
                            .live_identity(track_id)?
                            .map(|identity| (root_id, rel_path, current, format, identity)),
                        _ => None,
                    }
                }
                None => None,
            };
            let Some((root_id, rel_path, current, format, identity)) = state else {
                files.blocked.push((track_id.clone(), BaselineSkip::Gone));
                continue;
            };
            current_locs.insert(track_id.clone(), (root_id, rel_path));
            live.insert(
                track_id.clone(),
                (current.clone(), format, identity.clone()),
            );
            inputs.push(BaselineInput {
                track_id: track_id.clone(),
                baseline,
                current: current.clone(),
                pinned_current: current,
                identity: identity.clone(),
                pinned_identity: identity,
                format,
                pinned_format: format,
            });
        }
        let sandbox = open.sandbox.clone();
        let registry = self.core.live_registry();
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
            let removals: Vec<_> = self
                .db
                .read_blocking("publish.generated_for_track", |conn| {
                    super::publish::operations::generated_for_track(conn, &track_id)
                })
                .map_err(publish_error)?
                .into_iter()
                .filter(|output| {
                    sandbox
                        .resolve_no_symlink(&output.dest_root, &output.dest_rel)
                        .is_ok_and(|path| super::publish::paths::holds(&path, &output.sha256))
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
                current: current.managed.named(),
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
                    format: *format,
                    managed_updates: updates,
                    sidecars: Vec::new(),
                    staged_bytes_estimate: fingerprint.size + STAGED_HEADROOM_BYTES,
                    adopt: None,
                    replaces: None,
                    outputs: Vec::new(),
                    removals,
                    sealed: None,
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
        let retention = self.undo_retention_days().map_err(publish_error)?;
        let open = cell.open().map_err(publish_error)?;
        let mut bundle = PlanBundle {
            id: self.core.ids.new_id(),
            items: plan_items,
            profile_revision: PINNED_REVISION,
            naming_revision: PINNED_REVISION,
            policy_revision: policy_revision_u64(&self.core.live_registry()),
            put_back: None,
            added: None,
            removal: None,
        };
        super::publish::planner::seal_rows(&self.db, &mut bundle).map_err(publish_error)?;
        // Restoration bundles re-run the dry-run gates: a restore
        // onto an occupied path blocks instead of overwriting.
        CollisionGate::check_bundle(&open.sandbox, &bundle).map_err(publish_error)?;
        DiskPreflight::check(&bundle, &FsSpaceProbe::new(self.core.root_dirs.clone()))
            .map_err(publish_error)?;
        let token = self.core.ids.new_id();
        let token_hash = sha256_hex(token.as_bytes());
        let sealed = SealedPreview::seal(
            bundle,
            token_hash.clone(),
            PINNED_REVISION,
            self.core.clock.today_day() + PREVIEW_TTL_DAYS,
        );
        let live = self.seal_live(&open.sandbox, &sealed.bundle, &token_hash)?;
        open.publisher.set_undo_retention_days(retention);
        let outcome = open
            .publisher
            .publish(&sealed, &live, &docs)
            .map_err(publish_error)?;
        Ok(AppliedBundle {
            bundle_id: sealed.bundle.id.clone(),
            outcome,
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
            UndoSkip::CopyMissing => format!("{track_id} copy_missing"),
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

/// A management gate read that failed: a busy database answers busy, the
/// rest is a fault.
fn gate_error(error: DbError) -> ServiceError {
    match error {
        DbError::Busy { .. } | DbError::LaneClosed => ServiceError::Busy {
            cause: error.to_string(),
        },
        other => ServiceError::internal(&other),
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
            BaselineSkip::CopyMissing => format!("{track_id} copy_missing"),
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
