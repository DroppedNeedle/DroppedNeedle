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

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use utoipa::ToSchema;
use uuid::Uuid;

use super::error::SettingsError;
use super::library_policy::stable_hash;
use crate::ids::IdGenerator;
use crate::runtime_config::secret_sections::{LibraryRoot, TypedLibrary};
use crate::runtime_config::sections::{
    ArtistStandardization, ArtworkImageType, ArtworkProvider, DEFAULT_NAMING_TEMPLATE, FieldMode,
    GenreSource, Id3TextEncoding, Id3Version, LibraryManagement, LibraryManagementProfile,
    LibraryManagementRootAssignment, LibraryManagementRootOverrides, ManagedField,
    MetadataManagementSettings, Mp3ApePolicy, MultiDiscNamingMode, NamingScript,
    OrganizationManagementSettings, RawAacTagPolicy, ReplayGainMode, SourceCleanupMode,
    TaggingScript, WavTagPolicy,
};

mod activation;
mod bundle;
mod impact;
mod normalize;
mod presets;
mod revision;
mod script;
pub mod service;

pub use activation::*;
pub use bundle::*;
pub use impact::*;
pub use normalize::*;
pub use presets::*;
pub use revision::*;
pub use script::*;

/// A caller fault with a v2 message (400).
fn invalid(message: &str) -> SettingsError {
    SettingsError::InvalidInput {
        message: message.to_owned(),
    }
}
