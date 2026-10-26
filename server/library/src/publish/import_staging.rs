//! The hidden folder a download import copies its files into, inside the
//! library root they are going to, and the removal of what is left there.
//!
//! The import names each copy as a publish source, so the publisher's
//! rename never crosses a filesystem. A folder stays while any publish
//! that took files from it waits on recovery.

use std::path::{Path, PathBuf};

use super::HIDDEN_PREFIX;
use super::planner::FileFingerprint;

/// The hidden folder, relative to its library root, that an import
/// staged under `name` copies its files into. Publish journal rows name
/// it as their source, which is how a download task's writes are found.
pub(crate) fn staging_rel(name: &str) -> String {
    format!("{HIDDEN_PREFIX}import/{}", safe_name(name))
}

/// A task id as one folder name.
fn safe_name(task_id: &str) -> String {
    let cleaned: String = task_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "_".to_owned()
    } else {
        cleaned
    }
}

/// Whether every publish that took files from the staging folder
/// `task_dir` is over, the question `Management::download_publish_settled`
/// asks for a download task: cleaned up, undone, committed with only its
/// own cleanup left, or closed by an administrator after needing
/// attention.
pub(crate) fn staging_settled(
    conn: &rusqlite::Connection,
    task_dir: &str,
) -> rusqlite::Result<bool> {
    let folder = format!("{task_dir}/");
    let unsettled: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM library_publish_journal \
         WHERE bundle_id IN (SELECT bundle_id FROM library_publish_journal \
         WHERE substr(source_rel, 1, length(?1)) = ?1) \
         AND state NOT IN ('cleaned', 'compensated', 'cleanup_pending') \
         AND NOT (state = 'needs_attention' AND bundle_id IN \
         (SELECT bundle_id FROM library_publish_resolutions)))",
        rusqlite::params![folder],
        |row| row.get(0),
    )?;
    Ok(!unsettled)
}

/// Copy one landed file, opened by the caller, into the hidden import
/// folder, synced, hashing the bytes as they stream through.
pub(crate) fn copy_in(
    mut reader: std::fs::File,
    staged: &Path,
) -> std::io::Result<FileFingerprint> {
    use sha2::Digest as _;
    use std::io::{Read as _, Write as _};
    if let Some(parent) = staged.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut writer = std::fs::File::create(staged)?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    let mut size = 0u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        writer.write_all(&buffer[..read])?;
        size += read as u64;
    }
    writer.sync_all()?;
    let sha256 = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(FileFingerprint { size, sha256 })
}

/// Remove import folders a stopped process left behind, once no publish
/// is waiting on recovery (an unsettled bundle may still name one of
/// their copies). Blocking.
pub(crate) fn sweep_import_leftovers(roots: &[PathBuf], journal_settled: bool) {
    if !journal_settled {
        return;
    }
    for root in roots {
        let dir = root.join(format!("{HIDDEN_PREFIX}import"));
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() => {
                if let Err(error) = std::fs::remove_dir_all(&dir) {
                    tracing::warn!(dir = %dir.display(), %error, "import leftovers not removed");
                } else {
                    tracing::info!(dir = %dir.display(), "removed import leftovers");
                }
            }
            Ok(_) | Err(_) => {}
        }
    }
}

/// Remove the hidden import folder (and its parent when empty). The
/// publisher already removed the copies it published; anything left is a
/// copy of a failed import.
pub(crate) fn remove_staging(dir: &Path) {
    if let Err(error) = std::fs::remove_dir_all(dir)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(dir = %dir.display(), %error, "import staging folder not removed");
    }
    if let Some(parent) = dir.parent()
        && std::fs::remove_dir(parent).is_err()
    {
        tracing::debug!("import staging parent kept: other imports use it");
    }
}
