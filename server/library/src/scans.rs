//! Scan controls: requests by kind and scope, the policy guard,
//! estimates, history pages, failure lists, and pause, resume and stop.
//! The scan and watcher loop turns are in `loops`.
//!
//! Ported from v2's scan routes and coordinator. A request names roots
//! and path rules by id and carries the policy revision the caller saw
//! in the library settings; if the settings moved since, the request is
//! refused rather than scanning scopes the caller never looked at.

use std::sync::{Arc, Mutex};

use super::manage::publish_error;
use super::scan::coordinator::ScanRequestError;
use super::scan::models::{
    EffectivePolicy, Event, RunState, ScanFailureRecord, ScanInventoryItem, ScanKind, ScanRequest,
    ScanRequestResult, ScanRun, ScanScope, ScanTrigger,
};
use super::scan::pool::BlockingPool;
use super::scan::roots::{LibraryRoot, RootRegistry};
use super::scan::selection::{UnknownScopes, select_scopes};
use super::scan::sqlite_store::SqliteScanStore;
use super::scan::store::ScanStoreError;
use super::scan::watcher::WatcherState;
use super::service::ServiceError;
use super::settings::LibrarySettings;
use super::wiring::{LibraryCore, ScanCoordinator};
use droppedneedle_kernel::runtime_config::Plain;
use droppedneedle_kernel::runtime_config::secret_sections::TypedLibrary;

/// Most finished runs one history page carries.
pub const MAX_PAGE: usize = 50;
/// Most failure rows one page carries.
pub const MAX_FAILURE_PAGE: usize = 200;
/// Inventory rows a run detail view shows: the most recent ones.
const RUN_DETAIL_FILES: usize = 500;

/// What a caller asks a run to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunControl {
    Pause,
    Resume,
    Stop,
}

/// A run after a control, plus the scan stream revision it landed at.
#[derive(Debug, Clone)]
pub struct ControlledRun {
    pub run: ScanRun,
    pub stream_revision: u64,
}

pub(crate) fn store_error(error: ScanStoreError) -> ServiceError {
    match error {
        ScanStoreError::NotFound { .. } => ServiceError::NotFound,
        ScanStoreError::StaleRevision { message } | ScanStoreError::InvalidControl { message } => {
            ServiceError::Conflict { message }
        }
        ScanStoreError::Busy { .. } | ScanStoreError::LaneClosed => ServiceError::Busy {
            cause: error.to_string(),
        },
        ScanStoreError::TxTimeout { .. }
        | ScanStoreError::ReadCancelled { .. }
        | ScanStoreError::Stopped
        | ScanStoreError::Internal { .. } => ServiceError::Internal {
            cause: error.to_string(),
        },
    }
}

fn request_error(error: ScanRequestError) -> ServiceError {
    let message = error.to_string();
    match error {
        ScanRequestError::Disabled
        | ScanRequestError::StalePolicy
        | ScanRequestError::UnknownRoots => ServiceError::Conflict { message },
        ScanRequestError::EmptyScopes | ScanRequestError::BadCursor => {
            ServiceError::InvalidInput { message }
        }
        ScanRequestError::Store(cause) => store_error(cause),
    }
}

/// Refusal for ids the engine cannot scan. Ids the saved settings hold
/// belong to a root the engine had to leave out (its path is not
/// absolute, or it overlaps another root), so the message says that
/// instead of claiming they are gone.
fn unknown_scopes(settings: &TypedLibrary, UnknownScopes(ids): UnknownScopes) -> ServiceError {
    let saved = |id: &String| {
        settings
            .library_roots
            .iter()
            .any(|root| &root.id == id || root.rules.iter().any(|rule| &rule.id == id))
    };
    let message = if !ids.is_empty() && ids.iter().all(saved) {
        format!(
            "These library scopes are saved but cannot be scanned: {}. Check their folders \
             in Settings > Library.",
            ids.join(", ")
        )
    } else {
        "One or more selected library scopes no longer exist.".to_owned()
    };
    ServiceError::InvalidInput { message }
}

/// Scans: requests by kind and scope, history, failure lists and run
/// controls, adding a root, and the turns the scan and watcher loops run.
#[derive(Clone)]
pub struct Scans {
    pub coordinator: Arc<ScanCoordinator>,
    pub scan_store: Arc<SqliteScanStore>,
    /// The roots, their dirty marks and wakeups, and the publish cell a
    /// new root opens.
    pub core: LibraryCore,
    pub(crate) settings: LibrarySettings,
    /// Blocking pool shared by scan and the watcher.
    pub(crate) pool: BlockingPool,
    /// Watcher state across ticks.
    pub(crate) watcher_state: Arc<Mutex<WatcherState>>,
}

