//! Library Management publisher slice (stage 8).
//!
//! This is the only file-writing boundary for managed audio: a planner
//! builds an immutable preview, the administrator seals it, and the staged
//! publisher moves bytes through prepare, publish, catalog commit, and
//! cleanup. No other slice writes managed files.
//!
//! Authority: `.dev-notes/Plans/*Archive*/BeetsPicard/07-decisions.md`
//! (owner-signed D-decisions plus engineering E-decisions) and the live
//! acceptance checklist in `10-live-acceptance-checklist.md`. The port
//! keeps the backend behavior from
//! `backend/services/native/library_management_publisher.py` and its
//! recovery/undo/baseline siblings; nothing working there is removed.
//!
//! # Crash semantics (normative for this slice)
//!
//! SQLite and the filesystem are never claimed as one transaction (E3).
//! Instead every audio file, sidecar, and external-art output owns a
//! durable journal intent with a monotonic state machine
//! (`journal::JournalState`), and the whole album bundle moves through
//! one protocol:
//!
//! ```text
//! prepare -> publish -> catalog-commit -> cleanup
//! ```
//!
//! - **Prepare** writes destination-side hidden temps
//!   (`.droppedneedle-management-*`), re-reads and validates them, fsyncs
//!   file bytes, then fsyncs the containing directory. Journal:
//!   `prepared -> staged`. A crash here leaves only hidden temps, which
//!   recovery deletes after fingerprint comparison.
//! - **Publish** rechecks the sealed preview (settings, profile, naming,
//!   policy, accepted identity, override revisions, fingerprints, exact
//!   and Unicode/case-fold collisions) under one exclusive multi-root
//!   lease, then renames each staged temp onto its destination. Renames
//!   are same-directory `rename(2)`, so each file is atomic; a crash
//!   between two files is resolved per journal by comparing staged,
//!   destination, source, and backup fingerprints, never by assuming
//!   path existence means ownership. Journal: `staged -> published`.
//! - **Catalog commit** is one SQLite transaction guarded by
//!   compare-and-swap on catalog and subject revisions. Journal:
//!   `published -> committed`. A crash between the last rename and the
//!   commit resumes the commit only when every destination still holds
//!   the exact staged bytes.
//! - **Cleanup** removes verified old sources and backups, then prunes
//!   newly empty source directories. Journal: `committed -> cleaned`.
//!   Cleanup failure is durable `cleanup_pending` and retryable; it never
//!   rolls back the commit.
//!
//! Failure before commit compensates: published destinations whose bytes
//! still match the staged copy are removed, same-path backups are
//! restored, and unpublished temps are deleted. Committed journals are
//! never rolled back. Anything ambiguous (duplicate staged locations,
//! changed bytes, unsafe paths, mixed commit states, catalog/destination
//! disagreement) moves the whole bundle to `needs_attention` with
//! structured evidence and deletes nothing.
//!
//! Supporting rules, all enforced here and covered by briefs in
//! `server/tests/it/library_publish.rs`:
//!
//! - Same-filesystem rename only: temps live in the destination
//!   directory, so a cross-device move is impossible by construction.
//! - Disk preflight runs before the first byte is staged.
//! - Destinations are never overwritten while occupied (D10), even by
//!   identical bytes; resolution is explicit.
//! - Media symlinks are never followed (E11); symlinked inputs or path
//!   components fail closed.
//! - Hardlinked audio publishes to an independent destination inode; the
//!   other directory entry is untouched.
//! - Snapshots are content-addressed (SHA-256) and deduplicated (D11);
//!   first-management baselines are immutable and indefinite, while
//!   per-operation snapshots expire.
//! - Case/Unicode collisions use the NFC + full-casefold key, so two
//!   names that fold together can never both publish.
//! - Every publisher write lands under a sandbox root; anything else is
//!   rejected before any mutation.

pub mod archive;
pub mod journal;
pub mod operations;
pub mod paths;
pub mod planner;
pub mod publisher;
pub mod recovery;
pub mod snapshots;
pub mod staging;
pub mod tags_seam;
pub mod undo;

