//! Folder activations pinned before an option was removed.
//!
//! An activation pin is the revision of the folder's whole effective
//! profile, so it also hashed options that no longer exist. A read
//! rebuilds that revision from the stored document. A folder whose pin
//! matched it is pinned again to its profile as it reads now, unless a
//! removed option it used changed what it writes: that folder stays
//! stale until an admin activates it again.

use serde_json::Value;

use super::activation::pin_profile;
use super::revision::{profile_payload_revision, profile_revision};
use droppedneedle_kernel::runtime_config::sections::{LibraryManagement, LibraryManagementProfile};

/// Removed folder overrides nothing read, with the profile option each
/// one set.
const UNREAD_OVERRIDES: [(&str, [&str; 2]); 2] = [
    ("source_cleanup", ["organization", "source_cleanup"]),
    (
        "preserve_timestamps",
        ["file_behavior", "preserve_timestamps"],
    ),
];

/// The removed folder override that was honoured. A folder that set it
/// unlike its profile now follows the profile.
const EDITION_OVERRIDE: &str = "automatic_edition_acceptance_enabled";

/// The Cover Art Archive sources. A profile that listed neither fetched no
/// cover; now one is fetched whenever artwork is on.
const COVER_SOURCES: [&str; 2] = [
    "cover_art_archive_release",
    "cover_art_archive_release_group",
];

/// What a read did with the folders pinned before the removal.
#[derive(Debug, Default)]
pub(super) struct Carried {
    /// Folders pinned again: nothing they write changed.
    pub repinned: Vec<String>,
    /// Folders left stale, with why: a removed option changed what they
    /// write.
    pub kept_stale: Vec<(String, &'static str)>,
}

/// Re-pin every folder whose activation matched the stored document and
/// writes the same without the removed options, once per decoded
/// document.
pub(super) fn carry_removed_options(settings: &mut LibraryManagement) -> Carried {
    let mut carried = Carried::default();
    let Some(document) = settings.as_read.0.take() else {
        return carried;
    };
    for index in 0..settings.root_assignments.len() {
        let assignment = &settings.root_assignments[index];
        let Some(pinned) = assignment.activation_profile_revision.as_deref() else {
            continue;
        };
        let Ok(effective) = pin_profile(settings, assignment) else {
            continue;
        };
        let revision = profile_revision(&effective);
        let Some(stored) = Stored::find(&document, &effective.id, &assignment.root_id) else {
            continue;
        };
        if revision == pinned || stored.revision(&effective) != pinned {
            continue;
        }
        let root_id = assignment.root_id.clone();
        if let Some(reason) = stored.changed_writes(&effective) {
            carried.kept_stale.push((root_id, reason));
            continue;
        }
        settings.root_assignments[index].activation_profile_revision = Some(revision);
        carried.repinned.push(root_id);
    }
    carried
}

/// A folder's stored profile and overrides.
struct Stored<'a> {
    profile: &'a Value,
    overrides: Option<&'a Value>,
}

impl<'a> Stored<'a> {
    fn find(document: &'a Value, profile_id: &str, root_id: &str) -> Option<Self> {
        let profile = document
            .get("profiles")?
            .as_array()?
            .iter()
            .find(|profile| profile.get("id").and_then(Value::as_str) == Some(profile_id))?;
        let overrides = document
            .get("root_assignments")
            .and_then(Value::as_array)
            .and_then(|assignments| {
                assignments.iter().find(|assignment| {
                    assignment.get("root_id").and_then(Value::as_str) == Some(root_id)
                })
            })
            .and_then(|assignment| assignment.get("overrides"))
            .filter(|overrides| overrides.is_object());
        Some(Self { profile, overrides })
    }

    fn removed_override(&self, key: &str) -> Option<&'a Value> {
        self.overrides
            .and_then(|overrides| overrides.get(key))
            .filter(|value| !value.is_null())
    }

    /// The revision `effective` had with the options the stored document
    /// still holds: the stored profile's values for keys the profile no
    /// longer has, then the folder's removed overrides on top.
    fn revision(&self, effective: &LibraryManagementProfile) -> String {
        let mut payload = serde_json::to_value(effective).unwrap_or(Value::Null);
        fill_missing(&mut payload, self.profile);
        let removed = UNREAD_OVERRIDES
            .iter()
            .copied()
            .chain([(EDITION_OVERRIDE, ["identity", EDITION_OVERRIDE])]);
        for (key, [block, field]) in removed {
            if let Some(value) = self.removed_override(key)
                && let Some(block) = payload.get_mut(block).and_then(Value::as_object_mut)
            {
                block.insert(field.to_owned(), value.clone());
            }
        }
        profile_payload_revision(payload)
    }

    /// Why the folder writes differently without the removed options, if
    /// it does.
    fn changed_writes(&self, effective: &LibraryManagementProfile) -> Option<&'static str> {
        if let Some(value) = self.removed_override(EDITION_OVERRIDE)
            && value.as_bool() != Some(effective.identity.automatic_edition_acceptance_enabled)
        {
            return Some("its own automatic edition acceptance setting is gone");
        }
        let artwork_on = effective.artwork.embedded_enabled || effective.artwork.external_enabled;
        let providers = self
            .profile
            .get("artwork")
            .and_then(|artwork| artwork.get("providers"))
            .and_then(Value::as_array);
        if artwork_on
            && let Some(providers) = providers
            && !providers
                .iter()
                .any(|provider| COVER_SOURCES.iter().any(|source| provider == *source))
        {
            return Some("covers now come from the Cover Art Archive");
        }
        None
    }
}

/// Add every key `stored` has and `target` lacks, at any object depth.
fn fill_missing(target: &mut Value, stored: &Value) {
    let (Some(target), Some(stored)) = (target.as_object_mut(), stored.as_object()) else {
        return;
    };
    for (key, value) in stored {
        match target.get_mut(key) {
            None => {
                target.insert(key.clone(), value.clone());
            }
            Some(existing) => fill_missing(existing, value),
        }
    }
}
