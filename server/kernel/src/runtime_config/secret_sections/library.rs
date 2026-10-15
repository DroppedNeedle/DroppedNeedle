//! Typed library settings: roots, path policies, staging, naming, and
//! the AcoustID key.

use super::*;

// --- library_settings (typed roots + policies) ------------------------------

/// Per-path identification policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdentificationPolicy {
    /// Trust local metadata only.
    LocalMetadata,
    /// Automatic identification.
    #[default]
    Automatic,
    /// Excluded from the library.
    Excluded,
}

/// One path-policy rule inside a root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct LibraryPathRule {
    /// Rule id.
    pub id: String,
    /// Path relative to the root.
    pub relative_path: String,
    /// Policy for this path.
    pub policy: IdentificationPolicy,
}

/// One library root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LibraryRoot {
    /// Stable root id.
    pub id: String,
    /// Absolute path.
    pub path: String,
    /// Display label.
    pub label: String,
    /// Default policy.
    pub policy: IdentificationPolicy,
    /// Path rules.
    pub rules: Vec<LibraryPathRule>,
}

impl Default for LibraryRoot {
    fn default() -> Self {
        Self {
            id: String::new(),
            path: String::new(),
            label: String::new(),
            policy: IdentificationPolicy::Automatic,
            rules: Vec::new(),
        }
    }
}

/// Typed library settings: roots, policies, staging, naming, AcoustID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = LibrarySettings)]
#[serde(default)]
pub struct TypedLibrary {
    /// Library roots.
    pub library_roots: Vec<LibraryRoot>,
    /// Staging path.
    pub staging_path: String,
    /// Naming template.
    pub naming_template: String,
    /// AcoustID key (encrypted at rest).
    #[schema(value_type = String)]
    pub acoustid_api_key: Secret,
    /// Master switch: when false the app claims no new library work.
    pub enabled: bool,
}

impl Default for TypedLibrary {
    fn default() -> Self {
        Self {
            library_roots: Vec::new(),
            staging_path: String::new(),
            naming_template: DEFAULT_NAMING_TEMPLATE.to_owned(),
            acoustid_api_key: Secret::default(),
            enabled: true,
        }
    }
}

impl Section for TypedLibrary {
    const KEY: &'static str = "library_settings";
}

impl SecretSection for TypedLibrary {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.acoustid_api_key,
            mask: ACOUSTID_KEY_MASK,
            strip: false,
        }]
    }
}
