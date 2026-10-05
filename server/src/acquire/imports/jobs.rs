//! The `spotify:import` durable job and its downloads seam.
//!
//! v2 runs the populate as a `TaskRegistry` background task keyed
//! `spotify:import:{user}:{playlist}` (v2 Spotify routes),
//! answering the POST immediately and skipping the spawn when the key is
//! already running. This module keeps the key, the answer-fast shape, and
//! the single-flight rule, and records terminal states for the status route.
//!
//! [`SpotifyImportExecutor`] is the minimal local trait onto the downloads
//! state machine. [`TaskExecutor`] is the in-memory implementation that
//! spawns a task; a durable downloads-backed executor can replace it
//! without touching the handlers.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

/// Build the durable job key (v2 `task_key`, verbatim format).
pub fn import_job_key(user_id: &str, spotify_playlist_id: &str) -> String {
    format!("spotify:import:{user_id}:{spotify_playlist_id}")
}

/// Terminal-or-live state of one import job.
#[derive(Debug, Clone, PartialEq)]
pub enum JobState {
    /// Populate still running.
    Running {
        /// Internal playlist id being filled.
        playlist_id: String,
    },
    /// Populate finished.
    Done {
        /// Internal playlist id.
        playlist_id: String,
        /// Tracks written.
        track_count: usize,
    },
    /// Populate failed. The message is user-safe.
    Error {
        /// Internal playlist id.
        playlist_id: String,
        /// User-safe failure summary.
        message: String,
    },
}

/// One queued populate.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedSpotifyImport {
    /// Durable job key.
    pub key: String,
    /// Owning user.
    pub user_id: String,
    /// Spotify playlist id.
    pub spotify_playlist_id: String,
    /// Internal playlist id to fill.
    pub playlist_id: String,
}

/// Minimal local seam onto the downloads state machine: run one queued
/// populate. [`TaskExecutor`] below is the task-spawning in-memory
/// implementation.
pub trait SpotifyImportExecutor: Send + Sync {
    /// Run `job` to completion, recording its terminal state.
    fn execute(&self, job: QueuedSpotifyImport);
}

/// Registry behind the single-flight rule and the status route.
#[derive(Debug, Default)]
pub struct JobRegistry {
    inner: Mutex<JobTables>,
}

/// Terminal states kept for the status route; older ones are dropped.
pub const KEPT_TERMINAL_STATES: usize = 256;

/// Live keys plus terminal states, oldest terminal key first.
#[derive(Debug, Default)]
struct JobTables {
    running: HashSet<String>,
    states: HashMap<String, JobState>,
    finished: VecDeque<String>,
}

impl JobRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// True when `key` is currently running.
    pub fn is_running(&self, key: &str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .running
            .contains(key)
    }

    /// Latest known state for `key`, if any.
    pub fn state_for(&self, key: &str) -> Option<JobState> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .states
            .get(key)
            .cloned()
    }

    /// Mark `key` running. Returns false when it already runs (the lost
    /// race answers `already_running` instead of double-spawning, v2).
    pub fn mark_running(&self, key: &str, playlist_id: &str) -> bool {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !inner.running.insert(key.to_owned()) {
            return false;
        }
        inner.states.insert(
            key.to_owned(),
            JobState::Running {
                playlist_id: playlist_id.to_owned(),
            },
        );
        true
    }

    /// Record a terminal state and release the key.
    pub fn finish(&self, key: &str, state: JobState) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.running.remove(key);
        inner.states.insert(key.to_owned(), state);
        inner.finished.retain(|done| done != key);
        inner.finished.push_back(key.to_owned());
        while inner.finished.len() > KEPT_TERMINAL_STATES {
            if let Some(oldest) = inner.finished.pop_front()
                && !inner.running.contains(&oldest)
            {
                inner.states.remove(&oldest);
            }
        }
    }
}

/// Task-spawning executor: the memory [`SpotifyImportExecutor`]. Holds the
/// service behind an async runner closure so the trait itself stays sync
/// and object-safe for a durable implementation.
pub struct TaskExecutor<F> {
    registry: Arc<JobRegistry>,
    run: F,
}

impl<F> TaskExecutor<F>
where
    F: Fn(QueuedSpotifyImport) -> tokio::task::JoinHandle<(String, Result<usize, String>)>
        + Send
        + Sync
        + 'static,
{
    /// Wire the executor over a registry and a spawn closure.
    pub fn new(registry: Arc<JobRegistry>, run: F) -> Self {
        Self { registry, run }
    }
}

impl<F> SpotifyImportExecutor for TaskExecutor<F>
where
    F: Fn(QueuedSpotifyImport) -> tokio::task::JoinHandle<(String, Result<usize, String>)>
        + Send
        + Sync
        + 'static,
{
    fn execute(&self, job: QueuedSpotifyImport) {
        if !self.registry.mark_running(&job.key, &job.playlist_id) {
            return;
        }
        let registry = self.registry.clone();
        let key = job.key.clone();
        let playlist_id = job.playlist_id.clone();
        let handle = (self.run)(job);
        tokio::spawn(async move {
            let state = match handle.await {
                Ok((_, Ok(track_count))) => JobState::Done {
                    playlist_id,
                    track_count,
                },
                Ok((_, Err(message))) => JobState::Error {
                    playlist_id,
                    message,
                },
                Err(join_error) => JobState::Error {
                    playlist_id,
                    message: format!("import task failed: {join_error}"),
                },
            };
            registry.finish(&key, state);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states_are_bounded() {
        let registry = JobRegistry::new();
        for index in 0..KEPT_TERMINAL_STATES + 10 {
            let key = format!("job-{index}");
            assert!(registry.mark_running(&key, "p"));
            registry.finish(
                &key,
                JobState::Done {
                    playlist_id: "p".to_owned(),
                    track_count: 1,
                },
            );
        }
        assert!(registry.state_for("job-0").is_none());
        let last = format!("job-{}", KEPT_TERMINAL_STATES + 9);
        assert!(registry.state_for(&last).is_some());
    }
}
