//! MusicBrainz source lifecycle: direct updates plus the consent-bound
//! BrainzMash ceremony (stage/consent/verify/activate).
//!
//! v2 persisted the pending BrainzMash proposal in config; v3 keeps it
//! transient (process memory, one global proposal) while the section
//! carries only the settled state. The gates are unchanged: consent and
//! verification bind to the exact proposal (access revision, source id,
//! generation, disclosure version), a stale or outdated proposal is a
//! 409, and promotion pins the verified proposal as the active binding.
//! Source identity rotates (new uuid, generation+1) exactly when the
//! effective source changes, so caches fenced on identity drop stale
//! rows without a sweep.

use std::sync::Mutex;

use super::effects::Effects;
use super::models::{
    BrainzmashPendingProposal, MusicBrainzBindingRequest, MusicBrainzSettingsUpdate,
    MusicBrainzSettingsView, MusicBrainzVerifyRequest,
};
use super::verify::{ProbeVerdict, VerifyProbes, require_service_url};
use crate::bootstrap::Outbound;
use droppedneedle_kernel::db::DurableLane;
use droppedneedle_kernel::runtime_config::sections::is_mb_rate_policy_public_host;
use droppedneedle_kernel::runtime_config::sections::{
    BRAINZMASH_ENDPOINT, BRAINZMASH_RATE_LIMIT, MbSourceMode, MusicBrainzSettings,
    OFFICIAL_MB_API_BASE, OFFICIAL_MB_RATE_LIMIT, exact_nonblank,
};
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;
use droppedneedle_kernel::runtime_config::{ConfigStore, Section as _, UpdateError};
use droppedneedle_providers::musicbrainz::MbSource;

/// BrainzMash disclosure version. A proposal carrying any other version
/// is outdated and cannot proceed.
pub const BRAINZMASH_DISCLOSURE_VERSION: &str = "brainzmash-v1";

/// One transient pending proposal.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingProposal {
    /// Pinned endpoint.
    pub endpoint: String,
    /// Proposed access revision.
    pub access_revision: String,
    /// Proposed source identity.
    pub source_id: String,
    /// Proposed source generation.
    pub generation: i64,
    /// Proposed disclosure version.
    pub disclosure_version: String,
    /// Consent recorded.
    pub consented: bool,
    /// Endpoint verified.
    pub verified: bool,
}

impl PendingProposal {
    /// Fresh proposal for one generation.
    pub fn fresh(generation: i64) -> Self {
        Self {
            endpoint: BRAINZMASH_ENDPOINT.to_owned(),
            access_revision: uuid::Uuid::new_v4().to_string(),
            source_id: uuid::Uuid::new_v4().to_string(),
            generation: generation.max(1),
            disclosure_version: BRAINZMASH_DISCLOSURE_VERSION.to_owned(),
            consented: false,
            verified: false,
        }
    }

    /// Whether the proposal is policy-current: exact pinned endpoint,
    /// exact disclosure version, exact nonblank ids, positive generation.
    pub fn is_policy_current(&self) -> bool {
        exact_nonblank(&self.access_revision)
            && exact_nonblank(&self.source_id)
            && self.generation > 0
            && self.endpoint == BRAINZMASH_ENDPOINT
            && self.disclosure_version == BRAINZMASH_DISCLOSURE_VERSION
    }

    /// Whether a binding request names exactly this proposal.
    pub fn matches(&self, binding: &MusicBrainzBindingRequest) -> bool {
        self.is_policy_current()
            && self.access_revision == binding.access_revision
            && self.source_id == binding.source_id
            && self.generation == binding.generation
            && self.disclosure_version == binding.disclosure_version
    }
}

/// The lifecycle service: settled state in the settings, the pending
/// proposal in memory. The op lock serializes whole lifecycle operations
/// like v2's coordinator lock (always taken before the pending mutex; never
/// re-entered). It is an async lock because a save waits for its turn on
/// the writer lane while holding it; the pending mutex is only ever held
/// for a moment, never across a wait.
pub struct MusicBrainzLifecycle {
    /// Config store.
    pub store: std::sync::Arc<ConfigStore>,
    /// Whole-operation serialization.
    pub op: tokio::sync::Mutex<()>,
    /// Transient pending proposal.
    pub pending: Mutex<Option<PendingProposal>>,
    /// Tells when a save's effects (the MusicBrainz cache root) have run.
    pub effects: Effects,
    /// Where the two built-in sources are probed.
    outbound: Outbound,
}

