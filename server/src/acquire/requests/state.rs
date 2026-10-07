//! Requests state: the durable stores behind one handle.
//!
//! Services take this handle and clone what they need. Production passes
//! the downloads module's durable dispatch.

use std::sync::Arc;

use super::bridges::FollowDecisionSink;
use super::dispatch::DownloadDispatch;
use super::quota::QuotaLedger;
use super::sqlite::{
    EditionStore, FollowApprovalStore, PersonalMixStore, RequestStore, WantedStore,
};
use crate::acquire::db::AcquireDb;

/// Everything the requests router needs, built once.
#[derive(Clone)]
pub struct RequestsState {
    /// Request ledger.
    pub store: RequestStore,
    /// Quota ledger.
    pub quota: Arc<QuotaLedger>,
    /// Download-dispatch seam (the downloads module in production).
    pub dispatch: Arc<dyn DownloadDispatch>,
    /// Wanted watches (shared with the watcher loop).
    pub wanted: WantedStore,
    /// Auto-download approvals.
    pub follows: FollowApprovalStore,
    /// Personal-mix approvals and refresh guard.
    pub mixes: Arc<PersonalMixStore>,
    /// In-flight edition acquires.
    pub editions: EditionStore,
    /// The application database, for the library's chosen edition and
    /// album names.
    pub library: AcquireDb,
    /// Verdict sink into the collections follow rows. `None` in tests that
    /// run the routes alone; wiring connects the collections store.
    pub follow_sink: Option<Arc<dyn FollowDecisionSink>>,
    /// The plugin host, once boot attached it: new requests are announced
    /// to `subscriber` plugins.
    pub plugins: crate::acquire::wiring::PluginSlot,
    /// The personal-mix builder, once boot has built it. It needs this
    /// state for album intake, so it is set after construction.
    pub mixer: MixSlot,
}

/// The slot the personal-mix builder goes into.
pub type MixSlot = Arc<std::sync::OnceLock<Arc<super::mix::PersonalMixBuilder>>>;

impl RequestsState {
    /// State over one database, one quota ledger and one dispatch.
    pub fn new(
        db: &AcquireDb,
        quota: Arc<QuotaLedger>,
        dispatch: Arc<dyn DownloadDispatch>,
    ) -> Self {
        Self {
            store: RequestStore::new(db.clone()),
            quota,
            dispatch,
            wanted: WantedStore::new(db.clone()),
            follows: FollowApprovalStore::new(db.clone()),
            mixes: Arc::new(PersonalMixStore::new(db.clone())),
            editions: EditionStore::new(db.clone()),
            library: db.clone(),
            follow_sink: None,
            plugins: Default::default(),
            mixer: Default::default(),
        }
    }

    /// Test state over a scratch database and the scripted dispatch.
    /// Returns the state, the database (to seed users and rows) and the
    /// fake (to script outcomes and flip task states). Call inside a tokio
    /// runtime.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests() -> Result<(Self, AcquireDb, Arc<super::dispatch::ScriptedDispatch>), String>
    {
        use super::dispatch::ScriptedDispatch;

        let db = AcquireDb::scratch()?;
        let dispatch = ScriptedDispatch::new();
        let quota = Arc::new(QuotaLedger::unlimited(db.clone()));
        Ok((Self::new(&db, quota, dispatch.clone()), db, dispatch))
    }
}
