//! The Usenet download client in use: SABnzbd or NZBGet.
//!
//! One Usenet client runs at a time. [`UsenetQueue`] hands each call to
//! the one configured, so the source, the probes and the worker never
//! branch on which it is. Both answer the same handle, status and
//! materialization shapes.

use std::path::Path;
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
}

impl UsenetError {
    /// Whether this failure means bad credentials.
    pub fn is_auth(&self) -> bool {
        match self {
            UsenetError::Sabnzbd(error) => error.is_auth(),
            UsenetError::Nzbget(error) => error.is_auth(),
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
