//! The Library Management service behind `/settings/library-management/*`.
//!
//! Ports v2's `LibraryManagementProfileService`: settings reads and CAS
//! saves, impact previews, profile create/copy/update/delete, preset
//! diffs, activation health, and profile sharing. Every write goes
//! through [`LibraryManagementService::save_settings`], which normalizes,
//! re-validates presets and root assignments, and refuses an automatic
//! change without a current confirmed dry run.
//!
//! Settings are read from the store on every call, never cached. The
//! first read seeds the presets (v2 seeds exactly once, from the naming
//! template configured before Library Management existed) and later
//! reads persist preset-catalog migrations. All methods touch the config
//! file and library roots synchronously, so handlers run them on the
//! blocking pool.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use uuid::Uuid;

use super::activation::{
    ActivationHealth, activation_health, activation_is_current, validate_root_assignments,
};
use super::bundle::{
    PROFILE_BUNDLE_MIME_TYPE, PortableScript, export_profile_bundle, materialize_profile_bundle,
    parse_profile_bundle, preview_materialized_profile, profile_aspects, profile_bundle_filename,
    profile_import_warnings, resolve_import_names,
};
use super::impact::{ChangeImpact, active_automatic, classify};
use super::invalid;
use super::normalize::{migration_carry, normalize, validate_preset_provenance};
use super::presets::{PresetDiff, initial_settings, migrate_presets, preset_diff};
use super::revision::settings_revision;
use super::script::ScriptCompiler;
use crate::ids::IdGenerator;
use crate::runtime_config::ConfigStore;
use crate::runtime_config::secret_sections::TypedLibrary;
use crate::runtime_config::sections::{LibraryManagement, LibraryManagementProfile};
use crate::settings::error::SettingsError;
use crate::settings::models::{
    LibraryManagementProfileExportResponse, LibraryManagementProfileImportPreviewResponse,
    LibraryManagementProfileImportResponse, LibraryManagementProfileMutationResponse,
    LibraryManagementSettingsResponse,
};

const STALE_SETTINGS: &str =
    "Library Management settings changed. Refresh this page and try again.";

/// Library Management operations over the config store.
pub struct LibraryManagementService {
    store: Arc<ConfigStore>,
    ids: Arc<dyn IdGenerator>,
    compiler: Arc<dyn ScriptCompiler>,
    /// Serializes read-compare-write so two saves with the same expected
    /// revision cannot both land.
    write: Mutex<()>,
}

impl LibraryManagementService {
    /// Build over the shared store, the error-id mint, and a script
    /// compiler.
    pub fn new(
        store: Arc<ConfigStore>,
        ids: Arc<dyn IdGenerator>,
        compiler: Arc<dyn ScriptCompiler>,
    ) -> Self {
        Self {
            store,
            ids,
            compiler,
            write: Mutex::new(()),
        }
    }

    fn config(&self, error: crate::runtime_config::ConfigError) -> SettingsError {
        SettingsError::from_config(error, self.ids.as_ref())
    }

