//! Download-dispatch seam.
//!
//! Intake records the ask and decides who waits; the downloads module owns the
//! actual fetch. Everything intake needs from downloads lives in
//! [`DownloadDispatch`]: start one fetch, cancel one fetch, and read one
//! fetch's state for status sync. The downloads module implements the
//! durable side; [`ScriptedDispatch`] is the test fake.

#[cfg(any(test, feature = "test-support"))]
use std::collections::{HashMap, HashSet, VecDeque};
#[cfg(any(test, feature = "test-support"))]
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;

/// What triggered a dispatch. Origins decide quota exemptions, never routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchOrigin {
    /// A fresh ask from intake.
    User,
    /// An admin approval releasing a waiting ask.
    Approval,
    /// A retry of a terminal ask (quota-exempt: already admitted once).
    Retry,
    /// A wanted-watch redispatch (quota-exempt for the same reason).
    Wanted,
    /// An upgrade of owned files (size-neutral, quota-exempt).
    Upgrade,
    /// An edition fill/upgrade.
    Edition,
}

impl DispatchOrigin {
    /// Wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Approval => "approval",
            Self::Retry => "retry",
            Self::Wanted => "wanted",
            Self::Upgrade => "upgrade",
            Self::Edition => "edition",
        }
    }
}

/// One fetch to start.
#[derive(Debug, Clone)]
pub struct DispatchRequest {
    /// Primary owner id (immutable request attribution, never the actor).
    pub user_id: String,
    /// `album`, `track`, or `edition`.
    pub kind: String,
    /// Album, recording, or release-group id.
    pub key: String,
    /// Artist name.
    pub artist_name: String,
    /// Album or track title.
    pub title: String,
    /// What triggered this dispatch.
    pub origin: DispatchOrigin,
    /// Edition release id, when pinned.
    pub release_mbid: Option<String>,
    /// Caller-supplied idempotency key. A repeat dispatch with the same
    /// key answers the original task instead of minting a duplicate;
    /// `None` always mints fresh.
    pub idempotency_key: Option<String>,
}

/// How a dispatch call resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// A fetch started under this task id.
    Dispatched {
        /// Download task id.
        task_id: String,
    },
    /// The ask already lives in the library (v2 `ALREADY_IN_LIBRARY`).
    AlreadyInLibrary,
}

/// One fetch's state as status sync reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchTaskState {
    /// Still working.
    Active,
    /// Landed and imported.
    Imported,
    /// Landed short.
    Incomplete,
    /// Failed.
    Failed,
    /// Cancelled.
    Cancelled,
    /// Unknown task id.
    Missing,
}

/// Dispatch failures. Validation faults are the caller's problem (quota,
/// policy); transport faults are the downloads module's problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// The ask passed intake but failed admission (message is user-safe).
    Validation(String),
    /// The fetch could not start (message stays out of responses).
    Failed(String),
}

/// One fetch's progress snapshot as the request views read it. Every field
/// mirrors a `download_tasks` column; the views render what is present and
/// hide what is not (v2 served these same shapes, always empty).
#[derive(Debug, Clone, Default)]
pub struct TaskProgress {
    /// Task status wire string (`queued`, `downloading`, ...).
    pub status: String,
    /// Progress percent (0-100) the worker last reported.
    pub progress_percent: i64,
    /// Total transfer bytes, once known.
    pub total_size_bytes: Option<i64>,
    /// Bytes transferred so far.
    pub downloaded_bytes: i64,
    /// Last outcome text.
    pub error_message: Option<String>,
    /// Picked candidate quality (format label), once picked.
    pub quality: Option<String>,
    /// Fetch source (`soulseek`, `usenet`, `plugin:<key>`).
    pub protocol: String,
}

