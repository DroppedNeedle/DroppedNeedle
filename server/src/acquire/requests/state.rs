//! Requests state: in-memory stores behind one handle.
//!
//! Services take this handle and clone the arcs they need, so the memory
//! stores can become SQLite-backed ports without touching handler
//! signatures. Production passes the downloads module's durable dispatch.

use std::sync::Arc;

use super::bridges::{FollowDecisionSink, WatchView};
use super::dispatch::DownloadDispatch;
use super::ledger::{
    EditionStore, FollowApprovalStore, PersonalMixStore, RequestStore, WantedStore,
};
use super::quota::QuotaLedger;

/// Everything the requests router needs, built once.
#[derive(Clone)]
pub struct RequestsState {
    /// Request ledger.
    pub store: Arc<RequestStore>,
    /// Quota ledger.
    pub quota: Arc<QuotaLedger>,
    /// Download-dispatch seam (the downloads module in production).
    pub dispatch: Arc<dyn DownloadDispatch>,
    /// Wanted rows.
    pub wanted: Arc<WantedStore>,
    /// Auto-download approvals.
    pub follows: Arc<FollowApprovalStore>,
    /// Personal-mix approvals and refresh guard.
    pub mixes: Arc<PersonalMixStore>,
    /// In-flight edition acquires.
    pub editions: Arc<EditionStore>,
    /// Verdict sink into the collections follow rows. `None` in tests that
    /// run the routes alone; wiring connects the collections store.
    pub follow_sink: Option<Arc<dyn FollowDecisionSink>>,
    /// Watches owned by the flows watcher loop. `None` in tests that run
    /// the routes alone; wiring connects the loop registry for the wanted view.
    pub watch_view: Option<Arc<dyn WatchView>>,
}

impl RequestsState {
    /// State over one dispatch implementation and fresh memory stores.
    pub fn new(dispatch: Arc<dyn DownloadDispatch>) -> Self {
        Self {
            store: Arc::new(RequestStore::new()),
            quota: Arc::new(QuotaLedger::unlimited()),
            dispatch,
            wanted: Arc::new(WantedStore::new()),
            follows: Arc::new(FollowApprovalStore::new()),
            mixes: Arc::new(PersonalMixStore::new()),
            editions: Arc::new(EditionStore::new()),
            follow_sink: None,
            watch_view: None,
        }
    }

    /// Test state over the scripted fake. Returns the state plus the fake so
    /// tests can script outcomes and flip task states.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests() -> (Self, Arc<super::dispatch::ScriptedDispatch>) {
        use super::dispatch::ScriptedDispatch;

        let dispatch = ScriptedDispatch::new();
        (Self::new(dispatch.clone()), dispatch)
    }
}
