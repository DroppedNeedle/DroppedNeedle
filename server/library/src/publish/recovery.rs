//! Startup reconciliation: resume or compensate, never guess.
//!
//! Recovery decides at album-bundle scope. It never treats file
//! existence as ownership: source, temp, backup, and destination are
//! inspected as symlink-free regular files and compared with their
//! journaled SHA-256 fingerprints. The state machine recognizes
//! process death both before and after each filesystem rename, even
//! when the corresponding SQLite transition did not run.
//!
//! Recovery runs the same steps a publish does (see `step`), from
//! wherever a crash left the bundle; a publish runs them from `Staged`.
//!
//! - A fully prepared bundle finishes publication and the pinned
//!   catalog transaction. Fully prepared means every journal row its
//!   recorded operation calls for exists and is staged (its temp
//!   written, its before-state kept); a bundle that stopped partway
//!   through staging is unwound, never finished.
//! - A rejected pre-commit bundle is compensated only when every
//!   original and staged copy is unambiguous.
//! - A bundle with a row already compensated or needing attention has
//!   begun to unwind. It is never resumed: its remaining rows are unwound.
//!   Unless that unwind found no safe way to put a file back: then the
//!   remaining rows need attention too and nothing more is moved.
//! - A resume that is refused unwinds the bundle, as a publish does: an
//!   original that cannot be set aside, a file with no safe way onto its
//!   name, or a destination another file holds. That file is never
//!   touched.
//! - Duplicate staged locations, changed bytes, unsafe paths, mixed
//!   commit states, or catalog/destination disagreement move every
//!   still-active row in the bundle to `needs_attention` with durable
//!   structured evidence, deleting nothing.
//! - A staged or backup file found under its hidden name and its visible
//!   name at once, as one file (same device and inode), is a
//!   link-then-unlink placement that stopped halfway. It is finished by
//!   removing the hidden name. Two separate files with the same bytes
//!   stay ambiguous. A set-aside generated file in that state is a
//!   put-back that stopped, so its bundle is unwound, never resumed.
//! - Recovery places files only through `place::place_from_hidden`, so
//!   it cannot replace a file either. A set-aside original that cannot
//!   go home stays under its backup name and its row needs attention.
//! - Recovery sets an original aside only through
//!   `place::set_aside_with`, as the publisher does. An original that
//!   could not come back, or that changed since the journal was written,
//!   is left under its own name and the bundle needs attention.
//! - Committed journals are never rolled back: recovery verifies the
//!   catalog still names the journaled destination with matching
//!   bytes, then retries the pinned cleanup policy.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{CancelFlag, Db, Durable};
use droppedneedle_kernel::jobs::queue::JobQueue;

use super::journal::{FileJournal, JournalKind, JournalState, JournalStore, JournalWriter};
use super::paths::Sandbox;
use super::place::{PlaceError, Seams, place_with};
use super::publisher::{durable, long_read};
use super::step::{AdoptionFrom, Advanced, Hooks, Run};
use super::{PublishError, paths};

/// What recovery did with one bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Finished the renames and committed the pinned transaction.
    ResumedCommitted,
    /// Unwound every pre-commit journal by byte comparison.
    Compensated,
    /// Retried post-commit cleanup to completion.
    CleanupFinished,
    /// Evidence was ambiguous; the bundle needs an administrator.
    NeedsAttention(String),
    /// This pass could not settle the bundle (its root is not usable, a
    /// path is unsafe, a file operation failed); it stays as it was for a
    /// later pass.
    Deferred(String),
    /// Nothing left to do; all journals already terminal.
    Noop,
}

/// One bundle's recovery result.
#[derive(Debug, Clone)]
pub struct BundleRecovery {
    /// Bundle id.
    pub bundle_id: String,
    /// What happened.
    pub action: RecoveryAction,
}

/// A hidden file under a library root that no journal row names: a
/// staged copy or a set-aside original whose row was lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leftover {
    pub root_id: String,
    /// The root's folder.
    pub root_path: String,
    /// Path under the root.
    pub rel_path: String,
}

