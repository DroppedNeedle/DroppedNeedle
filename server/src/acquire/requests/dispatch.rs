//! Download-dispatch seam.
//!
//! Intake records the ask and decides who waits; the downloads slice owns the
//! actual fetch. Everything intake needs from downloads lives in
//! [`DownloadDispatch`]: start one fetch, cancel one fetch, and read one
//! fetch's state for status sync. Another slice implements the durable side;
//! this slice ships only the [`ScriptedDispatch`] fake for briefs.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

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
    /// An edition fill/upgrade (A:12).
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
    Failed,
    /// Cancelled.
    Cancelled,
    /// Unknown task id.
    Missing,
}

/// Dispatch failures. Validation faults are the caller's problem (quota,
/// policy); transport faults are the downloads slice's problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// The ask passed intake but failed admission (message is user-safe).
    Validation(String),
    /// The fetch could not start (message stays out of responses).
    Failed(String),
}

/// The durability seam. The downloads slice owns the implementation; intake
/// only starts, cancels, and polls through here.
pub trait DownloadDispatch: Send + Sync {
    /// Start one fetch for an admitted ask.
    fn dispatch(&self, request: &DispatchRequest) -> Result<DispatchOutcome, DispatchError>;
    /// Cancel one fetch. Unknown ids are a no-op success.
    fn cancel_task(&self, task_id: &str);
    /// Read one fetch's state for status sync.
    fn task_state(&self, task_id: &str) -> DispatchTaskState;
}

/// Scripted fake for briefs: canned outcomes in call order, every call
/// recorded, task states flipped by hand to simulate landing.
pub struct ScriptedDispatch {
    /// Queued outcomes, first call takes the front.
    script: Mutex<VecDeque<DispatchOutcome>>,
    /// Every dispatch call, in order.
    calls: Mutex<Vec<DispatchRequest>>,
    /// Cancelled task ids, in order.
    cancels: Mutex<Vec<String>>,
    /// Task states; fresh tasks start active.
    states: Mutex<HashMap<String, DispatchTaskState>>,
    /// Counter for minted task ids.
    next_task: Mutex<u64>,
}

impl ScriptedDispatch {
    /// Fake that dispatches every call as a fresh active task.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(VecDeque::new()),
            calls: Mutex::new(Vec::new()),
            cancels: Mutex::new(Vec::new()),
            states: Mutex::new(HashMap::new()),
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
}

impl Default for ScriptedDispatch {
    fn default() -> Self {
        Self {
            script: Mutex::new(VecDeque::new()),
            calls: Mutex::new(Vec::new()),
            cancels: Mutex::new(Vec::new()),
            states: Mutex::new(HashMap::new()),
            next_task: Mutex::new(1),
        }
    }
}

impl DownloadDispatch for ScriptedDispatch {
    fn dispatch(&self, request: &DispatchRequest) -> Result<DispatchOutcome, DispatchError> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(request.clone());
        }
        if let Ok(mut script) = self.script.lock()
            && let Some(outcome) = script.pop_front()
        {
            if let DispatchOutcome::Dispatched { task_id } = &outcome {
                self.set_state(task_id, DispatchTaskState::Active);
            }
            return Ok(outcome);
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
        Ok(DispatchOutcome::Dispatched { task_id })
    }

    fn cancel_task(&self, task_id: &str) {
        if let Ok(mut cancels) = self.cancels.lock() {
            cancels.push(task_id.to_owned());
        }
        self.set_state(task_id, DispatchTaskState::Cancelled);
    }

    fn task_state(&self, task_id: &str) -> DispatchTaskState {
        self.states
            .lock()
            .ok()
            .and_then(|states| states.get(task_id).copied())
            .unwrap_or(DispatchTaskState::Missing)
    }
}
