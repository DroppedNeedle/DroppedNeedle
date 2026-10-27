//! Turning automatic management on for a library folder: first a dry run
//! (a preview of the whole folder under the proposed settings, which
//! changes nothing), then a confirmation that saves the settings with the
//! dry run's pins. Automatic runs only go ahead while those pins still
//! match, so a changed profile or folder needs a new dry run.

use droppedneedle_kernel::db::Lane;
use std::sync::Arc;

use super::models::{
    JobDetail, JobIntent, JobMode, ManagementError, Origin, PreviewHandle, Selection, SelectionKind,
};
use super::profile;
use super::service::{JobSpec, Management, stale};
use super::store;
use crate::clock::LibraryClock as _;
use crate::management::settings::service::{ActivationProofPins, LibraryManagementService};
use crate::management::settings::settings_revision;
use crate::publish::snapshots::sha256_hex;
use crate::settings_models::LibraryManagementSettingsResponse;
use droppedneedle_kernel::runtime_config::sections::LibraryManagement;
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;

/// How a dry run ends once it has proved the settings that were saved.
const ACTIVATION_CONFIRMED: &str = "ACTIVATION_CONFIRMED";

/// A dry run request for one library folder.
pub struct ActivationRequest {
    pub root_id: String,
    pub settings: LibraryManagement,
    pub expected_settings_revision: String,
    pub expected_policy_revision: String,
    pub idempotency_key: Option<String>,
}

/// One dry run offered as proof.
pub struct ActivationProof {
    pub root_id: String,
    pub job_id: String,
    pub preview_token: String,
}

/// Save settings with the dry runs that confirm them.
pub struct ActivationConfirm {
    pub settings: LibraryManagement,
    pub proofs: Vec<ActivationProof>,
    pub expected_settings_revision: String,
    pub confirmation: bool,
}

/// A settings refusal as a management one.
fn settings_error(error: SettingsError) -> ManagementError {
    match error {
        SettingsError::InvalidInput { message } => ManagementError::Invalid(message),
        SettingsError::StaleRevision { message } => ManagementError::Conflict(message),
        other => ManagementError::Store(format!("{other:?}")),
    }
}

impl Management {
    /// The library policy revision activation pins compare against (the
    /// settings' own revision of the roots and rules, as in v2).
    pub(super) fn activation_policy_revision(&self) -> Result<String, ManagementError> {
        self.setup
            .core
            .policy_revision()
            .map_err(|error| ManagementError::Store(error.to_string()))
    }

    /// The settings page's service.
    fn settings_service(&self) -> Arc<LibraryManagementService> {
        self.setup.management_settings.clone()
    }

    /// Start a dry run of one folder under proposed settings. Blocking.
    pub fn create_activation(
        &self,
        request: ActivationRequest,
        actor: &str,
    ) -> Result<PreviewHandle, ManagementError> {
        let proposed = self
            .settings_service()
            .prepare_activation(
                request.settings,
                &request.root_id,
                &request.expected_settings_revision,
            )
            .map_err(settings_error)?;
        let registry = self.registry_at(
            &request.expected_policy_revision,
            "The library folders or their rules changed since this page loaded. Reload the page and try again.",
        )?;
        let assignment = profile::root_assignment(&proposed, &request.root_id);
        let pinned = profile::pin_assignment(&proposed, &assignment)?;
        let mut revisions = profile::revisions(&proposed, &pinned, registry.policy_revision());
        // Pinned to the settings saved now: the dry run goes stale when
        // anyone saves before it is confirmed.
        revisions.settings = request.expected_settings_revision.clone();
        let proposed_revision = settings_revision(&proposed);
        let selection = Selection {
            kind: SelectionKind::Roots,
            ids: vec![request.root_id.clone()],
            catalog_filter: None,
        };
        self.start_job(JobSpec {
            mode: JobMode::Preview,
            origin: Origin::Manual,
            actor: Some(actor),
            idempotency_key: request.idempotency_key.as_deref(),
            revisions,
            selection_json: serde_json::to_string(&selection)?,
            pinned: &pinned,
            target_root_id: None,
            linked_job_id: None,
            intent: JobIntent {
                activation_root_id: Some(request.root_id),
                ..JobIntent::default()
            },
            proposed_settings_revision: Some(&proposed_revision),
            retention_hours: proposed.preview_retention_hours,
        })
    }

    /// One dry run; refused for a job that is not one. Blocking.
    pub fn activation_detail(&self, job_id: &str) -> Result<JobDetail, ManagementError> {
        let detail = self.detail(job_id)?;
        if detail.proposed_settings_revision.is_none() {
            return Err(ManagementError::Invalid(
                "This job is not a dry run for turning on automatic management.".to_owned(),
            ));
        }
        Ok(detail)
    }

