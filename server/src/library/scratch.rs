//! Scratch directories for test bundles and library tests: a fresh
//! directory under the temp dir that is removed when the guard drops, so
//! test runs leave nothing behind.

use std::path::{Path, PathBuf};

/// A temp directory removed (with everything in it) on drop.
#[derive(Debug)]
pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    /// Create `droppedneedle-<tag>-<uuid>` under the temp dir.
    pub fn new(tag: &str) -> std::io::Result<Self> {
        let path =
            std::env::temp_dir().join(format!("droppedneedle-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    /// The directory.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, path = %self.path.display(), "scratch dir not removed");
        }
    }
}
