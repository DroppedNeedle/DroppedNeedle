//! Startup reconciliation: resume or compensate, never guess.
//!
//! Recovery decides at album-bundle scope. It never treats file
//! existence as ownership: source, temp, backup, and destination are
//! inspected as symlink-free regular files and compared with their
//! journaled SHA-256 fingerprints. The state machine recognizes
//! process death both before and after each filesystem rename, even
//! when the corresponding SQLite transition did not run.
//!
//! - A fully prepared bundle finishes publication and the pinned
//!   catalog transaction.
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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::Connection;

use super::journal::{FileJournal, JournalKind, JournalState, JournalStore};
use super::paths::Sandbox;
use super::place::{
    Original, PlaceError, Seams, SetAsideError, place_from_hidden, place_with, set_aside_with,
};
use super::publisher::{BundleCommit, Catalog, TrackCommit};
use super::snapshots::sha256_hex;
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

/// Open the connection a recovery pass runs on, through the database
/// factory. Recovery writes journal rows and then moves files, so its
/// rows are synced like the publisher's.
pub fn open_recovery_connection(db_path: &Path) -> Result<Connection, PublishError> {
    crate::db::open_journal_connection(db_path)
        .map_err(|error| PublishError::Store(error.to_string()))
}

/// A hidden file under a library root that no journal row names: a
/// staged copy or a set-aside original whose row was lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leftover {
    pub root_id: String,
    /// Path under the root.
    pub rel_path: String,
    /// When the file was last written, unix seconds.
    pub modified_at: i64,
}

/// What one leftover pass found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeftoverReport {
    /// Every leftover found, up to [`LEFTOVER_SCAN_MAX`].
    pub count: usize,
    /// The first [`LEFTOVER_LIST_MAX`] of them, by path.
    pub files: Vec<Leftover>,
}

/// A hidden name younger than this may belong to a publish in flight.
const LEFTOVER_MIN_AGE: Duration = Duration::from_secs(86_400);

/// Hidden names one pass holds while it checks them against the journal.
/// A pass that finds more stops walking and reports these.
pub const LEFTOVER_SCAN_MAX: usize = 10_000;

/// Leftovers a report lists.
pub const LEFTOVER_LIST_MAX: usize = 100;

