//! Outbound session attribution to Jellyfin, Navidrome, and Plex.
//!
//! The reporting traits are synchronous but attribution needs the network,
//! so [`ReportQueue`] hands each report to a bounded channel and a
//! background worker ([`run_report_worker`], spawned by `main`) performs
//! the HTTP. A full queue drops the report: attribution is best-effort and
//! never fails the player. Resolution is per-user fail-closed (no usable
//! connection means no report, never another account's client), matching
//! the v2 per-user factory rule.

use std::sync::Arc;

use crate::remotes::adapter::AdapterError;
use crate::remotes::connections::{
    ConnectionStore, CredentialCoder, ResolveError, resolve_connection,
};
use crate::remotes::jellyfin::JellyfinAdapter;
use crate::remotes::models::SourceName;
use crate::remotes::navidrome::NavidromeAdapter;
use crate::remotes::plex::PlexAdapter;

use super::ports::{ProviderFailure, RemoteReport, RemoteReporters};

/// Attribution operations the worker delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportOp {
    /// Playback started.
    Start,
    /// Playback heartbeat.
    Progress,
    /// Playback stopped.
    Stop,
    /// The play counted.
    Scrobble,
}

/// One queued attribution.
#[derive(Debug, Clone)]
pub struct QueuedReport {
    /// Remote source key (`jellyfin`, `navidrome`, or `plex`).
    pub source: String,
    /// Which attribution to deliver.
    pub op: ReportOp,
    /// The attributed play.
    pub report: RemoteReport,
}

/// Channel depth: attribution bursts past this are dropped, never queued
/// behind a struggling upstream.
pub const REPORT_QUEUE_DEPTH: usize = 256;

/// Non-blocking attribution handle over the worker channel.
#[derive(Debug, Clone)]
pub struct ReportQueue {
    tx: tokio::sync::mpsc::Sender<QueuedReport>,
}

impl ReportQueue {
    /// Build a queue and its worker receiver.
    pub fn channel() -> (Self, tokio::sync::mpsc::Receiver<QueuedReport>) {
        let (tx, rx) = tokio::sync::mpsc::channel(REPORT_QUEUE_DEPTH);
        (Self { tx }, rx)
    }

    /// Build a queue with no worker: every report drops immediately. For
    /// tests that never assert upstream attribution.
    pub fn detached() -> Self {
        let (queue, _) = Self::channel();
        queue
    }

    fn enqueue(&self, source: &str, op: ReportOp, report: &RemoteReport) {
        let queued = QueuedReport {
            source: source.to_owned(),
            op,
            report: report.clone(),
        };
        if self.tx.try_send(queued).is_err() {
            tracing::debug!(source, op = ?op, "report queue full; dropping attribution");
        }
    }
}

impl RemoteReporters for ReportQueue {
    fn report_start(&self, source: &str, report: &RemoteReport) -> Result<(), ProviderFailure> {
        self.enqueue(source, ReportOp::Start, report);
        Ok(())
    }

    fn report_progress(&self, source: &str, report: &RemoteReport) -> Result<(), ProviderFailure> {
        self.enqueue(source, ReportOp::Progress, report);
        Ok(())
    }

    fn report_stop(&self, source: &str, report: &RemoteReport) -> Result<(), ProviderFailure> {
        self.enqueue(source, ReportOp::Stop, report);
        Ok(())
    }

    fn scrobble(&self, source: &str, report: &RemoteReport) -> Result<(), ProviderFailure> {
        self.enqueue(source, ReportOp::Scrobble, report);
        Ok(())
    }
}

/// Drain attribution reports until every queue handle drops. Each report
/// resolves the reporter's own stored connection and delivers exactly one
/// upstream call; failures log and drop.
pub async fn run_report_worker(
    mut rx: tokio::sync::mpsc::Receiver<QueuedReport>,
    http: reqwest::Client,
    connections: Arc<dyn ConnectionStore>,
    coder: Arc<CredentialCoder>,
) {
    while let Some(queued) = rx.recv().await {
        deliver(&queued, &http, connections.as_ref(), coder.as_ref()).await;
    }
}

