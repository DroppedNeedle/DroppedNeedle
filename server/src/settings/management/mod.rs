//! Library Management settings: profiles, presets, naming and tagging
//! scripts, revisions, impact classification, activation health, and
//! profile sharing.
//!
//! Ports v2's profile service over the `library_management` section and
//! works on the section types directly. All content revisions (settings,
//! profile, script, naming-policy) hash byte-compatibly with v2
//! (ASCII-escaped canonical JSON), so migrated activations and CAS tokens
//! keep working. Golden vectors minted from v2 pin this in the settings
//! tests.
//!
//! Script validation rides behind the [`ScriptCompiler`] port: the
//! shipped structural compiler checks the documented rules (naming
//! shape, known variables/targets, append gates); the full expression
//! compiler is library-engine follow-up work behind the same port.

use super::error::SettingsError;

mod activation;
mod bundle;
mod impact;
mod normalize;
mod presets;
mod revision;
mod script;
pub mod service;

pub use activation::{
    ActivationHealth, activation_health, activation_is_current, migration_carry_applies,
    pin_naming_scripts, pin_profile, validate_root_assignments,
};
pub use bundle::{
    EncodedProfileBundle, MAX_MANAGEMENT_NAME_LENGTH, MAX_PROFILE_BUNDLE_BYTES,
    MAX_PROFILE_SHARE_CODE_CHARS, MaterializedProfileBundle, PROFILE_BUNDLE_FORMAT,
    PROFILE_BUNDLE_MIME_TYPE, PROFILE_BUNDLE_VERSION, PROFILE_PREVIEW_NAMESPACE,
    PROFILE_SHARE_CODE_PREFIX, ParsedProfileBundle, PortableDocument, PortablePayload,
    PortableScript, ProfileImportWarning, export_profile_bundle, materialize_profile_bundle,
    parse_profile_bundle, preview_materialized_profile, profile_aspects, profile_bundle_filename,
    profile_import_warnings, resolve_import_names, unique_import_name,
};
pub use impact::{
    ChangeImpact, active_automatic, classify, is_restrictive_profile_change, scope_payload,
};
pub use normalize::{migration_carry, normalize, validate_preset_provenance};
pub use presets::{
    COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID, LEGACY_DEFAULT_SIDECAR_PATTERNS,
    LEGACY_NAMING_PROFILE_ID, LEGACY_NAMING_SCRIPT_ID, MANAGED_FIELD_NAMES,
    MANAGEMENT_SCHEMA_VERSION, MERGEABLE_MANAGED_FIELD_NAMES, PICARD_MULTI_DISC_NAMING_SCRIPT_ID,
    PICARD_MULTI_DISC_NAMING_SOURCE, PICARD_NAMING_SCRIPT_ID, PICARD_ORGANIZER_PRESET_VERSION,
    PICARD_ORGANIZER_PROFILE_ID, PICARD_STANDARD_NAMING_SOURCE, PRESET_CATALOG_VERSION,
    PRESET_DIFF_GROUPS, PresetDiff, complete_library_organizer_profile,
    current_picard_preset_scripts, initial_settings, migrate_presets,
    picard_style_organizer_profile, preset_diff, preset_profile_for_origin,
    sidecar_default_history, unique_preset_name,
};
pub use revision::{
    naming_policy_revision, naming_script_revision, profile_revision, settings_revision,
    tagging_script_revision,
};
pub use script::{
    ExprVariable, NamingSegment, ScriptCompiler, ScriptError, StructuralCompiler, TaggingStatement,
    naming_variables,
};

/// A caller fault with a v2 message (400).
fn invalid(message: &str) -> SettingsError {
    SettingsError::InvalidInput {
        message: message.to_owned(),
    }
}