/// What one leftover pass found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeftoverReport {
    /// Every leftover found, up to [`LEFTOVER_SCAN_MAX`].
    pub count: usize,
    /// The first [`LEFTOVER_LIST_MAX`] of them, by path.
    pub files: Vec<Leftover>,
}

/// A hidden name written, renamed or linked more recently than this may
/// belong to a publish in flight.
const LEFTOVER_MIN_AGE: Duration = Duration::from_secs(86_400);

/// Hidden names one pass holds while it checks them against the journal.
/// A pass that finds more stops walking and reports these.
pub const LEFTOVER_SCAN_MAX: usize = 10_000;

/// Leftovers a report lists.
pub const LEFTOVER_LIST_MAX: usize = 100;

/// Find hidden files under `roots` that no journal row names, as of
/// `now`. Reads folder listings and the metadata of hidden names only:
/// it opens no file, follows no symlink, and changes nothing. The import
/// folder and the placement probe folder belong to their own cleanups and
/// are skipped. `None` when `cancelled` turned true.
/// Blocking.
pub fn find_leftovers(
    db: &Db,
    roots: &[paths::Root],
    now: SystemTime,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<LeftoverReport>, PublishError> {
    use std::os::unix::fs::MetadataExt as _;
    let now_secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    let import_dir = format!("{}import", super::HIDDEN_PREFIX);
    // Keyed by the absolute path as the journal writes it.
    let mut found: BTreeMap<String, Leftover> = BTreeMap::new();
    'roots: for root in roots {
        let mut folders: Vec<PathBuf> = vec![root.dir.clone()];
        while let Some(folder) = folders.pop() {
            if cancelled() {
                return Ok(None);
            }
            let entries = match std::fs::read_dir(&folder) {
                Ok(entries) => entries,
                Err(error) => {
                    tracing::warn!(folder = %folder.display(), %error, "leftover pass could not list a folder");
                    continue;
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        tracing::warn!(folder = %folder.display(), %error, "leftover pass could not read a folder entry");
                        continue;
                    }
                };
                // Neither call follows a symlink.
                let kind = match entry.file_type() {
                    Ok(kind) => kind,
                    Err(error) => {
                        tracing::warn!(path = %entry.path().display(), %error, "leftover pass could not tell what an entry is");
                        continue;
                    }
                };
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with(super::HIDDEN_PREFIX) {
                    if kind.is_dir() {
                        folders.push(entry.path());
                    }
                    continue;
                }
                if kind.is_symlink() || name == import_dir || name == super::place::PROBE_DIR {
                    continue;
                }
                let meta = match entry.metadata() {
                    Ok(meta) => meta,
                    Err(error) => {
                        tracing::warn!(path = %entry.path().display(), %error, "leftover pass could not read a hidden file's times");
                        continue;
                    }
                };
                // A set-aside original keeps its old modified time; the
                // change time moves when it is renamed or linked.
                let touched = meta.mtime().max(meta.ctime());
                if now_secs.saturating_sub(touched) > LEFTOVER_MIN_AGE.as_secs() as i64 {
                    let path = entry.path();
                    let rel_path = path
                        .strip_prefix(&root.dir)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned();
                    found.insert(
                        path.to_string_lossy().into_owned(),
                        Leftover {
                            root_id: root.id.clone(),
                            root_path: root.dir.to_string_lossy().into_owned(),
                            rel_path,
                        },
                    );
                    if found.len() >= LEFTOVER_SCAN_MAX {
                        tracing::warn!(
                            limit = LEFTOVER_SCAN_MAX,
                            "leftover pass stopped early: too many hidden files to hold"
                        );
                        break 'roots;
                    }
                }
            }
        }
    }
    // The journal is read after the walk, so a row written while the walk
    // ran still clears its file, and no read is open while it walks.
    if !found.is_empty() {
        db.read_long(
            "publish.leftover_names",
            &CancelFlag::never(),
            |conn| -> Result<(), PublishError> {
                let mut named = conn.prepare_cached(super::sql::JOURNAL_NAMES.text())?;
                let mut rows = named.query([])?;
                while let Some(row) = rows.next()? {
                    found.remove(&row.get::<_, String>(0)?);
                    if let Some(backup) = row.get::<_, Option<String>>(1)? {
                        found.remove(&backup);
                    }
                }
                Ok(())
            },
        )?;
    }
    Ok(Some(LeftoverReport {
        count: found.len(),
        files: found.into_values().take(LEFTOVER_LIST_MAX).collect(),
    }))
}