impl MusicBrainzLifecycle {
    /// Build over the shared store and save fan-out. `outbound`
    /// says where the two built-in sources are probed.
    pub fn new(store: std::sync::Arc<ConfigStore>, effects: Effects, outbound: Outbound) -> Self {
        Self {
            store,
            op: tokio::sync::Mutex::new(()),
            pending: Mutex::new(None),
            effects,
            outbound,
        }
    }

    fn config(&self, error: droppedneedle_kernel::runtime_config::ConfigError) -> SettingsError {
        SettingsError::from_config(error)
    }

    async fn begin_op(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.op.lock().await
    }

    /// One settings write of the MusicBrainz section: `next` gets the
    /// stored section and returns the one to save, in the same transaction.
    async fn update_settings<F>(&self, next: F) -> Result<MusicBrainzSettings, SettingsError>
    where
        F: FnOnce(MusicBrainzSettings) -> Result<MusicBrainzSettings, SettingsError>
            + Send
            + 'static,
    {
        self.store
            .update::<MusicBrainzSettings, SettingsError, _>(DurableLane::Foreground, next)
            .await
            .map_err(|error| match error {
                UpdateError::Rejected(error) => error,
                UpdateError::Config(error) => self.config(error),
            })
    }

    /// Build the GET view: settled state plus the transient pending echo.
    fn view(&self, settings: MusicBrainzSettings) -> MusicBrainzSettingsView {
        MusicBrainzSettingsView {
            settings,
            pending_brainzmash: self.pending().map(|proposal| BrainzmashPendingProposal {
                endpoint: proposal.endpoint,
                access_revision: proposal.access_revision,
                source_id: proposal.source_id,
                generation: proposal.generation,
                disclosure_version: proposal.disclosure_version,
                consented: proposal.consented,
                verified: proposal.verified,
            }),
        }
    }