/// The durability seam. The downloads module owns the implementation; intake
/// only starts, cancels, and polls through here.
pub trait DownloadDispatch: Send + Sync {
    /// Start one fetch for an admitted ask.
    fn dispatch<'a>(
        &'a self,
        request: &'a DispatchRequest,
    ) -> BoxFuture<'a, Result<DispatchOutcome, DispatchError>>;
    /// Cancel one fetch: the task stops and its live transfer is handed to
    /// cleanup. Unknown or finished ids are a no-op success.
    fn cancel_task<'a>(&'a self, task_id: &'a str) -> BoxFuture<'a, Result<(), DispatchError>>;
    /// Read one fetch's state for status sync.
    fn task_state<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<DispatchTaskState, DispatchError>>;
    /// Read one fetch's progress snapshot for the request views. Unknown
    /// ids answer None.
    fn task_progress<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<TaskProgress>, DispatchError>>;
    /// Whether one fetch can be reimported (failed or short-landed with
    /// its candidate still linked).
    fn reimportable<'a>(&'a self, task_id: &'a str) -> BoxFuture<'a, Result<bool, DispatchError>>;
    /// The pending auto-retry for one failed fetch, or None when no retry
    /// is scheduled (auto-retry off, ladder exhausted, or not failed).
    fn retry_schedule<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<RetrySchedule>, DispatchError>>;
    /// The newest fetch one owner started for an album or recording at or
    /// after `since` (epoch seconds). Startup recovery uses it to relink a
    /// request whose task was created but never linked.
    fn find_task_since<'a>(
        &'a self,
        owner: &'a str,
        kind: &'a str,
        key: &'a str,
        since: u64,
    ) -> BoxFuture<'a, Result<Option<String>, DispatchError>>;
}

/// One scheduled auto-retry for a failed fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrySchedule {
    /// Retries already spent.
    pub retry_count: u32,
    /// Retries the ladder allows.
    pub max_attempts: u32,
    /// Epoch seconds when the next retry is due.
    pub next_retry_at: u64,
}

/// Scripted fake for tests: canned outcomes in call order, every call
/// recorded, task states flipped by hand to simulate landing.
#[cfg(any(test, feature = "test-support"))]
pub struct ScriptedDispatch {
    /// Queued outcomes, first call takes the front.
    script: Mutex<VecDeque<DispatchOutcome>>,
    /// Every dispatch call, in order.
    calls: Mutex<Vec<DispatchRequest>>,
    /// Cancelled task ids, in order.
    cancels: Mutex<Vec<String>>,
    /// Task states; fresh tasks start active.
    states: Mutex<HashMap<String, DispatchTaskState>>,
    /// Progress snapshots by task id.
    progress: Mutex<HashMap<String, TaskProgress>>,
    /// Task ids the reimport guard passes for.
    reimportable_ids: Mutex<HashSet<String>>,
    /// Scripted auto-retry schedules by task id.
    retries: Mutex<HashMap<String, RetrySchedule>>,
    /// Counter for minted task ids.
    next_task: Mutex<u64>,
}

