//! Per-root read/write leases and revision fences.
//!
//! Port of the scan-relevant half of v2's library filesystem coordinator.
//! Scans hold a
//! read lease while walking; management publication takes the write lease
//! and bumps the revision, which tells the walker its generation was
//! superseded and must re-walk. Only a clean, un-degraded walk records
//! the scan fence that lets the reconciler trust missing detection.
//!
//! Also home to the management-artifact rule: any path with a
//! `.droppedneedle-management-` prefixed part or an exact `.recycle` part is
//! never library content.
//!
//! Scan and identify never write music files. Nothing in this module (or
//! anywhere else in scan) opens a library file for writing; the write
//! lease exists for the publisher, which owns publication.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Reserved hidden namespace for management sidecars.
pub const MANAGEMENT_ARTIFACT_PREFIX: &str = ".droppedneedle-management-";
/// Default recycle bin directory. An exact name match, not a prefix: an
/// unrelated dot-prefixed user folder must keep scanning (v2 #465).
pub const RECYCLE_BIN_DIRECTORY_NAME: &str = ".recycle";

/// True when a path is a management artifact or lives under the recycle
/// bin. Applies to both files and directories.
pub fn is_management_artifact(path: &Path) -> bool {
    path.components().any(|component| {
        let text = component.as_os_str().to_string_lossy();
        text.starts_with(MANAGEMENT_ARTIFACT_PREFIX) || text == RECYCLE_BIN_DIRECTORY_NAME
    })
}

#[derive(Debug, Default)]
struct RootLease {
    locked: bool,
    readers: usize,
    revision: u64,
}

#[derive(Debug, Default)]
struct CoordinatorState {
    roots: HashMap<String, RootLease>,
    scan_revisions: HashMap<(String, String), u64>,
}

/// Async read/write leases per root plus monotonic revision fences.
///
/// The lease is a plain mutex-guarded condition: readers proceed unless a
/// writer holds the root, writers wait for readers to drain, and every
/// release wakes the waiters. Hold times are short (one walk generation,
/// one publication).
#[derive(Debug, Clone, Default)]
pub struct FsCoordinator {
    inner: Arc<Mutex<CoordinatorState>>,
    changed: Arc<tokio::sync::Notify>,
}