/// Refuse mutable runtime when more than `STARTUP_RECOVERY_LIMIT`
/// bundles still have a row that has not committed. A committed bundle
/// waiting for its cleanup (a file waiting for its recycle bin) does not
/// count: it can only finish once writes run again.
pub fn startup_gate(db: &Db) -> Result<(), PublishError> {
    gate(db)
}

fn active_bundles(db: &Db) -> Result<Vec<String>, PublishError> {
    long_read(db, "publish.recovery_bundles", |conn| {
        JournalStore::new(conn).active_bundles()
    })
}

fn gate(db: &Db) -> Result<(), PublishError> {
    let precommit: i64 = long_read(db, "publish.recovery_gate", |conn| {
        Ok(conn
            .prepare_cached(super::sql::PRECOMMIT_BUNDLE_COUNT.text())?
            .query_row([], |row| row.get(0))?)
    })?;
    if precommit > super::STARTUP_RECOVERY_LIMIT as i64 {
        return Err(PublishError::Journal(
            format!("refusing startup with {precommit} recoverable bundles").into(),
        ));
    }
    Ok(())
}

/// Reconcile every bundle with nonterminal journals, in bundle-id
/// order. Runs at startup after schema ratchets and before scan,
/// import, acquisition, or operation workers. One bundle never stops
/// the others: a bundle naming a root that is no longer a usable library
/// root (removed or excluded), or one this pass fails on (an unsafe path,
/// a file operation, a write that ran out of time), is logged and left as
/// it is for a later pass; the publish maintenance tick retries it, and
/// its tracks take no new managed writes meanwhile. A database that
/// refuses a call (busy, or shutting down) ends the pass at that bundle,
/// since every later one would hear the same. Only the startup gate or an
/// unreadable journal fails the pass.
pub fn reconcile(
    db: &Db,
    sandbox: &Sandbox,
    jobs: &JobQueue,
    clock: &dyn Clock,
) -> Result<Vec<BundleRecovery>, PublishError> {
    reconcile_with(db, sandbox, jobs, clock, Seams::default())
}

/// [`reconcile`] on a filesystem made to lack a placement step. Tests
/// only; production never calls this.
#[cfg(any(test, feature = "test-support"))]
pub fn reconcile_with_seams(
    db: &Db,
    sandbox: &Sandbox,
    jobs: &JobQueue,
    clock: &dyn Clock,
    seams: Seams,
) -> Result<Vec<BundleRecovery>, PublishError> {
    reconcile_with(db, sandbox, jobs, clock, seams)
}

fn reconcile_with(
    db: &Db,
    sandbox: &Sandbox,
    jobs: &JobQueue,
    clock: &dyn Clock,
    seams: Seams,
) -> Result<Vec<BundleRecovery>, PublishError> {
    gate(db)?;
    let bundles = active_bundles(db)?;
    let mut results = Vec::new();
    for bundle_id in bundles {
        let action = match reconcile_bundle(db, sandbox, jobs, clock, &bundle_id, seams) {
            Ok(action) => action,
            Err(error) if error.is_interrupted() => {
                tracing::warn!(bundle_id, %error, "publish recovery stopped: the database refused it");
                results.push(BundleRecovery {
                    bundle_id,
                    action: RecoveryAction::Deferred(error.to_string()),
                });
                break;
            }
            Err(error) => {
                tracing::warn!(bundle_id, %error, "publish bundle left for the next recovery pass");
                RecoveryAction::Deferred(error.to_string())
            }
        };
        results.push(BundleRecovery { bundle_id, action });
    }
    Ok(results)
}

