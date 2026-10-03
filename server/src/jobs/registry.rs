//! The job registry: one runtime map plus the durable rows behind it.
//!
//! v2 scattered background work across bare `asyncio.create_task` calls and a
//! single in-process task map; the events kick
//! never registered at all, so a restart or a second kick could overlap it
//! silently. v3 registers every loop twice: a live handle in
//! [`JobRegistry`] (duplicate names rejected, cooperative cancel, grace
//! periods) and a liveness row in `durable_job_registry` (idle, running,
//! stopped, failed, plus heartbeats for the admin health view).
//!
//! [`RegistryStore`] is the seam between the two. Production binds
//! [`DurableRegistryStore`] over the writer lane; tests bind
//! [`MemoryRegistryStore`]. Store failures never kill a loop: the durable
//! adapter logs and carries on, because a registry write must not take down
//! the checkpoint pass it was recording.

use std::{
    collections::HashMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tokio::sync::Notify;

pub use crate::db::durable::{JobKind, JobRecord, JobState, WakeupChannel};
use crate::db::{DbError, durable::DurableWorkWakeups, writer::WriteLane};

/// Boxed sendable future for object-safe async ports (house style: `async fn`
/// is not object-safe, so traits hand-box).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Durable half of the registry. Infallible on purpose: implementations log
/// their own failures so a registry write can never fail the loop it tracks.
pub trait RegistryStore: Clone + Send + Sync + 'static {
    /// Insert the row, or refresh kind/channel on re-register.
    fn register_job(
        &self,
        name: &str,
        kind: JobKind,
        channel: Option<WakeupChannel>,
    ) -> BoxFuture<'_, ()>;
    /// Move a registered row to a new liveness state.
    fn set_job_state(&self, name: &str, state: JobState) -> BoxFuture<'_, ()>;
    /// Beat the row's heart without changing its state.
    fn heartbeat(&self, name: &str) -> BoxFuture<'_, ()>;
    /// Read one row, or nothing when the name never registered.
    fn get_job(&self, name: &str) -> BoxFuture<'_, Option<JobRecord>>;
    /// Every row in name order.
    fn list_jobs(&self) -> BoxFuture<'_, Vec<JobRecord>>;
}

/// In-memory registry rows for tests and for runtimes without a database.
#[derive(Debug, Clone, Default)]
pub struct MemoryRegistryStore {
    rows: Arc<Mutex<HashMap<String, JobRecord>>>,
}

impl MemoryRegistryStore {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }
}

impl RegistryStore for MemoryRegistryStore {
    fn register_job(
        &self,
        name: &str,
        kind: JobKind,
        channel: Option<WakeupChannel>,
    ) -> BoxFuture<'_, ()> {
        let rows = Arc::clone(&self.rows);
        let name = name.to_owned();
        Box::pin(async move {
            let now = now_unix();
            let mut guard = rows.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            guard
                .entry(name.clone())
                .and_modify(|row| {
                    row.kind = kind;
                    row.wakeup_channel = channel;
                    row.updated_at = now;
                })
                .or_insert_with(|| JobRecord {
                    name,
                    kind,
                    wakeup_channel: channel,
                    state: JobState::Idle,
                    last_heartbeat_at: None,
                    updated_at: now,
                });
        })
    }

    fn set_job_state(&self, name: &str, state: JobState) -> BoxFuture<'_, ()> {
        let rows = Arc::clone(&self.rows);
        let name = name.to_owned();
        Box::pin(async move {
            let now = now_unix();
            let mut guard = rows.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(row) = guard.get_mut(&name) {
                row.state = state;
                row.updated_at = now;
            }
        })
    }

    fn heartbeat(&self, name: &str) -> BoxFuture<'_, ()> {
        let rows = Arc::clone(&self.rows);
        let name = name.to_owned();
        Box::pin(async move {
            let now = now_unix();
            let mut guard = rows.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(row) = guard.get_mut(&name) {
                row.last_heartbeat_at = Some(now);
                row.updated_at = now;
            }
        })
    }

    fn get_job(&self, name: &str) -> BoxFuture<'_, Option<JobRecord>> {
        let rows = Arc::clone(&self.rows);
        let name = name.to_owned();
        Box::pin(async move {
            rows.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&name)
                .cloned()
        })
    }

    fn list_jobs(&self) -> BoxFuture<'_, Vec<JobRecord>> {
        let rows = Arc::clone(&self.rows);
        Box::pin(async move {
            let mut jobs: Vec<JobRecord> = rows
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .values()
                .cloned()
                .collect();
            jobs.sort_by(|left, right| left.name.cmp(&right.name));
            jobs
        })
    }
}

