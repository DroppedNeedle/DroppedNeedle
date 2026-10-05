//! Change-impact classification: which automatic roots gain or lose
//! file-writing scope under a candidate settings document.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::activation::{pin_naming_scripts, pin_profile};
use super::revision::{naming_policy_revision, settings_revision};
use crate::runtime_config::sections::{
    LibraryManagement, LibraryManagementProfile, LibraryManagementRootAssignment, NamingScript,
    TaggingScript,
};

/// Whether an assignment engages automatic work (custom editions excluded:
/// gaining custom-edition automation is always destructive on its own).
pub fn active_automatic(assignment: Option<&LibraryManagementRootAssignment>) -> bool {
    assignment.is_some_and(|assignment| {
        assignment.enabled
            && (assignment.automatic_acquisitions
                || assignment.automatic_drop_imports
                || assignment.automatic_scan_discovered)
    })
}

/// Scope payload for impact diffing: the effective profile minus
/// identity/notification, plus the naming/tagging script revisions it
/// pins. Missing scripts resolve to None (compare-equal), never fail.
pub fn scope_payload(
    settings: &LibraryManagement,
    assignment: &LibraryManagementRootAssignment,
) -> serde_json::Value {
    let mut payload = match pin_profile(settings, assignment) {
        Ok(effective) => serde_json::to_value(&effective).unwrap_or(serde_json::Value::Null),
        Err(_) => serde_json::Value::Null,
    };
    if let Some(map) = payload.as_object_mut() {
        for field in [
            "id",
            "name",
            "description",
            "preset_origin",
            "preset_version",
            "revision",
            "notification",
        ] {
            map.remove(field);
        }
    }
    let naming: BTreeMap<&str, &NamingScript> = settings
        .naming_scripts
        .iter()
        .map(|script| (script.id.as_str(), script))
        .collect();
    let tagging: BTreeMap<&str, &TaggingScript> = settings
        .tagging_scripts
        .iter()
        .map(|script| (script.id.as_str(), script))
        .collect();
    let (naming_id, multi_id, external_id, tagging_ids) = match pin_profile(settings, assignment) {
        Ok(effective) => (
            Some(effective.organization.naming_script_id.clone()),
            effective.organization.multi_disc_naming_script_id.clone(),
            effective.artwork.external_naming_script_id.clone(),
            effective.metadata.tagging_script_ids.clone(),
        ),
        Err(_) => (None, None, None, Vec::new()),
    };
    if let Some(map) = payload.as_object_mut() {
        map.insert(
            "_naming_script_revision".to_owned(),
            naming_id
                .and_then(|id| naming.get(id.as_str()))
                .map(|script| serde_json::Value::String(script.revision.clone()))
                .unwrap_or(serde_json::Value::Null),
        );
        map.insert(
            "_multi_disc_naming_script_revision".to_owned(),
            multi_id
                .and_then(|id| naming.get(id.as_str()))
                .map(|script| serde_json::Value::String(script.revision.clone()))
                .unwrap_or(serde_json::Value::Null),
        );
        map.insert(
            "_naming_policy_revision".to_owned(),
            match pin_profile(settings, assignment)
                .ok()
                .and_then(|effective| pin_naming_scripts(settings, &effective).ok())
            {
                Some((standard, multi_disc)) => serde_json::Value::String(naming_policy_revision(
                    &standard,
                    multi_disc.as_ref(),
                )),
                None => serde_json::Value::Null,
            },
        );
        map.insert(
            "_external_artwork_script_revision".to_owned(),
            external_id
                .and_then(|id| naming.get(id.as_str()))
                .map(|script| serde_json::Value::String(script.revision.clone()))
                .unwrap_or(serde_json::Value::Null),
        );
        map.insert(
            "_tagging_script_revisions".to_owned(),
            serde_json::Value::Array(
                tagging_ids
                    .iter()
                    .map(|id| {
                        tagging
                            .get(id.as_str())
                            .map(|script| serde_json::Value::String(script.revision.clone()))
                            .unwrap_or(serde_json::Value::Null)
                    })
                    .collect(),
            ),
        );
    }
    payload
}

fn field_mode_rank(mode: &str) -> i64 {
    match mode {
        "disabled" | "preserve" => 0,
        "fill_missing" => 1,
        "merge" => 2,
        "replace" => 3,
        _ => 3,
    }
}

