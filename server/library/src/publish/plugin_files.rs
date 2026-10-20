//! New files a plugin adds to the library (v2's
//! `write_plugin_managed_file`).
//!
//! A plugin may add one file at a time under a library root, never
//! replace one: the bytes go to a hidden temp beside the destination,
//! are synced, and are then hard-linked into place. The link fails when
//! the name is taken, so an existing file is never overwritten, even one
//! that appears between the check and the write. The same sandbox rules
//! as every publisher write apply: no `..`, no absolute paths, no
//! symlinked components.

use std::io::Write;
use std::path::PathBuf;

use super::PublishError;
use super::journal::{fsync_dir, fsync_file};
use super::paths::{Sandbox, ensure_no_symlink_below};

/// Tag in the temp names plugin writes use (after the hidden prefix), so
/// a startup sweep can tell them from the publisher's journaled temps.
pub const TEMP_TAG: &str = "plugin-";

/// Write `data` as a new file at `relative` under the root `root_id`.
/// `before_temp` sees the temp file's path before it is created, so the
/// caller can note it for a sweep after a crash. Answers the stored
/// absolute path.
pub fn write_new_file(
    sandbox: &Sandbox,
    root_id: &str,
    relative: &str,
    data: &[u8],
    before_temp: impl FnOnce(&std::path::Path) -> Result<(), PublishError>,
) -> Result<PathBuf, PublishError> {
    let dest = sandbox.resolve_no_symlink(root_id, relative)?;
    let parent = dest
        .parent()
        .ok_or_else(|| PublishError::UnsafePath("destination has no parent directory".into()))?
        .to_path_buf();
    std::fs::create_dir_all(&parent)?;
    // A component created or swapped meanwhile must not be a symlink.
    ensure_no_symlink_below(sandbox.root_dir(root_id)?, &parent)?;
    sandbox.ensure_under_roots(&dest)?;
    if std::fs::symlink_metadata(&dest).is_ok() {
        return Err(PublishError::Collision(format!(
            "{relative} already exists; plugins only add new files"
        )));
    }
    let temp = sandbox.temp_path_for(
        &dest,
        &format!("{TEMP_TAG}{}", uuid::Uuid::new_v4().simple()),
    )?;
    before_temp(&temp)?;
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(data)?;
        fsync_file(&file)?;
        match std::fs::hard_link(&temp, &dest) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(PublishError::Collision(format!(
                    "{relative} already exists; plugins only add new files"
                )))
            }
            Err(error) => Err(error.into()),
        }
    })();
    if let Err(error) = std::fs::remove_file(&temp)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "plugin library write left its temp file behind");
    }
    written?;
    fsync_dir(&parent)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::paths::Root;

    #[test]
    fn adds_new_files_and_never_overwrites() {
        let dir = droppedneedle_kernel::db::scratch::ScratchDir::new("plugin-files")
            .expect("scratch dir");
        let sandbox = Sandbox::new(vec![Root::library("r1", dir.path())]).expect("sandbox");
        let stored =
            write_new_file(&sandbox, "r1", "notes/one.txt", b"first", |_| Ok(())).expect("written");
        assert_eq!(std::fs::read(&stored).expect("reads"), b"first");
        assert!(matches!(
            write_new_file(&sandbox, "r1", "notes/one.txt", b"second", |_| Ok(())),
            Err(PublishError::Collision(_))
        ));
        assert_eq!(std::fs::read(&stored).expect("reads"), b"first");
        assert!(matches!(
            write_new_file(&sandbox, "r1", "../escape.txt", b"x", |_| Ok(())),
            Err(PublishError::UnsafePath(_))
        ));
        let leftovers = std::fs::read_dir(dir.path().join("notes"))
            .expect("lists")
            .count();
        assert_eq!(leftovers, 1, "no temp file is left behind");
    }
}