/// Production registry rows over the durable-work fabric. Every call funnels
/// through the background writer lane; failures log and resolve to nothing.
#[derive(Clone)]
pub struct DurableRegistryStore {
    wakeups: DurableWorkWakeups,
    lane: WriteLane,
}

impl DurableRegistryStore {
    /// Bind the store over a migrated pool's fabric and the writer lane.
    pub fn new(wakeups: DurableWorkWakeups, lane: WriteLane) -> Self {
        Self { wakeups, lane }
    }

    fn log_failure(operation: &str, name: &str, error: &DbError) {
        tracing::warn!(
            operation,
            name,
            error = %error,
            "job registry write failed; the loop carries on"
        );
    }
}

impl RegistryStore for DurableRegistryStore {
    fn register_job(
        &self,
        name: &str,
        kind: JobKind,
        channel: Option<WakeupChannel>,
    ) -> BoxFuture<'_, ()> {
        let store = self.clone();
        let name = name.to_owned();
        Box::pin(async move {
            if let Err(error) = store
                .wakeups
                .register_job(&store.lane, &name, kind, channel)
                .await
            {
                Self::log_failure("register", &name, &error);
            }
        })
    }

    fn set_job_state(&self, name: &str, state: JobState) -> BoxFuture<'_, ()> {
        let store = self.clone();
        let name = name.to_owned();
        Box::pin(async move {
            if let Err(error) = store.wakeups.set_job_state(&store.lane, &name, state).await {
                Self::log_failure("set-state", &name, &error);
            }
        })
    }

    fn heartbeat(&self, name: &str) -> BoxFuture<'_, ()> {
        let store = self.clone();
        let name = name.to_owned();
        Box::pin(async move {
            if let Err(error) = store.wakeups.heartbeat(&store.lane, &name).await {
                Self::log_failure("heartbeat", &name, &error);
            }
        })
    }

    fn get_job(&self, name: &str) -> BoxFuture<'_, Option<JobRecord>> {
        let store = self.clone();
        let name = name.to_owned();
        Box::pin(async move {
            store.wakeups.get_job(&name).await.unwrap_or_else(|error| {
                Self::log_failure("get", &name, &error);
                None
            })
        })
    }

    fn list_jobs(&self) -> BoxFuture<'_, Vec<JobRecord>> {
        let store = self.clone();
        Box::pin(async move {
            store.wakeups.list_jobs().await.unwrap_or_else(|error| {
                Self::log_failure("list", "-", &error);
                Vec::new()
            })
        })
    }
}

impl fmt::Debug for DurableRegistryStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DurableRegistryStore")
            .finish_non_exhaustive()
    }
}

/// How a loop finished. `Failed` carries the cause for the registry row and
/// the log; the domain tables hold anything longer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobExit {
    /// Clean shutdown: stop fired, or the loop chose to exit (one-shots,
    /// disabled plugins).
    Stopped,
    /// The task ended wrongly: panic, join failure, or an unrecoverable loop
    /// error. Cycle-level errors never surface here; loops log and continue.
    Failed(String),
}

/// What a running loop sees: its name, its stop signal, and the store.
#[derive(Clone)]
pub struct JobCtx<S> {
    name: String,
    stop: Arc<Notify>,
    store: S,
}

impl<S: RegistryStore> JobCtx<S> {
    /// The registered job name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Fires when the registry cancels this job. Loops select on it next to
    /// every sleep so shutdown never waits out a timer.
    pub fn stop(&self) -> &Arc<Notify> {
        &self.stop
    }

    /// Record one heartbeat on this job's row.
    pub async fn heartbeat(&self) {
        self.store.heartbeat(&self.name).await;
    }
}

/// Spawning failed because the name is already live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlreadyRunning;

impl fmt::Display for AlreadyRunning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("job is already running")
    }
}

impl std::error::Error for AlreadyRunning {}

struct Running {
    id: u64,
    stop: Arc<Notify>,
    handle: Mutex<Option<tokio::task::JoinHandle<JobExit>>>,
}

struct Shared<S> {
    running: Mutex<HashMap<String, Arc<Running>>>,
    next_id: AtomicU64,
    store: S,
}

/// Live job handles over durable rows. Cloneable; clones share one map.
pub struct JobRegistry<S> {
    shared: Arc<Shared<S>>,
}

impl<S: RegistryStore> Clone for JobRegistry<S> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<S: RegistryStore> fmt::Debug for JobRegistry<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobRegistry")
            .finish_non_exhaustive()
    }
}

