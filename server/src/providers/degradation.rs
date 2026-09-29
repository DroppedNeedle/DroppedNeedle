//! Typed degradation results and the request-scoped degradation context.
//!
//! Provider calls return [`ProviderOutcome`]: the value (when there is one)
//! plus the per-source status that produced it. Optional enrichment follows
//! the record-then-`None` rule: on failure the caller records the outcome
//! into the request's [`DegradationContext`] and returns `None`, and the
//! request still succeeds. The recording **is** the error signal —
//! aggregation boundaries and the response envelope read the context instead
//! of receiving errors. Ports v2's `IntegrationResult` and `degradation.py`,
//! with the context carried by a tokio task-local instead of a contextvar.

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

/// The outcome of one external-service call: data plus upstream status.
///
/// `data` is `None` only when `status` is [`IntegrationStatus::Error`].
/// `Degraded` always carries some data, possibly stale or partial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderOutcome<T> {
    /// The value, present unless the source failed completely.
    pub data: Option<T>,
    /// Lowercase provider key.
    pub source: &'static str,
    /// How healthy this answer is.
    pub status: IntegrationStatus,
    /// Short failure note, safe for logs.
    pub error: Option<String>,
    /// True only for deterministic payload-shape failures: the service is
    /// healthy but its answer violates the verified contract.
    pub deterministic: bool,
}

impl<T> ProviderOutcome<T> {
    /// A full answer from `source`.
    #[must_use]
    pub const fn ok(data: T, source: &'static str) -> Self {
        Self {
            data: Some(data),
            source,
            status: IntegrationStatus::Ok,
            error: None,
            deterministic: false,
        }
    }

    /// A partial answer from `source`, with a note about what is missing.
    #[must_use]
    pub fn degraded(data: T, source: &'static str, message: impl Into<String>) -> Self {
        Self {
            data: Some(data),
            source,
            status: IntegrationStatus::Degraded,
            error: Some(message.into()),
            deterministic: false,
        }
    }

    /// A complete failure of `source`: no data.
    #[must_use]
    pub fn error(source: &'static str, message: impl Into<String>) -> Self {
        Self {
            data: None,
            source,
            status: IntegrationStatus::Error,
            error: Some(message.into()),
            deterministic: false,
        }
    }

    /// A deterministic payload-shape failure: no data, and tracked
    /// separately from transient degradation in summaries.
    #[must_use]
    pub fn deterministic_error(source: &'static str, message: impl Into<String>) -> Self {
        Self {
            data: None,
            source,
            status: IntegrationStatus::Error,
            error: Some(message.into()),
            deterministic: true,
        }
    }

    /// Whether the source answered fully.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        matches!(self.status, IntegrationStatus::Ok)
    }

    /// Whether the answer is partial.
    #[must_use]
    pub const fn is_degraded(&self) -> bool {
        matches!(self.status, IntegrationStatus::Degraded)
    }

    /// Whether the source failed completely.
    #[must_use]
    pub const fn is_error(&self) -> bool {
        matches!(self.status, IntegrationStatus::Error)
    }

    /// The data, or `default` when the source failed completely.
    pub fn data_or(self, default: T) -> T {
        self.data.unwrap_or(default)
    }
}

/// The worst status across several outcomes: error beats degraded beats ok.
#[must_use]
pub fn aggregate_status(outcomes: &[IntegrationStatus]) -> IntegrationStatus {
    let mut worst = IntegrationStatus::Ok;
    for status in outcomes {
        worst = worst.max(*status);
    }
    worst
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

    /// Record one outcome, keeping the worst status per source.
    pub fn record<T>(&mut self, outcome: &ProviderOutcome<T>) {
        self.record_status(outcome.source, outcome.status, outcome.deterministic);
    }

    /// Record one raw status for `source`.
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
/// landed. Never nest this inside [`with_current`].
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

/// Record one outcome into the current request's context. See
/// [`record_current`].
pub fn record_outcome_current<T>(outcome: &ProviderOutcome<T>) -> bool {
    record_current(outcome.source, outcome.status, outcome.deterministic)
}

/// Read the current request's context without recording. Returns `None`
/// outside a request scope. The closure must not record (no nesting).
pub fn with_current<F, R>(read: F) -> Option<R>
where
    F: FnOnce(&DegradationContext) -> R,
{
    CURRENT.try_with(|cell| read(&cell.borrow())).ok()
}

/// Record a failure for an optional source and yield `None`: the
/// record-then-`None` half of optional enrichment. The caller returns this
/// `None` up and the request still succeeds; the recording in the request
/// context is the error signal.
pub fn degraded_none<T>(
    source: &'static str,
    status: IntegrationStatus,
    deterministic: bool,
) -> Option<T> {
    record_current(source, status, deterministic);
    None
}

/// Degradation port behind the slice clients: one note per failed optional
/// call. The audio slices landed with identical local copies of this trait
/// (and MusicBrainz with an operation-keyed variant); the integrator unified
/// them here so one fake records for every client and the production
/// [`CoreSink`](super::adapters::CoreSink) implements it once against the
/// request context. `source` is the lowercase provider key the matrix joins
/// on; callers fold any operation detail into `message`.
pub trait DegradationSink: Send + Sync {
    /// Record one note that this optional source failed.
    fn record(&self, source: &'static str, message: String);
}

/// A sink that drops every record. Handy until the wiring lands, and for
/// callers that handle absence themselves.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopSink;

impl DegradationSink for NoopSink {
    fn record(&self, _source: &'static str, _message: String) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worst_status_wins_per_source() {
        let mut context = DegradationContext::new();
        assert!(!context.has_degradation());
        context.record(&ProviderOutcome::ok(vec![1], "lastfm"));
        context.record(&ProviderOutcome::<Vec<u8>>::error("lastfm", "reset"));
        context.record(&ProviderOutcome::ok(vec![2], "lastfm"));
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
        context.record(&ProviderOutcome::<()>::deterministic_error(
            "musicbrainz",
            "null where an MBID belongs",
        ));
        assert!(context.has_deterministic_failure());
        assert!(context.deterministic_sources().contains("musicbrainz"));
    }

    #[test]
    fn aggregate_prefers_the_worst() {
        use IntegrationStatus::{Degraded, Error, Ok};
        assert_eq!(aggregate_status(&[Ok, Ok]), Ok);
        assert_eq!(aggregate_status(&[Ok, Degraded]), Degraded);
        assert_eq!(aggregate_status(&[Degraded, Error, Ok]), Error);
        assert_eq!(aggregate_status(&[]), Ok);
    }

    #[tokio::test]
    async fn scoped_collects_recordings() {
        let (output, context) = scoped(async {
            record_current("audiodb", IntegrationStatus::Error, false);
            let missing: Option<String> =
                degraded_none("lastfm", IntegrationStatus::Degraded, false);
            assert_eq!(missing, None);
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
        assert_eq!(with_current(|context| context.has_degradation()), None);
    }
}