fn genre_mode_rank(mode: &str) -> i64 {
    match mode {
        "fill_missing" => 0,
        "merge" => 1,
        "replace" => 2,
        _ => 2,
    }
}

fn ordered_subset(candidate: &[serde_json::Value], original: &[serde_json::Value]) -> bool {
    let kept: Vec<&serde_json::Value> = original
        .iter()
        .filter(|value| candidate.contains(value))
        .collect();
    candidate.len() == kept.len() && candidate.iter().zip(kept.iter()).all(|(a, b)| a == *b)
}

/// Artwork `preserve_existing_types` as a string set (image-type ids).
fn preserve_existing_types(profile: &serde_json::Value) -> BTreeSet<&str> {
    profile
        .get("artwork")
        .and_then(|artwork| artwork.get("preserve_existing_types"))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect()
        })
        .unwrap_or_default()
}

fn reset_safe_boolean(
    candidate: &mut serde_json::Value,
    old: &serde_json::Value,
    path: &[&str],
    safe_from: bool,
    safe_to: bool,
) {
    let mut old_node = old;
    let mut candidate_node = candidate;
    for key in &path[..path.len() - 1] {
        match (old_node.get(key), candidate_node.get_mut(key)) {
            (Some(next_old), Some(next_candidate)) => {
                old_node = next_old;
                candidate_node = next_candidate;
            }
            _ => return,
        }
    }
    let key = path[path.len() - 1];
    let old_value = old_node.get(key).and_then(serde_json::Value::as_bool);
    let candidate_value = candidate_node.get(key).and_then(serde_json::Value::as_bool);
    if old_value == Some(safe_from) && candidate_value == Some(safe_to) {
        candidate_node[key] = serde_json::Value::Bool(safe_from);
    }
}

