//! Taking away a file an earlier run generated (a cover image, a lyrics
//! file), under a `delete` journal. The file is first set aside under
//! the journal's hidden staged name; only after the bundle commits is the
//! set-aside copy deleted, and any unwind before that puts it back. Every
//! step checks the bytes are still the ones the earlier run wrote, so a
//! file someone replaced since is never touched.

use std::path::Path;

use super::snapshots::sha256_hex;
use super::{PublishError, paths};

fn holds(path: &Path, sha256: &str) -> bool {
    paths::read_regular_file(path).is_ok_and(|bytes| sha256_hex(&bytes) == sha256)
}

fn sync_parent(path: &Path) {
    if let Some(parent) = path.parent()
        && let Err(error) = super::journal::fsync_dir(parent)
    {
        tracing::warn!(%error, "directory sync failed");
    }
}

/// Check the generated file is still the one written, before anything is
/// journaled.
pub fn check(dest: &Path, sha256: &str) -> Result<(), PublishError> {
    if holds(dest, sha256) {
        Ok(())
    } else {
        Err(PublishError::Validation(
            "a generated file changed since it was written".into(),
        ))
    }
}

/// Move the generated file to its set-aside name. Safe to repeat: a file
/// already set aside is left there.
pub fn set_aside(dest: &Path, staged: &Path, sha256: &str) -> Result<(), PublishError> {
    let at_dest = holds(dest, sha256);
    let aside = holds(staged, sha256);
    match (at_dest, aside) {
        (false, true) if std::fs::symlink_metadata(dest).is_err() => Ok(()),
        (true, false) if std::fs::symlink_metadata(staged).is_err() => {
            std::fs::rename(dest, staged).map_err(PublishError::from)?;
            sync_parent(dest);
            Ok(())
        }
        _ => Err(PublishError::Validation(
            "a generated file and its set-aside copy disagree".into(),
        )),
    }
}

/// Undo [`set_aside`] before a commit: the copy goes back to an empty
/// path. Anything else is left as it is.
pub fn put_back(dest: &Path, staged: &Path, sha256: &str) -> Result<(), PublishError> {
    if std::fs::symlink_metadata(dest).is_err() && holds(staged, sha256) {
        std::fs::rename(staged, dest).map_err(PublishError::from)?;
        sync_parent(dest);
    }
    Ok(())
}

/// After the commit: delete the set-aside copy, when it is still the
/// generated file.
pub fn discard(staged: &Path, sha256: &str) -> Result<(), PublishError> {
    if holds(staged, sha256) {
        std::fs::remove_file(staged).map_err(PublishError::from)?;
        sync_parent(staged);
        return Ok(());
    }
    if std::fs::symlink_metadata(staged).is_err() {
        return Ok(());
    }
    Err(PublishError::Validation(
        "a set-aside generated file changed; it is kept".into(),
    ))
}
