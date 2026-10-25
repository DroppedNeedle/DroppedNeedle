//! Removing what a batch consumed, after the door's own record of it is
//! durable.

use std::path::{Path, PathBuf};

/// Remove consumed source files, then each folder they leave empty, but
/// only a folder strictly beneath one of `roots` (resolved the way the
/// paths are): a downloads or staging folder itself always stays, even
/// when a file landed straight in it was its last. Best effort: a
/// leftover is the orphan sweep's.
pub async fn remove_sources(paths: Vec<PathBuf>, roots: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    if let Err(error) = tokio::task::spawn_blocking(move || remove_blocking(&paths, &roots)).await {
        tracing::warn!(%error, "consumed source cleanup did not run");
    }
}

fn remove_blocking(paths: &[PathBuf], roots: &[PathBuf]) {
    let mut parents: Vec<&Path> = Vec::new();
    for path in paths {
        if let Err(error) = std::fs::remove_file(path)
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
        if std::fs::remove_dir(parent).is_err() {
            tracing::debug!(dir = %parent.display(), "source folder kept: not empty");
        }
    }
}
