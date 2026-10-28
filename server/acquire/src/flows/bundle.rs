//! The flows stores and the loop deps built over them.

use std::sync::Arc;

use crate::db::AcquireDb;
use crate::dispatch::DownloadDispatch;
use crate::downloads::worker::SourceList;
use crate::flows::loops::{FollowDeps, SweepDeps, SyncDeps, WantedDeps};
use crate::flows::seams::{ReleasePoll, TickSink};
use crate::flows::stores::{AdminDirectory, LibraryPresence, UpgradeWorklist};
use crate::follows::Follows;
use crate::requests::sqlite::{RequestStore, WantedStore};
use crate::requests::state::RequestsState;
use crate::target::Targets;
use droppedneedle_kernel::runtime_config::ConfigStore;

/// Flows stores plus the loop deps built over them.
#[derive(Clone)]
pub struct FlowsBundle {
    /// Wanted watches (shared with the wanted view).
    pub watches: WantedStore,
    /// Request ledger (shared with intake).
    pub ledger: RequestStore,
    /// Follow-poll cursors.
    pub follows: Follows,
    /// Upgrade worklist, read from the catalog.
    pub worklist: UpgradeWorklist,
    /// Library presence, read from the catalog.
    pub library: Arc<LibraryPresence>,
    /// Sweep ownership directory, read from the accounts.
    pub admins: AdminDirectory,
    /// Plugin ticks.
    pub ticks: Arc<dyn TickSink>,
    /// Wanted-watcher deps.
    pub wanted_deps: Arc<WantedDeps>,
    /// Follow-poll deps.
    pub follow_deps: Arc<FollowDeps>,
    /// Upgrade-sweep deps.
    pub sweep_deps: Arc<SweepDeps>,
    /// Status-sweep deps.
    pub sync_deps: Arc<SyncDeps>,
}

/// Flows bundle over the shared durable stores and live settings.
#[allow(clippy::too_many_arguments)]
pub(crate) fn flows_bundle(
    db: &AcquireDb,
    requests: &RequestsState,
    sources: SourceList,
    dispatch: Arc<DownloadDispatch>,
    settings: &ConfigStore,
    ticks: Arc<dyn TickSink>,
    release_poll: Arc<dyn ReleasePoll>,
    targets: Arc<Targets>,
) -> FlowsBundle {
    let watches = requests.wanted.clone();
    let ledger = requests.store.clone();
    let follows = requests.follows.clone();
    let worklist = UpgradeWorklist::new(db.clone());
    let library = Arc::new(LibraryPresence::over_catalog(db.db().clone()));
    let admins = AdminDirectory::over_users(db.clone());
    let wanted_deps = Arc::new(WantedDeps {
        settings: settings.live(),
        watches: watches.clone(),
        ledger: ledger.clone(),
        sources,
        downloads: dispatch.clone(),
        library: library.clone(),
        targets,
        ticks: ticks.clone(),
    });
    let follow_deps = Arc::new(FollowDeps {
        follows: follows.clone(),
        poll: release_poll,
        downloads: dispatch.clone(),
        requests: Arc::new(crate::requests::service::RequestsService::new(requests)),
        library: library.clone(),
        ticks: ticks.clone(),
        preferences: settings.live(),
        today: {
            let clock = db.clock().clone();
            Arc::new(move || {
                // Days since epoch to a UTC calendar date.
                utc_ymd(crate::db::epoch_secs(clock.as_ref()) / 86_400)
            })
        },
    });
    let sweep_deps = Arc::new(SweepDeps {
        policy: settings.live(),
        worklist: Arc::new(worklist.clone()),
        admins: admins.clone(),
        downloads: dispatch.clone(),
        ticks: ticks.clone(),
    });
    let sync_deps = Arc::new(SyncDeps {
        ledger: ledger.clone(),
        journal: dispatch.journal().clone(),
        library: library.clone(),
        ticks: ticks.clone(),
    });
    FlowsBundle {
        watches,
        ledger,
        follows,
        worklist,
        library,
        admins,
        ticks,
        wanted_deps,
        follow_deps,
        sweep_deps,
        sync_deps,
    }
}

/// Days since epoch to `YYYY-MM-DD` (Howard Hinnant's civil algorithm).
fn utc_ymd(days: u64) -> String {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    if month <= 2 {
        year += 1;
    }
    format!("{year:04}-{month:02}-{day:02}")
}