/// Find hidden files under `roots` that no journal row names, as of
/// `now`. Reads folder listings and the metadata of hidden names only:
/// it opens no file, follows no symlink, and changes nothing. The import
/// folder, the placement probe folder and plugin temps belong to their
/// own cleanups and are skipped. `None` when `cancelled` turned true.
/// Blocking.
pub fn find_leftovers(
    conn: &Connection,
    roots: &[paths::Root],
    now: SystemTime,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<LeftoverReport>, PublishError> {
    let import_dir = format!("{}import", super::HIDDEN_PREFIX);
    let plugin_temp = format!("{}{}", super::HIDDEN_PREFIX, super::plugin_files::TEMP_TAG);
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
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with(super::HIDDEN_PREFIX) {
                    if kind.is_dir() {
                        folders.push(entry.path());
                    }
                    continue;
                }
                if kind.is_symlink()
                    || name == import_dir
                    || name == super::place::PROBE_DIR
                    || name.starts_with(&plugin_temp)
                {
                    continue;
                }
                let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
                    continue;
                };
                if now
                    .duration_since(modified)
                    .is_ok_and(|age| age > LEFTOVER_MIN_AGE)
                {
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
                            rel_path,
                            modified_at: modified
                                .duration_since(SystemTime::UNIX_EPOCH)
                                .map_or(0, |since| since.as_secs() as i64),
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
    // ran still clears its file.
    if !found.is_empty() {
        let mut named = conn.prepare("SELECT staged, backup FROM library_publish_journal")?;
        let mut rows = named.query([])?;
        while let Some(row) = rows.next()? {
            found.remove(&row.get::<_, String>(0)?);
            if let Some(backup) = row.get::<_, Option<String>>(1)? {
                found.remove(&backup);
            }
        }
    }
    Ok(Some(LeftoverReport {
        count: found.len(),
        files: found.into_values().take(LEFTOVER_LIST_MAX).collect(),
    }))
}

/// Refuse mutable runtime when more than `STARTUP_RECOVERY_LIMIT`
/// bundles still hold nonterminal journals.
pub fn startup_gate(conn: &Connection) -> Result<(), PublishError> {
    let active = JournalStore::new(conn).active_bundles()?;
    if active.len() > super::STARTUP_RECOVERY_LIMIT {
        return Err(PublishError::Journal(format!(
            "refusing startup with {} recoverable bundles",
            active.len()
        )));
    }
    Ok(())
}

/// Reconcile every bundle with nonterminal journals, in bundle-id
/// order. Runs at startup after schema ratchets and before scan,
/// import, acquisition, or operation workers. One bundle never stops
/// the others: a bundle naming a root that is no longer a usable library
/// root (removed or excluded), or one this pass fails on (an unsafe path,
/// a file operation), is logged and left as it is for a later pass; the
/// publish maintenance tick retries it, and its tracks take no new
/// managed writes meanwhile. Only the startup gate or an unreadable
/// journal fails the pass.
pub fn reconcile<C: Catalog>(
    conn: &mut Connection,
    sandbox: &Sandbox,
    catalog: &C,
) -> Result<Vec<BundleRecovery>, PublishError> {
    reconcile_with(conn, sandbox, catalog, Seams::default())
}

/// [`reconcile`] on a filesystem made to lack a placement step. Tests
/// only; production never calls this.
#[cfg(any(test, feature = "test-support"))]
pub fn reconcile_with_seams<C: Catalog>(
    conn: &mut Connection,
    sandbox: &Sandbox,
    catalog: &C,
    seams: Seams,
) -> Result<Vec<BundleRecovery>, PublishError> {
    reconcile_with(conn, sandbox, catalog, seams)
}

fn reconcile_with<C: Catalog>(
    conn: &mut Connection,
    sandbox: &Sandbox,
    catalog: &C,
    seams: Seams,
) -> Result<Vec<BundleRecovery>, PublishError> {
    startup_gate(conn)?;
    let bundles = JournalStore::new(conn).active_bundles()?;
    let mut results = Vec::new();
    for bundle_id in bundles {
        let journals = JournalStore::new(conn).bundle(&bundle_id)?;
        let action = if let Some(root) = unusable_root(sandbox, &journals) {
            // The root may come back (a re-added or re-included root), and
            // the bundle may hold a hidden backup there: keep it as it is.
            tracing::warn!(bundle_id, root, "publish bundle waits for its root");
            RecoveryAction::Deferred(format!(
                "bundle {bundle_id} names root {root}, which is not a usable library root"
            ))
        } else {
            match reconcile_bundle(conn, sandbox, catalog, &bundle_id, seams) {
                Ok(action) => action,
                Err(error) => {
                    tracing::warn!(bundle_id, %error, "publish bundle left for the next recovery pass");
                    RecoveryAction::Deferred(error.to_string())
                }
            }
        };
        results.push(BundleRecovery { bundle_id, action });
    }
    Ok(results)
}

/// The first root a bundle's active journals name that the sandbox does
/// not hold.
fn unusable_root(sandbox: &Sandbox, journals: &[FileJournal]) -> Option<String> {
    journals
        .iter()
        .filter(|journal| !journal.state.is_terminal())
        .flat_map(|journal| {
            std::iter::once(journal.dest_root.as_str())
                .chain(journal.source.as_ref().map(|(root, _)| root.as_str()))
        })
        .find(|root| sandbox.root_dir(root).is_err())
        .map(str::to_owned)
}

/// Reconcile one bundle.
fn reconcile_bundle<C: Catalog>(
    conn: &mut Connection,
    sandbox: &Sandbox,
    catalog: &C,
    bundle_id: &str,
    seams: Seams,
) -> Result<RecoveryAction, PublishError> {
    let journals = JournalStore::new(conn).bundle(bundle_id)?;
    if journals.iter().all(|journal| journal.state.is_terminal()) {
        return Ok(RecoveryAction::Noop);
    }
    let evidence = match inspect_bundle(sandbox, &journals)? {
        BundleEvidence::Ambiguous(reason) => {
            flag_attention(conn, &journals)?;
            return Ok(RecoveryAction::NeedsAttention(reason));
        }
        BundleEvidence::Clear(clear) => clear,
    };
    // Whatever comes next may place a file: find out how each root the
    // bundle targets can do that, if nothing has yet.
    let roots: BTreeSet<&str> = journals
        .iter()
        .filter(|journal| !journal.state.is_terminal())
        .map(|journal| journal.dest_root.as_str())
        .collect();
    for root in roots {
        sandbox.probe_placement(root)?;
    }
    if evidence.any_committed {
        return reconcile_committed(conn, sandbox, catalog, bundle_id, &journals);
    }
    // A compensated or needs-attention row means an unwind already began
    // and stopped, between two rows or on an error. Such a bundle can
    // never commit, so it is never resumed.
    let unwinding = journals.iter().any(|journal| {
        matches!(
            journal.state,
            JournalState::Compensated | JournalState::NeedsAttention
        )
    });
    if unwinding && put_back_refused(sandbox, &journals) {
        flag_attention(conn, &journals)?;
        return Ok(RecoveryAction::NeedsAttention(format!(
            "bundle {bundle_id} has a file with no safe way back onto its name"
        )));
    }
    // A resume that is refused unwinds, as a publish does: the files
    // placed so far come off and their originals go home.
    let mut refused = None;
    if evidence.resumeable && !unwinding {
        match resume_precommit(conn, sandbox, catalog, bundle_id, &journals, seams)? {
            None => return Ok(RecoveryAction::ResumedCommitted),
            Some(reason) => {
                tracing::warn!(bundle_id, reason, "publish bundle unwound after its resume");
                refused = Some(reason);
            }
        }
    }
    let journals = JournalStore::new(conn).bundle(bundle_id)?;
    Ok(
        match (compensate_bundle(conn, sandbox, &journals, seams)?, refused) {
            (None, _) => RecoveryAction::Compensated,
            (Some(kept), None) => RecoveryAction::NeedsAttention(kept),
            (Some(kept), Some(refused)) => {
                RecoveryAction::NeedsAttention(format!("{refused}; then {kept}"))
            }
        },
    )
}

/// Whether an earlier unwind found no safe way to put a file back: a
/// needs-attention row whose set-aside file is still under its hidden
/// name, with the bytes the journal recorded, while its own name is free.
/// That unwind moved nothing more, since each further row would lose its
/// placed copy and not get its original back, and recovery does not
/// either. A hidden file with other bytes was kept for that reason alone,
/// and the unwind went on past it.
fn put_back_refused(sandbox: &Sandbox, journals: &[FileJournal]) -> bool {
    journals
        .iter()
        .filter(|journal| journal.state == JournalState::NeedsAttention)
        .any(|journal| {
            let (hidden, sha256, home) = if journal.kind == JournalKind::Delete {
                (
                    Some(journal.staged.as_str()),
                    Some(journal.staged_sha256.as_str()),
                    sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel),
                )
            } else {
                (
                    journal.backup.as_deref(),
                    journal.backup_sha256(),
                    paths::backup_home(
                        sandbox,
                        journal.source.as_ref(),
                        &journal.dest_root,
                        &journal.dest_rel,
                    ),
                )
            };
            let (Some(hidden), Ok(home)) = (hidden, home) else {
                return false;
            };
            let expected = sha256.unwrap_or_default();
            let intact = paths::read_regular_file(Path::new(hidden))
                .is_ok_and(|bytes| expected.is_empty() || sha256_hex(&bytes) == expected);
            intact && std::fs::symlink_metadata(&home).is_err()
        })
}

