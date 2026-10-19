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
//! No HTTP surface lives here: `settings` owns the settings routes,
//! `import` the v2 importer (which re-encrypts under this key), and
//! `providers::youtube` the client around [`QuotaStore`](quota::QuotaStore).

pub mod crypto;
pub mod error;
pub mod ingest;
pub mod mask;
pub mod origin;
pub mod quota;
pub mod secret;
pub mod secret_sections;
pub mod sections;
pub mod settings_error;
pub mod sql;
pub mod store;

pub use crypto::{Crypto, KeyOrigin};
pub use error::ConfigError;
pub use ingest::LegacyConfig;
pub use mask::Masked;
pub use quota::{QuotaStatus, QuotaStore};
pub use secret::Secret;
pub use secret_sections::SecretSection;
pub use sections::Section;
pub use store::{ConfigStore, UpdateError};

/// Every settings key v3 stores: kept sections plus the instance id. The
/// one-time copy of `config.json` ([`ingest`]) copies only these and names
/// any other key it leaves behind.
pub const KNOWN_TOP_LEVEL_KEYS: &[&str] = &[
    "user_preferences",
    "home_settings",
    "library_scan_schedule",
    "library_scan_filesystem_watcher",
    "advanced_settings",
    "download_client",
    "download_clients",
    "download_policy",
    "wanted",
    "source_priority",
    "usenet_search_backend",
    "indexers",
    "prowlarr",
    "lidarr_import",
    "jellyfin_settings",
    "navidrome_settings",
    "plex_settings",
    "listenbrainz_settings",
    "youtube_settings",
    "lastfm_settings",
    "lyrics_settings",
    "spotify_settings",
    "events",
    "wrapped_settings",
    "oidc_settings",
    "security_settings",
    "public_address",
    "connect_apps",
    "library_settings",
    "library_management",
    "musicbrainz_settings",
    "scrobble_settings",
    "primary_music_source",
    "get_it",
    "edition_preferences",
    "plugins",
    "instance_id",
    "_internal",
];

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
