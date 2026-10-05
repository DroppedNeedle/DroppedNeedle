//! Sibling seams: tag reads, identification, and the scan checkpoint.
//!
//! Tag parsing and provider identification live in `library::tags` and
//! `library::identify`. This
//! module defines the minimal traits the scan pipeline calls, plus null
//! implementations the tests use. The contracts are small on purpose:
//! read-only tag access, fire-and-forget identify enqueue, and a boolean
//! checkpoint the coordinator backs.
//!
//! Purity rule: every implementation must treat library files as
//! read-only. The `library_scan` purity test pins zero file writes across
//! full runs; a tag reader that writes (padding rewrites, mtime restores)
//! fails that test.

use std::collections::HashMap;
use std::path::Path;
#[cfg(any(test, feature = "test-support"))]
use std::sync::Mutex;

/// Tags for one file, as the indexer needs them. The real tag shape is
/// owned by `library::tags`; this struct carries only what indexing consumes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScannedTags {
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub duration_secs: Option<f64>,
    pub extra: HashMap<String, String>,
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

/// Fire-and-forget identify enqueue. Identify owns workers and
/// provider calls; the scan pipeline only offers album keys.
pub trait IdentifyQueue: Send + Sync {
    /// Offer one album key with its fresh track ids. Returns tracks queued.
    fn enqueue(&self, album_key: &str, track_ids: &[String]) -> usize;

    fn enqueued_tracks(&self) -> usize {
        0
    }
}

/// Null identify queue: counts offers, queues nothing.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct NullIdentifyQueue {
    tracks: Mutex<usize>,
    albums: Mutex<Vec<String>>,
}

#[cfg(any(test, feature = "test-support"))]
impl NullIdentifyQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn albums(&self) -> Vec<String> {
        self.albums
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl IdentifyQueue for NullIdentifyQueue {
    fn enqueue(&self, album_key: &str, track_ids: &[String]) -> usize {
        *self
            .tracks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) += track_ids.len();
        self.albums
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(album_key.to_owned());
        track_ids.len()
    }

    fn enqueued_tracks(&self) -> usize {
        *self
            .tracks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Scan checkpoint: true while the run may keep working. The coordinator
/// implements this over live policy and control state; the walker and the
/// indexer call it between batches. False means pause, stop, or a policy
/// change superseded the run, never a filesystem error.
pub trait Checkpoint: Send + Sync {
    fn check(&self, run_id: &str, frozen_policy_revision: &str) -> bool;
}