/// Filesystem truth for one bundle, by fingerprint comparison only.
struct ClearEvidence {
    any_committed: bool,
    resumeable: bool,
}

enum BundleEvidence {
    Clear(ClearEvidence),
    Ambiguous(String),
}

/// Inspect every journal path: each must resolve inside the sandbox
/// with no symlinks, and every present file must match either its
/// staged or its source fingerprint. Anything else is ambiguous.
fn inspect_bundle(
    sandbox: &Sandbox,
    journals: &[FileJournal],
) -> Result<BundleEvidence, PublishError> {
    let mut any_committed = false;
    let mut resumeable = true;
    for journal in journals {
        if journal.state.is_terminal() {
            continue;
        }
        if matches!(
            journal.state,
            JournalState::Committed | JournalState::CleanupPending
        ) {
            any_committed = true;
            continue;
        }
        let dest = match sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel) {
            Ok(path) => path,
            Err(_) => {
                return Ok(BundleEvidence::Ambiguous(format!(
                    "journal {} destination unsafe",
                    journal.id
                )));
            }
        };
        sandbox.ensure_under_roots(&dest).map_err(|_| {
            PublishError::Journal(format!("journal {} escapes sandbox", journal.id))
        })?;
        let staged = Path::new(&journal.staged);
        if sandbox.ensure_under_roots(staged).is_err() {
            return Ok(BundleEvidence::Ambiguous(format!(
                "journal {} staged path escapes sandbox",
                journal.id
            )));
        }
        if journal.kind == JournalKind::Delete {
            // A generated file being taken away: it is either still in
            // place or set aside, never both, and never anything else.
            let holds = |path: &Path| {
                paths::read_regular_file(path)
                    .is_ok_and(|bytes| sha256_hex(&bytes) == journal.staged_sha256)
            };
            let present = |path: &Path| std::fs::symlink_metadata(path).is_ok();
            let in_place = holds(&dest);
            let aside = holds(staged);
            // One file under both names: an unwind linked it home and
            // stopped, or the link test before a set-aside stopped before
            // removing its link. The two cannot be told apart, and in both
            // the file is in place, so the bundle unwinds.
            if in_place && paths::same_file(staged, &dest) {
                resumeable = false;
                continue;
            }
            if (in_place && present(staged))
                || (aside && present(&dest))
                || (!in_place && present(&dest))
                || (!aside && present(staged))
            {
                return Ok(BundleEvidence::Ambiguous(format!(
                    "journal {} generated file changed",
                    journal.id
                )));
            }
            if !in_place && !aside {
                resumeable = false;
            }
            continue;
        }
        let staged_bytes = paths::read_regular_file(staged).ok();
        let dest_bytes = paths::read_regular_file(&dest).ok();
        let staged_match = staged_bytes
            .as_deref()
            .map(|bytes| sha256_hex(bytes) == journal.staged_sha256)
            .unwrap_or(false);
        let dest_match = dest_bytes
            .as_deref()
            .map(|bytes| sha256_hex(bytes) == journal.staged_sha256)
            .unwrap_or(false);
        let dest_holds_source = match (&dest_bytes, journal.source_sha256.as_deref()) {
            (Some(bytes), Some(source_sha)) => sha256_hex(bytes) == source_sha,
            _ => false,
        };
        // A replacement not yet set aside: the destination still holds
        // the file it replaces.
        let dest_holds_replaced = match (&dest_bytes, journal.replaces.as_ref()) {
            (Some(bytes), Some(replaced)) => sha256_hex(bytes) == replaced.sha256,
            _ => false,
        };
        if staged_bytes.is_some() && !staged_match {
            return Ok(BundleEvidence::Ambiguous(format!(
                "journal {} staged bytes changed",
                journal.id
            )));
        }
        if dest_bytes.is_some() && !dest_match && !dest_holds_source && !dest_holds_replaced {
            return Ok(BundleEvidence::Ambiguous(format!(
                "journal {} destination holds foreign bytes",
                journal.id
            )));
        }
        // One file under both names is a link-then-unlink placement that
        // stopped halfway; resume removes the hidden name. Two files with
        // the same bytes cannot be told apart from a user's copy.
        if staged_match && dest_match && !paths::same_file(staged, &dest) {
            return Ok(BundleEvidence::Ambiguous(format!(
                "journal {} has duplicate staged locations",
                journal.id
            )));
        }
        if !staged_match && !dest_match {
            resumeable = false;
        }
        if let Some(backup_text) = journal.backup.as_deref() {
            let backup = Path::new(backup_text);
            if sandbox.ensure_under_roots(backup).is_err() {
                return Ok(BundleEvidence::Ambiguous(format!(
                    "journal {} backup escapes sandbox",
                    journal.id
                )));
            }
            if let Ok(bytes) = paths::read_regular_file(backup) {
                let expected = journal.backup_sha256().unwrap_or_default();
                if !expected.is_empty() && sha256_hex(&bytes) != expected {
                    return Ok(BundleEvidence::Ambiguous(format!(
                        "journal {} backup bytes changed",
                        journal.id
                    )));
                }
            }
        }
        if let Some((root, rel)) = journal.source.as_ref()
            && sandbox.resolve_no_symlink(root, rel).is_err()
        {
            return Ok(BundleEvidence::Ambiguous(format!(
                "journal {} source unsafe",
                journal.id
            )));
        }
    }
    Ok(BundleEvidence::Clear(ClearEvidence {
        any_committed,
        resumeable,
    }))
}