/// Reconcile one bundle through the steps a publish runs, from its rows
/// and its recorded operation. A bundle
/// naming a root that is not usable now (removed, excluded, or not
/// mounted) waits as it is: the root may come back, and the bundle may
/// hold a hidden backup there.
fn reconcile_bundle(
    db: &Db,
    sandbox: &Sandbox,
    jobs: &JobQueue,
    clock: &dyn Clock,
    bundle_id: &str,
    seams: Seams,
) -> Result<RecoveryAction, PublishError> {
    let operation = long_read(db, "publish.bundle_operation", |conn| {
        super::operations::operation_json(conn, bundle_id)
    })?;
    // Unwinding is always safe before a commit; a record that no longer
    // decodes must not hold the bundle's tracks for good.
    let plan = operation.and_then(|json| {
        serde_json::from_str::<super::planner::PlanBundle>(&json)
            .inspect_err(|error| {
                tracing::warn!(bundle_id, %error, "recorded operation does not decode");
            })
            .ok()
    });
    let now_ms = clock.now_ms();
    let temps = BTreeMap::new();
    let known = paths::Known::default();
    let run = Run {
        db,
        sandbox,
        jobs,
        seams,
        bin_seams: Seams::default(),
        now_ms,
        clock,
        adoption: AdoptionFrom::Provenance,
        known_removals: false,
        temps: &temps,
        known: &known,
        hooks: Hooks::default(),
    };
    Ok(match run.advance(bundle_id, plan.as_ref())? {
        Advanced::Noop => RecoveryAction::Noop,
        Advanced::FoldersWait => RecoveryAction::Deferred(format!(
            "bundle {bundle_id} has folders waiting for their library folder"
        )),
        Advanced::Waiting(root) => {
            tracing::warn!(bundle_id, root, "publish bundle waits for its root");
            RecoveryAction::Deferred(format!(
                "bundle {bundle_id} names root {root}, which is not a usable library root"
            ))
        }
        Advanced::Attention(reason) => RecoveryAction::NeedsAttention(reason),
        Advanced::Unwound { refused, kept } => match (kept, refused) {
            (None, _) => RecoveryAction::Compensated,
            (Some(kept), None) => RecoveryAction::NeedsAttention(kept),
            (Some(kept), Some(refused)) => {
                RecoveryAction::NeedsAttention(format!("{}; then {kept}", refused.reason))
            }
        },
        // Nothing committed, and the bundle is unwound; the next pass
        // finds it settled.
        Advanced::CommitRefused(error) => return Err(error),
        Advanced::Committed { .. } => RecoveryAction::ResumedCommitted,
        Advanced::CleanupFinished => RecoveryAction::CleanupFinished,
    })
}

/// Remove the folders a bundle made, once its unwind finished. A failure
/// leaves them recorded, and a later pass removes them.
pub(super) fn remove_folders(db: &Db, sandbox: &Sandbox, bundle_id: &str) {
    if let Err(error) = super::folders::remove(db, sandbox, bundle_id) {
        tracing::warn!(bundle_id, %error, "folders a write made wait for a later pass");
    }
}

/// Move every still-active row in the bundle to `needs_attention`, in one
/// durable write.
pub(super) fn flag_attention(db: &Db, journals: &[FileJournal]) -> Result<(), PublishError> {
    durable(db, "publish.journal_attention", |tx| {
        flag_rows(tx, journals)
    })
}