async fn deliver(
    queued: &QueuedReport,
    http: &reqwest::Client,
    connections: &dyn ConnectionStore,
    coder: &CredentialCoder,
) {
    let Some(source) = SourceName::parse(&queued.source) else {
        return;
    };
    let resolved =
        match resolve_connection(connections, coder, &queued.report.user_id, source).await {
            Ok(resolved) => resolved,
            Err(ResolveError::NotConfigured) => {
                tracing::debug!(
                    source = source.as_str(),
                    "no stored connection; dropping attribution"
                );
                return;
            }
            Err(ResolveError::Stale) => {
                tracing::debug!(
                    source = source.as_str(),
                    "stored credential stale; dropping attribution"
                );
                return;
            }
        };
    let outcome = match source {
        SourceName::Jellyfin => {
            let adapter = JellyfinAdapter::new(
                http.clone(),
                resolved.base_url,
                resolved.credential,
                resolved.user_id,
            );
            deliver_jellyfin(&adapter, queued.op, &queued.report).await
        }
        SourceName::Navidrome => {
            let adapter = NavidromeAdapter::new(
                http.clone(),
                resolved.base_url,
                resolved.username,
                resolved.credential,
            );
            deliver_navidrome(&adapter, queued.op, &queued.report).await
        }
        SourceName::Plex => {
            let adapter = PlexAdapter::new(
                http.clone(),
                resolved.base_url,
                resolved.credential,
                resolved.client_id,
                resolved.section_ids,
            );
            deliver_plex(&adapter, queued.op, &queued.report).await
        }
    };
    if let Err(error) = outcome {
        tracing::warn!(source = source.as_str(), %error, "remote attribution failed; continuing");
    }
}

/// Jellyfin session reports. v2 has no Jellyfin scrobble endpoint, so a
/// counted play reports Stopped, which is what marks the play server-side.
async fn deliver_jellyfin(
    adapter: &JellyfinAdapter,
    op: ReportOp,
    report: &RemoteReport,
) -> Result<(), AdapterError> {
    let ticks = report.position_ms.unwrap_or(0).saturating_mul(10_000);
    match op {
        ReportOp::Start => {
            adapter
                .report_session("/Sessions/Playing", &report.item_id, None, false)
                .await
        }
        ReportOp::Progress => {
            adapter
                .report_session(
                    "/Sessions/Playing/Progress",
                    &report.item_id,
                    Some(ticks),
                    report.is_paused,
                )
                .await
        }
        ReportOp::Stop | ReportOp::Scrobble => {
            adapter
                .report_session(
                    "/Sessions/Playing/Stopped",
                    &report.item_id,
                    Some(ticks),
                    false,
                )
                .await
        }
    }
}

/// Navidrome reports: now-playing on start and progress (the Subsonic API
/// has no progress call, so heartbeats re-report now-playing), scrobble
/// on a counted play, nothing on stop.
async fn deliver_navidrome(
    adapter: &NavidromeAdapter,
    op: ReportOp,
    report: &RemoteReport,
) -> Result<(), AdapterError> {
    match op {
        ReportOp::Start | ReportOp::Progress => adapter.report_now_playing(&report.item_id).await,
        ReportOp::Stop => Ok(()),
        ReportOp::Scrobble => adapter.scrobble(&report.item_id, now_millis()).await,
    }
}

/// Plex reports: timeline states for the session lifecycle, scrobble for
/// a counted play.
async fn deliver_plex(
    adapter: &PlexAdapter,
    op: ReportOp,
    report: &RemoteReport,
) -> Result<(), AdapterError> {
    match op {
        ReportOp::Start => adapter.timeline(&report.item_id, "playing").await,
        ReportOp::Progress => {
            let state = if report.is_paused {
                "paused"
            } else {
                "playing"
            };
            adapter.timeline(&report.item_id, state).await
        }
        ReportOp::Stop => adapter.timeline(&report.item_id, "stopped").await,
        ReportOp::Scrobble => adapter.scrobble(&report.item_id).await,
    }
}

/// Current unix time in millis, for Navidrome scrobble timestamps.
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
