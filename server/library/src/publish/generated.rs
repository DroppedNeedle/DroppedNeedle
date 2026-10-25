//! Taking away a file an earlier run generated (a cover image, a lyrics
//! file), under a `delete` journal. The file is first set aside under
//! the journal's hidden staged name; only after the bundle commits is the
//! set-aside copy deleted, and any unwind before that puts it back. Every
//! step checks the bytes are still the ones the earlier run wrote, so a
//! file someone replaced since is never touched. Both moves go through
//! `place`, so neither can replace a file or lose one.

use std::path::Path;

use super::place::{self, Original, PlaceError, PlacementMode, Seams};
use super::{PublishError, paths};

fn holds(path: &Path, sha256: &str) -> bool {
    paths::holds(path, sha256)
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

/// Move the generated file at `root:rel` (`dest`) to its set-aside name.
/// Safe to repeat: a file already set aside is left there. A set-aside
/// name that is the file itself, left by a put-back that linked it home
/// and stopped, is cleared first; any other file there refuses the move.
pub fn set_aside(
    mode: PlacementMode,
    root_dev: Option<u64>,
    (root, rel): (&str, &str),
    dest: &Path,
    staged: &Path,
    sha256: &str,
    seams: Seams,
) -> Result<(), PublishError> {
    if std::fs::symlink_metadata(dest).is_err() && holds(staged, sha256) {
        return Ok(());
    }
    if !holds(dest, sha256) {
        return Err(PublishError::Validation(
            "a generated file and its set-aside copy disagree".into(),
        ));
    }
    let original = Original {
        visible: dest,
        hidden: staged,
        home: dest,
        sha256: Some(sha256),
        root_dev,
        known: None,
    };
    place::set_aside_with(mode, original, seams, None)
        .map_err(|error| error.into_publish(root, rel))
}

/// Undo [`set_aside`] before a commit: the copy goes back onto a free
/// name, and a put-back that already linked it there is finished by
/// removing the set-aside name. A name another file holds is reported as
/// a collision with both files left as they are.
pub fn put_back(
    mode: PlacementMode,
    dest: &Path,
    staged: &Path,
    sha256: &str,
    seams: Seams,
) -> Result<(), PlaceError> {
    if !holds(staged, sha256) {
        return Ok(());
    }
    match place::place_with(mode, staged, dest, seams) {
        Ok(_) => Ok(()),
        Err(PlaceError::Unsynced(error)) => {
            tracing::warn!(%error, "directory sync failed");
            Ok(())
        }
        Err(error) => Err(error),
    }
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
