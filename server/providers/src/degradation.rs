//! The request-scoped degradation context.
//!
//! A request runs inside [`scoped`], and a provider call made for it that
//! fails records its source with [`record_current`]. A failed optional
//! source records and yields `None`, and the request still succeeds: the response reads the context to say which services were
//! degraded. The context rides a tokio task-local, so nothing threads it
//! through signatures.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    future::Future,
};

/// Per-source health inside one request. Derives severity order:
/// `Ok < Degraded < Error`, so the worst status wins comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntegrationStatus {
    /// The source answered fully.
    Ok,
    /// The source answered partially (stale cache, partial payload).
    Degraded,
    /// The source failed completely; no data came from it.
    Error,
}

/// Per-request accumulation of per-source statuses.
///
/// Keeps the worst status per source, plus the set of sources whose failures
/// were deterministic payload errors (tracked separately so callers can tell
/// "the provider is down" from "the provider changed shape").
#[derive(Debug, Default)]
pub struct DegradationContext {
    services: HashMap<&'static str, IntegrationStatus>,
    deterministic: HashSet<&'static str>,
}

impl DegradationContext {
    /// An empty context.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one status for `source`, keeping the worst per source.
    pub fn record_status(
        &mut self,
        source: &'static str,
        status: IntegrationStatus,
        deterministic: bool,
    ) {
        self.services
            .entry(source)
            .and_modify(|kept| *kept = (*kept).max(status))
            .or_insert(status);
        if deterministic {
            self.deterministic.insert(source);
        }
    }

    /// Every recorded source and its worst status.
    #[must_use]
    pub fn summary(&self) -> HashMap<&'static str, IntegrationStatus> {
        self.services.clone()
    }

    /// Only sources that are not `Ok`.
    #[must_use]
    pub fn degraded_summary(&self) -> HashMap<&'static str, IntegrationStatus> {
        self.services
            .iter()
            .filter(|(_, status)| **status != IntegrationStatus::Ok)
            .map(|(source, status)| (*source, *status))
            .collect()
    }

    /// Whether any source degraded or failed.
    #[must_use]
    pub fn has_degradation(&self) -> bool {
        self.services
            .values()
            .any(|status| *status != IntegrationStatus::Ok)
    }

    /// Sources whose recorded failures include a deterministic payload error.
    #[must_use]
    pub fn deterministic_sources(&self) -> HashSet<&'static str> {
        self.deterministic.clone()
    }

    /// Whether any deterministic payload failure was recorded.
    #[must_use]
    pub fn has_deterministic_failure(&self) -> bool {
        !self.deterministic.is_empty()
    }
}

tokio::task_local! {
    static CURRENT: RefCell<DegradationContext>;
}

/// Run `future` with a fresh degradation context installed, returning the
/// future's output plus everything it recorded. Middleware installs this per
/// request; handlers and repositories record into it without threading it
/// through every signature.
pub async fn scoped<F>(future: F) -> (F::Output, DegradationContext)
where
    F: Future,
{
    CURRENT
        .scope(RefCell::new(DegradationContext::new()), async {
            let output = future.await;
            let recorded = CURRENT.try_with(|cell| std::mem::take(&mut *cell.borrow_mut()));
            let context = recorded.unwrap_or_default();
            (output, context)
        })
        .await
}

/// Record into the current request's context, or do nothing outside a
/// request scope (background tasks, startup). Returns whether the recording
/// landed.
pub fn record_current(
    source: &'static str,
    status: IntegrationStatus,
    deterministic: bool,
) -> bool {
    CURRENT
        .try_with(|cell| {
            cell.borrow_mut()
                .record_status(source, status, deterministic);
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worst_status_wins_per_source() {
        let mut context = DegradationContext::new();
        assert!(!context.has_degradation());
        context.record_status("lastfm", IntegrationStatus::Ok, false);
        context.record_status("lastfm", IntegrationStatus::Error, false);
        context.record_status("lastfm", IntegrationStatus::Ok, false);
        assert_eq!(
            context.summary().get("lastfm"),
            Some(&IntegrationStatus::Error)
        );
        assert!(context.has_degradation());
        assert!(context.degraded_summary().contains_key("lastfm"));
    }

    #[test]
    fn deterministic_failures_track_separately() {
        let mut context = DegradationContext::new();
        assert!(!context.has_deterministic_failure());
        context.record_status("musicbrainz", IntegrationStatus::Error, true);
        assert!(context.has_deterministic_failure());
        assert!(context.deterministic_sources().contains("musicbrainz"));
    }

    #[tokio::test]
    async fn scoped_collects_recordings() {
        let (output, context) = scoped(async {
            record_current("audiodb", IntegrationStatus::Error, false);
            record_current("lastfm", IntegrationStatus::Degraded, false);
            "done"
        })
        .await;
        assert_eq!(output, "done");
        assert!(context.has_degradation());
        assert_eq!(
            context.degraded_summary().get("audiodb"),
            Some(&IntegrationStatus::Error)
        );
        assert_eq!(
            context.degraded_summary().get("lastfm"),
            Some(&IntegrationStatus::Degraded)
        );
    }

    #[tokio::test]
    async fn recording_outside_a_scope_is_a_quiet_no() {
        assert!(!record_current("lastfm", IntegrationStatus::Error, false));
    }
}
