//! Library Management publisher.
//!
//! This is the only file-writing boundary for managed audio: a planner
//! builds an immutable preview, the administrator seals it, and the staged
//! publisher moves bytes through prepare, publish, catalog commit, and
//! cleanup. Nothing else writes managed files.
//!
//! The port keeps the behavior of v2's library management publisher and
//! its recovery, undo and baseline services; nothing working there is
//! removed.
//!
//! # Crash semantics
//!
//! SQLite and the filesystem are never claimed as one transaction.
//! Instead every audio file, sidecar, and external-art output owns a
//! durable journal intent with a monotonic state machine
//! (`journal::JournalState`), and the whole album bundle moves through
//! one protocol:
//!
//! ```text
//! prepare -> publish -> catalog-commit -> cleanup
//! ```
//!
//! A publish stages its files, then runs the same steps recovery runs
//! after a crash, from `staged` on: one function reads what the rows and
//! the files show and decides the next step, whichever of the two calls
//! it.
//!
//! - **Prepare** writes destination-side hidden temps
//!   (`.droppedneedle-management-*`), re-reads and validates them, fsyncs
//!   file bytes, then fsyncs the containing directory. Journal:
//!   `prepared -> staged`. A crash here leaves only hidden temps, which
//!   recovery deletes after fingerprint comparison.
//! - **Publish** rechecks the sealed preview (settings, profile, naming,
//!   policy, accepted identity, override revisions, fingerprints, exact
//!   and Unicode/case-fold collisions) under one exclusive multi-root
//!   lease, then places each staged temp on its destination through
//!   `place::place_from_hidden`, which cannot replace a file that turned
//!   up there since the check. A file the write replaces in place is
//!   first set aside under a hidden backup by `place::set_aside_with`,
//!   which refuses a file it could not put back and returns one that
//!   changed since it was read. Each file is atomic; a crash
//!   between two files is resolved per journal by comparing staged,
//!   destination, source, and backup fingerprints, never by assuming
//!   path existence means ownership. Journal: `staged -> published`.
//! - **Catalog commit** is one SQLite transaction that first checks
//!   each moved track's own rows (place, availability, stat revision,
//!   album and identity) are still as sealed. Journal:
//!   `published -> committed`. A crash between the last rename and the
//!   commit resumes the commit only when every destination still holds
//!   the exact staged bytes.
//! - **Cleanup** removes verified old sources and backups, then prunes
//!   newly empty source directories, whether publish or recovery runs it. Journal: `committed -> cleaned`.
//!   Cleanup failure is durable `cleanup_pending` and retryable; it never
//!   rolls back the commit.
//!
//! Failure before commit compensates: published destinations whose bytes
//! still match the staged copy are removed, same-path backups are
//! restored, unpublished temps are deleted, and the folders the bundle
//! made go once they are empty. A backup that cannot go
//! home (its name was taken meanwhile, or its bytes changed) is kept, and
//! its row needs attention instead. Committed journals are never rolled
//! back. Anything ambiguous (duplicate staged locations,
//! changed bytes, unsafe paths, mixed commit states, catalog/destination
//! disagreement) moves the whole bundle to `needs_attention` with
//! structured evidence and deletes nothing.
//!
//! Supporting rules, all enforced here and covered by tests in
//! `server/tests/library/library_publish.rs`:
//!
//! - Same-filesystem rename only: temps live in the destination
//!   directory, so a cross-device move is impossible by construction.
//! - Disk preflight runs before the first byte is staged.
//! - Destinations are never overwritten while occupied, even by
//!   identical bytes; resolution is explicit.
//! - Media symlinks are never followed; symlinked inputs or path
//!   components fail closed.
//! - Hardlinked audio publishes to an independent destination inode; the
//!   other directory entry is untouched.
//! - Snapshots are content-addressed (SHA-256) and deduplicated;
//!   first-management baselines are immutable and indefinite, while
//!   per-operation snapshots expire.
//! - Case/Unicode collisions use the NFC + full-casefold key, so two
//!   names that fold together can never both publish.
//! - Every publisher write lands under a sandbox root; anything else is
//!   rejected before any mutation.
//! - A file leaving the library (`remove` rows, see [`remove`]) is set
//!   aside under its row, its track is marked missing in the catalog
//!   commit, and only then does it move into its recycle bin. It is never
//!   deleted, and an unwind puts it back.
//! - Remove extras' undo (`put_back` rows, see [`put_back`]) copies each
//!   recycled file home under its row and re-indexes its track in the
//!   commit; the recycled copy goes only after that.
//! - A file a plugin adds (`add` rows, see [`add`]) is placed only onto a
//!   free name, never by check-then-rename, and its undo takes it away
//!   only while it holds the bytes written.

