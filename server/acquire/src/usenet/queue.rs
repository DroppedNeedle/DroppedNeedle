//! The Usenet download clients: SABnzbd and NZBGet.
//!
//! [`UsenetQueue`] is one client, so the probes and the settings checks
//! never branch on which it is. [`UsenetClients`] is what the source
//! holds: new jobs go to one client (SABnzbd when both are on), and a job
//! already sent stays with the client that took it, told apart by its id
//! (NZBGet's are integers, SABnzbd's `SABnzbd_nzo_...`). Both answer the
//! same handle, status and materialization shapes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;

use super::nzbget::{NzbgetError, NzbgetQueue};
use super::sabnzbd::{
    Health, Materialization, MountDiagnosis, NzbGrab, SabnzbdError, SabnzbdQueue, TaskHandle,
    TaskStatus,
};
use crate::client_error::ClientError;

/// The configured Usenet download client.
#[derive(Clone)]
pub enum UsenetQueue {
    /// SABnzbd.
    Sabnzbd(Arc<SabnzbdQueue>),
    /// NZBGet.
    Nzbget(Arc<NzbgetQueue>),
}

/// A failed Usenet client call, from whichever client is in use.
#[derive(Debug, Clone, Error)]
pub enum UsenetError {
    /// SABnzbd's failure.
    #[error(transparent)]
    Sabnzbd(#[from] SabnzbdError),
    /// NZBGet's failure.
    #[error(transparent)]
    Nzbget(#[from] NzbgetError),
    /// The client that took the job is no longer set up.
    #[error("the {0} client that took this job is not set up")]
    ClientGone(&'static str),
}

impl UsenetError {
    /// Whether this failure means bad credentials.
    pub fn is_auth(&self) -> bool {
        match self {
            UsenetError::Sabnzbd(error) => error.is_auth(),
            UsenetError::Nzbget(error) => error.is_auth(),
            UsenetError::ClientGone(_) => false,
        }
    }

    /// Whether two jobs matched one handle.
    pub fn is_ambiguous(&self) -> bool {
        matches!(
            self,
            UsenetError::Sabnzbd(SabnzbdError::AmbiguousIdentity)
                | UsenetError::Nzbget(NzbgetError::AmbiguousIdentity)
        )
    }

    /// The client failure underneath, when it is one any client call can
    /// have.
    pub fn client_error(&self) -> Option<&ClientError> {
        match self {
            UsenetError::Sabnzbd(SabnzbdError::Client(error))
            | UsenetError::Nzbget(NzbgetError::Client(error)) => Some(error),
            _ => None,
        }
    }
}

macro_rules! delegate {
    ($self:ident, $queue:ident => $call:expr) => {
        match $self {
            UsenetQueue::Sabnzbd($queue) => $call.map_err(UsenetError::from),
            UsenetQueue::Nzbget($queue) => $call.map_err(UsenetError::from),
        }
    };
}

impl UsenetQueue {
    /// Client name for routing: `sabnzbd` or `nzbget`.
    pub fn client_name(&self) -> &'static str {
        match self {
            UsenetQueue::Sabnzbd(queue) => queue.client_name(),
            UsenetQueue::Nzbget(queue) => queue.client_name(),
        }
    }

    /// Whether the client had what it needs at build time.
    pub fn is_configured(&self) -> bool {
        match self {
            UsenetQueue::Sabnzbd(queue) => queue.is_configured(),
            UsenetQueue::Nzbget(queue) => queue.is_configured(),
        }
    }

    /// DroppedNeedle's mount of the client's completed folder.
    pub fn downloads_mount(&self) -> &Path {
        match self {
            UsenetQueue::Sabnzbd(queue) => queue.downloads_mount(),
            UsenetQueue::Nzbget(queue) => queue.downloads_mount(),
        }
    }

    /// Health never raises: failures become the error variant.
    pub async fn health_check(&self) -> Health {
        match self {
            UsenetQueue::Sabnzbd(queue) => queue.health_check().await,
            UsenetQueue::Nzbget(queue) => queue.health_check().await,
        }
    }

    /// The client's category names.
    pub async fn get_categories(&self) -> Result<Vec<String>, UsenetError> {
        delegate!(self, queue => queue.get_categories().await)
    }

    /// The client's completed folder.
    pub async fn get_complete_dir(&self) -> Result<String, UsenetError> {
        delegate!(self, queue => queue.get_complete_dir().await)
    }

    /// Enqueue one album release under `droppedneedle-{task_id}`.
    pub async fn enqueue_album(
        &self,
        task_id: &str,
        nzb_url: Option<&str>,
        grab: NzbGrab<'_>,
        category: Option<&str>,
        priority: Option<i32>,
        post_processing: Option<i32>,
    ) -> Result<TaskHandle, UsenetError> {
        delegate!(self, queue => queue
            .enqueue_album(task_id, nzb_url, grab, category, priority, post_processing)
            .await)
    }

    /// Enqueue one album release under a worker-built job name.
    #[allow(clippy::too_many_arguments)]
    pub async fn enqueue_album_as(
        &self,
        task_id: &str,
        job_name: &str,
        nzb_url: Option<&str>,
        grab: NzbGrab<'_>,
        category: Option<&str>,
        priority: Option<i32>,
        post_processing: Option<i32>,
    ) -> Result<TaskHandle, UsenetError> {
        delegate!(self, queue => queue
            .enqueue_album_as(task_id, job_name, nzb_url, grab, category, priority, post_processing)
            .await)
    }

    /// Enqueue one track under a track-derived job name.
    #[allow(clippy::too_many_arguments)]
    pub async fn enqueue_track(
        &self,
        task_id: &str,
        track_job: &str,
        nzb_url: Option<&str>,
        grab: NzbGrab<'_>,
        category: Option<&str>,
        priority: Option<i32>,
        post_processing: Option<i32>,
    ) -> Result<TaskHandle, UsenetError> {
        delegate!(self, queue => queue
            .enqueue_track(task_id, track_job, nzb_url, grab, category, priority, post_processing)
            .await)
    }

    /// The job's status, from the queue or history.
    pub async fn get_status(&self, handle: &TaskHandle) -> Result<TaskStatus, UsenetError> {
        delegate!(self, queue => queue.get_status(handle).await)
    }

    /// Stop an active job, removing only client-owned incomplete data.
    pub async fn abort(&self, handle: &TaskHandle) -> Result<bool, UsenetError> {
        delegate!(self, queue => queue.abort(handle).await)
    }

    /// Fresh evidence about the job's bytes and records.
    pub async fn inspect_materialization(
        &self,
        handle: &TaskHandle,
    ) -> Result<Materialization, UsenetError> {
        delegate!(self, queue => queue.inspect_materialization(handle).await)
    }

    /// Drop the client's records once local cleanup is durable.
    pub async fn discard_client_artifacts(&self, handle: &TaskHandle) -> Result<bool, UsenetError> {
        delegate!(self, queue => queue.discard_client_artifacts(handle).await)
    }

    /// Cross-check completions against the mount. Never raises.
    pub async fn diagnose_downloads_mount(&self) -> MountDiagnosis {
        match self {
            UsenetQueue::Sabnzbd(queue) => queue.diagnose_downloads_mount().await,
            UsenetQueue::Nzbget(queue) => queue.diagnose_downloads_mount().await,
        }
    }
}

/// The Usenet clients a job can be on: the one new jobs go to, then the
/// other client while it is set up, so a job it took is followed to the
/// end after a switch.
#[derive(Clone)]
pub struct UsenetClients {
    adds: UsenetQueue,
    others: Vec<UsenetQueue>,
}

impl From<UsenetQueue> for UsenetClients {
    fn from(adds: UsenetQueue) -> Self {
        UsenetClients::new(adds, [])
    }
}

impl UsenetClients {
    /// New jobs go to `adds`; `others` are the other clients that are set
    /// up, enabled or not.
    pub fn new(adds: UsenetQueue, others: impl IntoIterator<Item = UsenetQueue>) -> Self {
        UsenetClients {
            adds,
            others: others.into_iter().collect(),
        }
    }

    /// The client new jobs go to.
    pub fn adds(&self) -> &UsenetQueue {
        &self.adds
    }

    fn all(&self) -> impl Iterator<Item = &UsenetQueue> {
        std::iter::once(&self.adds).chain(&self.others)
    }

    /// The client a handle's job is on. A stored id names its client by
    /// its shape; before the id is stored every client is asked, the one
    /// new jobs go to first.
    fn for_handle(&self, handle: &TaskHandle) -> Result<Vec<&UsenetQueue>, UsenetError> {
        let Some(name) = client_for_id(&handle.nzo_id) else {
            return Ok(self.all().collect());
        };
        match self.all().find(|queue| queue.client_name() == name) {
            Some(queue) => Ok(vec![queue]),
            None => Err(UsenetError::ClientGone(name)),
        }
    }

    /// One client's answer while a handle is asked of several. The client
    /// new jobs go to (asked first) must answer; another that fails is
    /// skipped with a log line, so a client that is off and down does not
    /// hold up jobs that are not on it. `None` means skipped.
    fn answered<T>(
        position: usize,
        queue: &UsenetQueue,
        answer: Result<T, UsenetError>,
    ) -> Result<Option<T>, UsenetError> {
        match answer {
            Ok(answer) => Ok(Some(answer)),
            Err(error) if position > 0 => {
                tracing::warn!(client = queue.client_name(), %error, "usenet client skipped for a job without an id");
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// The folders a handle's job may land in.
    pub fn downloads_mounts(&self, handle: &TaskHandle) -> Result<Vec<PathBuf>, UsenetError> {
        Ok(self
            .for_handle(handle)?
            .into_iter()
            .map(|queue| queue.downloads_mount().to_path_buf())
            .collect())
    }

    /// The job's status from the client that holds it; `missing` when
    /// none does.
    pub async fn get_status(&self, handle: &TaskHandle) -> Result<TaskStatus, UsenetError> {
        let mut missing = TaskStatus {
            status: "missing".to_owned(),
            ..TaskStatus::default()
        };
        for (position, queue) in self.for_handle(handle)?.into_iter().enumerate() {
            let answer = queue.get_status(handle).await;
            let Some(status) = Self::answered(position, queue, answer)? else {
                continue;
            };
            if status.matched_transfers > 0 {
                return Ok(status);
            }
            missing = status;
        }
        Ok(missing)
    }

    /// Fresh evidence about the job from the client that holds it. A job
    /// no client holds reads `missing`, with the mount healthy only when
    /// every client asked answered and had a healthy one.
    pub async fn inspect_materialization(
        &self,
        handle: &TaskHandle,
    ) -> Result<Materialization, UsenetError> {
        let mut missing: Option<Materialization> = None;
        let mut all_healthy = true;
        for (position, queue) in self.for_handle(handle)?.into_iter().enumerate() {
            let answer = queue.inspect_materialization(handle).await;
            let Some(seen) = Self::answered(position, queue, answer)? else {
                all_healthy = false;
                continue;
            };
            if seen.state != "missing" {
                return Ok(seen);
            }
            all_healthy &= seen.mount_healthy;
            missing.get_or_insert(seen);
        }
        Ok(missing
            .map(|first| Materialization {
                mount_healthy: all_healthy,
                ..first
            })
            .unwrap_or_default())
    }

    /// Stop the job on the client that holds it. A job whose client is no
    /// longer set up has nothing left to stop.
    pub async fn abort(&self, handle: &TaskHandle) -> Result<bool, UsenetError> {
        let queues = match self.for_handle(handle) {
            Err(UsenetError::ClientGone(name)) => {
                tracing::info!(client = name, job = %handle.job_name, "client gone; nothing to abort");
                return Ok(true);
            }
            other => other?,
        };
        let mut done = true;
        for (position, queue) in queues.into_iter().enumerate() {
            let answer = queue.abort(handle).await;
            done &= Self::answered(position, queue, answer)?.unwrap_or(true);
        }
        Ok(done)
    }

    /// Drop the job's records on the client that holds it. A job whose
    /// client is no longer set up has no records left to drop.
    pub async fn discard_client_artifacts(&self, handle: &TaskHandle) -> Result<bool, UsenetError> {
        let queues = match self.for_handle(handle) {
            Err(UsenetError::ClientGone(name)) => {
                tracing::info!(client = name, job = %handle.job_name, "client gone; nothing to discard");
                return Ok(true);
            }
            other => other?,
        };
        let mut done = true;
        for (position, queue) in queues.into_iter().enumerate() {
            let answer = queue.discard_client_artifacts(handle).await;
            done &= Self::answered(position, queue, answer)?.unwrap_or(true);
        }
        Ok(done)
    }
}

/// The client a stored job id belongs to: NZBGet's ids are integers,
/// anything else is a SABnzbd nzo id. `None` before an id is stored.
pub fn client_for_id(nzo_id: &str) -> Option<&'static str> {
    if nzo_id.is_empty() {
        None
    } else if nzo_id.parse::<i64>().is_ok() {
        Some("nzbget")
    } else {
        Some("sabnzbd")
    }
}
