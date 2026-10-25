//! Typed runtime config (tier 2 of 2) plus the secrets core.
//!
//! Deployment values live in tier 1 (the environment via `AppConfig::load`;
//! see [`deployment`](crate::config::deployment) for the kept/dropped
//! registry). Everything
//! user-editable at runtime lives here as a typed section with a schema and
//! a getter on [`ConfigStore`](store::ConfigStore), stored as one row per
//! section in the database. Secrets follow the
//! exact-match mask-sentinel rule ([`mask`]) and are encrypted at rest by
//! [`Crypto`](crypto::Crypto); they are never logged (a test enforces it).
//!
//! No HTTP surface lives here: `settings` owns the settings routes and
//! `import` the v2 importer (which re-encrypts under this key).

pub mod crypto;
pub mod effects;
pub mod error;
pub mod ingest;
pub mod key_check;
pub mod mask;
pub mod origin;
pub mod plain;
pub mod registry;
pub mod secret;
pub mod secret_sections;
pub mod sections;
pub mod settings_error;
pub mod sql;
pub mod store;

pub use crypto::{Crypto, KeyOrigin};
pub use error::ConfigError;
pub use ingest::LegacyConfig;
pub use mask::{Masked, SECRET_MASK};
pub use plain::{Plain, PluginValues};
pub use registry::{KNOWN_TOP_LEVEL_KEYS, Listed};
pub use secret::Secret;
pub use secret_sections::SecretSection;
pub use sections::Section;
pub use store::{ConfigStore, Live, UpdateError};

/// One dropped section with the reason it is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DroppedSection {
    /// Former config-file key.
    pub key: &'static str,
    /// Why it is gone.
    pub note: &'static str,
}

/// Sections v3 dropped. The import validator rejects these in exports, and
/// the one-time copy of `config.json` leaves them behind.
pub const DROPPED_SECTIONS: &[DroppedSection] = &[
    DroppedSection {
        key: "library_sync_settings",
        note: "Legacy catalog; one-shot sync_frequency import only.",
    },
    DroppedSection {
        key: "library_scan_dirty_scopes",
        note: "Transient hints; mechanism stays runtime-only.",
    },
    DroppedSection {
        key: "local_files_settings",
        note: "Vestigial; zero consumers, no routes.",
    },
    DroppedSection {
        key: "_legacy_lidarr",
        note: "One-time backup; its plaintext key never carries forward.",
    },
    DroppedSection {
        key: "jellyfin_url",
        note: "Top-level mirror; the section URL wins.",
    },
];

/// Dropped section keys, for presence checks.
pub const DROPPED_SECTION_KEYS: &[&str] = &[
    "library_sync_settings",
    "library_scan_dirty_scopes",
    "local_files_settings",
    "_legacy_lidarr",
    "jellyfin_url",
];
