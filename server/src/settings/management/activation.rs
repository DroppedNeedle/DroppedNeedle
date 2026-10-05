//! Effective profiles per root, activation pins, and activation health.

use super::*;

/// Pin a profile for one root: overlay the assignment overrides onto a
/// detached copy of the assigned profile (the default profile when none
/// is assigned). Unknown override script ids fail closed (the dry run
/// cannot run against a script that does not exist).
pub fn pin_profile(
    settings: &LibraryManagement,
    assignment: &LibraryManagementRootAssignment,
) -> Result<LibraryManagementProfile, SettingsError> {
    let profile_id = assignment
        .profile_id
        .as_deref()
        .unwrap_or(&settings.default_profile_id);
    let mut profile = settings
        .profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .cloned()
        .ok_or_else(|| invalid(&format!("Assigned profile does not exist: {profile_id}")))?;
    let Some(overrides) = &assignment.overrides else {
        return Ok(profile);
    };
    if let Some(enabled) = overrides.metadata_enabled {
        profile.metadata.enabled = enabled;
    }
    if let Some(enabled) = overrides.genres_enabled {
        profile.genres.enabled = enabled;
    }
    if let Some(enabled) = overrides.embedded_artwork_enabled {
        profile.artwork.embedded_enabled = enabled;
    }
    if let Some(enabled) = overrides.external_artwork_enabled {
        profile.artwork.external_enabled = enabled;
    }
    if let Some(enabled) = overrides.rename_enabled {
        profile.organization.rename_enabled = enabled;
    }
    if let Some(enabled) = overrides.move_enabled {
        profile.organization.move_enabled = enabled;
    }
    if let Some(move_sidecars) = overrides.move_sidecars {
        profile.organization.move_sidecars = move_sidecars;
    }
    if let Some(cleanup) = overrides.source_cleanup {
        profile.organization.source_cleanup = cleanup;
    }
    if let Some(preserve) = overrides.preserve_timestamps {
        profile.file_behavior.preserve_timestamps = preserve;
    }
    if let Some(script_id) = &overrides.naming_script_id {
        if !settings
            .naming_scripts
            .iter()
            .any(|script| &script.id == script_id)
        {
            return Err(invalid(&format!(
                "Override naming script does not exist: {script_id}"
            )));
        }
        profile.organization.naming_script_id = script_id.clone();
    }
    match overrides.multi_disc_naming_mode {
        MultiDiscNamingMode::Inherit => {}
        MultiDiscNamingMode::Standard => {
            profile.organization.multi_disc_naming_script_id = None;
        }
        MultiDiscNamingMode::Script => {
            let Some(script_id) = &overrides.multi_disc_naming_script_id else {
                return Err(invalid(
                    "A root multi-disc script override references an unknown naming script.",
                ));
            };
            if !settings
                .naming_scripts
                .iter()
                .any(|script| &script.id == script_id)
            {
                return Err(invalid(
                    "A root multi-disc script override references an unknown naming script.",
                ));
            }
            profile.organization.multi_disc_naming_script_id = Some(script_id.clone());
        }
    }
    if let Some(enabled) = overrides.automatic_edition_acceptance_enabled {
        profile.identity.automatic_edition_acceptance_enabled = enabled;
    }
    Ok(profile)
}

/// Resolve the naming scripts a pinned profile needs: (standard,
/// multi-disc-or-None). Unknown ids fail closed.
pub fn pin_naming_scripts(
    settings: &LibraryManagement,
    effective: &LibraryManagementProfile,
) -> Result<(NamingScript, Option<NamingScript>), SettingsError> {
    let standard = settings
        .naming_scripts
        .iter()
        .find(|script| script.id == effective.organization.naming_script_id)
        .cloned()
        .ok_or_else(|| {
            invalid(&format!(
                "Naming script does not exist: {}",
                effective.organization.naming_script_id
            ))
        })?;
    let multi_disc = match &effective.organization.multi_disc_naming_script_id {
        None => None,
        Some(script_id) => Some(
            settings
                .naming_scripts
                .iter()
                .find(|script| &script.id == script_id)
                .cloned()
                .ok_or_else(|| {
                    invalid(&format!(
                        "Multi-disc naming script does not exist: {script_id}"
                    ))
                })?,
        ),
    };
    Ok((standard, multi_disc))
}

