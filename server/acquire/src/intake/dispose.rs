//! Removing what a batch consumed, after the door's own record of it is
//! durable.

use std::path::{Path, PathBuf};

use crate::landing::confine;

/// Remove consumed source files from one of the app's own folders (a
/// drop's staging folder), then each folder they leave empty, but only a
/// folder strictly beneath one of `roots` (resolved the way the paths
/// are): the folder itself always stays, even when a file landed straight
/// in it was its last. Best effort: a leftover is the orphan sweep's.
pub async fn remove_sources(paths: Vec<PathBuf>, roots: Vec<PathBuf>) {
    remove_with(paths, roots, |_, path, dir| {
        if dir {
            std::fs::remove_dir(path)
        } else {
            std::fs::remove_file(path)
        }
    })
    .await;
}

/// Remove a finished download's consumed files from the client's
/// `folders` (real paths) as [`remove_sources`] does, each removal made
/// beneath its folder, never through a link swapped in below it.
pub async fn remove_downloaded(paths: Vec<PathBuf>, folders: Vec<PathBuf>) {
    remove_with(paths, folders, |folders, path, dir| {
        if dir {
            confine::remove_dir_beneath(folders, path)
        } else {
            confine::remove_beneath(folders, path)
        }
    })
    .await;
}

/// `remove(roots, path, is_folder)` removes one file or empty folder.
async fn remove_with(
    paths: Vec<PathBuf>,
    roots: Vec<PathBuf>,
    remove: fn(&[PathBuf], &Path, bool) -> std::io::Result<()>,
) {
    if paths.is_empty() {
        return;
    }
    if let Err(error) =
        tokio::task::spawn_blocking(move || remove_blocking(&paths, &roots, remove)).await
    {
        tracing::warn!(%error, "consumed source cleanup did not run");
    }
}

fn remove_blocking(
    paths: &[PathBuf],
    roots: &[PathBuf],
    remove: fn(&[PathBuf], &Path, bool) -> std::io::Result<()>,
) {
    let mut parents: Vec<&Path> = Vec::new();
    for path in paths {
        if let Err(error) = remove(roots, path, false)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::info!(file = %path.display(), %error, "consumed source file kept");
        }
        if let Some(parent) = path.parent()
            && !parents.contains(&parent)
        {
            parents.push(parent);
        }
    }
    for parent in parents {
        let beneath = roots
            .iter()
            .any(|root| parent != root.as_path() && parent.starts_with(root));
        if !beneath {
            continue;
        }
        // Succeeds only once the folder is empty; a folder still holding
        // other files (held ones, extras) is meant to stay.
        if remove(roots, parent, true).is_err() {
            tracing::debug!(dir = %parent.display(), "source folder kept: not empty");
        }
    }
}