pub(super) fn flag_rows(tx: &Durable<'_>, journals: &[FileJournal]) -> Result<(), PublishError> {
    let writer = JournalWriter::new(tx);
    for journal in journals {
        if journal.state.is_terminal() {
            continue;
        }
        if !writer.transition(&journal.id, journal.state, JournalState::NeedsAttention)? {
            tracing::warn!(journal_id = %journal.id, "needs-attention not recorded: state moved");
        }
    }
    Ok(())
}

/// A bundle's rows in the order a publish or resume steps through them:
/// every file the bundle takes away first, so a file it then places finds
/// its path free.
pub(super) fn removals_first(journals: &[FileJournal]) -> Vec<&FileJournal> {
    let (removals, rest): (Vec<_>, Vec<_>) = journals
        .iter()
        .partition(|journal| journal.kind == JournalKind::Remove);
    removals.into_iter().chain(rest).collect()
}

/// A bundle's rows in the order an unwind steps through them: every
/// placed file comes off before a file the bundle took away goes home.
pub(super) fn placements_first(journals: &[FileJournal]) -> Vec<&FileJournal> {
    let (removals, rest): (Vec<_>, Vec<_>) = journals
        .iter()
        .partition(|journal| journal.kind == JournalKind::Remove);
    rest.into_iter().chain(removals).collect()
}

/// Compensate a pre-commit bundle, for the publisher after a failed
/// publish and for recovery: unwind every journal by byte comparison and
/// mark it compensated.
///
/// A row whose set-aside original is still under its backup name after
/// the unwind is never compensated: it goes to `needs_attention` with the
/// backup kept, and the reason is returned. When the original has no safe
/// way home at all, it stays under its backup name, nothing more is moved
/// and every row still active goes with it. `seams` is the default
/// outside tests.
pub(super) fn compensate_bundle(
    db: &Db,
    sandbox: &Sandbox,
    journals: &[FileJournal],
    seams: Seams,
) -> Result<Option<String>, PublishError> {
    let mut attention = None;
    let journals = placements_first(journals);
    for (index, journal) in journals.iter().copied().enumerate() {
        if journal.state.is_terminal()
            || matches!(
                journal.state,
                JournalState::Committed | JournalState::CleanupPending
            )
        {
            continue;
        }
        let next = match unwind_row(sandbox, journal, seams)? {
            Unwound::Clean => JournalState::Compensated,
            Unwound::BackupKept(reason) => {
                tracing::warn!(
                    journal_id = %journal.id,
                    reason,
                    "set-aside original kept under its backup name"
                );
                attention.get_or_insert(reason);
                JournalState::NeedsAttention
            }
            Unwound::NoSafeMethod(reason) => {
                let rest: Vec<FileJournal> = journals[index..]
                    .iter()
                    .map(|journal| (*journal).clone())
                    .collect();
                flag_attention(db, &rest)?;
                return Ok(Some(reason));
            }
        };
        let recorded = durable(db, "publish.journal_unwound", |tx| {
            JournalWriter::new(tx).transition(&journal.id, journal.state, next)
        })?;
        if !recorded {
            tracing::warn!(journal_id = %journal.id, "unwind not recorded: state moved");
        }
    }
    Ok(attention)
}

/// How one journal's unwind ended.
enum Unwound {
    /// Everything of the user's is back under its own name.
    Clean,
    /// The set-aside original, or a set-aside generated file, is still
    /// under its hidden name, and why.
    BackupKept(String),
    /// The original has no safe way back onto its name and stays under
    /// its hidden name. A placed copy that held the name is already gone.
    NoSafeMethod(String),
}

