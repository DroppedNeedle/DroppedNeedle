//! Staged publisher: prepare, publish, catalog commit, cleanup.
//!
//! Apply consumes the exact sealed preview. Every phase runs its
//! gates first: capability, collision, snapshot, journal, validation,
//! catalog commit, cleanup, and cache invalidation. Manual Apply
//! rejects stale file, identity, profile, or policy state before a
//! single byte moves; late collisions and catalog failures compensate
//! instead of leaving half-renamed bundles.
//!
//! Crash injection (`CrashPoint`) stops the protocol between durable
//! steps so the tests can prove that restart recovery always resumes
//! or compensates. Production never sets a crash point.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension};

use super::journal::{FileJournal, JournalKind, JournalState, JournalStore};
use super::paths::Sandbox;
use super::planner::{
    CapabilityGate, CollisionGate, DiskPreflight, PlanBundle, PlanKind, SealError, SealRecheck,
    SealedPreview, SpaceProbe,
};
use super::snapshots::{BlobStore, SnapshotStore, sha256_hex};
use super::tags_seam::TagDocument;
use super::{PublishError, paths};

/// One track row in the atomic catalog commit.
#[derive(Debug, Clone)]
pub struct TrackCommit {
    /// Stable local track id.
    pub track_id: String,
    /// Adopted root and relative path.
    pub root_id: String,
    pub rel_path: String,
    /// Adopted absolute path, stored as the track's `file_path`.
    pub file_path: String,
    /// SHA-256 the catalog must record.
    pub fingerprint: String,
    /// Management state after commit.
    pub mgmt_state: String,
}

/// The single-transaction catalog bundle commit.
#[derive(Debug, Clone)]
pub struct BundleCommit {
    /// Bundle id for history and invalidation.
    pub bundle_id: String,
    /// Catalog revision the plan was built against; the commit
    /// compare-and-swaps on it.
    pub expected_catalog_revision: u64,
    /// Track adoptions in this bundle.
    pub tracks: Vec<TrackCommit>,
}

/// Catalog port: where a managed track lives and what the publisher
/// last wrote there. Production is [`SqliteCatalog`], the catalog tables
/// the scan writes and the reads API serves.
pub trait Catalog {
    /// Current catalog revision.
    fn revision(&self, conn: &Connection) -> Result<u64, PublishError>;
    /// Locate a track: (root id, relative path, last published SHA-256,
    /// management state). The last two are empty for a track the
    /// publisher never wrote.
    fn locate(
        &self,
        conn: &Connection,
        track_id: &str,
    ) -> Result<Option<(String, String, String, String)>, PublishError>;
    /// Commit one bundle: CAS on the expected revision, move every track
    /// row (and its album's root) to its published location, bump the
    /// catalog once. Track and album ids stay as they are.
    fn commit_bundle(&self, conn: &Connection, commit: &BundleCommit) -> Result<(), PublishError>;
    /// Mark a track missing so streaming and search stay accurate when
    /// committed bytes cannot be recovered.
    fn mark_missing(&self, conn: &Connection, track_id: &str) -> Result<(), PublishError>;
}

