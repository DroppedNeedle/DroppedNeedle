//! Requests state: the durable stores behind one handle.
//!
//! Services take this handle and clone what they need.

use std::sync::Arc;

use super::quota::QuotaLedger;
use super::sqlite::{EditionStore, PersonalMixStore, RequestStore, WantedStore};
use crate::db::AcquireDb;
use crate::dispatch::DownloadDispatch;
use crate::follows::Follows;

/// Everything the requests router needs, built once.
#[derive(Clone)]
pub struct RequestsState {
    /// Request ledger.
    pub store: RequestStore,
    /// Quota ledger.
    pub quota: Arc<QuotaLedger>,
    /// Download dispatch.
    pub dispatch: Arc<DownloadDispatch>,
    /// Wanted watches (shared with the watcher loop).
    pub wanted: WantedStore,
    /// Follows and their auto-download approvals.
    pub follows: Follows,
    /// Personal-mix approvals and refresh guard.
    pub mixes: Arc<PersonalMixStore>,
    /// In-flight edition acquires.
    pub editions: EditionStore,
    /// The application database, for the library's chosen edition and
    /// album names.
    pub library: AcquireDb,
    /// The plugin host: new requests are announced to `subscriber`
    /// plugins. `None` in tests that run the routes alone.
    pub plugins: Option<Arc<droppedneedle_plugins::host::PluginHost>>,
    /// The personal-mix builder. `None` when ListenBrainz has no rate
    /// limit row, and in tests that run the routes alone.
    pub mixer: Option<Arc<super::mix::PersonalMixBuilder>>,
    /// The library's edition plans: "get missing tracks" fetches only what
    /// the chosen edition lacks. `None` in tests that run the routes alone.
    pub edition_plans: Option<Arc<dyn droppedneedle_library::operations::port::EditionPlans>>,
    /// The download policy the upgrade cutoff comes from: "get missing
    /// tracks" also asks for the edition's owned tracks below the cutoff.
    pub upgrade_policy: Option<
        droppedneedle_kernel::runtime_config::Live<
            droppedneedle_kernel::runtime_config::sections::DownloadPolicy,
        >,
    >,
    /// Whether a download source is set up and where downloads land.
    pub readiness: Option<AcquireReadiness>,
}

/// What "get missing tracks" checks before it asks for anything: a
/// source is set up and the staging disk has room.
#[derive(Clone)]
pub struct AcquireReadiness {
    /// Whether a download source (Soulseek, Usenet or a plugin) is on.
    /// Queued downloads wait while none is.
    pub sources: crate::readiness::SourceReadiness,
    /// Where downloads land before import.
    pub staging_root: std::path::PathBuf,
}

impl RequestsState {
    /// State over one database, one quota ledger and one dispatch, with no
    /// ports: the routes alone. Production fills the ports in
    /// [`crate::wiring`] before anything holds the state.
    pub fn new(db: &AcquireDb, quota: Arc<QuotaLedger>, dispatch: Arc<DownloadDispatch>) -> Self {
        Self {
            store: RequestStore::new(db.clone()),
            quota,
            dispatch,
            wanted: WantedStore::new(db.clone()),
            follows: Follows::new(db.clone()),
            mixes: Arc::new(PersonalMixStore::new(db.clone())),
            editions: EditionStore::new(db.clone()),
            library: db.clone(),
            plugins: None,
            mixer: None,
            edition_plans: None,
            upgrade_policy: None,
            readiness: None,
        }
    }

    /// Test state over a scratch database and the download journal on it,
    /// with default settings and staging beside the database. Returns the
    /// state, the database (to seed users and rows) and the dispatch (to
    /// read and settle its tasks). Call inside a tokio runtime.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests() -> Result<(Self, AcquireDb, Arc<DownloadDispatch>), String> {
        use crate::downloads::journal::Journal;

        let db = AcquireDb::scratch()?;
        let dir = db
            .path()
            .parent()
            .ok_or_else(|| "scratch database has no directory".to_owned())?
            .to_path_buf();
        let settings = crate::settings::scratch_settings(&dir.join("settings"))?;
        let dispatch = Arc::new(DownloadDispatch::new(
            Arc::new(Journal::new(
                db.clone(),
                Arc::new(crate::plugin_events::FlowTicks::new(
                    None,
                    droppedneedle_kernel::events::EventHub::new(),
                )),
            )),
            Arc::new(droppedneedle_kernel::ids::UuidGenerator),
            settings.live(),
        ));
        let quota = Arc::new(QuotaLedger::unlimited(db.clone()));
        Ok((Self::new(&db, quota, dispatch.clone()), db, dispatch))
    }
}