    fn lock(&self) -> Result<MutexGuard<'_, ()>, SettingsError> {
        self.write.lock().map_err(|cause| {
            SettingsError::internal(
                &format!("library management write lock poisoned: {cause}"),
                self.ids.as_ref(),
            )
        })
    }

    /// The typed library settings (roots and the legacy naming template).
    /// Only non-secret fields are used, so the masked read is enough.
    fn library(&self) -> Result<TypedLibrary, SettingsError> {
        self.store
            .get_masked::<TypedLibrary>()
            .map(crate::runtime_config::Masked::into_inner)
            .map_err(|error| self.config(error))
    }

    /// Current settings: seeded on first read, preset-migrated and
    /// normalized on every read, persisted when either changed anything.
    fn load(&self) -> Result<LibraryManagement, SettingsError> {
        let stored: LibraryManagement = self.store.get().map_err(|error| self.config(error))?;
        let mut settings = if stored.profiles.is_empty() {
            initial_settings(&self.library()?.naming_template)
        } else {
            let mut migrated = stored.clone();
            migrate_presets(&mut migrated);
            migrated
        };
        normalize(&mut settings, self.compiler.as_ref()).map_err(|error| {
            tracing::error!(?error, "stored library management settings are invalid");
            SettingsError::InvalidInput {
                message: "Library Management settings are invalid.".to_owned(),
            }
        })?;
        if settings != stored {
            self.store
                .save(settings.clone())
                .map_err(|error| self.config(error))?;
        }
        Ok(settings)
    }

    fn require_current(
        &self,
        settings: &LibraryManagement,
        expected: &str,
    ) -> Result<String, SettingsError> {
        let current = settings_revision(settings);
        if current != expected {
            return Err(SettingsError::StaleRevision {
                message: STALE_SETTINGS.to_owned(),
            });
        }
        Ok(current)
    }

    fn response(settings: LibraryManagement) -> LibraryManagementSettingsResponse {
        LibraryManagementSettingsResponse {
            settings_revision: settings_revision(&settings),
            settings,
        }
    }

    /// Normalize a candidate against the current settings: recompute
    /// revisions, carry or clear activation pins (never trusted from the
    /// client), then validate.
    fn normalized_candidate(
        &self,
        current: &LibraryManagement,
        proposed: LibraryManagement,
    ) -> Result<LibraryManagement, SettingsError> {
        let mut candidate = proposed;
        candidate.preset_catalog_version = current.preset_catalog_version;
        migration_carry(current, &mut candidate, &current.default_profile_id)?;
        normalize(&mut candidate, self.compiler.as_ref())?;
        validate_preset_provenance(current, &candidate)?;
        Ok(candidate)
    }

    /// The settings plus their revision.
    pub fn get_settings(&self) -> Result<LibraryManagementSettingsResponse, SettingsError> {
        let _guard = self.lock()?;
        self.load().map(Self::response)
    }

    /// One profile by id.
    pub fn get_profile(&self, profile_id: &str) -> Result<LibraryManagementProfile, SettingsError> {
        let _guard = self.lock()?;
        find_profile(&self.load()?, profile_id).cloned()
    }

    /// Classify a candidate without saving. `stale` reports whether the
    /// caller's revision is behind; the verdict is computed either way.
    pub fn preview_impact(
        &self,
        proposed: LibraryManagement,
        expected_settings_revision: Option<&str>,
    ) -> Result<ChangeImpact, SettingsError> {
        let _guard = self.lock()?;
        let current = self.load()?;
        let candidate = self.normalized_candidate(&current, proposed)?;
        validate_root_assignments(&candidate, &self.library()?)?;
        Ok(classify(&current, &candidate, expected_settings_revision))
    }

    /// Save a full candidate under CAS. A change that gives an automatic
    /// root new write scope needs a current confirmed dry run for that
    /// root, or the save is refused.
    pub fn save_settings(
        &self,
        proposed: LibraryManagement,
        expected_settings_revision: &str,
    ) -> Result<LibraryManagementSettingsResponse, SettingsError> {
        let _guard = self.lock()?;
        self.save_locked(proposed, expected_settings_revision)
    }

    fn save_locked(
        &self,
        proposed: LibraryManagement,
        expected_settings_revision: &str,
    ) -> Result<LibraryManagementSettingsResponse, SettingsError> {
        let current = self.load()?;
        let current_revision = self.require_current(&current, expected_settings_revision)?;
        let candidate = self.normalized_candidate(&current, proposed)?;
        let policy_revision = validate_root_assignments(&candidate, &self.library()?)?;
        let impact = classify(&current, &candidate, Some(expected_settings_revision));
        if impact.preview_required {
            for root_id in &impact.affected_root_ids {
                let Some(assignment) = candidate
                    .root_assignments
                    .iter()
                    .find(|assignment| &assignment.root_id == root_id)
                else {
                    continue;
                };
                if !active_automatic(Some(assignment)) {
                    continue;
                }
                let confirmed = activation_is_current(&candidate, assignment, &policy_revision)
                    && assignment.activation_settings_revision.as_deref()
                        == Some(current_revision.as_str());
                if !confirmed {
                    return Err(invalid(
                        "A current Library Management dry run must be confirmed before this \
                         automatic change can be enabled.",
                    ));
                }
            }
        }
        self.store
            .save(candidate.clone())
            .map_err(|error| self.config(error))?;
        Ok(Self::response(candidate))
    }

    /// Create a profile as a copy of the default profile.
    pub fn create_profile(
        &self,
        name: &str,
        description: &str,
        expected_settings_revision: &str,
    ) -> Result<LibraryManagementProfileMutationResponse, SettingsError> {
        let _guard = self.lock()?;
        let settings = self.load()?;
        let source = find_profile(&settings, &settings.default_profile_id)?.clone();
        self.copy_locked(
            settings,
            source,
            name,
            description,
            expected_settings_revision,
        )
    }

    /// Copy one profile under a new name.
    pub fn copy_profile(
        &self,
        profile_id: &str,
        name: &str,
        expected_settings_revision: &str,
    ) -> Result<LibraryManagementProfileMutationResponse, SettingsError> {
        let _guard = self.lock()?;
        let settings = self.load()?;
        let source = find_profile(&settings, profile_id)?.clone();
        let description = source.description.clone();
        self.copy_locked(
            settings,
            source,
            name,
            &description,
            expected_settings_revision,
        )
    }

    fn copy_locked(
        &self,
        mut settings: LibraryManagement,
        source: LibraryManagementProfile,
        name: &str,
        description: &str,
        expected_settings_revision: &str,
    ) -> Result<LibraryManagementProfileMutationResponse, SettingsError> {
        let mut copied = source;
        copied.id = Uuid::new_v4().to_string();
        copied.name = name.to_owned();
        copied.description = description.to_owned();
        copied.preset_origin = None;
        copied.preset_version = None;
        copied.revision = String::new();
        let copied_id = copied.id.clone();
        settings.profiles.push(copied);
        let saved = self.save_locked(settings, expected_settings_revision)?;
        mutation_response(saved, &copied_id)
    }

    /// Replace one profile. The body id must match the path id.
    pub fn update_profile(
        &self,
        profile_id: &str,
        profile: LibraryManagementProfile,
        expected_settings_revision: &str,
    ) -> Result<LibraryManagementProfileMutationResponse, SettingsError> {
        if profile.id != profile_id {
            return Err(invalid("The profile ID does not match the request path."));
        }
        let _guard = self.lock()?;
        let mut settings = self.load()?;
        let slot = settings
            .profiles
            .iter_mut()
            .find(|current| current.id == profile.id)
            .ok_or_else(|| invalid(MISSING_PROFILE))?;
        *slot = profile;
        let saved = self.save_locked(settings, expected_settings_revision)?;
        mutation_response(saved, profile_id)
    }

    /// Delete one custom profile that is neither the default nor assigned
    /// to a root. Scripts only it used go with it; preset scripts stay.
    pub fn delete_profile(
        &self,
        profile_id: &str,
        expected_settings_revision: &str,
    ) -> Result<LibraryManagementSettingsResponse, SettingsError> {
        let _guard = self.lock()?;
        let mut settings = self.load()?;
        let deleted = find_profile(&settings, profile_id)?.clone();
        if deleted.preset_origin.is_some() {
            return Err(invalid(
                "Built-in Library Management presets cannot be deleted.",
            ));
        }
        if settings.default_profile_id == profile_id {
            return Err(invalid("The default profile cannot be deleted."));
        }
        if settings
            .root_assignments
            .iter()
            .any(|assignment| assignment.profile_id.as_deref() == Some(profile_id))
        {
            return Err(invalid(
                "A profile assigned to a library root cannot be deleted.",
            ));
        }
        settings.profiles.retain(|profile| profile.id != profile_id);
        prune_orphaned_scripts(&mut settings, &deleted);
        self.save_locked(settings, expected_settings_revision)
    }

    /// Which groups of a preset-tracking profile differ from the preset.
    pub fn preset_diff(&self, profile_id: &str) -> Result<PresetDiff, SettingsError> {
        let _guard = self.lock()?;
        Ok(preset_diff(find_profile(&self.load()?, profile_id)?))
    }

    /// Dry-run activation health for the active automatic roots.
    pub fn activation_health(&self) -> Result<ActivationHealth, SettingsError> {
        let _guard = self.lock()?;
        let settings = self.load()?;
        Ok(activation_health(&settings, &self.library()?))
    }

    /// Export one profile as a share bundle (document plus share code).
    pub fn export_profile(
        &self,
        profile_id: &str,
        expected_settings_revision: &str,
    ) -> Result<LibraryManagementProfileExportResponse, SettingsError> {
        let _guard = self.lock()?;
        let settings = self.load()?;
        let settings_revision = self.require_current(&settings, expected_settings_revision)?;
        let profile = find_profile(&settings, profile_id)?;
        let bundle = export_profile_bundle(
            profile,
            &settings.naming_scripts,
            &settings.tagging_scripts,
            self.ids.as_ref(),
        )?;
        Ok(LibraryManagementProfileExportResponse {
            filename: profile_bundle_filename(&profile.name),
            mime_type: PROFILE_BUNDLE_MIME_TYPE.to_owned(),
            document: bundle.document,
            share_code: bundle.share_code,
            bundle_hash: bundle.bundle_hash,
            settings_revision,
        })
    }

    /// Parse a shared profile and show what importing it would add,
    /// without saving. Ids are deterministic so the preview is stable.
    pub fn preview_profile_import(
        &self,
        content: &str,
        expected_settings_revision: &str,
    ) -> Result<LibraryManagementProfileImportPreviewResponse, SettingsError> {
        let _guard = self.lock()?;
        let settings = self.load()?;
        let settings_revision = self.require_current(&settings, expected_settings_revision)?;
        let parsed = parse_profile_bundle(content)?;
        let preview = preview_materialized_profile(&parsed, self.compiler.as_ref())?;
        let resolved = resolve_import_names(&preview, &settings, self.compiler.as_ref())?;
        Ok(LibraryManagementProfileImportPreviewResponse {
            aspects: profile_aspects(&resolved.profile),
            warnings: profile_import_warnings(&resolved.profile),
            profile: resolved.profile,
            bundle_hash: parsed.bundle_hash,
            settings_revision,
            naming_scripts: resolved.naming_scripts,
            tagging_scripts: resolved.tagging_scripts,
        })
    }

    /// Import a shared profile the admin reviewed. The bundle must still
    /// hash to the reviewed hash; names that collide get a suffix and the
    /// profile takes the confirmed name.
    pub fn import_profile(
        &self,
        content: &str,
        reviewed_bundle_hash: &str,
        name: &str,
        expected_settings_revision: &str,
    ) -> Result<LibraryManagementProfileImportResponse, SettingsError> {
        let _guard = self.lock()?;
        let mut settings = self.load()?;
        self.require_current(&settings, expected_settings_revision)?;
        let parsed = parse_profile_bundle(content)?;
        if parsed.bundle_hash != reviewed_bundle_hash {
            return Err(SettingsError::StaleRevision {
                message: "The shared profile changed after it was reviewed. Review it again."
                    .to_owned(),
            });
        }
        let fresh_ids = |scripts: &[PortableScript]| -> BTreeMap<String, String> {
            scripts
                .iter()
                .map(|script| (script.key.clone(), Uuid::new_v4().to_string()))
                .collect()
        };
        let naming_ids = fresh_ids(&parsed.payload.naming_scripts);
        let tagging_ids = fresh_ids(&parsed.payload.tagging_scripts);
        let profile_id = Uuid::new_v4().to_string();
        let materialized = materialize_profile_bundle(
            &parsed,
            &profile_id,
            &naming_ids,
            &tagging_ids,
            self.compiler.as_ref(),
        )?;
        let mut resolved = resolve_import_names(&materialized, &settings, self.compiler.as_ref())?;
        resolved.profile.name = name.to_owned();
        let naming_added: BTreeSet<String> = resolved
            .naming_scripts
            .iter()
            .map(|script| script.id.clone())
            .collect();
        let tagging_added: BTreeSet<String> = resolved
            .tagging_scripts
            .iter()
            .map(|script| script.id.clone())
            .collect();
        settings.profiles.push(resolved.profile);
        settings.naming_scripts.extend(resolved.naming_scripts);
        settings.tagging_scripts.extend(resolved.tagging_scripts);
        let saved = self.save_locked(settings, expected_settings_revision)?;
        Ok(LibraryManagementProfileImportResponse {
            profile: find_profile(&saved.settings, &profile_id)?.clone(),
            settings_revision: saved.settings_revision,
            naming_scripts: saved
                .settings
                .naming_scripts
                .iter()
                .filter(|script| naming_added.contains(&script.id))
                .cloned()
                .collect(),
            tagging_scripts: saved
                .settings
                .tagging_scripts
                .iter()
                .filter(|script| tagging_added.contains(&script.id))
                .cloned()
                .collect(),
        })
    }
}

