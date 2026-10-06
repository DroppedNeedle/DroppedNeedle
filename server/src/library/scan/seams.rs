//! Scan seams: tag reads, identification, and the scan checkpoint.
//!
//! Tag parsing and provider identification live in `library::tags` and
//! `library::identify`. This
//! module defines the minimal traits the scan pipeline calls, plus null
//! implementations the tests use. The contracts are small on purpose:
//! read-only tag access and a boolean checkpoint the coordinator backs.
//! Identify offers ride the catalog commit itself.
//!
//! Purity rule: every implementation must treat library files as
//! read-only. The `library_scan` purity test pins zero file writes across
//! full runs; a tag reader that writes (padding rewrites, mtime restores)
//! fails that test.

use std::path::Path;
#[cfg(any(test, feature = "test-support"))]
use std::sync::Mutex;

/// Tags plus header properties for one file: everything the catalog
/// stores per track.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScannedTags {
    pub tag: crate::library::tags::AudioTag,
    pub header: crate::library::tags::read::HeaderInfo,
}

/// Why a tag read failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagReadError {
    /// Transient exhaustion (too many wedged readers, contended disk). The
    /// indexer records a persisted `TAG_READ_DEFERRED` marker and the file
    /// re-offers as changed next run.
    Deferred,
    /// The file cannot be tag-read at all. Counted in `errored_count`.
    Fatal,
}

/// Read-only tag access for one audio file. Implementations must never
/// write to the file: no padding rewrites, no mtime restores, no sidecars.
pub trait TagReader: Send + Sync {
    fn read_tags(&self, path: &Path) -> Result<ScannedTags, TagReadError>;
}

/// Null tag reader: every file reads clean with empty tags, so tests can
/// measure discovery and indexing without a tag stack.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct NullTagReader {
    reads: Mutex<u64>,
}

#[cfg(any(test, feature = "test-support"))]
impl NullTagReader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reads(&self) -> u64 {
        *self
            .reads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl TagReader for NullTagReader {
    fn read_tags(&self, _path: &Path) -> Result<ScannedTags, TagReadError> {
        *self
            .reads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
        Ok(ScannedTags::default())
    }
}

/// Tag reader that defers armed basenames once, then reads clean.
/// Tests arm a file mid-suite to reproduce deferral exhaustion against an
/// already indexed catalog row.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct ArmableDeferTagReader {
    inner: NullTagReader,
    armed: Mutex<std::collections::HashSet<String>>,
}

#[cfg(any(test, feature = "test-support"))]
impl ArmableDeferTagReader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn arm(&self, basename: &str) {
        self.armed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(basename.to_owned());
    }
}

#[cfg(any(test, feature = "test-support"))]
impl TagReader for ArmableDeferTagReader {
    fn read_tags(&self, path: &Path) -> Result<ScannedTags, TagReadError> {
        let basename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        if let Some(basename) = basename
            && self
                .armed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&basename)
        {
            return Err(TagReadError::Deferred);
        }
        self.inner.read_tags(path)
    }
}

/// Scan checkpoint: true while the run may keep working. The coordinator
/// implements this over live policy and control state; the walker and the
/// indexer call it between batches. False means pause, stop, or a policy
/// change superseded the run, never a filesystem error.
pub trait Checkpoint: Send + Sync {
    fn check(&self, run_id: &str, frozen_policy_revision: &str) -> bool;
}