impl Scans {
    /// The saved settings and the registry matching them. Refuses with a
    /// conflict when `expected` is given and no longer matches the
    /// settings' policy revision (the one `GET /settings/library` shows).
    fn guarded_registry(
        &self,
        expected: Option<&str>,
    ) -> Result<(std::sync::Arc<Plain<TypedLibrary>>, RootRegistry), ServiceError> {
        let settings = self
            .settings
            .library
            .get()
            .map_err(|error| ServiceError::internal(&error))?;
        let resolved = crate::policy::resolve(&settings)
            .map_err(|error| ServiceError::internal(&format!("{error:?}")))?;
        let stale = || ServiceError::Conflict {
            message: "The library policy changed. Refresh this page and try again.".to_owned(),
        };
        if expected.is_some_and(|expected| expected != resolved.policy_revision) {
            return Err(stale());
        }
        let wanted = super::settings::registry_from(&settings);
        let live = self.core.refresh_registry();
        if live.policy_revision() != wanted.policy_revision() {
            return Err(stale());
        }
        Ok((settings, live))
    }

    /// Request a scan by kind over the selected roots and rules (all
    /// roots when `scope_ids` is empty). `policy_reconcile` walks the
    /// scopes again under the current policy without rereading tags.
    /// Blocking.
    pub fn request_scan_run(
        &self,
        kind: ScanKind,
        scope_ids: &[String],
        expected_policy_revision: &str,
        user_id: &str,
    ) -> Result<ScanRequestResult, ServiceError> {
        let (settings, registry) = self.guarded_registry(Some(expected_policy_revision))?;
        let scopes = select_scopes(&registry, scope_ids)
            .map_err(|unknown| unknown_scopes(&settings, unknown))?;
        let trigger = if kind == ScanKind::PolicyReconcile {
            ScanTrigger::PolicyApply
        } else {
            ScanTrigger::Manual
        };
        self.coordinator
            .request_run(&ScanRequest {
                kind,
                trigger,
                scopes,
                requested_by_user_id: Some(user_id.to_owned()),
                policy_revision: registry.policy_revision().to_owned(),
            })
            .map_err(request_error)
    }

    /// Approximate file count for a scan over the selected scopes, and
    /// when it was taken. Blocking.
    pub fn estimate_scan(&self, scope_ids: &[String]) -> Result<(u64, f64), ServiceError> {
        let (settings, registry) = self.guarded_registry(None)?;
        let scopes = select_scopes(&registry, scope_ids)
            .map_err(|unknown| unknown_scopes(&settings, unknown))?;
        self.coordinator.estimate(&scopes).map_err(store_error)
    }

    /// The run holding the worker and the next queued one. Blocking.
    pub fn current_scan_runs(&self) -> Result<(Option<ScanRun>, Option<ScanRun>), ServiceError> {
        let runs = self.coordinator.current_runs().map_err(store_error)?;
        let active = runs
            .iter()
            .find(|run| run.state != RunState::Queued)
            .cloned();
        let queued = runs.into_iter().find(|run| run.state == RunState::Queued);
        Ok((active, queued))
    }