/// Whether an assignment's saved activation is current: the effective
/// profile revision, naming policy, and library policy all match the
/// pins, and the dry-run proof (preview token, hash, confirmation) is
/// present.
pub fn activation_is_current(
    settings: &LibraryManagement,
    assignment: &LibraryManagementRootAssignment,
    library_policy_revision: &str,
) -> bool {
    let (Some(pinned_profile), Some(pinned_naming), Some(pinned_policy)) = (
        assignment.activation_profile_revision.as_deref(),
        assignment.activation_naming_policy_revision.as_deref(),
        assignment.activation_policy_revision.as_deref(),
    ) else {
        return false;
    };
    if pinned_policy != library_policy_revision {
        return false;
    }
    if assignment
        .activation_preview_token
        .as_deref()
        .unwrap_or_default()
        .is_empty()
        || assignment
            .activation_preview_hash
            .as_deref()
            .unwrap_or_default()
            .is_empty()
        || assignment.activation_confirmed_at.is_none()
    {
        return false;
    }
    let Ok(effective) = pin_profile(settings, assignment) else {
        return false;
    };
    let mut detached = effective;
    detached.revision = profile_revision(&detached);
    if detached.revision != pinned_profile {
        return false;
    }
    let Ok((standard, multi_disc)) = pin_naming_scripts(settings, &detached) else {
        return false;
    };
    naming_policy_revision(&standard, multi_disc.as_ref()) == pinned_naming
}

/// Whether a default-only migration is the sole activation drift: the
/// stored profile carries the current project defaults and the
/// activation is current for that same effective profile with a
/// historical default list substituted back in. Read-only: pins
/// converge on the next confirmed dry run.
pub fn migration_carry_applies(
    settings: &LibraryManagement,
    assignment: &LibraryManagementRootAssignment,
    library_policy_revision: &str,
) -> bool {
    let Ok(effective) = pin_profile(settings, assignment) else {
        return false;
    };
    if effective.organization.sidecar_patterns
        != OrganizationManagementSettings::default().sidecar_patterns
    {
        return false;
    }
    for historical in sidecar_default_history() {
        let mut pre_migration = effective.clone();
        pre_migration.organization.sidecar_patterns = historical;
        pre_migration.revision = profile_revision(&pre_migration);
        // The historical substitution must reproduce the pinned profile
        // revision exactly; the naming/policy pins are unchanged.
        if assignment.activation_profile_revision.as_deref()
            == Some(pre_migration.revision.as_str())
            && activation_pins_match_except_profile(settings, assignment, library_policy_revision)
        {
            return true;
        }
    }
    false
}

fn activation_pins_match_except_profile(
    settings: &LibraryManagement,
    assignment: &LibraryManagementRootAssignment,
    library_policy_revision: &str,
) -> bool {
    let (Some(pinned_naming), Some(pinned_policy)) = (
        assignment.activation_naming_policy_revision.as_deref(),
        assignment.activation_policy_revision.as_deref(),
    ) else {
        return false;
    };
    if pinned_policy != library_policy_revision {
        return false;
    }
    if assignment
        .activation_preview_token
        .as_deref()
        .unwrap_or_default()
        .is_empty()
        || assignment
            .activation_preview_hash
            .as_deref()
            .unwrap_or_default()
            .is_empty()
        || assignment.activation_confirmed_at.is_none()
    {
        return false;
    }
    let Ok(effective) = pin_profile(settings, assignment) else {
        return false;
    };
    let Ok((standard, multi_disc)) = pin_naming_scripts(settings, &effective) else {
        return false;
    };
    naming_policy_revision(&standard, multi_disc.as_ref()) == pinned_naming
}