/// Move every still-active row in the bundle to `needs_attention`.
fn flag_attention(conn: &Connection, journals: &[FileJournal]) -> Result<(), PublishError> {
    let store = JournalStore::new(conn);
    for journal in journals {
        if journal.state.is_terminal() {
            continue;
        }
        if !store.transition(&journal.id, journal.state, JournalState::NeedsAttention)? {
            tracing::warn!(journal_id = %journal.id, "needs-attention not recorded: state moved");
        }
    }
    Ok(())
}

/// Finish a pre-commit bundle: complete any missing renames, then run
/// the pinned catalog transaction. Journals already past their rename
/// (destination holds staged bytes while the row still says staged)
/// transition forward without a second rename. `Some(reason)` when a file
/// was refused: nothing more is placed and the caller unwinds the bundle.
/// `seams` is the default outside tests.
fn resume_precommit<C: Catalog>(
    conn: &mut Connection,
    sandbox: &Sandbox,
    catalog: &C,
    bundle_id: &str,
    journals: &[FileJournal],
    seams: Seams,
) -> Result<Option<String>, PublishError> {
    for journal in journals {
        if journal.state.is_terminal()
            || matches!(
                journal.state,
                JournalState::Committed | JournalState::CleanupPending
            )
        {
            continue;
        }
        if let Some(reason) = resume_rename(conn, sandbox, journal, seams)? {
            return Ok(Some(reason));
        }
    }
    let refreshed = JournalStore::new(conn).bundle(bundle_id)?;
    let mut tracks = Vec::new();
    let mut expected_revision: Option<u64> = None;
    for journal in refreshed.iter() {
        if journal.state != JournalState::Published {
            return Err(PublishError::Journal(format!(
                "journal {} is {} at resume commit",
                journal.id,
                journal.state.as_str()
            )));
        }
        if let (Some(track_id), Some(rev), Some(mgmt)) = (
            journal.track_id.as_deref(),
            journal.catalog_revision,
            journal.mgmt_state.as_deref(),
        ) {
            expected_revision.get_or_insert(rev as u64);
            let dest = sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
            tracks.push(TrackCommit {
                track_id: track_id.to_string(),
                root_id: journal.dest_root.clone(),
                rel_path: journal.dest_rel.clone(),
                file_path: dest.to_string_lossy().into_owned(),
                fingerprint: journal.staged_sha256.clone(),
                mgmt_state: mgmt.to_string(),
                adopt: mgmt.starts_with(super::publisher::ADOPTED_STATE).then(|| {
                    let provenance = journal
                        .source
                        .as_ref()
                        .and_then(|(root, rel)| sandbox.resolve_no_symlink(root, rel).ok())
                        .map(|staged| crate::library::import::recovered_provenance(&staged))
                        .unwrap_or_default();
                    super::planner::Adoption::recovered(
                        provenance.task_id,
                        provenance.source_path,
                        provenance.ingest_source,
                    )
                }),
                replaces: journal.replaces.as_ref().and_then(|replaced| {
                    replaced
                        .track_id
                        .clone()
                        .map(|track| (track, replaced.recycle_to.clone()))
                }),
            });
        }
    }
    let expected = expected_revision
        .ok_or_else(|| PublishError::Journal("bundle has no committable track".into()))?;
    for journal in refreshed.iter() {
        if let Some(rev) = journal.catalog_revision
            && rev as u64 != expected
        {
            return Err(PublishError::Journal(
                "bundle has mixed catalog revisions".into(),
            ));
        }
    }
    let commit = BundleCommit {
        bundle_id: bundle_id.to_string(),
        expected_catalog_revision: expected,
        tracks,
    };
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(PublishError::from)?;
    if let Err(err) = catalog.commit_bundle(&tx, &commit) {
        drop(tx);
        if let Some(reason) = compensate_bundle(conn, sandbox, &refreshed, seams)? {
            tracing::warn!(
                bundle_id,
                reason,
                "publish bundle needs attention after its unwind"
            );
        }
        return Err(err);
    }
    let store = JournalStore::new(&tx);
    for journal in refreshed.iter() {
        let moved = store.transition(
            &journal.id,
            JournalState::Published,
            JournalState::Committed,
        )?;
        if !moved {
            return Err(PublishError::Journal(format!(
                "journal {} moved under resume commit",
                journal.id
            )));
        }
    }
    tx.commit().map_err(PublishError::from)?;
    finish_cleanup(conn, sandbox, bundle_id)?;
    Ok(None)
}

