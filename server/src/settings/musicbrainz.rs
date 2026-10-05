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

use super::effects::{SaveEffects, SavedSection};
use super::error::SettingsError;
use super::models::{
    BrainzmashPendingProposal, MusicBrainzBindingRequest, MusicBrainzSettingsUpdate,
    MusicBrainzSettingsView, MusicBrainzVerifyRequest,
};
use super::verify::{VerifyProbes, require_service_url};
use crate::ids::IdGenerator;
use crate::runtime_config::ConfigStore;
use crate::runtime_config::sections::{
    BRAINZMASH_CONCURRENT_SEARCHES, BRAINZMASH_ENDPOINT, BRAINZMASH_RATE_LIMIT, MbSourceMode,
    MusicBrainzSettings, OFFICIAL_MB_API_BASE, OFFICIAL_MB_CONCURRENT_SEARCHES,
    OFFICIAL_MB_RATE_LIMIT,
};

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

fn exact_nonblank(value: &str) -> bool {
    !value.is_empty() && value == value.trim()
}

/// Whether the pinned built-in BrainzMash source may serve traffic: the
/// mode is brainzmash, the endpoint is the approved origin, the
/// generation is positive, and the source id is exact nonblank. The
/// interactive binding is optional for the default; these checks fence
/// caches and transport to the approved origin.
pub fn is_brainzmash_active_binding_valid(settings: &MusicBrainzSettings) -> bool {
    settings.source_mode == MbSourceMode::Brainzmash
        && settings.api_url == BRAINZMASH_ENDPOINT
        && settings.generation > 0
        && exact_nonblank(&settings.source_id)
}

/// The lifecycle service: settled state in config, the pending proposal
/// in memory. The op mutex serializes whole lifecycle operations like
/// v2's coordinator lock (always taken before the pending mutex; never
/// re-entered).
pub struct MusicBrainzLifecycle {
    /// Config store.
    pub store: std::sync::Arc<ConfigStore>,
    /// Whole-operation serialization.
    pub op: Mutex<()>,
    /// Transient pending proposal.
    pub pending: Mutex<Option<PendingProposal>>,
    /// Error-id mint.
    pub ids: std::sync::Arc<dyn IdGenerator>,
    /// Post-save fan-out (source changes invalidate the cache root).
    pub effects: std::sync::Arc<dyn SaveEffects>,
}

impl MusicBrainzLifecycle {
    /// Build over the shared store, ids, and save fan-out.
    pub fn new(
        store: std::sync::Arc<ConfigStore>,
        ids: std::sync::Arc<dyn IdGenerator>,
        effects: std::sync::Arc<dyn SaveEffects>,
    ) -> Self {
        Self {
            store,
            op: Mutex::new(()),
            pending: Mutex::new(None),
            ids,
            effects,
        }
    }

    fn config(&self, error: crate::runtime_config::ConfigError) -> SettingsError {
        SettingsError::from_config(error, self.ids.as_ref())
    }