/// Dry-run activation health: stale roots (saved activation no longer
/// matches) and blocked roots (no dry run could help).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[schema(as = LibraryManagementActivationHealthResponse)]
pub struct ActivationHealth {
    /// Active roots whose activation no longer matches.
    pub stale_root_ids: Vec<String>,
    /// Active roots no dry run could help.
    pub blocked_root_ids: Vec<String>,
    /// Policy error, set only with a non-empty `blocked_root_ids`.
    pub blocked_reason: Option<String>,
}

/// Validate every root assignment against the library policy: the
/// policy resolves, the recycle bin overlaps no root, every assignment
/// names a known root, and every active root is an available writable
/// directory. Returns the resolved policy revision.
pub fn validate_root_assignments(
    settings: &LibraryManagement,
    library: &TypedLibrary,
) -> Result<String, SettingsError> {
    let resolved = crate::settings::library_policy::resolve(library)?;
    if !settings.recycle_bin_path.trim().is_empty() {
        let recycle = std::path::PathBuf::from(settings.recycle_bin_path.trim());
        for root in &resolved.settings.library_roots {
            let library_root = std::path::PathBuf::from(&root.path);
            if recycle == library_root
                || recycle.starts_with(&library_root)
                || library_root.starts_with(&recycle)
            {
                return Err(invalid(
                    "The Library Management recycle bin cannot overlap a library root.",
                ));
            }
        }
    }
    let roots: BTreeMap<&str, &LibraryRoot> = resolved
        .settings
        .library_roots
        .iter()
        .map(|root| (root.id.as_str(), root))
        .collect();
    for assignment in &settings.root_assignments {
        let Some(root) = roots.get(assignment.root_id.as_str()) else {
            return Err(invalid(
                "A Library Management assignment references an unknown root.",
            ));
        };
        if !active_automatic(Some(assignment)) {
            continue;
        }
        let path = std::path::Path::new(&root.path);
        if !path.exists() || !path.is_dir() {
            return Err(invalid(&format!(
                "Library root {} is not currently available.",
                root.label
            )));
        }
        // Writable check: the directory must accept a probe file. The
        // probe is created and removed atomically; failure means
        // read-only (or a permissions error either way).
        let probe = path.join(".droppedneedle-write-probe");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
        {
            Ok(_) => {
                let _ = std::fs::remove_file(&probe);
            }
            Err(_) => {
                return Err(invalid(&format!(
                    "Library root {} is not currently writable.",
                    root.label
                )));
            }
        }
    }
    Ok(resolved.policy_revision)
}

/// Compute activation health for the active automatic roots. One broken
/// assignment must not hide the others: roots whose effective profile
/// cannot resolve land in blocked; roots that resolve but no longer
/// match land in stale (unless a default-only migration carries them).
/// A policy that cannot resolve at all blocks every active root with
/// the policy error as the reason.
pub fn activation_health(settings: &LibraryManagement, library: &TypedLibrary) -> ActivationHealth {
    let mut health = ActivationHealth::default();
    let policy_revision = match validate_root_assignments(settings, library) {
        Ok(revision) => revision,
        Err(error) => {
            let blocked: Vec<String> = settings
                .root_assignments
                .iter()
                .filter(|assignment| active_automatic(Some(assignment)))
                .map(|assignment| assignment.root_id.clone())
                .collect();
            if !blocked.is_empty() {
                health.blocked_root_ids = blocked;
                health.blocked_reason = Some(match error {
                    SettingsError::InvalidInput { message } => message,
                    other => format!("{other:?}"),
                });
            }
            return health;
        }
    };
    for assignment in &settings.root_assignments {
        if !active_automatic(Some(assignment)) {
            continue;
        }
        let pinned = pin_profile(settings, assignment)
            .ok()
            .and_then(|effective| {
                pin_naming_scripts(settings, &effective)
                    .ok()
                    .map(|_| effective)
            });
        if pinned.is_none() {
            health.blocked_root_ids.push(assignment.root_id.clone());
            continue;
        }
        if activation_is_current(settings, assignment, &policy_revision)
            || migration_carry_applies(settings, assignment, &policy_revision)
        {
            continue;
        }
        health.stale_root_ids.push(assignment.root_id.clone());
    }
    health
}
