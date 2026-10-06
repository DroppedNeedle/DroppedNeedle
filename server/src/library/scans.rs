//! Scan controls: requests by kind and scope, the policy guard,
//! estimates, history pages, failure lists, and pause, resume and stop.
//!
//! Ported from v2's scan routes and coordinator. A request names roots
//! and path rules by id and carries the policy revision the caller saw
//! in the library settings; if the settings moved since, the request is
//! refused rather than scanning scopes the caller never looked at.

use super::scan::coordinator::ScanRequestError;
use super::scan::models::{
    ScanControl, ScanFailureRecord, ScanKind, ScanRequest, ScanRequestResult, ScanRun, ScanState,
    ScanTrigger,
};
use super::scan::roots::RootRegistry;
use super::scan::selection::{UnknownScopes, select_scopes};
use super::scan::store::ScanStoreError;
use super::service::ServiceError;
use super::wiring::LibrarySetup;
use crate::runtime_config::secret_sections::TypedLibrary;

/// Most finished runs one history page carries.
pub const MAX_PAGE: usize = 50;
/// Most failure rows one page carries.
pub const MAX_FAILURE_PAGE: usize = 200;

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

fn store_error(error: ScanStoreError) -> ServiceError {
    match error {
        ScanStoreError::NotFound { .. } => ServiceError::NotFound,
        ScanStoreError::StaleRevision { message } | ScanStoreError::InvalidControl { message } => {
            ServiceError::Conflict { message }
        }
        ScanStoreError::Internal { message } => ServiceError::Internal { cause: message },
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

impl LibrarySetup {
    /// The saved settings and the registry matching them. Refuses with a
    /// conflict when `expected` is given and no longer matches the
    /// settings' policy revision (the one `GET /settings/library` shows).
    fn guarded_registry(
        &self,
        expected: Option<&str>,
    ) -> Result<(TypedLibrary, RootRegistry), ServiceError> {
        let settings = self
            .config
            .get_masked::<TypedLibrary>()
            .map_err(|error| ServiceError::internal(&error))?
            .into_inner();
        let resolved = crate::settings::library_policy::resolve(&settings)
            .map_err(|error| ServiceError::internal(&format!("{error:?}")))?;
        let stale = || ServiceError::Conflict {
            message: "The library policy changed. Refresh this page and try again.".to_owned(),
        };
        if expected.is_some_and(|expected| expected != resolved.policy_revision) {
            return Err(stale());
        }
        let wanted = super::settings::registry_from(&settings);
        let live = self.refresh_registry();
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
    pub fn current_scan_runs(&self) -> (Option<ScanRun>, Option<ScanRun>) {
        let runs = self.coordinator.current();
        let active = runs
            .iter()
            .find(|run| run.state != ScanState::Queued)
            .cloned();
        let queued = runs.into_iter().find(|run| run.state == ScanState::Queued);
        (active, queued)
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
        let (kind, resume) = match control {
            RunControl::Pause => (ScanControl::Pause, false),
            RunControl::Resume => (ScanControl::Pause, true),
            RunControl::Stop => (ScanControl::Stop, false),
        };
        let (run, stream_revision) = self
            .coordinator
            .control(run_id, kind, resume, expected_revision)
            .map_err(store_error)?;
        Ok(ControlledRun {
            run,
            stream_revision,
        })
    }
}
