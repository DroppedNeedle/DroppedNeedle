//! Session reports from the web player to Jellyfin, Navidrome and Plex.
//!
//! The playback routes hand each report to [`ReportQueue`], a bounded
//! channel, and answer at once; a background worker
//! ([`run_report_worker`], on the supervisor through `MediaWorkers`)
//! sends them one at a time, so a start never arrives after its stop. A
//! full queue drops the report: playback never waits on one. Each report
//! is sent once, under the listener's own linked account
//! ([`SHARED_ACCOUNT_FALLBACK`]), through `ConnectionResolver::client`.
//! A server that does not answer gets no reports for [`FAILURE_HOLD_MS`],
//! so one that is down costs the serial worker one timeout a minute, not
//! one per listener; a refused account holds only that listener's reports.

use std::collections::HashMap;
use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::jobs::CancellationToken;
use droppedneedle_providers::RequestPriority;

use crate::remotes::adapter::{AdapterError, RemoteHandle};
use crate::remotes::connections::{ConnectionResolver, Reads, ResolveError, ResolvedConnection};
use crate::remotes::jellyfin::JellyfinAdapter;
use crate::remotes::models::SourceName;
use crate::remotes::navidrome::NavidromeAdapter;
use crate::remotes::plex::PlexAdapter;

use super::ports::{RemoteReport, RemoteReporters};
use super::rules::meets_subsonic_threshold;

/// Whose account a web-player play is reported under. `false`: only the
/// listener's own linked account, so nobody's plays land in another
/// person's history; a listener with no link is not reported. `true`
/// falls back to the admin's shared account for them, as v2 did.
const SHARED_ACCOUNT_FALLBACK: bool = false;

/// How long reports to a server that did not answer, or under an account
/// it refused, are dropped; also how often an error answer is logged as a
/// warning per server.
pub const FAILURE_HOLD_MS: i64 = 60_000;

/// The most plays whose start position the worker keeps. Past it they are
/// all forgotten, and a stop of one counts from the track's beginning.
const MAX_OPEN_PLAYS: usize = 10_000;

/// The session reports the worker delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportOp {
    /// Playback started.
    Start,
    /// Playback heartbeat or pause.
    Progress,
    /// Playback stopped.
    Stop,
}

/// One queued report.
#[derive(Debug, Clone)]
pub struct QueuedReport {
    /// The server it goes to.
    pub source: SourceName,
    /// Which report it is.
    pub op: ReportOp,
    /// The play.
    pub report: RemoteReport,
}

/// Channel depth: bursts past this are dropped, never queued behind a
/// struggling server.
pub const REPORT_QUEUE_DEPTH: usize = 256;

/// Non-blocking report handle over the worker channel.
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
    /// tests that never assert what a server was sent.
    #[cfg(any(test, feature = "test-support"))]
    pub fn detached() -> Self {
        let (queue, _) = Self::channel();
        queue
    }

    fn enqueue(&self, source: SourceName, op: ReportOp, report: &RemoteReport) {
        let queued = QueuedReport {
            source,
            op,
            report: report.clone(),
        };
        if self.tx.try_send(queued).is_err() {
            tracing::debug!(source = source.as_str(), op = ?op, "report queue full; dropping report");
        }
    }
}

impl RemoteReporters for ReportQueue {
    fn report_start(&self, source: SourceName, report: &RemoteReport) {
        self.enqueue(source, ReportOp::Start, report);
    }

    fn report_progress(&self, source: SourceName, report: &RemoteReport) {
        self.enqueue(source, ReportOp::Progress, report);
    }

    fn report_stop(&self, source: SourceName, report: &RemoteReport) {
        self.enqueue(source, ReportOp::Stop, report);
    }
}

/// The next queued item. Once `stop` is cancelled the queue is closed:
/// what is already in it still comes out, then `None`, so a clean stop
/// delivers everything that was queued.
pub(crate) async fn next_queued<T>(
    rx: &mut tokio::sync::mpsc::Receiver<T>,
    stop: &CancellationToken,
) -> Option<T> {
    tokio::select! {
        biased;
        item = rx.recv() => item,
        () = stop.cancelled() => {
            rx.close();
            rx.recv().await
        }
    }
}