impl<S: RegistryStore> JobRegistry<S> {
    /// Empty registry over the given rows.
    pub fn new(store: S) -> Self {
        Self {
            shared: Arc::new(Shared {
                running: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                store,
            }),
        }
    }

    /// The durable rows behind this registry.
    pub fn store(&self) -> &S {
        &self.shared.store
    }

    /// True while the name has a live task.
    pub fn is_running(&self, name: &str) -> bool {
        self.shared
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(name)
    }

    /// Names with live tasks, sorted.
    pub fn running_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .shared
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// Every durable row in name order.
    pub async fn list_jobs(&self) -> Vec<JobRecord> {
        self.shared.store.list_jobs().await
    }

    /// Spawn a loop or one-shot under `name`. Registers the durable row,
    /// marks it running, and runs `run` with a fresh stop signal. A finished
    /// task marks its row stopped or failed and unregisters itself, so a
    /// later spawn with the same name starts clean. Rejected while the name
    /// is live.
    pub async fn spawn<F, Fut>(
        &self,
        name: &str,
        kind: JobKind,
        channel: Option<WakeupChannel>,
        run: F,
    ) -> Result<(), AlreadyRunning>
    where
        F: FnOnce(JobCtx<S>) -> Fut + Send + 'static,
        Fut: Future<Output = JobExit> + Send + 'static,
    {
        let stop = Arc::new(Notify::new());
        let id = self.shared.next_id.fetch_add(1, Ordering::SeqCst);
        {
            let mut guard = self
                .shared
                .running
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.contains_key(name) {
                return Err(AlreadyRunning);
            }
            guard.insert(
                name.to_owned(),
                Arc::new(Running {
                    id,
                    stop: Arc::clone(&stop),
                    handle: Mutex::new(None),
                }),
            );
        }
        self.shared.store.register_job(name, kind, channel).await;
        self.shared
            .store
            .set_job_state(name, JobState::Running)
            .await;
        let ctx = JobCtx {
            name: name.to_owned(),
            stop,
            store: self.shared.store.clone(),
        };
        let shared = Arc::clone(&self.shared);
        let owned_name = name.to_owned();
        let handle = tokio::spawn(async move {
            // The user future runs on an inner task so a panic lands here as
            // a failed row, never as a stuck `running` entry with no task.
            let inner = tokio::spawn(run(ctx));
            let exit = match inner.await {
                Ok(exit) => exit,
                Err(join_error) => JobExit::Failed(format!("task ended: {join_error}")),
            };
            let state = match &exit {
                JobExit::Stopped => JobState::Stopped,
                JobExit::Failed(_) => JobState::Failed,
            };
            shared.store.set_job_state(&owned_name, state).await;
            if matches!(exit, JobExit::Failed(_)) {
                tracing::error!(job = %owned_name, ?exit, "background job failed");
            }
            let mut guard = shared
                .running
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.get(&owned_name).is_some_and(|live| live.id == id) {
                guard.remove(&owned_name);
            }
            exit
        });
        let guard = self
            .shared
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(live) = guard.get(name)
            && live.id == id
            && let Ok(mut slot) = live.handle.lock()
        {
            *slot = Some(handle);
        }
        Ok(())
    }

    /// Cancel one job: fire its stop, wait up to `grace` for the task, then
    /// give up waiting (the row keeps whatever the task records when it lands;
    /// unknown names and finished tasks are silent no-ops).
    pub async fn cancel(&self, name: &str, grace: Duration) {
        let live = self
            .shared
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(name);
        let Some(live) = live else {
            return;
        };
        // `notify_one` stores a permit when the task has not started waiting
        // yet; `notify_waiters` would drop the wakeup and strand the loop
        // until the grace runs out.
        live.stop.notify_one();
        let handle = live.handle.lock().ok().and_then(|mut slot| slot.take());
        let Some(handle) = handle else {
            return;
        };
        match tokio::time::timeout(grace, handle).await {
            Ok(Ok(_)) | Err(_) => {}
            Ok(Err(join_error)) => {
                self.shared
                    .store
                    .set_job_state(name, JobState::Failed)
                    .await;
                tracing::error!(job = %name, %join_error, "background job panicked");
            }
        }
    }

    /// Cancel everything live, each with the same grace. Unknown-finish races
    /// resolve per job as in [`JobRegistry::cancel`].
    pub async fn cancel_all(&self, grace: Duration) {
        let names = self.running_names();
        for name in names {
            self.cancel(&name, grace).await;
        }
    }
}

/// Current wall-clock as unix seconds for the timestamp columns.
fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}
