//! Folders a publish makes for its files, and their removal on unwind.
//!
//! A folder is recorded right after it is made and before anything is
//! written into it. Recording first could leave a row for a folder another
//! program made in the gap, which an unwind would then remove. Recording
//! after leaves one window, a crash between the two, whose only cost is an
//! empty folder left behind. An unwind removes exactly the recorded folders,
//! deepest first and only while empty; a folder that existed before is
//! never recorded, so never removed. The catalog commit forgets them.

use std::path::{Component, Path, PathBuf};

use droppedneedle_kernel::db::{Db, Durable};

use super::paths::{Sandbox, ensure_no_symlink_below};
use super::publisher::{durable, long_read};
use super::{PublishError, sql};

/// Make every missing folder from the root of `root_id` down to `dir`,
/// one level at a time, and record the ones made for `bundle_id`. A name
/// already there must be a folder and not a link. When the record cannot
/// be written, the folders just made are removed again before the error
/// returns.
pub(super) fn make(
    db: &Db,
    sandbox: &Sandbox,
    bundle_id: &str,
    root_id: &str,
    dir: &Path,
) -> Result<(), PublishError> {
    let root = sandbox.removal_root_dir(root_id)?.to_path_buf();
    let below = dir
        .strip_prefix(&root)
        .map_err(|_| PublishError::UnsafePath("folder is not inside its library folder".into()))?;
    let mut cursor = root.clone();
    let mut made: Vec<(PathBuf, String)> = Vec::new();
    let result = (|| {
        for part in below.components() {
            let Component::Normal(name) = part else {
                return Err(PublishError::UnsafePath(
                    "folder path is not plain names".into(),
                ));
            };
            cursor.push(name);
            match std::fs::create_dir(&cursor) {
                Ok(()) => {
                    let rel = cursor
                        .strip_prefix(&root)
                        .ok()
                        .and_then(Path::to_str)
                        .ok_or_else(|| PublishError::UnsafePath("folder name is not UTF-8".into()))?
                        .to_owned();
                    made.push((cursor.clone(), rel));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let meta = std::fs::symlink_metadata(&cursor).map_err(PublishError::from)?;
                    if meta.file_type().is_symlink() {
                        return Err(PublishError::UnsafePath(
                            format!("refusing to follow symlink: {}", name.to_string_lossy())
                                .into(),
                        ));
                    }
                    if !meta.is_dir() {
                        return Err(PublishError::Collision(
                            format!(
                                "a file holds the place of folder {}",
                                name.to_string_lossy()
                            )
                            .into(),
                        ));
                    }
                }
                Err(error) => return Err(PublishError::from(error)),
            }
        }
        Ok(())
    })();
    if !made.is_empty() {
        let recorded = durable(db, "publish.folders_made", |tx| {
            record(tx, bundle_id, root_id, &made)
        });
        if let Err(error) = recorded {
            take_back(&made);
            return Err(error);
        }
    }
    result
}

fn record(
    tx: &Durable<'_>,
    bundle_id: &str,
    root_id: &str,
    made: &[(PathBuf, String)],
) -> Result<(), PublishError> {
    let mut insert = tx.prepare_cached(sql::CREATED_FOLDER_INSERT.text())?;
    for (_, rel) in made {
        insert.execute(rusqlite::params![bundle_id, root_id, rel])?;
    }
    Ok(())
}

/// Remove folders this step just made and could not record, deepest first.
fn take_back(made: &[(PathBuf, String)]) {
    for (path, _) in made.iter().rev() {
        if let Err(error) = std::fs::remove_dir(path) {
            tracing::warn!(folder = %path.display(), %error, "a folder made for a write was left behind");
        }
    }
}

/// Forget the folders a bundle made, inside its catalog commit: they hold
/// its files now.
pub(super) fn forget(conn: &rusqlite::Connection, bundle_id: &str) -> Result<(), PublishError> {
    conn.prepare_cached(sql::CREATED_FOLDERS_DELETE.text())?
        .execute(rusqlite::params![bundle_id])?;
    Ok(())
}

fn listed(db: &Db, bundle_id: &str) -> Result<Vec<(String, String)>, PublishError> {
    long_read(db, "publish.folders_made", |conn| {
        let mut stmt = conn.prepare_cached(sql::CREATED_FOLDERS.text())?;
        let rows = stmt.query_map(rusqlite::params![bundle_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
}

/// After an unwind: remove the folders the bundle made, deepest first,
/// each only while empty, then forget them. A folder that holds anything
/// (a kept backup, another program's file) stays. A root the settings no
/// longer know drops its folders with a log line; one whose folder is not
/// there now keeps them for a later pass, as does a folder that could not
/// be removed for another reason. True when nothing is left recorded.
pub(super) fn remove(db: &Db, sandbox: &Sandbox, bundle_id: &str) -> Result<bool, PublishError> {
    let mut folders = listed(db, bundle_id)?;
    if folders.is_empty() {
        return Ok(true);
    }
    folders.sort_by(|left, right| {
        depth(&right.1)
            .cmp(&depth(&left.1))
            .then_with(|| left.cmp(right))
    });
    let mut settled = true;
    let mut dropped_roots: Vec<&str> = Vec::new();
    for (root_id, rel) in &folders {
        let root = match sandbox.removal_root_dir(root_id) {
            Ok(root) => root,
            Err(_) => {
                if !dropped_roots.contains(&root_id.as_str()) {
                    dropped_roots.push(root_id);
                    tracing::warn!(
                        bundle_id,
                        root_id,
                        "folders a write made under a library folder that is gone are forgotten"
                    );
                }
                continue;
            }
        };
        let path = root.join(rel);
        match ensure_no_symlink_below(root, &path) {
            Ok(()) => {}
            Err(PublishError::FolderMissing) => {
                settled = false;
                continue;
            }
            Err(PublishError::UnsafePath(_)) => {
                tracing::warn!(bundle_id, folder = %rel, "a folder a write made became a link; it is left as it is");
                continue;
            }
            Err(error) => {
                tracing::warn!(bundle_id, folder = %rel, %error, "a folder a write made could not be checked");
                settled = false;
                continue;
            }
        }
        match std::fs::remove_dir(&path) {
            Ok(()) => {
                if let Some(parent) = path.parent()
                    && let Err(error) = super::journal::fsync_dir(parent)
                {
                    tracing::warn!(dir = %parent.display(), %error, "directory sync failed");
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.kind() == std::io::ErrorKind::DirectoryNotEmpty
                    || error.raw_os_error() == Some(libc::ENOTEMPTY)
                    || error.raw_os_error() == Some(libc::EEXIST) => {}
            Err(error) => {
                tracing::warn!(bundle_id, folder = %rel, %error, "a folder a write made was not removed yet");
                settled = false;
            }
        }
    }
    if settled {
        durable(db, "publish.folders_removed", |tx| forget(tx, bundle_id))?;
    }
    Ok(settled)
}

fn depth(rel: &str) -> usize {
    Path::new(rel).components().count()
}
