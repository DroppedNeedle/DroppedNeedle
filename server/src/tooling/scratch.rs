//! A scratch directory removed when its guard drops.
//!
//! Tests and the test bundles create their throwaway files under one of
//! these, so a run leaves nothing behind in the system temp directory.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A unique directory under the system temp dir, deleted on drop.
#[derive(Debug)]
pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    /// Create a fresh directory named after `tag`.
    pub fn new(tag: &str) -> std::io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|span| span.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "dn-{tag}-{}-{}-{stamp}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    /// The directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for ScratchDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for ScratchDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(path = %self.path.display(), %error, "scratch dir not removed");
        }
    }
}