/// Unwind one pre-commit journal by byte comparison only: remove a
/// published destination that still holds the staged bytes, put a
/// set-aside original back on an empty name, and delete the staged temp.
/// A backup whose bytes changed or whose home is taken is decided before
/// anything is removed.
fn unwind_row(
    sandbox: &Sandbox,
    journal: &FileJournal,
    seams: Seams,
) -> Result<Unwound, PublishError> {
    let is_delete = match journal.kind {
        JournalKind::Remove => {
            // A removal's hidden copy is the user's file: it goes home, or
            // stays where it is when it cannot.
            let dest = sandbox.resolve_removal(&journal.dest_root, &journal.dest_rel)?;
            let aside = Path::new(&journal.staged);
            sandbox.ensure_under_removal_roots(aside)?;
            return Ok(
                match super::remove::put_back(
                    sandbox.placement_mode(&journal.dest_root),
                    &dest,
                    aside,
                    &journal.staged_sha256,
                    seams,
                )? {
                    super::remove::PutBack::Home => Unwound::Clean,
                    super::remove::PutBack::Kept(why) => Unwound::BackupKept(format!(
                        "journal {} keeps its file under a hidden name: {why}",
                        journal.id
                    )),
                    super::remove::PutBack::NoSafeWay => Unwound::NoSafeMethod(format!(
                        "journal {} has no safe way to put its file back",
                        journal.id
                    )),
                },
            );
        }
        JournalKind::Delete => true,
        JournalKind::Audio
        | JournalKind::Sidecar
        | JournalKind::Artwork
        | JournalKind::Output
        | JournalKind::Add
        | JournalKind::PutBack => false,
    };
    let dest = match journal.kind {
        // A put-back may go home under a remove-only root.
        JournalKind::PutBack => {
            let dest = sandbox.resolve_removal(&journal.dest_root, &journal.dest_rel)?;
            let staged = Path::new(&journal.staged);
            sandbox.ensure_under_removal_roots(staged)?;
            // Its temp is new under the row's own name: one still being
            // written is a part copy, and goes.
            if journal.state == JournalState::Prepared
                && std::fs::symlink_metadata(staged).is_ok()
                && !paths::same_file(staged, &dest)
            {
                std::fs::remove_file(staged).map_err(PublishError::from)?;
            }
            dest
        }
        // Its temp is new under the row's own name, so one still being
        // written is a part copy, and goes.
        JournalKind::Add => {
            let dest = sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
            let staged = Path::new(&journal.staged);
            sandbox.ensure_under_roots(staged)?;
            if journal.state == JournalState::Prepared
                && std::fs::symlink_metadata(staged).is_ok()
                && !paths::same_file(staged, &dest)
            {
                std::fs::remove_file(staged).map_err(PublishError::from)?;
            }
            dest
        }
        // The same for a staged copy: a row still being staged was never
        // placed, so its temp, whatever it holds, is a part copy.
        JournalKind::Audio | JournalKind::Sidecar | JournalKind::Artwork | JournalKind::Output => {
            let dest = sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
            let staged = Path::new(&journal.staged);
            if journal.state == JournalState::Prepared
                && std::fs::symlink_metadata(staged).is_ok()
                && !paths::same_file(staged, &dest)
            {
                sandbox.ensure_under_roots(staged)?;
                std::fs::remove_file(staged).map_err(PublishError::from)?;
            }
            dest
        }
        JournalKind::Delete | JournalKind::Remove => {
            sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?
        }
    };
    if is_delete {
        // A delete journal's staged copy is the generated file itself.
        let put_back = super::generated::put_back(
            sandbox.placement_mode(&journal.dest_root),
            &dest,
            Path::new(&journal.staged),
            &journal.staged_sha256,
            seams,
        );
        return match put_back {
            Ok(()) => Ok(Unwound::Clean),
            Err(PlaceError::Collision) => Ok(Unwound::BackupKept(format!(
                "another file took the name journal {} set its generated file aside from",
                journal.id
            ))),
            Err(PlaceError::Unavailable(_) | PlaceError::CrossDevice) => {
                Ok(Unwound::NoSafeMethod(format!(
                    "journal {} has no safe way to put its generated file back",
                    journal.id
                )))
            }
            Err(error) => Err(error.into_publish(&journal.dest_root, &journal.dest_rel)),
        };
    }
    let staged = Path::new(&journal.staged);
    // Matching bytes do not make the destination the placed copy: a row
    // with no tag change stages the original's own bytes. It is ours only
    // when the staged temp went there (it is gone, or is this same file)
    // and, for a file written in place, the original was set aside first.
    // A row still `Prepared` was never placed: its temp may be missing
    // only because it was never written.
    let staged_bytes_at_dest = paths::holds(&dest, &journal.staged_sha256);
    let temp_went_there = journal.state != JournalState::Prepared
        && (std::fs::symlink_metadata(staged).is_err() || paths::same_file(staged, &dest));
    let in_place = journal.source.as_ref().is_some_and(|(root, rel)| {
        sandbox
            .resolve_no_symlink(root, rel)
            .is_ok_and(|source| source == dest)
            || paths::case_alias(
                sandbox,
                (root, rel),
                (&journal.dest_root, &journal.dest_rel),
            )
    });
    let placed_at_dest = |original_aside: bool| {
        staged_bytes_at_dest && temp_went_there && (original_aside || !in_place)
    };
    // A journaled backup path with no file is normal: the original never
    // moved, or it is already home.
    let backup = journal
        .backup
        .as_deref()
        .map(Path::new)
        .filter(|backup| std::fs::symlink_metadata(backup).is_ok());
    let mut put_back = None;
    if let Some(backup) = backup {
        if sandbox.ensure_under_roots(backup).is_err() {
            return Ok(Unwound::BackupKept(format!(
                "journal {} backup escapes sandbox",
                journal.id
            )));
        }
        let home = paths::backup_home(
            sandbox,
            journal.source.as_ref(),
            &journal.dest_root,
            &journal.dest_rel,
        )?;
        if paths::same_file(backup, &home) {
            // An earlier put-back linked the original home and stopped:
            // only the hidden name is left to remove.
            std::fs::remove_file(backup).map_err(PublishError::from)?;
        } else {
            let expected = journal.backup_sha256().unwrap_or_default();
            let intact = if expected.is_empty() {
                paths::FileStat::read(backup).is_ok()
            } else {
                paths::holds(backup, expected)
            };
            if !intact {
                return Ok(Unwound::BackupKept(format!(
                    "journal {} backup bytes changed",
                    journal.id
                )));
            }
            if std::fs::symlink_metadata(&dest).is_ok() && !placed_at_dest(true) {
                return Ok(Unwound::BackupKept(format!(
                    "another file holds the name journal {} set its original aside from",
                    journal.id
                )));
            }
            put_back = Some((backup, home));
        }
    }
    if placed_at_dest(put_back.is_some()) {
        std::fs::remove_file(&dest).map_err(PublishError::from)?;
    }
    if let Some((backup, home)) = put_back {
        let mode = sandbox.placement_mode(&journal.dest_root);
        match place_with(mode, backup, &home, seams) {
            Ok(_) => {}
            // The original is home; a folder sync that failed does not
            // stop the rest of the unwind.
            Err(PlaceError::Unsynced(error)) => {
                tracing::warn!(journal_id = %journal.id, %error, "directory sync failed");
            }
            Err(PlaceError::Collision) => {
                return Ok(Unwound::BackupKept(format!(
                    "another file took the name journal {} set its original aside from",
                    journal.id
                )));
            }
            Err(PlaceError::Unavailable(_) | PlaceError::CrossDevice) => {
                return Ok(Unwound::NoSafeMethod(format!(
                    "journal {} has no safe way to put its original back",
                    journal.id
                )));
            }
            Err(error) => return Err(error.into_publish(&journal.dest_root, &journal.dest_rel)),
        }
    }
    if paths::holds(staged, &journal.staged_sha256) {
        std::fs::remove_file(staged).map_err(PublishError::from)?;
    }
    if let Some(parent) = dest.parent()
        && let Err(error) = super::journal::fsync_dir(parent)
    {
        tracing::warn!(dir = %parent.display(), %error, "directory sync failed");
    }
    Ok(Unwound::Clean)
}