/// Complete one journal's placement during resume, honoring the same
/// rules as publish: occupied foreign destinations block, same-path
/// originals are retained under a hidden backup first. `Some(reason)`
/// when the file is refused: another file holds its destination, it has
/// no safe way onto its name, or its original could not be set aside
/// safely. Neither it nor the file in its way is touched.
fn resume_rename(
    conn: &Connection,
    sandbox: &Sandbox,
    journal: &FileJournal,
    seams: Seams,
) -> Result<Option<String>, PublishError> {
    let occupied = || {
        Ok(Some(format!(
            "another file holds the destination of journal {}",
            journal.id
        )))
    };
    if journal.state == JournalState::Published {
        return Ok(None);
    }
    let dest = sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
    let staged = Path::new(&journal.staged);
    if journal.kind == JournalKind::Delete {
        match super::generated::set_aside(
            sandbox.placement_mode(&journal.dest_root),
            (&journal.dest_root, &journal.dest_rel),
            &dest,
            staged,
            &journal.staged_sha256,
            seams,
        ) {
            Ok(()) => {}
            Err(error @ (PublishError::NotLinkable(_) | PublishError::NoSafePlacement(_))) => {
                return Ok(Some(format!(
                    "journal {} could not set its generated file aside: {error}",
                    journal.id
                )));
            }
            Err(error) => return Err(error),
        }
        JournalStore::new(conn).transition(&journal.id, journal.state, JournalState::Published)?;
        return Ok(None);
    }
    if let Ok(bytes) = paths::read_regular_file(&dest) {
        if sha256_hex(&bytes) == journal.staged_sha256 {
            if paths::read_regular_file(staged).is_err() {
                JournalStore::new(conn).transition(
                    &journal.id,
                    journal.state,
                    JournalState::Published,
                )?;
                return Ok(None);
            }
            // The staged file is also still under its hidden name. As one
            // file, the placement below finishes it; as two, it is not
            // ours to pick between.
            if !paths::same_file(staged, &dest) {
                return Err(PublishError::Journal(format!(
                    "journal {} has duplicate staged locations",
                    journal.id
                )));
            }
        } else {
            let source_match = journal
                .source_sha256
                .as_deref()
                .map(|sha| sha256_hex(&bytes) == sha)
                .unwrap_or(false);
            let replaced_here = journal
                .replaces
                .as_ref()
                .is_some_and(|replaced| sha256_hex(&bytes) == replaced.sha256);
            if !source_match && !replaced_here {
                return occupied();
            }
            // The original at the destination: a same-path write, or a
            // case-only rename on a filesystem that folds case. Either way
            // it is set aside under a journaled backup, as the publisher
            // does.
            let same_path = journal
                .source
                .as_ref()
                .map(|(root, rel)| {
                    sandbox
                        .resolve_no_symlink(root, rel)
                        .map(|source| source == dest)
                        .unwrap_or(false)
                        || paths::case_alias(
                            sandbox,
                            (root, rel),
                            (&journal.dest_root, &journal.dest_rel),
                        )
                })
                .unwrap_or(false);
            // A move whose destination holds a separate file with the
            // source's bytes: a copy the user put there.
            if !same_path && !replaced_here {
                return occupied();
            }
            let backup = sandbox.backup_path_for(&dest, &journal.id)?;
            sandbox.ensure_under_roots(&backup)?;
            JournalStore::new(conn).set_backup(&journal.id, &backup.to_string_lossy())?;
            let home = paths::backup_home(
                sandbox,
                journal.source.as_ref(),
                &journal.dest_root,
                &journal.dest_rel,
            )?;
            let original = Original {
                visible: &dest,
                hidden: &backup,
                home: &home,
                sha256: journal.backup_sha256(),
            };
            let mode = sandbox.placement_mode(&journal.dest_root);
            match set_aside_with(mode, original, seams, None) {
                Ok(()) => {}
                Err(SetAsideError::Io(error)) => return Err(PublishError::from(error)),
                Err(error) => {
                    return Ok(Some(format!(
                        "journal {} could not set its original aside: {error}",
                        journal.id
                    )));
                }
            }
        }
    }
    let mode = sandbox.placement_mode(&journal.dest_root);
    match place_with(mode, staged, &dest, seams) {
        Ok(_) => {}
        Err(PlaceError::Collision) => return occupied(),
        Err(PlaceError::Unavailable(_) | PlaceError::CrossDevice) => {
            return Ok(Some(format!(
                "journal {} has no safe way onto its destination",
                journal.id
            )));
        }
        Err(error) => return Err(error.into_publish(&journal.dest_root, &journal.dest_rel)),
    }
    let current = JournalStore::new(conn)
        .get(&journal.id)?
        .ok_or_else(|| PublishError::Journal(format!("missing journal {}", journal.id)))?;
    if current.state != JournalState::Published {
        let moved = JournalStore::new(conn).transition(
            &journal.id,
            current.state,
            JournalState::Published,
        )?;
        if !moved {
            return Err(PublishError::Journal(format!(
                "journal {} moved under resume",
                journal.id
            )));
        }
    }
    Ok(None)
}