pub mod add;
pub mod archive;
pub mod bin;
pub mod bin_contents;
pub mod expiry;
mod folders;
pub mod generated;
pub(crate) mod import_staging;
pub mod journal;
pub mod operations;
pub mod paths;
pub mod place;
pub mod planner;
pub mod publisher;
pub mod put_back;
pub mod record;
pub mod recovery;
pub mod remove;
pub mod snapshots;
pub(crate) mod sql;
pub mod staging;
mod step;
pub mod tags_seam;
pub mod undo;
pub mod v2_baseline;

pub use archive::{ArchiveBlock, ArchiveEntry, ArchivePolicy, ArchiveReport, validate_archive};
pub use journal::{
    FileJournal, JournalKind, JournalReplacement, JournalState, JournalStore, JournalWriter,
    MgmtState, fsync_dir,
};
pub use paths::{CollisionKey, Sandbox, collision_key};
pub use place::{PlaceError, Placed, PlacementMode, place_from_hidden};
pub use planner::{
    Capability, CapabilityGate, DiskPreflight, FileFingerprint, GeneratedOutput, OutputKind,
    PlanBundle, PlanItem, PlanKind, ReleaseIdentity, Replacement, SealError, SealedPreview,
    SpaceProbe,
};
pub use publisher::{CrashPoint, PublishOutcome, Publisher};
pub use recovery::{BundleRecovery, RecoveryAction, reconcile, startup_gate};
pub use snapshots::{BaselineStore, BlobStore, SnapshotStore};
pub use tags_seam::{PictureRef, TagDocument, TagTarget, TagUpdate, TagUpdates};
pub use undo::{BaselineRestorePlan, UndoItem, UndoPlan, plan_baseline_restore, plan_undo};

/// Reserved hidden namespace for staging and backup artifacts.
///
/// Discovery prunes files and directories under this prefix, even
/// if a future artifact name keeps an audio extension.
pub const HIDDEN_PREFIX: &str = ".droppedneedle-management-";

/// Maximum catalog subjects planned per transaction batch.
pub const MAX_PLAN_SUBJECTS: usize = 500;

/// Maximum recoverable bundles at startup before mutable runtime
/// refuses to start.
pub const STARTUP_RECOVERY_LIMIT: usize = 500;

/// Default per-operation undo retention in days.
pub const DEFAULT_UNDO_RETENTION_DAYS: u32 = 90;

/// What a publisher gate refused: the sentence a person reads, and the
/// track and file it was about when the gate knew them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub message: String,
    pub track_id: Option<String>,
    pub file: Option<FileAt>,
}

/// A file inside a library folder or bin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAt {
    pub root_id: String,
    pub rel: String,
}

impl std::fmt::Display for Detail {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl From<String> for Detail {
    fn from(message: String) -> Self {
        Self {
            message,
            track_id: None,
            file: None,
        }
    }
}

impl From<&str> for Detail {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

/// Publisher errors. Variants name the gate that failed
/// so callers can tell a durable skip (stale input, late collision)
/// from a deterministic failure.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    /// Capability gate: the format or field combination cannot be
    /// managed, so the bundle is blocked before any mutation.
    #[error("capability blocked: {0}")]
    Capability(Detail),
    /// Collision gate: an occupied or fold-colliding destination.
    #[error("collision: {0}")]
    Collision(Detail),
    /// Snapshot gate: a required snapshot or baseline is missing,
    /// expired, or unverifiable.
    #[error("snapshot: {0}")]
    Snapshot(Detail),
    /// Journal gate: the durable intent is missing, out of order, or
    /// disagrees with the filesystem.
    #[error("journal: {0}")]
    Journal(Detail),
    /// Validation gate: re-read bytes, fingerprints, identity, or
    /// policy no longer match the sealed preview.
    #[error("validation: {0}")]
    Validation(Detail),
    /// Catalog-commit gate: the bundle's own rows moved since its seal,
    /// or the commit transaction refused a row.
    #[error("catalog commit: {0}")]
    Catalog(Detail),
    /// Cleanup gate: post-commit source/backup removal failed. The
    /// commit stands; cleanup stays pending and retryable.
    #[error("cleanup pending: {0}")]
    Cleanup(Detail),
    /// Path safety: traversal, escape, symlink, or out-of-sandbox path.
    #[error("unsafe path: {0}")]
    UnsafePath(Detail),
    /// The library folder is not there now (a share not mounted, a link
    /// to nothing), so nothing under it is touched until it is back.
    #[error("the library folder is not there")]
    FolderMissing,
    /// Archive safety: zip-slip, traversal, symlink, size, or bomb limit.
    #[error("archive blocked: {0}")]
    Archive(Detail),
    /// Disk preflight: a folder has less free space than the bundle
    /// needs there.
    #[error("insufficient space: root {root_id} needs {needed} bytes but has {free} free")]
    Space {
        root_id: String,
        needed: u64,
        free: u64,
    },
    /// Placement gate: this folder offers no way to put the file on its
    /// name that cannot replace another file. The text is the sentence the
    /// user reads.
    #[error("{0}")]
    NoSafePlacement(Detail),
    /// Placement gate: the file could be moved out of the way but not put
    /// back safely, so it was not touched. The text is the sentence the
    /// user reads, and it names the file.
    #[error("{0}")]
    NotLinkable(Detail),
    /// Injected crash for the crash-matrix tests. Production never
    /// constructs this; tests use it to prove resume-or-compensate.
    #[error("injected crash at {0}")]
    InjectedCrash(CrashPoint),
    /// SQLite or serialization failure inside the publisher stores.
    #[error("store: {0}")]
    Store(String),
    /// The database refused the call before it ran (busy, or shutting
    /// down): nothing it would have written landed. A publish stops here
    /// and leaves the bundle to recovery.
    #[error("store: {0}")]
    Interrupted(String),
    /// A write ran past its time budget and was rolled back: nothing it
    /// would have written landed.
    #[error("store: {0}")]
    TimedOut(String),
    /// Filesystem I/O failure.
    #[error("io: {0}")]
    Io(Detail),
}

impl From<rusqlite::Error> for PublishError {
    fn from(err: rusqlite::Error) -> Self {
        Self::from(droppedneedle_kernel::db::DbError::from(err))
    }
}

impl From<droppedneedle_kernel::db::DbError> for PublishError {
    fn from(err: droppedneedle_kernel::db::DbError) -> Self {
        use droppedneedle_kernel::db::DbError;
        match err {
            DbError::Busy { .. } | DbError::LaneClosed => Self::Interrupted(err.to_string()),
            DbError::TxTimeout { .. } => Self::TimedOut(err.to_string()),
            other => Self::Store(other.to_string()),
        }
    }
}

impl PublishError {
    /// The database refused the call before it ran; see
    /// [`PublishError::Interrupted`].
    pub fn is_interrupted(&self) -> bool {
        matches!(self, Self::Interrupted(_))
    }