    /// One page of finished runs, newest first, and the cursor for the
    /// next page. Blocking.
    pub fn scan_history_page(
        &self,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<(Vec<ScanRun>, Option<String>), ServiceError> {
        self.coordinator
            .history_page(limit.clamp(1, MAX_PAGE), cursor)
            .map_err(request_error)
    }

    /// One page of a run's failed paths, oldest first. Blocking.
    pub fn scan_run_failures(
        &self,
        run_id: &str,
        limit: usize,
        cursor: Option<i64>,
    ) -> Result<(Vec<ScanFailureRecord>, Option<i64>), ServiceError> {
        self.coordinator
            .scan_run_failures(run_id, limit.clamp(1, MAX_FAILURE_PAGE), cursor)
            .map_err(store_error)
    }

    /// Pause, resume or stop a run. `expected_revision` is the run's row
    /// revision as the caller saw it; repeating a control that already
    /// took effect answers the run as it is. Blocking.
    pub fn control_scan_run(
        &self,
        run_id: &str,
        control: RunControl,
        expected_revision: u64,
    ) -> Result<ControlledRun, ServiceError> {
        let event = match control {
            RunControl::Pause => Event::Pause,
            RunControl::Resume => Event::Resume,
            RunControl::Stop => Event::Stop,
        };
        let (run, stream_revision) = self
            .coordinator
            .control(run_id, event, expected_revision)
            .map_err(store_error)?;
        Ok(ControlledRun {
            run,
            stream_revision,
        })
    }

    /// Add a library root. The path must exist, be absolute, and be a
    /// directory; the id must be unused. Adding the first root
    /// enables the library and marks the root dirty so Hook B picks
    /// it up for an initial scan. Blocking file and database work:
    /// handlers run this off the async runtime.
    pub fn add_root(
        &self,
        id: Option<String>,
        path: String,
        policy: EffectivePolicy,
    ) -> Result<(LibraryRoot, String), ServiceError> {
        let dir =
            crate::policy::clean_absolute(&path).ok_or_else(|| ServiceError::InvalidInput {
                message: "Root path must be absolute".to_owned(),
            })?;
        let meta = std::fs::symlink_metadata(&dir).map_err(|_| ServiceError::InvalidInput {
            message: "Root path does not exist".to_owned(),
        })?;
        if !meta.file_type().is_dir() {
            return Err(ServiceError::InvalidInput {
                message: "Root path is not a directory".to_owned(),
            });
        }
        let id = id.unwrap_or_else(|| self.core.ids.new_id());
        if id.is_empty() || id.contains('/') || id.contains('\0') || id.contains("::") {
            return Err(ServiceError::InvalidInput {
                message: "Root id is not a plain name".to_owned(),
            });
        }
        let root = LibraryRoot::new(&id, dir, policy);
        self.settings.add_root(&root)?;
        // The refresh swaps the new root in and marks it dirty, so the
        // supervisor gives it an initial scan.
        let registry = self.core.refresh_registry();
        let revision = registry.policy_revision().to_owned();
        // Open (or reopen) the publish cell under the new root set.
        // A reconcile failure here is a 409: the root is registered
        // but publishing stays closed until recovery passes.
        {
            let mut cell = self
                .core
                .publish
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let session = cell.session(&registry);
            cell.refresh_to(session, &self.core.root_dirs)
                .map_err(publish_error)?;
        }
        Ok((root, revision))
    }

    /// Request a manual scan over one root or every scheduled root.
    pub fn request_scan(
        &self,
        root_id: Option<&str>,
        user_id: &str,
    ) -> Result<ScanRequestResult, ServiceError> {
        let registry = self.core.live_registry();
        let mut scopes = registry.scheduled_root_scopes();
        if let Some(wanted) = root_id {
            scopes.retain(|scope| scope.root_id == wanted);
            if scopes.is_empty() {
                return Err(ServiceError::NotFound);
            }
        }
        let result = self
            .coordinator
            .request_run(&ScanRequest {
                kind: ScanKind::Incremental,
                trigger: ScanTrigger::Manual,
                scopes,
                requested_by_user_id: Some(user_id.to_owned()),
                policy_revision: registry.policy_revision().to_owned(),
            })
            .map_err(|error| match error {
                super::scan::coordinator::ScanRequestError::Disabled => ServiceError::Conflict {
                    message: error.to_string(),
                },
                super::scan::coordinator::ScanRequestError::EmptyScopes => {
                    ServiceError::InvalidInput {
                        message: error.to_string(),
                    }
                }
                super::scan::coordinator::ScanRequestError::StalePolicy
                | super::scan::coordinator::ScanRequestError::UnknownRoots => {
                    ServiceError::Conflict {
                        message: error.to_string(),
                    }
                }
                super::scan::coordinator::ScanRequestError::BadCursor => {
                    ServiceError::InvalidInput {
                        message: error.to_string(),
                    }
                }
                super::scan::coordinator::ScanRequestError::Store(cause) => {
                    ServiceError::internal(&cause)
                }
            })?;
        Ok(result)
    }

    /// Every current run and the latest finished ones. Blocking.
    pub fn recent_scan_runs(
        &self,
        finished: usize,
    ) -> Result<(Vec<ScanRun>, Vec<ScanRun>), ServiceError> {
        let current = self.coordinator.current_runs().map_err(store_error)?;
        let history = self.coordinator.history(finished).map_err(store_error)?;
        Ok((current, history))
    }

    /// Run detail plus discovered files (bounded to the latest 500).
    pub fn run_detail(
        &self,
        run_id: &str,
    ) -> Result<(ScanRun, Vec<ScanScope>, Vec<ScanInventoryItem>), ServiceError> {
        let (run, scopes, _) = self.coordinator.snapshot(run_id).map_err(store_error)?;
        let files = self
            .scan_store
            .inventory_for_run(run_id, RUN_DETAIL_FILES)
            .map_err(store_error)?;
        Ok((run, scopes, files))
    }
}