/// What the worker remembers between reports, by kernel-clock milliseconds.
#[derive(Default)]
struct WorkerState {
    /// Servers that did not answer, until when no report goes to them.
    down_until: HashMap<SourceName, i64>,
    /// Listeners whose account a server refused, until when their reports
    /// to it are dropped.
    refused_until: HashMap<(String, SourceName), i64>,
    /// When an error answer from each server was last logged as a warning.
    warned_at: HashMap<SourceName, i64>,
    /// Where each open play started, by listener, server and item: a
    /// resumed track counts only what was heard after it.
    started_at: HashMap<(String, SourceName, String), i64>,
}

impl WorkerState {
    fn on_hold(&mut self, user_id: &str, source: SourceName, now_ms: i64) -> bool {
        self.down_until.retain(|_, until| *until > now_ms);
        self.refused_until.retain(|_, until| *until > now_ms);
        self.down_until.contains_key(&source)
            || self
                .refused_until
                .contains_key(&(user_id.to_owned(), source))
    }

    /// Note where a play starts, or take back where it started once it
    /// stops: the milliseconds heard by its stop, when the stop has a
    /// position.
    fn track(&mut self, source: SourceName, op: ReportOp, report: &RemoteReport) -> Option<i64> {
        let key = (report.user_id.clone(), source, report.item_id.clone());
        match op {
            ReportOp::Start => {
                if self.started_at.len() >= MAX_OPEN_PLAYS {
                    self.started_at.clear();
                }
                self.started_at.insert(key, report.position_ms.unwrap_or(0));
                None
            }
            ReportOp::Progress => None,
            ReportOp::Stop => {
                let from = self.started_at.remove(&key).unwrap_or(0);
                report.position_ms.map(|position| position - from)
            }
        }
    }

    /// Log a failed report, and hold what it calls for: a server that did
    /// not answer for everyone, a refused account for that listener. An
    /// error answer (a 4xx for one item, or a server error that came back
    /// at once) holds nothing and warns at most once a minute per server.
    fn failed(&mut self, user_id: &str, source: SourceName, error: &AdapterError, now_ms: i64) {
        let kind = failure_kind(error);
        match error {
            AdapterError::Transport(_) => {
                self.down_until.insert(source, now_ms + FAILURE_HOLD_MS);
                tracing::warn!(
                    source = source.as_str(),
                    kind,
                    "media server report failed; holding reports to it for a minute"
                );
            }
            AdapterError::Auth => {
                self.refused_until
                    .insert((user_id.to_owned(), source), now_ms + FAILURE_HOLD_MS);
                tracing::warn!(
                    source = source.as_str(),
                    kind,
                    "media server report failed; holding this listener's reports for a minute"
                );
            }
            AdapterError::Api(_) => {
                let quiet = self
                    .warned_at
                    .get(&source)
                    .is_some_and(|at| now_ms - at < FAILURE_HOLD_MS);
                if quiet {
                    tracing::debug!(source = source.as_str(), kind, "media server report failed");
                } else {
                    self.warned_at.insert(source, now_ms);
                    tracing::warn!(source = source.as_str(), kind, "media server report failed");
                }
            }
            _ => {
                tracing::debug!(
                    source = source.as_str(),
                    kind,
                    "media server report not taken"
                );
            }
        }
    }
}

/// Send queued reports until every queue handle drops or `stop` is
/// cancelled, delivering what was queued before the stop. Each report is
/// one call to the server and is never retried.
pub async fn run_report_worker(
    mut rx: tokio::sync::mpsc::Receiver<QueuedReport>,
    http: reqwest::Client,
    resolver: Arc<ConnectionResolver>,
    clock: Arc<dyn Clock>,
    stop: CancellationToken,
) {
    let mut state = WorkerState::default();
    while let Some(queued) = next_queued(&mut rx, &stop).await {
        deliver(
            &queued,
            &http,
            resolver.as_ref(),
            clock.as_ref(),
            &mut state,
        )
        .await;
    }
}

/// The account a report goes under (see [`SHARED_ACCOUNT_FALLBACK`]).
/// Nobody waits on a report, so a plex.tv lookup it needs is background.
async fn account(
    resolver: &ConnectionResolver,
    user_id: &str,
    source: SourceName,
) -> Result<ResolvedConnection, ResolveError> {
    let priority = RequestPriority::BackgroundSync;
    if SHARED_ACCOUNT_FALLBACK {
        resolver.resolve(user_id, source, priority).await
    } else {
        resolver.resolve_linked(user_id, source, priority).await
    }
}