/// Flush a directory entry change where a failure must not undo work
/// already done (cleanup and compensation): the next sync or recovery
/// pass covers it, so it is logged instead of returned.
fn sync_dir_logged(dir: &Path) {
    if let Err(error) = super::journal::fsync_dir(dir) {
        tracing::warn!(dir = %dir.display(), %error, "directory sync failed");
    }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

/// The application catalog: `local_tracks` holds where a track lives,
/// `library_track_management_state` what the publisher last wrote, and
/// `library_catalog_revision` the compare-and-swap counter (the same one
/// read caches key on, so a commit invalidates them).
#[derive(Debug, Clone, Default)]
pub struct SqliteCatalog;

impl Catalog for SqliteCatalog {
    fn revision(&self, conn: &Connection) -> Result<u64, PublishError> {
        let rev: i64 = conn.query_row(
            "SELECT value FROM library_catalog_revision WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(rev as u64)
    }

    fn locate(
        &self,
        conn: &Connection,
        track_id: &str,
    ) -> Result<Option<(String, String, String, String)>, PublishError> {
        conn.query_row(
            "SELECT t.root_id, t.relative_path, COALESCE(m.applied_projection_hash, ''), \
             COALESCE(m.last_outcome, '') \
             FROM local_tracks t \
             LEFT JOIN library_track_management_state m ON m.local_track_id = t.id \
             WHERE t.id = ?1",
            rusqlite::params![track_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(PublishError::from)
    }

    fn commit_bundle(&self, conn: &Connection, commit: &BundleCommit) -> Result<(), PublishError> {
        let changed = conn.execute(
            "UPDATE library_catalog_revision SET value = value + 1
             WHERE singleton = 1 AND value = ?1",
            rusqlite::params![commit.expected_catalog_revision as i64],
        )?;
        if changed != 1 {
            return Err(PublishError::Catalog(format!(
                "catalog moved under bundle {}",
                commit.bundle_id
            )));
        }
        let now = now_secs();
        for track in &commit.tracks {
            let moved = conn.execute(
                "UPDATE local_tracks SET root_id = ?2, relative_path = ?3, file_path = ?4, \
                 path_hash = ?5, row_revision = row_revision + 1 WHERE id = ?1",
                rusqlite::params![
                    track.track_id,
                    track.root_id,
                    track.rel_path,
                    track.file_path,
                    sha256_hex(track.rel_path.as_bytes())
                ],
            )?;
            if moved != 1 {
                return Err(PublishError::Catalog(format!(
                    "track {} is not in the catalog",
                    track.track_id
                )));
            }
            // An organize that moves a whole album into another root takes
            // the album row along; its id (and with it the identity) stays.
            conn.execute(
                "UPDATE local_albums SET root_id = ?2, row_revision = row_revision + 1 \
                 WHERE id = (SELECT local_album_id FROM local_tracks WHERE id = ?1) \
                 AND root_id <> ?2 AND NOT EXISTS (SELECT 1 FROM local_tracks t \
                 WHERE t.local_album_id = local_albums.id AND t.root_id <> ?2 \
                 AND t.availability = 'indexed')",
                rusqlite::params![track.track_id, track.root_id],
            )?;
            conn.execute(
                "INSERT INTO library_track_management_state (local_track_id, managed_root_id, \
                 applied_projection_hash, last_outcome, last_managed_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT (local_track_id) DO UPDATE SET \
                 managed_root_id = excluded.managed_root_id, \
                 applied_projection_hash = excluded.applied_projection_hash, \
                 last_outcome = excluded.last_outcome, \
                 last_managed_at = excluded.last_managed_at, \
                 row_revision = library_track_management_state.row_revision + 1",
                rusqlite::params![
                    track.track_id,
                    track.root_id,
                    track.fingerprint,
                    track.mgmt_state,
                    now
                ],
            )?;
        }
        Ok(())
    }

    fn mark_missing(&self, conn: &Connection, track_id: &str) -> Result<(), PublishError> {
        conn.execute(
            "UPDATE local_tracks SET availability = 'missing', missing_since = ?2 WHERE id = ?1",
            rusqlite::params![track_id, now_secs()],
        )?;
        conn.execute(
            "UPDATE library_catalog_revision SET value = value + 1 WHERE singleton = 1",
            [],
        )?;
        Ok(())
    }
}

/// Crash-injection point between durable steps. Each variant names the
/// last completed step; recovery must resume or compensate from there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashPoint {
    /// After staging and journaling, before publish.
    AfterStage,
    /// After a same-path write journaled its backup path, before the
    /// original moves there. Same-path bundles only, so not in
    /// [`CrashPoint::all`].
    AfterBackupJournaled,
    /// After the first destination rename.
    AfterFirstRename,
    /// After all renames, before the catalog transaction.
    BeforeCatalogCommit,
    /// After the catalog transaction commits, before cleanup.
    AfterCatalogCommit,
    /// Midway through post-commit cleanup.
    DuringCleanup,
}

impl CrashPoint {
    /// Text form used in the injected error.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AfterStage => "after-stage",
            Self::AfterBackupJournaled => "after-backup-journaled",
            Self::AfterFirstRename => "after-first-rename",
            Self::BeforeCatalogCommit => "before-catalog-commit",
            Self::AfterCatalogCommit => "after-catalog-commit",
            Self::DuringCleanup => "during-cleanup",
        }
    }

    /// Every injection point, for the per-phase crash test.
    pub fn all() -> Vec<Self> {
        vec![
            Self::AfterStage,
            Self::AfterFirstRename,
            Self::BeforeCatalogCommit,
            Self::AfterCatalogCommit,
            Self::DuringCleanup,
        ]
    }
}

/// Outcome of one bundle publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishOutcome {
    /// Committed and cleaned.
    Committed,
    /// Committed, but cleanup stays pending and retryable.
    CleanupPending,
}