/// Whether a profile change only narrows write scope: reset every
/// safe-direction boolean/list/threshold in a candidate copy, then the
/// change is restrictive exactly when nothing else differs.
pub fn is_restrictive_profile_change(
    old_profile: &LibraryManagementProfile,
    new_profile: &LibraryManagementProfile,
) -> bool {
    let old = serde_json::to_value(old_profile).unwrap_or(serde_json::Value::Null);
    let new = serde_json::to_value(new_profile).unwrap_or(serde_json::Value::Null);
    if old == new {
        return false;
    }
    let mut candidate = new.clone();
    for path in [
        ["metadata", "enabled"].as_slice(),
        &["metadata", "relationships", "enabled"],
        &["genres", "enabled"],
        &["artwork", "embedded_enabled"],
        &["artwork", "external_enabled"],
        &["organization", "rename_enabled"],
        &["organization", "move_enabled"],
        &["organization", "move_sidecars"],
        &["organization", "remove_empty_directories"],
        &["enrichment", "lyrics", "enabled"],
        &["enrichment", "replaygain", "enabled"],
        &["identity", "automatic_edition_acceptance_enabled"],
    ] {
        reset_safe_boolean(&mut candidate, &old, path, true, false);
    }
    for path in [
        ["metadata", "preserve_embedded_art_during_scrub"].as_slice(),
        &["genres", "listenbrainz_curated_only"],
        &["genres", "lastfm_whitelist_only"],
        &["genres", "write_primary_only_for_constrained_formats"],
        &["artwork", "approved_only"],
        &["artwork", "embedded_front_only"],
        &["artwork", "external_front_only"],
        &["artwork", "never_replace_with_smaller"],
        &["file_behavior", "preserve_timestamps"],
        &["file_behavior", "preserve_permissions"],
        &["file_behavior", "strict_capability_gate"],
        &["file_behavior", "validate_written_metadata"],
        &["file_behavior", "validate_technical_audio"],
    ] {
        reset_safe_boolean(&mut candidate, &old, path, false, true);
    }
    reset_safe_boolean(
        &mut candidate,
        &old,
        &["metadata", "scrub_unmanaged_tags"],
        true,
        false,
    );
    reset_safe_boolean(
        &mut candidate,
        &old,
        &["artwork", "overwrite_external_files"],
        true,
        false,
    );

    let old_fields: BTreeMap<&str, &serde_json::Value> = old
        .get("metadata")
        .and_then(|metadata| metadata.get("fields"))
        .and_then(serde_json::Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(|field| {
                    field
                        .get("field")
                        .and_then(serde_json::Value::as_str)
                        .map(|name| (name, field))
                })
                .collect()
        })
        .unwrap_or_default();
    let new_fields: BTreeMap<&str, &serde_json::Value> = new
        .get("metadata")
        .and_then(|metadata| metadata.get("fields"))
        .and_then(serde_json::Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(|field| {
                    field
                        .get("field")
                        .and_then(serde_json::Value::as_str)
                        .map(|name| (name, field))
                })
                .collect()
        })
        .unwrap_or_default();
    let fields_narrow = new_fields
        .keys()
        .all(|field| old_fields.contains_key(field))
        && new_fields.iter().all(|(field, value)| {
            let old_field = old_fields[field];
            let mode_ok = field_mode_rank(
                value
                    .get("mode")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            ) <= field_mode_rank(
                old_field
                    .get("mode")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            );
            let clear_ok = !value
                .get("clear_when_canonical_missing")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
                || old_field
                    .get("clear_when_canonical_missing")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
            mode_ok && clear_ok
        });
    if fields_narrow
        && let Some(fields) = candidate
            .get_mut("metadata")
            .and_then(|metadata| metadata.get_mut("fields"))
        && let Some(old_raw) = old
            .get("metadata")
            .and_then(|metadata| metadata.get("fields"))
    {
        *fields = old_raw.clone();
    }

    let old_preserved: BTreeSet<&str> = old
        .get("metadata")
        .and_then(|metadata| metadata.get("preserve_fields"))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect()
        })
        .unwrap_or_default();
    let new_preserved: BTreeSet<&str> = new
        .get("metadata")
        .and_then(|metadata| metadata.get("preserve_fields"))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect()
        })
        .unwrap_or_default();
    if new_preserved.is_superset(&old_preserved)
        && let Some(preserved) = candidate
            .get_mut("metadata")
            .and_then(|metadata| metadata.get_mut("preserve_fields"))
        && let Some(old_raw) = old
            .get("metadata")
            .and_then(|metadata| metadata.get("preserve_fields"))
    {
        *preserved = old_raw.clone();
    }

    let old_relationships = old
        .get("metadata")
        .and_then(|metadata| metadata.get("relationships"))
        .and_then(|relationships| relationships.get("types"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let new_relationships = new
        .get("metadata")
        .and_then(|metadata| metadata.get("relationships"))
        .and_then(|relationships| relationships.get("types"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if ordered_subset(&new_relationships, &old_relationships)
        && let Some(types) = candidate
            .get_mut("metadata")
            .and_then(|metadata| metadata.get_mut("relationships"))
            .and_then(|relationships| relationships.get_mut("types"))
    {
        *types = serde_json::Value::Array(old_relationships);
    }

    let genre_mode_ok = genre_mode_rank(
        new.get("genres")
            .and_then(|genres| genres.get("mode"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(""),
    ) <= genre_mode_rank(
        old.get("genres")
            .and_then(|genres| genres.get("mode"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(""),
    );
    if genre_mode_ok
        && let Some(mode) = candidate
            .get_mut("genres")
            .and_then(|genres| genres.get_mut("mode"))
        && let Some(old_raw) = old.get("genres").and_then(|genres| genres.get("mode"))
    {
        *mode = old_raw.clone();
    }
    let old_sources = old
        .get("genres")
        .and_then(|genres| genres.get("sources"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let new_sources = new
        .get("genres")
        .and_then(|genres| genres.get("sources"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if ordered_subset(&new_sources, &old_sources)
        && let Some(sources) = candidate
            .get_mut("genres")
            .and_then(|genres| genres.get_mut("sources"))
    {
        *sources = serde_json::Value::Array(old_sources);
    }
    let count_ok = new
        .get("genres")
        .and_then(|genres| genres.get("maximum_count"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(i64::MAX)
        <= old
            .get("genres")
            .and_then(|genres| genres.get("maximum_count"))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(i64::MIN);
    if count_ok
        && let Some(count) = candidate
            .get_mut("genres")
            .and_then(|genres| genres.get_mut("maximum_count"))
        && let Some(old_raw) = old
            .get("genres")
            .and_then(|genres| genres.get("maximum_count"))
    {
        *count = old_raw.clone();
    }
    for threshold in [
        "musicbrainz_minimum_count",
        "listenbrainz_minimum_count",
        "lastfm_minimum_weight",
    ] {
        let raises = new
            .get("genres")
            .and_then(|genres| genres.get(threshold))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(i64::MIN)
            >= old
                .get("genres")
                .and_then(|genres| genres.get(threshold))
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(i64::MAX);
        if raises
            && let Some(node) = candidate
                .get_mut("genres")
                .and_then(|genres| genres.get_mut(threshold))
            && let Some(old_raw) = old.get("genres").and_then(|genres| genres.get(threshold))
        {
            *node = old_raw.clone();
        }
    }

    for field in ["providers", "image_types"] {
        let old_list = old
            .get("artwork")
            .and_then(|artwork| artwork.get(field))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let new_list = new
            .get("artwork")
            .and_then(|artwork| artwork.get(field))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        if ordered_subset(&new_list, &old_list)
            && let Some(node) = candidate
                .get_mut("artwork")
                .and_then(|artwork| artwork.get_mut(field))
        {
            *node = serde_json::Value::Array(old_list);
        }
    }
    let old_preserve_types = preserve_existing_types(&old);
    let new_preserve_types = preserve_existing_types(&new);
    if new_preserve_types.is_superset(&old_preserve_types)
        && let Some(node) = candidate
            .get_mut("artwork")
            .and_then(|artwork| artwork.get_mut("preserve_existing_types"))
        && let Some(old_raw) = old
            .get("artwork")
            .and_then(|artwork| artwork.get("preserve_existing_types"))
    {
        *node = old_raw.clone();
    }
    for field in ["minimum_width", "minimum_height"] {
        let raises = new
            .get("artwork")
            .and_then(|artwork| artwork.get(field))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(i64::MIN)
            >= old
                .get("artwork")
                .and_then(|artwork| artwork.get(field))
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(i64::MAX);
        if raises
            && let Some(node) = candidate
                .get_mut("artwork")
                .and_then(|artwork| artwork.get_mut(field))
            && let Some(old_raw) = old.get("artwork").and_then(|artwork| artwork.get(field))
        {
            *node = old_raw.clone();
        }
    }

    let old_sidecars = old
        .get("organization")
        .and_then(|organization| organization.get("sidecar_patterns"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let new_sidecars = new
        .get("organization")
        .and_then(|organization| organization.get("sidecar_patterns"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if ordered_subset(&new_sidecars, &old_sidecars)
        && let Some(node) = candidate
            .get_mut("organization")
            .and_then(|organization| organization.get_mut("sidecar_patterns"))
    {
        *node = serde_json::Value::Array(old_sidecars);
    }
    let cleanup_narrows = old
        .get("organization")
        .and_then(|organization| organization.get("source_cleanup"))
        .and_then(serde_json::Value::as_str)
        == Some("remove_after_confirmed_move")
        && new
            .get("organization")
            .and_then(|organization| organization.get("source_cleanup"))
            .and_then(serde_json::Value::as_str)
            == Some("keep");
    if cleanup_narrows
        && let Some(node) = candidate
            .get_mut("organization")
            .and_then(|organization| organization.get_mut("source_cleanup"))
        && let Some(old_raw) = old
            .get("organization")
            .and_then(|organization| organization.get("source_cleanup"))
    {
        *node = old_raw.clone();
    }

    candidate == old
}

/// Impact verdict for a candidate: classification, preview requirement,
/// affected roots, and human reasons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = LibraryManagementChangeImpact)]
pub struct ChangeImpact {
    /// Current stored revision.
    pub current_settings_revision: String,
    /// Normalized candidate revision.
    pub proposed_settings_revision: String,
    /// The caller's revision was already stale.
    pub stale: bool,
    /// `no_change`, `harmless`, `restrictive`, or `destructive`.
    pub classification: String,
    /// A current dry run must be confirmed before enabling.
    pub preview_required: bool,
    /// Affected root ids.
    pub affected_root_ids: Vec<String>,
    /// Human reasons.
    pub reasons: Vec<String>,
}

/// Classify a candidate against the current settings.
pub fn classify(
    current: &LibraryManagement,
    candidate: &LibraryManagement,
    expected_settings_revision: Option<&str>,
) -> ChangeImpact {
    let current_revision = settings_revision(current);
    let proposed_revision = settings_revision(candidate);
    let stale = expected_settings_revision.is_some_and(|expected| expected != current_revision);
    if current_revision == proposed_revision {
        return ChangeImpact {
            current_settings_revision: current_revision,
            proposed_settings_revision: proposed_revision,
            stale,
            classification: "no_change".to_owned(),
            preview_required: false,
            affected_root_ids: Vec::new(),
            reasons: Vec::new(),
        };
    }
    let current_assignments: BTreeMap<&str, &LibraryManagementRootAssignment> = current
        .root_assignments
        .iter()
        .map(|assignment| (assignment.root_id.as_str(), assignment))
        .collect();
    let candidate_assignments: BTreeMap<&str, &LibraryManagementRootAssignment> = candidate
        .root_assignments
        .iter()
        .map(|assignment| (assignment.root_id.as_str(), assignment))
        .collect();
    let mut roots: BTreeSet<&str> = BTreeSet::new();
    roots.extend(current_assignments.keys().copied());
    roots.extend(candidate_assignments.keys().copied());

    let mut destructive = Vec::new();
    let mut restrictive = Vec::new();
    let mut harmless = Vec::new();
    let mut affected = BTreeSet::new();
    for root_id in roots {
        let old_assignment = current_assignments.get(root_id).copied();
        let new_assignment = candidate_assignments.get(root_id).copied();
        let old_active = active_automatic(old_assignment);
        let new_active = active_automatic(new_assignment);
        if !old_active && new_active {
            destructive.push(format!(
                "Automatic Library Management is enabled for root {root_id}."
            ));
            affected.insert(root_id.to_owned());
            continue;
        }
        if old_active && !new_active {
            restrictive.push(format!(
                "Automatic Library Management is reduced for root {root_id}."
            ));
            affected.insert(root_id.to_owned());
            continue;
        }
        let (Some(old_assignment), Some(new_assignment)) = (old_assignment, new_assignment) else {
            continue;
        };
        if !old_active {
            continue;
        }
        let added_trigger = (new_assignment.automatic_acquisitions
            && !old_assignment.automatic_acquisitions)
            || (new_assignment.automatic_drop_imports && !old_assignment.automatic_drop_imports)
            || (new_assignment.automatic_scan_discovered
                && !old_assignment.automatic_scan_discovered);
        let removed_trigger = (old_assignment.automatic_acquisitions
            && !new_assignment.automatic_acquisitions)
            || (old_assignment.automatic_drop_imports && !new_assignment.automatic_drop_imports)
            || (old_assignment.automatic_scan_discovered
                && !new_assignment.automatic_scan_discovered);
        let old_profile = pin_profile(current, old_assignment);
        let new_profile = pin_profile(candidate, new_assignment);
        let old_payload = scope_payload(current, old_assignment);
        let new_payload = scope_payload(candidate, new_assignment);
        if added_trigger {
            harmless.push(format!(
                "An automatic trigger is enabled for root {root_id}; the authorized write profile is unchanged."
            ));
            affected.insert(root_id.to_owned());
        }
        let old_custom = old_assignment.enabled
            && old_assignment.automatic_scan_discovered
            && old_assignment.automatic_custom_editions;
        let new_custom = new_assignment.enabled
            && new_assignment.automatic_scan_discovered
            && new_assignment.automatic_custom_editions;
        if new_custom && !old_custom {
            destructive.push(format!(
                "Automatic Custom edition management is enabled for root {root_id}."
            ));
            affected.insert(root_id.to_owned());
        }
        if old_payload != new_payload {
            affected.insert(root_id.to_owned());
            match (old_profile, new_profile) {
                (Ok(old), Ok(new)) if is_restrictive_profile_change(&old, &new) => {
                    restrictive.push(format!(
                        "The effective profile is restricted for root {root_id}."
                    ));
                }
                _ => {
                    destructive.push(format!(
                        "The effective profile changes write scope for root {root_id}."
                    ));
                }
            }
        } else if removed_trigger {
            restrictive.push(format!(
                "An automatic trigger is disabled for root {root_id}."
            ));
            affected.insert(root_id.to_owned());
        }
    }

    let (classification, reasons) = if destructive.is_empty() {
        if restrictive.is_empty() {
            (
                "harmless",
                if harmless.is_empty() {
                    vec!["No enabled automatic root gains file-writing scope.".to_owned()]
                } else {
                    harmless
                },
            )
        } else {
            restrictive.extend(harmless);
            ("restrictive", restrictive)
        }
    } else {
        destructive.extend(restrictive);
        ("destructive", destructive)
    };
    ChangeImpact {
        current_settings_revision: current_revision,
        proposed_settings_revision: proposed_revision,
        stale,
        classification: classification.to_owned(),
        preview_required: classification == "destructive",
        affected_root_ids: affected.into_iter().collect(),
        reasons,
    }
}
