//! The one "needs your decision" list: every file kept out of the library
//! for a person, whichever door it came through.
//!
//! A download the landing would not import on its own and an upload drop
//! import could not place both wait here, as one entry per download or
//! upload with its files ([`store`]). Every entry offers the same actions
//! ([`service`]): listen, import (retry, for holds our side caused), match
//! to an album (curators), re-check with AcoustID, discard, and for a
//! track found only as the album version, get it from the album. Each
//! action claims the entry in the database first, so two actions never
//! run on one entry.
//!
//! An entry expires 90 days after it was made, or after its newest file
//! arrived, and the list shows when. A supervised loop removes the held
//! copies of expired entries: only copies in the app's own two folders,
//! never a library file or a download client's folder.
//!
//! Files the import folder could not place wait here too, where they are:
//! they are the person's own copies, so their entry belongs to no user
//! (only admins see it), never expires, and a file leaves the folder only
//! when a person discards it or once the library holds its exact bytes.
//! They are read and removed beneath the import folder, and only while
//! that folder is still the import folder.

pub mod http;
pub mod service;
pub mod sql;
pub mod store;

use std::path::{Component, Path, PathBuf};

pub(crate) use self::service::spawn_held_prune;
pub use self::service::{HeldError, HeldService, PRUNE_JOB};
use self::store::HeldRoot;

/// The two app-owned folders held copies live in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldFolders {
    /// Where a download's held file is copied.
    pub held: PathBuf,
    /// The drop staging folder an upload waits in.
    pub drop: PathBuf,
}

impl HeldFolders {
    /// The app-owned folder of `root`; `None` for the import folder, which
    /// is checked on each use (see [`crate::import_folder::site`]).
    pub fn dir(&self, root: HeldRoot) -> Option<&Path> {
        match root {
            HeldRoot::Held => Some(&self.held),
            HeldRoot::Drop => Some(&self.drop),
            HeldRoot::Import => None,
        }
    }

    /// Where a held copy is: `path` below its root folder. `None` for a
    /// path that is absolute or climbs out, so nothing outside the two
    /// folders is ever read or deleted through a row, and for the import
    /// folder's files.
    pub fn locate(&self, root: HeldRoot, path: &str) -> Option<PathBuf> {
        below(self.dir(root)?, path)
    }
}

/// A file's size and modification time in milliseconds, as an
/// import-folder row records them when the file is listed.
pub fn listed_identity(meta: &std::fs::Metadata) -> (i64, i64) {
    let modified = meta
        .modified()
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |at| i64::try_from(at.as_millis()).unwrap_or(i64::MAX));
    (i64::try_from(meta.len()).unwrap_or(i64::MAX), modified)
}

/// Whether the file now at a held row's path (`meta`, read without
/// following a link) is still the file the row listed. A row that recorded
/// nothing is taken as listed.
pub fn as_listed(file: &store::HeldFile, meta: &std::fs::Metadata) -> bool {
    match (file.file_size, file.file_modified_at) {
        (Some(size), Some(modified)) => listed_identity(meta) == (size, modified),
        _ => true,
    }
}

/// `path` below `dir`: `None` for a path that is empty, absolute or climbs
/// out.
pub fn below(dir: &Path, path: &str) -> Option<PathBuf> {
    let relative = Path::new(path);
    let plain = !path.is_empty()
        && relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)));
    plain.then(|| dir.join(relative))
}

/// Remove one held copy below `dir`. True once the copy is gone: removed
/// now, already absent, or named by a path that cannot be ours. A copy
/// that could not be removed (no permission, a read error) answers false
/// and is logged, and the caller keeps its row so the held list still
/// shows it.
pub(crate) async fn remove_copy(dir: &Path, path: &str) -> bool {
    let folders = HeldFolders {
        held: dir.to_path_buf(),
        drop: dir.to_path_buf(),
    };
    let Some(path) = folders.locate(HeldRoot::Held, path) else {
        tracing::warn!(path, "held copy path refused");
        return true;
    };
    match tokio::fs::remove_file(&path).await {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "held copy not removed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_never_names_a_file_outside_its_folder() {
        let folders = HeldFolders {
            held: PathBuf::from("/data/cache/held"),
            drop: PathBuf::from("/data/imports/drop"),
        };
        assert_eq!(
            folders.locate(HeldRoot::Drop, "job/cd1/01.flac"),
            Some(PathBuf::from("/data/imports/drop/job/cd1/01.flac"))
        );
        for bad in ["", "/etc/passwd", "../music/a.flac", "job/../../x", "./a"] {
            assert_eq!(folders.locate(HeldRoot::Held, bad), None, "{bad}");
        }
    }
}
