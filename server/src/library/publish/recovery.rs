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
//! - Duplicate staged locations, changed bytes, unsafe paths, mixed
//!   commit states, or catalog/destination disagreement move every
//!   still-active row in the bundle to `needs_attention` with durable
//!   structured evidence, deleting nothing.
//! - Committed journals are never rolled back: recovery verifies the
//!   catalog still names the journaled destination with matching
//!   bytes, then retries the pinned cleanup policy.

use std::path::Path;

use rusqlite::Connection;

use super::journal::{FileJournal, JournalState, JournalStore};
use super::paths::Sandbox;
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
/// import, acquisition, or operation workers.
pub fn reconcile<C: Catalog>(
    conn: &mut Connection,
    sandbox: &Sandbox,
    catalog: &C,
) -> Result<Vec<BundleRecovery>, PublishError> {
    startup_gate(conn)?;
    let bundles = JournalStore::new(conn).active_bundles()?;
    let mut results = Vec::new();
    for bundle_id in bundles {
        let action = reconcile_bundle(conn, sandbox, catalog, &bundle_id)?;
        results.push(BundleRecovery { bundle_id, action });
    }
    Ok(results)
}

/// Reconcile one bundle.
fn reconcile_bundle<C: Catalog>(
    conn: &mut Connection,
    sandbox: &Sandbox,
    catalog: &C,
    bundle_id: &str,
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
    if evidence.any_committed {
        return reconcile_committed(conn, sandbox, catalog, bundle_id, &journals);
    }
    if evidence.resumeable {
        resume_precommit(conn, sandbox, catalog, bundle_id, &journals)?;
        return Ok(RecoveryAction::ResumedCommitted);
    }
    compensate_bundle(conn, sandbox, &journals)?;
    Ok(RecoveryAction::Compensated)
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
        if staged_bytes.is_some() && !staged_match {
            return Ok(BundleEvidence::Ambiguous(format!(
                "journal {} staged bytes changed",
                journal.id
            )));
        }
        if dest_bytes.is_some() && !dest_match && !dest_holds_source {
            return Ok(BundleEvidence::Ambiguous(format!(
                "journal {} destination holds foreign bytes",
                journal.id
            )));
        }
        if staged_match && dest_match {
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
                let expected = journal.source_sha256.as_deref().unwrap_or_default();
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
/// transition forward without a second rename.
fn resume_precommit<C: Catalog>(
    conn: &mut Connection,
    sandbox: &Sandbox,
    catalog: &C,
    bundle_id: &str,
    journals: &[FileJournal],
) -> Result<(), PublishError> {
    for journal in journals {
        if journal.state.is_terminal()
            || matches!(
                journal.state,
                JournalState::Committed | JournalState::CleanupPending
            )
        {
            continue;
        }
        resume_rename(conn, sandbox, journal)?;
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
            tracks.push(TrackCommit {
                track_id: track_id.to_string(),
                root_id: journal.dest_root.clone(),
                rel_path: journal.dest_rel.clone(),
                fingerprint: journal.staged_sha256.clone(),
                mgmt_state: mgmt.to_string(),
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
        compensate_bundle(conn, sandbox, &refreshed)?;
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
    Ok(())
}

/// Complete one journal's rename during resume, honoring the same
/// rules as publish: occupied foreign destinations block, same-path
/// originals are retained under a hidden backup first.
fn resume_rename(
    conn: &Connection,
    sandbox: &Sandbox,
    journal: &FileJournal,
) -> Result<(), PublishError> {
    if journal.state == JournalState::Published {
        return Ok(());
    }
    let dest = sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
    let staged = Path::new(&journal.staged);
    if let Ok(bytes) = paths::read_regular_file(&dest) {
        if sha256_hex(&bytes) == journal.staged_sha256 {
            if paths::read_regular_file(staged).is_err() {
                JournalStore::new(conn).transition(
                    &journal.id,
                    journal.state,
                    JournalState::Published,
                )?;
                return Ok(());
            }
            return Err(PublishError::Journal(format!(
                "journal {} has duplicate staged locations",
                journal.id
            )));
        }
        let source_match = journal
            .source_sha256
            .as_deref()
            .map(|sha| sha256_hex(&bytes) == sha)
            .unwrap_or(false);
        if !source_match {
            return Err(PublishError::Collision(format!(
                "late occupier at {}:{}",
                journal.dest_root, journal.dest_rel
            )));
        }
        let same_path = journal
            .source
            .as_ref()
            .map(|(root, rel)| {
                sandbox
                    .resolve_no_symlink(root, rel)
                    .map(|source| source == dest)
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if !same_path {
            return Err(PublishError::Collision(format!(
                "late occupier at {}:{}",
                journal.dest_root, journal.dest_rel
            )));
        }
        let backup = sandbox.backup_path_for(&dest, &journal.id)?;
        sandbox.ensure_under_roots(&backup)?;
        JournalStore::new(conn).set_backup(&journal.id, &backup.to_string_lossy())?;
        std::fs::rename(&dest, &backup).map_err(PublishError::from)?;
        if let Some(parent) = dest.parent() {
            super::journal::fsync_dir(parent)?;
        }
    }
    std::fs::rename(staged, &dest).map_err(PublishError::from)?;
    if let Some(parent) = dest.parent() {
        super::journal::fsync_dir(parent)?;
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
    Ok(())
}

/// Reconcile a bundle with committed journals: never roll back.
/// Verifies the catalog still names each journaled destination with
/// matching bytes, restores an exact remaining staged copy when the
/// destination went missing, and retries pinned cleanup.
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
        let dest = sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
        match paths::read_regular_file(&dest) {
            Ok(bytes) if sha256_hex(&bytes) == journal.staged_sha256 => {}
            _ => {
                let staged = Path::new(&journal.staged);
                match paths::read_regular_file(staged) {
                    Ok(bytes) if sha256_hex(&bytes) == journal.staged_sha256 => {
                        std::fs::copy(staged, &dest).map_err(PublishError::from)?;
                    }
                    _ => {
                        if let Some(track_id) = journal.track_id.as_deref() {
                            catalog.mark_missing(conn, track_id)?;
                        }
                        flag_attention(conn, journals)?;
                        return Ok(RecoveryAction::NeedsAttention(format!(
                            "committed output missing for journal {}",
                            journal.id
                        )));
                    }
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
        if let Some(backup_text) = journal.backup.as_deref()
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

/// Compensate a pre-commit bundle during recovery: same byte-compared
/// unwind as the publisher, marking journals compensated.
fn compensate_bundle(
    conn: &Connection,
    sandbox: &Sandbox,
    journals: &[FileJournal],
) -> Result<(), PublishError> {
    for journal in journals {
        if journal.state.is_terminal()
            || matches!(
                journal.state,
                JournalState::Committed | JournalState::CleanupPending
            )
        {
            continue;
        }
        let dest = sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
        if let Ok(bytes) = paths::read_regular_file(&dest)
            && sha256_hex(&bytes) == journal.staged_sha256
        {
            std::fs::remove_file(&dest).map_err(PublishError::from)?;
        }
        if let Some(backup_text) = journal.backup.as_deref()
            && std::fs::symlink_metadata(&dest).is_err()
            && let Ok(bytes) = paths::read_regular_file(Path::new(backup_text))
        {
            let expected = journal.source_sha256.as_deref().unwrap_or_default();
            if expected.is_empty() || sha256_hex(&bytes) == expected {
                std::fs::rename(Path::new(backup_text), &dest).map_err(PublishError::from)?;
            }
        }
        let staged = Path::new(&journal.staged);
        if let Ok(bytes) = paths::read_regular_file(staged)
            && sha256_hex(&bytes) == journal.staged_sha256
        {
            std::fs::remove_file(staged).map_err(PublishError::from)?;
        }
        if !JournalStore::new(conn).transition(
            &journal.id,
            journal.state,
            JournalState::Compensated,
        )? {
            tracing::warn!(journal_id = %journal.id, "compensation not recorded: state moved");
        }
    }
    Ok(())
}