    fn begin_op(&self) -> Result<std::sync::MutexGuard<'_, ()>, SettingsError> {
        self.op.lock().map_err(|cause| {
            SettingsError::internal(
                &format!("musicbrainz lifecycle lock poisoned: {cause}"),
                self.ids.as_ref(),
            )
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
            SettingsError::internal(
                &format!("musicbrainz proposal lock poisoned: {cause}"),
                self.ids.as_ref(),
            )
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
        let _op = self.begin_op()?;
        let previous: MusicBrainzSettings = self.store.get().map_err(|e| self.config(e))?;
        let mode = update.source_mode;
        if mode == MbSourceMode::Brainzmash {
            return Err(SettingsError::InvalidInput {
                message:
                    "BrainzMash moves through the consent-bound stage/consent/verify/activate flow"
                        .to_owned(),
            });
        }
        if mode == MbSourceMode::Official {
            let view = self.save_official(&previous)?;
            drop(_op);
            self.effects.after_save(SavedSection::MusicBrainz).await;
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
        let source_changed =
            previous.source_mode != mode || previous.api_url.trim_end_matches('/') != api_url;
        let (source_id, generation) = if source_changed || previous.source_id.trim().is_empty() {
            (
                uuid::Uuid::new_v4().to_string(),
                previous.generation.max(1) + 1,
            )
        } else {
            (previous.source_id.clone(), previous.generation.max(1))
        };
        let mut next = MusicBrainzSettings {
            source_mode: mode,
            api_url,
            rate_limit: update.rate_limit,
            concurrent_searches: update.concurrent_searches,
            community_acknowledged: update.community_acknowledged.unwrap_or(false),
            selected_source_mode: mode,
            source_id,
            generation,
            ..previous.clone()
        };
        next.active_brainzmash = None;
        next.clamped_to_official_limits = false;
        *self.pending_mut()? = None;
        let saved: MusicBrainzSettings = self.store.save(next).map_err(|e| self.config(e))?;
        let view = self.view(saved);
        drop(_op);
        self.effects.after_save(SavedSection::MusicBrainz).await;
        Ok(view)
    }

    fn save_official(
        &self,
        previous: &MusicBrainzSettings,
    ) -> Result<MusicBrainzSettingsView, SettingsError> {
        let source_changed = previous.source_mode != MbSourceMode::Official
            || previous.api_url.trim_end_matches('/') != OFFICIAL_MB_API_BASE;
        let (source_id, generation) = if source_changed || previous.source_id.trim().is_empty() {
            (
                uuid::Uuid::new_v4().to_string(),
                previous.generation.max(1) + 1,
            )
        } else {
            (previous.source_id.clone(), previous.generation.max(1))
        };
        let next = MusicBrainzSettings {
            source_mode: MbSourceMode::Official,
            api_url: OFFICIAL_MB_API_BASE.to_owned(),
            rate_limit: OFFICIAL_MB_RATE_LIMIT,
            concurrent_searches: OFFICIAL_MB_CONCURRENT_SEARCHES,
            community_acknowledged: false,
            selected_source_mode: MbSourceMode::Official,
            source_id,
            generation,
            active_brainzmash: None,
            clamped_to_official_limits: false,
        };
        *self.pending_mut()? = None;
        let saved: MusicBrainzSettings = self.store.save(next).map_err(|e| self.config(e))?;
        Ok(self.view(saved))
    }

    /// Stage a BrainzMash proposal without probing upstream: select the
    /// tier, rotate identity on source change, mint a fresh proposal.
    /// The save invalidates the MusicBrainz cache root.
    pub async fn stage(&self) -> Result<(MusicBrainzSettingsView, PendingProposal), SettingsError> {
        let _op = self.begin_op()?;
        let previous: MusicBrainzSettings = self.store.get().map_err(|e| self.config(e))?;
        let source_changed = previous.source_mode != MbSourceMode::Brainzmash;
        let (source_id, generation) = if source_changed || previous.source_id.trim().is_empty() {
            (
                uuid::Uuid::new_v4().to_string(),
                previous.generation.max(1) + 1,
            )
        } else {
            (previous.source_id.clone(), previous.generation.max(1))
        };
        let next = MusicBrainzSettings {
            source_mode: MbSourceMode::Brainzmash,
            api_url: BRAINZMASH_ENDPOINT.to_owned(),
            rate_limit: BRAINZMASH_RATE_LIMIT,
            concurrent_searches: BRAINZMASH_CONCURRENT_SEARCHES,
            community_acknowledged: previous.community_acknowledged,
            selected_source_mode: MbSourceMode::Brainzmash,
            source_id,
            generation,
            active_brainzmash: if source_changed {
                None
            } else {
                previous.active_brainzmash.clone()
            },
            clamped_to_official_limits: false,
        };
        let proposal = PendingProposal::fresh(generation + 1);
        *self.pending_mut()? = Some(proposal.clone());
        let saved: MusicBrainzSettings = self.store.save(next).map_err(|e| self.config(e))?;
        let view = self.view(saved);
        drop(_op);
        self.effects.after_save(SavedSection::MusicBrainz).await;
        Ok((view, proposal))
    }

    /// Record consent for the exact staged proposal and the consenting
    /// admin. A stale proposal or an outdated disclosure is a 409.
    pub fn consent(
        &self,
        binding: &MusicBrainzBindingRequest,
        admin_id: &str,
    ) -> Result<MusicBrainzSettingsView, SettingsError> {
        let _op = self.begin_op()?;
        let mut guard = self.pending_mut()?;
        let pending = guard.as_mut();
        match pending {
            Some(proposal) if proposal.matches(binding) => {
                if proposal.disclosure_version != BRAINZMASH_DISCLOSURE_VERSION {
                    return Err(SettingsError::Conflict {
                        message: "BrainzMash disclosure is outdated".to_owned(),
                    });
                }
                proposal.consented = true;
            }
            _ => {
                return Err(SettingsError::Conflict {
                    message: "BrainzMash proposal is stale".to_owned(),
                });
            }
        }
        drop(guard);
        let mut internal: crate::runtime_config::sections::InternalState =
            self.store.get().map_err(|e| self.config(e))?;
        internal.brainzmash_consent_admin = Some(admin_id.to_owned());
        let _: crate::runtime_config::sections::InternalState =
            self.store.save(internal).map_err(|e| self.config(e))?;
        let stored: MusicBrainzSettings = self.store.get().map_err(|e| self.config(e))?;
        Ok(self.view(stored))
    }

    /// Check that a binding names the exact consented proposal (the
    /// verify gate, before probing upstream).
    pub fn check_verify_binding(
        &self,
        binding: &MusicBrainzBindingRequest,
    ) -> Result<PendingProposal, SettingsError> {
        let _op = self.begin_op()?;
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
    pub fn record_verification(
        &self,
        binding: &MusicBrainzBindingRequest,
    ) -> Result<MusicBrainzSettingsView, SettingsError> {
        let _op = self.begin_op()?;
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
        // The locks live and die inside the sync core: no guard
        // crosses the effects await below (guards are not Send).
        let view = self.promote_verified(binding)?;
        self.effects.after_save(SavedSection::MusicBrainz).await;
        Ok(view)
    }

    /// Synchronous promotion core: validate the exact verified proposal,
    /// clear it, and persist the promoted source. Returns the GET view.
    fn promote_verified(
        &self,
        binding: &MusicBrainzBindingRequest,
    ) -> Result<MusicBrainzSettingsView, SettingsError> {
        let _op = self.begin_op()?;
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
        drop(guard);
        let promoted = MusicBrainzSettings {
            source_mode: MbSourceMode::Brainzmash,
            api_url: proposal.endpoint.trim_end_matches('/').to_owned(),
            rate_limit: BRAINZMASH_RATE_LIMIT,
            concurrent_searches: BRAINZMASH_CONCURRENT_SEARCHES,
            community_acknowledged: false,
            selected_source_mode: MbSourceMode::Brainzmash,
            source_id: proposal.source_id.clone(),
            generation: proposal.generation,
            active_brainzmash: Some(crate::runtime_config::sections::BrainzmashActiveBinding {
                endpoint: proposal.endpoint.clone(),
                access_revision: proposal.access_revision.clone(),
                source_id: proposal.source_id.clone(),
                generation: proposal.generation,
                disclosure_version: proposal.disclosure_version.clone(),
                consented: true,
                verified: true,
            }),
            clamped_to_official_limits: false,
        };
        let saved: MusicBrainzSettings = self.store.save(promoted).map_err(|e| self.config(e))?;
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
                self.check_verify_binding(binding)?;
                let verdict = probes.musicbrainz(BRAINZMASH_ENDPOINT).await;
                if !verdict.valid {
                    return Err(SettingsError::Upstream {
                        message: verdict.message,
                    });
                }
                if !self.proposal_is_current(binding) {
                    return Err(SettingsError::Conflict {
                        message: "BrainzMash proposal is stale.".to_owned(),
                    });
                }
                self.record_verification(binding)
            }
            MusicBrainzVerifyRequest::Update(update) => {
                let stored: MusicBrainzSettings = self.store.get().map_err(|e| self.config(e))?;
                if is_brainzmash_active_binding_valid(&stored) {
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
                    .unwrap_or_else(|| OFFICIAL_MB_API_BASE.to_owned());
                let api_url = require_service_url(&api_url, "MusicBrainz API URL")?;
                let verdict = probes.musicbrainz(&api_url).await;
                if !verdict.valid {
                    return Err(SettingsError::Upstream {
                        message: verdict.message,
                    });
                }
                self.get()
            }
        }
    }
}