/// Reconcile a bundle with committed journals: never roll back.
/// Verifies the catalog still names each journaled destination with
/// matching bytes, restores an exact remaining staged copy when the
/// destination went missing, and retries pinned cleanup. The staged copy
/// only ever goes onto a free name: a destination holding anything else
/// keeps it, and the staged copy stays beside it as evidence.
fn reconcile_committed<C: Catalog>(
    conn: &mut Connection,
    sandbox: &Sandbox,
    catalog: &C,
    bundle_id: &str,
    journals: &[FileJournal],
) -> Result<RecoveryAction, PublishError> {
    for journal in journals {
        if journal.state.is_terminal() {
            continue;
        }
        if matches!(journal.state, JournalState::Prepared | JournalState::Staged) {
            flag_attention(conn, journals)?;
            return Ok(RecoveryAction::NeedsAttention(format!(
                "bundle {bundle_id} has mixed commit states"
            )));
        }
        if journal.state == JournalState::Published {
            flag_attention(conn, journals)?;
            return Ok(RecoveryAction::NeedsAttention(format!(
                "bundle {bundle_id} disagrees with its catalog commit"
            )));
        }
        if journal.kind == JournalKind::Delete {
            // Its destination is meant to be gone; cleanup deletes the
            // set-aside copy.
            continue;
        }
        let dest = sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
        let staged = Path::new(&journal.staged);
        let holds = |path: &Path| {
            paths::read_regular_file(path)
                .is_ok_and(|bytes| sha256_hex(&bytes) == journal.staged_sha256)
        };
        let staged_holds = sandbox.ensure_under_roots(staged).is_ok() && holds(staged);
        let mode = sandbox.placement_mode(&journal.dest_root);
        let dest_free = matches!(
            std::fs::symlink_metadata(&dest),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        );
        if holds(&dest) {
            // Still under its hidden name too, as the same file: a
            // placement that linked and stopped. The hidden name goes.
            if staged_holds && paths::same_file(staged, &dest) {
                place_from_hidden(mode, staged, &dest)
                    .map_err(|error| error.into_publish(&journal.dest_root, &journal.dest_rel))?;
            }
        } else if !staged_holds {
            if let Some(track_id) = journal.track_id.as_deref() {
                catalog.mark_missing(conn, track_id)?;
            }
            flag_attention(conn, journals)?;
            return Ok(RecoveryAction::NeedsAttention(format!(
                "committed output missing for journal {}",
                journal.id
            )));
        } else if !dest_free {
            flag_attention(conn, journals)?;
            return Ok(RecoveryAction::NeedsAttention(format!(
                "another file holds the committed destination of journal {}",
                journal.id
            )));
        } else {
            match place_from_hidden(mode, staged, &dest) {
                Ok(_) => {}
                Err(
                    PlaceError::Collision | PlaceError::Unavailable(_) | PlaceError::CrossDevice,
                ) => {
                    flag_attention(conn, journals)?;
                    return Ok(RecoveryAction::NeedsAttention(format!(
                        "committed output for journal {} could not be restored without \
                         risking an overwrite",
                        journal.id
                    )));
                }
                Err(error) => {
                    return Err(error.into_publish(&journal.dest_root, &journal.dest_rel));
                }
            }
        }
        if let Some(track_id) = journal.track_id.as_deref() {
            match catalog.locate(conn, track_id)? {
                Some((root, rel, fingerprint, _)) => {
                    if root != journal.dest_root
                        || rel != journal.dest_rel
                        || fingerprint != journal.staged_sha256
                    {
                        flag_attention(conn, journals)?;
                        return Ok(RecoveryAction::NeedsAttention(format!(
                            "catalog disagrees with journal {}",
                            journal.id
                        )));
                    }
                }
                None => {
                    flag_attention(conn, journals)?;
                    return Ok(RecoveryAction::NeedsAttention(format!(
                        "catalog lost track {track_id}"
                    )));
                }
            }
        }
    }
    finish_cleanup(conn, sandbox, bundle_id)?;
    Ok(RecoveryAction::CleanupFinished)
}