pub use archive::{ArchiveBlock, ArchiveEntry, ArchivePolicy, ArchiveReport, validate_archive};
pub use journal::{FileJournal, JournalKind, JournalState, JournalStore, apply_schema, fsync_dir};
pub use paths::{CollisionKey, Sandbox, collision_key};
pub use planner::{
    AutomaticEligibility, AutomaticHold, Capability, CapabilityGate, DiskPreflight,
    FileFingerprint, PlanBundle, PlanItem, PlanKind, ReleaseIdentity, SealError, SealedPreview,
    SpaceProbe, TrackMapping,
};
pub use publisher::{Catalog, CrashPoint, NullCatalog, PublishOutcome, Publisher, SqliteCatalog};
pub use recovery::{BundleRecovery, RecoveryAction, reconcile, startup_gate};
pub use snapshots::{BaselineStore, BlobStore, SnapshotRef, SnapshotStore};
pub use tags_seam::TagDocument;
pub use undo::{BaselineRestorePlan, UndoItem, UndoPlan, plan_baseline_restore, plan_undo};

/// Reserved hidden namespace for staging and backup artifacts.
///
/// Discovery prunes files and directories under this prefix (E28), even
/// if a future artifact name keeps an audio extension.
pub const HIDDEN_PREFIX: &str = ".droppedneedle-management-";

/// Maximum catalog subjects planned per transaction batch (E15).
pub const MAX_PLAN_SUBJECTS: usize = 500;

/// Maximum recoverable bundles at startup before mutable runtime
/// refuses to start (E30).
pub const STARTUP_RECOVERY_LIMIT: usize = 500;

/// Default per-operation undo retention in days (D11).
pub const DEFAULT_UNDO_RETENTION_DAYS: u32 = 90;

/// Errors for the publisher slice. Variants name the gate that failed
/// so callers can tell a durable skip (stale input, late collision)
/// from a deterministic failure.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    /// Capability gate: the format or field combination cannot be
    /// managed, so the bundle is blocked before any mutation.
    #[error("capability blocked: {0}")]
    Capability(String),
    /// Collision gate: an occupied or fold-colliding destination.
    #[error("collision: {0}")]
    Collision(String),
    /// Snapshot gate: a required snapshot or baseline is missing,
    /// expired, or unverifiable.
    #[error("snapshot: {0}")]
    Snapshot(String),
    /// Journal gate: the durable intent is missing, out of order, or
    /// disagrees with the filesystem.
    #[error("journal: {0}")]
    Journal(String),
    /// Validation gate: re-read bytes, fingerprints, identity, or
    /// policy no longer match the sealed preview.
    #[error("validation: {0}")]
    Validation(String),
    /// Catalog-commit gate: the compare-and-swap transaction failed.
    #[error("catalog commit: {0}")]
    Catalog(String),
    /// Cleanup gate: post-commit source/backup removal failed. The
    /// commit stands; cleanup stays pending and retryable.
    #[error("cleanup pending: {0}")]
    Cleanup(String),
    /// Cache invalidation ran after commit and reported a fault. Like
    /// external notification (D27), this warns and never rolls back.
    #[error("cache invalidation: {0}")]
    CacheInvalidation(String),
    /// Path safety: traversal, escape, symlink, or out-of-sandbox path.
    #[error("unsafe path: {0}")]
    UnsafePath(String),
    /// Archive safety: zip-slip, traversal, symlink, size, or bomb limit.
    #[error("archive blocked: {0}")]
    Archive(String),
    /// Disk preflight: insufficient destination space.
    #[error("insufficient space: {0}")]
    Space(String),
    /// Injected crash for the crash-matrix briefs. Production never
    /// constructs this; tests use it to prove resume-or-compensate.
    #[error("injected crash at {0}")]
    InjectedCrash(String),
    /// SQLite or serialization failure inside the slice stores.
    #[error("store: {0}")]
    Store(String),
    /// Filesystem I/O failure.
    #[error("io: {0}")]
    Io(String),
}

impl From<rusqlite::Error> for PublishError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Store(err.to_string())
    }
}

impl From<std::io::Error> for PublishError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

impl From<serde_json::Error> for PublishError {
    fn from(err: serde_json::Error) -> Self {
        Self::Store(err.to_string())
    }
}
