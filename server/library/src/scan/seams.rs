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

use crate::tags::TrackFile;

/// Tags plus header properties for one file: everything the catalog
/// stores per track.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScannedTags {
    pub tag: crate::tags::AudioTag,
    pub header: crate::tags::read::HeaderInfo,
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

/// Read-only tag access for one audio file, read beneath its library
/// folder. Implementations must never write to the file: no padding
/// rewrites, no mtime restores, no sidecars.
pub trait TagReader: Send + Sync {
    fn read_tags(&self, file: TrackFile<'_>) -> Result<ScannedTags, TagReadError>;

    /// Whether the file's length is measured
    /// ([`crate::tags::read::HeaderInfo::duration_measured`]), for a
    /// catalog row that does not say. A reader that can tell from less
    /// than a full read overrides this.
    fn length_measured(&self, file: TrackFile<'_>) -> Result<bool, TagReadError> {
        self.read_tags(file)
            .map(|read| read.header.duration_measured)
    }
}

/// Scan checkpoint: true while the run may keep working. The coordinator
/// implements this over live policy and control state; the walker and the
/// indexer call it between batches. False means pause, stop, or a policy
/// change superseded the run, never a filesystem error.
pub trait Checkpoint: Send + Sync {
    fn check(&self, run_id: &str, frozen_policy_revision: &str) -> bool;
}