async fn deliver(
    queued: &QueuedReport,
    http: &reqwest::Client,
    resolver: &ConnectionResolver,
    clock: &dyn Clock,
    state: &mut WorkerState,
) {
    let QueuedReport { source, op, report } = queued;
    let source = *source;
    let heard_ms = state.track(source, *op, report);
    if state.on_hold(&report.user_id, source, clock.now_ms()) {
        tracing::debug!(
            source = source.as_str(),
            "reports to this server are on hold; dropping report"
        );
        return;
    }
    let resolved = match account(resolver, &report.user_id, source).await {
        Ok(resolved) => resolved,
        Err(ResolveError::NotConfigured) => {
            tracing::debug!(
                source = source.as_str(),
                "no linked account; dropping report"
            );
            return;
        }
        Err(ResolveError::Stale) => {
            tracing::debug!(
                source = source.as_str(),
                "linked account needs reconnecting; dropping report"
            );
            return;
        }
        Err(ResolveError::Store(cause)) => {
            tracing::warn!(
                source = source.as_str(),
                %cause,
                "connection lookup failed; dropping report"
            );
            return;
        }
    };
    let outcome = match &resolver.client(http, resolved, Reads::Background) {
        RemoteHandle::Jellyfin(adapter) => deliver_jellyfin(adapter, *op, report).await,
        RemoteHandle::Navidrome(adapter) => {
            deliver_navidrome(adapter, *op, report, heard_ms, clock.now_ms()).await
        }
        RemoteHandle::Plex(adapter) => deliver_plex(adapter, *op, report).await,
    };
    if let Err(error) = outcome {
        state.failed(&report.user_id, source, &error, clock.now_ms());
    }
}

/// What went wrong, without the server's address or answer.
fn failure_kind(error: &AdapterError) -> &'static str {
    match error {
        AdapterError::NotConfigured => "not configured",
        AdapterError::Auth => "credential refused",
        AdapterError::Api(_) => "error answer",
        AdapterError::NotFound => "not found",
        AdapterError::Unsupported(_) => "unsupported",
        AdapterError::Transport(_) => "no answer",
    }
}

/// Jellyfin 10.10.6 session reports (`PlaystateController`): a start
/// counts the play there and a stop with its position marks it played.
/// Jellyfin has no scrobble call.
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
        ReportOp::Stop => {
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

/// Navidrome 0.62.0 reports through `scrobble`, as Subsonic apps send it:
/// now playing on start, and the play itself on a stop once half the track
/// or four minutes was heard since the start (Navidrome's own threshold),
/// so a resumed track counts only what was played after it. Progress sends
/// nothing: each now-playing call makes Navidrome tell Last.fm or
/// ListenBrainz again, and the start's entry lasts to the track's end. A
/// stop with no position never counts.
async fn deliver_navidrome(
    adapter: &NavidromeAdapter,
    op: ReportOp,
    report: &RemoteReport,
    heard_ms: Option<i64>,
    now_ms: i64,
) -> Result<(), AdapterError> {
    match op {
        ReportOp::Start => adapter.report_now_playing(&report.item_id).await,
        ReportOp::Progress => Ok(()),
        ReportOp::Stop => {
            let heard = heard_ms.is_some_and(|heard| {
                meets_subsonic_threshold(heard, report.duration_ms.unwrap_or(0))
            });
            if heard {
                adapter.scrobble(&report.item_id, now_ms).await
            } else {
                Ok(())
            }
        }
    }
}

/// Plex timeline reports: `playing` on start (at the position playback
/// starts from), `playing` or `paused` on progress, `stopped` on stop.
/// Plex counts the play from the timeline, as it does for its own players,
/// so no scrobble is sent.
async fn deliver_plex(
    adapter: &PlexAdapter,
    op: ReportOp,
    report: &RemoteReport,
) -> Result<(), AdapterError> {
    let state = match op {
        ReportOp::Progress if report.is_paused => "paused",
        ReportOp::Start | ReportOp::Progress => "playing",
        ReportOp::Stop => "stopped",
    };
    let time_ms = report.position_ms.unwrap_or(0);
    adapter
        .timeline(&report.item_id, state, time_ms, report.duration_ms)
        .await
}
