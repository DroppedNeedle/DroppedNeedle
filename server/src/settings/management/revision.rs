//! Content revisions. Every hash is byte-compatible with v2
//! (ASCII-escaped canonical JSON), so migrated activations and CAS
//! tokens keep working.

use super::*;

/// Strip the legacy-compat shims before hashing (v2
/// `_remove_default_*`): default lyrics preservation, default
/// multi-disc naming (`inherit` + null script), and the default
/// identity section (automatic acceptance off).
fn strip_legacy_shims(profile: &mut serde_json::Value) {
    if let Some(enrichment) = profile.get_mut("enrichment")
        && let Some(lyrics) = enrichment.get_mut("lyrics")
        && lyrics.get("preserve_existing") == Some(&serde_json::Value::Bool(false))
        && let Some(map) = lyrics.as_object_mut()
    {
        map.remove("preserve_existing");
    }
    if let Some(overrides) = profile.get_mut("organization") {
        let is_default_multi =
            overrides.get("multi_disc_naming_script_id") == Some(&serde_json::Value::Null);
        if is_default_multi && let Some(map) = overrides.as_object_mut() {
            map.remove("multi_disc_naming_script_id");
        }
    }
    if let Some(identity) = profile.get_mut("identity")
        && identity.get("automatic_edition_acceptance_enabled")
            == Some(&serde_json::Value::Bool(false))
        && let Some(map) = identity.as_object_mut()
    {
        map.remove("automatic_edition_acceptance_enabled");
    }
}

/// Profile content revision: the full profile minus `revision` and the
/// legacy shims, hashed.
pub fn profile_revision(profile: &LibraryManagementProfile) -> String {
    let mut payload = serde_json::to_value(profile).unwrap_or(serde_json::Value::Null);
    if let Some(map) = payload.as_object_mut() {
        map.remove("revision");
    }
    strip_legacy_shims(&mut payload);
    stable_hash(&payload)
}

/// Settings content revision: the full settings with per-profile legacy
/// shims stripped, hashed. Includes profile/script revisions and
/// assignment activation pins.
pub fn settings_revision(settings: &LibraryManagement) -> String {
    let mut payload = serde_json::to_value(settings).unwrap_or(serde_json::Value::Null);
    if let Some(profiles) = payload.get_mut("profiles").and_then(|v| v.as_array_mut()) {
        for profile in profiles {
            strip_legacy_shims(profile);
        }
    }
    stable_hash(&payload)
}

/// Naming-script content revision (minus `revision`).
pub fn naming_script_revision(script: &NamingScript) -> String {
    let mut payload = serde_json::to_value(script).unwrap_or(serde_json::Value::Null);
    if let Some(map) = payload.as_object_mut() {
        map.remove("revision");
    }
    stable_hash(&payload)
}

/// Tagging-script content revision (minus `revision`).
pub fn tagging_script_revision(script: &TaggingScript) -> String {
    let mut payload = serde_json::to_value(script).unwrap_or(serde_json::Value::Null);
    if let Some(map) = payload.as_object_mut() {
        map.remove("revision");
    }
    stable_hash(&payload)
}

/// Naming-policy revision for a pinned profile: the standard script
/// revision alone, or the hashed standard+multi-disc pair.
pub fn naming_policy_revision(
    standard: &NamingScript,
    multi_disc: Option<&NamingScript>,
) -> String {
    match multi_disc {
        None => standard.revision.clone(),
        Some(multi) => {
            let payload = serde_json::json!({
                "policy_version": 1,
                "standard": {"id": standard.id, "revision": standard.revision},
                "multi_disc": {"id": multi.id, "revision": multi.revision},
            });
            stable_hash(&payload)
        }
    }
}