#[cfg(any(test, feature = "test-support"))]
impl ScriptedDispatch {
    /// Fake that dispatches every call as a fresh active task.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(VecDeque::new()),
            calls: Mutex::new(Vec::new()),
            cancels: Mutex::new(Vec::new()),
            states: Mutex::new(HashMap::new()),
            progress: Mutex::new(HashMap::new()),
            reimportable_ids: Mutex::new(HashSet::new()),
            retries: Mutex::new(HashMap::new()),
            next_task: Mutex::new(1),
        })
    }

    /// Queue one canned outcome for the next dispatch call.
    pub fn push_outcome(&self, outcome: DispatchOutcome) {
        if let Ok(mut script) = self.script.lock() {
            script.push_back(outcome);
        }
    }

    /// Flip a task to landed.
    pub fn land(&self, task_id: &str) {
        self.set_state(task_id, DispatchTaskState::Imported);
    }

    /// Flip a task to failed.
    pub fn fail(&self, task_id: &str) {
        self.set_state(task_id, DispatchTaskState::Failed);
    }

    /// A task's current scripted state.
    pub fn state_of(&self, task_id: &str) -> DispatchTaskState {
        self.states
            .lock()
            .ok()
            .and_then(|states| states.get(task_id).copied())
            .unwrap_or(DispatchTaskState::Missing)
    }

    /// Flip a task's state.
    pub fn set_state(&self, task_id: &str, state: DispatchTaskState) {
        if let Ok(mut states) = self.states.lock() {
            states.insert(task_id.to_owned(), state);
        }
    }

    /// Drain the recorded dispatch calls.
    pub fn take_calls(&self) -> Vec<DispatchRequest> {
        self.calls
            .lock()
            .map(|mut calls| std::mem::take(&mut *calls))
            .unwrap_or_default()
    }

    /// Drain the recorded cancels.
    pub fn take_cancels(&self) -> Vec<String> {
        self.cancels
            .lock()
            .map(|mut cancels| std::mem::take(&mut *cancels))
            .unwrap_or_default()
    }

    /// Script one task's progress snapshot for the request views.
    pub fn set_progress(&self, task_id: &str, progress: TaskProgress) {
        if let Ok(mut snapshots) = self.progress.lock() {
            snapshots.insert(task_id.to_owned(), progress);
        }
    }

    /// Script one task's pending auto-retry.
    pub fn set_retry(&self, task_id: &str, schedule: RetrySchedule) {
        if let Ok(mut retries) = self.retries.lock() {
            retries.insert(task_id.to_owned(), schedule);
        }
    }

    /// Script one task as passing the reimport guard.
    pub fn set_reimportable(&self, task_id: &str) {
        if let Ok(mut ids) = self.reimportable_ids.lock() {
            ids.insert(task_id.to_owned());
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Default for ScriptedDispatch {
    fn default() -> Self {
        Self {
            script: Mutex::new(VecDeque::new()),
            calls: Mutex::new(Vec::new()),
            cancels: Mutex::new(Vec::new()),
            states: Mutex::new(HashMap::new()),
            progress: Mutex::new(HashMap::new()),
            reimportable_ids: Mutex::new(HashSet::new()),
            retries: Mutex::new(HashMap::new()),
            next_task: Mutex::new(1),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ScriptedDispatch {
    fn dispatch_now(&self, request: &DispatchRequest) -> DispatchOutcome {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(request.clone());
        }
        if let Ok(mut script) = self.script.lock()
            && let Some(outcome) = script.pop_front()
        {
            if let DispatchOutcome::Dispatched { task_id } = &outcome {
                self.set_state(task_id, DispatchTaskState::Active);
            }
            return outcome;
        }
        let task_id = self
            .next_task
            .lock()
            .map(|mut next| {
                let id = format!("task-{next}");
                *next += 1;
                id
            })
            .unwrap_or_else(|_| "task-fallback".to_owned());
        self.set_state(&task_id, DispatchTaskState::Active);
        DispatchOutcome::Dispatched { task_id }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl DownloadDispatch for ScriptedDispatch {
    fn dispatch<'a>(
        &'a self,
        request: &'a DispatchRequest,
    ) -> BoxFuture<'a, Result<DispatchOutcome, DispatchError>> {
        Box::pin(async move { Ok(self.dispatch_now(request)) })
    }

    fn cancel_task<'a>(&'a self, task_id: &'a str) -> BoxFuture<'a, Result<(), DispatchError>> {
        Box::pin(async move {
            if let Ok(mut cancels) = self.cancels.lock() {
                cancels.push(task_id.to_owned());
            }
            self.set_state(task_id, DispatchTaskState::Cancelled);
            Ok(())
        })
    }

    fn task_state<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<DispatchTaskState, DispatchError>> {
        Box::pin(async move {
            Ok(self
                .states
                .lock()
                .ok()
                .and_then(|states| states.get(task_id).copied())
                .unwrap_or(DispatchTaskState::Missing))
        })
    }

    fn task_progress<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<TaskProgress>, DispatchError>> {
        Box::pin(async move {
            Ok(self
                .progress
                .lock()
                .ok()
                .and_then(|snapshots| snapshots.get(task_id).cloned()))
        })
    }

    fn reimportable<'a>(&'a self, task_id: &'a str) -> BoxFuture<'a, Result<bool, DispatchError>> {
        Box::pin(async move {
            Ok(self
                .reimportable_ids
                .lock()
                .map(|ids| ids.contains(task_id))
                .unwrap_or(false))
        })
    }

    fn retry_schedule<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<RetrySchedule>, DispatchError>> {
        Box::pin(async move {
            Ok(self
                .retries
                .lock()
                .ok()
                .and_then(|retries| retries.get(task_id).copied()))
        })
    }

    fn find_task_since<'a>(
        &'a self,
        _owner: &'a str,
        _kind: &'a str,
        _key: &'a str,
        _since: u64,
    ) -> BoxFuture<'a, Result<Option<String>, DispatchError>> {
        Box::pin(async move { Ok(None) })
    }
}