/// Retry pinned post-commit cleanup: remove verified sources and
/// backups, then mark journals cleaned. Failures return journals to
/// `cleanup_pending` for the next pass.
fn finish_cleanup(
    conn: &Connection,
    sandbox: &Sandbox,
    bundle_id: &str,
) -> Result<(), PublishError> {
    let journals = JournalStore::new(conn).bundle(bundle_id)?;
    for journal in journals {
        if !matches!(
            journal.state,
            JournalState::Committed | JournalState::CleanupPending
        ) {
            continue;
        }
        let mut failed = false;
        if journal.kind == JournalKind::Delete {
            let aside = Path::new(&journal.staged);
            if sandbox.ensure_under_roots(aside).is_err()
                || super::generated::discard(aside, &journal.staged_sha256).is_err()
            {
                failed = true;
            }
        } else if let (Some(replaced), Some(backup_text)) =
            (journal.replaces.as_ref(), journal.backup.as_deref())
        {
            // The replaced file goes to the recycle bin, never away.
            let moved = sandbox
                .ensure_under_roots(Path::new(backup_text))
                .and_then(|()| {
                    paths::recycle_backup(
                        sandbox.placement_mode(&journal.dest_root),
                        Path::new(backup_text),
                        Path::new(&replaced.recycle_to),
                        &replaced.sha256,
                    )
                });
            match moved {
                Ok(true) => {
                    JournalStore::new(conn).set_recycled(&journal.id, now_secs())?;
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(journal_id = %journal.id, %error, "replaced file not recycled yet");
                    failed = true;
                }
            }
        } else if let Some(backup_text) = journal.backup.as_deref()
            && sandbox.ensure_under_roots(Path::new(backup_text)).is_ok()
            && let Ok(bytes) = paths::read_regular_file(Path::new(backup_text))
        {
            let expected = journal.source_sha256.as_deref().unwrap_or_default();
            if (expected.is_empty() || sha256_hex(&bytes) == expected)
                && std::fs::remove_file(Path::new(backup_text)).is_err()
            {
                failed = true;
            }
        }
        if let Some((root, rel)) = journal.source.as_ref()
            && let Ok(source) = sandbox.resolve_no_symlink(root, rel)
            && let Ok(dest) = sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel)
            && source != dest
            && !paths::same_file(&source, &dest)
            && let Ok(bytes) = paths::read_regular_file(&source)
        {
            let expected = journal.source_sha256.as_deref().unwrap_or_default();
            if (expected.is_empty() || sha256_hex(&bytes) == expected)
                && std::fs::remove_file(&source).is_err()
            {
                failed = true;
            }
        }
        let store = JournalStore::new(conn);
        let cleaned =
            !failed && store.transition(&journal.id, journal.state, JournalState::Cleaned)?;
        if !cleaned
            && journal.state != JournalState::CleanupPending
            && !store.transition(&journal.id, journal.state, JournalState::CleanupPending)?
        {
            tracing::warn!(journal_id = %journal.id, "cleanup-pending not recorded: state moved");
        }
    }
    Ok(())
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
    conn: &Connection,
    sandbox: &Sandbox,
    journals: &[FileJournal],
    seams: Seams,
) -> Result<Option<String>, PublishError> {
    let store = JournalStore::new(conn);
    let mut attention = None;
    for (index, journal) in journals.iter().enumerate() {
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
                flag_attention(conn, &journals[index..])?;
                return Ok(Some(reason));
            }
        };
        if !store.transition(&journal.id, journal.state, next)? {
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
    let dest = sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
    if journal.kind == JournalKind::Delete {
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
    let staged_bytes_at_dest = paths::read_regular_file(&dest)
        .is_ok_and(|bytes| sha256_hex(&bytes) == journal.staged_sha256);
    let temp_went_there =
        std::fs::symlink_metadata(staged).is_err() || paths::same_file(staged, &dest);
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
            let intact = paths::read_regular_file(backup)
                .is_ok_and(|bytes| expected.is_empty() || sha256_hex(&bytes) == expected);
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
    if let Ok(bytes) = paths::read_regular_file(staged)
        && sha256_hex(&bytes) == journal.staged_sha256
    {
        std::fs::remove_file(staged).map_err(PublishError::from)?;
    }
    if let Some(parent) = dest.parent()
        && let Err(error) = super::journal::fsync_dir(parent)
    {
        tracing::warn!(dir = %parent.display(), %error, "directory sync failed");
    }
    Ok(Unwound::Clean)
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}