    /// The database refused the call or rolled it back past its time
    /// budget: nothing it would have written landed.
    pub fn wrote_nothing(&self) -> bool {
        matches!(self, Self::Interrupted(_) | Self::TimedOut(_))
    }

    /// The gate's detail, for the variants that carry one.
    pub fn detail(&self) -> Option<&Detail> {
        match self {
            Self::Capability(detail)
            | Self::Collision(detail)
            | Self::Snapshot(detail)
            | Self::Journal(detail)
            | Self::Validation(detail)
            | Self::Catalog(detail)
            | Self::Cleanup(detail)
            | Self::UnsafePath(detail)
            | Self::Archive(detail)
            | Self::NoSafePlacement(detail)
            | Self::NotLinkable(detail)
            | Self::Io(detail) => Some(detail),
            Self::FolderMissing
            | Self::Space { .. }
            | Self::InjectedCrash(_)
            | Self::Store(_)
            | Self::Interrupted(_)
            | Self::TimedOut(_) => None,
        }
    }

    fn detail_mut(&mut self) -> Option<&mut Detail> {
        match self {
            Self::Capability(detail)
            | Self::Collision(detail)
            | Self::Snapshot(detail)
            | Self::Journal(detail)
            | Self::Validation(detail)
            | Self::Catalog(detail)
            | Self::Cleanup(detail)
            | Self::UnsafePath(detail)
            | Self::Archive(detail)
            | Self::NoSafePlacement(detail)
            | Self::NotLinkable(detail)
            | Self::Io(detail) => Some(detail),
            Self::FolderMissing
            | Self::Space { .. }
            | Self::InjectedCrash(_)
            | Self::Store(_)
            | Self::Interrupted(_)
            | Self::TimedOut(_) => None,
        }
    }

    /// The same error, naming the track it was about unless it names one
    /// already.
    #[must_use]
    pub fn track(mut self, track_id: &str) -> Self {
        if let Some(detail) = self.detail_mut() {
            detail.track_id.get_or_insert_with(|| track_id.to_owned());
        }
        self
    }

    /// The same error, naming the file it was about unless it names one
    /// already.
    #[must_use]
    pub fn file(mut self, root_id: &str, rel: &str) -> Self {
        if let Some(detail) = self.detail_mut() {
            detail.file.get_or_insert_with(|| FileAt {
                root_id: root_id.to_owned(),
                rel: rel.to_owned(),
            });
        }
        self
    }
}

impl From<std::io::Error> for PublishError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string().into())
    }
}

impl From<serde_json::Error> for PublishError {
    fn from(err: serde_json::Error) -> Self {
        Self::Store(err.to_string())
    }
}
