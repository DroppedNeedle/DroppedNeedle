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
    /// plugins.
    pub plugins: Arc<droppedneedle_plugins::host::PluginHost>,
    /// The library's edition plans: "get missing tracks" fetches only what
    /// the chosen edition lacks.
    pub edition_plans: Arc<dyn droppedneedle_library::edition::port::EditionPlans>,
    /// The download policy the upgrade cutoff comes from: "get missing
    /// tracks" also asks for the edition's owned tracks below the cutoff.
    pub upgrade_policy: droppedneedle_kernel::runtime_config::Live<
        droppedneedle_kernel::runtime_config::sections::DownloadPolicy,
    >,
    /// Whether a download source is set up and where downloads land.
    pub readiness: AcquireReadiness,
    /// The library's album removal: an admin removing a discovery batch
    /// recycles the albums it brought in.
    pub albums: droppedneedle_library::removal::Mutations,
    /// Users' notices: a follower hears when someone else's ask starts
    /// their followed artist's release.
    pub events: droppedneedle_kernel::events::EventHub,
}

/// What the Weekly Mix routes read: the requests state and the mix
/// builder, which holds a requests state of its own to file its asks.
#[derive(Clone)]
pub struct MixRoutes {
    /// The requests state.
    pub requests: RequestsState,
    /// The personal-mix builder.
    pub mixer: Arc<super::mix::PersonalMixBuilder>,
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

/// What the requests state reaches outside its own stores.
pub struct RequestsPorts {
    /// See [`RequestsState::plugins`].
    pub plugins: Arc<droppedneedle_plugins::host::PluginHost>,
    /// See [`RequestsState::edition_plans`].
    pub edition_plans: Arc<dyn droppedneedle_library::edition::port::EditionPlans>,
    /// See [`RequestsState::upgrade_policy`].
    pub upgrade_policy: droppedneedle_kernel::runtime_config::Live<
        droppedneedle_kernel::runtime_config::sections::DownloadPolicy,
    >,
    /// See [`RequestsState::readiness`].
    pub readiness: AcquireReadiness,
    /// See [`RequestsState::albums`].
    pub albums: droppedneedle_library::removal::Mutations,
    /// See [`RequestsState::events`].
    pub events: droppedneedle_kernel::events::EventHub,
}

impl RequestsState {
    /// State over one database, one quota ledger, one dispatch and the
    /// ports it reaches.
    pub fn new(
        db: &AcquireDb,
        quota: Arc<QuotaLedger>,
        dispatch: Arc<DownloadDispatch>,
        ports: RequestsPorts,
    ) -> Self {
        Self {
            store: RequestStore::new(db.clone()),
            quota,
            dispatch,
            wanted: WantedStore::new(db.clone()),
            follows: Follows::new(db.clone()),
            mixes: Arc::new(PersonalMixStore::new(db.clone())),
            editions: EditionStore::new(db.clone()),
            library: db.clone(),
            plugins: ports.plugins,
            edition_plans: ports.edition_plans,
            upgrade_policy: ports.upgrade_policy,
            readiness: ports.readiness,
            albums: ports.albums,
            events: ports.events,
        }
    }
}