impl FsCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current revision for a root (0 when never written).
    pub fn revision(&self, root_id: &str) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .roots
            .get(root_id)
            .map(|lease| lease.revision)
            .unwrap_or(0)
    }

    /// Wait until `attempt` succeeds under the state lock. The wakeup is
    /// registered before the state is checked, so a release that lands
    /// between the check and the wait is never lost.
    async fn acquire<T>(&self, mut attempt: impl FnMut(&mut CoordinatorState) -> Option<T>) -> T {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self
                    .inner
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(granted) = attempt(&mut state) {
                    return granted;
                }
            }
            notified.await;
        }
    }

    /// Hold a read lease for `root_id` until the guard drops.
    pub async fn read(&self, root_id: &str) -> ReadGuard {
        self.acquire(|state| {
            let lease = state.roots.entry(root_id.to_owned()).or_default();
            if lease.locked {
                return None;
            }
            lease.readers += 1;
            Some(ReadGuard {
                owner: self.clone(),
                root_id: root_id.to_owned(),
            })
        })
        .await
    }

    /// Try the write lease once without waiting. `None` when a
    /// reader or another writer holds the root.
    pub fn try_write(&self, root_id: &str) -> Option<WriteGuard> {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let lease = state.roots.entry(root_id.to_owned()).or_default();
        if !lease.locked && lease.readers == 0 {
            lease.locked = true;
            return Some(WriteGuard {
                owner: self.clone(),
                root_id: root_id.to_owned(),
            });
        }
        None
    }

    /// Hold the write lease, spin-waiting with short sleeps. For
    /// `spawn_blocking` publish paths that cannot await; async
    /// callers use [`write`](Self::write) instead. Bumps the
    /// revision on release so in-flight walks detect supersede.
    pub fn blocking_write(&self, root_id: &str) -> WriteGuard {
        loop {
            if let Some(guard) = self.try_write(root_id) {
                return guard;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Hold the write lease for `root_id` until the guard drops. Bumps the
    /// revision on release so in-flight walks detect supersede.
    pub async fn write(&self, root_id: &str) -> WriteGuard {
        self.acquire(|state| {
            let lease = state.roots.entry(root_id.to_owned()).or_default();
            if lease.locked || lease.readers > 0 {
                return None;
            }
            lease.locked = true;
            Some(WriteGuard {
                owner: self.clone(),
                root_id: root_id.to_owned(),
            })
        })
        .await
    }

    /// Record the fence for a clean walk generation (v2
    /// `record_scan_revision`). Only clean, un-degraded walks call this.
    pub fn record_scan_revision(&self, run_id: &str, root_id: &str) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let revision = state
            .roots
            .get(root_id)
            .map(|lease| lease.revision)
            .unwrap_or(0);
        state
            .scan_revisions
            .insert((run_id.to_owned(), root_id.to_owned()), revision);
    }

    /// Fence recorded for a (run, root) pair, if any.
    pub fn scan_revision(&self, run_id: &str, root_id: &str) -> Option<u64> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .scan_revisions
            .get(&(run_id.to_owned(), root_id.to_owned()))
            .copied()
    }

    /// Forget every fence for a terminal run (v2 `forget_scan`).
    pub fn forget_scan(&self, run_id: &str) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.scan_revisions.retain(|(id, _), _| id != run_id);
    }

    fn release_read(&self, root_id: &str) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(lease) = state.roots.get_mut(root_id) {
            lease.readers = lease.readers.saturating_sub(1);
        }
        self.changed.notify_waiters();
    }

    fn release_write(&self, root_id: &str) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(lease) = state.roots.get_mut(root_id) {
            lease.locked = false;
            lease.revision += 1;
        }
        self.changed.notify_waiters();
    }
}

/// RAII read lease. Not `Send`-adjacent work: just hold across the walk.
pub struct ReadGuard {
    owner: FsCoordinator,
    root_id: String,
}

impl Drop for ReadGuard {
    fn drop(&mut self) {
        self.owner.release_read(&self.root_id);
    }
}

/// RAII write lease. Reserved for the publisher.
pub struct WriteGuard {
    owner: FsCoordinator,
    root_id: String,
}

impl Drop for WriteGuard {
    fn drop(&mut self) {
        self.owner.release_write(&self.root_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn artifact_rule_matches_v2() {
        assert!(is_management_artifact(Path::new(".recycle/a.flac")));
        assert!(is_management_artifact(Path::new(
            "x/.droppedneedle-management-9/a.flac"
        )));
        assert!(!is_management_artifact(Path::new(".recycle-bin/a.flac")));
        assert!(!is_management_artifact(Path::new("music/a.flac")));
        let _ = PathBuf::new;
    }

    #[tokio::test]
    async fn write_bumps_revision_and_fence_records() {
        let fs = FsCoordinator::new();
        assert_eq!(fs.revision("r1"), 0);
        {
            let _write = fs.write("r1").await;
        }
        assert_eq!(fs.revision("r1"), 1);
        fs.record_scan_revision("run-1", "r1");
        assert_eq!(fs.scan_revision("run-1", "r1"), Some(1));
        fs.forget_scan("run-1");
        assert_eq!(fs.scan_revision("run-1", "r1"), None);
    }

    /// A release racing a waiter's state check must still wake it: a lost
    /// wakeup parks the scan (and shutdown) until the next lease change.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn release_racing_a_waiter_always_wakes_it() {
        let fs = FsCoordinator::new();
        for _ in 0..500 {
            let write = fs.write("r1").await;
            let reader = {
                let fs = fs.clone();
                tokio::spawn(async move { drop(fs.read("r1").await) })
            };
            tokio::task::yield_now().await;
            drop(write);
            tokio::time::timeout(Duration::from_secs(2), reader)
                .await
                .expect("reader woke after the release")
                .expect("reader task");
        }
    }
}
