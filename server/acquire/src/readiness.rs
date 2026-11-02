//! Whether music can be acquired right now: the one answer requests,
//! manual search, the download queue and Discover read.
//!
//! A source counts once the saved settings turn it on: slskd or SABnzbd
//! with a URL and a key, or an enabled plugin download client. That is
//! the worker's own source list, so nothing tells a person to set up a
//! source the worker already uses. Reachability is reported beside it from
//! the probe cache, and never decides it: a source that is down still
//! takes requests, and the worker retries it.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use tokio::sync::watch;

use super::downloads::source_tag::SourceTag;
use crate::downloads::worker::{DownloadWorker, SourceList, WorkerSettings};
use droppedneedle_kernel::jobs::{AlreadyRunning, Supervisor};
use droppedneedle_plugins::host::PluginHost;

/// The download sources as they stand now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Readiness {
    /// Sources the saved settings turn on, in no particular order. Empty
    /// while the download settings do not read.
    pub configured: Vec<SourceTag>,
}

impl Readiness {
    /// Whether a download source is on. A request made while none is
    /// waits and starts by itself once one is saved.
    pub fn can_acquire(&self) -> bool {
        !self.configured.is_empty()
    }
}

/// Reads [`Readiness`] from the live settings on every call.
#[derive(Clone)]
pub struct SourceReadiness {
    sources: SourceList,
}

impl SourceReadiness {
    /// Readiness over the worker's sources.
    pub fn new(sources: SourceList) -> Self {
        Self { sources }
    }

    /// The sources as they stand now.
    pub fn now(&self) -> Readiness {
        let configured: Vec<SourceTag> = self
            .sources
            .current()
            .map(|sources| sources.iter().map(|source| source.tag()).collect())
            .unwrap_or_default();
        Readiness { configured }
    }

    /// Whether a download source is on now ([`Readiness::can_acquire`]).
    pub fn can_acquire(&self) -> bool {
        self.now().can_acquire()
    }
}

impl droppedneedle_catalog::discover::content::DownloadReadiness for SourceReadiness {
    fn can_acquire(&self) -> bool {
        SourceReadiness::can_acquire(self)
    }
}

/// Job name of the loop that follows the source settings and plugins.
pub const SOURCE_WATCH_JOB: &str = "acquire-source-watch";

/// What the source readiness reads, followed for changes.
#[derive(Clone)]
pub(crate) struct SourceNotices {
    pub(crate) settings: Arc<WorkerSettings>,
    pub(crate) plugins: Option<Arc<PluginHost>>,
}

/// Wait for any notice to change. False when one closed.
async fn any_changed(
    settings: &mut [watch::Receiver<i64>],
    plugins: Option<&mut watch::Receiver<u64>>,
) -> bool {
    let mut waits: Vec<BoxFuture<'_, bool>> = settings
        .iter_mut()
        .map(|notice| {
            Box::pin(async move { notice.changed().await.is_ok() }) as BoxFuture<'_, bool>
        })
        .collect();
    if let Some(notice) = plugins {
        waits.push(Box::pin(async move { notice.changed().await.is_ok() }));
    }
    if waits.is_empty() {
        return std::future::pending().await;
    }
    futures_util::future::select_all(waits).await.0
}

/// Follow everything the readiness reads. On a change, wake the
/// download worker, so a request held for want of a source starts at
/// once, and refresh the probes, so the status routes show the saved
/// client.
pub(crate) fn spawn_source_watch(
    supervisor: &Supervisor,
    notices: &SourceNotices,
    worker: Arc<DownloadWorker>,
    probe_wake: Arc<watch::Sender<u64>>,
) -> Result<(), AlreadyRunning> {
    let mut settings = notices.settings.notices();
    let mut plugins = notices.plugins.as_ref().map(|host| host.subscribe());
    supervisor.one_shot(SOURCE_WATCH_JOB, move |ctx| async move {
        loop {
            tokio::select! {
                () = ctx.token().cancelled() => return Ok(()),
                open = any_changed(&mut settings, plugins.as_mut()) => {
                    if !open {
                        tracing::warn!("a source settings notice closed; source changes are no longer followed");
                        ctx.token().cancelled().await;
                        return Ok(());
                    }
                }
            }
            worker.wake();
            probe_wake.send_modify(|count| *count = count.wrapping_add(1));
        }
    })
}