    fn pending_mut(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Option<PendingProposal>>, SettingsError> {
        self.pending.lock().map_err(|cause| {
            SettingsError::internal(&format!("musicbrainz proposal lock poisoned: {cause}"))
        })
    }

    /// Read the settled connection settings.
    pub fn get(&self) -> Result<MusicBrainzSettingsView, SettingsError> {
        let stored: MusicBrainzSettings = self.store.get().map_err(|e| self.config(e))?;
        Ok(self.view(stored))
    }

    /// Persist one normalized source change. BrainzMash is never a
    /// direct update: the dedicated ceremony owns it. Community updates
    /// require the disclosure acknowledgement; official updates force
    /// the public ceilings; identity rotates exactly on source change.
    /// The save invalidates the MusicBrainz cache root.
    pub async fn save_update(
        &self,
        update: &MusicBrainzSettingsUpdate,
    ) -> Result<MusicBrainzSettingsView, SettingsError> {
        let _op = self.begin_op().await;
        let mode = update.source_mode;
        if mode == MbSourceMode::Brainzmash {
            return Err(SettingsError::InvalidInput {
                message:
                    "BrainzMash moves through the consent-bound stage/consent/verify/activate flow"
                        .to_owned(),
            });
        }
        let mark = self.effects.mark(MusicBrainzSettings::KEY);
        if mode == MbSourceMode::Official {
            let view = self.save_official().await?;
            drop(_op);
            self.effects.saved(mark).await;
            return Ok(view);
        }
        let api_url = update.api_url.clone().unwrap_or_default();
        let api_url = api_url.trim().trim_end_matches('/').to_owned();
        if api_url.is_empty()
            || !(api_url.starts_with("http://") || api_url.starts_with("https://"))
        {
            return Err(SettingsError::InvalidInput {
                message: "MusicBrainz settings are incomplete or invalid".to_owned(),
            });
        }
        if mode == MbSourceMode::Community && !update.community_acknowledged.unwrap_or(false) {
            return Err(SettingsError::InvalidInput {
                message: "Community source acknowledgement is required".to_owned(),
            });
        }
        let rate_limit = update.rate_limit;
        let community_acknowledged = update.community_acknowledged.unwrap_or(false);
        *self.pending_mut()? = None;
        let saved = self
            .update_settings(move |previous| {
                let source_changed = previous.source_mode != mode
                    || previous.api_url.trim_end_matches('/') != api_url;
                let (source_id, generation) =
                    if source_changed || previous.source_id.trim().is_empty() {
                        (
                            uuid::Uuid::new_v4().to_string(),
                            previous.generation.max(1) + 1,
                        )
                    } else {
                        (previous.source_id.clone(), previous.generation.max(1))
                    };
                Ok(MusicBrainzSettings {
                    source_mode: mode,
                    api_url,
                    rate_limit,
                    community_acknowledged,
                    selected_source_mode: mode,
                    source_id,
                    generation,
                    active_brainzmash: None,
                    clamped_to_official_limits: false,
                })
            })
            .await?;
        let view = self.view(saved);
        drop(_op);
        self.effects.saved(mark).await;
        Ok(view)
    }

    async fn save_official(&self) -> Result<MusicBrainzSettingsView, SettingsError> {
        *self.pending_mut()? = None;
        let saved = self
            .update_settings(|previous| {
                let source_changed = previous.source_mode != MbSourceMode::Official
                    || previous.api_url.trim_end_matches('/') != OFFICIAL_MB_API_BASE;
                let (source_id, generation) =
                    if source_changed || previous.source_id.trim().is_empty() {
                        (
                            uuid::Uuid::new_v4().to_string(),
                            previous.generation.max(1) + 1,
                        )
                    } else {
                        (previous.source_id.clone(), previous.generation.max(1))
                    };
                Ok(MusicBrainzSettings {
                    source_mode: MbSourceMode::Official,
                    api_url: OFFICIAL_MB_API_BASE.to_owned(),
                    rate_limit: OFFICIAL_MB_RATE_LIMIT,
                    community_acknowledged: false,
                    selected_source_mode: MbSourceMode::Official,
                    source_id,
                    generation,
                    active_brainzmash: None,
                    clamped_to_official_limits: false,
                })
            })
            .await?;
        Ok(self.view(saved))
    }

    /// Stage a BrainzMash proposal without probing upstream: select the
    /// tier, rotate identity on source change, mint a fresh proposal.
    /// The save invalidates the MusicBrainz cache root.
    pub async fn stage(&self) -> Result<(MusicBrainzSettingsView, PendingProposal), SettingsError> {
        let _op = self.begin_op().await;
        let mark = self.effects.mark(MusicBrainzSettings::KEY);
        let saved = self
            .update_settings(|previous| {
                let source_changed = previous.source_mode != MbSourceMode::Brainzmash;
                let (source_id, generation) =
                    if source_changed || previous.source_id.trim().is_empty() {
                        (
                            uuid::Uuid::new_v4().to_string(),
                            previous.generation.max(1) + 1,
                        )
                    } else {
                        (previous.source_id.clone(), previous.generation.max(1))
                    };
                Ok(MusicBrainzSettings {
                    source_mode: MbSourceMode::Brainzmash,
                    api_url: BRAINZMASH_ENDPOINT.to_owned(),
                    rate_limit: BRAINZMASH_RATE_LIMIT,
                    community_acknowledged: previous.community_acknowledged,
                    selected_source_mode: MbSourceMode::Brainzmash,
                    source_id,
                    generation,
                    active_brainzmash: if source_changed {
                        None
                    } else {
                        previous.active_brainzmash
                    },
                    clamped_to_official_limits: false,
                })
            })
            .await?;
        // Built from the generation the save stored, after the commit.
        let proposal = PendingProposal::fresh(saved.generation + 1);
        *self.pending_mut()? = Some(proposal.clone());
        let view = self.view(saved);
        drop(_op);
        self.effects.saved(mark).await;
        Ok((view, proposal))
    }

    /// Record consent for the exact staged proposal. A stale proposal or
    /// an outdated disclosure is a 409.
    pub async fn consent(
        &self,
        binding: &MusicBrainzBindingRequest,
    ) -> Result<MusicBrainzSettingsView, SettingsError> {
        let _op = self.begin_op().await;
        self.mark_consented(binding)?;
        let stored: MusicBrainzSettings = self.store.get().map_err(|e| self.config(e))?;
        Ok(self.view(stored))
    }

    /// Mark the exact staged proposal consented, under the pending mutex
    /// alone (never held across a wait).
    fn mark_consented(&self, binding: &MusicBrainzBindingRequest) -> Result<(), SettingsError> {
        let mut guard = self.pending_mut()?;
        match guard.as_mut() {
            Some(proposal) if proposal.matches(binding) => {
                if proposal.disclosure_version != BRAINZMASH_DISCLOSURE_VERSION {
                    return Err(SettingsError::Conflict {
                        message: "BrainzMash disclosure is outdated".to_owned(),
                    });
                }
                proposal.consented = true;
                Ok(())
            }
            _ => Err(SettingsError::Conflict {
                message: "BrainzMash proposal is stale".to_owned(),
            }),
        }
    }

    /// Take the exact consented and verified proposal out of pending,
    /// under the pending mutex alone.
    fn take_verified(
        &self,
        binding: &MusicBrainzBindingRequest,
    ) -> Result<PendingProposal, SettingsError> {
        let mut guard = self.pending_mut()?;
        let proposal = match guard.as_ref() {
            Some(proposal) if proposal.matches(binding) => proposal.clone(),
            _ => {
                return Err(SettingsError::Conflict {
                    message: "BrainzMash proposal is stale".to_owned(),
                });
            }
        };
        if !proposal.consented {
            return Err(SettingsError::Conflict {
                message: "BrainzMash consent is required".to_owned(),
            });
        }
        if !proposal.verified {
            return Err(SettingsError::Conflict {
                message: "BrainzMash verification is required".to_owned(),
            });
        }
        *guard = None;
        Ok(proposal)
    }

    /// Check that a binding names the exact consented proposal (the
    /// verify gate, before probing upstream).
    pub async fn check_verify_binding(
        &self,
        binding: &MusicBrainzBindingRequest,
    ) -> Result<PendingProposal, SettingsError> {
        let _op = self.begin_op().await;
        let guard = self.pending_mut()?;
        match guard.as_ref() {
            Some(proposal) if proposal.matches(binding) => {
                if !proposal.consented {
                    return Err(SettingsError::Conflict {
                        message: "BrainzMash consent is required".to_owned(),
                    });
                }
                if proposal.disclosure_version != BRAINZMASH_DISCLOSURE_VERSION {
                    return Err(SettingsError::Conflict {
                        message: "BrainzMash disclosure is outdated".to_owned(),
                    });
                }
                Ok(proposal.clone())
            }
            _ => Err(SettingsError::Conflict {
                message: "BrainzMash proposal is stale".to_owned(),
            }),
        }
    }

    /// Whether the exact consented proposal is still current (the probe
    /// re-checks this after the network round trip).
    pub fn proposal_is_current(&self, binding: &MusicBrainzBindingRequest) -> bool {
        self.pending_mut()
            .ok()
            .and_then(|guard| guard.clone())
            .is_some_and(|proposal| proposal.matches(binding) && proposal.consented)
    }

    /// Record a successful verification for the exact consented proposal.
    pub async fn record_verification(
        &self,
        binding: &MusicBrainzBindingRequest,
    ) -> Result<MusicBrainzSettingsView, SettingsError> {
        let _op = self.begin_op().await;
        let mut guard = self.pending_mut()?;
        match guard.as_mut() {
            Some(proposal) if proposal.matches(binding) => {
                if !proposal.consented {
                    return Err(SettingsError::Conflict {
                        message: "BrainzMash consent is required".to_owned(),
                    });
                }
                proposal.verified = true;
            }
            _ => {
                return Err(SettingsError::Conflict {
                    message: "BrainzMash proposal is stale".to_owned(),
                });
            }
        }
        drop(guard);
        let stored: MusicBrainzSettings = self.store.get().map_err(|e| self.config(e))?;
        Ok(self.view(stored))
    }

    /// Promote the exact verified proposal: pin it as the active binding
    /// and switch the source to BrainzMash. The save invalidates the
    /// MusicBrainz cache root.
    pub async fn activate(
        &self,
        binding: &MusicBrainzBindingRequest,
    ) -> Result<MusicBrainzSettingsView, SettingsError> {
        let mark = self.effects.mark(MusicBrainzSettings::KEY);
        let view = self.promote_verified(binding).await?;
        self.effects.saved(mark).await;
        Ok(view)
    }

    /// Promotion core: validate the exact verified proposal, clear it, and
    /// persist the promoted source. Returns the GET view.
    async fn promote_verified(
        &self,
        binding: &MusicBrainzBindingRequest,
    ) -> Result<MusicBrainzSettingsView, SettingsError> {
        let _op = self.begin_op().await;
        let proposal = self.take_verified(binding)?;
        let promoted = MusicBrainzSettings {
            source_mode: MbSourceMode::Brainzmash,
            api_url: proposal.endpoint.trim_end_matches('/').to_owned(),
            rate_limit: BRAINZMASH_RATE_LIMIT,
            community_acknowledged: false,
            selected_source_mode: MbSourceMode::Brainzmash,
            source_id: proposal.source_id.clone(),
            generation: proposal.generation,
            active_brainzmash: Some(
                droppedneedle_kernel::runtime_config::sections::BrainzmashActiveBinding {
                    endpoint: proposal.endpoint.clone(),
                    access_revision: proposal.access_revision.clone(),
                    source_id: proposal.source_id.clone(),
                    generation: proposal.generation,
                    disclosure_version: proposal.disclosure_version.clone(),
                    consented: true,
                    verified: true,
                },
            ),
            clamped_to_official_limits: false,
        };
        let saved = self.update_settings(move |_| Ok(promoted)).await?;
        Ok(self.view(saved))
    }

    /// Read the current transient proposal, if any (the stage response
    /// echoes it so the UI can drive consent/verify/activate).
    pub fn pending(&self) -> Option<PendingProposal> {
        self.pending_mut().ok().and_then(|guard| guard.clone())
    }

    /// Verify a BrainzMash binding or probe a plain tier. A binding names
    /// the exact consented proposal and probes the pinned endpoint; an
    /// update probes its own URL (never BrainzMash). A failed probe is an
    /// upstream error; alternative probes conflict while BrainzMash is
    /// active.
    pub async fn verify(
        &self,
        probes: &dyn VerifyProbes,
        request: &MusicBrainzVerifyRequest,
    ) -> Result<MusicBrainzSettingsView, SettingsError> {
        match request {
            MusicBrainzVerifyRequest::Binding(binding) => {
                let current = self.get()?;
                let selected = if current.pending_brainzmash.is_some() {
                    current.settings.selected_source_mode
                } else {
                    current.settings.source_mode
                };
                if selected != MbSourceMode::Brainzmash {
                    return Err(SettingsError::InvalidInput {
                        message:
                            "BrainzMash binding is only valid for a selected BrainzMash proposal."
                                .to_owned(),
                    });
                }
                self.check_verify_binding(binding).await?;
                // The binding under test is the one being proven, so the
                // probe may send before the binding is active.
                let source = MbSource::BrainzMash {
                    base_url: self.outbound.brainzmash.clone(),
                    binding_valid: true,
                };
                let verdict = probes.musicbrainz(&source).await;
                if !verdict.valid {
                    return Err(verify_failed(verdict));
                }
                if !self.proposal_is_current(binding) {
                    return Err(SettingsError::Conflict {
                        message: "BrainzMash proposal is stale.".to_owned(),
                    });
                }
                self.record_verification(binding).await
            }
            MusicBrainzVerifyRequest::Update(update) => {
                let stored: MusicBrainzSettings = self.store.get().map_err(|e| self.config(e))?;
                if stored.brainzmash_binding_valid() {
                    return Err(SettingsError::Conflict {
                        message: "Alternative MusicBrainz tests are disabled while BrainzMash is \
                                  active; save to switch sources."
                            .to_owned(),
                    });
                }
                if update.source_mode == MbSourceMode::Brainzmash {
                    return Err(SettingsError::InvalidInput {
                        message: "BrainzMash verification requires the consent binding.".to_owned(),
                    });
                }
                let api_url = update
                    .api_url
                    .clone()
                    .filter(|url| !url.trim().is_empty())
                    .unwrap_or_else(|| self.outbound.musicbrainz.clone());
                let api_url = require_service_url(&api_url, "MusicBrainz API URL")?;
                // A public MusicBrainz host keeps the official pace, as a
                // saved source would.
                let source = if is_mb_rate_policy_public_host(&api_url) {
                    MbSource::Official { base_url: api_url }
                } else {
                    MbSource::Mirror {
                        base_url: api_url,
                        rate_per_sec: update.rate_limit,
                    }
                };
                let verdict = probes.musicbrainz(&source).await;
                if !verdict.valid {
                    return Err(verify_failed(verdict));
                }
                self.get()
            }
        }
    }
}

/// A failed MusicBrainz probe: an outage answers as the service being
/// down, with its wait; any other failure keeps the fixed 502.
fn verify_failed(verdict: ProbeVerdict) -> SettingsError {
    match verdict.error {
        Some(error) if error.is_retriable() => SettingsError::ProviderUnavailable {
            provider: error.source(),
            retry_after: error.retry_after_secs(),
            cause: verdict.message,
        },
        _ => SettingsError::Upstream {
            cause: verdict.message,
        },
    }
}