    /// Save the settings once every folder they turn on has a current,
    /// ready dry run of exactly these settings. Blocking.
    pub fn confirm_activation(
        &self,
        request: ActivationConfirm,
        actor: &str,
    ) -> Result<LibraryManagementSettingsResponse, ManagementError> {
        if !request.confirmation {
            return Err(ManagementError::Invalid(
                "Confirm that automatic management may change these files before saving."
                    .to_owned(),
            ));
        }
        if request.proofs.is_empty() {
            return Err(ManagementError::Invalid(
                "Run a dry run for the folder first.".to_owned(),
            ));
        }
        let mut roots: Vec<&str> = request
            .proofs
            .iter()
            .map(|proof| proof.root_id.as_str())
            .collect();
        roots.sort_unstable();
        roots.dedup();
        if roots.len() != request.proofs.len() {
            return Err(ManagementError::Invalid(
                "Each library folder can be confirmed once.".to_owned(),
            ));
        }
        let service = self.settings_service();
        let current = settings_revision(&self.settings()?);
        let policy = self.activation_policy_revision()?;
        let mut pins = Vec::new();
        for proof in &request.proofs {
            let proposed = service
                .prepare_activation(
                    request.settings.clone(),
                    &proof.root_id,
                    &request.expected_settings_revision,
                )
                .map_err(settings_error)?;
            let mut pin = self.check_proof(proof, &proposed, &current)?;
            // The dry run is current (its folders did not move), so the
            // pins name the folders' revision as the settings know it.
            pin.policy_revision = policy.clone();
            pin.confirmed_by = Some(actor.to_owned());
            pins.push(pin);
        }
        let saved = service
            .confirm_activation(request.settings, &request.expected_settings_revision, &pins)
            .map_err(settings_error)?;
        // A dry run is never applied: once it has proved the settings it
        // is done, and no longer holds its albums as planned work.
        let now = self.setup.clock.now_secs();
        if let Err(error) = self.setup.db.write_blocking(
            Lane::Foreground,
            "management.complete_without_work",
            |tx| {
                for proof in &request.proofs {
                    store::complete_without_work(tx, &proof.job_id, ACTIVATION_CONFIRMED, now)?;
                }
                Ok::<_, ManagementError>(())
            },
        ) {
            tracing::warn!(%error, "confirmed dry runs left ready until they expire");
        }
        // Albums skipped while the folder was off or out of date get
        // another look.
        if let Err(error) =
            self.setup
                .db
                .write_blocking(Lane::Foreground, "management.wake", |conn| {
                    Ok::<_, ManagementError>(super::automatic::wake(
                        conn,
                        None,
                        self.setup.clock.now_secs(),
                    )?)
                })
        {
            tracing::warn!(%error, "skipped albums not woken after activation");
        }
        Ok(saved)
    }

    /// The pins one proof gives, when it is a ready dry run of exactly
    /// this folder under exactly these settings.
    fn check_proof(
        &self,
        proof: &ActivationProof,
        proposed: &LibraryManagement,
        current_revision: &str,
    ) -> Result<ActivationProofPins, ManagementError> {
        let snapshot = self.snapshot(&proof.job_id)?;
        let presented = sha256_hex(proof.preview_token.as_bytes());
        if snapshot.token_hash.as_deref() != Some(presented.as_str()) {
            return Err(ManagementError::Invalid(
                "The dry run's confirmation token is not valid. Open the dry run again.".to_owned(),
            ));
        }
        if !self.detail(&proof.job_id)?.ready_for_confirmation {
            return Err(stale(
                "The dry run is not ready, expired, or out of date. Run a new dry run.",
            ));
        }
        let selection: Selection = serde_json::from_str(&snapshot.selection_json)?;
        let intent = snapshot.intent();
        if selection.kind != SelectionKind::Roots
            || selection.ids != [proof.root_id.clone()]
            || snapshot.target_root_id.is_some()
            || intent.activation_root_id.as_deref() != Some(proof.root_id.as_str())
        {
            return Err(ManagementError::Invalid(
                "The dry run does not cover exactly this library folder.".to_owned(),
            ));
        }
        let pinned = profile::pin_assignment(
            proposed,
            &profile::root_assignment(proposed, &proof.root_id),
        )?;
        let expected = profile::revisions(proposed, &pinned, &snapshot.policy_revision);
        if snapshot.profile_revision != expected.profile
            || snapshot.naming_revision != expected.naming
            || snapshot.proposed_settings_revision.as_deref()
                != Some(settings_revision(proposed).as_str())
            || snapshot.settings_revision != current_revision
        {
            return Err(stale(
                "The settings changed after the dry run. Run a new dry run with these settings.",
            ));
        }
        Ok(ActivationProofPins {
            root_id: proof.root_id.clone(),
            profile_revision: snapshot.profile_revision,
            naming_revision: snapshot.naming_revision,
            policy_revision: snapshot.policy_revision,
            preview_token: proof.preview_token.clone(),
            preview_hash: presented,
            confirmed_at: self.setup.clock.now_secs(),
            confirmed_by: None,
        })
    }
}