/// Staged publisher. Owns its SQLite connection so the catalog commit
/// plus journal transitions land in one transaction.
pub struct Publisher<C: Catalog, P: SpaceProbe> {
    sandbox: Sandbox,
    conn: Connection,
    catalog: C,
    space: P,
    gate: CapabilityGate,
    injector: Option<CrashPoint>,
    today_day: i64,
    undo_retention_days: i64,
}

impl<C: Catalog, P: SpaceProbe> Publisher<C, P> {
    /// Open a publisher on the application database through the
    /// database factory. The journal tables come from the migrations.
    pub fn open(
        sandbox: Sandbox,
        db_path: &Path,
        catalog: C,
        space: P,
        today_day: i64,
    ) -> Result<Self, PublishError> {
        let conn = crate::db::open_connection(db_path)
            .map_err(|error| PublishError::Store(error.to_string()))?;
        Ok(Self {
            sandbox,
            conn,
            catalog,
            space,
            gate: CapabilityGate::production(),
            injector: None,
            today_day,
            undo_retention_days: super::DEFAULT_UNDO_RETENTION_DAYS as i64,
        })
    }

    /// Arm one crash-injection point. Tests only; production never calls this.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_crash_point(&mut self, point: Option<CrashPoint>) {
        self.injector = point;
    }

    /// Borrow the publisher's database connection.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Publish one sealed bundle through prepare, publish, catalog
    /// commit, and cleanup. `docs` carries the current semantic tag
    /// document per track id for the staged writer seam.
    pub fn publish(
        &mut self,
        sealed: &SealedPreview,
        live: &SealRecheck,
        docs: &BTreeMap<String, TagDocument>,
    ) -> Result<PublishOutcome, PublishError> {
        let bundle = &sealed.bundle;
        self.run_pregates(sealed, live)?;
        // Durable source operation for undo: recorded after the gates
        // pass, before the first byte stages. Rows for operations that
        // then fail linger harmlessly (no snapshots, never writable).
        super::operations::record_operation(&self.conn, bundle, self.today_day)?;
        let staged = self.prepare(bundle, docs)?;
        self.maybe_crash(CrashPoint::AfterStage)?;
        let outcome = self.publish_and_commit(bundle, &staged);
        match outcome {
            Ok(outcome) => Ok(outcome),
            Err(err) => {
                if Self::compensatable(&err) {
                    self.compensate(bundle)?;
                }
                Err(err)
            }
        }
    }

    /// Pre-gates, in order: seal, capability (plus a v2 original that
    /// never arrived), collision, snapshot inputs, disk preflight. Nothing
    /// is staged until all pass.
    fn run_pregates(&self, sealed: &SealedPreview, live: &SealRecheck) -> Result<(), PublishError> {
        sealed.recheck(live).map_err(|err| match err {
            SealError::StaleFile(text) | SealError::StaleIdentity(text) => {
                PublishError::Validation(text)
            }
            SealError::StaleProfile(text) | SealError::StalePolicy(text) => {
                PublishError::Validation(text)
            }
            SealError::StaleCatalog(text) => PublishError::Validation(text),
            SealError::BadToken => PublishError::Validation("bad confirmation token".into()),
            SealError::Expired => PublishError::Validation("preview expired".into()),
        })?;
        let journals = JournalStore::new(&self.conn);
        let baselines = super::snapshots::BaselineStore::new(&self.conn);
        for item in sealed.bundle.items.iter() {
            self.gate.check(item)?;
            if baselines.v2_original_missing(&item.track_id)? {
                return Err(PublishError::Snapshot(format!(
                    "track {} has an original-file baseline from v2 that was not imported; \
                     run the v2 import again before managing it",
                    item.track_id
                )));
            }
            if let Some(held) =
                journals.unsettled_bundle_for_track(&item.track_id, &sealed.bundle.id)?
            {
                return Err(PublishError::Journal(format!(
                    "track {} has an unfinished managed write (bundle {held}) waiting for recovery",
                    item.track_id
                )));
            }
        }
        CollisionGate::check_bundle(&self.sandbox, &sealed.bundle)?;
        DiskPreflight::check(&sealed.bundle, &self.space)?;
        Ok(())
    }

    /// Prepare: verify sources, render staged bytes through the tag
    /// seam, write destination-side temps, capture before-state
    /// snapshots, and journal every intent as staged.
    fn prepare(
        &mut self,
        bundle: &PlanBundle,
        docs: &BTreeMap<String, TagDocument>,
    ) -> Result<Vec<PreparedFile>, PublishError> {
        let mut staged_files = Vec::new();
        for (ordinal, item) in bundle.items.iter().enumerate() {
            let source = self
                .sandbox
                .resolve_no_symlink(&item.source_root, &item.source_rel)?;
            let source_bytes = paths::read_regular_file(&source)?;
            if source_bytes.len() as u64 != item.fingerprint.size
                || sha256_hex(&source_bytes) != item.fingerprint.sha256
            {
                return Err(PublishError::Validation(format!(
                    "source for track {} changed after planning",
                    item.track_id
                )));
            }
            let before = docs
                .get(&item.track_id)
                .cloned()
                .unwrap_or_else(TagDocument::empty);
            let journal_id = journal_id(&bundle.id, ordinal, "audio");
            let dest = self
                .sandbox
                .resolve_no_symlink(&item.dest_root, &item.dest_rel)?;
            self.sandbox.ensure_under_roots(&dest)?;
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(PublishError::from)?;
            }
            let temp = self.sandbox.temp_path_for(&dest, &journal_id)?;
            self.sandbox.ensure_under_roots(&temp)?;
            // Real tag staging through the tags save wrapper.
            // Refusals surface before any journal or temp lands.
            let staged_bytes = super::staging::render_staged_bytes(
                &source_bytes,
                &item.format,
                &item.managed_updates,
                &temp,
            )?;
            let staged_sha = sha256_hex(&staged_bytes);
            let journals = JournalStore::new(&self.conn);
            let journal = FileJournal {
                id: journal_id.clone(),
                bundle_id: bundle.id.clone(),
                kind: JournalKind::Audio,
                source: Some((item.source_root.clone(), item.source_rel.clone())),
                dest_root: item.dest_root.clone(),
                dest_rel: item.dest_rel.clone(),
                staged: temp.to_string_lossy().to_string(),
                backup: None,
                source_sha256: Some(item.fingerprint.sha256.clone()),
                staged_sha256: staged_sha.clone(),
                track_id: Some(item.track_id.clone()),
                catalog_revision: Some(bundle.catalog_revision as i64),
                mgmt_state: Some(format!("managed:{}", bundle.profile_revision)),
                state: JournalState::Prepared,
                seq: 0,
            };
            // Journal first, then the temp: a crash between the two leaves
            // a row recovery knows about, never an orphan temp.
            if journals.get(&journal_id)?.is_none() {
                journals.insert(&journal)?;
            }
            write_staged_temp(&temp, &staged_bytes)?;
            journals.transition(&journal_id, JournalState::Prepared, JournalState::Staged)?;
            let prior_mgmt = self
                .catalog
                .locate(&self.conn, &item.track_id)?
                .map(|(_, _, _, state)| state)
                .filter(|state| !state.is_empty());
            let before_state = super::undo::BeforeState {
                doc: before,
                source_root: item.source_root.clone(),
                source_rel: item.source_rel.clone(),
                source_sha256: item.fingerprint.sha256.clone(),
                mgmt_state_before: prior_mgmt,
            };
            // First-management baseline: captured once, immutable
            // forever. A baseline that appeared concurrently wins; the
            // only Snapshot error capture raises is already-exists.
            if super::snapshots::BaselineStore::new(&self.conn)
                .get(&item.track_id)?
                .is_none()
            {
                let baseline_blob = BlobStore::new(&self.conn).put(&before_state.to_bytes()?)?;
                match super::snapshots::BaselineStore::new(&self.conn).capture(
                    &item.track_id,
                    &baseline_blob,
                    &item.source_root,
                    &item.source_rel,
                    self.today_day,
                ) {
                    Ok(()) => {}
                    Err(PublishError::Snapshot(_)) => {}
                    Err(other) => return Err(other),
                }
            }
            let blob = BlobStore::new(&self.conn).put(&before_state.to_bytes()?)?;
            let snapshots = SnapshotStore::new(&self.conn);
            snapshots.record(
                &format!("{}-{}", bundle.id, item.track_id),
                &bundle.id,
                &item.track_id,
                &blob,
                self.today_day,
                self.today_day + self.undo_retention_days,
            )?;
            for (side_ordinal, sidecar) in item.sidecars.iter().enumerate() {
                self.prepare_sidecar(bundle, &journal_id, side_ordinal, sidecar)?;
            }
            staged_files.push(PreparedFile {
                journal_id,
                track_id: item.track_id.clone(),
                kind: item.kind,
                staged_sha256: staged_sha,
                source_root: item.source_root.clone(),
                source_rel: item.source_rel.clone(),
            });
        }
        Ok(staged_files)
    }

    /// Stage one sidecar as a verified byte copy with its own journal.
    fn prepare_sidecar(
        &mut self,
        bundle: &PlanBundle,
        audio_journal_id: &str,
        side_ordinal: usize,
        sidecar: &super::planner::SidecarPlan,
    ) -> Result<(), PublishError> {
        let source = self
            .sandbox
            .resolve_no_symlink(&sidecar.source_root, &sidecar.source_rel)?;
        let bytes = paths::read_regular_file(&source)?;
        if bytes.len() as u64 != sidecar.fingerprint.size
            || sha256_hex(&bytes) != sidecar.fingerprint.sha256
        {
            return Err(PublishError::Validation(format!(
                "sidecar {} changed after planning",
                sidecar.source_rel
            )));
        }
        let journal_id = format!("{audio_journal_id}-sidecar-{side_ordinal}");
        let dest = self
            .sandbox
            .resolve_no_symlink(&sidecar.dest_root, &sidecar.dest_rel)?;
        self.sandbox.ensure_under_roots(&dest)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(PublishError::from)?;
        }
        let temp = self.sandbox.temp_path_for(&dest, &journal_id)?;
        self.sandbox.ensure_under_roots(&temp)?;
        let journals = JournalStore::new(&self.conn);
        if journals.get(&journal_id)?.is_none() {
            journals.insert(&FileJournal {
                id: journal_id.clone(),
                bundle_id: bundle.id.clone(),
                kind: JournalKind::Sidecar,
                source: Some((sidecar.source_root.clone(), sidecar.source_rel.clone())),
                dest_root: sidecar.dest_root.clone(),
                dest_rel: sidecar.dest_rel.clone(),
                staged: temp.to_string_lossy().to_string(),
                backup: None,
                source_sha256: Some(sidecar.fingerprint.sha256.clone()),
                staged_sha256: sha256_hex(&bytes),
                track_id: None,
                catalog_revision: Some(bundle.catalog_revision as i64),
                mgmt_state: None,
                state: JournalState::Prepared,
                seq: 0,
            })?;
        }
        write_staged_temp(&temp, &bytes)?;
        journals.transition(&journal_id, JournalState::Prepared, JournalState::Staged)?;
        Ok(())
    }

    /// Publish renames plus the atomic catalog commit, then cleanup.
    fn publish_and_commit(
        &mut self,
        bundle: &PlanBundle,
        staged: &[PreparedFile],
    ) -> Result<PublishOutcome, PublishError> {
        let journals = JournalStore::new(&self.conn).bundle(&bundle.id)?;
        let mut first = true;
        for journal in journals.iter() {
            self.publish_one(bundle, staged, journal)?;
            if first {
                first = false;
                self.maybe_crash(CrashPoint::AfterFirstRename)?;
            }
        }
        self.maybe_crash(CrashPoint::BeforeCatalogCommit)?;
        self.commit(bundle, staged)?;
        self.maybe_crash(CrashPoint::AfterCatalogCommit)?;
        let outcome = self.cleanup(bundle, staged)?;
        if outcome == PublishOutcome::CleanupPending {
            return Ok(outcome);
        }
        Ok(PublishOutcome::Committed)
    }

    /// Rename one staged journal onto its destination. Same-path
    /// writes retain the original under a hidden backup first; moves
    /// keep the source until after catalog commit. A late occupier
    /// fails the bundle instead of overwriting.
    fn publish_one(
        &mut self,
        bundle: &PlanBundle,
        staged: &[PreparedFile],
        journal: &FileJournal,
    ) -> Result<(), PublishError> {
        if journal.bundle_id != bundle.id {
            return Err(PublishError::Journal("journal bundle mismatch".into()));
        }
        let current = JournalStore::new(&self.conn)
            .get(&journal.id)?
            .ok_or_else(|| PublishError::Journal(format!("missing journal {}", journal.id)))?;
        if current.state == JournalState::Published {
            return Ok(());
        }
        if current.state != JournalState::Staged {
            return Err(PublishError::Journal(format!(
                "journal {} is in state {}",
                journal.id,
                current.state.as_str()
            )));
        }
        let dest = self
            .sandbox
            .resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
        self.sandbox.ensure_under_roots(&dest)?;
        let staged_path = Path::new(&journal.staged);
        self.sandbox.ensure_under_roots(staged_path)?;
        let staged_bytes = paths::read_regular_file(staged_path)?;
        if sha256_hex(&staged_bytes) != journal.staged_sha256 {
            return Err(PublishError::Validation(format!(
                "staged bytes for journal {} changed",
                journal.id
            )));
        }
        let prepared = staged.iter().find(|file| file.journal_id == journal.id);
        let same_path_self = match prepared {
            Some(file) if file.kind == PlanKind::SamePath => {
                let source = self
                    .sandbox
                    .resolve_no_symlink(&file.source_root, &file.source_rel)?;
                source == dest
            }
            _ => false,
        };
        match std::fs::symlink_metadata(&dest) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(PublishError::UnsafePath(format!(
                        "destination is a symlink: {}:{}",
                        journal.dest_root, journal.dest_rel
                    )));
                }
                if !same_path_self {
                    return Err(PublishError::Collision(format!(
                        "late occupier at {}:{}",
                        journal.dest_root, journal.dest_rel
                    )));
                }
                // Journal the backup before the rename: a crash between
                // the two then still knows where the original went.
                let backup = self.sandbox.backup_path_for(&dest, &journal.id)?;
                self.sandbox.ensure_under_roots(&backup)?;
                JournalStore::new(&self.conn).set_backup(&journal.id, &backup.to_string_lossy())?;
                self.maybe_crash(CrashPoint::AfterBackupJournaled)?;
                std::fs::rename(&dest, &backup).map_err(PublishError::from)?;
                if let Some(parent) = dest.parent() {
                    super::journal::fsync_dir(parent)?;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(PublishError::Io(err.to_string())),
        }
        std::fs::rename(staged_path, &dest).map_err(PublishError::from)?;
        if let Some(parent) = dest.parent() {
            super::journal::fsync_dir(parent)?;
        }
        let moved = JournalStore::new(&self.conn).transition(
            &journal.id,
            JournalState::Staged,
            JournalState::Published,
        )?;
        if !moved {
            return Err(PublishError::Journal(format!(
                "journal {} moved under publish",
                journal.id
            )));
        }
        Ok(())
    }

    /// Single-transaction commit: catalog CAS plus every journal
    /// `published -> committed`, then cache invalidation rows.
    fn commit(&mut self, bundle: &PlanBundle, staged: &[PreparedFile]) -> Result<(), PublishError> {
        let mut tracks = Vec::with_capacity(staged.len());
        for file in staged {
            let item = bundle
                .items
                .iter()
                .find(|item| item.track_id == file.track_id)
                .ok_or_else(|| PublishError::Journal("staged file without a plan item".into()))?;
            let dest = self
                .sandbox
                .resolve_no_symlink(&item.dest_root, &item.dest_rel)?;
            tracks.push(TrackCommit {
                track_id: file.track_id.clone(),
                root_id: item.dest_root.clone(),
                rel_path: item.dest_rel.clone(),
                file_path: dest.to_string_lossy().into_owned(),
                fingerprint: file.staged_sha256.clone(),
                mgmt_state: format!("managed:{}", bundle.profile_revision),
            });
        }
        let commit = BundleCommit {
            bundle_id: bundle.id.clone(),
            expected_catalog_revision: bundle.catalog_revision,
            tracks,
        };
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(PublishError::from)?;
        if let Err(err) = self.catalog.commit_bundle(&tx, &commit) {
            drop(tx);
            return Err(err);
        }
        let journals = JournalStore::new(&tx);
        for journal in journals.bundle(&bundle.id)? {
            if journal.state != JournalState::Published {
                return Err(PublishError::Journal(format!(
                    "journal {} is {} at commit",
                    journal.id,
                    journal.state.as_str()
                )));
            }
            let moved = journals.transition(
                &journal.id,
                JournalState::Published,
                JournalState::Committed,
            )?;
            if !moved {
                return Err(PublishError::Journal(format!(
                    "journal {} moved under commit",
                    journal.id
                )));
            }
        }
        tx.commit().map_err(PublishError::from)?;
        Ok(())
    }

    /// Post-commit cleanup: remove verified old sources and backups,
    /// prune newly empty source directories, and mark journals
    /// cleaned. Failures stay durable `cleanup_pending` and never roll
    /// back the commit.
    fn cleanup(
        &mut self,
        bundle: &PlanBundle,
        staged: &[PreparedFile],
    ) -> Result<PublishOutcome, PublishError> {
        let mut pending = false;
        let mut cleaned_any = false;
        let journals = JournalStore::new(&self.conn).bundle(&bundle.id)?;
        for journal in journals.iter() {
            if journal.state != JournalState::Committed {
                continue;
            }
            if let Err(error) = self.cleanup_one(journal, staged) {
                pending = true;
                tracing::warn!(journal_id = %journal.id, %error, "publish cleanup failed; left pending");
                if let Err(error) = JournalStore::new(&self.conn).transition(
                    &journal.id,
                    JournalState::Committed,
                    JournalState::CleanupPending,
                ) {
                    tracing::warn!(journal_id = %journal.id, %error, "cleanup-pending not recorded");
                }
                continue;
            }
            let moved = JournalStore::new(&self.conn).transition(
                &journal.id,
                JournalState::Committed,
                JournalState::Cleaned,
            )?;
            if !moved {
                pending = true;
            }
            if !cleaned_any {
                cleaned_any = true;
                self.maybe_crash(CrashPoint::DuringCleanup)?;
            }
        }
        if pending {
            Ok(PublishOutcome::CleanupPending)
        } else {
            Ok(PublishOutcome::Committed)
        }
    }

    /// Remove one journal's verified source or backup, then prune
    /// newly empty source parents. Unknown files are never deleted.
    fn cleanup_one(
        &self,
        journal: &FileJournal,
        staged: &[PreparedFile],
    ) -> Result<(), PublishError> {
        if let Some(backup_text) = journal.backup.as_deref() {
            let backup = Path::new(backup_text);
            self.sandbox.ensure_under_roots(backup)?;
            let bytes = paths::read_regular_file(backup)?;
            let expected = journal.source_sha256.as_deref().unwrap_or_default();
            if !expected.is_empty() && sha256_hex(&bytes) == expected {
                std::fs::remove_file(backup).map_err(PublishError::from)?;
                if let Some(parent) = backup.parent() {
                    sync_dir_logged(parent);
                }
            } else if expected.is_empty() {
                std::fs::remove_file(backup).map_err(PublishError::from)?;
            }
        }
        if let Some((root, rel)) = journal.source.as_ref() {
            let prepared = staged.iter().find(|file| file.journal_id == journal.id);
            let is_move = prepared
                .map(|file| file.kind == PlanKind::Move)
                .unwrap_or(true);
            if is_move || journal.kind == JournalKind::Sidecar {
                let source = self.sandbox.resolve_no_symlink(root, rel)?;
                let dest = self
                    .sandbox
                    .resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
                if source != dest
                    && let Ok(bytes) = paths::read_regular_file(&source)
                {
                    let expected = journal.source_sha256.as_deref().unwrap_or_default();
                    if expected.is_empty() || sha256_hex(&bytes) == expected {
                        std::fs::remove_file(&source).map_err(PublishError::from)?;
                        prune_empty_parents(&self.sandbox, root, &source)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Compensate a pre-commit failure: remove published destinations
    /// whose bytes still match the staged copy, restore same-path
    /// backups, delete unpublished temps, and mark journals
    /// compensated. Anything ambiguous is left for recovery to flag.
    fn compensate(&mut self, bundle: &PlanBundle) -> Result<(), PublishError> {
        let journals = JournalStore::new(&self.conn).bundle(&bundle.id)?;
        for journal in journals.iter() {
            if journal.state.is_terminal() || journal.state == JournalState::Committed {
                continue;
            }
            self.compensate_one(journal)?;
            let current = JournalStore::new(&self.conn)
                .get(&journal.id)?
                .ok_or_else(|| PublishError::Journal(format!("missing journal {}", journal.id)))?;
            if let Err(error) = JournalStore::new(&self.conn).transition(
                &journal.id,
                current.state,
                JournalState::Compensated,
            ) {
                tracing::warn!(journal_id = %journal.id, %error, "compensation not recorded");
            }
        }
        Ok(())
    }

    /// Unwind one pre-commit journal by byte comparison only.
    fn compensate_one(&self, journal: &FileJournal) -> Result<(), PublishError> {
        let dest = self
            .sandbox
            .resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
        if let Ok(bytes) = paths::read_regular_file(&dest)
            && sha256_hex(&bytes) == journal.staged_sha256
        {
            std::fs::remove_file(&dest).map_err(PublishError::from)?;
        }
        if let Some(backup_text) = journal.backup.as_deref() {
            let backup = Path::new(backup_text);
            if std::fs::symlink_metadata(&dest).is_err()
                && let Ok(bytes) = paths::read_regular_file(backup)
            {
                let expected = journal.source_sha256.as_deref().unwrap_or_default();
                if expected.is_empty() || sha256_hex(&bytes) == expected {
                    std::fs::rename(backup, &dest).map_err(PublishError::from)?;
                }
            }
        }
        let staged = Path::new(&journal.staged);
        if std::fs::symlink_metadata(staged).is_ok()
            && let Ok(bytes) = paths::read_regular_file(staged)
            && sha256_hex(&bytes) == journal.staged_sha256
        {
            std::fs::remove_file(staged).map_err(PublishError::from)?;
        }
        if let Some(parent) = dest.parent() {
            sync_dir_logged(parent);
        }
        Ok(())
    }

    /// Whether a publish failure compensates (pre-commit) rather than
    /// surfacing a post-commit state. Injected crashes compensate or
    /// resume through recovery, never here.
    fn compensatable(err: &PublishError) -> bool {
        !matches!(
            err,
            PublishError::InjectedCrash(_) | PublishError::Cleanup(_)
        )
    }

    /// Fail with the armed crash point, if it matches.
    fn maybe_crash(&self, point: CrashPoint) -> Result<(), PublishError> {
        if self.injector == Some(point) {
            return Err(PublishError::InjectedCrash(point.as_str().to_string()));
        }
        Ok(())
    }
}

/// One staged file: the journal plus the plan facts publish needs.
#[derive(Debug, Clone)]
struct PreparedFile {
    journal_id: String,
    track_id: String,
    kind: PlanKind,
    staged_sha256: String,
    source_root: String,
    source_rel: String,
}

/// Deterministic journal id for idempotent retry.
fn journal_id(bundle_id: &str, ordinal: usize, kind: &str) -> String {
    format!("{bundle_id}-{ordinal:03}-{kind}")
}

/// Write a staged temp atomically: create, write, fsync, re-read, and
/// fsync the directory. An existing temp with identical bytes is
/// reused so crashed attempts retry idempotently; mismatched bytes
/// fail closed.
fn write_staged_temp(temp: &Path, bytes: &[u8]) -> Result<(), PublishError> {
    if let Ok(current) = paths::read_regular_file(temp) {
        if current == bytes {
            return Ok(());
        }
        let name = temp
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("staging temp");
        return Err(PublishError::Journal(format!(
            "staged temp {name} holds foreign bytes"
        )));
    }
    let written = (|| {
        std::fs::write(temp, bytes).map_err(PublishError::from)?;
        let file = std::fs::File::open(temp).map_err(PublishError::from)?;
        super::journal::fsync_file(&file)?;
        drop(file);
        let reread = paths::read_regular_file(temp)?;
        if reread != bytes {
            return Err(PublishError::Validation(
                "staged temp failed re-read".into(),
            ));
        }
        if let Some(parent) = temp.parent() {
            super::journal::fsync_dir(parent)?;
        }
        Ok(())
    })();
    if written.is_err() {
        // A partial temp would hold foreign bytes for the retry; it is
        // ours (just created), so it goes.
        if let Err(error) = std::fs::remove_file(temp)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(temp = %temp.display(), %error, "partial staging temp not removed");
        }
    }
    written
}

/// Prune newly empty source parents up to (not including) the root.
/// Stops at the first non-empty directory; unknown files are never
/// deleted.
fn prune_empty_parents(
    sandbox: &Sandbox,
    root_id: &str,
    source: &Path,
) -> Result<(), PublishError> {
    let root = sandbox.root_dir(root_id)?.to_path_buf();
    let mut cursor = source.parent().map(Path::to_path_buf);
    while let Some(dir) = cursor {
        if dir == root || !dir.starts_with(&root) {
            break;
        }
        let mut entries = std::fs::read_dir(&dir).map_err(PublishError::from)?;
        if entries.next().is_some() {
            break;
        }
        std::fs::remove_dir(&dir).map_err(PublishError::from)?;
        cursor = dir.parent().map(Path::to_path_buf);
    }
    Ok(())
}
