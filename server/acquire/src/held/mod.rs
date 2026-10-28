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
    /// The root folder of `root`.
    pub fn dir(&self, root: HeldRoot) -> &Path {
        match root {
            HeldRoot::Held => &self.held,
            HeldRoot::Drop => &self.drop,
        }
    }

    /// Where a held copy is: `path` below its root folder. `None` for a
    /// path that is absolute or climbs out, so nothing outside the two
    /// folders is ever read or deleted through a row.
    pub fn locate(&self, root: HeldRoot, path: &str) -> Option<PathBuf> {
        let relative = Path::new(path);
        let plain = !path.is_empty()
            && relative
                .components()
                .all(|part| matches!(part, Component::Normal(_)));
        plain.then(|| self.dir(root).join(relative))
    }
}

/// Remove one held copy below `dir`. A copy already gone is fine; any
/// other failure is logged and leaves a stray file, which the held
/// folder's sweep removes at the next start.
pub(crate) async fn remove_copy(dir: &Path, path: &str) {
    let folders = HeldFolders {
        held: dir.to_path_buf(),
        drop: dir.to_path_buf(),
    };
    let Some(path) = folders.locate(HeldRoot::Held, path) else {
        tracing::warn!(path, "held copy path refused");
        return;
    };
    if let Err(error) = tokio::fs::remove_file(&path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %error, "held copy not removed");
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
