//! Sibling seams: tag reads, identification, and the scan checkpoint.
//!
//! Tag parsing and provider identification belong to sibling slices. This
//! module defines the minimal traits the scan pipeline calls, plus null
//! implementations the tests use. The contracts are small on purpose:
//! read-only tag access, fire-and-forget identify enqueue, and a boolean
//! checkpoint the coordinator backs.
//!
//! Purity rule: every implementation must treat library files as
//! read-only. The `library_scan` purity brief pins zero file writes across
//! full runs; a tag reader that writes (padding rewrites, mtime restores)
//! fails that brief by design.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// Tags for one file, as the indexer needs them. The real tag shape is
/// owned by the tag slice; this struct carries only what indexing consumes.
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
    /// re-offers as changed next run (v2 F-12).
    Deferred,
    /// The file cannot be tag-read at all. Counted in `errored_count`.
    Fatal,
}

/// Read-only tag access for one audio file. Implementations must never
/// write to the file: no padding rewrites, no mtime restores, no sidecars.
pub trait TagReader: Send + Sync {
    fn read_tags(&self, path: &Path) -> Result<ScannedTags, TagReadError>;
}

/// Null tag reader: every file reads clean with empty tags. The scan-rate
/// briefs use this to measure discovery and indexing without a tag stack.
#[derive(Debug, Default)]
pub struct NullTagReader {
    reads: Mutex<u64>,
}

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

impl TagReader for NullTagReader {
    fn read_tags(&self, _path: &Path) -> Result<ScannedTags, TagReadError> {
        *self
            .reads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
        Ok(ScannedTags::default())
    }
}

/// Tag reader that defers one nominated file, for the re-offer brief.
#[derive(Debug, Default)]
pub struct DeferOnceTagReader {
    inner: NullTagReader,
    defer_basename: Mutex<Option<String>>,
    deferred: Mutex<Vec<String>>,
}

impl DeferOnceTagReader {
    pub fn deferring(basename: &str) -> Self {
        Self {
            inner: NullTagReader::new(),
            defer_basename: Mutex::new(Some(basename.to_owned())),
            deferred: Mutex::new(Vec::new()),
        }
    }

    pub fn reads(&self) -> u64 {
        self.inner.reads()
    }

    pub fn deferred(&self) -> Vec<String> {
        self.deferred
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl TagReader for DeferOnceTagReader {
    fn read_tags(&self, path: &Path) -> Result<ScannedTags, TagReadError> {
        let basename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let mut slot = self
            .defer_basename
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot
            .as_ref()
            .is_some_and(|wanted| Some(wanted) == basename.as_ref())
        {
            *slot = None;
            self.deferred
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(path.display().to_string());
            return Err(TagReadError::Deferred);
        }
        drop(slot);
        self.inner.read_tags(path)
    }
}

/// Tag reader that defers armed basenames once, then reads clean.
/// Tests arm a file mid-suite to reproduce exhaustion against an already
/// indexed catalog row (the exact F-12 re-offer shape).
#[derive(Debug, Default)]
pub struct ArmableDeferTagReader {
    inner: NullTagReader,
    armed: Mutex<std::collections::HashSet<String>>,
    deferred: Mutex<Vec<String>>,
}

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

    pub fn reads(&self) -> u64 {
        self.inner.reads()
    }

    pub fn deferred(&self) -> Vec<String> {
        self.deferred
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

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
            self.deferred
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(path.display().to_string());
            return Err(TagReadError::Deferred);
        }
        self.inner.read_tags(path)
    }
}

/// Fire-and-forget identify enqueue. The identify slice owns workers and
/// provider calls; the scan pipeline only offers album keys.
pub trait IdentifyQueue: Send + Sync {
    /// Offer one album key with its fresh track ids. Returns tracks queued.
    fn enqueue(&self, album_key: &str, track_ids: &[String]) -> usize;

    fn enqueued_tracks(&self) -> usize {
        0
    }
}

/// Null identify queue: counts offers, queues nothing.
#[derive(Debug, Default)]
pub struct NullIdentifyQueue {
    tracks: Mutex<usize>,
    albums: Mutex<Vec<String>>,
}

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
/// change superseded the run, never a filesystem error (v2 F-INDEXREC-06).
pub trait Checkpoint: Send + Sync {
    fn check(&self, run_id: &str, frozen_policy_revision: &str) -> bool;
}

/// Checkpoint that always allows progress. Unit-test only.
#[derive(Debug, Default)]
pub struct AllowAll;

impl Checkpoint for AllowAll {
    fn check(&self, _run_id: &str, _frozen_policy_revision: &str) -> bool {
        true
    }
}

/// Checkpoint backed by a closure. Test-only.
pub struct FnCheckpoint<F> {
    check: F,
}

impl<F> FnCheckpoint<F>
where
    F: Fn(&str, &str) -> bool + Send + Sync,
{
    pub fn new(check: F) -> Self {
        Self { check }
    }
}

impl<F> Checkpoint for FnCheckpoint<F>
where
    F: Fn(&str, &str) -> bool + Send + Sync,
{
    fn check(&self, run_id: &str, frozen_policy_revision: &str) -> bool {
        (self.check)(run_id, frozen_policy_revision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defer_once_defers_exactly_one_read() {
        let reader = DeferOnceTagReader::deferring("a.flac");
        assert_eq!(
            reader.read_tags(Path::new("/music/a.flac")),
            Err(TagReadError::Deferred)
        );
        assert!(reader.read_tags(Path::new("/music/a.flac")).is_ok());
        assert_eq!(reader.deferred().len(), 1);
    }
}