const MISSING_PROFILE: &str = "The Library Management profile does not exist.";

fn find_profile<'a>(
    settings: &'a LibraryManagement,
    profile_id: &str,
) -> Result<&'a LibraryManagementProfile, SettingsError> {
    settings
        .profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| invalid(MISSING_PROFILE))
}

fn mutation_response(
    saved: LibraryManagementSettingsResponse,
    profile_id: &str,
) -> Result<LibraryManagementProfileMutationResponse, SettingsError> {
    Ok(LibraryManagementProfileMutationResponse {
        profile: find_profile(&saved.settings, profile_id)?.clone(),
        settings_revision: saved.settings_revision,
    })
}

/// Drop the naming and tagging scripts that only the deleted profile
/// used. Scripts still referenced by another profile or a root override,
/// and preset scripts, stay.
fn prune_orphaned_scripts(settings: &mut LibraryManagement, deleted: &LibraryManagementProfile) {
    let naming_refs = |profile: &LibraryManagementProfile| {
        [
            Some(profile.organization.naming_script_id.clone()),
            profile.organization.multi_disc_naming_script_id.clone(),
            profile.artwork.external_naming_script_id.clone(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<String>>()
    };
    let candidate_naming: BTreeSet<String> = naming_refs(deleted).into_iter().collect();
    let mut used_naming: BTreeSet<String> =
        settings.profiles.iter().flat_map(naming_refs).collect();
    used_naming.extend(
        settings
            .root_assignments
            .iter()
            .filter_map(|assignment| assignment.overrides.as_ref())
            .flat_map(|overrides| {
                [
                    overrides.naming_script_id.clone(),
                    overrides.multi_disc_naming_script_id.clone(),
                ]
            })
            .flatten(),
    );
    let candidate_tagging: BTreeSet<String> = deleted
        .metadata
        .tagging_script_ids
        .iter()
        .cloned()
        .collect();
    let used_tagging: BTreeSet<String> = settings
        .profiles
        .iter()
        .flat_map(|profile| profile.metadata.tagging_script_ids.iter().cloned())
        .collect();
    settings.naming_scripts.retain(|script| {
        !candidate_naming.contains(&script.id)
            || used_naming.contains(&script.id)
            || script.preset_origin.is_some()
    });
    settings.tagging_scripts.retain(|script| {
        !candidate_tagging.contains(&script.id)
            || used_tagging.contains(&script.id)
            || script.preset_origin.is_some()
    });
}
